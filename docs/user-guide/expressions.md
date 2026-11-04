# Expressions

One expression language serves every place that reads values from features:

- **Select By Attributes**, **Definition Query**, display filters, label classes, attribute
  constraints and reports use an expression as a condition (a `WHERE` clause): a feature
  matches when the expression is true.
- **Calculate Field** and calculation attribute rules use an expression as a value, stored in
  the field after conversion to the field's type.
- [Symbols by expression](cartography.md#symbols-by-expression) take a value per feature: a
  marker's size, the fill and outline colours, the outline width and a marker's angle.
- [Labels by expression](cartography.md#labeling) take a label class's text, size, colour and
  angle per feature.
- [Unique Values](cartography.md) can class features by an expression's value instead of a
  field's.

```sql
pop_est > 100000000 AND continent = 'Asia'
name LIKE 'S%' OR iso_a3 IN ('FRA', 'DEU', 'ITA')
"name" || ' (' || to_string(round(pop_est / 1e6)) || ' M)'
CASE WHEN pop_est > 1e8 THEN 'large' WHEN pop_est > 1e7 THEN 'medium' ELSE 'small' END
scale_linear(pop_est, 0, 1e9, 2, 12)
area('ac') BETWEEN 1 AND 5 AND zoning = 'R-1'
```

## Grammar

Keywords and function names are not case sensitive. Rules are listed from the loosest binding
to the tightest; `[ x ]` is optional, `{ x }` repeats zero or more times, `|` separates choices.

```ebnf
expression = or ;
or         = and { "OR" and } ;
and        = not { "AND" not } ;
not        = "NOT" not | comparison ;
comparison = concat [ compare_op concat
                    | "IS" [ "NOT" ] "NULL"
                    | [ "NOT" ] ( "LIKE" | "ILIKE" ) concat
                    | [ "NOT" ] "IN" "(" expression { "," expression } ")"
                    | [ "NOT" ] "BETWEEN" concat "AND" concat ] ;
compare_op = "=" | "==" | "<>" | "!=" | "<" | "<=" | ">" | ">=" ;
concat     = sum { "||" sum } ;
sum        = product { ( "+" | "-" ) product } ;
product    = unary { ( "*" | "/" | "%" ) unary } ;
unary      = ( "-" | "+" ) unary | power ;
power      = primary [ "^" unary ] ;
primary    = number | text | "TRUE" | "FALSE" | "NULL"
           | field | variable | call | case | "(" expression ")" ;
call       = name "(" [ expression { "," expression } ] ")" ;
case       = "CASE" [ expression ]
             "WHEN" expression "THEN" expression
             { "WHEN" expression "THEN" expression }
             [ "ELSE" expression ] "END" ;

number     = digits [ "." digits ] [ exponent ] | "." digits [ exponent ] ;
exponent   = ( "e" | "E" ) [ "+" | "-" ] digits ;
text       = "'" { character | "''" } "'" ;
field      = name | '"' { character | '""' } '"' | "!" { character } "!" ;
variable   = "@" name | "$" name ;
name       = ( letter | digit | "_" ) { letter | digit | "_" } ;   (* not starting with a digit *)
```

So `a OR b AND c` is `a OR (b AND c)`, `NOT a = b` is `NOT (a = b)`, `1 + 2 * 3` is 7,
`-2 ^ 2` is −4 and `2 ^ 3 ^ 2` is 512 (`^` groups from the right). A comparison takes one
operator: write `a < b AND b < c`, not `a < b < c`.

## Values

| Write | Meaning |
| --- | --- |
| `42`, `3.5`, `1e6`, `.5` | Numbers; whole numbers are integers |
| `'text'`, `'it''s'` | Text in single quotes; two quotes stand for one |
| `TRUE`, `FALSE`, `NULL` | Booleans and the missing value |
| `pop_est`, `"Pop Est"`, `!Pop Est!` | A field, by name, ignoring letter case; quote a name with spaces, punctuation or the name of a keyword (`"OR"`, `"null"`) |
| `@map_scale`, `$scale` | The scale denominator of the map being drawn: set in [symbols](cartography.md#symbols-by-expression), [labels](cartography.md#labeling) and Unique Values classes by expression, null in queries and Calculate Field |
| `@row_number` | The feature's position in the layer, from 1 |
| `$id` | The feature's id |
| `$area`, `$perimeter` | A polygon's area in square metres (holes subtract) and the length of its rings in metres |
| `$length` | A line's length in metres |

An unknown field, variable or function is an error that names it.

## Operators

| Operator | Result |
| --- | --- |
| `+ - * /` | Arithmetic; `/` always divides exactly (`7 / 2` is 3.5) |
| `%` | Remainder |
| `^` | Power |
| `\|\|` | Joins the two values as text; null when either is null |
| `= <> < <= > >=` | Comparison; `==` and `!=` are also accepted |
| `LIKE`, `ILIKE` | Text pattern: `%` matches any run of characters, `_` one character |
| `IN (…)` | Equal to one of the values |
| `BETWEEN a AND b` | `>= a AND <= b` |
| `IS NULL`, `IS NOT NULL` | Whether the value is missing |
| `AND OR NOT` | Logic |

Rules shared by all of them:

- **Comparing numbers and text.** A number and text that reads as a number compare as numbers
  (`'12' = 12` is true). Two texts compare as text, ignoring letter case, even when both read
  as numbers (`name = 'paris'` matches `Paris`; `'10' < '9'`; `LIKE` ignores case too).
- **Arithmetic** reads text as a number when it is one (`'12' + 1` is 13). `+` joins text that
  isn't (`name + ' km'`); `-`, `*`, `/` on such text give null.
- **Nulls.** Arithmetic with a null gives null. A comparison with a null is *unknown*: it
  matches neither `pop > 100` nor `NOT pop > 100`; `IS NULL` tests for it. `AND` is false when
  either side is false and `OR` true when either side is true, whatever the other side is.
- **Errors give null**: a division by zero, the square root of a negative number, a result too
  large to represent.
- **`IN`** compares with each value as `=` does; a `NULL` in the list matches nothing, so
  `x NOT IN (1, NULL)` is true for 2.
- **As a condition**, a value that isn't true or false counts as true when it is a non-zero
  number or non-empty text.

## Functions

| Function | Result |
| --- | --- |
| `if(condition, a[, b])` | `a` when the condition is true, otherwise `b` (or null) |
| `coalesce(a, b, …)` | The first value that isn't null |
| `nullif(a, b)` | Null when `a` equals `b`, otherwise `a` |
| `abs(x)` | Absolute value |
| `round(x[, places])` | Rounded half away from zero to `places` decimals (0 by default; negative rounds to tens, hundreds…) |
| `floor(x)`, `ceil(x)` | Rounded down, up |
| `sqrt(x)`, `exp(x)`, `ln(x)`, `log10(x)` | Square root, eˣ, natural and base-10 logarithm |
| `pi()` | π |
| `min(a, b, …)`, `max(a, b, …)` | The smallest, largest number; values that aren't numbers are skipped |
| `clamp(low, x, high)` | `x` kept within `low`…`high`; null when `low` is above `high` |
| `scale_linear(x, d0, d1, r0, r1)` | `x` mapped from the domain `d0`…`d1` to the range `r0`…`r1`, clamped to the domain |
| `scale_exp(x, d0, d1, r0, r1, exponent)` | As `scale_linear`, along a curve: 0.5 grows fast then slowly, 2 slowly then fast |
| `to_string(x)` | `x` as text |
| `to_int(x)`, `to_real(x)` | `x` as a whole number (cut toward zero), as a decimal number; null if it isn't a number |
| `length(s)` | Number of characters |
| `upper(s)`, `lower(s)`, `trim(s)` | Upper case, lower case, without surrounding spaces |
| `substr(s, start[, length])` | Part of `s` from character `start` (1 is the first; negative counts from the end); a negative length leaves that many characters off the end |
| `left(s, n)`, `right(s, n)` | The first, last `n` characters |
| `replace(s, find, with)` | Every `find` in `s` replaced by `with` |
| `concat(a, b, …)` | The values joined as text, nulls skipped |
| `color_rgb(r, g, b)`, `color_rgba(r, g, b, a)` | A colour as the text `'r,g,b'` or `'r,g,b,a'`, each channel 0–255 |
| `area([unit])` | A polygon's area in `'m2'` (the default), `'km2'`, `'ha'`, `'ac'`, `'ft2'` or `'mi2'` |
| `perimeter([unit])` | The length of a polygon's rings in `'m'` (the default), `'km'`, `'ft'` or `'mi'` |
| `line_length([unit])` | A line's length, in the same units as `perimeter` |

Areas and lengths are measured on the WGS 84 ellipsoid, as
[Calculate Geometry](editing.md) in the attribute table measures them, whatever
the layer's coordinate system (with the same
[two exceptions](analysis.md#geoprocessing-tools), a ring with a full-turn edge and one
winding around a pole). On a feature with no geometry, or coordinates that aren't
longitudes and latitudes, they are null. Measuring another kind of layer (`$area` on lines), or a
unit that isn't one of those, is an error.

## Limits

A number too large to represent (`1e999`) is an error. An expression is at most 10,000 characters and nests at most 64 levels: each parenthesis,
function call, `NOT`, sign before something other than a number, `^` and `CASE` counts (a
`CASE` two). One chain of `AND`, `OR`, `||`, `+`/`-` or `*`/`/`/`%`, and the arms of one
`CASE`, take at most 256 terms; a function at most 32 arguments; an `IN` list as many as fit.
Text an expression makes is cut at 64 KiB, and a `LIKE` pattern longer than 10,000 characters
matches nothing. A `LIKE` that would take more than about 16 million steps to match (a very long
text against a pattern of many `%`s) is unknown.

## Calculate Field

A number goes into a number field as it is: a decimal field keeps every digit of `1 / 3` (a
whole number past 2^53 becomes the nearest decimal, as a decimal field holds it). A whole-number
field holds whole numbers, so a decimal result, or text written as one, loses its fraction toward
zero (`2.7` is 2, `-2.7` is -2), and one past the 64-bit integer range, or not a number (NaN), is
left empty. Calculate Field, and a calculation attribute rule on each edit it applies to, says how
many results lost their fraction, how many were past the range and how many weren't numbers. Text
that doesn't read as a number at all (`n/a`) is left empty too, for now without a notice. A
number into a text field is written as RustGIS shows it, to six decimal places.

Calculate Field also takes the forms its earlier calculator took: a field name with spaces
written bare (`Pop Est * 2`) still works when the text isn't a valid expression, and on a layer
with a field named `true`, `false` or `null` such forms keep reading it as the field (in other
expressions, quote it: `"null"`). Text that starts and ends with a quote but holds more quotes
now reads as an expression: `'it''s'` is *it's* and `'a' || 'b'` is *ab*, where both were taken
as the text between the outer quotes.
