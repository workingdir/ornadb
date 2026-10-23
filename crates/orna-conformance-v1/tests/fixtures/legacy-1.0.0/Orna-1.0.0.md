---
title: Orna
subtitle: Language and runtime specification
lang: en-GB
---

# 1. About this specification {#scope}

Orna is a statically typed language for programs whose code, relational data and committed history share a Git repository. A local embedded runtime owns transactions and resumable execution. Git commits identify committed database snapshots; the logical current working database, **CWD**, also includes durable changes not yet published into Git.

This specification defines the language, repository semantics, embedded runtime, system interface, storage and live-presentation profiles. It does not prescribe a compiler implementation language, a user-interface toolkit or a hosting provider.

## 1.1 Conventions {#scope-document-status}

The specification comprises numbered requirements, algorithms, grammar productions, API schemas and format definitions. Examples and implementation notes are informative.

Examples specify their module context and dependencies. Signature tables use reference notation for intrinsic interfaces. Listings marked `text` contain signatures, placeholders or transcripts; listings marked `orna` contain source code.

## 1.2 Using this document {#scope-using-this-document}

The chapters progress from the language and data model to execution, repositories and interfaces. The table below provides entry points by topic. Requirement identifiers and API names link to their definitions; the [grammar](#grammar) and [diagnostic reference](#diagnostics) provide syntax and error lookups.

| To understand… | Start here | Continue with… |
|---|---|---|
| How to read and write a program | [Source and modules](#source-modules) | [Types](#types), [functions and expressions](#expressions), [worked examples](#examples) |
| What an update commits or rolls back | [Transactions and tasks](#execution) | [Assertions](#tables), [checkpoints](#checkpoints) |
| How branches relate to pending data | [Repository state](#repository) | [Branching](#branching), [publication](#publication) |
| How a value reaches disk | [Tables](#tables) | [Storage](#storage), [canonical formats](#formats) |
| How to inspect or control a runtime | [System operations](#system) | [System reference](#system-reference), [administration](#administration) |
| How a client follows live results | [Presentation](#presentation) | [Pages](#pages), [live protocol](#protocol) |
| How to implement and test a processor | [Grammar](#grammar) | [Conformance evidence](#conformance), [diagnostics](#diagnostics) |

## 1.3 Normative language and consistency {#scope-normative-language-and-consistency}

The words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY** and **OPTIONAL** have the meanings specified by [BCP 14](#ref-bcp14) only when capitalised. Lowercase prose uses those words in their ordinary sense.

Numbered requirements, named algorithms, grammar productions and machine-readable schemas are normative and apply together. Informative notes and examples do not override them. Conflicting normative provisions are specification defects, not implementation-defined alternatives.

Diagnostic codes identify primary error classifications. Implementations may add explanatory notes and secondary source spans without changing which forms the language accepts.

## 1.4 Conformance classes {#scope-conformance-classes}

| Class | Required scope |
|---|---|
| Language processor | Lexing, parsing, resolution, static inference/checking, ordinary evaluation, failures and assertions. |
| Repository engine | Logical CWD, Git snapshots, staging, branching, publication, identity, merge and storage validity. |
| Runtime | Activation transactions, owned tasks, streams, durable checkpoints, failure recovery and system observations. |
| CLI | The specified Git-compatible commands, Orna commands, diagnostics and machine-readable status. |
| REPL | Session module, safe inspection, completion contexts, session-owned work and presentation. |
| Server | Session establishment, trusted remote evaluation, page actions, live protocol and request recovery. |
| Renderer | Presentation tree, deterministic patch application, unsupported-node fallback and bounded rendering. |
| Codec | Typed encode/decode and the claimed canonical or external format profile. |
| Connector | Declared delivery, source identity, positions, replay, cancellation and preservation behaviour. |
| Storage profile | The logical table contract and every required encoding, integrity and recovery rule of the claimed profile. |

<span id="ORNA-CONF-001"></span>**ORNA-CONF-001** A conformance claim MUST identify its classes, implementation version and exact specification publication digest.

<span id="ORNA-CONF-002"></span>**ORNA-CONF-002** A claim MUST distinguish supported optional profiles from mandatory facilities and list the implementation tests actually executed.

<span id="ORNA-CONF-003"></span>**ORNA-CONF-003** An implementation MUST pass every applicable conformance fixture and behavioural test before claiming the corresponding class. A parsed example is not proof of type correctness or runtime behaviour.

<span id="ORNA-CONF-004"></span>**ORNA-CONF-004** Extensions MUST NOT change the meaning of valid source, repository trees or protocol messages in the claimed profile.

<span id="ORNA-CONF-005"></span>**ORNA-CONF-005** A language-processor claim MUST reject the invalid forms in the diagnostic corpus with the specified primary diagnostic class where the form can be identified.

<span id="ORNA-CONF-006"></span>**ORNA-CONF-006** A full-runtime claim MUST execute the assertion, automatic-failure, recovery, conversion, transaction, cancellation and cross-table validation scenarios applicable to it.

<span id="ORNA-CLOSURE-004"></span>**ORNA-CLOSURE-004** Configuration options MUST NOT change the language, identity, transaction, checkpoint or repository semantics specified here. Resource limits and presentation preferences may vary only where explicitly permitted and must remain observable.

## 1.5 Boundaries {#scope-boundaries}

Distributed transactions across independent databases, exactly-once effects in arbitrary external services, automatic resolution of all semantic merge conflicts and enterprise authorisation are outside this specification. Trusted-process deployments enforce the specified type safety, privacy boundaries, cancellation, integrity checks and destructive-operation preconditions.




# 2. Concepts and boundaries {#concepts}

Orna combines a typed language, relational data and Git-backed database history. The following concepts describe the relationships between source declarations, stored data and running programs.

## 2.1 The four states of a worktree {#concepts-the-four-states-of-a-worktree}

A database worktree has a committed `HEAD`, an index containing staged changes, ordinary unstaged files, and durable local runtime changes. The **CWD** is their reconciled logical database view, distinct from the process's filesystem current directory. Runtime changes can be queryable and durable before they appear in a Git commit.

```text
source modules ───→ resolved declarations ───→ typed program
                          │                       │
committed snapshots ──────┼──→ logical CWD ←── activations
                          │         │
                          │         └──→ durable local transactions
                          │                       │
                          └──────── publication ──┴──→ Git history
```

[Repository state](#repository) defines these layers precisely. [Branching](#branching) preserves ordinary Git behaviour; [publication](#publication) describes how local runtime data becomes committed while preserving staged and unstaged changes.

## 2.2 Values, declarations and relations {#concepts-values-declarations-and-relations}

A **value** has a static type. A **nominal type** has an identity distinct from its representation; a **transparent alias** does not. A **refined type** is nominal and validates its base value through declaration-owned assertions. Optional values use `Some(value)` and `null`.

A **module unit** contains declarations. A **row unit** contains one schema-directed record body. Module reachability begins at root `main.orna`; a recursive filesystem scan is not the program. See [source and modules](#source-modules) and [tables](#tables).

A **table** stores keyed rows. A **relation** is a typed, possibly lazy collection over which queries operate. A table is ordered by its canonical primary key, whereas a derived relation has only the order guaranteed by its operations. An **assertion** is an always-enforced proposition, not a debug-only test. See [relations](#relations) for observation and ordering.

## 2.3 Execution and progress {#concepts-execution-and-progress}

An **activation** is a root execution/atomicity boundary: one submitted input, a page action, a direct finite run, a child activation or a stream callback. Synchronous helper calls share their activation. A **task** is scheduled work with an activation or session owner. A **run** is a separately launched program; a **session** can own work across multiple prompt inputs.

When an owner ends, unfinished children are cancelled and joined. Cancellation is not an ordinary failure caught by `|?`. Open Orna transactions roll back; already committed work remains. See [execution](#execution).

A **stream** produces ordered items, possibly without end. A **consumer identity** identifies the resumable processing configuration, not only a function name. A **checkpoint** records the provider position from which that consumer safely resumes. Each processed item/batch and its checkpoint advance are committed together. A **failed delivery** has one stable identity across retries. See [streams](#streams) and [checkpoints](#checkpoints).

## 2.4 Identity and snapshots {#concepts-identity-and-snapshots}

An **ObjectId** identifies a declaration across ordinary edits and renames. A **revision** identifies one immutable meaning of that declaration. A **snapshot** identifies an exact database state: either a committed state or a pinned local CWD generation. A moving selector such as a branch name is resolved to a snapshot before it can be used as a stable reference.

A relation's `as_of` selects historical data and its decoding schema. Whole-program historical evaluation additionally selects historical code and dependencies. The distinction is observable; see [historical queries](#relations) and [system references](#system).

## 2.5 Presentation and encoding {#concepts-presentation-and-encoding}

**Inspect** provides structural developer output. **Display** provides human-facing text. **Present** provides a typed presentation tree for tables, trees, charts and other renderings. A **codec** encodes or decodes values in a specified format. Changing a formatter does not change equality, stored values, canonical bytes or Git identity. See [presentation](#presentation) and [canonical values](#formats).

The `sys` namespace is the mandatory typed introspection and control plane. The `std` namespace is optional ordinary library code, selected and pinned like other dependencies. The [intrinsic environment](#standard-library) remains sufficient for core integrity and execution without an optional library package.



# 3. Source, modules and namespaces {#source-modules}

## 3.1 File extension {#source-modules-file-extension}

Module units and loose row units both use `.orna` because both contain Orna syntax.

<span id="ORNA-SOURCE-001"></span>**ORNA-SOURCE-001** The parser MUST expose distinct entrypoints for module units and row units.

<span id="ORNA-SOURCE-002"></span>**ORNA-SOURCE-002** A module unit MUST contain declarations only at top level.

<span id="ORNA-SOURCE-003"></span>**ORNA-SOURCE-003** A row unit MUST contain exactly one record expression and MUST NOT contain declarations.

## 3.2 Namespace mapping {#source-modules-namespace-mapping}

Filesystem structure defines module namespaces.

```text
sensors/greenhouse/main.orna       -> sensors.greenhouse
sensors/greenhouse/input.orna -> sensors.greenhouse.input
```

<span id="ORNA-NS-001"></span>**ORNA-NS-001** A directory's `main.orna` MUST define that directory's namespace.

<span id="ORNA-NS-002"></span>**ORNA-NS-002** A non-`main.orna` module filename MUST add its stem as the final namespace component.

<span id="ORNA-NS-003"></span>**ORNA-NS-003** The repository root `main.orna` MUST define the root namespace.

<span id="ORNA-NS-004"></span>**ORNA-NS-004** A repository MUST NOT contain both `x.orna` and `x/main.orna` where both would define the same namespace.

## 3.3 Reserved namespaces {#source-modules-reserved-namespaces}

<span id="ORNA-NS-005"></span>**ORNA-NS-005** `sys` and `std` are the only reserved top-level namespaces.

<span id="ORNA-NS-006"></span>**ORNA-NS-006** `sys` MUST be built into every conforming implementation and MUST NOT be replaced or shadowed.

<span id="ORNA-NS-007"></span>**ORNA-NS-007** `std` MUST remain optional and replaceable, although its top-level name is reserved when present.

<span id="ORNA-NS-008"></span>**ORNA-NS-008** Source/module/table path components MUST be NFC and siblings MUST be unique under Unicode 16.0.0 toNFKC_Casefold. The host-independent check applies during load, rename and merge; a filesystem that distinguishes colliding names does not make them portable.

## 3.4 Imports {#source-modules-imports}

Valid forms:

```orna
use directory;
use sensors.greenhouse as climate;
use sensors.greenhouse.*;
use sensors.greenhouse.{Reading, recent};
use std.prelude as _;
```

<span id="ORNA-IMPORT-001"></span>**ORNA-IMPORT-001** `use a.b;` MUST make namespace `a.b` available under its final component unless it conflicts.

<span id="ORNA-IMPORT-002"></span>**ORNA-IMPORT-002** `use a.b as x;` MUST make the imported namespace available as `x`.

<span id="ORNA-IMPORT-003"></span>**ORNA-IMPORT-003** `use a.b.*;` MUST import all public names from `a.b` into the current lexical scope.

<span id="ORNA-IMPORT-004"></span>**ORNA-IMPORT-004** `use a.b.{X, y};` MUST import only the named public definitions.

<span id="ORNA-IMPORT-005"></span>**ORNA-IMPORT-005** `use x as _;` MUST import the prelude set explicitly exported by that pinned module, without binding a namespace alias. An absent prelude declaration imports no names; it does not mean wildcard import.

<span id="ORNA-IMPORT-006"></span>**ORNA-IMPORT-006** Importing a module MUST NOT execute arbitrary code, open a network connection or start a stream.

<span id="ORNA-IMPORT-007"></span>**ORNA-IMPORT-007** Wildcard imports are valid in committed modules and the REPL under the same rules. Local declarations and explicit imports take precedence; two wildcard imports that expose the same otherwise-unresolved name produce an ambiguity diagnostic. Import order MUST NOT select a winner.



# 4. Lexical structure and literal forms {#lexical}

## 4.1 Source encoding {#lexical-source-encoding}

<span id="ORNA-LEX-001"></span>**ORNA-LEX-001** Orna source MUST be UTF-8.

<span id="ORNA-LEX-002"></span>**ORNA-LEX-002** Implementations MUST preserve enough source position information to report line, column and byte-span diagnostics.

## 4.2 Comments {#lexical-comments}

```orna
// line comment

/* block comment */
```

<span id="ORNA-LEX-003"></span>**ORNA-LEX-003** Line comments begin with `//` and end at the line ending.

<span id="ORNA-LEX-004"></span>**ORNA-LEX-004** Block comments use `/* ... */` and MUST support nesting; delimiters inside a string literal do not start comments.

## 4.3 Identifiers and keywords {#lexical-identifiers-and-keywords}

Identifiers are Unicode-aware but keywords are ASCII.

<span id="ORNA-LEX-005"></span>**ORNA-LEX-005** Implementations MUST compare identifiers using Unicode NFC normalization while preserving original spelling for display.

<span id="ORNA-LEX-006"></span>**ORNA-LEX-006** Namespace and definition-name comparison MUST be case-sensitive.

<span id="ORNA-LEX-007"></span>**ORNA-LEX-007** The exact keyword set is `as`, `assert`, `base`, `break`, `case`, `continue`, `dim`, `else`, `enum`, `false`, `fn`, `for`, `if`, `impl`, `in`, `let`, `loop`, `null`, `offset`, `affine`, `protocol`, `pub`, `return`, `self`, `static`, `table`, `true`, `type`, `unit`, `use`, and `while`. Literal markers such as T, Z and f are not standalone reserved identifiers. Contextual names follow the rules below.

Invalid declaration/control spellings receive the targeted diagnostics in the [diagnostic reference](#diagnostics). They are not additional valid grammar productions.

<span id="ORNA-LEX-008"></span>**ORNA-LEX-008** `Some` and payload-free option `null` are core option-value spellings. `Result`, `Ok`, and `Err` are not core 1.0 type or variant spellings. User enum variants remain namespace-qualified in patterns unless explicitly imported; a bare identifier pattern otherwise introduces a binding.

<span id="ORNA-LEX-009"></span>**ORNA-LEX-009** Multi-character operators use longest-token matching. `|?`, `??`, `||`, `=>`, `..=`, `<=`, `>=`, `==`, and `!=` are indivisible tokens. A standalone postfix `?` token has no expression meaning in 1.0. The contiguous source `???` begins with `??` and is invalid rather than being interpreted as propagation followed by coalescing.

## 4.4 Core literals {#lexical-core-literals}

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

<span id="ORNA-LIT-001"></span>**ORNA-LIT-001** Integer source numerals have arbitrary-precision parse semantics and are range-checked when checked against a bounded integer type.

<span id="ORNA-LIT-002"></span>**ORNA-LIT-002** An unsuffixed fractional or exponent numeral is represented exactly during parsing and defaults to `Decimal` when no expected numeric type exists. It MAY be checked against an expected `Float` type, in which case conversion to IEEE-754 binary64 uses correctly rounded round-to-nearest, ties-to-even semantics. The `f` suffix explicitly selects `Float`.

<span id="ORNA-LIT-003"></span>**ORNA-LIT-003** `.decimal` explicitly selects `Decimal`. Money and exact quantity contexts MUST NOT silently infer an intermediate binary `Float`.

<span id="ORNA-LIT-004"></span>**ORNA-LIT-004** String interpolation expressions use `{ expression }` within a double-quoted string.

<span id="ORNA-LIT-005"></span>**ORNA-LIT-005** Date and instant literals use ISO-8601-compatible forms. In a contiguous token sequence matching a complete date or instant literal, the lexer MUST recognize that literal before considering the characters as separate integer and subtraction tokens.

<span id="ORNA-LIT-006"></span>**ORNA-LIT-006** Decimal, Float, Date and Instant token classes MUST be disjoint after lexical classification; a conforming lexer MUST NOT emit two different tokenizations for the same complete source numeral.

<span id="ORNA-LIT-007"></span>**ORNA-LIT-007** `decimal_expression.CurrencyType` constructs exact `Money<CurrencyType>` only when the selected nominal type implements `Currency`; it is not a general implicit conversion or a special `currency` declaration.

## 4.5 Collections, records and direct braced bodies {#lexical-collections-records-and-direct-braced-bodies}

```orna
let xs = [1, 2, 3];
let person = {
    name: "Alice",
    emails: ["alice@example.com"],
};
```

Record field order is source-preserving for presentation but structural for type equality.

<span id="ORNA-RECORD-001"></span>**ORNA-RECORD-001** A record literal field uses `name: expression`. Record-literal punning such as `{ name }` is not part of Orna.

<span id="ORNA-RECORD-002"></span>**ORNA-RECORD-002** `{}` in ordinary expression position is the empty record.

<span id="ORNA-RECORD-003"></span>**ORNA-RECORD-003** A free-standing block is not a general primary expression. Blocks occur only where the grammar explicitly accepts a block, including named function bodies, anonymous-function bodies, `case` arm bodies and control-flow bodies.

<span id="ORNA-ARROW-001"></span>**ORNA-ARROW-001** Immediately after an anonymous-function arrow `=>`, a direct braced body is classified before its contents are parsed:

1. `{}` is an empty block whose value is `Unit`;
2. a non-empty body whose first top-level item begins `identifier:` is a record expression;
3. every other direct braced body is a block expression.

<span id="ORNA-ARROW-002"></span>**ORNA-ARROW-002** The non-braced anonymous-function-body alternative MUST NOT begin with `{`. To return an empty record, source writes `() => ({})` for a zero-parameter anonymous function or `_ => ({})` where one parameter is required.

<span id="ORNA-ARROW-003"></span>**ORNA-ARROW-003** The equivalent direct-brace classification applies after a `case` arm's `:` delimiter. Orna has no statement-label syntax, so `identifier:` at the beginning of a direct braced body is unambiguously a record field.

<span id="ORNA-RECORD-004"></span>**ORNA-RECORD-004** A record literal used as the immediate condition or scrutinee expression of `if`, `while`, `for` or `case` MUST be parenthesized.

<span id="ORNA-PATTERN-001"></span>**ORNA-PATTERN-001** In a record pattern only, a bare field name is shorthand for a same-named binding: `{ count }` is exactly `{ count: count }`. This shorthand destructures an existing field; it does not construct a record and does not permit record-literal punning.

<span id="ORNA-PATTERN-002"></span>**ORNA-PATTERN-002** A record-pattern field written `name: pattern` matches field `name` and applies the nested pattern. Construction always uses explicit `name: expression`, while destructuring MAY use the shorthand from ORNA-PATTERN-001.

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


## 4.6 Contextual names and construction {#lexical-contextual-names-and-construction}

An ordinary identifier begins with `_` or Unicode 16.0.0 `XID_Start`; subsequent characters are `_` or `XID_Continue`. Identifier equality uses NFC with that same Unicode data version. Keyword recognition follows normalisation and is ASCII and case-sensitive. The exact reserved set is the `contextual_name` keyword list in the [grammar](#grammar); literal delimiters such as `T`, `Z` and `f` are not reserved identifiers merely because a lexical production mentions them.

A reserved word may appear as a member name after `.`, an enum-variant name, a named-argument label or a record field where the grammar uses `contextual_name`. It is not an unrestricted local binding. `self` is separately admitted as an expression within a receiver context; it is not a module-global variable.

```text
self.value
codec.decode(raw, as: Message)
sys.ObjectKind.table
```

`Name { field: value }` constructs a nominal record type or qualified enum payload. An unqualified type name and a qualified name follow the same construction rule. Resolution must identify a constructible type/variant; a value with that name does not gain a call operation merely because braces follow it. Field visibility, required fields and refinements are checked before the constructed value escapes.

A nominal constructor used immediately before the body delimiter of `if`, `while`, `for` or `case` must be parenthesised. In those control-header positions an unparenthesised `{` begins the control body, not a constructor. This removes ambiguity without changing ordinary `EmailAddress { value: raw }` expressions elsewhere.

<span id="ORNA-SYNTAX-001"></span>**ORNA-SYNTAX-001** Construction MUST check each supplied field once in written order, reject duplicates/unknown fields, apply defaults once and enforce visibility and refinements. Private fields cannot be bypassed by constructing a record with matching names.

## 4.7 Tuples and function types {#lexical-tuples-and-function-types}

`(value)` groups an expression; `(value,)` is a one-element tuple. `(a, b)` is a two-element tuple. `()` is the sole value of `Unit`, also the zero-element tuple. The corresponding type forms are `(T,)`, `(T, U)` and `()`; `()` and `Unit` name the same type. Tuple elements are immutable values, are evaluated left to right and are compared componentwise in position order when their component operations are defined.

A tuple pattern uses the same comma distinction. It must have the exact arity of the matched tuple. Parentheses followed by `=>` introduce a lambda parameter list, as specified by the parser lookahead rule; they do not construct a tuple first.

```orna
pub fn pair(name: Str, count: Int): (Str, Int) = (name, count);
pub fn singleton(value: Int): (Int,) = (value,);
```

A function type is `fn(T1, T2): R`; a zero-argument function type is `fn(): R`. Parameter labels/defaults belong to the declaration, not to this structural callable type. Calls through an unnamed structural function type use positional arguments. Failure information remains a separate inferred channel, not an extra result wrapper.

## 4.8 Numeric and string token boundaries {#lexical-numeric-and-string-token-boundaries}

An underscore is allowed only between two digits of the same numeral part. Leading, trailing and doubled separators are invalid. A hexadecimal or binary prefix must be followed by a digit; its underscores cannot replace that first digit. Exponent signs are part of the exponent; other leading signs are unary operators.

Calendar literals use the proleptic Gregorian calendar, years 0001–9999 and valid month/day combinations. Instant source literals require a full offset or `Z`, at most nine fractional second digits and seconds in 00–59. They are normalised to an absolute UTC instant; leap-second notation is rejected rather than silently rounded. A literal that is lexically date-shaped but calendar-invalid is a literal diagnostic, not subtraction.

String escapes are `\"`, `\\`, `\n`, `\r`, `\t`, `\0` and `\u{hex}`. A Unicode escape contains one to six hexadecimal digits naming a Unicode scalar, not a surrogate or value above U+10FFFF. `\u{7b}` writes a literal opening brace without starting interpolation. Interpolation expressions use the same lexer and nested-delimiter rules as ordinary source. Literal string data is not NFC-normalised; normalisation of identifiers does not rewrite user strings.

<span id="ORNA-SYNTAX-002"></span>**ORNA-SYNTAX-002** A processor MUST use the pinned lexical profile, validate calendar/scalar values after tokenisation, and report byte spans without reinterpreting malformed tokens as another expression.



# 5. Types, numbers and nominal values {#types}

## 5.1 Core type forms {#types-core-type-forms}

The following forms belong to the intrinsic type environment. Context and presentation interfaces are defined in their owning chapters; this list is not permission to introduce unversioned intrinsic types.

```text
Unit
Bool
Int
Float
Decimal
Str
Blob
Path
Digest
Uuid
Date
Instant
LocalDateTime
TimeOfDay
Duration
TimeZone
ZonedDateTime
Error
Range<T>
List<T>       written [T]
Option<T>     written T?
Relation<T>
Query<T>
Predicate<T>
Stream<T>
Money<C>
Quantity<N, U> or surface forms such as Float<kWh>
record types
function types
nominal enum types
nominal user types
refined nominal types
nominal table row types
stored table-reference types
protocol-constrained generic types
```

<span id="ORNA-TYPE-001"></span>**ORNA-TYPE-001** `T?` MUST mean `Option<T>` and MUST NOT mean an unchecked nullable value.

<span id="ORNA-TYPE-002"></span>**ORNA-TYPE-002** A failable expression has a successful value type and a separate abrupt failure channel carrying an `Error`; failure is not represented by `Result<T, E>` in source.

<span id="ORNA-TYPE-003"></span>**ORNA-TYPE-003** Automatic failure propagation MUST NOT silently discard a stream item. Stream retry, preservation, skip and dead-letter behavior remains explicit through the stream/checkpoint model.

<span id="ORNA-TYPE-004"></span>**ORNA-TYPE-004** Implementations MUST NOT use the same type name for raw bytes and a byte-count quantity.

<span id="ORNA-TYPE-005"></span>**ORNA-TYPE-005** User-defined closed variants use nominal `enum` declarations. Orna does not use the expression-pipeline token `|` as a type-union or variant-list operator.

<span id="ORNA-TYPE-006"></span>**ORNA-TYPE-006** `Result`, `Ok`, and `Err` are not reserved core type constructors or variants in 1.0.

<span id="ORNA-TYPE-007"></span>**ORNA-TYPE-007** A user-defined `type` declaration is either a transparent alias, a nominal type, or a refined nominal type as specified in [aliases, nominal types and refinements](#expressions).

<span id="ORNA-TYPE-008"></span>**ORNA-TYPE-008** The absence of a source annotation does not make a value dynamically typed. A conforming processor MUST infer a static type or issue a type-inference diagnostic.

## 5.2 Value and binding model {#types-value-and-binding-model}

Orna values are immutable. A local name introduced by `let` is a function-local slot whose binding may be reassigned by an assignment statement; reassignment replaces the entire immutable value and does not expose shared mutable object identity.

<span id="ORNA-VALUE-001"></span>**ORNA-VALUE-001** Primitive, list, record, enum, option, quantity, money, nominal and row values MUST have value semantics.

<span id="ORNA-VALUE-002"></span>**ORNA-VALUE-002** A row returned from a query is an immutable snapshot containing its table identity, primary key, snapshot context and logical fields. A later table update MUST NOT mutate an already-held row value.

<span id="ORNA-VALUE-003"></span>**ORNA-VALUE-003** Closures capture values at creation time. Table handles retain their database/snapshot context unless the program explicitly selects another context.

<span id="ORNA-VALUE-004"></span>**ORNA-VALUE-004** Cyclic ordinary list/record/nominal values are not constructible in version 1.0. Persistent graph cycles may exist through table references.

<span id="ORNA-VALUE-005"></span>**ORNA-VALUE-005** Memory management, pointer identity and finalization timing MUST NOT be observable language semantics.

<span id="ORNA-VALUE-006"></span>**ORNA-VALUE-006** Reassigning a `let` binding MUST NOT mutate any value previously captured, returned, stored or observed through another binding.

<span id="ORNA-VALUE-007"></span>**ORNA-VALUE-007** `var` is not a 1.0 binding declaration. A processor that recognizes the prohibited declaration context MUST emit `ORNA091-E-VAR` and propose `let`.

## 5.3 Numeric types and exact decimals {#types-numeric-types-and-exact-decimals}

<span id="ORNA-NUM-001"></span>**ORNA-NUM-001** `Int` MUST represent exact integers.

<span id="ORNA-NUM-002"></span>**ORNA-NUM-002** `Decimal` MUST represent finite exact base-10 values with implementation resource limits but without binary-float conversion.

<span id="ORNA-NUM-003"></span>**ORNA-NUM-003** `Float` MUST use IEEE-754 binary64 semantics and MUST NOT be used as the canonical representation of money.

<span id="ORNA-NUM-004"></span>**ORNA-NUM-004** Decimal addition, subtraction and multiplication MUST be exact unless an implementation resource limit is exceeded, in which case evaluation fails with a typed `Error`.

<span id="ORNA-NUM-005"></span>**ORNA-NUM-005** Decimal arithmetic MUST remain exact where the mathematical result has a finite decimal representation. Division whose result is not a finite decimal MUST fail unless an explicitly selected rounding/precision operation is used.

```orna
1.decimal.divide(
    3.decimal,
    scale: 6,
    rounding: half_even,
)
```

No process-global or ambient decimal precision changes the meaning of source text.

## 5.4 Float equality, sorting and aggregation {#types-float-equality-sorting-and-aggregation}

<span id="ORNA-FLOAT-001"></span>**ORNA-FLOAT-001** Ordinary Float equality and ordered comparison follow IEEE-754 binary64 semantics: `NaN != NaN`, every ordered comparison involving `NaN` is false, and `-0.0 == 0.0`.

<span id="ORNA-FLOAT-002"></span>**ORNA-FLOAT-002** Sorting Float values MUST use the IEEE 754 `totalOrder` predicate for binary64 values. In ascending order, negative NaNs precede negative infinity, finite negative values and `-0.0`; `-0.0` precedes `+0.0`; positive finite values and positive infinity follow; and positive NaNs come last. Signaling/quiet NaNs and distinct NaN payloads are ordered exactly as required by `totalOrder`.

<span id="ORNA-FLOAT-003"></span>**ORNA-FLOAT-003** `Float` MUST NOT be accepted as a primary-key component or default hash-key type.

<span id="ORNA-FLOAT-004"></span>**ORNA-FLOAT-004** Float arithmetic MUST NOT be silently reassociated or evaluated with unspecified fast-math transformations.

<span id="ORNA-FLOAT-005"></span>**ORNA-FLOAT-005** A Float aggregate MUST process rows in the relation's observable order. Empty `sum` returns additive zero; empty `min`, `max` and `mean` return `null`.

<span id="ORNA-FLOAT-006"></span>**ORNA-FLOAT-006** Float sorting order and ordinary equality are distinct relations. A sort MUST distinguish `-0.0` from `+0.0` and MAY distinguish NaN bit patterns even though ordinary equality treats the two zeros as equal and every NaN as unequal to every value, including itself.

<span id="ORNA-FLOAT-007"></span>**ORNA-FLOAT-007** For a non-empty Float input, `min` and `max` return the canonical NaN if any input is NaN. Otherwise they use numeric order, with `min` choosing `-0.0` when either zero sign is present and `max` choosing `+0.0`. Libraries MAY provide explicitly named finite-only or NaN-ignoring alternatives.

### 5.4.1 Algorithm FLOAT-TOTAL-1 {#types-algorithm-float-total-1}

For a binary64 value, interpret its exact 64 bits as an unsigned integer `bits` and compute an unsigned sortable key:

```text
if the sign bit is 1: key = bitwise_not(bits)
otherwise:             key = bits xor 0x8000000000000000
```

Compare keys as unsigned 64-bit integers. This is the normative bit-level implementation of the ordering required by `ORNA-FLOAT-002`; it orders every finite value, infinity, signed zero, signaling NaN, quiet NaN and NaN payload deterministically. The analogous binary32 algorithm uses the mask `0x80000000`. Implementations MAY use another algorithm only when every value pair produces the same order.

## 5.5 Blob and information quantity {#types-blob-and-information-quantity}

`Blob` is raw binary content. Storage size is a numeric quantity with an information unit.

```text
payload: Blob
used: Float<GiB>
```

## 5.6 Time model {#types-time-model}

`Instant` is an absolute point on the UTC timeline. `LocalDateTime` is civil time without a zone. `ZonedDateTime` is a local date-time plus an IANA time zone and resolved offset. `Duration` is elapsed time.

<span id="ORNA-TIME-001"></span>**ORNA-TIME-001** Telemetry timestamps SHOULD use `Instant`.

<span id="ORNA-TIME-002"></span>**ORNA-TIME-002** Calendar-day operations over `Instant` values MUST require or derive an explicit time zone.

Invalid:

```orna
readings | bucket_by(1.day)
```

when `readings.time` is an `Instant` and no zone can be derived.

Valid:

```orna
readings | bucket_by(1.day, zone: Europe.London)
```

<span id="ORNA-TIME-003"></span>**ORNA-TIME-003** Calendar-day bucketing MUST respect daylight-saving transitions; a local day MAY contain 23, 24 or 25 hours.

<span id="ORNA-TIME-004"></span>**ORNA-TIME-004** Fixed elapsed-time windows and calendar periods MUST be distinct operations even if their textual durations appear similar.

## 5.7 Ranges {#types-ranges}

`Range<T>` is an ordinary ordered span. It does not imply non-overlap or temporal uniqueness merely because it is stored in a table.

```text
1..5
2026-01-01..2027-01-01
start..
..end
1..=5
```

<span id="ORNA-RANGE-001"></span>**ORNA-RANGE-001** `a..b` constructs a half-open range containing values `x` for which `a <= x && x < b`.

<span id="ORNA-RANGE-002"></span>**ORNA-RANGE-002** `a..=b` constructs an inclusive range containing values `x` for which `a <= x && x <= b`.

<span id="ORNA-RANGE-003"></span>**ORNA-RANGE-003** Either endpoint MAY be omitted where the surrounding operation supplies a type and meaningful unbounded side.

<span id="ORNA-RANGE-004"></span>**ORNA-RANGE-004** Range endpoints MUST have one compatible ordered type. Empty ranges are valid ordinary values.

<span id="ORNA-RANGE-005"></span>**ORNA-RANGE-005** Ranges have a total order only when `T` has a total order: lower endpoint (unbounded first), then upper endpoint (unbounded last), then exclusivity before inclusivity at equal endpoints. This ordering is for sorting/serialization and does not imply overlap uniqueness.

<span id="ORNA-RANGE-006"></span>**ORNA-RANGE-006** `value in range` tests membership. Range iteration/slicing behavior is supplied by the relevant iterable/index operation rather than by special grammar.

<span id="ORNA-RANGE-007"></span>**ORNA-RANGE-007** Version 1.0 does not redefine primary-key equality as range overlap. Temporal exclusion/non-overlap constraints are represented by explicit table or cross-table assertions, not by making overlap masquerade as equality.

## 5.8 Dimensions and units {#types-dimensions-and-units}

```orna
pub dim Length;
pub dim Mass;
pub dim Time;
pub dim Temperature;
pub dim Speed = Length / Time;
pub dim Energy = Mass * Length^2 / Time^2;
pub dim Power = Energy / Time;

pub unit m : Length base;
pub unit kg : Mass base;
pub unit s : Time base;
pub unit K : Temperature base;
pub unit km : Length = 1000.m;
pub unit mile : Length = 1609.344.m;
pub unit min : Time = 60.s;
pub unit hour : Time = 60.min;
pub unit J : Energy = kg * m^2 / s^2;
pub unit kWh : Energy = 3.6e6.J;
pub unit mph : Speed = mile / hour;
pub unit C : Temperature = K offset 273.15 affine;
pub unit deltaC : Temperature = K;
```

<span id="ORNA-UNIT-001"></span>**ORNA-UNIT-001** Unit compatibility is determined from reduced dimension exponents, rational scale and affine behaviour, not from unit name alone.

<span id="ORNA-UNIT-002"></span>**ORNA-UNIT-002** Compatible units from attached databases interoperate when their structural definitions are equivalent.

<span id="ORNA-UNIT-003"></span>**ORNA-UNIT-003** Aggregation preserves dimensions according to the aggregate's algebra.

<span id="ORNA-UNIT-004"></span>**ORNA-UNIT-004** Adding quantities of incompatible dimensions is a compile-time error where types are known.

<span id="ORNA-UNIT-005"></span>**ORNA-UNIT-005** For an affine unit `U`, an absolute value and its corresponding linear delta quantity obey point/vector algebra:

```text
Absolute<U> - Absolute<U> -> Delta<U>
Absolute<U> + Delta<U>    -> Absolute<U>
Delta<U> + Absolute<U>    -> Absolute<U>
Absolute<U> + Absolute<U> -> error
scalar * Absolute<U>      -> error
scalar * Delta<U>         -> Delta<U>
```

```text
30.C - 20.C        // 10.deltaC
20.C + 5.deltaC    // 25.C
20.C + 5.C         // invalid
```

<span id="ORNA-UNIT-006"></span>**ORNA-UNIT-006** Integration reduces dimensions algebraically; integrating speed over time yields length.

<span id="ORNA-UNIT-007"></span>**ORNA-UNIT-007** Aggregation over affine absolute quantities follows this matrix:

- selection/order operations (`first`, `last`, `min`, `max`, `median`, percentile and mode) are permitted and return an absolute value;
- `sum` is invalid for absolute affine values and valid for delta values;
- non-empty `mean` is `base + mean(value - base)` for any selected base value in the input and returns an absolute value;
- `range` and standard deviation return the corresponding delta quantity;
- variance returns the square of the corresponding delta quantity;
- differences and derivatives operate on delta quantities;
- integration of an affine absolute quantity is invalid unless the caller first selects a meaningful linear reference.

<span id="ORNA-UNIT-008"></span>**ORNA-UNIT-008** Unit suffix resolution MUST be static and unambiguous in the current import context. A unit suffix does not perform an arbitrary `From` conversion.

## 5.9 Money and currencies {#types-money-and-currencies}

`Money<C>` is an exact decimal monetary amount whose currency is a nominal type implementing `Currency`.

```text
pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
}

pub type GBP {
    impl Currency {
        static code = "GBP";
        static minor_digits = 2;
    }
}

amount: Money<GBP>
rate: Money<GBP> / kWh
let price = 12.34.GBP;
```

<span id="ORNA-MONEY-001"></span>**ORNA-MONEY-001** Money MUST use exact decimal semantics.

<span id="ORNA-MONEY-002"></span>**ORNA-MONEY-002** Two amounts with different currency parameters MUST NOT be added without an explicit data-backed conversion.

<span id="ORNA-MONEY-003"></span>**ORNA-MONEY-003** Dimensional algebra MUST reduce an exact energy quantity such as `Decimal<kWh> * (Money<GBP> / kWh)` to `Money<GBP>`. A binary `Float` quantity MUST NOT enter an exact Money calculation without an explicit decimal conversion, scale and rounding rule.

<span id="ORNA-MONEY-004"></span>**ORNA-MONEY-004** A currency nominal type MUST implement `Currency` with stable static properties `code: Str` and `minor_digits: Int`. A currency symbol is deliberately not part of the protocol.

<span id="ORNA-MONEY-005"></span>**ORNA-MONEY-005** Currency conversion whose rate changes over time MUST require an effective instant or date and MUST be an explicit data-backed operation.

```orna
rates.convert(invoice.total, GBP, on: invoice.date)
```

<span id="ORNA-MONEY-006"></span>**ORNA-MONEY-006** Minor-unit precision controls conventional display and settlement quantization, not the internal precision of intermediate calculations.

<span id="ORNA-MONEY-007"></span>**ORNA-MONEY-007** Canonical JSON encoding of money SHOULD use a decimal string and currency code, for example `{ "amount": "12.34", "currency": "GBP" }`.

<span id="ORNA-MONEY-008"></span>**ORNA-MONEY-008** The grammar MUST NOT provide a special `currency` declaration. That invalid declaration receives `ORNA091-E-CURRENCY` with a rewrite to a nominal type and nested `Currency` implementation.

<span id="ORNA-MONEY-009"></span>**ORNA-MONEY-009** Formatting chooses symbols, placement, spacing, grouping and decimal punctuation from an explicit or activation-derived locale/format context. Presentation MUST NOT change money equality, storage, hashing or canonical serialization.

<span id="ORNA-MONEY-010"></span>**ORNA-MONEY-010** `12.34.GBP` constructs `Money<GBP>` exactly and MUST NOT parse through binary `Float`.

## 5.10 Inference-first static typing {#types-inference-first-static-typing}

The compiler infers omitted type annotations from literals, operators, calls, fields, control-flow joins, assignments, protocol selection, return paths and contextual expectations.

```orna
fn square(x) = x * x;

pub fn total(items: [LineItem]): Money<GBP> =
    items | map(item => item.price * item.quantity) | sum();
```

<span id="ORNA-INFER-001"></span>**ORNA-INFER-001** Omitted annotations MUST be inferred to one static type before evaluation.

<span id="ORNA-INFER-002"></span>**ORNA-INFER-002** Type inference MUST NOT silently fall back to a dynamic `Any` type.

<span id="ORNA-INFER-003"></span>**ORNA-INFER-003** Function parameter and return annotations are optional when the signature is unambiguously inferable.

<span id="ORNA-INFER-004"></span>**ORNA-INFER-004** Public functions SHOULD annotate externally meaningful parameter and return types, but absence of those annotations is not a syntax error when inference succeeds.

<span id="ORNA-INFER-005"></span>**ORNA-INFER-005** A compiler MUST expose the fully inferred exported signature through metadata, diagnostics and `sys.Function` exactly as if it had been written explicitly.

<span id="ORNA-INFER-006"></span>**ORNA-INFER-006** Inference MUST be independent of runtime data values and MUST produce the same signature for the same resolved source and dependency snapshot.

<span id="ORNA-INFER-007"></span>**ORNA-INFER-007** When inference is underconstrained or recursive constraints do not converge, the diagnostic MUST identify the smallest useful annotation site rather than demanding annotations everywhere.

<span id="ORNA-INFER-008"></span>**ORNA-INFER-008** Contextual numeric and collection inference MUST preserve the exactness rules of [numeric exactness](#types) and [literal forms](#lexical).

<span id="ORNA-INFER-009"></span>**ORNA-INFER-009** Protocol selection and nested implementations participate in inference, but overlapping implementations remain invalid.

<span id="ORNA-INFER-010"></span>**ORNA-INFER-010** Adding a redundant annotation MUST NOT change successful program behavior.

## 5.11 Transparent aliases and nominal types {#types-transparent-aliases-and-nominal-types}

The `type` declaration defines aliases, nominal types and refined types.

Transparent alias:

```orna
type UserId = Uuid;
```

Nominal type:

```orna
pub type EmailAddress {
    value: Str,

    impl From<Str> {
        fn from(value) {
            if !valid_email(value) {
                fail(error(
                    code: "email.invalid",
                    message: "invalid email address",
                ));
            }

            EmailAddress { value: value }
        }
    }

    impl Display {
        fn display(self, context) = self.value;
    }
}
```

<span id="ORNA-NOMINAL-001"></span>**ORNA-NOMINAL-001** `type Name = Existing;` declares a transparent alias with no new runtime or equality identity.

<span id="ORNA-NOMINAL-002"></span>**ORNA-NOMINAL-002** `type Name { ... }` declares a nominal type distinct from every structural record with the same fields.

<span id="ORNA-NOMINAL-003"></span>**ORNA-NOMINAL-003** `type Name = Base { ... }` declares a refined nominal type represented by `Base` and distinct from `Base`.

<span id="ORNA-NOMINAL-004"></span>**ORNA-NOMINAL-004** Orna 1.0 has no separate `opaque` type declaration.

<span id="ORNA-NOMINAL-005"></span>**ORNA-NOMINAL-005** A nominal field is private by default. Prefixing the field with `pub` exposes that field through the type's public representation.

<span id="ORNA-NOMINAL-006"></span>**ORNA-NOMINAL-006** Private fields are accessible to the owning type's nested implementations and inaccessible to unrelated modules.

<span id="ORNA-NOMINAL-007"></span>**ORNA-NOMINAL-007** Direct construction outside the owning type is permitted only when every supplied field is publicly constructible; otherwise callers use an exposed conversion or method.

<span id="ORNA-NOMINAL-008"></span>**ORNA-NOMINAL-008** Field visibility controls representation access, not value mutability. Nominal values remain immutable.

<span id="ORNA-NOMINAL-009"></span>**ORNA-NOMINAL-009** Nominal equality, order, hashing, display, presentation and codec behavior are supplied by explicit core rules or protocol implementations; structural coincidence alone does not grant them.

<span id="ORNA-NOMINAL-010"></span>**ORNA-NOMINAL-010** A formatter SHOULD keep nested implementations adjacent to the type whose representation they can access.

## 5.12 Refined types {#types-refined-types}

A refined type attaches always-enforced assertions to a base type:

```orna
type Meter = Int {
    assert >= 0;
    assert <= 100;
}
```

The declaration is read as “a `Meter` is an `Int` for which each proposition holds”. The candidate base value is the assertion owner subject; it is not exposed as a general source variable.

<span id="ORNA-REFINE-001"></span>**ORNA-REFINE-001** Constructing or converting into a refined type MUST evaluate every declaration assertion in source order.

<span id="ORNA-REFINE-002"></span>**ORNA-REFINE-002** A false refined-type assertion prevents construction and fails with a safe, source-linked assertion diagnostic.

<span id="ORNA-REFINE-003"></span>**ORNA-REFINE-003** A refined-type assertion MAY use a subjectless comparison such as `>= 0`, which elaborates to a predicate over the candidate base value.

<span id="ORNA-REFINE-004"></span>**ORNA-REFINE-004** The removed `where self` spelling is invalid and receives `ORNA-A091-001`.

<span id="ORNA-REFINE-005"></span>**ORNA-REFINE-005** Refined-type assertions are enforced in all build and runtime modes; they are not debug-only.

## 5.13 Explicit conversions {#types-explicit-conversions}

The target type owns its conversions through nested `From<Source>` implementations:

```orna
pub type EmailAddress {
    value: Str,

    impl From<Str> {
        fn from(value) {
            if !valid_email(value) {
                fail(error(code: "email.invalid", message: "invalid email"));
            }
            EmailAddress { value: value }
        }
    }

    impl From<EmailHeader> {
        fn from(header) = EmailAddress.from(header.address);
    }
}
```

<span id="ORNA-CONVERT-001"></span>**ORNA-CONVERT-001** `Target.from(source)` selects a `From<Source>` implementation nested in `Target`.

<span id="ORNA-CONVERT-002"></span>**ORNA-CONVERT-002** A nominal type MAY implement `From` for several distinct source types.

<span id="ORNA-CONVERT-003"></span>**ORNA-CONVERT-003** Two implementations whose source types overlap for the same target are invalid.

<span id="ORNA-CONVERT-004"></span>**ORNA-CONVERT-004** `From` is allowed to fail through the ordinary failure channel. Orna does not split the surface into `From` and `TryFrom`.

<span id="ORNA-CONVERT-005"></span>**ORNA-CONVERT-005** `TryFrom` is not a core 1.0 protocol and tooling SHOULD propose `From` when it recognizes the legacy design spelling.

<span id="ORNA-CONVERT-006"></span>**ORNA-CONVERT-006** Arbitrary `From` implementations are not applied implicitly. Source writes the conversion or uses a syntax whose semantics explicitly names the target, such as `12.34.GBP`.

<span id="ORNA-CONVERT-007"></span>**ORNA-CONVERT-007** A compiler MUST NOT discover or execute an implicit multi-step conversion chain.

<span id="ORNA-CONVERT-008"></span>**ORNA-CONVERT-008** Lossy, policy-dependent, locale-dependent or data-backed transformations MUST use a specifically named operation rather than a generic `From` implementation.

<span id="ORNA-CONVERT-009"></span>**ORNA-CONVERT-009** A conversion implementation may access private fields of its target because it is lexically owned by that target.

<span id="ORNA-CONVERT-010"></span>**ORNA-CONVERT-010** Conversion failure preserves the original error cause and aborts the activation unless recovered by `|?`.



## 5.14 Inference boundary algorithm {#types-inference-boundary-algorithm}

### 5.14.1 Algorithm INFER-1 {#types-algorithm-infer-1}

1. Parse declarations without inventing dynamic types for omitted annotations.
2. Create type variables for omitted parameter, local and return types.
3. Add constraints from literals, patterns, calls, operators, assignments, fields, case arms, returns, table schemas, protocol requirements and contextual types.
4. Resolve imported/exported signatures and nested implementation candidates from the captured dependency snapshot.
5. Solve constraints to a principal type where one exists.
6. Reject overlap, unsatisfied protocol bounds, incompatible assignments and ambiguous unresolved variables.
7. For recursion, iterate strongly connected declaration components; request a targeted annotation only when no stable principal solution exists.
8. Persist the inferred public signature and failure metadata in semantic catalogue output.

<span id="ORNA-INFER-011"></span>**ORNA-INFER-011** Inference errors MUST show the conflicting constraints and their source spans.

<span id="ORNA-INFER-012"></span>**ORNA-INFER-012** Refactoring a private helper from an explicit type to an equivalent inferred type MUST NOT change exported semantic identity by itself.

<span id="ORNA-INFER-013"></span>**ORNA-INFER-013** Semantic diffs SHOULD distinguish an annotation-only edit from an inferred signature change.

<span id="ORNA-INFER-014"></span>**ORNA-INFER-014** Historical execution uses the inferred signatures captured by the historical code snapshot, not signatures re-inferred from current source.

## 5.15 Nested implementation ownership {#types-nested-implementation-ownership}

A nested implementation's target is the immediately enclosing nominal type. There is no need to repeat it:

```orna
pub type Username {
    value: Str,

    impl Display {
        fn display(self, context) = self.value;
    }
}
```

<span id="ORNA-IMPL-002"></span>**ORNA-IMPL-002** A nested `impl P` semantically means implementation of protocol `P` for the immediately enclosing nominal type.

<span id="ORNA-IMPL-003"></span>**ORNA-IMPL-003** Nested implementations are not allowed inside a transparent alias because the alias has no separate nominal ownership.

<span id="ORNA-IMPL-004"></span>**ORNA-IMPL-004** A refined nominal type MAY contain nested implementations in the same body as its assertions.

<span id="ORNA-IMPL-005"></span>**ORNA-IMPL-005** Protocol implementation lookup is based on stable semantic identities, not merely source names.

<span id="ORNA-IMPL-006"></span>**ORNA-IMPL-006** Renaming a type through semantic rename preserves its nested implementation ownership.

## 5.16 Conversion selection algorithm {#types-conversion-selection-algorithm}

### 5.16.1 Algorithm CONVERT-1 {#types-algorithm-convert-1}

For explicit `Target.from(source)`:

1. Resolve `Target` to one nominal target type.
2. Infer/check the source's static type `S`.
3. Find nested implementations of `From<S'>` in `Target` for which `S` equals or validly instantiates `S'`.
4. Require exactly one most-specific non-overlapping implementation; specialization itself is not supported.
5. Invoke its `from` function.
6. If invocation fails, propagate normally. If it succeeds, require the target value type.
7. Do not search for an intermediate `U` and do not chain `Target.from(U.from(source))` implicitly.



# 6. Declarations, expressions and failure {#expressions}

## 6.1 Top-level declarations {#expressions-top-level-declarations}

Module units may declare imports, tables, functions, enums, protocols, dimensions, units, aliases, nominal/refined types, and constrained module assertions. Protocol implementations are nested in the nominal target type; currencies are ordinary types; there is no top-level `impl ... for ...` or `currency` declaration.

<span id="ORNA-MODULE-001"></span>**ORNA-MODULE-001** Arbitrary expression statements MUST NOT appear at module top level.

<span id="ORNA-MODULE-002"></span>**ORNA-MODULE-002** A module-scope `assert` is permitted only as the cross-table invariant form specified in [the tables chapter](#tables). It is not arbitrary module-load execution.

<span id="ORNA-MODULE-003"></span>**ORNA-MODULE-003** Loading source parses, resolves, infers and type-checks declarations and assertion plans; it never starts streams, performs external work or mutates current table state.

Invalid:

```orna
http.get("https://example.com");
```

Valid cross-table declaration:

```orna
assert every(Invoice, invoice =>
    exists(Customer, customer => customer.id == invoice.customer_id)
);
```

## 6.2 Functions and function values {#expressions-functions-and-function-values}

Expression body with inferred return type:

```orna
pub fn square(x: Int) = x * x;
```

Explicit return annotation uses `:`:

```orna
pub fn square(x: Int): Int = x * x;
```

Block body:

```orna
pub fn create_note(text: Str): Note {
    let note = Note.insert({
        created: now(),
        text: text,
    });

    note
}
```

Defaults:

```orna
pub fn recent(rows, duration = 7.days) =
    rows | filter(row => row.time >= now() - duration);
```

<span id="ORNA-FN-001"></span>**ORNA-FN-001** Calling a function MUST evaluate it at the time of the call against supplied values and the current database context.

<span id="ORNA-FN-002"></span>**ORNA-FN-002** A qualified function name without call parentheses denotes the function value and MUST NOT invoke it.

```text
messages.sync      // function value
messages.sync()    // invocation
```

<span id="ORNA-FN-003"></span>**ORNA-FN-003** Merely naming a function in the REPL MUST inspect the function and MUST NOT call it.

<span id="ORNA-FN-004"></span>**ORNA-FN-004** A function declaration MUST NOT imply persisted materialization of its result.

<span id="ORNA-FN-005"></span>**ORNA-FN-005** A function value has a statically known inferred or annotated parameter/return signature and may be stored in a local value, passed as an argument or returned from another function.

<span id="ORNA-FN-006"></span>**ORNA-FN-006** An explicit return annotation follows the parameter list as `: Type`.

<span id="ORNA-FN-007"></span>**ORNA-FN-007** `-> Type` is not function-return syntax in 1.0 and receives `ORNA091-E-RETURN-ARROW` with a mechanical `:` rewrite.

<span id="ORNA-FN-008"></span>**ORNA-FN-008** Parameters and return types MAY omit annotations where inference succeeds.

<span id="ORNA-FN-009"></span>**ORNA-FN-009** Every reachable return path and block tail contributes to return-type inference.

<span id="ORNA-FN-010"></span>**ORNA-FN-010** A function's inferred failure set is metadata on the callable and is not wrapped into its successful return type.

<span id="ORNA-FN-011"></span>**ORNA-FN-011** Merely declaring a function does not execute its body or its assertions.

<span id="ORNA-FN-012"></span>**ORNA-FN-012** Public functions SHOULD use explicit boundary annotations when those improve API stability, documentation or diagnostics; this is guidance, not a requirement to annotate ordinary code.

## 6.3 Enums and `case` {#expressions-enums-and-case}

```orna
pub enum SyncResult {
    ok { count: Int },
    failed { reason: Str, retryable: Bool },
}

pub fn count(result: SyncResult): Int =
    case result {
        SyncResult.ok { count }: count,
        SyncResult.failed { reason: _, retryable: _ }: 0,
    };
```

<span id="ORNA-ENUM-001"></span>**ORNA-ENUM-001** An enum is nominal and closed to variants declared in its body.

<span id="ORNA-ENUM-002"></span>**ORNA-ENUM-002** A payload-free variant is referenced as `Enum.variant`.

<span id="ORNA-ENUM-003"></span>**ORNA-ENUM-003** A payload variant uses a record payload and is constructed with explicit named fields, for example `SyncResult.ok { count: 3 }`. Destructuring uses record-pattern semantics, including the pattern-only shorthand `SyncResult.ok { count }`; construction does not permit that shorthand.

<span id="ORNA-ENUM-004"></span>**ORNA-ENUM-004** Variant payload field order is source-preserving for presentation and structural for matching.

<span id="ORNA-ENUM-005"></span>**ORNA-ENUM-005** A payload-free enum MAY be a primary-key component and is encoded by its canonical variant name. An enum variant carrying a payload MUST NOT be a primary-key component in version 1.0.

<span id="ORNA-ENUM-006"></span>**ORNA-ENUM-006** Variant identifiers SHOULD use lowercase `snake_case` for readability, but identifier case remains part of the name.

<span id="ORNA-CASE-001"></span>**ORNA-CASE-001** `case` is an expression and MUST be exhaustive for a statically closed scrutinee type.

<span id="ORNA-CASE-002"></span>**ORNA-CASE-002** Each arm has `pattern [if guard]: body` form. The colon introduces the arm body; `=>` remains anonymous-function syntax.

<span id="ORNA-CASE-003"></span>**ORNA-CASE-003** Arms are considered in source order; the first matching pattern whose guard evaluates to true is selected.

<span id="ORNA-CASE-004"></span>**ORNA-CASE-004** All reachable value-producing arm bodies MUST type-unify.

<span id="ORNA-CASE-005"></span>**ORNA-CASE-005** A guard is evaluated only after its pattern matches and before its body.

<span id="ORNA-CASE-006"></span>**ORNA-CASE-006** The `match` keyword and `pattern => body` arm surface are invalid in 1.0 and receive `ORNA091-E-MATCH`.

<span id="ORNA-CASE-007"></span>**ORNA-CASE-007** A processor MUST NOT interpret `case` as exception handling. It is ordinary exhaustive value branching; failures are handled by `|?`.

## 6.4 Generics, protocols and nested implementations {#expressions-generics-protocols-and-nested-implementations}

The protocol system is deliberately small and static. Bounds read like “`T` implements these protocols”:

```orna
pub protocol Display {
    fn display(self, context: DisplayContext): Str;
}

pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
}

pub fn maximum<T impl Order>(values: [T]): T? =
    values | sort_by(value => value) | last();
```

Implementations live inside their nominal target:

```orna
pub type EmailAddress {
    value: Str,

    impl Display {
        fn display(self, context) = self.value;
    }
}
```

<span id="ORNA-GENERIC-001"></span>**ORNA-GENERIC-001** Generic parameters are explicit in public source when a generic abstraction is intended; local type arguments may be inferred.

<span id="ORNA-GENERIC-002"></span>**ORNA-GENERIC-002** Protocol dispatch is static in version 1.0. Overlapping implementations are invalid.

<span id="ORNA-GENERIC-003"></span>**ORNA-GENERIC-003** A protocol implementation is lexically nested in the nominal target type. The current database must own that type declaration.

<span id="ORNA-GENERIC-004"></span>**ORNA-GENERIC-004** Specialization, associated types, higher-kinded types, negative implementations and runtime protocol objects are outside version 1.0.

<span id="ORNA-GENERIC-005"></span>**ORNA-GENERIC-005** A generic protocol bound uses `<T impl Protocol>`; several bounds use `+`, for example `<T impl Display + Order>`.

<span id="ORNA-GENERIC-006"></span>**ORNA-GENERIC-006** The removed colon-bound spelling `<T: Protocol>` is invalid and receives `ORNA091-E-BOUND-COLON`.

<span id="ORNA-GENERIC-007"></span>**ORNA-GENERIC-007** The removed top-level form `impl Protocol for Type` is invalid and receives `ORNA091-E-IMPL-FOR`; the implementation is moved into `Type` and omits `for Type`.

<span id="ORNA-GENERIC-008"></span>**ORNA-GENERIC-008** A protocol may declare instance functions and static properties.

<span id="ORNA-GENERIC-009"></span>**ORNA-GENERIC-009** A static protocol property is declared `static name: Type;` and implemented `static name = expression;`.

<span id="ORNA-GENERIC-010"></span>**ORNA-GENERIC-010** `static fn` is not a protocol-member category in 1.0. Behavior that does not require an instance is expressed as an ordinary function or as construction/conversion owned by the target type.

<span id="ORNA-GENERIC-011"></span>**ORNA-GENERIC-011** Every required protocol member must have exactly one compatible implementation in a conforming nested `impl` block.

<span id="ORNA-GENERIC-012"></span>**ORNA-GENERIC-012** Nested implementations may access the target type's private representation.

<span id="ORNA-GENERIC-013"></span>**ORNA-GENERIC-013** A nominal target MAY contain several nested implementations, including several different instantiations of one generic protocol such as `From<Str>` and `From<EmailHeader>`.

<span id="ORNA-GENERIC-014"></span>**ORNA-GENERIC-014** Implementation selection MUST NOT depend on source order when the target/source/protocol types are otherwise identical; such overlap is a static error.

<span id="ORNA-GENERIC-015"></span>**ORNA-GENERIC-015** A table declaration MAY contain nested protocol implementations for its nominal row type; those implementations are not stored row fields and do not affect table identity or storage.

## 6.5 Control flow and local bindings {#expressions-control-flow-and-local-bindings}

Orna uses braces to delimit blocks. Conditional expressions produce values, and local bindings may be reassigned as shown below.

```orna
let label =
    if total > 100.GBP {
        "large"
    } else {
        "small"
    };

let total = 0;
for item in items {
    total = total + item;
}
```

<span id="ORNA-CFLOW-001"></span>**ORNA-CFLOW-001** A block evaluates from left to right. A final expression without `;` is the block value; otherwise the block value is `Unit`.

<span id="ORNA-CFLOW-002"></span>**ORNA-CFLOW-002** Function receivers/arguments, list elements and record field expressions evaluate left to right in source order. `&&` and `||` short-circuit left to right.

<span id="ORNA-CFLOW-003"></span>**ORNA-CFLOW-003** `if` is an expression and all value-producing branches MUST type-unify.

<span id="ORNA-CFLOW-004"></span>**ORNA-CFLOW-004** `case` is an expression and MUST be exhaustive for closed types.

<span id="ORNA-CFLOW-005"></span>**ORNA-CFLOW-005** `let` creates a function-local binding slot containing an immutable value. Assignment may replace that slot's value when the pattern denotes one local identifier.

<span id="ORNA-CFLOW-006"></span>**ORNA-CFLOW-006** Assignment is a statement targeting an existing local `let` binding. It is not an expression and cannot occur inside a record field, argument, condition or returned value.

```orna
let count = 0;
count = count + 1;
count += 1;
```

<span id="ORNA-CFLOW-007"></span>**ORNA-CFLOW-007** `for` consumes a finite `Iterable`. An unbounded `Stream` is consumed by stream operations such as `for_each`.

<span id="ORNA-CFLOW-008"></span>**ORNA-CFLOW-008** `return` exits the current function; `break` exits the nearest loop and may carry a value; `continue` advances the nearest loop.

<span id="ORNA-CFLOW-009"></span>**ORNA-CFLOW-009** Orna has no implicit truthiness.

<span id="ORNA-CFLOW-010"></span>**ORNA-CFLOW-010** A braced control-flow expression (`if`, `case`, `for`, `while`, or `loop`) MAY be used as a statement without a trailing semicolon. When it is the final item of a block, it is the block's tail expression unless followed by `;`.

<span id="ORNA-PARSE-002"></span>**ORNA-PARSE-002** While parsing a block, a braced control-flow expression followed by another block item is a statement; the final unsemicolonated expression immediately before `}` is the block tail. `loop` used as a non-final statement requires `;` when its value would otherwise be ambiguous.

<span id="ORNA-CFLOW-011"></span>**ORNA-CFLOW-011** The removed `var` declaration MUST NOT be treated as a second mutability system. The mechanical migration is `var name = value;` to `let name = value;`.

## 6.6 Entry functions and `orna run` {#expressions-entry-functions-and-orna-run}

<span id="ORNA-RUN-001"></span>**ORNA-RUN-001** `orna run <qualified-function>` MUST invoke the named public function.

<span id="ORNA-RUN-002"></span>**ORNA-RUN-002** `orna run` without a function name MUST invoke root `main()` if present and MUST otherwise produce a diagnostic.

<span id="ORNA-RUN-003"></span>**ORNA-RUN-003** A file path MUST NOT acquire special execution behavior merely because it is named `ingest/main.orna` or appears under a particular directory.

A finite program exits when complete. A program consuming an unbounded stream remains active until cancelled or the source closes.

## 6.7 Anonymous functions and closures {#expressions-anonymous-functions-and-closures}

Anonymous functions use `=>`:

```text
value => value.name
(a, b) => a + b
() => sync(account)
message => {
    let decoded = decode(message);
    Email.insert(decoded);
}
```

<span id="ORNA-LAMBDA-001"></span>**ORNA-LAMBDA-001** One unparenthesized identifier or `_` before `=>` is a one-parameter anonymous function.

<span id="ORNA-LAMBDA-002"></span>**ORNA-LAMBDA-002** Zero or multiple parameters use parentheses.

<span id="ORNA-PARSE-001"></span>**ORNA-PARSE-001** When `(` begins an expression, the parser scans to the matching `)` while respecting nested delimiters. If the next significant token is `=>`, the construct is an anonymous-function parameter list; otherwise an empty pair is Unit, a comma at the outermost parenthesis level denotes a tuple, and a nonempty comma-free pair groups one expression. Implementations may use equivalent parsing machinery, but MUST produce the same parse.

<span id="ORNA-LAMBDA-003"></span>**ORNA-LAMBDA-003** Anonymous functions capture immutable lexical values. Such a value is called a closure when it captures at least one surrounding value.

<span id="ORNA-LAMBDA-004"></span>**ORNA-LAMBDA-004** `|...|` is not anonymous-function syntax. `|` is successful-value pipeline application, `|?` is failure recovery and `||` is logical OR.

<span id="ORNA-LAMBDA-005"></span>**ORNA-LAMBDA-005** Anonymous-function direct braced bodies follow [the lexical chapter](#lexical).

A bare named function is already a function value:

```orna
parallel([
    messages.sync,
    ledger.sync,
])
```

Use `() => ...` only when inline work or captured arguments are actually required.

## 6.8 Operators, success pipelines and recovery pipelines {#expressions-operators-success-pipelines-and-recovery-pipelines}

From lowest binding power to highest:

| Level | Forms | Associativity |
|---:|---|---|
| 1 | anonymous function `=>` | right |
| 2 | logical OR `\|\|` | left, short-circuiting |
| 3 | logical AND `&&` | left, short-circuiting |
| 4 | comparison `==`, `!=`, `<`, `<=`, `>`, `>=`, `in` | one operator; no chaining |
| 5 | option coalescing `??` | right |
| 6 | pipelines `\|`, `\|?` | left |
| 7 | ranges `..`, `..=` | non-associative |
| 8 | addition/subtraction `+`, `-` | left |
| 9 | multiplication/division/remainder `*`, `/`, `%` | left |
| 10 | exponent `^` | right |
| 11 | unary `!`, `-`, `+` | right |
| 12 | field access, calls and indexing | left |

<span id="ORNA-OP-001"></span>**ORNA-OP-001** A conforming parser MUST apply the precedence and associativity table above. Parentheses override it.

<span id="ORNA-PIPE-001"></span>**ORNA-PIPE-001** `|` is the canonical successful-value pipeline operator and has no bitwise meaning.

<span id="ORNA-PIPE-002"></span>**ORNA-PIPE-002** `value | function` is exactly `function(value)` after `value` completes successfully.

<span id="ORNA-PIPE-003"></span>**ORNA-PIPE-003** `value | function(a, named: b)` is exactly `function(value, a, named: b)`. The piped value is inserted as argument one.

<span id="ORNA-PIPE-004"></span>**ORNA-PIPE-004** Any callable may be a success-pipeline stage when its first parameter accepts the piped value. There is no separate pipe-function declaration or category.

<span id="ORNA-PIPE-005"></span>**ORNA-PIPE-005** A direct anonymous function used as a pipeline stage MUST be parenthesized:

```text
rows | (row => row.name)      // valid
rows | row => row.name        // invalid
```

<span id="ORNA-PIPE-006"></span>**ORNA-PIPE-006** Bitwise operations are complete ordinary functions supplied by `std.bits`, such as `bit_or`, `bit_and`, `bit_xor`, `bit_not`, `shift_left` and `shift_right`.

<span id="ORNA-PIPE-007"></span>**ORNA-PIPE-007** If evaluation to the left of `|` fails, the stage to its right is not evaluated and the failure continues outward.

<span id="ORNA-PIPE-008"></span>**ORNA-PIPE-008** `expression |? handler` invokes `handler(error)` only when `expression` fails. On success, the original value passes through without invoking the handler.

<span id="ORNA-PIPE-009"></span>**ORNA-PIPE-009** A recovery handler's successful return type MUST unify with the successful type to its left so later pipeline stages receive one static type.

<span id="ORNA-PIPE-010"></span>**ORNA-PIPE-010** If a recovery handler fails or explicitly calls `fail(error)`, that failure propagates normally.

<span id="ORNA-PIPE-011"></span>**ORNA-PIPE-011** `|` and `|?` associate leftward, permitting readable success/recovery sequences:

```orna
raw
    | decode
    | validate
    |? recover_invalid_message
    | store
```

<span id="ORNA-PIPE-012"></span>**ORNA-PIPE-012** `|?` is ordinary explicit recovery, not assertion syntax and not a stream dead-letter operation.

<span id="ORNA-COALESCE-001"></span>**ORNA-COALESCE-001** `a ?? b` evaluates `a` first. If `a` is `Some(value)`, it yields that value without evaluating `b`; otherwise it evaluates and yields `b`.

<span id="ORNA-COALESCE-002"></span>**ORNA-COALESCE-002** `??` associates rightward: `a ?? b ?? c` means `a ?? (b ?? c)`.

<span id="ORNA-COMPARE-001"></span>**ORNA-COMPARE-001** Equality and comparison operators do not chain. `a < b < c` and `a == b == c` are syntax errors; source writes `a < b && b < c` when that is intended.

<span id="ORNA-COMPARE-002"></span>**ORNA-COMPARE-002** Comparison operands evaluate left-to-right and each accepted operator yields `Bool`.

## 6.9 Fields, selectors and member calls {#expressions-fields-selectors-and-member-calls}

Field/property access, declared member calls and pipeline application are distinct.

```text
contact.full_name                 // stored/computed field selector
Contact.full_name(contact)        // the same selector as a first-class function
contact | transactions            // ordinary free function application
relation.count()                  // an actual declared Relation member
GBP.code                          // static protocol property
```

<span id="ORNA-MEMBER-001"></span>**ORNA-MEMBER-001** `value.field` resolves only a field selector declared by the value's nominal/structural type.

<span id="ORNA-MEMBER-002"></span>**ORNA-MEMBER-002** Every table field generates a first-class selector named `Table.field`. Selecting `row.field` applies that selector to the row.

<span id="ORNA-MEMBER-003"></span>**ORNA-MEMBER-003** `value.member(args)` is valid only for a member or protocol operation explicitly associated with the receiver type. Importing a free function whose first parameter happens to match MUST NOT silently make it a dot method.

<span id="ORNA-MEMBER-004"></span>**ORNA-MEMBER-004** Free functions remain ordinary calls and may always be used through the pipeline when argument one is compatible.

<span id="ORNA-MEMBER-005"></span>**ORNA-MEMBER-005** Core types may define genuine associated members such as relation `count`, `first`, or codec `encode`; this does not create unrestricted UFCS.

<span id="ORNA-MEMBER-006"></span>**ORNA-MEMBER-006** `Type.static_property` resolves a static property supplied by a protocol implementation and does not require construction of a value.

## 6.10 Failure propagation and recovery {#expressions-failure-propagation-and-recovery}

Ordinary source does not unwrap successful values. A called operation either produces its successful type or abruptly fails:

```orna
let decoded = std.encoding.json.decode(raw, as: Message);
let stored = Message.insert(decoded);
```

If decode fails, insertion is not attempted; the original failure leaves the function and, if unhandled, the activation.

```orna
let decoded =
    std.encoding.json.decode(raw, as: Message)
    |? (failure => Message.default());
```

<span id="ORNA-ERR-001"></span>**ORNA-ERR-001** A failure MUST propagate automatically through the current expression, function and activation until a matching `|?` recovery boundary or host boundary handles it.

<span id="ORNA-ERR-002"></span>**ORNA-ERR-002** Automatic propagation MUST preserve the original error value, causal chain and source-span information where available.

<span id="ORNA-ERR-003"></span>**ORNA-ERR-003** `fail(error_value)` abruptly completes with that error and has the bottom successful type, allowing it where any successful type is expected.

<span id="ORNA-ERR-004"></span>**ORNA-ERR-004** Cancellation is a distinct abrupt completion, not a normal `Error` that a broad `|?` handler may accidentally swallow.

<span id="ORNA-ERR-005"></span>**ORNA-ERR-005** Activation-owned resources MUST be released on return, failure, panic or cancellation. Deterministic cleanup SHOULD use scoped functions rather than user finalizers.

<span id="ORNA-ERR-006"></span>**ORNA-ERR-006** An unhandled failure in a database-writing activation MUST abort and roll back every Orna-controlled write in that activation.

<span id="ORNA-ERR-007"></span>**ORNA-ERR-007** External effects already performed before failure are not transactionally reversible; diagnostics and retry policy MUST remain honest about that boundary.

<span id="ORNA-ERR-008"></span>**ORNA-ERR-008** A postfix propagation operator `?` is invalid in expression position and receives `ORNA091-E-POSTFIX-QUESTION`.

<span id="ORNA-ERR-009"></span>**ORNA-ERR-009** There is no intrinsic `Result<T,E>`, `Ok(...)` or `Err(...)` failure surface. An unresolved use in that role receives `ORNA091-E-RESULT`. Independently declared user types and variants with these names remain ordinary declarations; this rule does not reserve their names.

<span id="ORNA-ERR-010"></span>**ORNA-ERR-010** Recovery uses `|?`, ordinary functions, `case` over ordinary values, or a combination of them. It does not require a parallel exception syntax.

<span id="ORNA-ERR-011"></span>**ORNA-ERR-011** A recovery handler executes at most once for the failure occurrence presented to that `|?` stage.

<span id="ORNA-ERR-012"></span>**ORNA-ERR-012** A successful recovery resumes evaluation at the next enclosing expression/pipeline point; it does not restart already completed stages.

### 6.10.1 Algorithm FAILURE-1: expression propagation {#expressions-algorithm-failure-1-expression-propagation}

1. Evaluate subexpressions in the order required by the containing construct.
2. If a subexpression completes successfully, continue with that value.
3. If it fails and the immediately enclosing construct is not the handling side of `|?`, skip remaining work in that construct and propagate the same failure.
4. If it is the left side of `|?`, invoke the handler exactly once with the failure value.
5. If the handler succeeds, substitute its value and continue. If it fails, propagate the handler's failure.
6. At an activation boundary, roll back Orna-controlled writes before reporting an unhandled failure.



## 6.11 Completion states {#expressions-completion-states}

Every expression evaluation has one of three completion states:

1. **success** with the expression's static value type;
2. **failure** with an `Error` value;
3. **cancellation**, which is controlled by runtime structured-concurrency semantics.

Failures are not successful values and are not encoded through an implicit enum wrapper. Optimizers and backends may use tagged internal representations, but source, type display, `sys.Function.return_type`, equality and codecs observe only the specified model.

<span id="ORNA-FAILURE-001"></span>**ORNA-FAILURE-001** A successful static return type excludes the failure channel.

<span id="ORNA-FAILURE-002"></span>**ORNA-FAILURE-002** The compiler MUST track whether reachable operations may fail sufficiently to preserve control flow, rollback and diagnostics, without requiring source-visible effect annotations in 1.0.

<span id="ORNA-FAILURE-003"></span>**ORNA-FAILURE-003** Public metadata SHOULD expose the inferred set of nominal failure types where knowable and `Error` where open-ended.

<span id="ORNA-FAILURE-004"></span>**ORNA-FAILURE-004** A broad recovery handler receives at least `Error`; a processor MAY infer a narrower nominal error type from the left expression.

<span id="ORNA-FAILURE-005"></span>**ORNA-FAILURE-005** Error values are ordinary inspectable values when explicitly received by a recovery handler, but they do not become successful results merely because they are values there.

<span id="ORNA-FAILURE-006"></span>**ORNA-FAILURE-006** Calling `fail(e)` from a recovery handler re-emits `e` unless it constructs another error deliberately.

<span id="ORNA-FAILURE-007"></span>**ORNA-FAILURE-007** Nested `|?` stages handle the nearest failure that reaches them according to left-associative pipeline evaluation.

<span id="ORNA-FAILURE-008"></span>**ORNA-FAILURE-008** A handler that returns a fallback value resumes with that value and does not rerun earlier successful/effectful stages.

<span id="ORNA-FAILURE-009"></span>**ORNA-FAILURE-009** Host boundaries record unhandled errors in `sys.Run`/`sys.Failure` as applicable and present a safe diagnostic.

<span id="ORNA-FAILURE-010"></span>**ORNA-FAILURE-010** The runtime MUST distinguish an activation failure from a preserved stream-item failure record; the latter is durable administration data produced by checkpoint policy.

## 6.12 Recovery examples {#expressions-recovery-examples}

```orna
fn parse_port(raw: Str): Port =
    raw
        | Int.parse
        | Port.from
        |? (failure => {
            log.warn(failure);
            8080 | Port.from
        });
```

```orna
fn require_message(raw: Str): Message =
    raw
        | std.encoding.json.decode(as: Message)
        |? (failure => fail(error(
            code: "message.invalid",
            message: "could not decode message",
            cause: failure,
        )));
```



# 7. Tables, rows and assertions {#tables}

## 7.1 One persistent relational noun {#tables-one-persistent-relational-noun}

<span id="ORNA-TABLE-001"></span>**ORNA-TABLE-001** `table` MUST be the only core declaration for persistent relational data.

The following declaration kinds are not part of conforming version 1.0 syntax:

```text
log
view
store
ingest
source
on
```

## 7.2 Explicit primary keys {#tables-explicit-primary-keys}

```orna
pub table Contact(id: Str) {
    name: Str,
    emails: [Str],
}
```

Parameters before the body form the ordered primary key. Body fields are non-key columns.

<span id="ORNA-KEY-001"></span>**ORNA-KEY-001** A table parameter list MUST define the complete primary key in parameter order.

<span id="ORNA-KEY-002"></span>**ORNA-KEY-002** Primary-key fields MUST be visible as ordinary logical row fields even if their loose-row values are encoded in the path.

## 7.3 Composite keys {#tables-composite-keys}

```orna
pub table Reading(sensor: Uuid, time: Instant) {
    temperature: Decimal,
    humidity: Decimal,
}
```

The logical key is `(sensor, time)`.

## 7.4 Automatic keys {#tables-automatic-keys}

```orna
pub table Note {
    created: Instant,
    text: Str,
}
```

<span id="ORNA-AUTOID-001"></span>**ORNA-AUTOID-001** A table without explicit key parameters MUST receive an implicit monotonically increasing `Int` key named `id`.

<span id="ORNA-AUTOID-002"></span>**ORNA-AUTOID-002** IDs allocated through one repository instance and its current hidden allocator MUST never be reused after deletion, reset, checkout or branch switching.

<span id="ORNA-AUTOID-003"></span>**ORNA-AUTOID-003** Gaps caused by failed or abandoned allocations are valid and expected.

<span id="ORNA-AUTOID-004"></span>**ORNA-AUTOID-004** The current allocator high-water mark MUST be stored outside rewindable branch trees using a hidden Git ref or an equivalent atomic monotonic mechanism.

Suggested ref:

```text
refs/orna/ids/<table-object-id>
```

<span id="ORNA-AUTOID-005"></span>**ORNA-AUTOID-005** A committed snapshot MUST include the allocator watermark known at that snapshot for historical introspection.

<span id="ORNA-AUTOID-006"></span>**ORNA-AUTOID-006** Independent offline clones have independent allocators and MAY allocate the same integer. A merge of different rows with the same automatic ID MUST create a typed row conflict and MUST NOT silently renumber either row or its references.

Where collision-free independent creation is required, the schema declares an explicit distributed key:

```orna
pub table Event(id: Uuid = uuid7()) {
    time: Instant,
    value: Str,
}
```

## 7.5 Key defaults {#tables-key-defaults}

```orna
pub table Contact(
    id: Uuid = uuid7()
) {
    name: Str,
    emails: [Str],
}
```

<span id="ORNA-KEYDEF-001"></span>**ORNA-KEYDEF-001** A key default MUST be an ordinary Orna expression evaluated once inside the insertion transaction.

<span id="ORNA-KEYDEF-002"></span>**ORNA-KEYDEF-002** A supplied key MUST bypass its default expression.

<span id="ORNA-KEYDEF-003"></span>**ORNA-KEYDEF-003** A generated key MUST be stored as the row's identity and MUST NOT be recomputed when referenced fields later change.

<span id="ORNA-KEYDEF-004"></span>**ORNA-KEYDEF-004** Key defaults that inspect current table state are allocation logic, not deterministic pure identities. Independent branches MAY allocate colliding or different keys; semantic merge MUST report resulting conflicts honestly.

## 7.6 Stored, defaulted and computed fields {#tables-stored-defaulted-and-computed-fields}

A table body has three field lifecycles:

```orna
pub table Contact(id: Str) {
    first: Str,
    last: Str,
    country: Str = "GB",
    full_name: Str => "{first} {last}",
}
```

| Form | Name | Logical behavior |
|---|---|---|
| `field: T` | required stored field | caller supplies a value and the row stores it |
| `field: T = expression` | defaulted stored field | expression runs once when an insertion omits the field; the resulting value belongs to that row |
| `field: T => expression` | computed field selector | no stored logical value; expression is evaluated from the row when observed |

<span id="ORNA-FIELD-001"></span>**ORNA-FIELD-001** An insertion-time default is an ordinary expression evaluated once inside the insertion transaction. A supplied value bypasses it.

<span id="ORNA-FIELD-002"></span>**ORNA-FIELD-002** The logical value selected for a defaulted field MUST NOT be recomputed merely because the declaration, table contents or referenced names later change.

<span id="ORNA-FIELD-003"></span>**ORNA-FIELD-003** When a closed deterministic constant default is introduced on a table containing older rows, snapshot metadata MAY record one frozen fallback value for physically missing fields. That frozen value belongs to the field-introduction revision. Later edits to the default affect future insertions only.

<span id="ORNA-FIELD-004"></span>**ORNA-FIELD-004** Future insertions logically store the evaluated default result. A storage profile MAY omit a value physically when it can reconstruct the row's already-frozen logical value without consulting a later declaration.

<span id="ORNA-FIELD-005"></span>**ORNA-FIELD-005** A row-dependent default may be used for future insertions, but introducing it to existing rows requires an explicit backfill or an optional field. Orna MUST NOT create a hidden compute-on-read era for a stored field.

<span id="ORNA-FIELD-006"></span>**ORNA-FIELD-006** `=>` declares a computed field. It is not stored as a logical row value, cannot be supplied during `insert`, and cannot be changed through `update`.

<span id="ORNA-FIELD-007"></span>**ORNA-FIELD-007** A computed field is evaluated from the row under the current snapshot whenever observed and automatically reflects changes to its stored-field dependencies.

<span id="ORNA-FIELD-008"></span>**ORNA-FIELD-008** A computed expression MUST be deterministic and row-local. It may read the current row and call pure helpers; it MUST NOT mutate tables, query unrelated tables, open streams, access secrets, perform I/O, or use ambient current time/randomness.

<span id="ORNA-FIELD-009"></span>**ORNA-FIELD-009** Within a key-default or computed-field expression, each table field name is available as an immutable lexical selector of the prospective/current row, and `self` names the complete row context. A local binding may shadow a field only inside an explicitly nested block.

<span id="ORNA-FIELD-010"></span>**ORNA-FIELD-010** Every stored/defaulted/computed field creates a selector such as `Contact.full_name`; `contact.full_name` applies it. Field dependencies appear in `sys.Dependency`.

<span id="ORNA-FIELD-011"></span>**ORNA-FIELD-011** The planner MAY cache/materialize a computed selector, but this MUST NOT change its value, mutation rules, dependencies or serialization. Computed fields are excluded from standalone stored-row encoding unless a codec explicitly requests a derived projection.

The difference between a computed field and an ordinary function is fieldhood, not computation: declaration inside the table gives parameterless property access and row-local restrictions. Arbitrary work remains an explicitly called or piped function.

## 7.7 Table-owned assertions {#tables-table-owned-assertions}

A table declaration owns propositions about its complete candidate relation:

```orna
pub table User(id: Uuid) {
    username: Username,
    email: EmailAddress?,
    role: Role,

    assert all_unique(user => user.username);
}
```

The table body already identifies the constrained relation. The canonical form therefore omits both a repeated table name and an explicit `self |` pipeline.

```orna
pub table Booking(id: Uuid) {
    starts: Instant,
    ends: Instant,
    cancelled: Bool,

    assert every(booking =>
        booking.cancelled || booking.ends > booking.starts
    );
}
```

`all_unique` and `every` are ordinary predicate constructors supplied by the relational core/reference `std`; they are not grammar keywords. Other deterministic relation predicates may be composed in exactly the same way.

<span id="ORNA-ASSERT-001"></span>**ORNA-ASSERT-001** `assert` is the only assertion-introducing keyword in Orna 1.0.

<span id="ORNA-ASSERT-002"></span>**ORNA-ASSERT-002** Assertions are enforced in all modes and MUST NOT be removed from release builds.

<span id="ORNA-ASSERT-003"></span>**ORNA-ASSERT-003** A table assertion is lexically contained by its owning table.

<span id="ORNA-ASSERT-004"></span>**ORNA-ASSERT-004** The owner subject of a table assertion is `Relation<Row>` for the complete unpublished candidate relation.

<span id="ORNA-ASSERT-005"></span>**ORNA-ASSERT-005** The candidate relation includes every insert, update, delete and re-key pending in the current validation boundary.

<span id="ORNA-ASSERT-006"></span>**ORNA-ASSERT-006** `all_unique(selector)` returns a predicate over a relation and tests uniqueness of the selector result according to its declared equality/null policy.

<span id="ORNA-ASSERT-007"></span>**ORNA-ASSERT-007** `every(predicate)` returns a predicate over a relation and is true only when the row predicate is true for every row.

<span id="ORNA-ASSERT-008"></span>**ORNA-ASSERT-008** The exact relation-predicate library is extensible through ordinary functions; adding one does not add assertion grammar.

<span id="ORNA-ASSERT-009"></span>**ORNA-ASSERT-009** A table assertion expression MUST type-check as `Predicate<Relation<Row>>` after owner-subject elaboration.

<span id="ORNA-ASSERT-010"></span>**ORNA-ASSERT-010** Table assertions are deterministic for a fixed candidate relation and captured database snapshot.

<span id="ORNA-ASSERT-011"></span>**ORNA-ASSERT-011** A table assertion MUST NOT perform network, process, UI or arbitrary filesystem effects, use nondeterministic randomness, or observe uncaptured wall-clock time.

<span id="ORNA-ASSERT-012"></span>**ORNA-ASSERT-012** Multiple assertions are conjunctive and evaluated in source order for deterministic diagnostics.

<span id="ORNA-ASSERT-013"></span>**ORNA-ASSERT-013** A false table assertion aborts the validating boundary before the candidate relation becomes visible.

<span id="ORNA-ASSERT-014"></span>**ORNA-ASSERT-014** Writes from an assertion-aborted boundary are rolled back.

<span id="ORNA-ASSERT-015"></span>**ORNA-ASSERT-015** A table assertion MUST observe a transactionally coherent candidate relation and MUST NOT observe a partially committed state.

<span id="ORNA-ASSERT-016"></span>**ORNA-ASSERT-016** The source `assert self | all_unique(...);` is invalid as a declaration assertion and receives `ORNA-A091-002`; the fix removes `self |`.

<span id="ORNA-ASSERT-017"></span>**ORNA-ASSERT-017** The source `assert TableName | all_unique(...);` is invalid as a declaration assertion and receives `ORNA-A091-002`; the fix removes the repeated owner.

<span id="ORNA-ASSERT-018"></span>**ORNA-ASSERT-018** Dedicated table-field `unique` and `check(...)` modifiers are not 1.0 grammar. They migrate to table-owned `assert` predicates.

<span id="ORNA-ASSERT-019"></span>**ORNA-ASSERT-019** An assertion-specific `else` arm is invalid. Expected operational failure handling uses the ordinary failure/recovery model.

<span id="ORNA-ASSERT-020"></span>**ORNA-ASSERT-020** `ensure`, `check`, `fact`, `constraint`, `constraints`, dedicated `unique`, and `|!` are not aliases for `assert`.

### 7.7.1 Algorithm ASSERT-TABLE-1: candidate-relation validation {#tables-algorithm-assert-table-1-candidate-relation-validation}

Given table `T`, committed logical relation `C`, pending mutation set `M`, and ordered assertions `a1...an`:

1. Enter the storage engine's normal transaction or unpublished candidate-state boundary.
2. Derive `R = apply(C, M)` without publishing it.
3. Elaborate each assertion predicate against owner subject type `Relation<T>`.
4. Evaluate assertions in source order against `R`.
5. If predicate evaluation fails, abort the boundary and propagate that failure.
6. If a predicate is false, select a deterministic safe witness where feasible, emit the table-assertion diagnostic, and abort.
7. Continue the surrounding commit, merge, checkout, rewrite or publication algorithm only after every applicable assertion succeeds.
8. Publish `R` only when the entire enclosing boundary succeeds.

## 7.8 Cross-table module assertions {#tables-cross-table-module-assertions}

Some invariants have no honest single-table owner. A module may declare a closed, deterministic proposition that depends on at least two distinct tables:

```orna
assert every(Invoice, invoice =>
    exists(Customer, customer => customer.id == invoice.customer_id)
);
```

A module assertion has no implicit `self` or relation subject. Its expression is an ordinary closed `Bool` over the unpublished candidate database.

<span id="ORNA-ASSERT-021"></span>**ORNA-ASSERT-021** A module assertion is valid only when its resolved dependency graph includes at least two distinct table objects.

<span id="ORNA-ASSERT-022"></span>**ORNA-ASSERT-022** A module assertion whose table dependencies all belong to one table is invalid and receives `ORNA-A091-003` with a fix to move it into that table.

<span id="ORNA-ASSERT-023"></span>**ORNA-ASSERT-023** A module assertion with no table dependency is invalid; module loading is not a compile-time execution facility.

<span id="ORNA-ASSERT-024"></span>**ORNA-ASSERT-024** A module assertion expression MUST type-check as a closed `Bool` and has no implicit owner subject.

<span id="ORNA-ASSERT-025"></span>**ORNA-ASSERT-025** Module assertions obey the same determinism and effect restrictions as table assertions.

<span id="ORNA-ASSERT-026"></span>**ORNA-ASSERT-026** A module assertion is evaluated whenever a validating boundary changes any table in its transitive dependency set.

<span id="ORNA-ASSERT-027"></span>**ORNA-ASSERT-027** Module assertions evaluate after all affected table-owned assertions and before publication of the candidate database.

<span id="ORNA-ASSERT-028"></span>**ORNA-ASSERT-028** Several applicable module assertions are ordered by owning module's stable ObjectId and then source span, providing deterministic first-failure behavior across modules.

<span id="ORNA-ASSERT-029"></span>**ORNA-ASSERT-029** A false module assertion aborts the complete candidate database boundary and rolls back all Orna-controlled writes in that boundary.

<span id="ORNA-ASSERT-030"></span>**ORNA-ASSERT-030** A cross-table assertion MUST NOT be attached arbitrarily to one table merely to avoid module syntax; ownership should reflect the proposition's dependency scope.

### 7.8.1 Algorithm ASSERT-DATABASE-1: cross-table validation {#tables-algorithm-assert-database-1-cross-table-validation}

1. Compute the unpublished candidate database after all pending writes and schema projection for the boundary.
2. Determine affected module assertions from stable dependency metadata.
3. Validate affected table assertions first.
4. Sort affected module assertions by stable owner identity and source span.
5. Evaluate each closed Boolean against the same candidate database snapshot.
6. On failure or false, abort the complete boundary and preserve a safe deterministic witness where feasible.
7. Publish table state, Git/CWD state and any coupled checkpoint only after all applicable assertions succeed.


## 7.9 Permitted key values {#tables-permitted-key-values}

Version 1.0 primary-key components may contain values with stable equality, total order and canonical encoding:

- `Bool`;
- `Int`;
- canonical `Decimal`;
- `Str`;
- `Uuid`;
- `Date`;
- `Instant`;
- payload-free enum variants;
- table-row references whose referenced primary key is itself permitted and canonically encodable;
- tuples composed from permitted key values.

<span id="ORNA-KEY-003"></span>**ORNA-KEY-003** `Float` MUST NOT be accepted as a primary-key component.

<span id="ORNA-KEY-004"></span>**ORNA-KEY-004** A key's logical type MUST NOT be restricted to the host filesystem's native filename rules.

<span id="ORNA-KEY-005"></span>**ORNA-KEY-005** A stored table reference MAY be a key component when the referenced table key is permitted and canonically encodable. The path encodes the referenced row key, not an embedded row.

<span id="ORNA-KEY-006"></span>**ORNA-KEY-006** `Range<T>` is not a special overlap-identity key in version 1.0. If used as ordinary stored data, overlapping ranges are valid. A future temporal-exclusion feature must use an explicit, separately specified table constraint rather than redefining equality.

This means v1 has no built-in declaration for a gap-permitting exclusive timeline such as employment or vehicle ownership. Such data may still be represented with an ordinary identity key plus `Range<T>` fields and validated by ordinary code, but kernel-enforced temporal exclusion is explicitly deferred rather than implementation-defined.

## 7.10 Key-to-path encoding {#tables-key-to-path-encoding}

Editable row files use one canonical, reversible path encoding. The internal compatibility identifier is `key-path-v1`; normal user-facing output calls this **editable row storage** and does not expose that identifier. The exact algorithm and vectors are also reproduced in `profiles/key-path.md`, but the numbered requirements in this section are the single authoritative requirement identifiers.

### 7.10.1 Type-specific key text {#tables-type-specific-key-text}

Each key component is first converted to canonical Orna text for its logical type:

- `Bool`: `false` or `true`;
- `Int`: shortest signed decimal form;
- `Decimal`: canonical exact decimal form;
- `Str`: its exact Unicode scalar sequence;
- `Uuid`: lowercase hyphenated form;
- `Date`: `YYYY-MM-DD`;
- `Instant`: canonical UTC RFC 3339 form;
- a payload-free enum: its stable variant spelling;
- a stored table reference: the referenced primary-key components;
- a tuple/composite key: its components recursively, in declared order.

<span id="ORNA-PATH-001"></span>**ORNA-PATH-001** ASCII letters `A-Z` and `a-z`, digits `0-9`, `.`, `_` and `-` MUST remain byte-for-byte readable unless the complete component is specially reserved below.

```text
alice-smith -> alice-smith
K1          -> K1
2026-09-03  -> 2026-09-03
42          -> 42
```

<span id="ORNA-PATH-002"></span>**ORNA-PATH-002** Literal `~` and every byte outside the readable set MUST be encoded as `~` followed by exactly two lowercase hexadecimal digits representing one UTF-8 byte.

```text
a~b       -> a~7eb
foo/bar   -> foo~2fbar
hello you -> hello~20you
é         -> ~c3~a9
```

<span id="ORNA-PATH-003"></span>**ORNA-PATH-003** The empty key component MUST encode as the complete component `~ff`. The byte `0xff` cannot occur in well-formed UTF-8, so this reserved two-digit form cannot collide with an encoded non-empty canonical key. `~ff` is valid only as the entire encoded component; any occurrence inside a longer component is malformed. A literal string key `"~ff"` encodes as `~7eff`.

<span id="ORNA-PATH-004"></span>**ORNA-PATH-004** `.` MUST encode as `~2e`; `..` MUST encode as `~2e~2e`. Every trailing `.` byte MUST be escaped.

<span id="ORNA-PATH-005"></span>**ORNA-PATH-005** Repository format 1 MUST use one host-independent reserved-name predicate. Let `original` be the canonical key text and let `trimmed` be `original` with trailing ASCII spaces and dots removed for this predicate only. Compare ASCII letters case-insensitively. The component is reserved when (a) `original` is exactly `.` or `..`, (b) `trimmed` is `.git`, or (c) the portion of `trimmed` before its first `.` is `CON`, `PRN`, `AUX`, `NUL`, `CLOCK$`, `CONIN$`, `CONOUT$`, `COM1` through `COM9`, or `LPT1` through `LPT9`. A reserved component MUST be made safe by hex-escaping the first UTF-8 byte that would otherwise be emitted. This corpus and algorithm MUST NOT vary with host locale, filesystem or installed Git version.

```text
con     -> ~63on
NUL.txt -> ~4eUL.txt
.git    -> ~2egit
```

<span id="ORNA-PATH-006"></span>**ORNA-PATH-006** Letter case MUST be preserved in the encoded path, but editable storage MUST be portable by construction. Within any one editable-key directory, two distinct encoded components whose ASCII letters become equal after ASCII lowercase conversion are a path collision on every host. Insert, direct-row discovery, checkout/load, re-key, storage rewrite and semantic merge MUST reject such a pair before any path is overwritten. No implementation may accept the pair merely because its current filesystem is case-sensitive, and no implementation may add an order-dependent suffix. Under `automatic` placement, a collision is reported rather than silently moving an existing editable row; the user may explicitly place/rewrite the affected rows or table in compact storage.

Examples of forbidden editable sibling pairs:

```text
Alice / alice
K1    / k1
```

<span id="ORNA-PATH-007"></span>**ORNA-PATH-007** Composite primary keys MUST map to nested components in declared key order. `.orna` is appended only to the final component and is not part of the encoded key.

<span id="ORNA-PATH-008"></span>**ORNA-PATH-008** An encoded key component MUST contain at most **200 UTF-8 bytes before the final `.orna` extension**.

<span id="ORNA-PATH-009"></span>**ORNA-PATH-009** The complete table-relative path, including separators and the final `.orna` extension, MUST contain at most **1024 UTF-8 bytes**.

<span id="ORNA-PATH-010"></span>**ORNA-PATH-010** A key exceeding either limit MUST NOT be truncated, hashed or silently renamed. Editable storage fails before mutation; automatic storage placement may choose compact storage only when doing so does not silently move an already editable collision peer.

<span id="ORNA-PATH-011"></span>**ORNA-PATH-011** Decoding MUST reject malformed escapes, uppercase hexadecimal escapes, `~ff` outside a complete empty component, and non-canonical aliases. Both identities MUST hold:

```text
decode(encode(key)) = key
encode(decode(path)) = path
```

For example, `~61lice` is rejected because `alice` is the canonical readable encoding, `~e` is rejected, and `~FF` is rejected.

<span id="ORNA-PATH-012"></span>**ORNA-PATH-012** Path decoding and materialisation MUST prevent absolute paths, traversal, symlink escape and writes outside the table's row directory.

These constants and encodings are fixed for repository format 1. Cross-platform tests are release evidence, not permission for implementations to choose different limits or collision semantics.

## 7.11 Loose row units {#tables-loose-row-units}

For a table declared in `contacts/main.orna`:

```text
contacts/Contact/alice-smith.orna
```

contains:

```orna
{
    name: "Alice Smith",
    emails: ["alice@example.com"],
}
```

<span id="ORNA-ROW-001"></span>**ORNA-ROW-001** A loose row unit MUST contain non-key fields only.

<span id="ORNA-ROW-002"></span>**ORNA-ROW-002** The row key MUST be reconstructed from the row path and table key schema.

<span id="ORNA-ROW-003"></span>**ORNA-ROW-003** A row unit's record fields MUST be validated against the table schema.

<span id="ORNA-ROW-004"></span>**ORNA-ROW-004** Unknown fields, missing required fields and incompatible values MUST produce diagnostics.

<span id="ORNA-ROW-005"></span>**ORNA-ROW-005** Comments and source formatting MAY be preserved in manually edited row units.

## 7.12 Mutations {#tables-mutations}

```orna
let alice = Contact.insert({
    name: "Alice Smith",
    emails: ["alice@example.com"],
});

Contact.update(alice.id, {
    emails: ["alice@new.example"],
});

Contact.delete(alice.id);
```

<span id="ORNA-MUT-001"></span>**ORNA-MUT-001** Rows are values; mutations are operations on tables.

<span id="ORNA-MUT-002"></span>**ORNA-MUT-002** Successful mutations MUST become visible in CWD atomically at the enclosing activation boundary.

<span id="ORNA-MUT-003"></span>**ORNA-MUT-003** Ordinary mutations MUST NOT automatically create a human Git commit unless publication rules for managed compact data apply.

<span id="ORNA-MUT-004"></span>**ORNA-MUT-004** Primary keys MUST be immutable through ordinary `update`.

<span id="ORNA-MUT-005"></span>**ORNA-MUT-005** An explicitly keyed table MUST provide the ordinary table operation `Table.rekey(old_key, new_key)`. It changes one row's primary key atomically within the current activation.

<span id="ORNA-MUT-006"></span>**ORNA-MUT-006** `rekey` MUST fail without changing CWD when the old key is absent, the new key already exists, the new key is invalid, or deferred referential validation fails.

<span id="ORNA-MUT-007"></span>**ORNA-MUT-007** Referential constraints are validated at activation commit. A program MAY update dependent references and re-key the target in the same activation; no invalid intermediate state is observable outside that activation.

<span id="ORNA-MUT-008"></span>**ORNA-MUT-008** Non-optional references use `restrict` by default. Orna v1 does not perform an implicit cascade. A caller that wants dependent references changed MUST update them explicitly in the same activation.

<span id="ORNA-MUT-009"></span>**ORNA-MUT-009** Re-keying a table with the implicit automatic integer key is prohibited. Such an identity is never rewritten; create a new row and delete the old row instead.

<span id="ORNA-MUT-010"></span>**ORNA-MUT-010** `sys.Change` and semantic diff MUST represent a successful explicit re-key as one `rekey` change containing the old and new keys, even if its physical representation is a file move or a compact deletion plus insertion.

<span id="ORNA-MUT-011"></span>**ORNA-MUT-011** A raw filesystem path rename is not authoritative evidence of semantic continuity. Unless it corresponds to a pending explicit `rekey` intent, it is interpreted as deletion plus insertion; tooling MAY suggest the explicit operation but MUST NOT silently guess.

## 7.13 Stable semantic identity and rename {#tables-stable-semantic-identity-and-rename}

Persistent definitions have committed stable `ObjectId` values separate from their names and revision hashes.

<span id="ORNA-OBJECT-001"></span>**ORNA-OBJECT-001** Stable IDs MUST exist for databases, modules, tables, columns, functions, nominal types, units, currencies and pages.

<span id="ORNA-OBJECT-002"></span>**ORNA-OBJECT-002** Stable IDs MUST be stored in canonical committed metadata rather than source annotations.

<span id="ORNA-OBJECT-003"></span>**ORNA-OBJECT-003** `orna mv <old> <new>` and semantic LSP rename MUST preserve the target ObjectId, update affected source/storage mappings and leave reviewable CWD changes without committing automatically.

<span id="ORNA-OBJECT-004"></span>**ORNA-OBJECT-004** A manual unassisted rename that cannot be tied to committed identity metadata MUST be treated conservatively as delete plus create. Orna MAY offer explicit adoption but MUST NOT guess silently.

<span id="ORNA-OBJECT-005"></span>**ORNA-OBJECT-005** Retired IDs MUST NOT be reused for an unrelated definition.

## 7.14 Stored table references {#tables-stored-table-references}

A table type used as a field denotes a stored reference to a row of that table:

```orna
pub table Vehicle(id: Uuid) {
    owner: directory.Contact,
}
```

<span id="ORNA-REF-001"></span>**ORNA-REF-001** A stored reference contains database identity, table ObjectId, primary key and snapshot context as required; it does not embed a duplicate row.

<span id="ORNA-REF-002"></span>**ORNA-REF-002** `vehicle.owner.key` is available without loading the target row. `vehicle.owner.name` resolves through the same snapshot context and may lower to a lookup or join.

<span id="ORNA-REF-003"></span>**ORNA-REF-003** Non-optional references default to `restrict` on target delete or re-key. Weak/dangling references and automatic cascade are outside version 1.0.

## 7.15 Filesystem equivalence {#tables-filesystem-equivalence}

For loose rows:

```text
create row file -> insert
edit row file   -> update
delete row file -> delete
rename path     -> explicit re-key candidate
```

<span id="ORNA-ROW-006"></span>**ORNA-ROW-006** Orna MUST give equivalent logical meaning to valid direct filesystem edits and corresponding table operations.

## 7.16 Crash-safe loose-row projection {#tables-crash-safe-loose-row-projection}

A multi-row activation cannot rely on several filesystem writes being atomic. Therefore programmatic loose-table mutations first commit to the Turso activation transaction and are then projected to row files through a recoverable intent.

Algorithm **ROW-PROJECT-1**:

1. During the activation transaction, store each logical mutation, its base blob/hash and intended canonical row body.
2. Commit the activation; the changes are now visible in CWD through the Turso overlay.
3. For each affected row, write a temporary file beside the destination, flush it, then atomically rename it over the destination where the platform permits.
4. Record each successful projection in Turso.
5. After every row is projected, mark the projection batch complete.
6. On restart, replay incomplete projections idempotently.

<span id="ORNA-PROJECT-001"></span>**ORNA-PROJECT-001** Queries MUST use the Turso overlay while a committed activation has not yet been fully projected to loose row files.

<span id="ORNA-PROJECT-002"></span>**ORNA-PROJECT-002** A crash during file projection MUST NOT lose or partially roll back the logical activation.

<span id="ORNA-PROJECT-003"></span>**ORNA-PROJECT-003** If a row file changed externally after the activation's recorded base hash, Orna MUST NOT overwrite it silently; it MUST create a typed CWD conflict.

<span id="ORNA-PROJECT-004"></span>**ORNA-PROJECT-004** Before staging or committing a loose row, Orna MUST complete or diagnose its pending projection.



## 7.17 Assertion categories {#tables-assertion-categories}

| Source owner | Required expression after elaboration | Implicit subject |
|---|---|---|
| Executable block | `Bool` | null |
| Refined type | `Predicate<Base>` | candidate base value |
| Table | `Predicate<Relation<Row>>` | complete candidate relation |
| Module | closed `Bool` with at least two table dependencies | null; reads candidate database |

<span id="ORNA-ASSERT-031"></span>**ORNA-ASSERT-031** An executable assertion evaluates its closed Boolean exactly once at the statement position.

<span id="ORNA-ASSERT-032"></span>**ORNA-ASSERT-032** A false executable assertion fails the activation with an `AssertionFailure` carrying the owning function, source span and safe values where available.

<span id="ORNA-ASSERT-033"></span>**ORNA-ASSERT-033** A subjectless refined comparison is semantic elaboration and MUST NOT be implemented as a textual rewrite introducing a source-visible global `self`.

<span id="ORNA-ASSERT-034"></span>**ORNA-ASSERT-034** The implicit owner subject exists only during refined/table assertion elaboration and MUST NOT leak into nested ordinary declarations, closures, exports or stored values.

<span id="ORNA-ASSERT-035"></span>**ORNA-ASSERT-035** Assertion name resolution otherwise follows ordinary lexical/module resolution.

<span id="ORNA-ASSERT-036"></span>**ORNA-ASSERT-036** An empty `assert;` is invalid and receives `ORNA-A091-011`.

<span id="ORNA-ASSERT-037"></span>**ORNA-ASSERT-037** Every assertion clause ends with `;` and a missing terminator receives `ORNA-A091-005`.

<span id="ORNA-ASSERT-038"></span>**ORNA-ASSERT-038** A declaration assertion whose predicate type is incompatible with its owner receives `ORNA-A091-004` showing expected owner subject and actual type.

<span id="ORNA-ASSERT-039"></span>**ORNA-ASSERT-039** An effectful or nondeterministic declaration assertion receives `ORNA-A091-007` identifying the forbidden effect.

<span id="ORNA-ASSERT-040"></span>**ORNA-ASSERT-040** An implementation MAY stop after the first false assertion in deterministic order unless an explicit diagnostic mode requests all independent failures.

## 7.18 Validation boundaries {#tables-validation-boundaries}

<span id="ORNA-ASSERT-041"></span>**ORNA-ASSERT-041** Refined assertions run whenever a base value is explicitly constructed or converted into the refined type.

<span id="ORNA-ASSERT-042"></span>**ORNA-ASSERT-042** Table and applicable module assertions run before ordinary transaction commit.

<span id="ORNA-ASSERT-043"></span>**ORNA-ASSERT-043** They also run before accepting a merge candidate that changes logical state.

<span id="ORNA-ASSERT-044"></span>**ORNA-ASSERT-044** They run before accepting checkout/reset state as current when logical state changes.

<span id="ORNA-ASSERT-045"></span>**ORNA-ASSERT-045** They run before compact publication, editable/compact rewrite or other representation transition becomes visible.

<span id="ORNA-ASSERT-046"></span>**ORNA-ASSERT-046** Concurrent writers validate against the transactionally serialized candidate state selected by the storage engine.

<span id="ORNA-ASSERT-047"></span>**ORNA-ASSERT-047** A replayable stream checkpoint MUST NOT advance when an assertion aborts the item or batch transaction.

<span id="ORNA-ASSERT-048"></span>**ORNA-ASSERT-048** Git history, compact pages, loose rows and the embedded high-rate tail are representations of one logical state and MUST NOT bypass assertions.

<span id="ORNA-ASSERT-049"></span>**ORNA-ASSERT-049** Evaluation failure inside an assertion propagates through the normal failure model and aborts the same boundary as a false assertion.

<span id="ORNA-ASSERT-050"></span>**ORNA-ASSERT-050** Assertion validation MUST preserve transaction atomicity and MUST NOT publish a state for which only a subset of applicable assertions ran.

## 7.19 Diagnostics and privacy {#tables-diagnostics-and-privacy}

<span id="ORNA-ASSERT-051"></span>**ORNA-ASSERT-051** A failure diagnostic identifies the failed assertion source span and owning declaration/module.

<span id="ORNA-ASSERT-052"></span>**ORNA-ASSERT-052** A refined failure identifies the candidate value through safe presentation rules.

<span id="ORNA-ASSERT-053"></span>**ORNA-ASSERT-053** A table/cross-table diagnostic identifies the predicate and, when feasible, a deterministic witness.

<span id="ORNA-ASSERT-054"></span>**ORNA-ASSERT-054** Diagnostics MUST NOT reveal secrets or values marked non-presentable.

<span id="ORNA-ASSERT-055"></span>**ORNA-ASSERT-055** A machine-applicable rewrite SHOULD be attached when migration is unambiguous.

<span id="ORNA-ASSERT-056"></span>**ORNA-ASSERT-056** A fixer MUST NOT rewrite arbitrary pipelines or `self` uses outside a recognized declaration-assertion legacy form.

<span id="ORNA-ASSERT-057"></span>**ORNA-ASSERT-057** Assertion failure MUST NOT be encoded as a hidden stored Boolean column, synthetic row or implicit index.

<span id="ORNA-ASSERT-058"></span>**ORNA-ASSERT-058** An optimizer MAY implement `all_unique`, `every` and cross-table predicates through indexes, anti-joins or incremental plans only when results and deterministic witnesses equal the reference logical evaluation.

<span id="ORNA-ASSERT-059"></span>**ORNA-ASSERT-059** Assertion dependencies appear in `sys.Dependency` and assertion definitions in `sys.Assertion`.

<span id="ORNA-ASSERT-060"></span>**ORNA-ASSERT-060** Presentation, formatting and diagnostic witness selection MUST NOT affect equality, storage, Git hashes or canonical serialization.

## 7.20 Required assertion diagnostics {#tables-required-assertion-diagnostics}

| Code | Condition | Primary remedy |
|---|---|---|
| `ORNA-A091-001` | legacy refined `where self` | use a brace-delimited `assert` block |
| `ORNA-A091-002` | `assert self | ...` or repeated table owner | remove explicit owner pipeline |
| `ORNA-A091-003` | one-table module assertion | move assertion into that table |
| `ORNA-A091-004` | predicate incompatible with owner | supply predicate for shown owner subject |
| `ORNA-A091-005` | missing assertion semicolon | terminate with `;` |
| `ORNA-A091-006` | assertion-specific `else` | use ordinary failure/recovery flow |
| `ORNA-A091-007` | forbidden effect/nondeterminism | remove identified effect |
| `ORNA-A091-008` | executable/refined assertion false | inspect owner, proposition and safe value |
| `ORNA-A091-009` | table/cross-table assertion false | inspect deterministic safe witness |
| `ORNA-A091-010` | removed alias/mini-language | use sole `assert` form |
| `ORNA-A091-011` | empty assertion | provide proposition |
| `ORNA-A091-012` | zero-table module assertion | place check in executable/test code |



# 8. Relations, queries and historical evaluation {#relations}

## 8.1 Relation values and bounded observation {#relations-relation-values-and-bounded-observation}

A table expression produces `Relation<RowType>`.

```orna
directory.Contact
    | filter(c => c.name.starts_with("A"))
    | sort_by(c => c.name)
```

<span id="ORNA-REL-001"></span>**ORNA-REL-001** A relation value MUST remain composable without requiring full materialization.

<span id="ORNA-REL-002"></span>**ORNA-REL-002** Relation presentation MAY request a bounded window of rows without enumerating the whole relation.

A submitted relation expression has been evaluated to a relation value even when only a window is enumerated. REPL previews MUST communicate estimates visually, for example `≈42.1k rows`, rather than describing the value as “not executed”.

## 8.2 Observable relation order {#relations-observable-relation-order}

<span id="ORNA-ORDER-001"></span>**ORNA-ORDER-001** A base table scan MUST be ordered by ascending canonical primary key.

<span id="ORNA-ORDER-002"></span>**ORNA-ORDER-002** `filter` and one-to-one `map` preserve input order; `flat_map` uses input order followed by each produced value's order; `take`, `drop`, windows and `pairs` require/preserve order.

<span id="ORNA-ORDER-003"></span>**ORNA-ORDER-003** `sort_by` establishes a stable order, `distinct` keeps the first occurrence, `union` yields left then right, grouped results use group-key order, and joins preserve left order with each right match in right-key order.

<span id="ORNA-ORDER-004"></span>**ORNA-ORDER-004** An optimizer may reorder internally but MUST restore the declared observable order before values are observed.

## 8.3 Equality and relation comparison {#relations-equality-and-relation-comparison}

Values compare structurally or nominally according to their types. Decimal equality ignores representational scale. Row values compare table ObjectId, primary key, snapshot context and logical fields. Stored row references compare target database/table identities and key.

<span id="ORNA-EQ-001"></span>**ORNA-EQ-001** Relation-wide `==` is invalid because sequence equality and set-of-rows equality are different operations. Libraries may provide explicitly named operations such as `same_sequence` and `same_rows`.

## 8.4 Reusable derived data {#relations-reusable-derived-data}

```orna
pub fn readings_above(limit: Decimal) =
    energy.Reading
        | filter(reading => reading.value > limit);
```

<span id="ORNA-QUERY-001"></span>**ORNA-QUERY-001** A named live calculation MUST be an ordinary function rather than a `view` declaration.

<span id="ORNA-QUERY-002"></span>**ORNA-QUERY-002** Calling the function evaluates its logical query against the current context unless arguments are explicitly snapshot-pinned.

<span id="ORNA-QUERY-003"></span>**ORNA-QUERY-003** Two calls are not implicitly collapsed merely because their source spelling and arguments match. Effectful or nondeterministic calls execute independently.

<span id="ORNA-QUERY-004"></span>**ORNA-QUERY-004** An implementation MAY reuse a previous call result only when the function is internally proven read-only and deterministic and its code, arguments, snapshot context, activation time and data dependencies have not changed. Reuse is an optimization, not a semantic guarantee.

<span id="ORNA-QUERY-005"></span>**ORNA-QUERY-005** Writes performed earlier in the same activation participate in dependency invalidation and read-your-writes. A later call MUST observe those writes when its dependencies include the changed table.

<span id="ORNA-QUERY-006"></span>**ORNA-QUERY-006** Actual reuse/materialization decisions SHOULD be visible through `sys.Materialization` and `orna explain` without requiring user effect annotations.

## 8.5 Current code over historical data {#relations-current-code-over-historical-data}

```orna
energy.Reading.as_of(sys.snapshot("HEAD~10"))
```

resolves the current table definition and current calling code while reading compatible data from the selected commit.

<span id="ORNA-HIST-001"></span>**ORNA-HIST-001** Data pinning MUST NOT silently switch the entire executing program to historical code.

## 8.6 Whole-program historical evaluation {#relations-whole-program-historical-evaluation}

```text
let old = sys.database.as_of(sys.snapshot("HEAD~10"));

old.energy.daily()
old.directory.Contact
old.sys.Table
```

The returned database snapshot object is a read-only root namespace for the historical code, schemas, rows, attached database pins, language edition, `std` commit and committed semantic metadata of that snapshot.

<span id="ORNA-HIST-002"></span>**ORNA-HIST-002** Calling a function through a database snapshot object invokes the historical definition.

<span id="ORNA-HIST-003"></span>**ORNA-HIST-003** Historical snapshot execution MUST NOT mutate current CWD, advance live checkpoints, use current secrets, open connectors or perform external effects.

<span id="ORNA-HIST-004"></span>**ORNA-HIST-004** Values from different snapshot contexts MUST NOT be mixed implicitly.

<span id="ORNA-HIST-005"></span>**ORNA-HIST-005** If required historical Unicode, time-zone, codec or edition support is unavailable, Orna reports incomplete reproducibility rather than silently substituting current semantics.

## 8.7 Materialization and optimization {#relations-materialization-and-optimization}

There is no user-visible `store` or `cache` declaration in version 1.0.

<span id="ORNA-PLAN-001"></span>**ORNA-PLAN-001** Materialization, caching, indexing and incremental maintenance MUST NOT change logical function results.

<span id="ORNA-PLAN-002"></span>**ORNA-PLAN-002** Planner choices MUST be introspectable through `sys.Plan`, `sys.Storage`, `sys.Materialization` and `orna explain`.

<span id="ORNA-PLAN-003"></span>**ORNA-PLAN-003** Every optimization MUST have a correct scan/reference fallback.

Versioned planner hints are deferred until real workloads establish a useful policy vocabulary.

## 8.8 Activation consistency and call reuse {#relations-activation-consistency-and-call-reuse}

An activation already captures one starting CWD generation, snapshot context and `now()` value while observing its own writes. This gives consistency; it does not imply that every function is memoized.

<span id="ORNA-CALL-001"></span>**ORNA-CALL-001** Two calls in one activation that are proven read-only and deterministic, have equal arguments, use the same code revision/context and observe unchanged dependencies MUST return observationally equal values.

<span id="ORNA-CALL-002"></span>**ORNA-CALL-002** A write, checkpoint movement, external observation or any dependency change between calls invalidates reuse. Effectful or nondeterministic calls MUST NOT be collapsed merely because their source and arguments look equal.

<span id="ORNA-CALL-003"></span>**ORNA-CALL-003** An implementation MAY evaluate an eligible call once and reuse the value. It MAY instead evaluate it repeatedly, provided the observable result and dependency context are identical.

<span id="ORNA-CALL-004"></span>**ORNA-CALL-004** When reuse/materialization occurs, `sys.Query`, `sys.Plan` or `sys.Materialization` MUST make the decision inspectable. The language does not promise a particular evaluation count for pure calls.

Example:

```orna
let before = Contact | count;
Contact.insert({ name: "Alice" });
let after = Contact | count;       // sees the write; cannot reuse `before`
```

## 8.9 Dependencies and lineage {#relations-dependencies-and-lineage}

The compiler/runtime maintains exact dependency edges where statically knowable and observed edges where dynamic.

<span id="ORNA-DEP-001"></span>**ORNA-DEP-001** Dependencies MUST identify source and destination objects with typed `sys.Object` references.

<span id="ORNA-DEP-002"></span>**ORNA-DEP-002** Dependency kinds SHOULD include imports, calls, reads, writes, references, renders, uses-type and uses-unit.

<span id="ORNA-DEP-003"></span>**ORNA-DEP-003** Dependency information MUST be used for impact analysis, live invalidation, semantic diff, merge validation and selective testing where applicable.

## 8.10 Attached databases and pinning {#relations-attached-databases-and-pinning}

<span id="ORNA-ATTACH-001"></span>**ORNA-ATTACH-001** Cross-database reads MAY compose relations from consistent snapshots of each attached database.

<span id="ORNA-ATTACH-002"></span>**ORNA-ATTACH-002** Orna MUST NOT claim atomic cross-database writes when attached databases have independent transaction logs.

<span id="ORNA-ATTACH-003"></span>**ORNA-ATTACH-003** Structural unit equivalence MUST be checked across database boundaries.

<span id="ORNA-PACKAGE-001"></span>**ORNA-PACKAGE-001** Version 1.0 MUST NOT require a package registry, semantic-version solver or lockfile where exact Git commit pins already determine dependency state.

<span id="ORNA-PACKAGE-002"></span>**ORNA-PACKAGE-002** Checking out an historical parent snapshot MUST resolve each attached database to the commit pinned by that snapshot.

<span id="ORNA-PACKAGE-003"></span>**ORNA-PACKAGE-003** `std` remains an ordinary optional attached Orna database/library except for mandatory core language and `sys` facilities.



# 9. Core operations and the standard library {#standard-library}

The language supplies a small intrinsic environment sufficient to evaluate source, query and update tables, consume streams, validate assertions and inspect `sys`. The optional `std` namespace contains ordinary, explicitly imported library code pinned to a Git snapshot. The two are not interchangeable: deleting an optional formatter or connector cannot disable database integrity.

## 9.1 Resolution and versioning {#standard-library-resolution-and-versioning}

Core type names, `Some`, `null`, `error`, `fail`, `now`, `uuid7` and the relational/stream operations below are available in the root intrinsic environment. Ordinary lexical declarations may shadow unqualified helpers; `sys` itself cannot be shadowed. `CWD` and `HEAD` are context-bound snapshot selectors, not wall-clock values. Evaluating `HEAD` in an unborn repository fails; it does not produce an empty synthetic commit.

`std` imports use ordinary `use` rules. `use std.prelude as _;` imports the prelude exports recorded by that pinned module; it does not import every standard module. `std.query` and `std.stream` may re-export the intrinsic operators with identical identities and behaviour. A user-defined function of the same name remains an ordinary function and is checked for the effects required by its use.

<span id="ORNA-LIB-001"></span>**ORNA-LIB-001** A program's library behaviour MUST be determined by its captured dependency snapshots. A historical program cannot silently substitute the currently installed `std` or current time-zone data.

<span id="ORNA-LIB-002"></span>**ORNA-LIB-002** Core integrity, transactions and assertions MUST work without `std`. An absent optional module produces the ordinary import diagnostic; it cannot be filled by an undocumented host library with the same spelling.

<span id="ORNA-LIB-003"></span>**ORNA-LIB-003** This section's portable operations MUST have the stated argument, order, failure and effect semantics when supplied under the reference-library profile. Other modules expose their exact public declarations through their pinned source and `sys`; their mere namespace name is not a claim of an unspecified portable API.

## 9.2 Signature notation {#standard-library-signature-notation}

In the following tables, `T`, `U` and `K` are type parameters, and `fn(T): U` is a function type. `Collection<T>` in explanatory prose means either `[T]` or `Relation<T>`; it is not a new source type. Each listed collection operation has those two explicit overloads and preserves the input container kind unless its result is scalar. `Stream<T>` operations are separate because they have waiting, ownership and checkpoint effects.

Arguments are evaluated left to right. A pipeline supplies argument one: `rows | filter(test)` calls `filter(rows, test)`. It does not first call `filter(test)` and guess a missing collection. Predicate factories explicitly listed below are genuine one-argument calls.

## 9.3 Core relation operations {#standard-library-core-relation-operations}

| Operation | Successful result | Contract |
|---|---|---|
| `filter(rows, predicate: fn(T): Bool)` | Same collection kind of `T` | Retain matching values in input order. No implicit truthiness. |
| `map(rows, transform: fn(T): U)` | Same collection kind of `U` | One result per input, preserving order. |
| `flat_map(rows, transform)` | Same collection kind | Concatenate each finite returned collection in input order; preserve inner order. |
| `sort_by(rows, key: fn(T): K)` | Same collection kind of `T` | Stable ascending order by K; ties preserve input order. Float uses total order. |
| `take(rows, count: Int)` / `drop(rows, count: Int)` | Same collection kind | Nonnegative count; do not enumerate beyond what the result requires. Negative count fails. |
| `distinct(rows)` | Same collection kind of `T` | Keep the first value in each lawful equality class. Default Float hashing is unavailable. |
| `union(left, right)` | Common collection kind | Concatenate left then right; duplicates remain. Use `distinct` explicitly for set-like behaviour. |
| `count(rows)` | `Int` | Exact finite count. Resource failure is explicit, not a truncated count. |
| `first(rows)` | `T?` | `Some(first value)` or `null`; need not enumerate the remainder. |
| `one(rows)` / `one(rows, predicate)` | `T` | Require exactly one matching value; zero and multiple matches are distinct cardinality errors. |
| `sum(rows)` | Numeric element type | Exact addition for Int/Decimal/Money; additive zero for empty input; Float follows observable order. |
| `min(rows)` / `max(rows)` | `T?` | `null` for empty input; otherwise `Some`. Float follows the specified NaN/signed-zero rules. |
| `every(rows, predicate)` | `Bool` | True for empty input; short-circuit at the first false value in observable order. |
| `exists(rows, predicate)` | `Bool` | False for empty input; short-circuit at the first true value. |

Relation members `count`, `first`, `one`, `as_of` and generated field selectors are real associated operations, not unrestricted free-function-to-method conversion. Other operators use explicit calls or pipelines unless the type declares an associated member.

<span id="ORNA-LIB-004"></span>**ORNA-LIB-004** Read-only query callbacks MUST be effect-checked before optimisation. A relational plan may not hide an external side effect inside an expression that an optimiser could reorder, omit or repeat.

<span id="ORNA-LIB-005"></span>**ORNA-LIB-005** Failed callbacks propagate through the ordinary failure channel. A collection operator MUST NOT skip failed elements or substitute `null` unless that behaviour is requested by a separately named operation.

### 9.3.1 Predicate factories {#standard-library-predicate-factories}

`Predicate<S>` is the structural callable type `fn(S): Bool`. `every(test)` constructs a predicate over `Relation<T>` that applies the row test. `all_unique(selector)` constructs a predicate over `Relation<T>` whose selected keys must have a lawful equality relation and compatible hash or order implementation. No `Float`-based default equality key is accepted.

`all_unique` compares complete selected values. For optional selected keys, `null` equals `null`; two absent values therefore violate uniqueness. To exclude missing values, write an explicit deterministic predicate over a filtered relation. There is no implicit SQL-style null exclusion. On failure, the reference witness is the earliest duplicate pair in canonical row order. An index implementation must produce that same safe witness when one is requested.

```orna
pub table Account(id: Str) {
    handle: Str,
    assert all_unique(account => account.handle);
}
```

Factories are ordinary functions, not additional keywords. The declaration supplies the candidate relation; the source does not repeat it through `self |`.

## 9.4 Core table operations {#standard-library-core-table-operations}

Table mutation arguments are checked against a record shape derived from the table schema. This shape is a compile-time constraint and has no separately exported source type.

| Call | Result | Required behaviour |
|---|---|---|
| `T.insert(fields)` | `T` row value | Construct a complete new row, evaluate defaults once and fail on an existing key. Missing automatic/defaulted keys are allocated; missing other keys fail. |
| `T.upsert(fields)` | `T` row value | Insert when absent; otherwise replace explicitly supplied stored non-key fields while preserving omitted existing fields. On the insert path all required fields must be supplied or defaulted. |
| `T.update(key, fields)` | `T` row value | Require an existing row; patch only supplied stored non-key fields. Unknown, computed and primary-key fields fail before mutation. |
| `T.delete(key)` | `Unit` | Require an existing row and delete it from the candidate state. References are checked at commit. A missing key is an error, not a successful deletion count. |
| `T.rekey(old_key, new_key)` | `T` row value | Require an explicitly keyed table; preserve fields and semantic change identity, then validate references at commit. |

A one-component key is supplied as that component's type. A multi-component key is a tuple in declaration order. Each operation observes earlier successful writes in the same activation, but returned row values remain immutable observations. No mutation directly edits an already held row value.

<span id="ORNA-LIB-006"></span>**ORNA-LIB-006** Mutation return values and missing-key behaviour MUST follow this table. Errors include the table identity and safe key, without disclosing redacted values. A failing operation's tentative writes cannot leak into the rest of the activation through a recovery handler.

For the last rule, each mutation has a statement savepoint: a recoverable operation failure restores its own partial changes before control reaches `|?`. Earlier successful operations in the enclosing activation remain tentative until that activation commits or rolls back. A declaration assertion is validated at the enclosing transaction boundary and may still abort the entire activation.

## 9.5 Core finite streams {#standard-library-core-finite-streams}

`Stream.from_list(values, source_identity: Str)` constructs a finite, replayable list-backed stream for deterministic input and testing. It performs no I/O. The effective source identity combines the supplied name with the canonical typed digest of the complete immutable list. Reusing a label for different list contents therefore cannot resume against the old contents by accident.

The sole partition is `null`. Position format `orna.list.v1` stores the zero-based index of the next item as an unsigned canonical integer. Position 0 precedes the first item; position equal to the list length denotes exhaustion. Resume outside 0…length fails. The successor of item i is i+1. A provider cannot treat the index as a mutable list offset after construction.

`for_each(stream, action: fn(T): Unit)` processes one item at a time. Each callback and corresponding checkpoint movement is a separate activation, and the enclosing call returns `Unit` after exhaustion. A returned value other than Unit must be discarded explicitly in the callback body. The operator propagates cancellation and reports blocked failures through the checkpoint model rather than silently continuing.

<span id="ORNA-LIB-007"></span>**ORNA-LIB-007** The list-backed source MUST supply the complete declared identity, replay and preservation contract. It MUST NOT require an unprovided connector host or external account.

## 9.6 Collection and query library {#standard-library-collection-and-query-library}

The following helpers are supplied by `std.collection` and `std.query`. They are pure for pure inputs/callbacks; their finite-input and ordering requirements are checked rather than hidden behind unbounded buffering.

| Function | Contract |
|---|---|
| `chunk(values, size)` | Consecutive nonempty chunks; final chunk may be shorter. Size must be positive. Empty input gives no chunks. |
| `flatten(values)` | One level of concatenation, preserving outer and inner order. |
| `partition(values, predicate)` | A two-element tuple `(matching, remaining)`, each preserving input order. Predicate evaluated once per value. |
| `zip(left, right)` | Ordered pairs until the shorter collection ends. `zip_exact` fails when lengths differ. |
| `unique(values)` | Same first-occurrence semantics as core `distinct`; this is a library helper, not a field modifier. |
| `group_by(values, key)` | Groups in key order, each group's rows in input order. Key comparison must be lawful. |
| `pairs(values)` | Adjacent overlapping pairs; length n produces max(0,n−1) pairs. |
| `window(values, size, step = 1)` | Complete positional windows only; positive size and step. A separately named partial-window option must be explicit. |
| `split_when(values, predicate)` | Start a new group before an item where the boundary predicate is true; no empty groups are inserted. |
| `rank(values, key)` | Stable sort by key, then assign one-based competition ranks; equal keys share a rank, and the next rank skips their count. |
| `asof_join(left, right, time, by)` | For each left row choose the latest right row at or before its time in the equal `by` group. Equal right timestamps resolve by right canonical key order, choosing the last. Missing matches are `null`. |
| `bucket_by(values, period, zone)` | Group time values by explicit elapsed or calendar boundaries. Calendar bucketing of Instant requires a zone. No fixed-seconds approximation of a local day is allowed. |

`pivot`, `unpivot` and domain-specific query helpers may be provided by a pinned library package. Their result schemas must be statically derivable or explicitly selected; they cannot introduce an implicit dynamic-record fallback. Their exact package declarations, not this catalogue label, determine their optional API.

## 9.7 Standard stream operators {#standard-library-standard-stream-operators}

| Operation | Behaviour and limits |
|---|---|
| `batch(stream, size)` | Ordered batches of at most positive size; the final batch may be shorter. Checkpoint covers the whole committed batch. |
| `buffer(stream, capacity)` | Bounded queue with positive capacity; default backpressure, no implicit drop. |
| `merge(streams)` | Interleave by observed arrival order, preserving each input's order. Cross-source order is intentionally nondeterministic and cannot be used as a deterministic assertion. |
| `throttle(stream, duration)` | Emit at most one item per positive elapsed interval; the caller selects and records the explicit drop policy before a nonreplayable source is admitted. |
| `debounce(stream, duration)` | Retain the latest item until a positive quiet interval elapses; dropped intermediate delivery semantics must be declared, not mistaken for durable processing of every item. |
| `retry(stream, policy)` | Retry the same blocked delivery under its provider contract and bounded delay policy; never silently advance progress on exhaustion. |
| `recover(stream, handler)` | Explicitly handles source/decoding errors only where the provider contract permits recovery. It cannot acknowledge an unprocessed ordered delivery without the skip protocol. |

Stateful operators must either keep restart-recomputable state or persist it in ordinary tables together with the input checkpoint. A buffer in RAM is not a durable checkpoint. A pipeline combining several checkpointed source roots must separate them into named consumer functions with explicit durable identities.

## 9.8 Concurrency, text and bit operations {#standard-library-concurrency-text-and-bit-operations}

`std.concurrent.parallel`, `race` and `timeout` follow [task ownership](#execution). `sleep(duration)` is cancellation-aware, accepts nonnegative elapsed duration and does not promise exact scheduling at the requested instant. It has a clock/waiting effect and is not permitted in declaration assertions.

`std.text` provides `trim`, `split`, `join`, `starts_with`, `ends_with`, `contains`, `replace`, `normalise`, `lower` and `upper`. Text indices and lengths are Unicode scalar positions unless the function is explicitly named for UTF-8 bytes or grapheme clusters. Casing and normalisation use the pinned Unicode data version; locale-sensitive operations require an explicit locale. `split` preserves empty fields, including trailing fields; an empty separator splits into scalars. Replace is non-overlapping left-to-right. Regex behaviour is part of an explicitly pinned regex package, not an implicit host dialect.

`std.bits` provides `bit_or`, `bit_and`, `bit_xor`, `bit_not`, `shift_left` and `shift_right` over Int using an unbounded signed two's-complement model. Shift counts must be nonnegative. Right shift is arithmetic; left shift is exact subject to resource limits. `bit_not(x) = -x - 1`. The source token `|` always remains a pipeline.

## 9.9 Exact money, time and statistics {#standard-library-exact-money-time-and-statistics}

`Currency` is a core protocol because `Money<C>` must remain meaningful without `std`. `std.money` re-exports that protocol and supplies explicit rounding, allocation and formatting. Currency symbols and placement are locale data; `GBP.code` is identity metadata, not a universal display symbol.

`allocate(amount, weights)` requires nonnegative exact integer weights and a positive total weight. It rounds each share toward zero at the currency's minor-unit scale, then distributes remaining minor units by descending exact fractional remainder, ties by input index. Negative amounts allocate the magnitude and restore signs. The shares sum exactly to the original minor-unit amount; a non-minor-unit input requires an explicit rounding choice first.

`std.time` distinguishes elapsed arithmetic from calendar arithmetic. Ambiguous local times require an explicit earlier/later offset choice; nonexistent local times fail unless an explicit adjustment policy is supplied. The time-zone database edition is recorded with dependency/runtime metadata. Formatting uses a supplied immutable locale/time-zone context.

The `duration.compact`, `duration.clock`, `duration.words` and `duration.iso` formatter values expose `.format(value, context: ...)`. They are interchangeable presentation choices, not storage conversions. The ISO form is an elapsed duration format, not a way to turn a month into a fixed number of seconds.

`std.stats.mean` returns `null` for empty input. Exact inputs retain exact sums; a nonterminating exact division requires explicit scale/rounding. `median` uses total-order sorting and returns the middle value, or the explicitly rounded/exact mean of the middle two. `percentile` requires a probability in [0,1] and a named interpolation method; there is no unrecorded host default. Histogram bins are ordered, nonoverlapping half-open ranges except an explicitly closed final upper bound. Rate, derivative and integration require ordered timestamps and state their unit transformation and treatment of equal timestamps.

## 9.10 Codecs and host I/O {#standard-library-codecs-and-host-i-o}

Every codec owns `encode(value)` and `decode(input, as: T)`. Canonical Orna text and the binary value profile are defined in [canonical formats](#formats). JSON decoding is schema-directed, rejects duplicate object keys, distinguishes missing fields from explicit null, and parses numbers without first rounding through binary Float. Unknown fields fail unless an explicit decoding option permits ignoring them. JSON encoding of unsupported values fails rather than dropping fields or emitting nonstandard numeric tokens.

Base64 uses the RFC 4648 standard alphabet with `=` padding and no inserted whitespace. The decoder rejects invalid characters, noncanonical padding and unused nonzero trailing bits. URL-safe Base64 is a separately named codec profile. CSV, XML, YAML, TOML and MIME are optional codec packages; their source pins, schema mapping and selected format editions must be stated by the package before interoperability is claimed.

`std.io.fs`, `std.io.process` and `std.net` are explicit host-effect boundaries. A filesystem operation states its root/path and overwrite mode, process invocation separates executable from argument strings, and HTTP/WebSocket clients expose status, headers, body, cancellation and bounded reads. No shell command is assembled implicitly from interpolated strings. These operations are unavailable to assertions, read-only watches, presenters and historical execution. Their errors propagate normally; database rollback does not undo an external write or request.

SOPS is an optional secret provider. A stable `SecretRef` is not secret plaintext; resolving it is a separately checked host effect. Generic encoding and presentation never reveal the resolved value.

## 9.11 Presentation helpers and tests {#standard-library-presentation-helpers-and-tests}

`std.ui` functions construct the core presentation tree. A helper such as `Field(label, value)` explicitly presents the typed value and returns a presentation node; it is not an implicit heterogeneous-list conversion. `Text`, `Rows`, `Cols`, `Stack`, `Details`, `Table`, `Tree`, `Code`, `Diff` and `Chart` all retain an Inspect-compatible fallback. `Button`, `Form` and `Input` carry typed action/input descriptions; only a server-created action handle can execute an event.

`std.test` supplies expectations, fixtures, property generators and isolated database helpers. Language-level `assert` remains core. A test fixture declares its source, initial snapshots, inputs, expected success/failure and external-effect policy. A temporary branch is not isolation unless the worktree-local CWD and runtime ownership are isolated too. No test may silently use the developer's local database, credentials or live services.



# 10. Activations, transactions and task ownership {#execution}

## 10.1 Automatic activation transactions {#execution-automatic-activation-transactions}

A top-level activation that performs Orna table mutations owns a database transaction. Activation roots include one submitted REPL input, one direct `orna run` invocation, one page action, one stream item or configured batch callback, and one explicitly created concurrent child activation.

<span id="ORNA-TXN-001"></span>**ORNA-TXN-001** Orna-controlled table writes within an activation MUST commit together on successful completion.

<span id="ORNA-TXN-002"></span>**ORNA-TXN-002** If an error, panic or cancellation escapes the activation, all Orna-controlled writes performed by that activation MUST roll back.

<span id="ORNA-TXN-003"></span>**ORNA-TXN-003** Nested ordinary function calls participate in the transaction of the activation root and MUST NOT independently commit.

<span id="ORNA-TXN-004"></span>**ORNA-TXN-004** An unbounded stream program MUST NOT hold one transaction for its lifetime; each item or configured ordered batch callback is a distinct activation.

## 10.2 Read semantics {#execution-read-semantics}

<span id="ORNA-TXN-005"></span>**ORNA-TXN-005** An activation MUST capture one starting CWD generation, one snapshot context and one activation time. All reads and repeated function calls in that activation observe that context, `now()` returns the captured activation time, and the activation observes its own successful writes before commit.

<span id="ORNA-TXN-006"></span>**ORNA-TXN-006** Other activations MUST NOT observe partial writes from an uncommitted activation.

## 10.3 External effects {#execution-external-effects}

```text
insert a local row
perform an external HTTP request
insert another local row
```

If the final insert fails, Orna rolls back its database writes but cannot un-send the HTTP request.

<span id="ORNA-TXN-007"></span>**ORNA-TXN-007** Orna MUST NOT imply that external effects are transactionally reversible.

<span id="ORNA-TXN-008"></span>**ORNA-TXN-008** User-visible effect annotations are not required for Orna to identify table mutations it owns. Internal effect information MAY be inferred for diagnostics, planning and introspection.


## 10.4 Task ownership and termination {#execution-task-ownership-and-termination}

An **activation** is the unit of execution and database atomicity. A **task** is scheduled work owned by an activation or session. A task need not be an operating-system process. A **run** is a separately launched program with its own root owner. These identities are distinct from the process currently hosting the embedded runtime.

Ordinary synchronous calls share their caller's activation. Returning from a helper function therefore does not close the activation. Returning from an activation root does. The REPL session is a longer-lived owner: submitting another prompt does not end that session.

<span id="ORNA-CONCUR-001"></span>**ORNA-CONCUR-001** Every asynchronous child MUST have exactly one activation or session owner. On normal completion, failure, cancellation or loss of that owner, the runtime MUST request cancellation of all unfinished children and join their termination before marking the owner terminal. Normal completion does not wait for unfinished children to complete their intended jobs; it stops them and waits for cleanup.

The ownership rule is lexical at the activation boundary, not based on whether a handle remains reachable. Keeping or discarding an `InvocationHandle` does not detach, reparent or cancel its invocation. A child cannot extend its owner's lifetime by retaining a reference to it.

### 10.4.1 Starting and awaiting {#execution-starting-and-awaiting}

`sys.start` returns after validating the target and arguments, allocating the invocation, assigning its owner, and accepting responsibility for termination. Its default transaction mode is `separate`. `read_only` is also permitted. `inherit` fails with `sys.invoke.transaction_mode`: an asynchronously scheduled child may not concurrently use its parent's transaction. Synchronous `sys.invoke` may use `inherit`.

A direct REPL expression or direct binding whose root call is `sys.start(...)`, `sys.admin.retry_failure(...)` or `sys.admin.replay_failure(...)` assigns the invocation to the REPL session. A `sys.start` reached while executing another operation belongs to that operation's activation, including when reached through ordinary helper functions. Host APIs that submit a direct start explicitly identify the session owner; they cannot infer ownership from a later stored handle.

For example, these two REPL inputs may be submitted separately:

```orna
let job = sys.start(worker, arguments, as: Int);
sys.await(job)
```

Here `worker` and `arguments` are already resolved typed values. The invocation remains session-owned between inputs. By contrast, a function that starts work and returns without awaiting it closes its activation by cancelling the unfinished work.

`sys.await` does not transfer ownership. It returns a complete `sys.InvocationResult<T>` when the target terminates. Expiry of its timeout raises `sys.invoke.await_timeout`; it does not itself cancel the target. If that unhandled failure subsequently ends the owner, owner termination cancels its children for that separate reason. A successful result containing an optional `T` retains the outer success wrapper, so success with `null` is distinguishable from absence of a result.

### 10.4.2 Ownership termination algorithm TASK-END-1 {#execution-ownership-termination-algorithm-task-end-1}

1. Stop admitting new children under the ending owner. A racing start either joins the recorded child set or fails before executing; it cannot become ownerless.
2. Record the owner's requested completion: success with a value, ordinary failure, or cancellation.
3. Request cancellation of every child that has not already reached a terminal state. Repeat this recursively through their owned descendants.
4. Join each child's termination and release activation-owned resources. Cancellation checks occur at bounded VM, loop, stream and host-call boundaries. An adapter that cannot interrupt an external call must isolate it and prevent it from making further Orna state changes after its lease is revoked.
5. Roll back still-open child transactions. A child's transaction that committed before cancellation remains committed. Cancellation is not history reversal.
6. Complete the owner's own transaction: validate and commit on success, or roll back on failure or cancellation. No child sharing mutable activation resources remains active at this point.
7. Publish terminal invocation and task metadata and release the owner lease. A late completion from a fenced child cannot replace the terminal result or commit another transaction.

<span id="ORNA-TASK-001"></span>**ORNA-TASK-001** A child MUST NOT commit after the runtime has acknowledged its cancellation and terminal state. Lease fencing and transaction generation checks MUST enforce this when execution is hosted in a different process.

<span id="ORNA-TASK-002"></span>**ORNA-TASK-002** A graceful REPL close MUST cancel session-owned invocations and watches without terminating unrelated sessions or independently launched runs in the same runtime.

<span id="ORNA-TASK-003"></span>**ORNA-TASK-003** A hard process termination MUST NOT be described as executing cleanup in the dying process. A surviving runtime cancels work after loss of its owner lease; a restarted runtime recovers or rolls back open transactions before admitting writes. Committed changes remain durable.

<span id="ORNA-TASK-004"></span>**ORNA-TASK-004** The runtime MUST expose bounded cancellation checkpoints and adapter cancellation limitations. A task stuck in untrusted or noncooperative code cannot retain a writable transaction after its ownership lease has been revoked. An implementation may terminate an isolated worker to enforce this rule.

### 10.4.3 Concurrent combinators {#execution-concurrent-combinators}

`std.concurrent` supplies ordinary functions rather than new control-flow syntax. Callback arrays must have one common successful result type; heterogeneous results require an explicitly declared record or enum.

<span id="ORNA-CONCUR-002"></span>**ORNA-CONCUR-002** `parallel` MUST return successful child results in input order, independent of completion order.

<span id="ORNA-CONCUR-003"></span>**ORNA-CONCUR-003** Each `parallel` child is a separate activation and transaction. A successful child's committed writes MUST NOT be rolled back merely because another child fails.

<span id="ORNA-CONCUR-004"></span>**ORNA-CONCUR-004** Programs requiring one atomic database update SHOULD perform concurrent read-only or external work first, then apply the combined table mutations in the parent activation.

<span id="ORNA-CONCUR-005"></span>**ORNA-CONCUR-005** `race` MUST return the first observed successful result, cancel and join all losers, and choose the lowest input-index ordinary failure if every child fails. Simultaneous successes use the runtime's recorded completion order; no wall-clock ordering between simultaneous events is promised.

<span id="ORNA-CONCUR-006"></span>**ORNA-CONCUR-006** `timeout` MUST request cancellation and join its child before failing with its timeout diagnostic. It never returns a successful partial value.

For `parallel`, the first observed failure cancels unfinished siblings. After joining, the lowest input-index ordinary failure among failed children is the reported primary cause; other failures are attached as ordered causes. Cancellation requested by the parent remains cancellation and is not converted into an ordinary aggregate failure. An empty `parallel` returns `[]`; an empty `race` fails without scheduling work. Negative timeouts are argument errors; zero timeouts check already-terminal results before cancelling unfinished work.

## 10.5 Cancellation and bounded streams {#execution-cancellation-and-bounded-streams}

<span id="ORNA-CANCEL-001"></span>**ORNA-CANCEL-001** Cancellation is distinct from ordinary failure and MUST roll back the interrupted activation's open transaction. `|?` cannot catch it.

<span id="ORNA-CANCEL-002"></span>**ORNA-CANCEL-002** Interpreters, VMs, loops and stream operators MUST check cancellation at bounded execution points.

<span id="ORNA-BACKPRESSURE-001"></span>**ORNA-BACKPRESSURE-001** Stream channels MUST be bounded. A source that cannot be backpressured MUST declare or receive an explicit buffer, drop or fail policy before use.

<span id="ORNA-BACKPRESSURE-002"></span>**ORNA-BACKPRESSURE-002** `for_each` is sequential by default. Concurrent processing MUST be explicit, bounded and consistent with the connector's partition-order and checkpoint rules.

Cancelling a stream callback rolls back both its writes and its checkpoint advance. Stopping the surrounding run prevents admission of the next delivery. Neither action undoes previously committed deliveries.



# 11. Stream programs and consumer identity {#streams}

## 11.1 Producing and consuming streams {#streams-producing-and-consuming-streams}

A stream is a sequence whose next value may require waiting or external work. A relation is a queryable value over a selected database state. Neither starts merely because its type or module is loaded.

A finite, self-contained example uses a list-backed source:

```orna
pub fn ingest(samples: Stream<Sample>) =
    samples | for_each(sample => {
        Reading.insert({
            sensor: sample.sensor,
            sequence: sample.sequence,
            value: sample.value,
        });
    });
```

The [sensor project](#examples) defines `Sample`, `Reading` and an offline source with a replay position. Each callback is its own activation; the function returns when the finite source closes. An unbounded source keeps the function running until closure, failure or cancellation.

`orna run sensors.ingest` requires its parameters to be supplied through the ordinary invocation binding rules. A zero-argument `main` wrapper may select configuration explicitly. Naming a file `ingest.orna` has no execution effect.

Parallel consumers use ordinary function values when no arguments are captured. A wrapper lambda is needed only to bind arguments or add work. A consumer function has one checkpointed source root; a coordinator may call several separately named consumers, each with its own identity and lease.

## 11.2 Durable consumer identity {#streams-durable-consumer-identity}

A checkpoint belongs to a durable consumer identity, not a process ID or source line.

Version 1.0 limits one durable consumer function to one checkpointed source root.

<span id="ORNA-CONSUMER-001"></span>**ORNA-CONSUMER-001** Consumer identity is derived from database identity, the stable ObjectId of the invoked public function and canonical invocation arguments.

<span id="ORNA-CONSUMER-002"></span>**ORNA-CONSUMER-002** The connector additionally supplies source identity, partition identity and position-format version. Secret plaintext MUST NOT participate in identity; a stable secret reference name may.

<span id="ORNA-CONSUMER-003"></span>**ORNA-CONSUMER-003** Function code hashes are revisions, not identity. Editing or semantically renaming the function continues from the same checkpoint.

<span id="ORNA-CONSUMER-004"></span>**ORNA-CONSUMER-004** Deleting and recreating a function creates a new consumer identity even when the spelling is reused.

<span id="ORNA-CONSUMER-005"></span>**ORNA-CONSUMER-005** A durable consumer function containing more than one checkpointed source root MUST produce a diagnostic instructing the author to extract separate named consumer functions.

Stateful map/filter logic is replayed from the source in version 1.0. Stateful windows/joins either persist their state in ordinary tables or remain restart-recomputable.

## 11.3 Source identity {#streams-source-identity}

A connector declares whether it is finite/unbounded and replayable, and supplies stable source identity, partition, position format/version, resume behavior and failed-payload preservation/refetch behavior.

<span id="ORNA-CONNECTOR-001"></span>**ORNA-CONNECTOR-001** Source identity MUST be based on stable semantic configuration, not memory address, task order or AST position.

## 11.4 Duplicate consumers {#streams-duplicate-consumers}

<span id="ORNA-CONSUMER-006"></span>**ORNA-CONSUMER-006** One clone MUST prevent two local processes from concurrently owning the same consumer identity and partition.

<span id="ORNA-CONSUMER-007"></span>**ORNA-CONSUMER-007** Running the same consumer against the same source from independent writable clones is outside the base version 1.0 guarantee unless the connector explicitly defines partitioning or idempotence semantics.

<span id="ORNA-CONSUMER-008"></span>**ORNA-CONSUMER-008** On merge, equal checkpoints merge. Opaque divergent positions create `sys.CheckpointConflict`; Orna MUST NOT choose a seemingly greatest opaque token.

<span id="ORNA-CONSUMER-009"></span>**ORNA-CONSUMER-009** Base Orna cannot detect concurrent ownership of the same consumer in independent clones before they synchronize, because each clone has independent local state and no coordinator. No cross-clone diagnostic is guaranteed. A later checkpoint conflict MAY reveal divergence, but duplicate source reads or external effects may already have occurred.


## 11.5 Consumption requirements {#streams-consumption-requirements}

<span id="ORNA-STREAM-001"></span>**ORNA-STREAM-001** Importing or loading a module MUST NOT start a stream.

<span id="ORNA-STREAM-002"></span>**ORNA-STREAM-002** A finite stream consumer returns when the source is exhausted.

<span id="ORNA-STREAM-003"></span>**ORNA-STREAM-003** An unbounded stream consumer remains running until cancelled, failed or closed.

<span id="ORNA-STREAM-004"></span>**ORNA-STREAM-004** Each item or configured ordered batch callback is a separate activation transaction; an unbounded consumer MUST NOT hold one transaction for its lifetime.

<span id="ORNA-STREAM-005"></span>**ORNA-STREAM-005** Function values and anonymous functions have identical call semantics when used as stream/concurrency callbacks; wrapper lambdas MUST NOT be required where no arguments are captured.


# 12. Checkpoints and failed deliveries {#checkpoints}

A checkpoint records how far one durable consumer has committed. A failure record describes one preserved delivery that could not be processed. Neither is identified by a process ID, a display label or an incrementing retry number.

## 12.1 Checkpoint identity and position {#checkpoints-checkpoint-identity-and-position}

<span id="ORNA-CP-001"></span>**ORNA-CP-001** Current checkpoints MUST be stored transactionally in Turso with the table writes produced by the corresponding item or batch.

<span id="ORNA-CP-002"></span>**ORNA-CP-002** Publication MUST include checkpoint watermarks corresponding exactly to the included rows. It cannot publish a later watermark while omitting writes required by that watermark.

<span id="ORNA-CP-003"></span>**ORNA-CP-003** `sys.Checkpoint` exposes current checkpoint state and committed historical watermarks through snapshot selection.

<span id="ORNA-CP-004"></span>**ORNA-CP-004** A position-format change MUST carry a version. Incompatible formats require explicit migration, reset or adoption; bytewise or textual ordering cannot substitute for a provider-defined comparator.

A checkpoint row is keyed by `(consumer_identity, source_identity, partition)`. `sys.ConsumerIdentity` includes the database, stable consumer function identity and canonical bound arguments. Its key therefore distinguishes two runs of the same function with different source configurations. `partition = null` denotes the single unpartitioned source; it is not an empty-string partition.

`sys.CheckpointPosition` retains the provider and format identity with its opaque bytes. Generic tooling can compare two positions for equality only when those identities agree. It cannot infer that a numerically or lexicographically larger token represents later progress.

`sys.consumer_identity(function, arguments)` computes the durable identity. `sys.checkpoint(consumer_identity, source_identity, partition: ...)` retrieves the corresponding row or `null`. A bare function value, a `sys.FunctionRef`, a display name and a consumer identity are not interchangeable.

## 12.2 Processing algorithm DELIVERY-1 {#checkpoints-processing-algorithm-delivery-1}

1. Obtain the durable consumer lease and load its current checkpoint/version. A newly registered consumer obtains the provider's initial position and an initial version before reading its first delivery.
2. Fetch an item or bounded ordered batch outside the write transaction. Record its source identity, partition, delivery position and any provider-defined successor position. Preserve enough payload or protected refetch information to recover the delivery if processing fails.
3. Begin an activation against one CWD generation. Invoke the handler; all Orna table writes belong to this activation. Validate references and assertions.
4. Compare the current checkpoint with the expected version and starting position. Atomically commit handler writes and the new checkpoint. A concurrent reset or owner change fails the comparison and rolls the activation back.
5. On ordinary failure, roll back handler writes and checkpoint movement. In a separate short metadata transaction, create or update the single failure record for this delivery and pause further ordered delivery admission.
6. On cancellation, roll back the open activation. A cancellation alone does not manufacture an ordinary processing error. If a retry record already exists, release its execution lease and return it to the appropriate recoverable state after rollback is established.

<span id="ORNA-CP-005"></span>**ORNA-CP-005** Activation failure MUST roll back both table writes and checkpoint advancement.

<span id="ORNA-CP-006"></span>**ORNA-CP-006** An ordered consumer MUST NOT advance beyond a failed delivery unless that exact delivery is explicitly skipped with a provider-supported successor.

## 12.3 One record per delivery {#checkpoints-one-record-per-delivery}

<span id="ORNA-FAIL-001"></span>**ORNA-FAIL-001** Repeated processing failures for the same delivery MUST update one durable failure record. Retry number is state, not identity; failed retries MUST NOT append successor failure rows.

<span id="ORNA-FAIL-002"></span>**ORNA-FAIL-002** A retry limit MUST NOT silently discard a delivery. The source remains paused, or an explicit bounded retry policy retries the same delivery.

<span id="ORNA-FAIL-003"></span>**ORNA-FAIL-003** Explicit skip MUST atomically preserve the delivery's payload or protected refetch reference, update the existing failure record to `skipped` and advance the checkpoint past exactly that delivery. If no failure record exists, the same operation creates that one record; it does not create a second identity for an already recorded delivery.

<span id="ORNA-FAIL-004"></span>**ORNA-FAIL-004** Replay of a skipped delivery runs the current compatible handler on preserved/refetched data without rewinding or advancing the live checkpoint.

<span id="ORNA-FAIL-005"></span>**ORNA-FAIL-005** Failure identity MUST be the exact composite `(consumer_identity, source_identity, partition, position_format, position)`. A position digest is an index accelerator only; equality still verifies the complete typed position. `sys.FailureRef` encodes that natural identity and does not add a synthetic delivery ID.

The row has `version: sys.FailureVersion`, `attempt_count`, latest safe `error`, timestamps, payload/refetch metadata, checkpoint preconditions and current status. `attempt_count` counts admitted processing attempts, including an attempt interrupted before its external call completed. It is not a promise about exactly how many times an external provider observed an effect. Each durable state transition changes `version`.

Optional bounded trace records may describe individual attempts. They are not authoritative failure identities, cannot grow without a retention policy and cannot be required to locate the current blocked delivery.

## 12.4 State machine {#checkpoints-state-machine}

| Starting state | Operation | Result |
|---|---|---|
| No row | Handler fails | One `open` row, attempt count 1, unchanged checkpoint. |
| `open` | Accept retry under version/checkpoint CAS | Same row becomes `retrying`; count increases; one execution lease. |
| `retrying` | Handler succeeds | Writes, checkpoint movement and `recovered` transition commit together. |
| `retrying` | Handler fails or is cancelled | Writes roll back; same row returns to `open`; checkpoint unchanged. |
| `open` | Authorised skip | Same row becomes `skipped`; preserve delivery and advance checkpoint atomically. |
| `skipped` | Accept replay | Same row becomes `replaying`; live checkpoint is not involved. |
| `replaying` | Handler succeeds | Replay writes and `replayed` transition commit together; live checkpoint unchanged. |
| `replaying` | Handler fails or is cancelled | Replay writes roll back; same row returns to `skipped`. |
| `skipped` or `replayed` | Acknowledge | Same row becomes `resolved`; no table replay or checkpoint movement. |

`recovered` and `resolved` are terminal administration states. A later explicit source reset may cause the same delivery to be encountered again; its stable identity remains the same, and a newly failed processing cycle reopens the row under a new version rather than inventing a new key. Its prior successful audit metadata remains in bounded invocation/change history. A transition cannot be inferred solely from the display status; the version is always checked.

<span id="ORNA-SYS-061"></span>**ORNA-SYS-061** Successful item/batch writes and checkpoint movement MUST commit atomically.

<span id="ORNA-SYS-062"></span>**ORNA-SYS-062** Failure or cancellation MUST leave the committed checkpoint unchanged and roll back the interrupted handler's writes.

<span id="ORNA-SYS-063"></span>**ORNA-SYS-063** Checkpoint changes MUST compare both the expected checkpoint version and typed position.

<span id="ORNA-SYS-064"></span>**ORNA-SYS-064** Stale checkpoint preconditions MUST fail with `sys.checkpoint.conflict`. Stale failure-row versions MUST fail with `sys.failure.stale_version`. Neither failure permits handler execution or progress movement.

<span id="ORNA-SYS-065"></span>**ORNA-SYS-065** A replayable provider promises resumption only under its declared delivery contract; this MUST NOT be represented as exactly-once effects in arbitrary external systems.

<span id="ORNA-SYS-066"></span>**ORNA-SYS-066** Payload retention MUST obey the secret/redaction rules. A protected reference and digest may replace plaintext retention, but the exact source position remains part of delivery identity.

<span id="ORNA-SYS-121"></span>**ORNA-SYS-121** `retry_failure` MUST compare the failure's expected version/status and the blocking checkpoint precondition, acquire the sole delivery lease, and durably mark the same row `retrying` before user code executes.

<span id="ORNA-SYS-122"></span>**ORNA-SYS-122** A successful retry MUST atomically commit its writes, checkpoint movement and `recovered` state. A failed retry MUST keep the same failure identity, leave the checkpoint unchanged, update the diagnostic/version and return the row to `open` after rollback.

<span id="ORNA-SYS-123"></span>**ORNA-SYS-123** `replay_failure` MUST execute preserved nonblocking data in a separate activation. It MUST NOT move the live checkpoint; failure returns the row to `skipped`, and success commits its writes with the `replayed` transition.

<span id="ORNA-SYS-124"></span>**ORNA-SYS-124** `resolve_failure` MUST NOT acknowledge away a blocking `open` or `retrying` record. Only successful processing or an explicit atomic skip can clear that progress barrier.

## 12.5 Administration and stale tools {#checkpoints-administration-and-stale-tools}

Every retry, skip, replay and resolve call supplies `expected_version`. Status alone is insufficient: a row may go from `open` to `retrying` and back to `open` while a tool still holds its earlier observation.

A safe interaction reads a typed failure row, displays the intended action and submits its reference and version. If the row has changed, the tool refreshes it and asks for new operator intent where the action is destructive; it does not automatically retry a stale skip with newer preconditions.

Checkpoint reset requires a paused consumer, no in-flight callback and a matching expected version/position. A current blocking failure must first be resolved by retry or skip; reset cannot turn a blocking error into an unrecorded discard. A format-incompatible reset is rejected. The audit row records the old/new typed positions and reason, with secrets redacted.

If the runtime crashes while an attempt is marked `retrying` or `replaying`, recovery first establishes that its invocation transaction did not commit. It then fences the expired lease and returns the same record to `open` or `skipped`. If commit did occur, recovery completes the matching terminal metadata instead. It never starts a second callback while the first can still commit.

## 12.6 Recovery between clones {#checkpoints-recovery-between-clones}

<span id="ORNA-CP-007"></span>**ORNA-CP-007** Restart on the same worktree MUST resume from its durable Turso checkpoint.

<span id="ORNA-CP-008"></span>**ORNA-CP-008** A different clone MUST resume from the checkpoint present in its selected fetched snapshot, not a guessed local copy of another clone's tail.

<span id="ORNA-CP-009"></span>**ORNA-CP-009** Recovery of unpublished data after machine loss requires replayable source data or a separately preserved local tail. Git replication alone does not contain unpublished state.



# 13. Inspection, display, presentation and codecs {#presentation}

## 13.1 Four distinct concepts {#presentation-four-distinct-concepts}

Orna distinguishes:

1. **Inspect** - structural developer representation.
2. **Display** - friendly human text.
3. **Present** - rich typed rendering tree.
4. **Codec** - stable typed serialization/deserialization.

<span id="ORNA-PRES-001"></span>**ORNA-PRES-001** Implementations MUST NOT treat terminal presentation as canonical persistence encoding.

## 13.2 Inspect {#presentation-inspect}

Every value has an automatically derived structural fallback.

```text
Money<GBP> {
    coefficient: 1234,
    scale: 2,
}
```

<span id="ORNA-PRES-002"></span>**ORNA-PRES-002** Inspect MUST expose type and structure, may truncate large collections, and MUST redact secret values.

## 13.3 Display {#presentation-display}

Display is friendly text:

```text
£12.34
2h 14m
Alice Smith
```

<span id="ORNA-PRES-003"></span>**ORNA-PRES-003** Display MAY be lossy and contextual.

<span id="ORNA-PRES-004"></span>**ORNA-PRES-004** Changing Display MUST NOT change equality, hashing, storage, Git object IDs, query results or codec output.

## 13.4 Dynamic display formatters {#presentation-dynamic-display-formatters}

Formatter values may depend on locale, time zone, precision, width and unit preferences.

```text
std.time.duration.compact.format(duration) // "2h 14m"
std.time.duration.clock.format(duration)   // "02:14:00"
std.time.duration.words.format(duration)   // "2 hours, 14 minutes"
std.time.duration.iso.format(duration)     // "PT2H14M"
```

A REPL session may override defaults:

```text
:display Duration std.time.duration.clock
:display Duration default
```

A page may override presentation locally:

```orna
Table(rows, columns: {
    duration: Column(display: std.time.duration.clock),
})
```

<span id="ORNA-PRES-005"></span>**ORNA-PRES-005** Session display overrides MUST be ephemeral unless the user explicitly saves presentation settings.

## 13.5 User-defined Inspect, Display and Present {#presentation-user-defined-inspect-display-and-present}

Version 1.0 defines the same minimal protocol mechanism used by ordinary generic code:

```orna
pub type FriendlyDuration = Duration {
    impl Display {
        fn display(self, context: DisplayContext): Str =
            std.time.duration.compact(self, context);
    }
}

pub table Contact(id: Uuid) {
    name: Str,
    emails: [Str],

    impl Present {
        fn present(self, context: PresentContext): PresentTree =
            std.ui.Details([
                ("Name", self.name),
                ("Emails", self.emails),
            ]);
    }
}
```

<span id="ORNA-PRES-006"></span>**ORNA-PRES-006** Every value has a host-derived, cycle-safe and bounded Inspect fallback.

<span id="ORNA-PRES-007"></span>**ORNA-PRES-007** Display returns human-oriented `Str`; Present returns the core typed presentation tree.

<span id="ORNA-PRES-008"></span>**ORNA-PRES-008** Display/Present implementations are read-only, deterministic relative to their immutable context and subject to time/instruction/output budgets. Database writes, connectors, network, filesystem, process execution, secret reveal and nondeterministic time/randomness are unavailable.

<span id="ORNA-PRES-009"></span>**ORNA-PRES-009** A presenter failure falls back to Inspect with a diagnostic.

## 13.6 Present {#presentation-present}

Present returns a typed tree that may represent tables, objects, source, diffs, charts and custom widgets.

Default selection order:

```text
explicit presenter
-> session/page presentation override
-> type Present
-> type Display
-> derived Inspect
```

<span id="ORNA-PRES-010"></span>**ORNA-PRES-010** A renderer that does not recognize a rich node MUST fall back to an Inspect-compatible representation rather than fail to show the value.

## 13.7 Codecs {#presentation-codecs}

Codecs own their operations:

```text
std.encoding.json.encode(value)
std.encoding.json.decode(text, as: Contact)

std.encoding.orna.encode(value)
std.encoding.orna.decode(text, as: Contact)
```

<span id="ORNA-CODEC-001"></span>**ORNA-CODEC-001** A codec MUST provide typed `encode(value)` and `decode(input, as: T)` operations.

<span id="ORNA-CODEC-002"></span>**ORNA-CODEC-002** Decode MUST produce `T` on success or fail with a `DecodeError` preserving format location/path information. It MUST NOT wrap the successful value in `Result`.

<span id="ORNA-CODEC-003"></span>**ORNA-CODEC-003** Canonical Orna encoding MUST be deterministic, full-precision, unambiguous and round-trippable for supported values.

## 13.8 Row-body encoding versus standalone encoding {#presentation-row-body-encoding-versus-standalone-encoding}

A loose row file omits its key because the key is in the path.

Standalone encoding of the logical row includes the key:

```orna
{
    id: "alice-smith",
    name: "Alice Smith",
    emails: ["alice@example.com"],
}
```

<span id="ORNA-CODEC-004"></span>**ORNA-CODEC-004** Generic value encoding of a table row MUST include all logical fields, including primary-key fields.

<span id="ORNA-CODEC-005"></span>**ORNA-CODEC-005** Loading a row unit MUST combine table schema, decoded path key and decoded row body.

<span id="ORNA-CODEC-006"></span>**ORNA-CODEC-006** Exact checked-out source text MAY differ from canonical re-encoding because comments and formatting may be preserved. Exact source is obtained through `sys.File`, not by encoding the semantic value.



# 14. Pages and live views {#pages}

## 14.1 Pages as ordinary values {#pages-pages-as-ordinary-values}

```text
A page function returns a Page value.
The Page's view callback returns a Present tree.
An action callback performs one ordinary activation.
```

<span id="ORNA-PAGE-001"></span>**ORNA-PAGE-001** Pages MUST be ordinary values returned by functions, not a separate component declaration grammar.

<span id="ORNA-PAGE-002"></span>**ORNA-PAGE-002** Widgets and layouts MUST be ordinary values that can be composed by normal functions.

## 14.2 Watch model {#pages-watch-model}

Every connected page root is a watch over a typed value expression.

```text
CWD/system change
-> dependency graph finds affected watches
-> re-evaluate affected expression/subtree
-> compare old/new presentation trees
-> emit contextual delta
```

<span id="ORNA-LIVE-001"></span>**ORNA-LIVE-001** The same watch and delta mechanism MUST support tables, charts, text, maps, Git views, runtime views and custom widgets.

<span id="ORNA-LIVE-002"></span>**ORNA-LIVE-002** If a fine-grained delta cannot be derived, the runtime MUST fall back to replacing the nearest stable presentation subtree. There MUST be no data type for which live refresh is impossible merely because a specialized delta is absent.

## 14.3 Stable presentation identity {#pages-stable-presentation-identity}

- record children are keyed by field name;
- relation rows are keyed by primary key;
- UI nodes use explicit keys when supplied and otherwise a stable call-site-derived key;
- unkeyed lists use position and may require subtree replacement.

<span id="ORNA-LIVE-003"></span>**ORNA-LIVE-003** A renderer MUST apply deltas in sequence order.

<span id="ORNA-LIVE-004"></span>**ORNA-LIVE-004** A client detecting a missing base revision MUST request or accept a complete resynchronization snapshot.

## 14.4 Live transport and programmable clients {#pages-live-transport-and-programmable-clients}

The required live transport profile is the WebSocket subprotocol `orna.present.v1` with deterministic CBOR messages. This is implementation terminology and is absent from normal user-facing output.

<span id="ORNA-WIRE-001"></span>**ORNA-WIRE-001** Every watch begins with a complete Present-tree snapshot. Deltas are an optimization; replacement of the nearest stable subtree, including the root, is the universal correctness fallback.

<span id="ORNA-WIRE-002"></span>**ORNA-WIRE-002** Each watch has a server-issued stable identifier and a monotonically increasing revision. A delta names its base revision and new revision. A client MUST apply the complete ordered patch atomically only when the base equals its current revision; otherwise it MUST discard the patch and obtain a complete snapshot.

<span id="ORNA-WIRE-003"></span>**ORNA-WIRE-003** The patch vocabulary is exactly `add`, `remove`, `replace` and `move`. Paths are typed sequences of record fields, relation keys and list indexes. A root replacement uses the empty path.

<span id="ORNA-WIRE-004"></span>**ORNA-WIRE-004** Record children use field identity, relation rows use table `ObjectId` plus primary key, explicitly keyed UI children use that key, and unkeyed lists use position. Missing stable identity affects efficiency only; replacement MUST remain correct for every value.

<span id="ORNA-WIRE-005"></span>**ORNA-WIRE-005** Connection queues, decoded message size, collection size, nesting depth and decompressed bytes MUST be bounded. A slow client MAY skip intermediate visual states and receive the newest complete snapshot. Database rows are not lost by visual coalescing.

<span id="ORNA-WIRE-006"></span>**ORNA-WIRE-006** Reconnection does not require retained delta history. The client resubscribes and receives a complete current snapshot.

### 14.4.1 Page actions {#pages-page-actions}

Normal page controls use page-produced action handles. An event contains the watch, page revision, action handle, request identity and typed input; it never relies on source text copied from the rendered page.

<span id="ORNA-WIRE-007"></span>**ORNA-WIRE-007** A stale or unknown action handle MUST NOT execute. The host returns the current snapshot or a user-readable stale-view result.

<span id="ORNA-WIRE-008"></span>**ORNA-WIRE-008** One action is one normal activation transaction. Successful writes commit together and invalidate watches; an escaping error or cancellation rolls them back.

### 14.4.2 Explicit remote Orna evaluation {#pages-explicit-remote-orna-evaluation}

A trusted programmable client, browser developer console, LLM or remote CLI MAY intentionally submit Orna source.

<span id="ORNA-EVAL-001"></span>**ORNA-EVAL-001** Source is executable only inside an explicit `eval` or `watch` operation. Ordinary strings, fields, page events and protocol values MUST never be implicitly parsed as Orna.

<span id="ORNA-EVAL-002"></span>**ORNA-EVAL-002** The host MUST parse, resolve, type-check and execute submitted source with the same language implementation, module graph, activation semantics, diagnostics and presentation system as the local REPL. A client-supplied AST, bytecode or query plan is never trusted as authoritative.

<span id="ORNA-EVAL-003"></span>**ORNA-EVAL-003** `eval` accepts one REPL input and may read or mutate the served clone's CWD. It is one activation transaction.

<span id="ORNA-EVAL-004"></span>**ORNA-EVAL-004** `watch` accepts an expression and remains live through the universal watch mechanism. A watch expression may read tables and `sys`, call deterministic read-only functions and use activation time. It MUST NOT mutate tables, reveal secrets, open connectors, perform network/filesystem/process operations or use randomness. The host derives this from the resolved call graph; no user effect annotation is required.

<span id="ORNA-EVAL-005"></span>**ORNA-EVAL-005** A time-dependent watch such as `now() - 1.min` is permitted and records an explicit clock dependency so it is reevaluated at the required boundary.

<span id="ORNA-EVAL-006"></span>**ORNA-EVAL-006** Remote REPL sessions use the same ephemeral-module model as the local REPL: imports, bindings and declared helper functions persist for the session, and wildcard imports follow ordinary language rules.

### 14.4.3 Request recovery {#pages-request-recovery}

<span id="ORNA-EVAL-007"></span>**ORNA-EVAL-007** Every mutating remote action or `eval` carries a client-generated 128-bit request identity. The host binds that identity to a canonical fingerprint of the operation.

<span id="ORNA-EVAL-008"></span>**ORNA-EVAL-008** A request's terminal claim, successful Orna-controlled writes and compact terminal outcome MUST commit together. Durable admission reservations are distinct; [REQUEST-1](#protocol) defines crash and uncertain-external-effect handling. A matching completed identity returns its recorded outcome rather than executing again.

<span id="ORNA-EVAL-009"></span>**ORNA-EVAL-009** Reusing a request identity for different source, action or typed input MUST be rejected.

<span id="ORNA-EVAL-010"></span>**ORNA-EVAL-010** The compact request identity/fingerprint/outcome record is durable database runtime metadata and remains reserved. Rich result presentation may be discarded according to documented retention; loss of the rich result MUST NOT permit re-execution under the same identity.

<span id="ORNA-EVAL-011"></span>**ORNA-EVAL-011** The deduplication guarantee covers Orna-controlled database writes only. External effects cannot be made transactional. Clients MUST NOT automatically retry a mutating evaluation after uncertain delivery; they recover its recorded status or inspect current state.

### 14.4.4 Wire messages and values {#pages-wire-messages-and-values}

Client operations are `subscribe`, `unsubscribe`, `resync`, `event`, `eval`, `watch`, `cancel` and `request_status`. Host operations are `snapshot`, `delta`, `result`, `diagnostic` and `request_status_result`. WebSocket control frames provide connection close, ping and pong.

<span id="ORNA-WIRE-009"></span>**ORNA-WIRE-009** The binary envelope, message fields, deterministic CBOR rules and exact encodings for `Decimal`, `Money`, UUID/ObjectId, dates, instants, duration, quantities, enums, typed records, references, diagnostics and Present nodes are defined in `profiles/live-protocol.md` and form part of repository/server conformance.

<span id="ORNA-WIRE-010"></span>**ORNA-WIRE-010** Secret values are not wire-encodable. A redacted secret handle may be presented only as non-revealing metadata.

<span id="ORNA-WIRE-011"></span>**ORNA-WIRE-011** WebSocket compression is optional and cannot change semantics. Normal same-origin checks and the trusted authenticated VPN/SSH/reverse-proxy boundary apply; core Orna does not add a grants database.

<span id="ORNA-WIRE-012"></span>**ORNA-WIRE-012** Protocol failures are rendered normally as user concepts such as “the live view was refreshed” or “this operation may already have completed.” Raw CBOR, revision or framing details appear only in explicit developer inspection.

A production server-profile claim requires independent client/server interoperability, loss/reorder/reconnect simulation, malformed-input fuzzing, bounded slow-client tests and equality with the reference vectors. These are evidence obligations for an implementation, not unresolved semantics.



# 15. The system model {#system}

## 15.1 Role and namespace {#system-role-and-namespace}

`sys` is the implementation-provided model of the attached database and its execution. Unlike [the standard library](#standard-library), it is not an importable replacement package. Its rows and results are ordinary typed values; its relation handles are read-only query sources.

Use `sys.catalog` for declarations, `sys.history` and `sys.git` for retained history, `sys.rt` for live execution, `sys.storage` for physical placement, and `sys.admin` for explicit state changes. The [complete reference](#system-reference) lists every member, field, enum and signature. The [administrative contract](#administration) defines mutation preconditions.

The familiar query form works for inspection:

```orna
sys.catalog.objects
    | map(object => object.qualified_name)
```

This example requires only a database with catalogue metadata. It returns semantic object names, not executable function values. As with application relations, an explicit sort is needed when output order matters.

<span id="ORNA-SYS-001"></span>**ORNA-SYS-001** Structural references within `sys` MUST use typed references where possible, not unstructured path strings.

<span id="ORNA-SYS-002"></span>**ORNA-SYS-002** Every Orna 1.0 implementation MUST provide the complete portable `sys` surface applicable to each conformance class it claims.

<span id="ORNA-SYS-003"></span>**ORNA-SYS-003** User source MUST NOT declare, replace, shadow or monkey-patch the root `sys` namespace or any portable member specified in this chapter.

<span id="ORNA-SYS-004"></span>**ORNA-SYS-004** A vendor extension under `sys.vendor.<vendor>` MUST NOT change portable row identity, field meaning, ordering, failure codes, transaction boundaries or redaction rules.

<span id="ORNA-SYS-005"></span>**ORNA-SYS-005** The unrecognised member `sys.runtime` MUST produce `ORNA100-E-SYS-RUNTIME` with `sys.rt` as the suggested spelling. It is not an alias.

<span id="ORNA-SYS-010"></span>**ORNA-SYS-010** Portable names are case-sensitive. `sys.Table`, `sys.catalog.tables` and `sys.table` are not interchangeable spellings unless this specification explicitly defines them.

## 15.2 Compatibility and observations {#system-compatibility-and-observations}

`sys.rt.info()` returns the exact coordinates declared by `sys.RuntimeInfo`. The selected `std` package is identified by its retained source snapshot, not by assuming that every repository uses the same package release. Unicode and timezone-data coordinates are reported separately where relevant.

A system query can inspect one of five kinds of data:

| Availability | Observation and lifetime |
|---|---|
| Catalogue | Declarations and semantic metadata of the pinned source snapshot. |
| Durable observation | A runtime event deliberately retained in local state or a published snapshot. |
| Local durable | Present in the logical CWD, not necessarily in a Git commit. |
| Live | Current owner/session/operation state; invalid outside that runtime generation. |
| Derived | Computed from available source observations, preserving their pin and redaction. |

A row in an old snapshot saying that a run was running is a historical observation, not evidence that the run is alive now. The runtime view contains only its current live subset. A missing blob is not the same as a known empty relation.

<span id="ORNA-SYS-006"></span>**ORNA-SYS-006** A conforming runtime MUST expose the compatibility fields of `sys.RuntimeInfo` in the [system reference](#system-reference), including exact language, system API, codec, storage and protocol coordinates; it MUST NOT claim a coordinate while omitting its required surface.

<span id="ORNA-SYS-007"></span>**ORNA-SYS-007** Unknown additive fields from a newer compatible `sys` minor version MUST be preserved or ignored safely according to the consumer's decoding mode; they MUST NOT be interpreted as a known field with a different meaning.

<span id="ORNA-SYS-008"></span>**ORNA-SYS-008** A consumer requiring an unavailable `sys` coordinate or profile MUST fail before performing writes.

<span id="ORNA-SYS-009"></span>**ORNA-SYS-009** A grouped catalogue/history handle and its canonical PascalCase relation MUST identify the same rows at the same snapshot. A `sys.rt` handle is the current-runtime live subset of its canonical relation, not a promise that all retained historical rows are live.

<span id="ORNA-SYS-011"></span>**ORNA-SYS-011** A historical system query MUST be snapshot-pinned and MUST NOT silently resolve any referenced object against a newer CWD or `HEAD`.

<span id="ORNA-SYS-012"></span>**ORNA-SYS-012** A historical durable-system query MUST return exactly the observations retained by its snapshot. Known absence is an empty relation; pruned, missing or unhydrated data MUST be reported as unavailable, never substituted with observed emptiness.

<span id="ORNA-SYS-013"></span>**ORNA-SYS-013** A live `sys.rt` relation MUST NOT accept `as_of`. The implementation MUST direct callers to the corresponding durable relation when one exists.

<span id="ORNA-SYS-014"></span>**ORNA-SYS-014** CWD system relations MAY overlay newer local durable observations from Turso, but each row MUST identify whether it is committed, local-only or live-derived.

<span id="ORNA-SYS-015"></span>**ORNA-SYS-015** A historical row that states a run was `running` records the state observed at that snapshot; it MUST NOT imply that the process remains alive.

<span id="ORNA-SYS-022"></span>**ORNA-SYS-022** `sys.database.cwd` MUST remain stable for one evaluation snapshot and MUST change only at a defined transaction/snapshot boundary.

<span id="ORNA-SYS-023"></span>**ORNA-SYS-023** `sys.database.writable` reports whether the current attachment permits Orna-controlled writes; it does not assert that every later write will succeed.

<span id="ORNA-SYS-024"></span>**ORNA-SYS-024** Every function activation MUST observe one stable `sys.current.snapshot` unless it explicitly opens a nested operation whose API documents a new snapshot.

<span id="ORNA-SYS-027"></span>**ORNA-SYS-027** `sys.rt.id` identifies the current local owner/runtime generation and MUST change after an owner restart that invalidates prior live handles.

<span id="ORNA-SYS-028"></span>**ORNA-SYS-028** `sys.rt` MUST be available to every Orna command that opens a database, including direct embedded commands and commands temporarily coordinated through a local owner process.

<span id="ORNA-SYS-029"></span>**ORNA-SYS-029** `orna serve` MUST NOT be treated as the unique or authoritative `sys.rt`; it is one optional client/host mode over the same embedded-first database model.

<span id="ORNA-SYS-030"></span>**ORNA-SYS-030** Reads from `sys.rt` MUST be internally consistent to one observation instant or expose an `observed_at` field allowing the caller to detect a mixed observation.

## 15.3 Identity, references and context {#system-identity-references-and-context}

An object identity answers “which declaration?”; a revision identity answers “which immutable definition?”; a snapshot answers “which database state?”. A reference keeps all context necessary to resolve its target. Renaming a declaration does not casually turn it into an unrelated object.

Every canonical system row has a read-only `.reference`. The reference is not the row itself and is not a mutable proxy. Ordinary source obtains it explicitly. A `sys.FunctionRef` is likewise not a closure and a `sys.ConsumerIdentity` is not a function name.

`sys.current` describes the current activation: snapshot, optional transaction/invocation/run/session/client, locale, timezone, trace and cancellation context. Optional fields use `null` for absence; no fabricated zero identifier stands for “not present”. Singleton views are immutable observations, refreshed only at specified boundaries.

[Snapshot encoding](#formats) distinguishes committed snapshots from exact CWD generations. Serialising a live handle produces a non-operational description. It does not grant the ability to resume an old task after a process restart.

<span id="ORNA-SYS-016"></span>**ORNA-SYS-016** Opaque system identifiers MUST compare by the identity they name, MUST have deterministic canonical encoding and MUST NOT be implicitly interchangeable with `Str`, `Uuid` or another identifier type.

<span id="ORNA-SYS-017"></span>**ORNA-SYS-017** A stable `sys.ObjectId` identifies one semantic object across ordinary edits and semantic renames; a `sys.RevisionId` identifies one immutable revision of that object.

<span id="ORNA-SYS-018"></span>**ORNA-SYS-018** Reusing an identifier for a semantically unrelated object is repository corruption and MUST be diagnosed by verification before publication.

<span id="ORNA-SYS-019"></span>**ORNA-SYS-019** Dereferencing a snapshot-pinned reference MUST use its pinned snapshot unless the caller explicitly asks to re-resolve by stable object identity in another snapshot.

<span id="ORNA-SYS-020"></span>**ORNA-SYS-020** A live handle presented to a different runtime owner MUST fail with `sys.handle.foreign_runtime`; it MUST NOT be treated as an identifier for a coincidentally equal live object.

<span id="ORNA-SYS-021"></span>**ORNA-SYS-021** Canonical serialization of a live handle MUST mark it non-resumable. Decoding it in a later process yields descriptive data, not an operational handle.

<span id="ORNA-SYS-025"></span>**ORNA-SYS-025** Child calls inherit locale, timezone, trace and cancellation context unless an explicit API parameter replaces one of them.

<span id="ORNA-SYS-026"></span>**ORNA-SYS-026** `sys.current.transaction` is present only when the activation participates in an Orna transaction. Its absence MUST NOT be represented by a fabricated zero identifier.

<span id="ORNA-SYS-111"></span>**ORNA-SYS-111** Every canonical system-relation row MUST expose a read-only `reference` property whose type is the generated alias for that row type.

<span id="ORNA-SYS-112"></span>**ORNA-SYS-112** A row reference MUST encode or otherwise preserve the row's natural key and resolution context; for a naturally keyed row such as `sys.Checkpoint` or `sys.Failure`, it MUST NOT introduce a synthetic identifier.

<span id="ORNA-SYS-113"></span>**ORNA-SYS-113** A row value MUST NOT be implicitly converted to a row reference or vice versa. Source uses `row.reference` to obtain a reference and an applicable relation/function to dereference it.

<span id="ORNA-SYS-114"></span>**ORNA-SYS-114** Possession of a reference MUST NOT bypass visibility, retention, redaction, trust, transaction or administrative-precondition checks.

<span id="ORNA-SYS-135"></span>**ORNA-SYS-135** Source spans and maps MUST retain their pinned file/snapshot context and canonical UTF-8 coordinates through diagnostics, source lookup, blame and presentation.

<span id="ORNA-SYS-136"></span>**ORNA-SYS-136** Snapshot selection is represented by explicit overloads for `SnapshotRef`, `CommitRef`, `BranchRef`, `TagRef`, `GitOid` and `Str`; Orna 1.0 defines no implicit `SnapshotLike` union or coercion type.

<span id="ORNA-SYS-137"></span>**ORNA-SYS-137** Singleton view values MUST be immutable observations. Administrative state MUST NOT be changed by field assignment or by mutating a collection obtained from a view.

## 15.4 Catalogue and dependency graph {#system-catalogue-and-dependency-graph}

The catalogue records the fully resolved meaning of source: modules, objects, revisions, fields, parameters, return types, protocols, implementations, table keys, assertions, dimensions and currencies. Inference changes how much users write, not how much metadata implementers publish.

Source identity and semantic identity are separate. Exact source bytes include formatting; semantic hashes omit irrelevant formatting but include all observable declaration meaning. A catalogue row states whether an annotation was authored or inferred.

Dependency edges record their kind and confidence. A table assertion's dependencies are the relations it validates, not only the tables mentioned by spelling in its immediate expression. Dynamic dependencies discovered while evaluating a query are attributed to that activation.

Traversal uses a visited set keyed by object identity and snapshot. `direct` visits one edge layer. `transitive` traverses until no new objects remain, retains the shortest discovered depth, and breaks equal-depth ordering by canonical object identity and edge kind. It never loops on recursive functions or mutually referring types. Exact edge categories and fields are in `sys.Dependency`.

<span id="ORNA-SYS-031"></span>**ORNA-SYS-031** A state transition MUST follow the state machine specified for its row type; direct assignment of a state enum is not an administrative API.

<span id="ORNA-SYS-032"></span>**ORNA-SYS-032** A terminal invocation or run MUST NOT become non-terminal under the same execution identity; another execution has a new invocation/run identity. A durable failed-delivery identity follows the separate, versioned [delivery state machine](#checkpoints) and is preserved across attempts.

<span id="ORNA-SYS-033"></span>**ORNA-SYS-033** `qualified_name` is the name at the row's snapshot, not a timeless property of the stable object identity.

<span id="ORNA-SYS-034"></span>**ORNA-SYS-034** `semantic_hash` MUST exclude non-semantic formatting while `source_hash`, when present, identifies exact retained source bytes.

<span id="ORNA-SYS-036"></span>**ORNA-SYS-036** A generated definition MUST identify its generator/provenance where retained and MUST NOT be misreported as authored source.

<span id="ORNA-SYS-037"></span>**ORNA-SYS-037** Inferred types and inferred failure sets MUST be published in catalogue metadata exactly as if they had been written explicitly.

<span id="ORNA-SYS-038"></span>**ORNA-SYS-038** Catalogue metadata MUST retain whether an annotation was explicit so semantic tools can distinguish an annotation edit from an actual signature change.

<span id="ORNA-SYS-039"></span>**ORNA-SYS-039** Effect metadata MUST be a conservative superset of effects the function may perform; it MUST NOT omit a possible write, external effect or failure merely because an optimiser removed it in one build.

<span id="ORNA-SYS-040"></span>**ORNA-SYS-040** `sys.Table.assertions` MUST include every owner-scoped table assertion in source order and MUST expose the complete relation dependencies used for validation.

<span id="ORNA-SYS-041"></span>**ORNA-SYS-041** `sys.Assertion` MUST represent the contextual `assert` declarations specified by [assertions](#tables). A processor MUST NOT invent separate portable constraint/check/ensure/fact object kinds.

<span id="ORNA-SYS-042"></span>**ORNA-SYS-042** A table row count marked inexact MUST never be used as if it were an exact integrity fact.

<span id="ORNA-SYS-043"></span>**ORNA-SYS-043** Dependency traversal MUST be cycle-safe and deterministic.

<span id="ORNA-SYS-044"></span>**ORNA-SYS-044** A statically proven edge MUST use `exact` confidence. A conservative dynamic edge MAY use `possible`; tooling MUST NOT present `possible` as a proven call or write.

<span id="ORNA-SYS-045"></span>**ORNA-SYS-045** `sys.dependencies` and `sys.dependents` MUST preserve edge kind and source evidence rather than return only a set of names.

<span id="ORNA-SYS-046"></span>**ORNA-SYS-046** Diagnostic codes, primary spans and structured causes MUST remain available independently of a particular terminal or UI rendering.

## 15.5 Sources, diagnostics and redaction {#system-redaction}

`sys.source` returns an authored or generated `sys.SourceDocument` pinned to its file and snapshot. `sys.describe` returns structured metadata. Neither is a request to reveal credentials, arbitrary host paths or unretained historical source.

Coordinates use zero-based UTF-8 byte offsets and half-open `[start, end)` spans. Lines and columns presented to humans are derived from those bytes; tab width is a renderer decision, not a source-map coordinate. Generated source identifies its generator when that information is retained.

A diagnostic has a stable code, severity, safe message, source spans, optional notes and causal references. Localisation changes the presentation, not the code or causal identity. Redaction happens before a value crosses a system, trace or codec boundary. A protected value retains a safe explicit marker; it is not replaced with plausible fake text.

<span id="ORNA-SYS-035"></span>**ORNA-SYS-035** Source paths returned outside explicitly trusted developer inspection MUST be repository-relative or redacted according to [redaction rules](#system-redaction).

<span id="ORNA-SYS-047"></span>**ORNA-SYS-047** Rendering a diagnostic MUST NOT mutate it, change its code or alter the underlying failure identity.

<span id="ORNA-SYS-048"></span>**ORNA-SYS-048** Secret values, decrypted source fragments, authorization material and unredacted connector payloads MUST NOT appear in diagnostic `message`, labels, causes, help, structured data, traces or presentation fallbacks.

<span id="ORNA-SYS-049"></span>**ORNA-SYS-049** A Git commit row and a semantic snapshot row MUST remain distinguishable even when they correspond one-to-one.

<span id="ORNA-SYS-050"></span>**ORNA-SYS-050** Hidden Orna refs MUST be visible through `sys.Ref.hidden` in explicit developer inspection and MUST remain valid ordinary Git refs.

<span id="ORNA-SYS-051"></span>**ORNA-SYS-051** A semantic diff MUST classify rename and rekey when identity evidence proves them; otherwise it MUST report conservative delete/add rather than invent identity.

<span id="ORNA-SYS-052"></span>**ORNA-SYS-052** Missing promisor objects MUST be represented explicitly. Introspection MUST NOT fabricate unavailable source, rows or statistics.

<span id="ORNA-SYS-070"></span>**ORNA-SYS-070** Storage locations and host paths are sensitive metadata and MUST follow the path-redaction rules.

<span id="ORNA-SYS-071"></span>**ORNA-SYS-071** `sys.Secret` MUST never expose decrypted secret bytes, derived authorization headers or a value formatter that reveals them.

<span id="ORNA-SYS-096"></span>**ORNA-SYS-096** A portable failure code MUST retain its meaning throughout the 1.x compatibility line.

<span id="ORNA-SYS-097"></span>**ORNA-SYS-097** Implementations MAY add structured fields and vendor causes, but MUST NOT replace a required portable primary code with a vendor-only code.

<span id="ORNA-SYS-098"></span>**ORNA-SYS-098** Failure messages may be localized; code, structured fields and causal identity remain stable.

<span id="ORNA-SYS-099"></span>**ORNA-SYS-099** Redaction MUST occur at the system-value boundary and MUST NOT depend solely on a renderer remembering to hide a value.

<span id="ORNA-SYS-100"></span>**ORNA-SYS-100** A redacted value MUST preserve safe type/identity metadata and an explicit redaction marker; it MUST NOT substitute plausible fake plaintext.

<span id="ORNA-SYS-101"></span>**ORNA-SYS-101** Canonical codecs MUST refuse to encode a protected value in reveal mode unless an explicitly trusted API outside generic `sys` grants that operation. No portable generic reveal API exists in 1.0.

<span id="ORNA-SYS-102"></span>**ORNA-SYS-102** Hashes of low-entropy secret values MUST be treated as sensitive. Domain separation alone does not make a dictionary-attackable hash safe; a public identifier MUST NOT reveal such a hash.

## 15.6 History, plans and storage {#system-history-plans-and-storage}

`sys.snapshot` resolves a supported selector to a pinned `sys.SnapshotRef`. A string selector is resolved once; moving its branch later does not change the returned reference. Root analysis functions read metadata without switching branches or creating commits.

`sys.diff` compares semantic snapshots. `sys.changes` describes changes within the selected context. A `sys.DiffEntry` wraps its `change`; the object belongs to `entry.change.object`, not an invented flattened field. `sys.Commit` has distinct `authored_at` and `committed_at` fields.

`sys.explain` returns a plan reference with structured nodes and dependency metadata. A `sys.Query` can refer to a plan; a plan does not invent an inverse `.query` field. An optimiser may change the plan while preserving results and observable ordering.

Storage metadata distinguishes an observed profile, a future placement preference, and an explicit rewrite target. Changing a preference is not a physical rewrite. A rewrite preserves keys, logical values, ordering and canonical row meaning, so its semantic row diff is empty. [Storage profiles](#storage) define representability and format limits.

<span id="ORNA-SYS-053"></span>**ORNA-SYS-053** Explain output MUST be structured `sys.Plan` data. Human-readable tables or trees are `Present` renderings and are not the semantic result.

<span id="ORNA-SYS-054"></span>**ORNA-SYS-054** Actual execution counters MUST be absent when not measured; they MUST NOT be populated with estimates while marked actual.

<span id="ORNA-SYS-055"></span>**ORNA-SYS-055** A plan is snapshot-specific and MUST NOT be reused against a different schema revision without revalidation.

<span id="ORNA-SYS-056"></span>**ORNA-SYS-056** Every reflective `sys.invoke` or `sys.start` call MUST create an invocation identity before user code begins.

<span id="ORNA-SYS-057"></span>**ORNA-SYS-057** An unhandled ordinary failure MUST mark the invocation failed and roll back its Orna-controlled transaction before the failure is reported as terminal.

<span id="ORNA-SYS-058"></span>**ORNA-SYS-058** Cancellation MUST be recorded separately from ordinary failure and MUST bypass `|?` recovery inside the cancelled activation.

<span id="ORNA-SYS-059"></span>**ORNA-SYS-059** A run's `live` field is meaningful only for the current observation and MUST be false or absent in a purely historical projection.

<span id="ORNA-SYS-060"></span>**ORNA-SYS-060** Trace retention MAY be bounded, but the absence of discarded trace detail MUST be explicit and MUST NOT change invocation outcome.

<span id="ORNA-SYS-067"></span>**ORNA-SYS-067** Physical storage rows are introspection data and MUST NOT change the logical contents, ordering, equality or serialization of table rows.

<span id="ORNA-SYS-068"></span>**ORNA-SYS-068** A compact segment MUST identify its schema, key range, generation and content digest sufficiently to detect stale or overlapping publication.

<span id="ORNA-SYS-069"></span>**ORNA-SYS-069** Missing lazily hydrated objects MUST be exposed as unavailable; a query may hydrate them according to policy but MUST NOT silently substitute incomplete results.

<span id="ORNA-SYS-072"></span>**ORNA-SYS-072** Build metadata MUST identify the complete source snapshot and compatibility coordinates used to produce an artifact.

<span id="ORNA-SYS-073"></span>**ORNA-SYS-073** A specification fixture listed as planned evidence MUST NOT be represented as passed unless an implementation actually executed it.

<span id="ORNA-SYS-074"></span>**ORNA-SYS-074** Every read/analysis function MUST return a normal typed value or relation that can be queried, encoded where supported and presented independently.

<span id="ORNA-SYS-075"></span>**ORNA-SYS-075** A root analysis function MUST NOT change checkout, refs, table contents, checkpoints, process state or retained failure state.

<span id="ORNA-SYS-076"></span>**ORNA-SYS-076** Name resolution and history functions MUST respect visibility and source redaction even in a trusted deployment.

<span id="ORNA-SYS-115"></span>**ORNA-SYS-115** `sys.Storage.profile` MUST describe observed placement and `sys.Storage.preference` MUST describe future automatic placement policy; implementations MUST NOT conflate the two.

<span id="ORNA-SYS-116"></span>**ORNA-SYS-116** Setting storage preference MUST NOT rewrite existing logical rows or produce a semantic row change.

<span id="ORNA-SYS-117"></span>**ORNA-SYS-117** A storage rewrite MUST preserve every logical key/value, ordering and canonical row meaning, and its semantic row diff MUST be empty.

<span id="ORNA-SYS-118"></span>**ORNA-SYS-118** A rewrite to editable storage MUST fail before publication with `sys.storage.unrepresentable_key` when any row key cannot be represented by the canonical path codec or when the normative editable resource bounds cannot be met.

<span id="ORNA-SYS-119"></span>**ORNA-SYS-119** A rewrite collision, stale generation or concurrently changed input MUST fail with `sys.storage.rewrite_conflict` and leave the prior generation authoritative.

<span id="ORNA-SYS-120"></span>**ORNA-SYS-120** `sys.storage` is a grouping namespace, not a callable selector. Portable source MUST use `sys.Storage`, `sys.storage.objects` and the explicit `sys.admin` storage functions.

## 15.7 Reflective invocation {#system-reflective-invocation}

Reflection is typed invocation, not source evaluation. The result witness `as: T` selects a typed boundary; `sys.Value` is an explicit type-preserving envelope, not an inference fallback or an implicit conversion chain.

A `sys.ArgumentMap` accepts named arguments at the reflective call boundary, retains each exact originating type, rejects duplicate names, and orders names by canonical UTF-8 for identity hashing. The normal source record is admitted here only by the explicit reflective parameter contract; arbitrary records do not silently become `sys.Value` elsewhere.

### 15.7.1 Algorithm INVOKE-1 {#system-algorithm-invoke-1}

1. Resolve the `sys.FunctionRef` in its pinned snapshot. If `at` is `null`, use that pin—not the caller's current CWD. An explicit `at` must identify the same pinned snapshot; otherwise fail with `sys.invoke.snapshot_mismatch`. To select another revision, explicitly resolve the function in that snapshot before invoking it.
2. Check visibility and that the declaration is callable. Reject unresolved generic arguments and bound values whose static types do not match the signature.
3. Bind each named argument once. Apply declared defaults in the function's pinned context, reject unknown/missing names, and validate the explicit result witness before any target effect.
4. Select the documented execution mode. An inherited transaction is permitted only when its snapshot agrees with the target and the operation is synchronous. An asynchronous start uses a separate transaction or read-only mode, never a transaction whose owner can complete before it.
5. Capture locale, timezone, trace and cancellation ownership. Compute the identity tuple from function identity, revision, snapshot, typed bound arguments including defaults, expected return type, mode and relevant context.
6. Atomically admit the idempotency key, where supplied. A retained entry with a different tuple fails; an identical terminal entry returns its retained outcome. An identical active entry refers to that same operation, not a second execution. Missing/pruned outcome information is not evidence that re-execution is safe.
7. Create the invocation observation, run the target and record one terminal outcome. Commit owned Orna writes only on successful completion; otherwise roll them back. Follow [child termination](#execution) before declaring the owner terminal.
8. Publish a redacted result/diagnostic and retained trace state. A crash after an external side effect does not permit a false exactly-once guarantee.

`sys.start` returns a typed handle. `sys.await` returns one complete `sys.InvocationResult<T>` or raises `sys.invoke.await_timeout`; a timeout does not cancel or detach the target. `sys.cancel` acts on an invocation handle, not a durable-run reference. [Task ownership](#execution) determines whether an invocation belongs to an operation or a REPL session.

### 15.7.2 Result invariants {#system-result-invariants}

A successful `sys.InvocationResult<T>` has a present value and no ordinary failure. A failed result has a failure and no value. A cancelled or orphaned result has no successful value. It may carry a diagnostic explaining termination, but that diagnostic does not turn cancellation into an ordinary catchable failure inside the cancelled target. The result never represents a running or timed-out target. A timed-out wait returns no partial result.

An `Option<T>` result may itself be `null`. The outer presence flag in the invocation result must preserve that distinction; `Some(null)` is a successful optional result, not an absent result. [Canonical values](#formats) preserve nested options.

<span id="ORNA-SYS-077"></span>**ORNA-SYS-077** `sys.invoke` MUST NOT evaluate arbitrary source text or bypass the parser, resolver, type checker, visibility rules, effect rules or transaction model.

<span id="ORNA-SYS-078"></span>**ORNA-SYS-078** An argument envelope MUST NOT allow a value to masquerade as another static type merely because its data representation is compatible.

<span id="ORNA-SYS-079"></span>**ORNA-SYS-079** An idempotency key binds to function identity, exact revision, snapshot, expected result type, canonical typed arguments, invocation mode and relevant context. Reuse for a different tuple MUST fail with `sys.invoke.idempotency_mismatch`.

<span id="ORNA-SYS-080"></span>**ORNA-SYS-080** Returning a cached idempotent success is permitted only after the implementation proves the complete identity tuple matches and the retained result is valid under the same codec/type version.

<span id="ORNA-SYS-081"></span>**ORNA-SYS-081** `sys.start` MUST return a handle bound to one runtime generation and one invocation identity.

<span id="ORNA-SYS-082"></span>**ORNA-SYS-082** `sys.await` timeout MUST NOT cancel, detach, reparent or otherwise change the target invocation.

<span id="ORNA-SYS-083"></span>**ORNA-SYS-083** `sys.cancel` MUST be idempotent and MUST NOT convert cancellation into an ordinary recoverable failure inside the target.

<span id="ORNA-SYS-129"></span>**ORNA-SYS-129** Every type named by a portable `sys` field or function signature MUST be a language type, a core presentation/query type, a canonical system relation, an opaque identifier, a closed enum or a supporting value type enumerated by `api/sys.json`; undocumented pseudo-types are forbidden.

<span id="ORNA-SYS-130"></span>**ORNA-SYS-130** `sys.Value` MUST NOT be used as an implicit inference fallback or an implicit conversion route. It MUST preserve exact originating type identity and MUST require an explicit reflective API to unbox or validate it.

<span id="ORNA-SYS-131"></span>**ORNA-SYS-131** `sys.ArgumentMap` MUST reject duplicate names, preserve each argument's exact originating type and produce a deterministic canonical name order for idempotency hashing.

<span id="ORNA-SYS-132"></span>**ORNA-SYS-132** The typed `sys.invoke<T>` and `sys.start<T>` overloads MUST validate the function's declared result against the explicit `as: T` witness before any function effect occurs. A mismatch MUST fail with `sys.invoke.return_type`; no conversion chain is attempted.

<span id="ORNA-SYS-133"></span>**ORNA-SYS-133** `sys.InvocationResult<T>` MUST describe one complete terminal outcome and MUST satisfy the value/failure invariants stated above.

<span id="ORNA-SYS-134"></span>**ORNA-SYS-134** `sys.CheckpointPosition` MUST remain opaque outside its provider contract; generic code MUST NOT order it, increment it or synthesize a successor.

<span id="ORNA-SYS-138"></span>**ORNA-SYS-138** A conforming implementation MUST expose the supporting-type fields and invariants in `api/sys.json` and MUST reject a portable API claim whose type graph contains an unresolved name.

## 15.8 Mutation and observation consistency {#system-mutation-and-observation-consistency}

Portable system rows are read-only. Setters, row mutators and collection mutation cannot bypass the [administrative operations](#administration).

Each administrative command records an invocation and safe outcome. Commands spanning Git and local database state use their specified journal/recovery protocol; the word “atomic” does not imply a hidden transaction shared by arbitrary Git processes and an external server.

Multi-relation inspection pins one snapshot or observation instant. A continuation token binds the snapshot, filter, order and redaction context; it cannot be used to splice results from another generation. Relation order remains unspecified unless explicitly ordered.

Retention distinguishes known absence from unavailable detail. Losing optional traces cannot invalidate an advertised complete committed database snapshot, and reading history never restarts an old program.

<span id="ORNA-SYS-084"></span>**ORNA-SYS-084** Direct mutation of a portable `sys` relation MUST fail at resolve/type-check time where statically evident and otherwise before any write is performed.

<span id="ORNA-SYS-085"></span>**ORNA-SYS-085** Every administrative function MUST define its transaction boundary, compare-and-set preconditions, failure codes and durable audit row.

<span id="ORNA-SYS-086"></span>**ORNA-SYS-086** An administrative function MUST validate all affected assertions before publishing its state transition.

<span id="ORNA-SYS-087"></span>**ORNA-SYS-087** A failed administrative operation MUST leave repository refs, logical CWD, checkpoints and runtime state at the last valid boundary described by its algorithm.

<span id="ORNA-SYS-088"></span>**ORNA-SYS-088** The 1.0 `sys.admin` boundary relies on OS/process isolation, Git/SSH credentials and the authenticated network perimeter; it MUST NOT be misrepresented as a built-in enterprise grants or role engine.

<span id="ORNA-SYS-089"></span>**ORNA-SYS-089** A continuation token MUST NOT be reused against a different snapshot, order, filter or redaction context.

<span id="ORNA-SYS-090"></span>**ORNA-SYS-090** Default iteration over an unordered system relation MUST NOT be documented as stable merely because one implementation happens to use object-ID order.

<span id="ORNA-SYS-091"></span>**ORNA-SYS-091** A multi-relation inspection operation requiring a coherent view MUST pin one observation snapshot/instant and expose it in the result.

<span id="ORNA-SYS-092"></span>**ORNA-SYS-092** A client that detects an observation-generation change during a live pagination sequence MUST restart or explicitly accept a new generation; rows MUST NOT be silently spliced across generations.

<span id="ORNA-SYS-093"></span>**ORNA-SYS-093** Retention policy MUST NOT delete data required to reproduce a committed logical snapshot while that snapshot remains advertised as complete.

<span id="ORNA-SYS-094"></span>**ORNA-SYS-094** Pruned optional detail MUST be represented as unavailable metadata, not as an empty value that could be mistaken for observed emptiness.

<span id="ORNA-SYS-095"></span>**ORNA-SYS-095** Retention and publication of runtime observations MUST never cause external effects or restart a program merely because a historical relation is queried.

## 15.9 Conformance obligations {#system-conformance-obligations}

The complete schema is published both in the [system reference](#system-reference) and `api/sys.json`. Generated declarations are signature notation, not an independent redefinition of the language grammar. A disagreement between those representations is a specification defect, not implementation freedom.

The requirements below specify implementation evidence. The package's authoring checks and semantic models cover only their explicitly reported scope; they are not a declaration that a completed Orna engine has executed all of these obligations.

<span id="ORNA-SYS-103"></span>**ORNA-SYS-103** A conformance claim MUST list every unavailable optional retention/detail profile but MUST NOT call a required portable member optional.

<span id="ORNA-SYS-104"></span>**ORNA-SYS-104** The conformance suite MUST test every root function with success, not-found/wrong-kind, snapshot, redaction and cancellation/failure cases applicable to that function.

<span id="ORNA-SYS-105"></span>**ORNA-SYS-105** The suite MUST verify that active valid examples use `sys.rt` and that `sys.runtime` appears only as an explicitly invalid name or diagnostic subject.

<span id="ORNA-SYS-106"></span>**ORNA-SYS-106** The suite MUST prove live handles fail across runtime generations and that snapshot references remain stable across CWD changes.

<span id="ORNA-SYS-107"></span>**ORNA-SYS-107** The suite MUST prove `sys.await` timeout leaves the target running and independently cancellable.

<span id="ORNA-SYS-108"></span>**ORNA-SYS-108** The suite MUST prove checkpoint compare-and-set, stream-write atomicity, failure retry/skip transitions and assertion validation under injected crashes and cancellation.

<span id="ORNA-SYS-109"></span>**ORNA-SYS-109** The suite MUST prove secret and path redaction before Inspect, Display, Present, codec and trace boundaries.

<span id="ORNA-SYS-110"></span>**ORNA-SYS-110** The suite MUST compare grouped handles with the same-snapshot canonical relations, applying the documented current-runtime/live filter to `sys.rt` handles rather than comparing them with all historical rows.

<span id="ORNA-SYS-125"></span>**ORNA-SYS-125** The suite MUST prove every canonical relation row exposes the correctly typed snapshot/runtime-pinned `reference`, including natural-key references that introduce no synthetic identity.

<span id="ORNA-SYS-126"></span>**ORNA-SYS-126** The suite MUST prove storage preference does not rewrite existing rows, storage rewrite produces an empty semantic row diff, unrepresentable editable keys fail before publication and stale generations leave the prior representation authoritative.

<span id="ORNA-SYS-127"></span>**ORNA-SYS-127** The suite MUST verify [failure administration](#checkpoints), including stable delivery identity, versioned retries, skip, replay and resolve, and MUST reject system-row mutator methods.

<span id="ORNA-SYS-128"></span>**ORNA-SYS-128** The suite MUST test both `sys.source` and `sys.history` overloads with current, renamed, historical, missing, partially cloned and redacted files/objects.

<span id="ORNA-SYS-139"></span>**ORNA-SYS-139** The suite MUST exercise every snapshot-selector overload and prove that the returned `SnapshotRef` is pinned even when a named branch later moves.

<span id="ORNA-SYS-140"></span>**ORNA-SYS-140** The suite MUST exercise erased and typed invocation/start overloads, reject a mismatched `as: T` before effects, and prove `sys.Value` is never selected by ordinary failed inference.

<span id="ORNA-SYS-141"></span>**ORNA-SYS-141** The suite MUST validate every supporting value-type field, invariant and canonical encoding entry against `api/sys.json`.



# 16. Administrative operations {#administration}

## 16.1 Administrative boundary {#administration-administrative-boundary}

Administrative functions operate on repository, storage or execution state that ordinary table mutation cannot reach. They are not methods on mutable system rows. [The API reference](#system-reference) gives each exact signature; this chapter supplies the corresponding transition contract.

A call is admitted only through the trusted local process boundary or an authenticated endpoint explicitly permitting administration. Possessing a row reference or knowing a plan token is not additional authority. An application evaluation endpoint does not acquire administrative permission merely because it accepts Orna expressions.

<span id="ORNA-ADMIN-001"></span>**ORNA-ADMIN-001** Every administrative call MUST record an invocation with the function, safe arguments, owner, observed generation and terminal outcome. State-changing calls MUST retain an audit event in the same local transaction as their durable transition, or in the recoverable journal for a Git transition. A failed call records failure without claiming that its requested state change occurred.

<span id="ORNA-ADMIN-002"></span>**ORNA-ADMIN-002** Administrative operations MUST NOT run reentrantly inside an activation holding uncommitted table writes. Such a call fails with `sys.admin.busy`; the caller may finish its transaction and submit administration as a new activation. Read-only planning and verification may inspect a pinned generation without mutating it.

Every argument is resolved and type-checked before mutation. A reference from another runtime fails when it denotes a live object. A snapshot reference remains pinned. Missing data is hydrated only under the configured availability policy. Failure to obtain required objects leaves the prior authoritative generation unchanged.

### 16.1.1 Shared preconditions and failure classes {#administration-shared-preconditions-and-failure-classes}

All mutating operations require a writable attachment and fail with `sys.admin.read_only` otherwise. All reject invalid argument types through the ordinary type/argument diagnostics. `sys.admin.busy` identifies an incompatible active owner/lease or a reentrant write activation. Invalid candidate invariants produce `sys.admin.assertion_failed`. Missing objects produce `sys.admin.missing_object` or the more specific snapshot/source/storage code before mutation. A cancellation before durable admission has no transition; after admission the operation follows its journal or transaction to a safe boundary and reports whether it completed or was cancelled.

A successful no-op returns the documented current value or `false`, not an invented new generation. Unexpected I/O failure is an ordinary failure with a retained safe cause and a recovery record if admission already occurred. It must not be translated into success. State-dependent conditions are rechecked under the lock or compare-and-set transaction; checking them only when a preview is generated is insufficient.

## 16.2 Repository operations {#administration-repository-operations}

| Operation | Required state and target | Atomic boundary, audit and failure behaviour |
|---|---|---|
| `plan_checkout` | Resolvable committed target; no mutation permission required to inspect an otherwise accessible target. | Pins HEAD, CWD, index, worktree and pending generation into `sys.CheckoutPlan`. Records a read invocation, not a checkout event. Does not pause consumers, create objects or switch HEAD. Missing or invalid target fails without changes. |
| `create_branch` | Valid non-existing local branch name; supplied committed start point or current committed HEAD. | Git compare-and-set from absent to target commit, with a local journal/audit event. Does not switch, stage, commit, pause streams or alter pending changes. `sys.git.unborn_head`, `sys.git.uncommitted_snapshot`, `sys.git.invalid_ref` or `sys.git.branch_exists` leave all existing refs and CWD untouched. |
| `checkout` | Resolvable committed target; safe carry-forward, or exact authorised destructive plan; valid target schema and assertions. | [CHECKOUT-1](#branching). Records target attachment and preserved/discarded sets. Dirty conflict gives `sys.git.dirty_conflict`; a stale token gives `sys.git.stale_plan`. Neither admits a partial switch. Force cannot bypass invariants. |
| `commit` | Valid staged state, nonempty staged delta, safe author identity and message; no conflicting publication lease. | Builds the staged candidate, validates it, creates its commit, then uses the [publication reconciliation protocol](#publication) to preserve unstaged work. Records commit/ref transition and exact staged generation. An unchanged index gives `sys.git.nothing_to_commit`; stale state is re-planned or rejected before ref change. It never consumes a newer unstaged tail. |

`sys.admin.commit` has no implicit “allow empty” option. The Git-compatible CLI may expose Git's explicit option separately. Default author information comes from configured Git identity; missing identity fails before creating a commit. The committer and commit timestamp are recorded independently of the authored identity and timestamp.

### 16.2.1 Unborn branches {#administration-unborn-branches}

`create_branch` is equivalent to creating a ref at an existing commit, so an absent default HEAD commit fails. `orna switch -c name` in an unborn worktree follows Git's distinct behaviour: it selects the new unborn symbolic branch and preserves staged and unstaged content; no branch ref points to a fabricated commit. The first actual commit creates that branch ref. The operation is journalled like other symbolic-HEAD changes and does not reset allocators or checkpoints.

## 16.3 Storage operations {#administration-storage-operations}

| Operation | Preconditions | Atomic boundary and result |
|---|---|---|
| `flush` | Optional table exists and is writable; frozen pending generation selected. | Seal that generation durably into storage objects and update local placement metadata. Do not move a Git ref or consume later writes. Return exact rows/bytes/segments sealed. An empty tail is a successful zero result. Physical failure leaves old data authoritative and reports unavailable/corrupt storage as appropriate. |
| `compact` | Selected segments exist, are verified and do not overlap an incompatible maintenance lease. | Build replacements off to the side, compare the input generation, then atomically replace the local manifest. Preserve semantic rows, keys and checkpoints. Record a compaction job. Stale inputs give `sys.storage.rewrite_conflict`; cancellation discards unadmitted outputs. |
| `set_storage_preference` | Valid table and enum preference. | One local metadata transaction and storage-change audit; changes future automatic placement only. Return the observed storage row with the new preference. No row rewrite or semantic data diff occurs. |
| `rewrite_storage` | All input objects available; target representation can encode every row and key; explicit target and current input generation. | Validate candidate output, compare input generation, replace placement as one recoverable generation. Return counts and the resulting storage reference. Unrepresentable editable keys give `sys.storage.unrepresentable_key`; stale/colliding input gives `sys.storage.rewrite_conflict`; both preserve prior placement. |
| `verify` | Readable selected scope; a pinned observation generation. | Inspect without repair. Return all discovered errors/warnings in `sys.VerificationReport`; the report distinguishes corruption from unavailable promised bytes. A failed integrity check is report data, not an implicit repair. Failure to complete the scan is `sys.admin.verification_failed` with a partial safe diagnostic, never a clean report. |

Objects written before failed compaction admission are unreferenced temporary objects and may be garbage-collected after recovery. A reference to old pinned data continues to resolve for its retained lifetime; switching the active manifest does not rewrite the meaning of that reference. No maintenance operation advances a source checkpoint.

## 16.4 Runs, streams and delivery state {#administration-runs-streams-and-delivery-state}

| Operation | Preconditions | Transition and outcome |
|---|---|---|
| `cancel_run` | Run reference resolves in the current owner context. | Request cancellation, stop admission of new items, cancel and join descendants, roll back open transactions, then mark the run terminal. Return `true` only if this call first requested cancellation; an already terminal/cancelling run returns `false`. Previously committed items remain. |
| `pause_stream` | Stream is live and belongs to the current owner; caller does not hold its callback transaction. | Stop new item admission and wait for the current item/batch boundary. Mark paused and record the reason. Return `false` if already paused/terminal; do not cancel unrelated streams. Cancellation of the pause request leaves either the prior state or an acknowledged paused state, explicitly reported. |
| `resume_stream` | Stream is paused, its run is live, source is available, consumer identity is unchanged and no incompatible checkpoint reset or blocking failure is unresolved. | Acquire admission lease and mark running atomically. Return `false` if already running. Failure leaves it paused; it never guesses a source position or clears a failure. |
| `reset_checkpoint` | Source supports the requested position; stream paused and no in-flight delivery; expected version and position match; no unresolved blocking failure. | Compare-and-set position and version plus an audit `sys.CheckpointUpdate` in one transaction. Preserve provider format and full consumer identity. Stale state gives `sys.checkpoint.conflict`, unsupported position gives `sys.checkpoint.not_replayable`. No callback is executed. |
| `retry_failure` | Same stable delivery, expected failure version/status, current checkpoint still at its recorded predecessor, replayable payload/source and no attempt lease. | Lease the delivery, increment attempt count/version, create a new invocation and execute its callback. Follow [DELIVERY-1](#checkpoints). Failed retry returns the same failure identity to open, without advancing progress. |
| `skip_failure` | Blocking open delivery, matching failure version/status and checkpoint predecessor; provider supplies a safe successor; reason present. | Move the checkpoint, mark the same failure skipped and write an audit record in one transaction. No user callback runs. Unsupported skip gives `sys.failure.skip_unsupported`; stale failure gives `sys.failure.stale_version`. |
| `replay_failure` | Preserved nonblocking skipped delivery, matching failure version/status, available safe payload/source and no replay lease. | Execute separately; do not rewind or advance the live checkpoint. Success marks replayed; ordinary failure returns to skipped with updated diagnostic/version. Return a handle owned under the normal task rules. |
| `resolve_failure` | Nonblocking failure with matching version/status and an explicit reason. | Record resolution without marking the delivery as successfully processed. Blocking open/retrying state gives `sys.failure.state_conflict`; callers must retry or explicitly skip it. No checkpoint movement. |

Each returned retry/replay handle follows [task ownership](#execution). An administrative request admitted as a direct REPL start may be session-owned; an operation that calls it and then ends cancels unfinished work. The lifetime never depends on whether a client happens to keep the handle in memory.

## 16.5 Cancellation and crash recovery {#administration-cancellation-and-crash-recovery}

<span id="ORNA-ADMIN-003"></span>**ORNA-ADMIN-003** A state-changing command MUST expose one of: no admission, admitted and recoverable, or terminal. On reconnect, an unknown outcome MUST be inspected through its retained invocation/journal identity before another non-idempotent command is issued.

The host must distinguish inability to report a response from inability to commit an operation. A disconnected response after a successful commit does not turn that commit into a failure. A retry with the same idempotency identity observes that recorded outcome. External effects remain subject to the limitations in [execution](#execution); the administrative interface does not make an arbitrary provider exactly-once.



# 17. The interactive session {#repl}

## 17.1 Session model {#repl-session-model}

The REPL behaves like an ephemeral module. Imports, bindings and function declarations persist for the session.

```text
> use sensors.greenhouse.*;
> let recent = Reading | filter(r => r.time > now() - 1.hour);
> fn warm() = recent | filter(r => r.temperature > 25.C);
```

<span id="ORNA-REPL-001"></span>**ORNA-REPL-001** The REPL MUST use ordinary `use` syntax for namespace access and MUST NOT require a separate `:cd` namespace model.

## 17.2 Last result and status {#repl-last-result-and-status}

```text
$_
```

is the last successful REPL result, inspired by shell conventions.

```text
$?
```

is the last execution status/error value if supported.

<span id="ORNA-REPL-002"></span>**ORNA-REPL-002** `$_` and `$?` are REPL bindings and MUST NOT become implicit globals in module source.

## 17.3 Syntax-aware editing {#repl-syntax-aware-editing}

The REPL SHOULD provide:

- syntax highlighting while typing;
- matching-brace highlighting;
- automatic indentation;
- multiline input until syntax is complete;
- context/type-aware completion;
- signatures and documentation in completion;
- clickable source locations;
- persistent history;
- a pager for large output.

## 17.4 Eager previews {#repl-eager-previews}

Safe previews show both value and type:

```text
3 : Int
"alice-smith" : Str
£12.34 : Money<GBP>
1.5 hour : Float<hour>
```

For a relation expression, a dim ghost preview may show:

```text
╰─ Relation<sensors.greenhouse.Reading> · ≈42.1k rows
```

The `≈` and ghost placement communicate estimation/laziness without prose such as “not executed”.

<span id="ORNA-REPL-003"></span>**ORNA-REPL-003** Eager preview MUST NOT perform mutations, external I/O or other effects.

<span id="ORNA-REPL-004"></span>**ORNA-REPL-004** A relation preview MUST NOT require complete enumeration.

After submission, the relation may be presented as a bounded table window and fetched incrementally during navigation.

## 17.5 Default presentation {#repl-default-presentation}

```text
> directory.Contact
```

may render:

```text
directory.Contact : Relation<Contact> · 428 rows · CWD

id                name             emails
alice-smith       Alice Smith      2
bob-jones         Bob Jones        1
...
```

The type remains visible.

## 17.6 Console commands {#repl-console-commands}

Console operations are prefixed with `:` so they are not confused with language functions:

```text
:help [name]
:doc expression
:type expression
:source expression
:open expression
:inspect expression
:members expression
:table expression
:plan expression
:trace expression
:watch expression
:unwatch id
:at CWD|HEAD|ref
:history [query]
:copy expression [--format orna|json|csv]
:display Type formatter|default
:timing on|off
:clear
:quit
```

<span id="ORNA-REPL-005"></span>**ORNA-REPL-005** `:at` changes the default snapshot context for the session; it MUST NOT mutate repository HEAD.

<span id="ORNA-REPL-006"></span>**ORNA-REPL-006** `:watch` MUST use the same dependency and delta machinery as page sessions.



# 18. Command-line interface {#cli}

## 18.1 Git-compatible commands {#cli-git-compatible-commands}

```text
orna init      clone      status     add        commit     log
orna diff      branch     checkout   switch     merge      reset
orna push      pull       fetch      remote     stash      tag
orna blame     show       restore    mv
```

<span id="ORNA-CLI-001"></span>**ORNA-CLI-001** If Git already has the operation, Orna SHOULD preserve its name, flags and observable semantics. `orna mv` additionally preserves Orna semantic identity when the target is a definition or row key.

## 18.2 Orna-specific commands {#cli-small-orna-specific-surface}

The Orna-specific commands are:

```text
orna repl
orna run
orna serve
orna check
orna fmt
orna explain
orna verify
orna prune
```

There are no required `orna stream`, `orna checkpoint`, `orna failure`, `orna grants` or extension-permission command hierarchies.

<span id="ORNA-CLI-002"></span>**ORNA-CLI-002** Runtime/checkpoint/failure detail and uncommon recovery operations SHOULD be available through typed `sys` values in the REPL and optional `std.devtools` pages.

## 18.3 Status {#cli-status}

```text
$ orna status

On branch main
Your branch is ahead of 'origin/main' by 3 commits.

Changes to be committed:
  modified   energy/main.orna

Changes in CWD:
  modified   directory.Contact/alice-smith

Runtime data:
  sensors.greenhouse.Reading
    184,221 rows · 21.8 MiB · oldest 49s

Programs:
  messages.sync    blocked · invalid MIME · 12 attempts
  bank.sync           running · checkpoint page:92
```

<span id="ORNA-CLI-003"></span>**ORNA-CLI-003** `orna status --porcelain` MUST provide stable machine-readable output.

<span id="ORNA-CLI-004"></span>**ORNA-CLI-004** `orna status --short` SHOULD preserve Git's compact style while adding stable codes for runtime CWD summaries.

## 18.4 Diagnostics {#cli-diagnostics}

```text
error[E0312]: column `email` cannot become non-optional
  ┌─ contacts/main.orna:8:5
  │
8 │     email: Str,
  │     ^^^^^^^^^^ 14 existing rows contain null
  │
  = affected table: directory.Contact
  = required by: contacts.page()
  help: provide a default or update the existing rows first
```

<span id="ORNA-DIAG-001"></span>**ORNA-DIAG-001** Errors MUST have stable codes, concise titles, source spans when available, causes and actionable help where known.

<span id="ORNA-DIAG-002"></span>**ORNA-DIAG-002** `orna explain <code>` SHOULD provide extended documentation.

## 18.5 Output controls {#cli-output-controls}

```text
-v / -vv
-q
--color auto|always|never
--format human|short|json
```

<span id="ORNA-CLI-005"></span>**ORNA-CLI-005** Dynamic progress MUST be suppressed when output is not a terminal unless explicitly forced.

<span id="ORNA-CLI-006"></span>**ORNA-CLI-006** Terminal hyperlinks SHOULD be emitted when supported.

## 18.6 Visual language {#cli-visual-language}

```text
green   success / valid
yellow  warning / change
red     error / conflict
cyan    object names / refs / paths
dim     secondary metadata
```

No emoji-heavy celebration or decorative ASCII branding is required.


## 18.7 User-facing diagnostics {#cli-user-facing-diagnostics}

<span id="ORNA-UX-001"></span>**ORNA-UX-001** Normal CLI output, REPL output, diagnostics, generated user documentation and default frontend text MUST describe the problem and remedy using Orna concepts.

<span id="ORNA-UX-002"></span>**ORNA-UX-002** Normal user-facing output MUST NOT require the user to understand internal profile names, storage libraries, embedded-database terminology, wire encodings, object formats or extension ABI terminology.

<span id="ORNA-UX-003"></span>**ORNA-UX-003** `--verbose` MAY provide additional Orna-level context. Raw implementation details require `--debug` or an equivalent explicitly technical interface.

<span id="ORNA-UX-004"></span>**ORNA-UX-004** A diagnostic MUST lead with the user-visible condition and an actionable remedy. An implementation MAY attach machine-readable technical causes without rendering them by default.


# 19. Repository, CWD, index and HEAD {#repository}

## 19.1 Required repository form {#repository-required-repository-form}

A minimal database has this shape:

```text
example/
├── .git/
├── main.orna
└── ... reachable modules and data ...
```

<span id="ORNA-REPO-001"></span>**ORNA-REPO-001** `main.orna` at the repository root MUST be the root module.

<span id="ORNA-REPO-002"></span>**ORNA-REPO-002** The module graph, not a recursive scan of every `.orna` file, MUST determine which module units form the database program.

<span id="ORNA-REPO-003"></span>**ORNA-REPO-003** Files not reachable from the root module MUST NOT contribute declarations to the database merely because they exist.

<span id="ORNA-REPO-004"></span>**ORNA-REPO-004** Row units belonging to a reachable table are database data even though row units are not imported individually.

## 19.2 CWD, index and HEAD {#repository-cwd-index-and-head}

Orna preserves Git's three human-facing states while adding durable runtime changes:

```text
HEAD      committed snapshot
INDEX     changes staged through Git/Orna
WORKTREE  ordinary checked-out file changes
RUNTIME   durable Turso-backed pending rows and system state
CWD       HEAD + INDEX/WORKTREE differences + RUNTIME differences
```

<span id="ORNA-STATE-001"></span>**ORNA-STATE-001** `HEAD` MUST mean the actual selected Git commit and MUST NOT silently include uncommitted hot data.

<span id="ORNA-STATE-002"></span>**ORNA-STATE-002** Plain relation access MUST default to CWD.

```orna
directory.Contact
```

is equivalent to:

```orna
directory.Contact.as_of(CWD)
```

<span id="ORNA-STATE-003"></span>**ORNA-STATE-003** `relation.as_of(HEAD)` MUST read the selected committed snapshot without the local runtime tail.

<span id="ORNA-STATE-004"></span>**ORNA-STATE-004** `relation.as_of(snapshot)` MUST use the exact supplied SnapshotRef. Git revision expressions are resolved explicitly through `sys.snapshot(selector)`; a bare branch expression is not additional source grammar.

<span id="ORNA-STATE-004A"></span>**ORNA-STATE-004A** Relation-level `as_of` MUST pin data and decoding schema, not silently select historical function code. Whole-program historical evaluation uses the database snapshot object defined in [relations](#relations).

<span id="ORNA-STATE-005"></span>**ORNA-STATE-005** Wall-clock `now()` MUST remain distinct from database-state selectors such as `CWD` and `HEAD`.

## 19.3 Committed Orna metadata {#repository-committed-orna-metadata}

A small tracked `.orna/` directory is reserved for canonical metadata that must exist in every snapshot but is not user schema source. It is distinct from local runtime files under `.git/orna/`.

```text
.orna/
├── format.orna
├── database.orna
├── checkpoints/
├── allocators/
├── runs/
└── failures/
```

<span id="ORNA-META-001"></span>**ORNA-META-001** Tracked `.orna/` MUST contain versioned metadata required to interpret or inspect a snapshot. It MUST NOT contain local caches, credentials or the authoritative unpublished runtime tail.

<span id="ORNA-META-002"></span>**ORNA-META-002** `.orna/format.orna` MUST identify the repository format and storage profile versions.

<span id="ORNA-META-003"></span>**ORNA-META-003** `.orna/database.orna` MUST contain the stable database identity created by `orna init`.

<span id="ORNA-META-004"></span>**ORNA-META-004** Checkpoint and allocator files in `.orna/` are snapshot watermarks; their current non-rewindable/local counterparts remain in Turso or hidden refs.

<span id="ORNA-META-005"></span>**ORNA-META-005** Implementations MUST NOT store rebuildable query/compiler caches in the tracked `.orna/` directory.

## 19.4 Local runtime files {#repository-local-runtime-files}

The embedded runtime requires local state that is not ordinary repository content.

<span id="ORNA-LOCAL-001"></span>**ORNA-LOCAL-001** Local runtime state MUST live under a path resolved through Git's per-worktree administrative directory, conceptually `git rev-parse --git-path orna/`.

A typical layout is:

```text
.git/orna/
├── state.db       # Turso: authoritative uncommitted runtime CWD
├── cache.db       # rebuildable compiler/query/storage indexes
├── runtime.sock   # optional local coordination socket
└── locks/
```

<span id="ORNA-LOCAL-002"></span>**ORNA-LOCAL-002** `state.db` MUST be treated as authoritative for runtime changes that have not yet been published to Git.

<span id="ORNA-LOCAL-003"></span>**ORNA-LOCAL-003** `cache.db` MUST be rebuildable from Git plus `state.db`.

<span id="ORNA-LOCAL-004"></span>**ORNA-LOCAL-004** Local runtime files MUST NOT be added to ordinary Git commits.

## 19.5 Any clone may operate independently {#repository-any-clone-may-operate-independently}

<span id="ORNA-EMBED-001"></span>**ORNA-EMBED-001** An Orna clone MUST remain usable without `orna serve` or any external Orna daemon.

<span id="ORNA-EMBED-002"></span>**ORNA-EMBED-002** `orna repl`, `orna run`, `orna status` and other local commands MUST be capable of opening the embedded Turso state in-process when no other local owner exists.

<span id="ORNA-EMBED-003"></span>**ORNA-EMBED-003** When overlapping local processes require one writable Turso handle, they MAY coordinate through a local socket and temporary owner process; this MUST remain an implementation detail and MUST NOT require a network server.

<span id="ORNA-EMBED-004"></span>**ORNA-EMBED-004** Version 1.0 MUST NOT rely for correctness on experimental multi-process Turso file access.

## 19.6 Independent clones {#repository-independent-clones}

Each clone has its own CWD.

<span id="ORNA-CLONE-001"></span>**ORNA-CLONE-001** Orna MUST treat each clone's CWD as independent.

<span id="ORNA-CLONE-002"></span>**ORNA-CLONE-002** Commits, fetch, push and merge are the synchronization mechanism between clones.

<span id="ORNA-CLONE-003"></span>**ORNA-CLONE-003** Two clones MAY make independent changes. Non-fast-forward pushes and semantic merges MUST be handled through Git-compatible workflows rather than hidden distributed locking.

<span id="ORNA-CLONE-004"></span>**ORNA-CLONE-004** Independent clones MUST NOT run the same durable external consumer concurrently unless its connector profile explicitly supports safe partitioning, coordination or idempotent duplicate processing. Base conformance provides no cross-clone exactly-once guarantee.


## 19.7 Repository compatibility {#repository-repository-compatibility}

<span id="ORNA-CORE-001"></span>**ORNA-CORE-001** An Orna repository MUST be a valid Git repository readable by ordinary Git tooling.

<span id="ORNA-CORE-002"></span>**ORNA-CORE-002** Orna MUST NOT substitute a proprietary commit graph for Git.

<span id="ORNA-CORE-003"></span>**ORNA-CORE-003** A Git commit used as an Orna database snapshot MUST identify the code, schema, human-readable rows, large-table manifests, referenced large-table objects and committed system watermarks required to reproduce that snapshot.


# 20. Embedded ownership and local durability {#embedded}

## 20.1 Purpose {#embedded-purpose}

Turso provides local transactional state for:

- pending high-rate rows;
- current checkpoints;
- retry/failure state;
- publication intents;
- runtime system observations that have not been published;
- local indexes that need transaction coupling.

<span id="ORNA-TURSO-001"></span>**ORNA-TURSO-001** The runtime MUST use an embedded Turso/SQLite-compatible database, not require a Turso server, for the local CWD state profile.

<span id="ORNA-TURSO-002"></span>**ORNA-TURSO-002** Runtime row writes and corresponding checkpoint updates MUST occur in one Turso transaction.

<span id="ORNA-TURSO-003"></span>**ORNA-TURSO-003** The runtime MUST use durability settings appropriate to its claimed failure model and MUST document whether process crash, OS crash and power loss are covered.

## 20.2 Local ownership {#embedded-local-ownership}

<span id="ORNA-TURSO-004"></span>**ORNA-TURSO-004** At most one local process may own the writable embedded state when the selected Turso mode only supports single-process access.

<span id="ORNA-TURSO-005"></span>**ORNA-TURSO-005** Other local commands SHOULD communicate with the temporary owner over a local IPC socket when one exists.

<span id="ORNA-TURSO-006"></span>**ORNA-TURSO-006** When no owner exists, any command requiring access MAY become the owner for its lifetime.

The coordination process is created as needed; local operation does not require a persistent daemon.


## 20.3 Git implementation boundary {#embedded-git-implementation-boundary}

<span id="ORNA-IMPL-001"></span>**ORNA-IMPL-001** Implementations MAY use `gix`, libgit2 or the Git executable provided produced objects/refs and observable behavior conform to standard Git.


# 21. Branching, staging and checkout {#branching}

## 21.1 Branches and pending changes {#branching-branches-and-pending-changes}

A branch is a Git reference to a commit, not a container for uncommitted work. `HEAD` selects the current branch or a detached commit. The worktree, its staging area and its local durable CWD overlay belong to the worktree instance.

```bash
git branch experiment       # create at HEAD; remain on the current branch
git switch -c experiment    # create at HEAD and select it
```

The corresponding `orna` commands preserve these meanings. Neither command creates a commit. Creating and selecting a branch at the current `HEAD` carries staged changes, unstaged changes and pending logical database changes unchanged. The new branch's committed history remains the same as the old `HEAD` until a commit is made.

<span id="ORNA-BRANCH-001"></span>**ORNA-BRANCH-001** Creating a branch without an explicit start point MUST resolve the current Git `HEAD`, create the new branch by compare-and-set against nonexistence, and leave CWD and the index unchanged. The operation MUST NOT commit pending data.

<span id="ORNA-BRANCH-002"></span>**ORNA-BRANCH-002** Selecting a new branch at the same commit MUST preserve the staged/unstaged distinction, pending table changes, local allocator high-water marks and source checkpoints. A branch switch cannot reset an allocator or discard unpublished rows merely because they are not in Git yet.

<span id="ORNA-BRANCH-003"></span>**ORNA-BRANCH-003** Switching to another commit MUST carry nonconflicting local changes using Git-compatible worktree/index rules and equivalent logical row/schema checks. If a change would be overwritten, or the resulting candidate database would be invalid, the default operation MUST fail with CWD, index and `HEAD` unchanged.

<span id="ORNA-BRANCH-004"></span>**ORNA-BRANCH-004** A historical snapshot containing no Git commit is not a branch start point. An unborn `HEAD` fails `create_branch` without an explicit committed start point. Selecting a new unborn symbolic branch with `orna switch -c` is separately defined in [administration](#administration). Orphan-branch creation remains a separate explicit Git operation and does not manufacture an empty commit.

An explicit committed start point may be a commit, peeled tag or another branch. Snapshot references must belong to this repository or resolve to a commit reachable through an explicitly attached repository import; a database identifier alone does not authorise cross-repository reference creation.

### 21.1.1 Staging logical changes {#branching-staging-logical-changes}

`orna add` stages the selected logical CWD generation for the named paths or tables. Later edits remain unstaged. For loose rows, the staged tree is the normal Git index. For pending compact rows, Orna maintains a staged logical delta bound to the index generation, and builds its staged immutable objects without consuming newer unstaged deltas. A stage record stores its base commit, per-key before/after state and schema revision.

`orna commit` and `sys.admin.commit` commit the staged state, not every value visible in CWD. They validate the staged candidate schema and assertions, build a commit from that candidate, and preserve unstaged changes against the resulting commit. An empty staged delta is a no-op error unless an explicit Git-compatible empty-commit option is used. Automatic publication has a distinct rule: it publishes only the frozen runtime batch and never unrelated user staging.

A plain Git commit can operate on materialised staged paths. It cannot implicitly publish rows that have never been materialised or staged into its index. `orna status` reports that remaining local tail. A later Orna command reconciles the actual `HEAD` with its index-generation record and refuses stale staging instead of silently selecting an old overlay.

### 21.1.2 Checkout preview and consent {#branching-checkout-preview-and-consent}

`sys.admin.plan_checkout(target)` is read-only. It resolves the target and returns `sys.CheckoutPlan`: the exact target commit/branch, expected `HEAD`, CWD generation, index and worktree digests, pending generation, affected consumers, conflicts and any changes a destructive checkout would discard.

The plan token is SHA-256 over the canonical typed plan, excluding the token itself. It is a state precondition, not an authentication credential. `sys.admin.checkout(..., expected_plan: token)` rechecks it while holding the worktree mutation lock. Any relevant change produces `sys.git.stale_plan`.

A nonforced checkout needs no preview token when it can recompute and apply a safe carry-forward under the lock. A forced checkout requires both `force: true` and a matching plan token. The host must have obtained explicit consent for the listed discard set; a noninteractive caller expresses that consent by supplying both arguments. Force cannot bypass schema validity, assertions, object availability, active-transaction fencing or repository-integrity checks.

The target form preserves attachment semantics: a `BranchRef`, or a string resolving unambiguously to a local branch, selects that branch; an exact commit, snapshot, object ID or tag selects detached `HEAD`. No implicit branch guessing occurs for an ambiguous string.

### 21.1.3 Checkout algorithm CHECKOUT-1 {#branching-checkout-algorithm-checkout-1}

1. Acquire the worktree mutation lock, observe the current symbolic or detached `HEAD`, and prevent new activation admission during the switch.
2. Wait for ordinary active transactions to reach a boundary. A forced operation may request their cancellation and join cleanup, but may not terminate unrelated worktrees. Reject reentrant checkout from an activation that owns uncommitted table writes.
3. Resolve the committed target, obtain required objects and validate its source graph, schema, stored values, assertions and checkpoint formats in isolation.
4. Compute the staged, unstaged and pending logical carry-forward. Conflicting local edits cause `sys.git.dirty_conflict` unless the exact destructive plan has been authorised. Untracked or ignored paths are never overwritten as an incidental consequence of resolving a database change.
5. Pause affected consumers at completed item/batch boundaries. Preserve their worktree-local progress when switching to the same state or carrying compatible changes. If their checkpoint/code/source configuration is incompatible, leave them paused and report the reason; do not silently restart at a guessed position.
6. Recheck the plan, index, worktree and `HEAD` preconditions. Write a durable transition journal containing before/after state and exact discard consent, then apply the Git index/worktree and logical CWD transition as one recoverable generation change.
7. Publish the selected `HEAD` and CWD generation only when their relationship is recoverable. Existing pinned readers continue reading their old generation; new readers observe the new generation. Complete the journal and release admission.
8. Resume only consumers proven compatible with the resulting state. A consumer requiring an explicit reset remains paused. Return the selected snapshot and make the operation inspectable through invocation/change metadata.

On a crash, recovery compares journalled object identities and generations; it completes the recorded transition or preserves the old state. It never runs a blind hard reset over subsequently changed files. Unexpected external edits are preserved and reported as conflicts.



# 22. Publication and crash recovery {#publication}

The logical CWD combines committed rows with durable pending mutations. Publishing changes their representation and replication boundary: it makes an already committed local activation available through a Git commit. It is not a second execution of that activation.

## 22.1 Pending state and policy {#publication-pending-state-and-policy}

<span id="ORNA-HOT-001"></span>**ORNA-HOT-001** High-rate writes MUST become durable and queryable in embedded Turso before publication.

<span id="ORNA-HOT-002"></span>**ORNA-HOT-002** Status output MUST summarise pending high-rate changes rather than emit one path per pending row.

<span id="ORNA-PUB-001"></span>**ORNA-PUB-001** The default compact writer target is 16 MiB of compressed data, with an 8–32 MiB normal tuning range and the compact profile's 64 MiB file bound. An implementation MAY choose a target within that range but MUST expose the effective policy. A changed tuning value does not change logical rows or checkpoint meaning.

<span id="ORNA-PUB-002"></span>**ORNA-PUB-002** A maximum pending age MAY cause publication of a smaller complete batch; the profile default is 60 seconds.

<span id="ORNA-PUB-003"></span>**ORNA-PUB-003** Publication policy MUST NOT introduce a different table declaration kind.

<span id="ORNA-PUB-004"></span>**ORNA-PUB-004** Effective policy and pending/published state MUST be visible through `sys.Storage` and runtime maintenance metadata.

## 22.2 Index and worktree invariants {#publication-index-and-worktree-invariants}

Let **H** be the captured old commit tree, **N** the proposed publication tree, **I** the ordinary Git index, **W** the ordinary working tree, and **P** the frozen runtime mutation batch. The publisher builds **N** from **H + P**, not from **I** or **W**.

The publisher must also compute a reconciled index **I′** and worktree **W′**. Their purpose is to preserve the user's staged difference from the new `HEAD` and the user's unstaged difference from the new index. Merely changing `HEAD` while leaving **I = H** would stage the inverse of **P**. A later ordinary commit could then reverse the publication.

For every managed path changed by **P**, automatic publication requires that its existing index/worktree entries agree with the captured managed base or a journalled Orna projection of the same batch. A conflicting human edit pauses publication. For unrelated paths, staged and unstaged differences are preserved independently, including partially staged files, deletions, file modes and untracked paths. The publisher does not use a hard reset or a blanket `git add`.

<span id="ORNA-PUB-010"></span>**ORNA-PUB-010** A publication commit MUST use a private tree/index built only from the captured base and frozen runtime batch. It MUST NOT commit unrelated human edits.

<span id="ORNA-PUB-011"></span>**ORNA-PUB-011** Before publication is reported complete, the ordinary index MUST be reconciled so it contains no accidental staged reversal of published managed paths, while preserving every unrelated staged change and staged/unstaged boundary.

<span id="ORNA-PUB-012"></span>**ORNA-PUB-012** Unresolved merges, rebases, overlapping managed-path edits or unreconcilable index state MUST pause automatic publication. Durable ingestion may continue within configured storage limits; exhaustion fails explicitly rather than discarding the tail.

## 22.3 Publication algorithm PUB-1 {#publication-publication-algorithm-pub-1}

The following ordering specifies observable durability and recovery requirements. An implementation may use equivalent platform primitives but cannot omit ordinary-index reconciliation.

1. **Freeze.** In a short Turso transaction, select a contiguous committed mutation range, allocate its batch identity, and record the included source/checkpoint watermarks and base generation. Mark it `preparing`; do not remove the pending mutations. New activations may append after this range.
2. **Capture.** Under the worktree publication lock, capture the symbolic/detached `HEAD` identity and old object ID, normal index digest and relevant worktree states. Refuse conflicting merge/rebase state and managed-path edits. Resolve `.git` paths through Git's per-worktree administrative directory.
3. **Encode.** Produce immutable segments, schemas and manifests for the frozen batch. Fold repeated mutations in transaction order to one authoritative final mutation per key. Write and flush complete Git objects. Objects not subsequently referenced may remain unreachable.
4. **Build candidate.** With a private index, build **N = H + P** and a commit whose parent is the captured `HEAD`. Human staged or unstaged contents are not inputs. A table publication generation comes from that table's captured manifest.
5. **Plan reconciliation.** Compute **I′** using a Git-compatible two-tree carry-forward from **H** to **N**, preserving unrelated staged changes. Compute **W′** preserving unrelated unstaged changes. Reject managed-path overlaps. Validate staged and CWD database candidates separately; carrying human work cannot make either candidate silently invalid.
6. **Journal.** Durably record the batch, expected/target refs, old/new index bytes or immutable equivalents, affected old/new worktree entries, object hashes, generation and cleanup watermark. Journal data is local metadata, not ordinary committed source. It must be sufficient to distinguish every crash boundary below.
7. **Lock and revalidate.** Acquire the ordinary Git `index.lock` before exposing a changed ref. Hold it until the reconciled index is installed. Obtain the Git ref transaction/expected-old-value protection and recheck the captured symbolic `HEAD`, index and relevant files. Abort if they changed. Cooperating Orna worktree mutators remain excluded by the publication lock.
8. **Prepare files.** Write temporary replacement files and flush them. For each existing affected file, preserve the displaced bytes in the recovery journal/quarantine before installing a replacement. If an external editor changed the path after capture, retain those bytes and stop with a conflict rather than overwrite them. Filesystem writers outside the locking protocol cannot participate in the multi-file transaction. Their conflicting edits must be preserved for reconciliation.
9. **Advance and reconcile.** Advance the target ref using compare-and-set against the old object ID. While the ordinary index remains locked, atomically install **I′** and finish the planned worktree replacements, recording progress. If an external Git process moves a ref after the protected update, recovery treats it as a new state to reconcile; it never forces the ref back.
10. **Publish local boundary.** In Turso, record the publication commit and completion state, then consume only the batch's frozen pending range. Newer tail mutations remain. Logical readers switch using the batch watermark so a mutation is visible exactly once: either through the old commit plus tail, or through the new commit with that batch masked out of the tail.
11. **Complete.** Flush the final journal state, release the normal index lock and worktree lock, and mark the batch complete. Notify storage observers. Do not emit an insert/delete row delta merely because a row moved into a compact file.

<span id="ORNA-PUB-005"></span>**ORNA-PUB-005** Complete objects and their required durability barriers MUST precede a visible ref that names them.

<span id="ORNA-PUB-006"></span>**ORNA-PUB-006** Failure before ref advancement MUST preserve the pending batch. Unreferenced objects are permitted; a false successful publication result is not.

<span id="ORNA-PUB-007"></span>**ORNA-PUB-007** Failure after ref advancement MUST recover index/worktree reconciliation as well as Turso cleanup. Recognising the batch in the committed manifest is necessary but not sufficient.

<span id="ORNA-PUB-008"></span>**ORNA-PUB-008** A reader MUST observe the old snapshot plus the unpublished tail or the new snapshot with the published batch excluded from the tail. It MUST NOT observe duplicate or missing logical rows.

<span id="ORNA-PUB-009"></span>**ORNA-PUB-009** Ref updates MUST use expected-old-object compare-and-set and revalidate the selected branch/`HEAD` relationship. Concurrent commits MUST NOT be overwritten.

<span id="ORNA-PUB-016"></span>**ORNA-PUB-016** Recovery MUST preserve unrelated staged and unstaged changes. An index lock left by a crashed publisher may be removed only after owner-liveness and journal checks establish that it is that publisher's abandoned lock.

<span id="ORNA-PUB-017"></span>**ORNA-PUB-017** A normal Git commit made after completed publication MUST not reverse published managed data unless the user explicitly staged such a reversal.

### 22.3.1 Recovery states {#publication-recovery-states}

| Observed state | Required action |
|---|---|
| Ref still at H; no candidate visible | Restore only journal-owned partial projections when their hashes match; keep P pending; release abandoned locks safely. |
| Ref at N; ordinary index still I | Complete I′ and W′ reconciliation from the durable journal before admitting further Orna commits; preserve unexpected edits as conflicts. |
| Ref at N; index I′; cleanup absent | Verify the batch manifest, apply the visibility watermark and finish idempotent tail cleanup. |
| Ref at N; cleanup complete | Verify journal completion; remove only temporary/quarantined data no longer needed and explicitly resolved. |
| Ref no longer H or N | Preserve journal and user files, inspect ancestry/batch presence and reconcile under the new ref. If safety cannot be proved, stop with a recovery conflict. Never overwrite the newer ref. |
| Index or affected path differs from both recorded states | Preserve that state; report a typed index/worktree conflict. Do not interpret it as permission to reset. |

A ref update and an index-file rename are not one filesystem transaction. The lock, journal and recovery procedure establish the safe boundary. A command that bypasses Git's index lock and writes arbitrary files is outside cooperative execution, but its unexpected contents must still be preserved when detected.

## 22.4 Shutdown and transfer {#publication-shutdown-and-transfer}

<span id="ORNA-PUB-013"></span>**ORNA-PUB-013** Graceful writer shutdown SHOULD publish eligible batches after its children have terminated and open activations have reached a boundary. A pending publication conflict does not justify discarding the local tail.

<span id="ORNA-PUB-014"></span>**ORNA-PUB-014** Status MUST report remaining unpublished CWD changes.

<span id="ORNA-PUB-015"></span>**ORNA-PUB-015** Transferring resumable execution to another clone requires publishing and pushing the recoverable state, then fetching the resulting snapshot in the receiving clone. Loss of an unpublished local tail can be repaired only by replayable source data or an independently preserved copy of that tail.

Example publication message:

```text
orna: publish runtime data

sensors.Reading     48,120 rows
warehouse.Event       240 rows
```



# 23. Physical storage and representation {#storage}

## 23.1 Logical transparency {#storage-logical-transparency}

<span id="ORNA-STORAGE-001"></span>**ORNA-STORAGE-001** Editable and compact storage MUST expose the same logical table, query and mutation interface.

<span id="ORNA-STORAGE-002"></span>**ORNA-STORAGE-002** Callers MUST NOT need to know the physical profile to query a table.

## 23.2 Loose profile {#storage-loose-profile}

The loose profile stores one row per `.orna` row unit and is optimized for human editing and Git-style browsing.

<span id="ORNA-STORAGE-003"></span>**ORNA-STORAGE-003** Loose profile row paths MUST follow the table namespace, table name and key path encoding.

<span id="ORNA-STORAGE-004"></span>**ORNA-STORAGE-004** Direct row-file edits MUST remain reviewable through ordinary Git diffs.

## 23.3 Compact storage profile {#storage-compact-storage-profile}

Large or high-rate rows use **compact storage**: immutable, key-sorted batches encoded as **Apache Parquet with Zstandard compression** under the repository-format-1 profile.

<span id="ORNA-COMPACT-001"></span>**ORNA-COMPACT-001** Compact storage MUST expose exactly the same logical ordered key-to-row table, query, mutation, history and merge semantics as editable row storage.

<span id="ORNA-COMPACT-002"></span>**ORNA-COMPACT-002** Every compact data file MUST be a valid Parquet file using data-page version 2, Zstandard compression, page checksums and complete rows sorted by the table's full canonical primary key.

<span id="ORNA-COMPACT-003"></span>**ORNA-COMPACT-003** Stable Orna column `ObjectId` values, logical types, units, currency identities, schema revision and physical encodings MUST be recorded in file metadata. Readers MUST resolve columns by stable identity rather than current display name.

<span id="ORNA-COMPACT-004"></span>**ORNA-COMPACT-004** Computed fields MUST NOT be physically stored as authoritative row values. They are evaluated from stored dependencies under the requested snapshot.

<span id="ORNA-COMPACT-005"></span>**ORNA-COMPACT-005** The writer target is 16 MiB compressed per data file, with a normal range of 8-32 MiB and a hard maximum of 64 MiB. A row group closes at 65,536 rows or 16 MiB uncompressed, whichever occurs first. An age-triggered publication MAY produce a smaller valid file.

<span id="ORNA-COMPACT-006"></span>**ORNA-COMPACT-006** New rows create immutable `data` files. Changes to existing keys create immutable complete-row `replacement` files. Deletions create immutable key-only `deletion` files. Within one branch lineage, the visible mutation for a key with the greatest publication generation wins. Numeric generations MUST NOT be used to choose a winner between concurrent branch mutations; semantic three-way merge resolves or conflicts them.

<span id="ORNA-COMPACT-007"></span>**ORNA-COMPACT-007** Compact files MUST NOT be routinely rewritten merely to combine older batches. An explicit consolidation is permitted only when overlays materially harm performance; its estimated extra retained history MUST be shown before execution and its semantic diff MUST be empty.

<span id="ORNA-COMPACT-008"></span>**ORNA-COMPACT-008** Each table's compact state MUST be described by a sharded canonical manifest. A shard contains at most 256 entries. Every entry records segment identity, role, profile and encoder version, schema fingerprint, Git object ID, key bounds, applicable time bounds, row count, compressed size, column descriptors and index availability; physical statistics are in the verified Parquet metadata.

<span id="ORNA-COMPACT-009"></span>**ORNA-COMPACT-009** Query planning MUST prune manifest shards, files, row groups and pages whose key/statistic bounds cannot satisfy the query, and MUST read only required columns where the physical format supports projection.

<span id="ORNA-COMPACT-010"></span>**ORNA-COMPACT-010** Unknown required profile or encoder versions MUST be rejected before partial logical results are returned.

<span id="ORNA-COMPACT-011"></span>**ORNA-COMPACT-011** Publication MUST follow [PUB-1](#publication), including ordinary-index reconciliation and local watermark recovery. A visible committed snapshot MUST never refer to an incomplete file or leave a staged inverse of managed publication data.

<span id="ORNA-COMPACT-012"></span>**ORNA-COMPACT-012** The format MUST support exact Orna values. Common primitive columns use standard Parquet logical/physical encodings. Values outside a selected optimized physical representation use a versioned lossless fallback encoding; they MUST NOT be rounded or passed through a binary `Float` implicitly.

<span id="ORNA-COMPACT-013"></span>**ORNA-COMPACT-013** A production compact-storage claim requires the published benchmark/fault profile: at least 10,000 samples/second for 24 hours on declared hardware, zero lost acknowledged rows, zero duplicate logical rows, bounded memory, stable storage no larger than 1.5 times the same corpus in Prometheus blocks, selective pruning, and all publication fault points passing. Failure blocks the production claim; it does not change the normative format or table semantics.

<span id="ORNA-COMPACT-014"></span>**ORNA-COMPACT-014** A publication attempt MUST read the table manifest from its expected base HEAD and use that manifest's `next_generation` as the candidate generation. All files created by the attempt share that generation. Before encoding, pending mutations are collapsed to one final mutation per key, so one generation cannot contain two authoritative mutations for the same key. Compact-storage precedence has no separate semantic `sequence` number.

<span id="ORNA-COMPACT-015"></span>**ORNA-COMPACT-015** If branch compare-and-swap fails, the candidate generation was never committed. The publisher MUST reload the current HEAD and table manifest and rebuild the manifest using the generation obtained from that base. Immutable data files MAY be reused because generation is manifest metadata; a stale candidate generation MUST NOT be forced onto a changed table manifest.

<span id="ORNA-COMPACT-016"></span>**ORNA-COMPACT-016** Independent branches MAY assign the same generation to disjoint mutations. During semantic merge, generation never resolves concurrent changes to the same key: identical mutations coalesce, different mutations create a typed row conflict, and disjoint mutations coexist. The merged manifest sets `next_generation` greater than every retained generation; an explicit conflict resolution is written in a fresh generation.

<span id="ORNA-COMPACT-017"></span>**ORNA-COMPACT-017** A compact `Float` column that emits bounds MUST declare Parquet `IEEE_754_TOTAL_ORDER`. Every emitted column-chunk `Statistics` object MUST contain exact `nan_count`, and every emitted `ColumnIndex` MUST contain one exact `nan_counts` entry per page. Bounds MUST be the smallest and largest non-NaN values under `FLOAT-TOTAL-1`; if all non-null values in the scope are NaN, they MUST instead be the smallest and largest NaN values under that order. Missing, malformed or inconsistent NaN counts make NaN presence unknown and MUST disable every pruning decision that is not safe under that uncertainty. Float statistics MUST never cause a false-negative query result.

<span id="ORNA-COMPACT-018"></span>**ORNA-COMPACT-018** A compact file's `schema_id` identifies the schema revision used to encode it. Reading that file through another compatible snapshot MUST project columns by stable field `ObjectId` and apply the schema-evolution rules: renamed fields retain identity, missing optional fields become `null`, frozen introduction fallbacks are read from committed schema metadata, and computed fields are evaluated rather than loaded.

<span id="ORNA-COMPACT-019"></span>**ORNA-COMPACT-019** In a valid committed manifest lineage, two authoritative mutations for the same key and generation are corruption. The condition does not apply to unresolved inputs from independent branches before semantic merge; those inputs are compared as branch changes and either coalesced or reported as conflicts.

The following sections define physical mappings, manifest schemas and [cross-reader tests](#storage-cross-reader-obligation).

## 23.4 Hybrid storage and placement {#storage-hybrid-storage-and-placement}

One table may contain some keys as editable `.orna` row files and other keys in compact batches. This avoids a destructive whole-table mode switch.

<span id="ORNA-STORAGE-005"></span>**ORNA-STORAGE-005** In a valid snapshot, one logical key MUST have exactly one authoritative physical representation. A loose row and compact row for the same key is corruption unless an in-progress publication shadow is hidden from snapshot readers and has an identical canonical row hash.

<span id="ORNA-STORAGE-006"></span>**ORNA-STORAGE-006** Direct creation of a valid row file creates an editable row. Updating an existing editable row through the table API preserves editable placement. Updating or deleting an existing compact row produces a compact replacement or deletion.

<span id="ORNA-STORAGE-007"></span>**ORNA-STORAGE-007** Every table has a committed placement preference with one of three values: `automatic`, `editable` or `compact`. The default is `automatic`. The preference is operational metadata, not part of the table's logical type.

<span id="ORNA-STORAGE-008"></span>**ORNA-STORAGE-008** Under `automatic`, a table with no compact data publishes programmatic rows as editable while the resulting table has at most 10,000 rows, the publication contains at most 8 MiB of canonical row bodies and every row path is representable. Otherwise that publication uses compact storage. Once compact data exists, later programmatic insertions default to compact storage; existing editable rows remain editable.

<span id="ORNA-STORAGE-009"></span>**ORNA-STORAGE-009** `editable` requires programmatic new rows to use editable files and fails before mutation when a key/path cannot be represented. `compact` directs programmatic new rows to compact storage. Neither preference prevents an explicit valid direct row-file edit, so a table can remain hybrid.

<span id="ORNA-STORAGE-010"></span>**ORNA-STORAGE-010** Advanced storage preference and rewrite operations use explicit typed administration. A caller obtains the table's `sys.TableRef`, then calls `sys.admin.set_storage_preference(table.reference, sys.StoragePreference.editable)` or `sys.admin.rewrite_storage(table.reference, to: sys.StorageRewriteTarget.compact)`. `sys.storage` remains a read-only grouping namespace; these operations add no language grammar or built-in CLI command family.

<span id="ORNA-STORAGE-011"></span>**ORNA-STORAGE-011** A rewrite between editable and compact placement is one recoverable storage-only operation. It MUST preserve every logical key/value and snapshot ordering, produce an empty semantic row diff, and refuse conversion to editable form when any key/path or resource bound would be violated.

<span id="ORNA-STORAGE-012"></span>**ORNA-STORAGE-012** Storage placement is visible through `sys.Storage`, but ordinary table reads and writes MUST NOT require callers to branch on physical placement.

<span id="ORNA-STORAGE-013"></span>**ORNA-STORAGE-013** Editable/compact key disjointness MUST be checked when a program inserts a row, when direct row files are discovered or reconciled, when a snapshot is opened or checked out, when a row is re-keyed, when storage is rewritten, when compact data is published, when repositories are verified, and when semantic merge constructs a candidate result.

<span id="ORNA-STORAGE-014"></span>**ORNA-STORAGE-014** A disjointness check MUST establish exact key existence. Manifest and shard bounds MAY reject impossible overlaps quickly, but range overlap alone MUST NOT be treated as an exact duplicate. If a range can contain the key, the implementation MUST use an exact index lookup or scan sufficient to prove presence or absence; it MUST NOT decompress every compact row when indexed lookup can decide the question.

<span id="ORNA-STORAGE-015"></span>**ORNA-STORAGE-015** If an editable/compact duplicate is found during insert, discovery, checkout, publication, re-key or verification, no representation may silently overwrite the other. A merge reports a typed conflict and leaves CWD unchanged when the conflict budget is exceeded. A recoverable rewrite MAY hold temporary physical shadows only behind one generation barrier and only when their canonical row hashes are identical.


## 23.5 Compact repository layout {#storage-compact-repository-layout}

For table ObjectId `T`, committed metadata is rooted at:

```text
.orna/storage/T/
├── policy.orna
├── manifest.orna
├── shards/
│   └── <shard-number>.orna
└── data/
    └── <first-two-id-chars>/<segment-id>.parquet
```

`segment-id` is a UUIDv7 generated before file creation. The manifest stores the actual Git object ID and a SHA-256 content checksum; path names are locators, not integrity identities.

A table may also have editable rows under its ordinary table directory. One key may not be authoritative in both places.


## 23.6 Segment closure {#storage-segment-closure}

Writer defaults:

```text
target compressed file       16 MiB
normal writer range            8-32 MiB
hard file maximum             64 MiB
row-group maximum             65,536 rows
row-group uncompressed max    16 MiB
age publication default       60 seconds
```

A file closes when the target is met at a row-group boundary, at the hard maximum, or when age-triggered publication flushes complete pending rows. A single valid row larger than the normal limits may occupy one file up to the hard maximum; larger rows are not supported by this profile and remain editable or receive a typed error.


## 23.7 Generations and visibility {#storage-generations-and-visibility}

Each file has one role:

- `data`: newly inserted complete rows;
- `replacement`: complete replacement rows for keys already visible in an ancestor layer;
- `deletion`: complete primary keys only.

A **publication generation** is the precedence unit within one branch lineage.

1. A publication attempt reads the compact table manifest from its expected base `HEAD`.
2. Its candidate generation is exactly that manifest's `next_generation`.
3. Every data, replacement and deletion file created by that publication shares the candidate generation.
4. Before files are encoded, all pending mutations in the frozen publication batch are folded in transaction order to one final mutation per logical key. An insert followed by deletion of a key absent from the base produces no compact mutation; other chains produce one complete final row or one deletion.
5. Within one lineage, the visible mutation for a key with the greatest generation wins.
6. A generation has no separate semantic sequence number. Segment ordering is metadata ordering only and cannot change row precedence.
7. Two authoritative mutations for the same key and generation in one valid committed lineage are corruption.

Generations do not decide between concurrent Git branches. Two branches may independently allocate the same numeric generation. Semantic three-way merge compares their logical key changes against the merge base:

- identical changes to one key coalesce;
- different changes to one key create a row conflict;
- disjoint key changes coexist even when their numeric generations are equal.

A conflict resolution is written as a new mutation in a fresh generation of the merged manifest.

### 23.7.1 Publication allocation and retry {#storage-publication-allocation-and-retry}

The manifest's `next_generation` is the authority for that table in that snapshot. One local publisher serializes publication attempts for a table/branch within a clone.

If branch compare-and-swap fails, the candidate generation was never committed. The publisher reloads current `HEAD` and the current table manifest, then obtains a candidate generation from that base. It may reuse already completed immutable Parquet files when their logical contents remain valid, because generation is carried only by manifest metadata. It must rebuild the manifest and must not force a stale generation onto a changed table manifest. The number may remain unchanged only when the table manifest itself is unchanged by the intervening commit.

After a successful semantic merge, `next_generation` is one greater than the greatest generation retained by the merged table. Generation numbers need not be contiguous; failed attempts and merge normalization may leave gaps.


## 23.8 Exact physical encoding profile {#storage-exact-physical-encoding-profile}

This profile adopts the Apache `parquet-format` **2.13.0** Thrift and logical-type definitions. `2.13.0` is the specification release, not the file's `FileMetaData.version`. Writers set that field to 1, use Data Page V2 headers, and advertise actual encodings in column metadata. A library option named `version="2.6"` is not a file-format identifier or a substitute for this profile.

Value pages use PLAIN or RLE_DICTIONARY encoding; definition/repetition levels use the standard RLE encoding. Readers must support both value encodings. Other value encodings require a separately negotiated storage profile and are not silently emitted here. Each compressed page is a standard Zstandard frame without an out-of-band dictionary, with the page checksum required by the profile. Readers apply configured decoded-size bounds before allocating buffers.

Every file contains UTF-8 key-value metadata:

| Key | Exact value |
|---|---|
| `orna.profile` | `compact-storage-v1` |
| `orna.table` | Lowercase canonical table UUID. |
| `orna.schema.sha256` | Lowercase 64-hex logical schema fingerprint. |
| `orna.schema.ovb` | Padded standard Base64 of the exact OVB-1 schema descriptor. |
| `orna.columns.ovb` | Padded standard Base64 of the physical column descriptors. |
| `orna.encoder` | Producer/version text, diagnostic only. |

The physical column descriptor array is ordered by physical leaf path. Each entry is `[field_id_path, physical_path, logical_type_node, encoding_kind, parameters]`. `field_id_path` contains field UUIDs and reserved structural components `list`, `element`, `value` or tuple ordinals; `physical_path` is the exact Parquet path. Encoding kinds and parameters are fixed below. Unknown mappings fail before data interpretation.

A top-level field's physical name is `f_` followed by its lowercase 32-hex UUID without hyphens. Nested nominal fields use the same form; structural fields use `n_` followed by lowercase hex UTF-8 bytes of the NFC name. A Parquet numeric `field_id`, when supplied, is only a file-local ordinal; stable Orna field identity comes from the UUID mapping. Adding a column may change ordinals but cannot change its logical identity.

| Orna type | Encoding kind | Physical representation and parameters |
|---|---|---|
| Bool | `bool` | BOOLEAN; parameters `[]`. |
| Float | `float64` | DOUBLE; parameters `[]`; canonical quiet NaN on write, signed zero retained. |
| Str | `utf8` | BYTE_ARRAY with STRING logical annotation; parameters `[]`. |
| Blob | `blob` | Unannotated BYTE_ARRAY; parameters `[]`. |
| Uuid / UUID-backed ID | `uuid` | FIXED_LEN_BYTE_ARRAY(16), UUID annotation; network-order bytes; parameters `[opaque_type_id_or_null]`. |
| Date | `date` | INT32 DATE, signed days from 1970-01-01; parameters `[]`. |
| Instant | `instant_ns` | INT64 TIMESTAMP(NANOS, adjustedToUTC=true), when every value fits signed 64-bit nanoseconds; parameters `[]`. |
| Duration | `duration_ns` | Unannotated INT64 nanoseconds when every value fits; parameters `[]`. |
| Int | `int64` | Signed INT64 when every value fits; parameters `[]`. |
| Int | `ovb` | BYTE_ARRAY containing the exact OVB-1 integer for each value; parameters `[1]`. |
| Decimal | `decimal` | Standard DECIMAL, parameters `[precision, scale, storage_width]` as specified below. |
| Decimal / out-of-range time | `ovb` | BYTE_ARRAY with the complete tagged OVB-1 value; parameters `[1]`. |
| Money / Quantity | `numeric` | Underlying exact/numeric column, parameters `[numeric_mapping, nominal_currency_or_unit_id]`. |
| Option<T> | `option` | OPTIONAL group containing the T representation under `value`; parameters `[]`. Group absent means null; present with value means Some. |
| List<T> | `list` | Standard three-level LIST: group → repeated `list` → `element`; T mapping determines element structure. |
| Record / tuple | `record` / `tuple` | Group with each declared field/component mapping. Empty record/Unit uses required BOOLEAN `_unit = true`. |
| Enum | `enum` | Group with required FIXED_LEN_BYTE_ARRAY(16) `variant`, and at most one optional `v_<variant-id>` payload group. Payload-free chosen variant has no payload group. |
| Refined value | `refined` | Base representation with nominal type ID in parameters; decoding enforces its refinements. |
| Stored reference | `reference` | Group with required database/table UUIDs and complete canonical key-component mappings. |
| Other serialisable composite | `ovb` | BYTE_ARRAY of complete OVB-1 value when this fallback is explicitly chosen in the descriptor. |

The `ovb` fallback is permitted for any serialisable type, not only values outside an optimised numeric range. A reader that supports this profile must always support the fallback. It is never permissible to reinterpret a BYTE_ARRAY as an integer, decimal or enum without its descriptor.

For standard DECIMAL, precision is 1…38 and scale is 0…precision. The unscaled integer is exactly `value × 10^scale`; every value must produce an integer fitting the declared precision. Precision up to 9 uses INT32, up to 18 uses INT64, otherwise FIXED_LEN_BYTE_ARRAY of the smallest length n such that `10^precision−1 < 2^(8n−1)`. Fixed bytes are signed big-endian two's complement with sign extension to that exact width. Values that cannot share the declared precision/scale use OVB fallback; rounding is forbidden. Money's currency minor digits are not permission to round its stored amount.

Option is represented by an optional **group**, not merely by making the underlying scalar optional. This matters for `T??`: an absent outer group and a present group whose inner group is absent represent `null` and `Some(null)` respectively. Lists preserve null elements, empty lists and absent optional lists distinctly. A chosen empty enum payload is represented by its payload group with `_unit = true`; this differs from a payload-free variant.

<span id="ORNA-STORAGE-FORMAT-001"></span>**ORNA-STORAGE-FORMAT-001** Physical descriptors MUST fully specify every field's mapping. A reader MUST reject incompatible or incomplete descriptors rather than infer types from column names or sample values.

<span id="ORNA-STORAGE-FORMAT-002"></span>**ORNA-STORAGE-FORMAT-002** Logical row/schema identity MUST use canonical logical values. Parquet file hashes identify physical bytes; two valid encoders may produce different physical files with identical logical row hashes.

### 23.8.1 Manifest fields {#storage-manifest-fields}

`manifest.orna` is canonical schema-directed Orna data with required fields `profile: Str`, `table: Uuid`, `schema: Digest`, `next_generation: Int` and `shards: [Shard]`. Profile is exactly `compact-storage-v1`; next_generation is positive. A Shard has `number: Int`, `min_key`, `max_key`, `entries: Int`, `file: Str`, and `hash: Digest`; key values use the table key schema. Numbers are nonnegative, bounds inclusive and entries in 1…256. An empty table has `shards: []`.

A shard file is the canonical record `{ entries: [...] }`. Each entry has exactly: `segment_id: Uuid`, `role: Str` (`data`, `replacement` or `deletion`), `generation: Int`, `schema_id: Digest`, `profile_version: Int` (1), `encoder_version: Str`, `relative_path: Str`, `git_object_id: sys.GitOid`, `sha256: Digest`, `min_key`, `max_key`, `min_event_time: Instant?`, `max_event_time: Instant?`, `row_count: Int`, `compressed_bytes: Int`, `columns: Blob`, `row_group_index: Bool`, and `bloom: Bool`. `columns` is the exact OVB physical-descriptor array. Optional times use `null`; one bound cannot be present without the other. Row count and bytes are positive, generation is positive and below next_generation, and paths are safe table-relative paths. Unknown fields require a different profile version.

A shard's file/hash must name its actual canonical bytes. Every entry's object ID, checksum, size, role, key bounds, schema and physical metadata are verified against the file before it is treated as authoritative. A partial clone may retain promised hashes without hydrating all bytes, but cannot mark an unverified absent blob as verified. Failure to hydrate is an availability error, not an empty table.

### 23.8.2 Cross-reader obligation {#storage-cross-reader-obligation}

A storage implementation must decode the golden OVB values and schema descriptors, write/read primitive and nested test files, and compare decoded logical rows with an independent Parquet implementation. Cross-reader tests cover nested Options, page checksums and floating-point statistics in addition to the canonical value vectors.

## 23.9 Statistics and safe pruning {#storage-statistics-and-safe-pruning}

Primary-key min/max and row count are required at manifest, file and row-group levels. Ordered non-secret columns record min/max and null count when values have a normative total ordering. Bloom filters are optional. Statistics are never trusted as row data; malformed, missing or inconsistent statistics cause the reader to scan more, never return false negatives.

Float statistics use the Parquet floating-point statistics contract and Orna's `FLOAT-TOTAL-1` bit order:

- `FileMetaData.column_orders` declares `IEEE_754_TOTAL_ORDER` for every physical `DOUBLE` used for Orna `Float`;
- every emitted column-chunk `Statistics` object contains exact `nan_count`, including zero;
- every emitted page-level `ColumnIndex` contains `nan_counts` with exactly one exact count per indexed data page;
- min/max use the smallest/largest non-NaN values under `FLOAT-TOTAL-1`;
- when every non-null value in a scope is NaN, min/max use the smallest/largest NaN in that scope under `FLOAT-TOTAL-1`;
- `-0.0` and `+0.0` remain distinct in ordering and bounds;
- a missing count does not mean zero: it means NaN presence is unknown;
- a malformed count, count-list length mismatch, negative count or count inconsistent with available value/null totals invalidates the affected statistics scope;
- invalid or missing Float statistics are ignored for pruning, while the data remains readable by scanning;
- no Float predicate may prune a scope unless the bounds plus NaN information prove that no row in the scope can match under Orna comparison semantics.

Examples of the last rule:

- `value == NaN` matches nothing under ordinary Orna equality and therefore does not need bounds;
- an explicitly named `is_nan(value)` predicate may prune only when a present exact count is zero;
- ordinary ordered comparisons ignore NaN rows because every such comparison with NaN is false, so non-NaN bounds may be used when they are valid;
- total-order sorting and total-order-specific functions use the total-order bounds and the exact NaN counts.

Orna ordinary comparisons still follow `ORNA-FLOAT-001`; the total order is used for sorting and physical/statistical ordering, not to redefine `NaN` comparison or equality.


## 23.10 Hybrid disjointness {#storage-hybrid-disjointness}

Editable and compact key sets are disjoint in every visible snapshot. The invariant is checked at all of these boundaries:

1. programmatic insert or upsert placement;
2. direct editable row discovery and working-tree reconciliation;
3. opening, checking out or verifying a committed snapshot;
4. explicit row re-key;
5. editable/compact storage rewrite;
6. compact publication;
7. semantic merge before a candidate result becomes CWD;
8. repository integrity verification.

For one candidate editable key, the implementation first uses manifest/shard key bounds to reject impossible compact matches. When a range may contain the key, it performs an exact indexed point lookup or a sufficient physical scan. A range overlap is not proof that the exact key exists. Implementations must not decode every compact row solely to check disjointness when the required exact index is available.

If a duplicate is found, neither representation silently wins or overwrites the other. Insert, discovery, checkout, re-key, publication and verification return a typed conflict/error. Semantic merge records a typed conflict and obeys the configured conflict/resource ceiling; exceeding the ceiling leaves CWD unchanged.

During a recoverable storage rewrite, temporary physical shadows may exist, but one publication-generation barrier hides one side from readers and the two canonical row hashes must be identical. Queries acquire one generation and never observe both.

This is normative for repository format 1.

## 23.11 Placement {#storage-placement}

A table's logical row map is the union of disjoint editable and compact key sets. The committed preference is `automatic`, `editable` or `compact`; it changes placement of future programmatic rows, never table type or query semantics.

`automatic` uses editable placement only while the table has no compact data, would remain at or below 10,000 rows, the pending publication is at or below 8 MiB of canonical row bodies and every path is valid. Otherwise the publication is compact. Existing editable rows are not silently converted.

Direct valid filesystem row creation is always an explicit editable insertion. Existing rows preserve placement on normal update. Explicit `sys.admin.rewrite_storage` is the only way to migrate existing rows between placements.

## 23.12 Disjointness enforcement {#storage-disjointness-enforcement}

One logical key may have one authoritative placement only. Orna checks exact editable/compact disjointness during:

1. programmatic insert or upsert placement;
2. direct row-file discovery and working-tree reconciliation;
3. snapshot open, checkout and repository verification;
4. explicit re-key;
5. storage rewrite;
6. compact publication;
7. semantic merge before the result becomes CWD.

Manifest and shard bounds are a pruning aid, not proof that a key exists. When a candidate key falls inside a compact range, Orna performs an exact point lookup through the compact key index or scans only the necessary physical range. It does not accept a duplicate because the host filesystem happens to hide one representation, and it does not decode the entire table when the exact index can answer the question.

A discovered duplicate is a typed storage conflict. No representation silently wins. A merge records the conflict; if the conflict budget is exceeded, CWD remains unchanged. During a recoverable rewrite, temporary physical shadows are allowed only behind the publication generation barrier and only when their canonical row hashes are identical.

## 23.13 Re-key {#storage-re-key}

```orna
Contact.rekey("alice-smith", "alice-jones")
```

1. Resolve the complete old and new keys under the activation snapshot.
2. Require old to exist and new not to exist.
3. Require an explicitly keyed table; implicit automatic integer IDs cannot be re-keyed.
4. Record one re-key intent in the activation transaction.
5. Allow the same activation to update every dependent stored reference.
6. At commit, validate all uniqueness, key encoding and referential constraints against the final state.
7. Commit all row/reference changes or none of them.
8. Project an editable row to the new path recoverably, or write compact deletion+insertion mutations as needed.
9. Report one semantic `rekey` change with old/new keys.

Default reference behavior is `restrict`. There is no automatic cascade in v1. A raw file rename without an Orna re-key intent is delete+insert.



# 24. Git history, remotes and partial clones {#git-history}

## 24.1 Complete logical commits {#git-history-complete-logical-commits}

<span id="ORNA-GIT-001"></span>**ORNA-GIT-001** A commit MUST logically name the complete database snapshot, including all segment objects it references.

<span id="ORNA-GIT-002"></span>**ORNA-GIT-002** A partial clone MAY omit local copies of promised blobs while preserving their object IDs and reachability.

<span id="ORNA-GIT-003"></span>**ORNA-GIT-003** Laziness is a local materialization property, not permission to create incomplete logical commits.

## 24.2 Partial clone {#git-history-partial-clone}

Recommended clone behavior for large repositories:

```bash
git clone --filter=blob:none --no-checkout <remote>
```

followed by selective checkout/materialization.

<span id="ORNA-GIT-004"></span>**ORNA-GIT-004** A partial-clone-aware implementation MUST distinguish sparse checkout (working-tree paths) from partial clone (locally present Git objects).

<span id="ORNA-GIT-005"></span>**ORNA-GIT-005** Query planning SHOULD use committed manifests to identify required segment blobs before fetching them.

<span id="ORNA-GIT-006"></span>**ORNA-GIT-006** Missing promised objects MAY be fetched lazily from configured promisor remotes.

## 24.3 Large Object Promisors {#git-history-large-object-promisors}

Git's Large Object Promisor work is relevant but not assumed universally available.

<span id="ORNA-GIT-007"></span>**ORNA-GIT-007** Orna MUST NOT require github.com or any specific host to support an external large-object promisor.

<span id="ORNA-GIT-008"></span>**ORNA-GIT-008** A self-hosted implementation MAY place large Git blobs on a separate promisor remote while the main remote serves commits, trees and normal blobs.

## 24.4 Remotes {#git-history-remotes}

Normal Git semantics apply:

```bash
orna remote add origin server:~/git/example.git
orna push -u origin main
```

<span id="ORNA-REMOTE-001"></span>**ORNA-REMOTE-001** Moving a repository to another host MUST use ordinary remote, push and set-url operations rather than requiring commit conversion.

<span id="ORNA-REMOTE-002"></span>**ORNA-REMOTE-002** Orna MUST NOT invent a `remote migrate` command for behavior already expressible by Git.

<span id="ORNA-REMOTE-003"></span>**ORNA-REMOTE-003** Orna-aware `clone`, `fetch`, `pull` and `push` MUST synchronize required internal refs under `refs/orna/*` in addition to ordinary requested branch/tag refs.

<span id="ORNA-REMOTE-004"></span>**ORNA-REMOTE-004** Before a branch snapshot whose allocator watermark depends on an internal allocator ref becomes visible on a remote, that remote's allocator ref MUST be advanced to at least that watermark. An implementation MAY use an atomic multi-ref update; otherwise it advances the allocator ref first. A failed later branch update may create gaps but MUST NOT permit ID reuse.

<span id="ORNA-REMOTE-005"></span>**ORNA-REMOTE-005** A repository transferred with plain Git remains readable. If required `refs/orna/*` continuity is absent or stale, Orna MUST diagnose the condition before allocating IDs or claiming checkpoint/allocator continuity; it MUST NOT silently guess.

## 24.5 No rolling-window history {#git-history-no-rolling-window-history}

<span id="ORNA-GIT-009"></span>**ORNA-GIT-009** Orna MUST NOT silently delete old table rows from HEAD merely because older commits retain them.

<span id="ORNA-GIT-010"></span>**ORNA-GIT-010** Retention or history rewriting MUST be explicit and destructive behavior MUST be clearly diagnosed.



# 25. Schema evolution, semantic diff and merge {#evolution}

## 25.1 No migration files {#evolution-no-migration-files}

<span id="ORNA-MERGE-001"></span>**ORNA-MERGE-001** The semantic difference between two valid snapshots is the migration between them.

<span id="ORNA-MERGE-002"></span>**ORNA-MERGE-002** Orna MUST NOT require an independent SQL-style migration file merely to express a declarative schema change.

## 25.2 Rename {#evolution-rename}

<span id="ORNA-SCHEMA-001"></span>**ORNA-SCHEMA-001** Semantic rename uses stable ObjectIds and `orna mv`; old snapshots retain old names mapped to the same identity.

<span id="ORNA-SCHEMA-002"></span>**ORNA-SCHEMA-002** A plain rename without identity continuity is delete plus create.

## 25.3 Adding and changing fields {#evolution-adding-and-changing-fields}

Adding an optional field is metadata-compatible:

```text
email: Str?
```

Older rows read `null`.

A stored default is insertion behavior:

```text
country: Str = "GB"
```

New rows that omit `country` receive and logically store the value produced once at insertion.

For rows that predate the field, a closed constant default may be committed as a frozen field-introduction fallback. The fallback value is pinned to that schema revision. Later changing the declaration to `"US"` affects future inserts only; it does not change historical rows that previously resolved to `"GB"`.

A row-dependent default may be used for future insertions, but existing rows require an explicit backfill. Orna never turns a stored field into an undocumented compute-on-read field.

A computed field is declared separately:

```text
full_name: Str => "{first} {last}"
```

It is a read-only row-local selector, not stored data. It automatically changes when `first` or `last` changes and may not be supplied or updated directly.

<span id="ORNA-SCHEMA-003"></span>**ORNA-SCHEMA-003** Optional additions MUST NOT require rewriting every existing row.

<span id="ORNA-SCHEMA-004"></span>**ORNA-SCHEMA-004** An insert-time default is evaluated once for future rows. A frozen introduction fallback for older rows MUST be stored in committed schema metadata and MUST NOT change when the source default expression is later edited.

<span id="ORNA-SCHEMA-005"></span>**ORNA-SCHEMA-005** Only a closed deterministic constant may become an implicit fallback for rows that predate a field. Row-dependent or effectful defaults require explicit backfill for existing rows.

<span id="ORNA-SCHEMA-006"></span>**ORNA-SCHEMA-006** A required stored field without a complete default/backfill is invalid while existing rows lack a value.

<span id="ORNA-SCHEMA-007"></span>**ORNA-SCHEMA-007** A computed field is not a migration/backfill mechanism and MUST follow the computed-field rules in [the tables chapter](#tables).

A backfill is ordinary Orna code, not a migration language. Large backfills may use checkpointed batches.

## 25.4 References {#evolution-references}

<span id="ORNA-SCHEMA-008"></span>**ORNA-SCHEMA-008** Non-optional stored references default to `restrict` on delete/re-key. Automatic cascade is outside version 1.0.

The program may update dependants and delete the target within one ordinary activation transaction.

## 25.5 Three-way merge algorithm MERGE-1 {#evolution-three-way-merge-algorithm-merge-1}

1. Resolve merge base, ours and theirs through Git.
2. Parse and resolve each module graph and stable ObjectIds.
3. Perform source-aware and definition-aware three-way merges.
4. Derive the candidate merged schema.
5. For every table, compare editable-tree or compact-manifest/content digests before reading rows:
   - if ours equals theirs, reuse it;
   - if ours equals base, take theirs;
   - if theirs equals base, take ours;
   - otherwise descend only into changed and overlapping key ranges.
6. Merge changed editable rows and compact logical mutations by primary key.
7. Validate candidate rows against candidate schema, references and constraints.
8. Rebuild dependency impact for affected definitions.
9. Type-check affected functions, pages and stream programs.
10. Produce a valid isolated merge result or typed conflicts. Do not partially mutate ordinary CWD.
11. Move the isolated result into CWD only after it is complete and within configured resource/conflict budgets.

<span id="ORNA-MERGE-003"></span>**ORNA-MERGE-003** An implementation MUST use manifest, tree or segment digest equality to skip identical subtrees before decompressing or comparing their rows.

<span id="ORNA-MERGE-004"></span>**ORNA-MERGE-004** Merge work SHOULD be proportional to changed or overlapping key ranges rather than total table size where the storage profile exposes suitable digests and bounds. A genuinely table-wide change may still require table-wide work.

<span id="ORNA-MERGE-005"></span>**ORNA-MERGE-005** A merge MUST have explicit resource and conflict-count budgets. If a budget is exceeded, the operation stops with CWD unchanged and reports the affected tables/ranges plus a lower bound on discovered conflicts. It MUST NOT exhaust memory or create millions of materialized conflict objects silently.

<span id="ORNA-MERGE-006"></span>**ORNA-MERGE-006** Distinct compatible optional columns SHOULD merge automatically.

<span id="ORNA-MERGE-007"></span>**ORNA-MERGE-007** Incompatible edits to the same column type MUST produce a schema conflict.

<span id="ORNA-MERGE-008"></span>**ORNA-MERGE-008** Independent edits to different fields of one keyed row SHOULD merge automatically.

<span id="ORNA-MERGE-009"></span>**ORNA-MERGE-009** Different edits to the same field MUST produce a row conflict unless an explicit type-specific merge rule exists.

<span id="ORNA-MERGE-010"></span>**ORNA-MERGE-010** Generated-key and automatic-ID collisions are normal row conflicts and MUST NOT be silently renamed.

<span id="ORNA-MERGE-011"></span>**ORNA-MERGE-011** Divergent opaque checkpoints MUST produce `sys.CheckpointConflict` rather than a guessed merge.

## 25.6 Semantic diff {#evolution-semantic-diff}

```text
$ orna diff main..feature

directory.Contact
  + column phone: Str?
  ~ 2 rows

energy.Tariff
  + 2026-09

energy.daily()
  affected by energy.Tariff

/energy
  affected through energy.daily()
```

<span id="ORNA-DIFF-001"></span>**ORNA-DIFF-001** Semantic diff SHOULD report schema, keyed row, result and dependency changes where available.

<span id="ORNA-DIFF-002"></span>**ORNA-DIFF-002** Raw Git diff MUST remain available.

<span id="ORNA-DIFF-003"></span>**ORNA-DIFF-003** Pure physical representation changes MUST be distinguishable from logical data changes.



# 26. Trust, isolation and extensions {#security}

Orna operates within a trusted environment. Host machines, OS accounts and repository access controls define the trust boundary.

<span id="ORNA-TRUST-001"></span>**ORNA-TRUST-001** Local commands trust the invoking OS user. Orna v1 does not define principals, groups, grants, row ACLs, device roles or an enterprise policy language.

<span id="ORNA-TRUST-002"></span>**ORNA-TRUST-002** `orna serve` binds to loopback by default. Remote deployment SHOULD use normal SSH, Tailscale, or a trusted authenticated reverse proxy/TLS boundary.

<span id="ORNA-TRUST-003"></span>**ORNA-TRUST-003** Public anonymous mutation or arbitrary remote REPL execution is outside the trusted v1 profile.

## 26.1 Extensions {#security-extensions}

Portable extensions use the WebAssembly Component Model/WIT where practical; system integrations may use a versioned out-of-process adapter; native extensions are explicitly trusted implementation-specific code.

<span id="ORNA-EXT-001"></span>**ORNA-EXT-001** Orna MUST NOT require a separate handwritten permissions/package manifest merely to restate a component's WIT imports and exports.

<span id="ORNA-EXT-002"></span>**ORNA-EXT-002** WIT imports/exports and the pinned Git object identify the portable interface and code. Installing/running the extension is the trust decision.

<span id="ORNA-EXT-003"></span>**ORNA-EXT-003** A Wasm extension MUST NOT receive raw pointers/handles to Turso or Git internals. Host calls use typed interfaces so a trap cannot corrupt repository/runtime memory.

<span id="ORNA-EXT-004"></span>**ORNA-EXT-004** Implementations SHOULD enforce coarse execution budgets and cancellation for portable components. These controls bound execution; they do not grant per-device permissions.

<span id="ORNA-EXT-005"></span>**ORNA-EXT-005** Native extensions run with the host process's trust and MUST be labeled as such. Orna does not sandbox native extensions.

## 26.2 SOPS and repository privacy {#security-sops-and-repository-privacy}

SOPS-encrypted files may be committed normally. Decryption identities remain outside Git. Secret values are opaque/redacted by default across Inspect, Display, Present, codecs, diagnostics and traces.

Normal row deletion is versioned deletion, not erasure from Git history, remotes or backups. Explicit history rewrite/prune remains a separate destructive workflow.

## 26.3 Public multi-user deployments {#security-public-multi-user-deployments}

Multi-user authentication and authorisation are outside this profile. Trusted deployments do not require a separate multi-user authorisation service.



# 27. Secrets and encrypted files {#secrets}

## 27.1 Principles {#secrets-principles}

Credentials cannot be plaintext Git data, but encrypted secret documents may be versioned.

<span id="ORNA-SECRET-001"></span>**ORNA-SECRET-001** Orna MUST NOT commit plaintext secret values by default.

<span id="ORNA-SECRET-002"></span>**ORNA-SECRET-002** Secret values MUST be redacted from Inspect, Display, Present, diagnostics, traces and `sys` unless an explicitly privileged operation requests disclosure.

## 27.2 SOPS profile {#secrets-sops-profile}

The recommended version 1.0 provider uses SOPS with `age`, PGP or a supported KMS.

```text
example/
├── .sops.yaml
└── secrets/
    └── personal.sops.yaml
```

The private `age` identity or KMS credential remains outside Git.

<span id="ORNA-SOPS-001"></span>**ORNA-SOPS-001** An SOPS provider MUST decrypt in memory and SHOULD avoid writing plaintext temporary files.

<span id="ORNA-SOPS-002"></span>**ORNA-SOPS-002** A clone without the decryption identity MUST remain able to inspect committed non-secret data and MUST report dependent runs as unavailable rather than corrupt.

## 27.3 Secret references {#secrets-secret-references}

```orna
let credential = std.secret.ref("messages.inbox");
google.mail(account: credential)
```

<span id="ORNA-SECRET-003"></span>**ORNA-SECRET-003** A `SecretRef` may be displayed and serialized by stable name; the resolved secret value may not.

<span id="ORNA-SECRET-004"></span>**ORNA-SECRET-004** `sys.Secret` MUST expose metadata such as name, provider and availability but MUST NOT expose secret contents.



# 28. Serving a database {#serving}

## 28.1 Optional network host {#serving-optional-network-host}

`orna serve` provides network access to the current clone. Local commands use the [embedded runtime](#embedded) directly and do not require the server.

<span id="ORNA-SERVE-001"></span>**ORNA-SERVE-001** `orna serve` provides Git transport, page/query endpoints and WebSocket presentation deltas for its clone.

<span id="ORNA-SERVE-002"></span>**ORNA-SERVE-002** `orna serve` MUST NOT automatically invoke root `main()` or unrelated stream programs.

<span id="ORNA-SERVE-003"></span>**ORNA-SERVE-003** A different clone running `orna serve` serves that clone's HEAD and CWD; CWD MUST remain local to that clone.

<span id="ORNA-SERVE-004"></span>**ORNA-SERVE-004** The default listener MUST bind only to loopback unless the operator explicitly selects another address.

## 28.2 Deployment trust {#serving-trusted-personal-deployment-model}

Host machines and local OS accounts are trusted. The [trust model](#security) defines the isolation boundary.

<span id="ORNA-SERVE-005"></span>**ORNA-SERVE-005** Core Orna does not define principals, groups, grants, per-device roles or row-level permissions.

Remote exposure is expected to use existing boundaries such as Tailscale, SSH, host firewall rules or an authenticated reverse proxy. Git-over-SSH uses ordinary SSH keys. HTTPS deployments inherit authentication from the configured trusted proxy where desired.

<span id="ORNA-SERVE-006"></span>**ORNA-SERVE-006** Binding beyond loopback MUST be explicit and MUST produce a clear status indication of the exposed interfaces.

Multi-user authorisation is outside this profile; local trusted operation MUST NOT depend on it.

## 28.3 Git transport and browser frontend {#serving-git-transport-and-browser-frontend}

<span id="ORNA-SERVE-007"></span>**ORNA-SERVE-007** Git transport MUST remain usable when the optional Git/database web frontend is absent.

<span id="ORNA-SERVE-008"></span>**ORNA-SERVE-008** The default frontend SHOULD be an ordinary Orna application, preferably `std.devtools`.

<span id="ORNA-SERVE-009"></span>**ORNA-SERVE-009** When installed as `/`, the default frontend SHOULD lead with database tables and also expose files, commits, branches, functions, dependencies, storage and runtime state.

## 28.4 Applications and renderers {#serving-applications-and-renderers}

An Orna application is a reachable collection of functions, pages, tables and assets. No `CREATE APPLICATION` grammar or application manifest is required merely to group code.

Presentation trees are renderer-neutral. Terminal, web and future native clients render the same value tree and fall back to Inspect-compatible nodes when needed.



# 29. Canonical values and schema descriptors {#formats}

This section defines the canonical binary representation used by compact-value fallbacks, semantic digests, request fingerprints and portable wire values. It does not replace the human-editable Orna text codec. Presentation strings, locale formatting and physical Parquet compression are not inputs to canonical logical identity.

## 29.1 Binary value profile OVB-1 {#formats-binary-value-profile-ovb-1}

OVB-1 is an application profile of [CBOR](#ref-cbor). Every length and integer argument uses the shortest permitted head. Arrays, byte strings, text strings and maps have definite length. Maps have no duplicate keys and are ordered by unsigned lexicographic comparison of each key's complete deterministic encoding. Text must be well-formed UTF-8; ordinary string data is not normalised. Indefinite items, CBOR undefined, unsupported simple values and unregistered tags are errors.

Orna Float always uses a CBOR binary64 item, even when a shorter float could express the same number. This is an explicit application-level deterministic rule, not a claim to use CBOR's shortest-float representation. Negative zero retains its sign. All encoded NaNs use bits `0x7ff8000000000000`; infinities remain signed infinities.

Integers in the CBOR major-type 0/1 ranges use those forms. Larger positive integers use tag 2 with minimal unsigned big-endian magnitude bytes; larger negative integers use tag 3 with the minimal magnitude of `−1−n`. A bignum with a leading zero or a value representable directly in major type 0/1 is noncanonical. Zero is the single byte `00`, not an empty bignum. This rule completely defines arbitrary-size integer bytes without host endianness or signed-big-integer conventions.

<span id="ORNA-FORMAT-001"></span>**ORNA-FORMAT-001** Canonical encoders MUST produce the unique OVB-1 representation for supported values. Strict decoders MUST reject noncanonical aliases before using bytes as an identity or fingerprint.

### 29.1.1 Type-preserving values {#formats-type-preserving-values}

An untagged CBOR Bool, Int, Str or Blob represents that corresponding Orna primitive. Arrays represent lists. Bare CBOR null is reserved for absent metadata fields, not the complete representation of a nested Orna Option. All other portable values use the following registry. Integer codes are exact and not extensible without a negotiated profile version.

| Tag | Value | Payload and constraints |
|---:|---|---|
| 37 | UUID / ObjectId | Exactly 16 network-order bytes. The enclosing type descriptor distinguishes opaque identifier types. |
| 60000 | Decimal | `[coefficient, exponent10]`. Both are Int. Zero is `[0,0]`; nonzero coefficient has no trailing factor of 10. |
| 60001 | Date | Canonical `YYYY-MM-DD` text, validated Gregorian date. |
| 60002 | Instant | `[unix_seconds, nanosecond]`; floor seconds, nanosecond 0…999999999. No leap-second alias. |
| 60003 | LocalDateTime | Canonical local date/time text with exactly nine fractional digits and no offset. |
| 60004 | TimeZone | IANA identifier text; resolution uses the enclosing snapshot's recorded time-zone dataset. |
| 60005 | Duration | `[seconds, nanosecond]` with floor-normalised seconds and nonnegative nanosecond remainder. |
| 60006 | Quantity | `[number, unit_object_id]`; the type descriptor identifies dimension, scale and offset. |
| 60007 | Money | `[Decimal, currency_object_id]`; the Decimal is tag 60000, never an intermediate Float. |
| 60008 | Enum | `[type_object_id, variant_object_id, payload_record_or_null]`; variant and payload schema must agree. |
| 60009 | Record / nominal record / logical row | `[type_object_id_or_null, fields]`, where fields is an array of `[field_id_or_name, value]` in deterministic field-key order. A full row includes key fields. |
| 60010 | Pinned row reference | `[database_id, table_id, key, snapshot]`; snapshot uses the exact snapshot representation below. |
| 60011 | Diagnostic | The diagnostic map defined by the live protocol; safe fields only. |
| 60012 | Present node | `[kind, stable_key_or_null, properties, children]` as specified by the live protocol. |
| 60013 | Option | `[0]` for `null`, `[1, value]` for `Some(value)`. Nested Options remain distinct. |
| 60014 | Unit | Empty array `[]`; no other payload. |
| 60015 | Tuple | Ordered array of component values. Zero arity canonicalises to Unit/tag 60014. |
| 60016 | Error | Map `{0: code, 1: message, 2: causes, 3: safe_details}`; causes are ordered Error values, details are a text-keyed map. Cycles fail. |
| 60017 | TimeOfDay | Integer nanoseconds since local midnight in 0…86399999999999. |
| 60018 | ZonedDateTime | `[Instant, TimeZone, offset_seconds]`; offset must agree with the pinned zone dataset at that instant. |
| 60019 | Range | `[lower: Option, upper: Option, upper_inclusive: Bool]`; endpoint types must agree. Lower endpoint, when present, is inclusive. |
| 60020 | Refined nominal value | `[type_object_id, base_value]`; decoding validates the referenced refinement before returning the nominal value. |
| 60021 | Stored table reference | `[database_id, table_id, key]`; resolution uses the containing row's snapshot. Export as an independently pinned value uses tag 60010 instead. |
| 60022 | Live-handle description | `[original_type_id, runtime_id, natural_key, false]`; never reconstructs an operational handle. Typed decoding as an operational handle fails. |

Structural field keys are NFC field names. Nominal and row field keys are stable field ObjectIds. Encoders sort by the encoded field key, not by presentation order. Equal field keys are invalid. A record schema must be known from its type ID or from the explicit `as: T` decoding witness. Private field encoding is governed by the type's codec implementation and cannot be requested by circumventing visibility through a structural record.

A `sys.Value` carries both its exact originating type descriptor and the OVB-1 value. A tag or UUID alone does not permit a value of one opaque identifier type to masquerade as another. Secret plaintext, source closures, open streams, database transaction handles and other operational resources are not ordinary portable values. A live-handle description is data about a resource, not permission to revive it.

### 29.1.2 Snapshot encoding {#formats-snapshot-encoding}

A snapshot is exactly one of:

```text
[0, database_uuid, runtime_uuid, generation_uint, snapshot_id_bytes]
[1, database_uuid, git_hash_algorithm, commit_oid_bytes]
```

For a CWD pin, `snapshot_id_bytes` is the 32-byte SHA-256 digest of ASCII `orna.snapshot.v1`, a zero byte and OVB-1 structural encoding of `[0, database_uuid, runtime_uuid, generation_uint]`. The digest does not include itself. A runtime generation must never identify two different logical states. `runtime_uuid` fences owner restarts. The mapping to the exact logical generation is retained for the pin's documented lifetime. For committed snapshots, `git_hash_algorithm` is `"sha1"` with a 20-byte object ID or `"sha256"` with a 32-byte object ID. An attached database uses its own database UUID in the same committed form, not a different ambiguous shorthand.

<span id="ORNA-FORMAT-002"></span>**ORNA-FORMAT-002** A CWD reference MUST identify its captured generation. A context-free `[0]` is invalid. A decoder unable to resolve a pin MUST report `sys.snapshot.expired` or `sys.snapshot.incomplete`; it MUST NOT rebind the reference to current CWD.

<span id="ORNA-FORMAT-003"></span>**ORNA-FORMAT-003** A referenced database, type, table and key schema MUST agree. Decoding data from another snapshot does not implicitly import its schema or replace the current execution context.

## 29.2 Domain-separated digests {#formats-domain-separated-digests}

`SHA256(domain || 00 || payload)` means the ASCII domain bytes, one zero byte and the exact OVB-1 payload. Domains have no terminating zero of their own. The following strings are fixed:

| Identity | Domain | Payload |
|---|---|---|
| Stored logical row | `orna.row.v1` | `[database_id, table_id, canonical_key, canonical_stored_fields]`; computed fields excluded. |
| Storage schema | `orna.schema.v1` | Exact schema descriptor below. |
| Durable consumer | `orna.consumer.v1` | `[database_id, function_object_id, canonical_typed_bound_arguments]`; code revision excluded. |
| Argument identity | `orna.arguments.v1` | Arguments sorted by NFC name; each includes its exact type descriptor and value. |
| Request fingerprint | `orna.request.v1` | Operation kind plus the canonical body fields specified by the protocol; no request ID or credential. |
| Checkout plan | `orna.checkout.v1` | Exact typed plan excluding its token. |
| Preserved payload | `orna.payload.v1` | `[media_type, bytes]`; secret payload hashes remain protected metadata. |

Default arguments are bound once before a consumer or invocation identity is computed. A changing default such as a clock-derived configuration gives a different canonical argument identity; it is not silently omitted. Durable consumers should use explicit stable configuration. Function code revisions are excluded from consumer identity but included in reflective invocation idempotency identity.

Digests do not replace equality where a collision could change semantics. Natural-key lookup verifies exact typed key equality. A mismatch between declared content digest and bytes is corruption, not a choice of another decoding.

## 29.3 Closed storage schema descriptor {#formats-closed-storage-schema-descriptor}

The descriptor is an OVB-1 map with exactly these integer keys:

| Key | Type | Meaning |
|---:|---|---|
| 0 | UInt, value 1 | Descriptor version. |
| 1 | UUID | Table ObjectId. |
| 2 | Array of UUID | Complete primary-key field IDs, in key order. |
| 3 | Array of field descriptors | Fields ordered by unsigned field-ID bytes. |
| 4 | Array of nominal definitions | Definitions ordered by unsigned type-ID bytes. |

A field descriptor is `[field_id, name, type_node, role, fallback]`. `role` is 0 for key, 1 for stored field or 2 for computed field. `fallback` is `[0]` for no introduction fallback, `[1, value]` for the frozen field-introduction fallback or `[2, expression_digest]` for a computed selector. An insert-time default is code in the schema revision, not a mutable read-time fallback; it is not substituted for `[1,value]`. Optional physically missing fields obtain `null` under the schema-evolution rules even when fallback is `[0]`.

A nominal definition is `[type_id, kind, body]`. Kind 0 is a record with its field-descriptor array; 1 is an enum with variants `[variant_id, name, payload_fields]`; 2 is a refined value with `[base_type_node, assertion_semantic_digests]`; 3 is a unit with `[dimension_vector, scale_numerator, scale_denominator, offset_numerator, offset_denominator, affine]`; 4 is a currency with `[code, minor_digits]`. Vectors and arrays have the orders stated here; duplicate IDs are invalid.

Type nodes are closed positional arrays:

```text
[0, primitive_name]              primitive from the list below
[1, element_type]                List
[2, element_type]                Option
[3, [component_types...]]        Tuple (zero components is Unit)
[4, [[name, type]...]]            Structural record, names sorted
[5, nominal_type_id]             Definition in key 4
[6, database_id, table_id,
    [key_component_types...]]    Stored table reference
[7, numeric_type, unit_id]       Quantity
[8, currency_type_id]            Money
[9, endpoint_type]               Range
```

The primitive names are `Bool`, `Int`, `Float`, `Decimal`, `Str`, `Blob`, `Uuid`, `Date`, `Instant`, `LocalDateTime`, `TimeOfDay`, `Duration`, `TimeZone`, `ZonedDateTime` and `Unit`. A type outside this serialisable subset requires a versioned explicit codec representation or is rejected for stored fields. Transparent aliases lower to their target. Nominal references resolve through the descriptor graph; stored references can be cyclic across tables without embedding recursive row contents. A recursively embedded value type with no finite constructible representation is invalid.

Unknown keys, type codes, roles or definition kinds are rejected. A canonical descriptor is complete: every referenced nominal definition is present, every key field appears once, and a computed field is never a primary-key field. Its schema fingerprint is the domain-separated digest above. File-specific physical encodings are not inputs to this logical schema fingerprint.

## 29.4 Canonical Orna text {#formats-canonical-orna-text}

The canonical text codec is schema-directed: `decode(input, as: T)` supplies the exact type and snapshot. It accepts only data constructors in the supplied type, never arbitrary function execution, imports or declarations. Row files are the same data subset with key and computed fields omitted. Loading a row does not execute user code other than deterministic declared construction/refinement validation.

Canonical output is UTF-8 without a byte-order mark, uses LF, two-space indentation for nonempty records, one space after `:`, comma-separated array/tuple elements, a trailing comma after each record field, and one final LF. Record field order is primary-key order followed by stored fields in stable field-ID order for table rows; structural records use NFC name order. Arrays and tuples retain value order. No comments or interpolation occur in canonical output.

Int uses minimal decimal digits with no `+`, leading zeros or separators. Decimal uses its normalised coefficient followed by `e`, a minimal signed decimal exponent (no `+`) and `.decimal`; zero is `0e0.decimal`. Finite Float uses exactly 17 significant decimal digits in scientific form with lowercase `e`, a minimal exponent and suffix `f`; signed zero is retained. Nonfinite Float uses `Float.nan`, `Float.infinity` or `-Float.infinity`. Strings use JSON-style quote/backslash/newline/carriage-return/tab escapes where shared by Orna; other control scalars and a literal opening brace use minimal lowercase `\u{...}`. Ordinary non-control Unicode scalars remain UTF-8.

Bool uses `true`/`false`, Option uses `null` or `Some(value)`, Unit uses `()`, and tuples use the comma distinction from the grammar. Uuid and opaque UUID-backed IDs use quoted lowercase canonical UUID text under their explicit type witness. Blob uses quoted padded standard Base64. Date/Instant use the validated literal forms; other time values use their OVB component structure rendered as data tuples under the type witness. Enums use the variant name resolved in the supplied type and an explicit record payload when needed. Nominal records use their declared field record; refined values use the base representation and are revalidated. Money and quantities use the exact numeric representation under their supplied type witness; no display symbol or locale is encoded.

Stored references use the target's schema-directed key representation under their declared database/table type. Independently pinned references include the snapshot descriptor and therefore cannot be confused with relative stored references. Generic standalone row encoding includes primary keys; a row-body encoding must be requested through the repository row writer. Unsupported values fail with a decode/encode diagnostic rather than losing type information or private data.

<span id="ORNA-FORMAT-004"></span>**ORNA-FORMAT-004** Canonical text decoding MUST be type-directed and non-executable. It MUST distinguish optional absence, scalar values, row references and nominal construction according to the supplied schema, and validate all refinements before returning a value.


## 29.5 Portable opaque and contextual values {#formats-portable-opaque-and-contextual-values}

The following encodings complete the system-value boundary. UUIDs occurring in the structural slots of a snapshot, schema or row-reference encoding use the representation explicitly assigned to that slot. Encoding an opaque ID as an independent typed value additionally preserves its nominal kind.

| Tag | Value | Exact representation |
|---|---|---|
| 60023 | Portable opaque system identifier | `[qualified_type_name, representation]`, using the closed table below. |
| 60024 | Path | `[flavour, text]`; flavour is `repo`, `posix` or `windows`. Repository paths are safe relative slash-separated paths; other flavours are descriptive host paths subject to redaction, not executable access grants. |
| 60025 | Digest | `["sha256", bytes32]`. Display is lowercase 64-hex; no implicit string conversion. |
| 60026 | Explicit `sys.Value` box | `[closed_type_descriptor, value]`. The descriptor identifies the exact pinned originating type; the value must validate against it. |
| 60027 | Materialised finite Relation | `[ordered: Bool, rows]`. Rows are an array of complete typed values. An unordered relation is sorted by canonical encoded value for canonical encoding, preserving duplicate multiplicity; this does not add an observable source query order. |

The UUID-backed system identifier names are `sys.DatabaseId`, `sys.FileId`, `sys.DefinitionId`, `sys.ObjectId`, `sys.RuntimeId`, `sys.TransactionId`, `sys.InvocationId`, `sys.RunId`, `sys.QueryId`, `sys.SessionId`, `sys.ClientId`, `sys.TraceId`, `sys.SpanId`, `sys.SegmentId` and `sys.BuildId`. Their representation is a tag-37 UUID. `sys.RevisionId`, `sys.SnapshotId` and `sys.ConsumerIdentity` use 32 digest bytes. `sys.CheckpointVersion` and `sys.FailureVersion` use nonnegative arbitrary-precision integers that increase on transitions. `sys.GitOid` uses `["sha1", bytes20]` or `["sha256", bytes32]` and cannot be confused with a semantic revision digest.

`sys.RevisionId` is the semantic revision's SHA-256 identity. A `sys.SnapshotId` is the domain-separated digest using `orna.snapshot.v1`, zero byte and the OVB structural bytes of the committed descriptor, or the first four fields of a CWD descriptor. The CWD descriptor’s fifth field carries that result and is excluded from its own input. The snapshot descriptor—not that digest alone—retains repository/hash-algorithm and local-generation context. Consumer identity uses the consumer hash tuple specified earlier. Version integers never wrap or reset when progress is reset; a restored snapshot still receives a fresh local version when installed as current state.

Path and Digest are intrinsic nominal types used by the system and repository APIs. A path codec does not normalise an arbitrary host path into another filesystem's semantics. A repository-path operation validates the `repo` flavour before use. A typed `sys.Value` cannot make a protected path or secret revealable. Function closures, unbounded streams and executable process/session handles are not serialisable operational values.

A finite relation may be encoded only after its complete bounded observation succeeds. Exceeding the configured resource bound fails; the codec must not emit a truncated successful relation. A stored table field may not use a live Relation in place of a finite stored list; materialisation and the field's declared type are explicit.


# 30. Live protocol and session recovery {#protocol}

## 30.1 Transport and value profile {#protocol-transport-and-value-profile}

The live protocol uses the WebSocket subprotocol `orna.present.v1` over RFC 6455. One binary WebSocket message contains one complete OVB-1 CBOR envelope. Fragmented WebSocket frames are reassembled before decoding. Text application messages are rejected; ping, pong and close retain their WebSocket meanings. TLS is required outside an explicitly trusted loopback transport.

The [canonical value profile](#formats) defines integer, float, decimal, option, nominal-value, reference and snapshot bytes. Protocol structure uses the untagged CBOR maps/arrays specified here; application values use their OVB-1 typed representations. A protocol decoder must not confuse structural null fields with a typed optional result.

<span id="ORNA-PROTO-001"></span>**ORNA-PROTO-001** An implementation MUST decode and validate the complete message, required fields, canonical forms and configured limits before admitting its operation. It MUST reject duplicate map keys and noncanonical integer/length forms; it MUST NOT execute a partially decoded request.

## 30.2 Session creation and ownership {#protocol-session-creation-and-ownership}

A trusted client creates a session with `POST /orna/session`, content type `application/json`, using UTF-8 JSON without duplicate members. The request is exactly `{ "database": "<database-uuid>", "protocol": "orna.present.v1" }`. The database must already be exposed by the host; this request does not attach arbitrary filesystem paths. The endpoint checks its authentication/perimeter policy and Origin before creating execution state.

A successful response is HTTP 201 with `session`, `database` and `runtime` UUID strings, `resume_token` (unpadded Base64url of 32 unpredictable bytes), `websocket_path` (`/orna/live/<session-uuid>`), `lease_ms`, and `limits`. The response sets an HttpOnly, SameSite=Strict session cookie scoped to that WebSocket path; Secure is required on TLS. The cookie authenticates the WebSocket upgrade. The session UUID alone is not a capability. Native clients may retain and send the same cookie through their HTTP/WebSocket stack.

A client resumes with `POST /orna/session/<session-uuid>/resume` and exactly `{ "resume_token": "...", "protocol": "orna.present.v1" }`. A live, unexpired session returns HTTP 200 with the same session identity, current runtime identity, a rotated token/cookie and limits. Only one active WebSocket is attached to a session; a successful resume replaces the old attachment, not its owned tasks. A failed or expired resume returns HTTP 410 and never fabricates a continuation of the old runtime.

`DELETE /orna/session/<session-uuid>` with `Authorization: Bearer <resume_token>` explicitly ends the session. The token must be the currently retained, unexpired token for that exact session. The WebSocket-path cookie alone is not sent to this HTTP endpoint and is not its authentication mechanism. Deletion also checks Origin under the same trusted-client policy. It stops new requests, cancels session-owned work, joins cleanup, and returns HTTP 204 after orderly termination. Loss of the WebSocket starts the advertised finite reconnection lease. No new client request is admitted on a lost connection. Expiry terminates the owner and cancels its children. The default lease is 30,000 ms; a host may advertise a different positive bounded value no greater than 300,000 ms. Explicit close or local REPL death does not wait for this network grace period.

Creation/resume errors use JSON `{ "code": "...", "message": "..." }` and an appropriate status: 400 malformed request, 401/403 unauthenticated or prohibited origin, 404 unavailable database, 409 incompatible protocol/runtime, 410 expired session, 413 limits exceeded, 503 temporarily unavailable. No error includes plaintext credentials or host-private paths. Session HTTP JSON permits no unknown members in version 1; an extension requires explicit negotiation.

<span id="ORNA-PROTO-002"></span>**ORNA-PROTO-002** Session resumption MUST preserve request reservations and terminal outcomes for that session while its lease remains valid. A new runtime generation MUST invalidate operational handles from the old generation. Durable request recovery may report old outcomes as data without reviving old handles or owners.

## 30.3 Envelope and field rules {#protocol-envelope-and-field-rules}

Every envelope contains exactly these required structural fields:

| Key | Type | Meaning |
|---|---|---|
| 0 | UInt | Protocol major version, exactly 1. |
| 1 | UInt16 | Message type from the registry below. |
| 2 | ByteString(16) or null | Request ID. Required and nonnull on every client operation. Server notifications may use null as specified below. |
| 3 | ByteString(16) or null | Watch ID. Required only for messages bound to an existing watch; otherwise null. |
| 4 | Map<UInt16, value> | Type-specific body. |

All maps use definite lengths and canonical key order. Unknown message types fail. Unknown envelope keys fail. In a body, an unknown key 0…32767 is optional extension data and is ignored after bounded decoding; an unknown key 32768…65535 is a mandatory extension and causes rejection. Known version-1 fields below are required unless explicitly marked optional. No extension may reinterpret a known key. Extensions are included in fingerprint bytes even if an endpoint ignores their semantics.

The request ID is a client-chosen 128-bit identifier, compared as bytes, scoped by session identity. Watch and action/resource handles are server-chosen 128-bit identifiers scoped by session/runtime. An ID is not source text, a user-visible name or an authority grant. Reusing the same request ID for different input fails with `wire.request_mismatch`.

## 30.4 Complete message registry {#protocol-complete-message-registry}

| Type | Direction | Name | Envelope watch | Body fields |
|---|---|---|---|---|
| 0 | Client → host | subscribe | null | 0 resource handle bytes16; 1 PresentationContext. |
| 1 | Client → host | unsubscribe | existing ID | Empty map. Cancels and removes this watch; an already absent watch is an idempotent success. |
| 2 | Client → host | resync | existing ID | Empty map. Host replies with the latest full snapshot. |
| 3 | Client → host | event | existing ID | 0 page revision UInt; 1 action handle bytes16; 2 typed event value; 3 fingerprint bytes32. |
| 4 | Client → host | eval | null | 0 source Str; 1 DatabaseContext; 2 PresentationContext; 3 fingerprint bytes32. |
| 5 | Client → host | watch | null | 0 source Str; 1 DatabaseContext; 2 PresentationContext; optional 3 refresh floor as nonnegative Duration. |
| 6 | Client → host | cancel | null | 0 target kind (0 request, 1 watch); 1 target ID bytes16. The cancel request cannot target itself. |
| 7 | Client → host | request_status | null | 0 target request ID bytes16; 1 its expected fingerprint bytes32. Never executes the target request. |
| 16 | Host → client | snapshot | existing/new ID | 0 revision UInt; 1 complete PresentNode; 2 exact pinned Snapshot. |
| 17 | Host → client | delta | existing ID | 0 base revision UInt; 1 new revision UInt; 2 ordered operations; 3 exact pinned Snapshot. Request ID is null. |
| 18 | Host → client | result | null | 0 status (0 success, 1 ordinary failure, 2 cancellation, 3 terminal outcome retained without rich value); 1 typed value or structural null; 2 fingerprint bytes32; 3 Diagnostic or null. |
| 19 | Host → client | diagnostic | existing ID or null | 0 Diagnostic; optional 1 recoverable Bool. Request ID identifies a rejected request, or null for a watch/connection diagnostic. |
| 20 | Host → client | request_status_result | null | 0 target ID bytes16; 1 state (0 unknown, 1 reserved, 2 running, 3 terminal, 4 orphaned/uncertain); 2 fingerprint bytes32 or null; 3 retained result-body map or null. Request ID identifies the status request. |

An initial subscribe/watch or explicit resync receives a snapshot with its originating request ID. Subsequent automatic snapshots use a null request ID. Snapshot revisions start at 0 and increase; every delta has `new > base`. Resetting an existing watch revision to 0 without issuing a new watch ID is forbidden. A result for success carries the complete typed value, including Unit or a tagged Option; failure/cancellation carry structural null in field 1. A cancellation diagnostic describes termination but is not an ordinary catchable failure inside the target.

A completed control operation returns typed Unit in a success result. Cancelling another request acknowledges the cancellation request separately; the target eventually has its own terminal result. A terminal/unknown target cannot be turned into a different request by cancellation. `unsubscribe` and watch cancellation remove resources idempotently.

### 30.4.1 Shared structures {#protocol-shared-structures}

**DatabaseContext** is map `{0: database_uuid, 1: snapshot_or_null}`. Null selects the current CWD at admission. A supplied snapshot is exact and permits only read-only evaluation unless it denotes the admitted writable CWD generation. The host pins the admitted context once; retrying an identical reserved request does not resolve its null selector again against a newer CWD.

**PresentationContext** is map `{0: locale, 1: timezone_or_null, 2: width_or_null, 3: theme, 4: supported_kinds}`. Locale is BCP 47 text accepted by the configured presentation package; timezone is an IANA identifier; width is a positive integer in character columns for a terminal or CSS pixels for a browser, with renderer mode declared by `theme` prefix `terminal/`, `web/` or `native/`. Theme is a presentation hint, not executable code. `supported_kinds` is an array of kind identifiers. Unsupported locale/theme falls back to the host's declared default and is reported in snapshot metadata; it does not alter stored values or canonical hashes.

**Diagnostic** is tag 60011 around map `{0: code, 1: severity, 2: message, 3: spans, 4: notes, 5: causes, 6: redacted}`. Code and message are text; severity is 0 note, 1 help, 2 warning, 3 error, 4 fatal; notes are safe strings; causes are nested diagnostics subject to depth limits; redacted is Bool. Each span is `[snapshot, file_path, start_byte, end_byte]`, with a repository-relative UTF-8 path or explicit redacted marker, and half-open byte offsets. A missing span is represented by an empty spans array, not a fabricated file. Optional key 7 carries a stable diagnostic UUID when retained.

These are transport structures, not user-declarable records whose exact field names must be guessed from JSON. Their corresponding typed system metadata preserves the same source and causal semantics.

## 30.5 Present nodes and patches {#protocol-present-nodes-and-patches}

A Present node is tag 60012 around `[kind, stable_key_or_null, properties, children]`. Kind is a stable text name or object UUID. Properties map stable text/field UUID keys to typed values. Children are an ordered array of Present nodes. Stable keys use one of `[0, field_name_or_id]`, `[1, table_uuid, complete_primary_key]` or `[3, explicit_typed_key]`. Unkeyed children have null keys and use position. Sibling stable keys must be unique.

A renderer may choose another visual layout, but it must preserve the logical content. An unknown kind is rendered as an inspectable structural node with its safe properties and children; it is not dropped. An action property is an opaque session-bound action handle with a declared input type, not JavaScript or Orna source to execute.

A patch operation is exactly one of:

```text
[0, path, value]             add
[1, path]                    remove
[2, path, value]             replace
[3, from_path, to_path]      move
```

Path components are `[0, field_name_or_id]` for a property, `[1, table_uuid, primary_key]` for a relation child, `[2, index]` for a positional child, and `[3, explicit_key]` for a keyed child. A property component is terminal unless its value is itself a Present node. Patches do not walk arbitrary private fields inside application values; replace that whole property instead.

`add` requires an absent property/key or a child insertion index in 0…length. `remove` requires an existing nonroot target. `replace` requires an existing target; the empty path replaces the complete root. `move` removes its source first and resolves the destination in that resulting tree, cannot move a node inside itself, and preserves its stable identity. Duplicate sibling keys, missing paths and invalid types invalidate the entire patch.

<span id="ORNA-PROTO-003"></span>**ORNA-PROTO-003** The client MUST apply the ordered operations to a temporary tree and publish them atomically only if its current revision equals the declared base and every operation succeeds. Otherwise it MUST discard the patch and request resynchronisation. It MUST NOT leave a partially patched visible tree.

Fine-grained deltas are optional optimisations. Root replacement is always available. A host may coalesce visual updates for a slow client into its latest complete snapshot; this does not drop database rows or move source checkpoints.

## 30.6 Actions, evaluation and request identity {#protocol-actions-evaluation-and-request-identity}

A resource handle names a page/watchable value produced by the session's evaluation or configured entry function. An action handle is tied to the issuing watch and page revision. A stale action, wrong watch, incompatible typed input or unknown handle fails before any action execution; the host may send a new snapshot so the user can act on current state.

An eval source is exactly one REPL input. A watch source is exactly one expression whose resolved call graph is read-only and externally effect-free; its clock dependency is permitted and scheduled explicitly. Ordinary text in a page or event is never executable source.

For an event/eval, the fingerprint is SHA-256 of ASCII `orna.request.v1`, a zero byte, and canonical OVB encoding of `[session_id, message_type, envelope_watch, body_without_fingerprint]`. Identifiers in this structural tuple are byte strings. The host recomputes and compares it. Every supported optional extension field participates in these bytes. Non-event/eval requests obtain a fingerprint by the same rule using their entire body; clients need not send the redundant fingerprint field.

### 30.6.1 Algorithm REQUEST-1 {#protocol-algorithm-request-1}

1. Decode, authenticate, enforce session ownership, validate the message and compute its fingerprint.
2. In a local durable reservation transaction, compare `(session, request_id)`. A different fingerprint fails. An existing terminal record returns the recorded outcome. An active record identifies that same operation; it does not schedule another copy.
3. Pin the requested database context and record admission with the safe operation identity. Resolve and type-check the source/action before its first user effect. Failure records one terminal diagnostic without writes.
4. Execute one activation with the reservation lease. Orna-controlled writes, the terminal claim and the compact terminal outcome are committed together on success. Failure/cancellation rolls back the user transaction and records terminal failure/cancellation separately. A read-only result also becomes a retained terminal outcome.
5. After a crash, a reserved operation without a terminal outcome is not automatically replayed. If it was proven to have performed only Orna-controlled transactional effects, recovery may establish rollback and mark it orphaned; if external effects may have occurred, report uncertainty. Neither case manufactures success or silently repeats external effects.
6. Send the terminal result if connected. A lost response does not undo a committed outcome. `request_status` reads the record without executing the request. Retention may prune rich output but keeps the identity/fingerprint and terminal disposition for the advertised reservation lifetime.

<span id="ORNA-PROTO-004"></span>**ORNA-PROTO-004** A durable admission reservation and a terminal transactional claim are distinct records/states. The terminal claim and successful Orna writes MUST share one transaction. A reservation without a terminal claim MUST NOT be treated as permission to execute a potentially effectful request again.

## 30.7 Limits, connection errors and reconnection {#protocol-limits-connection-errors-and-reconnection}

The `limits` JSON object contains `max_message_bytes`, `max_depth`, `max_nodes`, `max_collection_items`, `max_outgoing_bytes` and `request_retention_ms`, all positive integers. The minimum accepted profile is 16,777,216 encoded bytes, 64 nesting levels and 100,000 Present nodes. The host must advertise a bounded outgoing queue and a reservation-retention duration at least as long as the session lease. Compression, when negotiated, has separate decoded-size enforcement and cannot evade these bounds.

Malformed CBOR, an invalid envelope/version, unsupported mandatory extensions or invalid message direction closes the connection with code 1002 after a safe diagnostic where possible. Unsupported text application messages use 1003; excessive size uses 1009. A well-formed request with a stale handle, wrong fingerprint, invalid program or forbidden effect receives a correlated diagnostic without closing unrelated watches. Portable protocol codes are `wire.invalid_message`, `wire.unsupported`, `wire.limit`, `wire.request_mismatch`, `wire.stale_action`, `wire.unknown_handle`, `wire.read_only`, `wire.session_expired`, `wire.snapshot_expired` and `wire.outcome_unknown`.

On reconnect the host sends complete snapshots for resumed watches; delta history need not survive. A newly created session has new operational handles. A client must request retained status or inspect current state after uncertain delivery, not automatically resend a mutating action under a new request ID.

The message registry is also available as `profiles/live-messages.json`. The accompanying structural tests validate fields and bounds; independent client/server execution and malformed-frame fuzzing remain separate server-profile conformance obligations.



# 31. Worked reference database {#examples}

The reference database demonstrates book lending, stock transfers, exact values and resumable sensor ingestion. Its five modules run with local data and the intrinsic Orna environment. The source files are available in [`examples/reference/`](examples/reference/README.md).

## 31.1 Project layout {#examples-project-layout}

```text
main.orna
library.orna
warehouse.orna
sensors.orna
values.orna
```

The root imports the four modules explicitly. Only files reachable from `main.orna` belong to the program.

```orna
use library;
use warehouse;
use sensors;
use values;

pub fn seed() {
    library.seed();
    warehouse.seed();
}

pub fn exercise() {
    library.lend("book-1", "reader-1");
    warehouse.transfer("north", "south", "pencil", 3);
}
```

## 31.2 Lending: keys and assertions {#examples-lending-keys-and-assertions}

A loan uses its book ID as its primary key, so at most one borrower can hold a given book. Table-owned assertions check local row content. The module assertion relates two tables and therefore has no implicit owner subject. Lending a missing book fails at commit; no invalid loan is published. After seed and exercise, only book-2 is returned by available. See [tables](#tables), [assertion validation](#tables), and [transactions](#execution).

```orna
pub table Book(id: Str) {
    title: Str,
    assert every(book => book.title != "");
}

pub table Loan(book_id: Str) {
    borrower: Str,
    assert every(loan => loan.borrower != "");
}

assert every(Loan, loan =>
    exists(Book, book => book.id == loan.book_id)
);

pub fn seed() {
    Book.insert({ id: "book-1", title: "The Night Garden" });
    Book.insert({ id: "book-2", title: "A Map of Small Things" });
}

pub fn lend(book_id: Str, borrower: Str) {
    Loan.insert({ book_id: book_id, borrower: borrower });
}

pub fn return_book(book_id: Str) {
    Loan.delete(book_id);
}

pub fn available() =
    Book | filter(book => !exists(Loan, loan => loan.book_id == book.id));
```

## 31.3 Inventory: one atomic transfer {#examples-inventory-one-atomic-transfer}

The composite key distinguishes stock at two locations. A transfer reads both rows in one activation, checks its preconditions, then updates both. From quantities 12 and 4, transferring 3 produces 9 and 7. A failure after the first tentative update still rolls back both changes. See [relation operators](#standard-library) and [activation transactions](#execution).

```orna
pub table Stock(location: Str, sku: Str) {
    quantity: Int,
    assert every(stock => stock.quantity >= 0);
}

pub fn seed() {
    Stock.insert({ location: "north", sku: "pencil", quantity: 12 });
    Stock.insert({ location: "south", sku: "pencil", quantity: 4 });
}

pub fn transfer(from_location: Str, to_location: Str, sku: Str, amount: Int) {
    assert amount > 0;
    assert from_location != to_location;
    let origin = Stock | filter(stock =>
        stock.location == from_location && stock.sku == sku
    ) | one();
    let destination = Stock | filter(stock =>
        stock.location == to_location && stock.sku == sku
    ) | one();
    assert origin.quantity >= amount;
    Stock.update((from_location, sku), { quantity: origin.quantity - amount });
    Stock.update((to_location, sku), { quantity: destination.quantity + amount });
}
```

## 31.4 Sensors: resumable finite input {#examples-sensors-resumable-finite-input}

The sample type is a nominal value; Reading is persistent data. The list source has a complete built-in identity and replay contract. Three successful callbacks produce three rows and a next-item checkpoint of 3. Restarting the same consumer resumes at exhaustion. It does not repeatedly insert the same rows. See [streams](#streams), [checkpoints](#checkpoints), and [finite-source semantics](#standard-library).

```orna
pub type Sample {
    pub sensor: Str,
    pub sequence: Int,
    pub value: Decimal,
}

pub table Reading(sensor: Str, sequence: Int) {
    value: Decimal,
    assert every(reading => reading.sequence >= 0);
}

pub fn input() = Stream.from_list([
    Sample { sensor: "greenhouse", sequence: 0, value: 18.25 },
    Sample { sensor: "greenhouse", sequence: 1, value: 18.50 },
    Sample { sensor: "greenhouse", sequence: 2, value: 18.75 },
], source_identity: "example:sensors:v1");

pub fn ingest() {
    input() | for_each(sample => {
        Reading.insert({
            sensor: sample.sensor,
            sequence: sample.sequence,
            value: sample.value,
        });
    });
}
```

## 31.5 Values: refinement, variants and option {#examples-values-refinement-variants-and-option}

Score has an Int representation with always-enforced bounds. Availability is an ordinary enum with a payload-bearing variant. The Option example deliberately shows both Some and null. Neither enum branching nor optional values are an exception-catching mechanism. See [types](#types), [case expressions](#expressions), and [failure recovery](#expressions).

```orna
pub type Score = Int {
    assert >= 0;
    assert <= 100;
}

pub enum Availability {
    ready,
    waiting { reason: Str },
}

pub fn describe(value: Availability): Str = case value {
    Availability.ready: "ready",
    Availability.waiting { reason }: "waiting: {reason}",
};

pub fn optional_name(value: Str?): Str = case value {
    Some(name): name,
    null: "anonymous",
};

pub fn add(left: Int, right: Int): Int = left + right;
```

## 31.6 Interactive inspection {#examples-interactive-inspection}

The following are individual REPL inputs after loading the reference database. They are not additional module-level executable statements.

```text
library.available() | map(book => book.title)

sys.catalog.objects | map(object => object.qualified_name)

let function = sys.resolve_function("values.add");
sys.invoke(function, { left: 2, right: 3 }, as: Int)
```

The invocation returns 5. Its argument record is checked by the explicitly reflective ArgumentMap boundary; it does not permit arbitrary implicit boxing elsewhere. To inspect history, resolve a snapshot explicitly before using it:

```orna
let previous = sys.snapshot("HEAD~1");
library.Book.as_of(previous)
```

This reads historical data under the current query's code. A whole-program historical evaluation uses a database snapshot object instead; see [historical evaluation](#relations). A repository without an earlier commit fails the selector rather than returning an empty table.

## 31.7 Cancellation without changing result types {#examples-cancellation-without-changing-result-types}

A synchronous helper can wait, catch only an ordinary timeout, and await again. Both branches return the same `sys.InvocationResult<Int>` type:

```orna
pub fn wait_for_int(job: sys.InvocationHandle<Int>): sys.InvocationResult<Int> =
    sys.await(job, timeout: 1.s) |? (failure => {
        if failure.code == "sys.invoke.await_timeout" {
            sys.await(job)
        } else {
            fail(failure)
        }
    });
```

The helper is illustrative source requiring a live handle supplied by its caller. It does not change that handle's owner. An unhandled error that ends the owner triggers child cancellation for that separate reason; a timed-out wait alone does not.

## 31.8 Testing these examples {#examples-testing-these-examples}

Each example case runs in a disposable database with empty tables. The distributed expectations specify initial state, inputs, exact resulting rows and failure outcomes. Parser acceptance is separate from resolver/type/effect checking, and both are separate from execution. A conforming implementation must run all three stages before claiming the example project passes.



# 32. Conformance cases and evidence {#conformance}

## 32.1 Test corpus {#conformance-conventional-conformance-tests}

The conformance corpus contains source fixtures, complete projects, behavioural scenarios and value vectors:

```text
examples/valid/      modules/rows that must parse, resolve and type-check
examples/invalid/    source that must fail in the stated phase with the stated diagnostic
examples/reference/       complete reference database project
tests/               language-independent cases and vector oracles
tools/               selected executable reference checks
```

A generated JSON index describes the fixtures and their expected outcomes for use by implementation test runners.

<span id="ORNA-TEST-001"></span>**ORNA-TEST-001** Every valid language fixture requires successful parse, name resolution and type checking. Parser-only success MUST NOT be presented as full validity.

<span id="ORNA-TEST-002"></span>**ORNA-TEST-002** Every invalid fixture identifies the expected failing phase and stable diagnostic code.

<span id="ORNA-TEST-003"></span>**ORNA-TEST-003** The reference `examples/reference/` project is loaded as one complete database project: parse reachable modules, resolve/type-check them with its declared intrinsic environment and supplied modules, discover every loose row unit belonging to reachable tables, reconstruct keys from paths, and validate row fields/units against table schemas.

<span id="ORNA-TEST-004"></span>**ORNA-TEST-004** Bundle validation distinguishes: a test is specified, a test exists in an implementation, and a test passed. Document/index checks MUST NOT claim that Orna source executed when no implementation was run.

<span id="ORNA-TEST-005"></span>**ORNA-TEST-005** The corpus covers each normative syntax alternative, precedence boundary and deliberately invalid ambiguous form. It need not manufacture a positive and negative file for every mechanical EBNF helper production.

<span id="ORNA-TEST-006"></span>**ORNA-TEST-006** A generated requirement-to-test coverage report SHOULD expose untested normative behaviour, but it MUST remain a reporting layer over ordinary tests rather than a new test language or CI system. Informative rationale is excluded from the denominator.

<span id="ORNA-TEST-009"></span>**ORNA-TEST-009** `tests/conformance-manifest.json` MUST index valid, invalid and complete-project fixtures. A complete-project fixture MUST include `load_rows`, which validates every discovered loose row against its resolved table schema and units.

<span id="ORNA-TEST-010"></span>**ORNA-TEST-010** `tests/requirement-evidence.json` MUST attach a non-empty `tests` list to every numbered requirement. Entries may name ordinary source fixtures, project fixtures, vector suites, behavioural/fault scenarios, interoperability suites, benchmark suites or an explicit inspection requirement. The mapping is a test plan, not evidence that an implementation has executed it.

<span id="ORNA-TEST-011"></span>**ORNA-TEST-011** Machine-testable normative behaviour MUST be identified as requiring an executable fixture, vector, project, scenario or implementation suite. A plan without an implemented test remains unexecuted. Bundle validation MUST fail when a requirement has no recorded evidence obligation; production conformance additionally requires the applicable implementation evidence to pass.

## 32.2 Branch-based isolation {#conformance-branch-based-isolation}

A branch provides a natural isolated database snapshot for tests and experiments. Fixtures may be inserted into a temporary branch; teardown may be deleting that branch rather than truncating a shared database.

<span id="ORNA-TEST-007"></span>**ORNA-TEST-007** Test tooling SHOULD support temporary branch/worktree isolation without requiring a separately administered test server.

<span id="ORNA-TEST-008"></span>**ORNA-TEST-008** Branch-based tests MUST NOT imply that mocks or conventional unit tests are forbidden; the branch model is an integration-state primitive, not a ban on other testing methods.

## 32.3 Mandatory behavioral scenarios {#conformance-mandatory-behavioral-scenarios}

At minimum:

- CWD includes a local row while HEAD does not;
- automatic IDs never reuse a deleted/reset value;
- nested function writes roll back with the activation;
- an external HTTP effect is not rolled back;
- checkpoint and row writes commit together;
- poison failure pauses without infinite durable rows;
- explicit skip preserves/references payload and advances checkpoint;
- publication crashes before and after ref update recover correctly;
- automatic data commit excludes staged human changes;
- semantic merge handles compatible and conflicting schema/row edits;
- historical `sys.Run.as_of(HEAD)` is valid;
- page delta falls back to subtree replacement for an unkeyed value;
- display override does not change codec output;
- partial clone fetches only required segment blobs.


## 32.4 Evidence levels {#conformance-evidence-levels}

A fixture is a proposed test input and oracle. A syntax probe checks only its declared parsing expectation. A semantic reference model tests a selected algorithm outside an Orna implementation. A full implementation test additionally resolves, checks effects/types, executes and verifies results in the specified environment. These evidence levels MUST NOT be conflated.

The machine-readable evidence register maps each executed check to its actual subject, command and result. A requirement-family label is organisational metadata, not proof that every requirement in that family was exercised. Unexecuted implementation and interoperability obligations remain explicitly unexecuted.

<span id="ORNA-EVIDENCE-001"></span>**ORNA-EVIDENCE-001** A release report MUST distinguish authored conformance cases from executed implementation evidence and MUST NOT infer a pass from a filename, requirement association, file count or manifest validity.

<span id="ORNA-EVIDENCE-002"></span>**ORNA-EVIDENCE-002** A complete example project MUST include every required module/schema or pin an available dependency with an exact interface. Placeholder connector attachment points do not make an example self-contained.


## 32.5 Scope of conformance {#conformance-scope-of-conformance}

<span id="ORNA-CLOSURE-002"></span>**ORNA-CLOSURE-002** Benchmark, cross-platform, interoperability, security and fault obligations determine an implementation's evidence for its claimed profile. They MUST NOT be used as permission to choose different observable semantics.

<span id="ORNA-CLOSURE-003"></span>**ORNA-CLOSURE-003** Features outside the declared language/profile are absent, not implicitly implementation-defined. A source program relying on such a feature MUST receive an unsupported-feature or resolution diagnostic rather than silently acquiring vendor-specific behaviour under the same portable coordinate.

Conformance requires agreement with all applicable normative provisions. Report conflicting provisions as specification defects.


# 33. Diagnostic reference {#diagnostics}

## 33.1 Invalid forms and corrections {#diagnostics-invalid-forms-and-corrections}

| Invalid form | Corresponding Orna form | Diagnostic |
|---|---|---|
| `fn f(...) -> T` | `fn f(...): T` | `ORNA091-E-RETURN-ARROW` |
| `var x = v;` | `let x = v;` | `ORNA091-E-VAR` |
| `match x { p => y }` | `case x { p: y }` | `ORNA091-E-MATCH` |
| `Result<T,E>`, `Ok`, `Err` | successful `T` plus automatic failure | `ORNA091-E-RESULT` |
| `operation?` | `operation` (automatic propagation) | `ORNA091-E-POSTFIX-QUESTION` |
| `operation` recovery via `match Result` | `operation |? handler` | `ORNA091-E-RESULT` |
| `pub currency GBP { ... }` | `pub type GBP { impl Currency { ... } }` | `ORNA091-E-CURRENCY` |
| `static symbol` currency identity | locale-aware `std.money.format` data | `ORNA091-E-CURRENCY-SYMBOL` |
| `impl P for T { ... }` | nested `impl P { ... }` inside `T` | `ORNA091-E-IMPL-FOR` |
| `<T: P>` | `<T impl P>` | `ORNA091-E-BOUND-COLON` |
| `static fn make(...): T` in a protocol | static property or ordinary function | `ORNA091-E-STATIC-FN` |
| `TryFrom<S>` | `From<S>` that may fail | `ORNA091-E-TRYFROM` |
| implicit `A` to `B` to `C` conversion search | explicit named conversion steps | `ORNA091-E-CONVERSION-CHAIN` |
| `opaque Name` | nominal `type Name { ... }` | `ORNA091-E-OPAQUE` |
| `field: T unique` | table `assert all_unique(...)` | `ORNA091-E-FIELD-CONSTRAINT` |
| `field: T check(expr)` | table `assert every(...)` | `ORNA091-E-FIELD-CONSTRAINT` |
| `type T = B where self ...` | refined brace assertion block | `ORNA-A091-001` |
| `assert self | predicate` | `assert predicate` in owner | `ORNA-A091-002` |
| `assert Table | predicate` | `assert predicate` in table | `ORNA-A091-002` |
| one-table module `assert` | move into table body | `ORNA-A091-003` |
| `ensure`, `fact`, `constraint`, `constraints`, `|!` | `assert` | `ORNA-A091-010` |
| `assert p else e` | ordinary recovery/control flow | `ORNA-A091-006` |



## 33.2 Automated corrections {#diagnostics-automated-corrections}

<span id="ORNA-MIGRATE-003"></span>**ORNA-MIGRATE-003** Removal of postfix `?`, replacement of a function return arrow, and `var` to `let` are mechanical only when token-aware parsing proves the context.

<span id="ORNA-MIGRATE-004"></span>**ORNA-MIGRATE-004** A tool MUST NOT rewrite anonymous-function `=>` while migrating case arms.


## 33.3 Diagnostic classifications {#diagnostics-diagnostic-classifications}

The following diagnostic classifications apply to current source. Diagnostic codes are stable primary classifications; implementations may add structured notes and secondary spans.

| Code | Trigger | Primary remedy |
|---|---|---|
| `ORNA-A091-001` | refined type uses `where self` | use a brace-delimited `assert` block |
| `ORNA-A091-002` | table assertion begins with `self |` or its table name | remove the redundant owner pipeline |
| `ORNA-A091-003` | module assertion depends on only one table | move it into that table |
| `ORNA-A091-004` | declaration predicate is incompatible with its owner subject | supply a predicate for the reported owner type |
| `ORNA-A091-005` | assertion semicolon is missing | terminate the clause with `;` |
| `ORNA-A091-006` | assertion-specific `else` appears | use ordinary failure/recovery or control flow |
| `ORNA-A091-007` | declaration assertion has a forbidden effect or nondeterminism | remove the identified effect |
| `ORNA-A091-008` | executable or refined assertion is false | inspect owner, proposition and safe value |
| `ORNA-A091-009` | table or cross-table assertion is false | inspect the deterministic safe witness |
| `ORNA-A091-010` | unsupported assertion construct | use the sole `assert` form |
| `ORNA-A091-011` | assertion is empty | provide a proposition |
| `ORNA-A091-012` | module assertion has no table dependency | place the check in executable/test code |
| `ORNA091-E-RETURN-ARROW` | function return uses `->` | replace it with `:` |
| `ORNA091-E-VAR` | local declaration uses `var` | use `let`; assignment may replace the slot |
| `ORNA091-E-MATCH` | value branching uses `match`/arrow arms | use `case` and colon arms |
| `ORNA091-E-RESULT` | source uses core `Result`, `Ok`, or `Err` types | return the successful type; use automatic failure and `|?` |
| `ORNA091-E-POSTFIX-QUESTION` | expression uses postfix propagation `?` | remove it; failures propagate automatically |
| `ORNA091-E-CURRENCY` | special `currency` declaration | use a nominal type with nested `Currency` implementation |
| `ORNA091-E-CURRENCY-SYMBOL` | `Currency` declares a universal static symbol | move symbols/placement to locale formatter data |
| `ORNA091-E-IMPL-FOR` | top-level `impl P for T` | nest `impl P` inside `T` |
| `ORNA091-E-BOUND-COLON` | generic bound uses `<T: P>` | use `<T impl P>` |
| `ORNA091-E-STATIC-FN` | protocol declares a static function | use a static property or ordinary function |
| `ORNA091-E-TRYFROM` | source uses `TryFrom<S>` | use `From<S>`; it may fail |
| `ORNA091-E-CONVERSION-CHAIN` | context requires an unspoken multi-step conversion | name each conversion explicitly |
| `ORNA091-E-OPAQUE` | source uses an `opaque` type declaration | use the unified nominal/refined `type` form |
| `ORNA091-E-FIELD-CONSTRAINT` | field uses `unique` or `check(...)` | write an owner-local table assertion |

Diagnostics for secrets MUST redact sensitive values. Fixes MUST be token-aware: they may not rewrite anonymous-function `=>`, optional type suffixes `T?`, option coalescing `??`, or arbitrary ordinary pipelines merely because similar tokens occur nearby.



# 34. System API reference {#system-reference}

This reference defines system names, field types and callable interfaces. The [system model](#system) explains identity, snapshots, reflection and redaction; [administration](#administration) specifies state-changing preconditions. A machine-readable schema is available in [`api/sys.json`](api/sys.json).

Signatures use qualified callable names and type parameters such as `T`. Source declaration syntax is defined by the [grammar](#grammar). Optional types use `?`; their absence value is `null`. All relation rows are read-only.

Field types and declared invariants apply together. Source, host-path and payload fields also follow availability and redaction rules. `null` represents defined absence; unavailable mandatory data produces the specified failure.

## 34.1 Namespace index {#system-reference-namespace-index}

| Group | Use |
|---|---|
| `sys.database`, `sys.current`, `sys.rt`, `sys.repl` | Current attachment, activation, runtime and optional REPL observations. |
| `sys.catalog` | Declarations, types, source and dependencies. |
| `sys.history`, `sys.git` | Snapshots, semantic changes and Git objects. |
| `sys.storage`, `sys.build` | Physical placement and build/test metadata. |
| Root functions | Typed resolution, inspection, invocation and execution control. |
| `sys.admin` | Explicit state transitions; never row setters. |

## 34.2 Singleton views {#system-reference-singleton-views}

| Name | Type | Availability |
|---|---|---|
| `sys.database` | `sys.DatabaseView` | always while a database is attached |
| `sys.current` | `sys.CurrentContext` | every activation |
| `sys.rt` | `sys.RuntimeView` | every command/runtime owner |
| `sys.repl` | `sys.ReplView` | REPL only; otherwise sys.context.repl_unavailable |

## 34.3 Opaque identifiers {#system-reference-opaque-identifiers}

An opaque identifier cannot be implicitly substituted for a string, UUID or another identifier. Its domain and canonical encoding remain part of the named type.

### 34.3.1 `sys.DatabaseId` {#api-type-sys-databaseid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.2 `sys.FileId` {#api-type-sys-fileid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.3 `sys.DefinitionId` {#api-type-sys-definitionid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.4 `sys.ObjectId` {#api-type-sys-objectid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.5 `sys.RevisionId` {#api-type-sys-revisionid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.6 `sys.SnapshotId` {#api-type-sys-snapshotid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.7 `sys.RuntimeId` {#api-type-sys-runtimeid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.8 `sys.TransactionId` {#api-type-sys-transactionid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.9 `sys.InvocationId` {#api-type-sys-invocationid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.10 `sys.RunId` {#api-type-sys-runid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.11 `sys.QueryId` {#api-type-sys-queryid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.12 `sys.SessionId` {#api-type-sys-sessionid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.13 `sys.ClientId` {#api-type-sys-clientid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.14 `sys.TraceId` {#api-type-sys-traceid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.15 `sys.SpanId` {#api-type-sys-spanid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.16 `sys.CheckpointVersion` {#api-type-sys-checkpointversion}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.17 `sys.SegmentId` {#api-type-sys-segmentid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.18 `sys.BuildId` {#api-type-sys-buildid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.19 `sys.GitOid` {#api-type-sys-gitoid}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.20 `sys.FailureVersion` {#api-type-sys-failureversion}

Nominal identity value; see [identity and encoding](#formats).

### 34.3.21 `sys.ConsumerIdentity` {#api-type-sys-consumeridentity}

Nominal identity value; see [identity and encoding](#formats).

## 34.4 Typed row-reference aliases {#system-reference-typed-row-reference-aliases}

Each alias is a pinned `sys.RowRef<T>` for the named canonical relation. It is not an implicit conversion from the row value. Obtain it with `.reference`.

| Alias | Target |
|---|---|
| `sys.DatabaseRef` | `sys.Database` |
| `sys.NamespaceRef` | `sys.Namespace` |
| `sys.ModuleRef` | `sys.Module` |
| `sys.ImportRef` | `sys.Import` |
| `sys.FileRef` | `sys.File` |
| `sys.ObjectRef` | `sys.Object` |
| `sys.RevisionRef` | `sys.Revision` |
| `sys.DefinitionRef` | `sys.Definition` |
| `sys.TypeRef` | `sys.Type` |
| `sys.TypeParameterRef` | `sys.TypeParameter` |
| `sys.FieldRef` | `sys.Field` |
| `sys.VariantRef` | `sys.Variant` |
| `sys.ProtocolRef` | `sys.Protocol` |
| `sys.ProtocolMemberRef` | `sys.ProtocolMember` |
| `sys.ImplementationRef` | `sys.Implementation` |
| `sys.FunctionRef` | `sys.Function` |
| `sys.ParameterRef` | `sys.Parameter` |
| `sys.EffectRef` | `sys.Effect` |
| `sys.TableRef` | `sys.Table` |
| `sys.ColumnRef` | `sys.Column` |
| `sys.KeyRef` | `sys.Key` |
| `sys.AssertionRef` | `sys.Assertion` |
| `sys.ReferenceRef` | `sys.Reference` |
| `sys.PageRef` | `sys.Page` |
| `sys.DimensionRef` | `sys.Dimension` |
| `sys.UnitRef` | `sys.Unit` |
| `sys.CurrencyRef` | `sys.Currency` |
| `sys.SecretRequirementRef` | `sys.SecretRequirement` |
| `sys.ExtensionRef` | `sys.Extension` |
| `sys.DependencyRef` | `sys.Dependency` |
| `sys.DiagnosticRef` | `sys.Diagnostic` |
| `sys.SnapshotRef` | `sys.Snapshot` |
| `sys.SnapshotObjectRef` | `sys.SnapshotObject` |
| `sys.ChangeRef` | `sys.Change` |
| `sys.DiffEntryRef` | `sys.DiffEntry` |
| `sys.FileVersionRef` | `sys.FileVersion` |
| `sys.GitRepositoryRef` | `sys.GitRepository` |
| `sys.CommitRef` | `sys.Commit` |
| `sys.TreeEntryRef` | `sys.TreeEntry` |
| `sys.RefRef` | `sys.Ref` |
| `sys.BranchRef` | `sys.Branch` |
| `sys.TagRef` | `sys.Tag` |
| `sys.RemoteRef` | `sys.Remote` |
| `sys.StashRef` | `sys.Stash` |
| `sys.GitObjectRef` | `sys.GitObject` |
| `sys.WorktreeEntryRef` | `sys.WorktreeEntry` |
| `sys.QueryRef` | `sys.Query` |
| `sys.PlanRef` | `sys.Plan` |
| `sys.PlanNodeRef` | `sys.PlanNode` |
| `sys.InvocationRef` | `sys.Invocation` |
| `sys.InvocationArgumentRef` | `sys.InvocationArgument` |
| `sys.RunRef` | `sys.Run` |
| `sys.TransactionRef` | `sys.Transaction` |
| `sys.TraceRef` | `sys.Trace` |
| `sys.SpanRef` | `sys.Span` |
| `sys.TraceEventRef` | `sys.TraceEvent` |
| `sys.StreamRef` | `sys.Stream` |
| `sys.CheckpointRef` | `sys.Checkpoint` |
| `sys.CheckpointUpdateRef` | `sys.CheckpointUpdate` |
| `sys.FailureRef` | `sys.Failure` |
| `sys.SessionRef` | `sys.Session` |
| `sys.ClientRef` | `sys.Client` |
| `sys.LeaseRef` | `sys.Lease` |
| `sys.ListenerRef` | `sys.Listener` |
| `sys.StorageRef` | `sys.Storage` |
| `sys.SegmentRef` | `sys.Segment` |
| `sys.StatisticRef` | `sys.Statistic` |
| `sys.IndexRef` | `sys.Index` |
| `sys.MaterializationRef` | `sys.Materialization` |
| `sys.AllocatorRef` | `sys.Allocator` |
| `sys.HydrationRef` | `sys.Hydration` |
| `sys.CompactionRef` | `sys.Compaction` |
| `sys.MaintenanceJobRef` | `sys.MaintenanceJob` |
| `sys.StorageFileRef` | `sys.StorageFile` |
| `sys.SecretRef` | `sys.Secret` |
| `sys.SettingRef` | `sys.Setting` |
| `sys.BuildRef` | `sys.Build` |
| `sys.TestRef` | `sys.Test` |

## 34.5 Closed enumerations {#system-reference-closed-enumerations}

### 34.5.1 `sys.RuntimeMode` {#api-type-sys-runtimemode}



Values: `repl`, `run`, `serve`, `command_owner`, `embedded`.

### 34.5.2 `sys.ObjectKind` {#api-type-sys-objectkind}



Values: `namespace`, `module`, `table`, `column`, `key`, `assertion`, `function`, `parameter`, `type`, `field`, `variant`, `protocol`, `implementation`, `page`, `dimension`, `unit`, `currency`, `secret_requirement`, `extension`.

### 34.5.3 `sys.Visibility` {#api-type-sys-visibility}



Values: `private`, `module`, `package`, `public`.

### 34.5.4 `sys.Severity` {#api-type-sys-severity}



Values: `note`, `help`, `warning`, `error`, `fatal`.

### 34.5.5 `sys.SnapshotKind` {#api-type-sys-snapshotkind}



Values: `commit`, `logical_cwd`, `synthetic_merge`, `build_input`.

### 34.5.6 `sys.ChangeKind` {#api-type-sys-changekind}



Values: `add`, `modify`, `delete`, `rename`, `rekey`, `rewrite`.

### 34.5.7 `sys.ChangeArea` {#api-type-sys-changearea}



Values: `cwd`, `staged`, `commit`, `merge`, `publication`, `storage`.

### 34.5.8 `sys.RunStatus` {#api-type-sys-runstatus}



Values: `starting`, `running`, `completed`, `failed`, `cancelled`, `orphaned`.

### 34.5.9 `sys.InvocationStatus` {#api-type-sys-invocationstatus}



Values: `queued`, `running`, `succeeded`, `failed`, `cancelled`, `orphaned`.

### 34.5.10 `sys.TransactionStatus` {#api-type-sys-transactionstatus}



Values: `active`, `committing`, `committed`, `rolling_back`, `rolled_back`, `failed`.

### 34.5.11 `sys.StreamStatus` {#api-type-sys-streamstatus}



Values: `starting`, `running`, `paused`, `backing_off`, `completed`, `failed`, `cancelled`, `orphaned`.

### 34.5.12 `sys.FailureStatus` {#api-type-sys-failurestatus}



Values: `open`, `retrying`, `recovered`, `skipped`, `replaying`, `replayed`, `resolved`.

### 34.5.13 `sys.LeaseStatus` {#api-type-sys-leasestatus}



Values: `acquiring`, `held`, `releasing`, `expired`, `lost`.

### 34.5.14 `sys.BuildStatus` {#api-type-sys-buildstatus}



Values: `queued`, `running`, `passed`, `failed`, `cancelled`, `orphaned`.

### 34.5.15 `sys.TestStatus` {#api-type-sys-teststatus}



Values: `not_run`, `running`, `passed`, `failed`, `skipped`.

### 34.5.16 `sys.AssertionOwnerKind` {#api-type-sys-assertionownerkind}



Values: `executable`, `refined_type`, `table`, `module`.

### 34.5.17 `sys.AssertionScope` {#api-type-sys-assertionscope}



Values: `activation`, `value_construction`, `transaction_candidate`, `snapshot_candidate`.

### 34.5.18 `sys.ClientKind` {#api-type-sys-clientkind}



Values: `cli`, `repl`, `server`, `renderer`, `embedded`, `tool`.

### 34.5.19 `sys.DependencyConfidence` {#api-type-sys-dependencyconfidence}



Values: `exact`, `conservative`, `possible`.

### 34.5.20 `sys.DependencyKind` {#api-type-sys-dependencykind}



Values: `import`, `type_reference`, `call`, `table_read`, `table_write`, `assertion`, `implementation`, `page_entry`, `renderer_requirement`, `extension_import`, `storage_projection`.

### 34.5.21 `sys.DiffScope` {#api-type-sys-diffscope}



Values: `all`, `semantic`, `source`, `rows`, `storage`.

### 34.5.22 `sys.EffectKind` {#api-type-sys-effectkind}



Values: `table_read`, `table_write`, `network`, `filesystem`, `process`, `clock`, `randomness`, `secret_access`, `ui`, `git`, `storage_admin`, `runtime_admin`.

### 34.5.23 `sys.ExtensionTrust` {#api-type-sys-extensiontrust}



Values: `sandboxed`, `trusted`.

### 34.5.24 `sys.FileKind` {#api-type-sys-filekind}



Values: `module`, `row`, `manifest`, `package_manifest`, `storage_manifest`, `compact_segment`, `asset`, `secret_document`, `generated`, `other`.

### 34.5.25 `sys.GitObjectKind` {#api-type-sys-gitobjectkind}



Values: `commit`, `tree`, `blob`, `tag`.

### 34.5.26 `sys.HostAccess` {#api-type-sys-hostaccess}



Values: `filesystem_read`, `filesystem_write`, `network`, `process`, `environment`, `clock`, `device`.

### 34.5.27 `sys.IndexKind` {#api-type-sys-indexkind}



Values: `primary`, `btree`, `hash`, `full_text`, `vector`, `provider`.

### 34.5.28 `sys.InvokeTransaction` {#api-type-sys-invoketransaction}



Values: `inherit`, `separate`, `read_only`.

### 34.5.29 `sys.ListenerKind` {#api-type-sys-listenerkind}



Values: `git_http`, `query_http`, `websocket`, `renderer`, `admin`, `custom`.

### 34.5.30 `sys.MaintenanceKind` {#api-type-sys-maintenancekind}



Values: `flush`, `compact`, `verify`, `hydrate`, `prune`, `checkpoint_publish`, `storage_rewrite`.

### 34.5.31 `sys.PlanNodeKind` {#api-type-sys-plannodekind}



Values: `scan`, `index_lookup`, `filter`, `project`, `join`, `aggregate`, `sort`, `limit`, `materialize`, `invoke`, `assertion_validate`, `checkpoint_update`, `external`.

### 34.5.32 `sys.ProtocolMemberKind` {#api-type-sys-protocolmemberkind}



Values: `function`, `static_property`.

### 34.5.33 `sys.ReferenceAction` {#api-type-sys-referenceaction}



Values: `restrict`, `cascade`, `set_none`.

### 34.5.34 `sys.SettingScope` {#api-type-sys-settingscope}



Values: `runtime`, `database`, `repository`, `session`.

### 34.5.35 `sys.SettingSource` {#api-type-sys-settingsource}



Values: `default`, `manifest`, `environment`, `command_line`, `local_config`.

### 34.5.36 `sys.StatisticKind` {#api-type-sys-statistickind}



Values: `min`, `max`, `null_count`, `distinct_count`, `bloom`, `histogram`.

### 34.5.37 `sys.StorageFileKind` {#api-type-sys-storagefilekind}



Values: `cwd_database`, `wal`, `lock`, `socket`, `segment_cache`, `temporary`, `identity`, `other`.

### 34.5.38 `sys.StorageProfile` {#api-type-sys-storageprofile}



Values: `empty`, `editable`, `compact`, `hybrid`.

### 34.5.39 `sys.StoragePreference` {#api-type-sys-storagepreference}



Values: `automatic`, `editable`, `compact`.

### 34.5.40 `sys.StorageRewriteTarget` {#api-type-sys-storagerewritetarget}



Values: `editable`, `compact`.

### 34.5.41 `sys.TypeKind` {#api-type-sys-typekind}



Values: `alias`, `nominal`, `refined`, `enum`, `protocol`, `table_row`, `generic_parameter`, `constructed`.

### 34.5.42 `sys.VerificationStatus` {#api-type-sys-verificationstatus}



Values: `unknown`, `pending`, `valid`, `invalid`.

### 34.5.43 `sys.VerifyScope` {#api-type-sys-verifyscope}



Values: `database`, `repository`, `storage`, `history`, `checkpoints`, `all`.

### 34.5.44 `sys.ChangeTargetKind` {#api-type-sys-changetargetkind}



Values: `object`, `file`, `row`.

## 34.6 Supporting value types {#system-reference-supporting-value-types}

### 34.6.1 `sys.RowRef<T>` {#api-type-sys-rowref-t}

Snapshot/runtime-pinned reference to one canonical system-relation row.

This type has no public structural fields; use the operations that explicitly accept it.

Invariant: preserves the row natural key and resolution context.

Invariant: does not grant authority.

Invariant: live references are runtime-generation-bound.

### 34.6.2 `sys.Value` {#api-type-sys-value}

Explicit existential envelope for a value crossing a reflective system boundary.

| Field | Type |
|---|---|
| `type` | `sys.TypeRef` |
| `redacted` | `Bool` |
| `canonical_digest` | `Digest?` |

Invariant: is not a dynamic Any fallback.

Invariant: retains the exact originating static type.

Invariant: cannot be implicitly unboxed or converted.

### 34.6.3 `sys.Argument` {#api-type-sys-argument}

One named typed reflective-call argument.

| Field | Type |
|---|---|
| `name` | `Str` |
| `value` | `sys.Value` |

### 34.6.4 `sys.ArgumentMap` {#api-type-sys-argumentmap}

Immutable ordered collection of uniquely named reflective-call arguments.

| Field | Type |
|---|---|
| `entries` | `[sys.Argument]` |

Invariant: names are unique.

Invariant: canonical order is Unicode scalar-value order by name.

Invariant: record literals are boxed only under an ArgumentMap expected type.

### 34.6.5 `sys.ValueMetadata<T>` {#api-type-sys-valuemetadata-t}

Safe type, protocol and codec metadata for a value without private-field disclosure.

| Field | Type |
|---|---|
| `static_type` | `sys.TypeRef` |
| `nominal_type` | `sys.TypeRef?` |
| `protocols` | `Relation<sys.Protocol>` |
| `codecs` | `[Str]` |
| `redacted` | `Bool` |

### 34.6.6 `sys.InvocationHandle<T>` {#api-type-sys-invocationhandle-t}

Operational handle for one accepted invocation in one runtime generation.

| Field | Type |
|---|---|
| `invocation` | `sys.InvocationRef` |
| `runtime` | `sys.RuntimeId` |
| `result_type` | `sys.TypeRef` |
| `resumable` | `Bool` |

Invariant: resumable is false in 1.0.

Invariant: usable only by sys.await and sys.cancel.

Invariant: descriptive decoding never revives operational authority.

### 34.6.7 `sys.InvocationResult<T>` {#api-type-sys-invocationresult-t}

Complete terminal result returned by sys.await.

| Field | Type |
|---|---|
| `invocation` | `sys.InvocationRef` |
| `status` | `sys.InvocationStatus` |
| `value` | `T?` |
| `failure` | `sys.Diagnostic?` |
| `started` | `Instant?` |
| `ended` | `Instant` |
| `duration` | `Duration?` |

Invariant: status is terminal.

Invariant: succeeded has value and no failure.

Invariant: failed has failure and no value.

Invariant: cancelled/orphaned have no value and may carry a diagnostic.

### 34.6.8 `sys.ExpressionRef` {#api-type-sys-expressionref}

Snapshot-pinned reference to a resolved expression in a definition.

| Field | Type |
|---|---|
| `owner` | `sys.ObjectRef` |
| `definition` | `sys.DefinitionRef` |
| `span` | `sys.SourceSpan` |
| `semantic_hash` | `Digest` |

### 34.6.9 `sys.SourceSpan` {#api-type-sys-sourcespan}

Half-open canonical UTF-8 source range with one-based human coordinates.

| Field | Type |
|---|---|
| `file` | `sys.FileRef` |
| `start_byte` | `Int` |
| `end_byte` | `Int` |
| `start_line` | `Int` |
| `start_column` | `Int` |
| `end_line` | `Int` |
| `end_column` | `Int` |

Invariant: 0 <= start_byte <= end_byte.

Invariant: line and column values are one-based.

### 34.6.10 `sys.SourceMapEntry` {#api-type-sys-sourcemapentry}

Mapping from a generated span to an authored span and optional original name.

| Field | Type |
|---|---|
| `generated` | `sys.SourceSpan` |
| `original` | `sys.SourceSpan?` |
| `original_name` | `Str?` |

### 34.6.11 `sys.DiagnosticLabel` {#api-type-sys-diagnosticlabel}

One source label attached to a structured diagnostic.

| Field | Type |
|---|---|
| `span` | `sys.SourceSpan` |
| `message` | `Str` |
| `primary` | `Bool` |

### 34.6.12 `sys.SourceDocument` {#api-type-sys-sourcedocument}

Exact retained source plus source maps and explicit unavailability state.

| Field | Type |
|---|---|
| `file` | `sys.FileRef` |
| `snapshot` | `sys.SnapshotRef` |
| `text` | `Str?` |
| `exact_hash` | `Digest` |
| `encoding` | `Str` |
| `generated` | `Bool` |
| `maps` | `Relation<sys.SourceMapEntry>` |
| `unavailable_reason` | `Str?` |

Invariant: text and unavailable_reason are not both absent.

Invariant: text hashes to exact_hash after canonical decoding.

### 34.6.13 `sys.Attribution` {#api-type-sys-attribution}

Source or semantic blame attribution for one exact target range.

| Field | Type |
|---|---|
| `file` | `sys.FileRef` |
| `span` | `sys.SourceSpan` |
| `snapshot` | `sys.SnapshotRef` |
| `commit` | `sys.CommitRef?` |
| `author` | `sys.PersonIdentity?` |
| `authored_at` | `Instant?` |
| `object` | `sys.ObjectRef?` |
| `semantic` | `Bool` |

### 34.6.14 `sys.PersonIdentity` {#api-type-sys-personidentity}

Git-compatible authored identity without signing or authorization semantics.

| Field | Type |
|---|---|
| `name` | `Str` |
| `email` | `Str?` |

### 34.6.15 `sys.ObjectDescription` {#api-type-sys-objectdescription}

Structured summary of one semantic object at its pinned revision.

| Field | Type |
|---|---|
| `object` | `sys.ObjectRef` |
| `revision` | `sys.RevisionRef` |
| `kind` | `sys.ObjectKind` |
| `qualified_name` | `Str` |
| `definition` | `sys.DefinitionRef?` |
| `docs` | `Str?` |
| `signature` | `Str?` |
| `metadata` | `sys.Value` |

### 34.6.16 `sys.Explanation` {#api-type-sys-explanation}

Structured causal explanation of a diagnostic.

| Field | Type |
|---|---|
| `diagnostic` | `sys.Diagnostic` |
| `summary` | `Str` |
| `causes` | `[sys.Diagnostic]` |
| `suggestions` | `[Str]` |
| `related_objects` | `[sys.ObjectRef]` |
| `plan` | `sys.PlanRef?` |

### 34.6.17 `sys.CheckpointPosition` {#api-type-sys-checkpointposition}

Provider-specific replay position with portable equality and canonical encoding.

| Field | Type |
|---|---|
| `format` | `Str` |
| `digest` | `Digest` |

Invariant: payload is not generically orderable.

Invariant: equality includes format and canonical payload.

Invariant: provider APIs own construction and comparison beyond equality.

### 34.6.18 `sys.CompatibilityInfo` {#api-type-sys-compatibilityinfo}

Compatibility tuple embedded in builds and artifacts.

| Field | Type |
|---|---|
| `language_version` | `Str` |
| `sys_version` | `Str` |
| `canonical_orna_codec_version` | `Str` |
| `repository_layout_version` | `Str` |
| `storage_manifest_version` | `Str` |
| `presentation_protocol_version` | `Str` |
| `supported_profiles` | `[Str]` |

### 34.6.19 `sys.RuntimeInfo` {#api-type-sys-runtimeinfo}

Exact language, sys, storage, presentation and implementation coordinates for the current runtime.

| Field | Type |
|---|---|
| `language_version` | `Str` |
| `sys_version` | `Str` |
| `canonical_orna_codec_version` | `Str` |
| `repository_layout_version` | `Str` |
| `storage_manifest_version` | `Str` |
| `presentation_protocol_version` | `Str` |
| `implementation_name` | `Str` |
| `implementation_version` | `Str` |
| `build_id` | `Str` |
| `supported_profiles` | `[Str]` |
| `runtime` | `sys.RuntimeId` |
| `mode` | `sys.RuntimeMode` |
| `read_only` | `Bool` |
| `std_snapshot` | `sys.SnapshotRef?` |
| `unicode_version` | `Str` |
| `timezone_database_version` | `Str?` |

### 34.6.20 `sys.FlushResult` {#api-type-sys-flushresult}

Result of sealing durable pending rows without a logical data change.

| Field | Type |
|---|---|
| `tables` | `[sys.TableRef]` |
| `rows` | `Int` |
| `segments` | `[sys.SegmentRef]` |
| `bytes` | `Int` |
| `generation` | `Int` |
| `semantic_changes` | `Int` |

Invariant: semantic_changes is zero.

### 34.6.21 `sys.CompactionResult` {#api-type-sys-compactionresult}

Result of an atomic physical segment compaction.

| Field | Type |
|---|---|
| `tables` | `[sys.TableRef]` |
| `input_segments` | `[sys.SegmentRef]` |
| `output_segments` | `[sys.SegmentRef]` |
| `rows` | `Int` |
| `bytes_before` | `Int` |
| `bytes_after` | `Int` |
| `generation` | `Int` |
| `semantic_changes` | `Int` |

Invariant: semantic_changes is zero.

### 34.6.22 `sys.StorageRewriteResult` {#api-type-sys-storagerewriteresult}

Result of a generation-CAS rewrite between editable and compact physical representations.

| Field | Type |
|---|---|
| `table` | `sys.TableRef` |
| `from` | `sys.StorageProfile` |
| `to` | `sys.StorageProfile` |
| `rows` | `Int` |
| `previous_generation` | `Int` |
| `generation` | `Int` |
| `semantic_diff` | `Relation<sys.DiffEntry>` |

Invariant: semantic_diff is empty.

Invariant: generation is published only after complete verification.

### 34.6.23 `sys.VerificationReport` {#api-type-sys-verificationreport}

Structured integrity-verification outcome.

| Field | Type |
|---|---|
| `scope` | `sys.VerifyScope` |
| `status` | `sys.VerificationStatus` |
| `started` | `Instant` |
| `ended` | `Instant` |
| `checked` | `sys.Value` |
| `diagnostics` | `Relation<sys.Diagnostic>` |

Invariant: invalid has at least one error/fatal diagnostic.

Invariant: valid has no error/fatal diagnostic.

### 34.6.24 `sys.CancellationView` {#api-type-sys-cancellationview}

Immutable activation-local cancellation observation.

| Field | Type |
|---|---|
| `requested` | `Bool` |
| `reason` | `Str?` |
| `requested_at` | `Instant?` |

### 34.6.25 `sys.PresentationOverride` {#api-type-sys-presentationoverride}

Session-local presentation selection for one type.

| Field | Type |
|---|---|
| `type` | `sys.TypeRef` |
| `renderer` | `Str?` |
| `mode` | `Str` |
| `formatter` | `sys.FunctionRef?` |

### 34.6.26 `sys.Watch` {#api-type-sys-watch}

REPL watch expression and its most recent safe observation.

| Field | Type |
|---|---|
| `expression` | `sys.ExpressionRef` |
| `label` | `Str?` |
| `last_value` | `sys.Value?` |
| `last_diagnostic` | `sys.Diagnostic?` |
| `updated` | `Instant?` |

### 34.6.27 `sys.DatabaseView` {#api-type-sys-databaseview}

Current attached database/worktree descriptor exposed by sys.database.

| Field | Type |
|---|---|
| `id` | `sys.DatabaseId` |
| `name` | `Str` |
| `root` | `Path` |
| `root_module` | `sys.ModuleRef` |
| `cwd` | `sys.SnapshotRef` |
| `head` | `sys.SnapshotRef?` |
| `branch` | `sys.BranchRef?` |
| `writable` | `Bool` |
| `attached_as` | `Str?` |
| `repository_layout_version` | `Str` |
| `storage_manifest_version` | `Str` |

### 34.6.28 `sys.CurrentContext` {#api-type-sys-currentcontext}

Immutable context of the current activation exposed by sys.current.

| Field | Type |
|---|---|
| `snapshot` | `sys.SnapshotRef` |
| `transaction` | `sys.TransactionRef?` |
| `invocation` | `sys.InvocationRef?` |
| `run` | `sys.RunRef?` |
| `session` | `sys.SessionRef?` |
| `client` | `sys.ClientRef?` |
| `logical_cwd` | `Path` |
| `locale` | `Locale` |
| `timezone` | `TimeZone` |
| `trace` | `sys.TraceRef?` |
| `cancellation` | `sys.CancellationView` |
| `present_context` | `PresentContext` |

### 34.6.29 `sys.RuntimeView` {#api-type-sys-runtimeview}

Current live runtime owner and live relation handles exposed by sys.rt.

| Field | Type |
|---|---|
| `id` | `sys.RuntimeId` |
| `mode` | `sys.RuntimeMode` |
| `started` | `Instant` |
| `owner_pid` | `Int?` |
| `owner_endpoint` | `Str?` |
| `read_only` | `Bool` |
| `runs` | `Relation<sys.Run>` |
| `streams` | `Relation<sys.Stream>` |
| `transactions` | `Relation<sys.Transaction>` |
| `invocations` | `Relation<sys.Invocation>` |
| `queries` | `Relation<sys.Query>` |
| `sessions` | `Relation<sys.Session>` |
| `clients` | `Relation<sys.Client>` |
| `traces` | `Relation<sys.Trace>` |
| `failures` | `Relation<sys.Failure>` |
| `diagnostics` | `Relation<sys.Diagnostic>` |
| `leases` | `Relation<sys.Lease>` |
| `listeners` | `Relation<sys.Listener>` |
| `plans` | `Relation<sys.Plan>` |
| `plan_nodes` | `Relation<sys.PlanNode>` |
| `invocation_arguments` | `Relation<sys.InvocationArgument>` |
| `spans` | `Relation<sys.Span>` |
| `events` | `Relation<sys.TraceEvent>` |
| `checkpoints` | `Relation<sys.Checkpoint>` |
| `checkpoint_updates` | `Relation<sys.CheckpointUpdate>` |

### 34.6.30 `sys.ReplView` {#api-type-sys-replview}

Interactive session presentation and watch state exposed by sys.repl.

| Field | Type |
|---|---|
| `session` | `sys.SessionRef` |
| `renderer` | `Str` |
| `width` | `Int?` |
| `height` | `Int?` |
| `locale` | `Locale` |
| `timezone` | `TimeZone` |
| `presentation_overrides` | `Relation<sys.PresentationOverride>` |
| `watched_expressions` | `Relation<sys.Watch>` |

### 34.6.31 `sys.ChangeTarget` {#api-type-sys-changetarget}

Discriminated identity of one changed semantic object, source file or table row.

| Field | Type |
|---|---|
| `kind` | `sys.ChangeTargetKind` |
| `object` | `sys.ObjectId?` |
| `file` | `sys.FileId?` |
| `table` | `sys.ObjectId?` |
| `row_key` | `sys.Value?` |

Invariant: object: only object is present; file: only file is present; row: only table and row_key are present.

Invariant: row_key retains a non-secret key type and canonical value.

Invariant: equality includes kind and all populated identity fields.

### 34.6.32 `sys.CheckpointConflict` {#api-type-sys-checkpointconflict}

A three-way merge conflict between incomparable or divergent checkpoint positions.

| Field | Type |
|---|---|
| `consumer_identity` | `sys.ConsumerIdentity` |
| `source_identity` | `Str` |
| `partition` | `Str?` |
| `base` | `sys.CheckpointPosition?` |
| `left` | `sys.CheckpointPosition?` |
| `right` | `sys.CheckpointPosition?` |
| `left_snapshot` | `sys.SnapshotRef` |
| `right_snapshot` | `sys.SnapshotRef` |
| `reason` | `Str` |

Invariant: reason is divergent_position, incompatible_format, or delete_update.

Invariant: opaque positions are never numerically ordered.

### 34.6.33 `sys.RowConflict` {#api-type-sys-rowconflict}

A three-way merge conflict at one stable table identity and primary key.

| Field | Type |
|---|---|
| `table` | `sys.TableRef` |
| `key` | `sys.Value` |
| `base` | `sys.Value?` |
| `left` | `sys.Value?` |
| `right` | `sys.Value?` |
| `reason` | `Str` |

Invariant: absence means no row, not a row with null fields.

Invariant: reason is update_update, delete_update, key_collision, or assertion_failure.

### 34.6.34 `sys.CheckoutPlan` {#api-type-sys-checkoutplan}

Read-only preview bound to the exact target and local worktree state.

| Field | Type |
|---|---|
| `token` | `Digest` |
| `target` | `sys.SnapshotRef` |
| `target_branch` | `Str?` |
| `base_head` | `sys.GitOid?` |
| `cwd` | `sys.SnapshotRef` |
| `index_digest` | `Digest` |
| `worktree_digest` | `Digest` |
| `pending_generation` | `Int` |
| `would_discard` | `[sys.ChangeTarget]` |
| `active_consumers` | `[sys.ConsumerIdentity]` |
| `conflicts` | `[sys.Diagnostic]` |

Invariant: token is a domain-separated digest of all plan inputs in canonical order.

Invariant: the plan performs no mutation.

Invariant: force is not permission to accept a stale token.

## 34.7 Canonical system relations {#system-reference-canonical-system-relations}

### 34.7.1 `sys.Database` {#api-type-sys-database}

Database/worktree attachments visible at a snapshot.

**Grouped handle:** `sys.catalog.databases`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.DatabaseRef`.

| Field | Type |
|---|---|
| `reference` | `sys.DatabaseRef` |
| `id` | `sys.DatabaseId` |
| `name` | `Str` |
| `root` | `Path` |
| `root_module` | `sys.ModuleRef` |
| `cwd` | `sys.SnapshotRef` |
| `head` | `sys.SnapshotRef?` |
| `branch` | `sys.BranchRef?` |
| `writable` | `Bool` |
| `attached_as` | `Str?` |
| `repository_layout_version` | `Str` |
| `storage_manifest_version` | `Str` |

### 34.7.2 `sys.Namespace` {#api-type-sys-namespace}

Resolved namespace tree.

**Grouped handle:** `sys.catalog.namespaces`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.NamespaceRef`.

| Field | Type |
|---|---|
| `reference` | `sys.NamespaceRef` |
| `object` | `sys.ObjectRef` |
| `database` | `sys.DatabaseRef` |
| `name` | `Str` |
| `parent` | `sys.NamespaceRef?` |
| `docs` | `Str?` |

### 34.7.3 `sys.Module` {#api-type-sys-module}

Resolved source module.

**Grouped handle:** `sys.catalog.modules`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ModuleRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ModuleRef` |
| `object` | `sys.ObjectRef` |
| `namespace` | `sys.NamespaceRef` |
| `file` | `sys.FileRef` |
| `semantic_hash` | `Digest` |
| `reachable` | `Bool` |
| `diagnostics` | `Relation<sys.Diagnostic>` |

### 34.7.4 `sys.Import` {#api-type-sys-import}

Resolved import edge.

**Grouped handle:** `sys.catalog.imports`. **Availability:** catalogue. **Natural key:** `module + position`. **Reference:** `sys.ImportRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ImportRef` |
| `module` | `sys.ModuleRef` |
| `position` | `Int` |
| `source` | `Str` |
| `target` | `sys.ObjectRef?` |
| `alias` | `Str?` |
| `visibility` | `sys.Visibility` |
| `span` | `sys.SourceSpan` |

### 34.7.5 `sys.File` {#api-type-sys-file}

Repository file/source unit metadata.

**Grouped handle:** `sys.catalog.files`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.FileRef`.

| Field | Type |
|---|---|
| `reference` | `sys.FileRef` |
| `id` | `sys.FileId` |
| `database` | `sys.DatabaseRef` |
| `path` | `Path` |
| `kind` | `sys.FileKind` |
| `git_object` | `sys.GitObjectRef?` |
| `exact_hash` | `Digest` |
| `size` | `Int` |
| `status` | `sys.ChangeKind?` |
| `text_available` | `Bool` |
| `generated` | `Bool` |

### 34.7.6 `sys.Object` {#api-type-sys-object}

Stable semantic object identity at a snapshot.

**Grouped handle:** `sys.catalog.objects`. **Availability:** catalogue. **Natural key:** `id + snapshot`. **Reference:** `sys.ObjectRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ObjectRef` |
| `id` | `sys.ObjectId` |
| `kind` | `sys.ObjectKind` |
| `qualified_name` | `Str` |
| `current_revision` | `sys.RevisionRef` |
| `definition` | `sys.DefinitionRef?` |
| `visibility` | `sys.Visibility` |
| `snapshot` | `sys.SnapshotRef` |

### 34.7.7 `sys.Revision` {#api-type-sys-revision}

Immutable semantic revision of a stable object.

**Grouped handle:** `sys.catalog.revisions`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.RevisionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.RevisionRef` |
| `id` | `sys.RevisionId` |
| `object` | `sys.ObjectRef` |
| `semantic_hash` | `Digest` |
| `source_hash` | `Digest?` |
| `introduced_in` | `sys.SnapshotRef?` |
| `supersedes` | `sys.RevisionRef?` |
| `definition` | `sys.DefinitionRef?` |

### 34.7.8 `sys.Definition` {#api-type-sys-definition}

Source definition and span of an object revision.

**Grouped handle:** `sys.catalog.definitions`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.DefinitionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.DefinitionRef` |
| `id` | `sys.DefinitionId` |
| `object` | `sys.ObjectRef` |
| `module` | `sys.ModuleRef` |
| `file` | `sys.FileRef` |
| `span` | `sys.SourceSpan` |
| `docs` | `Str?` |

### 34.7.9 `sys.Type` {#api-type-sys-type}

Resolved static type declaration or constructed type metadata.

**Grouped handle:** `sys.catalog.types`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.TypeRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TypeRef` |
| `object` | `sys.ObjectRef` |
| `kind` | `sys.TypeKind` |
| `transparent` | `Bool` |
| `base` | `sys.TypeRef?` |
| `parameters` | `Relation<sys.TypeParameter>` |
| `fields` | `Relation<sys.Field>` |
| `variants` | `Relation<sys.Variant>` |
| `assertions` | `Relation<sys.Assertion>` |

### 34.7.10 `sys.TypeParameter` {#api-type-sys-typeparameter}

Generic parameter and protocol bounds.

**Grouped handle:** `sys.catalog.type_parameters`. **Availability:** catalogue. **Natural key:** `owner + position`. **Reference:** `sys.TypeParameterRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TypeParameterRef` |
| `owner` | `sys.ObjectRef` |
| `name` | `Str` |
| `position` | `Int` |
| `bounds` | `Relation<sys.Protocol>` |
| `inferred` | `Bool` |

### 34.7.11 `sys.Field` {#api-type-sys-field}

Nominal type field metadata.

**Grouped handle:** `sys.catalog.fields`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.FieldRef`.

| Field | Type |
|---|---|
| `reference` | `sys.FieldRef` |
| `object` | `sys.ObjectRef` |
| `owner` | `sys.TypeRef` |
| `name` | `Str` |
| `position` | `Int` |
| `type` | `sys.TypeRef` |
| `visibility` | `sys.Visibility` |
| `docs` | `Str?` |

### 34.7.12 `sys.Variant` {#api-type-sys-variant}

Enum/sum variant metadata.

**Grouped handle:** `sys.catalog.variants`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.VariantRef`.

| Field | Type |
|---|---|
| `reference` | `sys.VariantRef` |
| `object` | `sys.ObjectRef` |
| `owner` | `sys.TypeRef` |
| `name` | `Str` |
| `position` | `Int` |
| `payload_type` | `sys.TypeRef?` |
| `docs` | `Str?` |

### 34.7.13 `sys.Protocol` {#api-type-sys-protocol}

Protocol declaration metadata.

**Grouped handle:** `sys.catalog.protocols`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ProtocolRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ProtocolRef` |
| `object` | `sys.ObjectRef` |
| `parameters` | `Relation<sys.TypeParameter>` |
| `members` | `Relation<sys.ProtocolMember>` |
| `docs` | `Str?` |

### 34.7.14 `sys.ProtocolMember` {#api-type-sys-protocolmember}

Required protocol function or static property.

**Grouped handle:** `sys.catalog.protocol_members`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ProtocolMemberRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ProtocolMemberRef` |
| `object` | `sys.ObjectRef` |
| `protocol` | `sys.ProtocolRef` |
| `name` | `Str` |
| `kind` | `sys.ProtocolMemberKind` |
| `type` | `sys.TypeRef` |
| `position` | `Int` |

### 34.7.15 `sys.Implementation` {#api-type-sys-implementation}

Nested protocol implementation owned by its target type.

**Grouped handle:** `sys.catalog.implementations`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ImplementationRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ImplementationRef` |
| `object` | `sys.ObjectRef` |
| `protocol` | `sys.ProtocolRef` |
| `target` | `sys.TypeRef` |
| `source_type` | `sys.TypeRef?` |
| `owner` | `sys.ObjectRef` |
| `members` | `Relation<sys.Function>` |

### 34.7.16 `sys.Function` {#api-type-sys-function}

Resolved function signature, effects and dependency surface.

**Grouped handle:** `sys.catalog.functions`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.FunctionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.FunctionRef` |
| `object` | `sys.ObjectRef` |
| `definition` | `sys.DefinitionRef` |
| `parameters` | `Relation<sys.Parameter>` |
| `return_type` | `sys.TypeRef` |
| `failure_types` | `Relation<sys.Type>` |
| `effects` | `Relation<sys.Effect>` |
| `reads` | `Relation<sys.Table>` |
| `writes` | `Relation<sys.Table>` |
| `calls` | `Relation<sys.Function>` |
| `inferred_signature` | `Bool` |
| `docs` | `Str?` |

### 34.7.17 `sys.Parameter` {#api-type-sys-parameter}

Function parameter metadata.

**Grouped handle:** `sys.catalog.parameters`. **Availability:** catalogue. **Natural key:** `function + position`. **Reference:** `sys.ParameterRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ParameterRef` |
| `function` | `sys.FunctionRef` |
| `name` | `Str` |
| `position` | `Int` |
| `type` | `sys.TypeRef` |
| `optional` | `Bool` |
| `default_expression` | `sys.ExpressionRef?` |
| `explicit_type` | `Bool` |

### 34.7.18 `sys.Effect` {#api-type-sys-effect}

Conservative static effect metadata.

**Grouped handle:** `sys.catalog.effects`. **Availability:** catalogue. **Natural key:** `function + kind + target`. **Reference:** `sys.EffectRef`.

| Field | Type |
|---|---|
| `reference` | `sys.EffectRef` |
| `function` | `sys.FunctionRef` |
| `kind` | `sys.EffectKind` |
| `target` | `sys.ObjectRef?` |
| `conditional` | `Bool` |

### 34.7.19 `sys.Table` {#api-type-sys-table}

Persistent relation declaration and storage projection.

**Grouped handle:** `sys.catalog.tables`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.TableRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TableRef` |
| `object` | `sys.ObjectRef` |
| `definition` | `sys.DefinitionRef` |
| `key` | `sys.KeyRef` |
| `columns` | `Relation<sys.Column>` |
| `assertions` | `Relation<sys.Assertion>` |
| `references` | `Relation<sys.Reference>` |
| `storage` | `sys.StorageRef` |
| `row_count` | `Int?` |
| `row_count_exact` | `Bool` |

### 34.7.20 `sys.Column` {#api-type-sys-column}

Table column metadata.

**Grouped handle:** `sys.catalog.columns`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ColumnRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ColumnRef` |
| `object` | `sys.ObjectRef` |
| `table` | `sys.TableRef` |
| `name` | `Str` |
| `position` | `Int` |
| `type` | `sys.TypeRef` |
| `optional` | `Bool` |
| `key_position` | `Int?` |
| `default_expression` | `sys.ExpressionRef?` |
| `computed_expression` | `sys.ExpressionRef?` |
| `docs` | `Str?` |

### 34.7.21 `sys.Key` {#api-type-sys-key}

Primary-key definition and path/storage codec.

**Grouped handle:** `sys.catalog.keys`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.KeyRef`.

| Field | Type |
|---|---|
| `reference` | `sys.KeyRef` |
| `object` | `sys.ObjectRef` |
| `table` | `sys.TableRef` |
| `columns` | `Relation<sys.Column>` |
| `automatic` | `Bool` |
| `allocator` | `sys.AllocatorRef?` |
| `path_codec` | `Str` |

### 34.7.22 `sys.Assertion` {#api-type-sys-assertion}

Always-enforced executable, refined, table or cross-table proposition.

**Grouped handle:** `sys.catalog.assertions`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.AssertionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.AssertionRef` |
| `object` | `sys.ObjectRef` |
| `owner` | `sys.ObjectRef` |
| `owner_kind` | `sys.AssertionOwnerKind` |
| `subject_type` | `sys.TypeRef?` |
| `expression` | `sys.ExpressionRef` |
| `dependencies` | `Relation<sys.Dependency>` |
| `definition` | `sys.DefinitionRef` |
| `source_order` | `Int` |
| `scope` | `sys.AssertionScope` |
| `deterministic` | `Bool` |

### 34.7.23 `sys.Reference` {#api-type-sys-reference}

Stored table-reference edge.

**Grouped handle:** `sys.catalog.references`. **Availability:** catalogue. **Natural key:** `from_column`. **Reference:** `sys.ReferenceRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ReferenceRef` |
| `from_table` | `sys.TableRef` |
| `from_column` | `sys.ColumnRef` |
| `to_table` | `sys.TableRef` |
| `to_key` | `sys.KeyRef` |
| `optional` | `Bool` |
| `on_delete` | `sys.ReferenceAction` |
| `on_rekey` | `sys.ReferenceAction` |

### 34.7.24 `sys.Page` {#api-type-sys-page}

Function-derived page entry metadata.

**Grouped handle:** `sys.catalog.pages`. **Availability:** catalogue. **Natural key:** `function`. **Reference:** `sys.PageRef`.

| Field | Type |
|---|---|
| `reference` | `sys.PageRef` |
| `function` | `sys.FunctionRef` |
| `path` | `Str` |
| `dependencies` | `Relation<sys.Dependency>` |
| `definition` | `sys.DefinitionRef` |

### 34.7.25 `sys.Dimension` {#api-type-sys-dimension}

Reduced physical dimension declaration.

**Grouped handle:** `sys.catalog.dimensions`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.DimensionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.DimensionRef` |
| `object` | `sys.ObjectRef` |
| `exponents` | `sys.Value` |
| `definition` | `sys.DefinitionRef` |

### 34.7.26 `sys.Unit` {#api-type-sys-unit}

Unit scale/offset and dimension.

**Grouped handle:** `sys.catalog.units`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.UnitRef`.

| Field | Type |
|---|---|
| `reference` | `sys.UnitRef` |
| `object` | `sys.ObjectRef` |
| `dimension` | `sys.DimensionRef` |
| `scale` | `Decimal` |
| `offset` | `Decimal` |
| `affine` | `Bool` |
| `definition` | `sys.DefinitionRef` |

### 34.7.27 `sys.Currency` {#api-type-sys-currency}

Nominal type implementing Currency.

**Grouped handle:** `sys.catalog.currencies`. **Availability:** catalogue. **Natural key:** `type`. **Reference:** `sys.CurrencyRef`.

| Field | Type |
|---|---|
| `reference` | `sys.CurrencyRef` |
| `type` | `sys.TypeRef` |
| `code` | `Str` |
| `minor_digits` | `Int` |
| `definition` | `sys.DefinitionRef` |

### 34.7.28 `sys.SecretRequirement` {#api-type-sys-secretrequirement}

Declared secret dependency without plaintext.

**Grouped handle:** `sys.catalog.secret_requirements`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.SecretRequirementRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SecretRequirementRef` |
| `object` | `sys.ObjectRef` |
| `name` | `Str` |
| `required_by` | `sys.ObjectRef` |
| `optional` | `Bool` |
| `definition` | `sys.DefinitionRef` |

### 34.7.29 `sys.Extension` {#api-type-sys-extension}

Extension component interface and coarse trust declaration.

**Grouped handle:** `sys.catalog.extensions`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ExtensionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ExtensionRef` |
| `object` | `sys.ObjectRef` |
| `component_digest` | `Digest` |
| `interface` | `Str` |
| `declared_imports` | `[Str]` |
| `declared_exports` | `[Str]` |
| `host_access` | `[sys.HostAccess]` |
| `trust` | `sys.ExtensionTrust` |

### 34.7.30 `sys.Dependency` {#api-type-sys-dependency}

Typed semantic dependency edge.

**Grouped handle:** `sys.catalog.dependencies`. **Availability:** catalogue. **Natural key:** `from + to + kind + span`. **Reference:** `sys.DependencyRef`.

| Field | Type |
|---|---|
| `reference` | `sys.DependencyRef` |
| `from` | `sys.ObjectRef` |
| `to` | `sys.ObjectRef` |
| `kind` | `sys.DependencyKind` |
| `definition` | `sys.DefinitionRef?` |
| `span` | `sys.SourceSpan?` |
| `confidence` | `sys.DependencyConfidence` |
| `conditional` | `Bool` |

### 34.7.31 `sys.Diagnostic` {#api-type-sys-diagnostic}

Structured diagnostic independent of renderer.

**Grouped handle:** `sys.rt.diagnostics`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.DiagnosticRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.DiagnosticRef` |
| `id` | `Str` |
| `severity` | `sys.Severity` |
| `code` | `Str` |
| `message` | `Str` |
| `object` | `sys.ObjectRef?` |
| `definition` | `sys.DefinitionRef?` |
| `primary_span` | `sys.SourceSpan?` |
| `labels` | `[sys.DiagnosticLabel]` |
| `causes` | `[sys.Diagnostic]` |
| `help` | `[Str]` |
| `data` | `sys.Value?` |
| `redacted` | `Bool` |
| `trace` | `sys.TraceRef?` |

### 34.7.32 `sys.Snapshot` {#api-type-sys-snapshot}

Committed, logical-CWD or synthetic database snapshot.

**Grouped handle:** `sys.history.snapshots`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.SnapshotRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SnapshotRef` |
| `id` | `sys.SnapshotId` |
| `kind` | `sys.SnapshotKind` |
| `database` | `sys.DatabaseRef` |
| `git_commit` | `sys.CommitRef?` |
| `logical_generation` | `Int?` |
| `created` | `Instant` |
| `parents` | `Relation<sys.Snapshot>` |
| `complete` | `Bool` |
| `missing_objects` | `Relation<sys.GitObject>` |

### 34.7.33 `sys.SnapshotObject` {#api-type-sys-snapshotobject}

Object revision membership in a snapshot.

**Grouped handle:** `sys.history.snapshot_objects`. **Availability:** catalogue. **Natural key:** `snapshot + object`. **Reference:** `sys.SnapshotObjectRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SnapshotObjectRef` |
| `snapshot` | `sys.SnapshotRef` |
| `object` | `sys.ObjectRef` |
| `revision` | `sys.RevisionRef` |
| `reachable` | `Bool` |

### 34.7.34 `sys.Change` {#api-type-sys-change}

Semantic/source/row change entry.

**Grouped handle:** `sys.history.changes`. **Availability:** catalogue. **Natural key:** `snapshot + area + target`. **Reference:** `sys.ChangeRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ChangeRef` |
| `snapshot` | `sys.SnapshotRef` |
| `target` | `sys.ChangeTarget` |
| `area` | `sys.ChangeArea` |
| `kind` | `sys.ChangeKind` |
| `object` | `sys.ObjectRef?` |
| `file` | `sys.FileRef?` |
| `table` | `sys.TableRef?` |
| `key` | `sys.Value?` |
| `before` | `sys.Value?` |
| `after` | `sys.Value?` |
| `old_name` | `Str?` |
| `new_name` | `Str?` |

Invariant: object/file/table/key projections agree exactly with target discriminator.

Invariant: snapshot identifies the compared result generation.

Invariant: one consolidated change per snapshot, area and typed target.

### 34.7.35 `sys.DiffEntry` {#api-type-sys-diffentry}

Snapshot-to-snapshot semantic diff entry.

**Grouped handle:** `sys.history.diffs`. **Availability:** derived. **Natural key:** `from + to + change.area + change.target`. **Reference:** `sys.DiffEntryRef`.

| Field | Type |
|---|---|
| `reference` | `sys.DiffEntryRef` |
| `from` | `sys.SnapshotRef` |
| `to` | `sys.SnapshotRef` |
| `change` | `sys.Change` |
| `confidence` | `sys.DependencyConfidence` |
| `details` | `sys.Value?` |

### 34.7.36 `sys.FileVersion` {#api-type-sys-fileversion}

One retained version of a repository file at a snapshot.

**Grouped handle:** `sys.history.file_versions`. **Availability:** derived. **Natural key:** `file + snapshot`. **Reference:** `sys.FileVersionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.FileVersionRef` |
| `file` | `sys.FileRef` |
| `snapshot` | `sys.SnapshotRef` |
| `path` | `Path` |
| `kind` | `sys.FileKind` |
| `git_object` | `sys.GitObjectRef?` |
| `exact_hash` | `Digest` |
| `size` | `Int` |
| `change` | `sys.ChangeKind?` |
| `text_available` | `Bool` |
| `generated` | `Bool` |

### 34.7.37 `sys.GitRepository` {#api-type-sys-gitrepository}

Underlying real Git repository metadata.

**Grouped handle:** `sys.git.repositories`. **Availability:** catalogue. **Natural key:** `database`. **Reference:** `sys.GitRepositoryRef`.

| Field | Type |
|---|---|
| `reference` | `sys.GitRepositoryRef` |
| `database` | `sys.DatabaseRef` |
| `git_dir` | `Path` |
| `worktree` | `Path?` |
| `bare` | `Bool` |
| `object_format` | `Str` |
| `partial_clone` | `Bool` |

### 34.7.38 `sys.Commit` {#api-type-sys-commit}

Git commit and corresponding Orna snapshot.

**Grouped handle:** `sys.git.commits`. **Availability:** catalogue. **Natural key:** `oid`. **Reference:** `sys.CommitRef`.

| Field | Type |
|---|---|
| `reference` | `sys.CommitRef` |
| `oid` | `sys.GitOid` |
| `snapshot` | `sys.SnapshotRef` |
| `parents` | `Relation<sys.Commit>` |
| `author` | `sys.PersonIdentity` |
| `committer` | `sys.PersonIdentity` |
| `message` | `Str` |
| `authored_at` | `Instant` |
| `committed_at` | `Instant` |
| `tree` | `sys.GitObjectRef` |

### 34.7.39 `sys.TreeEntry` {#api-type-sys-treeentry}

Git tree entry.

**Grouped handle:** `sys.git.tree_entries`. **Availability:** catalogue. **Natural key:** `tree + path`. **Reference:** `sys.TreeEntryRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TreeEntryRef` |
| `tree` | `sys.GitObjectRef` |
| `path` | `Path` |
| `mode` | `Int` |
| `object` | `sys.GitObjectRef` |
| `kind` | `sys.GitObjectKind` |

### 34.7.40 `sys.Ref` {#api-type-sys-ref}

Git ref, including hidden Orna refs.

**Grouped handle:** `sys.git.refs`. **Availability:** catalogue. **Natural key:** `name`. **Reference:** `sys.RefRef`.

| Field | Type |
|---|---|
| `reference` | `sys.RefRef` |
| `name` | `Str` |
| `target` | `sys.GitObjectRef` |
| `symbolic_target` | `Str?` |
| `hidden` | `Bool` |
| `remote` | `sys.RemoteRef?` |

### 34.7.41 `sys.Branch` {#api-type-sys-branch}

Local/remote branch projection.

**Grouped handle:** `sys.git.branches`. **Availability:** catalogue. **Natural key:** `name`. **Reference:** `sys.BranchRef`.

| Field | Type |
|---|---|
| `reference` | `sys.BranchRef` |
| `name` | `Str` |
| `commit` | `sys.CommitRef` |
| `current` | `Bool` |
| `upstream` | `sys.RefRef?` |
| `ahead` | `Int?` |
| `behind` | `Int?` |

### 34.7.42 `sys.Tag` {#api-type-sys-tag}

Git tag projection.

**Grouped handle:** `sys.git.tags`. **Availability:** catalogue. **Natural key:** `name`. **Reference:** `sys.TagRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TagRef` |
| `name` | `Str` |
| `object` | `sys.GitObjectRef` |
| `annotated` | `Bool` |
| `message` | `Str?` |
| `tagger` | `sys.PersonIdentity?` |

### 34.7.43 `sys.Remote` {#api-type-sys-remote}

Git remote without plaintext credentials.

**Grouped handle:** `sys.git.remotes`. **Availability:** catalogue. **Natural key:** `name`. **Reference:** `sys.RemoteRef`.

| Field | Type |
|---|---|
| `reference` | `sys.RemoteRef` |
| `name` | `Str` |
| `fetch_urls` | `[Str]` |
| `push_urls` | `[Str]` |
| `promisor` | `Bool` |
| `last_error` | `sys.Diagnostic?` |

### 34.7.44 `sys.Stash` {#api-type-sys-stash}

Git stash projection.

**Grouped handle:** `sys.git.stashes`. **Availability:** catalogue. **Natural key:** `index`. **Reference:** `sys.StashRef`.

| Field | Type |
|---|---|
| `reference` | `sys.StashRef` |
| `index` | `Int` |
| `commit` | `sys.CommitRef` |
| `message` | `Str` |
| `created` | `Instant?` |

### 34.7.45 `sys.GitObject` {#api-type-sys-gitobject}

Git object availability and type.

**Grouped handle:** `sys.git.objects`. **Availability:** catalogue. **Natural key:** `oid`. **Reference:** `sys.GitObjectRef`.

| Field | Type |
|---|---|
| `reference` | `sys.GitObjectRef` |
| `oid` | `sys.GitOid` |
| `kind` | `sys.GitObjectKind` |
| `size` | `Int?` |
| `available_locally` | `Bool` |
| `promisor_remote` | `sys.RemoteRef?` |

### 34.7.46 `sys.WorktreeEntry` {#api-type-sys-worktreeentry}

Physical Git worktree status, distinct from logical CWD state.

**Grouped handle:** `sys.git.worktree`. **Availability:** local-durable. **Natural key:** `path`. **Reference:** `sys.WorktreeEntryRef`.

| Field | Type |
|---|---|
| `reference` | `sys.WorktreeEntryRef` |
| `path` | `Path` |
| `index_status` | `sys.ChangeKind?` |
| `worktree_status` | `sys.ChangeKind?` |
| `ignored` | `Bool` |
| `untracked` | `Bool` |

### 34.7.47 `sys.Query` {#api-type-sys-query}

Query execution observation.

**Grouped handle:** `sys.rt.queries`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.QueryRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.QueryRef` |
| `id` | `sys.QueryId` |
| `expression` | `Str?` |
| `function` | `sys.FunctionRef?` |
| `snapshot` | `sys.SnapshotRef` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.InvocationStatus` |
| `rows` | `Int?` |
| `bytes` | `Int?` |
| `duration` | `Duration?` |
| `plan` | `sys.PlanRef?` |
| `trace` | `sys.TraceRef?` |
| `failure` | `sys.Diagnostic?` |

### 34.7.48 `sys.Plan` {#api-type-sys-plan}

Structured explain plan.

**Grouped handle:** `sys.rt.plans`. **Availability:** derived. **Natural key:** `id`. **Reference:** `sys.PlanRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.PlanRef` |
| `id` | `Str` |
| `snapshot` | `sys.SnapshotRef` |
| `root` | `sys.PlanNodeRef` |
| `estimated_cost` | `Decimal?` |
| `actual_available` | `Bool` |
| `warnings` | `[sys.Diagnostic]` |

### 34.7.49 `sys.PlanNode` {#api-type-sys-plannode}

One structured plan operation.

**Grouped handle:** `sys.rt.plan_nodes`. **Availability:** derived. **Natural key:** `plan + position`. **Reference:** `sys.PlanNodeRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.PlanNodeRef` |
| `plan` | `sys.PlanRef` |
| `parent` | `sys.PlanNodeRef?` |
| `position` | `Int` |
| `kind` | `sys.PlanNodeKind` |
| `inputs` | `Relation<sys.PlanNode>` |
| `object` | `sys.ObjectRef?` |
| `estimated_rows` | `Int?` |
| `actual_rows` | `Int?` |
| `estimated_bytes` | `Int?` |
| `actual_bytes` | `Int?` |
| `predicate` | `sys.ExpressionRef?` |
| `details` | `sys.Value` |

### 34.7.50 `sys.Invocation` {#api-type-sys-invocation}

Function activation/invocation observation.

**Grouped handle:** `sys.rt.invocations`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.InvocationRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.InvocationRef` |
| `id` | `sys.InvocationId` |
| `function` | `sys.FunctionRef` |
| `snapshot` | `sys.SnapshotRef` |
| `parent` | `sys.InvocationRef?` |
| `run` | `sys.RunRef?` |
| `owner_session` | `sys.SessionRef?` |
| `transaction` | `sys.TransactionRef?` |
| `arguments` | `Relation<sys.InvocationArgument>` |
| `started` | `Instant?` |
| `ended` | `Instant?` |
| `status` | `sys.InvocationStatus` |
| `result_type` | `sys.TypeRef?` |
| `failure` | `sys.Diagnostic?` |
| `trace` | `sys.TraceRef?` |
| `idempotency_key_hash` | `Digest?` |

Invariant: exactly one launch owner: parent invocation or owner_session, except a top-level command run.

Invariant: ordinary function calls do not detach ownership.

Invariant: terminal state is published only after all children terminate and resources are released.

### 34.7.51 `sys.InvocationArgument` {#api-type-sys-invocationargument}

Redaction-safe invocation argument metadata.

**Grouped handle:** `sys.rt.invocation_arguments`. **Availability:** durable-observation. **Natural key:** `invocation + position`. **Reference:** `sys.InvocationArgumentRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.InvocationArgumentRef` |
| `invocation` | `sys.InvocationRef` |
| `name` | `Str` |
| `position` | `Int` |
| `type` | `sys.TypeRef` |
| `value` | `sys.Value?` |
| `digest` | `Digest?` |
| `redacted` | `Bool` |

### 34.7.52 `sys.Run` {#api-type-sys-run}

Top-level durable program-run observation.

**Grouped handle:** `sys.rt.runs`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.RunRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.RunRef` |
| `id` | `sys.RunId` |
| `consumer_identity` | `sys.ConsumerIdentity` |
| `function` | `sys.FunctionRef` |
| `source_identity` | `Str?` |
| `snapshot` | `sys.SnapshotRef` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status_at_snapshot` | `sys.RunStatus` |
| `observed_at` | `Instant` |
| `runtime_id` | `sys.RuntimeId?` |
| `live` | `Bool` |
| `invocation` | `sys.InvocationRef` |
| `checkpoint_count` | `Int` |
| `failure` | `sys.Diagnostic?` |

### 34.7.53 `sys.Transaction` {#api-type-sys-transaction}

Orna transaction observation and atomic write/checkpoint set.

**Grouped handle:** `sys.rt.transactions`. **Availability:** local-durable. **Natural key:** `id`. **Reference:** `sys.TransactionRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.TransactionRef` |
| `id` | `sys.TransactionId` |
| `activation` | `sys.InvocationRef` |
| `snapshot` | `sys.SnapshotRef` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.TransactionStatus` |
| `writes` | `Relation<sys.Change>` |
| `checkpoint_updates` | `Relation<sys.CheckpointUpdate>` |
| `failure` | `sys.Diagnostic?` |
| `rolled_back` | `Bool` |

### 34.7.54 `sys.Trace` {#api-type-sys-trace}

Bounded structured trace.

**Grouped handle:** `sys.rt.traces`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.TraceRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.TraceRef` |
| `id` | `sys.TraceId` |
| `root_invocation` | `sys.InvocationRef` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `sampled` | `Bool` |
| `spans` | `Relation<sys.Span>` |
| `detail_available` | `Bool` |

### 34.7.55 `sys.Span` {#api-type-sys-span}

Nested trace span.

**Grouped handle:** `sys.rt.spans`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.SpanRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.SpanRef` |
| `id` | `sys.SpanId` |
| `trace` | `sys.TraceRef` |
| `parent` | `sys.SpanRef?` |
| `name` | `Str` |
| `definition` | `sys.DefinitionRef?` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.InvocationStatus` |
| `attributes` | `sys.Value` |
| `events` | `Relation<sys.TraceEvent>` |

### 34.7.56 `sys.TraceEvent` {#api-type-sys-traceevent}

Trace event with redaction-safe attributes.

**Grouped handle:** `sys.rt.events`. **Availability:** durable-observation. **Natural key:** `span + sequence`. **Reference:** `sys.TraceEventRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.TraceEventRef` |
| `span` | `sys.SpanRef` |
| `sequence` | `Int` |
| `time` | `Instant` |
| `name` | `Str` |
| `attributes` | `sys.Value` |
| `diagnostic` | `sys.Diagnostic?` |

### 34.7.57 `sys.Stream` {#api-type-sys-stream}

Finite/unbounded stream execution observation.

**Grouped handle:** `sys.rt.streams`. **Availability:** durable-observation. **Natural key:** `run + source_identity + partition`. **Reference:** `sys.StreamRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.StreamRef` |
| `run` | `sys.RunRef` |
| `producer` | `sys.ObjectRef` |
| `consumer` | `sys.FunctionRef?` |
| `consumer_identity` | `sys.ConsumerIdentity` |
| `source_identity` | `Str` |
| `partition` | `Str?` |
| `status_at_snapshot` | `sys.StreamStatus` |
| `items_seen` | `Int` |
| `items_committed` | `Int` |
| `items_failed` | `Int` |
| `checkpoint` | `sys.CheckpointRef?` |
| `last_item_at` | `Instant?` |
| `last_failure` | `sys.FailureRef?` |
| `last_diagnostic` | `sys.Diagnostic?` |
| `observed_at` | `Instant` |
| `live` | `Bool` |

### 34.7.58 `sys.Checkpoint` {#api-type-sys-checkpoint}

Durable resumable consumer progress.

**Grouped handle:** `sys.rt.checkpoints`. **Availability:** local-durable. **Natural key:** `consumer_identity + source_identity + partition`. **Reference:** `sys.CheckpointRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.CheckpointRef` |
| `consumer` | `sys.FunctionRef` |
| `consumer_identity` | `sys.ConsumerIdentity` |
| `source_identity` | `Str` |
| `partition` | `Str?` |
| `position` | `sys.CheckpointPosition` |
| `position_format` | `Str` |
| `version` | `sys.CheckpointVersion` |
| `replayable` | `Bool` |
| `updated` | `Instant` |
| `published_snapshot` | `sys.SnapshotRef?` |
| `last_transaction` | `sys.TransactionRef` |

### 34.7.59 `sys.CheckpointUpdate` {#api-type-sys-checkpointupdate}

Atomic checkpoint compare-and-set update.

**Grouped handle:** `sys.rt.checkpoint_updates`. **Availability:** local-durable. **Natural key:** `transaction + checkpoint`. **Reference:** `sys.CheckpointUpdateRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.CheckpointUpdateRef` |
| `transaction` | `sys.TransactionRef` |
| `checkpoint` | `sys.CheckpointRef` |
| `expected_version` | `sys.CheckpointVersion` |
| `before` | `sys.CheckpointPosition?` |
| `after` | `sys.CheckpointPosition` |
| `committed` | `Bool` |

### 34.7.60 `sys.Failure` {#api-type-sys-failure}

One durable blocked or preserved delivery; repeated attempts update this record.

**Grouped handle:** `sys.rt.failures`. **Availability:** local-durable. **Natural key:** `consumer_identity + source_identity + partition + position_format + position`. **Reference:** `sys.FailureRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.FailureRef` |
| `consumer` | `sys.FunctionRef` |
| `consumer_identity` | `sys.ConsumerIdentity` |
| `source_identity` | `Str` |
| `partition` | `Str?` |
| `position_format` | `Str` |
| `position` | `sys.CheckpointPosition` |
| `position_digest` | `Digest` |
| `version` | `sys.FailureVersion` |
| `attempt_count` | `Int` |
| `last_attempt_at` | `Instant?` |
| `payload` | `Blob?` |
| `payload_reference` | `Str?` |
| `payload_digest` | `Digest?` |
| `error` | `sys.Diagnostic` |
| `created` | `Instant` |
| `status` | `sys.FailureStatus` |
| `status_changed` | `Instant` |
| `reason` | `Str?` |
| `replayed_at` | `Instant?` |
| `replacement_invocation` | `sys.InvocationRef?` |
| `checkpoint_before` | `sys.CheckpointPosition` |
| `checkpoint_after` | `sys.CheckpointPosition?` |
| `checkpoint_version_before` | `sys.CheckpointVersion` |

Invariant: attempt_count starts at one after the first failure and is monotonic.

Invariant: version changes on every committed state transition.

Invariant: position digest accelerates lookup but does not substitute for canonical position equality.

Invariant: retry failure updates this record without creating a successor record.

Invariant: only one retry or replay may hold this delivery lease.

Invariant: payload is redacted unless the explicit privileged payload-access boundary permits disclosure.

### 34.7.61 `sys.Session` {#api-type-sys-session}

Interactive/page session.

**Grouped handle:** `sys.rt.sessions`. **Availability:** live. **Natural key:** `id`. **Reference:** `sys.SessionRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.SessionRef` |
| `id` | `sys.SessionId` |
| `started` | `Instant` |
| `last_seen` | `Instant` |
| `client` | `sys.ClientRef?` |
| `locale` | `Locale` |
| `timezone` | `TimeZone` |
| `renderer` | `Str?` |

### 34.7.62 `sys.Client` {#api-type-sys-client}

Attached CLI/REPL/server/renderer client.

**Grouped handle:** `sys.rt.clients`. **Availability:** live. **Natural key:** `id`. **Reference:** `sys.ClientRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.ClientRef` |
| `id` | `sys.ClientId` |
| `kind` | `sys.ClientKind` |
| `connected` | `Instant` |
| `last_seen` | `Instant` |
| `protocol_version` | `Str?` |
| `remote` | `Str?` |
| `redacted` | `Bool` |

### 34.7.63 `sys.Lease` {#api-type-sys-lease}

Local owner/consumer coordination lease.

**Grouped handle:** `sys.rt.leases`. **Availability:** local-durable. **Natural key:** `name`. **Reference:** `sys.LeaseRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.LeaseRef` |
| `name` | `Str` |
| `holder` | `sys.RuntimeId` |
| `status` | `sys.LeaseStatus` |
| `acquired` | `Instant` |
| `expires` | `Instant?` |
| `generation` | `Int` |

### 34.7.64 `sys.Listener` {#api-type-sys-listener}

Optional network listener owned by the current runtime.

**Grouped handle:** `sys.rt.listeners`. **Availability:** live. **Natural key:** `id`. **Reference:** `sys.ListenerRef`.

The grouped handle selects the current-runtime observation. The canonical relation also supports retained observations where its availability class permits them; historical rows are not silently included in a live view.

| Field | Type |
|---|---|
| `reference` | `sys.ListenerRef` |
| `id` | `Str` |
| `kind` | `sys.ListenerKind` |
| `address` | `Str` |
| `started` | `Instant` |
| `clients` | `Int` |
| `tls` | `Bool` |
| `authenticated` | `Bool` |

### 34.7.65 `sys.Storage` {#api-type-sys-storage}

Logical object's physical storage status.

**Grouped handle:** `sys.storage.objects`. **Availability:** local-durable. **Natural key:** `object`. **Reference:** `sys.StorageRef`.

| Field | Type |
|---|---|
| `reference` | `sys.StorageRef` |
| `object` | `sys.ObjectRef` |
| `profile` | `sys.StorageProfile` |
| `preference` | `sys.StoragePreference` |
| `rows` | `Int?` |
| `rows_exact` | `Bool` |
| `bytes` | `Int?` |
| `pending_rows` | `Int` |
| `pending_bytes` | `Int` |
| `compression` | `Str?` |
| `logical_generation` | `Int` |
| `last_publication` | `Instant?` |
| `last_snapshot` | `sys.SnapshotRef?` |
| `location` | `Path?` |
| `publication_policy` | `sys.Value` |
| `verification` | `sys.VerificationStatus` |

### 34.7.66 `sys.Segment` {#api-type-sys-segment}

Immutable compact table segment.

**Grouped handle:** `sys.storage.segments`. **Availability:** catalogue. **Natural key:** `id`. **Reference:** `sys.SegmentRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SegmentRef` |
| `id` | `sys.SegmentId` |
| `table` | `sys.TableRef` |
| `git_object` | `sys.GitObjectRef` |
| `content_digest` | `Digest` |
| `schema_hash` | `Digest` |
| `min_key` | `sys.Value` |
| `max_key` | `sys.Value` |
| `rows` | `Int` |
| `bytes` | `Int` |
| `statistics` | `Relation<sys.Statistic>` |
| `available_locally` | `Bool` |
| `promisor_remote` | `sys.RemoteRef?` |
| `generation` | `Int` |

### 34.7.67 `sys.Statistic` {#api-type-sys-statistic}

Segment statistics used for safe planning/pruning.

**Grouped handle:** `sys.storage.statistics`. **Availability:** catalogue. **Natural key:** `segment + column + kind`. **Reference:** `sys.StatisticRef`.

| Field | Type |
|---|---|
| `reference` | `sys.StatisticRef` |
| `segment` | `sys.SegmentRef` |
| `column` | `sys.ColumnRef` |
| `kind` | `sys.StatisticKind` |
| `value` | `sys.Value?` |
| `exact` | `Bool` |
| `valid` | `Bool` |

### 34.7.68 `sys.Index` {#api-type-sys-index}

Logical/physical index metadata.

**Grouped handle:** `sys.storage.indexes`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.IndexRef`.

| Field | Type |
|---|---|
| `reference` | `sys.IndexRef` |
| `object` | `sys.ObjectRef` |
| `table` | `sys.TableRef` |
| `columns` | `Relation<sys.Column>` |
| `kind` | `sys.IndexKind` |
| `unique` | `Bool` |
| `materialized` | `Bool` |
| `valid` | `Bool` |

### 34.7.69 `sys.Materialization` {#api-type-sys-materialization}

Cached/materialized function/query result.

**Grouped handle:** `sys.storage.materializations`. **Availability:** local-durable. **Natural key:** `object + snapshot`. **Reference:** `sys.MaterializationRef`.

| Field | Type |
|---|---|
| `reference` | `sys.MaterializationRef` |
| `object` | `sys.ObjectRef` |
| `snapshot` | `sys.SnapshotRef` |
| `digest` | `Digest` |
| `rows` | `Int?` |
| `bytes` | `Int?` |
| `created` | `Instant` |
| `valid` | `Bool` |

### 34.7.70 `sys.Allocator` {#api-type-sys-allocator}

Automatic-key allocator state.

**Grouped handle:** `sys.storage.allocators`. **Availability:** local-durable. **Natural key:** `table`. **Reference:** `sys.AllocatorRef`.

| Field | Type |
|---|---|
| `reference` | `sys.AllocatorRef` |
| `table` | `sys.TableRef` |
| `strategy` | `Str` |
| `state` | `sys.Value` |
| `generation` | `Int` |
| `updated` | `Instant` |

### 34.7.71 `sys.Hydration` {#api-type-sys-hydration}

Lazy-object hydration progress.

**Grouped handle:** `sys.storage.hydrations`. **Availability:** live. **Natural key:** `object`. **Reference:** `sys.HydrationRef`.

| Field | Type |
|---|---|
| `reference` | `sys.HydrationRef` |
| `object` | `sys.GitObjectRef` |
| `remote` | `sys.RemoteRef?` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `bytes` | `Int?` |
| `status` | `sys.BuildStatus` |
| `failure` | `sys.Diagnostic?` |

### 34.7.72 `sys.Compaction` {#api-type-sys-compaction}

Atomic compact-segment rewrite.

**Grouped handle:** `sys.storage.compactions`. **Availability:** local-durable. **Natural key:** `id`. **Reference:** `sys.CompactionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.CompactionRef` |
| `id` | `Str` |
| `table` | `sys.TableRef` |
| `input_segments` | `Relation<sys.Segment>` |
| `output_segments` | `Relation<sys.Segment>` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.BuildStatus` |
| `transaction` | `sys.TransactionRef?` |
| `failure` | `sys.Diagnostic?` |

### 34.7.73 `sys.MaintenanceJob` {#api-type-sys-maintenancejob}

Verification/prune/flush/maintenance operation.

**Grouped handle:** `sys.storage.maintenance`. **Availability:** local-durable. **Natural key:** `id`. **Reference:** `sys.MaintenanceJobRef`.

| Field | Type |
|---|---|
| `reference` | `sys.MaintenanceJobRef` |
| `id` | `Str` |
| `kind` | `sys.MaintenanceKind` |
| `object` | `sys.ObjectRef?` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.BuildStatus` |
| `progress` | `Decimal?` |
| `failure` | `sys.Diagnostic?` |

### 34.7.74 `sys.StorageFile` {#api-type-sys-storagefile}

Local runtime/storage file projection.

**Grouped handle:** `sys.storage.files`. **Availability:** local-durable. **Natural key:** `path`. **Reference:** `sys.StorageFileRef`.

| Field | Type |
|---|---|
| `reference` | `sys.StorageFileRef` |
| `path` | `Path` |
| `kind` | `sys.StorageFileKind` |
| `bytes` | `Int` |
| `digest` | `Digest?` |
| `required` | `Bool` |
| `available` | `Bool` |
| `redacted` | `Bool` |

### 34.7.75 `sys.Secret` {#api-type-sys-secret}

Secret availability metadata without plaintext.

**Grouped handle:** `sys.catalog.secrets`. **Availability:** local-durable. **Natural key:** `name`. **Reference:** `sys.SecretRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SecretRef` |
| `name` | `Str` |
| `available` | `Bool` |
| `provider` | `Str` |
| `encrypted_file` | `sys.FileRef?` |
| `recipients` | `[Str]` |
| `required_by` | `Relation<sys.Object>` |
| `last_error` | `sys.Diagnostic?` |

### 34.7.76 `sys.Setting` {#api-type-sys-setting}

Effective configuration with explicit redaction.

**Grouped handle:** `sys.catalog.settings`. **Availability:** local-durable. **Natural key:** `name + scope`. **Reference:** `sys.SettingRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SettingRef` |
| `name` | `Str` |
| `value` | `sys.Value?` |
| `redacted` | `Bool` |
| `source` | `sys.SettingSource` |
| `scope` | `sys.SettingScope` |
| `effective_at` | `sys.SnapshotRef?` |

### 34.7.77 `sys.Build` {#api-type-sys-build}

Reproducible build observation.

**Grouped handle:** `sys.build.builds`. **Availability:** durable-observation. **Natural key:** `id`. **Reference:** `sys.BuildRef`.

| Field | Type |
|---|---|
| `reference` | `sys.BuildRef` |
| `id` | `sys.BuildId` |
| `snapshot` | `sys.SnapshotRef` |
| `compiler` | `Str` |
| `compatibility` | `sys.CompatibilityInfo` |
| `package_lock_digest` | `Digest?` |
| `artifact_digest` | `Digest?` |
| `started` | `Instant` |
| `ended` | `Instant?` |
| `status` | `sys.BuildStatus` |
| `diagnostics` | `Relation<sys.Diagnostic>` |

### 34.7.78 `sys.Test` {#api-type-sys-test}

Test execution observation.

**Grouped handle:** `sys.build.tests`. **Availability:** durable-observation. **Natural key:** `build + name`. **Reference:** `sys.TestRef`.

| Field | Type |
|---|---|
| `reference` | `sys.TestRef` |
| `build` | `sys.BuildRef` |
| `name` | `Str` |
| `object` | `sys.ObjectRef?` |
| `status` | `sys.TestStatus` |
| `started` | `Instant?` |
| `ended` | `Instant?` |
| `duration` | `Duration?` |
| `diagnostic` | `sys.Diagnostic?` |

## 34.8 Callable operations {#system-reference-callable-operations}

Read operations do not mutate database state. Invocation/start operations follow [INVOKE-1](#system); admin operations follow [administrative transitions](#administration). Resolution and retained-data failures are ordinary typed failures, not missing/empty successful results.

### 34.8.1 `sys.meta` {#api-call-sys-meta}

Return safe static/nominal/codec/protocol metadata for a value.

```text
fn sys.meta<T>(value: T): sys.ValueMetadata<T>
```

**Effect:** read.

### 34.8.2 `sys.object` {#api-call-sys-object}

Look up a stable object at a snapshot.

```text
fn sys.object(id: sys.ObjectId, at: sys.SnapshotRef = sys.current.snapshot): sys.ObjectRef
```

**Effect:** read.

### 34.8.3 `sys.resolve` {#api-call-sys-resolve}

Resolve a semantic name with ordinary visibility/import rules.

```text
fn sys.resolve(name: Str, kind: sys.ObjectKind? = null, at: sys.SnapshotRef = sys.current.snapshot, from: sys.ModuleRef? = null): sys.ObjectRef
```

**Effect:** read.

### 34.8.4 `sys.resolve_function` {#api-call-sys-resolve-function}

Resolve a function reference.

```text
fn sys.resolve_function(name: Str, at: sys.SnapshotRef = sys.current.snapshot, from: sys.ModuleRef? = null): sys.FunctionRef
```

**Effect:** read.

### 34.8.5 `sys.describe` {#api-call-sys-describe}

Return structured object description.

```text
fn sys.describe(object: sys.ObjectRef): sys.ObjectDescription
```

**Effect:** read.

### 34.8.6 `sys.source(ObjectRef)` {#api-call-sys-source-objectref}

Return retained exact source/source maps for an object subject to redaction.

```text
fn sys.source(object: sys.ObjectRef): sys.SourceDocument
```

**Effect:** read.

### 34.8.7 `sys.source(FileRef)` {#api-call-sys-source-fileref}

Return retained exact source/source maps for a file subject to redaction.

```text
fn sys.source(file: sys.FileRef): sys.SourceDocument
```

**Effect:** read.

### 34.8.8 `sys.history(ObjectRef)` {#api-call-sys-history-objectref}

Return stable semantic-object revision history.

```text
fn sys.history(object: sys.ObjectRef): Relation<sys.Revision>
```

**Effect:** read.

### 34.8.9 `sys.history(FileRef)` {#api-call-sys-history-fileref}

Return retained versions of one repository file.

```text
fn sys.history(file: sys.FileRef): Relation<sys.FileVersion>
```

**Effect:** read.

### 34.8.10 `sys.blame(FileRef)` {#api-call-sys-blame-fileref}

Return source attribution.

```text
fn sys.blame(target: sys.FileRef): Relation<sys.Attribution>
```

**Effect:** read.

### 34.8.11 `sys.blame(RowRef)` {#api-call-sys-blame-rowref}

Return semantic row/field attribution.

```text
fn sys.blame<T>(target: sys.RowRef<T>): Relation<sys.Attribution>
```

**Effect:** read.

### 34.8.12 `sys.dependencies` {#api-call-sys-dependencies}

Traverse outgoing dependency edges.

```text
fn sys.dependencies(object: sys.ObjectRef, transitive: Bool = false, kinds: [sys.DependencyKind]? = null): Relation<sys.Dependency>
```

**Effect:** read.

### 34.8.13 `sys.dependents` {#api-call-sys-dependents}

Traverse incoming dependency edges.

```text
fn sys.dependents(object: sys.ObjectRef, transitive: Bool = false, kinds: [sys.DependencyKind]? = null): Relation<sys.Dependency>
```

**Effect:** read.

### 34.8.14 `sys.snapshot(SnapshotRef)` {#api-call-sys-snapshot-snapshotref}

Return an already resolved snapshot reference.

```text
fn sys.snapshot(reference: sys.SnapshotRef = sys.database.cwd): sys.SnapshotRef
```

**Effect:** read.

### 34.8.15 `sys.snapshot(CommitRef)` {#api-call-sys-snapshot-commitref}

Resolve a commit snapshot.

```text
fn sys.snapshot(reference: sys.CommitRef): sys.SnapshotRef
```

**Effect:** read.

### 34.8.16 `sys.snapshot(BranchRef)` {#api-call-sys-snapshot-branchref}

Resolve the branch target observed by the reference.

```text
fn sys.snapshot(reference: sys.BranchRef): sys.SnapshotRef
```

**Effect:** read.

### 34.8.17 `sys.snapshot(TagRef)` {#api-call-sys-snapshot-tagref}

Resolve the peeled tag target.

```text
fn sys.snapshot(reference: sys.TagRef): sys.SnapshotRef
```

**Effect:** read.

### 34.8.18 `sys.snapshot(GitOid)` {#api-call-sys-snapshot-gitoid}

Resolve an exact Git object identifier.

```text
fn sys.snapshot(reference: sys.GitOid): sys.SnapshotRef
```

**Effect:** read.

### 34.8.19 `sys.snapshot(Str)` {#api-call-sys-snapshot-str}

Resolve a Git revision expression in the attached repository.

```text
fn sys.snapshot(reference: Str): sys.SnapshotRef
```

**Effect:** read.

### 34.8.20 `sys.diff` {#api-call-sys-diff}

Compute semantic/source/row diff.

```text
fn sys.diff(from: sys.SnapshotRef, to: sys.SnapshotRef, scope: sys.DiffScope = sys.DiffScope.all): Relation<sys.DiffEntry>
```

**Effect:** read.

### 34.8.21 `sys.changes` {#api-call-sys-changes}

Report changes represented by/relative to a state.

```text
fn sys.changes(snapshot: sys.SnapshotRef = sys.database.cwd, area: sys.ChangeArea? = null): Relation<sys.Change>
```

**Effect:** read.

### 34.8.22 `sys.explain(Query)` {#api-call-sys-explain-query}

Return structured query plan.

```text
fn sys.explain<T>(query: Query<T>): sys.Plan
```

**Effect:** read.

### 34.8.23 `sys.explain(FunctionRef)` {#api-call-sys-explain-functionref}

Return structured function/effect plan.

```text
fn sys.explain(function: sys.FunctionRef): sys.Plan
```

**Effect:** read.

### 34.8.24 `sys.explain(Diagnostic)` {#api-call-sys-explain-diagnostic}

Return structured causal explanation.

```text
fn sys.explain(diagnostic: sys.Diagnostic): sys.Explanation
```

**Effect:** read.

### 34.8.25 `sys.checkpoint` {#api-call-sys-checkpoint}

Read a checkpoint snapshot.

```text
fn sys.checkpoint(consumer_identity: sys.ConsumerIdentity, source_identity: Str, partition: Str? = null): sys.Checkpoint?
```

**Effect:** read.

### 34.8.26 `sys.render_diagnostic` {#api-call-sys-render-diagnostic}

Render without altering structured diagnostic.

```text
fn sys.render_diagnostic(diagnostic: sys.Diagnostic, context: PresentContext = sys.current.present_context): PresentTree
```

**Effect:** read.

### 34.8.27 `sys.rt.info` {#api-call-sys-rt-info}

Return exact language/sys/storage/protocol compatibility coordinates.

```text
fn sys.rt.info(): sys.RuntimeInfo
```

**Effect:** read.

### 34.8.28 `sys.invoke(Value)` {#api-call-sys-invoke-value}

Reflectively invoke and return an explicitly erased value envelope.

```text
fn sys.invoke(function: sys.FunctionRef, arguments: sys.ArgumentMap, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.inherit, idempotency_key: Str? = null): sys.Value
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### 34.8.29 `sys.invoke<T>` {#api-call-sys-invoke-t}

Reflectively invoke after validating the declared result against an explicit type witness.

```text
fn sys.invoke<T>(function: sys.FunctionRef, arguments: sys.ArgumentMap, as: T, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.inherit, idempotency_key: Str? = null): T
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### 34.8.30 `sys.start(Value)` {#api-call-sys-start-value}

Start an awaitable invocation with an explicitly erased result.

```text
fn sys.start(function: sys.FunctionRef, arguments: sys.ArgumentMap, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.separate, idempotency_key: Str? = null): sys.InvocationHandle<sys.Value>
```

**Effect:** invoke.

Ownership: Current operation owns the child, except a direct REPL sys.start expression/binding is session-owned. Inherit transaction mode is rejected; separate/read_only are permitted.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### 34.8.31 `sys.start<T>` {#api-call-sys-start-t}

Start an awaitable invocation after validating an explicit result type witness.

```text
fn sys.start<T>(function: sys.FunctionRef, arguments: sys.ArgumentMap, as: T, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.separate, idempotency_key: Str? = null): sys.InvocationHandle<T>
```

**Effect:** invoke.

Ownership: Current operation owns the child, except a direct REPL sys.start expression/binding is session-owned. Inherit transaction mode is rejected; separate/read_only are permitted.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### 34.8.32 `sys.await` {#api-call-sys-await}

Wait for a terminal result; timeout does not cancel.

```text
fn sys.await<T>(invocation: sys.InvocationHandle<T>, timeout: Duration? = null): sys.InvocationResult<T>
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### 34.8.33 `sys.cancel` {#api-call-sys-cancel}

Idempotently request invocation cancellation.

```text
fn sys.cancel<T>(invocation: sys.InvocationHandle<T>, reason: Str? = null): Bool
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### 34.8.34 `sys.admin.commit` {#api-call-sys-admin-commit}

Commit the validated staged state, preserving unstaged CWD changes and unpublished tail.

```text
fn sys.admin.commit(message: Str, author: sys.PersonIdentity? = null): sys.CommitRef
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.35 `sys.admin.checkout(SnapshotRef)` {#api-call-sys-admin-checkout-snapshotref}

Validate and select an already resolved snapshot.

```text
fn sys.admin.checkout(target: sys.SnapshotRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.36 `sys.admin.checkout(CommitRef)` {#api-call-sys-admin-checkout-commitref}

Validate and select a commit snapshot.

```text
fn sys.admin.checkout(target: sys.CommitRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.37 `sys.admin.checkout(BranchRef)` {#api-call-sys-admin-checkout-branchref}

Validate and select a branch target.

```text
fn sys.admin.checkout(target: sys.BranchRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.38 `sys.admin.checkout(TagRef)` {#api-call-sys-admin-checkout-tagref}

Validate and select a peeled tag target.

```text
fn sys.admin.checkout(target: sys.TagRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.39 `sys.admin.checkout(GitOid)` {#api-call-sys-admin-checkout-gitoid}

Validate and select an exact Git object.

```text
fn sys.admin.checkout(target: sys.GitOid, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.40 `sys.admin.checkout(Str)` {#api-call-sys-admin-checkout-str}

Resolve, validate and select a Git revision expression.

```text
fn sys.admin.checkout(target: Str, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.41 `sys.admin.create_branch(SnapshotRef)` {#api-call-sys-admin-create-branch-snapshotref}

Create a branch at the supplied committed snapshot or current HEAD; do not switch or commit pending changes.

```text
fn sys.admin.create_branch(name: Str, at: sys.SnapshotRef? = null): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.42 `sys.admin.create_branch(CommitRef)` {#api-call-sys-admin-create-branch-commitref}

Create a Git branch at a commit.

```text
fn sys.admin.create_branch(name: Str, at: sys.CommitRef): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.43 `sys.admin.create_branch(BranchRef)` {#api-call-sys-admin-create-branch-branchref}

Create a Git branch at another branch target.

```text
fn sys.admin.create_branch(name: Str, at: sys.BranchRef): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.44 `sys.admin.create_branch(TagRef)` {#api-call-sys-admin-create-branch-tagref}

Create a Git branch at a peeled tag target.

```text
fn sys.admin.create_branch(name: Str, at: sys.TagRef): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.45 `sys.admin.create_branch(GitOid)` {#api-call-sys-admin-create-branch-gitoid}

Create a Git branch at an exact Git object.

```text
fn sys.admin.create_branch(name: Str, at: sys.GitOid): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.46 `sys.admin.create_branch(Str)` {#api-call-sys-admin-create-branch-str}

Resolve a Git revision expression and create a branch there.

```text
fn sys.admin.create_branch(name: Str, at: Str): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.47 `sys.admin.flush` {#api-call-sys-admin-flush}

Durably seal pending rows without changing logical contents.

```text
fn sys.admin.flush(table: sys.TableRef? = null): sys.FlushResult
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.48 `sys.admin.compact` {#api-call-sys-admin-compact}

Rewrite physical segments atomically.

```text
fn sys.admin.compact(table: sys.TableRef? = null): sys.CompactionResult
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.49 `sys.admin.set_storage_preference` {#api-call-sys-admin-set-storage-preference}

Set future automatic placement preference without rewriting existing rows.

```text
fn sys.admin.set_storage_preference(table: sys.TableRef, preference: sys.StoragePreference): sys.Storage
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.50 `sys.admin.rewrite_storage` {#api-call-sys-admin-rewrite-storage}

Atomically rewrite physical placement while preserving logical rows.

```text
fn sys.admin.rewrite_storage(table: sys.TableRef, to: sys.StorageRewriteTarget): sys.StorageRewriteResult
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.51 `sys.admin.verify` {#api-call-sys-admin-verify}

Verify repository, metadata, storage and checkpoint invariants.

```text
fn sys.admin.verify(scope: sys.VerifyScope = sys.VerifyScope.database): sys.VerificationReport
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.52 `sys.admin.cancel_run` {#api-call-sys-admin-cancel-run}

Cancel a durable program run.

```text
fn sys.admin.cancel_run(run: sys.RunRef, reason: Str? = null): Bool
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.53 `sys.admin.pause_stream` {#api-call-sys-admin-pause-stream}

Pause at an item/batch transaction boundary.

```text
fn sys.admin.pause_stream(stream: sys.StreamRef, reason: Str? = null): Bool
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.54 `sys.admin.resume_stream` {#api-call-sys-admin-resume-stream}

Resume a paused stream.

```text
fn sys.admin.resume_stream(stream: sys.StreamRef): Bool
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.55 `sys.admin.reset_checkpoint` {#api-call-sys-admin-reset-checkpoint}

Compare-and-set checkpoint reset.

```text
fn sys.admin.reset_checkpoint(checkpoint: sys.CheckpointRef, expected_version: sys.CheckpointVersion, expected_position: sys.CheckpointPosition, to: sys.CheckpointPosition, reason: Str): sys.Checkpoint
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.56 `sys.admin.retry_failure` {#api-call-sys-admin-retry-failure}

Lease and retry the same blocked delivery; keep identity stable across failed attempts.

```text
fn sys.admin.retry_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus = sys.FailureStatus.open): sys.InvocationHandle<sys.Value>
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.57 `sys.admin.skip_failure` {#api-call-sys-admin-skip-failure}

Atomically skip a supported failed position and move progress.

```text
fn sys.admin.skip_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus, reason: Str): sys.Checkpoint
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.58 `sys.admin.replay_failure` {#api-call-sys-admin-replay-failure}

Reprocess a preserved skipped delivery without rewinding the live checkpoint.

```text
fn sys.admin.replay_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus = sys.FailureStatus.skipped): sys.InvocationHandle<sys.Value>
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.59 `sys.admin.resolve_failure` {#api-call-sys-admin-resolve-failure}

Resolve a preserved failure without marking its processing as successful.

```text
fn sys.admin.resolve_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus, reason: Str): sys.Failure
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.60 `sys.consumer_identity` {#api-call-sys-consumer-identity}

Derive durable consumer identity from database identity, stable function ObjectId and canonical typed arguments. Reject secret plaintext and unsupported argument values.

```text
fn sys.consumer_identity(function: sys.FunctionRef, arguments: sys.ArgumentMap): sys.ConsumerIdentity
```

**Effect:** read.

### 34.8.61 `sys.admin.plan_checkout(SnapshotRef)` {#api-call-sys-admin-plan-checkout-snapshotref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.SnapshotRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.62 `sys.admin.plan_checkout(CommitRef)` {#api-call-sys-admin-plan-checkout-commitref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.CommitRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.63 `sys.admin.plan_checkout(BranchRef)` {#api-call-sys-admin-plan-checkout-branchref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.BranchRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.64 `sys.admin.plan_checkout(TagRef)` {#api-call-sys-admin-plan-checkout-tagref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.TagRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.65 `sys.admin.plan_checkout(GitOid)` {#api-call-sys-admin-plan-checkout-gitoid}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.GitOid): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### 34.8.66 `sys.admin.plan_checkout(Str)` {#api-call-sys-admin-plan-checkout-str}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: Str): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

## 34.9 Portable failure codes {#system-reference-portable-failure-codes}

A failure code is stable machine-readable classification. The message may be localised; the code must not be replaced by vendor-only wording. Unless a more specific transition is stated, failure before admission performs no requested mutation.

| Code | Condition |
|---|---|
| `sys.version.incompatible` | Required API/profile coordinate is unavailable. |
| `sys.context.repl_unavailable` | The operation requires a REPL context that is absent. |
| `sys.object.not_found` | No object matches the exact accessible selector. |
| `sys.object.ambiguous` | The selector matches more than one allowed object. |
| `sys.object.wrong_kind` | The resolved object is not the requested kind. |
| `sys.snapshot.not_found` | The snapshot selector cannot be resolved. |
| `sys.snapshot.incomplete` | Required snapshot objects are absent or unavailable. |
| `sys.source.unavailable` | Required source bytes or maps are not retained/available. |
| `sys.source.redacted` | The requested source content is protected. |
| `sys.handle.foreign_runtime` | A live handle belongs to another runtime generation. |
| `sys.handle.expired` | A same-runtime handle no longer names a live retained operation. |
| `sys.invoke.argument_missing` | A required argument has no supplied value or default. |
| `sys.invoke.argument_unknown` | An argument name is not declared by the target. |
| `sys.invoke.argument_type` | An argument has an incompatible exact type or invalid duplicate binding. |
| `sys.invoke.return_type` | The result witness disagrees with the declared result before effects. |
| `sys.invoke.effect_unavailable` | The selected context does not permit a target effect. |
| `sys.invoke.idempotency_mismatch` | The key is already bound to a different complete invocation identity. |
| `sys.invoke.await_timeout` | Waiting expired; the target was not cancelled by the wait. |
| `sys.invoke.not_callable` | The resolved object is not an invocable function. |
| `sys.invoke.revision_unavailable` | The pinned target revision cannot be loaded. |
| `sys.checkpoint.not_found` | A required checkpoint reference does not resolve. The optional lookup function instead returns null for known absence. |
| `sys.checkpoint.conflict` | The checkpoint version/position or active lease differs from its expected state. |
| `sys.checkpoint.not_replayable` | The provider cannot resume/reset to the requested position. |
| `sys.failure.state_conflict` | The requested failure transition is not allowed from this state. |
| `sys.failure.skip_unsupported` | The provider cannot safely advance past this delivery. |
| `sys.admin.read_only` | The attachment cannot perform the requested administrative mutation. |
| `sys.admin.busy` | An incompatible owner/lease or reentrant write activation prevents admission. |
| `sys.admin.assertion_failed` | The candidate state violates a required assertion. |
| `sys.admin.missing_object` | Required objects cannot be obtained before admission. |
| `sys.admin.verification_failed` | Verification could not complete; no clean result is asserted. |
| `sys.storage.corrupt` | Physical bytes, schema, manifest or digest contradict the profile. |
| `sys.storage.unavailable` | Required physical data cannot be obtained. |
| `sys.storage.unrepresentable_key` | An editable rewrite cannot encode a key/path or meet the declared bounds. |
| `sys.storage.rewrite_conflict` | Inputs changed, collide or became stale before rewrite admission. |
| `sys.redaction.denied` | A generic operation attempted to reveal protected content. |
| `sys.invoke.snapshot_mismatch` | An explicit snapshot conflicts with the function reference or inherited transaction pin. |
| `sys.invoke.transaction_mode` | The requested transaction mode is invalid, including asynchronous inheritance. |
| `sys.git.unborn_head` | No default committed HEAD exists. |
| `sys.git.uncommitted_snapshot` | A branch/checkout target does not identify a commit. |
| `sys.git.dirty_conflict` | Local staged, unstaged or pending changes would be overwritten. |
| `sys.git.stale_plan` | The checkout preview no longer matches current state. |
| `sys.failure.stale_version` | The failure changed after the caller observed its version. |
| `sys.snapshot.expired` | A CWD pin is no longer retained and cannot be rebound. |
| `sys.git.invalid_ref` | The proposed local branch name is invalid under the adopted Git ref rules. |
| `sys.git.branch_exists` | Branch creation expected nonexistence but the branch is already present. |
| `sys.git.nothing_to_commit` | No staged change is present for this nonempty-commit operation. |


# 35. Complete grammar {#grammar}

The grammar has three entry points: `module_unit`, `row_unit` and `repl_input`. It specifies syntactic structure; contextual lexical classification, type/effect checking, value validity and resource limits remain additional requirements. The complete grammar is included below and also distributed as `grammar/orna.ebnf`.

```ebnf
(* Orna 1.0.0 syntactic grammar. Lexical classification and contextual
   restrictions are specified in the Lexical structure and Expressions chapters.
   Entry points: module_unit, row_unit, repl_input. This grammar does not
   substitute for resolution, type checking, effect checking or evaluation. *)

module_unit       = { top_level_item } ;
repl_input        = use_declaration
                  | let_statement
                  | function_declaration
                  | expression
                  ;
row_unit          = record_expression , [ ";" ] ;

top_level_item    = use_declaration
                  | table_declaration
                  | function_declaration
                  | protocol_declaration
                  | enum_declaration
                  | dimension_declaration
                  | unit_declaration
                  | type_declaration
                  | assertion_statement
                  ;

visibility        = [ "pub" ] ;

use_declaration   = "use" , namespace_path , use_tail , ";" ;
use_tail          = empty
                  | "as" , ( identifier | "_" )
                  | "." , "*"
                  | "." , "{" , identifier , { "," , identifier } , [ "," ] , "}"
                  ;
namespace_path    = identifier , { "." , identifier } ;
qualified_name    = identifier , { "." , contextual_name } ;
qualified_variant = identifier , "." , contextual_name , { "." , contextual_name } ;

(* Tables contain stored/computed fields and owner-local assertions. *)
table_declaration = visibility , "table" , identifier ,
                    [ "(" , key_parameters , ")" ] ,
                    "{" , { table_member } , "}" ;
table_member      = field_declaration | assertion_clause | nested_impl_declaration ;
key_parameters    = key_parameter , { "," , key_parameter } , [ "," ] ;
key_parameter     = identifier , ":" , type_expression , [ "=" , expression ] ;
field_declaration = identifier , ":" , type_expression ,
                    [ field_initializer ] , "," ;
field_initializer = "=" , expression        (* insert-time stored default *)
                  | "=>" , expression       (* computed field selector *)
                  ;

(* Type annotations are optional when inference has enough constraints. *)
function_declaration = visibility , "fn" , identifier ,
                       [ generic_parameters ] ,
                       "(" , [ function_parameters ] , ")" ,
                       [ ":" , type_expression ] ,
                       ( "=" , expression , ";" | block_expression ) ;
function_parameters = function_parameter , { "," , function_parameter } , [ "," ] ;
function_parameter  = ( "self" | identifier ) ,
                      [ ":" , type_expression ] , [ "=" , expression ] ;

generic_parameters = "<" , generic_parameter , { "," , generic_parameter } , [ "," ] , ">" ;
generic_parameter  = identifier , [ "impl" , protocol_bounds ] ;
protocol_bounds    = qualified_protocol , { "+" , qualified_protocol } ;
qualified_protocol = qualified_name , [ "<" , type_arguments , ">" ] ;

(* Static protocols have instance functions and static properties. *)
protocol_declaration = visibility , "protocol" , identifier , [ generic_parameters ] ,
                       "{" , { protocol_member } , "}" ;
protocol_member   = protocol_function | protocol_static_property ;
protocol_function = "fn" , identifier , [ generic_parameters ] ,
                    "(" , [ function_parameters ] , ")" ,
                    [ ":" , type_expression ] , ";" ;
protocol_static_property = "static" , identifier , ":" , type_expression , ";" ;

(* `type` is the only user-defined type declaration family. *)
type_declaration  = visibility , "type" , identifier , [ generic_parameters ] ,
                    ( "=" , type_expression , ";"
                    | "=" , type_expression , refinement_body
                    | nominal_type_body
                    ) ;
refinement_body   = "{" , { assertion_clause | nested_impl_declaration } , "}" ;
nominal_type_body = "{" , { nominal_type_member } , "}" ;
nominal_type_member = nominal_field | assertion_clause | nested_impl_declaration ;
nominal_field     = visibility , identifier , ":" , type_expression ,
                    [ "=" , expression ] , "," ;
nested_impl_declaration = "impl" , qualified_protocol ,
                          "{" , { impl_member } , "}" ;
impl_member       = impl_function | impl_static_property ;
impl_function     = "fn" , identifier , [ generic_parameters ] ,
                    "(" , [ function_parameters ] , ")" ,
                    [ ":" , type_expression ] ,
                    ( "=" , expression , ";" | block_expression ) ;
impl_static_property = "static" , identifier , [ ":" , type_expression ] ,
                       "=" , expression , ";" ;

enum_declaration = visibility , "enum" , identifier , [ generic_parameters ] ,
                   "{" , [ enum_variants ] , "}" ;
enum_variants    = enum_variant , { "," , enum_variant } , [ "," ] ;
enum_variant     = contextual_name , [ "{" , [ enum_payload_fields ] , "}" ] ;
enum_payload_fields = enum_payload_field , { "," , enum_payload_field } , [ "," ] ;
enum_payload_field  = identifier , ":" , type_expression ;

dimension_declaration = visibility , "dim" , identifier ,
                        [ "=" , dimension_expression ] , ";" ;
dimension_expression  = dimension_term , { ( "*" | "/" ) , dimension_term } ;
dimension_term        = qualified_name , [ "^" , signed_integer ] ;
unit_declaration  = visibility , "unit" , identifier , ":" , qualified_name ,
                    ( "base"
                    | "=" , expression , [ "offset" , decimal_source_numeral ] , [ "affine" ]
                    ) , ";" ;

type_expression   = type_product ;
type_product      = type_postfix , { ( "*" | "/" ) , type_postfix } ;
type_postfix      = type_primary , { "?" } ;
type_primary      = qualified_name , [ "<" , type_arguments , ">" ]
                  | "[" , type_expression , "]"
                  | "{" , [ type_fields ] , "}"
                  | "(" , type_expression , ")"
                  | tuple_type
                  | function_type
                  ;
function_type     = "fn" , "(" , [ type_expression , { "," , type_expression } , [ "," ] ] , ")" , ":" , type_expression ;
tuple_type        = "(" , ")"
                  | "(" , type_expression , "," , [ type_expression , { "," , type_expression } , [ "," ] ] , ")" ;

type_arguments    = type_expression , { "," , type_expression } ;
type_fields       = type_field , { "," , type_field } , [ "," ] ;
type_field        = contextual_name , ":" , type_expression ;

(* A block is parsed item-by-item using ORNA-PARSE-002. *)
block_expression  = "{" , { block_item } , [ expression ] , "}" ;
block_item        = semicolon_statement | braced_control_statement ;
semicolon_statement = let_statement
                    | assignment_statement
                    | assertion_statement
                    | expression_statement
                    | return_statement
                    | break_statement
                    | continue_statement
                    ;
braced_control_statement = ( if_expression | case_expression | for_expression | while_expression ) , [ ";" ]
                          | loop_expression , ";"
                          ;
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

pattern           = "_"
                  | literal_pattern
                  | constructor_pattern
                  | enum_record_pattern
                  | qualified_variant
                  | tuple_pattern
                  | record_pattern
                  | list_pattern
                  | identifier
                  ;
constructor_pattern = qualified_name , "(" , [ pattern_list ] , ")" ;
enum_record_pattern = qualified_variant , "{" , [ record_pattern_fields ] , "}" ;
tuple_pattern      = "(" , ")" | "(" , pattern , "," , [ pattern_list ] , ")" ;
record_pattern     = "{" , [ record_pattern_fields ] , "}" ;
list_pattern       = "[" , [ pattern_list ] , "]" ;
pattern_list       = pattern , { "," , pattern } , [ "," ] ;
record_pattern_fields = record_pattern_field , { "," , record_pattern_field } , [ "," ] ;
record_pattern_field  = contextual_name , [ ":" , pattern ] ;
literal_pattern   = instant_literal | date_literal | float_literal | decimal_source_numeral | integer_literal | string_literal | "true" | "false" | "null" ;

(* Anonymous functions have the lowest expression precedence. *)
expression        = lambda_expression ;
lambda_expression = lambda_parameters , "=>" , lambda_body
                  | logical_or_expression
                  ;
lambda_parameters = identifier
                  | "_"
                  | "(" , [ lambda_parameter_list ] , ")"
                  ;
lambda_parameter_list = lambda_parameter , { "," , lambda_parameter } , [ "," ] ;
lambda_parameter  = pattern , [ ":" , type_expression ] ;

(* Direct braced lambda and case bodies are classified before full parsing.
   `non_braced_expression` MUST NOT begin with `{`. *)
lambda_body       = direct_record_body | block_expression | non_braced_expression ;
case_body         = direct_record_body | block_expression | non_braced_expression ;
direct_record_body = "{" , record_fields , "}" ;
non_braced_expression = lambda_expression ;

logical_or_expression  = logical_and_expression , { "||" , logical_and_expression } ;
logical_and_expression = equality_expression , { "&&" , equality_expression } ;
equality_expression    = comparison_expression ;
comparison_expression  = coalesce_expression , [ comparison_operator , coalesce_expression ] ;
coalesce_expression    = pipeline_expression , [ "??" , coalesce_expression ] ;

(* `|` passes a successful value as argument one. `|?` invokes its stage only
   for a failure and otherwise passes the successful value through unchanged. *)
pipeline_expression = range_expression , { pipeline_operator , pipeline_stage } ;
pipeline_operator = "|" | "|?" ;
pipeline_stage    = postfix_expression ;

range_expression = additive_expression , [ range_operator , [ additive_expression ] ]
                 | range_operator , additive_expression
                 ;
range_operator    = ".." | "..=" ;

additive_expression = multiplicative_expression , { ( "+" | "-" ) , multiplicative_expression } ;
multiplicative_expression = power_expression , { ( "*" | "/" | "%" ) , power_expression } ;
power_expression  = unary_expression , [ "^" , power_expression ] ;
unary_expression  = { "!" | "-" | "+" } , postfix_expression ;

postfix_expression = primary_expression , { postfix_operator } ;
postfix_operator  = "." , contextual_name
                  | "(" , [ argument_list ] , ")"
                  | "[" , expression , "]"
                  ;
argument_list     = argument , { "," , argument } , [ "," ] ;
argument          = [ contextual_name , ":" ] , expression ;

primary_expression = repl_binding | literal
                   | generic_call_expression
                   | nominal_constructor
                   | "self"
                   | tuple_expression
                   | qualified_name
                   | record_expression
                   | list_expression
                   | if_expression
                   | case_expression
                   | for_expression
                   | while_expression
                   | loop_expression
                   | "(" , expression , ")"
                   ;
generic_call_expression = qualified_name , "<" , type_arguments , ">" , "(" , [ argument_list ] , ")" ;
nominal_constructor = qualified_name , "{" , [ record_fields ] , "}" ;
tuple_expression = "(" , ")"
                 | "(" , expression , "," , [ expression , { "," , expression } , [ "," ] ] , ")" ;

(* A leading record literal in a condition/scrutinee must be parenthesised. *)
if_expression     = "if" , expression , block_expression ,
                    [ "else" , ( if_expression | block_expression ) ] ;
case_expression   = "case" , expression , "{" , { case_arm } , "}" ;
case_arm          = pattern , [ "if" , expression ] , ":" , case_body , [ "," ] ;
for_expression    = "for" , pattern , "in" , expression , block_expression ;
while_expression  = "while" , expression , block_expression ;
loop_expression   = "loop" , block_expression ;

record_expression = "{" , [ record_fields ] , "}" ;
record_fields     = record_field , { "," , record_field } , [ "," ] ;
record_field      = contextual_name , ":" , expression ;
list_expression   = "[" , [ expression , { "," , expression } , [ "," ] ] , "]" ;

literal           = instant_literal
                  | date_literal
                  | float_literal
                  | decimal_source_numeral
                  | integer_literal
                  | string_literal
                  | "true"
                  | "false"
                  | "null"
                  ;

contextual_name   = identifier | "as" | "assert" | "base" | "break" | "case" | "continue" | "dim" | "else" | "enum" | "false" | "fn" | "for" | "if" | "impl" | "in" | "let" | "loop" | "null" | "offset" | "affine" | "protocol" | "pub" | "return" | "self" | "static" | "table" | "true" | "type" | "unit" | "use" | "while" ;

repl_binding      = "$_" | "$?" ;

identifier        = identifier_start , { identifier_continue } ;
identifier_start  = "_" | unicode_letter ;
identifier_continue = identifier_start | unicode_digit ;

integer_literal   = decimal_integer
                  | "0x" , hex_digit , { [ "_" ] , hex_digit }
                  | "0b" , binary_digit , { [ "_" ] , binary_digit }
                  ;
decimal_integer   = ascii_digit , { [ "_" ] , ascii_digit } ;
decimal_source_numeral = decimal_integer , "." , decimal_integer , [ exponent ]
                       | decimal_integer , exponent
                       ;
float_literal     = ( decimal_integer , "." , decimal_integer , [ exponent ]
                  | decimal_integer , exponent
                  | decimal_integer ) , "f"
                  ;
signed_integer    = [ "+" | "-" ] , integer_literal ;
exponent          = ( "e" | "E" ) , [ "+" | "-" ] , decimal_integer ;

string_literal    = '"' , { string_character | interpolation } , '"' ;
interpolation     = "{" , expression , "}" ;
string_character  = escape_sequence | ? any Unicode scalar except unescaped quote, backslash, or interpolation opener ? ;
escape_sequence   = "\\" , ( '"' | "\\" | "n" | "r" | "t" | "0" | unicode_escape ) ;
unicode_escape    = "u{" , hex_digit , { hex_digit } , "}" ;

date_literal      = digit4 , "-" , digit2 , "-" , digit2 ;
instant_literal   = date_literal , "T" , digit2 , ":" , digit2 , ":" , digit2 ,
                    [ "." , ascii_digit , { ascii_digit } ] , ( "Z" | offset ) ;
offset            = ( "+" | "-" ) , digit2 , ":" , digit2 ;

empty              = ;
unicode_letter     = ? Unicode 16.0.0 XID_Start character ? ;
unicode_digit      = ? Unicode 16.0.0 XID_Continue character not already admitted as identifier_start ? ;
ascii_digit        = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
hex_digit          = ? 0-9, a-f, A-F ? ;
binary_digit       = "0" | "1" ;
digit2             = ascii_digit , ascii_digit ;
digit4             = digit2 , digit2 ;
```



# 36. Normative dependencies {#references}

The editions below are the adopted external definitions. A newer installed library may implement them, but a newer edition does not silently change the Orna contract. Where this specification restricts a format, its stated profile applies. External errata require explicit adoption.

| Reference | Adopted edition and scope |
|---|---|
| <span id="ref-bcp14"></span>[RFC 2119](https://www.rfc-editor.org/rfc/rfc2119) and [RFC 8174](https://www.rfc-editor.org/rfc/rfc8174) | BCP 14 requirement words; normative meaning applies only to uppercase forms. |
| [Unicode 16.0.0](https://www.unicode.org/versions/Unicode16.0.0/) | Character repertoire and data for identifiers, canonical normalisation and case portability checks. |
| [UAX #31, revision 41](https://www.unicode.org/reports/tr31/tr31-41.html) | Unicode 16 identifier properties; the Orna XID/NFC profile is specified in [lexical structure](#lexical). |
| [UAX #15, revision 56](https://www.unicode.org/reports/tr15/tr15-56.html) | Unicode 16 normalisation. Program strings are not implicitly normalised merely because identifiers are. |
| [IEEE 754-2019](https://standards.ieee.org/ieee/754/6210/) | Binary64 arithmetic, rounding and total-order basis, with Orna's NaN aggregation rules stated in [types](#types). |
| <span id="ref-cbor"></span>[RFC 8949](https://www.rfc-editor.org/rfc/rfc8949) | CBOR structure. OVB-1 selects its own deterministic restrictions, including binary64 floats, as defined in [canonical values](#formats). |
| [RFC 9562](https://www.rfc-editor.org/rfc/rfc9562) | UUID representation and version 7. UUID network-order bytes are used in canonical encodings. |
| [RFC 4648](https://www.rfc-editor.org/rfc/rfc4648) | Base16/Base64 alphabets; each Orna profile states its padding and alphabet choice. |
| [RFC 3339](https://www.rfc-editor.org/rfc/rfc3339) | Timestamp interchange basis; Orna's calendar range, precision and leap-second restrictions are explicit in [lexical structure](#lexical). |
| [RFC 8259](https://www.rfc-editor.org/rfc/rfc8259) | JSON syntax for the selected codec and session HTTP structures. Orna rejects duplicate keys and non-scalar Unicode. |
| [RFC 6455](https://www.rfc-editor.org/rfc/rfc6455) | WebSocket framing, upgrades and control frames for `orna.present.v1`. Application messages are specified in [the live protocol](#protocol). |
| [RFC 8878](https://www.rfc-editor.org/rfc/rfc8878) | Zstandard frames used by compact storage; no out-of-band dictionary is assumed. |
| [Apache parquet-format 2.13.0](https://github.com/apache/parquet-format/tree/apache-parquet-format-2.13.0) | Thrift metadata, logical types, data pages and Float statistics selected by [compact storage](#storage). The specification-release number is not FileMetaData.version. |
| [Git reference updates](https://git-scm.com/docs/git-update-ref), [index tree updates](https://git-scm.com/docs/git-read-tree), [branch](https://git-scm.com/docs/git-branch) and [switch](https://git-scm.com/docs/git-switch) | External command behaviour is informative background; the adopted branch, staging, ref-CAS and publication rules are fully stated in [branching](#branching) and [publication](#publication), not delegated to an unspecified future Git feature. |

IANA timezone data and optional codec/connector packages change independently of the language. Operations whose result depends on those datasets must identify the selected dataset/package snapshot. The library reference specifies how that context is captured. A historical program must not silently replace it with the host's newest dataset.

A build records the versions of its implementation dependencies, including Turso, SOPS, Git libraries and renderer frameworks. These dependencies implement the observable behaviour specified here; their internal APIs and additional features are not part of the Orna interface.

