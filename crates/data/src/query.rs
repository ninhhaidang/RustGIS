//! Expressions: SQL `WHERE` clauses for Select By Attributes and definition queries, and values
//! computed per feature for Calculate Field.
//!
//! Grammar, loosest first:
//! `or := and (OR and)*; and := not (AND not)*; not := NOT not | cmp;`
//! `cmp := concat [op concat | IS [NOT] NULL | [NOT] LIKE|ILIKE concat | [NOT] IN (expr, …) |`
//! `[NOT] BETWEEN concat AND concat]; op := = | == | <> | != | < | <= | > | >=;`
//! `concat := sum (|| sum)*; sum := product ((+|-) product)*; product := unary ((*|/|%) unary)*;`
//! `unary := (-|+) unary | power; power := primary [^ unary];`
//! `primary := number | 'text' | TRUE | FALSE | NULL | field | @variable | $variable |`
//! `function(expr, …) | CASE … END | '(' or ')'`.
//!
//! Fields are bare names, `"quoted"` or `!quoted!`; text uses single quotes (`''` is a quote).
//! Text matching (`=`, `LIKE`, `IN`) is case-insensitive; two values that read as numbers compare
//! as numbers. Logic is SQL's three-valued: a comparison with a null is unknown. One exception,
//! kept from the WHERE language this replaced so saved clauses keep their rows: a NULL in an `IN`
//! list matches nothing, so `2 NOT IN (1, NULL)` is true where SQL makes it unknown. A `LIKE`
//! too costly to match ([`MAX_LIKE_STEPS`]) is unknown. Arithmetic on a
//! null, a division by zero or an overflow gives null; `+` joins text that isn't a number.

use crate::{Dataset, Measure, Value};
use std::borrow::Cow;

#[cfg(test)]
mod legacy;

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    /// `field op literal`, the common clause, without evaluating either side as an expression.
    Cmp {
        field: usize,
        op: Op,
        lit: Value,
    },
    Like {
        field: usize,
        pattern: String,
        negate: bool,
    },
    IsNull {
        field: usize,
        negate: bool,
    },
    In {
        field: usize,
        values: Vec<Value>,
        negate: bool,
    },
    Lit(Value),
    Field(usize),
    Var(Var),
    Compare(Op, Box<Expr>, Box<Expr>),
    LikeExpr {
        e: Box<Expr>,
        pattern: Box<Expr>,
        negate: bool,
    },
    IsNullExpr {
        e: Box<Expr>,
        negate: bool,
    },
    InExpr {
        e: Box<Expr>,
        list: Vec<Expr>,
        negate: bool,
    },
    Between {
        e: Box<Expr>,
        lo: Box<Expr>,
        hi: Box<Expr>,
        negate: bool,
    },
    /// `first op term op term …`, folded from the left.
    Chain(Box<Expr>, Vec<(Arith, Expr)>),
    Concat(Vec<Expr>),
    /// `CASE [subject] WHEN … THEN … [ELSE …] END`.
    Case {
        subject: Option<Box<Expr>>,
        arms: Vec<(Expr, Expr)>,
        otherwise: Option<Box<Expr>>,
    },
    Call(Func, Vec<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Arith {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
}

/// What a variable reads from the caller's [`Env`] or the row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Var {
    /// `@map_scale`, `$scale`: the scale denominator of the map being drawn (null elsewhere).
    MapScale,
    /// `@row_number`: the row's position, from 1.
    RowNumber,
    /// `$id`: the feature's id.
    Id,
    /// `$area`, `$perimeter`, `$length`, `area(unit)`, `perimeter(unit)`, `line_length(unit)`:
    /// the feature's geometry measured on the WGS 84 ellipsoid, divided by the unit's size.
    Measure(Measure, f64),
}

/// Values an expression reads from where it runs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Env {
    pub map_scale: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Func {
    If,
    Coalesce,
    NullIf,
    Abs,
    Round,
    Floor,
    Ceil,
    Sqrt,
    Exp,
    Ln,
    Log10,
    Pi,
    Min,
    Max,
    Clamp,
    ScaleLinear,
    ScaleExp,
    ToString,
    ToInt,
    ToReal,
    Length,
    Upper,
    Lower,
    Trim,
    Substr,
    Left,
    Right,
    Replace,
    Concat,
    ColorRgb,
    ColorRgba,
    /// Parsed into [`Var::Measure`], like the two below.
    Area,
    Perimeter,
    LineLength,
}

/// Functions by name, with their fewest and most arguments.
const FUNCS: &[(&str, Func, usize, usize)] = &[
    ("if", Func::If, 2, 3),
    ("coalesce", Func::Coalesce, 1, MAX_ARGS),
    ("nullif", Func::NullIf, 2, 2),
    ("abs", Func::Abs, 1, 1),
    ("round", Func::Round, 1, 2),
    ("floor", Func::Floor, 1, 1),
    ("ceil", Func::Ceil, 1, 1),
    ("sqrt", Func::Sqrt, 1, 1),
    ("exp", Func::Exp, 1, 1),
    ("ln", Func::Ln, 1, 1),
    ("log10", Func::Log10, 1, 1),
    ("pi", Func::Pi, 0, 0),
    ("min", Func::Min, 1, MAX_ARGS),
    ("max", Func::Max, 1, MAX_ARGS),
    ("clamp", Func::Clamp, 3, 3),
    ("scale_linear", Func::ScaleLinear, 5, 5),
    ("scale_exp", Func::ScaleExp, 6, 6),
    ("to_string", Func::ToString, 1, 1),
    ("to_int", Func::ToInt, 1, 1),
    ("to_real", Func::ToReal, 1, 1),
    ("length", Func::Length, 1, 1),
    ("upper", Func::Upper, 1, 1),
    ("lower", Func::Lower, 1, 1),
    ("trim", Func::Trim, 1, 1),
    ("substr", Func::Substr, 2, 3),
    ("left", Func::Left, 2, 2),
    ("right", Func::Right, 2, 2),
    ("replace", Func::Replace, 3, 3),
    ("concat", Func::Concat, 1, MAX_ARGS),
    ("color_rgb", Func::ColorRgb, 3, 3),
    ("color_rgba", Func::ColorRgba, 4, 4),
    ("area", Func::Area, 0, 1),
    ("perimeter", Func::Perimeter, 0, 1),
    ("line_length", Func::LineLength, 0, 1),
];

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    /// A double-quoted (or `!`-quoted) field name: never a keyword or literal.
    QuotedIdent(String),
    Str(String),
    Num(f64),
    /// A comparison operator.
    Op(String),
    Arith(Arith),
    Concat,
    /// `@name` or `$name`, with its sigil.
    Var(String),
    LParen,
    RParen,
    Comma,
}

/// Longest clause accepted (characters).
pub const MAX_LEN: usize = 10_000;
/// Deepest nesting accepted.
const MAX_DEPTH: usize = 64;
/// Most terms in one chain (AND, OR, `+`, `||`, CASE arms).
const MAX_TERMS: usize = 256;
/// Most arguments of one function.
const MAX_ARGS: usize = 32;
/// Longest text an expression makes (bytes); longer results are cut.
pub const MAX_TEXT: usize = 64 * 1024;

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    if s.chars().count() > MAX_LEN {
        return Err("the expression is too long".into());
    }
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    let word = |i: &mut usize| -> String {
        let st = *i;
        while cs.get(*i).is_some_and(|c| c.is_alphanumeric() || *c == '_') {
            *i += 1;
        }
        cs.get(st..*i).map(|s| s.iter().collect()).unwrap_or_default()
    };
    while let Some(&c) = cs.get(i) {
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                out.push(Tok::LParen);
                i += 1
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1
            }
            '\'' | '"' => {
                let q = c;
                let mut v = String::new();
                i += 1;
                loop {
                    match cs.get(i) {
                        None => return Err("unterminated quote".into()),
                        Some(&ch) if ch == q => {
                            if cs.get(i + 1) == Some(&q) {
                                v.push(q);
                                i += 2;
                            } else {
                                i += 1;
                                break;
                            }
                        }
                        Some(&ch) => {
                            v.push(ch);
                            i += 1
                        }
                    }
                }
                out.push(if q == '"' { Tok::QuotedIdent(v) } else { Tok::Str(v) });
            }
            '!' if cs.get(i + 1) != Some(&'=') => {
                let st = i + 1;
                let end = cs.get(st..).and_then(|r| r.iter().position(|c| *c == '!')).ok_or("unexpected '!'")?;
                out.push(Tok::QuotedIdent(cs.get(st..st + end).map(|s| s.iter().collect()).unwrap_or_default()));
                i = st + end + 1;
            }
            '=' | '<' | '>' | '!' => {
                let mut op = c.to_string();
                if let Some(&n) = cs.get(i + 1)
                    && (n == '=' || (c == '<' && n == '>'))
                {
                    op.push(n);
                    i += 1;
                }
                i += 1;
                out.push(Tok::Op(op));
            }
            '|' if cs.get(i + 1) == Some(&'|') => {
                out.push(Tok::Concat);
                i += 2;
            }
            '+' | '-' | '*' | '/' | '%' | '^' => {
                out.push(Tok::Arith(match c {
                    '+' => Arith::Add,
                    '-' => Arith::Sub,
                    '*' => Arith::Mul,
                    '/' => Arith::Div,
                    '%' => Arith::Mod,
                    _ => Arith::Pow,
                }));
                i += 1;
            }
            c if c.is_ascii_digit() || c == '.' => {
                let st = i;
                while cs.get(i).is_some_and(|c| c.is_ascii_digit() || *c == '.') {
                    i += 1;
                }
                // An exponent: `e`, an optional sign, digits.
                if cs.get(i).is_some_and(|c| *c == 'e' || *c == 'E') {
                    let sign = usize::from(cs.get(i + 1).is_some_and(|c| *c == '+' || *c == '-'));
                    if cs.get(i + 1 + sign).is_some_and(|c| c.is_ascii_digit()) {
                        i += 1 + sign;
                        while cs.get(i).is_some_and(|c| c.is_ascii_digit()) {
                            i += 1;
                        }
                    }
                }
                let t: String = cs.get(st..i).map(|s| s.iter().collect()).unwrap_or_default();
                out.push(Tok::Num(t.parse().map_err(|_| format!("bad number '{t}'"))?));
            }
            '@' | '$' => {
                i += 1;
                let name = word(&mut i);
                if name.is_empty() {
                    return Err(format!("unexpected '{c}'"));
                }
                out.push(Tok::Var(format!("{c}{name}")));
            }
            c if c.is_alphanumeric() || c == '_' => out.push(Tok::Ident(word(&mut i))),
            other => return Err(format!("unexpected '{other}'")),
        }
    }
    Ok(out)
}

/// A number literal: whole numbers are integers.
fn num(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9e15 { Value::Int(n as i64) } else { Value::Float(n) }
}

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    ds: &'a Dataset,
    depth: usize,
}

impl Parser<'_> {
    fn peek_kw(&self, kw: &str) -> bool {
        self.kw_at(self.pos, kw)
    }
    fn kw_at(&self, i: usize, kw: &str) -> bool {
        matches!(self.toks.get(i), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case(kw))
    }
    fn eat_kw(&mut self, kw: &str) -> bool {
        let y = self.peek_kw(kw);
        if y {
            self.pos += 1;
        }
        y
    }
    fn eat(&mut self, t: &Tok) -> bool {
        let y = self.toks.get(self.pos) == Some(t);
        if y {
            self.pos += 1;
        }
        y
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn deeper(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("the expression is nested too deeply".into());
        }
        Ok(())
    }
    fn expect(&mut self, t: Tok, what: &str) -> Result<(), String> {
        if self.eat(&t) { Ok(()) } else { Err(format!("expected {what}, found {}", show(self.toks.get(self.pos)))) }
    }
    fn or(&mut self) -> Result<Expr, String> {
        self.deeper()?;
        let mut terms = vec![self.and()?];
        while self.eat_kw("OR") {
            if terms.len() >= MAX_TERMS {
                return Err("the expression has too many terms".into());
            }
            terms.push(self.and()?);
        }
        self.depth -= 1;
        Ok(
            if terms.len() == 1
                && let Some(t) = terms.pop()
            {
                t
            } else {
                Expr::Or(terms)
            },
        )
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut terms = vec![self.not()?];
        while self.eat_kw("AND") {
            if terms.len() >= MAX_TERMS {
                return Err("the expression has too many terms".into());
            }
            terms.push(self.not()?);
        }
        Ok(
            if terms.len() == 1
                && let Some(t) = terms.pop()
            {
                t
            } else {
                Expr::And(terms)
            },
        )
    }
    fn not(&mut self) -> Result<Expr, String> {
        if self.eat_kw("NOT") {
            self.deeper()?;
            let e = self.not()?;
            self.depth -= 1;
            return Ok(Expr::Not(Box::new(e)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let l = self.concat()?;
        if self.eat_kw("IS") {
            let negate = self.eat_kw("NOT");
            if !self.eat_kw("NULL") {
                return Err("expected NULL after IS".into());
            }
            return Ok(match l {
                Expr::Field(field) => Expr::IsNull { field, negate },
                e => Expr::IsNullExpr { e: Box::new(e), negate },
            });
        }
        let negate = self.eat_kw("NOT");
        if self.eat_kw("LIKE") || self.eat_kw("ILIKE") {
            let p = self.concat()?;
            return Ok(match (l, p) {
                (Expr::Field(field), Expr::Lit(Value::Text(pattern))) => Expr::Like { field, pattern, negate },
                (e, p) => Expr::LikeExpr { e: Box::new(e), pattern: Box::new(p), negate },
            });
        }
        if self.eat_kw("IN") {
            self.expect(Tok::LParen, "'(' after IN")?;
            let mut list = vec![self.item()?];
            loop {
                match self.next() {
                    Some(Tok::Comma) => list.push(self.item()?),
                    Some(Tok::RParen) => break,
                    other => return Err(format!("expected ',' or ')', found {}", show(other.as_ref()))),
                }
            }
            return Ok(match l {
                Expr::Field(field) if list.iter().all(|e| matches!(e, Expr::Lit(_))) => {
                    let values = list.into_iter().filter_map(|e| if let Expr::Lit(v) = e { Some(v) } else { None }).collect();
                    Expr::In { field, values, negate }
                }
                e => Expr::InExpr { e: Box::new(e), list, negate },
            });
        }
        if self.eat_kw("BETWEEN") {
            let lo = self.concat()?;
            if !self.eat_kw("AND") {
                return Err("expected AND after BETWEEN …".into());
            }
            let hi = self.concat()?;
            return Ok(Expr::Between { e: Box::new(l), lo: Box::new(lo), hi: Box::new(hi), negate });
        }
        if negate {
            return Err("expected LIKE, IN or BETWEEN after NOT".into());
        }
        let op = match self.toks.get(self.pos) {
            Some(Tok::Op(o)) => match o.as_str() {
                "=" | "==" => Op::Eq,
                "<>" | "!=" => Op::Ne,
                "<" => Op::Lt,
                "<=" => Op::Le,
                ">" => Op::Gt,
                ">=" => Op::Ge,
                _ => return Err(format!("unknown operator '{o}'")),
            },
            _ => return Ok(l),
        };
        self.pos += 1;
        let r = self.concat()?;
        Ok(match (l, r) {
            (Expr::Field(field), Expr::Lit(lit)) => Expr::Cmp { field, op, lit },
            (l, r) => Expr::Compare(op, Box::new(l), Box::new(r)),
        })
    }
    /// An item of an IN list. A single value (`-2`, `'x'`, `NULL`, a field) is at the clause's
    /// own depth, as the items were once only literals; anything else nests, so
    /// `1 IN (1 IN (…))` meets the limit.
    fn item(&mut self) -> Result<Expr, String> {
        let sign = usize::from(matches!(self.toks.get(self.pos), Some(Tok::Arith(Arith::Add | Arith::Sub))));
        let single =
            matches!(self.toks.get(self.pos + sign), Some(Tok::Num(_) | Tok::Str(_) | Tok::Ident(_) | Tok::QuotedIdent(_) | Tok::Var(_)))
                && matches!(self.toks.get(self.pos + sign + 1), Some(Tok::Comma | Tok::RParen));
        if single {
            return self.unary();
        }
        self.or()
    }
    fn concat(&mut self) -> Result<Expr, String> {
        let mut terms = vec![self.sum()?];
        while self.eat(&Tok::Concat) {
            if terms.len() >= MAX_TERMS {
                return Err("the expression has too many terms".into());
            }
            terms.push(self.sum()?);
        }
        Ok(
            if terms.len() == 1
                && let Some(t) = terms.pop()
            {
                t
            } else {
                Expr::Concat(terms)
            },
        )
    }
    /// A chain of `ops` over `term`s, folded from the left.
    fn chain(&mut self, ops: &[Arith], term: fn(&mut Self) -> Result<Expr, String>) -> Result<Expr, String> {
        let first = term(self)?;
        let mut rest = Vec::new();
        while let Some(Tok::Arith(a)) = self.toks.get(self.pos).cloned()
            && ops.contains(&a)
        {
            self.pos += 1;
            if rest.len() + 1 >= MAX_TERMS {
                return Err("the expression has too many terms".into());
            }
            rest.push((a, term(self)?));
        }
        Ok(if rest.is_empty() { first } else { Expr::Chain(Box::new(first), rest) })
    }
    fn sum(&mut self) -> Result<Expr, String> {
        self.chain(&[Arith::Add, Arith::Sub], Self::product)
    }
    fn product(&mut self) -> Result<Expr, String> {
        self.chain(&[Arith::Mul, Arith::Div, Arith::Mod], Self::unary)
    }
    fn unary(&mut self) -> Result<Expr, String> {
        let sign = match self.toks.get(self.pos) {
            Some(Tok::Arith(Arith::Sub)) => -1,
            Some(Tok::Arith(Arith::Add)) => 1,
            _ => return self.power(),
        };
        self.pos += 1;
        // A sign before a number is part of the literal and nests nothing.
        let nests = !matches!(self.toks.get(self.pos), Some(Tok::Num(_)));
        if nests {
            self.deeper()?;
        }
        let e = self.unary()?;
        if nests {
            self.depth -= 1;
        }
        Ok(match (sign, e) {
            (1, e) => e,
            // A negative number stays a literal, so `pop > -5` keeps the fast comparison.
            (_, Expr::Lit(Value::Int(i))) => Expr::Lit(i.checked_neg().map(Value::Int).unwrap_or(Value::Float(-(i as f64)))),
            (_, Expr::Lit(Value::Float(f))) => Expr::Lit(Value::Float(-f)),
            (_, e) => Expr::Chain(Box::new(Expr::Lit(Value::Int(0))), vec![(Arith::Sub, e)]),
        })
    }
    fn power(&mut self) -> Result<Expr, String> {
        let base = self.primary()?;
        if !self.eat(&Tok::Arith(Arith::Pow)) {
            return Ok(base);
        }
        self.deeper()?;
        let exp = self.unary()?;
        self.depth -= 1;
        Ok(Expr::Chain(Box::new(base), vec![(Arith::Pow, exp)]))
    }
    /// Whether the token at `i` starts the operator of a comparison.
    fn comparison_at(&self, i: usize) -> bool {
        matches!(self.toks.get(i), Some(Tok::Op(_))) || ["IS", "LIKE", "ILIKE", "IN", "NOT", "BETWEEN"].iter().any(|k| self.kw_at(i, k))
    }
    fn field(&self, name: &str) -> Result<Expr, String> {
        self.ds.field_index(name).map(Expr::Field).ok_or_else(|| format!("no field named '{name}'"))
    }
    /// `m` in units of `size`, on a layer of the kind it measures.
    fn measure(&self, name: &str, m: Measure, size: f64) -> Result<Expr, String> {
        let want = m.geometry_type();
        if self.ds.geometry_type != want {
            let hint = if m == Measure::Length { " (use perimeter on polygons)" } else { "" };
            return Err(format!("'{name}' measures {} layers{hint}", want.label().to_ascii_lowercase()));
        }
        Ok(Expr::Var(Var::Measure(m, size)))
    }
    /// A value: a literal, a field, a variable (a measure only on a layer it fits), a call, a
    /// `CASE` or a bracketed expression.
    fn primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::LParen) => {
                let e = self.or()?;
                if self.next() != Some(Tok::RParen) {
                    return Err("missing ')'".into());
                }
                Ok(e)
            }
            // A number too large to represent (`1e999`) would be infinity, which nothing reads.
            Some(Tok::Num(n)) if n.is_finite() => Ok(Expr::Lit(num(n))),
            Some(Tok::Num(_)) => Err("the number is too large".into()),
            Some(Tok::Str(s)) => Ok(Expr::Lit(Value::Text(s))),
            Some(Tok::QuotedIdent(s)) => self.field(&s),
            Some(Tok::Var(v)) => match v.to_ascii_lowercase().as_str() {
                "@map_scale" | "$scale" => Ok(Expr::Var(Var::MapScale)),
                "@row_number" => Ok(Expr::Var(Var::RowNumber)),
                "$id" => Ok(Expr::Var(Var::Id)),
                name => match name.strip_prefix('$').and_then(Measure::from_key) {
                    Some(m) => self.measure(&v, m, 1.0),
                    None => Err(format!("unknown variable '{v}'")),
                },
            },
            Some(Tok::Ident(s)) => {
                if self.toks.get(self.pos) == Some(&Tok::LParen) {
                    return self.call(&s);
                }
                // A field named like a keyword is still read as the field before a comparison
                // (`null = 'x'`), as the WHERE language always has.
                let lower = s.to_ascii_lowercase();
                let keyword = matches!(lower.as_str(), "true" | "false" | "null" | "case")
                    && !(self.ds.field_index(&s).is_some() && self.comparison_at(self.pos));
                match (keyword, lower.as_str()) {
                    (true, "true") => Ok(Expr::Lit(Value::Bool(true))),
                    (true, "false") => Ok(Expr::Lit(Value::Bool(false))),
                    (true, "null") => Ok(Expr::Lit(Value::Null)),
                    (true, _) => self.case(),
                    _ => self.field(&s),
                }
            }
            other => Err(format!("expected a value, found {}", show(other.as_ref()))),
        }
    }
    /// A call of the function `name`, its arguments counted; a measure's call becomes the
    /// variable it reads, its unit (fixed text) checked here.
    fn call(&mut self, name: &str) -> Result<Expr, String> {
        let &(fname, f, min, max) =
            FUNCS.iter().find(|(n, ..)| n.eq_ignore_ascii_case(name)).ok_or_else(|| format!("unknown function '{name}'"))?;
        self.pos += 1; // '('
        let mut args = Vec::new();
        if !self.eat(&Tok::RParen) {
            loop {
                if args.len() >= MAX_ARGS {
                    return Err(format!("{fname} has too many arguments"));
                }
                args.push(self.or()?);
                match self.next() {
                    Some(Tok::Comma) => {}
                    Some(Tok::RParen) => break,
                    other => return Err(format!("expected ',' or ')', found {}", show(other.as_ref()))),
                }
            }
        }
        if args.len() < min || args.len() > max {
            let n = if min == max {
                min.to_string()
            } else if max == MAX_ARGS {
                format!("{min} or more")
            } else {
                format!("{min} to {max}")
            };
            return Err(format!("{fname} takes {n} argument{}", if n == "1" { "" } else { "s" }));
        }
        let m = match f {
            Func::Area => Measure::Area,
            Func::Perimeter => Measure::Perimeter,
            Func::LineLength => Measure::Length,
            _ => return Ok(Expr::Call(f, args)),
        };
        // The unit is fixed text, so an unknown one is an error here rather than a null per row.
        let size = match args.first() {
            None => 1.0,
            Some(Expr::Lit(Value::Text(u))) => m.unit(u).ok_or_else(|| {
                let keys: Vec<&str> = m.units().iter().map(|u| u.0).collect();
                format!("{fname} has no unit '{u}': use one of {}", keys.join(", "))
            })?,
            // QGIS's area($geometry) and the like land here.
            Some(_) => {
                let example = if m == Measure::Area { "ha" } else { "km" };
                return Err(format!("{fname} measures the feature's own geometry: its argument is a unit as text, such as '{example}'"));
            }
        };
        self.measure(fname, m, size)
    }
    fn case(&mut self) -> Result<Expr, String> {
        self.deeper()?;
        let subject = if self.peek_kw("WHEN") { None } else { Some(Box::new(self.or()?)) };
        let mut arms = Vec::new();
        while self.eat_kw("WHEN") {
            if arms.len() >= MAX_TERMS {
                return Err("the expression has too many terms".into());
            }
            let when = self.or()?;
            if !self.eat_kw("THEN") {
                return Err(format!("expected THEN, found {}", show(self.toks.get(self.pos))));
            }
            arms.push((when, self.or()?));
        }
        if arms.is_empty() {
            return Err(format!("expected WHEN, found {}", show(self.toks.get(self.pos))));
        }
        let otherwise = if self.eat_kw("ELSE") { Some(Box::new(self.or()?)) } else { None };
        if !self.eat_kw("END") {
            return Err(format!("expected END, found {}", show(self.toks.get(self.pos))));
        }
        self.depth -= 1;
        Ok(Expr::Case { subject, arms, otherwise })
    }
}

fn show(t: Option<&Tok>) -> String {
    match t {
        None => "the end".into(),
        Some(Tok::Ident(s)) => format!("'{s}'"),
        Some(Tok::QuotedIdent(s)) => format!("\"{s}\""),
        Some(Tok::Str(s)) => format!("'{s}'"),
        Some(Tok::Num(n)) => n.to_string(),
        Some(Tok::Op(o)) => format!("'{o}'"),
        Some(Tok::Arith(a)) => format!(
            "'{}'",
            match a {
                Arith::Add => '+',
                Arith::Sub => '-',
                Arith::Mul => '*',
                Arith::Div => '/',
                Arith::Mod => '%',
                Arith::Pow => '^',
            }
        ),
        Some(Tok::Concat) => "'||'".into(),
        Some(Tok::Var(v)) => format!("'{v}'"),
        Some(Tok::LParen) => "'('".into(),
        Some(Tok::RParen) => "')'".into(),
        Some(Tok::Comma) => "','".into(),
    }
}

/// Parses an expression (a clause, or a value) against a dataset's schema.
pub fn parse(text: &str, ds: &Dataset) -> Result<Expr, String> {
    let toks = lex(text)?;
    if toks.is_empty() {
        return Err("the expression is empty".into());
    }
    let mut p = Parser { toks, pos: 0, ds, depth: 0 };
    let e = p.or()?;
    if p.pos < p.toks.len() {
        return Err(format!("unexpected {}", show(p.toks.get(p.pos))));
    }
    Ok(e)
}

fn eq(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) if !matches!(a, Value::Text(_)) || !matches!(b, Value::Text(_)) => x == y,
        _ => a.to_string().eq_ignore_ascii_case(&b.to_string()),
    }
}

/// A comparison of two values: unknown when either is null.
fn compare(a: &Value, op: Op, b: &Value) -> Option<bool> {
    if a.is_null() || b.is_null() {
        return None;
    }
    Some(match op {
        Op::Eq => eq(a, b),
        Op::Ne => !eq(a, b),
        _ => match a.partial_cmp(b) {
            Some(o) => match op {
                Op::Lt => o.is_lt(),
                Op::Le => o.is_le(),
                Op::Gt => o.is_gt(),
                Op::Ge => o.is_ge(),
                _ => false,
            },
            None => false,
        },
    })
}

/// The most steps one `LIKE` takes to match. A long text against a pattern of many `%`s costs
/// up to text × pattern steps (64 KiB of text and a 10,000-character pattern, 6.5 × 10⁸); past
/// this (a tenth of a second or so), the match is unknown rather than holding up the map.
pub const MAX_LIKE_STEPS: usize = 1 << 24;

/// SQL LIKE with `%` and `_`, case-insensitive; no match when it would take more than
/// [`MAX_LIKE_STEPS`].
pub fn like(text: &str, pat: &str) -> bool {
    like_within(text, pat).unwrap_or(false)
}

/// SQL LIKE with `%` and `_`, case-insensitive: `None` when it would take more than
/// [`MAX_LIKE_STEPS`].
fn like_within(text: &str, pat: &str) -> Option<bool> {
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    // Iterative wildcard matching with backtracking on the last '%'.
    let (mut ti, mut pi) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    let mut steps = 0usize;
    while ti < t.len() {
        steps += 1;
        if steps > MAX_LIKE_STEPS {
            return None;
        }
        match p.get(pi) {
            Some('%') => {
                star = Some(pi);
                mark = ti;
                pi += 1;
            }
            Some(c) if *c == '_' || Some(c) == t.get(ti) => {
                ti += 1;
                pi += 1;
            }
            _ => match star {
                Some(s) => {
                    pi = s + 1;
                    mark += 1;
                    ti = mark;
                }
                None => return Some(false),
            },
        }
    }
    Some(p.get(pi..).is_some_and(|rest| rest.iter().all(|c| *c == '%')))
}

/// Evaluates for one row: whether the row matches.
pub fn eval(e: &Expr, ds: &Dataset, row: usize) -> bool {
    matches(e, ds, row, &Env::default())
}

/// Whether the row matches, with variables from `env`.
pub fn matches(e: &Expr, ds: &Dataset, row: usize, env: &Env) -> bool {
    Eval { ds, row, env }.cond(e) == Some(true)
}

/// The expression's value for one row.
pub fn value(e: &Expr, ds: &Dataset, row: usize, env: &Env) -> Value {
    Eval { ds, row, env }.value(e).into_owned()
}

/// Whether a value counts as true: a non-zero number, non-empty text; null is unknown.
fn truth(v: &Value) -> Option<bool> {
    match v {
        Value::Null => None,
        Value::Bool(b) => Some(*b),
        Value::Int(i) => Some(*i != 0),
        Value::Float(f) => (!f.is_nan()).then_some(*f != 0.0),
        Value::Text(s) => Some(!s.is_empty()),
    }
}

fn bool3(b: Option<bool>) -> Value {
    b.map(Value::Bool).unwrap_or(Value::Null)
}

/// Text, cut to [`MAX_TEXT`] at a character boundary.
fn text(mut s: String) -> Value {
    if s.len() > MAX_TEXT {
        let cut = s.char_indices().map(|(i, _)| i).take_while(|i| *i <= MAX_TEXT).last().unwrap_or(0);
        s.truncate(cut);
    }
    Value::Text(s)
}

/// A float result: null when it isn't a finite number.
fn real(f: f64) -> Value {
    if f.is_finite() { Value::Float(f) } else { Value::Null }
}

/// A whole-number result: an integer when it fits.
fn whole(f: f64) -> Value {
    if f.is_finite() && f.abs() < 9e15 { Value::Int(f as i64) } else { real(f) }
}

fn arith(op: Arith, a: &Value, b: &Value) -> Value {
    if a.is_null() || b.is_null() {
        return Value::Null;
    }
    let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) else {
        return if op == Arith::Add { text(format!("{a}{b}")) } else { Value::Null };
    };
    if let (Value::Int(i), Value::Int(j)) = (a, b) {
        let exact = match op {
            Arith::Add => i.checked_add(*j),
            Arith::Sub => i.checked_sub(*j),
            Arith::Mul => i.checked_mul(*j),
            Arith::Mod => return i.checked_rem(*j).map(Value::Int).unwrap_or(Value::Null),
            _ => None,
        };
        if let Some(v) = exact {
            return Value::Int(v);
        }
    }
    match op {
        Arith::Add => real(x + y),
        Arith::Sub => real(x - y),
        Arith::Mul => real(x * y),
        Arith::Div if y != 0.0 => real(x / y),
        Arith::Mod if y != 0.0 => real(x % y),
        Arith::Pow => real(x.powf(y)),
        _ => Value::Null,
    }
}

/// `text` with `from` replaced by `to`, stopping at [`MAX_TEXT`].
fn replace(s: &str, from: &str, to: &str) -> Value {
    if from.is_empty() {
        return text(s.to_string());
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(from) {
        out.push_str(rest.get(..i).unwrap_or(""));
        out.push_str(to);
        rest = rest.get(i + from.len()..).unwrap_or("");
        if out.len() > MAX_TEXT {
            return text(out);
        }
    }
    out.push_str(rest);
    text(out)
}

fn channel(v: f64) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

struct Eval<'a> {
    ds: &'a Dataset,
    row: usize,
    env: &'a Env,
}

impl<'a> Eval<'a> {
    /// SQL's three-valued logic: `None` is "unknown", what a comparison with a null gives. `NOT`
    /// leaves it unknown, so `NOT (pop > 100)` doesn't match a row whose `pop` is null.
    fn cond(&self, e: &'a Expr) -> Option<bool> {
        let (ds, row) = (self.ds, self.row);
        match e {
            Expr::And(terms) => {
                let mut all = Some(true);
                for t in terms {
                    match self.cond(t) {
                        Some(false) => return Some(false),
                        None => all = None,
                        Some(true) => {}
                    }
                }
                all
            }
            Expr::Or(terms) => {
                let mut any = Some(false);
                for t in terms {
                    match self.cond(t) {
                        Some(true) => return Some(true),
                        None => any = None,
                        Some(false) => {}
                    }
                }
                any
            }
            Expr::Not(a) => self.cond(a).map(|v| !v),
            Expr::IsNull { field, negate } => Some(ds.value(row, *field).is_null() != *negate),
            Expr::Like { field, pattern, negate } => {
                let v = ds.value(row, *field);
                if v.is_null() {
                    return None;
                }
                like_within(&v.to_string(), pattern).map(|m| m != *negate)
            }
            Expr::In { field, values, negate } => {
                let v = ds.value(row, *field);
                (!v.is_null()).then(|| values.iter().any(|x| !x.is_null() && eq(v, x)) != *negate)
            }
            Expr::Cmp { field, op, lit } => compare(ds.value(row, *field), *op, lit),
            Expr::Compare(op, l, r) => compare(&self.value(l), *op, &self.value(r)),
            Expr::IsNullExpr { e, negate } => Some(self.value(e).is_null() != *negate),
            Expr::LikeExpr { e, pattern, negate } => {
                let (v, p) = (self.value(e), self.value(pattern));
                // A pattern no longer than a clause, as a written one is: matching costs text × pattern.
                if v.is_null() || p.is_null() {
                    return None;
                }
                let p = p.to_string();
                // A longer pattern matches nothing, so NOT LIKE holds.
                if p.chars().count() > MAX_LEN {
                    return Some(*negate);
                }
                like_within(&v.to_string(), &p).map(|m| m != *negate)
            }
            Expr::InExpr { e, list, negate } => {
                let v = self.value(e);
                (!v.is_null()).then(|| {
                    list.iter().any(|x| {
                        let x = self.value(x);
                        !x.is_null() && eq(&v, &x)
                    }) != *negate
                })
            }
            Expr::Between { e, lo, hi, negate } => {
                let v = self.value(e);
                let within = match (compare(&v, Op::Ge, &self.value(lo)), compare(&v, Op::Le, &self.value(hi))) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                };
                within.map(|w| w != *negate)
            }
            other => truth(&self.value(other)),
        }
    }

    /// `e`'s value on this row: null where it can't be worked out (a missing field, an error).
    fn value(&self, e: &'a Expr) -> Cow<'a, Value> {
        match e {
            Expr::Lit(v) => Cow::Borrowed(v),
            Expr::Field(i) => Cow::Borrowed(self.ds.value(self.row, *i)),
            Expr::Var(v) => Cow::Owned(match v {
                Var::MapScale => self.env.map_scale.map(real).unwrap_or(Value::Null),
                Var::RowNumber => i64::try_from(self.row).ok().and_then(|r| r.checked_add(1)).map(Value::Int).unwrap_or(Value::Null),
                Var::Id => self.ds.features.get(self.row).and_then(|f| i64::try_from(f.id).ok()).map(Value::Int).unwrap_or(Value::Null),
                Var::Measure(m, size) => self
                    .ds
                    .features
                    .get(self.row)
                    .and_then(|f| f.geometry.as_ref())
                    .and_then(|g| g.wgs84_measure(*m))
                    // A hole larger than its polygon measures below zero: no sensible area.
                    .filter(|v| *v >= 0.0)
                    .map(|v| real(v / size))
                    .unwrap_or(Value::Null),
            }),
            Expr::Chain(first, rest) => {
                let mut acc = self.value(first).into_owned();
                for (op, t) in rest {
                    acc = arith(*op, &acc, &self.value(t));
                }
                Cow::Owned(acc)
            }
            Expr::Concat(terms) => {
                let mut s = String::new();
                for t in terms {
                    let v = self.value(t);
                    if v.is_null() {
                        return Cow::Owned(Value::Null);
                    }
                    if s.len() <= MAX_TEXT {
                        s.push_str(&v.to_string());
                    }
                }
                Cow::Owned(text(s))
            }
            Expr::Case { subject, arms, otherwise } => {
                let subject = subject.as_ref().map(|s| self.value(s));
                for (when, then) in arms {
                    let hit = match &subject {
                        Some(s) => compare(s, Op::Eq, &self.value(when)),
                        None => self.cond(when),
                    };
                    if hit == Some(true) {
                        return self.value(then);
                    }
                }
                match otherwise {
                    Some(o) => self.value(o),
                    None => Cow::Owned(Value::Null),
                }
            }
            Expr::Call(f, args) => self.call(*f, args),
            logical => Cow::Owned(bool3(self.cond(logical))),
        }
    }

    /// The value of the function `f` over `args` on this row.
    fn call(&self, f: Func, args: &'a [Expr]) -> Cow<'a, Value> {
        let arg = |i: usize| args.get(i).map(|a| self.value(a)).unwrap_or(Cow::Owned(Value::Null));
        let n = |i: usize| arg(i).as_f64();
        let txt = |i: usize| {
            let v = arg(i);
            (!v.is_null()).then(|| v.to_string())
        };
        let num1 = |g: fn(f64) -> f64| Cow::Owned(n(0).map(|x| real(g(x))).unwrap_or(Value::Null));
        let text1 = |g: fn(&str) -> String| Cow::Owned(txt(0).map(|s| text(g(&s))).unwrap_or(Value::Null));
        Cow::Owned(match f {
            Func::If => {
                let pick = if args.first().and_then(|c| self.cond(c)) == Some(true) { 1 } else { 2 };
                return arg(pick);
            }
            Func::Coalesce => {
                for a in args {
                    let v = self.value(a);
                    if !v.is_null() {
                        return v;
                    }
                }
                Value::Null
            }
            Func::NullIf => {
                let (a, b) = (arg(0), arg(1));
                if compare(&a, Op::Eq, &b) == Some(true) {
                    Value::Null
                } else {
                    return a;
                }
            }
            Func::Abs => match &*arg(0) {
                Value::Int(i) => i.checked_abs().map(Value::Int).unwrap_or(Value::Null),
                v => v.as_f64().map(|x| real(x.abs())).unwrap_or(Value::Null),
            },
            Func::Round => {
                let places = if args.len() > 1 { n(1).map(|p| p.clamp(-15.0, 15.0) as i32) } else { Some(0) };
                match (arg(0).into_owned(), places) {
                    (Value::Int(i), Some(p)) if p >= 0 => Value::Int(i),
                    (v, Some(p)) => match v.as_f64() {
                        Some(x) => {
                            let m = 10f64.powi(p);
                            // So large it has no places to round: as it is.
                            let r = if (x * m).is_finite() { (x * m).round() / m } else { x };
                            if p <= 0 { whole(r) } else { real(r) }
                        }
                        None => Value::Null,
                    },
                    _ => Value::Null,
                }
            }
            Func::Floor => n(0).map(|x| whole(x.floor())).unwrap_or(Value::Null),
            Func::Ceil => n(0).map(|x| whole(x.ceil())).unwrap_or(Value::Null),
            Func::Sqrt => return num1(f64::sqrt),
            Func::Exp => return num1(f64::exp),
            Func::Ln => return num1(f64::ln),
            Func::Log10 => return num1(f64::log10),
            Func::Pi => Value::Float(std::f64::consts::PI),
            Func::Min | Func::Max => {
                let mut best: Option<(f64, Cow<'a, Value>)> = None;
                for a in args {
                    let v = self.value(a);
                    let Some(x) = v.as_f64() else { continue };
                    let better = match &best {
                        None => true,
                        Some((b, _)) => (f == Func::Min && x < *b) || (f == Func::Max && x > *b),
                    };
                    if better {
                        best = Some((x, v));
                    }
                }
                return best.map(|(_, v)| v).unwrap_or(Cow::Owned(Value::Null));
            }
            Func::Clamp => {
                // Each argument once: evaluated again, nested clamps would cost 2^depth.
                let (lo, x, hi) = (arg(0), arg(1), arg(2));
                match (lo.as_f64(), x.as_f64(), hi.as_f64()) {
                    (Some(l), Some(v), Some(h)) if l <= h => {
                        return if v < l {
                            lo
                        } else if v > h {
                            hi
                        } else {
                            x
                        };
                    }
                    _ => Value::Null,
                }
            }
            Func::ScaleLinear | Func::ScaleExp => {
                let exponent = if f == Func::ScaleExp { n(5) } else { Some(1.0) };
                match (n(0), n(1), n(2), n(3), n(4), exponent) {
                    (Some(x), Some(d0), Some(d1), Some(r0), Some(r1), Some(e)) => {
                        let t = if d1 == d0 { if x <= d0 { 0.0 } else { 1.0 } } else { ((x - d0) / (d1 - d0)).clamp(0.0, 1.0) };
                        real(r0 + (r1 - r0) * t.powf(e))
                    }
                    _ => Value::Null,
                }
            }
            Func::ToString => txt(0).map(text).unwrap_or(Value::Null),
            Func::ToInt => match &*arg(0) {
                Value::Int(i) => Value::Int(*i),
                v => v.as_f64().map(|x| whole(x.trunc())).unwrap_or(Value::Null),
            },
            Func::ToReal => n(0).map(Value::Float).unwrap_or(Value::Null),
            Func::Length => txt(0).map(|s| Value::Int(s.chars().count() as i64)).unwrap_or(Value::Null),
            Func::Upper => return text1(str::to_uppercase),
            Func::Lower => return text1(str::to_lowercase),
            Func::Trim => return text1(|s| s.trim().to_string()),
            Func::Substr => match (txt(0), n(1)) {
                (Some(s), Some(start)) => {
                    let cs: Vec<char> = s.chars().collect();
                    let len = cs.len() as f64;
                    let from = if start > 0.0 {
                        start - 1.0
                    } else if start < 0.0 {
                        (len + start).max(0.0)
                    } else {
                        0.0
                    };
                    let to = match args.get(2).map(|_| n(2)) {
                        None => len,
                        Some(Some(l)) if l < 0.0 => len + l,
                        Some(Some(l)) => from + l,
                        Some(None) => return Cow::Owned(Value::Null),
                    };
                    let (from, to) = (from.clamp(0.0, len) as usize, to.clamp(0.0, len) as usize);
                    text(cs.get(from..to.max(from)).map(|c| c.iter().collect()).unwrap_or_default())
                }
                _ => Value::Null,
            },
            Func::Left | Func::Right => match (txt(0), n(1)) {
                (Some(s), Some(k)) => {
                    let cs: Vec<char> = s.chars().collect();
                    let k = k.clamp(0.0, cs.len() as f64) as usize;
                    let part = if f == Func::Left { cs.get(..k) } else { cs.get(cs.len() - k..) };
                    text(part.map(|c| c.iter().collect()).unwrap_or_default())
                }
                _ => Value::Null,
            },
            Func::Replace => match (txt(0), txt(1), txt(2)) {
                (Some(s), Some(from), Some(to)) => replace(&s, &from, &to),
                _ => Value::Null,
            },
            Func::Concat => {
                let mut s = String::new();
                for a in args {
                    let v = self.value(a);
                    if !v.is_null() && s.len() <= MAX_TEXT {
                        s.push_str(&v.to_string());
                    }
                }
                text(s)
            }
            Func::ColorRgb => match (n(0), n(1), n(2)) {
                (Some(r), Some(g), Some(b)) => Value::Text(format!("{},{},{}", channel(r), channel(g), channel(b))),
                _ => Value::Null,
            },
            Func::ColorRgba => match (n(0), n(1), n(2), n(3)) {
                (Some(r), Some(g), Some(b), Some(a)) => Value::Text(format!("{},{},{},{}", channel(r), channel(g), channel(b), channel(a))),
                _ => Value::Null,
            },
            // Parsed into variables, so never called.
            Func::Area | Func::Perimeter | Func::LineLength => Value::Null,
        })
    }
}

/// Rows matching the clause.
pub fn select(text: &str, ds: &Dataset) -> Result<Vec<usize>, String> {
    let e = parse(text, ds)?;
    Ok((0..ds.features.len()).filter(|r| eval(&e, ds, *r)).collect())
}

#[cfg(test)]
mod tests;
