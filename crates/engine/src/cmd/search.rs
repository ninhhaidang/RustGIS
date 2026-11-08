//! The map's search box: one query matched against a coordinate (a latitude and longitude, or a
//! UTM or MGRS grid reference), the map's layers, its
//! bookmarks and the values in its layers' attribute tables. Places (geocoding) are the UI's,
//! asked for on Enter: the public servers don't allow searching as one types.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use rustgis_data::Value as Attr;
use rustgis_geom::{Coord, Extent, GridError};
use serde_json::{Value, json};

use super::{f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        // Not a view command: going to a feature selects it.
        C::new("map.search", "Search", "Map", search_cmd).inputs(search_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"query": "a latitude and longitude (40.7, -74.0; 40°42'N 74°0'W), a UTM position (33U 412345 5523456; 33 north 412345 5523456) or an MGRS reference (33UVP1234523456; 33U VP 12345 23456), a layer, a bookmark or a value in a layer's table", "limit"?: 1–50, "go"?: true (the first hit) | index}"#,
        ),
    ]
}

/// The fewest characters a query needs to search attribute values.
pub const MIN_FEATURE_QUERY: usize = 2;
/// Most matches listed for one layer's values.
const PER_LAYER: usize = 5;
/// Most rows of a layer whose values are searched; a larger layer's later rows aren't. The
/// search box searches as one types, on the frame, so this stays small enough to be quick.
const MAX_ROWS: usize = 50_000;
/// The most hits `map.search` lists unless told otherwise; the search box lists as many, so an
/// index it picks is one the command finds.
pub const LIMIT: usize = 30;
/// Most characters of a query, and of a value shown.
const MAX_CHARS: usize = 200;

/// Identifies a tile layer across stack reordering without relying on catalog metadata. It uses
/// temporal source metadata rather than the current URL, which changes with the time slider.
#[derive(Clone, Debug, PartialEq)]
pub struct TileIdentity {
    name: String,
    kind: rustgis_map::tiles::TileKind,
    /// Static URL, present only for non-dated sources.
    url: Option<String>,
    time_url: Option<String>,
    /// Digest of the per-date addresses, which stay put while the time slider moves. A dated cube
    /// lists thousands of them, too many to keep a copy of (so a digest, compared within a
    /// session, the only place an identity is used).
    dated_sources: u64,
    overlay: bool,
    bounds: Option<[f64; 4]>,
    layers: Vec<String>,
    style_layers: String,
}

impl TileIdentity {
    fn of(tile: &rustgis_map::tiles::TileLayer) -> Self {
        Self {
            name: tile.name.clone(),
            kind: tile.kind,
            url: (tile.time_url.is_none() && tile.time_urls.is_empty()).then(|| tile.url.clone()),
            time_url: tile.time_url.clone(),
            dated_sources: digest(&tile.time_urls),
            overlay: tile.overlay,
            bounds: tile.bounds,
            layers: tile.layers.clone(),
            style_layers: tile.style_layers.clone(),
        }
    }

    pub(crate) fn matches(&self, tile: &rustgis_map::tiles::TileLayer) -> bool {
        self.name == tile.name
            && self.kind == tile.kind
            && self.url.as_ref().is_none_or(|url| url == &tile.url)
            && self.time_url == tile.time_url
            && self.dated_sources == digest(&tile.time_urls)
            && self.overlay == tile.overlay
            && self.bounds == tile.bounds
            && self.layers == tile.layers
            && self.style_layers == tile.style_layers
    }
}

/// Digest of a layer's per-date addresses and dates, which stand in for the list itself: two
/// dated layers that share a name are told apart by their sources, not just by having some.
fn digest(time_urls: &[(f64, String)]) -> u64 {
    let mut h = DefaultHasher::new();
    time_urls.len().hash(&mut h);
    for (date, url) in time_urls {
        date.to_bits().hash(&mut h);
        url.hash(&mut h);
    }
    h.finish()
}

/// What a search found.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    /// A point typed as a latitude and longitude, a UTM position or an MGRS reference (the centre
    /// of the square it names).
    Coordinate { lon: f64, lat: f64 },
    /// A layer whose name matches.
    Layer { id: u32, name: String },
    /// A web tile layer whose name matches; `index` is the stack position it held when the search
    /// ran, which tells alike layers apart when a hit is picked.
    Tile { index: usize, identity: TileIdentity, name: String, overlay: bool },
    /// A bookmark whose name matches.
    Bookmark { name: String },
    /// A feature with a matching value.
    /// `name` is the feature's name, when the layer has a name field and it isn't the one matched.
    Feature { layer: u32, layer_name: String, row: usize, field: String, value: String, name: Option<String> },
}

impl Hit {
    /// The hit as `map.search` returns it.
    pub fn to_json(&self) -> Value {
        match self {
            Hit::Coordinate { lon, lat } => json!({"kind": "coordinate", "lon": lon, "lat": lat, "label": format_lat_lon(*lat, *lon)}),
            Hit::Layer { id, name } => json!({"kind": "layer", "layer": id, "label": name}),
            Hit::Tile { index, name, overlay, .. } => json!({"kind": "tile", "index": index, "label": name, "overlay": overlay}),
            Hit::Bookmark { name } => json!({"kind": "bookmark", "label": name}),
            Hit::Feature { layer, layer_name, row, field, value, name } => {
                json!({"kind": "feature", "layer": layer, "layerName": layer_name, "row": row, "field": field, "label": value, "name": name})
            }
        }
    }
}

/// The results of a search, and the layers whose values were searched only in part.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Results {
    pub hits: Vec<Hit>,
    pub partial: Vec<u32>,
    /// Why the query, written as a UTM position or an MGRS reference, names no place (a band
    /// that doesn't hold its northing, square letters the zone doesn't use, `S` that may mean
    /// south); `None` when it isn't written as one, or reads.
    pub grid_error: Option<GridError>,
}

/// Searches the active map for `query`: a coordinate first, then layers, bookmarks and features.
pub fn search(s: &Session, query: &str, limit: usize) -> Results {
    search_map(s.map(), query, limit)
}

/// Builds bounded hits without selecting one or opening tile metadata.
fn search_map(map: &rustgis_map::Map, query: &str, limit: usize) -> Results {
    let q: String = query.trim().chars().take(MAX_CHARS).collect();
    let mut out = Results::default();
    if q.is_empty() {
        return out;
    }
    // A query that is plainly a coordinate (a decimal point, a degree mark, a hemisphere, a UTM
    // zone and band) isn't looked for in the tables; two whole numbers (an id, years, a range)
    // may be either, and so may a bare 100 km MGRS square, which names Sentinel-2's tiles (33UVP);
    // an MGRS reference with digits is plainly a coordinate. UTM and MGRS are the Earth's grids,
    // so on another body's map they name no place.
    let mut plainly_coordinate = false;
    if let Some(c) = parse_lat_lon(&q) {
        out.hits.push(Hit::Coordinate { lon: c.x, lat: c.y });
        plainly_coordinate = q.chars().any(|c| matches!(c, '.' | '°' | 'º' | '\'' | '′' | '"' | '″') || c.is_alphabetic());
    } else if map.body().is_earth() {
        match read_grid_reference(&q) {
            Ok(c) => {
                out.hits.push(Hit::Coordinate { lon: c.x, lat: c.y });
                plainly_coordinate = rustgis_geom::parse_mgrs(&q).map_or(true, |m| m.digits > 0);
            }
            Err(GridError::Syntax) => {}
            Err(e) => out.grid_error = Some(e),
        }
    }
    let needle = q.to_lowercase();
    for l in &map.layers {
        if contains_ci(&l.name, &needle) {
            out.hits.push(Hit::Layer { id: l.id, name: l.name.clone() });
        }
    }
    for (index, t) in map.tiles.iter().enumerate() {
        if contains_ci(&t.name, &needle) {
            out.hits.push(Hit::Tile { index, identity: TileIdentity::of(t), name: t.name.clone(), overlay: t.overlay });
        }
    }
    for b in &map.bookmarks {
        if contains_ci(&b.name, &needle) {
            out.hits.push(Hit::Bookmark { name: b.name.clone() });
        }
    }
    if q.chars().count() >= MIN_FEATURE_QUERY && !plainly_coordinate {
        let number = q.parse::<f64>().ok().filter(|n| n.is_finite());
        for l in map.layers.iter().filter(|l| !l.is_raster() && !l.graphics) {
            // A field called name (or the like) ranks ahead of the others when they match as well.
            let name_fi = name_field(&l.dataset.fields);
            // The best few: an exact value, then one that starts with the query, then the rest;
            // each feature by its best-matching field.
            let mut best: Vec<(u8, u8, usize, usize)> = Vec::new();
            for (row, feat) in l.dataset.features.iter().enumerate().take(MAX_ROWS) {
                let mut mine: Option<(u8, u8, usize, usize)> = None;
                for (fi, v) in feat.attrs.iter().enumerate() {
                    let rank = match v {
                        Attr::Text(t) => rank_text(t, &needle),
                        Attr::Int(i) => number.filter(|n| *n == *i as f64).map(|_| 0),
                        Attr::Float(x) => number.filter(|n| n == x).map(|_| 0),
                        _ => None,
                    };
                    if let Some(rank) = rank {
                        let key = (rank, u8::from(Some(fi) != name_fi), row, fi);
                        if mine.is_none_or(|m| key < m) {
                            mine = Some(key);
                        }
                    }
                }
                best.extend(mine);
                if best.len() >= PER_LAYER * 8 {
                    best.sort();
                    best.truncate(PER_LAYER);
                    if best.iter().all(|b| b.0 == 0 && b.1 == 0) {
                        break;
                    }
                }
            }
            best.sort();
            best.truncate(PER_LAYER);
            if l.dataset.features.len() > MAX_ROWS {
                out.partial.push(l.id);
            }
            for (_, _, row, fi) in best {
                let Some(feat) = l.dataset.features.get(row) else { continue };
                let Some(v) = feat.attrs.get(fi) else { continue };
                let field = l.dataset.fields.get(fi).map(|f| f.display().to_string()).unwrap_or_default();
                let value: String = v.to_string().chars().take(MAX_CHARS).collect();
                let name = name_fi
                    .filter(|n| *n != fi)
                    .and_then(|n| feat.attrs.get(n))
                    .filter(|v| !matches!(v, Attr::Null))
                    .map(|v| v.to_string().chars().take(MAX_CHARS).collect());
                out.hits.push(Hit::Feature { layer: l.id, layer_name: l.name.clone(), row, field, value, name });
            }
        }
    }
    out.hits.truncate(limit.max(1));
    out
}

/// The field that names a layer's features: `name`, or one starting with it (`NAME_EN`,
/// `name_long`), or a title or label.
fn name_field(fields: &[rustgis_data::Field]) -> Option<usize> {
    let low: Vec<String> = fields.iter().map(|f| f.name.to_ascii_lowercase()).collect();
    low.iter()
        .position(|n| n == "name")
        .or_else(|| low.iter().position(|n| n.starts_with("name")))
        .or_else(|| low.iter().position(|n| matches!(n.as_str(), "title" | "label")))
}

/// 0 for the same text, 1 for one starting with the query, 2 for one containing it.
fn rank_text(t: &str, needle: &str) -> Option<u8> {
    if t.is_ascii() && needle.is_ascii() {
        // Lowercasing other text can change its length in bytes, so only here is this a bound.
        if t.len() < needle.len() {
            return None;
        }
        let (h, n) = (t.as_bytes(), needle.as_bytes());
        if h.eq_ignore_ascii_case(n) {
            return Some(0);
        }
        if h.get(..n.len()).is_some_and(|p| p.eq_ignore_ascii_case(n)) {
            return Some(1);
        }
        return h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n)).then_some(2);
    }
    let low = t.to_lowercase();
    if low == needle {
        Some(0)
    } else if low.starts_with(needle) {
        Some(1)
    } else if low.contains(needle) {
        Some(2)
    } else {
        None
    }
}

/// Whether `t` contains `needle` (already lowercase), ignoring case.
fn contains_ci(t: &str, needle: &str) -> bool {
    rank_text(t, needle).is_some()
}

/// Goes to a hit: centres a coordinate, zooms to a layer or a bookmark, selects a feature and
/// zooms to it.
pub fn go_to(s: &mut Session, hit: &Hit) -> CmdResult {
    match hit {
        Hit::Coordinate { lon, lat } => {
            s.zoom_to(&around(Coord::new(*lon, *lat)));
            Ok(Value::Null)
        }
        Hit::Layer { id, name } => {
            let e = s.map().layer(*id).map(|l| l.shown_extent()).ok_or_else(|| format!("no layer '{name}'"))?;
            s.active_layer = Some(*id);
            if !e.is_empty() {
                s.zoom_to(&e);
            }
            Ok(Value::Null)
        }
        Hit::Tile { index, identity, name, .. } => {
            let at = super::tiles::index_of_identity(s, identity, *index)
                .ok_or_else(|| format!("tile layer '{name}' has changed; search again"))?;
            super::tiles::zoom_to_index(s, at).map(|_| Value::Null)
        }
        Hit::Bookmark { name } => {
            let cam =
                s.map().bookmarks.iter().find(|b| b.name == *name).map(|b| b.camera).ok_or_else(|| format!("no bookmark '{name}'"))?;
            s.map_mut().camera = cam;
            s.record_extent();
            Ok(Value::Null)
        }
        Hit::Feature { layer, layer_name, row, .. } => {
            if s.map().layer(*layer).is_none_or(|l| *row >= l.dataset.features.len()) {
                return Err(format!("{layer_name} has changed; search again"));
            }
            s.active_layer = Some(*layer);
            super::select::set(s, *layer, vec![*row], super::select::Mode::New);
            let e = s
                .map()
                .layer(*layer)
                .and_then(|l| l.dataset.features.get(*row))
                .and_then(|f| f.geometry.as_ref())
                .map(|g| g.extent())
                .unwrap_or_default();
            // A feature without a shape (a table row) is still selected.
            if !e.is_empty() {
                s.zoom_to(&if e.width() == 0.0 && e.height() == 0.0 { around(e.center()) } else { e });
            }
            Ok(Value::Null)
        }
    }
}

/// A town-sized view around a point (about 1:150,000 on a screen).
fn around(c: Coord) -> Extent {
    Extent::new(c.x - 0.05, c.y - 0.04, c.x + 0.05, c.y + 0.04)
}

/// Resolves the command's actual bounded hit list and optional destination without going to it.
fn search_request(map: &rustgis_map::Map, p: &Value) -> Result<(Results, Option<usize>), String> {
    let query = ps(p, "query").map(str::trim).filter(|q| !q.is_empty()).ok_or("query is required")?;
    let limit = f(p, "limit").filter(|l| l.is_finite()).map(|l| l.clamp(1.0, 50.0) as usize).unwrap_or(LIMIT);
    let r = search_map(map, query, limit);
    let pick = match p.get("go") {
        None | Some(Value::Null | Value::Bool(false)) => None,
        Some(Value::Bool(true)) => Some(0),
        Some(v) => Some(
            v.as_u64()
                .or_else(|| v.as_f64().filter(|x| x.fract() == 0.0 && *x >= 0.0 && *x <= 1e6).map(|x| x as u64))
                .and_then(|n| usize::try_from(n).ok())
                .ok_or("go is true or the index of a hit (0 for the first)")?,
        ),
    };
    if let Some(i) = pick {
        r.hits.get(i).ok_or_else(|| match (r.hits.is_empty(), r.grid_error) {
            (true, Some(e)) => format!("nothing matches '{query}': as a grid reference, {e}"),
            (true, None) => format!("nothing matches '{query}'"),
            (false, _) => format!("no hit {i}"),
        })?;
    }
    Ok((r, pick))
}

/// Binds a chosen tile's raw route; other hits retain the ordinary layer context.
fn search_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let (hits, pick) = search_request(input.map(), p)?;
    match pick.and_then(|i| hits.hits.get(i)) {
        Some(Hit::Tile { index, .. }) => Ok(super::tiles::tile_inputs(input.map(), [*index])),
        _ => super::Inputs::single_layer(input, p),
    }
}

/// Only going to a tile hit can ask its coverage reader to open the selected source.
pub(crate) fn metadata_reach(input: &super::InputContext<'_>, p: &Value) -> crate::policy::Effects {
    let tile = search_request(input.map(), p).ok().and_then(|(hits, pick)| match pick.and_then(|i| hits.hits.get(i)) {
        Some(Hit::Tile { index, .. }) => input.map().tiles.get(*index),
        _ => None,
    });
    tile.map_or(crate::policy::Effects::NONE, super::tiles::coverage_reach)
}

/// Uses the same bounded hit choice as approval before changing the view or selection.
fn search_cmd(s: &mut Session, p: &Value) -> CmdResult {
    let (r, pick) = search_request(s.map(), p)?;
    if let Some(hit) = pick.and_then(|i| r.hits.get(i)) {
        go_to(s, hit)?;
    }
    let mut out = json!({"hits": r.hits.iter().map(Hit::to_json).collect::<Vec<_>>(), "partial": r.partial});
    if let Some(e) = r.grid_error {
        out["gridError"] = json!(e.to_string());
    }
    Ok(out)
}

/// A place typed as an MGRS reference (the centre of the square it names) or a UTM position, in
/// the forms [`rustgis_geom::parse_mgrs`] and [`rustgis_geom::parse_utm`] accept; `None` for
/// anything else, an ambiguous UTM zone letter or a reference that names no place.
pub fn parse_grid_reference(text: &str) -> Option<Coord> {
    read_grid_reference(text).ok()
}

/// As [`parse_grid_reference`], with why a reference was refused: [`GridError::Syntax`] for text
/// written as neither form, otherwise the reason of the form it was written in. A 100 km square
/// alone written with spaces (`5 Ave`, `4K TV`) is a number and a word as often as a reference,
/// so one that names no square is [`GridError::Syntax`] too; written as one word (`38TMJ`), it
/// says why.
pub fn read_grid_reference(text: &str) -> Result<Coord, GridError> {
    match rustgis_geom::parse_mgrs(text) {
        Ok(m) => m.centre(),
        Err(mgrs) => match rustgis_geom::parse_utm(text) {
            Err(GridError::Syntax) if spaced_square(text) => Err(GridError::Syntax),
            Err(GridError::Syntax) => Err(mgrs),
            utm => utm,
        },
    }
}

/// Whether `text` has spaces and no digits after its leading number: written so, an MGRS
/// reference is a 100 km square alone.
fn spaced_square(text: &str) -> bool {
    let text = text.trim();
    let after_zone = text.trim_start_matches(|c: char| c.is_ascii_digit());
    text.contains(char::is_whitespace) && !after_zone.contains(|c: char| c.is_ascii_digit())
}

/// `lat, lon` rounded to six decimals (about 0.1 m), without trailing zeros.
pub fn format_lat_lon(lat: f64, lon: f64) -> String {
    let r = |v: f64| {
        let t = format!("{v:.6}");
        let t = t.trim_end_matches('0').trim_end_matches('.');
        if t == "-0" { "0".to_string() } else { t.to_string() }
    };
    format!("{}, {}", r(lat), r(lon))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok {
    Num(f64),
    /// A mark after a number: ° (true), or ' or " (false).
    Unit(bool),
    Hemi(char),
    Comma,
}

fn tokens(text: &str) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    // A sign starts a number only after a space, a comma or a mark, not straight after a digit.
    let mut prev = ' ';
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() || c == '.' || (matches!(c, '-' | '+' | '−') && !prev.is_ascii_digit() && prev != '.') {
            let mut s = String::new();
            if matches!(c, '-' | '−') {
                s.push('-');
                chars.next();
            } else if c == '+' {
                chars.next();
            }
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() || d == '.' {
                    s.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(Tok::Num(s.parse::<f64>().ok().filter(|v| v.is_finite())?));
            prev = '0';
        } else if c.is_alphabetic() {
            let mut word = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_alphabetic() {
                    word.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            let up = word.to_ascii_uppercase();
            match up.as_str() {
                "N" | "S" | "E" | "W" => out.push(Tok::Hemi(up.chars().next()?)),
                _ => return None,
            }
            prev = 'a';
        } else {
            chars.next();
            prev = c;
            match c {
                '°' | 'º' => out.push(Tok::Unit(true)),
                '\'' | '′' | '’' | '"' | '″' | '”' => out.push(Tok::Unit(false)),
                ',' | ';' => out.push(Tok::Comma),
                c if c.is_whitespace() => {}
                _ => return None,
            }
        }
        if out.len() > 32 {
            return None;
        }
    }
    Some(out)
}

/// One axis: degrees, and maybe minutes and seconds; `None` for anything else.
fn axis(nums: &[f64]) -> Option<f64> {
    let (d, rest) = nums.split_first()?;
    if rest.len() > 2 || rest.iter().any(|v| !(0.0..60.0).contains(v)) {
        return None;
    }
    let m = rest.first().copied().unwrap_or(0.0);
    let sec = rest.get(1).copied().unwrap_or(0.0);
    let v = d.abs() + m / 60.0 + sec / 3600.0;
    Some(if d.is_sign_negative() { -v } else { v })
}

/// A latitude and longitude typed in decimal degrees (`40.7, -74.0`), degrees and minutes or
/// degrees, minutes and seconds (`40°42'46"N 74°0'21"W`), with N/S/E/W before or after each
/// number. Latitude comes first unless the hemispheres say otherwise, or the first number can
/// only be a longitude.
pub fn parse_lat_lon(text: &str) -> Option<Coord> {
    let toks = tokens(text)?;
    let hemis: Vec<char> = toks.iter().filter_map(|t| if let Tok::Hemi(c) = t { Some(*c) } else { None }).collect();
    let mut groups: Vec<(Vec<f64>, Option<char>)> = Vec::new();
    if !hemis.is_empty() {
        if hemis.len() != 2 {
            return None;
        }
        // A letter before the first number marks each axis where it starts; otherwise where it ends.
        let prefix = matches!(toks.iter().find(|t| !matches!(t, Tok::Comma)), Some(Tok::Hemi(_)));
        let mut cur: (Vec<f64>, Option<char>) = (Vec::new(), None);
        for t in &toks {
            match *t {
                Tok::Num(v) => cur.0.push(v),
                Tok::Hemi(h) if prefix => {
                    if !cur.0.is_empty() {
                        groups.push(std::mem::take(&mut cur));
                    }
                    cur.1 = Some(h);
                }
                Tok::Hemi(h) => {
                    cur.1 = Some(h);
                    groups.push(std::mem::take(&mut cur));
                }
                Tok::Unit(_) | Tok::Comma => {}
            }
        }
        if !cur.0.is_empty() || cur.1.is_some() {
            groups.push(cur);
        }
    } else if toks.contains(&Tok::Comma) {
        let mut cur = Vec::new();
        for t in &toks {
            match *t {
                Tok::Num(v) => cur.push(v),
                Tok::Comma => groups.push((std::mem::take(&mut cur), None)),
                _ => {}
            }
        }
        groups.push((cur, None));
    } else if toks.contains(&Tok::Unit(true)) {
        // Degrees, minutes and seconds with their marks but no comma: an axis starts at each
        // number marked as degrees.
        let mut cur: Vec<f64> = Vec::new();
        for (i, t) in toks.iter().enumerate() {
            if let Tok::Num(v) = *t {
                if toks.get(i + 1) == Some(&Tok::Unit(true)) && !cur.is_empty() {
                    groups.push((std::mem::take(&mut cur), None));
                }
                cur.push(v);
            }
        }
        groups.push((cur, None));
    } else {
        let nums: Vec<f64> = toks.iter().filter_map(|t| if let Tok::Num(v) = t { Some(*v) } else { None }).collect();
        if nums.len() != 2 {
            return None;
        }
        groups = nums.into_iter().map(|v| (vec![v], None)).collect();
    }
    let [(a, ha), (b, hb)] = <[_; 2]>::try_from(groups).ok()?;
    let (mut a, mut b) = (axis(&a)?, axis(&b)?);
    let sign = |v: f64, h: char| if matches!(h, 'S' | 'W') { -v.abs() } else { v };
    let (lat, lon) = match (ha, hb) {
        (Some(x), Some(y)) => {
            a = sign(a, x);
            b = sign(b, y);
            match (matches!(x, 'N' | 'S'), matches!(y, 'N' | 'S')) {
                (true, false) => (a, b),
                (false, true) => (b, a),
                _ => return None,
            }
        }
        (None, None) => {
            if a.abs() > 90.0 && b.abs() <= 90.0 {
                (b, a)
            } else {
                (a, b)
            }
        }
        _ => return None,
    };
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    Some(Coord::new(lon, lat))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ll(t: &str) -> Option<(f64, f64)> {
        parse_lat_lon(t).map(|c| ((c.y * 1e4).round() / 1e4, (c.x * 1e4).round() / 1e4))
    }

    #[test]
    fn coordinates_parse() {
        assert_eq!(ll("40.7, -74.0"), Some((40.7, -74.0)));
        assert_eq!(ll("40.7 -74.0"), Some((40.7, -74.0)));
        assert_eq!(ll("-122.4, 37.8"), Some((37.8, -122.4)));
        assert_eq!(ll("40.7N 74.0W"), Some((40.7, -74.0)));
        assert_eq!(ll("74.0 W, 40.7 N"), Some((40.7, -74.0)));
        assert_eq!(ll("N 40.7 W 74"), Some((40.7, -74.0)));
        assert_eq!(ll("40°42'46\"N 74°0'21\"W"), Some((40.7128, -74.0058)));
        assert_eq!(ll("40°42.767' -74°0.35'"), Some((40.7128, -74.0058)));
        assert_eq!(ll("40 42 46 N, 74 0 21 W"), Some((40.7128, -74.0058)));
        assert_eq!(ll("51.5074,-0.1278"), Some((51.5074, -0.1278)));
    }

    #[test]
    fn grid_references_are_found() {
        let s = Session::new(crate::sample::world_project());
        let near = |r: &Results, lon: f64, lat: f64| matches!(r.hits.first(), Some(Hit::Coordinate { lon: x, lat: y }) if (x - lon).abs() < 1e-4 && (y - lat).abs() < 1e-4);
        // GeographicLib's GeoConvert example, 33.3° N 44.4° E: 38n 444140.54 3684706.36,
        // 38SMB4414084706.
        for q in [
            "38north 444141 3684706",
            "38NORTH 444141 3684706",
            "38S 444141 3684706",
            "38s 444140.54, 3684706.36",
            "38 north 444141 3684706",
            "38N 444141 3684706",
            "38SMB4414084706",
            "38S MB 44140 84706",
            "38smb 44140 84706",
        ] {
            let r = search(&s, q, 50);
            assert!(near(&r, 44.4, 33.3), "{q}: {r:?}");
        }
        // The UTM readout of a southern place, zone and hemisphere, is found where it was read.
        let ba = Coord::new(-58.4, -34.6);
        let text = rustgis_geom::format_utm(ba).unwrap();
        assert!(text.starts_with("21south "), "{text}");
        assert!(near(&search(&s, &text, 50), ba.x, ba.y), "{text}");
        // A 100 km square is found at its centre.
        let r = search(&s, "38SMB", 50);
        let u = rustgis_geom::to_utm(match r.hits.first() {
            Some(Hit::Coordinate { lon, lat }) => Coord::new(*lon, *lat),
            other => panic!("{other:?}"),
        })
        .unwrap();
        assert!((u.easting - 450_000.0).abs() < 1e-3 && (u.northing - 3_650_000.0).abs() < 1e-3, "{u:?}");
        // A table holding tile names is searched for an MGRS reference, not for a UTM position.
        let mut s = s;
        s.run(
            "layer.addDataText",
            &json!({"fileName": "tiles.csv", "text": "tile,lon,lat\n38SMB,44.5,33\n38S 444141 3684706,44.4,33.3\n"}),
        )
        .unwrap();
        let r = search(&s, "38SMB", 50);
        assert!(matches!(r.hits.first(), Some(Hit::Coordinate { .. })), "{r:?}");
        assert!(r.hits.iter().any(|h| matches!(h, Hit::Feature { value, .. } if value == "38SMB")), "{r:?}");
        let r = search(&s, "38S 444141 3684706", 50);
        assert!(!r.hits.iter().any(|h| matches!(h, Hit::Feature { .. })), "{r:?}");
        // Only the bare 100 km square names a tile: a reference with digits is plainly a place.
        s.run("layer.addDataText", &json!({"fileName": "refs.csv", "text": "ref,lon,lat\n38SMB4414084706,44.4,33.3\n"})).unwrap();
        let r = search(&s, "38SMB4414084706", 50);
        assert!(matches!(r.hits.first(), Some(Hit::Coordinate { .. })), "{r:?}");
        assert!(!r.hits.iter().any(|h| matches!(h, Hit::Feature { .. })), "{r:?}");
        // The command goes there.
        s.run("map.search", &json!({"query": "38SMB4414084706", "go": true})).unwrap();
        let centre = s.map().camera.center_lonlat();
        assert!((centre.x - 44.4).abs() < 1e-3 && (centre.y - 33.3).abs() < 1e-3, "{centre:?}");
    }

    #[test]
    fn bad_grid_references_find_no_place() {
        let s = Session::new(crate::sample::world_project());
        for q in [
            // Band S with a northing outside it may be the southern hemisphere.
            "33S 412345 5523456",
            // The northing isn't in band T; the square letters aren't zone 38's; odd digits.
            "33T 412345 5523456",
            "38SAB4414084706",
            "38SMB441408470",
            "38TMB4414084706",
            // Polar (UPS) references aren't read.
            "38Z 444141 3684706",
            "38ZMB4414084706",
            "99X 1 2",
            "1C",
        ] {
            assert!(parse_grid_reference(q).is_none(), "{q}");
            assert!(!search(&s, q, 50).hits.iter().any(|h| matches!(h, Hit::Coordinate { .. })), "{q}");
        }
        // The reason a reference was refused comes with the results and the command's answer,
        // rather than reading as nothing found.
        for (q, why) in [
            ("33S 412345 5523456", GridError::Ambiguous),
            ("33T 412345 5523456", GridError::BandMismatch),
            ("38SAB4414084706", GridError::Square),
            ("38SMB441408470", GridError::Digits),
            ("38TMB4414084706", GridError::BandMismatch),
            ("38TMJ", GridError::BandMismatch),
            // A 100 km square written as one word, as a reference is, still says why.
            ("38SAB", GridError::Square),
            ("38zmb", GridError::Polar),
            ("38S AB 44140 84706", GridError::Square),
            ("38Z 444141 3684706", GridError::Polar),
            ("38ZMB4414084706", GridError::Polar),
            ("99X 1 2", GridError::Zone),
            // A bad zone is judged before a polar or unknown band.
            ("0AAA", GridError::Zone),
            ("61AVE", GridError::Zone),
            ("33U 1000000 5523456", GridError::OutsideZone),
        ] {
            assert_eq!(read_grid_reference(q), Err(why), "{q}");
            assert_eq!(search(&s, q, 50).grid_error, Some(why), "{q}");
        }
        // Text not written as a grid reference, and references that read, carry no reason.
        // A number and a three-letter word (a polar band, a square letter the zone doesn't use, I
        // or O, a zone past 60) is ordinary text, and so is a number past 60 before any words.
        for q in [
            "1C",
            "Paris",
            "Countries",
            "12 Main St",
            "2020",
            "40.7, -74.0",
            "38SMB",
            "38S 444141 3684706",
            "10 20",
            "5 Ave",
            "7 Bay",
            "3 Rds",
            "2 Oak",
            "99 Ave",
            "4K TV",
            "75 Main St",
            "99 Problems",
        ] {
            assert_eq!(search(&s, q, 50).grid_error, None, "{q}");
        }
        let mut s = s;
        let v = s.run("map.search", &json!({"query": "33S 412345 5523456"})).unwrap();
        assert!(v["gridError"].as_str().is_some_and(|e| e.contains("south")), "{v}");
        assert!(s.run("map.search", &json!({"query": "Paris"})).unwrap().get("gridError").is_none());
        let e = s.run("map.search", &json!({"query": "33S 412345 5523456", "go": true})).unwrap_err();
        assert!(e.contains("nothing matches") && e.contains("33 south"), "{e}");
        let e = s.run("map.search", &json!({"query": "Zzyzx Qq", "go": true})).unwrap_err();
        assert_eq!(e, "nothing matches 'Zzyzx Qq'");
        let e = s.run("map.search", &json!({"query": "5 Ave", "go": true})).unwrap_err();
        assert_eq!(e, "nothing matches '5 Ave'");
        // MGRS-looking garbage never panics.
        let mut r = 1u64;
        let alphabet: Vec<char> = "0123456789 ACHINOSUVXZacz.-,°\u{0}é٣".chars().collect();
        for _ in 0..5_000 {
            let mut q = String::new();
            for _ in 0..(r % 24) {
                r = r.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                q.push(alphabet[(r >> 33) as usize % alphabet.len()]);
            }
            let _ = parse_grid_reference(&q);
            let _ = parse_lat_lon(&q);
        }
    }

    /// UTM and MGRS are the Earth's grids: on another body's map they find no place (and give
    /// no reason), while a latitude and longitude still does.
    #[test]
    fn grid_references_are_the_earths() {
        let mut s = Session::new(crate::sample::world_project());
        s.run("map.body", &json!({"name": "mars", "basemap": false})).unwrap();
        for q in ["38SMB4414084706", "38S 444141 3684706", "33S 412345 5523456"] {
            let r = search(&s, q, 50);
            assert!(!r.hits.iter().any(|h| matches!(h, Hit::Coordinate { .. })), "{q}: {r:?}");
            assert_eq!(r.grid_error, None, "{q}");
        }
        assert!(matches!(search(&s, "33.3, 44.4", 50).hits.first(), Some(Hit::Coordinate { .. })));
        s.run("map.body", &json!({"name": "earth", "basemap": false})).unwrap();
        assert!(matches!(search(&s, "38SMB4414084706", 50).hits.first(), Some(Hit::Coordinate { .. })));
    }

    #[test]
    fn non_coordinates_are_refused() {
        for t in
            ["Paris", "New York", "95, 200", "40.7", "40.7N 74W 3E", "1 2 3", "40N 50S", "", "12-5", "40° 61' N 74 W", "Seattle 47 -122"]
        {
            assert_eq!(parse_lat_lon(t), None, "{t}");
        }
    }

    #[test]
    fn formats_round() {
        assert_eq!(format_lat_lon(40.7, -74.00000001), "40.7, -74");
        assert_eq!(format_lat_lon(-0.0000001, 2.5), "0, 2.5");
    }

    #[test]
    fn finds_layers_and_values() {
        let s = Session::new(crate::sample::world_project());
        let r = search(&s, "cities", 50);
        assert!(r.hits.iter().any(|h| matches!(h, Hit::Layer { name, .. } if name == "Cities")), "{r:?}");
        let r = search(&s, "Tokyo", 50);
        assert!(r.hits.iter().any(|h| matches!(h, Hit::Feature { value, .. } if value == "Tokyo")), "{r:?}");
        // A city in Togo is found by its country, and named.
        let r = search(&s, "togo", 50);
        assert!(r.hits.iter().any(|h| matches!(h, Hit::Feature { field, name: Some(_), .. } if field != "name")), "{r:?}");
        let r = search(&s, "48.85, 2.35", 50);
        assert_eq!(r.hits, vec![Hit::Coordinate { lon: 2.35, lat: 48.85 }]);
        // Two whole numbers may be a coordinate or values: both are listed.
        let r = search(&s, "10 20", 50);
        assert_eq!(r.hits.first(), Some(&Hit::Coordinate { lon: 20.0, lat: 10.0 }));
        // Lowercasing İ makes it longer, so a match isn't refused by its length.
        assert_eq!(rank_text("İstanbul", "i̇stanbul"), Some(0));
        assert!(search(&s, "   ", 50).hits.is_empty());
    }

    #[test]
    fn finds_tile_layers_without_bounds_or_visibility() {
        let mut s = Session::new(crate::Project::default());
        s.run(
            "map.addTiles",
            &json!({
                "url": "https://example.invalid/wms?BBOX={bbox-epsg-3857}",
                "name": "OSM-WMS", "overlay": true
            }),
        )
        .unwrap();
        s.run(
            "map.addTiles",
            &json!({
                "url": "https://example.invalid/{z}/{x}/{y}.png",
                "name": "OSM streets"
            }),
        )
        .unwrap();
        s.run("map.setTiles", &json!({"name": "OSM streets", "visible": false})).unwrap();
        let r = s.run("map.search", &json!({"query": "  oSm  "})).unwrap();
        assert_eq!(
            r["hits"],
            json!([
                {"kind": "tile", "index": 0, "label": "OSM-WMS", "overlay": true},
                {"kind": "tile", "index": 1, "label": "OSM streets", "overlay": false}
            ])
        );
        let r = s.run("map.search", &json!({"query": "OSM-WMS", "limit": 1})).unwrap();
        assert_eq!(r["hits"][0]["index"], 0);
        let before = s.map().camera;
        let e = s.run("map.search", &json!({"query": "OSM-WMS", "go": true})).unwrap_err();
        assert!(e.contains("doesn't say what it covers"), "{e}");
        assert_eq!(s.map().camera, before);
    }

    #[test]
    fn going_to_tile_hits_uses_their_bounds() {
        let mut s = Session::new(crate::Project::default());
        for (name, bounds) in [("Coverage west", [-20.0, -5.0, -10.0, 5.0]), ("Coverage east", [10.0, -5.0, 20.0, 5.0])] {
            s.run(
                "map.addTiles",
                &json!({
                    "url": format!("https://example.invalid/{}/{{z}}/{{x}}/{{y}}.png", name.replace(' ', "-")),
                    "name": name, "bounds": bounds, "overlay": true
                }),
            )
            .unwrap();
        }
        for (go, lon) in [(json!(true), 15.0), (json!(1), -15.0)] {
            s.run("map.search", &json!({"query": "Coverage", "go": go})).unwrap();
            let c = rustgis_geom::merc_to_lonlat(s.map().camera.center);
            assert!((c.x - lon).abs() < 1e-6 && c.y.abs() < 1e-6, "{c:?}");
            assert!(s.map().camera.res < 10_000.0, "{:?}", s.map().camera);
        }
    }

    #[test]
    fn retained_tile_hits_follow_reordering_and_reject_removed_layers() {
        let mut s = Session::new(crate::Project::default());
        for (name, bounds) in [("Coverage west", [-20.0, -5.0, -10.0, 5.0]), ("Coverage east", [10.0, -5.0, 20.0, 5.0])] {
            s.run(
                "map.addTiles",
                &json!({
                    "url": format!("https://example.invalid/{}/{{z}}/{{x}}/{{y}}.png", name.replace(' ', "-")),
                    "name": name, "bounds": bounds, "overlay": true
                }),
            )
            .unwrap();
        }
        let hit = search(&s, "Coverage east", LIMIT).hits.remove(0);
        s.run("map.setTiles", &json!({"name": "Coverage east", "move": "bottom"})).unwrap();
        go_to(&mut s, &hit).unwrap();
        let c = rustgis_geom::merc_to_lonlat(s.map().camera.center);
        assert!((c.x - 15.0).abs() < 1e-6, "{c:?}");
        s.run("map.removeTiles", &json!({"name": "Coverage east"})).unwrap();
        let e = go_to(&mut s, &hit).unwrap_err();
        assert!(e.contains("has changed; search again"), "{e}");
    }

    #[test]
    fn retained_tile_hit_resolves_past_the_limited_search_results() {
        let mut s = Session::new(crate::Project::default());
        for i in 0..rustgis_map::tiles::MAX_TILE_LAYERS - 1 {
            s.run(
                "map.addTiles",
                &json!({
                    "url": format!("https://example.invalid/filler-{i}/{{z}}/{{x}}/{{y}}.png"),
                    "name": format!("Coverage filler {i}"), "overlay": true,
                    "bounds": [-5.0, -5.0, 5.0, 5.0]
                }),
            )
            .unwrap();
        }
        s.run(
            "map.addTiles",
            &json!({
                "url": "https://example.invalid/target/{z}/{x}/{y}.png",
                "name": "Coverage target", "overlay": true,
                "bounds": [10.0, -5.0, 20.0, 5.0]
            }),
        )
        .unwrap();
        let hit = search(&s, "Coverage", LIMIT).hits.remove(0);
        s.run("map.setTiles", &json!({"name": "Coverage target", "move": "bottom"})).unwrap();
        let Hit::Tile { index, identity, .. } = &hit else {
            panic!("expected tile hit");
        };
        assert_eq!(super::super::tiles::index_of_identity(&s, identity, *index), Some(rustgis_map::tiles::MAX_TILE_LAYERS - 1));
        assert!(!search(&s, "Coverage", LIMIT).hits.iter().any(|h| matches!(h, Hit::Tile { name, .. } if name == "Coverage target")));
        go_to(&mut s, &hit).unwrap();
        let c = rustgis_geom::merc_to_lonlat(s.map().camera.center);
        assert!((c.x - 15.0).abs() < 1e-6, "{c:?}");
    }

    #[test]
    fn retained_dated_tile_hits_survive_time_url_changes() {
        let mut s = Session::new(crate::Project::default());
        s.run(
            "map.addTiles",
            &json!({
                "url": "https://example.invalid/template/{z}/{x}/{y}.png",
                "name": "Dated template", "bounds": [10.0, -5.0, 20.0, 5.0]
            }),
        )
        .unwrap();
        s.map_mut().tiles[0].time_url = Some("https://example.invalid/{date:YYYY}/{z}/{x}/{y}.png".into());
        s.map_mut().tiles[0].url = "https://example.invalid/2024/{z}/{x}/{y}.png".into();
        let template_hit = search(&s, "Dated template", LIMIT).hits.remove(0);
        s.map_mut().tiles[0].url = "https://example.invalid/2025/{z}/{x}/{y}.png".into();
        go_to(&mut s, &template_hit).unwrap();

        s.run(
            "map.addTiles",
            &json!({
                "url": "https://example.invalid/cube/2024/{z}/{x}/{y}.png",
                "name": "Dated cube", "bounds": [10.0, -5.0, 20.0, 5.0]
            }),
        )
        .unwrap();
        let cube = s.map_mut().tiles.get_mut(0).unwrap();
        cube.time_urls = vec![
            (2024.0, "https://example.invalid/cube/2024/{z}/{x}/{y}.png".into()),
            (2025.0, "https://example.invalid/cube/2025/{z}/{x}/{y}.png".into()),
        ];
        let cube_hit = search(&s, "Dated cube", LIMIT).hits.remove(0);
        s.map_mut().tiles[0].url = "https://example.invalid/cube/2025/{z}/{x}/{y}.png".into();
        go_to(&mut s, &cube_hit).unwrap();
    }

    #[test]
    fn alike_tile_hits_still_resolve() {
        let mut s = Session::new(crate::Project::default());
        s.run(
            "map.addTiles",
            &json!({
                "url": "https://example.invalid/twin/{z}/{x}/{y}.png",
                "name": "Twin", "bounds": [10.0, -5.0, 20.0, 5.0]
            }),
        )
        .unwrap();
        // The same layer twice: `map.addTiles` replaces it, but a project load only repairs each
        // layer and truncates the stack, duplicates and all, so two alike layers do occur.
        let twin = s.map().tiles[0].clone();
        s.map_mut().tiles.insert(0, twin);
        let hits = search(&s, "Twin", LIMIT).hits;
        assert_eq!(hits.len(), 2, "{hits:?}");
        for hit in &hits {
            let Hit::Tile { index, identity, .. } = hit else {
                panic!("expected a tile hit, got {hit:?}");
            };
            // Alike layers are told apart by where the row was listed, not left to go stale.
            assert_eq!(super::super::tiles::index_of_identity(&s, identity, *index), Some(*index));
            go_to(&mut s, hit).unwrap();
        }
        let c = rustgis_geom::merc_to_lonlat(s.map().camera.center);
        assert!((c.x - 15.0).abs() < 1e-6, "{c:?}");
    }

    #[test]
    fn tile_hits_tell_dated_sources_apart() {
        let mut s = Session::new(crate::Project::default());
        for i in 0..2 {
            s.run(
                "map.addTiles",
                &json!({
                    "url": format!("https://example.invalid/cube-{i}/2024/{{z}}/{{x}}/{{y}}.png"),
                    "name": "Coverage", "bounds": [10.0, -5.0, 20.0, 5.0]
                }),
            )
            .unwrap();
            // A dated cube: the current address is the slider's, the list is the source.
            let t = s.map_mut().tiles.get_mut(0).unwrap();
            t.time_urls = vec![(2024.0, format!("https://example.invalid/cube-{i}/{{z}}/{{x}}/{{y}}.png"))];
        }
        let hits = search(&s, "Coverage", LIMIT).hits;
        assert_eq!(hits.len(), 2, "{hits:?}");
        // Each identity names its own layer, even asked for from the other's place in the stack.
        for hit in &hits {
            let Hit::Tile { index, identity, .. } = hit else {
                panic!("expected a tile hit, got {hit:?}");
            };
            assert_eq!(super::super::tiles::index_of_identity(&s, identity, 1 - *index), Some(*index), "{hit:?}");
        }
    }

    #[test]
    fn going_to_a_feature_selects_it() {
        let mut s = Session::new(crate::sample::world_project());
        let v = search_cmd(&mut s, &json!({"query": "Tokyo", "go": true})).unwrap_or_default();
        assert!(v["hits"].as_array().is_some_and(|h| !h.is_empty()));
        assert!(s.map().layers.iter().any(|l| !l.selection.is_empty()));
        assert!(search_cmd(&mut s, &json!({"query": "zzqqxx-none", "go": true})).is_err());
        assert!(search_cmd(&mut s, &json!({"query": "Tokyo", "go": 999})).is_err());
        assert!(search_cmd(&mut s, &json!({"query": "Tokyo", "go": 0.0})).is_ok());
        let e = search_cmd(&mut s, &json!({"query": "Tokyo", "go": -1.5})).err().unwrap_or_default();
        assert!(e.starts_with("go is"), "{e}");
    }
}
