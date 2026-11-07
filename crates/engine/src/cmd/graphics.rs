//! Home › Add (Add Graphics Layer) and Edit › Graphics: shapes, text and pictures drawn on the
//! map in graphics layers.

use base64::Engine;
use rustgis_data::{Geometry, Value as Attr};
use rustgis_geom::{Coord, lonlat_to_merc, merc_to_lonlat};
use rustgis_map::layout::TextAlign;
use rustgis_map::{Rgba, graphics};
use serde_json::{Value, json};

use super::{b, f, ps, uint};
use crate::{CmdResult, CommandSpec as C, Session};

/// Largest picture accepted (16 MiB).
const MAX_PICTURE: usize = 16 << 20;
/// Most vertices in one graphic.
const MAX_POINTS: usize = 100_000;

/// Keeps in-memory graphics available without file access; explicit picture paths add that reach.
pub fn commands() -> Vec<C> {
    vec![
        C::new("graphics.newLayer", "Add Graphics Layer", "Home › Add", |s, p| {
            let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Graphics");
            let id = new_layer(s, name);
            Ok(json!({"layer": id}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name"?: string}"#),
        C::new("graphics.add", "Add Graphic", "Edit › Graphics", add).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r##"{"shape": "point"|"text"|"line"|"curve"|"freehand"|"polygon"|"lasso"|"rectangle"|"circle"|"ellipse"|"picture", "points": [[lon, lat], …] (rectangle, ellipse, picture: two corners; circle: centre and a point on the edge), "layer"?: graphics layer, "text"?: string (dynamic tokens: {map} {scale} {date} {time} {crs} {layers} {center}), "fill"?: "#RRGGBBAA", "outline"?: "#RRGGBBAA", "width"?: number, "size"?: number, "fontSize"?: number, "bold"?: bool, "italic"?: bool, "align"?: "left"|"center"|"right" (a text's lines in their block, which stays centred on its point), "letterSpacing"?: points, -5 to 50, "image"?: base64 PNG/JPEG, "path"?: picture file}"##,
        ),
        C::new("graphics.style", "Graphic Style", "Edit › Graphics", style).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(super::need_selection)
            .params(r##"{"fill"?: "#RRGGBBAA", "outline"?: "#RRGGBBAA", "width"?: number, "size"?: number, "fontSize"?: number, "bold"?: bool, "italic"?: bool, "align"?: "left"|"center"|"right", "letterSpacing"?: points, -5 to 50, "text"?: string}"##),
        C::new("graphics.edit", "Edit Graphic", "Edit › Graphics", edit).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r##"{"layer": graphics layer, "row": number, "points"?: [[lon, lat]] (a point or a text: where it sits), "text"?: string, "fontSize"?: number, "bold"?: bool, "italic"?: bool, "align"?: "left"|"center"|"right", "letterSpacing"?: points, -5 to 50, "fill"?: "#RRGGBBAA", "outline"?: "#RRGGBBAA", "width"?: number, "size"?: number}"##,
        ),
        C::new("graphics.keepTool", "Keep the last tool active", "Edit › Graphics", |s, p| {
            s.keep_tool = b(p, "on").unwrap_or(!s.keep_tool);
            Ok(json!({"keepTool": s.keep_tool}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"on"?: bool}"#),
    ]
}

/// Adds a graphics layer with a unique name and makes it active.
fn new_layer(s: &mut Session, name: &str) -> u32 {
    let base: String = name.chars().take(200).collect();
    let mut name = base.clone();
    let mut i = 1;
    while s.map().layer_by_name(&name).is_some() {
        i += 1;
        name = format!("{base} ({i})");
    }
    let id = s.map_mut().add_graphics(&name);
    s.active_layer = Some(id);
    id
}

/// The graphics layer to draw in: the one named, the active one, the first one, or a new one.
fn target(s: &mut Session, p: &Value) -> Result<u32, String> {
    if p.get("layer").is_some() {
        let id = s.layer_id(p)?;
        return if s.map().layer(id).is_some_and(|l| l.graphics) { Ok(id) } else { Err("that isn't a graphics layer".into()) };
    }
    let map = s.map();
    let found = s
        .active_layer
        .filter(|id| map.layer(*id).is_some_and(|l| l.graphics))
        .or_else(|| map.layers.iter().find(|l| l.graphics).map(|l| l.id));
    Ok(match found {
        Some(id) => id,
        None => new_layer(s, "Graphics"),
    })
}

fn points(p: &Value) -> Result<Vec<Coord>, String> {
    let arr = p.get("points").and_then(Value::as_array).ok_or("points is required")?;
    if arr.len() > MAX_POINTS {
        return Err(format!("a graphic takes at most {MAX_POINTS} points"));
    }
    arr.iter()
        .map(|v| {
            let x = v.get(0).and_then(Value::as_f64).filter(|x| x.is_finite() && x.abs() <= 540.0);
            let y = v.get(1).and_then(Value::as_f64).filter(|y| y.is_finite() && y.abs() <= 90.0);
            match (x, y) {
                (Some(x), Some(y)) => Ok(Coord::new(x, y.clamp(-85.0, 85.0))),
                _ => Err("each point is [lon, lat]".to_string()),
            }
        })
        .collect()
}

fn need(pts: &[Coord], n: usize, shape: &str) -> Result<(), String> {
    if pts.len() < n { Err(format!("a {shape} needs at least {n} points")) } else { Ok(()) }
}

/// The first and last points (two corners, or a centre and an edge point).
fn ends(pts: &[Coord], shape: &str) -> Result<(Coord, Coord), String> {
    need(pts, 2, shape)?;
    match (pts.first(), pts.last()) {
        (Some(a), Some(b)) => Ok((*a, *b)),
        _ => Err(format!("a {shape} needs at least 2 points")),
    }
}

fn ring(mut pts: Vec<Coord>) -> Vec<Coord> {
    if let Some(first) = pts.first().copied() {
        pts.push(first);
    }
    pts
}

/// A smooth curve through the points (Catmull-Rom).
fn curve(pts: &[Coord]) -> Vec<Coord> {
    let m: Vec<Coord> = pts.iter().map(|c| lonlat_to_merc(*c)).collect();
    let n = m.len();
    let at = |i: isize| m.get(i.clamp(0, n as isize - 1) as usize).copied().unwrap_or_default();
    let mut out = Vec::new();
    for i in 0..n.saturating_sub(1) {
        let (p0, p1, p2, p3) = (at(i as isize - 1), at(i as isize), at(i as isize + 1), at(i as isize + 2));
        for k in 0..8 {
            let t = k as f64 / 8.0;
            let (t2, t3) = (t * t, t * t * t);
            let c = |a: f64, b: f64, c: f64, d: f64| {
                0.5 * (2.0 * b + (c - a) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2 + (3.0 * b - a - 3.0 * c + d) * t3)
            };
            out.push(Coord::new(c(p0.x, p1.x, p2.x, p3.x), c(p0.y, p1.y, p2.y, p3.y)));
        }
    }
    if let Some(l) = m.last() {
        out.push(*l);
    }
    out.into_iter().map(merc_to_lonlat).collect()
}

/// An ellipse inscribed in the box between two corners, drawn round on the flat map.
fn ellipse(a: Coord, b: Coord) -> Vec<Coord> {
    let (a, b) = (lonlat_to_merc(a), lonlat_to_merc(b));
    let (cx, cy) = ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    let (rx, ry) = ((a.x - b.x).abs() / 2.0, (a.y - b.y).abs() / 2.0);
    ring(
        (0..72)
            .map(|i| {
                let t = i as f64 / 72.0 * std::f64::consts::TAU;
                merc_to_lonlat(Coord::new(cx + rx * t.cos(), cy + ry * t.sin()))
            })
            .collect(),
    )
}

fn geometry(shape: &str, pts: Vec<Coord>) -> Result<Geometry, String> {
    Ok(match shape {
        "point" | "text" => Geometry::Point(*pts.first().ok_or("a point needs a location")?),
        "line" | "freehand" => {
            need(&pts, 2, shape)?;
            Geometry::Line(vec![pts])
        }
        "curve" => {
            need(&pts, 2, shape)?;
            Geometry::Line(vec![curve(&pts)])
        }
        "polygon" | "lasso" => {
            need(&pts, 3, shape)?;
            Geometry::Polygon(vec![vec![ring(pts)]])
        }
        "rectangle" | "picture" => {
            let (a, b) = ends(&pts, shape)?;
            if a.x == b.x || a.y == b.y {
                return Err(format!("a {shape} needs two different corners"));
            }
            let (x0, x1, y0, y1) = (a.x.min(b.x), a.x.max(b.x), a.y.min(b.y), a.y.max(b.y));
            Geometry::Polygon(vec![vec![vec![
                Coord::new(x0, y0),
                Coord::new(x1, y0),
                Coord::new(x1, y1),
                Coord::new(x0, y1),
                Coord::new(x0, y0),
            ]]])
        }
        "circle" => {
            let (c, e) = ends(&pts, shape)?;
            let (c, e) = (lonlat_to_merc(c), lonlat_to_merc(e));
            let r = ((e.x - c.x).powi(2) + (e.y - c.y).powi(2)).sqrt();
            if r <= 0.0 {
                return Err("a circle needs a radius".into());
            }
            Geometry::Polygon(vec![vec![ellipse(
                merc_to_lonlat(Coord::new(c.x - r, c.y - r)),
                merc_to_lonlat(Coord::new(c.x + r, c.y + r)),
            )]])
        }
        "ellipse" => {
            let (a, b) = ends(&pts, shape)?;
            if a.x == b.x || a.y == b.y {
                return Err("an ellipse needs two different corners".into());
            }
            Geometry::Polygon(vec![vec![ellipse(a, b)]])
        }
        other => return Err(format!("unknown shape '{other}'")),
    })
}

/// The italic, alignment and letter spacing a command gives a text (#535): `None` where not given.
#[derive(Default)]
struct TextStyle {
    italic: Option<bool>,
    align: Option<TextAlign>,
    spacing: Option<f64>,
}

impl TextStyle {
    /// From `italic`, `align` and `letterSpacing`, read and checked as `layout.update` reads a
    /// layout text's, so the same text takes the same values on the map and on the page.
    fn of(p: &Value) -> Result<Self, String> {
        let (italic, align, spacing) = super::layout::text_style(p, false, TextAlign::Left, 0.0)?;
        let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
        Ok(TextStyle {
            italic: given("italic").then_some(italic),
            align: given("align").then_some(align),
            spacing: given("letterSpacing").then_some(spacing),
        })
    }

    fn is_empty(&self) -> bool {
        self.italic.is_none() && self.align.is_none() && self.spacing.is_none()
    }

    /// The fields to set and their values; a layer made before a field gains it here.
    fn sets(&self, ds: &mut rustgis_data::Dataset) -> Vec<(&'static str, Attr)> {
        use rustgis_data::FieldType;
        let mut out = Vec::new();
        if let Some(v) = self.italic {
            ds.ensure_field("italic", FieldType::Boolean);
            out.push(("italic", Attr::Bool(v)));
        }
        if let Some(a) = self.align {
            ds.ensure_field("align", FieldType::Text);
            out.push(("align", Attr::Text(a.code().into())));
        }
        if let Some(v) = self.spacing {
            ds.ensure_field("letter_spacing", FieldType::Double);
            out.push(("letter_spacing", Attr::Float(v)));
        }
        out
    }
}

fn color(p: &Value, k: &str) -> Result<Option<String>, String> {
    match ps(p, k) {
        Some(c) => Rgba::from_hex(c).map(|c| Some(c.hex())).ok_or_else(|| format!("{k} must be #RRGGBB or #RRGGBBAA")),
        None => Ok(None),
    }
}

/// The picture's bytes as base64, from `image` or a file `path`.
pub(crate) fn picture(p: &Value) -> Result<String, String> {
    let b64 = match (ps(p, "image"), ps(p, "path")) {
        (Some(b), _) => b.trim().to_string(),
        (None, Some(path)) => {
            use std::io::Read;
            // Read at most one byte past the limit, so a huge or endless file is refused.
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .and_then(|f| f.take(MAX_PICTURE as u64 + 1).read_to_end(&mut bytes))
                .map_err(|e| format!("{path}: {e}"))?;
            if bytes.len() > MAX_PICTURE {
                return Err("the picture is too large".into());
            }
            base64::engine::general_purpose::STANDARD.encode(bytes)
        }
        (None, None) => return Err("a picture needs an image or a path".into()),
    };
    if b64.len() > MAX_PICTURE / 3 * 4 + 4 {
        return Err("the picture is too large".into());
    }
    if rustgis_render::picture_size(&b64).is_none() {
        return Err("the picture isn't a PNG or JPEG image".into());
    }
    Ok(b64)
}

fn add(s: &mut Session, p: &Value) -> CmdResult {
    let shape = ps(p, "shape").ok_or("shape is required")?.trim().to_ascii_lowercase();
    if !graphics::SHAPES.contains(&shape.as_str()) {
        return Err(format!("unknown shape '{shape}'"));
    }
    let g = geometry(&shape, points(p)?)?;
    let text = ps(p, "text").map(|t| t.chars().take(2000).collect::<String>());
    if shape == "text" && text.as_deref().is_none_or(|t| t.trim().is_empty()) {
        return Err("text is required".into());
    }
    let image = if shape == "picture" { Some(picture(p)?) } else { None };
    let (fill, outline) = (color(p, "fill")?, color(p, "outline")?);
    let style = TextStyle::of(p)?;
    let id = target(s, p)?;
    let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
    let num = |k: &str, min: f64, max: f64| f(p, k).map(|v| Attr::Float(v.clamp(min, max))).unwrap_or(Attr::Null);
    let opt = |v: Option<String>| v.map(Attr::Text).unwrap_or(Attr::Null);
    let attrs = vec![
        Attr::Text(shape.clone()),
        opt(text),
        opt(fill),
        opt(outline),
        num("width", 0.0, 50.0),
        num("size", 0.0, 200.0),
        num("fontSize", *graphics::FONT_SIZE.start(), *graphics::FONT_SIZE.end()),
        opt(image),
    ];
    l.dataset.push(Some(g), attrs);
    let row = l.dataset.features.len() - 1;
    // By name, as a layer made before the field lacks it.
    if let Some(bold) = b(p, "bold") {
        let fi = l.dataset.ensure_field("bold", rustgis_data::FieldType::Boolean);
        if let Some(cell) = l.dataset.features.get_mut(row).and_then(|ft| ft.attrs.get_mut(fi)) {
            *cell = Attr::Bool(bold);
        }
    }
    for (k, v) in style.sets(&mut l.dataset) {
        let fi = l.dataset.field_index(k);
        if let Some(cell) = fi.and_then(|fi| l.dataset.features.get_mut(row).and_then(|ft| ft.attrs.get_mut(fi))) {
            *cell = v;
        }
    }
    l.touch_geometry();
    Ok(json!({"layer": id, "row": row, "shape": shape}))
}

fn style(s: &mut Session, p: &Value) -> CmdResult {
    restyle(s, p, None)
}

/// Changes one graphic, named by layer and row, as `graphics.style` changes the selected ones.
fn edit(s: &mut Session, p: &Value) -> CmdResult {
    // Never the active layer: a row means nothing without the layer it was read from.
    if !p.get("layer").is_some_and(|v| v.is_number() || v.is_string()) {
        return Err("layer is required".into());
    }
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).filter(|l| l.graphics).ok_or("that isn't a graphics layer")?;
    let row =
        uint(p, "row").and_then(|r| usize::try_from(r).ok()).filter(|r| *r < l.dataset.features.len()).ok_or("no graphic at that row")?;
    if let Some(t) = ps(p, "text") {
        if graphics::kind(&l.dataset, row) != "text" {
            return Err("only a text graphic has text".into());
        }
        if t.chars().take(2000).all(char::is_whitespace) {
            return Err("text is required".into());
        }
    }
    // Placed again (a text dragged on the map, #516): only what sits at one point; other shapes
    // move with `edit.move`.
    let placed = match p.get("points") {
        Some(_) => {
            let kind = graphics::kind(&l.dataset, row).to_string();
            if !matches!(kind.as_str(), "point" | "text") {
                return Err("only a point or a text graphic is placed again by its points".into());
            }
            Some(geometry(&kind, points(p)?)?)
        }
        None => None,
    };
    if let Some(g) = placed {
        let l = s.map_mut().layer_mut(id).ok_or("no such layer")?;
        if let Some(ft) = l.dataset.features.get_mut(row) {
            ft.geometry = Some(g);
        }
        l.touch_geometry();
        // A null is no style, as `restyle` reads it: placed with only nulls, it is just placed.
        let styled = ["text", "fill", "outline", "width", "size", "fontSize", "bold", "italic", "align", "letterSpacing"]
            .iter()
            .any(|k| p.get(*k).is_some_and(|v| !v.is_null()));
        if !styled {
            return Ok(json!({"placed": 1}));
        }
    }
    restyle(s, p, Some((id, row)))
}

/// Sets the style (and a text graphic's text) of one graphic, or else of the selected ones.
fn restyle(s: &mut Session, p: &Value, one: Option<(u32, usize)>) -> CmdResult {
    let (fill, outline) = (color(p, "fill")?, color(p, "outline")?);
    let text = ps(p, "text").map(|t| t.chars().take(2000).collect::<String>());
    // Font sizes are kept within the range they are drawn at (graphics::font_size).
    let fonts = &graphics::FONT_SIZE;
    let nums = [("width", "width", 0.0, 50.0), ("size", "size", 0.0, 200.0), ("font_size", "fontSize", *fonts.start(), *fonts.end())];
    let bold = b(p, "bold");
    let style = TextStyle::of(p)?;
    if fill.is_none()
        && outline.is_none()
        && text.is_none()
        && bold.is_none()
        && style.is_empty()
        && nums.iter().all(|(_, k, _, _)| f(p, k).is_none())
    {
        return Err("give a fill, outline, width, size, fontSize, bold, italic, align, letterSpacing or text".into());
    }
    let mut n = 0;
    let targeted = |l: &rustgis_map::Layer| one.map_or(!l.selection.is_empty(), |(id, _)| l.id == id);
    for l in s.map_mut().layers.iter_mut().filter(|l| l.graphics && targeted(l)) {
        let mut sets: Vec<(&str, Attr)> = Vec::new();
        if let Some(c) = &fill {
            sets.push(("fill", Attr::Text(c.clone())));
        }
        if let Some(c) = &outline {
            sets.push(("outline", Attr::Text(c.clone())));
        }
        for (field, k, min, max) in nums {
            if let Some(v) = f(p, k) {
                sets.push((field, Attr::Float(v.clamp(min, max))));
            }
        }
        // A layer made before the field gains it here.
        if let Some(v) = bold {
            l.dataset.ensure_field("bold", rustgis_data::FieldType::Boolean);
            sets.push(("bold", Attr::Bool(v)));
        }
        sets.extend(style.sets(&mut l.dataset));
        let sets: Vec<(Option<usize>, Attr)> = sets.into_iter().map(|(k, v)| (l.dataset.field_index(k), v)).collect();
        let text_field = l.dataset.field_index("text");
        let rows = one.map_or_else(|| l.selection.clone(), |(_, row)| vec![row]);
        for r in rows {
            let is_text = graphics::kind(&l.dataset, r) == "text";
            let Some(ft) = l.dataset.features.get_mut(r) else { continue };
            for (fi, v) in &sets {
                if let Some(cell) = fi.and_then(|fi| ft.attrs.get_mut(fi)) {
                    *cell = v.clone();
                }
            }
            // Only text graphics show text.
            if let (Some(t), Some(fi), true) = (&text, text_field, is_text)
                && let Some(cell) = ft.attrs.get_mut(fi)
            {
                *cell = Attr::Text(t.clone());
            }
            n += 1;
        }
    }
    if n == 0 {
        return Err("select graphics first".into());
    }
    Ok(json!({"styled": n}))
}
