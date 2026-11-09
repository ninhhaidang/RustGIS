//! MapLibre GL style spec (v8) subset: style, sources and layers; expressions, legacy filters and functions; CSS colors.
//!
//! Paint/layout values and filters compile once into a small tree ([`Expr`]) and evaluate per feature. Hostile JSON
//! never panics: unknown operators and type errors evaluate to [`Val::Null`], nesting is capped at 32 levels, arrays,
//! strings and counts are capped.

use crate::mvt::{GeomKind, Value};
use serde_json::{Map, Value as Json};
use std::cmp::Ordering;
use std::collections::HashMap;

/// Parses a CSS color (moved to [`rustgis_data::color::parse`], which the expression language shares).
pub use rustgis_data::color::parse as parse_color;
use rustgis_data::color::unit_u8;

/// Straight (not premultiplied) RGBA.
pub type Rgba = [u8; 4];

const BLACK: Rgba = [0, 0, 0, 255];
const MAX_DEPTH: usize = 32;
const MAX_ARRAY: usize = 4096;
const MAX_STR: usize = 1 << 16;
const MAX_LAYERS: usize = 10_000;
const MAX_SOURCES: usize = 256;
/// The most sprites a style's are read from.
pub(crate) const MAX_SPRITES: usize = 16;
/// The paint properties that fill with an image of a sprite.
pub(crate) const PATTERNS: [&str; 2] = ["fill-pattern", "fill-extrusion-pattern"];
/// The longest sprite id read: pictures and meshes each keep the ids they were made from.
const MAX_SPRITE_ID: usize = 128;
const MAX_PROPS: usize = 256;
const MAX_STOPS: usize = 1024;
/// The most lights a multidirectional hillshade takes: each shades every pixel again.
const MAX_LIGHTS: usize = 16;
const MAX_URLS: usize = 64;
const MAX_DASHES: usize = 64;

// ---------------------------------------------------------------------------------------------------------------
// Colors
// ---------------------------------------------------------------------------------------------------------------

/// Interpolates in premultiplied space so fades to/from `transparent` don't darken.
fn lerp_color(p: Rgba, q: Rgba, t: f64) -> Rgba {
    let pm = |[r, g, b, a]: Rgba| {
        let a = f64::from(a) / 255.0;
        [f64::from(r) * a, f64::from(g) * a, f64::from(b) * a, a]
    };
    let mut m = [0.0f64; 4];
    for ((o, x), y) in m.iter_mut().zip(pm(p)).zip(pm(q)) {
        *o = x + (y - x) * t;
    }
    let [r, g, b, a] = m;
    if a <= 0.0 {
        return [0, 0, 0, 0];
    }
    let c = |v: f64| (v / a).clamp(0.0, 255.0).round() as u8;
    [c(r), c(g), c(b), unit_u8(a)]
}

// ---------------------------------------------------------------------------------------------------------------
// Values and feature context
// ---------------------------------------------------------------------------------------------------------------

/// Feature being styled.
#[derive(Clone, Copy, Debug)]
pub struct FeatureCtx<'a> {
    pub zoom: f32,
    pub kind: GeomKind,
    pub id: Option<u64>,
    pub props: &'a [(String, Value)],
    /// The style's sprite, while an icon is chosen: `["image", name]` is `name` when it has
    /// that image, else null.
    pub images: Option<&'a crate::sprite::Sprite>,
}

impl FeatureCtx<'static> {
    /// Context with no feature (background, raster, zoom-only properties).
    pub fn zoom_only(zoom: f32) -> Self {
        FeatureCtx { zoom, kind: GeomKind::Unknown, id: None, props: &[], images: None }
    }
}

impl FeatureCtx<'_> {
    fn prop(&self, key: &str) -> Option<&Value> {
        self.props.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// Expression value.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Color(Rgba),
    Arr(Vec<Val>),
}

static NULL: Val = Val::Null;

impl Val {
    /// Converts JSON literally (objects become `Null`).
    pub fn from_json(j: &Json) -> Val {
        Self::from_json_at(j, 0)
    }

    fn from_json_at(j: &Json, depth: usize) -> Val {
        match j {
            Json::Null | Json::Object(_) => Val::Null,
            Json::Bool(b) => Val::Bool(*b),
            Json::Number(n) => n.as_f64().map_or(Val::Null, Val::Num),
            Json::String(s) => Val::Str(s.clone()),
            Json::Array(a) if depth < MAX_DEPTH => Val::Arr(a.iter().take(MAX_ARRAY).map(|v| Self::from_json_at(v, depth + 1)).collect()),
            Json::Array(_) => Val::Null,
        }
    }

    fn from_prop(v: &Value) -> Val {
        match v {
            Value::String(s) => Val::Str(s.clone()),
            Value::Number(n) => Val::Num(*n),
            Value::Bool(b) => Val::Bool(*b),
        }
    }

    /// Finite number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Val::Num(n) if n.is_finite() => Some(*n),
            _ => None,
        }
    }

    /// String contents.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Color, parsing strings with [`parse_color`].
    pub fn as_color(&self) -> Option<Rgba> {
        match self {
            Val::Color(c) => Some(*c),
            Val::Str(s) => parse_color(s),
            _ => None,
        }
    }

    /// `to-string` semantics.
    pub fn to_text(&self) -> String {
        match self {
            Val::Null => String::new(),
            Val::Bool(b) => b.to_string(),
            Val::Num(n) => fmt_num(*n),
            Val::Str(s) => s.clone(),
            Val::Color([r, g, b, a]) => format!("rgba({r},{g},{b},{})", fmt_num(f64::from(*a) / 255.0)),
            Val::Arr(items) => {
                let mut s = String::from("[");
                for (i, x) in items.iter().enumerate() {
                    if i > 0 {
                        push_capped(&mut s, ",");
                    }
                    push_capped(&mut s, &x.to_text());
                    if s.len() >= MAX_STR {
                        break;
                    }
                }
                push_capped(&mut s, "]");
                s
            }
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Val::Null => "null",
            Val::Bool(_) => "boolean",
            Val::Num(_) => "number",
            Val::Str(_) => "string",
            Val::Color(_) => "color",
            Val::Arr(_) => "array",
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Val::Null => false,
            Val::Bool(b) => *b,
            Val::Num(n) => *n != 0.0 && !n.is_nan(),
            Val::Str(s) => !s.is_empty(),
            Val::Color(_) | Val::Arr(_) => true,
        }
    }

    fn to_number(&self) -> Option<f64> {
        match self {
            Val::Null => Some(0.0),
            Val::Bool(b) => Some(f64::from(u8::from(*b))),
            Val::Num(n) => Some(*n),
            Val::Str(s) => s.trim().parse().ok(),
            Val::Color(_) | Val::Arr(_) => None,
        }
    }

    fn to_color(&self) -> Option<Rgba> {
        match self {
            Val::Arr(a) if (3..=4).contains(&a.len()) => {
                let n: Vec<f64> = a.iter().map(Val::as_f64).collect::<Option<_>>()?;
                let ch = |i: usize| n.get(i).map_or(0, |v| v.clamp(0.0, 255.0).round() as u8);
                Some([ch(0), ch(1), ch(2), n.get(3).map_or(255, |a| unit_u8(*a))])
            }
            v => v.as_color(),
        }
    }
}

fn fmt_num(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { format!("{n}") }
}

/// Appends at most up to [`MAX_STR`] bytes, on a char boundary.
fn push_capped(s: &mut String, t: &str) {
    let room = MAX_STR.saturating_sub(s.len());
    if t.len() <= room {
        s.push_str(t);
        return;
    }
    let mut end = room;
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    s.push_str(t.get(..end).unwrap_or(""));
}

fn geom_name(k: GeomKind) -> &'static str {
    match k {
        GeomKind::Point => "Point",
        GeomKind::LineString => "LineString",
        GeomKind::Polygon => "Polygon",
        GeomKind::Unknown => "Unknown",
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------------------------------------------

/// A compiled expression, filter or legacy function.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr(Node);

impl Expr {
    /// Paint/layout value: literal, expression or legacy `{"stops": …}` function. A top-level array whose head is
    /// not an operator (`["Noto Sans Regular"]`, `[2, 1]`) is a literal.
    pub fn property(json: &Json) -> Expr {
        Expr(compile_property(json))
    }

    /// Layer filter, legacy or expression syntax (detected as MapLibre does).
    pub fn filter(json: &Json) -> Expr {
        Expr(if is_expression_filter(json, 0) { compile(json, 0) } else { compile_legacy(json, 0) })
    }

    /// Evaluates against a feature.
    pub fn eval(&self, f: &FeatureCtx) -> Val {
        eval(&self.0, f, &mut Vec::new())
    }

    /// Whether it reads nothing of a feature (its properties, id or geometry type): the same
    /// for every feature at a zoom.
    pub fn feature_free(&self) -> bool {
        free(&self.0)
    }

    /// Whether it depends on the zoom.
    pub fn uses_zoom(&self) -> bool {
        calls(&self.0, Op::Zoom)
    }

    /// Whether it asks the sprite for an image (`["image", name]`).
    pub fn uses_image(&self) -> bool {
        calls(&self.0, Op::Image)
    }

    /// A `color-relief-color` at `zoom` as colours by height: an `interpolate` or `step` on
    /// `["elevation"]` whose stops' colours are worked out once, or a single colour. `None` for
    /// anything else, which a layer can't be drawn from pixel by pixel.
    pub fn elevation_ramp(&self, zoom: f32) -> Option<ElevationRamp> {
        let f = FeatureCtx::zoom_only(zoom);
        let color = |n: &Node| eval(n, &f, &mut Vec::new()).as_color();
        let height = |n: &Node| matches!(n, Node::Call(Op::Elevation, _));
        let (base, below, stops) = match &self.0 {
            Node::Interp(base, input, stops) if height(input) => (Some(*base), None, stops),
            Node::Step(input, first, stops) if height(input) => (None, Some(color(first)?), stops),
            Node::Lit(_) => return Some(ElevationRamp { base: None, below: color(&self.0)?, stops: Vec::new() }),
            _ => return None,
        };
        let stops = stops.iter().map(|(h, n)| Some((*h, color(n)?))).collect::<Option<Vec<_>>>()?;
        // The spec's stops ascend; others have no meaning.
        if !stops.is_sorted_by(|a, b| a.0 <= b.0) {
            return None;
        }
        let below = below.or_else(|| stops.first().map(|s| s.1))?;
        Some(ElevationRamp { base, below, stops })
    }
}

/// The highest zoom a style is drawn at (see `template::style_zoom`).
const MAX_STYLE_ZOOM: f32 = 24.0;

/// How many steps to a zoom [`StyleLayer::color_relief_undrawable`] checks a layer's colours at,
/// between the zooms they name.
const ZOOM_GRID: f32 = 16.0;

/// Collects the numbers `n` names: its literals and the stops of its steps and ramps, up to a
/// few thousand (the grid still checks between past that).
fn numbers(n: &Node, out: &mut Vec<f64>) {
    if out.len() >= 4096 {
        return;
    }
    let stops = |list: &[(f64, Node)], out: &mut Vec<f64>| {
        for (at, o) in list {
            out.push(*at);
            numbers(o, out);
        }
    };
    match n {
        Node::Lit(Val::Num(v)) => out.push(*v),
        Node::Lit(_) | Node::Var(_) | Node::LegacyGet(_) | Node::LegacyHas(_) => {}
        Node::Call(_, args) => args.iter().for_each(|a| numbers(a, out)),
        Node::Match(input, arms, default) => {
            numbers(input, out);
            arms.iter().for_each(|(_, o)| numbers(o, out));
            numbers(default, out);
        }
        Node::Step(input, first, list) => {
            numbers(input, out);
            numbers(first, out);
            stops(list, out);
        }
        Node::Interp(_, input, list) => {
            numbers(input, out);
            stops(list, out);
        }
        Node::Let(binds, body) => {
            binds.iter().for_each(|(_, v)| numbers(v, out));
            numbers(body, out);
        }
        Node::Tokens(inner) | Node::LegacyIn(inner, _) => numbers(inner, out),
    }
}

/// Colours by height, from a `color-relief-color` (see [`Expr::elevation_ramp`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ElevationRamp {
    /// An `interpolate`'s exponential base (1: linear); `None` for a `step`.
    base: Option<f64>,
    /// The colour below the first stop.
    below: Rgba,
    /// Heights and their colours, ascending.
    stops: Vec<(f64, Rgba)>,
}

impl ElevationRamp {
    /// The colour of a height, as the expression gives it.
    pub fn color(&self, h: f64) -> Rgba {
        // At or below an `interpolate`'s first height, its first stop, as the expression gives it
        // when that height is repeated.
        if let (Some(_), Some(first)) = (self.base, self.stops.first())
            && h <= first.0
        {
            return first.1;
        }
        // The stops at or below the height.
        let i = self.stops.partition_point(|s| s.0 <= h);
        let (lo, hi) = (i.checked_sub(1).and_then(|j| self.stops.get(j)), self.stops.get(i));
        match (self.base, lo, hi) {
            (_, None, _) => self.below,
            (None, Some(lo), _) | (Some(_), Some(lo), None) => lo.1,
            (Some(base), Some(lo), Some(hi)) => lerp_color(lo.1, hi.1, interp_t(base, h - lo.0, hi.0 - lo.0)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Get,
    Has,
    Id,
    GeomType,
    Zoom,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Not,
    All,
    Any,
    NoneOf,
    In,
    Case,
    Coalesce,
    ToNumber,
    ToString,
    ToBoolean,
    ToColor,
    IsNumber,
    IsString,
    IsBoolean,
    IsArray,
    TypeOf,
    Concat,
    Upcase,
    Downcase,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Min,
    Max,
    Abs,
    Round,
    Floor,
    Ceil,
    Sqrt,
    Ln,
    Log10,
    Log2,
    E,
    Pi,
    Ln2,
    Length,
    At,
    IndexOf,
    Slice,
    Format,
    Rgb,
    /// A `color-relief` layer's height (see [`Expr::elevation_ramp`]); null anywhere else.
    Elevation,
    Image,
}

fn op_of(name: &str) -> Option<Op> {
    use Op::*;
    Some(match name {
        "get" => Get,
        "has" => Has,
        "id" => Id,
        "geometry-type" => GeomType,
        "zoom" => Zoom,
        "elevation" => Elevation,
        "==" => Eq,
        "!=" => Ne,
        "<" => Lt,
        "<=" => Le,
        ">" => Gt,
        ">=" => Ge,
        "!" => Not,
        "all" => All,
        "any" => Any,
        "none" => NoneOf,
        "in" => In,
        "case" => Case,
        "coalesce" => Coalesce,
        "to-number" => ToNumber,
        "to-string" => ToString,
        "to-boolean" => ToBoolean,
        "to-color" => ToColor,
        "number" => IsNumber,
        "string" => IsString,
        "boolean" => IsBoolean,
        "array" => IsArray,
        "typeof" => TypeOf,
        "concat" => Concat,
        "upcase" => Upcase,
        "image" => Image,
        "downcase" => Downcase,
        "+" => Add,
        "-" => Sub,
        "*" => Mul,
        "/" => Div,
        "%" => Rem,
        "^" => Pow,
        "min" => Min,
        "max" => Max,
        "abs" => Abs,
        "round" => Round,
        "floor" => Floor,
        "ceil" => Ceil,
        "sqrt" => Sqrt,
        "ln" => Ln,
        "log10" => Log10,
        "log2" => Log2,
        "e" => E,
        "pi" => Pi,
        "ln2" => Ln2,
        "length" => Length,
        "at" => At,
        "index-of" => IndexOf,
        "slice" => Slice,
        "format" => Format,
        "rgb" | "rgba" => Rgb,
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Lit(Val),
    Call(Op, Vec<Node>),
    /// input, (labels, output)…, default
    Match(Box<Node>, Vec<(Vec<Val>, Node)>, Box<Node>),
    /// input, output below the first stop, (stop, output)…
    Step(Box<Node>, Box<Node>, Vec<(f64, Node)>),
    /// exponential base (1 = linear), input, (stop, output)…
    Interp(f64, Box<Node>, Vec<(f64, Node)>),
    Let(Vec<(String, Node)>, Box<Node>),
    Var(String),
    /// Legacy `{token}` substitution on a string result.
    Tokens(Box<Node>),
    LegacyGet(String),
    LegacyHas(String),
    LegacyIn(Box<Node>, Vec<Val>),
}

/// Whether `n` holds a call of `op`.
fn calls(n: &Node, op: Op) -> bool {
    let any = |list: &[(f64, Node)]| list.iter().any(|(_, o)| calls(o, op));
    match n {
        Node::Call(o, args) => *o == op || args.iter().any(|a| calls(a, op)),
        Node::Match(input, arms, default) => calls(input, op) || arms.iter().any(|(_, o)| calls(o, op)) || calls(default, op),
        Node::Step(input, first, stops) => calls(input, op) || calls(first, op) || any(stops),
        Node::Interp(_, input, stops) => calls(input, op) || any(stops),
        Node::Let(binds, body) => binds.iter().any(|(_, v)| calls(v, op)) || calls(body, op),
        Node::Tokens(inner) | Node::LegacyIn(inner, _) => calls(inner, op),
        Node::Lit(_) | Node::Var(_) | Node::LegacyGet(_) | Node::LegacyHas(_) => false,
    }
}

/// A finite upper bound for numeric results at this zoom. Unknown arithmetic stays unbounded;
/// a cap or a finite set of possible outputs can still bound a feature-dependent expression.
fn numeric_upper(n: &Node, f: &FeatureCtx) -> Option<f64> {
    if free(n) {
        return eval(n, f, &mut Vec::new()).as_f64().filter(|v| v.is_finite());
    }
    let bound = |n| numeric_upper(n, f);
    match n {
        Node::Call(Op::Min, args) => args.iter().filter_map(bound).reduce(f64::min),
        Node::Call(Op::Max | Op::Coalesce | Op::IsNumber, args) => numeric_upper_all(args.iter(), f),
        Node::Call(Op::Case, args) => {
            numeric_upper_all(args.iter().skip(1).step_by(2).chain(args.last().filter(|_| args.len() % 2 == 1)), f)
        }
        Node::Match(_, arms, default) => numeric_upper_all(arms.iter().map(|(_, n)| n).chain(std::iter::once(default.as_ref())), f),
        Node::Step(_, first, stops) => numeric_upper_all(std::iter::once(first.as_ref()).chain(stops.iter().map(|(_, n)| n)), f),
        // Interpolation and other arithmetic may create NaN or lose precision even from
        // bounded inputs. Only an enclosing proven cap may narrow their full margin.
        _ => None,
    }
}

fn numeric_upper_all<'a>(nodes: impl Iterator<Item = &'a Node>, f: &FeatureCtx) -> Option<f64> {
    nodes.map(|n| numeric_upper(n, f)).try_fold(f64::NEG_INFINITY, |upper, n| Some(upper.max(n?))).filter(|v| v.is_finite())
}

/// See [`Expr::feature_free`].
fn free(n: &Node) -> bool {
    match n {
        Node::Lit(_) | Node::Var(_) => true,
        Node::Call(op, args) => !matches!(op, Op::Get | Op::Has | Op::Id | Op::GeomType | Op::Image) && args.iter().all(free),
        Node::Match(input, arms, default) => free(input) && arms.iter().all(|(_, o)| free(o)) && free(default),
        Node::Step(input, first, stops) => free(input) && free(first) && stops.iter().all(|(_, o)| free(o)),
        Node::Interp(_, input, stops) => free(input) && stops.iter().all(|(_, o)| free(o)),
        Node::Let(binds, body) => binds.iter().all(|(_, v)| free(v)) && free(body),
        Node::Tokens(_) | Node::LegacyGet(_) | Node::LegacyHas(_) | Node::LegacyIn(..) => false,
    }
}

fn null() -> Node {
    Node::Lit(Val::Null)
}

fn compile_property(j: &Json) -> Node {
    match j {
        Json::Object(o) => compile_function(o),
        Json::Array(a) => match a.split_first() {
            Some((Json::String(name), args)) => compile_op(name, args, 0).unwrap_or_else(|| Node::Lit(Val::from_json(j))),
            _ => Node::Lit(Val::from_json(j)),
        },
        _ => Node::Lit(Val::from_json(j)),
    }
}

/// Compiles a nested expression; unknown operators become `Null`, non-operator arrays literals.
fn compile(j: &Json, depth: usize) -> Node {
    if depth > MAX_DEPTH {
        return null();
    }
    match j {
        Json::Array(a) => match a.split_first() {
            Some((Json::String(name), args)) => compile_op(name, args, depth).unwrap_or_else(null),
            _ => Node::Lit(Val::from_json_at(j, depth)),
        },
        _ => Node::Lit(Val::from_json_at(j, depth)),
    }
}

/// `None` for an unknown operator.
fn compile_op(name: &str, args: &[Json], depth: usize) -> Option<Node> {
    let d = depth + 1;
    if d > MAX_DEPTH {
        return Some(null());
    }
    let arg = |i: usize| args.get(i).map_or_else(null, |j| compile(j, d));
    let stops = |from: usize| -> Vec<(f64, Node)> {
        args.get(from..)
            .unwrap_or(&[])
            .as_chunks::<2>()
            .0
            .iter()
            .take(MAX_STOPS)
            .filter_map(|p| Some((p.first()?.as_f64()?, compile(p.get(1)?, d))))
            .collect()
    };
    Some(match name {
        "literal" => Node::Lit(args.first().map_or(Val::Null, |j| Val::from_json_at(j, depth))),
        "!has" => Node::Call(Op::Not, vec![Node::Call(Op::Has, vec![arg(0)])]),
        "match" => {
            let rest = args.get(1..).unwrap_or(&[]);
            let Some((default, pairs)) = rest.split_last() else {
                return Some(null());
            };
            let arms = pairs
                .as_chunks::<2>()
                .0
                .iter()
                .take(MAX_STOPS)
                .filter_map(|p| {
                    let labels = match p.first()? {
                        Json::Array(xs) => xs.iter().take(MAX_ARRAY).map(|x| Val::from_json_at(x, d)).collect(),
                        x => vec![Val::from_json_at(x, d)],
                    };
                    Some((labels, compile(p.get(1)?, d)))
                })
                .collect();
            Node::Match(Box::new(arg(0)), arms, Box::new(compile(default, d)))
        }
        "step" => Node::Step(Box::new(arg(0)), Box::new(arg(1)), stops(2)),
        "interpolate" | "interpolate-hcl" | "interpolate-lab" => {
            let curve = args.first().and_then(Json::as_array);
            let base = match curve.and_then(|c| c.first()).and_then(Json::as_str) {
                Some("exponential") => curve.and_then(|c| c.get(1)).and_then(Json::as_f64).filter(|b| b.is_finite() && *b > 0.0),
                _ => None,
            };
            Node::Interp(base.unwrap_or(1.0), Box::new(arg(1)), stops(2))
        }
        "let" => {
            let Some((body, binds)) = args.split_last() else {
                return Some(null());
            };
            let binds = binds
                .as_chunks::<2>()
                .0
                .iter()
                .take(MAX_STOPS)
                .filter_map(|p| Some((p.first()?.as_str()?.to_owned(), compile(p.get(1)?, d))))
                .collect();
            Node::Let(binds, Box::new(compile(body, d)))
        }
        "var" => Node::Var(args.first().and_then(Json::as_str).unwrap_or("").to_owned()),
        "format" => Node::Call(Op::Format, args.iter().filter(|j| !j.is_object()).take(MAX_ARRAY).map(|j| compile(j, d)).collect()),
        "number-format" => Node::Call(Op::ToString, vec![arg(0)]),
        "within" => Node::Lit(Val::Bool(false)),
        "is-supported-script" => Node::Lit(Val::Bool(true)),
        "properties" | "feature-state" | "line-progress" | "accumulated" | "heatmap-density" | "collator" | "resolved-locale"
        | "global-state" | "object" | "distance" => null(),
        _ => Node::Call(op_of(name)?, args.iter().take(MAX_ARRAY).map(|j| compile(j, d)).collect()),
    })
}

/// MapLibre's `isExpressionFilter` (the MapLibre style specification, BSD-3-Clause; see `INCLUDED`
/// in `xtask/src/notices.rs`).
fn is_expression_filter(j: &Json, depth: usize) -> bool {
    let a = match j {
        Json::Bool(_) => return true,
        Json::Array(a) if !a.is_empty() => a,
        _ => return false,
    };
    if depth > MAX_DEPTH {
        return true;
    }
    let second = a.get(1);
    match a.first().and_then(Json::as_str) {
        Some("has") => a.len() >= 2 && !matches!(second.and_then(Json::as_str), Some("$id" | "$type")),
        Some("in") => a.len() >= 3 && (!second.is_some_and(Json::is_string) || a.get(2).is_some_and(Json::is_array)),
        Some("!in" | "!has" | "none") => false,
        Some("==" | "!=" | ">" | ">=" | "<" | "<=") => {
            a.len() != 3 || second.is_some_and(Json::is_array) || a.get(2).is_some_and(Json::is_array)
        }
        Some("any" | "all") => a.iter().skip(1).all(|f| is_expression_filter(f, depth + 1)),
        _ => true,
    }
}

fn compile_legacy(j: &Json, depth: usize) -> Node {
    let no = Node::Lit(Val::Bool(false));
    let Json::Array(a) = j else {
        return Node::Lit(Val::Bool(j.as_bool() == Some(true)));
    };
    if depth > MAX_DEPTH {
        return no;
    }
    let key = a.get(1).and_then(Json::as_str).map(str::to_owned);
    let not = |n: Node| Node::Call(Op::Not, vec![n]);
    match a.first().and_then(Json::as_str) {
        Some(op @ ("all" | "any" | "none")) => {
            let op = match op {
                "all" => Op::All,
                "any" => Op::Any,
                _ => Op::NoneOf,
            };
            Node::Call(op, a.iter().skip(1).take(MAX_ARRAY).map(|x| compile_legacy(x, depth + 1)).collect())
        }
        Some(op @ ("==" | "!=" | "<" | "<=" | ">" | ">=")) => match (key, a.get(2), op_of(op)) {
            (Some(k), Some(v), Some(op)) => Node::Call(op, vec![Node::LegacyGet(k), Node::Lit(Val::from_json(v))]),
            _ => no,
        },
        Some(op @ ("in" | "!in")) => {
            let Some(k) = key else { return no };
            let set = a.iter().skip(2).take(MAX_ARRAY).map(Val::from_json).collect();
            let n = Node::LegacyIn(Box::new(Node::LegacyGet(k)), set);
            if op == "in" { n } else { not(n) }
        }
        Some(op @ ("has" | "!has")) => {
            let Some(k) = key else { return no };
            if op == "has" { Node::LegacyHas(k) } else { not(Node::LegacyHas(k)) }
        }
        _ => no,
    }
}

/// Legacy property function: zoom, property or zoom-and-property; exponential, interval, categorical or identity.
fn compile_function(o: &Map<String, Json>) -> Node {
    let default = o.get("default").map_or(Val::Null, Val::from_json);
    let property = o.get("property").and_then(Json::as_str);
    let base = o.get("base").and_then(Json::as_f64).filter(|b| b.is_finite() && *b > 0.0).unwrap_or(1.0);
    let ftype = o.get("type").and_then(Json::as_str);
    let input = || property.map_or_else(|| Node::Call(Op::Zoom, vec![]), |p| Node::Call(Op::Get, vec![Node::Lit(Val::Str(p.into()))]));
    let with_default = |n: Node| {
        if property.is_some() && default != Val::Null { Node::Call(Op::Coalesce, vec![n, Node::Lit(default.clone())]) } else { n }
    };
    if ftype == Some("identity") {
        return with_default(input());
    }
    let stops: Vec<(&Json, &Json)> = o
        .get("stops")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .take(MAX_STOPS)
        .filter_map(|s| {
            let s = s.as_array()?;
            Some((s.first()?, s.get(1)?))
        })
        .collect();
    let Some(&(k0, _)) = stops.first() else { return Node::Lit(default) };
    let node = if k0.is_object() {
        let mut groups: Vec<(f64, Vec<(&Json, &Json)>)> = Vec::new();
        for &(k, v) in &stops {
            let (Some(z), Some(val)) = (k.get("zoom").and_then(Json::as_f64), k.get("value")) else {
                continue;
            };
            match groups.last_mut() {
                Some((gz, g)) if *gz == z => g.push((val, v)),
                _ => groups.push((z, vec![(val, v)])),
            }
        }
        let kind = ftype.unwrap_or_else(|| infer_function_type(groups.first().map_or(&[][..], |g| g.1.as_slice())));
        let outs: Vec<(f64, Node)> = groups.iter().map(|(z, g)| (*z, build_function(kind, input(), g, base))).collect();
        let zoom = Node::Call(Op::Zoom, vec![]);
        if kind == "exponential" { Node::Interp(base, Box::new(zoom), outs) } else { step_from(zoom, outs) }
    } else {
        build_function(ftype.unwrap_or_else(|| infer_function_type(&stops)), input(), &stops, base)
    };
    with_default(node)
}

fn infer_function_type(stops: &[(&Json, &Json)]) -> &'static str {
    match stops.first() {
        Some((k, _)) if k.is_string() || k.is_boolean() => "categorical",
        Some((_, v))
            if v.is_number()
                || v.as_str().is_some_and(|s| parse_color(s).is_some())
                || v.as_array().is_some_and(|a| !a.is_empty() && a.iter().all(Json::is_number)) =>
        {
            "exponential"
        }
        _ => "interval",
    }
}

fn build_function(kind: &str, input: Node, stops: &[(&Json, &Json)], base: f64) -> Node {
    let out = |v: &Json| Node::Lit(Val::from_json(v));
    let numeric = || stops.iter().filter_map(|(k, v)| Some((k.as_f64()?, out(v)))).collect::<Vec<_>>();
    match kind {
        "categorical" => {
            let arms = stops.iter().map(|(k, v)| (vec![Val::from_json(k)], out(v))).collect();
            Node::Match(Box::new(input), arms, Box::new(null()))
        }
        "interval" => step_from(input, numeric()),
        _ => Node::Interp(base, Box::new(input), numeric()),
    }
}

fn step_from(input: Node, stops: Vec<(f64, Node)>) -> Node {
    let mut it = stops.into_iter();
    let Some((_, first)) = it.next() else { return null() };
    Node::Step(Box::new(input), Box::new(first), it.collect())
}

type Vars = Vec<(String, Val)>;

fn eval(n: &Node, f: &FeatureCtx, vars: &mut Vars) -> Val {
    match n {
        Node::Lit(v) => v.clone(),
        Node::Call(op, args) => call(*op, args, f, vars),
        Node::Match(input, arms, default) => {
            let v = eval(input, f, vars);
            let hit = arms.iter().find(|(labels, _)| labels.contains(&v)).map_or(&**default, |(_, o)| o);
            eval(hit, f, vars)
        }
        Node::Step(input, first, stops) => {
            let Some(x) = eval(input, f, vars).as_f64() else { return Val::Null };
            let mut out: &Node = first;
            for (k, o) in stops {
                if x < *k {
                    break;
                }
                out = o;
            }
            eval(out, f, vars)
        }
        Node::Interp(base, input, stops) => match eval(input, f, vars).as_f64() {
            Some(x) => interpolate(*base, x, stops, f, vars),
            None => Val::Null,
        },
        Node::Let(binds, body) => {
            let mark = vars.len();
            for (k, e) in binds {
                let v = eval(e, f, vars);
                vars.push((k.clone(), v));
            }
            let r = eval(body, f, vars);
            vars.truncate(mark);
            r
        }
        Node::Var(name) => vars.iter().rev().find(|(k, _)| k == name).map_or(Val::Null, |(_, v)| v.clone()),
        Node::Tokens(inner) => match eval(inner, f, vars) {
            Val::Str(s) => Val::Str(substitute_tokens(&s, f)),
            v => v,
        },
        Node::LegacyGet(k) => match k.as_str() {
            "$type" => Val::Str(geom_name(f.kind).into()),
            "$id" => f.id.map_or(Val::Null, |i| Val::Num(i as f64)),
            _ => f.prop(k).map_or(Val::Null, Val::from_prop),
        },
        Node::LegacyHas(k) => Val::Bool(match k.as_str() {
            "$type" => true,
            "$id" => f.id.is_some(),
            _ => f.prop(k).is_some(),
        }),
        Node::LegacyIn(key, set) => {
            if let Node::LegacyGet(k) = &**key {
                return Val::Bool(set.iter().any(|v| reads(k, true, f, v)));
            }
            let v = eval(key, f, vars);
            Val::Bool(set.contains(&v))
        }
    }
}

/// Whether the feature's `key` (a legacy key, `$type` and `$id` among them, when `legacy`) is
/// `v`, without making a value of it: filters compare thousands of features with every layer.
fn reads(key: &str, legacy: bool, f: &FeatureCtx, v: &Val) -> bool {
    match (key, legacy) {
        ("$type", true) => matches!(v, Val::Str(s) if s == geom_name(f.kind)),
        ("$id", true) => match f.id {
            Some(i) => *v == Val::Num(i as f64),
            None => *v == Val::Null,
        },
        _ => match f.prop(key) {
            Some(Value::String(s)) => matches!(v, Val::Str(t) if t == s),
            Some(Value::Number(n)) => matches!(v, Val::Num(m) if m == n),
            Some(Value::Bool(b)) => matches!(v, Val::Bool(c) if c == b),
            None => *v == Val::Null,
        },
    }
}

fn interpolate(base: f64, x: f64, stops: &[(f64, Node)], f: &FeatureCtx, vars: &mut Vars) -> Val {
    let (Some((z0, first)), Some((zn, last))) = (stops.first(), stops.last()) else {
        return Val::Null;
    };
    if x <= *z0 {
        return eval(first, f, vars);
    }
    if x >= *zn {
        return eval(last, f, vars);
    }
    for w in stops.windows(2) {
        if let [(lo, a), (hi, b)] = w
            && x < *hi
        {
            let t = interp_t(base, x - lo, hi - lo);
            let (va, vb) = (eval(a, f, vars), eval(b, f, vars));
            return lerp_val(&va, &vb, t);
        }
    }
    eval(last, f, vars)
}

fn interp_t(base: f64, pos: f64, span: f64) -> f64 {
    if span.is_nan() || span <= 0.0 {
        return 0.0;
    }
    let linear = pos / span;
    let t = if (base - 1.0).abs() < 1e-9 { linear } else { (base.powf(pos) - 1.0) / (base.powf(span) - 1.0) };
    let t = if t.is_finite() { t } else { linear };
    if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 }
}

fn lerp_val(a: &Val, b: &Val, t: f64) -> Val {
    match (a, b) {
        (Val::Num(x), Val::Num(y)) => Val::Num(x + (y - x) * t),
        (Val::Arr(x), Val::Arr(y)) if x.len() == y.len() && x.iter().chain(y).all(|v| matches!(v, Val::Num(_))) => {
            Val::Arr(x.iter().zip(y).map(|(p, q)| lerp_val(p, q, t)).collect())
        }
        _ => match (a.as_color(), b.as_color()) {
            (Some(p), Some(q)) => Val::Color(lerp_color(p, q, t)),
            _ if t < 1.0 => a.clone(),
            _ => b.clone(),
        },
    }
}

fn order(a: &Val, b: &Val) -> Option<Ordering> {
    match (a, b) {
        (Val::Num(x), Val::Num(y)) => x.partial_cmp(y),
        (Val::Str(x), Val::Str(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn nums(v: &[Val]) -> Option<Vec<f64>> {
    v.iter()
        .map(|x| match x {
            Val::Num(n) => Some(*n),
            _ => None,
        })
        .collect()
}

/// Negative indices count from the end (JS `slice` semantics).
fn norm_index(i: f64, len: usize) -> usize {
    let l = len as f64;
    let i = if i < 0.0 { (l + i).max(0.0) } else { i.min(l) };
    if i.is_nan() { 0 } else { i as usize }
}

fn call(op: Op, args: &[Node], f: &FeatureCtx, vars: &mut Vars) -> Val {
    use Op::*;
    // Lazily evaluated forms.
    match op {
        All => return Val::Bool(args.iter().all(|a| eval(a, f, vars) == Val::Bool(true))),
        Any => return Val::Bool(args.iter().any(|a| eval(a, f, vars) == Val::Bool(true))),
        NoneOf => return Val::Bool(!args.iter().any(|a| eval(a, f, vars) == Val::Bool(true))),
        Case => {
            for p in args.as_chunks::<2>().0 {
                if let [c, o] = p
                    && eval(c, f, vars) == Val::Bool(true)
                {
                    return eval(o, f, vars);
                }
            }
            return match args.len() % 2 {
                1 => args.last().map_or(Val::Null, |d| eval(d, f, vars)),
                _ => Val::Null,
            };
        }
        Coalesce => {
            for a in args {
                let v = eval(a, f, vars);
                if v != Val::Null {
                    return v;
                }
            }
            return Val::Null;
        }
        // A feature's property against a literal, compared where it is.
        Eq | Ne => {
            let read = match args {
                [Node::LegacyGet(k), Node::Lit(v)] => Some(reads(k, true, f, v)),
                [Node::Call(Get, key), Node::Lit(v)] => match key.as_slice() {
                    [Node::Lit(Val::Str(k))] => Some(reads(k, false, f, v)),
                    _ => None,
                },
                _ => None,
            };
            if let Some(is) = read {
                return Val::Bool(is == (op == Eq));
            }
        }
        _ => {}
    }
    let v: Vec<Val> = args.iter().map(|a| eval(a, f, vars)).collect();
    let arg = |i: usize| v.get(i).unwrap_or(&NULL);
    let num = |i: usize| match arg(i) {
        Val::Num(n) => Some(*n),
        _ => None,
    };
    let unary = |g: fn(f64) -> f64| num(0).map_or(Val::Null, |x| Val::Num(g(x)));
    let binary = |g: fn(f64, f64) -> f64| match (num(0), num(1)) {
        (Some(a), Some(b)) => Val::Num(g(a, b)),
        _ => Val::Null,
    };
    let first_of = |pred: fn(&Val) -> bool| v.iter().find(|x| pred(x)).cloned().unwrap_or(Val::Null);
    match op {
        Get => match arg(0) {
            Val::Str(k) => f.prop(k).map_or(Val::Null, Val::from_prop),
            _ => Val::Null,
        },
        Has => Val::Bool(matches!(arg(0), Val::Str(k) if f.prop(k).is_some())),
        Id => f.id.map_or(Val::Null, |i| Val::Num(i as f64)),
        GeomType => Val::Str(geom_name(f.kind).into()),
        Zoom => Val::Num(f64::from(f.zoom)),
        Elevation => Val::Null,
        Eq => Val::Bool(arg(0) == arg(1)),
        Ne => Val::Bool(arg(0) != arg(1)),
        Lt | Le | Gt | Ge => {
            let o = order(arg(0), arg(1));
            Val::Bool(match op {
                Lt => o == Some(Ordering::Less),
                Le => matches!(o, Some(Ordering::Less | Ordering::Equal)),
                Gt => o == Some(Ordering::Greater),
                _ => matches!(o, Some(Ordering::Greater | Ordering::Equal)),
            })
        }
        Not => match arg(0) {
            Val::Bool(b) => Val::Bool(!b),
            _ => Val::Null,
        },
        In => Val::Bool(match (arg(0), arg(1)) {
            (Val::Str(n), Val::Str(h)) => h.contains(n.as_str()),
            (n @ (Val::Num(_) | Val::Bool(_)), Val::Str(h)) => h.contains(&n.to_text()),
            (n, Val::Arr(h)) => h.contains(n),
            _ => false,
        }),
        ToNumber => v.iter().find_map(Val::to_number).map_or(Val::Null, Val::Num),
        ToString => Val::Str(arg(0).to_text()),
        ToBoolean => Val::Bool(arg(0).truthy()),
        ToColor => v.iter().find_map(Val::to_color).map_or(Val::Null, Val::Color),
        IsNumber => first_of(|x| matches!(x, Val::Num(_))),
        IsString => first_of(|x| matches!(x, Val::Str(_))),
        IsBoolean => first_of(|x| matches!(x, Val::Bool(_))),
        IsArray => match v.last() {
            Some(a @ Val::Arr(_)) => a.clone(),
            _ => Val::Null,
        },
        TypeOf => Val::Str(arg(0).type_name().into()),
        Concat | Format => {
            let mut s = String::new();
            for x in &v {
                push_capped(&mut s, &x.to_text());
            }
            Val::Str(s)
        }
        Upcase => arg(0).as_str().map_or(Val::Null, |s| Val::Str(s.to_uppercase())),
        Downcase => arg(0).as_str().map_or(Val::Null, |s| Val::Str(s.to_lowercase())),
        Image => match (arg(0).as_str(), f.images) {
            (Some(name), Some(sprite)) if sprite.has(name) => Val::Str(name.to_owned()),
            _ => Val::Null,
        },
        Add => nums(&v).map_or(Val::Null, |n| Val::Num(n.iter().sum())),
        Mul => nums(&v).map_or(Val::Null, |n| Val::Num(n.iter().product())),
        Sub => match (num(0), num(1), v.len()) {
            (Some(a), _, 1) => Val::Num(-a),
            (Some(a), Some(b), 2) => Val::Num(a - b),
            _ => Val::Null,
        },
        Div => binary(|a, b| a / b),
        Rem => binary(|a, b| a % b),
        Pow => binary(f64::powf),
        Min | Max => {
            match nums(&v) {
                Some(n) if !n.is_empty() => {
                    Val::Num(n.into_iter().fold(if op == Min { f64::INFINITY } else { f64::NEG_INFINITY }, |a, b| {
                        if op == Min { a.min(b) } else { a.max(b) }
                    }))
                }
                _ => Val::Null,
            }
        }
        Abs => unary(f64::abs),
        Round => unary(f64::round),
        Floor => unary(f64::floor),
        Ceil => unary(f64::ceil),
        Sqrt => unary(f64::sqrt),
        Ln => unary(f64::ln),
        Log10 => unary(f64::log10),
        Log2 => unary(f64::log2),
        E => Val::Num(std::f64::consts::E),
        Pi => Val::Num(std::f64::consts::PI),
        Ln2 => Val::Num(std::f64::consts::LN_2),
        Length => match arg(0) {
            Val::Str(s) => Val::Num(s.chars().count() as f64),
            Val::Arr(a) => Val::Num(a.len() as f64),
            _ => Val::Null,
        },
        At => match (num(0), arg(1)) {
            (Some(i), Val::Arr(a)) if i >= 0.0 && i.fract() == 0.0 => a.get(i as usize).cloned().unwrap_or(Val::Null),
            _ => Val::Null,
        },
        IndexOf => index_of(arg(0), arg(1), num(2).unwrap_or(0.0)),
        Slice => {
            let start = num(1).unwrap_or(0.0);
            match arg(0) {
                Val::Str(s) => {
                    let len = s.chars().count();
                    let (a, b) = (norm_index(start, len), norm_index(num(2).unwrap_or(len as f64), len));
                    Val::Str(s.chars().skip(a).take(b.saturating_sub(a)).collect())
                }
                Val::Arr(x) => {
                    let (a, b) = (norm_index(start, x.len()), norm_index(num(2).unwrap_or(x.len() as f64), x.len()));
                    Val::Arr(x.get(a..b.max(a)).unwrap_or(&[]).to_vec())
                }
                _ => Val::Null,
            }
        }
        Rgb => match (num(0), num(1), num(2)) {
            (Some(r), Some(g), Some(b)) => {
                let c = |x: f64| x.clamp(0.0, 255.0).round() as u8;
                Val::Color([c(r), c(g), c(b), num(3).map_or(255, unit_u8)])
            }
            _ => Val::Null,
        },
        All | Any | NoneOf | Case | Coalesce => Val::Null,
    }
}

fn index_of(needle: &Val, hay: &Val, from: f64) -> Val {
    let found = match (needle, hay) {
        (n, Val::Str(h)) if !matches!(n, Val::Null | Val::Arr(_) | Val::Color(_)) => {
            let start = norm_index(from, h.chars().count());
            let byte = h.char_indices().nth(start).map_or(h.len(), |(b, _)| b);
            let n = n.to_text();
            h.get(byte..).and_then(|rest| rest.find(n.as_str())).and_then(|off| h.get(..byte + off)).map(|pre| pre.chars().count())
        }
        (n, Val::Arr(a)) => {
            let start = norm_index(from, a.len());
            a.iter().skip(start).position(|x| x == n).map(|p| p + start)
        }
        _ => return Val::Null,
    };
    Val::Num(found.map_or(-1.0, |i| i as f64))
}

/// Replaces legacy `{key}` tokens with property values (missing → empty).
fn substitute_tokens(s: &str, f: &FeatureCtx) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        push_capped(&mut out, rest.get(..open).unwrap_or(""));
        let after = rest.get(open + 1..).unwrap_or("");
        let Some(close) = after.find('}') else {
            push_capped(&mut out, rest.get(open..).unwrap_or(""));
            return out;
        };
        if let Some(v) = f.prop(after.get(..close).unwrap_or("")) {
            push_capped(&mut out, &Val::from_prop(v).to_text());
        }
        rest = after.get(close + 1..).unwrap_or("");
    }
    push_capped(&mut out, rest);
    out
}

// ---------------------------------------------------------------------------------------------------------------
// Style, sources, layers
// ---------------------------------------------------------------------------------------------------------------

/// A style's `sprite` array: its sprites as (id, address), at most [`MAX_SPRITES`] of the entries
/// that can be read, each id once; and what was left out, a line per reason.
fn parse_sprites(entries: &[Json]) -> (Vec<(String, String)>, Vec<String>) {
    let mut sprites: Vec<(String, String)> = Vec::new();
    let (mut unread, mut repeated, mut past) = (0usize, 0usize, 0usize);
    // An empty id or address is none: an empty address would resolve to the style's own folder.
    let given = |v: Option<&Json>| str_of(v).filter(|s| !s.is_empty());
    for s in entries {
        match (given(s.get("id")).filter(|id| id.len() <= MAX_SPRITE_ID), given(s.get("url"))) {
            (Some(id), _) if sprites.iter().any(|(i, _)| *i == id) => repeated += 1,
            (Some(_), Some(_)) if sprites.len() >= MAX_SPRITES => past += 1,
            (Some(id), Some(url)) => sprites.push((id, url)),
            _ => unread += 1,
        }
    }
    let total = entries.len();
    let mut left_out = Vec::new();
    if unread > 0 {
        left_out
            .push(format!("{unread} of {total} sprites left out, so their icons and patterns aren't drawn: no usable id or no address"));
    }
    if repeated > 0 {
        left_out.push(format!("{repeated} of {total} sprites left out, so their icons and patterns aren't drawn: another has the same id"));
    }
    if past > 0 {
        left_out.push(format!(
            "{past} of {total} sprites left out, so their icons and patterns aren't drawn: only the first {MAX_SPRITES} with a usable id of their own and an address are used"
        ));
    }
    (sprites, left_out)
}

/// A parsed style. URLs are left as given.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub name: String,
    pub sources: Vec<(String, Source)>,
    pub layers: Vec<StyleLayer>,
    pub glyphs: Option<String>,
    /// The style's sprites as (id, address). Images of the sprite with the id `default`, which
    /// a style giving just one address has, are named `<name>`; those of the others `<id>:<name>`.
    pub sprites: Vec<(String, String)>,
    /// What of its `sprite` array couldn't be read, and why.
    pub sprites_left_out: Vec<String>,
    /// How many sprites it lists, read or not.
    pub sprites_listed: usize,
    /// Where the style places the map, from its root properties.
    pub view: StyleView,
}

/// A style's root `center`, `zoom`, `bearing` and `pitch`: where MapLibre opens a map that was
/// given no position of its own. Each is kept only when it is a usable number.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StyleView {
    /// Longitude and latitude, in degrees.
    pub center: Option<[f64; 2]>,
    /// MapLibre's zoom (512-pixel tiles).
    pub zoom: Option<f64>,
    /// Degrees clockwise from north.
    pub bearing: Option<f64>,
    /// Degrees of tilt from looking straight down.
    pub pitch: Option<f64>,
}

impl StyleView {
    /// Reads the view from a style's root object.
    pub fn parse(o: &Map<String, Json>) -> StyleView {
        let num = |k: &str| o.get(k).and_then(Json::as_f64).filter(|v| v.is_finite());
        let center = o.get("center").and_then(Json::as_array).and_then(|a| match a.as_slice() {
            [lon, lat] => {
                let (lon, lat) = (lon.as_f64()?, lat.as_f64()?);
                ((-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat)).then_some([lon, lat])
            }
            _ => None,
        });
        StyleView { center, zoom: num("zoom").filter(|z| (0.0..=30.0).contains(z)), bearing: num("bearing"), pitch: num("pitch") }
    }

    /// Whether the style says nothing about where to look.
    pub fn is_empty(&self) -> bool {
        *self == StyleView::default()
    }
}

/// A style source.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Vector {
        tiles: Vec<String>,
        /// Runtime provenance of the tile templates; `None` for a user-selected template.
        from: Option<crate::address::Origin>,
        url: Option<String>,
        min_zoom: u8,
        max_zoom: u8,
        attribution: String,
    },
    Raster {
        tiles: Vec<String>,
        /// Runtime provenance of the tile templates; `None` for a user-selected template.
        from: Option<crate::address::Origin>,
        url: Option<String>,
        tile_size: u32,
        min_zoom: u8,
        max_zoom: u8,
        attribution: String,
    },
    /// Elevation tiles, for `hillshade` and `color-relief` layers.
    RasterDem {
        tiles: Vec<String>,
        /// Runtime provenance of the tile templates; `None` for a user-selected template.
        from: Option<crate::address::Origin>,
        url: Option<String>,
        tile_size: u32,
        min_zoom: u8,
        max_zoom: u8,
        attribution: String,
        encoding: DemEncoding,
    },
    /// geojson, image, video… (ignored by the renderer).
    Other,
}

/// How a `raster-dem` source's pixels give heights (its `encoding`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DemEncoding {
    /// `(R × 256 × 256 + G × 256 + B) × 0.1 − 10000` (the default).
    Mapbox,
    /// `R × 256 + G + B / 256 − 32768`.
    Terrarium,
    /// `R × red + G × green + B × blue − base` (`redFactor`, `greenFactor`, `blueFactor`, `baseShift`;
    /// 1, 1, 1 and 0 when not given). The base is taken off, as MapLibre's own encodings take
    /// off theirs (Mapbox's is `custom` with 6553.6, 25.6, 0.1 and 10000).
    Custom { red: f64, green: f64, blue: f64, base: f64 },
}

impl DemEncoding {
    fn parse(j: &Json) -> DemEncoding {
        let factor = |k: &str, d: f64| j.get(k).and_then(Json::as_f64).filter(|v| v.is_finite()).unwrap_or(d);
        match j.get("encoding").and_then(Json::as_str) {
            Some("terrarium") => DemEncoding::Terrarium,
            Some("custom") => DemEncoding::Custom {
                red: factor("redFactor", 1.0),
                green: factor("greenFactor", 1.0),
                blue: factor("blueFactor", 1.0),
                base: factor("baseShift", 0.0),
            },
            _ => DemEncoding::Mapbox,
        }
    }

    /// Whether every pixel's height fits in an `f32`: a custom encoding's factors may be too
    /// large.
    pub fn fits(self) -> bool {
        match self {
            DemEncoding::Mapbox | DemEncoding::Terrarium => true,
            DemEncoding::Custom { red, green, blue, base } => {
                255.0 * (red.abs() + green.abs() + blue.abs()) + base.abs() < f64::from(f32::MAX)
            }
        }
    }

    /// The height, in metres, of a pixel's colour.
    pub fn height(self, [r, g, b]: [u8; 3]) -> f32 {
        let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
        let h = match self {
            DemEncoding::Mapbox => (r * 65536.0 + g * 256.0 + b) * 0.1 - 10000.0,
            DemEncoding::Terrarium => r * 256.0 + g + b / 256.0 - 32768.0,
            DemEncoding::Custom { red, green, blue, base } => r * red + g * green + b * blue - base,
        };
        h as f32
    }
}

fn str_of(v: Option<&Json>) -> Option<String> {
    v.and_then(Json::as_str).map(str::to_owned)
}

fn strings(v: Option<&Json>) -> Vec<String> {
    v.and_then(Json::as_array).into_iter().flatten().filter_map(Json::as_str).take(MAX_URLS).map(str::to_owned).collect()
}

fn zoom_u8(v: Option<&Json>, default: u8) -> u8 {
    v.and_then(Json::as_f64).filter(|z| z.is_finite()).map_or(default, |z| z.clamp(0.0, 30.0).round() as u8)
}

impl Source {
    /// Parses a `sources` entry.
    pub fn parse(j: &Json) -> Source {
        let get = |k: &str| j.get(k);
        let (tiles, url, attribution) = (strings(get("tiles")), str_of(get("url")), str_of(get("attribution")).unwrap_or_default());
        let (min_zoom, max_zoom) = (zoom_u8(get("minzoom"), 0), zoom_u8(get("maxzoom"), 22));
        match get("type").and_then(Json::as_str) {
            Some("vector") => Source::Vector { tiles, from: None, url, min_zoom, max_zoom, attribution },
            Some("raster") => {
                let tile_size = get("tileSize").and_then(Json::as_u64).map_or(512, |t| t.clamp(1, 4096) as u32);
                Source::Raster { tiles, from: None, url, tile_size, min_zoom, max_zoom, attribution }
            }
            Some("raster-dem") => {
                let tile_size = get("tileSize").and_then(Json::as_u64).map_or(512, |t| t.clamp(1, 4096) as u32);
                Source::RasterDem { tiles, from: None, url, tile_size, min_zoom, max_zoom, attribution, encoding: DemEncoding::parse(j) }
            }
            _ => Source::Other,
        }
    }

    /// Fills tiles, zoom range and attribution from a TileJSON document (fields it lacks are kept).
    /// A `raster-dem` source keeps its own `encoding`, as MapLibre does.
    pub fn apply_tilejson(&mut self, tilejson: &Json) {
        match self {
            Source::Vector { tiles, min_zoom, max_zoom, attribution, .. }
            | Source::Raster { tiles, min_zoom, max_zoom, attribution, .. }
            | Source::RasterDem { tiles, min_zoom, max_zoom, attribution, .. } => {
                let t = strings(tilejson.get("tiles"));
                if !t.is_empty() {
                    *tiles = t;
                }
                *min_zoom = zoom_u8(tilejson.get("minzoom"), *min_zoom);
                *max_zoom = zoom_u8(tilejson.get("maxzoom"), *max_zoom);
                if let Some(a) = str_of(tilejson.get("attribution")) {
                    *attribution = a;
                }
            }
            Source::Other => {}
        }
    }
}

impl Style {
    /// Parses a style document. Fails only when it is not an object with a `layers` array; bad layers are skipped.
    pub fn parse(json: &Json) -> Result<Style, String> {
        let o = json.as_object().ok_or("style is not a JSON object")?;
        let layers = o.get("layers").and_then(Json::as_array).ok_or("style has no layers array")?;
        let sources = o
            .get("sources")
            .and_then(Json::as_object)
            .into_iter()
            .flatten()
            .take(MAX_SOURCES)
            .map(|(k, v)| (k.clone(), Source::parse(v)))
            .collect();
        let (sprites, sprites_left_out) = match o.get("sprite") {
            // (An empty address would resolve to the style's own folder.)
            Some(Json::String(s)) if !s.is_empty() => (vec![("default".to_owned(), s.clone())], Vec::new()),
            Some(Json::Array(a)) => parse_sprites(a),
            _ => (Vec::new(), Vec::new()),
        };
        Ok(Style {
            name: str_of(o.get("name")).unwrap_or_default(),
            sources,
            layers: layers.iter().take(MAX_LAYERS).filter_map(StyleLayer::parse).collect(),
            glyphs: str_of(o.get("glyphs")),
            sprites,
            sprites_left_out,
            sprites_listed: o.get("sprite").map_or(0, |s| s.as_array().map_or(1, Vec::len)),
            view: StyleView::parse(o),
        })
    }

    /// Source by id.
    pub fn source(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|(k, _)| k == id).map(|(_, s)| s)
    }
}

/// Layer type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayerKind {
    Background,
    Fill,
    Line,
    Symbol,
    Raster,
    Circle,
    FillExtrusion,
    /// Shading from a `raster-dem` source's slopes.
    Hillshade,
    /// Colours by height from a `raster-dem` source.
    ColorRelief,
    Other,
}

/// `hillshade-method`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HillshadeMethod {
    #[default]
    Standard,
    Basic,
    Combined,
    Igor,
    Multidirectional,
}

/// A hillshade layer's paint (see [`StyleLayer::hillshade`]). The light's direction is degrees
/// clockwise from north and its altitude degrees above the horizon; the multidirectional method
/// takes a light from each direction, with the altitude and colours at the same place in their
/// lists (the last one standing in for those missing).
#[derive(Clone, Debug, PartialEq)]
pub struct Hillshade {
    pub method: HillshadeMethod,
    /// 0 to 1 (default 0.5).
    pub exaggeration: f32,
    /// Default 335.
    pub directions: Vec<f32>,
    /// Default 45.
    pub altitudes: Vec<f32>,
    /// Default black.
    pub shadows: Vec<Rgba>,
    /// Default white.
    pub highlights: Vec<Rgba>,
    /// Steep slopes' colour (default black; the standard method's only).
    pub accent: Rgba,
}

/// `line-cap`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

/// `line-join`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineJoin {
    Bevel,
    Round,
    #[default]
    Miter,
}

/// `symbol-placement`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SymbolPlacement {
    #[default]
    Point,
    Line,
    LineCenter,
}

/// `text-transform`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextTransform {
    #[default]
    None,
    Uppercase,
    Lowercase,
}

/// A style layer with compiled filter, paint and layout.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleLayer {
    pub id: String,
    pub kind: LayerKind,
    pub source: Option<String>,
    pub source_layer: Option<String>,
    pub min_zoom: f32,
    pub max_zoom: f32,
    /// `layout.visibility != "none"`.
    pub visible: bool,
    filter: Option<Expr>,
    paint: HashMap<String, Expr>,
    layout: HashMap<String, Expr>,
    /// See [`StyleLayer::color_relief_undrawable`]; worked out once, when parsed.
    relief_undrawable: bool,
}

impl StyleLayer {
    /// Parses one `layers` entry; `None` if it is not an object.
    pub fn parse(j: &Json) -> Option<StyleLayer> {
        let o = j.as_object()?;
        let kind = match o.get("type").and_then(Json::as_str) {
            Some("background") => LayerKind::Background,
            Some("fill") => LayerKind::Fill,
            Some("line") => LayerKind::Line,
            Some("symbol") => LayerKind::Symbol,
            Some("raster") => LayerKind::Raster,
            Some("circle") => LayerKind::Circle,
            Some("fill-extrusion") => LayerKind::FillExtrusion,
            Some("hillshade") => LayerKind::Hillshade,
            Some("color-relief") => LayerKind::ColorRelief,
            _ => LayerKind::Other,
        };
        let layout_json = o.get("layout").and_then(Json::as_object);
        let props = |m: Option<&Map<String, Json>>| -> HashMap<String, Expr> {
            m.into_iter()
                .flatten()
                .take(MAX_PROPS)
                .map(|(k, v)| {
                    let n = compile_property(v);
                    let n = if (k == "text-field" || k == "icon-image") && !v.is_array() { Node::Tokens(Box::new(n)) } else { n };
                    (k.clone(), Expr(n))
                })
                .collect()
        };
        let zoom = |k: &str, d: f32| o.get(k).and_then(Json::as_f64).map(|z| z as f32).filter(|z| z.is_finite()).unwrap_or(d);
        let mut layer = StyleLayer {
            id: str_of(o.get("id")).unwrap_or_default(),
            kind,
            source: str_of(o.get("source")),
            source_layer: str_of(o.get("source-layer")),
            min_zoom: zoom("minzoom", 0.0),
            max_zoom: zoom("maxzoom", 24.0),
            visible: layout_json.and_then(|l| l.get("visibility")).and_then(Json::as_str) != Some("none"),
            filter: o.get("filter").filter(|f| !f.is_null()).map(Expr::filter),
            paint: props(o.get("paint").and_then(Json::as_object)),
            layout: props(layout_json),
            relief_undrawable: false,
        };
        layer.relief_undrawable = layer.find_relief_undrawable();
        Some(layer)
    }

    /// Filter result (true without a filter).
    pub fn matches(&self, f: &FeatureCtx) -> bool {
        self.filter.as_ref().is_none_or(|e| e.eval(f) == Val::Bool(true))
    }

    /// Visible and `minzoom <= zoom < maxzoom`.
    pub fn visible_at(&self, zoom: f32) -> bool {
        self.visible && zoom >= self.min_zoom && zoom < self.max_zoom
    }

    /// Raw paint value; `None` when the property is absent.
    pub fn paint_value(&self, name: &str, f: &FeatureCtx) -> Option<Val> {
        self.paint.get(name).map(|e| e.eval(f))
    }

    /// Raw layout value; `None` when the property is absent.
    pub fn layout_value(&self, name: &str, f: &FeatureCtx) -> Option<Val> {
        self.layout.get(name).map(|e| e.eval(f))
    }

    /// Whether a layout property is set (e.g. `icon-image`).
    pub fn has_layout(&self, name: &str) -> bool {
        self.layout.contains_key(name)
    }

    /// Whether a paint property is set (e.g. `fill-pattern`).
    pub fn has_paint(&self, name: &str) -> bool {
        self.paint.contains_key(name)
    }

    /// Whether the paint property `name` depends on the zoom.
    pub fn paint_uses_zoom(&self, name: &str) -> bool {
        self.paint.get(name).is_some_and(Expr::uses_zoom)
    }

    /// A conservative width at this zoom, including the property's default when evaluation
    /// fails. `None` leaves an unbounded expression to the drawing's full margin.
    pub(crate) fn paint_width_bound(&self, name: &str, zoom: f32, default: f32) -> Option<f32> {
        let f = FeatureCtx::zoom_only(zoom);
        let Some(expr) = self.paint.get(name) else { return Some(default) };
        if expr.feature_free() {
            return Some(self.width(name, &f, default));
        }
        numeric_upper(&expr.0, &f).map(|v| (v as f32).max(default)).filter(|v| v.is_finite())
    }

    /// Whether a layout property can differ between features at the same zoom.
    pub(crate) fn layout_uses_features(&self, name: &str) -> bool {
        self.layout.get(name).is_some_and(|expr| !expr.feature_free())
    }

    /// Whether the paint property `name` asks the sprite for an image.
    pub fn paint_uses_image(&self, name: &str) -> bool {
        self.paint.get(name).is_some_and(Expr::uses_image)
    }

    /// Whether it draws icons of the style's sprites: a label's. (Another kind of layer may name
    /// one, and draw none.)
    pub fn draws_icons(&self) -> bool {
        self.kind == LayerKind::Symbol && self.has_layout("icon-image")
    }

    /// Whether it fills with patterns of the style's sprites. (A line painted with a pattern isn't
    /// drawn at all: see `draw::drawable`.)
    pub fn draws_patterns(&self) -> bool {
        matches!(self.kind, LayerKind::Fill | LayerKind::FillExtrusion) && PATTERNS.iter().any(|p| self.has_paint(p))
    }

    /// Whether it draws images of the style's sprites.
    pub fn draws_images(&self) -> bool {
        self.draws_icons() || self.draws_patterns()
    }

    /// Whether every paint and layout property of the layer is the same for every feature at a
    /// zoom (see [`Expr::feature_free`]).
    pub fn paint_feature_free(&self) -> bool {
        self.paint.values().chain(self.layout.values()).all(Expr::feature_free)
    }

    fn pval(&self, name: &str, f: &FeatureCtx) -> Val {
        self.paint_value(name, f).unwrap_or(Val::Null)
    }

    fn lval(&self, name: &str, f: &FeatureCtx) -> Val {
        self.layout_value(name, f).unwrap_or(Val::Null)
    }

    fn pnum(&self, name: &str, f: &FeatureCtx) -> Option<f32> {
        self.pval(name, f).as_f64().map(|v| v as f32).filter(|v| v.is_finite())
    }

    fn pcolor(&self, name: &str, f: &FeatureCtx) -> Option<Rgba> {
        self.pval(name, f).as_color()
    }

    fn opacity(&self, name: &str, f: &FeatureCtx) -> f32 {
        self.pnum(name, f).map_or(1.0, |v| v.clamp(0.0, 1.0))
    }

    fn width(&self, name: &str, f: &FeatureCtx, default: f32) -> f32 {
        self.pnum(name, f).map_or(default, |v| v.max(0.0))
    }

    fn lstr(&self, name: &str, f: &FeatureCtx) -> Option<String> {
        match self.lval(name, f) {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }

    /// `background-color` (default black).
    pub fn background_color(&self, zoom: f32) -> Option<Rgba> {
        Some(self.pcolor("background-color", &FeatureCtx::zoom_only(zoom)).unwrap_or(BLACK))
    }

    /// `background-opacity` (default 1).
    pub fn background_opacity(&self, zoom: f32) -> f32 {
        self.opacity("background-opacity", &FeatureCtx::zoom_only(zoom))
    }

    fn fill_prop(&self, extrusion: &'static str, fill: &'static str) -> &'static str {
        if self.kind == LayerKind::FillExtrusion { extrusion } else { fill }
    }

    /// `fill-color` / `fill-extrusion-color` (default black).
    pub fn fill_color(&self, f: &FeatureCtx) -> Option<Rgba> {
        Some(self.pcolor(self.fill_prop("fill-extrusion-color", "fill-color"), f).unwrap_or(BLACK))
    }

    /// `fill-opacity` / `fill-extrusion-opacity` (default 1).
    pub fn fill_opacity(&self, f: &FeatureCtx) -> f32 {
        self.opacity(self.fill_prop("fill-extrusion-opacity", "fill-opacity"), f)
    }

    /// `fill-outline-color`; `None` = use the fill color.
    pub fn fill_outline_color(&self, f: &FeatureCtx) -> Option<Rgba> {
        self.pcolor("fill-outline-color", f)
    }

    /// `line-color` (default black).
    pub fn line_color(&self, f: &FeatureCtx) -> Option<Rgba> {
        Some(self.pcolor("line-color", f).unwrap_or(BLACK))
    }

    /// `line-width` (default 1).
    pub fn line_width(&self, f: &FeatureCtx) -> f32 {
        self.width("line-width", f, 1.0)
    }

    /// `line-opacity` (default 1).
    pub fn line_opacity(&self, f: &FeatureCtx) -> f32 {
        self.opacity("line-opacity", f)
    }

    /// `line-gap-width` (default 0).
    pub fn line_gap_width(&self, f: &FeatureCtx) -> f32 {
        self.width("line-gap-width", f, 0.0)
    }

    /// `line-offset` (default 0).
    pub fn line_offset(&self, f: &FeatureCtx) -> f32 {
        self.pnum("line-offset", f).unwrap_or(0.0)
    }

    /// `line-dasharray` in line widths; `None` when absent or invalid (negative, all zero).
    pub fn line_dasharray(&self, f: &FeatureCtx) -> Option<Vec<f32>> {
        let Val::Arr(a) = self.pval("line-dasharray", f) else { return None };
        let d: Vec<f32> = a.iter().take(MAX_DASHES).map(|v| v.as_f64().map(|x| x as f32)).collect::<Option<_>>()?;
        (!d.is_empty() && d.iter().all(|x| x.is_finite() && *x >= 0.0) && d.iter().sum::<f32>() > 0.0).then_some(d)
    }

    /// `line-cap` (default butt).
    pub fn line_cap(&self, f: &FeatureCtx) -> LineCap {
        match self.lstr("line-cap", f).as_deref() {
            Some("round") => LineCap::Round,
            Some("square") => LineCap::Square,
            _ => LineCap::Butt,
        }
    }

    /// `line-join` (default miter).
    pub fn line_join(&self, f: &FeatureCtx) -> LineJoin {
        match self.lstr("line-join", f).as_deref() {
            Some("round") => LineJoin::Round,
            Some("bevel") => LineJoin::Bevel,
            _ => LineJoin::Miter,
        }
    }

    /// `circle-color` (default black).
    pub fn circle_color(&self, f: &FeatureCtx) -> Option<Rgba> {
        Some(self.pcolor("circle-color", f).unwrap_or(BLACK))
    }

    /// `circle-radius` (default 5).
    pub fn circle_radius(&self, f: &FeatureCtx) -> f32 {
        self.width("circle-radius", f, 5.0)
    }

    /// `circle-opacity` (default 1).
    pub fn circle_opacity(&self, f: &FeatureCtx) -> f32 {
        self.opacity("circle-opacity", f)
    }

    /// `circle-stroke-color`; `None` when absent.
    pub fn circle_stroke_color(&self, f: &FeatureCtx) -> Option<Rgba> {
        self.pcolor("circle-stroke-color", f)
    }

    /// `circle-stroke-width` (default 0).
    pub fn circle_stroke_width(&self, f: &FeatureCtx) -> f32 {
        self.width("circle-stroke-width", f, 0.0)
    }

    /// `raster-opacity` (default 1).
    pub fn raster_opacity(&self, zoom: f32) -> f32 {
        self.opacity("raster-opacity", &FeatureCtx::zoom_only(zoom))
    }

    /// `color-relief-color` as colours by height; `None` when absent or not one the renderer
    /// can draw (see [`Expr::elevation_ramp`]).
    pub fn color_relief_ramp(&self, zoom: f32) -> Option<ElevationRamp> {
        self.paint.get("color-relief-color")?.elevation_ramp(zoom)
    }

    /// Whether it is a shown `color-relief` layer that RustGIS can't colour at some zoom it
    /// shows at (its `color-relief-color` isn't a [`color_relief_ramp`](Self::color_relief_ramp)
    /// there), and so skips there.
    pub fn color_relief_undrawable(&self) -> bool {
        self.relief_undrawable
    }

    /// Works out [`color_relief_undrawable`](Self::color_relief_undrawable): the colours are
    /// checked at each zoom from 0 to [`MAX_STYLE_ZOOM`] the layer shows at that the expression
    /// names (the stops and thresholds of zoom steps, ramps and comparisons), and on a grid of
    /// [`ZOOM_GRID`] steps to a zoom for anything worked out from the zoom between them.
    fn find_relief_undrawable(&self) -> bool {
        if self.kind != LayerKind::ColorRelief || !self.visible {
            return false;
        }
        let shown = |z: &f32| (0.0..=MAX_STYLE_ZOOM).contains(z) && self.visible_at(*z);
        let Some(expr) = self.paint.get("color-relief-color") else {
            return (0..=MAX_STYLE_ZOOM as u32).map(|z| z as f32).chain([self.min_zoom]).any(|z| shown(&z));
        };
        let lo = self.min_zoom.clamp(0.0, MAX_STYLE_ZOOM);
        if !expr.uses_zoom() {
            return [lo, MAX_STYLE_ZOOM].iter().any(shown) && expr.elevation_ramp(lo).is_none();
        }
        let mut named = Vec::new();
        numbers(&expr.0, &mut named);
        let grid = (0..=((MAX_STYLE_ZOOM - lo) * ZOOM_GRID) as u32).map(|i| lo + i as f32 / ZOOM_GRID);
        let named = named.into_iter().filter(|z| z.is_finite()).map(|z| z as f32);
        [lo, MAX_STYLE_ZOOM].into_iter().chain(grid).chain(named).filter(shown).any(|z| expr.elevation_ramp(z).is_none())
    }

    /// `color-relief-opacity` (default 1).
    pub fn color_relief_opacity(&self, zoom: f32) -> f32 {
        self.opacity("color-relief-opacity", &FeatureCtx::zoom_only(zoom))
    }

    /// Whether `resampling` is `nearest` (default `linear`): heights are read from the nearest
    /// pixel rather than blended, which keeps category codes from blending into heights.
    pub fn resampling_nearest(&self, zoom: f32) -> bool {
        self.pval("resampling", &FeatureCtx::zoom_only(zoom)).as_str() == Some("nearest")
    }

    /// The `hillshade-*` paint properties at `zoom`.
    pub fn hillshade(&self, zoom: f32) -> Hillshade {
        let f = FeatureCtx::zoom_only(zoom);
        // A number or colour, or for the multidirectional method a list of them: one that isn't
        // usable is the default, so the lights keep their places in each list.
        fn many<T: Copy>(v: Val, one: impl Fn(&Val) -> Option<T>, default: T) -> Vec<T> {
            let list: Vec<T> = match v {
                Val::Arr(a) => a.iter().take(MAX_LIGHTS).map(|v| one(v).unwrap_or(default)).collect(),
                v => vec![one(&v).unwrap_or(default)],
            };
            if list.is_empty() { vec![default] } else { list }
        }
        let num = |v: &Val| v.as_f64().map(|v| v as f32).filter(|v| v.is_finite());
        let colors = |name: &str, d: Rgba| many(self.pval(name, &f), Val::as_color, d);
        Hillshade {
            method: match self.pval("hillshade-method", &f).as_str() {
                Some("basic") => HillshadeMethod::Basic,
                Some("combined") => HillshadeMethod::Combined,
                Some("igor") => HillshadeMethod::Igor,
                Some("multidirectional") => HillshadeMethod::Multidirectional,
                _ => HillshadeMethod::Standard,
            },
            exaggeration: self.pnum("hillshade-exaggeration", &f).map_or(0.5, |v| v.clamp(0.0, 1.0)),
            directions: many(self.pval("hillshade-illumination-direction", &f), num, 335.0),
            altitudes: many(self.pval("hillshade-illumination-altitude", &f), num, 45.0),
            shadows: colors("hillshade-shadow-color", BLACK),
            highlights: colors("hillshade-highlight-color", [255, 255, 255, 255]),
            accent: self.pcolor("hillshade-accent-color", &f).unwrap_or(BLACK),
        }
    }

    /// `text-field` as plain text (tokens substituted, formatting and images dropped, `text-transform` applied,
    /// trimmed); `None` when empty.
    pub fn text_field(&self, f: &FeatureCtx) -> Option<String> {
        let s = match self.lval("text-field", f) {
            Val::Null | Val::Arr(_) => return None,
            Val::Str(s) => s,
            v => v.to_text(),
        };
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        Some(match self.text_transform(f) {
            TextTransform::None => s.to_owned(),
            TextTransform::Uppercase => s.to_uppercase(),
            TextTransform::Lowercase => s.to_lowercase(),
        })
    }

    /// `text-size` (default 16).
    pub fn text_size(&self, f: &FeatureCtx) -> f32 {
        self.lval("text-size", f).as_f64().map(|v| v as f32).filter(|v| v.is_finite()).map_or(16.0, |v| v.max(0.0))
    }

    /// `text-color` (default black).
    pub fn text_color(&self, f: &FeatureCtx) -> Option<Rgba> {
        Some(self.pcolor("text-color", f).unwrap_or(BLACK))
    }

    /// `text-halo-color`; `None` when absent (spec default transparent).
    pub fn text_halo_color(&self, f: &FeatureCtx) -> Option<Rgba> {
        self.pcolor("text-halo-color", f)
    }

    /// `text-halo-width` (default 0).
    pub fn text_halo_width(&self, f: &FeatureCtx) -> f32 {
        self.width("text-halo-width", f, 0.0)
    }

    /// `text-transform` (default none).
    pub fn text_transform(&self, f: &FeatureCtx) -> TextTransform {
        match self.lstr("text-transform", f).as_deref() {
            Some("uppercase") => TextTransform::Uppercase,
            Some("lowercase") => TextTransform::Lowercase,
            _ => TextTransform::None,
        }
    }

    /// `symbol-placement` (default point).
    pub fn symbol_placement(&self, f: &FeatureCtx) -> SymbolPlacement {
        match self.lstr("symbol-placement", f).as_deref() {
            Some("line") => SymbolPlacement::Line,
            Some("line-center") => SymbolPlacement::LineCenter,
            _ => SymbolPlacement::Point,
        }
    }

    /// `symbol-sort-key`.
    pub fn symbol_sort_key(&self, f: &FeatureCtx) -> Option<f32> {
        self.lval("symbol-sort-key", f).as_f64().map(|v| v as f32).filter(|v| v.is_finite())
    }

    /// `text-font` stack (default `["Open Sans Regular", "Arial Unicode MS Regular"]`).
    pub fn text_font(&self, f: &FeatureCtx) -> Vec<String> {
        match self.lval("text-font", f) {
            Val::Arr(a) if !a.is_empty() => a.into_iter().filter_map(|v| if let Val::Str(s) = v { Some(s) } else { None }).collect(),
            _ => vec!["Open Sans Regular".into(), "Arial Unicode MS Regular".into()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn props(p: &[(&str, Value)]) -> Vec<(String, Value)> {
        p.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn s(v: &str) -> Value {
        Value::String(v.into())
    }

    fn n(v: f64) -> Value {
        Value::Number(v)
    }

    fn ctx(zoom: f32, kind: GeomKind, props: &[(String, Value)]) -> FeatureCtx<'_> {
        FeatureCtx { zoom, kind, id: Some(42), props, images: None }
    }

    fn ev(j: Json, f: &FeatureCtx) -> Val {
        Expr::property(&j).eval(f)
    }

    fn filt(j: Json, f: &FeatureCtx) -> bool {
        Expr::filter(&j).eval(f) == Val::Bool(true)
    }

    fn str_val(v: &str) -> Val {
        Val::Str(v.into())
    }

    #[test]
    fn lookups_and_comparisons() {
        let p = props(&[("name", s("Main St")), ("rank", n(3.0)), ("oneway", Value::Bool(true)), ("name:latin", s("Roma"))]);
        let f = ctx(10.0, GeomKind::LineString, &p);
        assert_eq!(ev(json!(["get", "name"]), &f), str_val("Main St"));
        assert_eq!(ev(json!(["get", "name:latin"]), &f), str_val("Roma"));
        assert_eq!(ev(json!(["get", "missing"]), &f), Val::Null);
        assert_eq!(ev(json!(["get", "rank", ["properties"]]), &f), Val::Num(3.0));
        assert_eq!(ev(json!(["has", "rank"]), &f), Val::Bool(true));
        assert_eq!(ev(json!(["!has", "rank"]), &f), Val::Bool(false));
        assert_eq!(ev(json!(["id"]), &f), Val::Num(42.0));
        assert_eq!(ev(json!(["geometry-type"]), &f), str_val("LineString"));
        assert_eq!(ev(json!(["zoom"]), &f), Val::Num(10.0));
        assert_eq!(ev(json!(["properties"]), &f), Val::Null);
        assert_eq!(ev(json!(["feature-state", "hover"]), &f), Val::Null);
        assert_eq!(ev(json!(["within", {}]), &f), Val::Bool(false));
        for (e, want) in [
            (json!(["==", ["get", "rank"], 3]), true),
            (json!(["==", ["get", "rank"], "3"]), false),
            (json!(["!=", ["get", "missing"], 3]), true),
            (json!(["<", ["get", "rank"], 4]), true),
            (json!(["<=", ["get", "rank"], 3]), true),
            (json!([">", ["get", "rank"], 3]), false),
            (json!([">=", ["get", "name"], "Main"]), true),
            (json!(["<", ["get", "name"], 4]), false),
            (json!(["!", ["has", "x"]]), true),
            (json!(["all", ["has", "name"], ["==", ["get", "oneway"], true]]), true),
            (json!(["all"]), true),
            (json!(["any", ["has", "x"], ["has", "y"]]), false),
            (json!(["any"]), false),
            (json!(["none", ["has", "x"]]), true),
            (json!(["in", ["get", "rank"], ["literal", [1, 2, 3]]]), true),
            (json!(["in", "St", ["get", "name"]]), true),
            (json!(["in", ["get", "name"], ["literal", ["a", "b"]]]), false),
            (json!(true), true),
            (json!(["==", ["geometry-type"], "LineString"]), true),
        ] {
            assert_eq!(filt(e.clone(), &f), want, "{e}");
        }
    }

    #[test]
    fn control_flow() {
        let p = props(&[("class", s("primary")), ("rank", n(5.0))]);
        let f = ctx(12.0, GeomKind::LineString, &p);
        let m = json!(["match", ["get", "class"], ["motorway", "trunk"], 1, "primary", 2, 0]);
        assert_eq!(ev(m, &f), Val::Num(2.0));
        assert_eq!(ev(json!(["match", ["get", "class"], ["motorway", "trunk"], 1, 0]), &f), Val::Num(0.0));
        assert_eq!(ev(json!(["match", ["get", "rank"], [4, 5], "hit", "miss"]), &f), str_val("hit"));
        assert_eq!(ev(json!(["match", ["geometry-type"], ["MultiLineString", "LineString"], true, false]), &f), Val::Bool(true));
        assert_eq!(ev(json!(["case", ["has", "x"], 1, ["==", ["get", "rank"], 5], 2, 3]), &f), Val::Num(2.0));
        assert_eq!(ev(json!(["case", false, 1, 3]), &f), Val::Num(3.0));
        assert_eq!(ev(json!(["coalesce", ["get", "name_en"], ["get", "name"], "fallback"]), &f), str_val("fallback"));
        assert_eq!(ev(json!(["coalesce", ["get", "nope"]]), &f), Val::Null);
        assert_eq!(ev(json!(["step", ["zoom"], 0, 10, 1, 12, 2, 14, 3]), &f), Val::Num(2.0));
        assert_eq!(ev(json!(["step", ["zoom"], "lo", 13, "hi"]), &f), str_val("lo"));
        assert_eq!(ev(json!(["step", ["get", "class"], 0, 1, 1]), &f), Val::Null);
        assert_eq!(ev(json!(["let", "a", 2, "b", 3, ["*", ["var", "a"], ["var", "b"]]]), &f), Val::Num(6.0));
        assert_eq!(ev(json!(["var", "unbound"]), &f), Val::Null);
        assert_eq!(Expr::filter(&json!(["no-such-op", 1, 2])).eval(&f), Val::Null);
        assert_eq!(ev(json!(["coalesce", ["no-such-op", 1, 2], 3]), &f), Val::Num(3.0));
        assert_eq!(ev(json!(["+", 1, ["no-such-op"]]), &f), Val::Null);
    }

    #[test]
    fn interpolation() {
        let f = FeatureCtx::zoom_only(1.0);
        assert_eq!(ev(json!(["interpolate", ["linear"], ["zoom"], 0, 0, 2, 10]), &f), Val::Num(5.0));
        let e = ev(json!(["interpolate", ["exponential", 2], ["zoom"], 0, 0, 2, 30]), &f);
        assert!(matches!(e, Val::Num(x) if (x - 10.0).abs() < 1e-9), "{e:?}");
        assert_eq!(ev(json!(["interpolate", ["cubic-bezier", 0, 0, 1, 1], ["zoom"], 0, 0, 2, 10]), &f), Val::Num(5.0));
        assert_eq!(ev(json!(["interpolate", ["linear"], ["zoom"], 5, 1, 10, 2]), &f), Val::Num(1.0));
        assert_eq!(ev(json!(["interpolate", ["linear"], ["zoom"], -5, 1, 0, 2]), &f), Val::Num(2.0));
        assert_eq!(ev(json!(["interpolate", ["linear"], ["zoom"], 0, "#000000", 2, "#ffffff"]), &f), Val::Color([128, 128, 128, 255]));
        assert_eq!(ev(json!(["interpolate-hcl", ["linear"], ["zoom"], 0, "red", 2, "blue"]), &f), Val::Color([128, 0, 128, 255]));
        assert_eq!(
            ev(json!(["interpolate", ["linear"], ["zoom"], 0, "transparent", 2, "rgba(255,0,0,1)"]), &f),
            Val::Color([255, 0, 0, 128])
        );
        assert_eq!(
            ev(json!(["interpolate", ["linear"], ["zoom"], 0, ["literal", [0, 0]], 2, ["literal", [2, 4]]]), &f),
            Val::Arr(vec![Val::Num(1.0), Val::Num(2.0)])
        );
        assert_eq!(ev(json!(["interpolate", ["linear"], ["get", "x"], 0, 0, 2, 10]), &f), Val::Null);
        assert_eq!(ev(json!(["interpolate", ["linear"], ["zoom"]]), &f), Val::Null);
    }

    #[test]
    fn math_strings_types_arrays() {
        let p = props(&[("name", s("Hello")), ("n", s("12.5"))]);
        let f = ctx(3.0, GeomKind::Point, &p);
        let num = |j: Json| ev(j, &f).as_f64().unwrap_or(f64::NAN);
        assert_eq!(num(json!(["+", 1, 2, 3])), 6.0);
        assert_eq!(num(json!(["-", 5, 2])), 3.0);
        assert_eq!(num(json!(["-", 5])), -5.0);
        assert_eq!(num(json!(["*", 2, 3, 4])), 24.0);
        assert_eq!(num(json!(["/", 7, 2])), 3.5);
        assert_eq!(num(json!(["%", 7, 4])), 3.0);
        assert_eq!(num(json!(["^", 2, 10])), 1024.0);
        assert_eq!(num(json!(["min", 3, 1, 2])), 1.0);
        assert_eq!(num(json!(["max", 3, 1, 2])), 3.0);
        assert_eq!(num(json!(["abs", -2])), 2.0);
        assert_eq!(num(json!(["round", 2.5])), 3.0);
        assert_eq!(num(json!(["floor", 2.7])), 2.0);
        assert_eq!(num(json!(["ceil", 2.1])), 3.0);
        assert_eq!(num(json!(["sqrt", 16])), 4.0);
        assert_eq!(num(json!(["ln", ["e"]])), 1.0);
        assert_eq!(num(json!(["log10", 1000])), 3.0);
        assert_eq!(num(json!(["log2", 8])), 3.0);
        assert!((num(json!(["pi"])) - std::f64::consts::PI).abs() < 1e-12);
        assert_eq!(num(json!(["ln2"])), std::f64::consts::LN_2);
        assert_eq!(ev(json!(["+", 1, "a"]), &f), Val::Null);
        assert_eq!(num(json!(["to-number", ["get", "n"]])), 12.5);
        assert_eq!(num(json!(["to-number", "x", 7])), 7.0);
        assert_eq!(num(json!(["to-number", true])), 1.0);
        assert_eq!(ev(json!(["to-string", 3]), &f), str_val("3"));
        assert_eq!(ev(json!(["to-string", 2.5]), &f), str_val("2.5"));
        assert_eq!(ev(json!(["to-string", ["to-color", "red"]]), &f), str_val("rgba(255,0,0,1)"));
        assert_eq!(ev(json!(["number-format", 3, {"locale": "en"}]), &f), str_val("3"));
        assert_eq!(ev(json!(["to-boolean", ""]), &f), Val::Bool(false));
        assert_eq!(ev(json!(["to-boolean", "x"]), &f), Val::Bool(true));
        assert_eq!(ev(json!(["to-color", "nope", "#0f0"]), &f), Val::Color([0, 255, 0, 255]));
        assert_eq!(ev(json!(["to-color", ["literal", [1, 2, 3, 0.5]]]), &f), Val::Color([1, 2, 3, 128]));
        assert_eq!(ev(json!(["rgb", 1, 2, 3]), &f), Val::Color([1, 2, 3, 255]));
        assert_eq!(ev(json!(["rgba", 300, 2, 3, 0.5]), &f), Val::Color([255, 2, 3, 128]));
        assert_eq!(ev(json!(["number", ["get", "name"], 4]), &f), Val::Num(4.0));
        assert_eq!(ev(json!(["string", ["get", "name"]]), &f), str_val("Hello"));
        assert_eq!(ev(json!(["boolean", ["get", "name"], false]), &f), Val::Bool(false));
        assert_eq!(ev(json!(["typeof", ["get", "name"]]), &f), str_val("string"));
        assert_eq!(ev(json!(["concat", ["get", "name"], " ", 1, true]), &f), str_val("Hello 1true"));
        assert_eq!(ev(json!(["upcase", ["get", "name"]]), &f), str_val("HELLO"));
        assert_eq!(ev(json!(["downcase", ["get", "name"]]), &f), str_val("hello"));
        assert_eq!(num(json!(["length", ["get", "name"]])), 5.0);
        assert_eq!(num(json!(["length", ["literal", [1, 2]]])), 2.0);
        assert_eq!(ev(json!(["at", 1, ["literal", ["a", "b"]]]), &f), str_val("b"));
        assert_eq!(ev(json!(["at", 5, ["literal", ["a", "b"]]]), &f), Val::Null);
        assert_eq!(num(json!(["index-of", "l", ["get", "name"]])), 2.0);
        assert_eq!(num(json!(["index-of", "l", ["get", "name"], 3])), 3.0);
        assert_eq!(num(json!(["index-of", "z", ["get", "name"]])), -1.0);
        assert_eq!(num(json!(["index-of", 2, ["literal", [1, 2]]])), 1.0);
        assert_eq!(ev(json!(["slice", ["get", "name"], 1, 3]), &f), str_val("el"));
        assert_eq!(ev(json!(["slice", ["get", "name"], -2]), &f), str_val("lo"));
        assert_eq!(ev(json!(["slice", ["literal", [1, 2, 3]], 1]), &f), Val::Arr(vec![Val::Num(2.0), Val::Num(3.0)]));
        assert_eq!(ev(json!(["array", ["literal", [1]]]), &f), Val::Arr(vec![Val::Num(1.0)]));
        assert_eq!(ev(json!([1, 2]), &f), Val::Arr(vec![Val::Num(1.0), Val::Num(2.0)]));
        assert_eq!(ev(json!(["Noto Sans Bold"]), &f), Val::Arr(vec![str_val("Noto Sans Bold")]));
        assert_eq!(ev(json!(["image", "x"]), &f), Val::Null);
    }

    #[test]
    fn image_names_a_sprite_by_its_id() {
        let at = json!({"x": 0, "y": 0, "width": 2, "height": 2});
        let picture = || tiny_skia::Pixmap::new(2, 2).unwrap();
        let sprite =
            crate::sprite::Sprite::of(vec![("nautical", json!({"light/red": at}), picture()), ("base", json!({"bus": at}), picture())]);
        let f = FeatureCtx { images: Some(&sprite), ..ctx(10.0, GeomKind::Point, &[]) };
        assert_eq!(ev(json!(["image", "nautical:light/red"]), &f), str_val("nautical:light/red"));
        // Not from another sprite, nor with no id: on to the next choice.
        assert_eq!(ev(json!(["image", "base:light/red"]), &f), Val::Null);
        assert_eq!(ev(json!(["image", "light/red"]), &f), Val::Null);
        assert_eq!(ev(json!(["coalesce", ["image", "base:light/red"], ["image", "base:bus"]]), &f), str_val("base:bus"));
    }

    #[test]
    fn expressions_say_what_they_depend_on() {
        let on = |j: Json| Expr::property(&j);
        assert!(on(json!(["step", ["zoom"], ["image", "a"], 2.5, ["image", "b"]])).uses_zoom());
        assert!(on(json!({"stops": [[2, "a"], [3, "b"]]})).uses_zoom());
        assert!(!on(json!(["get", "pattern"])).uses_zoom() && !on(json!("a")).uses_zoom());
        assert!(on(json!(["coalesce", ["get", "x"], ["image", "a"]])).uses_image());
        assert!(on(json!(["case", ["has", "x"], ["image", "a"], "b"])).uses_image());
        assert!(!on(json!(["get", "pattern"])).uses_image() && !on(json!("a")).uses_image());
    }

    #[test]
    fn reads_a_styles_sprites_and_says_what_it_left_out() {
        let parse = |sprite: Json| Style::parse(&json!({"version": 8, "sprite": sprite, "sources": {}, "layers": []})).unwrap();
        // One address is the `default` sprite; a list keeps its ids.
        assert_eq!(parse(json!("https://x/s")).sprites, [("default".to_owned(), "https://x/s".to_owned())]);
        assert!(parse(json!("")).sprites.is_empty());
        let listed = parse(json!([{"id": "base", "url": "a"}]));
        assert_eq!(listed.sprites, [("base".to_owned(), "a".to_owned())]);
        assert!(listed.sprites_left_out.is_empty());
        // What can't be read doesn't count towards the limit: a sprite after it is still read.
        let mut list: Vec<Json> = (0..MAX_SPRITES + 2).map(|_| json!({"url": "no-id"})).collect();
        list.push(json!({"id": "default", "url": "b"}));
        let late = parse(Json::Array(list));
        assert_eq!(late.sprites, [("default".to_owned(), "b".to_owned())]);
        assert_eq!(late.sprites_left_out.len(), 1);
        let total = MAX_SPRITES + 3;
        assert!(
            late.sprites_left_out[0].starts_with(&format!("{} of {total} sprites left out", MAX_SPRITES + 2)),
            "{:?}",
            late.sprites_left_out
        );
        // A repeated id, and those past the limit, each said with how many.
        let mut list: Vec<Json> = (0..MAX_SPRITES + 3).map(|i| json!({"id": format!("s{i}"), "url": "u"})).collect();
        list.push(json!({"id": "s0", "url": "again"}));
        let many = parse(Json::Array(list));
        assert_eq!(many.sprites.len(), MAX_SPRITES);
        assert_eq!(many.sprites[0].1, "u");
        let said = many.sprites_left_out.join("; ");
        assert!(
            said.contains("1 of 20 sprites left out") && said.contains("same id") && said.contains("3 of 20 sprites left out"),
            "{said}"
        );
        // An empty id or address is none.
        let empty = parse(json!([{"id": "a", "url": ""}, {"id": "", "url": "b"}, {"id": "c", "url": "d"}]));
        assert_eq!(empty.sprites, [("c".to_owned(), "d".to_owned())]);
        assert!(empty.sprites_left_out[0].starts_with("2 of 3 sprites left out"), "{:?}", empty.sprites_left_out);
        // An id too long to keep with every picture and mesh is none either.
        let long = parse(json!([{"id": "x".repeat(MAX_SPRITE_ID + 1), "url": "a"}, {"id": "x".repeat(MAX_SPRITE_ID), "url": "b"}]));
        assert_eq!(long.sprites.len(), 1);
        assert_eq!(long.sprites[0].1, "b");
    }

    #[test]
    fn image_is_its_name_when_the_sprite_has_it() {
        let index = json!({"light/red": {"x": 0, "y": 0, "width": 2, "height": 2}, "generic": {"x": 0, "y": 0, "width": 2, "height": 2}});
        let Some(sheet) = tiny_skia::Pixmap::new(2, 2) else { panic!("no pixmap") };
        let sprite = crate::sprite::Sprite::new(&index, sheet);
        let props = [("colour".to_owned(), Value::String("red".into()))];
        let f = FeatureCtx { images: Some(&sprite), ..ctx(10.0, GeomKind::Point, &props) };
        let light = |colour: &str| json!(["image", ["concat", "light/", colour]]);
        assert_eq!(ev(light("red"), &f), str_val("light/red"));
        assert_eq!(ev(light("blue"), &f), Val::Null);
        // The first image the sprite has, as a style picks a light's icon by its colour.
        let pick = |c: Json| ev(json!(["coalesce", ["image", ["concat", "light/", c]], ["image", "generic"]]), &f);
        assert_eq!(pick(json!(["get", "colour"])), str_val("light/red"));
        assert_eq!(pick(json!("blue")), str_val("generic"));
        // Without the sprite, no image is there to be had.
        let bare = FeatureCtx { images: None, ..f };
        assert_eq!(ev(light("red"), &bare), Val::Null);
        assert!(!Expr::property(&light("red")).feature_free(), "depends on the sprite");
    }

    #[test]
    fn legacy_filters() {
        let p = props(&[("class", s("park")), ("admin_level", n(4.0))]);
        let poly = ctx(10.0, GeomKind::Polygon, &p);
        let point = FeatureCtx { id: None, ..ctx(10.0, GeomKind::Point, &p) };
        for (e, want_poly, want_point) in [
            (json!(["==", "$type", "Polygon"]), true, false),
            (json!(["!=", "$type", "Polygon"]), false, true),
            (json!(["in", "$type", "Point", "LineString"]), false, true),
            (json!(["==", "class", "park"]), true, true),
            (json!(["!=", "class", "wood"]), true, true),
            (json!(["!=", "missing", "x"]), true, true),
            (json!(["==", "missing", "x"]), false, false),
            (json!(["<=", "admin_level", 4]), true, true),
            (json!([">", "admin_level", 4]), false, false),
            (json!(["<", "class", 4]), false, false),
            (json!(["in", "class", "park", "wood"]), true, true),
            (json!(["!in", "class", "park", "wood"]), false, false),
            (json!(["in", "admin_level", 2, 4]), true, true),
            (json!(["has", "class"]), true, true),
            (json!(["!has", "class"]), false, false),
            (json!(["has", "$id"]), true, false),
            (json!(["==", "$id", 42]), true, false),
            (json!(["all", ["==", "$type", "Polygon"], ["==", "class", "park"]]), true, false),
            (json!(["any", ["==", "$type", "Point"], ["==", "class", "x"]]), false, true),
            (json!(["none", ["==", "$type", "Point"]]), true, false),
            (json!(["all"]), true, true),
            (json!(["all", ["==", ["get", "class"], "park"], ["has", "class"]]), true, true),
            (json!(["bogus"]), false, false),
            (json!("bogus"), false, false),
        ] {
            assert_eq!(filt(e.clone(), &poly), want_poly, "poly {e}");
            assert_eq!(filt(e.clone(), &point), want_point, "point {e}");
        }
    }

    #[test]
    fn legacy_functions() {
        let at = |z: f32| FeatureCtx::zoom_only(z);
        let zf = json!({"base": 2, "stops": [[0, 0], [2, 30]]});
        assert!(matches!(ev(zf.clone(), &at(1.0)), Val::Num(x) if (x - 10.0).abs() < 1e-9));
        assert_eq!(ev(zf, &at(5.0)), Val::Num(30.0));
        assert_eq!(ev(json!({"stops": [[0, "#000"], [10, "#fff"]]}), &at(5.0)), Val::Color([128, 128, 128, 255]));
        assert_eq!(ev(json!({"stops": [[0, "butt"], [10, "round"]]}), &at(9.0)), str_val("butt"));
        assert_eq!(ev(json!({"type": "interval", "stops": [[5, 1], [10, 2]]}), &at(2.0)), Val::Num(1.0));
        assert_eq!(ev(json!({"type": "interval", "stops": [[5, 1], [10, 2]]}), &at(12.0)), Val::Num(2.0));

        let p = props(&[("class", s("primary")), ("rank", n(5.0)), ("color", s("#00f"))]);
        let f = ctx(10.0, GeomKind::LineString, &p);
        let cat =
            json!({"property": "class", "type": "categorical", "stops": [["primary", "red"], ["secondary", "blue"]], "default": "gray"});
        assert_eq!(ev(cat.clone(), &f).as_color(), Some([255, 0, 0, 255]));
        let other = props(&[("class", s("x"))]);
        assert_eq!(ev(cat, &ctx(1.0, GeomKind::Point, &other)).as_color(), parse_color("gray"));
        assert_eq!(ev(json!({"property": "class", "stops": [["primary", 3]]}), &f), Val::Num(3.0));
        assert_eq!(ev(json!({"property": "rank", "stops": [[0, 0], [10, 100]]}), &f), Val::Num(50.0));
        assert_eq!(ev(json!({"property": "color", "type": "identity"}), &f).as_color(), Some([0, 0, 255, 255]));
        assert_eq!(ev(json!({"property": "nope", "type": "identity", "default": 7}), &f), Val::Num(7.0));
        let zp = json!({"property": "rank", "stops": [[{"zoom": 0, "value": 0}, 0], [{"zoom": 0, "value": 10}, 10],
            [{"zoom": 20, "value": 0}, 0], [{"zoom": 20, "value": 10}, 30]]});
        assert_eq!(ev(zp, &f), Val::Num(10.0));
        assert_eq!(ev(json!({"stops": []}), &f), Val::Null);
        assert_eq!(ev(json!({"stops": [], "default": 1}), &f), Val::Num(1.0));
    }

    fn layer(j: Json) -> StyleLayer {
        StyleLayer::parse(&j).unwrap()
    }

    #[test]
    fn text_fields() {
        let p = props(&[("name:latin", s("Roma")), ("name:nonlatin", s("Ρώμη")), ("name", s("Rome")), ("ele", n(1234.0))]);
        let f = ctx(10.0, GeomKind::Point, &p);
        let tf = |v: Json| layer(json!({"id": "t", "type": "symbol", "layout": {"text-field": v}})).text_field(&f);
        assert_eq!(tf(json!("{name:latin}\n{name:nonlatin}")).as_deref(), Some("Roma\nΡώμη"));
        assert_eq!(tf(json!("{name:latin}\n{name:xx}")).as_deref(), Some("Roma"));
        assert_eq!(tf(json!("{ele} m")).as_deref(), Some("1234 m"));
        assert_eq!(tf(json!("{missing}")), None);
        assert_eq!(tf(json!("plain {unterminated")).as_deref(), Some("plain {unterminated"));
        assert_eq!(tf(json!(["get", "name:latin"])).as_deref(), Some("Roma"));
        assert_eq!(tf(json!(["get", "ele"])).as_deref(), Some("1234"));
        assert_eq!(tf(json!(["coalesce", ["get", "name_en"], ["get", "name"]])).as_deref(), Some("Rome"));
        let fmt = json!(["format", ["get", "name:latin"], {"font-scale": 1.2}, "\n", {},
            ["get", "name:nonlatin"], {"text-font": ["literal", ["X"]]}, ["image", "x"]]);
        assert_eq!(tf(fmt).as_deref(), Some("Roma\nΡώμη"));
        assert_eq!(
            tf(json!([
                "case",
                ["has", "name:nonlatin"],
                ["concat", ["get", "name:latin"], " ", ["get", "name:nonlatin"]],
                ["get", "name"]
            ]))
            .as_deref(),
            Some("Roma Ρώμη")
        );
        assert_eq!(tf(json!({"stops": [[0, "{name}"], [12, "{name:latin}"]]})).as_deref(), Some("Rome"));
        let up = layer(json!({"id": "t", "type": "symbol", "layout": {"text-field": "{name}", "text-transform": "uppercase"}}));
        assert_eq!(up.text_field(&f).as_deref(), Some("ROME"));
        assert_eq!(up.text_transform(&f), TextTransform::Uppercase);
    }

    #[test]
    fn layer_properties_and_defaults() {
        let p = props(&[("class", s("motorway"))]);
        let f = ctx(10.0, GeomKind::LineString, &p);
        let l = layer(json!({
            "id": "roads", "type": "line", "source": "omt", "source-layer": "transportation", "minzoom": 5, "maxzoom": 18,
            "filter": ["==", "class", "motorway"],
            "layout": {"line-cap": "round", "line-join": ["step", ["zoom"], "bevel", 8, "round"], "visibility": "visible"},
            "paint": {"line-color": "hsl(26,87%,62%)", "line-width": {"base": 1.2, "stops": [[5, 0], [7, 1], [20, 18]]},
                "line-opacity": 2, "line-dasharray": [2, 1], "line-gap-width": 3}
        }));
        assert_eq!((l.kind, l.source.as_deref(), l.source_layer.as_deref()), (LayerKind::Line, Some("omt"), Some("transportation")));
        assert!(l.visible_at(5.0) && l.visible_at(17.9) && !l.visible_at(18.0) && !l.visible_at(4.0));
        assert!(l.matches(&f));
        assert!(!l.matches(&ctx(10.0, GeomKind::LineString, &[])));
        assert_eq!(l.line_color(&f), parse_color("hsl(26,87%,62%)"));
        assert!(l.line_width(&f) > 1.0 && l.line_width(&f) < 18.0);
        assert_eq!(l.line_opacity(&f), 1.0);
        assert_eq!(l.line_dasharray(&f), Some(vec![2.0, 1.0]));
        assert_eq!(l.line_gap_width(&f), 3.0);
        assert_eq!(l.line_cap(&f), LineCap::Round);
        assert_eq!(l.line_join(&f), LineJoin::Round);
        assert_eq!(l.line_join(&ctx(3.0, GeomKind::LineString, &p)), LineJoin::Bevel);

        let bare = layer(json!({"id": "x", "type": "circle", "layout": {"visibility": "none"}}));
        assert!(!bare.visible && !bare.visible_at(10.0));
        assert!(bare.matches(&f));
        assert_eq!(bare.circle_color(&f), Some(BLACK));
        assert_eq!(bare.circle_radius(&f), 5.0);
        assert_eq!(bare.circle_opacity(&f), 1.0);
        assert_eq!(bare.circle_stroke_color(&f), None);
        assert_eq!(bare.circle_stroke_width(&f), 0.0);
        assert_eq!(bare.fill_color(&f), Some(BLACK));
        assert_eq!(bare.fill_outline_color(&f), None);
        assert_eq!(bare.line_width(&f), 1.0);
        assert_eq!(bare.line_dasharray(&f), None);
        assert_eq!(bare.text_size(&f), 16.0);
        assert_eq!(bare.text_color(&f), Some(BLACK));
        assert_eq!(bare.text_halo_color(&f), None);
        assert_eq!(bare.text_halo_width(&f), 0.0);
        assert_eq!(bare.symbol_placement(&f), SymbolPlacement::Point);
        assert_eq!(bare.symbol_sort_key(&f), None);
        assert_eq!(bare.text_font(&f), vec!["Open Sans Regular".to_string(), "Arial Unicode MS Regular".to_string()]);
        assert_eq!(bare.background_color(3.0), Some(BLACK));
        assert_eq!(bare.background_opacity(3.0), 1.0);
        assert_eq!(bare.raster_opacity(3.0), 1.0);
        assert_eq!(bare.line_cap(&f), LineCap::Butt);
        assert_eq!(bare.line_join(&f), LineJoin::Miter);

        let ext =
            layer(json!({"id": "b", "type": "fill-extrusion", "paint": {"fill-extrusion-color": "#abc", "fill-extrusion-opacity": 0.8}}));
        assert_eq!(ext.fill_color(&f), Some([0xaa, 0xbb, 0xcc, 255]));
        assert_eq!(ext.fill_opacity(&f), 0.8);

        let sym =
            layer(json!({"id": "s", "type": "symbol", "layout": {"symbol-placement": "line-center", "symbol-sort-key": ["get", "rank"],
            "text-font": ["Noto Sans Bold", "x"], "text-size": ["interpolate", ["linear"], ["zoom"], 0, 10, 20, 30]},
            "paint": {"text-halo-color": "rgba(255,255,255,0.7)", "text-halo-width": 1.5, "text-color": "nonsense"}}));
        let rp = props(&[("rank", n(3.0))]);
        let rf = ctx(10.0, GeomKind::LineString, &rp);
        assert_eq!(sym.symbol_placement(&rf), SymbolPlacement::LineCenter);
        assert_eq!(sym.symbol_sort_key(&rf), Some(3.0));
        assert_eq!(sym.text_font(&rf), vec!["Noto Sans Bold".to_string(), "x".to_string()]);
        assert_eq!(sym.text_size(&rf), 20.0);
        assert_eq!(sym.text_halo_color(&rf), Some([255, 255, 255, 179]));
        assert_eq!(sym.text_halo_width(&rf), 1.5);
        assert_eq!(sym.text_color(&rf), Some(BLACK), "invalid falls back to default");
    }

    #[test]
    fn sources_and_tilejson() {
        let st = Style::parse(&json!({
            "version": 8, "name": "t", "glyphs": "g/{fontstack}/{range}.pbf",
            "sprite": [{"id": "x", "url": "a"}, {"id": "default", "url": "b"}],
            "sources": {
                "v": {"type": "vector", "url": "https://x/tiles.json"},
                "r": {"type": "raster", "tiles": ["https://r/{z}/{x}/{y}.png"], "tileSize": 256, "maxzoom": 19, "attribution": "OSM"},
                "g": {"type": "geojson", "data": {}}
            },
            "layers": [{"id": "bg", "type": "background", "paint": {"background-color": "#eee"}}, 5, {"id": "u", "type": "heatmap"}]
        }))
        .unwrap();
        assert_eq!(st.name, "t");
        assert_eq!(st.sprites, vec![("x".to_owned(), "a".to_owned()), ("default".to_owned(), "b".to_owned())]);
        assert_eq!(st.glyphs.as_deref(), Some("g/{fontstack}/{range}.pbf"));
        assert_eq!(st.layers.len(), 2);
        assert_eq!(st.layers[1].kind, LayerKind::Other);
        assert_eq!(st.layers[0].background_color(0.0), Some([0xee, 0xee, 0xee, 255]));
        assert_eq!(
            st.source("r"),
            Some(&Source::Raster {
                tiles: vec!["https://r/{z}/{x}/{y}.png".into()],
                from: None,
                url: None,
                tile_size: 256,
                min_zoom: 0,
                max_zoom: 19,
                attribution: "OSM".into()
            })
        );
        assert_eq!(st.source("g"), Some(&Source::Other));
        let mut v = st.source("v").cloned().unwrap();
        v.apply_tilejson(&json!({"tiles": ["https://x/{z}/{x}/{y}.pbf"], "minzoom": 0, "maxzoom": 14, "attribution": "© OMT"}));
        assert_eq!(
            v,
            Source::Vector {
                tiles: vec!["https://x/{z}/{x}/{y}.pbf".into()],
                from: None,
                url: Some("https://x/tiles.json".into()),
                min_zoom: 0,
                max_zoom: 14,
                attribution: "© OMT".into()
            }
        );
        assert!(Style::parse(&json!([])).is_err());
        assert!(Style::parse(&json!({"layers": 3})).is_err());
    }

    /// A style's root view: kept where it is a usable number, left out otherwise.
    #[test]
    fn style_root_view() {
        let st = Style::parse(&json!({"layers": [], "center": [-73.9857, 40.7484], "zoom": 15.5, "bearing": 30, "pitch": 60})).unwrap();
        assert_eq!(st.view, StyleView { center: Some([-73.9857, 40.7484]), zoom: Some(15.5), bearing: Some(30.0), pitch: Some(60.0) });
        assert!(!st.view.is_empty());
        assert!(Style::parse(&json!({"layers": []})).unwrap().view.is_empty());
        for bad in [
            json!({"layers": [], "center": [200, 0], "zoom": -1, "bearing": "north", "pitch": null}),
            json!({"layers": [], "center": [0, 95, 3], "zoom": 1e300}),
            json!({"layers": [], "center": "0,0", "zoom": [1]}),
            json!({"layers": [], "center": [null, 0]}),
        ] {
            assert!(Style::parse(&bad).unwrap().view.is_empty(), "{bad}");
        }
    }

    fn sample_features() -> Vec<(GeomKind, Vec<(String, Value)>)> {
        vec![
            (GeomKind::Point, vec![]),
            (
                GeomKind::Point,
                props(&[
                    ("class", s("rail")),
                    ("name", s("Central")),
                    ("name:latin", s("Central")),
                    ("rank", n(3.0)),
                    ("subclass", s("florist")),
                ]),
            ),
            (
                GeomKind::LineString,
                props(&[
                    ("class", s("motorway")),
                    ("brunnel", s("bridge")),
                    ("ramp", n(1.0)),
                    ("oneway", n(-1.0)),
                    ("ref", s("A1")),
                    ("ref_length", n(2.0)),
                    ("network", s("us-interstate")),
                ]),
            ),
            (
                GeomKind::LineString,
                props(&[
                    ("class", s("river")),
                    ("name:nonlatin", s("Река")),
                    ("admin_level", n(2.0)),
                    ("disputed", n(1.0)),
                    ("maritime", Value::Bool(true)),
                ]),
            ),
            (
                GeomKind::Polygon,
                props(&[
                    ("class", s("residential")),
                    ("render_height", n(20.0)),
                    ("render_min_height", n(0.0)),
                    ("colour", s("#f00")),
                    ("hide_3d", Value::Bool(true)),
                ]),
            ),
            (GeomKind::Polygon, props(&[("class", n(5.0)), ("name", Value::Bool(true)), ("rank", s("x"))])),
        ]
    }

    /// Every accessor; must never panic.
    fn touch_all(l: &StyleLayer, f: &FeatureCtx) -> usize {
        let _ = l.matches(f);
        let _ = (l.visible_at(f.zoom), l.background_color(f.zoom), l.background_opacity(f.zoom), l.raster_opacity(f.zoom));
        let _ = (l.fill_color(f), l.fill_opacity(f), l.fill_outline_color(f));
        let _ = (l.line_color(f), l.line_width(f), l.line_opacity(f), l.line_dasharray(f), l.line_cap(f), l.line_join(f));
        let _ = (l.line_gap_width(f), l.line_offset(f));
        let _ = (l.circle_color(f), l.circle_radius(f), l.circle_opacity(f), l.circle_stroke_color(f), l.circle_stroke_width(f));
        let _ = (l.text_size(f), l.text_color(f), l.text_halo_color(f), l.text_halo_width(f), l.text_transform(f));
        let _ = (l.symbol_placement(f), l.symbol_sort_key(f), l.text_font(f));
        let mut hits = 0;
        for k in l.paint.keys() {
            let _ = l.paint_value(k, f);
        }
        for k in l.layout.keys() {
            let _ = l.layout_value(k, f);
        }
        if l.text_field(f).is_some() {
            hits += 1;
        }
        hits
    }

    /// A complete basemap style of the kind web maps publish: both source kinds, sprites and
    /// glyphs, every layer type a basemap uses, and the expressions such styles use (`case`, `coalesce`,
    /// `concat`, `geometry-type`, `has`, `interpolate`, `match`, `step`, `to-string`, `zoom` and
    /// the comparisons) in filters, paint and layout. Every accessor is evaluated for every
    /// layer, sample feature and zoom, then a few results are checked.
    #[test]
    fn full_basemap_style() {
        let line = json!(["match", ["geometry-type"], ["LineString", "MultiLineString"], true, false]);
        let not_tunnel = json!(["!=", ["get", "brunnel"], "tunnel"]);
        let name = json!([
            "case",
            ["has", "name:nonlatin"],
            ["concat", ["get", "name:latin"], " ", ["get", "name:nonlatin"]],
            ["coalesce", ["get", "name_en"], ["get", "name"]]
        ]);
        let road_width = |base: f64, top: f64| json!(["interpolate", ["exponential", 1.3], ["zoom"], 6, base, 18, top]);
        let json = json!({
            "version": 8,
            "sources": {
                "relief": {"type": "raster", "tiles": ["https://tiles.example.com/relief/{z}/{x}/{y}.png"], "tileSize": 256, "maxzoom": 6},
                "streets": {"type": "vector", "url": "https://tiles.example.com/streets.json"}
            },
            "sprite": "https://tiles.example.com/sprites/basic",
            "glyphs": "https://tiles.example.com/fonts/{fontstack}/{range}.pbf",
            "layers": [
                {"id": "paper", "type": "background", "paint": {"background-color": "#f4f1ea"}},
                {"id": "relief", "type": "raster", "source": "relief", "maxzoom": 8,
                 "paint": {"raster-opacity": ["interpolate", ["exponential", 1.4], ["zoom"], 0, 0.5, 7, 0.05]}},
                {"id": "parks", "type": "fill", "source": "streets", "source-layer": "landuse",
                 "filter": ["all", ["match", ["geometry-type"], ["MultiPolygon", "Polygon"], true, false], ["match", ["get", "class"], ["park", "garden"], true, false]],
                 "paint": {"fill-color": ["match", ["get", "class"], "park", "#cfe8c0", "#dcebd0"], "fill-opacity": ["interpolate", ["linear"], ["zoom"], 8, 0.4, 12, 0.9], "fill-outline-color": "#b5d3a4", "fill-antialias": true}},
                {"id": "marsh", "type": "fill", "source": "streets", "source-layer": "landcover", "filter": ["==", ["get", "class"], "wetland"],
                 "paint": {"fill-pattern": "marsh_dots", "fill-translate-anchor": "map"}},
                {"id": "water", "type": "fill", "source": "streets", "source-layer": "water", "filter": not_tunnel,
                 "paint": {"fill-color": "#a9cbe8"}},
                {"id": "built_up", "type": "fill", "source": "streets", "source-layer": "building", "maxzoom": 15,
                 "filter": ["!", ["has", "hide_3d"]], "paint": {"fill-color": ["coalesce", ["get", "colour"], "#ddd6cc"]}},
                {"id": "buildings_3d", "type": "fill-extrusion", "source": "streets", "source-layer": "building", "minzoom": 14,
                 "paint": {"fill-extrusion-color": "#e0d8cc", "fill-extrusion-height": ["get", "render_height"], "fill-extrusion-base": ["get", "render_min_height"], "fill-extrusion-opacity": 0.7}},
                {"id": "river", "type": "line", "source": "streets", "source-layer": "waterway",
                 "filter": ["all", line, ["==", ["get", "class"], "river"], not_tunnel],
                 "layout": {"line-cap": "round"}, "paint": {"line-color": "#a9cbe8", "line-width": ["interpolate", ["linear"], ["zoom"], 8, 0.5, 16, 4]}},
                {"id": "path", "type": "line", "source": "streets", "source-layer": "transportation", "minzoom": 14,
                 "filter": ["==", ["get", "class"], "path"],
                 "paint": {"line-color": "#a08a70", "line-dasharray": [1.5, 1], "line-width": 1, "line-opacity": ["step", ["zoom"], 0.5, 16, 1]}},
                {"id": "railway", "type": "line", "source": "streets", "source-layer": "transportation",
                 "filter": ["match", ["get", "class"], ["rail", "transit"], true, false],
                 "paint": {"line-color": "#9a9a9a", "line-width": ["step", ["zoom"], 1, 14, 2]}},
                {"id": "road_casing", "type": "line", "source": "streets", "source-layer": "transportation", "minzoom": 6,
                 "filter": ["all", ["match", ["get", "class"], ["motorway", "trunk", "primary"], true, false], ["match", ["get", "brunnel"], ["bridge", "tunnel"], false, true]],
                 "layout": {"line-join": "round"}, "paint": {"line-color": "#b07040", "line-gap-width": road_width(0.5, 14.0), "line-width": 1}},
                {"id": "road_motorway", "type": "line", "source": "streets", "source-layer": "transportation", "minzoom": 6,
                 "filter": ["all", ["==", ["get", "class"], "motorway"], ["<=", ["coalesce", ["get", "ramp"], 0], 1], ["match", ["get", "brunnel"], ["tunnel"], false, true]],
                 "layout": {"line-cap": "round", "line-join": "round"},
                 "paint": {"line-color": ["interpolate", ["linear"], ["zoom"], 6, "hsl(24,80%,60%)", 9, "#f7b267"], "line-width": road_width(0.5, 14.0)}},
                {"id": "boundary", "type": "line", "source": "streets", "source-layer": "boundary",
                 "filter": ["all", [">=", ["get", "admin_level"], 2], ["<", ["get", "admin_level"], 5], ["!=", ["get", "maritime"], true]],
                 "paint": {"line-color": "#8e7d9e", "line-dasharray": [3, 2], "line-width": ["case", ["==", ["get", "disputed"], 1], 0.8, 1.4]}},
                {"id": "river_label", "type": "symbol", "source": "streets", "source-layer": "waterway", "minzoom": 9, "filter": line,
                 "layout": {"symbol-placement": "line", "symbol-spacing": 300, "text-field": name, "text-font": ["Serif Italic"], "text-size": 13, "text-letter-spacing": 0.15, "text-max-width": 6, "text-rotation-alignment": "map"},
                 "paint": {"text-color": "#4f7fae", "text-halo-color": "rgba(255,255,255,0.8)", "text-halo-width": 1.2}},
                {"id": "shield", "type": "symbol", "source": "streets", "source-layer": "transportation_name",
                 "filter": ["all", ["has", "ref"], ["<=", ["get", "ref_length"], 6]],
                 "layout": {"icon-image": ["concat", "shield_", ["match", ["get", "network"], ["us-interstate", "us-highway"], ["get", "network"], "road"], "_", ["get", "ref_length"]],
                            "icon-rotation-alignment": "viewport", "icon-size": 1, "text-field": ["to-string", ["get", "ref"]], "text-font": ["Sans Bold"], "text-size": 10, "text-rotation-alignment": "viewport", "symbol-placement": ["step", ["zoom"], "point", 10, "line"]}},
                {"id": "one_way", "type": "symbol", "source": "streets", "source-layer": "transportation", "minzoom": 16,
                 "filter": ["==", ["get", "oneway"], 1],
                 "layout": {"icon-image": "arrow", "icon-rotate": 90, "icon-allow-overlap": true, "symbol-placement": "line", "symbol-spacing": 80}},
                {"id": "place", "type": "symbol", "source": "streets", "source-layer": "place",
                 "filter": ["match", ["get", "class"], ["city", "town"], true, false],
                 "layout": {"text-field": name, "text-font": ["Sans Regular"], "text-transform": "uppercase", "text-size": ["interpolate", ["exponential", 1.2], ["zoom"], 4, 11, 11, 18],
                            "text-anchor": "top", "text-offset": [0, 0.4], "text-padding": 2, "text-optional": true, "icon-image": ["match", ["get", "class"], "city", "dot_large", "dot_small"], "icon-optional": false},
                 "paint": {"text-color": "#333", "text-halo-color": "#fff", "text-halo-width": 1, "text-halo-blur": 0.5}}
            ]
        });
        let st = Style::parse(&json).unwrap();
        let mut counts: HashMap<LayerKind, usize> = HashMap::new();
        for l in &st.layers {
            *counts.entry(l.kind).or_default() += 1;
        }
        let want = [
            (LayerKind::Line, 6),
            (LayerKind::Symbol, 4),
            (LayerKind::Fill, 4),
            (LayerKind::Background, 1),
            (LayerKind::Raster, 1),
            (LayerKind::FillExtrusion, 1),
        ];
        assert_eq!(counts, want.into_iter().collect());
        assert!(matches!(st.source("streets"), Some(Source::Vector { url: Some(_), .. })));
        assert!(matches!(st.source("relief"), Some(Source::Raster { tile_size: 256, max_zoom: 6, .. })));
        assert!(st.glyphs.is_some() && !st.sprites.is_empty());

        let feats = sample_features();
        let mut labels = 0;
        let mut matched = 0;
        for z in 0..=20 {
            for l in &st.layers {
                for (kind, p) in &feats {
                    let f = FeatureCtx { zoom: z as f32 + 0.5, kind: *kind, id: Some(1), props: p, images: None };
                    labels += touch_all(l, &f);
                    matched += usize::from(l.matches(&f));
                }
            }
        }
        assert!(labels > 0 && matched > 0);

        // Spot checks.
        let get = |id: &str| st.layers.iter().find(|l| l.id == id).unwrap();
        let water = get("water");
        let lake = props(&[("class", s("lake"))]);
        assert!(water.matches(&ctx(5.0, GeomKind::Polygon, &lake)));
        let motorway = get("road_motorway");
        let mp = props(&[("class", s("motorway")), ("ramp", n(0.0))]);
        let mf = ctx(14.0, GeomKind::LineString, &mp);
        assert!(motorway.matches(&mf));
        assert!(motorway.line_width(&mf) > 1.0);
        let wl = get("river_label");
        let rp = props(&[("name:latin", s("Tevere")), ("name:nonlatin", s("Τίβερη"))]);
        let rf = ctx(14.0, GeomKind::LineString, &rp);
        assert_eq!(wl.text_field(&rf).as_deref(), Some("Tevere Τίβερη"));
        assert_eq!(wl.symbol_placement(&rf), SymbolPlacement::Line);
        assert!(wl.text_font(&rf).iter().any(|f| f.contains("Italic")));
        // 0.5 + (0.05 − 0.5)·(1.4³ − 1)/(1.4⁷ − 1) at zoom 3.
        assert!((0.40..0.44).contains(&get("relief").raster_opacity(3.0)));
        // A step to a word: the shields follow their road from zoom 10.
        let shield = get("shield");
        let sp = props(&[("ref", s("A1")), ("ref_length", n(2.0))]);
        assert_eq!(shield.symbol_placement(&ctx(8.0, GeomKind::LineString, &sp)), SymbolPlacement::Point);
        assert_eq!(shield.symbol_placement(&ctx(14.0, GeomKind::LineString, &sp)), SymbolPlacement::Line);
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    const OPS: &[&str] = &[
        "literal",
        "get",
        "has",
        "!has",
        "id",
        "geometry-type",
        "zoom",
        "properties",
        "==",
        "!=",
        "<",
        "<=",
        ">",
        ">=",
        "!",
        "all",
        "any",
        "none",
        "in",
        "!in",
        "match",
        "case",
        "coalesce",
        "step",
        "interpolate",
        "interpolate-hcl",
        "to-number",
        "to-string",
        "to-boolean",
        "to-color",
        "number",
        "string",
        "boolean",
        "array",
        "concat",
        "upcase",
        "downcase",
        "+",
        "-",
        "*",
        "/",
        "%",
        "^",
        "min",
        "max",
        "abs",
        "round",
        "floor",
        "ceil",
        "sqrt",
        "ln",
        "log10",
        "length",
        "at",
        "index-of",
        "slice",
        "format",
        "number-format",
        "image",
        "let",
        "var",
        "rgb",
        "rgba",
        "feature-state",
        "within",
        "bogus",
        "$type",
    ];

    fn random_json(r: &mut Rng, depth: usize) -> Json {
        match r.below(if depth > 12 { 5 } else { 9 }) {
            0 => Json::Null,
            1 => json!(r.below(2) == 0),
            2 => json!((r.next() % 41) as f64 / 2.0 - 10.0),
            3 => json!(["name", "class", "a", "#f00", "red", "{name}", "$type", "Point", "", "x"][r.below(10)]),
            4 => json!({"stops": [[0, 1], [10, "#fff"]], "property": "rank", "base": 2}),
            5 if r.below(4) == 0 => json!({"x": 1}),
            _ => {
                let mut a = vec![json!(OPS[r.below(OPS.len())])];
                for _ in 0..r.below(6) {
                    a.push(random_json(r, depth + 1));
                }
                Json::Array(a)
            }
        }
    }

    #[test]
    fn fuzz_random_expressions() {
        let mut r = Rng(0x2545_f491_4f6c_dd1d);
        let feats = sample_features();
        for i in 0..5000 {
            let j = random_json(&mut r, 0);
            let (kind, p) = &feats[i % feats.len()];
            let f = FeatureCtx { zoom: (i % 23) as f32, kind: *kind, id: Some(i as u64), props: p, images: None };
            let _ = Expr::property(&j).eval(&f);
            let _ = Expr::filter(&j).eval(&f);
            let l = StyleLayer::parse(&json!({"id": "f", "type": "symbol", "filter": j.clone(),
                "paint": {"text-color": j.clone(), "line-width": j.clone(), "line-dasharray": j.clone()},
                "layout": {"text-field": j.clone(), "text-font": j.clone()}}));
            if let Some(l) = l {
                touch_all(&l, &f);
            }
        }
        // Deep nesting is capped, not a stack overflow.
        let mut deep = json!(1);
        for _ in 0..1000 {
            deep = Json::Array(vec![json!("+"), deep, json!(1)]);
        }
        let f = FeatureCtx::zoom_only(1.0);
        assert_eq!(Expr::property(&deep).eval(&f), Val::Null);
        let mut deep_lit = json!(1);
        for _ in 0..200 {
            deep_lit = Json::Array(vec![deep_lit]);
        }
        let _ = Expr::property(&json!(["literal", deep_lit])).eval(&f);
        let _ = Expr::filter(&json!(["all", deep_lit])).eval(&f);
        // Exponential string growth through let/var is capped.
        let mut e = vec![json!("let"), json!("a"), json!("xxxxxxxx")];
        for _ in 0..40 {
            e.extend([json!("a"), json!(["concat", ["var", "a"], ["var", "a"]])]);
        }
        e.push(json!(["var", "a"]));
        let v = Expr::property(&Json::Array(e)).eval(&f);
        assert_eq!(v.to_text().len(), MAX_STR);
    }

    #[test]
    fn raster_dem_sources_and_their_layers_parse() {
        use serde_json::json;
        let style = Style::parse(&json!({
            "sources": {
                "a": {"type": "raster-dem", "url": "https://x/dem.json", "tileSize": 256, "encoding": "terrarium"},
                "b": {"type": "raster-dem", "tiles": ["https://x/{z}/{x}/{y}.png"]},
                "c": {"type": "raster-dem", "tiles": [], "encoding": "custom", "redFactor": 1, "baseShift": 5}
            },
            "layers": [
                {"id": "relief", "type": "color-relief", "source": "a",
                    "paint": {"color-relief-color": ["interpolate", ["linear"], ["elevation"], 0, "#000000", 5000, "#ffffff"]}},
                {"id": "shade", "type": "hillshade", "source": "a", "paint": {
                    "hillshade-method": "multidirectional",
                    "hillshade-illumination-direction": [270, 315],
                    "hillshade-shadow-color": ["#111", "#222"],
                    "hillshade-exaggeration": 3
                }}
            ]
        }))
        .unwrap();
        assert!(matches!(
            style.source("a"),
            Some(Source::RasterDem { tile_size: 256, encoding: DemEncoding::Terrarium, url: Some(_), .. })
        ));
        assert!(matches!(style.source("b"), Some(Source::RasterDem { tile_size: 512, encoding: DemEncoding::Mapbox, .. })));
        assert!(matches!(
            style.source("c"),
            Some(Source::RasterDem { encoding: DemEncoding::Custom { red: 1.0, green: 1.0, blue: 1.0, base: 5.0 }, .. })
        ));
        assert_eq!(style.layers.iter().map(|l| l.kind).collect::<Vec<_>>(), [LayerKind::ColorRelief, LayerKind::Hillshade]);
        // Its ramp, as the drawing reads it.
        assert_eq!(style.layers[0].color_relief_ramp(10.0).map(|r| r.color(2500.0)), Some([128, 128, 128, 255]));
        let hs = style.layers[1].hillshade(10.0);
        assert_eq!(hs.method, HillshadeMethod::Multidirectional);
        assert_eq!(hs.directions, [270.0, 315.0]);
        assert_eq!(hs.altitudes, [45.0]);
        assert_eq!(hs.shadows, [[0x11, 0x11, 0x11, 255], [0x22, 0x22, 0x22, 255]]);
        assert_eq!(hs.highlights, [[255, 255, 255, 255]]);
        // Exaggeration is 0 to 1.
        assert_eq!(hs.exaggeration, 1.0);
        let plain = StyleLayer::parse(&json!({"id": "s", "type": "hillshade", "source": "a"})).unwrap().hillshade(10.0);
        assert_eq!((plain.method, plain.exaggeration, plain.directions), (HillshadeMethod::Standard, 0.5, vec![335.0]));
        // A missing `raster-dem` source's TileJSON keeps its encoding.
        let mut a = style.source("a").unwrap().clone();
        a.apply_tilejson(&json!({"tiles": ["https://x/{z}/{x}/{y}.webp"], "encoding": "mapbox", "maxzoom": 12}));
        assert!(matches!(a, Source::RasterDem { encoding: DemEncoding::Terrarium, max_zoom: 12, .. }));
    }

    #[test]
    fn elevation_ramps_colour_as_the_expression_does() {
        use serde_json::json;
        // The same ramp on zoom, which the expression language evaluates itself.
        let stops = json!([-100, "#0b1f3a", -10, "rgba(169,212,238,0.5)", 0, "#ffffff", 2, "#d4d4d4"]);
        for (head, by_zoom) in [
            (json!(["interpolate", ["linear"]]), json!(["interpolate", ["linear"], ["zoom"]])),
            (json!(["interpolate", ["exponential", 1.5]]), json!(["interpolate", ["exponential", 1.5], ["zoom"]])),
        ] {
            let mut on_height = head.as_array().unwrap().clone();
            on_height.push(json!(["elevation"]));
            on_height.extend(stops.as_array().unwrap().iter().cloned());
            let mut on_zoom = by_zoom.as_array().unwrap().clone();
            on_zoom.extend(stops.as_array().unwrap().iter().cloned());
            let ramp = Expr::property(&Json::Array(on_height)).elevation_ramp(10.0).unwrap();
            let expr = Expr::property(&Json::Array(on_zoom));
            for h in [-500.0f32, -100.0, -55.5, -10.0, -3.25, 0.0, 1.0, 2.0, 9.0] {
                assert_eq!(Some(ramp.color(f64::from(h))), expr.eval(&FeatureCtx::zoom_only(h)).as_color(), "{head} at {h}");
            }
        }
        let step = Expr::property(&json!(["step", ["elevation"], "#000000", 0, "#ff0000", 5, "#00ff00"])).elevation_ramp(3.0).unwrap();
        assert_eq!(
            [step.color(-1.0), step.color(0.0), step.color(4.9), step.color(5.0)],
            [[0, 0, 0, 255], [255, 0, 0, 255], [255, 0, 0, 255], [0, 255, 0, 255]]
        );
        assert_eq!(Expr::property(&json!("#336699")).elevation_ramp(3.0).unwrap().color(123.0), [0x33, 0x66, 0x99, 255]);
        // Not on the height, not ascending, or not colours: not a ramp.
        assert!(Expr::property(&json!(["interpolate", ["linear"], ["zoom"], 0, "#000", 5, "#fff"])).elevation_ramp(3.0).is_none());
        assert!(Expr::property(&json!(["interpolate", ["linear"], ["elevation"], 5, "#000", 0, "#fff"])).elevation_ramp(3.0).is_none());
        assert!(Expr::property(&json!(["interpolate", ["linear"], ["elevation"], 0, 1, 5, 2])).elevation_ramp(3.0).is_none());
        // Elsewhere the height is null.
        assert_eq!(Expr::property(&json!(["elevation"])).eval(&FeatureCtx::zoom_only(3.0)), Val::Null);
        // Repeated heights colour as the expression does, the first stop at the first height.
        let ramp_on = |input: &str| {
            Expr::property(&json!([
                "interpolate",
                ["linear"],
                [input],
                0,
                "#ff0000",
                0,
                "#00ff00",
                5,
                "#0000ff",
                5,
                "#ffffff",
                9,
                "#000000"
            ]))
        };
        let (ramp, by_zoom) = (ramp_on("elevation").elevation_ramp(0.0).unwrap(), ramp_on("zoom"));
        for h in [-1.0, 0.0, 2.5, 5.0, 7.0, 9.0, 10.0] {
            assert_eq!(Some(ramp.color(h)), by_zoom.eval(&FeatureCtx::zoom_only(h as f32)).as_color(), "at {h}");
        }
    }

    #[test]
    fn hillshade_lists_keep_their_places() {
        let layer = |paint: serde_json::Value| {
            StyleLayer::parse(&json!({"id": "h", "type": "hillshade", "source": "dem", "paint": paint})).unwrap().hillshade(0.0)
        };
        // An unusable entry, or one past an f32, is the default where it stands.
        let hs =
            layer(json!({"hillshade-method": "multidirectional", "hillshade-illumination-direction": ["literal", [0, "bad", 180, 1e300]],
            "hillshade-illumination-altitude": ["literal", [30, 45, 60, 75]], "hillshade-shadow-color": ["literal", ["#f00", 7]]}));
        assert_eq!(hs.directions, [0.0, 335.0, 180.0, 335.0]);
        assert_eq!(hs.altitudes, [30.0, 45.0, 60.0, 75.0]);
        assert_eq!(hs.shadows, [[255, 0, 0, 255], BLACK]);
        // Each light shades every pixel: a few are taken.
        let many: Vec<u32> = (0..1000).collect();
        let hs = layer(json!({"hillshade-method": "multidirectional", "hillshade-illumination-direction": ["literal", many]}));
        assert_eq!(hs.directions.len(), MAX_LIGHTS);
        // An empty list is the default.
        assert_eq!(layer(json!({"hillshade-illumination-direction": ["literal", []]})).directions, [335.0]);
    }
}

#[cfg(test)]
mod feature_free_tests {
    use super::*;

    #[test]
    fn expressions_reading_the_feature_are_told_apart() {
        let free = |j: Json| Expr::property(&j).feature_free();
        assert!(free(serde_json::json!(3)));
        assert!(free(serde_json::json!(["interpolate", ["linear"], ["zoom"], 5, 1, 10, 4])));
        assert!(free(serde_json::json!({"stops": [[5, 1], [10, 4]]})));
        assert!(!free(serde_json::json!(["get", "width"])));
        assert!(!free(serde_json::json!(["match", ["get", "class"], "a", 1, 2])));
        assert!(!free(serde_json::json!(["interpolate", ["linear"], ["zoom"], 5, ["get", "w"], 10, 4])));
        assert!(!free(serde_json::json!({"property": "w", "stops": [[1, 1], [2, 2]]})));
        assert!(!free(serde_json::json!(["==", ["geometry-type"], "Point"])));
    }

    #[test]
    fn filters_compare_properties_in_place_as_they_did() {
        let props = vec![("class".to_string(), Value::String("motorway".into())), ("rank".to_string(), Value::Number(3.0))];
        let f = FeatureCtx { zoom: 10.0, kind: GeomKind::LineString, id: Some(7), props: &props, images: None };
        let is = |j: Json| Expr::filter(&j).eval(&f) == Val::Bool(true);
        assert!(is(serde_json::json!(["==", "class", "motorway"])));
        assert!(!is(serde_json::json!(["!=", "class", "motorway"])));
        assert!(is(serde_json::json!(["!=", "missing", "x"])));
        assert!(is(serde_json::json!(["==", "$type", "LineString"])));
        assert!(is(serde_json::json!(["==", "$id", 7])));
        assert!(is(serde_json::json!(["==", "rank", 3])));
        assert!(!is(serde_json::json!(["==", "rank", "3"])));
        assert!(is(serde_json::json!(["in", "class", "trunk", "motorway"])));
        assert!(!is(serde_json::json!(["in", "$type", "Point", "Polygon"])));
        assert!(is(serde_json::json!(["!in", "class", "trunk"])));
        assert!(is(serde_json::json!(["==", ["get", "class"], "motorway"])));
        assert!(is(serde_json::json!(["!=", ["get", "nothing"], "motorway"])));
        assert!(is(serde_json::json!(["all", ["==", "class", "motorway"], ["<", "rank", 4]])));
    }
}
