//! The corners a publication layout's surrounds take, and favourite arrangements.

use super::*;

/// Where an arrangement puts each surround of a publication layout: the corner the locator, the
/// north arrow, the legend and the scale bar are each at home in (0 top left, 1 top right, 2 bottom
/// left, 3 bottom right), all four different (#1379).
pub type Homes = [usize; 4];

/// The arrangements by name. `classic` is the one layouts had before there was a choice.
pub const ARRANGEMENTS: [(&str, Homes); 4] =
    [("classic", [0, 1, 2, 3]), ("mirrored", [1, 0, 3, 2]), ("legendRight", [0, 1, 3, 2]), ("legendTop", [2, 1, 0, 3])];

/// The keys of an arrangement given surround by surround, in [`Homes`]'s order.
pub(in crate::cmd::layout) const ARRANGEMENT_KEYS: [&str; 4] = ["locator", "northArrow", "legend", "scaleBar"];

/// The corners by name, in [`Homes`]'s numbering.
pub(in crate::cmd::layout) const CORNER_NAMES: [&str; 4] = ["top-left", "top-right", "bottom-left", "bottom-right"];

/// A surround's place in [`Homes`].
pub(in crate::cmd::layout) fn home_slot(kind: Surround) -> usize {
    match kind {
        Surround::Locator => 0,
        Surround::Arrow => 1,
        Surround::Legend => 2,
        Surround::Scale => 3,
    }
}

/// At most this many arrangements are saved by name, each name at most this many characters
/// (#1379).
pub const MAX_SAVED_ARRANGEMENTS: usize = 20;
pub const MAX_ARRANGEMENT_NAME: usize = 40;

/// Names an arrangement can't be saved under: those `"arrangement"` reads otherwise, and those
/// the answer gives.
pub(in crate::cmd::layout) const RESERVED_ARRANGEMENT_NAMES: [&str; 4] = ["best", "favourite", "custom", "veil"];

/// Two arrangement names the same but for case (beyond ASCII too).
pub(in crate::cmd::layout) fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// The arrangements saved by name (`layout.arrangement {"save": …}`), each its name and corners;
/// an entry that isn't an arrangement is passed over.
pub(in crate::cmd::layout) fn saved_arrangements(saved: &serde_json::Map<String, Value>) -> impl Iterator<Item = (&String, Homes)> {
    let none = serde_json::Map::new();
    saved
        .iter()
        .filter(|(_, corners)| corners.is_object())
        .filter_map(move |(name, corners)| parse_arrangement(corners, &none).ok().map(|h| (name, h)))
}

/// An arrangement: one of [`ARRANGEMENTS`] or of the `saved` ones by name, or a corner for each
/// surround (`{"locator": "top-left", "northArrow": "top-right", "legend": "bottom-left",
/// "scaleBar": "bottom-right"}`, every corner once). `Value::Null` (none given) is `classic`.
pub fn parse_arrangement(v: &Value, saved: &serde_json::Map<String, Value>) -> Result<Homes, String> {
    let names = || {
        let saved = saved_arrangements(saved).map(|(n, _)| n.as_str());
        ARRANGEMENTS.iter().map(|(n, _)| *n).chain(saved).map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ")
    };
    match v {
        Value::Null => Ok(ARRANGEMENTS[0].1),
        Value::String(name) => arrangement_named(name, saved)
            .map(|(_, h)| h)
            .ok_or_else(|| format!("arrangement is {}, or a corner for each surround, not \"{name}\"", names())),
        Value::Object(o) => {
            if let Some(k) = o.keys().find(|k| !ARRANGEMENT_KEYS.contains(&k.as_str())) {
                return Err(format!(
                    "an arrangement gives \"locator\", \"northArrow\", \"legend\" and \"scaleBar\" a corner each, not \"{k}\""
                ));
            }
            let mut homes = [0usize; 4];
            for (i, key) in ARRANGEMENT_KEYS.iter().enumerate() {
                let corner = o.get(*key).and_then(Value::as_str).map(str::trim).unwrap_or("");
                homes[i] = CORNER_NAMES
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case(corner))
                    .ok_or_else(|| format!("arrangement: \"{key}\" is \"top-left\", \"top-right\", \"bottom-left\" or \"bottom-right\""))?;
            }
            if (0..4).any(|i| homes[..i].contains(&homes[i])) {
                return Err("an arrangement puts each surround in a corner of its own".into());
            }
            Ok(homes)
        }
        _ => Err(format!("arrangement is {}, or a corner for each surround", names())),
    }
}

/// The area (mm²) the rectangles cover twice, pair by pair.
pub(crate) fn overlap_area(rects: &[Rect]) -> f64 {
    let mut total = 0.0;
    for (i, a) in rects.iter().enumerate() {
        for b in &rects[i + 1..] {
            let (w, h) = ((a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0), (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1));
            total += w.max(0.0) * h.max(0.0);
        }
    }
    total
}

/// Which arrangement `best` takes, from each one's (area moved, area left over the data) in mm², in
/// the order tried (#1379): of those moving within a square millimetre of the least, the one
/// leaving the least over the data by more than a square millimetre, the first tried on a tie.
/// Both bands are measured from the best value, so they don't add up across tries. `None` when no
/// score is a number.
pub(crate) fn pick_arrangement(scores: &[(f64, f64)]) -> Option<usize> {
    let least = scores.iter().map(|s| s.0).fold(f64::INFINITY, f64::min);
    let near: Vec<usize> = (0..scores.len()).filter(|&i| scores[i].0 <= least + 1.0).collect();
    let fewest = near.iter().map(|&i| scores[i].1).fold(f64::INFINITY, f64::min);
    near.into_iter().find(|&i| scores[i].1 <= fewest + 1.0)
}

/// The name of an arrangement, or `"custom"` for one given corner by corner.
pub(in crate::cmd::layout) fn arrangement_name(homes: Homes) -> &'static str {
    ARRANGEMENTS.iter().find(|(_, h)| *h == homes).map_or("custom", |(n, _)| n)
}

/// A built-in or saved arrangement by `name` (case aside): its name as it is kept, and its corners.
pub(in crate::cmd::layout) fn arrangement_named(name: &str, saved: &serde_json::Map<String, Value>) -> Option<(String, Homes)> {
    let name = name.trim();
    let built_in = ARRANGEMENTS.iter().find(|(n, _)| same_name(n, name)).map(|(n, h)| ((*n).to_string(), *h));
    built_in.or_else(|| saved_arrangements(saved).find(|(n, _)| same_name(n, name)).map(|(n, h)| (n.clone(), h)))
}

/// What an arrangement given as `v` is called in the answer: its name; given corner by corner,
/// the name of the built-in or saved one with those corners, else `"custom"`.
pub(in crate::cmd::layout) fn arrangement_label(v: &Value, homes: Homes, saved: &serde_json::Map<String, Value>) -> String {
    let by_corners = || {
        ARRANGEMENTS
            .iter()
            .find(|(_, h)| *h == homes)
            .map(|(n, _)| (*n).to_string())
            .or_else(|| saved_arrangements(saved).find(|(_, h)| *h == homes).map(|(n, _)| n.clone()))
            .unwrap_or_else(|| "custom".into())
    };
    match v {
        Value::String(name) => arrangement_named(name, saved).map_or_else(by_corners, |(n, _)| n),
        _ => by_corners(),
    }
}

/// The corners of an arrangement, by surround.
pub(in crate::cmd::layout) fn corners_of(homes: Homes) -> Value {
    Value::Object(ARRANGEMENT_KEYS.iter().zip(homes).map(|(k, h)| ((*k).to_string(), json!(CORNER_NAMES[h]))).collect())
}

/// Refuses a name an arrangement can't be saved under: blank, longer than
/// [`MAX_ARRANGEMENT_NAME`], with control characters, or one `"arrangement"` already reads.
pub(in crate::cmd::layout) fn check_arrangement_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.trim() != name || name.chars().count() > MAX_ARRANGEMENT_NAME || name.chars().any(char::is_control) {
        return Err(format!("an arrangement's name is 1 to {MAX_ARRANGEMENT_NAME} characters, without control characters"));
    }
    if ARRANGEMENTS.iter().map(|(n, _)| *n).chain(RESERVED_ARRANGEMENT_NAMES).any(|n| same_name(n, name)) {
        return Err(format!("\"{name}\" names an arrangement already: choose another name"));
    }
    Ok(())
}

/// Whether this version would use a saved arrangement `name` with `corners`: a name
/// `layout.arrangement` saves, and a corner for each surround.
pub fn usable_saved_arrangement(name: &str, corners: &Value) -> bool {
    check_arrangement_name(name).is_ok() && corners.is_object() && parse_arrangement(corners, &serde_json::Map::new()).is_ok()
}

/// The arrangements saved by name as the preferences keep them (a JSON object of names and
/// corners), and the entries passed over, as they were: not an arrangement this version reads, a
/// name `layout.arrangement` wouldn't save, a name twice (case aside), or past
/// [`MAX_SAVED_ARRANGEMENTS`]. `None` for text that isn't such an object.
pub fn read_saved_arrangements(text: &str) -> Option<(serde_json::Map<String, Value>, serde_json::Map<String, Value>)> {
    let Ok(Value::Object(entries)) = serde_json::from_str::<Value>(text) else { return None };
    let (mut kept, mut passed) = (serde_json::Map::new(), serde_json::Map::new());
    for (name, corners) in entries {
        let usable =
            usable_saved_arrangement(&name, &corners) && !kept.keys().any(|k| same_name(k, &name)) && kept.len() < MAX_SAVED_ARRANGEMENTS;
        if usable {
            kept.insert(name, corners);
        } else {
            passed.insert(name, corners);
        }
    }
    Some((kept, passed))
}

/// The arrangements `layout.publication` tries for `"arrangement": asked` (#1379), each by the name
/// its answer gives it: for `"best"` (or none) the favourite first, so it wins a tie, then the
/// built-in ones and those saved by name, one of each set of corners.
pub(crate) fn arrangements_to_try(s: &Session, asked: &Value) -> Result<Vec<(String, Homes)>, String> {
    let saved = &s.layout_arrangements;
    let favourite = parse_arrangement(&s.layout_favourite, saved).unwrap_or(ARRANGEMENTS[0].1);
    let favourite = (arrangement_label(&s.layout_favourite, favourite, saved), favourite);
    Ok(match asked {
        Value::String(a) if a.trim().eq_ignore_ascii_case("favourite") => vec![favourite],
        Value::Null => arrangements_to_try(s, &json!("best"))?,
        Value::String(a) if a.trim().eq_ignore_ascii_case("best") => {
            let built_in = ARRANGEMENTS.iter().map(|(n, h)| ((*n).to_string(), *h));
            let mut all = vec![favourite];
            for (name, homes) in built_in.chain(saved_arrangements(saved).map(|(n, h)| (n.clone(), h))) {
                if !all.iter().any(|(_, h)| *h == homes) {
                    all.push((name, homes));
                }
            }
            all
        }
        v => {
            let homes = parse_arrangement(v, saved)?;
            vec![(arrangement_label(v, homes, saved), homes)]
        }
    })
}

/// Sets or shows the favourite arrangement of publication layouts (#1379): the one
/// `"arrangement": "favourite"` asks for, and the first `"best"` tries (it wins a tie). Saves
/// arrangements by name, which `"arrangement"` then takes as it takes the built-in ones and
/// `"best"` tries too, and removes them. All kept with the app's preferences, not the project.
/// A removal, then a save, then the favourite; checked before anything changes.
pub(in crate::cmd::layout) fn arrangement(s: &mut Session, p: &Value) -> CmdResult {
    let text = |key: &str, what: &str| match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.trim().to_string())),
        Some(_) => Err(format!("{key} is {what}")),
    };
    let (remove, save) = (text("remove", "the name of a saved arrangement")?, text("save", "a name for \"arrangement\"")?);
    // An arrangement given with nothing to save it under would do nothing.
    if save.is_none() && p.get("arrangement").is_some_and(|v| !v.is_null()) {
        return Err("\"arrangement\" is saved under a name given as \"save\"; the favourite is set with \"favourite\"".into());
    }
    let mut saved = s.layout_arrangements.clone();
    let mut warnings: Vec<String> = Vec::new();
    let mut favourite = s.layout_favourite.clone();
    if let Some(name) = remove {
        let key = saved.keys().find(|k| same_name(k, &name)).cloned().ok_or_else(|| format!("no arrangement is saved as \"{name}\""))?;
        let corners = saved.remove(&key).unwrap_or(Value::Null);
        // A favourite named after it keeps its corners, unless this call gives another.
        let replaced = p.get("favourite").is_some_and(|v| !v.is_null());
        if !replaced && favourite.as_str().is_some_and(|f| same_name(f.trim(), &key)) {
            favourite = corners;
            warnings.push(format!("the favourite was the arrangement \"{key}\": it keeps its corners"));
        }
    }
    if let Some(name) = save {
        let homes = match p.get("arrangement") {
            None | Some(Value::Null) => return Err("save takes \"arrangement\": a name, or a corner for each surround".into()),
            Some(v) => parse_arrangement(v, &saved)?,
        };
        check_arrangement_name(&name)?;
        match saved.keys().find(|k| same_name(k, &name)).cloned() {
            Some(old) => {
                saved.remove(&old);
                warnings.push(format!("the arrangement saved as \"{old}\" is replaced"));
            }
            None if saved.len() >= MAX_SAVED_ARRANGEMENTS => {
                return Err(format!("at most {MAX_SAVED_ARRANGEMENTS} arrangements are saved: remove one first"));
            }
            None => {}
        }
        saved.insert(name, corners_of(homes));
    }
    if let Some(v) = p.get("favourite").filter(|v| !v.is_null()) {
        parse_arrangement(v, &saved)?;
        favourite = v.clone();
    }
    let homes = parse_arrangement(&favourite, &saved)?;
    (s.layout_arrangements, s.layout_favourite) = (saved, favourite);
    let names: Vec<&str> =
        ARRANGEMENTS.iter().map(|(n, _)| *n).chain(saved_arrangements(&s.layout_arrangements).map(|(n, _)| n.as_str())).collect();
    Ok(
        json!({"favourite": s.layout_favourite, "corners": corners_of(homes), "arrangements": names, "saved": s.layout_arrangements, "warnings": warnings}),
    )
}
