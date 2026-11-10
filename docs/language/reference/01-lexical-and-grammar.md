# 1. Lexical structure and grammar

## Source text

Source is UTF-8. The processor keeps line, column and byte offsets for every
token and reports diagnostics against byte spans. A malformed token is reported
as malformed; it is never re-read as a different token.

Comments are `// to end of line` and `/* block */`. Block comments nest. Comment
delimiters inside a string literal do not start a comment.

## Identifiers and keywords

An identifier starts with `_` or a Unicode 16.0.0 `XID_Start` character and
continues with `_` or `XID_Continue` characters. Identifiers are compared after
NFC normalisation, case-sensitively; the original spelling is kept for display.
String contents are never normalised.

The keywords are ASCII and case-sensitive:

```text
as assert base break case continue dim else enum false fn for if impl in let
loop native null offset affine protocol pub return self static table true type
unit use while
```

`dim`, `unit`, `base`, `offset` and `affine` are reserved and have no meaning in
1.2; a source that uses them as declaration keywords receives a targeted
diagnostic. `T`, `Z` and `f` in literals are not keywords.

A keyword may be used as a member name after `.`, as an enum variant name, as a
named-argument label and as a record field name (`contextual_name` in the
grammar). It cannot be a local variable, parameter or declaration name. `self`
is an expression only inside a receiver context.

`Some` and `null` are the option spellings (chapter 2). `Result`, `Ok` and
`Err` are not reserved; a program may declare types or variants with those
names.

## Tokens

Operators use longest match. These tokens are indivisible: `|?` `??` `||` `&&`
`=>` `..=` `..` `<=` `>=` `==` `!=` `+=` `-=` `*=` `/=`. A standalone postfix `?`
has no expression meaning. In an expression the source `???` is the token `??`
followed by `?` and is an error. In a type, `T?` is the option type
(chapter 2).

### Numeric literals

```text
42  1_000  0xFF  0b1010          Int
3.1415  2e-3  12.34.decimal      Decimal
3.1415f  1e9f  2f                Float
12.34.GBP                        Money<GBP>
1.day  7.days                    Duration
```

Rules:

1. An integer literal is decimal digits, `0x` hex digits or `0b` binary digits.
   Its value has arbitrary precision. Whether it fits is a quota matter, not a
   lexical one (chapter 2).
2. A literal with a fraction or exponent and no suffix is a `Decimal`. It is
   parsed exactly; no binary float is involved.
3. The suffix `f` makes any decimal numeral a `Float` (IEEE-754 binary64,
   correctly rounded).
4. An underscore is allowed only between two digits of one numeral part.
   Leading, trailing and doubled underscores are errors. A `0x` or `0b` prefix
   must be followed by a digit.
5. A leading `+` or `-` is the unary operator, not part of the literal. An
   exponent sign belongs to the exponent.
6. A dot after an integer starts a fraction only when a digit follows. Otherwise
   it is a member access on the integer literal: `1.day`, `12.34.decimal` and
   `12.34.GBP` are a literal followed by `.name`. The names `decimal`, a
   currency type and the built-in duration names (`day`, `days` and the rest of
   the duration set) are resolved by the checker; any other name is an error.
   There is no user-declared unit suffix.

### Dates and instants

```text
2026-09-01
2026-09-01T14:30:00Z
2026-09-01T14:30:00.250+01:00
```

A complete date or instant is recognised before the characters could be read as
integers and subtraction. Calendar values use the proleptic Gregorian calendar
with years 0001 to 9999 and valid month and day combinations. An instant needs
`Z` or a numeric offset, at most nine fractional-second digits and seconds in
00 to 59; leap-second notation is rejected. It is normalised to UTC. A
date-shaped token with an invalid calendar value is a literal error, never a
subtraction.

The `Decimal`, `Float`, `Date` and `Instant` token classes are disjoint: one
source numeral has exactly one tokenisation.

### Strings

A string literal is `"..."` with these escapes: `\"` `\\` `\n` `\r` `\t` `\0`
and `\u{hex}` (one to six hex digits naming a Unicode scalar; surrogates and
values above U+10FFFF are errors). `{ expression }` inside a string
interpolates the expression's display text. `\u{7b}` writes a literal `{`
without starting interpolation. Interpolated expressions use the ordinary
lexer.

## Modules and names

A file's path gives its namespace: `a/b.orna` is `a.b`, and `a/main.orna` is
`a`. The root `main.orna` is the root namespace. A repository may not contain
both `x.orna` and `x/main.orna`. Path components are NFC and must be unique under
Unicode 16.0.0 NFKC casefold.

`sys`, `core` and `std` are the reserved top-level namespaces. `sys` and `core`
are built in and cannot be replaced or shadowed. `std` is replaceable
ordinary Orna.

```orna
use geometry;                      // namespace as `geometry`
use geometry.shapes as shapes;
use geometry.shapes.*;             // every pub name
use geometry.shapes.{Circle, area};
use std.prelude as _;              // the prelude set, no alias
pub use geometry.shapes.area;      // re-export under the same name
pub use geometry.shapes as g;      // re-export under another name
```

* A local declaration wins over an explicit import, and an explicit import over
  a wildcard. Two wildcard imports that expose the same unresolved name are an
  ambiguity error; import order never picks a winner.
* A wildcard imports only `pub` names. `pub use` makes the imported name part
  of this module's public surface, so other modules reach it as
  `thismodule.name`. A re-export keeps the identity of the original
  declaration; it creates no copy.
* Importing executes nothing.

## Native declarations

```orna
native fn bit_or(a: Int, b: Int): Int;     // bound at link by key
```

A `native fn` has no body and ends with `;`. The parser and the checker accept
it in any module unit. The binding key is `(module path, name)`. Keys in `core`
and `sys` bind to the engine implementation; every other key binds at link to a
symbol in the shared library the root project names in its manifest, with a
pinned digest. An unbound key is a link error before evaluation.
A native declaration lists its effects in the engine's declaration metadata
(chapter 4); there is no effect syntax for Orna-written functions.

## Grammar

Notation: `,` sequence, `|` choice, `[ ]` optional, `{ }` repeat zero or more,
`( )` grouping, quoted text is a token. Entry points are `module_unit`,
`row_unit` and `repl_input`.

```text
module_unit     = { top_level_item } ;
repl_input      = use_declaration | let_statement | function_declaration | expression ;
row_unit        = record_expression , [ ";" ] ;

top_level_item  = use_declaration | table_declaration | function_declaration
                | native_declaration | protocol_declaration | enum_declaration
                | type_declaration | assertion_statement ;
visibility      = [ "pub" ] ;

use_declaration = visibility , "use" , namespace_path , use_tail , ";" ;
use_tail        = empty | "as" , ( identifier | "_" ) | "." , "*"
                | "." , "{" , identifier , { "," , identifier } , [ "," ] , "}" ;
namespace_path  = identifier , { "." , identifier } ;
qualified_name  = identifier , { "." , contextual_name } ;
qualified_variant = identifier , "." , contextual_name , { "." , contextual_name } ;
```

`pub use` takes only the empty tail or `as name`; `pub use p.*;` and
`pub use p.{a, b};` are errors.

### Declarations

```text
table_declaration = visibility , "table" , identifier ,
                    [ "(" , key_parameters , ")" ] , "{" , { table_member } , "}" ;
table_member    = field_declaration | assertion_clause | nested_impl_declaration ;
key_parameters  = key_parameter , { "," , key_parameter } , [ "," ] ;
key_parameter   = identifier , ":" , type_expression , [ "=" , expression ] ;
field_declaration = identifier , ":" , type_expression , [ field_initializer ] , "," ;
field_initializer = "=" , expression | "=>" , expression ;

function_declaration = visibility , "fn" , identifier , [ generic_parameters ] ,
                       "(" , [ function_parameters ] , ")" , [ ":" , type_expression ] ,
                       ( "=" , expression , ";" | block_expression ) ;
native_declaration   = visibility , "native" , "fn" , identifier , [ generic_parameters ] ,
                       "(" , [ function_parameters ] , ")" , ":" , type_expression , ";" ;
function_parameters  = function_parameter , { "," , function_parameter } , [ "," ] ;
function_parameter   = ( "self" | identifier ) , [ ":" , type_expression ] ,
                       [ "=" , expression ] ;
generic_parameters   = "<" , generic_parameter , { "," , generic_parameter } , [ "," ] , ">" ;
generic_parameter    = identifier , [ "impl" , protocol_bounds ] ;
protocol_bounds      = qualified_protocol , { "+" , qualified_protocol } ;
qualified_protocol   = qualified_name , [ "<" , type_arguments , ">" ] ;

protocol_declaration = visibility , "protocol" , identifier , [ generic_parameters ] ,
                       "{" , { protocol_member } , "}" ;
protocol_member      = protocol_function | protocol_static_property ;
protocol_function    = "fn" , identifier , [ generic_parameters ] ,
                       "(" , [ function_parameters ] , ")" , [ ":" , type_expression ] , ";" ;
protocol_static_property = "static" , identifier , ":" , type_expression , ";" ;

type_declaration  = visibility , "type" , identifier , [ generic_parameters ] ,
                    ( "=" , type_expression , ";"
                    | "=" , type_expression , refinement_body
                    | nominal_type_body ) ;
refinement_body   = "{" , { assertion_clause | nested_impl_declaration } , "}" ;
nominal_type_body = "{" , { nominal_type_member } , "}" ;
nominal_type_member = nominal_field | assertion_clause | nested_impl_declaration ;
nominal_field     = visibility , identifier , ":" , type_expression ,
                    [ "=" , expression ] , "," ;
nested_impl_declaration = "impl" , qualified_protocol , "{" , { impl_member } , "}" ;
impl_member       = impl_function | impl_static_property ;
impl_function     = "fn" , identifier , [ generic_parameters ] ,
                    "(" , [ function_parameters ] , ")" , [ ":" , type_expression ] ,
                    ( "=" , expression , ";" | block_expression ) ;
impl_static_property = "static" , identifier , [ ":" , type_expression ] , "=" , expression , ";" ;

enum_declaration  = visibility , "enum" , identifier , [ generic_parameters ] ,
                    "{" , [ enum_variants ] , "}" ;
enum_variants     = enum_variant , { "," , enum_variant } , [ "," ] ;
enum_variant      = contextual_name , [ "{" , [ enum_payload_fields ] , "}" ] ;
enum_payload_fields = enum_payload_field , { "," , enum_payload_field } , [ "," ] ;
enum_payload_field  = identifier , ":" , type_expression ;
```

### Types

```text
type_expression = type_postfix ;
type_postfix    = type_primary , [ "?" ] ;
type_primary    = qualified_name , [ "<" , type_arguments , ">" ]
                | "[" , type_expression , "]"
                | "{" , [ type_fields ] , "}"
                | "(" , type_expression , ")"
                | tuple_type | function_type ;
function_type   = "fn" , "(" , [ type_expression , { "," , type_expression } , [ "," ] ] , ")" ,
                  ":" , type_expression ;
tuple_type      = "(" , ")" | "(" , type_expression , "," ,
                  [ type_expression , { "," , type_expression } , [ "," ] ] , ")" ;
type_arguments  = type_expression , { "," , type_expression } ;
type_fields     = type_field , { "," , type_field } , [ "," ] ;
type_field      = contextual_name , ":" , type_expression ;
```

The type parser reads `??` as two `?`. A second `?` is a syntactic accept and a
type error (`T??`, chapter 2).

### Statements and blocks

```text
block_expression  = "{" , { block_item } , [ expression ] , "}" ;
block_item        = semicolon_statement | braced_control_statement ;
semicolon_statement = let_statement | assignment_statement | assertion_statement
                    | expression_statement | return_statement | break_statement
                    | continue_statement ;
braced_control_statement = ( if_expression | case_expression | for_expression
                           | while_expression ) , [ ";" ]
                         | loop_expression , ";" ;
let_statement     = "let" , pattern , [ ":" , type_expression ] , "=" , expression , ";" ;
assignment_statement = identifier , assignment_operator , expression , ";" ;
assignment_operator  = "=" | "+=" | "-=" | "*=" | "/=" ;
assertion_statement = "assert" , assertion_expression , ";" ;
assertion_clause  = "assert" , assertion_expression , ";" ;
assertion_expression = subjectless_predicate | expression ;
subjectless_predicate = comparison_operator , expression ;
comparison_operator = "<" | "<=" | ">" | ">=" | "==" | "!=" | "in" ;
expression_statement = expression , ";" ;
return_statement  = "return" , [ expression ] , ";" ;
break_statement   = "break" , [ expression ] , ";" ;
continue_statement = "continue" , ";" ;
```

A block item is parsed one at a time. A braced control expression followed by
another item is a statement; the last unsemicolonated expression before `}` is
the block value. A `loop` used as a non-final statement needs `;`.

### Patterns

```text
pattern           = "_" | literal_pattern | constructor_pattern | enum_record_pattern
                  | qualified_variant | tuple_pattern | record_pattern | list_pattern
                  | identifier ;
constructor_pattern = qualified_name , "(" , [ pattern_list ] , ")" ;
enum_record_pattern = qualified_variant , "{" , [ record_pattern_fields ] , "}" ;
tuple_pattern     = "(" , ")" | "(" , pattern , "," , [ pattern_list ] , ")" ;
record_pattern    = "{" , [ record_pattern_fields ] , "}" ;
list_pattern      = "[" , [ pattern_list ] , "]" ;
pattern_list      = pattern , { "," , pattern } , [ "," ] ;
record_pattern_fields = record_pattern_field , { "," , record_pattern_field } , [ "," ] ;
record_pattern_field  = contextual_name , [ ":" , pattern ] ;
literal_pattern   = instant_literal | date_literal | float_literal | decimal_numeral
                  | integer_literal | string_literal | "true" | "false" | "null" ;
```

### Expressions

```text
expression        = lambda_expression ;
lambda_expression = lambda_parameters , "=>" , lambda_body | logical_or_expression ;
lambda_parameters = identifier | "_" | "(" , [ lambda_parameter_list ] , ")" ;
lambda_parameter_list = lambda_parameter , { "," , lambda_parameter } , [ "," ] ;
lambda_parameter  = pattern , [ ":" , type_expression ] ;
lambda_body       = direct_record_body | block_expression | non_braced_expression ;
case_body         = direct_record_body | block_expression | non_braced_expression ;
direct_record_body = "{" , record_fields , "}" ;
non_braced_expression = lambda_expression ;      (* never begins with "{" *)

logical_or_expression  = logical_and_expression , { "||" , logical_and_expression } ;
logical_and_expression = comparison_expression , { "&&" , comparison_expression } ;
comparison_expression  = coalesce_expression , [ comparison_operator , coalesce_expression ] ;
coalesce_expression    = pipeline_expression , [ "??" , coalesce_expression ] ;
pipeline_expression    = range_expression , { ( "|" | "|?" ) , postfix_expression } ;
range_expression = additive_expression , [ ( ".." | "..=" ) , [ additive_expression ] ]
                 | ( ".." | "..=" ) , additive_expression ;
additive_expression       = multiplicative_expression , { ( "+" | "-" ) , multiplicative_expression } ;
multiplicative_expression = power_expression , { ( "*" | "/" | "%" ) , power_expression } ;
power_expression  = unary_expression , [ "^" , power_expression ] ;
unary_expression  = { "!" | "-" | "+" } , postfix_expression ;
postfix_expression = primary_expression , { postfix_operator } ;
postfix_operator  = "." , contextual_name | "(" , [ argument_list ] , ")" | "[" , expression , "]" ;
argument_list     = argument , { "," , argument } , [ "," ] ;
argument          = [ contextual_name , ":" ] , expression ;

primary_expression = repl_binding | literal | generic_call_expression | nominal_constructor
                   | "self" | tuple_expression | qualified_name | record_expression
                   | list_expression | if_expression | case_expression | for_expression
                   | while_expression | loop_expression | "(" , expression , ")" ;
generic_call_expression = qualified_name , "<" , type_arguments , ">" ,
                          "(" , [ argument_list ] , ")" ;
nominal_constructor = qualified_name , "{" , [ record_fields ] , "}" ;
tuple_expression  = "(" , ")" | "(" , expression , "," ,
                    [ expression , { "," , expression } , [ "," ] ] , ")" ;
if_expression     = "if" , expression , block_expression ,
                    [ "else" , ( if_expression | block_expression ) ] ;
case_expression   = "case" , expression , "{" , { case_arm } , "}" ;
case_arm          = pattern , [ "if" , expression ] , ":" , case_body , [ "," ] ;
for_expression    = "for" , pattern , "in" , expression , block_expression ;
while_expression = "while" , expression , block_expression ;
loop_expression   = "loop" , block_expression ;
record_expression = "{" , [ record_fields ] , "}" ;
record_fields     = record_field , { "," , record_field } , [ "," ] ;
record_field      = contextual_name , ":" , expression ;
list_expression   = "[" , [ expression , { "," , expression } , [ "," ] ] , "]" ;

literal           = instant_literal | date_literal | float_literal | decimal_numeral
                  | integer_literal | string_literal | "true" | "false" | "null" ;
repl_binding      = "$_" | "$?" ;
```

Lexical productions (`identifier`, numerals, strings, dates, instants) are as
described under Tokens above.

### Precedence

Lowest binding first.

| Level | Forms | Associativity |
|---:|---|---|
| 1 | lambda `=>` | right |
| 2 | `\|\|` | left, short-circuit |
| 3 | `&&` | left, short-circuit |
| 4 | `==` `!=` `<` `<=` `>` `>=` `in` | none; never chained |
| 5 | `??` | right |
| 6 | `\|` `\|?` | left |
| 7 | `..` `..=` | none |
| 8 | `+` `-` | left |
| 9 | `*` `/` `%` | left |
| 10 | `^` | right |
| 11 | unary `!` `-` `+` | right |
| 12 | `.name`, call, index | left |

`a < b < c` and `a == b == c` are syntax errors; write `a < b && b < c`.
Unary operators bind tighter than `^`, so `-2 ^ 2` is `(-2) ^ 2`; write `-(2 ^ 2)` for the other reading.

### Context-sensitive rules

1. **Parenthesis.** When `(` begins an expression, the parser scans to the
   matching `)`. If the next token is `=>` it is a lambda parameter list.
   Otherwise `()` is `Unit`, a top-level comma makes a tuple, and a comma-free
   pair groups. `(x,)` is a one-element tuple.
2. **Direct braced body.** After `=>` or a `case` arm `:`, a `{` body is
   classified before it is parsed: `{}` is an empty block of type `Unit`; a body
   whose first item is `identifier :` is a record expression; anything else is a
   block. To return the empty record, write `() => ({})`.
3. **Control headers.** A record literal or nominal constructor that is the
   whole condition or scrutinee of `if`, `while`, `for` or `case` must be
   parenthesised; an unparenthesised `{` there starts the body.
4. **Pipeline stage.** A stage is a postfix expression. A lambda stage must be
   parenthesised: `xs | (x => x + 1)`.
5. **Record punning.** A record literal field is always `name: expression`.
   Only a record pattern allows the bare `{ count }` shorthand.
6. **Type arguments.** The parser first tries `generic_call_expression`:
   a qualified name, `<`, type arguments, `>`, then `(`. If that whole shape
   does not match, `<` is the comparison operator.
7. **Assignment** is a statement, not an expression. Its target is one local
   variable (chapter 3).
8. **Module top level** holds declarations only; an expression statement there
   is an error. A module-level `assert` must depend on at least two tables.

## Changes from 1.1.0

* Added `pub use path;` and `pub use path as name;`. The 1.1 grammar had no
  re-export.
* Added `native fn` declarations (any module unit; keys outside `core` and
  `sys` bind at link to a manifest-named shared library) and the keyword
  `native`.
* Removed `dimension_declaration` and `unit_declaration` from
  `top_level_item`, and the `*` and `/` operators from `type_expression`
  (`Money<GBP> / kWh` no longer exists). The words `dim`, `unit`, `base`,
  `offset` and `affine` stay reserved.
* `type_postfix` takes at most one `?`; `T??` is a type error, not a stack of
  options.
* Explicit type arguments on calls are the one way to name a type for a call
  (`decode<Message>(raw)`). The 1.1 `as: Message` named argument is deprecated.
* Resolution adds `core` as a reserved namespace. 1.1 reserved only `sys` and
  `std`.
* Block-level assignment is unchanged; the assignment target may now be a
  variable captured from an enclosing function (chapter 3).
* No literal syntax is added for `Map`, `Set` or `Bytes`; they are built by core
  functions and printed in the canonical text form (chapter 2).
* The `Quantity` literal forms and unit suffixes (`3.kWh`) are removed.

## Decided by

D1 (annotation rules reflected in function declarations), D3 (`T??` error),
D5 (type arguments, no overloading), D8 (units removed), D10 (assignment),
D11 (`pub use`), D12 (`native fn`, `core`; amended 2026-11-10: any module may
declare a native, see `docs/language/DECIDED.md`).
