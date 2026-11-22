use super::*;
use crate::{Field, FieldType, GeometryType};

fn ds() -> Dataset {
    let mut d = Dataset::new("t", GeometryType::None, vec![Field::new("name", FieldType::Text), Field::new("pop", FieldType::Integer)]);
    for (n, p) in [("Paris", 2_100_000), ("Lyon", 500_000), ("Lille", 230_000)] {
        d.push(None, vec![Value::Text(n.into()), Value::Int(p)]);
    }
    d.push(None, vec![Value::Text("Nowhere".into()), Value::Null]);
    d
}

#[test]
fn clauses() {
    let d = ds();
    assert_eq!(select("pop > 300000", &d).unwrap(), vec![0, 1]);
    assert_eq!(select("NAME LIKE 'l%'", &d).unwrap(), vec![1, 2]);
    assert_eq!(select("name = 'paris' OR (pop < 300000 AND NOT name = 'Lyon')", &d).unwrap(), vec![0, 2]);
    assert_eq!(select("pop IS NULL", &d).unwrap(), vec![3]);
    assert_eq!(select("\"name\" IN ('Lyon', 'Lille')", &d).unwrap(), vec![1, 2]);
    assert_eq!(select("name NOT IN ('Lyon')", &d).unwrap(), vec![0, 2, 3]);
    assert!(select("bogus = 1", &d).is_err());
    assert!(select("pop >", &d).is_err());
}

#[test]
fn nulls_are_unknown_not_false() {
    let d = ds();
    // Row 3 has no population: neither a comparison nor its negation matches it.
    assert_eq!(select("pop > 300000", &d).unwrap(), vec![0, 1]);
    assert_eq!(select("NOT pop > 300000", &d).unwrap(), vec![2]);
    assert_eq!(select("NOT (pop > 300000 OR pop < 300000)", &d).unwrap(), Vec::<usize>::new());
    assert_eq!(select("NOT (pop > 300000 AND name = 'Lyon')", &d).unwrap(), vec![0, 2, 3]);
    // A false side settles AND, a true side settles OR, whatever the null side is.
    assert_eq!(select("pop > 1 OR name = 'Nowhere'", &d).unwrap(), vec![0, 1, 2, 3]);
    assert_eq!(select("NOT (pop > 1 AND name = 'Paris')", &d).unwrap(), vec![1, 2, 3]);
    assert_eq!(select("NOT (pop > 1 AND name = 'Nowhere')", &d).unwrap(), vec![0, 1, 2]);
    // IS NULL is never unknown.
    assert_eq!(select("NOT pop IS NULL", &d).unwrap(), vec![0, 1, 2]);
    assert_eq!(select("NOT name LIKE 'l%'", &d).unwrap(), vec![0, 3]);
}

#[test]
fn quoted_field_names_are_never_keywords() {
    let mut d = Dataset::new("t", GeometryType::None, vec![Field::new("OR", FieldType::Integer), Field::new("null", FieldType::Text)]);
    d.push(None, vec![Value::Int(1), Value::Text("x".into())]);
    assert_eq!(select("\"OR\" = 1 AND \"null\" = 'x'", &d).unwrap(), vec![0]);
}

#[test]
fn hostile_input() {
    let d = ds();
    let long_chain = vec!["pop = 1"; 2000].join(" AND ");
    for t in ["", "(", "((((", "'", "name = 'x' AND", &"(".repeat(5000), &"NOT ".repeat(5000), "!", "pop = 1e999999", &long_chain] {
        let _ = select(t, &d);
    }
    assert!(select(&long_chain, &d).is_err());
    for t in ["1e999 + 1", "pop < 1e999", "-1e999"] {
        assert!(parse(t, &d).is_err_and(|e| e.contains("too large")), "{t}");
    }
    assert!(like("abc", "a%c") && like("abc", "_b_") && !like("abc", "a_") && like("", "%"));
}

fn v(text: &str) -> Value {
    let d = ds();
    value(&parse(text, &d).unwrap_or_else(|e| panic!("{text}: {e}")), &d, 0, &Env { map_scale: Some(25_000.0), ..Env::default() })
}

fn t(s: &str) -> Value {
    Value::Text(s.into())
}

#[test]
fn values_and_precedence() {
    assert_eq!(v("1 + 2 * 3"), Value::Int(7));
    assert_eq!(v("(1 + 2) * 3"), Value::Int(9));
    assert_eq!(v("7 / 2"), Value::Float(3.5));
    assert_eq!(v("7 % 3"), Value::Int(1));
    assert_eq!(v("-2 ^ 2"), Value::Float(-4.0));
    assert_eq!(v("2 ^ 3 ^ 2"), Value::Float(512.0));
    assert_eq!(v("1 - -1"), Value::Int(2));
    assert_eq!(v("1e-3 * 1000"), Value::Float(1.0));
    assert_eq!(v("pop / 0"), Value::Null);
    assert_eq!(v("9223372036854775807 + 1"), Value::Float(9223372036854775808.0));
    assert_eq!(v("pop + NULL"), Value::Null);
    assert_eq!(v("name + ' km'"), t("Paris km"));
    assert_eq!(v("name - 1"), Value::Null);
    assert_eq!(v("'12' + 3"), Value::Float(15.0));
    assert_eq!(v("name || ' (' || pop || ')'"), t("Paris (2100000)"));
    assert_eq!(v("'it''s'"), t("it's"));
    assert_eq!(v("!name!"), t("Paris"));
    assert_eq!(v("pop > 1 AND name = 'paris'"), Value::Bool(true));
    assert_eq!(v("NOT pop > 1"), Value::Bool(false));
    assert_eq!(v("NULL > 1"), Value::Null);
    assert_eq!(v("pop BETWEEN 2000000 AND 3000000"), Value::Bool(true));
    assert_eq!(v("pop NOT BETWEEN 2000000 AND 3000000"), Value::Bool(false));
    assert_eq!(v("length(name) IN (4, 5)"), Value::Bool(true));
    assert_eq!(v("upper(name) ILIKE 'par%'"), Value::Bool(true));
    assert_eq!(v("@map_scale"), Value::Float(25_000.0));
    assert_eq!(v("$scale / 1000"), Value::Float(25.0));
    assert_eq!(v("@row_number"), Value::Int(1));
    assert_eq!(v("$id"), Value::Int(1));
}

#[test]
fn case_expressions() {
    assert_eq!(v("CASE WHEN pop > 1e7 THEN 'huge' WHEN pop > 1e6 THEN 'big' ELSE 'small' END"), t("big"));
    assert_eq!(v("CASE name WHEN 'lyon' THEN 1 WHEN 'PARIS' THEN 2 END"), Value::Int(2));
    assert_eq!(v("CASE WHEN pop < 0 THEN 1 END"), Value::Null);
    assert_eq!(v("case when true then 'x' else 'y' end"), t("x"));
}

#[test]
fn functions() {
    assert_eq!(v("if(pop > 1, 'yes', 'no')"), t("yes"));
    assert_eq!(v("if(NULL, 'yes', 'no')"), t("no"));
    assert_eq!(v("if(FALSE, 1)"), Value::Null);
    assert_eq!(v("coalesce(NULL, NULL, 3)"), Value::Int(3));
    assert_eq!(v("nullif(name, 'PARIS')"), Value::Null);
    assert_eq!(v("abs(-3)"), Value::Int(3));
    assert_eq!(v("round(2.5)"), Value::Int(3));
    assert_eq!(v("round(-2.5)"), Value::Int(-3));
    assert_eq!(v("round(2.71828, 2)"), Value::Float(2.72));
    assert_eq!(v("round(1234, -2)"), Value::Int(1200));
    assert_eq!(v("floor(-1.5)"), Value::Int(-2));
    assert_eq!(v("ceil(1.2)"), Value::Int(2));
    assert_eq!(v("sqrt(16)"), Value::Float(4.0));
    assert_eq!(v("sqrt(-1)"), Value::Null);
    assert_eq!(v("ln(0)"), Value::Null);
    assert_eq!(v("log10(1000)"), Value::Float(3.0));
    assert_eq!(v("round(pi() * 100)"), Value::Int(314));
    assert_eq!(v("min(3, NULL, 'x', 1.5)"), Value::Float(1.5));
    assert_eq!(v("max(3, 7, 5)"), Value::Int(7));
    assert_eq!(v("clamp(0, 15, 10)"), Value::Int(10));
    assert_eq!(v("clamp(0, -5, 10)"), Value::Int(0));
    assert_eq!(v("scale_linear(5, 0, 10, 100, 200)"), Value::Float(150.0));
    assert_eq!(v("scale_linear(50, 0, 10, 100, 200)"), Value::Float(200.0));
    assert_eq!(v("scale_exp(5, 0, 10, 0, 100, 2)"), Value::Float(25.0));
    assert_eq!(v("to_string(1.5) || 'x'"), t("1.5x"));
    assert_eq!(v("to_int('3.9')"), Value::Int(3));
    assert_eq!(v("to_int('x')"), Value::Null);
    assert_eq!(v("to_real(2)"), Value::Float(2.0));
    assert_eq!(v("length('héllo')"), Value::Int(5));
    assert_eq!(v("upper(name) || lower(name)"), t("PARISparis"));
    assert_eq!(v("trim('  a ')"), t("a"));
    assert_eq!(v("substr('abcdef', 2, 3)"), t("bcd"));
    assert_eq!(v("substr('abcdef', -2)"), t("ef"));
    assert_eq!(v("substr('abcdef', 2, -1)"), t("bcde"));
    assert_eq!(v("substr('abc', 10)"), t(""));
    assert_eq!(v("left('abc', 2) || right('abc', 2) || left('abc', -1) || right('abc', 9)"), t("abbcabc"));
    assert_eq!(v("replace('a-b-c', '-', '+')"), t("a+b+c"));
    assert_eq!(v("concat('a', NULL, 1)"), t("a1"));
    assert_eq!(v("color_rgb(255, 128.4, -3)"), t("255,128,0"));
    assert_eq!(v("COLOR_RGBA(1, 2, 3, 300)"), t("1,2,3,255"));
}

#[test]
fn errors_name_what_is_wrong() {
    let d = ds();
    let err = |s: &str| parse(s, &d).err().unwrap_or_default();
    assert_eq!(err("nosuch(1)"), "unknown function 'nosuch'");
    assert_eq!(err("@nosuch"), "unknown variable '@nosuch'");
    assert_eq!(err("bogus + 1"), "no field named 'bogus'");
    assert_eq!(err("round()"), "round takes 1 to 2 arguments");
    assert_eq!(err("pi(1)"), "pi takes 0 arguments");
    assert_eq!(err("abs(1, 2)"), "abs takes 1 argument");
    assert_eq!(err("CASE WHEN TRUE THEN 1"), "expected END, found the end");
    assert!(parse(&format!("max({})", vec!["1"; 40].join(",")), &d).is_err());
    assert!(parse("1 < 2 < 3", &d).is_err());
}

#[test]
fn keyword_named_fields_compare_as_before() {
    let mut d = Dataset::new(
        "t",
        GeometryType::None,
        vec![Field::new("null", FieldType::Text), Field::new("case", FieldType::Integer), Field::new("true", FieldType::Integer)],
    );
    d.push(None, vec![Value::Text("x".into()), Value::Int(2), Value::Int(0)]);
    assert_eq!(select("null = 'x' AND case = 2 AND true = 0", &d).unwrap(), vec![0]);
    // On the right of a comparison, NULL is still the null literal.
    assert_eq!(select("\"null\" = NULL", &d).unwrap(), Vec::<usize>::new());
}

#[test]
fn text_results_are_capped() {
    let d = ds();
    let big =
        "replace(replace(replace(replace('aaaaaaaaaa', 'a', 'aaaaaaaaaa'), 'a', 'aaaaaaaaaa'), 'a', 'aaaaaaaaaa'), 'a', 'aaaaaaaaaa')";
    let Value::Text(s) = value(&parse(&format!("{big} || {big}"), &d).unwrap(), &d, 0, &Env::default()) else { panic!() };
    assert!(s.len() <= MAX_TEXT);
}

/// A tiny deterministic generator (no dependency).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as usize
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.next() % xs.len()]
    }
}

fn wide() -> Dataset {
    let mut d = Dataset::new(
        "t",
        GeometryType::None,
        vec![
            Field::new("name", FieldType::Text),
            Field::new("pop", FieldType::Integer),
            Field::new("area", FieldType::Double),
            Field::new("flag", FieldType::Boolean),
            Field::new("code", FieldType::Text),
            Field::new("OR", FieldType::Integer),
            Field::new("null", FieldType::Text),
        ],
    );
    let names = ["Paris", "lyon", "Lille", "", "St. Étienne", "100", "<Null>", "a_b%c"];
    for i in 0..48i64 {
        let n = names[(i % 8) as usize];
        let nul = |k: i64| i % k == 0;
        d.push(
            None,
            vec![
                if nul(7) { Value::Null } else { Value::Text(n.into()) },
                if nul(5) { Value::Null } else { Value::Int(i * 1000 - 9000) },
                if nul(6) { Value::Null } else { Value::Float(i as f64 * 0.75 - 3.0) },
                if nul(9) { Value::Null } else { Value::Bool(i % 2 == 0) },
                if nul(4) { Value::Null } else { Value::Text(format!("{:03}", i % 13)) },
                Value::Int(i % 3),
                if nul(3) { Value::Null } else { Value::Text(["x", "X", "y"][(i % 3) as usize].into()) },
            ],
        );
    }
    d
}

fn clause(g: &mut Lcg, depth: usize) -> String {
    let fields = ["name", "pop", "area", "flag", "code", "\"OR\"", "\"null\"", "NAME", "\"pop\"", "null"];
    let lits = [
        "0", "1", "-5", "-9000", "1000", "2.25", "-0.75", "1e3", "'Paris'", "'paris'", "'100'", "'012'", "''", "'<Null>'", "TRUE", "false",
        "NULL", "'x'", "'a''b'", ".5",
    ];
    let ops = ["=", "==", "<>", "!=", "<", "<=", ">", ">="];
    let pats = ["'%'", "'l%'", "'%e'", "'_y%'", "'1%'", "'a\\_b%'", "'%n%'", "''", "'P_RIS'"];
    match if depth > 3 { g.next() % 4 } else { g.next() % 8 } {
        0 | 1 => format!("{} {} {}", g.pick(&fields), g.pick(&ops), g.pick(&lits)),
        2 => format!("{} {}LIKE {}", g.pick(&fields), g.pick(&["", "NOT "]), g.pick(&pats)),
        3 => {
            let n = 1 + g.next() % 4;
            let vals: Vec<&str> = (0..n).map(|_| g.pick(&lits)).collect();
            format!("{} {}IN ({})", g.pick(&fields), g.pick(&["", "NOT ", "not "]), vals.join(", "))
        }
        4 => format!("{} IS {}NULL", g.pick(&fields), g.pick(&["", "NOT "])),
        5 => format!("NOT {}", clause(g, depth + 1)),
        6 => format!("({})", clause(g, depth + 1)),
        _ => format!("{} {} {}", clause(g, depth + 1), g.pick(&["AND", "OR", "and", "or"]), clause(g, depth + 1)),
    }
}

/// Every clause the WHERE parser accepted before the expression language selects the same rows.
#[test]
fn differential_against_the_old_where_parser() {
    let d = wide();
    let mut hand = vec![
        "pop > 300000",
        "NAME LIKE 'l%'",
        "name = 'paris' OR (pop < 300000 AND NOT name = 'Lyon')",
        "pop IS NULL",
        "\"name\" IN ('Lyon', 'Lille')",
        "name NOT IN ('Lyon')",
        "pop=-5000",
        "pop>=-5000",
        "area < -1",
        "pop = 1e3",
        "flag = true",
        "flag <> FALSE",
        "code = 12",
        "code = '012'",
        "code > 5",
        "name IN (NULL)",
        "name = NULL",
        "\"OR\" = 1 AND \"null\" = 'x'",
        "null = 'x'",
        "null <> 'y'",
        "NOT NOT pop > 0",
        "((pop > 0))",
        "name LIKE '%'",
        "name IN ('<Null>')",
        "area = 2.25",
        "pop <> 1000",
        "pop = '1000'",
        "name > 'm'",
        "flag = 1",
        "pop >= .5",
    ];
    let mut g = Lcg(0x5eed);
    let generated: Vec<String> = (0..6000).map(|_| clause(&mut g, 0)).collect();
    hand.extend(generated.iter().map(String::as_str));
    let mut accepted = 0;
    for c in hand {
        let Ok(old) = legacy::select(c, &d) else { continue };
        accepted += 1;
        assert_eq!(select(c, &d), Ok(old), "{c}");
        // The fast nodes and the general ones agree: the same clause as a value expression.
        let e = parse(c, &d).unwrap();
        for r in 0..d.features.len() {
            assert_eq!(value(&e, &d, r, &Env::default()) == Value::Bool(true), eval(&e, &d, r), "{c} row {r}");
        }
    }
    assert!(accepted > 4000, "only {accepted} clauses accepted by the old parser");
}

/// Junk never panics, parsed or evaluated.
#[test]
fn junk_never_panics() {
    let d = wide();
    let bits = [
        "(",
        ")",
        ",",
        "'",
        "\"",
        "!",
        "@",
        "$",
        "||",
        "|",
        "+",
        "-",
        "*",
        "/",
        "%",
        "^",
        "=",
        "<",
        ">",
        "1",
        "0",
        "1e",
        "1e-",
        ".",
        "9e999",
        "name",
        "pop",
        "NOT",
        "AND",
        "OR",
        "IS",
        "NULL",
        "IN",
        "LIKE",
        "BETWEEN",
        "CASE",
        "WHEN",
        "THEN",
        "ELSE",
        "END",
        "if",
        "round",
        "substr",
        "replace",
        "scale_exp",
        "min",
        "@map_scale",
        "$id",
        "'x'",
        " ",
        "é",
        "\u{0}",
        "to_int",
        "color_rgba",
        "-9223372036854775808",
        "9223372036854775807",
    ];
    let mut g = Lcg(42);
    for _ in 0..20000 {
        let n = 1 + g.next() % 14;
        let s: String = (0..n).map(|_| g.pick(&bits)).collect::<Vec<_>>().join(if g.next().is_multiple_of(2) { " " } else { "" });
        if let Ok(e) = parse(&s, &d) {
            for r in [0, 1, 7, 999] {
                let _ = value(&e, &d, r, &Env { map_scale: Some(f64::NAN), ..Env::default() });
                let _ = eval(&e, &d, r);
            }
        }
    }
    for deep in [
        "(".repeat(200) + "1" + &")".repeat(200),
        "-".repeat(500) + "1",
        "abs(".repeat(100) + "1" + &")".repeat(100),
        "2^".repeat(300) + "2",
    ] {
        assert!(parse(&deep, &d).is_err(), "{}", &deep[..20]);
    }
}

/// Nesting as deep as the old parser took still parses, and the deepest expressions evaluate on a
/// test thread's small stack.
#[test]
fn nesting_limits() {
    let d = wide();
    for inner in ["pop = -1", "pop IN (1, -2)", "name LIKE 'a%'"] {
        let c = format!("{}{inner}{}", "(".repeat(63), ")".repeat(63));
        assert!(legacy::select(&c, &d).is_ok(), "{c}");
        assert_eq!(select(&c, &d), legacy::select(&c, &d), "{c}");
    }
    let deepest = [
        format!("{}pop{}", "abs(".repeat(63), ")".repeat(63)),
        format!("{}pop{}", "(-".repeat(31), ")".repeat(31)),
        format!("{}1{}", "if(TRUE, ".repeat(63), ")".repeat(63)),
        // Each CASE nests twice: itself and its THEN.
        format!("{}1{}", "CASE WHEN TRUE THEN ".repeat(31), " END".repeat(31)),
    ];
    for e in &deepest {
        let parsed = parse(e, &d).unwrap_or_else(|err| panic!("{err}: {e}"));
        for r in 0..d.features.len() {
            let _ = value(&parsed, &d, r, &Env::default());
        }
    }
    assert!(parse(&format!("{}pop{}", "abs(".repeat(64), ")".repeat(64)), &d).is_err());
    // An IN list's items nest unless they are single values.
    let chain = format!("{}1{}", "1 IN (".repeat(65), ")".repeat(65));
    assert!(parse(&chain, &d).is_err_and(|e| e.contains("nested too deeply")), "{chain}");
    let shallow = format!("{}1{}", "1 IN (".repeat(64), ")".repeat(64));
    assert!(parse(&shallow, &d).is_ok());
    for c in ["pop IN (NULL, 'x', name, -2)", "pop IN (+3)"] {
        let c = format!("{}{c}{}", "(".repeat(63), ")".repeat(63));
        assert!(parse(&c, &d).is_ok(), "{c}");
    }
    // Each argument of a clamp is evaluated once: nested 40 deep, they return at all (2^40
    // evaluations a row wouldn't), and as one clamp would.
    let clamps = format!("{}pop{}", "clamp(0, ".repeat(40), ", 10)".repeat(40));
    let (parsed, one) = (parse(&clamps, &d).unwrap(), parse("clamp(0, pop, 10)", &d).unwrap());
    for r in 0..d.features.len() {
        assert_eq!(value(&parsed, &d, r, &Env::default()), value(&one, &d, r, &Env::default()), "row {r}");
    }
}

#[test]
fn a_null_anywhere_in_a_join_is_null() {
    let d = ds();
    let big =
        "replace(replace(replace(replace('xxxxxxxxxx', 'x', 'xxxxxxxxxx'), 'x', 'xxxxxxxxxx'), 'x', 'xxxxxxxxxx'), 'x', 'xxxxxxxxxx')";
    let joined = format!("{big} || {big}");
    assert!(matches!(value(&parse(&joined, &d).unwrap(), &d, 0, &Env::default()), Value::Text(_)));
    assert_eq!(value(&parse(&format!("{joined} || NULL"), &d).unwrap(), &d, 0, &Env::default()), Value::Null);
}

#[test]
fn a_computed_in_list_skips_nulls_and_a_long_pattern_matches_nothing() {
    let d = ds();
    let rows = |e: &str| select(e, &d).unwrap_or_else(|err| panic!("{err}: {e}"));
    // `<Null>` is how a null shows, not what it equals.
    let mut odd = Dataset::new("t", GeometryType::None, vec![Field::new("name", FieldType::Text)]);
    odd.push(None, vec![Value::Text("<null>".into())]);
    assert_eq!(select("upper(name) IN (NULL)", &odd).unwrap(), Vec::<usize>::new());
    assert_eq!(select("name IN (NULL)", &odd).unwrap(), Vec::<usize>::new());
    assert_eq!(select("name NOT IN (NULL)", &odd).unwrap(), vec![0]);
    assert_eq!(value(&parse("round(1e307, 2)", &odd).unwrap(), &odd, 0, &Env::default()), Value::Float(1e307));
    assert_eq!(select("upper(name) NOT IN (NULL)", &odd).unwrap(), vec![0]);
    // About 65,000 '%'s (text is cut at 64 KiB): longer than a pattern may be.
    let long = "name LIKE replace(replace(replace(replace(replace('x', 'x', 'xxxxxxxxxx'), 'x', 'xxxxxxxxxx'), 'x', 'xxxxxxxxxx'), 'x', 'xxxxxxxxxx'), 'x', '%%%%%%%%%%')";
    assert_eq!(rows(long), Vec::<usize>::new());
    assert_eq!(rows(&long.replace("LIKE", "NOT LIKE")), vec![0, 1, 2, 3]);
    // A match too costly to finish (64 KiB of a's against '%', 1,000 a's and a b) is unknown:
    // neither LIKE nor NOT LIKE holds.
    // 10^n a's, made by replacing.
    let a = |n: usize| format!("{}'x'{}, 'x', 'aaaaaaaaaa')", "replace(".repeat(n), ", 'x', 'xxxxxxxxxx')".repeat(n - 1));
    let costly = format!("{} LIKE '%' || {} || 'b'", a(5), a(3));
    assert_eq!(rows(&costly), Vec::<usize>::new());
    assert_eq!(rows(&costly.replace(" LIKE ", " NOT LIKE ")), Vec::<usize>::new());
    assert!(!like(&"a".repeat(65536), &format!("%{}b", "a".repeat(1000))));
    // A costly LIKE leaves the others in its clause theirs: a cheap one after it (every name is
    // like itself) still matches. But four at their cap spend what a clause's LIKEs may take for
    // one row together (#1398), so one after them is unknown: on Paris's row only, as each row
    // has a budget of its own.
    let cheap = "name LIKE name";
    assert_eq!(rows(&format!("{costly} OR {cheap}")), vec![0, 1, 2, 3]);
    let four = [costly.as_str(); 4].join(" OR ");
    assert_eq!(rows(&format!("(name = 'Paris' AND ({four})) OR {cheap}")), vec![1, 2, 3]);
}

#[test]
fn geometry_is_measured_on_the_ellipsoid() {
    use crate::{Geometry, Measure};
    use rustgis_geom::{Coord, SQUARE_METRES_PER_ACRE};
    // Parcels near Memphis: squares 0.001° (about 2.5 acres) and 0.002° (about 10 acres) a side.
    let square = |x: f64, y: f64, d: f64| {
        let ring = vec![Coord::new(x, y), Coord::new(x + d, y), Coord::new(x + d, y + d), Coord::new(x, y + d), Coord::new(x, y)];
        Geometry::Polygon(vec![vec![ring]])
    };
    let mut d = Dataset::new("parcels", GeometryType::Polygon, vec![Field::new("zoning", FieldType::Text)]);
    d.push(Some(square(-90.0, 35.1, 0.001)), vec![Value::Text("R-1".into())]);
    d.push(Some(square(-90.01, 35.1, 0.002)), vec![Value::Text("R-1".into())]);
    d.push(Some(square(-90.02, 35.1, 0.002)), vec![Value::Text("C-2".into())]);
    d.push(None, vec![Value::Text("R-1".into())]);
    d.push(Some(square(500.0, 35.1, 0.002)), vec![Value::Text("R-1".into())]);

    let f = |text: &str, row: usize| value(&parse(text, &d).unwrap(), &d, row, &Env::default()).as_f64();
    // The same figures as Calculate Geometry stores.
    for (text, m) in [("$area", Measure::Area), ("$PERIMETER", Measure::Perimeter)] {
        let want = d.features.get(1).and_then(|f| f.geometry.as_ref()).and_then(|g| g.wgs84_measure(m));
        assert_eq!(f(text, 1), want, "{text}");
    }
    let acres = f("area('AC')", 1).unwrap();
    assert!((acres - f("$area", 1).unwrap() / SQUARE_METRES_PER_ACRE).abs() < 1e-12 && (9.0..11.0).contains(&acres), "{acres}");
    assert_eq!(f("area()", 1), f("$area", 1));
    assert_eq!(f("perimeter('m')", 1), f("$perimeter", 1));
    // No geometry, or coordinates that aren't longitudes and latitudes: null.
    assert_eq!(f("$area", 3), None);
    assert_eq!(f("$area", 4), None);
    assert_eq!(select("area('ac') BETWEEN 5 AND 20 AND zoning = 'R-1'", &d).unwrap(), vec![1]);

    // A measure of another kind of layer is an error, not a column of nulls.
    assert!(parse("$length > 1", &d).is_err_and(|e| e.contains("perimeter")));
    assert!(parse("line_length('mi') > 1", &d).is_err_and(|e| e.contains("polyline")));
    // So is a unit it doesn't know, or one that isn't fixed text.
    assert!(parse("area('acres') > 1", &d).is_err_and(|e| e.contains("ac, ft2")));
    assert!(parse("area('ft') > 1", &d).is_err());
    assert!(parse("area(zoning) > 1", &d).is_err_and(|e| e.contains("as text")));
    let lines = Dataset::new("roads", GeometryType::Polyline, vec![]);
    assert!(parse("$area > 1", &lines).is_err_and(|e| e.contains("polygon")));
    assert!(parse("$length > 1 AND line_length('mi') < 2", &lines).is_ok());
}

#[test]
fn ambiguous_polar_area_is_a_counted_expression_error() {
    use crate::{Geometry, Measure};
    use rustgis_geom::Coord;
    let c = Coord::new;
    let bad = vec![c(-180.0, 90.0), c(180.0, 90.0), c(-90.0, -90.0), c(90.0, -80.0)];
    let world = vec![c(-180.0, -90.0), c(180.0, -90.0), c(180.0, 90.0), c(-180.0, 90.0)];
    let mut ds = Dataset::new("Polar", GeometryType::Polygon, vec![]);
    ds.push(Some(Geometry::Polygon(vec![vec![bad.clone()]])), vec![]);
    ds.push(Some(Geometry::Polygon(vec![vec![world.clone(), bad]])), vec![]);
    for expression in ["$area", "area('km2')", "$area > 0"] {
        let error = parse(expression, &ds).unwrap_err();
        assert!(error.contains("Polar") && error.contains("2 ring(s) in 2 feature(s)") && error.contains("area is ambiguous"), "{error}");
    }
    let geometry = ds.features[0].geometry.as_ref().unwrap();
    assert_eq!(geometry.wgs84_measure(Measure::Area), None);
    assert!(geometry.wgs84_measure(Measure::Perimeter).is_some_and(f64::is_finite));
    let perimeter = parse("$perimeter", &ds).unwrap();
    assert!(value(&perimeter, &ds, 0, &Env::default()).as_f64().is_some_and(f64::is_finite));
    let mut clean = Dataset::new("World", GeometryType::Polygon, vec![]);
    clean.push(Some(Geometry::Polygon(vec![vec![world]])), vec![]);
    let expression = parse("$area", &clean).unwrap();
    assert!(value(&expression, &clean, 0, &Env::default()).as_f64().is_some_and(|area| area > 5e14));
}

#[test]
fn where_clauses_compare_dates() {
    let mut d = Dataset::new("t", GeometryType::None, vec![Field::new("issued", FieldType::Date), Field::new("ts", FieldType::DateTime)]);
    for (day, at) in
        [("2025-12-31", "2026-03-05T00:00:00"), ("2026-01-01", "2026-03-05T09:00:00+02:00"), ("2026-06-30", "2026-03-04T23:00:00-05:00")]
    {
        d.push(None, vec![Value::parse(day, FieldType::Date), Value::parse(at, FieldType::DateTime)]);
    }
    d.push(None, vec![Value::Null, Value::Null]);
    assert_eq!(select("issued >= '2026-01-01'", &d).unwrap(), vec![1, 2]);
    assert_eq!(select("issued BETWEEN '2026-01-01' AND '2026-03-31'", &d).unwrap(), vec![1]);
    assert_eq!(select("issued IN ('2026-06-30', '2025-12-31')", &d).unwrap(), vec![0, 2]);
    // A date-time equals a date's midnight; offsets compare by instant (09:00+02:00 is 07:00Z).
    assert_eq!(select("ts = '2026-03-05'", &d).unwrap(), vec![0]);
    assert_eq!(select("ts < '2026-03-05T08:00Z'", &d).unwrap(), vec![0, 1, 2]);
    assert_eq!(select("ts > '2026-03-05T04:00:00Z'", &d).unwrap(), vec![1]);
    assert_eq!(select("ts > issued", &d).unwrap(), vec![0, 1]);
    // Text that isn't a full date still compares as text, as saved clauses did.
    assert_eq!(select("issued > '2026'", &d).unwrap(), vec![1, 2]);
    assert_eq!(select("issued LIKE '2026-%'", &d).unwrap(), vec![1, 2]);
    // Arithmetic on dates is an error, not a column of nulls.
    assert_eq!(parse("issued + 30", &d).err().unwrap_or_default(), "'issued' holds dates, and arithmetic on dates isn't supported yet");
    // Also a date reached through a function or CASE, or given to a number's function: each
    // would be null for every row.
    for e in ["coalesce(issued) + 1", "-nullif(issued, '2026-01-01')", "CASE WHEN 1 = 1 THEN issued END * 2", "if(true, 1, issued) + 1"] {
        assert_eq!(parse(e, &d).err().unwrap_or_default(), "'issued' holds dates, and arithmetic on dates isn't supported yet", "{e}");
    }
    assert_eq!(parse("abs(issued)", &d).err().unwrap_or_default(), "'issued' holds dates, and abs on dates isn't supported yet");
    // A date against anything that gives a number would compare text with digits: an error.
    for e in [
        "issued >= 20260101",
        "20260101 < issued",
        "issued = 20260305",
        "ts IN (1, 2)",
        "issued BETWEEN 20260101 AND 20261231",
        "issued >= 2026 * 10000 + 101",
        "issued > @row_number",
        "nullif(issued, 20260101) IS NULL",
        "CASE issued WHEN 20260101 THEN 1 ELSE 0 END = 1",
        "20260101 = if(true, issued, 1)",
        "nullif(20260101, if(true, issued, 1)) IS NULL",
        "CASE 20260101 WHEN if(true, issued, 1) THEN 1 ELSE 0 END = 1",
        "issued < pi()",
        "issued >= if(true, 20260101, 20250101)",
        "issued = CASE WHEN true THEN 20260305 END",
        "issued = coalesce(20260305, 1)",
        "issued = (1 = 1)",
    ] {
        assert!(parse(e, &d).err().unwrap_or_default().contains("not a number"), "{e}");
    }
    // Also a number a field holds or a function gives; text still compares.
    let n = Dataset::new(
        "t",
        GeometryType::None,
        vec![Field::new("issued", FieldType::Date), Field::new("n", FieldType::Integer), Field::new("s", FieldType::Text)],
    );
    // Also every branch of if, coalesce and CASE, and arithmetic, which reads text that is a number
    // as one.
    for e in [
        "issued > n",
        "issued = abs(n)",
        "n < issued",
        "issued >= if(n > 0, n * 10000 + 101, issued)",
        "issued = coalesce(n, issued)",
        "issued = CASE WHEN n > 0 THEN n ELSE issued END",
        "issued > s + 1",
        "issued = s + 'x'",
    ] {
        assert!(parse(e, &n).err().unwrap_or_default().contains("not a number"), "{e}");
    }
    for e in ["issued > s", "issued >= '2026-' || '01-01'", "issued = coalesce(s, '2026-01-01')", "issued >= upper(s)", "issued = NULL"] {
        assert!(parse(e, &n).is_ok(), "{e}");
    }
    assert!(parse("issued > ts", &d).is_ok(), "two dates compare");
    assert!(parse("issued = @page_name", &d).is_ok(), "the name of a page can be date text");
    assert!(parse("issued = @page_number", &d).is_err(), "the page number is numeric");
    // A date without dashes is text no date reads as: an error asking for YYYY-MM-DD. A shorter
    // partial date still compares as text.
    for e in [
        "issued >= '20260101'",
        "issued = '20260305'",
        "'20260101' < issued",
        "issued IN ('2026-01-01', '20260101')",
        "issued >= '2026/03/05'",
        "issued >= '2026-1-1'",
        "issued = '05.03.2026'",
    ] {
        assert!(parse(e, &d).err().unwrap_or_default().contains("write the date as YYYY-MM-DD"), "{e}");
    }
    // With a time part, the advice is a date-time's.
    for e in ["ts >= '2026/03/05 14:30'", "ts >= '2026-03-05 14:30:00 +02:00'", "ts < '2026-03-05T14:30:00.1234567'"] {
        let error = parse(e, &d).err().unwrap_or_default();
        assert!(
            error.contains("write the date-time as ISO 8601 ('2026-03-05T14:30:00+02:00'") && !error.contains("YYYY-MM-DD"),
            "{e}: {error}"
        );
    }
    for e in [
        "issued > '2026'",
        "issued >= '2026-03'",
        "issued LIKE '2026-%'",
        "ts >= '2026-03-05 14:30'",
        "issued < '2026-03-05 revision 2'",
        "issued < '2026-03-05 14:30 revision 2'",
        "issued < '2026 report 03 page 05'",
        "issued < '2026-03/05'",
    ] {
        assert!(parse(e, &d).is_ok(), "{e}");
    }
    for e in ["to_int(issued)", "to_real(ts)", "color_rgb(issued, 0, 0)"] {
        assert!(parse(e, &d).err().unwrap_or_default().contains("on dates isn't supported yet"), "{e}");
    }
    assert!(parse("coalesce(issued, '2026-01-01')", &d).is_ok(), "a date passed through is no arithmetic");
    assert!(parse("-ts", &d).is_err());
    assert_eq!(value(&parse("issued || ''", &d).unwrap(), &d, 0, &Env::default()), Value::Text("2025-12-31".into()));
}

/// Layers every catalog example fits on: a table, a polygon and a line, each with one feature.
fn catalog_layers() -> Vec<Dataset> {
    use crate::Geometry;
    use rustgis_geom::Coord;
    let ring = vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0), Coord::new(0.01, 0.01), Coord::new(0.0, 0.01), Coord::new(0.0, 0.0)];
    let mut table = Dataset::new("table", GeometryType::None, vec![]);
    table.push(None, vec![]);
    let mut polygons = Dataset::new("polygons", GeometryType::Polygon, vec![]);
    polygons.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
    let mut lines = Dataset::new("lines", GeometryType::Polyline, vec![]);
    lines.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)]])), vec![]);
    vec![table, polygons, lines]
}

/// The value `text` gives on the first layer of `layers` it parses on.
fn catalog_value(text: &str, layers: &[Dataset]) -> Option<Value> {
    let env =
        Env { map_scale: Some(25_000.0), page: Some(std::sync::Arc::new(SeriesPage { feature_id: 42, name: "North".into(), number: 1 })) };
    layers.iter().find_map(|d| parse(text, d).ok().map(|e| value(&e, d, 0, &env)))
}

#[test]
fn every_function_has_one_catalog_entry_that_works() {
    let layers = catalog_layers();
    let mut names = std::collections::HashSet::new();
    for &f in Func::ALL {
        let s = f.spec();
        assert_eq!(s.func, f);
        assert!(names.insert(s.name), "{} is listed twice", s.name);
        assert_eq!(s.name, s.name.to_ascii_lowercase());
        assert!(s.signature.starts_with(&format!("{}(", s.name)) && s.signature.ends_with(')'), "{}", s.signature);
        assert!(s.min <= s.max && s.max <= MAX_ARGS, "{}", s.name);
        assert!(!s.description.is_empty(), "{} has no description", s.name);
        assert!(s.example.starts_with(&format!("{}(", s.name)), "{}'s example calls another function", s.name);
        // The example parses where the function fits and gives a value.
        let v = catalog_value(s.example, &layers);
        assert!(v.is_some_and(|v| v != Value::Null), "{} gives nothing", s.example);
    }
    // A call resolves through the catalog, in any letter case.
    assert!(catalog_value("ROUND(1.5)", &layers).is_some());
    assert!(parse("nonesuch(1)", &layers[0]).is_err_and(|e| e.contains("unknown function")));
}

#[test]
fn every_variable_has_one_catalog_entry_that_works() {
    let layers = catalog_layers();
    let mut names = std::collections::HashSet::new();
    for s in VARS {
        assert!(!s.names.is_empty() && !s.description.is_empty());
        for name in s.names {
            assert!(names.insert(name.to_ascii_lowercase()), "{name} is listed twice");
            assert!(name.starts_with(['@', '$']), "{name}");
            assert!(catalog_value(name, &layers).is_some_and(|v| v != Value::Null), "{name} gives nothing");
            assert!(catalog_value(&name.to_ascii_uppercase(), &layers).is_some(), "{name} in upper case");
        }
    }
    assert!(parse("@nonesuch", &layers[0]).is_err_and(|e| e.contains("unknown variable")));
}

#[test]
fn every_keyword_is_listed_once_and_read() {
    let mut words = std::collections::HashSet::new();
    for k in Kw::ALL {
        assert!(words.insert(k.word()), "{} is listed twice", k.word());
        assert_eq!(k.word(), k.word().to_ascii_uppercase());
        assert!(!k.description().is_empty());
    }
    // Each one in a clause that uses it, in lower case.
    let d = ds();
    let clause = "NOT pop IS NULL AND (name LIKE 'p%' OR name ILIKE 'l%') AND pop IN (1, 2100000) \
                  AND pop BETWEEN 1 AND 3000000 AND TRUE AND NOT FALSE \
                  AND CASE WHEN pop > 1 THEN TRUE ELSE FALSE END";
    let used: Vec<Kw> =
        Kw::ALL.iter().copied().filter(|k| clause.split_whitespace().any(|w| w.trim_matches(['(', ')']) == k.word())).collect();
    assert_eq!(used, Kw::ALL, "the clause should use every keyword");
    assert_eq!(select(&clause.to_ascii_lowercase(), &d).unwrap(), vec![0]);
}

/// The user guide's section headed `## {heading}`, up to the next one. A Windows checkout may
/// give the guide CRLF line endings (`core.autocrlf`), so they are read as LF.
fn guide_section(heading: &str) -> String {
    let guide = include_str!("../../../../docs/user-guide/expressions.md").replace("\r\n", "\n");
    let start = guide.find(&format!("\n## {heading}\n")).unwrap_or_else(|| panic!("no {heading} section"));
    let rest = guide.get(start + 1..).unwrap_or_default();
    rest.get(3..).and_then(|r| r.find("\n## ")).map_or(rest, |end| rest.get(..end + 3).unwrap_or(rest)).to_string()
}

#[test]
fn the_user_guide_lists_what_the_catalogs_hold() {
    let listed = guide_section("Functions");
    for s in super::functions() {
        assert!(listed.contains(&format!("`{}`", s.signature)), "docs/user-guide/expressions.md: Functions should list `{}`", s.signature);
    }
    // And nothing else: every call written in the table's first column is a catalog function.
    for row in listed.lines().filter(|l| l.starts_with("| `")) {
        let first = row.split('|').nth(1).unwrap_or_default();
        for call in first.split('`').skip(1).step_by(2) {
            let name = call.split('(').next().unwrap_or_default();
            assert!(super::functions().any(|s| s.name == name), "the guide lists `{call}`, which isn't a function");
        }
    }
    // The units the measures take.
    for m in [Measure::Area, Measure::Perimeter, Measure::Length] {
        for (unit, _) in m.units() {
            assert!(listed.contains(&format!("'{unit}'")), "Functions should name the unit '{unit}'");
        }
    }
    let values = guide_section("Values");
    for name in VARS.iter().flat_map(|s| s.names) {
        assert!(values.contains(&format!("`{name}`")), "Values should list `{name}`");
    }
    let grammar = guide_section("Grammar");
    for k in Kw::ALL {
        assert!(grammar.contains(&format!("\"{}\"", k.word())), "the grammar should use \"{}\"", k.word());
    }
}

#[test]
fn page_variables_are_typed_and_null_without_a_series() {
    let mut ds = ds();
    let id = 1_u64 << 53;
    ds.features[0].id = id;
    ds.features[1].id = id + 1;
    let env = Env { page: Some(std::sync::Arc::new(SeriesPage { feature_id: id, name: "North".into(), number: 2 })), ..Env::default() };
    for (text, expected) in [
        ("@PAGE_FEATURE_ID", Value::Int(i64::try_from(id).unwrap())),
        ("@page_name", Value::Text("North".into())),
        ("@page_number", Value::Int(2)),
    ] {
        let e = parse(text, &ds).unwrap();
        assert_eq!(value(&e, &ds, 0, &env), expected);
        assert_eq!(value(&e, &ds, 0, &Env::default()), Value::Null);
        assert!(reads_series_page(text));
    }
    let same = parse("$id = @page_feature_id", &ds).unwrap();
    assert!(matches(&same, &ds, 0, &env));
    assert!(!matches(&same, &ds, 1, &env), "adjacent large feature IDs have different pages");
    let different = parse("@page_feature_id <> $id", &ds).unwrap();
    assert!(!matches(&different, &ds, 0, &env));
    assert!(matches(&different, &ds, 1, &env));
    let listed = parse("$id IN (@page_feature_id)", &ds).unwrap();
    assert!(matches(&listed, &ds, 0, &env));
    assert!(!matches(&listed, &ds, 1, &env), "IN keeps adjacent large IDs apart too");
    let after = parse("$id > @page_feature_id", &ds).unwrap();
    assert_eq!((matches(&after, &ds, 0, &env), matches(&after, &ds, 1, &env)), (false, true), "ordering is exact too");
    let written = parse("$id = '9007199254740993'", &ds).unwrap();
    assert_eq!((eval(&written, &ds, 0), eval(&written, &ds, 1)), (false, true), "an id written as text is that id");
    assert!(!eval(&same, &ds, 0));
    assert!(!reads_series_page("'@page_feature_id' = !@page_number!"));
}

/// Each error that gives null says which it was; a null read from a field, written, or left by
/// a guarded branch, and an error whose null a later step replaced, say nothing.
#[test]
fn errors_say_why_they_are_null() {
    let d = ds();
    let why = |text: &str, row: usize| {
        let e = parse(text, &d).unwrap_or_else(|e| panic!("{text}: {e}"));
        value_or_fault(&e, &d, row, &Env::default())
    };
    for (text, fault) in [
        ("pop / 0", Fault::DivisionByZero),
        ("pop % 0", Fault::DivisionByZero),
        ("1.5 % 0", Fault::DivisionByZero),
        ("0 ^ -1", Fault::DivisionByZero),
        ("sqrt(0 - pop)", Fault::NegativeRoot),
        ("ln(0)", Fault::Logarithm),
        ("log10(0 - 1)", Fault::Logarithm),
        ("exp(1000)", Fault::Overflow),
        ("10 ^ 400", Fault::Overflow),
        ("(0 - 8) ^ 0.5", Fault::NotANumber),
        ("name * 2", Fault::Text),
        ("sqrt(name)", Fault::Text),
        ("to_real(name)", Fault::Text),
        ("clamp(10, pop, 0)", Fault::ClampBounds),
        ("scale_exp(0, 0, 10, 1, 2, -1)", Fault::DivisionByZero),
        ("round(pop / 0) + 1", Fault::DivisionByZero),
        // An error's null in either position of an operator still reports.
        ("(1 / 0) * 2", Fault::DivisionByZero),
        ("2 * (1 / 0)", Fault::DivisionByZero),
        ("1 / 0 * 2", Fault::DivisionByZero),
        ("sqrt(1 / 0)", Fault::DivisionByZero),
        ("round(1 / 0, 0)", Fault::DivisionByZero),
        ("nullif(pop / 0, 1)", Fault::DivisionByZero),
        ("'(' || (pop / 0) || ')'", Fault::DivisionByZero),
    ] {
        assert_eq!(why(text, 0), (Value::Null, Some(fault)), "{text}");
    }
    // A function that skips nulls has each null argument as a cause: fixing the error alone gives
    // a value, so the error is said even beside a missing argument.
    for text in ["coalesce(1 / 0, pop)", "max(1 / 0, pop)", "min(pop, sqrt(-1))"] {
        assert!(why(text, 3).1.is_some(), "{text}");
    }
    // Null in, null out, without an error; what an expression asks for or guards against.
    for (text, row) in [
        ("pop / 2", 3),
        ("null", 0),
        ("if(pop > 0, 1)", 3),
        ("if(pop / 0 > 1, 1, null)", 0),
        ("CASE WHEN pop / 0 > 1 THEN 1 END", 0),
        ("CASE pop WHEN 1 / 0 THEN 1 END", 0),
        ("nullif(pop, pop)", 0),
        // The division's null was replaced; the result is null because `pop` is.
        ("coalesce(1 / 0, 0) / pop", 3),
        // An error that only decided a condition, a CASE's arm or what nullif compares with.
        ("if((1 / 0 IS NULL) AND NULL, 1)", 0),
        ("CASE 1 / 0 WHEN 1 THEN 2 ELSE NULL END", 0),
        ("nullif(NULL, 1 / 0)", 0),
        // A missing operand makes an operator's result null whatever else failed.
        ("1 / 0 + pop", 3),
        ("pop * (1 / 0)", 3),
        ("(1 / 0) || pop", 3),
        ("clamp(pop, 1 / 0, 5)", 3),
        // Empty or `<null>` text is missing, not text that isn't a number.
        ("'' * 2", 0),
        ("'' * (1 / 0)", 0),
        ("(1 / 0) - '<null>'", 0),
        ("sqrt('<null>')", 0),
        ("clamp('', 1 / 0, 5)", 0),
        ("'' + '' - (1 / 0)", 0),
        ("round('', 1 / 0)", 0),
    ] {
        assert_eq!(why(text, row), (Value::Null, None), "{text}");
    }
    assert_eq!(why("coalesce(pop / 0, 7)", 0), (Value::Int(7), None));
    // The arm a CASE takes keeps its own error.
    assert_eq!(why("CASE 1 / 0 WHEN 1 THEN 2 ELSE sqrt(-1) END", 0), (Value::Null, Some(Fault::NegativeRoot)));
    assert_eq!(why("(1 / 0 IS NULL) AND NULL", 0), (Value::Null, None));
    assert_eq!(arith(Arith::Mod, &Value::Int(i64::MIN), &Value::Int(-1)), Ok(Value::Int(0)));
    // A stored infinity or NaN isn't joined as text: `+` on it is null, and says why.
    let mut odd = Dataset::new("odd", GeometryType::None, vec![Field::new("x", FieldType::Double)]);
    odd.push(None, vec![Value::Float(f64::INFINITY)]);
    odd.push(None, vec![Value::Float(f64::NAN)]);
    for (text, row) in [("x + 1", 0), ("x + 1", 1), ("'pop ' + x", 0)] {
        let e = parse(text, &odd).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(value_or_fault(&e, &odd, row, &Env::default()), (Value::Null, Some(Fault::NotANumber)), "{text}, row {row}");
    }
    assert_eq!(why("sqrt(0)", 0), (Value::Float(0.0), None));
}
