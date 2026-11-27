//! The parts of a publication a call can leave out or set up (#3094): `"none"`, or an object of
//! the options its element takes elsewhere.

use super::*;

/// A north arrow's: when it is drawn, and its look as `layout.update` takes it.
const NORTH_ARROW_KEYS: &[&str] = &["show", "style", "fill", "outline"];

/// The options object `v` given as `key`, without its `null`s, which leave the default; a key
/// not in `keys` is refused.
fn options(key: &str, v: &Value, keys: &[&str]) -> Result<Option<Value>, String> {
    let Some(given) = v.as_object() else { return Ok(None) };
    if let Some(other) = given.keys().find(|k| !keys.contains(&k.as_str())) {
        return Err(format!("{key} takes no \"{other}\" (it reads {})", keys.join(", ")));
    }
    let given: serde_json::Map<String, Value> = given.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), v.clone())).collect();
    Ok(Some(Value::Object(given)))
}

/// The part `key` of `p`, `default` unless given: `None` for `"none"`, else `default` with what
/// `set` reads from its options.
fn part<T>(p: &Value, key: &str, keys: &[&str], default: T, set: impl FnOnce(&Value, T) -> Result<T, String>) -> Result<Option<T>, String> {
    let Some(v) = p.get(key).filter(|v| !v.is_null()) else { return Ok(Some(default)) };
    if v.as_str().is_some_and(|t| t.trim() == "none") {
        return Ok(None);
    }
    match options(key, v, keys)? {
        Some(o) => set(&o, default).map(Some),
        None => Err(format!("{key} is \"none\" or an object with any of {}", keys.join(", "))),
    }
}

/// The map frame's grid: `default` with the `"grid"` options over it, or none. A graticule given
/// no step keeps the default's, which the reference text says.
pub(super) fn publication_grid(p: &Value, default: Grid) -> Result<Option<Grid>, String> {
    let step = default.step;
    // A frame grid's options as `layout.addElement` takes them, without the keys that say which
    // element and frame it is, and `off`, which `"none"` says here.
    let keys: Vec<&str> = ADD_GRID_KEYS.iter().copied().filter(|k| !["layout", "kind", "frame", "off"].contains(k)).collect();
    part(p, "grid", &keys, default, |o, base| {
        let mut grid = grid_params(o, base)?;
        // The keys only the other kind of grid reads: a graticule draws degrees, a measured grid
        // its system's units.
        let (kind, unread): (&str, &[&str]) = match grid.kind {
            GridKind::Graticule => ("a graticule", &["crs", "short", "mgrs"]),
            GridKind::Measured => ("a measured grid", &["format", "hemisphere"]),
        };
        if let Some(key) = unread.iter().find(|key| o.get(**key).is_some()) {
            return Err(format!("grid takes no \"{key}\" for {kind}"));
        }
        if grid.kind == GridKind::Graticule && grid.step <= 0.0 {
            grid.step = step;
        }
        Ok(grid)
    })
}

/// The scale bar, its text `text_pt` points unless `"scaleBar"` says otherwise, or none. Its
/// frame is set once there is one.
pub(super) fn publication_scale_bar(p: &Value, text_pt: f64) -> Result<Option<ElementKind>, String> {
    let mut default = ElementKind::scale_bar(0);
    if let ElementKind::ScaleBar { text_size, .. } = &mut default {
        *text_size = text_pt;
    }
    // Its options as `layout.update` takes them.
    let keys: Vec<&str> = SCALE_BAR_LOOK_KEYS.iter().chain(&SCALE_BAR_KEYS).copied().collect();
    part(p, "scaleBar", &keys, default, |o, base| Ok(restyle(&base, o)?.unwrap_or(base)))
}

/// When a publication draws its north arrow, and the arrow: `Some(true)` always ("always"),
/// `Some(false)` never ("none"), `None` left to the extent ("auto", the default: none at a
/// continental span, #1379). An object gives its look, with "show" for when. Its frame is set once
/// there is one.
pub(super) fn publication_north_arrow(p: &Value) -> Result<(Option<bool>, ElementKind), String> {
    let refused = || format!("northArrow is \"auto\", \"always\" or \"none\", or an object with any of {}", NORTH_ARROW_KEYS.join(", "));
    let arrow = ElementKind::north_arrow(0);
    let Some(v) = p.get("northArrow").filter(|v| !v.is_null()) else { return Ok((None, arrow)) };
    if let Some(o) = options("northArrow", v, NORTH_ARROW_KEYS)? {
        let show = match o.get("show").map(|v| v.as_str().map(str::trim)) {
            None | Some(Some("auto")) => None,
            Some(Some("always")) => Some(true),
            Some(_) => return Err("northArrow's show is \"auto\" or \"always\"; \"northArrow\": \"none\" leaves it out".into()),
        };
        let arrow = restyle(&arrow, &o)?.unwrap_or(arrow);
        return Ok((show, arrow));
    }
    match v.as_str().map(str::trim) {
        Some("auto") => Ok((None, arrow)),
        Some("always") => Ok((Some(true), arrow)),
        Some("none") => Ok((Some(false), arrow)),
        _ => Err(refused()),
    }
}
