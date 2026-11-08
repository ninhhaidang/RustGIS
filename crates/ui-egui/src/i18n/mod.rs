//! Interface languages. Strings in the code stay English and are the lookup keys; a catalog per
//! language (`<code>.tsv`) maps them to display text when the UI is drawn. Command ids, project
//! files, the CLI and data never see translated text, so scripts are unaffected. A string with no
//! translation shows in English, so coverage can grow one entry at a time.
//!
//! The `language` preference is `auto` by default: the UI follows the system's languages
//! (`RUSTGIS_LANGUAGE`, then the OS or browser preferences) when there is a catalog, else English.
//! Settings › Language picks one by hand.
//!
//! The language set follows GeoLibre's, minus scripts egui can't yet shape or lay out
//! (Hebrew, Hindi, Thai). Persian is included: egui shapes its script and lays out a
//! line whose first strong character is of it back to front, which pure Persian
//! survives; [`rtl`] fixes the Latin runs and brackets inside such lines, and the
//! Persian catalog starts its lines with Persian text, for a line that starts with a
//! Latin letter is laid out front to back. [`paragraph`] shows long text: the
//! toolkit's own wrapping fills rows from the end of a back-to-front line, so its
//! rows are broken ahead of time. Laying out the widgets themselves right to left
//! is future work.
//!
//! # Catalog format
//! UTF-8, one `English<TAB>Translation` pair or `Context<TAB>English<TAB>Translation` per line; `#` starts a comment line; `\n` and `\t`
//! inside a field are a line break and a tab. `{name}` placeholders must be kept (they may move).
//!
//! # Adding a language
//! Add `xx.tsv` (copy the keys of another catalog) and one row to [`LANGUAGES`].

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::OnceLock;

mod rtl;

/// The preference value that follows the system.
pub const AUTO: &str = "auto";

/// Writing systems that need an extra font.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    /// Latin, Cyrillic, Greek or Georgian: covered by the interface fonts.
    Alphabetic,
    Cjk(CjkFont),
    /// Arabic script (Persian): a face with Arabic and Latin letters, drawn ahead of the
    /// interface font.
    Arabic,
    /// Bengali: an Indic script with a face discovered on the desktop where available.
    Bengali,
    /// Lao: a face discovered on the desktop where available, after the interface font.
    Lao,
}

/// Which CJK face reads best for a language (the glyph shapes differ by region).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CjkFont {
    SimplifiedChinese,
    Japanese,
    Korean,
}

pub struct LangInfo {
    /// Lowercase BCP 47 code; also the preference value.
    pub code: &'static str,
    /// The language's name in itself.
    pub name: &'static str,
    /// The name in English (for the picker's tooltip).
    pub english: &'static str,
    pub script: Script,
    source: &'static str,
    catalog: OnceLock<Catalog>,
    /// Catalog values in display form, for a language laid out back to front.
    visual: OnceLock<Catalog>,
}

macro_rules! lang {
    ($code:literal, $name:literal, $en:literal, $script:expr, $file:literal) => {
        LangInfo {
            code: $code,
            name: $name,
            english: $en,
            script: $script,
            source: include_str!($file),
            catalog: OnceLock::new(),
            visual: OnceLock::new(),
        }
    };
}

/// The registry. English first: it is the source language and the fallback.
pub static LANGUAGES: [LangInfo; 21] = [
    LangInfo {
        code: "en",
        name: "English",
        english: "English",
        script: Script::Alphabetic,
        source: "",
        catalog: OnceLock::new(),
        visual: OnceLock::new(),
    },
    lang!("de", "Deutsch", "German", Script::Alphabetic, "de.tsv"),
    lang!("es", "Español", "Spanish", Script::Alphabetic, "es.tsv"),
    lang!("fr", "Français", "French", Script::Alphabetic, "fr.tsv"),
    lang!("it", "Italiano", "Italian", Script::Alphabetic, "it.tsv"),
    lang!("nl", "Nederlands", "Dutch", Script::Alphabetic, "nl.tsv"),
    lang!("pl", "Polski", "Polish", Script::Alphabetic, "pl.tsv"),
    lang!("pt", "Português", "Portuguese", Script::Alphabetic, "pt.tsv"),
    lang!("ru", "Русский", "Russian", Script::Alphabetic, "ru.tsv"),
    lang!("tr", "Türkçe", "Turkish", Script::Alphabetic, "tr.tsv"),
    lang!("az", "Azərbaycanca", "Azerbaijani", Script::Alphabetic, "az.tsv"),
    lang!("id", "Bahasa Indonesia", "Indonesian", Script::Alphabetic, "id.tsv"),
    lang!("vi", "Tiếng Việt", "Vietnamese", Script::Alphabetic, "vi.tsv"),
    lang!("fa", "فارسی", "Persian (Farsi)", Script::Arabic, "fa.tsv"),
    lang!("zh", "简体中文", "Chinese (Simplified)", Script::Cjk(CjkFont::SimplifiedChinese), "zh.tsv"),
    lang!("ja", "日本語", "Japanese", Script::Cjk(CjkFont::Japanese), "ja.tsv"),
    lang!("ko", "한국어", "Korean", Script::Cjk(CjkFont::Korean), "ko.tsv"),
    lang!("ka", "ქართული", "Georgian", Script::Alphabetic, "ka.tsv"),
    lang!("lo", "ພາສາລາວ", "Lao", Script::Lao, "lo.tsv"),
    lang!("bn", "বাংলা", "Bengali", Script::Bengali, "bn.tsv"),
    lang!("el", "Ελληνικά", "Greek", Script::Alphabetic, "el.tsv"),
];

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Catalogs group keys by context; the empty context holds ordinary two-column entries.
type Catalog = HashMap<String, HashMap<String, String>>;

/// Parses the context-free entries of a catalog, reporting malformed and duplicate rows.
/// Contextual entries are checked too; the UI loads them through [`parse_catalog`].
pub fn parse(source: &str) -> (HashMap<String, String>, Vec<String>) {
    let (mut catalog, errors) = parse_catalog(source);
    (catalog.remove("").unwrap_or_default(), errors)
}

/// Parses both row formats without allowing a duplicate to replace an earlier translation.
/// Diagnostics carry one-based line numbers; callers add the catalog's language or filename.
fn parse_catalog(source: &str) -> (Catalog, Vec<String>) {
    let mut catalog = Catalog::new();
    let mut errors = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let columns: Vec<_> = line.split('\t').collect();
        let (context, en, tr) = match columns.as_slice() {
            [en, tr] => ("", *en, *tr),
            [context, en, tr] => (*context, *en, *tr),
            _ => {
                errors.push(format!("line {}: expected [context<TAB>]English<TAB>translation", i + 1));
                continue;
            }
        };
        if en.is_empty() || tr.trim().is_empty() {
            errors.push(format!("line {}: English and translation must not be empty", i + 1));
            continue;
        }
        let entries = catalog.entry(unescape(context)).or_default();
        match entries.entry(unescape(en)) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(unescape(tr));
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                errors.push(format!("line {}: duplicate key {en:?} in context {context:?}", i + 1));
            }
        }
    }
    (catalog, errors)
}

impl LangInfo {
    /// Loads each catalog once, keeping owned strings for the lifetime of the registry entry.
    fn catalog(&self) -> &Catalog {
        self.catalog.get_or_init(|| {
            let (catalog, errors) = parse_catalog(self.source);
            for e in errors {
                log::warn!("{} catalog: {e}", self.code);
            }
            catalog
        })
    }

    /// Catalog values in display form, grouped by context. Empty for languages without RTL text.
    fn visual_catalog(&self) -> &Catalog {
        self.visual.get_or_init(|| {
            if !rtl::has_rtl(self.source) {
                return HashMap::new();
            }
            self.catalog()
                .iter()
                .map(|(context, entries)| (context.clone(), entries.iter().map(|(key, value)| (key.clone(), rtl::visual(value))).collect()))
                .collect()
        })
    }

    /// Looks up a context without allocating a combined context/text key on each frame.
    fn lookup(&self, context: &str, s: &str, visual: bool) -> Option<&str> {
        let catalog = if visual { self.visual_catalog() } else { self.catalog() };
        catalog.get(context)?.get(s).map(String::as_str)
    }
}

/// A language the UI can be shown in.
#[derive(Clone, Copy)]
pub struct Lang(&'static LangInfo);

impl PartialEq for Lang {
    fn eq(&self, o: &Self) -> bool {
        self.0.code == o.0.code
    }
}

impl std::fmt::Debug for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Lang({})", self.0.code)
    }
}

impl Lang {
    pub const EN: Lang = Lang(&LANGUAGES[0]);

    pub fn code(self) -> &'static str {
        self.0.code
    }
    pub fn name(self) -> &'static str {
        self.0.name
    }
    pub fn english(self) -> &'static str {
        self.0.english
    }
    pub fn script(self) -> Script {
        self.0.script
    }
    pub fn from_code(code: &str) -> Option<Lang> {
        LANGUAGES.iter().find(|l| l.code.eq_ignore_ascii_case(code.trim())).map(Lang)
    }
    /// Resolves a preference: a code, or [`AUTO`] (or anything unknown) for the system language.
    pub fn from_pref(pref: &str) -> Lang {
        Lang::from_code(pref).unwrap_or_else(system_lang)
    }
    pub fn all() -> impl Iterator<Item = Lang> {
        LANGUAGES.iter().map(Lang)
    }
    /// Number of translated strings.
    pub fn len(self) -> usize {
        self.0.catalog().values().map(HashMap::len).sum()
    }
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// The registered language for a locale tag such as `de_DE.UTF-8`, `pt-BR` or `zh-Hans-CN`.
pub fn lang_from_tag(tag: &str) -> Option<Lang> {
    let base = tag.split(['.', '@']).next().unwrap_or("").replace('_', "-").to_ascii_lowercase();
    if matches!(base.as_str(), "c" | "posix") {
        return Some(Lang::EN);
    }
    let mut parts = base.split('-');
    let primary = parts.next().unwrap_or("");
    // Only Simplified Chinese has a catalog: Traditional-script locales fall back to English
    // rather than get Simplified text.
    if primary == "zh" && parts.any(|p| matches!(p, "hant" | "tw" | "hk" | "mo")) {
        return None;
    }
    Lang::from_code(primary)
}

/// The first of the system's preferred languages that has a catalog.
pub fn first_supported<'a>(tags: impl IntoIterator<Item = &'a str>) -> Option<Lang> {
    tags.into_iter().find_map(lang_from_tag)
}

/// The system language (looked up once); English when unknown or unsupported.
pub fn system_lang() -> Lang {
    if cfg!(test) {
        return Lang::EN;
    }
    static SYSTEM: OnceLock<usize> = OnceLock::new();
    let i = *SYSTEM.get_or_init(|| {
        let from_env = std::env::var("RUSTGIS_LANGUAGE").ok().and_then(|v| lang_from_tag(&v));
        let lang = from_env.or_else(|| {
            let tags: Vec<String> = sys_locale::get_locales().take(16).collect();
            first_supported(tags.iter().map(String::as_str))
        });
        lang.and_then(|l| LANGUAGES.iter().position(|x| x.code == l.code())).unwrap_or(0)
    });
    LANGUAGES.get(i).map(Lang).unwrap_or(Lang::EN)
}

thread_local! {
    static CURRENT: Cell<Lang> = const { Cell::new(Lang::EN) };
}

/// Sets the language the UI is drawn in.
pub fn set_current(lang: Lang) {
    CURRENT.set(lang);
}

pub fn current() -> Lang {
    CURRENT.get()
}

/// `s` in the current language.
pub fn t(s: &str) -> &str {
    tr(current(), s)
}

/// `s` in `lang`, in display form: a line of right-to-left text has its Latin runs
/// pre-reversed, so it reads correctly when the toolkit lays it out back to front.
/// Strings without a translation come back unchanged. Text the catalog doesn't have may
/// be a plugin's (a tool name, a parameter label…): its own `i18n` translates it.
pub fn tr(lang: Lang, s: &str) -> &str {
    if let Some(t) = lang.0.lookup("", s, true) {
        return t;
    }
    if let Some(t) = lang.0.lookup("", s, false) {
        return t;
    }
    if lang == Lang::EN {
        return s;
    }
    plugin_tr(lang, s).unwrap_or(s)
}

/// Translates a homograph in the given context, falling back to the ordinary translation.
/// Missing contextual and ordinary entries show the English source, never the context name.
pub fn tr_context<'a>(lang: Lang, context: &str, s: &'a str) -> &'a str {
    lang.0.lookup(context, s, true).or_else(|| lang.0.lookup(context, s, false)).unwrap_or_else(|| tr(lang, s))
}

/// Contextual translation in the current language, in display form.
pub fn t_context<'a>(context: &str, s: &'a str) -> &'a str {
    tr_context(current(), context, s)
}

/// Contextual translation in logical order, for fragments inserted into a formatted message.
pub fn t_raw_context<'a>(context: &str, s: &'a str) -> &'a str {
    let lang = current();
    lang.0.lookup(context, s, false).unwrap_or_else(|| tr_raw(lang, s))
}

/// `s` in `lang`, in logical order: [`tr`] without the display form, for a message whose
/// placeholders are filled — and whose separators are joined — before it is shown. A
/// plugin's translation is used when the catalog has none, so [`fmt`] and [`location`]
/// translate plugin strings as well.
pub fn tr_raw(lang: Lang, s: &str) -> &str {
    if let Some(t) = lang.0.lookup("", s, false) {
        return t;
    }
    if lang == Lang::EN {
        return s;
    }
    plugin_tr_raw(lang, s).unwrap_or(s)
}

/// `s` in the current language, in logical order: [`t`] without the display form, for a
/// value filled into [`fmt`], which shows the finished message in display form:
/// `tl_raw!` instead of `tl!` inside `fmt`'s arguments.
pub fn t_raw(s: &str) -> &str {
    tr_raw(current(), s)
}

/// Lookups kept before the plugin translation cache starts over (dynamic text, such as layer
/// names, also misses the catalog).
const PLUGIN_CACHE: usize = 20_000;

thread_local! {
    /// Plugin translations in display form, handed out so far, leaked once each so
    /// lookups return `&'static str`
    /// like the catalogs. Bounded: plugins' `i18n` tables are limited in size.
    static PLUGIN_STRINGS: std::cell::RefCell<std::collections::HashSet<&'static str>> = Default::default();
    /// The same, in logical order, for messages whose placeholders are filled
    /// ([`fmt`]) or whose segments are joined ([`location`]) before they are shown.
    static PLUGIN_RAW_STRINGS: std::cell::RefCell<std::collections::HashSet<&'static str>> = Default::default();
    /// Plugin lookups this frame and the ones before, misses included: (plugin revision, language,
    /// logical and display results). `tr` runs for every label every frame.
    static PLUGIN_CACHE_MAP: std::cell::RefCell<(u64, &'static str, std::collections::HashMap<String, Option<(&'static str, &'static str)>>)> =
        Default::default();
}

/// `s` leaked once, so repeated lookups of the same plugin string return the same
/// `&'static str`.
fn leak_once(seen: &mut std::collections::HashSet<&'static str>, s: String) -> &'static str {
    if let Some(&kept) = seen.get(s.as_str()) {
        return kept;
    }
    let kept: &'static str = Box::leak(s.into_boxed_str());
    seen.insert(kept);
    kept
}

/// `s` translated by a loaded plugin's `i18n`, in logical order and in display form.
fn plugin_tr_pair(lang: Lang, s: &str) -> Option<(&'static str, &'static str)> {
    let revision = rustgis_engine::plugins::revision();
    PLUGIN_CACHE_MAP.with_borrow_mut(|(rev, code, map)| {
        if *rev != revision || *code != lang.code() || map.len() > PLUGIN_CACHE {
            *rev = revision;
            *code = lang.code();
            map.clear();
        }
        if let Some(hit) = map.get(s) {
            return *hit;
        }
        let found = rustgis_engine::plugins::translate(lang.code(), s).map(|t| {
            let shown = rtl::visual(&t);
            let raw = PLUGIN_RAW_STRINGS.with_borrow_mut(|seen| leak_once(seen, t));
            let vis = PLUGIN_STRINGS.with_borrow_mut(|seen| leak_once(seen, shown));
            (raw, vis)
        });
        map.insert(s.to_string(), found);
        found
    })
}

/// `s` translated by a loaded plugin's `i18n`, in display form.
fn plugin_tr(lang: Lang, s: &str) -> Option<&'static str> {
    plugin_tr_pair(lang, s).map(|(_, vis)| vis)
}

/// `s` translated by a loaded plugin's `i18n`, in logical order.
fn plugin_tr_raw(lang: Lang, s: &str) -> Option<&'static str> {
    plugin_tr_pair(lang, s).map(|(raw, _)| raw)
}

/// A location such as `Map › Navigate`, translated segment by segment and shown in
/// display form as one line.
pub fn location(loc: &str) -> String {
    let lang = current();
    let loc = loc.split(" › ").map(|s| tr_raw(lang, s)).collect::<Vec<_>>().join(" › ");
    rtl::visual(&loc)
}

/// Translates `template`, fills its `{name}` placeholders in one pass (values are never
/// re-read as templates, so a layer named `{n}` stays intact), then puts the message in
/// display form: a line that starts with right-to-left text has its Latin runs reversed
/// so it reads correctly when the toolkit lays it out back to front. Values go in in
/// logical order — [`t_raw`] (`tl_raw!`) for translated fragments, raw data as it is —
/// for the finished message is transformed as a whole.
pub fn fmt(template: &str, args: &[(&str, &str)]) -> String {
    rtl::visual(&fmt_raw(template, args))
}

/// [`fmt`] in logical order, without the display form: for a message shown with
/// [`paragraph`], which fits a right-to-left message to its rows and puts each row in
/// display form itself.
pub fn fmt_raw(template: &str, args: &[(&str, &str)]) -> String {
    fill_placeholders(tr_raw(current(), template), args)
}

/// Translates a template in its context, then fills placeholders before RTL display conversion.
pub fn fmt_context(context: &str, template: &str, args: &[(&str, &str)]) -> String {
    rtl::visual(&fill_placeholders(t_raw_context(context, template), args))
}

/// Fills placeholders once, keeping inserted data literal, and leaves the line in logical
/// order: callers put the completed line in display form as a whole, or not at all.
fn fill_placeholders(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some((before, after)) = rest.split_once('{') {
        out.push_str(before);
        let Some((name, tail)) = after.split_once('}') else {
            out.push('{');
            out.push_str(after);
            return out;
        };
        match args.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => out.push_str(v),
            None => {
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

thread_local! {
    /// Paragraph rows fitted by [`paragraph`], keyed by (text, width bucket, font):
    /// a redrawing frame hands the same text, width and font back, and measuring
    /// every row again would be work for nothing. Bounded: a handful of long
    /// paragraphs are on screen at a time.
    static FITTED: std::cell::RefCell<HashMap<(String, u64, u32), String>> = Default::default();
}

/// A hashable stand-in for the font a paragraph is fitted in.
fn font_key(font_id: &egui::FontId) -> u32 {
    let family = if font_id.family == egui::FontFamily::Proportional {
        0x1000
    } else if font_id.family == egui::FontFamily::Monospace {
        0x2000
    } else {
        0x3000
    };
    font_id.size.to_bits() ^ family
}

/// Shows `s` in the current language as a wrapped paragraph. For a language laid
/// out back to front, the rows are broken to the width ahead of time ([`rtl::fit`]:
/// the toolkit's own wrapping fills rows from the end of such a line), given in
/// display form, and aligned to the right, the way the paragraph reads; every
/// other language gets a normal wrapping label. `font_id` and `color` are the
/// label's: they also measure the rows.
pub fn paragraph(ui: &mut egui::Ui, s: &str, font_id: egui::FontId, color: egui::Color32) -> egui::Response {
    let text = tr_raw(current(), s).to_owned();
    let wrap = ui.available_width();
    let mut halign = egui::Align::LEFT;
    let text = if rtl::has_rtl(&text) {
        halign = egui::Align::RIGHT;
        // Rows are fitted to a width bucket, never to a fraction of a pixel, so the
        // next redraw of the same width reuses them. Below the width of a single
        // word a row overflows anyway, and the toolkit's wrapping takes over.
        let bucket = (((wrap / 8.0).floor()).max(1.0) as u64) * 8;
        let font = font_key(&font_id);
        let key = (text.clone(), bucket, font);
        FITTED.with_borrow_mut(|fitted| {
            if fitted.len() > 64 {
                fitted.clear();
            }
            if let Some(rows) = fitted.get(&key) {
                return rows.clone();
            }
            let width = |t: &str| -> f32 {
                let job = egui::text::LayoutJob::simple(t.to_owned(), font_id.clone(), color, f32::INFINITY);
                ui.painter().layout_job(job).rect.width()
            };
            let rows = rtl::fit(&text, (bucket as f32 - 2.0).max(50.0), &width);
            fitted.insert(key, rows.clone());
            rows
        })
    } else {
        text
    };
    let mut job = egui::text::LayoutJob::simple(text, font_id, color, wrap);
    job.halign = halign;
    ui.add(egui::Label::new(ui.painter().layout_job(job)))
}

/// `s` in the current language: `tl!("Explore")`.
#[macro_export]
macro_rules! tl {
    ($s:expr, context = $context:expr) => {
        $crate::i18n::t_context($context, $s)
    };
    ($s:expr) => {
        $crate::i18n::t($s)
    };
}

/// `s` in the current language, in logical order: `tl_raw!("Add")` — for values
/// filled into [`fmt`], which shows the finished message in display form, where
/// `tl!`'s display form would be transformed twice.
#[macro_export]
macro_rules! tl_raw {
    ($s:expr, context = $context:expr) => {
        $crate::i18n::t_raw_context($context, $s)
    };
    ($s:expr) => {
        $crate::i18n::t_raw($s)
    };
}

#[cfg(test)]
mod tests;
