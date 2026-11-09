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
    value(&parse(text, &d).unwrap_or_else(|e| panic!("{text}: {e}")), &d, 0, &Env { map_scale: Some(25_000.0) })
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
                let _ = value(&e, &d, r, &Env { map_scale: Some(f64::NAN) });
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
