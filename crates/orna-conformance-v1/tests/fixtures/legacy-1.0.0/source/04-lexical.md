# 4. Lexical structure and literal forms {#lexical}

## Source encoding

**ORNA-LEX-001** Orna source MUST be UTF-8.

**ORNA-LEX-002** Implementations MUST preserve enough source position information to report line, column and byte-span diagnostics.

## Comments

```orna
// line comment

/* block comment */
```

**ORNA-LEX-003** Line comments begin with `//` and end at the line ending.

**ORNA-LEX-004** Block comments use `/* ... */` and MUST support nesting; delimiters inside a string literal do not start comments.

## Identifiers and keywords

Identifiers are Unicode-aware but keywords are ASCII.

**ORNA-LEX-005** Implementations MUST compare identifiers using Unicode NFC normalization while preserving original spelling for display.

**ORNA-LEX-006** Namespace and definition-name comparison MUST be case-sensitive.

**ORNA-LEX-007** The exact keyword set is `as`, `assert`, `base`, `break`, `case`, `continue`, `dim`, `else`, `enum`, `false`, `fn`, `for`, `if`, `impl`, `in`, `let`, `loop`, `null`, `offset`, `affine`, `protocol`, `pub`, `return`, `self`, `static`, `table`, `true`, `type`, `unit`, `use`, and `while`. Literal markers such as T, Z and f are not standalone reserved identifiers. Contextual names follow the rules below.

Invalid declaration/control spellings receive the targeted diagnostics in the [diagnostic reference](#diagnostics). They are not additional valid grammar productions.

**ORNA-LEX-008** `Some` and payload-free option `null` are core option-value spellings. `Result`, `Ok`, and `Err` are not core 1.0 type or variant spellings. User enum variants remain namespace-qualified in patterns unless explicitly imported; a bare identifier pattern otherwise introduces a binding.

**ORNA-LEX-009** Multi-character operators use longest-token matching. `|?`, `??`, `||`, `=>`, `..=`, `<=`, `>=`, `==`, and `!=` are indivisible tokens. A standalone postfix `?` token has no expression meaning in 1.0. The contiguous source `???` begins with `??` and is invalid rather than being interpreted as propagation followed by coalescing.

## Core literals

```text
42
-18
3.1415
3.1415f
12.34.decimal
12.34.GBP
true
false
null
"hello"
"hello {name}"
2026-09-01
2026-09-01T14:30:00Z
[1, 2, 3]
{ name: "Alice", active: true }
```

**ORNA-LIT-001** Integer source numerals have arbitrary-precision parse semantics and are range-checked when checked against a bounded integer type.

**ORNA-LIT-002** An unsuffixed fractional or exponent numeral is represented exactly during parsing and defaults to `Decimal` when no expected numeric type exists. It MAY be checked against an expected `Float` type, in which case conversion to IEEE-754 binary64 uses correctly rounded round-to-nearest, ties-to-even semantics. The `f` suffix explicitly selects `Float`.

**ORNA-LIT-003** `.decimal` explicitly selects `Decimal`. Money and exact quantity contexts MUST NOT silently infer an intermediate binary `Float`.

**ORNA-LIT-004** String interpolation expressions use `{ expression }` within a double-quoted string.

**ORNA-LIT-005** Date and instant literals use ISO-8601-compatible forms. In a contiguous token sequence matching a complete date or instant literal, the lexer MUST recognize that literal before considering the characters as separate integer and subtraction tokens.

**ORNA-LIT-006** Decimal, Float, Date and Instant token classes MUST be disjoint after lexical classification; a conforming lexer MUST NOT emit two different tokenizations for the same complete source numeral.

**ORNA-LIT-007** `decimal_expression.CurrencyType` constructs exact `Money<CurrencyType>` only when the selected nominal type implements `Currency`; it is not a general implicit conversion or a special `currency` declaration.

## Collections, records and direct braced bodies

```orna
let xs = [1, 2, 3];
let person = {
    name: "Alice",
    emails: ["alice@example.com"],
};
```

Record field order is source-preserving for presentation but structural for type equality.

**ORNA-RECORD-001** A record literal field uses `name: expression`. Record-literal punning such as `{ name }` is not part of Orna.

**ORNA-RECORD-002** `{}` in ordinary expression position is the empty record.

**ORNA-RECORD-003** A free-standing block is not a general primary expression. Blocks occur only where the grammar explicitly accepts a block, including named function bodies, anonymous-function bodies, `case` arm bodies and control-flow bodies.

**ORNA-ARROW-001** Immediately after an anonymous-function arrow `=>`, a direct braced body is classified before its contents are parsed:

1. `{}` is an empty block whose value is `Unit`;
2. a non-empty body whose first top-level item begins `identifier:` is a record expression;
3. every other direct braced body is a block expression.

**ORNA-ARROW-002** The non-braced anonymous-function-body alternative MUST NOT begin with `{`. To return an empty record, source writes `() => ({})` for a zero-parameter anonymous function or `_ => ({})` where one parameter is required.

**ORNA-ARROW-003** The equivalent direct-brace classification applies after a `case` arm's `:` delimiter. Orna has no statement-label syntax, so `identifier:` at the beginning of a direct braced body is unambiguously a record field.

**ORNA-RECORD-004** A record literal used as the immediate condition or scrutinee expression of `if`, `while`, `for` or `case` MUST be parenthesized.

**ORNA-PATTERN-001** In a record pattern only, a bare field name is shorthand for a same-named binding: `{ count }` is exactly `{ count: count }`. This shorthand destructures an existing field; it does not construct a record and does not permit record-literal punning.

**ORNA-PATTERN-002** A record-pattern field written `name: pattern` matches field `name` and applies the nested pattern. Construction always uses explicit `name: expression`, while destructuring MAY use the shorthand from ORNA-PATTERN-001.

These forms are distinct:

```text
sample => {
    time: sample.time,
    value: sample.value,
}

sample => {
    let decoded = decode(sample);
    Reading.insert(decoded);
}

() => ({})

case result {
    SyncResult.ok { count }: { value: count },
    SyncResult.failed { reason, retryable: _ }: {
        log(reason);
        { value: fallback }
    },
}
```


## Contextual names and construction

An ordinary identifier begins with `_` or Unicode 16.0.0 `XID_Start`; subsequent characters are `_` or `XID_Continue`. Identifier equality uses NFC with that same Unicode data version. Keyword recognition follows normalisation and is ASCII and case-sensitive. The exact reserved set is the `contextual_name` keyword list in the [grammar](#grammar); literal delimiters such as `T`, `Z` and `f` are not reserved identifiers merely because a lexical production mentions them.

A reserved word may appear as a member name after `.`, an enum-variant name, a named-argument label or a record field where the grammar uses `contextual_name`. It is not an unrestricted local binding. `self` is separately admitted as an expression within a receiver context; it is not a module-global variable.

```text
self.value
codec.decode(raw, as: Message)
sys.ObjectKind.table
```

`Name { field: value }` constructs a nominal record type or qualified enum payload. An unqualified type name and a qualified name follow the same construction rule. Resolution must identify a constructible type/variant; a value with that name does not gain a call operation merely because braces follow it. Field visibility, required fields and refinements are checked before the constructed value escapes.

A nominal constructor used immediately before the body delimiter of `if`, `while`, `for` or `case` must be parenthesised. In those control-header positions an unparenthesised `{` begins the control body, not a constructor. This removes ambiguity without changing ordinary `EmailAddress { value: raw }` expressions elsewhere.

**ORNA-SYNTAX-001** Construction MUST check each supplied field once in written order, reject duplicates/unknown fields, apply defaults once and enforce visibility and refinements. Private fields cannot be bypassed by constructing a record with matching names.

## Tuples and function types

`(value)` groups an expression; `(value,)` is a one-element tuple. `(a, b)` is a two-element tuple. `()` is the sole value of `Unit`, also the zero-element tuple. The corresponding type forms are `(T,)`, `(T, U)` and `()`; `()` and `Unit` name the same type. Tuple elements are immutable values, are evaluated left to right and are compared componentwise in position order when their component operations are defined.

A tuple pattern uses the same comma distinction. It must have the exact arity of the matched tuple. Parentheses followed by `=>` introduce a lambda parameter list, as specified by the parser lookahead rule; they do not construct a tuple first.

```orna
pub fn pair(name: Str, count: Int): (Str, Int) = (name, count);
pub fn singleton(value: Int): (Int,) = (value,);
```

A function type is `fn(T1, T2): R`; a zero-argument function type is `fn(): R`. Parameter labels/defaults belong to the declaration, not to this structural callable type. Calls through an unnamed structural function type use positional arguments. Failure information remains a separate inferred channel, not an extra result wrapper.

## Numeric and string token boundaries

An underscore is allowed only between two digits of the same numeral part. Leading, trailing and doubled separators are invalid. A hexadecimal or binary prefix must be followed by a digit; its underscores cannot replace that first digit. Exponent signs are part of the exponent; other leading signs are unary operators.

Calendar literals use the proleptic Gregorian calendar, years 0001–9999 and valid month/day combinations. Instant source literals require a full offset or `Z`, at most nine fractional second digits and seconds in 00–59. They are normalised to an absolute UTC instant; leap-second notation is rejected rather than silently rounded. A literal that is lexically date-shaped but calendar-invalid is a literal diagnostic, not subtraction.

String escapes are `\"`, `\\`, `\n`, `\r`, `\t`, `\0` and `\u{hex}`. A Unicode escape contains one to six hexadecimal digits naming a Unicode scalar, not a surrogate or value above U+10FFFF. `\u{7b}` writes a literal opening brace without starting interpolation. Interpolation expressions use the same lexer and nested-delimiter rules as ordinary source. Literal string data is not NFC-normalised; normalisation of identifiers does not rewrite user strings.

**ORNA-SYNTAX-002** A processor MUST use the pinned lexical profile, validate calendar/scalar values after tokenisation, and report byte spans without reinterpreting malformed tokens as another expression.

