//! What can be typed at a point of an expression being written (#1240): the word being typed,
//! and what the language accepts in its place. It reads the text with the real lexer and parser,
//! which note what they look for where the text ends ([`super::Probe`]), so there is no second
//! grammar here to fall behind: whatever the parser learns to read is offered too.

use std::cell::{OnceCell, RefCell};
use std::ops::Range;

use super::{FuncSpec, Kw, Lexed, Open, Parser, Probe, Tok, lex_spans};
use crate::Dataset;

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
    })
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
