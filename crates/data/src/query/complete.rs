//! What can be typed at a point of an expression being written (#1240): the word being typed,
//! and what the language accepts in its place. It reads the text with the real lexer and parser,
//! which note what they look for where the text ends ([`super::Probe`]), so there is no second
//! grammar here to fall behind: whatever the parser learns to read is offered too.

use std::cell::{OnceCell, RefCell};
use std::ops::Range;

use super::{Arith, FuncSpec, Kw, Lexed, Open, Parser, Probe, Tok, lex_first, lex_spans};
use crate::value::ClassText;
use crate::{Dataset, Value};

/// Where the cursor is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// Among the expression's words: fields, functions, operators…
    Code,
    /// Inside `'text'`: no word of the language goes there (a field's values may), so nothing
    /// is said to go there; the call and the compared field still are.
    Text,
    /// Inside a quoted field name (`"Pop Est`, `!Pop Est`): a field goes there.
    Field,
}

/// What may be typed at a point of an expression, from [`complete_at`].
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    /// The characters the word being typed takes (empty at the cursor when none is): what an
    /// accepted item replaces. In [`Place::Text`] and [`Place::Field`] it starts at the quote.
    pub word: Range<usize>,
    /// What the word holds so far, without its quote and with a doubled quote read as one
    /// (`'O''Br` holds `O'Br`): what items are matched against.
    pub prefix: String,
    pub place: Place,
    /// A value can go here: a field, a function, a variable, a literal, `CASE`.
    pub operand: bool,
    /// The keywords that can go here, in [`Kw::ALL`] order. With an operand they include the
    /// ones that are values (`NULL`, `TRUE`, `FALSE`) and `CASE`.
    pub keywords: Vec<Kw>,
    /// The operators and punctuation that can go here (`=`, `+`, `||`, `)`, `,`…), each once
    /// and in one spelling (`<>`, not `!=`; `=`, not `==`). Where a value goes, the `(` or the
    /// sign (`-1`) that can start one isn't listed: the value is (`operand`).
    pub symbols: Vec<&'static str>,
    /// The call whose arguments the cursor is in, the innermost; `None` outside a call or in a
    /// call of a name no function has.
    pub call: Option<Call>,
    /// The field (its index in the dataset) a value typed here is compared with: `name = ▮`,
    /// `name LIKE '▮`, `name IN ('a', ▮`, `pop BETWEEN ▮`.
    pub field: Option<usize>,
    /// Whether the value typed here is a `LIKE` / `ILIKE` pattern, where `%` and `_` match more
    /// than themselves (`name LIKE '▮`).
    pub pattern: bool,
    /// Whether a value typed here is the right-hand side of a comparison, `LIKE`, `IN` or
    /// `BETWEEN`, whatever its left side is: a field ([`field`](Self::field) says which) or
    /// something computed (`upper(name) = ▮`), which has no field to take values from.
    pub compared: bool,
}

/// A call the cursor is in, from [`complete_at`]: its function, which argument (from 0), and
/// where its `(` is (a char index), what help about it is anchored at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Call {
    pub spec: FuncSpec,
    pub arg: usize,
    pub open: usize,
}

/// The order symbols are given in.
const SYMBOLS: [&str; 16] = ["=", "<>", "<", "<=", ">", ">=", "+", "-", "*", "/", "%", "^", "||", "(", ")", ","];

/// What may be typed at `cursor` (in characters, as a text box counts them; past the end is the
/// end) of `text`, an expression over `ds`'s fields. Only the text before the cursor is read, and
/// it may be unfinished: an open quote, bracket or `CASE`, a field no layer has. `None` when
/// that text can't be read into words at all, wherever the fault is: a character the language
/// has no use for (`#`), a number written wrong (`1.2.3`), an `@` or `$` with no name before
/// more text, or longer than [`super::MAX_LEN`]. Nothing is offered then until it is put right,
/// as the error the box shows for it says. The same when the text before the cursor stops being
/// an expression before it ends (`pop > 1 ) AND`, `pop BETWEEN 1 OR`): nothing can follow it.
pub fn complete_at(text: &str, cursor: usize, ds: &Dataset) -> Option<Completion> {
    let before: String = text.chars().take(cursor).collect();
    let cursor = before.chars().count();
    let Lexed { mut toks, spans, open } = lex_spans(&before, true).ok()?;
    let (place, word) = match open {
        Some((Open::Text, at)) => (Place::Text, at..cursor),
        Some((Open::Field(_), at)) => (Place::Field, at..cursor),
        // A name or variable that ends at the cursor is still being typed.
        None => match (toks.last(), spans.last()) {
            (Some(Tok::Ident(_) | Tok::Var(_)), Some(span)) if span.end == cursor => {
                let word = span.clone();
                toks.pop();
                (Place::Code, word)
            }
            _ => (Place::Code, cursor..cursor),
        },
    };
    let quote = usize::from(place != Place::Code);
    let prefix: String = before.chars().skip(word.start + quote).collect();
    // The quote doubled is the quote itself, as the lexer reads it (`!` has no such escape).
    let prefix = match open {
        Some((Open::Text, _)) => prefix.replace("''", "'"),
        Some((Open::Field('"'), _)) => prefix.replace("\"\"", "\""),
        _ => prefix,
    };
    let mut parser = Parser {
        toks,
        pos: 0,
        ds,
        depth: 0,
        area_check: OnceCell::new(),
        validate_areas: false,
        probe: Some(RefCell::new(Probe::default())),
        calls: Vec::new(),
    };
    // Read for what it notes on the way: an error at the end (nothing more to read) is expected.
    // A reading that never got to the end stopped at a mistake before it.
    let _ = parser.or();
    let probe = parser.probe.take()?.into_inner();
    if !probe.reached {
        return None;
    }
    let mut keywords = probe.keywords;
    if probe.operand {
        keywords.extend([Kw::Null, Kw::True, Kw::False, Kw::Case]);
    }
    let keywords = Kw::ALL.iter().copied().filter(|k| keywords.contains(k)).collect();
    let symbols = SYMBOLS.into_iter().filter(|s| probe.symbols.contains(s)).collect();
    let call = probe.call.and_then(|(f, arg, tok)| Some(Call { spec: f?.spec(), arg, open: spans.get(tok)?.start }));
    // Inside text only its call and the field it is compared with mean anything; inside a
    // field's quotes, a field name: no keyword nor symbol goes in a name.
    let code = place == Place::Code;
    Some(Completion {
        word,
        prefix,
        place,
        operand: probe.operand && place != Place::Text,
        keywords: if code { keywords } else { Vec::new() },
        symbols: if code { symbols } else { Vec::new() },
        call,
        field: probe.field,
        pattern: probe.pattern,
        compared: probe.compared,
    })
}

/// The brackets `text`, a part of an expression, leaves open, and those it closes that it didn't
/// open, read as the lexer reads it: none in a text or a quoted name (`')'`, `"a)"`, `!a)!`).
/// `None` when it can't be read into words (as [`complete_at`]); a quote left open at the end
/// holds the rest.
pub fn brackets(text: &str) -> Option<(usize, usize)> {
    let (mut open, mut unopened) = (0usize, 0usize);
    for t in lex_spans(text, true).ok()?.toks {
        match t {
            Tok::LParen => open += 1,
            Tok::RParen => match open.checked_sub(1) {
                Some(n) => open = n,
                None => unopened += 1,
            },
            _ => {}
        }
    }
    Some((open, unopened))
}

/// What `text`, what follows a cursor where a value goes, starts with, as [`follows`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Follows {
    /// A value, which a value written there would stand beside (`= ▮'Africa'`, `▮!name!`): a
    /// text or quoted name (finished or not), a number, a sign (a value before `-5` would
    /// subtract it), a bracket, a variable, a `CASE`, or a name that isn't a word coming after a
    /// value.
    Value,
    /// A comparison, which takes the value before it as its left side: a comparison operator
    /// (`▮= 'Old'`, `▮!= 'Old'`) or `IS`, `NOT`, `LIKE`, `ILIKE`, `IN`, `BETWEEN`.
    Comparison,
    /// An operator that makes a value of the one before it (`▮* 2`, `▮|| 'x'`).
    Operator,
    /// Anything else: nothing, a closing bracket, a comma, `AND`, `OR`, `WHEN`, `THEN`, `ELSE`,
    /// `END`, or what can't be read into words.
    Other,
}

/// What `text`, what follows a cursor where a value goes, starts with: its first word read as
/// the lexer reads it, so `!=` is an operator and `!a!` a name. Only that word is read, so what
/// comes after it needn't read (`'Africa'#`).
pub fn follows(text: &str) -> Follows {
    let Some(Lexed { toks, open, .. }) = lex_first(text, true, 1).ok() else { return Follows::Other };
    let left_side = [Kw::Is, Kw::Not, Kw::Like, Kw::Ilike, Kw::In, Kw::Between];
    let joins = [Kw::And, Kw::Or, Kw::When, Kw::Then, Kw::Else, Kw::End];
    let is = |word: &str, kws: &[Kw]| kws.iter().any(|k| k.word().eq_ignore_ascii_case(word));
    match toks.into_iter().next() {
        Some(Tok::Str(_) | Tok::QuotedIdent(_) | Tok::Num(_) | Tok::Var(_) | Tok::LParen | Tok::Arith(Arith::Add | Arith::Sub)) => {
            Follows::Value
        }
        Some(Tok::Op(_)) => Follows::Comparison,
        Some(Tok::Concat | Tok::Arith(_)) => Follows::Operator,
        Some(Tok::Ident(word)) if is(&word, &left_side) => Follows::Comparison,
        Some(Tok::Ident(word)) if is(&word, &joins) => Follows::Other,
        Some(Tok::Ident(_)) => Follows::Value,
        Some(Tok::RParen | Tok::Comma) => Follows::Other,
        // A text or name begun and never closed holds the rest.
        None if open.is_some() => Follows::Value,
        None => Follows::Other,
    }
}

/// The most features [`field_values`] reads of a layer. Where a value compared with a field is
/// typed, its values are read again as the text changes, on the thread that draws, so a layer of
/// millions mustn't be read whole each time (nor kept read: an edit, a reload or a view read would
/// have to end that). An evenly spread sample of this many finds the values that matter; a value
/// only a few features hold may be missed, and the list says when it read a sample.
pub const VALUES_READ: usize = 10_000;

/// The longest text [`field_values`] gives, in chars. A longer one is a description, not a
/// value anyone compares with by typing it, and a list as wide as its widest item would run off
/// the screen with it.
pub const VALUE_CHARS: usize = 100;

/// A value of a field, from [`field_values`]: as an expression writes it ([`literal`]: `'O''Br'`,
/// `3.25`), and as it reads (`O'Br`), what is typed inside its quotes.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldValue {
    pub written: String,
    pub text: String,
}

/// The distinct values of a field that [`field_values`] read, and of how many features.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldValues {
    /// Each value once, by what is written (`3` and `3.0` are one), in Unique Values' order (by
    /// number when they all read as numbers). No null, true or false (`IS NULL`, `TRUE` and
    /// `FALSE` are the language's own words), no number that isn't finite or is written longer
    /// than [`VALUE_CHARS`] (`1e300` in full), no text that breaks a line or is longer than it.
    pub values: Vec<FieldValue>,
    /// How many features were read.
    pub read: usize,
    /// How many features the dataset holds: more than `read` when a sample was read.
    pub of: usize,
    /// How many of the features read have a value left out of `values`: a text or float written
    /// longer than [`VALUE_CHARS`], a text that breaks a line, an infinite float or a whole number
    /// too large to write exactly ([`literal`]), for a list of `values` to say it isn't all of
    /// them. Null and NaN are no value, and aren't counted.
    pub left_out: usize,
}

/// The distinct values field `field` of `ds` holds, read from at most `most` of its features,
/// evenly spread: what can be typed as a value compared with that field (`name = '▮`). A
/// streamed layer's dataset holds the features read for the view only (`Dataset::windowed`), so
/// those are its values here.
pub fn field_values(ds: &Dataset, field: usize, most: usize) -> FieldValues {
    let of = ds.features.len();
    let step = usize::try_from(crate::source::stride(of as u64, most.max(1))).unwrap_or(usize::MAX);
    let mut seen = std::collections::HashSet::new();
    let mut values = Vec::new();
    let (mut read, mut left_out) = (0, 0);
    for f in ds.features.iter().step_by(step) {
        read += 1;
        let Some(v) = f.attrs.get(field) else { continue };
        if matches!(v, Value::Text(t) if t.chars().nth(VALUE_CHARS).is_some() || t.chars().any(char::is_control)) {
            left_out += 1;
            continue;
        }
        let written = literal(v);
        let unlisted = match v {
            // A float is written in full (`1e300` as 301 digits): too long to list, as a text is.
            Value::Float(x) => x.is_infinite() || written.as_ref().is_some_and(|w| w.len() > VALUE_CHARS),
            // 9e15 or more, which the lexer would read as a float.
            Value::Int(_) => written.is_none(),
            _ => false,
        };
        if unlisted {
            left_out += 1;
            continue;
        }
        if let Some(written) = written
            && !seen.contains(&written)
        {
            seen.insert(written.clone());
            values.push((ClassText::new(v.to_string()), written));
        }
    }
    let by_number = ClassText::numeric(values.iter().map(|(t, _)| t));
    values.sort_by(|a, b| if by_number { a.0.cmp(&b.0) } else { a.0.text().cmp(b.0.text()) });
    let values = values.into_iter().map(|(text, written)| FieldValue { written, text: text.into_text() }).collect();
    FieldValues { values, read, of, left_out }
}

/// `v` as an expression writes it, to compare a field with: a number as it is (in full, `3.25`,
/// not rounded as a table shows it), a text, date or date-time in single quotes (a `'` in it
/// doubled, as the lexer reads it), true or false as `TRUE` or `FALSE`. None for what is no
/// literal: null, a number that isn't finite, and a whole number of 9e15 or more, which the lexer reads as a
/// float (`query::num`), so that it would pick no feature, or its neighbour's.
pub fn literal(v: &Value) -> Option<String> {
    match v {
        Value::Int(i) if i.unsigned_abs() < 9_000_000_000_000_000 => Some(i.to_string()),
        // `-0` is `0`, the one value.
        Value::Float(x) if x.is_finite() => Some(if *x == 0.0 { 0.0 } else { *x }.to_string()),
        Value::Text(t) => Some(format!("'{}'", t.replace('\'', "''"))),
        Value::Date(_) | Value::DateTime(_) => Some(format!("'{v}'")),
        Value::Bool(b) => Some(if *b { "TRUE" } else { "FALSE" }.to_string()),
        Value::Int(_) | Value::Float(_) | Value::Null => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Func;
    use crate::{Field, FieldType, GeometryType};

    fn ds() -> Dataset {
        Dataset::new(
            "t",
            GeometryType::Polygon,
            vec![Field::new("name", FieldType::Text), Field::new("pop", FieldType::Integer), Field::new("Pop Est", FieldType::Integer)],
        )
    }

    /// Each value is written as the language reads it back: the expression comparing the field
    /// with what [`literal`] wrote picks the feature that holds it, and no other (a text compares
    /// letter case aside, so these differ in more than case).
    #[test]
    fn a_value_written_picks_its_feature() {
        use crate::{Date, DateTime};
        let fields = vec![
            Field::new("t", FieldType::Text),
            Field::new("i", FieldType::Integer),
            Field::new("r", FieldType::Double),
            Field::new("d", FieldType::Date),
            Field::new("s", FieldType::DateTime),
            Field::new("b", FieldType::Boolean),
        ];
        let mut d = Dataset::new("t", GeometryType::Point, fields);
        // Each boolean is one feature's, so `b = …` picks only that one: the other rows' are null.
        let rows = [
            ("O'Brien", -5, 3.25, "2026-03-05", "2026-03-05T08:00:00Z", Value::Bool(true)),
            ("Pop Est", 7, 0.1 + 0.2, "1999-12-31", "2026-03-05T08:00:00+02:00", Value::Bool(false)),
            ("", 0, 1e-7, "2000-02-29", "2026-03-05T08:00:00.250", Value::Null),
            ("x", 8_999_999_999_999_999, 2.5, "2000-03-01", "2026-03-05T09:00:00Z", Value::Null),
            ("y", 8_999_999_999_999_998, 3.5, "2000-03-02", "2026-03-05T10:00:00Z", Value::Null),
        ];
        for (t, i, r, day, at, b) in rows {
            let attrs = vec![
                Value::Text(t.into()),
                Value::Int(i),
                Value::Float(r),
                Value::Date(Date::parse(day).unwrap()),
                Value::DateTime(DateTime::parse(at).unwrap()),
                b,
            ];
            d.push(None, attrs);
        }
        for (row, f) in d.features.clone().iter().enumerate() {
            for (i, v) in f.attrs.iter().enumerate().filter(|(_, v)| **v != Value::Null) {
                let written = literal(v).unwrap();
                let name = &d.fields[i].name;
                assert_eq!(crate::query::select(&format!("{name} = {written}"), &d), Ok(vec![row]), "{name} = {written}");
            }
        }
        assert_eq!(literal(&Value::Text("O'Brien".into())).as_deref(), Some("'O''Brien'"));
        assert_eq!(literal(&Value::Float(3.0)).as_deref(), Some("3"));
        assert_eq!(literal(&Value::Float(-0.0)).as_deref(), Some("0"));
        assert_eq!(literal(&Value::Int(8_999_999_999_999_999)).as_deref(), Some("8999999999999999"));
        assert_eq!(literal(&Value::Int(-8_999_999_999_999_999)).as_deref(), Some("-8999999999999999"));
        assert_eq!(literal(&Value::Bool(false)).as_deref(), Some("FALSE"));
        for v in [
            Value::Null,
            Value::Float(f64::NAN),
            Value::Float(f64::INFINITY),
            Value::Int(9_000_000_000_000_000),
            Value::Int(9_007_199_254_740_993),
            Value::Int(i64::MIN),
        ] {
            assert_eq!(literal(&v), None, "{v:?}");
        }
    }

    /// What follows a cursor is read as the lexer reads it: `!=` is an operator, `!a!` a name, a
    /// text never closed a value, and a bracket in a text or quoted name is no bracket.
    #[test]
    fn what_follows_is_read_as_the_lexer_reads_it() {
        for (text, then) in [
            ("'a'", Follows::Value),
            (" !a b!", Follows::Value),
            ("\"a\"", Follows::Value),
            ("'Afr", Follows::Value),
            ("!Af", Follows::Value),
            ("-5", Follows::Value),
            ("+5", Follows::Value),
            ("(1)", Follows::Value),
            ("@row_number", Follows::Value),
            ("name", Follows::Value),
            ("upper(name)", Follows::Value),
            ("CASE WHEN 1 THEN 2 END", Follows::Value),
            ("'Africa' AND pop > #", Follows::Value),
            ("'Africa'#", Follows::Value),
            ("!=#", Follows::Comparison),
            (" != 1", Follows::Comparison),
            ("<> 1", Follows::Comparison),
            ("= 'Old'", Follows::Comparison),
            ("IN ('a')", Follows::Comparison),
            ("not like 'a%'", Follows::Comparison),
            ("|| 'x'", Follows::Operator),
            ("* 2", Follows::Operator),
            ("", Follows::Other),
            (") AND", Follows::Other),
            (", 'b')", Follows::Other),
            ("AND pop > 1", Follows::Other),
            ("then 1", Follows::Other),
            ("#", Follows::Other),
        ] {
            assert_eq!(follows(text), then, "{text}");
        }
        assert_eq!(brackets("((a = ')' AND \"b)\" = !c)! "), Some((2, 0)));
        assert_eq!(brackets(" AND b = 1)) "), Some((0, 2)));
        assert_eq!(brackets("(a = 'open ("), Some((1, 0)));
        assert_eq!(brackets("#"), None);
    }

    /// A field's values: each once, in Unique Values' order, without what is no literal (null, a
    /// text that breaks a line or too long to list); of a large layer, an evenly spread sample.
    #[test]
    fn a_fields_values_are_read_once_each_from_a_bounded_sample() {
        let fields = vec![Field::new("t", FieldType::Text), Field::new("n", FieldType::Double), Field::new("b", FieldType::Boolean)];
        let mut d = Dataset::new("t", GeometryType::Point, fields);
        let too_long = "x".repeat(VALUE_CHARS + 1);
        for (t, n) in [
            ("b", Value::Int(10)),
            ("A", Value::Float(9.0)),
            ("b", Value::Float(10.0)),
            ("two\nlines", Value::Null),
            ("A", Value::Float(f64::NAN)),
            ("A", Value::Float(1e300)),
            ("A", Value::Float(-1e-300)),
            (too_long.as_str(), Value::Null),
        ] {
            d.push(None, vec![Value::Text(t.into()), n, Value::Bool(true)]);
        }
        d.push(None, vec![Value::Text("x".repeat(VALUE_CHARS)), Value::Float(-1.5)]);
        let written = |field| field_values(&d, field, VALUES_READ).values.into_iter().map(|v| v.written).collect::<Vec<_>>();
        let v = field_values(&d, 0, VALUES_READ);
        assert_eq!((v.read, v.of), (9, 9));
        // The text of two lines and the one too long are counted as left out; null and NaN are no
        // value to list, not one left out.
        assert_eq!(v.left_out, 2);
        assert_eq!(field_values(&d, 1, VALUES_READ).left_out, 2, "1e300 and -1e-300 in full");
        // A number too large to write exactly, and an infinite one, are left out and counted.
        let mut big = Dataset::new("t", GeometryType::Point, vec![Field::new("n", FieldType::Double)]);
        for n in [Value::Int(9_000_000_000_000_000), Value::Float(f64::INFINITY), Value::Float(f64::NEG_INFINITY), Value::Int(7)] {
            big.push(None, vec![n]);
        }
        let got = field_values(&big, 0, VALUES_READ);
        assert_eq!((got.left_out, got.values.len()), (3, 1));
        let longest = "x".repeat(VALUE_CHARS);
        assert_eq!(written(0), ["'A'".to_string(), "'b'".to_string(), format!("'{longest}'")], "up to the longest text listed");
        assert_eq!(v.values.first(), Some(&FieldValue { written: "'A'".into(), text: "A".into() }));
        assert_eq!(written(1), ["-1.5", "9", "10"], "by number, `10` and `10.0` once, none written too long");
        assert_eq!(written(2), ["TRUE"], "as the keyword, once");
        assert_eq!(field_values(&d, 2, VALUES_READ).left_out, 0);
        assert!(field_values(&d, 9, VALUES_READ).values.is_empty(), "no such field");
        // Past the most read: every other feature, from the first.
        let mut big = Dataset::new("t", GeometryType::Point, vec![Field::new("i", FieldType::Integer)]);
        for i in 0..10 {
            big.push(None, vec![Value::Int(i)]);
        }
        let v = field_values(&big, 0, 5);
        assert_eq!((v.read, v.of), (5, 10));
        assert_eq!(v.values.iter().map(|v| v.written.as_str()).collect::<Vec<_>>(), ["0", "2", "4", "6", "8"]);
        let v = field_values(&big, 0, 4);
        assert_eq!((v.read, v.of, v.values.len()), (4, 10, 4), "no more than the most read");
        assert_eq!(field_values(&big, 0, 0).read, 1, "at least one");
        let empty = field_values(&Dataset::new("t", GeometryType::Point, vec![]), 0, 5);
        assert_eq!((empty.read, empty.of, empty.values.len()), (0, 0, 0));
    }

    /// At the end of `text`.
    fn at_end(text: &str) -> Completion {
        complete_at(text, text.chars().count(), &ds()).unwrap()
    }

    const OPERATORS: [&str; 13] = ["=", "<>", "<", "<=", ">", ">=", "+", "-", "*", "/", "%", "^", "||"];

    #[test]
    fn an_empty_expression_takes_a_value_or_not() {
        let c = at_end("");
        assert_eq!((c.word, c.place, c.operand), (0..0, Place::Code, true));
        assert_eq!(c.keywords, [Kw::Not, Kw::Null, Kw::True, Kw::False, Kw::Case]);
        assert!(c.symbols.is_empty());
        assert_eq!((c.call, c.field), (None, None));
    }

    #[test]
    fn a_name_being_typed_is_the_word() {
        let c = at_end("pop > 1 AND na");
        assert_eq!((c.word, c.prefix.as_str(), c.operand), (12..14, "na", true));
        let c = at_end("$ar");
        assert_eq!((c.word, c.prefix.as_str(), c.operand), (0..3, "$ar", true));
        // A sigil alone is a variable begun.
        let c = at_end("pop > $");
        assert_eq!((c.word, c.prefix.as_str(), c.field), (6..7, "$", Some(1)));
    }

    #[test]
    fn after_a_value_come_operators() {
        let c = at_end("pop ");
        assert!(!c.operand);
        assert_eq!(c.symbols, OPERATORS);
        assert_eq!(c.keywords, [Kw::And, Kw::Or, Kw::Not, Kw::Is, Kw::Like, Kw::Ilike, Kw::In, Kw::Between]);
        // After a whole comparison, no second comparison: AND, OR and arithmetic.
        let c = at_end("pop > 5 ");
        assert_eq!(c.keywords, [Kw::And, Kw::Or]);
        assert_eq!(c.symbols, ["+", "-", "*", "/", "%", "^", "||"]);
        assert_eq!(c.field, None, "not a value being compared any more");
    }

    #[test]
    fn a_comparison_says_its_field() {
        for (text, place, field) in [
            ("pop > ", Place::Code, 1),
            ("name = 'Pa", Place::Text, 0),
            ("name LIKE '", Place::Text, 0),
            ("name NOT IN ('a', ", Place::Code, 0),
            ("pop BETWEEN ", Place::Code, 1),
            ("pop BETWEEN 1 AND ", Place::Code, 1),
            ("\"Pop Est\" >= ", Place::Code, 2),
        ] {
            let c = at_end(text);
            assert_eq!((c.place, c.field), (place, Some(field)), "{text}");
        }
        let c = at_end("name = 'Pa");
        assert_eq!((c.word, c.prefix.as_str()), (7..10, "Pa"));
        // Inside text no word of the language goes: only the call and the field are said.
        assert!(!c.operand && c.keywords.is_empty() && c.symbols.is_empty());
        let c = at_end("area('h");
        assert_eq!((c.place, c.call.map(|c| (c.spec.func, c.arg))), (Place::Text, Some((Func::Area, 0))));
        // A doubled quote is one quote, as the lexer reads it.
        assert_eq!(at_end("name = 'O''Br").prefix, "O'Br");
        // A value compared with something that isn't a field has no field to take values from.
        assert_eq!(at_end("upper(name) = ").field, None);
    }

    /// A value is compared whatever its left side is: a field, or something computed that has no
    /// field to list values of.
    #[test]
    fn a_value_compared_with_anything_is_said_compared() {
        for text in [
            "name = ",
            "pop >= ",
            "name LIKE ",
            "name IN ('a', ",
            "pop BETWEEN ",
            "pop BETWEEN 1 AND ",
            "upper(name) = ",
            "upper(name) LIKE '",
            "pop + 1 BETWEEN ",
            "pop * 2 IN (",
            // A bracket where the value goes holds the value.
            "name = (",
            "pop > ((",
            "name IN ((",
        ] {
            assert!(at_end(text).compared, "{text}");
        }
        assert_eq!(at_end("name = (").field, Some(0));
        // A bracket that starts a clause, or holds one, compares nothing.
        for text in ["", "pop ", "pop > 5 ", "upper(", "name = 'a' AND ", "pop BETWEEN 1 ", "(", "pop > 5 AND (", "name = (pop > 1 AND "] {
            assert!(!at_end(text).compared, "{text}");
        }
    }

    #[test]
    fn a_call_says_its_function_and_argument() {
        let call = |c: &Completion| c.call.map(|c| (c.spec.func, c.arg, c.open));
        let c = at_end("round(");
        assert_eq!(call(&c), Some((Func::Round, 0, 5)));
        assert!(c.operand && c.symbols.contains(&")"));
        let c = at_end("round(pop, ");
        assert_eq!(call(&c), Some((Func::Round, 1, 5)));
        let c = at_end("round(abs(pop");
        assert_eq!(call(&c), Some((Func::Abs, 0, 9)), "the innermost");
        let c = at_end("round(abs(pop), 2");
        assert_eq!(call(&c), Some((Func::Round, 1, 5)));
        // Where its `(` is counts characters, not bytes.
        assert_eq!(call(&at_end("'é' || upper(")), Some((Func::Upper, 0, 12)));
        assert!(c.symbols.contains(&",") && c.symbols.contains(&")"));
        assert_eq!(at_end("round(pop) ").call, None, "closed");
        assert_eq!(at_end("nosuch(").call, None, "a name no function has");
        assert!(at_end("nosuch(").operand);
    }

    #[test]
    fn case_offers_its_next_word() {
        assert_eq!(at_end("CASE ").keywords, [Kw::Not, Kw::Null, Kw::True, Kw::False, Kw::Case, Kw::When]);
        let c = at_end("CASE WHEN pop > 1 ");
        assert!(c.keywords.contains(&Kw::Then) && c.keywords.contains(&Kw::And));
        assert!(!c.keywords.contains(&Kw::End));
        let c = at_end("CASE WHEN pop > 1 THEN 'big' ");
        for k in [Kw::When, Kw::Else, Kw::End] {
            assert!(c.keywords.contains(&k), "{k:?}");
        }
        let c = at_end("CASE WHEN pop > 1 THEN 'big' ELSE 'small' ");
        assert!(c.keywords.contains(&Kw::End));
        assert!(!c.keywords.contains(&Kw::When) && !c.keywords.contains(&Kw::Else), "nothing but END after ELSE's value");
    }

    #[test]
    fn is_and_between_offer_what_completes_them() {
        assert_eq!(at_end("pop IS ").keywords, [Kw::Not, Kw::Null]);
        assert_eq!(at_end("pop IS NOT ").keywords, [Kw::Null]);
        assert!(at_end("pop BETWEEN 1 ").keywords.contains(&Kw::And));
        assert_eq!(at_end("pop NOT ").keywords, [Kw::Like, Kw::Ilike, Kw::In, Kw::Between]);
        assert_eq!(at_end("pop IN ").symbols, ["("]);
    }

    #[test]
    fn a_quoted_field_being_typed() {
        let c = at_end("\"Pop E");
        // A name goes in the quotes: no keyword nor symbol.
        assert!(c.keywords.is_empty() && c.symbols.is_empty(), "{c:?}");
        assert_eq!((c.place, c.word, c.prefix.as_str(), c.operand), (Place::Field, 0..6, "Pop E", true));
        let c = at_end("pop > 1 AND !Pop");
        assert_eq!((c.place, c.word, c.prefix.as_str()), (Place::Field, 12..16, "Pop"));
        // A doubled `"` is one; `!` has no such escape.
        assert_eq!(at_end("\"Pop \"\"E").prefix, "Pop \"E");
    }

    #[test]
    fn what_was_typed_wrong_before_doesnt_stop_it() {
        // A field no layer has, a function no one has, a date check: still read for what follows.
        assert!(at_end("bogus = 1 AND ").operand);
        assert!(at_end("nosuch(pop) > 1 OR ").operand);
        assert_eq!(at_end("bogus = ").field, None);
        // A measure on a layer it doesn't fit is still a value.
        assert_eq!(at_end("$length ").keywords.first(), Some(&Kw::And));
    }

    #[test]
    fn only_the_text_before_the_cursor_counts() {
        let c = complete_at("pop > 1 AND name = 'x'", 8, &ds()).unwrap();
        assert_eq!(c.keywords, [Kw::And, Kw::Or]);
        // Past the end is the end; characters, not bytes.
        let c = complete_at("año > ", 99, &ds()).unwrap();
        assert_eq!((c.word, c.operand), (6..6, true));
        let c = complete_at("año", 2, &ds()).unwrap();
        assert_eq!((c.word, c.prefix.as_str()), (0..2, "añ"));
    }

    #[test]
    fn hostile_text_is_refused_or_read_within_the_limits() {
        // Deeper than the parser reads: nothing, and nothing overflows.
        let deep = "(".repeat(super::super::MAX_LEN);
        assert!(complete_at(&deep, usize::MAX, &ds()).is_none());
        // Longer than an expression may be.
        assert!(complete_at(&"a".repeat(super::super::MAX_LEN + 1), usize::MAX, &ds()).is_none());
        // A character the language has no use for.
        assert!(complete_at("pop # ", 6, &ds()).is_none());
        // Many arguments, many terms: within the limits, then nothing.
        let args = format!("coalesce({}", "1, ".repeat(200));
        assert!(complete_at(&args, usize::MAX, &ds()).is_none(), "past the most arguments");
        let terms = "pop = 1 OR ".repeat(300);
        assert!(complete_at(&terms, usize::MAX, &ds()).is_none(), "past the most terms");
        for text in ["CASE CASE CASE", "a(", "1e", "pop IN ("] {
            assert!(complete_at(text, usize::MAX, &ds()).is_some(), "{text}");
        }
        // A quote or sigil alone is a word begun.
        for (text, place) in [("'", Place::Text), ("\"", Place::Field), ("!", Place::Field), ("@", Place::Code), ("$", Place::Code)] {
            let c = at_end(text);
            assert_eq!((c.place, c.word), (place, 0..1), "{text}");
        }
        // Text that can't be read into words, wherever the fault is before the cursor.
        for text in ["|", "pop # 1 AND ", "pop > 1.2.3 AND ", "pop > $ "] {
            assert!(complete_at(text, usize::MAX, &ds()).is_none(), "{text}");
        }
        // Text that stops being an expression before the cursor: nothing can follow it.
        for text in [")))", ",,,", "pop > 1 ) AND ", "pop BETWEEN 1 OR ", "pop pop "] {
            assert!(complete_at(text, usize::MAX, &ds()).is_none(), "{text}");
        }
    }
}
