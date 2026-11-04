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

/// Words of the expression language beyond a clause's: a bare one is never a field.
const EXPRESSION_WORDS: [&str; 6] = ["CASE", "WHEN", "THEN", "ELSE", "END", "BETWEEN"];

/// The words RustGIS's SQL has (`ILIKE` is its `LIKE`, which ignores case).
fn keyword(w: &str) -> Option<&'static str> {
    Some(match w.to_ascii_uppercase().as_str() {
        "AND" => "AND",
        "OR" => "OR",
        "NOT" => "NOT",
        "IN" => "IN",
        "LIKE" | "ILIKE" => "LIKE",
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
    use super::{MAX_LEN, to_sql};

    #[test]
    fn filters_carry_over_and_the_rest_is_refused() {
        assert_eq!(to_sql(r#""Symbol" = 'Ortsteil_20-49'"#).as_deref(), Some(r#""Symbol" = 'Ortsteil_20-49'"#));
        assert_eq!(
            to_sql(r#"pop >= 1e3 AND ("name" ILIKE 'z%' OR kind IN ('a', 'b''s')) AND note IS NOT NULL"#).as_deref(),
            Some(r#""pop" >= 1000 AND ( "name" LIKE 'z%' OR "kind" IN ( 'a' , 'b''s' ) ) AND "note" IS NOT NULL"#)
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
