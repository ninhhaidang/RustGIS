//! Finding a coordinate system by name or code in terrio-crs's catalog, for the coordinate system
//! pickers: `lv95`, `switzerland`, `2056`, `EPSG:2056` and `epsg lv95` all find EPSG:2056.

use std::collections::HashMap;
use std::sync::OnceLock;

/// What a coordinate system found by [`search`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// In degrees of longitude and latitude (EPSG:4326).
    Geographic,
    /// On a map projection, in metres or feet (EPSG:2056).
    Projected,
}

/// A coordinate system of the catalog.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// Its code as RustGIS reads it: `EPSG:2056`, `ESRI:102033`.
    pub code: String,
    /// Its name: `CH1903+ / LV95`.
    pub name: String,
    pub kind: Kind,
    /// The name of its area of use: `Liechtenstein; Switzerland.`
    pub area: Option<String>,
    /// Its projection's method, for a projected system: `Transverse Mercator`.
    pub method: Option<String>,
}

/// One catalog entry, with what a search compares in lower case.
struct Entry {
    found: Found,
    authority: &'static str,
    number: u32,
    /// The code's digits.
    digits: String,
    /// `epsg:2056`.
    key: String,
    /// The name in lower case.
    search: String,
    /// Its letters and digits alone: `wgs84utmzone32n`, for words typed without the spaces.
    compact: String,
    /// The name's words in lower case: `ch1903`, `lv95` for `CH1903+ / LV95`.
    tokens: Vec<String>,
    /// Those words with a space between and around each: ` ch1903 lv95 `.
    phrase: String,
    /// The area of use's name in lower case.
    area: String,
    /// The places the area of use names first, in lower case: `liechtenstein`, `switzerland` for
    /// `Liechtenstein; Switzerland.`, `usa` for `USA - Indiana - Dearborn, Ohio and Switzerland
    /// counties`.
    places: Vec<String>,
    /// The projection's method in lower case.
    method: String,
}

/// The current EPSG systems in degrees or projected, and the projected ESRI ones (the ESRI codes
/// RustGIS reads), read from the catalog once a run.
fn entries() -> &'static [Entry] {
    static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        let mut out = Vec::new();
        for (authority, kinds) in [
            ("EPSG", vec![terrio_crs::CrsKind::Geographic2D, terrio_crs::CrsKind::Projected]),
            ("ESRI", vec![terrio_crs::CrsKind::Projected]),
        ] {
            let query = terrio_crs::CrsQuery { authorities: vec![authority.into()], kinds, ..Default::default() };
            for e in terrio_crs::list_crs(&query) {
                let kind = if e.kind == terrio_crs::CrsKind::Projected { Kind::Projected } else { Kind::Geographic };
                let code = format!("{}:{}", e.authority, e.code);
                let lower = e.name.to_lowercase();
                let tokens = words_of(&lower);
                out.push(Entry {
                    authority: e.authority,
                    number: e.code,
                    digits: e.code.to_string(),
                    key: code.to_ascii_lowercase(),
                    phrase: format!(" {} ", tokens.join(" ")),
                    compact: tokens.concat(),
                    tokens,
                    search: lower,
                    area: e.area_name.as_deref().unwrap_or_default().to_lowercase(),
                    places: places(e.area_name.as_deref().unwrap_or_default()),
                    method: e.projection_method.as_deref().unwrap_or_default().to_lowercase(),
                    found: Found { code, name: e.name, kind, area: e.area_name, method: e.projection_method },
                });
            }
        }
        out
    })
}

/// Where each entry is in [`entries`], by its `epsg:2056` key.
fn index() -> &'static HashMap<String, usize> {
    static INDEX: OnceLock<HashMap<String, usize>> = OnceLock::new();
    INDEX.get_or_init(|| entries().iter().enumerate().map(|(i, e)| (e.key.clone(), i)).collect())
}

/// The words of `text`, without the marks between them: `ch1903`, `lv95` for `ch1903+ / lv95`.
fn words_of(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// The places an area of use's name lists first: each `;`-separated part up to its first ` - `,
/// until a part goes on with ` - `, whose own `;` list names places within it (`United States
/// (USA) - Indiana - counties of Adams; …; Switzerland; …` is the United States only).
fn places(area: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in area.split(';') {
        let (place, within) = match part.split_once(" - ") {
            Some((place, _)) => (place, true),
            None => (part, false),
        };
        let place = place.trim().trim_end_matches('.').trim().to_lowercase();
        if !place.is_empty() {
            out.push(place);
        }
        if within {
            break;
        }
    }
    out
}

/// How well `e` matches the query, lower first, and how many words were found only inside a word
/// of the name (`93` in `ISN93`, not beginning `Lambert-93`'s); `None` when it doesn't match. Every
/// word must be in the name, the projection's method or the area of use's name, begin the code or
/// `authority:code`, or be the authority.
fn rank(e: &Entry, words: &[String], whole: &str) -> Option<(u8, usize)> {
    let mut code_word = false;
    let mut beside_name = false;
    let mut inside = 0;
    for w in words {
        // A number begins the code, or `epsg:2` its key; a letter alone (`e`) begins no code.
        let in_code = (w.bytes().all(|b| b.is_ascii_digit()) && e.digits.starts_with(w.as_str()))
            || (w.contains(':') && e.key.starts_with(w.as_str()));
        // `lambert93`, `wgs84` are words of the name typed without the space or mark between.
        let glued = || w.len() > 3 && e.compact.contains(&w.chars().filter(|c| c.is_alphanumeric()).collect::<String>());
        let in_name = in_code || e.search.contains(w.as_str()) || w.eq_ignore_ascii_case(e.authority) || glued();
        if !in_name && !e.method.contains(w.as_str()) && !e.area.contains(w.as_str()) {
            return None;
        }
        // A word's own words (`ch1903+` is `ch1903`) each begin one of the name's.
        let starts_a_word =
            w.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).all(|p| e.tokens.iter().any(|t| t.starts_with(p)));
        // Among words, a number beginning only the code (`32` of 32601 in `utm 32`) is inside too.
        if in_name && !starts_a_word && (words.len() > 1 || !in_code) {
            inside += 1;
        }
        code_word |= in_code;
        beside_name |= !in_name;
    }
    let r = if whole == e.digits || whole == e.key {
        0
    } else if code_word && words.len() == 1 {
        // A code begun (`205`); a number among words (`lambert 93`) is a word of the name.
        1
    } else if e.places.iter().any(|p| p == whole) {
        // A country typed: the systems for that country before those naming it otherwise.
        2
    } else if beside_name {
        5
    } else if e.search.starts_with(whole) {
        3
    } else {
        4
    };
    Some((r, inside))
}

/// The text searched for, in lower case, and its words.
fn query(text: &str) -> (String, Vec<String>) {
    // No name is longer: a pasted text past it is searched by its start, so a frame stays short.
    let text = text.get(..text.char_indices().nth(200).map_or(text.len(), |(i, _)| i)).unwrap_or(text);
    // `EPSG: 2056` and `epsg : 2056` are one code.
    let mut whole = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    for spaced in [" :", ": "] {
        whole = whole.replace(spaced, ":");
    }
    // `EPSG 2056` is `EPSG:2056` too.
    let mut joined: Vec<String> = Vec::new();
    for w in whole.split_whitespace() {
        match joined.last_mut() {
            Some(last) if (last == "epsg" || last == "esri") && w.bytes().all(|b| b.is_ascii_digit()) => {
                *last = format!("{last}:{w}");
            }
            _ => joined.push(w.to_string()),
        }
    }
    let whole = joined.join(" ");
    // A word without a letter or digit (`/`, `+`) says nothing of a system.
    let words = joined.into_iter().filter(|w| w.chars().any(char::is_alphanumeric)).collect();
    (whole, words)
}

/// Whether `e` is on WGS 84 or ETRS89, the datums most data comes in today.
fn current_datum(e: &Entry) -> bool {
    ["wgs 84", "etrs89"].iter().any(|d| e.search.strip_prefix(d).is_some_and(|rest| rest.is_empty() || rest.starts_with(" /")))
}

/// The coordinate systems matching `text` (words of the name, the projection's method or the area
/// of use, a code or `EPSG:code`, in any case), best first: the code typed, codes beginning with it,
/// names beginning with the text, other names, then systems matched by their method or area of use;
/// within each, names with every word beginning one of theirs first, then those with the words in
/// a row, WGS 84 and ETRS89, EPSG before ESRI, shorter names, and by code. At most `limit`, and only projected ones when
/// `projected_only` (a map is drawn in one). Nothing for blank text.
pub fn search(text: &str, projected_only: bool, limit: usize) -> Vec<Found> {
    let (whole, words) = query(text);
    if words.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<((u8, usize), &Entry)> = entries()
        .iter()
        .filter(|e| !projected_only || e.found.kind == Kind::Projected)
        .filter_map(|e| rank(e, &words, &whole).map(|r| (r, e)))
        .collect();
    // Among equal name matches, the systems on today's global datums first (`utm 32n wgs` is WGS
    // 84's zone before WGS 72's), and the plain system before its variants (`ETRS89 / UTM zone 32N`
    // before `… (N-E)`); codes typed stay in code order.
    // The words typed in a row in the name (`lambert 93` in `… / Lambert-93`) before apart.
    let typed = format!(" {} ", words_of(&whole).join(" "));
    hits.sort_by_cached_key(|((r, inside), e)| {
        (
            *r,
            *inside,
            !e.phrase.contains(&typed),
            !current_datum(e),
            e.authority != "EPSG",
            if *r >= 2 { e.search.len() } else { 0 },
            e.number,
        )
    });
    hits.into_iter().take(limit).map(|(_, e)| e.found.clone()).collect()
}

/// The catalog's entry for `code` (`EPSG:2056`, `esri:102033`, `2056`), if it lists it.
pub fn lookup(code: &str) -> Option<&'static Found> {
    lookup_entry(code).map(|e| &e.found)
}

fn lookup_entry(code: &str) -> Option<&'static Entry> {
    let lower = code.to_ascii_lowercase();
    let mut parts = lower.split(|c: char| c == ':' || c.is_whitespace()).filter(|p| !p.is_empty());
    let found = |key: String| index().get(&key).map(|&i| &entries()[i]);
    match (parts.next(), parts.next(), parts.next()) {
        // `2056` is EPSG's; a number RustGIS doesn't read as EPSG's (as `102100`, Web Mercator, is)
        // and ESRI lists (`102033`) is ESRI's, as [`super::crs::Crs::parse`] reads it.
        (Some(n), None, None) if n.bytes().all(|b| b.is_ascii_digit()) => {
            // `02056` is 2056.
            let n = n.parse::<u32>().ok()?;
            found(format!("epsg:{n}")).or_else(|| super::crs::bare_number_is_esri(n).then(|| found(format!("esri:{n}"))).flatten())
        }
        // `EPSG:2056`, `epsg : 2056`, `EPSG 2056`.
        (Some(authority), Some(n), None) => {
            // `EPSG:02056` is EPSG:2056 too.
            let n = if n.bytes().all(|b| b.is_ascii_digit()) { n.parse::<u32>().ok()?.to_string() } else { n.to_string() };
            found(format!("{authority}:{n}"))
        }
        _ => None,
    }
}

/// Whether the system `code` matches `text` as [`search`] matches it: by its code, the words of
/// its name, its projection's method or its area of use.
pub fn matches(code: &str, text: &str) -> bool {
    let (whole, words) = query(text);
    !words.is_empty() && lookup_entry(code).is_some_and(|e| rank(e, &words, &whole).is_some())
}

/// The catalog's name of `code` (`EPSG:2056`, `esri:102033`, `2056`), if it lists it.
pub fn name_of(code: &str) -> Option<&'static str> {
    lookup(code).map(|f| f.name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_system_is_found_by_its_code_and_by_words_of_its_name() {
        for text in ["2056", "EPSG:2056", "epsg: 2056", "lv95", "CH1903+ LV95", "epsg lv95"] {
            let found = search(text, false, 50);
            assert!(found.iter().any(|f| f.code == "EPSG:2056" && f.name == "CH1903+ / LV95"), "{text}: {found:?}");
        }
        // The code typed comes first.
        assert_eq!(search("2056", false, 5)[0].code, "EPSG:2056");
        assert_eq!(search("esri:102033", false, 5)[0].code, "ESRI:102033");
    }

    #[test]
    fn a_map_is_offered_only_projected_systems() {
        assert_eq!(search("4326", false, 1)[0].kind, Kind::Geographic);
        assert!(search("wgs 84", true, 500).iter().all(|f| f.kind == Kind::Projected));
        assert!(!search("4326", true, 500).iter().any(|f| f.code == "EPSG:4326"));
    }

    #[test]
    fn blank_or_unknown_text_finds_nothing() {
        assert!(search("  ", false, 10).is_empty());
        assert!(search("no such system anywhere", false, 10).is_empty());
        // A country finds the systems used there, after those it names.
        let swiss = search("switzerland", true, 500);
        assert!(swiss.iter().any(|f| f.code == "EPSG:2056"), "{swiss:?}");
        // Switzerland's own before Switzerland County, Indiana (EPSG:7285).
        let at = |code: &str| swiss.iter().position(|f| f.code == code);
        assert!(at("EPSG:2056") < at("EPSG:7285"), "{swiss:?}");
        // Every system for Switzerland before those of Indiana's Switzerland County (EPSG:26773).
        let (bern, indiana) = (at("EPSG:21780"), at("EPSG:26773"));
        assert!(bern.is_some() && indiana.is_some() && bern < indiana, "{swiss:?}");
        assert_eq!(places("Liechtenstein; Switzerland."), ["liechtenstein", "switzerland"]);
        assert_eq!(places("United States (USA) - Indiana - counties of Adams; Switzerland; Union."), ["united states (usa)"]);
        // So does a projection's method, with the area: Swiss Oblique Mercator.
        let oblique = search("oblique mercator switzerland", true, 50);
        assert!(oblique.iter().any(|f| f.code == "EPSG:2056"), "{oblique:?}");
        assert!(search("lambert azimuthal", true, 500).iter().any(|f| f.code == "EPSG:3035"));
        assert_eq!(search("utm", false, 7).len(), 7, "the limit holds");
        // WGS 84's zone before WGS 72's (32232), ETRS89's before ED50's (23032).
        assert_eq!(search("utm 32n wgs", true, 5)[0].code, "EPSG:32632");
        assert_eq!(search("utm 32n", true, 5)[0].code, "EPSG:25832");
        // A number among words is a word of the name, best where it begins one: Lambert-93, not
        // EPSG:9311 or ISN93.
        assert_eq!(search("lambert 93", true, 5)[0].code, "EPSG:2154");
        assert_eq!(search("utm 32", true, 5)[0].code, "EPSG:25832");
        // Spaces around the colon, and marks alone, change nothing.
        assert_eq!(search("epsg : 2056", false, 5)[0].code, "EPSG:2056");
        assert_eq!(search("EPSG:  2056", false, 5)[0].code, "EPSG:2056");
        assert_eq!(search("EPSG 3263", false, 5)[0].code, "EPSG:3263");
        assert_eq!(search("esri 102033", false, 5)[0].code, "ESRI:102033");
        assert!(search("/ +", false, 5).is_empty());
        // Words typed without their spaces.
        assert_eq!(search("lambert93", true, 5)[0].code, "EPSG:2154");
        assert_eq!(search("wgs84 utm 32n", true, 5)[0].code, "EPSG:32632");
        // A letter alone begins no code.
        assert!(search("e", false, 60).iter().all(|f| f.name.to_lowercase().contains('e')
            || f.area.as_deref().unwrap_or_default().to_lowercase().contains('e')
            || f.method.as_deref().unwrap_or_default().to_lowercase().contains('e')));
        assert!(matches("EPSG:2056", "switzerland") && matches("EPSG:2056", "oblique mercator"));
        assert!(!matches("EPSG:2056", "france") && !matches("EPSG:2056", " "));
        assert_eq!(search("+ lv95", false, 5)[0].code, "EPSG:2056");
    }

    #[test]
    fn a_code_is_named_from_the_catalog() {
        assert_eq!(name_of("EPSG:2056"), Some("CH1903+ / LV95"));
        assert_eq!(name_of("2056"), Some("CH1903+ / LV95"));
        assert_eq!(name_of("epsg:4326"), Some("WGS 84"));
        assert_eq!(name_of("vn2000tm3"), None);
        assert_eq!(lookup("4326").map(|f| f.kind), Some(Kind::Geographic));
        for spelt in ["EPSG 2056", "epsg : 2056", " EPSG:2056 "] {
            assert_eq!(lookup(spelt).map(|f| f.code.as_str()), Some("EPSG:2056"), "{spelt}");
        }
        assert_eq!(lookup("102033").map(|f| f.code.as_str()), Some("ESRI:102033"));
        // Web Mercator's old number stays EPSG's, as the parser reads it.
        assert_eq!(lookup("102100"), None);
        assert_eq!(lookup("0102033").map(|f| f.code.as_str()), Some("ESRI:102033"));
        assert_eq!(lookup("EPSG:02056").map(|f| f.code.as_str()), Some("EPSG:2056"));
        assert_eq!(lookup("EPSG:2056 x"), None);
    }
}
