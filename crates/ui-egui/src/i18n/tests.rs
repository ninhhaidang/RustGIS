use super::*;

#[test]
fn parse_and_escape() {
    let (m, e) = parse("# c\nOpen\tÖffnen\nA\\nB\tX\\nY\nbad line\n");
    assert_eq!(m.get("Open").map(String::as_str), Some("Öffnen"));
    assert_eq!(m.get("A\nB").map(String::as_str), Some("X\nY"));
    assert_eq!(e.len(), 1);
}

#[test]
fn every_catalog_parses_cleanly_and_keeps_placeholders() {
    for l in &LANGUAGES[1..] {
        let (catalog, errors) = parse_catalog(l.source);
        assert!(errors.is_empty(), "{}: {errors:?}", l.code);
        for (en, tr) in catalog.values().flat_map(|entries| entries.iter()) {
            let names = |s: &str| -> Vec<String> {
                let mut v: Vec<String> = s.split('{').skip(1).filter_map(|p| p.split_once('}').map(|x| x.0.to_string())).collect();
                v.sort();
                v
            };
            assert_eq!(names(en), names(tr), "{}: placeholders differ in {en:?}", l.code);
        }
    }
}

#[test]
fn lookup_fallback_and_tags() {
    assert_eq!(tr(Lang::EN, "Explore"), "Explore");
    assert_eq!(tr(Lang::EN, "no such string"), "no such string");
    assert_eq!(lang_from_tag("de_DE.UTF-8").map(Lang::code), Some("de"));
    assert_eq!(lang_from_tag("pt-BR").map(Lang::code), Some("pt"));
    assert_eq!(lang_from_tag("bn-BD").map(Lang::code), Some("bn"));
    assert_eq!(lang_from_tag("pl_PL.UTF-8").map(Lang::code), Some("pl"));
    assert_eq!(lang_from_tag("el_GR.UTF-8").map(Lang::code), Some("el"));
    assert_eq!(lang_from_tag("el-CY").map(Lang::code), Some("el"));
    assert_eq!(lang_from_tag("zh-Hans-CN").map(Lang::code), Some("zh"));
    assert_eq!(lang_from_tag("zh_CN.UTF-8").map(Lang::code), Some("zh"));
    assert_eq!(lang_from_tag("zh-TW"), None);
    assert_eq!(lang_from_tag("zh-Hant-HK"), None);
    assert_eq!(lang_from_tag("C").map(Lang::code), Some("en"));
    assert_eq!(lang_from_tag("xx-YY"), None);
    assert_eq!(first_supported(["xx", "fr-CA", "de"]).map(Lang::code), Some("fr"));
    assert_eq!(fmt("{n} of {total} selected", &[("n", "3"), ("total", "{n}")]), "3 of {n} selected");
}

#[test]
fn bengali_registration_and_fallback() {
    let bn = Lang::from_code("bn").unwrap_or(Lang::EN);
    assert_eq!(bn.name(), "বাংলা");
    assert_eq!(bn.english(), "Bengali");
    assert_eq!(bn.script(), Script::Bengali);
    assert_eq!(tr(bn, "Open"), "খুলুন");
    assert_eq!(tr(bn, "A string not in the Bengali catalog"), "A string not in the Bengali catalog");
}

#[test]
fn polish_registration_and_fallback() {
    let pl = Lang::from_code("pl").unwrap_or(Lang::EN);
    assert_eq!(pl.name(), "Polski");
    assert_eq!(pl.english(), "Polish");
    assert_eq!(pl.script(), Script::Alphabetic);
    assert_eq!(tr(pl, "Open"), "Otwórz");
    assert_eq!(tr(pl, "A string not in the Polish catalog"), "A string not in the Polish catalog");
}

#[test]
fn greek_registration_and_fallback() {
    let el = Lang::from_code("el").unwrap_or(Lang::EN);
    assert_eq!(el.name(), "Ελληνικά");
    assert_eq!(el.english(), "Greek");
    assert_eq!(el.script(), Script::Alphabetic);
    assert_eq!(tr(el, "Open"), "Άνοιγμα");
    assert_eq!(tr(el, "A string not in the Greek catalog"), "A string not in the Greek catalog");
}

#[test]
fn plugin_strings_use_the_plugins_translations() {
    use rustgis_engine::plugins;
    let pkg = plugins::Package::from_zip(&plugins::testing::package("i18n-fallback", &[])).unwrap();
    plugins::load(pkg, "test").unwrap();
    let de = Lang::from_code("de").unwrap();
    // Not in RustGIS's catalog: the plugin's `i18n` has it.
    assert_eq!(tr(de, "Hello"), "Hallo");
    assert_eq!(tr(Lang::EN, "Hello"), "Hello");
    assert_eq!(tr(Lang::from_code("fr").unwrap(), "Hello"), "Hello");
    // RustGIS's own catalog comes first.
    assert_eq!(tr(de, "Open"), de.0.lookup("", "Open", false).unwrap_or("Open"));
    plugins::unload("i18n-fallback");
}

#[test]
fn start_up_project_strings_have_real_translations() {
    let keys = [
        "Last Project",
        "Choose Project…",
        "No project opened or saved yet",
        "What RustGIS opens with: the World Atlas sample, a blank map, a project you choose, or the last project opened or saved.",
        "RustGIS didn't close properly last time, so it started with the World Atlas instead of your start-up project. Open the project from the Project menu when you're ready.",
        "The start-up project {path} isn't a RustGIS project file.",
        "The start-up project {path} didn't open: {error}",
    ];
    for l in &LANGUAGES[1..] {
        let (catalog, _) = parse(l.source);
        for key in keys {
            let translated = catalog.get(key).unwrap_or_else(|| panic!("{}: missing {key}", l.code));
            assert_ne!(translated, key, "{}: untranslated {key}", l.code);
        }
    }
}

#[test]
fn chatgpt_recovery_and_controls_have_real_translations() {
    use rustgis_assistant::{Error, Recovery};
    let mut keys = vec![
        "Weekly usage",
        "Open browser again",
        "Copy sign-in link",
        "Sign in to choose a model.",
        "Choose an account",
        "Diagnostics",
        "No models are available for this account.",
        "The reply stopped before completion; no actions were run.",
    ];
    let errors: Vec<_> = [
        Recovery::SignIn,
        Recovery::AccountChanged,
        Recovery::Consent,
        Recovery::Timeout,
        Recovery::Cancelled,
        Recovery::Auth,
        Recovery::Configuration,
        Recovery::Unsupported,
        Recovery::Ineligible,
        Recovery::Usage,
        Recovery::Later,
    ]
    .into_iter()
    .map(|r| Error::new(r, ""))
    .collect();
    keys.extend(errors.iter().map(Error::message));
    for l in &LANGUAGES[1..] {
        let (catalog, _) = parse(l.source);
        for key in &keys {
            let translated = catalog.get(*key).unwrap_or_else(|| panic!("{}: missing {key}", l.code));
            assert_ne!(translated, key, "{}: untranslated {key}", l.code);
        }
    }
}

#[test]
fn assistant_note_translations_are_real_when_available() {
    for l in &LANGUAGES[1..] {
        let (m, _) = parse(l.source);
        for template in rustgis_assistant::provider::NOTE_TEMPLATES {
            // AGENTS.md permits English fallback for new keys until the maintainer translates.
            if let Some(tr) = m.get(*template) {
                assert_ne!(tr, template, "{}: {template:?} isn't translated", l.code);
            }
        }
    }
}

#[test]
fn skills_controls_are_in_every_catalog() {
    let keys = [
        "Skills",
        "Built in",
        "Plugin",
        "Yours",
        "Shared",
        "Add skill…",
        "Skill (SKILL.md or .zip)",
        "Add folder…",
        "Added: {names}",
        "Skills for the AI Assistant: {names}",
    ];
    // Sentences, which no language leaves in English.
    let sentences = [
        "Your skills are kept in the skills folder of the app's settings folder. Shared skills (~/.agents/skills) are off until you turn them on.",
        "The assistant follows a skill's instructions when it uses it. Whatever they say, its commands that need your approval still ask.",
        "It has no code: nothing in it runs.",
    ];
    for l in &LANGUAGES[1..] {
        let (m, _) = parse(l.source);
        for key in keys.iter().chain(&sentences) {
            assert!(m.contains_key(*key), "{}: missing {key:?}", l.code);
        }
        for key in sentences {
            assert_ne!(m.get(key).map(String::as_str), Some(key), "{}: {key:?} isn't translated", l.code);
        }
    }
}

#[test]
fn letting_agents_control_rustgis_is_in_every_catalog() {
    let keys = [
        "Let your Agents Control RustGIS",
        "Agents on this computer that connect (Claude Code, Codex, Cursor and others) work on the map open here, with RustGIS's tools: they can open and save files and use the web. Scripts and plugin management stay off.",
        "Add this MCP server to your agent:",
        "Or run, for Claude Code or Codex:",
    ];
    for l in &LANGUAGES[1..] {
        let (m, _) = parse(l.source);
        for key in keys {
            let tr = m.get(key).unwrap_or_else(|| panic!("{}: missing {key:?}", l.code));
            assert_ne!(tr, key, "{}: {key:?} isn't translated", l.code);
        }
    }
}

#[test]
fn agents_on_this_computer_are_in_every_catalog() {
    let keys = [
        "An agent on this computer asks to:",
        "Always allow",
        "Let the assistant hand tasks to agents on this computer",
        "Found: {names}. An agent works on this map through RustGIS's tools and with its own (web, scripts, files) in a folder of its own, kept in Documents › RustGIS › Agent work. It asks you before each action.",
        "Approve agents' actions without asking",
        "Agent threads",
        "Waiting for your answer",
        "Idle",
        "{agent} was stopped by you.",
        "Agents then run commands, change files and use the web on this computer without asking you.",
    ];
    for l in &LANGUAGES[1..] {
        let (m, _) = parse(l.source);
        for key in keys {
            let tr = m.get(key).unwrap_or_else(|| panic!("{}: missing {key:?}", l.code));
            assert_ne!(tr, key, "{}: {key:?} isn't translated", l.code);
        }
    }
}

#[test]
fn plugin_permissions_are_in_every_catalog() {
    use rustgis_plugins::manifest::Permission;
    for l in &LANGUAGES[1..] {
        let (m, _) = parse(l.source);
        for p in [Permission::Edit, Permission::Files, Permission::Network] {
            let key = p.describe();
            let tr = m.get(key).unwrap_or_else(|| panic!("{}: missing {key:?}", l.code));
            assert_ne!(tr, key, "{}: {key:?} isn't translated", l.code);
        }
    }
}

#[test]
fn registry_process_plugin_notes_are_in_every_catalog() {
    for l in &LANGUAGES[1..] {
        let (m, _) = parse(l.source);
        for key in [
            "Not sandboxed",
            "Desktop and command line only",
            "{name} runs as a program on your computer: it needs the desktop app or rustgis-cli.",
            "Installing it downloads Python {version} and the packages it needs from the internet.",
        ] {
            let tr = m.get(key).unwrap_or_else(|| panic!("{}: missing {key:?}", l.code));
            assert_ne!(tr, key, "{}: {key:?} isn't translated", l.code);
        }
    }
}

#[test]
fn vn2000_translations_in_vietnamese_catalog() {
    let vi = Lang::from_code("vi").unwrap();
    assert_eq!(tr(vi, "Provinces:"), "Tỉnh thành:");
    assert_eq!(
        tr(
            vi,
            "Vietnam national coordinate system (VN-2000) using 3-degree Transverse Mercator with a 7-parameter Helmert shift to WGS 84."
        ),
        "Hệ tọa độ quốc gia Việt Nam (VN-2000) dùng phép chiếu Transverse Mercator 3 độ với phép chuyển đổi 7 tham số Helmert sang WGS 84."
    );
    assert_eq!(
        tr(vi, "Vietnam national coordinate system (VN-2000) UTM zone 48N (102°E–108°E, central meridian 105°E)."),
        "Hệ tọa độ quốc gia Việt Nam (VN-2000) múi chiếu UTM 48N (102°Đ–108°Đ, kinh tuyến trục 105°Đ)."
    );
    assert_eq!(
        tr(vi, "Vietnam national coordinate system (VN-2000) UTM zone 49N (108°E–114°E, central meridian 111°E)."),
        "Hệ tọa độ quốc gia Việt Nam (VN-2000) múi chiếu UTM 49N (108°Đ–114°Đ, kinh tuyến trục 111°Đ)."
    );
}

#[test]
fn contexts_keep_homographs_separate_and_report_duplicate_lines() {
    let (catalog, errors) = parse_catalog(
        "Open\tDefault\nverb\tOpen\tÖffnen\nstate\tOpen\tOffen\n\tOpen\tDuplicate\nverb\tOpen\tDuplicate\nx\tKey\tValue\textra\n",
    );
    assert_eq!(catalog[""]["Open"], "Default");
    assert_eq!(catalog["verb"]["Open"], "Öffnen");
    assert_eq!(catalog["state"]["Open"], "Offen");
    assert_eq!(errors.len(), 3);
    assert!(errors[0].starts_with("line 4: duplicate"));
    assert!(errors[1].starts_with("line 5: duplicate"));
    assert!(errors[2].starts_with("line 6: expected"));
}

#[test]
fn context_lookup_fallback_macros_and_formatted_values() {
    static INFO: LangInfo = LangInfo {
        code: "test",
        name: "Test",
        english: "Test",
        script: Script::Alphabetic,
        source: "Open\tDefault\nverb\tOpen\tÖffnen\nstate\tOpen\tOffen\ncount\t{n} open\t{n} offen\nrtl\tOpen\tباز RustGIS\n",
        catalog: OnceLock::new(),
        visual: OnceLock::new(),
    };
    let lang = Lang(&INFO);
    assert_eq!(tr_context(lang, "verb", "Open"), "Öffnen");
    assert_eq!(tr_context(lang, "state", "Open"), "Offen");
    assert_eq!(tr_context(lang, "unknown", "Open"), "Default");
    assert_eq!(tr_context(lang, "verb", "Missing"), "Missing");
    assert_eq!(tr_context(Lang::EN, "verb", "Open"), "Open");
    let previous = current();
    set_current(lang);
    // Restore the thread-local language even if an assertion panics.
    let result = std::panic::catch_unwind(|| {
        assert_eq!(tl!("Open", context = "verb"), "Öffnen");
        assert_eq!(tl_raw!("Open", context = "rtl"), "باز RustGIS");
        assert_eq!(tl!("Open", context = "rtl"), rtl::visual("باز RustGIS"));
        assert_eq!(fmt_context("count", "{n} open", &[("n", "{literal}")]), "{literal} offen");
    });
    set_current(previous);
    result.unwrap();
}
