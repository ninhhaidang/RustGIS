//! QGIS expressions as RustGIS ones. The part of QGIS's expression language a filter mostly is
//! (fields compared with values, `LIKE`, `IS NULL`, `IN`, joined by `AND`, `OR` and `NOT`) is
//! written out as RustGIS SQL, as it always was; the rest (functions, arithmetic, `CASE`,
//! variables) is kept as it is for RustGIS's expression language
//! ([`rustgis_data::query`]), which reads much of QGIS's. Either is checked against a layer's
//! fields where it is used, and what doesn't read there is noted.

/// The most characters of an expression read.
const MAX_LEN: usize = 10_000;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    /// A field name: written back double-quoted.
    Field(String),
    /// A text value: written back single-quoted.
    Text(String),
    /// A number, a comparison, a keyword, a bracket or a comma: written back as it is.
    Word(String),
}

/// `expr` as a RustGIS expression: written out as a RustGIS SQL clause when it is one, else as
/// it is. `None` only when it is empty or too long to read.
pub fn to_sql(expr: &str) -> Option<String> {
    let expr = expr.trim();
    if expr.is_empty() || expr.chars().count() > MAX_LEN {
        return None;
    }
    Some(clause(expr).unwrap_or_else(|| rustgis_scales(expr)))
}

/// Whether a QGIS expression reads the map's scale (`@map_scale`, `$scale`), outside quotes.
pub(super) fn reads_scale(expr: &str) -> bool {
    rustgis_scales(expr) != expr
}

/// Whether a QGIS expression measures its geometry (`$area`, `$perimeter`, `$length`), outside
/// quotes. QGIS measures in the project's units, which the import doesn't read yet, so such an
/// expression is left out rather than read in square metres and metres.
pub fn reads_measure(expr: &str) -> bool {
    map_vars(expr, |c, name| (c == '$' && rustgis_data::Measure::from_key(name).is_some()).then(String::new)) != expr
}

/// A QGIS expression with its map scale (`@map_scale`, `$scale`, QGIS's at 96 dpi) read as
/// RustGIS's ([`super::QGIS_SCALE`] of it), so `@map_scale < 50000` picks the same zoom in both;
/// text within quotes is left as it is.
pub(super) fn rustgis_scales(expr: &str) -> String {
    map_vars(expr, |c, name| {
        ((c == '@' && name.eq_ignore_ascii_case("map_scale")) || (c == '$' && name.eq_ignore_ascii_case("scale")))
            .then(|| format!("(@map_scale / {})", super::QGIS_SCALE))
    })
}

/// `expr` with each variable outside quotes (`@name` or `$name`, its sigil and name passed to
/// `f`) replaced by what `f` gives, or kept when it gives `None`.
fn map_vars(expr: &str, f: impl Fn(char, &str) -> Option<String>) -> String {
    let chars: Vec<char> = expr.chars().collect();
    let mut out = String::with_capacity(expr.len());
    let mut quote: Option<char> = None;
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                out.push(c);
                i += 1;
            }
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                out.push(c);
                i += 1;
            }
            None if c == '@' || c == '$' => {
                let end =
                    (i + 1..=chars.len()).find(|j| !chars.get(*j).is_some_and(|d| d.is_alphanumeric() || *d == '_')).unwrap_or(chars.len());
                let name: String = chars.get(i + 1..end).unwrap_or_default().iter().collect();
                // (Right after a word too: `WHEN@map_scale` reads the scale, in QGIS and in RustGIS,
                // whose names hold neither `@` nor `$`.)
                match f(c, &name) {
                    Some(with) => out.push_str(&with),
                    None => {
                        out.push(c);
                        out.push_str(&name);
                    }
                }
                i = end;
            }
            None => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// `expr` as a RustGIS SQL clause, or `None` when it uses more of QGIS's language than that has.
fn clause(expr: &str) -> Option<String> {
    let tokens = tokenize(expr)?;
    let mut out = Vec::with_capacity(tokens.len());
    for t in &tokens {
        out.push(match t {
            Token::Field(f) => format!("\"{}\"", f.replace('"', "")),
            Token::Text(s) => format!("'{}'", s.replace('\'', "''")),
            Token::Word(w) => w.clone(),
        });
    }
    Some(out.join(" "))
}

/// The field and values of a filter that only picks values of one field: `"f" = 'v'`, `f = 3`,
/// `"f" IN ('a', 'b')`. `None` for any other.
pub(super) fn equality(expr: &str) -> Option<(String, Vec<String>)> {
    let tokens = tokenize(expr.trim())?;
    let value = |t: &Token| match t {
        Token::Text(s) => Some(s.clone()),
        Token::Word(w) if w.parse::<f64>().is_ok() => Some(w.clone()),
        _ => None,
    };
    match tokens.as_slice() {
        [Token::Field(f), Token::Word(eq), v] if eq == "=" => Some((f.clone(), vec![value(v)?])),
        [Token::Field(f), Token::Word(kw), Token::Word(open), rest @ .., Token::Word(close)]
            if kw == "IN" && open == "(" && close == ")" =>
        {
            let values: Vec<String> = rest.iter().step_by(2).map(value).collect::<Option<_>>()?;
            let commas_ok = rest.iter().skip(1).step_by(2).all(|t| *t == Token::Word(",".into()));
            (commas_ok && !values.is_empty()).then(|| (f.clone(), values))
        }
        _ => None,
    }
}

/// Whose rules a QGIS filter's text comparisons follow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Dialect {
    /// QGIS's expressions (renderer and label rules): text matches exactly, but for `ILIKE`.
    Qgis,
    /// SQLite's (a GeoPackage's or SpatiaLite's filter through GDAL, whose `LIKE` ignores the case
    /// of every letter; a virtual layer's `WHERE`, plain SQLite, whose `LIKE` ignores that of ASCII
    /// letters only); the rest matches exactly.
    // A virtual layer's `LIKE` on non-ASCII letters isn't noted (it is read as GDAL's): a dialect
    // of its own would tell the two apart.
    Sqlite,
    /// OGR SQL's (any other file's filter): `=`, `<>`, `IN`, `<`, `>` and `BETWEEN` ignore the case
    /// of ASCII letters; `LIKE` matches case exactly (GDAL 3.1 on).
    Ogr,
}

fn cased(c: char) -> bool {
    c.is_lowercase() || c.is_uppercase()
}

/// Whether `expr`, read under `d`'s rules, compares a field with text that RustGIS, ignoring
/// letter case, can match where QGIS doesn't. A field compared with a field isn't noted (types
/// aren't known at import).
pub(super) fn case_differs(expr: &str, d: Dialect) -> bool {
    let expr = expr.trim();
    if expr.chars().count() > MAX_LEN {
        return false;
    }
    let Some(tokens) = tokenize(expr) else {
        // Past a clause, a filter is read only for its texts and the operators outside them, so
        // `format_date("d", 'yyyy') = 'x'` is noted too; walking the parsed expression would not.
        // Each part between its `AND`s and `OR`s is read on its own, so a `lower(…)` in one of
        // them doesn't excuse the texts in another.
        return cut_at_joins(expr).iter().any(|part| unreadable_differs(part, d));
    };
    const OPS: [&str; 9] = ["=", "<>", "!=", "<", "<=", ">", ">=", "LIKE", "ILIKE"];
    let is_op = |t: &Token| matches!(t, Token::Word(w) if OPS.contains(&w.as_str()) || w == "IN");
    let is_join = |t: &Token| matches!(t, Token::Word(w) if w == "AND" || w == "OR");
    tokens.iter().enumerate().any(|(i, t)| {
        let Token::Text(s) = t else { return false };
        let before = tokens.get(..i).unwrap_or_default().iter().rev().take_while(|t| !is_join(t)).find(|t| is_op(t));
        let after = tokens.get(i + 1..).unwrap_or_default().iter().take_while(|t| !is_join(t)).find(|t| is_op(t));
        let Some(Token::Word(op)) = before.or(after) else { return false };
        let ascii = s.chars().any(|c| c.is_ascii_alphabetic());
        match op.as_str() {
            "=" | "<>" | "!=" | "IN" => ascii && d != Dialect::Ogr,
            // (OGR SQL orders text ignoring ASCII case: only other letters differ.)
            "<" | "<=" | ">" | ">=" if d == Dialect::Ogr => s.chars().any(|c| !c.is_ascii() && cased(c)),
            "<" | "<=" | ">" | ">=" => s.chars().any(cased),
            "LIKE" if d == Dialect::Sqlite => false,
            "LIKE" => s.chars().any(cased),
            _ => false,
        }
    })
}

/// [`case_differs`] for a filter [`tokenize`] doesn't read: its texts and the operators outside
/// them, read by hand.
fn unreadable_differs(expr: &str, d: Dialect) -> bool {
    let chars: Vec<char> = expr.chars().collect();
    let (mut i, mut texts, mut ascii, mut wide, mut outer) = (0, false, false, false, String::new());
    let (mut has_upper, mut has_lower) = (false, false);
    while let Some(&c) = chars.get(i) {
        match c {
            '"' | '\'' => match quoted(&chars, i, c) {
                Some((text, next)) => {
                    texts |= c == '\'' && text.chars().any(cased);
                    ascii |= c == '\'' && text.chars().any(|c| c.is_ascii_alphabetic());
                    wide |= c == '\'' && text.chars().any(|c| !c.is_ascii() && cased(c));
                    has_upper |= c == '\'' && text.chars().any(char::is_uppercase);
                    has_lower |= c == '\'' && text.chars().any(char::is_lowercase);
                    // The quote stays, so a call can tell a field (`lower("n")`) from a text (`lower('X')`).
                    outer.push(c);
                    i = next;
                }
                None => return false,
            },
            _ => {
                outer.push(c.to_ascii_lowercase());
                i += 1;
            }
        }
    }
    // Folding the case by hand (`lower("n") = 'x'`) is how QGIS ignores it, when the folded field is
    // all that is compared: not for a text folded (`"n" = lower('X')`), nor a concatenation, nor a
    // call or a `CASE` beside it, and only when the text is folded too (`lower("n") = 'X'` never
    // matches in QGIS).
    let words: Vec<&str> = outer.split(|c: char| !(c.is_alphanumeric() || c == '_')).collect();
    let folds_field = |f: &str| {
        outer.match_indices(f).any(|(at, _)| {
            let start = outer.get(..at).is_some_and(|b| !b.ends_with(|c: char| c.is_alphanumeric() || c == '_'));
            let arg = outer.get(at + f.len()..).and_then(|r| r.trim_start().strip_prefix('(')).map(str::trim_start);
            // A quoted field (`"n"`), or a bare one (`n`) closed by its bracket.
            let field = arg.is_some_and(|a| {
                let name = a.split(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or_default();
                let bare = name.starts_with(|c: char| c.is_alphabetic() || c == '_')
                    && !matches!(name, "null" | "true" | "false")
                    && a.get(name.len()..).is_some_and(|rest| rest.trim_start().starts_with(')'));
                a.starts_with('"') || bare
            });
            start && field
        })
    };
    let other_call = outer.match_indices('(').any(|(at, _)| {
        let head = outer.get(..at).unwrap_or_default().trim_end();
        let name = head.rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or_default();
        !matches!(name, "lower" | "upper")
    });
    let case_expr = words.iter().any(|w| matches!(*w, "case" | "when" | "then" | "else"));
    let (lower_field, upper_field) = (folds_field("lower"), folds_field("upper"));
    let folded_text = (!lower_field || !has_upper) && (!upper_field || !has_lower);
    let exempt = (lower_field || upper_field) && folded_text && !other_call && !case_expr && !outer.contains("||");
    if !texts || exempt {
        return false;
    }
    let cmp = outer.replace("<>", " ").replace("!=", " ");
    let order = cmp.contains(['<', '>']) || words.contains(&"between");
    let like = words.contains(&"like");
    let eq = (outer.contains('=') || outer.contains("<>") || words.contains(&"in")) && ascii;
    match d {
        Dialect::Qgis => order || like || eq,
        Dialect::Sqlite => order || eq,
        Dialect::Ogr => (order && wide) || like,
    }
}

/// `expr` cut at its `AND`s and `OR`s outside quotes, each part a filter of its own; an `AND` of
/// a `BETWEEN` stays with it. An unclosed quote leaves `expr` whole.
fn cut_at_joins(expr: &str) -> Vec<String> {
    let chars: Vec<char> = expr.chars().collect();
    let (mut parts, mut part, mut between, mut i) = (Vec::new(), String::new(), false, 0);
    while let Some(&c) = chars.get(i) {
        if c == '"' || c == '\'' {
            let Some((_, next)) = quoted(&chars, i, c) else { return vec![expr.to_string()] };
            part.extend(chars.get(i..next).unwrap_or_default());
            i = next;
        } else if c.is_alphanumeric() || c == '_' {
            let start = i;
            while chars.get(i).is_some_and(|d| d.is_alphanumeric() || *d == '_') {
                i += 1;
            }
            let word: String = chars.get(start..i).unwrap_or_default().iter().collect();
            match word.to_ascii_lowercase().as_str() {
                "between" => between = true,
                "and" if between => between = false,
                "and" | "or" => {
                    parts.push(std::mem::take(&mut part));
                    continue;
                }
                _ => {}
            }
            part.push_str(&word);
        } else {
            part.push(c);
            i += 1;
        }
    }
    parts.push(part);
    parts
}

/// Whether `expr` has `NOT IN` with NULL in its list: RustGIS skips the NULL, so a value in none
/// of the rest is kept, where SQL (QGIS's expressions, SQLite) takes it as unknown.
pub(super) fn null_in_not_in(expr: &str) -> bool {
    let expr = expr.trim();
    if expr.chars().count() > MAX_LEN {
        return false;
    }
    // Read by its words outside quotes, so a filter that doesn't tokenize (a function, `BETWEEN`,
    // arithmetic) is read as well.
    let chars: Vec<char> = expr.chars().collect();
    let mut outer = String::with_capacity(expr.len());
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if c == '"' || c == '\'' {
            let Some((_, next)) = quoted(&chars, i, c) else { return false };
            outer.push(' ');
            i = next;
        } else {
            outer.push(c.to_ascii_lowercase());
            i += 1;
        }
    }
    let spaced = outer.replace('(', " ( ").replace(')', " ) ").replace(',', " ");
    let words: Vec<&str> = spaced.split_whitespace().collect();
    (0..words.len()).any(|i| {
        if words.get(i) != Some(&"not") {
            return false;
        }
        // `NOT IN (`, or `NOT f IN (` with the field unquoted (a quoted one is blanked above), each
        // maybe in brackets: `NOT (p IN (1, NULL))`.
        let mut at = i + 1;
        while words.get(at) == Some(&"(") {
            at += 1;
        }
        if words.get(at) != Some(&"in") {
            at += 1;
        }
        if words.get(at) != Some(&"in") || words.get(at + 1) != Some(&"(") {
            return false;
        }
        // The list ends at the `)` that closes its own `(`: a call inside it has one of its own.
        let mut depth = 0usize;
        let mut list = words.get(at + 2..).unwrap_or_default().iter().take_while(|w| {
            match **w {
                "(" => depth += 1,
                ")" if depth == 0 => return false,
                ")" => depth -= 1,
                _ => {}
            }
            true
        });
        list.any(|w| *w == "null")
    })
}

/// Words of the expression language beyond a clause's: a bare one is never a field.
const EXPRESSION_WORDS: [&str; 6] = ["CASE", "WHEN", "THEN", "ELSE", "END", "BETWEEN"];

/// The words RustGIS's SQL has.
fn keyword(w: &str) -> Option<&'static str> {
    Some(match w.to_ascii_uppercase().as_str() {
        "AND" => "AND",
        "OR" => "OR",
        "NOT" => "NOT",
        "IN" => "IN",
        "LIKE" => "LIKE",
        "ILIKE" => "ILIKE",
        "IS" => "IS",
        "NULL" => "NULL",
        _ => return None,
    })
}

fn tokenize(s: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        match c {
            c if c.is_whitespace() => i += 1,
            '"' => {
                let (text, next) = quoted(&chars, i, '"')?;
                out.push(Token::Field(text));
                i = next;
            }
            '\'' => {
                let (text, next) = quoted(&chars, i, '\'')?;
                out.push(Token::Text(text));
                i = next;
            }
            '(' | ')' | ',' => {
                out.push(Token::Word(c.to_string()));
                i += 1;
            }
            '=' => {
                out.push(Token::Word("=".into()));
                i += 1;
            }
            '<' | '>' | '!' => {
                let two: String = chars.get(i..(i + 2).min(chars.len())).unwrap_or_default().iter().collect();
                let op = match two.as_str() {
                    "<=" | ">=" | "<>" | "!=" => two,
                    _ if c != '!' => c.to_string(),
                    _ => return None,
                };
                i += op.chars().count();
                out.push(Token::Word(op));
            }
            c if c.is_ascii_digit() || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) => {
                let start = i;
                i += 1;
                while chars.get(i).is_some_and(|d| d.is_ascii_digit() || *d == '.') {
                    i += 1;
                }
                // An exponent: 1e3, 2.5E-4.
                if chars.get(i).is_some_and(|e| *e == 'e' || *e == 'E') {
                    let sign = usize::from(chars.get(i + 1).is_some_and(|s| *s == '-' || *s == '+'));
                    if chars.get(i + 1 + sign).is_some_and(char::is_ascii_digit) {
                        i += 1 + sign;
                        while chars.get(i).is_some_and(char::is_ascii_digit) {
                            i += 1;
                        }
                    }
                }
                let n: String = chars.get(start..i).unwrap_or_default().iter().collect();
                let v = n.parse::<f64>().ok()?;
                // An exponent is written out (0.001 for 1e-3): RustGIS SQL reads plain decimals.
                out.push(Token::Word(if n.contains(['e', 'E']) && v.is_finite() { format!("{v}") } else { n }));
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while chars.get(i).is_some_and(|d| d.is_alphanumeric() || *d == '_') {
                    i += 1;
                }
                let w: String = chars.get(start..i).unwrap_or_default().iter().collect();
                // A word followed by a bracket is a function call.
                let next = chars.get(i..).unwrap_or_default().iter().find(|c| !c.is_whitespace());
                match keyword(&w) {
                    // TRUE and FALSE are values, bare as RustGIS SQL has them, not fields.
                    _ if w.eq_ignore_ascii_case("true") || w.eq_ignore_ascii_case("false") => out.push(Token::Word(w.to_lowercase())),
                    Some(k) => out.push(Token::Word(k.into())),
                    // The expression language's own words: more than a clause, kept as written.
                    None if EXPRESSION_WORDS.iter().any(|k| w.eq_ignore_ascii_case(k)) => return None,
                    None if next == Some(&'(') => return None,
                    None => out.push(Token::Field(w)),
                }
            }
            // Arithmetic, concatenation, variables (@…) and the rest.
            _ => return None,
        }
        if out.len() > 2000 {
            return None;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The text between the quote at `start` and its closing one (a doubled quote stands for
/// itself), and where reading goes on.
fn quoted(chars: &[char], start: usize, q: char) -> Option<(String, usize)> {
    let mut text = String::new();
    let mut i = start + 1;
    loop {
        match chars.get(i) {
            None => return None,
            Some(&c) if c == q && chars.get(i + 1) == Some(&q) => {
                text.push(q);
                i += 2;
            }
            Some(&c) if c == q => return Some((text, i + 1)),
            Some(&c) => {
                text.push(c);
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Dialect, MAX_LEN, case_differs, null_in_not_in, to_sql};

    #[test]
    fn case_differs_by_dialect() {
        use Dialect::{Ogr, Qgis, Sqlite};
        for (e, qgis, sqlite, ogr) in [
            ("\"k\" = 'x'", true, true, false),
            ("'x' = \"k\"", true, true, false),
            ("\"k\" IN ('a', 'B')", true, true, false),
            ("\"k\" > 'M'", true, true, false),
            ("\"k\" > 'É'", true, true, true),
            ("\"k\" LIKE 'x%'", true, false, true),
            ("\"k\" LIKE 'é%'", true, false, true),
            ("\"k\" ILIKE 'x%'", false, false, false),
            ("\"k\" = '12' AND \"p\" > 5", false, false, false),
            ("\"k\" = 'é'", false, false, false),
            ("\"p\" > 5 AND \"k\" = 'x'", true, true, false),
            ("format_date(\"d\", 'yyyy') = 'x'", true, true, false),
            ("\"a\" + 1 = 'x'", true, true, false),
            ("\"a\" + 1 LIKE 'x'", true, false, true),
            ("\"a\" + 1 <> 'x'", true, true, false),
            ("(\"n\" || '') != 'x'", true, true, false),
            ("\"k\" BETWEEN 'A' AND 'M'", true, true, false),
            ("\"k\" NOT BETWEEN 'a' AND 'm'", true, true, false),
            ("\"p\" BETWEEN 1 AND 5", false, false, false),
            ("left(\"n\", 1) = 'É'", false, false, false),
            ("left(\"n\", 1) > 'É'", true, true, true),
            ("lower(\"n\") = 'x'", false, false, false),
            // A folded field is exempt only when the text is folded too: `lower("n") = 'X'` never matches in QGIS.
            ("lower(\"kind\") = 'x'", false, false, false),
            ("upper(\"kind\") = 'X'", false, false, false),
            ("lower(\"kind\") = 'X'", true, true, false),
            ("upper(\"kind\") = 'x'", true, true, false),
            ("lower(\"kind\") LIKE 'X%'", true, false, true),
            // A bare field in the fold is the same field.
            ("lower(kind) = 'x'", false, false, false),
            ("lower(kind) = 'X'", true, true, false),
            // A call or a CASE beside the fold doesn't excuse the other texts.
            ("if(lower(\"n\") = 'x', \"kind\" = 'Y', FALSE)", true, true, false),
            ("if(lower(\"n\") = 'x', \"kind\" = 'y', FALSE)", true, true, false),
            ("CASE WHEN lower(\"n\") = 'x' THEN \"kind\" = 'y' END", true, true, false),
            // A folded text, or a folded part of a concatenation, doesn't fold the whole compared text.
            ("\"k\" = lower('X')", true, true, false),
            ("lower(\"prefix\") || \"kind\" = 'xy'", true, true, false),
            ("flower(\"n\") = 'x'", true, true, false),
            ("upper(\"n\") LIKE '%PARK%'", false, false, false),
            // A folded field in one part doesn't excuse the texts of the others.
            ("\"k\" = 'x' AND lower(\"n\") = 'x'", true, true, false),
            ("lower(\"n\") = 'x' OR \"k\" = 'x'", true, true, false),
            ("\"k\" BETWEEN 'A' AND 'M' AND upper(\"n\") = 'X'", true, true, false),
            ("upper(\"n\") = 'X' OR \"k\" LIKE 'x%'", true, false, true),
            ("format_date(\"d\", 'yyyy')", false, false, false),
            ("\"a\" + 1 > 2", false, false, false),
            ("@map_scale < 5000", false, false, false),
            ("\"X\" || '1' = '1'", false, false, false),
        ] {
            assert_eq!([case_differs(e, Qgis), case_differs(e, Sqlite), case_differs(e, Ogr)], [qgis, sqlite, ogr], "{e}");
        }
        assert!(!case_differs(&format!("k = 'x' AND {}", "a".repeat(MAX_LEN)), Qgis));
        assert!(null_in_not_in("\"p\" NOT IN (1, NULL)") && null_in_not_in("p not in (null)"));
        // A filter that doesn't tokenize (a function, arithmetic, `BETWEEN`) is read the same way.
        assert!(null_in_not_in("\"a\" + 1 > 2 AND \"p\" NOT IN (1, NULL)"));
        assert!(null_in_not_in("upper(\"n\") = 'x' OR p NOT IN ('a', NULL)"));
        // A call in the list has its own brackets: the NULL after it is still in the list, and an
        // unquoted field after NOT is the same predicate.
        assert!(null_in_not_in("\"p\" NOT IN (length('ab'), NULL)") && null_in_not_in("p NOT IN (abs(1), NULL)"));
        assert!(null_in_not_in("NOT p IN (1, NULL)") && null_in_not_in("NOT \"p\" IN (1, NULL)"));
        assert!(!null_in_not_in("\"p\" NOT IN (abs(1), 2)") && !null_in_not_in("NOT p IN (1, 2)"));
        // A NOT over a bracketed IN is the same predicate, NULL in its list or not.
        assert!(null_in_not_in("NOT (p IN (1, NULL))") && null_in_not_in("NOT (\"p\" IN (1, NULL))"));
        assert!(!null_in_not_in("NOT (p IN (1, 2))"));
        assert!(
            !null_in_not_in("\"p\" NOT IN (1, 2)") && !null_in_not_in("\"p\" IN (1, NULL)") && !null_in_not_in("p IN (1) AND q IS NULL")
        );
        // Text 'NULL', a field named NULL, and a NULL outside the list don't count.
        assert!(
            !null_in_not_in("\"p\" NOT IN ('NULL', 2)")
                && !null_in_not_in("\"NULL\" NOT IN (1, 2)")
                && !null_in_not_in("p NOT IN (1) OR q IS NULL")
                && !null_in_not_in("a + 1 > 2 AND p NOT IN (1, 2)")
        );
    }

    #[test]
    fn filters_carry_over_and_the_rest_is_refused() {
        assert_eq!(to_sql(r#""Symbol" = 'Ortsteil_20-49'"#).as_deref(), Some(r#""Symbol" = 'Ortsteil_20-49'"#));
        assert_eq!(
            to_sql(r#"pop >= 1e3 AND ("name" ILIKE 'z%' OR kind IN ('a', 'b''s')) AND note IS NOT NULL"#).as_deref(),
            Some(r#""pop" >= 1000 AND ( "name" ILIKE 'z%' OR "kind" IN ( 'a' , 'b''s' ) ) AND "note" IS NOT NULL"#)
        );
        assert_eq!(to_sql("a <> -2.5").as_deref(), Some(r#""a" <> -2.5"#));
        // Exponents written out, and TRUE and FALSE as values, as RustGIS SQL reads them.
        assert_eq!(to_sql("d < 2.5E-4 OR d > -1e+2").as_deref(), Some(r#""d" < 0.00025 OR "d" > -100"#));
        assert_eq!(to_sql("flag = TRUE AND gone = false").as_deref(), Some(r#""flag" = true AND "gone" = false"#));
        // The rest is kept as it is, for the expression language to read where it is used.
        for kept in [
            "upper(\"name\") = 'X'",
            "\"a\" + 1 > 2",
            "\"a\" || 'x' = 'y'",
            "@row_number < 5000",
            "'unclosed",
            "a ! b",
            // The expression language's words aren't fields.
            "CASE WHEN \"a\" > 1 THEN 'x' ELSE 'y' END",
            "case when a > 1 then 'x' end",
            "a BETWEEN 1 AND 2",
        ] {
            assert_eq!(to_sql(&format!("  {kept} ")).as_deref(), Some(kept), "{kept}");
        }
        assert_eq!(to_sql("  "), None);
        // QGIS's map scale read as RustGIS's, outside quotes only.
        let k = super::super::QGIS_SCALE;
        assert_eq!(to_sql("@map_scale < 5000").as_deref(), Some(format!("(@map_scale / {k}) < 5000").as_str()));
        assert_eq!(
            super::rustgis_scales("$scale > 1 AND \"@map_scale\" = '$scale' AND @map_scales = 2 AND a$scale"),
            format!("(@map_scale / {k}) > 1 AND \"@map_scale\" = '$scale' AND @map_scales = 2 AND a(@map_scale / {k})")
        );
        assert_eq!(to_sql(&"a".repeat(MAX_LEN + 1)), None);
        // A scale right after a word is read and converted too, as the expression reads it.
        assert_eq!(
            to_sql("CASE WHEN@map_scale < 5 THEN 'a' END").as_deref(),
            Some(format!("CASE WHEN(@map_scale / {k}) < 5 THEN 'a' END").as_str())
        );
        assert!(super::reads_scale("CASE WHEN@map_scale < 5 THEN 'a' END") && super::reads_scale("a$scale"));
        assert!(!super::reads_scale("\"@map_scale\" = '$scale' AND @map_scales = 2"));
    }
}
