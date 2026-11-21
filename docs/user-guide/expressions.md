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

In Select By Attributes, the Layer Properties Query tab and a label class's SQL query, the
**Functions** and **Variables** menus list the language's catalog in the same list the
expression box suggests from. The highlighted item's help shows beside the list (below it in a
narrow window), or the help of the item under the pointer: a function's signature, description
and example, or a variable's description. The list shows ten items at a time and scrolls to the
rest. Up and Down move the highlight, Enter or a click appends the item to the expression, and
Escape, Tab or a click elsewhere closes the list; from the keyboard, Tab to the menu and press
Enter or Space to open it (Space again closes it). While the list is open, shortcuts with Ctrl
(⌘) still work. A function opens its argument list, or inserts the complete call when it takes
no arguments.

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
| `@page_feature_id` | Integer id of the current [Map Series](layouts.md#map-series) index feature |
| `@page_name` | Text shown as the current Map Series page name, with the same fallback as `{pageName}` |
| `@page_number` | Current Map Series page number as an integer, starting at 1 |
| `@row_number` | The feature's position in the layer, from 1 |
| `$id` | The feature's id |
| `$area`, `$perimeter` | A polygon's area in square metres (holes subtract) and the length of its rings in metres |
| `$length` | A line's length in metres |

The three page variables are available to symbols, labels, definition queries and display filters
in every map frame on a series page. They are null outside a series page, including in the map
view, tools and Calculate Field, so `$id = @page_feature_id` matches no feature there. A definition
query such as `$id = @page_feature_id` therefore hides every feature in the map view and the
Layers pane; to mark a page's feature, style it with Symbols by Expression instead.

An unknown field, variable or function is an error that names it.

## Operators

| Operator | Result |
| --- | --- |
| `+ - * /` | Arithmetic; `/` always divides exactly (`7 / 2` is 3.5); null for a division by zero |
| `%` | Remainder; null for a division by zero |
| `^` | Power; null for a division by zero (`0 ^ -1`) or a result that isn't a finite number (`(-8) ^ 0.5`) |
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
- **Comparing dates.** A date or date-time field compares with text that is a full ISO 8601
  date (`issued >= '2026-01-01'`, `seen < '2026-03-05T08:00Z'`) as a date. A date is its
  midnight, so `seen = '2026-03-05'` matches midnight only: use a range for the whole day.
  Date-times with different offsets compare by instant, and one without a zone as if it were
  UTC; a date is its UTC midnight, so `2026-03-05T01:00+02:00` (2026-03-04 at 23:00 UTC) falls
  before `'2026-03-05'`. Converting that date-time into a date field keeps the day it shows,
  2026-03-05. Text that isn't a full date (`'2026'`) compares as text, against the date's ISO text.
  A date compares only with text or another date: against a number (`issued >= 20260101`,
  `issued > year`, arithmetic) it's an error, so put the date in quotes, as `YYYY-MM-DD`. A
  date written without dashes in quotes (`issued >= '20260101'`) is an error too; one that
  comes from a function or a text field (`coalesce('20260101')`, a `YYYYMMDD` column) compares
  as text, so write dates with dashes. Use `||` to join text.
  Arithmetic on a date is an error for now, also on one reached through `coalesce`, `nullif`,
  `if` or `CASE` (`coalesce(issued) + 1`), and so are the number functions (`abs`, `round`,
  `min`, …) given a date.
- **Arithmetic** reads text as a number when it is one (`'12' + 1` is 13). `+` joins text that
  isn't (`name + ' km'`); `-`, `*`, `/` on such text give null.
- **Nulls.** Arithmetic with a null gives null. A comparison with a null is *unknown*: it
  matches neither `pop > 100` nor `NOT pop > 100`; `IS NULL` tests for it. `AND` is false when
  either side is false and `OR` true when either side is true, whatever the other side is.
- **Errors give null**: a division by zero, the square root of a negative number, the logarithm
  of zero or a negative number, a result too large to represent, a value that isn't a finite
  number, text that isn't a number in `-`, `*`, `/`, `%` or `^` or as a number argument, a clamp
  whose low bound is above its high bound. Where the result is stored (Calculate Field,
  calculation attribute rules), a notice says how many rows an error made null, which error,
  and the first three of those rows.
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
| `sqrt(x)`, `exp(x)`, `ln(x)`, `log10(x)` | Square root, eˣ, natural and base-10 logarithm; null for a negative number (`sqrt`), when too large to represent (`exp`), for zero or a negative number (`ln`, `log10`) |
| `pi()` | π |
| `min(a, b, …)`, `max(a, b, …)` | The smallest, largest number; values that aren't numbers are skipped |
| `clamp(low, x, high)` | `x` kept within `low`…`high`; null when `low` is above `high` |
| `scale_linear(x, d0, d1, r0, r1)` | `x` mapped from the domain `d0`…`d1` to the range `r0`…`r1`, clamped to the domain |
| `scale_exp(x, d0, d1, r0, r1, exponent)` | As `scale_linear`, along a curve: 0.5 grows fast then slowly, 2 slowly then fast; null with a negative exponent at the domain's start |
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
[exception](analysis.md#geoprocessing-tools) for a ring winding around a pole; a ring with a
full-turn edge, such as a globe-wide band, counts its whole parallels there too). On a feature with no geometry, or coordinates that aren't
longitudes and latitudes, they are null. Measuring another kind of layer (`$area` on lines), or a
unit that isn't one of those, is an error.
An area expression is also refused if the layer contains a ring through both poles
whose winding exceeds one whole surface. The error counts the affected rings and features;
it does not store an impossible area or silently replace it with null. Perimeter expressions,
ordinary polar caps and whole-world rectangles still work. Attribute rules check only the edited rows, so an unrelated ambiguous feature does not block a clean-row edit. Geometry edits that would invalidate a saved area filter, symbol or label expression are refused and rolled back.

Saved display expressions are checked against the layer’s fields separately from current area values. When an opened project or refreshed layer contains an ambiguous area, the affected features’ area values are unavailable and a counted notice explains this; the expressions are retained and clean features still evaluate normally. Attribute-only edits can retain that existing geometry; new or changed geometry must pass the display check. Area constraints also guard archive restoration. An unrelated field deletion does not discard a valid area expression or rule, and source repair still checks its fields. Unique Values validates the rows kept by the definition query. Streamed display expressions report affected features once per installed window, with a count scoped to the decoded window. Interactive streamed area queries retain clean matches and report unavailable areas among decoded features, including rows their query omits. Full-resolution streamed exports still stop at the first ambiguous decoded chunk and state the scope of that count.

Report filters are retained when a report is imported or fitted to another layer with the
required fields. Building a report whose filter uses an ambiguous area returns a counted error
instead of dropping the filter and listing every record.

Live expression editors check fields and syntax while typing, so a retained area clause does
not claim that clean features have lost their labels. **Check** and **Apply** still validate
the data and report an ambiguous area.

## Limits

A number too large to represent (`1e999`) is an error. An expression is at most 10,000 characters and nests at most 64 levels: each parenthesis,
function call, `NOT`, sign before something other than a number, `^` and `CASE` counts (a
`CASE` two). One chain of `AND`, `OR`, `||`, `+`/`-` or `*`/`/`/`%`, and the arms of one
`CASE`, take at most 256 terms; a function at most 32 arguments; an `IN` list as many as fit.
Text an expression makes is cut at 64 KiB, and a `LIKE` pattern longer than 10,000 characters
matches nothing. A `LIKE` that would take more than about 16 million steps to match (a very long
text against a pattern of many `%`s) is unknown.

## Calculate Field

Calculate Field fills an existing field; it does not create one. If the target field is missing,
the error points to **Add Field** (`edit.addField`): create the field first, then calculate it.

As the expression is typed, a list under the cursor suggests what can go there: the fields
(quoted when their name needs it: `"Pop Est"`), the functions, the variables and the keywords
that start with what's typed, each with its help beside (a function's signature, description
and example; a field's type). Ctrl+Space opens it with nothing typed (on macOS, where Ctrl+Space
may switch the input source, type the first letter instead). ↑/↓ move through it, Enter
or Tab take the highlighted item (Enter doesn't calculate while the list is open) and Escape
closes it. Inside a quoted field name (`"Pop`) only fields are suggested. A function taken from
the list comes with its brackets, the cursor inside them (after them when it takes no arguments).
While the list is open the expression isn't checked: `pop_`, with `pop_est` suggested, is a word
still being chosen. It is checked once the list closes (an item taken, Escape, the cursor moved
away, the box left, or nothing matching what's typed), and its error shown then if it's still
invalid (`pop_x`: no field named 'pop_x').

A `(` typed closes itself, the cursor inside, unless text follows it or it's in a text or a field
name; so does a `'` or `"` where a value can start (at the start, after a space, `(`, `,` or an
operator). A `)` typed before a `)` steps over it, as does a quote typed before the one that ends
its text or name, and Backspace in an empty `()`, `''` or `""` deletes both. Pasting adds nothing.

Inside a function's brackets, its signature floats by the cursor while no list is open, with the
argument being typed marked (`round(x[, places])`) and what the function gives. A list that
opens hides it, and it comes back when the list closes; Escape with no list open closes it until
the cursor leaves that call.

The same list, signature and closing brackets help in the other one-line expression boxes: the
**By Expression** boxes of a layer's symbol and of a label class, Unique Values' **Expression**,
a report's **Where**, an attribute rule's expression and the ribbon's **Display Filters** box
(Style › Drawing Effects). Apart from the rule's, which **Add** applies, and the display filter's,
which **Set Filter** applies, those boxes apply what's written when they lose focus, and a click
in the list keeps the focus, so a field taken from it isn't applied half written; while the list
is open Escape closes it, not the box nor the Display Filters menu.

They help too in the clause boxes of several lines: Select By Attributes' **Where**, the Layer
Properties **Query** tab and a label class's SQL query. There the list shows under the cursor's
line and the signature over the line of its call. While the list is open, Enter takes its item
rather than break the line or apply the clause, and Escape closes it, not the dialog. In the Query
tab, Escape with the list or a signature shown closes that and keeps the clause typed; the next
Escape drops the clause.

A number goes into a number field as it is: a decimal field keeps every digit of `1 / 3` (a
whole number past 2^53 becomes the nearest decimal, as a decimal field holds it). A whole-number
field holds whole numbers, so a decimal result, or text written as one, loses its fraction toward
zero (`2.7` is 2, `-2.7` is -2), and one past the 64-bit integer range, or not a number (NaN), is
left empty. Calculate Field, and a calculation attribute rule on each edit it applies to, says how
many results lost their fraction, how many were past the range and how many weren't numbers. Text
that doesn't read as a number at all (`n/a`) is left empty too, for now without a notice. A
number into a text field is written as RustGIS shows it, to six decimal places.

**Compare codes with different spellings.** With zoning codes in the `current` and `proposed`
fields of the same parcel layer, add a **Text** field named `changed` and turn editing on.
Use this expression in **Calculate Field** for `changed`.

```sql
CASE
  WHEN current IS NULL OR proposed IS NULL THEN 'Unknown'
  WHEN replace(current, '-', '') = replace(proposed, '-', '') THEN 'No'
  ELSE 'Yes'
END
```

Here `N-1` and `N1` both compare as `N1`, while `N-1` and `N2` give `Yes`.
Strip hyphens only when they have no meaning in the coding schemes. Select `changed = 'Yes'`
to find parcels with a different proposed code.

Calculate Field also takes the forms its earlier calculator took: a field name with spaces
written bare (`Pop Est * 2`) still works when the text isn't a valid expression, and on a layer
with a field named `true`, `false` or `null` such forms keep reading it as the field (in other
expressions, quote it: `"null"`). Text that starts and ends with a quote but holds more quotes
now reads as an expression: `'it''s'` is *it's* and `'a' || 'b'` is *ab*, where both were taken
as the text between the outer quotes.
