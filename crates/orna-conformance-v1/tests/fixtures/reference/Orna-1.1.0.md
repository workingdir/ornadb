# Orna 1.1.0 {#orna-specification}

**Final consolidated source reference — publication revision final-2026-10-05.**

Conceptual integrity: one executable Git snapshot, one indexed row store,
annotated immutable Blobs, one mutation engine, and editable VFS projections.
Repository writer format 3. Legacy readers retain their own recorded profiles.

This is a specification and implementation plan, not a compiled engine release.
Reference checks do not establish production/FUSE/crash/scale conformance.

[Start here](START-HERE.md) · [Plan](PLAN.md) · [Walkthrough](WALKTHROUGH.md) ·
[Migration](MIGRATION.md) · [Goal](implementation/ORCHESTRATOR.md) ·
[Validation](evidence/VALIDATION.md)

# 1. About this specification {#scope}

Orna is a statically typed language for programs whose code, relational data and committed history share a Git repository. A local embedded runtime owns transactions and resumable execution. Git commits identify committed database snapshots; the logical current working database, **CWD**, also includes durable changes not yet published into Git.

This specification defines the language, repository semantics, embedded runtime, system interface, storage and live-presentation profiles. It does not prescribe a compiler implementation language, a user-interface toolkit or a hosting provider.

## Conventions {#scope-document-status}

The specification comprises numbered requirements, algorithms, grammar productions, API schemas and format definitions. Examples and implementation notes are informative.

Examples specify their module context and dependencies. Signature tables use reference notation for intrinsic interfaces. Listings marked `text` contain signatures, placeholders or transcripts; listings marked `orna` contain source code.

## Using this document

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

## Normative language and consistency

The words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, **RECOMMENDED**, **MAY** and **OPTIONAL** have the meanings specified by [BCP 14](#ref-bcp14) only when capitalised. Lowercase prose uses those words in their ordinary sense.

Numbered requirements, named algorithms, grammar productions and machine-readable schemas are normative and apply together. Informative notes and examples do not override them. Conflicting normative provisions are specification defects, not implementation-defined alternatives.

Diagnostic codes identify primary error classifications. Implementations may add explanatory notes and secondary source spans without changing which forms the language accepts.

## Conformance classes

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

**ORNA-CONF-001** A conformance claim MUST identify its classes, implementation version and exact specification publication digest.

**ORNA-CONF-002** A claim MUST distinguish supported optional profiles from mandatory facilities and list the implementation tests actually executed.

**ORNA-CONF-003** An implementation MUST pass every applicable conformance fixture and behavioural test before claiming the corresponding class. A parsed example is not proof of type correctness or runtime behaviour.

**ORNA-CONF-004** Extensions MUST NOT change the meaning of valid source, repository trees or protocol messages in the claimed profile.

**ORNA-CONF-005** A language-processor claim MUST reject the invalid forms in the diagnostic corpus with the specified primary diagnostic class where the form can be identified.

**ORNA-CONF-006** A full-runtime claim MUST execute the assertion, automatic-failure, recovery, conversion, transaction, cancellation and cross-table validation scenarios applicable to it.

**ORNA-CLOSURE-004** Configuration options MUST NOT change the language, identity, transaction, checkpoint or repository semantics specified here. Resource limits and presentation preferences may vary only where explicitly permitted and must remain observable.

## Boundaries

Distributed transactions across independent databases, exactly-once effects in arbitrary external services, automatic resolution of all semantic merge conflicts and enterprise authorisation are outside this specification. Trusted-process deployments enforce the specified type safety, privacy boundaries, cancellation, integrity checks and destructive-operation preconditions.

## Final 1.1.0 scope and conceptual integrity

Conceptual integrity is the governing principle: one executable Git snapshot,
one stored row representation, one immutable content graph, one mutation and
validation engine, and optional UI/filesystem projections. This publication
supersedes the earlier 1.1.0 authoring draft by exact artifact identity.

The Repository-engine 1.1 claim requires repository format 3, OGS-1, ORP-1,
OGB-2, annotated Blob/value profiles and UPGRADE-3. The `vfs-linux-v1` claim
requires both read and validated write support from [VFS](#vfs); merely a
read-only mount is not that completed profile. MIME-1 applies to new Blob
values independently of installing a mount.

The first product milestone is a real table/Run/result/content path and an
editable workspace mount. It need not expose watches, saved queries, theme
systems or a complete Wasm UI. Existing language/protocol requirements remain
available; staged delivery does not lower the full conformance bar.

`source-hosted-app-v1`, `orna-browser-client-v1` and `media-inspection-v1` are
separate conditional claims. Their host capabilities must be implemented and
tested, not silently replaced by native app code or made a prerequisite for
reading a broken database. No machine-learning model is required for basic
Blob capture/inspection. [Profiles](profiles/conformance-1.1.json) identify
required sets; implementation status is separate from specification status.

# 2. Concepts and boundaries {#concepts}

Orna combines a typed language, relational data and Git-backed database history. The following concepts describe the relationships between source declarations, stored data and running programs.

## The four states of a worktree

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

## Values, declarations and relations

A **value** has a static type. A **nominal type** has an identity distinct from its representation; a **transparent alias** does not. A **refined type** is nominal and validates its base value through declaration-owned assertions. Optional values use `Some(value)` and `null`.

A **module unit** contains declarations. A **row unit** contains one schema-directed record body. Module reachability begins at root `main.orna`; a recursive filesystem scan is not the program. See [source and modules](#source-modules) and [tables](#tables).

A **table** stores keyed rows. A **relation** is a typed, possibly lazy collection over which queries operate. A table is ordered by its canonical primary key, whereas a derived relation has only the order guaranteed by its operations. An **assertion** is an always-enforced proposition, not a debug-only test. See [relations](#relations) for observation and ordering.

## Execution and progress

An **activation** is a root execution/atomicity boundary: one submitted input, a page action, a direct finite run, a child activation or a stream callback. Synchronous helper calls share their activation. A **task** is scheduled work with an activation or session owner. A **run** is a separately launched program; a **session** can own work across multiple prompt inputs.

When an owner ends, unfinished children are cancelled and joined. Cancellation is not an ordinary failure caught by `|?`. Open Orna transactions roll back; already committed work remains. See [execution](#execution).

A **stream** produces ordered items, possibly without end. A **consumer identity** identifies the resumable processing configuration, not only a function name. A **checkpoint** records the provider position from which that consumer safely resumes. Each processed item/batch and its checkpoint advance are committed together. A **failed delivery** has one stable identity across retries. See [streams](#streams) and [checkpoints](#checkpoints).

## Identity and snapshots

An **ObjectId** identifies a declaration across ordinary edits and renames. A **revision** identifies one immutable meaning of that declaration. A **snapshot** identifies an exact database state: either a committed state or a pinned local CWD generation. A moving selector such as a branch name is resolved to a snapshot before it can be used as a stable reference.

A relation's `as_of` selects historical data and its decoding schema. Whole-program historical evaluation additionally selects historical code and dependencies. The distinction is observable; see [historical queries](#relations) and [system references](#system).

## Presentation and encoding

**Inspect** provides structural developer output. **Display** provides human-facing text. **Present** provides a typed presentation tree for tables, trees, charts and other renderings. A **codec** encodes or decodes values in a specified format. Changing a formatter does not change equality, stored values, canonical bytes or Git identity. See [presentation](#presentation) and [canonical values](#formats).

The `sys` namespace is the mandatory typed introspection and control plane. The `std` namespace is optional ordinary library code, selected and pinned like other dependencies. The [intrinsic environment](#standard-library) remains sufficient for core integrity and execution without an optional library package.

# 3. Source, modules and namespaces {#source-modules}

## File extension

Module source, explicit row-interchange documents and VFS `data.orna` projections use `.orna` syntax. Their entrypoints and schemas remain distinct. Format-3 persisted rows are ORP-1 records, not physical loose-row files; legacy row files are read only through their recorded import context.

**ORNA-SOURCE-001** The parser MUST expose distinct entrypoints for module units and row units.

**ORNA-SOURCE-002** A module unit MUST contain declarations only at top level.

**ORNA-SOURCE-003** A row unit MUST contain exactly one record expression and MUST NOT contain declarations.

## Namespace mapping

Filesystem structure defines module namespaces.

```text
sensors/greenhouse/main.orna       -> sensors.greenhouse
sensors/greenhouse/input.orna -> sensors.greenhouse.input
```

**ORNA-NS-001** A directory's `main.orna` MUST define that directory's namespace.

**ORNA-NS-002** A non-`main.orna` module filename MUST add its stem as the final namespace component.

**ORNA-NS-003** The repository root `main.orna` MUST define the root namespace.

**ORNA-NS-004** A repository MUST NOT contain both `x.orna` and `x/main.orna` where both would define the same namespace.

## Reserved namespaces

**ORNA-NS-005** `sys` and `std` are the only reserved top-level namespaces.

**ORNA-NS-006** `sys` MUST be built into every conforming implementation and MUST NOT be replaced or shadowed.

**ORNA-NS-007** `std` MUST remain optional and replaceable, although its top-level name is reserved when present.

**ORNA-NS-008** Source/module/table path components MUST be NFC and siblings MUST be unique under Unicode 16.0.0 toNFKC_Casefold. The host-independent check applies during load, rename and merge; a filesystem that distinguishes colliding names does not make them portable.

## Imports

Valid forms:

```orna
use directory;
use sensors.greenhouse as climate;
use sensors.greenhouse.*;
use sensors.greenhouse.{Reading, recent};
use std.prelude as _;
```

**ORNA-IMPORT-001** `use a.b;` MUST make namespace `a.b` available under its final component unless it conflicts.

**ORNA-IMPORT-002** `use a.b as x;` MUST make the imported namespace available as `x`.

**ORNA-IMPORT-003** `use a.b.*;` MUST import all public names from `a.b` into the current lexical scope.

**ORNA-IMPORT-004** `use a.b.{X, y};` MUST import only the named public definitions.

**ORNA-IMPORT-005** `use x as _;` MUST import the prelude set explicitly exported by that pinned module, without binding a namespace alias. An absent prelude declaration imports no names; it does not mean wildcard import.

**ORNA-IMPORT-006** Importing a module MUST NOT execute arbitrary code, open a network connection or start a stream.

**ORNA-IMPORT-007** Wildcard imports are valid in committed modules and the REPL under the same rules. Local declarations and explicit imports take precedence; two wildcard imports that expose the same otherwise-unresolved name produce an ambiguity diagnostic. Import order MUST NOT select a winner.

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

# 5. Types, numbers and nominal values {#types}

## Core type forms

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

**ORNA-TYPE-001** `T?` MUST mean `Option<T>` and MUST NOT mean an unchecked nullable value.

**ORNA-TYPE-002** A failable expression has a successful value type and a separate abrupt failure channel carrying an `Error`; failure is not represented by `Result<T, E>` in source.

**ORNA-TYPE-003** Automatic failure propagation MUST NOT silently discard a stream item. Stream retry, preservation, skip and dead-letter behavior remains explicit through the stream/checkpoint model.

**ORNA-TYPE-004** Implementations MUST NOT use the same type name for raw bytes and a byte-count quantity.

**ORNA-TYPE-005** User-defined closed variants use nominal `enum` declarations. Orna does not use the expression-pipeline token `|` as a type-union or variant-list operator.

**ORNA-TYPE-006** `Result`, `Ok`, and `Err` are not reserved core type constructors or variants in 1.0.

**ORNA-TYPE-007** A user-defined `type` declaration is either a transparent alias, a nominal type, or a refined nominal type as specified in [aliases, nominal types and refinements](#expressions).

**ORNA-TYPE-008** The absence of a source annotation does not make a value dynamically typed. A conforming processor MUST infer a static type or issue a type-inference diagnostic.

## Value and binding model

Orna values are immutable. A local name introduced by `let` is a function-local slot whose binding may be reassigned by an assignment statement; reassignment replaces the entire immutable value and does not expose shared mutable object identity.

**ORNA-VALUE-001** Primitive, list, record, enum, option, quantity, money, nominal and row values MUST have value semantics.

**ORNA-VALUE-002** A row returned from a query is an immutable snapshot containing its table identity, primary key, snapshot context and logical fields. A later table update MUST NOT mutate an already-held row value.

**ORNA-VALUE-003** Closures capture values at creation time. Table handles retain their database/snapshot context unless the program explicitly selects another context.

**ORNA-VALUE-004** Cyclic ordinary list/record/nominal values are not constructible in version 1.0. Persistent graph cycles may exist through table references.

**ORNA-VALUE-005** Memory management, pointer identity and finalization timing MUST NOT be observable language semantics.

**ORNA-VALUE-006** Reassigning a `let` binding MUST NOT mutate any value previously captured, returned, stored or observed through another binding.

**ORNA-VALUE-007** `var` is not a 1.0 binding declaration. A processor that recognizes the prohibited declaration context MUST emit `ORNA091-E-VAR` and propose `let`.

## Numeric types and exact decimals

**ORNA-NUM-001** `Int` MUST represent exact integers.

**ORNA-NUM-002** `Decimal` MUST represent finite exact base-10 values with implementation resource limits but without binary-float conversion.

**ORNA-NUM-003** `Float` MUST use IEEE-754 binary64 semantics and MUST NOT be used as the canonical representation of money.

**ORNA-NUM-004** Decimal addition, subtraction and multiplication MUST be exact unless an implementation resource limit is exceeded, in which case evaluation fails with a typed `Error`.

**ORNA-NUM-005** Decimal arithmetic MUST remain exact where the mathematical result has a finite decimal representation. Division whose result is not a finite decimal MUST fail unless an explicitly selected rounding/precision operation is used.

```orna
1.decimal.divide(
    3.decimal,
    scale: 6,
    rounding: half_even,
)
```

No process-global or ambient decimal precision changes the meaning of source text.

## Float equality, sorting and aggregation

**ORNA-FLOAT-001** Ordinary Float equality and ordered comparison follow IEEE-754 binary64 semantics: `NaN != NaN`, every ordered comparison involving `NaN` is false, and `-0.0 == 0.0`.

**ORNA-FLOAT-002** Sorting Float values MUST use the IEEE 754 `totalOrder` predicate for binary64 values. In ascending order, negative NaNs precede negative infinity, finite negative values and `-0.0`; `-0.0` precedes `+0.0`; positive finite values and positive infinity follow; and positive NaNs come last. Signaling/quiet NaNs and distinct NaN payloads are ordered exactly as required by `totalOrder`.

**ORNA-FLOAT-003** `Float` MUST NOT be accepted as a primary-key component or default hash-key type.

**ORNA-FLOAT-004** Float arithmetic MUST NOT be silently reassociated or evaluated with unspecified fast-math transformations.

**ORNA-FLOAT-005** A Float aggregate MUST process rows in the relation's observable order. Empty `sum` returns additive zero; empty `min`, `max` and `mean` return `null`.

**ORNA-FLOAT-006** Float sorting order and ordinary equality are distinct relations. A sort MUST distinguish `-0.0` from `+0.0` and MAY distinguish NaN bit patterns even though ordinary equality treats the two zeros as equal and every NaN as unequal to every value, including itself.

**ORNA-FLOAT-007** For a non-empty Float input, `min` and `max` return the canonical NaN if any input is NaN. Otherwise they use numeric order, with `min` choosing `-0.0` when either zero sign is present and `max` choosing `+0.0`. Libraries MAY provide explicitly named finite-only or NaN-ignoring alternatives.

### Algorithm FLOAT-TOTAL-1

For a binary64 value, interpret its exact 64 bits as an unsigned integer `bits` and compute an unsigned sortable key:

```text
if the sign bit is 1: key = bitwise_not(bits)
otherwise:             key = bits xor 0x8000000000000000
```

Compare keys as unsigned 64-bit integers. This is the normative bit-level implementation of the ordering required by `ORNA-FLOAT-002`; it orders every finite value, infinity, signed zero, signaling NaN, quiet NaN and NaN payload deterministically. The analogous binary32 algorithm uses the mask `0x80000000`. Implementations MAY use another algorithm only when every value pair produces the same order.

## Blob and information quantity

`Blob` is immutable binary content with canonical declared media type and optional suffix; its physical representation may be lazy. Its raw-byte commitment is independent of those annotations. Its exact value, range, identity and storage rules are in [Blob values](#blob-values) and [Git-native storage](#blob-storage). It is not a filename or a byte-count quantity. Storage size is a numeric quantity with an information unit.

```text
payload: Blob
used: Float<GiB>
```

## Time model

`Instant` is an absolute point on the UTC timeline. `LocalDateTime` is civil time without a zone. `ZonedDateTime` is a local date-time plus an IANA time zone and resolved offset. `Duration` is elapsed time.

**ORNA-TIME-001** Telemetry timestamps SHOULD use `Instant`.

**ORNA-TIME-002** Calendar-day operations over `Instant` values MUST require or derive an explicit time zone.

Invalid:

```orna
readings | bucket_by(1.day)
```

when `readings.time` is an `Instant` and no zone can be derived.

Valid:

```orna
readings | bucket_by(1.day, zone: Europe.London)
```

**ORNA-TIME-003** Calendar-day bucketing MUST respect daylight-saving transitions; a local day MAY contain 23, 24 or 25 hours.

**ORNA-TIME-004** Fixed elapsed-time windows and calendar periods MUST be distinct operations even if their textual durations appear similar.

## Ranges

`Range<T>` is an ordinary ordered span. It does not imply non-overlap or temporal uniqueness merely because it is stored in a table.

```text
1..5
2026-01-01..2027-01-01
start..
..end
1..=5
```

**ORNA-RANGE-001** `a..b` constructs a half-open range containing values `x` for which `a <= x && x < b`.

**ORNA-RANGE-002** `a..=b` constructs an inclusive range containing values `x` for which `a <= x && x <= b`.

**ORNA-RANGE-003** Either endpoint MAY be omitted where the surrounding operation supplies a type and meaningful unbounded side.

**ORNA-RANGE-004** Range endpoints MUST have one compatible ordered type. Empty ranges are valid ordinary values.

**ORNA-RANGE-005** Ranges have a total order only when `T` has a total order: lower endpoint (unbounded first), then upper endpoint (unbounded last), then exclusivity before inclusivity at equal endpoints. This ordering is for sorting/serialization and does not imply overlap uniqueness.

**ORNA-RANGE-006** `value in range` tests membership. Range iteration/slicing behavior is supplied by the relevant iterable/index operation rather than by special grammar.

**ORNA-RANGE-007** Version 1.0 does not redefine primary-key equality as range overlap. Temporal exclusion/non-overlap constraints are represented by explicit table or cross-table assertions, not by making overlap masquerade as equality.

## Dimensions and units

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

**ORNA-UNIT-001** Unit compatibility is determined from reduced dimension exponents, rational scale and affine behaviour, not from unit name alone.

**ORNA-UNIT-002** Compatible units from attached databases interoperate when their structural definitions are equivalent.

**ORNA-UNIT-003** Aggregation preserves dimensions according to the aggregate's algebra.

**ORNA-UNIT-004** Adding quantities of incompatible dimensions is a compile-time error where types are known.

**ORNA-UNIT-005** For an affine unit `U`, an absolute value and its corresponding linear delta quantity obey point/vector algebra:

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

**ORNA-UNIT-006** Integration reduces dimensions algebraically; integrating speed over time yields length.

**ORNA-UNIT-007** Aggregation over affine absolute quantities follows this matrix:

- selection/order operations (`first`, `last`, `min`, `max`, `median`, percentile and mode) are permitted and return an absolute value;
- `sum` is invalid for absolute affine values and valid for delta values;
- non-empty `mean` is `base + mean(value - base)` for any selected base value in the input and returns an absolute value;
- `range` and standard deviation return the corresponding delta quantity;
- variance returns the square of the corresponding delta quantity;
- differences and derivatives operate on delta quantities;
- integration of an affine absolute quantity is invalid unless the caller first selects a meaningful linear reference.

**ORNA-UNIT-008** Unit suffix resolution MUST be static and unambiguous in the current import context. A unit suffix does not perform an arbitrary `From` conversion.

## Money and currencies

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

**ORNA-MONEY-001** Money MUST use exact decimal semantics.

**ORNA-MONEY-002** Two amounts with different currency parameters MUST NOT be added without an explicit data-backed conversion.

**ORNA-MONEY-003** Dimensional algebra MUST reduce an exact energy quantity such as `Decimal<kWh> * (Money<GBP> / kWh)` to `Money<GBP>`. A binary `Float` quantity MUST NOT enter an exact Money calculation without an explicit decimal conversion, scale and rounding rule.

**ORNA-MONEY-004** A currency nominal type MUST implement `Currency` with stable static properties `code: Str` and `minor_digits: Int`. A currency symbol is deliberately not part of the protocol.

**ORNA-MONEY-005** Currency conversion whose rate changes over time MUST require an effective instant or date and MUST be an explicit data-backed operation.

```orna
rates.convert(invoice.total, GBP, on: invoice.date)
```

**ORNA-MONEY-006** Minor-unit precision controls conventional display and settlement quantization, not the internal precision of intermediate calculations.

**ORNA-MONEY-007** Canonical JSON encoding of money SHOULD use a decimal string and currency code, for example `{ "amount": "12.34", "currency": "GBP" }`.

**ORNA-MONEY-008** The grammar MUST NOT provide a special `currency` declaration. That invalid declaration receives `ORNA091-E-CURRENCY` with a rewrite to a nominal type and nested `Currency` implementation.

**ORNA-MONEY-009** Formatting chooses symbols, placement, spacing, grouping and decimal punctuation from an explicit or activation-derived locale/format context. Presentation MUST NOT change money equality, storage, hashing or canonical serialization.

**ORNA-MONEY-010** `12.34.GBP` constructs `Money<GBP>` exactly and MUST NOT parse through binary `Float`.

## Inference-first static typing

The compiler infers omitted type annotations from literals, operators, calls, fields, control-flow joins, assignments, protocol selection, return paths and contextual expectations.

```orna
fn square(x) = x * x;

pub fn total(items: [LineItem]): Money<GBP> =
    items | map(item => item.price * item.quantity) | sum();
```

**ORNA-INFER-001** Omitted annotations MUST be inferred to one static type before evaluation.

**ORNA-INFER-002** Type inference MUST NOT silently fall back to a dynamic `Any` type.

**ORNA-INFER-003** Function parameter and return annotations are optional when the signature is unambiguously inferable.

**ORNA-INFER-004** Public functions SHOULD annotate externally meaningful parameter and return types, but absence of those annotations is not a syntax error when inference succeeds.

**ORNA-INFER-005** A compiler MUST expose the fully inferred exported signature through metadata, diagnostics and `sys.Function` exactly as if it had been written explicitly.

**ORNA-INFER-006** Inference MUST be independent of runtime data values and MUST produce the same signature for the same resolved source and dependency snapshot.

**ORNA-INFER-007** When inference is underconstrained or recursive constraints do not converge, the diagnostic MUST identify the smallest useful annotation site rather than demanding annotations everywhere.

**ORNA-INFER-008** Contextual numeric and collection inference MUST preserve the exactness rules of [numeric exactness](#types) and [literal forms](#lexical).

**ORNA-INFER-009** Protocol selection and nested implementations participate in inference, but overlapping implementations remain invalid.

**ORNA-INFER-010** Adding a redundant annotation MUST NOT change successful program behavior.

## Transparent aliases and nominal types

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

**ORNA-NOMINAL-001** `type Name = Existing;` declares a transparent alias with no new runtime or equality identity.

**ORNA-NOMINAL-002** `type Name { ... }` declares a nominal type distinct from every structural record with the same fields.

**ORNA-NOMINAL-003** `type Name = Base { ... }` declares a refined nominal type represented by `Base` and distinct from `Base`.

**ORNA-NOMINAL-004** Orna 1.0 has no separate `opaque` type declaration.

**ORNA-NOMINAL-005** A nominal field is private by default. Prefixing the field with `pub` exposes that field through the type's public representation.

**ORNA-NOMINAL-006** Private fields are accessible to the owning type's nested implementations and inaccessible to unrelated modules.

**ORNA-NOMINAL-007** Direct construction outside the owning type is permitted only when every supplied field is publicly constructible; otherwise callers use an exposed conversion or method.

**ORNA-NOMINAL-008** Field visibility controls representation access, not value mutability. Nominal values remain immutable.

**ORNA-NOMINAL-009** Nominal equality, order, hashing, display, presentation and codec behavior are supplied by explicit core rules or protocol implementations; structural coincidence alone does not grant them.

**ORNA-NOMINAL-010** A formatter SHOULD keep nested implementations adjacent to the type whose representation they can access.

## Refined types

A refined type attaches always-enforced assertions to a base type:

```orna
type Meter = Int {
    assert >= 0;
    assert <= 100;
}
```

The declaration is read as “a `Meter` is an `Int` for which each proposition holds”. The candidate base value is the assertion owner subject; it is not exposed as a general source variable.

**ORNA-REFINE-001** Constructing or converting into a refined type MUST evaluate every declaration assertion in source order.

**ORNA-REFINE-002** A false refined-type assertion prevents construction and fails with a safe, source-linked assertion diagnostic.

**ORNA-REFINE-003** A refined-type assertion MAY use a subjectless comparison such as `>= 0`, which elaborates to a predicate over the candidate base value.

**ORNA-REFINE-004** The removed `where self` spelling is invalid and receives `ORNA-A091-001`.

**ORNA-REFINE-005** Refined-type assertions are enforced in all build and runtime modes; they are not debug-only.

## Explicit conversions

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

**ORNA-CONVERT-001** `Target.from(source)` selects a `From<Source>` implementation nested in `Target`.

**ORNA-CONVERT-002** A nominal type MAY implement `From` for several distinct source types.

**ORNA-CONVERT-003** Two implementations whose source types overlap for the same target are invalid.

**ORNA-CONVERT-004** `From` is allowed to fail through the ordinary failure channel. Orna does not split the surface into `From` and `TryFrom`.

**ORNA-CONVERT-005** `TryFrom` is not a core 1.0 protocol and tooling SHOULD propose `From` when it recognizes the legacy design spelling.

**ORNA-CONVERT-006** Arbitrary `From` implementations are not applied implicitly. Source writes the conversion or uses a syntax whose semantics explicitly names the target, such as `12.34.GBP`.

**ORNA-CONVERT-007** A compiler MUST NOT discover or execute an implicit multi-step conversion chain.

**ORNA-CONVERT-008** Lossy, policy-dependent, locale-dependent or data-backed transformations MUST use a specifically named operation rather than a generic `From` implementation.

**ORNA-CONVERT-009** A conversion implementation may access private fields of its target because it is lexically owned by that target.

**ORNA-CONVERT-010** Conversion failure preserves the original error cause and aborts the activation unless recovered by `|?`.



## Inference boundary algorithm

### Algorithm INFER-1

1. Parse declarations without inventing dynamic types for omitted annotations.
2. Create type variables for omitted parameter, local and return types.
3. Add constraints from literals, patterns, calls, operators, assignments, fields, case arms, returns, table schemas, protocol requirements and contextual types.
4. Resolve imported/exported signatures and nested implementation candidates from the captured dependency snapshot.
5. Solve constraints to a principal type where one exists.
6. Reject overlap, unsatisfied protocol bounds, incompatible assignments and ambiguous unresolved variables.
7. For recursion, iterate strongly connected declaration components; request a targeted annotation only when no stable principal solution exists.
8. Persist the inferred public signature and failure metadata in semantic catalogue output.

**ORNA-INFER-011** Inference errors MUST show the conflicting constraints and their source spans.

**ORNA-INFER-012** Refactoring a private helper from an explicit type to an equivalent inferred type MUST NOT change exported semantic identity by itself.

**ORNA-INFER-013** Semantic diffs SHOULD distinguish an annotation-only edit from an inferred signature change.

**ORNA-INFER-014** Historical execution uses the inferred signatures captured by the historical code snapshot, not signatures re-inferred from current source.

## Nested implementation ownership

A nested implementation's target is the immediately enclosing nominal type. There is no need to repeat it:

```orna
pub type Username {
    value: Str,

    impl Display {
        fn display(self, context) = self.value;
    }
}
```

**ORNA-IMPL-002** A nested `impl P` semantically means implementation of protocol `P` for the immediately enclosing nominal type.

**ORNA-IMPL-003** Nested implementations are not allowed inside a transparent alias because the alias has no separate nominal ownership.

**ORNA-IMPL-004** A refined nominal type MAY contain nested implementations in the same body as its assertions.

**ORNA-IMPL-005** Protocol implementation lookup is based on stable semantic identities, not merely source names.

**ORNA-IMPL-006** Renaming a type through semantic rename preserves its nested implementation ownership.

## Conversion selection algorithm

### Algorithm CONVERT-1

For explicit `Target.from(source)`:

1. Resolve `Target` to one nominal target type.
2. Infer/check the source's static type `S`.
3. Find nested implementations of `From<S'>` in `Target` for which `S` equals or validly instantiates `S'`.
4. Require exactly one most-specific non-overlapping implementation; specialization itself is not supported.
5. Invoke its `from` function.
6. If invocation fails, propagate normally. If it succeeds, require the target value type.
7. Do not search for an intermediate `U` and do not chain `Target.from(U.from(source))` implicitly.

# 6. Declarations, expressions and failure {#expressions}

## Top-level declarations

Module units may declare imports, tables, functions, enums, protocols, dimensions, units, aliases, nominal/refined types, and constrained module assertions. Protocol implementations are nested in the nominal target type; currencies are ordinary types; there is no top-level `impl ... for ...` or `currency` declaration.

**ORNA-MODULE-001** Arbitrary expression statements MUST NOT appear at module top level.

**ORNA-MODULE-002** A module-scope `assert` is permitted only as the cross-table invariant form specified in [the tables chapter](#tables). It is not arbitrary module-load execution.

**ORNA-MODULE-003** Loading source parses, resolves, infers and type-checks declarations and assertion plans; it never starts streams, performs external work or mutates current table state.

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

## Functions and function values

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

**ORNA-FN-001** Calling a function MUST evaluate it at the time of the call against supplied values and the current database context.

**ORNA-FN-002** A qualified function name without call parentheses denotes the function value and MUST NOT invoke it.

```text
messages.sync      // function value
messages.sync()    // invocation
```

**ORNA-FN-003** Merely naming a function in the REPL MUST inspect the function and MUST NOT call it.

**ORNA-FN-004** A function declaration MUST NOT imply persisted materialization of its result.

**ORNA-FN-005** A function value has a statically known inferred or annotated parameter/return signature and may be stored in a local value, passed as an argument or returned from another function.

**ORNA-FN-006** An explicit return annotation follows the parameter list as `: Type`.

**ORNA-FN-007** `-> Type` is not function-return syntax in 1.0 and receives `ORNA091-E-RETURN-ARROW` with a mechanical `:` rewrite.

**ORNA-FN-008** Parameters and return types MAY omit annotations where inference succeeds.

**ORNA-FN-009** Every reachable return path and block tail contributes to return-type inference.

**ORNA-FN-010** A function's inferred failure set is metadata on the callable and is not wrapped into its successful return type.

**ORNA-FN-011** Merely declaring a function does not execute its body or its assertions.

**ORNA-FN-012** Public functions SHOULD use explicit boundary annotations when those improve API stability, documentation or diagnostics; this is guidance, not a requirement to annotate ordinary code.

## Enums and `case`

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

**ORNA-ENUM-001** An enum is nominal and closed to variants declared in its body.

**ORNA-ENUM-002** A payload-free variant is referenced as `Enum.variant`.

**ORNA-ENUM-003** A payload variant uses a record payload and is constructed with explicit named fields, for example `SyncResult.ok { count: 3 }`. Destructuring uses record-pattern semantics, including the pattern-only shorthand `SyncResult.ok { count }`; construction does not permit that shorthand.

**ORNA-ENUM-004** Variant payload field order is source-preserving for presentation and structural for matching.

**ORNA-ENUM-005** A payload-free enum MAY be a primary-key component and is encoded by its canonical variant name. An enum variant carrying a payload MUST NOT be a primary-key component in version 1.0.

**ORNA-ENUM-006** Variant identifiers SHOULD use lowercase `snake_case` for readability, but identifier case remains part of the name.

**ORNA-CASE-001** `case` is an expression and MUST be exhaustive for a statically closed scrutinee type.

**ORNA-CASE-002** Each arm has `pattern [if guard]: body` form. The colon introduces the arm body; `=>` remains anonymous-function syntax.

**ORNA-CASE-003** Arms are considered in source order; the first matching pattern whose guard evaluates to true is selected.

**ORNA-CASE-004** All reachable value-producing arm bodies MUST type-unify.

**ORNA-CASE-005** A guard is evaluated only after its pattern matches and before its body.

**ORNA-CASE-006** The `match` keyword and `pattern => body` arm surface are invalid in 1.0 and receive `ORNA091-E-MATCH`.

**ORNA-CASE-007** A processor MUST NOT interpret `case` as exception handling. It is ordinary exhaustive value branching; failures are handled by `|?`.

## Generics, protocols and nested implementations

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

**ORNA-GENERIC-001** Generic parameters are explicit in public source when a generic abstraction is intended; local type arguments may be inferred.

**ORNA-GENERIC-002** Protocol dispatch is static in version 1.0. Overlapping implementations are invalid.

**ORNA-GENERIC-003** A protocol implementation is lexically nested in the nominal target type. The current database must own that type declaration.

**ORNA-GENERIC-004** Specialization, associated types, higher-kinded types, negative implementations and runtime protocol objects are outside version 1.0.

**ORNA-GENERIC-005** A generic protocol bound uses `<T impl Protocol>`; several bounds use `+`, for example `<T impl Display + Order>`.

**ORNA-GENERIC-006** The removed colon-bound spelling `<T: Protocol>` is invalid and receives `ORNA091-E-BOUND-COLON`.

**ORNA-GENERIC-007** The removed top-level form `impl Protocol for Type` is invalid and receives `ORNA091-E-IMPL-FOR`; the implementation is moved into `Type` and omits `for Type`.

**ORNA-GENERIC-008** A protocol may declare instance functions and static properties.

**ORNA-GENERIC-009** A static protocol property is declared `static name: Type;` and implemented `static name = expression;`.

**ORNA-GENERIC-010** `static fn` is not a protocol-member category in 1.0. Behavior that does not require an instance is expressed as an ordinary function or as construction/conversion owned by the target type.

**ORNA-GENERIC-011** Every required protocol member must have exactly one compatible implementation in a conforming nested `impl` block.

**ORNA-GENERIC-012** Nested implementations may access the target type's private representation.

**ORNA-GENERIC-013** A nominal target MAY contain several nested implementations, including several different instantiations of one generic protocol such as `From<Str>` and `From<EmailHeader>`.

**ORNA-GENERIC-014** Implementation selection MUST NOT depend on source order when the target/source/protocol types are otherwise identical; such overlap is a static error.

**ORNA-GENERIC-015** A table declaration MAY contain nested protocol implementations for its nominal row type; those implementations are not stored row fields and do not affect table identity or storage.

## Control flow and local bindings

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

**ORNA-CFLOW-001** A block evaluates from left to right. A final expression without `;` is the block value; otherwise the block value is `Unit`.

**ORNA-CFLOW-002** Function receivers/arguments, list elements and record field expressions evaluate left to right in source order. `&&` and `||` short-circuit left to right.

**ORNA-CFLOW-003** `if` is an expression and all value-producing branches MUST type-unify.

**ORNA-CFLOW-004** `case` is an expression and MUST be exhaustive for closed types.

**ORNA-CFLOW-005** `let` creates a function-local binding slot containing an immutable value. Assignment may replace that slot's value when the pattern denotes one local identifier.

**ORNA-CFLOW-006** Assignment is a statement targeting an existing local `let` binding. It is not an expression and cannot occur inside a record field, argument, condition or returned value.

```orna
let count = 0;
count = count + 1;
count += 1;
```

**ORNA-CFLOW-007** `for` consumes a finite `Iterable`. An unbounded `Stream` is consumed by stream operations such as `for_each`.

**ORNA-CFLOW-008** `return` exits the current function; `break` exits the nearest loop and may carry a value; `continue` advances the nearest loop.

**ORNA-CFLOW-009** Orna has no implicit truthiness.

**ORNA-CFLOW-010** A braced control-flow expression (`if`, `case`, `for`, `while`, or `loop`) MAY be used as a statement without a trailing semicolon. When it is the final item of a block, it is the block's tail expression unless followed by `;`.

**ORNA-PARSE-002** While parsing a block, a braced control-flow expression followed by another block item is a statement; the final unsemicolonated expression immediately before `}` is the block tail. `loop` used as a non-final statement requires `;` when its value would otherwise be ambiguous.

**ORNA-CFLOW-011** The removed `var` declaration MUST NOT be treated as a second mutability system. The mechanical migration is `var name = value;` to `let name = value;`.

## Entry functions and `orna run`

**ORNA-RUN-001** `orna run <qualified-function>` MUST invoke the named public function.

**ORNA-RUN-002** `orna run` without a function name MUST invoke root `main()` if present and MUST otherwise produce a diagnostic.

**ORNA-RUN-003** A file path MUST NOT acquire special execution behavior merely because it is named `ingest/main.orna` or appears under a particular directory.

A finite program exits when complete. A program consuming an unbounded stream remains active until cancelled or the source closes.

## Anonymous functions and closures

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

**ORNA-LAMBDA-001** One unparenthesized identifier or `_` before `=>` is a one-parameter anonymous function.

**ORNA-LAMBDA-002** Zero or multiple parameters use parentheses.

**ORNA-PARSE-001** When `(` begins an expression, the parser scans to the matching `)` while respecting nested delimiters. If the next significant token is `=>`, the construct is an anonymous-function parameter list; otherwise an empty pair is Unit, a comma at the outermost parenthesis level denotes a tuple, and a nonempty comma-free pair groups one expression. Implementations may use equivalent parsing machinery, but MUST produce the same parse.

**ORNA-LAMBDA-003** Anonymous functions capture immutable lexical values. Such a value is called a closure when it captures at least one surrounding value.

**ORNA-LAMBDA-004** `|...|` is not anonymous-function syntax. `|` is successful-value pipeline application, `|?` is failure recovery and `||` is logical OR.

**ORNA-LAMBDA-005** Anonymous-function direct braced bodies follow [the lexical chapter](#lexical).

A bare named function is already a function value:

```orna
parallel([
    messages.sync,
    ledger.sync,
])
```

Use `() => ...` only when inline work or captured arguments are actually required.

## Operators, success pipelines and recovery pipelines

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

**ORNA-OP-001** A conforming parser MUST apply the precedence and associativity table above. Parentheses override it.

**ORNA-PIPE-001** `|` is the canonical successful-value pipeline operator and has no bitwise meaning.

**ORNA-PIPE-002** `value | function` is exactly `function(value)` after `value` completes successfully.

**ORNA-PIPE-003** `value | function(a, named: b)` is exactly `function(value, a, named: b)`. The piped value is inserted as argument one.

**ORNA-PIPE-004** Any callable may be a success-pipeline stage when its first parameter accepts the piped value. There is no separate pipe-function declaration or category.

**ORNA-PIPE-005** A direct anonymous function used as a pipeline stage MUST be parenthesized:

```text
rows | (row => row.name)      // valid
rows | row => row.name        // invalid
```

**ORNA-PIPE-006** Bitwise operations are complete ordinary functions supplied by `std.bits`, such as `bit_or`, `bit_and`, `bit_xor`, `bit_not`, `shift_left` and `shift_right`.

**ORNA-PIPE-007** If evaluation to the left of `|` fails, the stage to its right is not evaluated and the failure continues outward.

**ORNA-PIPE-008** `expression |? handler` invokes `handler(error)` only when `expression` fails. On success, the original value passes through without invoking the handler.

**ORNA-PIPE-009** A recovery handler's successful return type MUST unify with the successful type to its left so later pipeline stages receive one static type.

**ORNA-PIPE-010** If a recovery handler fails or explicitly calls `fail(error)`, that failure propagates normally.

**ORNA-PIPE-011** `|` and `|?` associate leftward, permitting readable success/recovery sequences:

```orna
raw
    | decode
    | validate
    |? recover_invalid_message
    | store
```

**ORNA-PIPE-012** `|?` is ordinary explicit recovery, not assertion syntax and not a stream dead-letter operation.

**ORNA-COALESCE-001** `a ?? b` evaluates `a` first. If `a` is `Some(value)`, it yields that value without evaluating `b`; otherwise it evaluates and yields `b`.

**ORNA-COALESCE-002** `??` associates rightward: `a ?? b ?? c` means `a ?? (b ?? c)`.

**ORNA-COMPARE-001** Equality and comparison operators do not chain. `a < b < c` and `a == b == c` are syntax errors; source writes `a < b && b < c` when that is intended.

**ORNA-COMPARE-002** Comparison operands evaluate left-to-right and each accepted operator yields `Bool`.

## Fields, selectors and member calls

Field/property access, declared member calls and pipeline application are distinct.

```text
contact.full_name                 // stored/computed field selector
Contact.full_name(contact)        // the same selector as a first-class function
contact | transactions            // ordinary free function application
relation.count()                  // an actual declared Relation member
GBP.code                          // static protocol property
```

**ORNA-MEMBER-001** `value.field` resolves only a field selector declared by the value's nominal/structural type.

**ORNA-MEMBER-002** Every table field generates a first-class selector named `Table.field`. Selecting `row.field` applies that selector to the row.

**ORNA-MEMBER-003** `value.member(args)` is valid only for a member or protocol operation explicitly associated with the receiver type. Importing a free function whose first parameter happens to match MUST NOT silently make it a dot method.

**ORNA-MEMBER-004** Free functions remain ordinary calls and may always be used through the pipeline when argument one is compatible.

**ORNA-MEMBER-005** Core types may define genuine associated members such as relation `count`, `first`, or codec `encode`; this does not create unrestricted UFCS.

**ORNA-MEMBER-006** `Type.static_property` resolves a static property supplied by a protocol implementation and does not require construction of a value.

## Failure propagation and recovery

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

**ORNA-ERR-001** A failure MUST propagate automatically through the current expression, function and activation until a matching `|?` recovery boundary or host boundary handles it.

**ORNA-ERR-002** Automatic propagation MUST preserve the original error value, causal chain and source-span information where available.

**ORNA-ERR-003** `fail(error_value)` abruptly completes with that error and has the bottom successful type, allowing it where any successful type is expected.

**ORNA-ERR-004** Cancellation is a distinct abrupt completion, not a normal `Error` that a broad `|?` handler may accidentally swallow.

**ORNA-ERR-005** Activation-owned resources MUST be released on return, failure, panic or cancellation. Deterministic cleanup SHOULD use scoped functions rather than user finalizers.

**ORNA-ERR-006** An unhandled failure in a database-writing activation MUST abort and roll back every Orna-controlled write in that activation.

**ORNA-ERR-007** External effects already performed before failure are not transactionally reversible; diagnostics and retry policy MUST remain honest about that boundary.

**ORNA-ERR-008** A postfix propagation operator `?` is invalid in expression position and receives `ORNA091-E-POSTFIX-QUESTION`.

**ORNA-ERR-009** There is no intrinsic `Result<T,E>`, `Ok(...)` or `Err(...)` failure surface. An unresolved use in that role receives `ORNA091-E-RESULT`. Independently declared user types and variants with these names remain ordinary declarations; this rule does not reserve their names.

**ORNA-ERR-010** Recovery uses `|?`, ordinary functions, `case` over ordinary values, or a combination of them. It does not require a parallel exception syntax.

**ORNA-ERR-011** A recovery handler executes at most once for the failure occurrence presented to that `|?` stage.

**ORNA-ERR-012** A successful recovery resumes evaluation at the next enclosing expression/pipeline point; it does not restart already completed stages.

### Algorithm FAILURE-1: expression propagation

1. Evaluate subexpressions in the order required by the containing construct.
2. If a subexpression completes successfully, continue with that value.
3. If it fails and the immediately enclosing construct is not the handling side of `|?`, skip remaining work in that construct and propagate the same failure.
4. If it is the left side of `|?`, invoke the handler exactly once with the failure value.
5. If the handler succeeds, substitute its value and continue. If it fails, propagate the handler's failure.
6. At an activation boundary, roll back Orna-controlled writes before reporting an unhandled failure.



## Completion states

Every expression evaluation has one of three completion states:

1. **success** with the expression's static value type;
2. **failure** with an `Error` value;
3. **cancellation**, which is controlled by runtime structured-concurrency semantics.

Failures are not successful values and are not encoded through an implicit enum wrapper. Optimizers and backends may use tagged internal representations, but source, type display, `sys.Function.return_type`, equality and codecs observe only the specified model.

**ORNA-FAILURE-001** A successful static return type excludes the failure channel.

**ORNA-FAILURE-002** The compiler MUST track whether reachable operations may fail sufficiently to preserve control flow, rollback and diagnostics, without requiring source-visible effect annotations in 1.0.

**ORNA-FAILURE-003** Public metadata SHOULD expose the inferred set of nominal failure types where knowable and `Error` where open-ended.

**ORNA-FAILURE-004** A broad recovery handler receives at least `Error`; a processor MAY infer a narrower nominal error type from the left expression.

**ORNA-FAILURE-005** Error values are ordinary inspectable values when explicitly received by a recovery handler, but they do not become successful results merely because they are values there.

**ORNA-FAILURE-006** Calling `fail(e)` from a recovery handler re-emits `e` unless it constructs another error deliberately.

**ORNA-FAILURE-007** Nested `|?` stages handle the nearest failure that reaches them according to left-associative pipeline evaluation.

**ORNA-FAILURE-008** A handler that returns a fallback value resumes with that value and does not rerun earlier successful/effectful stages.

**ORNA-FAILURE-009** Host boundaries record unhandled errors in `sys.Run`/`sys.Failure` as applicable and present a safe diagnostic.

**ORNA-FAILURE-010** The runtime MUST distinguish an activation failure from a preserved stream-item failure record; the latter is durable administration data produced by checkpoint policy.

## Recovery examples

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

## One persistent relational noun

**ORNA-TABLE-001** `table` MUST be the only core declaration for persistent relational data.

The following declaration kinds are not part of conforming version 1.0 syntax:

```text
log
view
store
ingest
source
on
```

## Explicit primary keys

```orna
pub table Contact(id: Str) {
    name: Str,
    emails: [Str],
}
```

Parameters before the body form the ordered primary key. Body fields are non-key columns.

**ORNA-KEY-001** A table parameter list MUST define the complete primary key in parameter order.

**ORNA-KEY-002** Primary-key fields MUST be visible as ordinary logical row fields even when a virtual/interchange row document obtains its key from a schema-aware path. Canonical persisted rows retain the complete key in the primary index.

## Composite keys

```orna
pub table Reading(sensor: Uuid, time: Instant) {
    temperature: Decimal,
    humidity: Decimal,
}
```

The logical key is `(sensor, time)`.

## Automatic keys

```orna
pub table Note {
    created: Instant,
    text: Str,
}
```

**ORNA-AUTOID-001** A table without explicit key parameters MUST receive an implicit monotonically increasing `Int` key named `id`.

**ORNA-AUTOID-002** IDs allocated through one repository instance and its current hidden allocator MUST never be reused after deletion, reset, checkout or branch switching.

**ORNA-AUTOID-003** Gaps caused by failed or abandoned allocations are valid and expected.

**ORNA-AUTOID-004** The current allocator high-water mark MUST be stored outside rewindable branch trees using a hidden Git ref or an equivalent atomic monotonic mechanism.

Suggested ref:

```text
refs/orna/ids/<table-object-id>
```

**ORNA-AUTOID-005** A committed snapshot MUST include the allocator watermark known at that snapshot for historical introspection.

**ORNA-AUTOID-006** Independent offline clones have independent allocators and MAY allocate the same integer. A merge of different rows with the same automatic ID MUST create a typed row conflict and MUST NOT silently renumber either row or its references.

Where collision-free independent creation is required, the schema declares an explicit distributed key:

```orna
pub table Event(id: Uuid = uuid7()) {
    time: Instant,
    value: Str,
}
```

## Key defaults

```orna
pub table Contact(
    id: Uuid = uuid7()
) {
    name: Str,
    emails: [Str],
}
```

**ORNA-KEYDEF-001** A key default MUST be an ordinary Orna expression evaluated once inside the insertion transaction.

**ORNA-KEYDEF-002** A supplied key MUST bypass its default expression.

**ORNA-KEYDEF-003** A generated key MUST be stored as the row's identity and MUST NOT be recomputed when referenced fields later change.

**ORNA-KEYDEF-004** Key defaults that inspect current table state are allocation logic, not deterministic pure identities. Independent branches MAY allocate colliding or different keys; semantic merge MUST report resulting conflicts honestly.

## Stored, defaulted and computed fields

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

**ORNA-FIELD-001** An insertion-time default is an ordinary expression evaluated once inside the insertion transaction. A supplied value bypasses it.

**ORNA-FIELD-002** The logical value selected for a defaulted field MUST NOT be recomputed merely because the declaration, table contents or referenced names later change.

**ORNA-FIELD-003** When a closed deterministic constant default is introduced on a table containing older rows, snapshot metadata MAY record one frozen fallback value for physically missing fields. That frozen value belongs to the field-introduction revision. Later edits to the default affect future insertions only.

**ORNA-FIELD-004** Future insertions logically store the evaluated default result. A storage profile MAY omit a value physically when it can reconstruct the row's already-frozen logical value without consulting a later declaration.

**ORNA-FIELD-005** A row-dependent default may be used for future insertions, but introducing it to existing rows requires an explicit backfill or an optional field. Orna MUST NOT create a hidden compute-on-read era for a stored field.

**ORNA-FIELD-006** `=>` declares a computed field. It is not stored as a logical row value, cannot be supplied during `insert`, and cannot be changed through `update`.

**ORNA-FIELD-007** A computed field is evaluated from the row under the current snapshot whenever observed and automatically reflects changes to its stored-field dependencies.

**ORNA-FIELD-008** A computed expression MUST be deterministic and row-local. It may read the current row and call pure helpers; it MUST NOT mutate tables, query unrelated tables, open streams, access secrets, perform I/O, or use ambient current time/randomness.

**ORNA-FIELD-009** Within a key-default or computed-field expression, each table field name is available as an immutable lexical selector of the prospective/current row, and `self` names the complete row context. A local binding may shadow a field only inside an explicitly nested block.

**ORNA-FIELD-010** Every stored/defaulted/computed field creates a selector such as `Contact.full_name`; `contact.full_name` applies it. Field dependencies appear in `sys.Dependency`.

**ORNA-FIELD-011** The planner MAY cache/materialize a computed selector, but this MUST NOT change its value, mutation rules, dependencies or serialization. Computed fields are excluded from standalone stored-row encoding unless a codec explicitly requests a derived projection.

The difference between a computed field and an ordinary function is fieldhood, not computation: declaration inside the table gives parameterless property access and row-local restrictions. Arbitrary work remains an explicitly called or piped function.

## Table-owned assertions

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

**ORNA-ASSERT-001** `assert` is the only assertion-introducing keyword in Orna 1.0.

**ORNA-ASSERT-002** Assertions are enforced in all modes and MUST NOT be removed from release builds.

**ORNA-ASSERT-003** A table assertion is lexically contained by its owning table.

**ORNA-ASSERT-004** The owner subject of a table assertion is `Relation<Row>` for the complete unpublished candidate relation.

**ORNA-ASSERT-005** The candidate relation includes every insert, update, delete and re-key pending in the current validation boundary.

**ORNA-ASSERT-006** `all_unique(selector)` returns a predicate over a relation and tests uniqueness of the selector result according to its declared equality/null policy.

**ORNA-ASSERT-007** `every(predicate)` returns a predicate over a relation and is true only when the row predicate is true for every row.

**ORNA-ASSERT-008** The exact relation-predicate library is extensible through ordinary functions; adding one does not add assertion grammar.

**ORNA-ASSERT-009** A table assertion expression MUST type-check as `Predicate<Relation<Row>>` after owner-subject elaboration.

**ORNA-ASSERT-010** Table assertions are deterministic for a fixed candidate relation and captured database snapshot.

**ORNA-ASSERT-011** A table assertion MUST NOT perform network, process, UI or arbitrary filesystem effects, use nondeterministic randomness, or observe uncaptured wall-clock time.

**ORNA-ASSERT-012** Multiple assertions are conjunctive and evaluated in source order for deterministic diagnostics.

**ORNA-ASSERT-013** A false table assertion aborts the validating boundary before the candidate relation becomes visible.

**ORNA-ASSERT-014** Writes from an assertion-aborted boundary are rolled back.

**ORNA-ASSERT-015** A table assertion MUST observe a transactionally coherent candidate relation and MUST NOT observe a partially committed state.

**ORNA-ASSERT-016** The source `assert self | all_unique(...);` is invalid as a declaration assertion and receives `ORNA-A091-002`; the fix removes `self |`.

**ORNA-ASSERT-017** The source `assert TableName | all_unique(...);` is invalid as a declaration assertion and receives `ORNA-A091-002`; the fix removes the repeated owner.

**ORNA-ASSERT-018** Dedicated table-field `unique` and `check(...)` modifiers are not 1.0 grammar. They migrate to table-owned `assert` predicates.

**ORNA-ASSERT-019** An assertion-specific `else` arm is invalid. Expected operational failure handling uses the ordinary failure/recovery model.

**ORNA-ASSERT-020** `ensure`, `check`, `fact`, `constraint`, `constraints`, dedicated `unique`, and `|!` are not aliases for `assert`.

### Algorithm ASSERT-TABLE-1: candidate-relation validation

Given table `T`, committed logical relation `C`, pending mutation set `M`, and ordered assertions `a1...an`:

1. Enter the storage engine's normal transaction or unpublished candidate-state boundary.
2. Derive `R = apply(C, M)` without publishing it.
3. Elaborate each assertion predicate against owner subject type `Relation<T>`.
4. Evaluate assertions in source order against `R`.
5. If predicate evaluation fails, abort the boundary and propagate that failure.
6. If a predicate is false, select a deterministic safe witness where feasible, emit the table-assertion diagnostic, and abort.
7. Continue the surrounding commit, merge, checkout, rewrite or publication algorithm only after every applicable assertion succeeds.
8. Publish `R` only when the entire enclosing boundary succeeds.

## Cross-table module assertions

Some invariants have no honest single-table owner. A module may declare a closed, deterministic proposition that depends on at least two distinct tables:

```orna
assert every(Invoice, invoice =>
    exists(Customer, customer => customer.id == invoice.customer_id)
);
```

A module assertion has no implicit `self` or relation subject. Its expression is an ordinary closed `Bool` over the unpublished candidate database.

**ORNA-ASSERT-021** A module assertion is valid only when its resolved dependency graph includes at least two distinct table objects.

**ORNA-ASSERT-022** A module assertion whose table dependencies all belong to one table is invalid and receives `ORNA-A091-003` with a fix to move it into that table.

**ORNA-ASSERT-023** A module assertion with no table dependency is invalid; module loading is not a compile-time execution facility.

**ORNA-ASSERT-024** A module assertion expression MUST type-check as a closed `Bool` and has no implicit owner subject.

**ORNA-ASSERT-025** Module assertions obey the same determinism and effect restrictions as table assertions.

**ORNA-ASSERT-026** A module assertion is evaluated whenever a validating boundary changes any table in its transitive dependency set.

**ORNA-ASSERT-027** Module assertions evaluate after all affected table-owned assertions and before publication of the candidate database.

**ORNA-ASSERT-028** Several applicable module assertions are ordered by owning module's stable ObjectId and then source span, providing deterministic first-failure behavior across modules.

**ORNA-ASSERT-029** A false module assertion aborts the complete candidate database boundary and rolls back all Orna-controlled writes in that boundary.

**ORNA-ASSERT-030** A cross-table assertion MUST NOT be attached arbitrarily to one table merely to avoid module syntax; ownership should reflect the proposition's dependency scope.

### Algorithm ASSERT-DATABASE-1: cross-table validation

1. Compute the unpublished candidate database after all pending writes and schema projection for the boundary.
2. Determine affected module assertions from stable dependency metadata.
3. Validate affected table assertions first.
4. Sort affected module assertions by stable owner identity and source span.
5. Evaluate each closed Boolean against the same candidate database snapshot.
6. On failure or false, abort the complete boundary and preserve a safe deterministic witness where feasible.
7. Publish table state, Git/CWD state and any coupled checkpoint only after all applicable assertions succeed.


## Permitted key values

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

**ORNA-KEY-003** `Float` MUST NOT be accepted as a primary-key component.

**ORNA-KEY-004** A key's logical type MUST NOT be restricted to the host filesystem's native filename rules.

**ORNA-KEY-005** A stored table reference MAY be a key component when the referenced table key is permitted and canonically encodable. The path encodes the referenced row key, not an embedded row.

**ORNA-KEY-006** `Range<T>` is not a special overlap-identity key in version 1.0. If used as ordinary stored data, overlapping ranges are valid. A future temporal-exclusion feature must use an explicit, separately specified table constraint rather than redefining equality.

This means v1 has no built-in declaration for a gap-permitting exclusive timeline such as employment or vehicle ownership. Such data may still be represented with an ordinary identity key plus `Range<T>` fields and validated by ordinary code, but kernel-enforced temporal exclusion is explicitly deferred rather than implementation-defined.

## Persisted rows and path projections

Keys are logical values, not host paths. New-format rows live in one ordered
row store. The virtual filesystem projects a table/key to a row directory
with `data.orna` and content fields using [VFS-1](#vfs). Legacy `key-path-v1`
loose-row rules are frozen in the compatibility archive and MUST NOT impose
case-folding/path-length restrictions on new-format table keys.

A row projection is data-only and schema-directed. Key fields are supplied by
the directory address; repeated keys in `data.orna` are rejected. Complete
interchange exports may include keys explicitly under their export profile.
No arbitrary row file is executed on load. Copying a projected row document
alone does not copy the referenced content.

## Mutations

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

**ORNA-MUT-001** Rows are values; mutations are operations on tables.

**ORNA-MUT-002** Successful mutations MUST become visible in CWD atomically at the enclosing activation boundary.

**ORNA-MUT-003** Ordinary mutations MUST NOT automatically create a human Git commit unless an explicitly enabled publication policy publishes the accepted batch.

**ORNA-MUT-004** Primary keys MUST be immutable through ordinary `update`.

**ORNA-MUT-005** An explicitly keyed table MUST provide the ordinary table operation `Table.rekey(old_key, new_key)`. It changes one row's primary key atomically within the current activation.

**ORNA-MUT-006** `rekey` MUST fail without changing CWD when the old key is absent, the new key already exists, the new key is invalid, or deferred referential validation fails.

**ORNA-MUT-007** Referential constraints are validated at activation commit. A program MAY update dependent references and re-key the target in the same activation; no invalid intermediate state is observable outside that activation.

**ORNA-MUT-008** Non-optional references use `restrict` by default. Orna v1 does not perform an implicit cascade. A caller that wants dependent references changed MUST update them explicitly in the same activation.

**ORNA-MUT-009** Re-keying a table with the implicit automatic integer key is prohibited. Such an identity is never rewritten; create a new row and delete the old row instead.

**ORNA-MUT-010** `sys.Change` and semantic diff MUST represent a successful explicit re-key as one `rekey` change containing the old and new keys, even if its physical representation is a VFS directory change or a page replacement.

**ORNA-MUT-011** A raw filesystem path rename is not authoritative evidence of semantic continuity. Unless it corresponds to a pending explicit `rekey` intent, it is interpreted as deletion plus insertion; tooling MAY suggest the explicit operation but MUST NOT silently guess.

## Stable semantic identity and rename

Persistent definitions have committed stable `ObjectId` values separate from their names and revision hashes.

**ORNA-OBJECT-001** Stable IDs MUST exist for databases, modules, tables, columns, functions, nominal types, units, currencies and pages.

**ORNA-OBJECT-002** Stable IDs MUST be stored in canonical committed metadata rather than source annotations.

**ORNA-OBJECT-003** `orna mv <old> <new>` and semantic LSP rename MUST preserve the target ObjectId, update affected source/storage mappings and leave reviewable CWD changes without committing automatically.

**ORNA-OBJECT-004** A manual unassisted rename that cannot be tied to committed identity metadata MUST be treated conservatively as delete plus create. Orna MAY offer explicit adoption but MUST NOT guess silently.

**ORNA-OBJECT-005** Retired IDs MUST NOT be reused for an unrelated definition.

## Stored table references

A table type used as a field denotes a stored reference to a row of that table:

```orna
pub table Vehicle(id: Uuid) {
    owner: directory.Contact,
}
```

**ORNA-REF-001** A stored reference contains database identity, table ObjectId, primary key and snapshot context as required; it does not embed a duplicate row.

**ORNA-REF-002** `vehicle.owner.key` is available without loading the target row. `vehicle.owner.name` resolves through the same snapshot context and may lower to a lookup or join.

**ORNA-REF-003** Non-optional references default to `restrict` on target delete or re-key. Weak/dangling references and automatic cascade are outside version 1.0.

## Filesystem projection and mutation equivalence

The VFS is an optional adapter over the same row engine. A completed valid
save of `Music/london/data.orna` performs one ordinary row replacement against
its captured version; a Blob file save replaces that field's immutable value.
There is no authoritative loose-row projection to synchronize. Filesystem
save, local row commitment and Git publication are distinct boundaries.
See EDIT-1 for durable candidates, invalid edits and editor limitations.

## Assertion categories

| Source owner | Required expression after elaboration | Implicit subject |
|---|---|---|
| Executable block | `Bool` | null |
| Refined type | `Predicate<Base>` | candidate base value |
| Table | `Predicate<Relation<Row>>` | complete candidate relation |
| Module | closed `Bool` with at least two table dependencies | null; reads candidate database |

**ORNA-ASSERT-031** An executable assertion evaluates its closed Boolean exactly once at the statement position.

**ORNA-ASSERT-032** A false executable assertion fails the activation with an `AssertionFailure` carrying the owning function, source span and safe values where available.

**ORNA-ASSERT-033** A subjectless refined comparison is semantic elaboration and MUST NOT be implemented as a textual rewrite introducing a source-visible global `self`.

**ORNA-ASSERT-034** The implicit owner subject exists only during refined/table assertion elaboration and MUST NOT leak into nested ordinary declarations, closures, exports or stored values.

**ORNA-ASSERT-035** Assertion name resolution otherwise follows ordinary lexical/module resolution.

**ORNA-ASSERT-036** An empty `assert;` is invalid and receives `ORNA-A091-011`.

**ORNA-ASSERT-037** Every assertion clause ends with `;` and a missing terminator receives `ORNA-A091-005`.

**ORNA-ASSERT-038** A declaration assertion whose predicate type is incompatible with its owner receives `ORNA-A091-004` showing expected owner subject and actual type.

**ORNA-ASSERT-039** An effectful or nondeterministic declaration assertion receives `ORNA-A091-007` identifying the forbidden effect.

**ORNA-ASSERT-040** An implementation MAY stop after the first false assertion in deterministic order unless an explicit diagnostic mode requests all independent failures.

## Validation boundaries

**ORNA-ASSERT-041** Refined assertions run whenever a base value is explicitly constructed or converted into the refined type.

**ORNA-ASSERT-042** Table and applicable module assertions run before ordinary transaction commit.

**ORNA-ASSERT-043** They also run before accepting a merge candidate that changes logical state.

**ORNA-ASSERT-044** They run before accepting checkout/reset state as current when logical state changes.

**ORNA-ASSERT-045** They run before graph publication, migration or a new physical root becomes visible.

**ORNA-ASSERT-046** Concurrent writers validate against the transactionally serialized candidate state selected by the storage engine.

**ORNA-ASSERT-047** A replayable stream checkpoint MUST NOT advance when an assertion aborts the item or batch transaction.

**ORNA-ASSERT-048** Git snapshots, indexed row pages and the embedded unpublished tail are representations of one logical state and MUST NOT bypass assertions.

**ORNA-ASSERT-049** Evaluation failure inside an assertion propagates through the normal failure model and aborts the same boundary as a false assertion.

**ORNA-ASSERT-050** Assertion validation MUST preserve transaction atomicity and MUST NOT publish a state for which only a subset of applicable assertions ran.

## Diagnostics and privacy

**ORNA-ASSERT-051** A failure diagnostic identifies the failed assertion source span and owning declaration/module.

**ORNA-ASSERT-052** A refined failure identifies the candidate value through safe presentation rules.

**ORNA-ASSERT-053** A table/cross-table diagnostic identifies the predicate and, when feasible, a deterministic witness.

**ORNA-ASSERT-054** Diagnostics MUST NOT reveal secrets or values marked non-presentable.

**ORNA-ASSERT-055** A machine-applicable rewrite SHOULD be attached when migration is unambiguous.

**ORNA-ASSERT-056** A fixer MUST NOT rewrite arbitrary pipelines or `self` uses outside a recognized declaration-assertion legacy form.

**ORNA-ASSERT-057** Assertion failure MUST NOT be encoded as a hidden stored Boolean column, synthetic row or implicit index.

**ORNA-ASSERT-058** An optimizer MAY implement `all_unique`, `every` and cross-table predicates through indexes, anti-joins or incremental plans only when results and deterministic witnesses equal the reference logical evaluation.

**ORNA-ASSERT-059** Assertion dependencies appear in `sys.Dependency` and assertion definitions in `sys.Assertion`.

**ORNA-ASSERT-060** Presentation, formatting and diagnostic witness selection MUST NOT affect equality, storage, Git hashes or canonical serialization.

## Required assertion diagnostics

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

## Relation values and bounded observation

A table expression produces `Relation<RowType>`.

```orna
directory.Contact
    | filter(c => c.name.starts_with("A"))
    | sort_by(c => c.name)
```

**ORNA-REL-001** A relation value MUST remain composable without requiring full materialization.

**ORNA-REL-002** Relation presentation MAY request a bounded window of rows without enumerating the whole relation.

A submitted relation expression has been evaluated to a relation value even when only a window is enumerated. REPL previews MUST communicate estimates visually, for example `≈42.1k rows`, rather than describing the value as “not executed”.

## Observable relation order

**ORNA-ORDER-001** A base table scan MUST be ordered by ascending canonical primary key.

**ORNA-ORDER-002** `filter` and one-to-one `map` preserve input order; `flat_map` uses input order followed by each produced value's order; `take`, `drop`, windows and `pairs` require/preserve order.

**ORNA-ORDER-003** `sort_by` establishes a stable order, `distinct` keeps the first occurrence, `union` yields left then right, grouped results use group-key order, and joins preserve left order with each right match in right-key order.

**ORNA-ORDER-004** An optimizer may reorder internally but MUST restore the declared observable order before values are observed.

## Equality and relation comparison

Values compare structurally or nominally according to their types. Decimal equality ignores representational scale. Row values compare table ObjectId, primary key, snapshot context and logical fields. Stored row references compare target database/table identities and key.

**ORNA-EQ-001** Relation-wide `==` is invalid because sequence equality and set-of-rows equality are different operations. Libraries may provide explicitly named operations such as `same_sequence` and `same_rows`.

## Reusable derived data

```orna
pub fn readings_above(limit: Decimal) =
    energy.Reading
        | filter(reading => reading.value > limit);
```

**ORNA-QUERY-001** A named live calculation MUST be an ordinary function rather than a `view` declaration.

**ORNA-QUERY-002** Calling the function evaluates its logical query against the current context unless arguments are explicitly snapshot-pinned.

**ORNA-QUERY-003** Two calls are not implicitly collapsed merely because their source spelling and arguments match. Effectful or nondeterministic calls execute independently.

**ORNA-QUERY-004** An implementation MAY reuse a previous call result only when the function is internally proven read-only and deterministic and its code, arguments, snapshot context, activation time and data dependencies have not changed. Reuse is an optimization, not a semantic guarantee.

**ORNA-QUERY-005** Writes performed earlier in the same activation participate in dependency invalidation and read-your-writes. A later call MUST observe those writes when its dependencies include the changed table.

**ORNA-QUERY-006** Actual reuse/materialization decisions SHOULD be visible through `sys.Materialization` and `orna explain` without requiring user effect annotations.

## Current code over historical data

```orna
energy.Reading.as_of(sys.snapshot("HEAD~10"))
```

resolves the current table definition and current calling code while reading compatible data from the selected commit.

**ORNA-HIST-001** Data pinning MUST NOT silently switch the entire executing program to historical code.

## Whole-program historical evaluation

```text
let old = sys.database.as_of(sys.snapshot("HEAD~10"));

old.energy.daily()
old.directory.Contact
old.sys.Table
```

The returned database snapshot object is a read-only root namespace for the historical code, schemas, rows, attached database pins, language edition, `std` commit and committed semantic metadata of that snapshot.

**ORNA-HIST-002** Calling a function through a database snapshot object invokes the historical definition.

**ORNA-HIST-003** Historical snapshot execution MUST NOT mutate current CWD, advance live checkpoints, use current secrets, open connectors or perform external effects.

**ORNA-HIST-004** Values from different snapshot contexts MUST NOT be mixed implicitly.

**ORNA-HIST-005** If required historical Unicode, time-zone, codec or edition support is unavailable, Orna reports incomplete reproducibility rather than silently substituting current semantics.

## Materialization and optimization

There is no user-visible `store` or `cache` declaration in version 1.0.

**ORNA-PLAN-001** Materialization, caching, indexing and incremental maintenance MUST NOT change logical function results.

**ORNA-PLAN-002** Planner choices MUST be introspectable through `sys.Plan`, `sys.Storage`, `sys.Materialization` and `orna explain`.

**ORNA-PLAN-003** Every optimization MUST have a correct scan/reference fallback.

Versioned planner hints are deferred until real workloads establish a useful policy vocabulary.

## Activation consistency and call reuse

An activation already captures one starting CWD generation, snapshot context and `now()` value while observing its own writes. This gives consistency; it does not imply that every function is memoized.

**ORNA-CALL-001** Two calls in one activation that are proven read-only and deterministic, have equal arguments, use the same code revision/context and observe unchanged dependencies MUST return observationally equal values.

**ORNA-CALL-002** A write, checkpoint movement, external observation or any dependency change between calls invalidates reuse. Effectful or nondeterministic calls MUST NOT be collapsed merely because their source and arguments look equal.

**ORNA-CALL-003** An implementation MAY evaluate an eligible call once and reuse the value. It MAY instead evaluate it repeatedly, provided the observable result and dependency context are identical.

**ORNA-CALL-004** When reuse/materialization occurs, `sys.Query`, `sys.Plan` or `sys.Materialization` MUST make the decision inspectable. The language does not promise a particular evaluation count for pure calls.

Example:

```orna
let before = Contact | count;
Contact.insert({ name: "Alice" });
let after = Contact | count;       // sees the write; cannot reuse `before`
```

## Dependencies and lineage

The compiler/runtime maintains exact dependency edges where statically knowable and observed edges where dynamic.

**ORNA-DEP-001** Dependencies MUST identify source and destination objects with typed `sys.Object` references.

**ORNA-DEP-002** Dependency kinds SHOULD include imports, calls, reads, writes, references, renders, uses-type and uses-unit.

**ORNA-DEP-003** Dependency information MUST be used for impact analysis, live invalidation, semantic diff, merge validation and selective testing where applicable.

## Attached databases and pinning

**ORNA-ATTACH-001** Cross-database reads MAY compose relations from consistent snapshots of each attached database.

**ORNA-ATTACH-002** Orna MUST NOT claim atomic cross-database writes when attached databases have independent transaction logs.

**ORNA-ATTACH-003** Structural unit equivalence MUST be checked across database boundaries.

**ORNA-PACKAGE-001** Version 1.0 MUST NOT require a package registry, semantic-version solver or lockfile where exact Git commit pins already determine dependency state.

**ORNA-PACKAGE-002** Checking out an historical parent snapshot MUST resolve each attached database to the commit pinned by that snapshot.

**ORNA-PACKAGE-003** `std` remains an ordinary optional attached Orna database/library except for mandatory core language and `sys` facilities.

# 9. Core operations and the standard library {#standard-library}

The language supplies a small intrinsic environment sufficient to evaluate source, query and update tables, consume streams, validate assertions and inspect `sys`. The optional `std` namespace contains ordinary, explicitly imported library code pinned to a Git snapshot. The two are not interchangeable: deleting an optional formatter or connector cannot disable database integrity.

## Resolution and versioning

Core type names, `Some`, `null`, `error`, `fail`, `now`, `uuid7` and the relational/stream operations below are available in the root intrinsic environment. Ordinary lexical declarations may shadow unqualified helpers; `sys` itself cannot be shadowed. `CWD` and `HEAD` are context-bound snapshot selectors, not wall-clock values. Evaluating `HEAD` in an unborn repository fails; it does not produce an empty synthetic commit.

`std` imports use ordinary `use` rules. `use std.prelude as _;` imports the prelude exports recorded by that pinned module; it does not import every standard module. `std.query` and `std.stream` may re-export the intrinsic operators with identical identities and behaviour. A user-defined function of the same name remains an ordinary function and is checked for the effects required by its use.

**ORNA-LIB-001** A program's library behaviour MUST be determined by its captured dependency snapshots. A historical program cannot silently substitute the currently installed `std` or current time-zone data.

**ORNA-LIB-002** Core integrity, transactions and assertions MUST work without `std`. An absent optional module produces the ordinary import diagnostic; it cannot be filled by an undocumented host library with the same spelling.

**ORNA-LIB-003** This section's portable operations MUST have the stated argument, order, failure and effect semantics when supplied under the reference-library profile. Other modules expose their exact public declarations through their pinned source and `sys`; their mere namespace name is not a claim of an unspecified portable API.

## Signature notation

In the following tables, `T`, `U` and `K` are type parameters, and `fn(T): U` is a function type. `Collection<T>` in explanatory prose means either `[T]` or `Relation<T>`; it is not a new source type. Each listed collection operation has those two explicit overloads and preserves the input container kind unless its result is scalar. `Stream<T>` operations are separate because they have waiting, ownership and checkpoint effects.

Arguments are evaluated left to right. A pipeline supplies argument one: `rows | filter(test)` calls `filter(rows, test)`. It does not first call `filter(test)` and guess a missing collection. Predicate factories explicitly listed below are genuine one-argument calls.

## Core relation operations

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

**ORNA-LIB-004** Read-only query callbacks MUST be effect-checked before optimisation. A relational plan may not hide an external side effect inside an expression that an optimiser could reorder, omit or repeat.

**ORNA-LIB-005** Failed callbacks propagate through the ordinary failure channel. A collection operator MUST NOT skip failed elements or substitute `null` unless that behaviour is requested by a separately named operation.

### Predicate factories

`Predicate<S>` is the structural callable type `fn(S): Bool`. `every(test)` constructs a predicate over `Relation<T>` that applies the row test. `all_unique(selector)` constructs a predicate over `Relation<T>` whose selected keys must have a lawful equality relation and compatible hash or order implementation. No `Float`-based default equality key is accepted.

`all_unique` compares complete selected values. For optional selected keys, `null` equals `null`; two absent values therefore violate uniqueness. To exclude missing values, write an explicit deterministic predicate over a filtered relation. There is no implicit SQL-style null exclusion. On failure, the reference witness is the earliest duplicate pair in canonical row order. An index implementation must produce that same safe witness when one is requested.

```orna
pub table Account(id: Str) {
    handle: Str,
    assert all_unique(account => account.handle);
}
```

Factories are ordinary functions, not additional keywords. The declaration supplies the candidate relation; the source does not repeat it through `self |`.

## Core table operations

Table mutation arguments are checked against a record shape derived from the table schema. This shape is a compile-time constraint and has no separately exported source type.

| Call | Result | Required behaviour |
|---|---|---|
| `T.insert(fields)` | `T` row value | Construct a complete new row, evaluate defaults once and fail on an existing key. Missing automatic/defaulted keys are allocated; missing other keys fail. |
| `T.upsert(fields)` | `T` row value | Insert when absent; otherwise replace explicitly supplied stored non-key fields while preserving omitted existing fields. On the insert path all required fields must be supplied or defaulted. |
| `T.update(key, fields)` | `T` row value | Require an existing row; patch only supplied stored non-key fields. Unknown, computed and primary-key fields fail before mutation. |
| `T.delete(key)` | `Unit` | Require an existing row and delete it from the candidate state. References are checked at commit. A missing key is an error, not a successful deletion count. |
| `T.rekey(old_key, new_key)` | `T` row value | Require an explicitly keyed table; preserve fields and semantic change identity, then validate references at commit. |

A one-component key is supplied as that component's type. A multi-component key is a tuple in declaration order. Each operation observes earlier successful writes in the same activation, but returned row values remain immutable observations. No mutation directly edits an already held row value.

**ORNA-LIB-006** Mutation return values and missing-key behaviour MUST follow this table. Errors include the table identity and safe key, without disclosing redacted values. A failing operation's tentative writes cannot leak into the rest of the activation through a recovery handler.

For the last rule, each mutation has a statement savepoint: a recoverable operation failure restores its own partial changes before control reaches `|?`. Earlier successful operations in the enclosing activation remain tentative until that activation commits or rolls back. A declaration assertion is validated at the enclosing transaction boundary and may still abort the entire activation.

## Core finite streams

`Stream.from_list(values, source_identity: Str)` constructs a finite, replayable list-backed stream for deterministic input and testing. It performs no I/O. The effective source identity combines the supplied name with the canonical typed digest of the complete immutable list. Reusing a label for different list contents therefore cannot resume against the old contents by accident.

The sole partition is `null`. Position format `orna.list.v1` stores the zero-based index of the next item as an unsigned canonical integer. Position 0 precedes the first item; position equal to the list length denotes exhaustion. Resume outside 0…length fails. The successor of item i is i+1. A provider cannot treat the index as a mutable list offset after construction.

`for_each(stream, action: fn(T): Unit)` processes one item at a time. Each callback and corresponding checkpoint movement is a separate activation, and the enclosing call returns `Unit` after exhaustion. A returned value other than Unit must be discarded explicitly in the callback body. The operator propagates cancellation and reports blocked failures through the checkpoint model rather than silently continuing.

**ORNA-LIB-007** The list-backed source MUST supply the complete declared identity, replay and preservation contract. It MUST NOT require an unprovided connector host or external account.

## Collection and query library

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

## Standard stream operators

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

## Concurrency, text and bit operations

`std.concurrent.parallel`, `race` and `timeout` follow [task ownership](#execution). `sleep(duration)` is cancellation-aware, accepts nonnegative elapsed duration and does not promise exact scheduling at the requested instant. It has a clock/waiting effect and is not permitted in declaration assertions.

`std.text` provides `trim`, `split`, `join`, `starts_with`, `ends_with`, `contains`, `replace`, `normalise`, `lower` and `upper`. Text indices and lengths are Unicode scalar positions unless the function is explicitly named for UTF-8 bytes or grapheme clusters. Casing and normalisation use the pinned Unicode data version; locale-sensitive operations require an explicit locale. `split` preserves empty fields, including trailing fields; an empty separator splits into scalars. Replace is non-overlapping left-to-right. Regex behaviour is part of an explicitly pinned regex package, not an implicit host dialect.

`std.bits` provides `bit_or`, `bit_and`, `bit_xor`, `bit_not`, `shift_left` and `shift_right` over Int using an unbounded signed two's-complement model. Shift counts must be nonnegative. Right shift is arithmetic; left shift is exact subject to resource limits. `bit_not(x) = -x - 1`. The source token `|` always remains a pipeline.

## Exact money, time and statistics

`Currency` is a core protocol because `Money<C>` must remain meaningful without `std`. `std.money` re-exports that protocol and supplies explicit rounding, allocation and formatting. Currency symbols and placement are locale data; `GBP.code` is identity metadata, not a universal display symbol.

`allocate(amount, weights)` requires nonnegative exact integer weights and a positive total weight. It rounds each share toward zero at the currency's minor-unit scale, then distributes remaining minor units by descending exact fractional remainder, ties by input index. Negative amounts allocate the magnitude and restore signs. The shares sum exactly to the original minor-unit amount; a non-minor-unit input requires an explicit rounding choice first.

`std.time` distinguishes elapsed arithmetic from calendar arithmetic. Ambiguous local times require an explicit earlier/later offset choice; nonexistent local times fail unless an explicit adjustment policy is supplied. The time-zone database edition is recorded with dependency/runtime metadata. Formatting uses a supplied immutable locale/time-zone context.

The `duration.compact`, `duration.clock`, `duration.words` and `duration.iso` formatter values expose `.format(value, context: ...)`. They are interchangeable presentation choices, not storage conversions. The ISO form is an elapsed duration format, not a way to turn a month into a fixed number of seconds.

`std.stats.mean` returns `null` for empty input. Exact inputs retain exact sums; a nonterminating exact division requires explicit scale/rounding. `median` uses total-order sorting and returns the middle value, or the explicitly rounded/exact mean of the middle two. `percentile` requires a probability in [0,1] and a named interpolation method; there is no unrecorded host default. Histogram bins are ordered, nonoverlapping half-open ranges except an explicitly closed final upper bound. Rate, derivative and integration require ordered timestamps and state their unit transformation and treatment of equal timestamps.

## Codecs and host I/O

Every codec owns `encode(value)` and `decode(input, as: T)`. Canonical Orna text and the binary value profile are defined in [canonical formats](#formats). JSON decoding is schema-directed, rejects duplicate object keys, distinguishes missing fields from explicit null, and parses numbers without first rounding through binary Float. Unknown fields fail unless an explicit decoding option permits ignoring them. JSON encoding of unsupported values fails rather than dropping fields or emitting nonstandard numeric tokens.

Base64 uses the RFC 4648 standard alphabet with `=` padding and no inserted whitespace. The decoder rejects invalid characters, noncanonical padding and unused nonzero trailing bits. URL-safe Base64 is a separately named codec profile. CSV, XML, YAML, TOML and MIME are optional codec packages; their source pins, schema mapping and selected format editions must be stated by the package before interoperability is claimed.

`std.io.fs`, `std.io.process` and `std.net` are explicit host-effect boundaries. A filesystem operation states its root/path and overwrite mode, process invocation separates executable from argument strings, and HTTP/WebSocket clients expose status, headers, body, cancellation and bounded reads. No shell command is assembled implicitly from interpolated strings. These operations are unavailable to assertions, read-only watches, presenters and historical execution. Their errors propagate normally; database rollback does not undo an external write or request.

SOPS is an optional secret provider. A stable `SecretRef` is not secret plaintext; resolving it is a separately checked host effect. Generic encoding and presentation never reveal the resolved value.

## Presentation helpers and tests

`std.ui` functions construct the core presentation tree. A helper such as `Field(label, value)` explicitly presents the typed value and returns a presentation node; it is not an implicit heterogeneous-list conversion. `Text`, `Rows`, `Cols`, `Stack`, `Details`, `Table`, `Tree`, `Code`, `Diff` and `Chart` all retain an Inspect-compatible fallback. `Button`, `Form` and `Input` carry typed action/input descriptions; only a server-created action handle can execute an event.

`std.test` supplies expectations, fixtures, property generators and isolated database helpers. Language-level `assert` remains core. A test fixture declares its source, initial snapshots, inputs, expected success/failure and external-effect policy. A temporary branch is not isolation unless the worktree-local CWD and runtime ownership are isolated too. No test may silently use the developer's local database, credentials or live services.

## Binary values and resources

The [Blob API](#blob-values) supplies native integrity and bounded I/O capabilities. `std.io.fs.read_blob` delegates to `sys.blob.capture_file`; package-resource reading delegates to `sys.blob.resource`. Both return ordinary Blob values but have different snapshot/effect boundaries. Media decoders, HTML responses, views and optional analysis remain ordinary versioned libraries. Neither a universal File table nor a devtools-owned asset primitive is required. A `std` submodule pin remains valid; editable app vendoring is explicit, not a silent override of that pin.

# 10. Activations, transactions and task ownership {#execution}

## Automatic activation transactions

A top-level activation that performs Orna table mutations owns a database transaction. Activation roots include one submitted REPL input, one direct `orna run` invocation, one page action, one stream item or configured batch callback, and one explicitly created concurrent child activation.

**ORNA-TXN-001** Orna-controlled table writes within an activation MUST commit together on successful completion.

**ORNA-TXN-002** If an error, panic or cancellation escapes the activation, all Orna-controlled writes performed by that activation MUST roll back.

**ORNA-TXN-003** Nested ordinary function calls participate in the transaction of the activation root and MUST NOT independently commit.

**ORNA-TXN-004** An unbounded stream program MUST NOT hold one transaction for its lifetime; each item or configured ordered batch callback is a distinct activation.

## Read semantics

**ORNA-TXN-005** An activation MUST capture one starting CWD generation, one snapshot context and one activation time. All reads and repeated function calls in that activation observe that context, `now()` returns the captured activation time, and the activation observes its own successful writes before commit.

**ORNA-TXN-006** Other activations MUST NOT observe partial writes from an uncommitted activation.

## External effects

```text
insert a local row
perform an external HTTP request
insert another local row
```

If the final insert fails, Orna rolls back its database writes but cannot un-send the HTTP request.

**ORNA-TXN-007** Orna MUST NOT imply that external effects are transactionally reversible.

**ORNA-TXN-008** User-visible effect annotations are not required for Orna to identify table mutations it owns. Internal effect information MAY be inferred for diagnostics, planning and introspection.


## Task ownership and termination

An **activation** is the unit of execution and database atomicity. A **task** is scheduled work owned by an activation or session. A task need not be an operating-system process. A **run** is a separately launched program with its own root owner. These identities are distinct from the process currently hosting the embedded runtime.

Ordinary synchronous calls share their caller's activation. Returning from a helper function therefore does not close the activation. Returning from an activation root does. The REPL session is a longer-lived owner: submitting another prompt does not end that session.

**ORNA-CONCUR-001** Every asynchronous child MUST have exactly one activation or session owner. On normal completion, failure, cancellation or loss of that owner, the runtime MUST request cancellation of all unfinished children and join their termination before marking the owner terminal. Normal completion does not wait for unfinished children to complete their intended jobs; it stops them and waits for cleanup.

The ownership rule is lexical at the activation boundary, not based on whether a handle remains reachable. Keeping or discarding an `InvocationHandle` does not detach, reparent or cancel its invocation. A child cannot extend its owner's lifetime by retaining a reference to it.

### Starting and awaiting

`sys.start` returns after validating the target and arguments, allocating the invocation, assigning its owner, and accepting responsibility for termination. Its default transaction mode is `separate`. `read_only` is also permitted. `inherit` fails with `sys.invoke.transaction_mode`: an asynchronously scheduled child may not concurrently use its parent's transaction. Synchronous `sys.invoke` may use `inherit`.

A direct REPL expression or direct binding whose root call is `sys.start(...)`, `sys.admin.retry_failure(...)` or `sys.admin.replay_failure(...)` assigns the invocation to the REPL session. A `sys.start` reached while executing another operation belongs to that operation's activation, including when reached through ordinary helper functions. Host APIs that submit a direct start explicitly identify the session owner; they cannot infer ownership from a later stored handle.

For example, these two REPL inputs may be submitted separately:

```orna
let job = sys.start(worker, arguments, as: Int);
sys.await(job)
```

Here `worker` and `arguments` are already resolved typed values. The invocation remains session-owned between inputs. By contrast, a function that starts work and returns without awaiting it closes its activation by cancelling the unfinished work.

`sys.await` does not transfer ownership. It returns a complete `sys.InvocationResult<T>` when the target terminates. Expiry of its timeout raises `sys.invoke.await_timeout`; it does not itself cancel the target. If that unhandled failure subsequently ends the owner, owner termination cancels its children for that separate reason. A successful result containing an optional `T` retains the outer success wrapper, so success with `null` is distinguishable from absence of a result.

### Ownership termination algorithm TASK-END-1

1. Stop admitting new children under the ending owner. A racing start either joins the recorded child set or fails before executing; it cannot become ownerless.
2. Record the owner's requested completion: success with a value, ordinary failure, or cancellation.
3. Request cancellation of every child that has not already reached a terminal state. Repeat this recursively through their owned descendants.
4. Join each child's termination and release activation-owned resources. Cancellation checks occur at bounded VM, loop, stream and host-call boundaries. An adapter that cannot interrupt an external call must isolate it and prevent it from making further Orna state changes after its lease is revoked.
5. Roll back still-open child transactions. A child's transaction that committed before cancellation remains committed. Cancellation is not history reversal.
6. Complete the owner's own transaction: validate and commit on success, or roll back on failure or cancellation. No child sharing mutable activation resources remains active at this point.
7. Publish terminal invocation and task metadata and release the owner lease. A late completion from a fenced child cannot replace the terminal result or commit another transaction.

**ORNA-TASK-001** A child MUST NOT commit after the runtime has acknowledged its cancellation and terminal state. Lease fencing and transaction generation checks MUST enforce this when execution is hosted in a different process.

**ORNA-TASK-002** A graceful REPL close MUST cancel session-owned invocations and watches without terminating unrelated sessions or independently launched runs in the same runtime.

**ORNA-TASK-003** A hard process termination MUST NOT be described as executing cleanup in the dying process. A surviving runtime cancels work after loss of its owner lease; a restarted runtime recovers or rolls back open transactions before admitting writes. Committed changes remain durable.

**ORNA-TASK-004** The runtime MUST expose bounded cancellation checkpoints and adapter cancellation limitations. A task stuck in untrusted or noncooperative code cannot retain a writable transaction after its ownership lease has been revoked. An implementation may terminate an isolated worker to enforce this rule.

### Concurrent combinators

`std.concurrent` supplies ordinary functions rather than new control-flow syntax. Callback arrays must have one common successful result type; heterogeneous results require an explicitly declared record or enum.

**ORNA-CONCUR-002** `parallel` MUST return successful child results in input order, independent of completion order.

**ORNA-CONCUR-003** Each `parallel` child is a separate activation and transaction. A successful child's committed writes MUST NOT be rolled back merely because another child fails.

**ORNA-CONCUR-004** Programs requiring one atomic database update SHOULD perform concurrent read-only or external work first, then apply the combined table mutations in the parent activation.

**ORNA-CONCUR-005** `race` MUST return the first observed successful result, cancel and join all losers, and choose the lowest input-index ordinary failure if every child fails. Simultaneous successes use the runtime's recorded completion order; no wall-clock ordering between simultaneous events is promised.

**ORNA-CONCUR-006** `timeout` MUST request cancellation and join its child before failing with its timeout diagnostic. It never returns a successful partial value.

For `parallel`, the first observed failure cancels unfinished siblings. After joining, the lowest input-index ordinary failure among failed children is the reported primary cause; other failures are attached as ordered causes. Cancellation requested by the parent remains cancellation and is not converted into an ordinary aggregate failure. An empty `parallel` returns `[]`; an empty `race` fails without scheduling work. Negative timeouts are argument errors; zero timeouts check already-terminal results before cancelling unfinished work.

## Cancellation and bounded streams

**ORNA-CANCEL-001** Cancellation is distinct from ordinary failure and MUST roll back the interrupted activation's open transaction. `|?` cannot catch it.

**ORNA-CANCEL-002** Interpreters, VMs, loops and stream operators MUST check cancellation at bounded execution points.

**ORNA-BACKPRESSURE-001** Stream channels MUST be bounded. A source that cannot be backpressured MUST declare or receive an explicit buffer, drop or fail policy before use.

**ORNA-BACKPRESSURE-002** `for_each` is sequential by default. Concurrent processing MUST be explicit, bounded and consistent with the connector's partition-order and checkpoint rules.

Cancelling a stream callback rolls back both its writes and its checkpoint advance. Stopping the surrounding run prevents admission of the next delivery. Neither action undoes previously committed deliveries.

# 11. Stream programs and consumer identity {#streams}

## Producing and consuming streams

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

## Durable consumer identity

A checkpoint belongs to a durable consumer identity, not a process ID or source line.

Version 1.0 limits one durable consumer function to one checkpointed source root.

**ORNA-CONSUMER-001** Consumer identity is derived from database identity, the stable ObjectId of the invoked public function and canonical invocation arguments.

**ORNA-CONSUMER-002** The connector additionally supplies source identity, partition identity and position-format version. Secret plaintext MUST NOT participate in identity; a stable secret reference name may.

**ORNA-CONSUMER-003** Function code hashes are revisions, not identity. Editing or semantically renaming the function continues from the same checkpoint.

**ORNA-CONSUMER-004** Deleting and recreating a function creates a new consumer identity even when the spelling is reused.

**ORNA-CONSUMER-005** A durable consumer function containing more than one checkpointed source root MUST produce a diagnostic instructing the author to extract separate named consumer functions.

Stateful map/filter logic is replayed from the source in version 1.0. Stateful windows/joins either persist their state in ordinary tables or remain restart-recomputable.

## Source identity

A connector declares whether it is finite/unbounded and replayable, and supplies stable source identity, partition, position format/version, resume behavior and failed-payload preservation/refetch behavior.

**ORNA-CONNECTOR-001** Source identity MUST be based on stable semantic configuration, not memory address, task order or AST position.

## Duplicate consumers

**ORNA-CONSUMER-006** One clone MUST prevent two local processes from concurrently owning the same consumer identity and partition.

**ORNA-CONSUMER-007** Running the same consumer against the same source from independent writable clones is outside the base version 1.0 guarantee unless the connector explicitly defines partitioning or idempotence semantics.

**ORNA-CONSUMER-008** On merge, equal checkpoints merge. Opaque divergent positions create `sys.CheckpointConflict`; Orna MUST NOT choose a seemingly greatest opaque token.

**ORNA-CONSUMER-009** Base Orna cannot detect concurrent ownership of the same consumer in independent clones before they synchronize, because each clone has independent local state and no coordinator. No cross-clone diagnostic is guaranteed. A later checkpoint conflict MAY reveal divergence, but duplicate source reads or external effects may already have occurred.


## Consumption requirements

**ORNA-STREAM-001** Importing or loading a module MUST NOT start a stream.

**ORNA-STREAM-002** A finite stream consumer returns when the source is exhausted.

**ORNA-STREAM-003** An unbounded stream consumer remains running until cancelled, failed or closed.

**ORNA-STREAM-004** Each item or configured ordered batch callback is a separate activation transaction; an unbounded consumer MUST NOT hold one transaction for its lifetime.

**ORNA-STREAM-005** Function values and anonymous functions have identical call semantics when used as stream/concurrency callbacks; wrapper lambdas MUST NOT be required where no arguments are captured.

# 12. Checkpoints and failed deliveries {#checkpoints}

A checkpoint records how far one durable consumer has committed. A failure record describes one preserved delivery that could not be processed. Neither is identified by a process ID, a display label or an incrementing retry number.

## Checkpoint identity and position

**ORNA-CP-001** Current checkpoints MUST be stored transactionally in Turso with the table writes produced by the corresponding item or batch.

**ORNA-CP-002** Publication MUST include checkpoint watermarks corresponding exactly to the included rows. It cannot publish a later watermark while omitting writes required by that watermark.

**ORNA-CP-003** `sys.Checkpoint` exposes current checkpoint state and committed historical watermarks through snapshot selection.

**ORNA-CP-004** A position-format change MUST carry a version. Incompatible formats require explicit migration, reset or adoption; bytewise or textual ordering cannot substitute for a provider-defined comparator.

A checkpoint row is keyed by `(consumer_identity, source_identity, partition)`. `sys.ConsumerIdentity` includes the database, stable consumer function identity and canonical bound arguments. Its key therefore distinguishes two runs of the same function with different source configurations. `partition = null` denotes the single unpartitioned source; it is not an empty-string partition.

`sys.CheckpointPosition` retains the provider and format identity with its opaque bytes. Generic tooling can compare two positions for equality only when those identities agree. It cannot infer that a numerically or lexicographically larger token represents later progress.

`sys.consumer_identity(function, arguments)` computes the durable identity. `sys.checkpoint(consumer_identity, source_identity, partition: ...)` retrieves the corresponding row or `null`. A bare function value, a `sys.FunctionRef`, a display name and a consumer identity are not interchangeable.

## Processing algorithm DELIVERY-1

1. Obtain the durable consumer lease and load its current checkpoint/version. A newly registered consumer obtains the provider's initial position and an initial version before reading its first delivery.
2. Fetch an item or bounded ordered batch outside the write transaction. Record its source identity, partition, delivery position and any provider-defined successor position. Preserve enough payload or protected refetch information to recover the delivery if processing fails.
3. Begin an activation against one CWD generation. Invoke the handler; all Orna table writes belong to this activation. Validate references and assertions.
4. Compare the current checkpoint with the expected version and starting position. Atomically commit handler writes and the new checkpoint. A concurrent reset or owner change fails the comparison and rolls the activation back.
5. On ordinary failure, roll back handler writes and checkpoint movement. In a separate short metadata transaction, create or update the single failure record for this delivery and pause further ordered delivery admission.
6. On cancellation, roll back the open activation. A cancellation alone does not manufacture an ordinary processing error. If a retry record already exists, release its execution lease and return it to the appropriate recoverable state after rollback is established.

**ORNA-CP-005** Activation failure MUST roll back both table writes and checkpoint advancement.

**ORNA-CP-006** An ordered consumer MUST NOT advance beyond a failed delivery unless that exact delivery is explicitly skipped with a provider-supported successor.

## One record per delivery

**ORNA-FAIL-001** Repeated processing failures for the same delivery MUST update one durable failure record. Retry number is state, not identity; failed retries MUST NOT append successor failure rows.

**ORNA-FAIL-002** A retry limit MUST NOT silently discard a delivery. The source remains paused, or an explicit bounded retry policy retries the same delivery.

**ORNA-FAIL-003** Explicit skip MUST atomically preserve the delivery's payload or protected refetch reference, update the existing failure record to `skipped` and advance the checkpoint past exactly that delivery. If no failure record exists, the same operation creates that one record; it does not create a second identity for an already recorded delivery.

**ORNA-FAIL-004** Replay of a skipped delivery runs the current compatible handler on preserved/refetched data without rewinding or advancing the live checkpoint.

**ORNA-FAIL-005** Failure identity MUST be the exact composite `(consumer_identity, source_identity, partition, position_format, position)`. A position digest is an index accelerator only; equality still verifies the complete typed position. `sys.FailureRef` encodes that natural identity and does not add a synthetic delivery ID.

The row has `version: sys.FailureVersion`, `attempt_count`, latest safe `error`, timestamps, payload/refetch metadata, checkpoint preconditions and current status. `attempt_count` counts admitted processing attempts, including an attempt interrupted before its external call completed. It is not a promise about exactly how many times an external provider observed an effect. Each durable state transition changes `version`.

Optional bounded trace records may describe individual attempts. They are not authoritative failure identities, cannot grow without a retention policy and cannot be required to locate the current blocked delivery.

## State machine

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

**ORNA-SYS-061** Successful item/batch writes and checkpoint movement MUST commit atomically.

**ORNA-SYS-062** Failure or cancellation MUST leave the committed checkpoint unchanged and roll back the interrupted handler's writes.

**ORNA-SYS-063** Checkpoint changes MUST compare both the expected checkpoint version and typed position.

**ORNA-SYS-064** Stale checkpoint preconditions MUST fail with `sys.checkpoint.conflict`. Stale failure-row versions MUST fail with `sys.failure.stale_version`. Neither failure permits handler execution or progress movement.

**ORNA-SYS-065** A replayable provider promises resumption only under its declared delivery contract; this MUST NOT be represented as exactly-once effects in arbitrary external systems.

**ORNA-SYS-066** Payload retention MUST obey the secret/redaction rules. A protected reference and digest may replace plaintext retention, but the exact source position remains part of delivery identity.

**ORNA-SYS-121** `retry_failure` MUST compare the failure's expected version/status and the blocking checkpoint precondition, acquire the sole delivery lease, and durably mark the same row `retrying` before user code executes.

**ORNA-SYS-122** A successful retry MUST atomically commit its writes, checkpoint movement and `recovered` state. A failed retry MUST keep the same failure identity, leave the checkpoint unchanged, update the diagnostic/version and return the row to `open` after rollback.

**ORNA-SYS-123** `replay_failure` MUST execute preserved nonblocking data in a separate activation. It MUST NOT move the live checkpoint; failure returns the row to `skipped`, and success commits its writes with the `replayed` transition.

**ORNA-SYS-124** `resolve_failure` MUST NOT acknowledge away a blocking `open` or `retrying` record. Only successful processing or an explicit atomic skip can clear that progress barrier.

## Administration and stale tools

Every retry, skip, replay and resolve call supplies `expected_version`. Status alone is insufficient: a row may go from `open` to `retrying` and back to `open` while a tool still holds its earlier observation.

A safe interaction reads a typed failure row, displays the intended action and submits its reference and version. If the row has changed, the tool refreshes it and asks for new operator intent where the action is destructive; it does not automatically retry a stale skip with newer preconditions.

Checkpoint reset requires a paused consumer, no in-flight callback and a matching expected version/position. A current blocking failure must first be resolved by retry or skip; reset cannot turn a blocking error into an unrecorded discard. A format-incompatible reset is rejected. The audit row records the old/new typed positions and reason, with secrets redacted.

If the runtime crashes while an attempt is marked `retrying` or `replaying`, recovery first establishes that its invocation transaction did not commit. It then fences the expired lease and returns the same record to `open` or `skipped`. If commit did occur, recovery completes the matching terminal metadata instead. It never starts a second callback while the first can still commit.

## Recovery between clones

**ORNA-CP-007** Restart on the same worktree MUST resume from its durable Turso checkpoint.

**ORNA-CP-008** A different clone MUST resume from the checkpoint present in its selected fetched snapshot, not a guessed local copy of another clone's tail.

**ORNA-CP-009** Recovery of unpublished data after machine loss requires replayable source data or a separately preserved local tail. Git replication alone does not contain unpublished state.

# 13. Inspection, display, presentation and codecs {#presentation}

## Four distinct concepts

Orna distinguishes:

1. **Inspect** - structural developer representation.
2. **Display** - friendly human text.
3. **Present** - rich typed rendering tree.
4. **Codec** - stable typed serialization/deserialization.

**ORNA-PRES-001** Implementations MUST NOT treat terminal presentation as canonical persistence encoding.

## Inspect

Every value has an automatically derived structural fallback.

```text
Money<GBP> {
    coefficient: 1234,
    scale: 2,
}
```

**ORNA-PRES-002** Inspect MUST expose type and structure, may truncate large collections, and MUST redact secret values.

## Display

Display is friendly text:

```text
£12.34
2h 14m
Alice Smith
```

**ORNA-PRES-003** Display MAY be lossy and contextual.

**ORNA-PRES-004** Changing Display MUST NOT change equality, hashing, storage, Git object IDs, query results or codec output.

## Dynamic display formatters

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

**ORNA-PRES-005** Session display overrides MUST be ephemeral unless the user explicitly saves presentation settings.

## User-defined Inspect, Display and Present

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

**ORNA-PRES-006** Every value has a host-derived, cycle-safe and bounded Inspect fallback.

**ORNA-PRES-007** Display returns human-oriented `Str`; Present returns the core typed presentation tree.

**ORNA-PRES-008** Display/Present implementations are read-only, deterministic relative to their immutable context and subject to time/instruction/output budgets. Database writes, connectors, network, filesystem, process execution, secret reveal and nondeterministic time/randomness are unavailable.

**ORNA-PRES-009** A presenter failure falls back to Inspect with a diagnostic.

## Present

Present returns a typed tree that may represent tables, objects, source, diffs, charts and custom widgets.

Default selection order:

```text
explicit presenter
-> session/page presentation override
-> type Present
-> type Display
-> derived Inspect
```

**ORNA-PRES-010** A renderer that does not recognize a rich node MUST fall back to an Inspect-compatible representation rather than fail to show the value.

## Codecs

Codecs own their operations:

```text
std.encoding.json.encode(value)
std.encoding.json.decode(text, as: Contact)

std.encoding.orna.encode(value)
std.encoding.orna.decode(text, as: Contact)
```

**ORNA-CODEC-001** A codec MUST provide typed `encode(value)` and `decode(input, as: T)` operations.

**ORNA-CODEC-002** Decode MUST produce `T` on success or fail with a `DecodeError` preserving format location/path information. It MUST NOT wrap the successful value in `Result`.

**ORNA-CODEC-003** Canonical Orna encoding MUST be deterministic, full-precision, unambiguous and round-trippable for supported values.

## Row-body encoding versus standalone encoding

A VFS row document omits its key because the key is selected by its row directory. Legacy loose-row interchange uses its recorded path codec. Neither projection is a second format-3 stored row copy.

Standalone encoding of the logical row includes the key:

```orna
{
    id: "alice-smith",
    name: "Alice Smith",
    emails: ["alice@example.com"],
}
```

**ORNA-CODEC-004** Generic value encoding of a table row MUST include all logical fields, including primary-key fields.

**ORNA-CODEC-005** Loading a row document MUST combine its admitted table schema, explicit/path key and decoded stored fields under the selected interchange/VFS context; arbitrary host paths cannot supply authority. Format-3 persistence encodes the key and stored fields directly through ORP-1.

**ORNA-CODEC-006** Exact checked-out source text MAY differ from canonical re-encoding because comments and formatting may be preserved. Exact source is obtained through `sys.File`, not by encoding the semantic value.

## Blob values {#blob-presentation}

The default structural [Blob inspector](#blob-values) presents safe length/content metadata without materialising bytes. Rich media viewing or downloading is explicit and uses a snapshot-bound, authorised Blob reader. Declared media type and optional suffix are Blob annotations; byte identity is separate. A filename is a VFS projection, not a content identity. Inspecting arbitrary source, HTML or SVG must not execute it. No devtools-specific Asset table is required by a renderer.


## Codec selection in format 3

The legacy portable text/OVB-1 descriptions are compatibility encodings, not the new annotated-Blob row representation. Format-3 values use [OVB-2/ROV-3/SOV-3](#formats). New Blob-bearing human row documents use small references through VFS-1 or explicit complete snapshot export; they never expand media as Base64. Full portable byte export is explicit and bounded/streamed.

# 14. Pages and live views {#pages}

## Pages as ordinary values

```text
A page function returns a Page value.
The Page's view callback returns a Present tree.
An action callback performs one ordinary activation.
```

**ORNA-PAGE-001** Pages MUST be ordinary values returned by functions, not a separate component declaration grammar.

**ORNA-PAGE-002** Widgets and layouts MUST be ordinary values that can be composed by normal functions.

## Watch model

Every connected page root is a watch over a typed value expression.

```text
CWD/system change
-> dependency graph finds affected watches
-> re-evaluate affected expression/subtree
-> compare old/new presentation trees
-> emit contextual delta
```

**ORNA-LIVE-001** The same watch and delta mechanism MUST support tables, charts, text, maps, Git views, runtime views and custom widgets.

**ORNA-LIVE-002** If a fine-grained delta cannot be derived, the runtime MUST fall back to replacing the nearest stable presentation subtree. There MUST be no data type for which live refresh is impossible merely because a specialized delta is absent.

## Stable presentation identity

- record children are keyed by field name;
- relation rows are keyed by primary key;
- UI nodes use explicit keys when supplied and otherwise a stable call-site-derived key;
- unkeyed lists use position and may require subtree replacement.

**ORNA-LIVE-003** A renderer MUST apply deltas in sequence order.

**ORNA-LIVE-004** A client detecting a missing base revision MUST request or accept a complete resynchronization snapshot.

## Live transport and programmable clients

The required live transport profile is the WebSocket subprotocol `orna.present.v1` with deterministic CBOR messages. This is implementation terminology and is absent from normal user-facing output.

**ORNA-WIRE-001** Every watch begins with a complete Present-tree snapshot. Deltas are an optimization; replacement of the nearest stable subtree, including the root, is the universal correctness fallback.

**ORNA-WIRE-002** Each watch has a server-issued stable identifier and a monotonically increasing revision. A delta names its base revision and new revision. A client MUST apply the complete ordered patch atomically only when the base equals its current revision; otherwise it MUST discard the patch and obtain a complete snapshot.

**ORNA-WIRE-003** The patch vocabulary is exactly `add`, `remove`, `replace` and `move`. Paths are typed sequences of record fields, relation keys and list indexes. A root replacement uses the empty path.

**ORNA-WIRE-004** Record children use field identity, relation rows use table `ObjectId` plus primary key, explicitly keyed UI children use that key, and unkeyed lists use position. Missing stable identity affects efficiency only; replacement MUST remain correct for every value.

**ORNA-WIRE-005** Connection queues, decoded message size, collection size, nesting depth and decompressed bytes MUST be bounded. A slow client MAY skip intermediate visual states and receive the newest complete snapshot. Database rows are not lost by visual coalescing.

**ORNA-WIRE-006** Reconnection does not require retained delta history. The client resubscribes and receives a complete current snapshot.

### Page actions

Normal page controls use page-produced action handles. An event contains the watch, page revision, action handle, request identity and typed input; it never relies on source text copied from the rendered page.

**ORNA-WIRE-007** A stale or unknown action handle MUST NOT execute. The host returns the current snapshot or a user-readable stale-view result.

**ORNA-WIRE-008** One action is one normal activation transaction. Successful writes commit together and invalidate watches; an escaping error or cancellation rolls them back.

### Explicit remote Orna evaluation

A trusted programmable client, browser developer console, LLM or remote CLI MAY intentionally submit Orna source.

**ORNA-EVAL-001** Source is executable only inside an explicit `eval` or `watch` operation. Ordinary strings, fields, page events and protocol values MUST never be implicitly parsed as Orna.

**ORNA-EVAL-002** The host MUST parse, resolve, type-check and execute submitted source with the same language implementation, module graph, activation semantics, diagnostics and presentation system as the local REPL. A client-supplied AST, bytecode or query plan is never trusted as authoritative.

**ORNA-EVAL-003** `eval` accepts one REPL input and may read or mutate the served clone's CWD. It is one activation transaction.

**ORNA-EVAL-004** `watch` accepts an expression and remains live through the universal watch mechanism. A watch expression may read tables and `sys`, call deterministic read-only functions and use activation time. It MUST NOT mutate tables, reveal secrets, open connectors, perform network/filesystem/process operations or use randomness. The host derives this from the resolved call graph; no user effect annotation is required.

**ORNA-EVAL-005** A time-dependent watch such as `now() - 1.min` is permitted and records an explicit clock dependency so it is reevaluated at the required boundary.

**ORNA-EVAL-006** Remote REPL sessions use the same ephemeral-module model as the local REPL: imports, bindings and declared helper functions persist for the session, and wildcard imports follow ordinary language rules.

### Request recovery

**ORNA-EVAL-007** Every mutating remote action or `eval` carries a client-generated 128-bit request identity. The host binds that identity to a canonical fingerprint of the operation.

**ORNA-EVAL-008** A request's terminal claim, successful Orna-controlled writes and compact terminal outcome MUST commit together. Durable admission reservations are distinct; [REQUEST-1](#protocol) defines crash and uncertain-external-effect handling. A matching completed identity returns its recorded outcome rather than executing again.

**ORNA-EVAL-009** Reusing a request identity for different source, action or typed input MUST be rejected.

**ORNA-EVAL-010** The compact request identity/fingerprint/outcome record is durable database runtime metadata and remains reserved. Rich result presentation may be discarded according to documented retention; loss of the rich result MUST NOT permit re-execution under the same identity.

**ORNA-EVAL-011** The deduplication guarantee covers Orna-controlled database writes only. External effects cannot be made transactional. Clients MUST NOT automatically retry a mutating evaluation after uncertain delivery; they recover its recorded status or inspect current state.

### Wire messages and values

Client operations are `subscribe`, `unsubscribe`, `resync`, `event`, `eval`, `watch`, `cancel` and `request_status`. Host operations are `snapshot`, `delta`, `result`, `diagnostic` and `request_status_result`. WebSocket control frames provide connection close, ping and pong.

**ORNA-WIRE-009** The binary envelope, message fields, deterministic CBOR rules and exact encodings for `Decimal`, `Money`, UUID/ObjectId, dates, instants, duration, quantities, enums, typed records, references, diagnostics and Present nodes are defined in `profiles/live-protocol.md` and form part of repository/server conformance.

**ORNA-WIRE-010** Secret values are not wire-encodable. A redacted secret handle may be presented only as non-revealing metadata.

**ORNA-WIRE-011** WebSocket compression is optional and cannot change semantics. Normal same-origin checks and the trusted authenticated VPN/SSH/reverse-proxy boundary apply; core Orna does not add a grants database.

**ORNA-WIRE-012** Protocol failures are rendered normally as user concepts such as “the live view was refreshed” or “this operation may already have completed.” Raw CBOR, revision or framing details appear only in explicit developer inspection.

A production server-profile claim requires independent client/server interoperability, loss/reorder/reconnect simulation, malformed-input fuzzing, bounded slow-client tests and equality with the reference vectors. These are evidence obligations for an implementation, not unresolved semantics.

## Minimal interfaces and large values

The [minimal devtools profile](#source-apps) is a delivery slice, not a replacement of the live system. Static results and manual refresh do not imply live queries. Large Blob contents are not substituted with unregistered reference tags in the protocol; render safe metadata and use an explicitly authorised byte response. Local drafts and server revisions remain distinct, and a captured value does not become a reactive slot.

# 15. The system model {#system}

## Role and namespace

`sys` is the implementation-provided model of the attached database and its execution. Unlike [the standard library](#standard-library), it is not an importable replacement package. Its rows and results are ordinary typed values; its relation handles are read-only query sources.

Use `sys.catalog` for declarations, `sys.history` and `sys.git` for retained history, `sys.rt` for live execution, `sys.storage` for physical placement, and `sys.admin` for explicit state changes. The [complete reference](#system-reference) lists every member, field, enum and signature. The [administrative contract](#administration) defines mutation preconditions.

The familiar query form works for inspection:

```orna
sys.catalog.objects
    | map(object => object.qualified_name)
```

This example requires only a database with catalogue metadata. It returns semantic object names, not executable function values. As with application relations, an explicit sort is needed when output order matters.

**ORNA-SYS-001** Structural references within `sys` MUST use typed references where possible, not unstructured path strings.

**ORNA-SYS-002** Every Orna 1.0 implementation MUST provide the complete portable `sys` surface applicable to each conformance class it claims.

**ORNA-SYS-003** User source MUST NOT declare, replace, shadow or monkey-patch the root `sys` namespace or any portable member specified in this chapter.

**ORNA-SYS-004** A vendor extension under `sys.vendor.<vendor>` MUST NOT change portable row identity, field meaning, ordering, failure codes, transaction boundaries or redaction rules.

**ORNA-SYS-005** The unrecognised member `sys.runtime` MUST produce `ORNA100-E-SYS-RUNTIME` with `sys.rt` as the suggested spelling. It is not an alias.

**ORNA-SYS-010** Portable names are case-sensitive. `sys.Table`, `sys.catalog.tables` and `sys.table` are not interchangeable spellings unless this specification explicitly defines them.

## Compatibility and observations

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

**ORNA-SYS-006** A conforming runtime MUST expose the compatibility fields of `sys.RuntimeInfo` in the [system reference](#system-reference), including exact language, system API, codec, storage and protocol coordinates; it MUST NOT claim a coordinate while omitting its required surface.

**ORNA-SYS-007** Unknown additive fields from a newer compatible `sys` minor version MUST be preserved or ignored safely according to the consumer's decoding mode; they MUST NOT be interpreted as a known field with a different meaning.

**ORNA-SYS-008** A consumer requiring an unavailable `sys` coordinate or profile MUST fail before performing writes.

**ORNA-SYS-009** A grouped catalogue/history handle and its canonical PascalCase relation MUST identify the same rows at the same snapshot. A `sys.rt` handle is the current-runtime live subset of its canonical relation, not a promise that all retained historical rows are live.

**ORNA-SYS-011** A historical system query MUST be snapshot-pinned and MUST NOT silently resolve any referenced object against a newer CWD or `HEAD`.

**ORNA-SYS-012** A historical durable-system query MUST return exactly the observations retained by its snapshot. Known absence is an empty relation; pruned, missing or unhydrated data MUST be reported as unavailable, never substituted with observed emptiness.

**ORNA-SYS-013** A live `sys.rt` relation MUST NOT accept `as_of`. The implementation MUST direct callers to the corresponding durable relation when one exists.

**ORNA-SYS-014** CWD system relations MAY overlay newer local durable observations from Turso, but each row MUST identify whether it is committed, local-only or live-derived.

**ORNA-SYS-015** A historical row that states a run was `running` records the state observed at that snapshot; it MUST NOT imply that the process remains alive.

**ORNA-SYS-022** `sys.database.cwd` MUST remain stable for one evaluation snapshot and MUST change only at a defined transaction/snapshot boundary.

**ORNA-SYS-023** `sys.database.writable` reports whether the current attachment permits Orna-controlled writes; it does not assert that every later write will succeed.

**ORNA-SYS-024** Every function activation MUST observe one stable `sys.current.snapshot` unless it explicitly opens a nested operation whose API documents a new snapshot.

**ORNA-SYS-027** `sys.rt.id` identifies the current local owner/runtime generation and MUST change after an owner restart that invalidates prior live handles.

**ORNA-SYS-028** `sys.rt` MUST be available to every Orna command that opens a database, including direct embedded commands and commands temporarily coordinated through a local owner process.

**ORNA-SYS-029** `orna serve` MUST NOT be treated as the unique or authoritative `sys.rt`; it is one optional client/host mode over the same embedded-first database model.

**ORNA-SYS-030** Reads from `sys.rt` MUST be internally consistent to one observation instant or expose an `observed_at` field allowing the caller to detect a mixed observation.

## Identity, references and context

An object identity answers “which declaration?”; a revision identity answers “which immutable definition?”; a snapshot answers “which database state?”. A reference keeps all context necessary to resolve its target. Renaming a declaration does not casually turn it into an unrelated object.

Every canonical system row has a read-only `.reference`. The reference is not the row itself and is not a mutable proxy. Ordinary source obtains it explicitly. A `sys.FunctionRef` is likewise not a closure and a `sys.ConsumerIdentity` is not a function name.

`sys.current` describes the current activation: snapshot, optional transaction/invocation/run/session/client, locale, timezone, trace and cancellation context. Optional fields use `null` for absence; no fabricated zero identifier stands for “not present”. Singleton views are immutable observations, refreshed only at specified boundaries.

[Snapshot encoding](#formats) distinguishes committed snapshots from exact CWD generations. Serialising a live handle produces a non-operational description. It does not grant the ability to resume an old task after a process restart.

**ORNA-SYS-016** Opaque system identifiers MUST compare by the identity they name, MUST have deterministic canonical encoding and MUST NOT be implicitly interchangeable with `Str`, `Uuid` or another identifier type.

**ORNA-SYS-017** A stable `sys.ObjectId` identifies one semantic object across ordinary edits and semantic renames; a `sys.RevisionId` identifies one immutable revision of that object.

**ORNA-SYS-018** Reusing an identifier for a semantically unrelated object is repository corruption and MUST be diagnosed by verification before publication.

**ORNA-SYS-019** Dereferencing a snapshot-pinned reference MUST use its pinned snapshot unless the caller explicitly asks to re-resolve by stable object identity in another snapshot.

**ORNA-SYS-020** A live handle presented to a different runtime owner MUST fail with `sys.handle.foreign_runtime`; it MUST NOT be treated as an identifier for a coincidentally equal live object.

**ORNA-SYS-021** Canonical serialization of a live handle MUST mark it non-resumable. Decoding it in a later process yields descriptive data, not an operational handle.

**ORNA-SYS-025** Child calls inherit locale, timezone, trace and cancellation context unless an explicit API parameter replaces one of them.

**ORNA-SYS-026** `sys.current.transaction` is present only when the activation participates in an Orna transaction. Its absence MUST NOT be represented by a fabricated zero identifier.

**ORNA-SYS-111** Every canonical system-relation row MUST expose a read-only `reference` property whose type is the generated alias for that row type.

**ORNA-SYS-112** A row reference MUST encode or otherwise preserve the row's natural key and resolution context; for a naturally keyed row such as `sys.Checkpoint` or `sys.Failure`, it MUST NOT introduce a synthetic identifier.

**ORNA-SYS-113** A row value MUST NOT be implicitly converted to a row reference or vice versa. Source uses `row.reference` to obtain a reference and an applicable relation/function to dereference it.

**ORNA-SYS-114** Possession of a reference MUST NOT bypass visibility, retention, redaction, trust, transaction or administrative-precondition checks.

**ORNA-SYS-135** Source spans and maps MUST retain their pinned file/snapshot context and canonical UTF-8 coordinates through diagnostics, source lookup, blame and presentation.

**ORNA-SYS-136** Snapshot selection is represented by explicit overloads for `SnapshotRef`, `CommitRef`, `BranchRef`, `TagRef`, `GitOid` and `Str`; Orna 1.0 defines no implicit `SnapshotLike` union or coercion type.

**ORNA-SYS-137** Singleton view values MUST be immutable observations. Administrative state MUST NOT be changed by field assignment or by mutating a collection obtained from a view.

## Catalogue and dependency graph

The catalogue records the fully resolved meaning of source: modules, objects, revisions, fields, parameters, return types, protocols, implementations, table keys, assertions, dimensions and currencies. Inference changes how much users write, not how much metadata implementers publish.

Source identity and semantic identity are separate. Exact source bytes include formatting; semantic hashes omit irrelevant formatting but include all observable declaration meaning. A catalogue row states whether an annotation was authored or inferred.

Dependency edges record their kind and confidence. A table assertion's dependencies are the relations it validates, not only the tables mentioned by spelling in its immediate expression. Dynamic dependencies discovered while evaluating a query are attributed to that activation.

Traversal uses a visited set keyed by object identity and snapshot. `direct` visits one edge layer. `transitive` traverses until no new objects remain, retains the shortest discovered depth, and breaks equal-depth ordering by canonical object identity and edge kind. It never loops on recursive functions or mutually referring types. Exact edge categories and fields are in `sys.Dependency`.

**ORNA-SYS-031** A state transition MUST follow the state machine specified for its row type; direct assignment of a state enum is not an administrative API.

**ORNA-SYS-032** A terminal invocation or run MUST NOT become non-terminal under the same execution identity; another execution has a new invocation/run identity. A durable failed-delivery identity follows the separate, versioned [delivery state machine](#checkpoints) and is preserved across attempts.

**ORNA-SYS-033** `qualified_name` is the name at the row's snapshot, not a timeless property of the stable object identity.

**ORNA-SYS-034** `semantic_hash` MUST exclude non-semantic formatting while `source_hash`, when present, identifies exact retained source bytes.

**ORNA-SYS-036** A generated definition MUST identify its generator/provenance where retained and MUST NOT be misreported as authored source.

**ORNA-SYS-037** Inferred types and inferred failure sets MUST be published in catalogue metadata exactly as if they had been written explicitly.

**ORNA-SYS-038** Catalogue metadata MUST retain whether an annotation was explicit so semantic tools can distinguish an annotation edit from an actual signature change.

**ORNA-SYS-039** Effect metadata MUST be a conservative superset of effects the function may perform; it MUST NOT omit a possible write, external effect or failure merely because an optimiser removed it in one build.

**ORNA-SYS-040** `sys.Table.assertions` MUST include every owner-scoped table assertion in source order and MUST expose the complete relation dependencies used for validation.

**ORNA-SYS-041** `sys.Assertion` MUST represent the contextual `assert` declarations specified by [assertions](#tables). A processor MUST NOT invent separate portable constraint/check/ensure/fact object kinds.

**ORNA-SYS-042** A table row count marked inexact MUST never be used as if it were an exact integrity fact.

**ORNA-SYS-043** Dependency traversal MUST be cycle-safe and deterministic.

**ORNA-SYS-044** A statically proven edge MUST use `exact` confidence. A conservative dynamic edge MAY use `possible`; tooling MUST NOT present `possible` as a proven call or write.

**ORNA-SYS-045** `sys.dependencies` and `sys.dependents` MUST preserve edge kind and source evidence rather than return only a set of names.

**ORNA-SYS-046** Diagnostic codes, primary spans and structured causes MUST remain available independently of a particular terminal or UI rendering.

## Sources, diagnostics and redaction {#system-redaction}

`sys.source` returns an authored or generated `sys.SourceDocument` pinned to its file and snapshot. `sys.describe` returns structured metadata. Neither is a request to reveal credentials, arbitrary host paths or unretained historical source.

Coordinates use zero-based UTF-8 byte offsets and half-open `[start, end)` spans. Lines and columns presented to humans are derived from those bytes; tab width is a renderer decision, not a source-map coordinate. Generated source identifies its generator when that information is retained.

A diagnostic has a stable code, severity, safe message, source spans, optional notes and causal references. Localisation changes the presentation, not the code or causal identity. Redaction happens before a value crosses a system, trace or codec boundary. A protected value retains a safe explicit marker; it is not replaced with plausible fake text.

**ORNA-SYS-035** Source paths returned outside explicitly trusted developer inspection MUST be repository-relative or redacted according to [redaction rules](#system-redaction).

**ORNA-SYS-047** Rendering a diagnostic MUST NOT mutate it, change its code or alter the underlying failure identity.

**ORNA-SYS-048** Secret values, decrypted source fragments, authorization material and unredacted connector payloads MUST NOT appear in diagnostic `message`, labels, causes, help, structured data, traces or presentation fallbacks.

**ORNA-SYS-049** A Git commit row and a semantic snapshot row MUST remain distinguishable even when they correspond one-to-one.

**ORNA-SYS-050** Hidden Orna refs MUST be visible through `sys.Ref.hidden` in explicit developer inspection and MUST remain valid ordinary Git refs.

**ORNA-SYS-051** A semantic diff MUST classify rename and rekey when identity evidence proves them; otherwise it MUST report conservative delete/add rather than invent identity.

**ORNA-SYS-052** Missing promisor objects MUST be represented explicitly. Introspection MUST NOT fabricate unavailable source, rows or statistics.

**ORNA-SYS-070** Storage locations and host paths are sensitive metadata and MUST follow the path-redaction rules.

**ORNA-SYS-071** `sys.Secret` MUST never expose decrypted secret bytes, derived authorization headers or a value formatter that reveals them.

**ORNA-SYS-096** A portable failure code MUST retain its meaning throughout the 1.x compatibility line.

**ORNA-SYS-097** Implementations MAY add structured fields and vendor causes, but MUST NOT replace a required portable primary code with a vendor-only code.

**ORNA-SYS-098** Failure messages may be localized; code, structured fields and causal identity remain stable.

**ORNA-SYS-099** Redaction MUST occur at the system-value boundary and MUST NOT depend solely on a renderer remembering to hide a value.

**ORNA-SYS-100** A redacted value MUST preserve safe type/identity metadata and an explicit redaction marker; it MUST NOT substitute plausible fake plaintext.

**ORNA-SYS-101** Canonical codecs MUST refuse to encode a protected value in reveal mode unless an explicitly trusted API outside generic `sys` grants that operation. No portable generic reveal API exists in 1.0.

**ORNA-SYS-102** Hashes of low-entropy secret values MUST be treated as sensitive. Domain separation alone does not make a dictionary-attackable hash safe; a public identifier MUST NOT reveal such a hash.

## History, plans and storage

`sys.snapshot` resolves a supported selector to a pinned `sys.SnapshotRef`. A string selector is resolved once; moving its branch later does not change the returned reference. Root analysis functions read metadata without switching branches or creating commits.

`sys.diff` compares semantic snapshots. `sys.changes` describes changes within the selected context. A `sys.DiffEntry` wraps its `change`; the object belongs to `entry.change.object`, not an invented flattened field. `sys.Commit` has distinct `authored_at` and `committed_at` fields.

`sys.explain` returns a plan reference with structured nodes and dependency metadata. A `sys.Query` can refer to a plan; a plan does not invent an inverse `.query` field. An optimiser may change the plan while preserving results and observable ordering.

Storage metadata describes the single ORP-1 row store, its schema/root generations and measured availability. Physical maintenance preserves logical rows. Placement preferences and editable/compact rewrites are retired, not hidden settings. [Storage](#storage) defines the current profile.

**ORNA-SYS-053** Explain output MUST be structured `sys.Plan` data. Human-readable tables or trees are `Present` renderings and are not the semantic result.

**ORNA-SYS-054** Actual execution counters MUST be absent when not measured; they MUST NOT be populated with estimates while marked actual.

**ORNA-SYS-055** A plan is snapshot-specific and MUST NOT be reused against a different schema revision without revalidation.

**ORNA-SYS-056** Every reflective `sys.invoke` or `sys.start` call MUST create an invocation identity before user code begins.

**ORNA-SYS-057** An unhandled ordinary failure MUST mark the invocation failed and roll back its Orna-controlled transaction before the failure is reported as terminal.

**ORNA-SYS-058** Cancellation MUST be recorded separately from ordinary failure and MUST bypass `|?` recovery inside the cancelled activation.

**ORNA-SYS-059** A run's `live` field is meaningful only for the current observation and MUST be false or absent in a purely historical projection.

**ORNA-SYS-060** Trace retention MAY be bounded, but the absence of discarded trace detail MUST be explicit and MUST NOT change invocation outcome.

**ORNA-SYS-067** Physical storage rows are introspection data and MUST NOT change the logical contents, ordering, equality or serialization of table rows.

**ORNA-SYS-068** A stored page MUST bind schema, logical key interval, generation and native object identity sufficiently to detect stale, overlapping or mismatched publication.

**ORNA-SYS-069** Missing lazily hydrated objects MUST be exposed as unavailable; a query may hydrate them according to policy but MUST NOT silently substitute incomplete results.

**ORNA-SYS-072** Build metadata MUST identify the complete source snapshot and compatibility coordinates used to produce an artifact.

**ORNA-SYS-073** A specification fixture listed as planned evidence MUST NOT be represented as passed unless an implementation actually executed it.

**ORNA-SYS-074** Every read/analysis function MUST return a normal typed value or relation that can be queried, encoded where supported and presented independently.

**ORNA-SYS-075** A root analysis function MUST NOT change checkout, refs, table contents, checkpoints, process state or retained failure state.

**ORNA-SYS-076** Name resolution and history functions MUST respect visibility and source redaction even in a trusted deployment.

**ORNA-SYS-115** `sys.Storage.profile` MUST identify the one recorded row profile; no new-format future-placement preference is exposed.

**ORNA-SYS-116** New-format admission MUST reject retired storage-preference functions rather than select a hidden legacy writer.

**ORNA-SYS-117** Physical maintenance MUST preserve every logical key/value, ordering and annotated Blob meaning; semantic row diff MUST be empty.

**ORNA-SYS-118** VFS path limits MUST be handled through reversible projection or explicit view errors, not by switching a table storage mode.

**ORNA-SYS-119** A maintenance stale-generation collision MUST preserve the prior authoritative root and report sys.storage.rewrite_conflict.

**ORNA-SYS-120** `sys.storage` is a grouping namespace, not a callable selector. Portable source MUST use `sys.Storage`, `sys.storage.objects` and the explicit `sys.admin` storage functions.

## Reflective invocation

Reflection is typed invocation, not source evaluation. The result witness `as: T` selects a typed boundary; `sys.Value` is an explicit type-preserving envelope, not an inference fallback or an implicit conversion chain.

A `sys.ArgumentMap` accepts named arguments at the reflective call boundary, retains each exact originating type, rejects duplicate names, and orders names by canonical UTF-8 for identity hashing. The normal source record is admitted here only by the explicit reflective parameter contract; arbitrary records do not silently become `sys.Value` elsewhere.

### Algorithm INVOKE-1

1. Resolve the `sys.FunctionRef` in its pinned snapshot. If `at` is `null`, use that pin—not the caller's current CWD. An explicit `at` must identify the same pinned snapshot; otherwise fail with `sys.invoke.snapshot_mismatch`. To select another revision, explicitly resolve the function in that snapshot before invoking it.
2. Check visibility and that the declaration is callable. Reject unresolved generic arguments and bound values whose static types do not match the signature.
3. Bind each named argument once. Apply declared defaults in the function's pinned context, reject unknown/missing names, and validate the explicit result witness before any target effect.
4. Select the documented execution mode. An inherited transaction is permitted only when its snapshot agrees with the target and the operation is synchronous. An asynchronous start uses a separate transaction or read-only mode, never a transaction whose owner can complete before it.
5. Capture locale, timezone, trace and cancellation ownership. Compute the identity tuple from function identity, revision, snapshot, typed bound arguments including defaults, expected return type, mode and relevant context.
6. Atomically admit the idempotency key, where supplied. A retained entry with a different tuple fails; an identical terminal entry returns its retained outcome. An identical active entry refers to that same operation, not a second execution. Missing/pruned outcome information is not evidence that re-execution is safe.
7. Create the invocation observation, run the target and record one terminal outcome. Commit owned Orna writes only on successful completion; otherwise roll them back. Follow [child termination](#execution) before declaring the owner terminal.
8. Publish a redacted result/diagnostic and retained trace state. A crash after an external side effect does not permit a false exactly-once guarantee.

`sys.start` returns a typed handle. `sys.await` returns one complete `sys.InvocationResult<T>` or raises `sys.invoke.await_timeout`; a timeout does not cancel or detach the target. `sys.cancel` acts on an invocation handle, not a durable-run reference. [Task ownership](#execution) determines whether an invocation belongs to an operation or a REPL session.

### Result invariants

A successful `sys.InvocationResult<T>` has a present value and no ordinary failure. A failed result has a failure and no value. A cancelled or orphaned result has no successful value. It may carry a diagnostic explaining termination, but that diagnostic does not turn cancellation into an ordinary catchable failure inside the cancelled target. The result never represents a running or timed-out target. A timed-out wait returns no partial result.

An `Option<T>` result may itself be `null`. The outer presence flag in the invocation result must preserve that distinction; `Some(null)` is a successful optional result, not an absent result. [Canonical values](#formats) preserve nested options.

**ORNA-SYS-077** `sys.invoke` MUST NOT evaluate arbitrary source text or bypass the parser, resolver, type checker, visibility rules, effect rules or transaction model.

**ORNA-SYS-078** An argument envelope MUST NOT allow a value to masquerade as another static type merely because its data representation is compatible.

**ORNA-SYS-079** An idempotency key binds to function identity, exact revision, snapshot, expected result type, canonical typed arguments, invocation mode and relevant context. Reuse for a different tuple MUST fail with `sys.invoke.idempotency_mismatch`.

**ORNA-SYS-080** Returning a cached idempotent success is permitted only after the implementation proves the complete identity tuple matches and the retained result is valid under the same codec/type version.

**ORNA-SYS-081** `sys.start` MUST return a handle bound to one runtime generation and one invocation identity.

**ORNA-SYS-082** `sys.await` timeout MUST NOT cancel, detach, reparent or otherwise change the target invocation.

**ORNA-SYS-083** `sys.cancel` MUST be idempotent and MUST NOT convert cancellation into an ordinary recoverable failure inside the target.

**ORNA-SYS-129** Every type named by a portable `sys` field or function signature MUST be a language type, a core presentation/query type, a canonical system relation, an opaque identifier, a closed enum or a supporting value type enumerated by `api/sys.json`; undocumented pseudo-types are forbidden.

**ORNA-SYS-130** `sys.Value` MUST NOT be used as an implicit inference fallback or an implicit conversion route. It MUST preserve exact originating type identity and MUST require an explicit reflective API to unbox or validate it.

**ORNA-SYS-131** `sys.ArgumentMap` MUST reject duplicate names, preserve each argument's exact originating type and produce a deterministic canonical name order for idempotency hashing.

**ORNA-SYS-132** The typed `sys.invoke<T>` and `sys.start<T>` overloads MUST validate the function's declared result against the explicit `as: T` witness before any function effect occurs. A mismatch MUST fail with `sys.invoke.return_type`; no conversion chain is attempted.

**ORNA-SYS-133** `sys.InvocationResult<T>` MUST describe one complete terminal outcome and MUST satisfy the value/failure invariants stated above.

**ORNA-SYS-134** `sys.CheckpointPosition` MUST remain opaque outside its provider contract; generic code MUST NOT order it, increment it or synthesize a successor.

**ORNA-SYS-138** A conforming implementation MUST expose the supporting-type fields and invariants in `api/sys.json` and MUST reject a portable API claim whose type graph contains an unresolved name.

## Mutation and observation consistency

Portable system rows are read-only. Setters, row mutators and collection mutation cannot bypass the [administrative operations](#administration).

Each administrative command records an invocation and safe outcome. Commands spanning Git and local database state use their specified journal/recovery protocol; the word “atomic” does not imply a hidden transaction shared by arbitrary Git processes and an external server.

Multi-relation inspection pins one snapshot or observation instant. A continuation token binds the snapshot, filter, order and redaction context; it cannot be used to splice results from another generation. Relation order remains unspecified unless explicitly ordered.

Retention distinguishes known absence from unavailable detail. Losing optional traces cannot invalidate an advertised complete committed database snapshot, and reading history never restarts an old program.

**ORNA-SYS-084** Direct mutation of a portable `sys` relation MUST fail at resolve/type-check time where statically evident and otherwise before any write is performed.

**ORNA-SYS-085** Every administrative function MUST define its transaction boundary, compare-and-set preconditions, failure codes and durable audit row.

**ORNA-SYS-086** An administrative function MUST validate all affected assertions before publishing its state transition.

**ORNA-SYS-087** A failed administrative operation MUST leave repository refs, logical CWD, checkpoints and runtime state at the last valid boundary described by its algorithm.

**ORNA-SYS-088** The 1.0 `sys.admin` boundary relies on OS/process isolation, Git/SSH credentials and the authenticated network perimeter; it MUST NOT be misrepresented as a built-in enterprise grants or role engine.

**ORNA-SYS-089** A continuation token MUST NOT be reused against a different snapshot, order, filter or redaction context.

**ORNA-SYS-090** Default iteration over an unordered system relation MUST NOT be documented as stable merely because one implementation happens to use object-ID order.

**ORNA-SYS-091** A multi-relation inspection operation requiring a coherent view MUST pin one observation snapshot/instant and expose it in the result.

**ORNA-SYS-092** A client that detects an observation-generation change during a live pagination sequence MUST restart or explicitly accept a new generation; rows MUST NOT be silently spliced across generations.

**ORNA-SYS-093** Retention policy MUST NOT delete data required to reproduce a committed logical snapshot while that snapshot remains advertised as complete.

**ORNA-SYS-094** Pruned optional detail MUST be represented as unavailable metadata, not as an empty value that could be mistaken for observed emptiness.

**ORNA-SYS-095** Retention and publication of runtime observations MUST never cause external effects or restart a program merely because a historical relation is queried.

## Conformance obligations

The complete schema is published both in the [system reference](#system-reference) and `api/sys.json`. Generated declarations are signature notation, not an independent redefinition of the language grammar. A disagreement between those representations is a specification defect, not implementation freedom.

The requirements below specify implementation evidence. The package's authoring checks and semantic models cover only their explicitly reported scope; they are not a declaration that a completed Orna engine has executed all of these obligations.

**ORNA-SYS-103** A conformance claim MUST list every unavailable optional retention/detail profile but MUST NOT call a required portable member optional.

**ORNA-SYS-104** The conformance suite MUST test every root function with success, not-found/wrong-kind, snapshot, redaction and cancellation/failure cases applicable to that function.

**ORNA-SYS-105** The suite MUST verify that active valid examples use `sys.rt` and that `sys.runtime` appears only as an explicitly invalid name or diagnostic subject.

**ORNA-SYS-106** The suite MUST prove live handles fail across runtime generations and that snapshot references remain stable across CWD changes.

**ORNA-SYS-107** The suite MUST prove `sys.await` timeout leaves the target running and independently cancellable.

**ORNA-SYS-108** The suite MUST prove checkpoint compare-and-set, stream-write atomicity, failure retry/skip transitions and assertion validation under injected crashes and cancellation.

**ORNA-SYS-109** The suite MUST prove secret and path redaction before Inspect, Display, Present, codec and trace boundaries.

**ORNA-SYS-110** The suite MUST compare grouped handles with the same-snapshot canonical relations, applying the documented current-runtime/live filter to `sys.rt` handles rather than comparing them with all historical rows.

**ORNA-SYS-125** The suite MUST prove every canonical relation row exposes the correctly typed snapshot/runtime-pinned `reference`, including natural-key references that introduce no synthetic identity.

**ORNA-SYS-126** The suite MUST prove one row writer, annotation-aware semantic identity, physical-rewrite neutrality and stale-generation rejection across Orna and VFS edits.

**ORNA-SYS-127** The suite MUST verify [failure administration](#checkpoints), including stable delivery identity, versioned retries, skip, replay and resolve, and MUST reject system-row mutator methods.

**ORNA-SYS-128** The suite MUST test both `sys.source` and `sys.history` overloads with current, renamed, historical, missing, partially cloned and redacted files/objects.

**ORNA-SYS-139** The suite MUST exercise every snapshot-selector overload and prove that the returned `SnapshotRef` is pinned even when a named branch later moves.

**ORNA-SYS-140** The suite MUST exercise erased and typed invocation/start overloads, reject a mismatched `as: T` before effects, and prove `sys.Value` is never selected by ordinary failed inference.

**ORNA-SYS-141** The suite MUST validate every supporting value-type field, invariant and canonical encoding entry against `api/sys.json`.

# 16. Administrative operations {#administration}

## Administrative boundary

Administrative functions operate on repository, storage or execution state that ordinary table mutation cannot reach. They are not methods on mutable system rows. [The API reference](#system-reference) gives each exact signature; this chapter supplies the corresponding transition contract.

A call is admitted only through the trusted local process boundary or an authenticated endpoint explicitly permitting administration. Possessing a row reference or knowing a plan token is not additional authority. An application evaluation endpoint does not acquire administrative permission merely because it accepts Orna expressions.

**ORNA-ADMIN-001** Every administrative call MUST record an invocation with the function, safe arguments, owner, observed generation and terminal outcome. State-changing calls MUST retain an audit event in the same local transaction as their durable transition, or in the recoverable journal for a Git transition. A failed call records failure without claiming that its requested state change occurred.

**ORNA-ADMIN-002** Administrative operations MUST NOT run reentrantly inside an activation holding uncommitted table writes. Such a call fails with `sys.admin.busy`; the caller may finish its transaction and submit administration as a new activation. Read-only planning and verification may inspect a pinned generation without mutating it.

Every argument is resolved and type-checked before mutation. A reference from another runtime fails when it denotes a live object. A snapshot reference remains pinned. Missing data is hydrated only under the configured availability policy. Failure to obtain required objects leaves the prior authoritative generation unchanged.

### Shared preconditions and failure classes

All mutating operations require a writable attachment and fail with `sys.admin.read_only` otherwise. All reject invalid argument types through the ordinary type/argument diagnostics. `sys.admin.busy` identifies an incompatible active owner/lease or a reentrant write activation. Invalid candidate invariants produce `sys.admin.assertion_failed`. Missing objects produce `sys.admin.missing_object` or the more specific snapshot/source/storage code before mutation. A cancellation before durable admission has no transition; after admission the operation follows its journal or transaction to a safe boundary and reports whether it completed or was cancelled.

A successful no-op returns the documented current value or `false`, not an invented new generation. Unexpected I/O failure is an ordinary failure with a retained safe cause and a recovery record if admission already occurred. It must not be translated into success. State-dependent conditions are rechecked under the lock or compare-and-set transaction; checking them only when a preview is generated is insufficient.

## Repository operations

| Operation | Required state and target | Atomic boundary, audit and failure behaviour |
|---|---|---|
| `plan_checkout` | Resolvable committed target; no mutation permission required to inspect an otherwise accessible target. | Pins HEAD, CWD, index, worktree and pending generation into `sys.CheckoutPlan`. Records a read invocation, not a checkout event. Does not pause consumers, create objects or switch HEAD. Missing or invalid target fails without changes. |
| `create_branch` | Valid non-existing local branch name; supplied committed start point or current committed HEAD. | Git compare-and-set from absent to target commit, with a local journal/audit event. Does not switch, stage, commit, pause streams or alter pending changes. `sys.git.unborn_head`, `sys.git.uncommitted_snapshot`, `sys.git.invalid_ref` or `sys.git.branch_exists` leave all existing refs and CWD untouched. |
| `checkout` | Resolvable committed target; safe carry-forward, or exact authorised destructive plan; valid target schema and assertions. | [CHECKOUT-1](#branching). Records target attachment and preserved/discarded sets. Dirty conflict gives `sys.git.dirty_conflict`; a stale token gives `sys.git.stale_plan`. Neither admits a partial switch. Force cannot bypass invariants. |
| `commit` | Valid staged state, nonempty staged delta, safe author identity and message; no conflicting publication lease. | Builds the staged candidate, validates it, creates its commit, then uses the [publication reconciliation protocol](#publication) to preserve unstaged work. Records commit/ref transition and exact staged generation. An unchanged index gives `sys.git.nothing_to_commit`; stale state is re-planned or rejected before ref change. It never consumes a newer unstaged tail. |

`sys.admin.commit` has no implicit “allow empty” option. The Git-compatible CLI may expose Git's explicit option separately. Default author information comes from configured Git identity; missing identity fails before creating a commit. The committer and commit timestamp are recorded independently of the authored identity and timestamp.

### Unborn branches

`create_branch` is equivalent to creating a ref at an existing commit, so an absent default HEAD commit fails. `orna switch -c name` in an unborn worktree follows Git's distinct behaviour: it selects the new unborn symbolic branch and preserves staged and unstaged content; no branch ref points to a fabricated commit. The first actual commit creates that branch ref. The operation is journalled like other symbolic-HEAD changes and does not reset allocators or checkpoints.

## Storage operations

| Operation | Preconditions | Atomic boundary and result |
|---|---|---|
| `flush` | Optional table exists and is writable; frozen pending generation selected. | Seal that generation durably into storage objects and update the frozen local root metadata. Do not move a Git ref or consume later writes. Return exact rows/bytes/storage nodes sealed. An empty tail is a successful zero result. Physical failure leaves old data authoritative and reports unavailable/corrupt storage as appropriate. |
| `compact` | Selected indexed storage roots exist, are verified and do not overlap an incompatible maintenance lease. | Build replacements off to the side, compare the input generation, then atomically admit the local canonical root. Preserve semantic rows, keys and checkpoints. Record a compaction job. Stale inputs give `sys.storage.rewrite_conflict`; cancellation discards unadmitted outputs. |
| `verify` | Readable selected scope; a pinned observation generation. | Inspect without repair. Return all discovered errors/warnings in `sys.VerificationReport`; the report distinguishes corruption from unavailable promised bytes. A failed integrity check is report data, not an implicit repair. Failure to complete the scan is `sys.admin.verification_failed` with a partial safe diagnostic, never a clean report. |

Objects written before failed compaction admission are unreferenced temporary objects and may be garbage-collected after recovery. A reference to old pinned data continues to resolve for its retained lifetime; switching the active root does not rewrite the meaning of that reference. No maintenance operation advances a source checkpoint.

## Runs, streams and delivery state

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

## Cancellation and crash recovery

**ORNA-ADMIN-003** A state-changing command MUST expose one of: no admission, admitted and recoverable, or terminal. On reconnect, an unknown outcome MUST be inspected through its retained invocation/journal identity before another non-idempotent command is issued.

The host must distinguish inability to report a response from inability to commit an operation. A disconnected response after a successful commit does not turn that commit into a failure. A retry with the same idempotency identity observes that recorded outcome. External effects remain subject to the limitations in [execution](#execution); the administrative interface does not make an arbitrary provider exactly-once.



## Reviewed commit boundary (1.1)

`sys.admin.review_commit()` validates the current staged candidate and returns a `sys.CommitReview`. It performs no staging or branch mutation. It fails on unresolved index entries, invalid candidate schema/rows/Blob closure, absent required objects or an unavailable CWD pin. The candidate is the actual complete index tree, not a list of display labels.

The review token is `SHA256(ASCII("orna.commit-review.v1") || 00 || OVB-1(payload))`, where payload is the ordered array `[database_uuid, runtime_uuid, head_oid_or_null, head_ref_or_null, index_tree_oid, index_digest, worktree_digest, cwd_snapshot_descriptor, pending_generation, repository_format_uint]`. OIDs and digests use the existing typed encodings; absent structural fields use CBOR null. `index_digest` is SHA-256 over the captured complete index-file bytes. `worktree_digest` is SHA-256 of `orna.commit-review-worktree.v1`, a zero byte, and OVB-1 of an array sorted by repository-path UTF-8 bytes, with entries `[safe_relative_path, git_mode_uint, SHA256(current_file_bytes)]` for all candidate-managed paths whose ordinary worktree content will be reconciled; absent paths use mode 0 and null digest. It is an explicit full byte observation of these ordinary files, not external media payload hydration. The journal retains that exact path set; extra managed paths discovered at final validation invalidate the review. Sparse excluded content paths are omitted from this worktree byte inventory and are already bound by `index_tree`; they MUST NOT be hydrated merely to produce a review. Symlinks are hashed as their raw link-target bytes without following them. Gitlinks bind the checked-out dependency HEAD OID as ASCII without a newline; the publisher does not reconcile or rewrite files inside the dependency. A split-index reader must validate every referenced shared index before accepting the complete index tree and flags.

`sys.admin.commit_reviewed(review, message, author)` rechecks administrative authority and recomputes/compares every review input under the ordinary index lock plus publication lock. It then creates and publishes exactly the reviewed candidate using the same ref CAS, durability and recovery rules as `sys.admin.commit`. Concurrent changes fail `sys.git.stale_review`; the command does not silently refresh the review and retry. Supplying a forged review cannot grant authority. Empty candidates follow `sys.git.nothing_to_commit`. Message/author are bound at invocation; their omission from the review token permits drafting a message after reviewing the contents. The implementation uses the existing versioned operation/request identity for lost-result recovery and does not call this operation again with a new identity after an uncertain result.

The older `sys.admin.commit` remains unchanged and is not retrospectively described as a reviewed-candidate operation. UI controls that promise to commit reviewed contents must use the guarded operation.

`compact` is physical maintenance of the single recorded profile, not a storage-mode selector. Legacy segment-named result fields are retained only as compatibility-shaped observations; their items identify bounded row pages under the current schema. The new API marks this deliberately rather than maintaining Parquet as another writer.

# 17. The interactive session {#repl}

## Session model

The REPL behaves like an ephemeral module. Imports, bindings and function declarations persist for the session.

```text
> use sensors.greenhouse.*;
> let recent = Reading | filter(r => r.time > now() - 1.hour);
> fn warm() = recent | filter(r => r.temperature > 25.C);
```

**ORNA-REPL-001** The REPL MUST use ordinary `use` syntax for namespace access and MUST NOT require a separate `:cd` namespace model.

## Last result and status

```text
$_
```

is the last successful REPL result, inspired by shell conventions.

```text
$?
```

is the last execution status/error value if supported.

**ORNA-REPL-002** `$_` and `$?` are REPL bindings and MUST NOT become implicit globals in module source.

## Syntax-aware editing

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

## Eager previews

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

**ORNA-REPL-003** Eager preview MUST NOT perform mutations, external I/O or other effects.

**ORNA-REPL-004** A relation preview MUST NOT require complete enumeration.

After submission, the relation may be presented as a bounded table window and fetched incrementally during navigation.

## Default presentation

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

## Console commands

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

**ORNA-REPL-005** `:at` changes the default snapshot context for the session; it MUST NOT mutate repository HEAD.

**ORNA-REPL-006** `:watch` MUST use the same dependency and delta machinery as page sessions.

# 18. Command-line interface {#cli}

## Git-compatible commands

```text
orna init      clone      status     add        commit     log
orna diff      branch     checkout   switch     merge      reset
orna push      pull       fetch      remote     stash      tag
orna blame     show       restore    mv
```

**ORNA-CLI-001** If Git already has the operation, Orna SHOULD preserve its name, flags and observable semantics. `orna mv` additionally preserves Orna semantic identity when the target is a definition or row key.

## Orna-specific commands {#cli-small-orna-specific-surface}

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

**ORNA-CLI-002** Runtime/checkpoint/failure detail and uncommon recovery operations SHOULD be available through typed `sys` values in the REPL and optional `std.devtools` pages.

## Status

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

**ORNA-CLI-003** `orna status --porcelain` MUST provide stable machine-readable output.

**ORNA-CLI-004** `orna status --short` SHOULD preserve Git's compact style while adding stable codes for runtime CWD summaries.

## Diagnostics {#cli-diagnostics}

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

**ORNA-DIAG-001** Errors MUST have stable codes, concise titles, source spans when available, causes and actionable help where known.

**ORNA-DIAG-002** `orna explain <code>` SHOULD provide extended documentation.

## Output controls

```text
-v / -vv
-q
--color auto|always|never
--format human|short|json
```

**ORNA-CLI-005** Dynamic progress MUST be suppressed when output is not a terminal unless explicitly forced.

**ORNA-CLI-006** Terminal hyperlinks SHOULD be emitted when supported.

## Visual language

```text
green   success / valid
yellow  warning / change
red     error / conflict
cyan    object names / refs / paths
dim     secondary metadata
```

No emoji-heavy celebration or decorative ASCII branding is required.


## User-facing diagnostics

**ORNA-UX-001** Normal CLI output, REPL output, diagnostics, generated user documentation and default frontend text MUST describe the problem and remedy using Orna concepts.

**ORNA-UX-002** Normal user-facing output MUST NOT require the user to understand internal profile names, storage libraries, embedded-database terminology, wire encodings, object formats or extension ABI terminology.

**ORNA-UX-003** `--verbose` MAY provide additional Orna-level context. Raw implementation details require `--debug` or an equivalent explicitly technical interface.

**ORNA-UX-004** A diagnostic MUST lead with the user-visible condition and an actionable remedy. An implementation MAY attach machine-readable technical causes without rendering them by default.


## Format-3 paths and editing

`orna mount`, `orna unmount` and `orna edit` use VFS-1's exact forms. The mount
itself is not a Git worktree. Git-compatible commands target the backing
repository. `orna add --row TABLE KEY_TEXT` and `orna restore --staged --row
TABLE KEY_TEXT` stage/unstage a logical row into the admitted index store;
`--table TABLE` selects a complete table explicitly. KEY_TEXT is one canonical
Orna key expression, not a host path. CLI parsers must not consume it as shell
source. Those extensions never redirect plain `git add` to a generated VFS.

`orna upgrade --to-format 3` prepares UPGRADE-3 against the selected current
workspace and publishes only after explicit reviewed confirmation (or existing
authorized noninteractive mode). No automatic downgrade command is provided.
`orna export DEST --at SELECTOR` freezes and verifies complete selected snapshot
closure and dependencies before claiming an offline-complete copy. Runtime
private sessions and unaccepted drafts are excluded; exporting CWD first
creates a private frozen snapshot without moving the user's branch.

# 19. Repository, CWD, index and HEAD {#repository}


## One executable snapshot

**Conceptual integrity:** Git is the published database, not an optional export
of it. One commit binds program, schema, table data, binary content and exact
external dependency gitlinks. A clone can acquire omitted objects lazily;
logical reconstruction does not mean downloading or recompiling everything.

```text
library/
  main.orna
  music.orna
  .gitmodules                 # only when dependencies require it
  stdlib/std/                 # explicit pinned dependency, when selected
  .orna/
    database.orna
    store/                    # native Git tree, normally sparsely excluded
  .git/                       # directory OR gitfile; not a fixed runtime path
```

**ORNA-REPO-001** `main.orna` MUST remain the fixed root module.
**ORNA-REPO-002** The reachable module graph MUST determine the database program; recursive filename scanning is not module admission.
**ORNA-REPO-003** Unreachable `.orna` files MUST NOT acquire declarations or execute because they exist.
**ORNA-REPO-004** Stored rows MUST belong to stable reachable table identities in the format-3 store; rows are not individually imported modules or executable scripts.
**ORNA-CORE-001** Published repositories MUST use ordinary Git objects, refs and commits readable by ordinary Git tooling.
**ORNA-CORE-002** The implementation MUST NOT substitute a proprietary history graph, LFS, or a separate mandatory media service.
**ORNA-CORE-003** A published commit MUST bind complete source/schema/data/content closure and exact dependency gitlinks; local availability and globally complete snapshot identity MUST be distinguished.

## HEAD, index and CWD

`HEAD` is the selected Git commit. The ordinary index is Git's staged tree and
flags. The worktree contains checked-out source/resource edits. `state.db`
contains accepted but unpublished logical changes. CWD is the admitted
composition of these observations, never an arbitrary last-writer union.
Overlapping managed-store or schema edits are explicit conflicts.

**ORNA-STATE-001** `HEAD` MUST exclude uncommitted runtime data.
**ORNA-STATE-002** Plain relation access MUST select CWD.
**ORNA-STATE-003** `relation.as_of(HEAD)` MUST exclude the unpublished tail.
**ORNA-STATE-004** Explicit snapshots MUST resolve exactly, not silently rebind to a later branch head.
**ORNA-STATE-004A** Relation-level `as_of` MUST pin data and its decoding schema; whole-program historical execution is separately selected.
**ORNA-STATE-005** Database selectors MUST NOT replace wall-clock `now()`.

## Exactly two required tracked metadata entries

`database.orna` is a bounded DATA record, not configuration code:

```orna
{
    repository_format: 3,
    database_id: "9f237b7e-6844-4498-bcd5-d24641c07449",
}
```

The physical format coordinate is not the language or application version.
The UUID is generated once, preserved across branches/clones, and is not a
credential. The current commit cannot embed its own final hash. `.orna/store`
is already named by that commit's tree: no duplicate root hash is needed.

**ORNA-META-001** Format-3 tracked `.orna/` MUST contain only `database.orna` and `store`; required published system records MUST use the shared store.
**ORNA-META-002** `database.orna` MUST carry `repository_format: 3` and the stable database UUID; a separate `format.orna` is a legacy-format input, not another new-format authority.
**ORNA-META-003** Initialization MUST generate one UUIDv4, preserve existing source, and neither stage nor create a Git commit implicitly. Changing database identity requires an explicit fork-as-new-database migration.
**ORNA-META-004** Published checkpoint/allocator/runtime summaries MUST remain historical observations; checking them out MUST NOT rewind live fences, leases or non-rewindable allocators.
**ORNA-META-005** Caches, credentials, mounts, sessions, rejected edit drafts and unpublished changes MUST NOT be written into tracked `.orna/`.

## Private local ownership

Resolve administrative paths through Git (`git rev-parse --git-path orna`),
including linked worktrees. Do not append `/.git` to a guessed directory.

```text
<per-worktree-git-admin>/orna/
  state.db       # durable unpublished changes and journals
  cache.db       # optional, rebuildable
  scratch/       # bounded capture/edit/recovery candidates
  runtime.sock   # when an owner runs
  owner.lock     # when required by platform
```

WAL/SHM companions and required locks are real durability machinery, not
clutter to delete. Mount/edit records live in the same journal, not sidecar
JSON beside each row. Cache loss is recoverable; loss of unpublished accepted
state is not automatically recoverable from Git.

**ORNA-LOCAL-001** Private state MUST use the per-worktree administrative path.
**ORNA-LOCAL-002** Unpublished accepted state MUST be authoritative and crash-durable under the declared failure model.
**ORNA-LOCAL-003** Derived caches MUST be disposable and keyed by source/schema/store/interface generations.
**ORNA-LOCAL-004** Private state MUST never be added to ordinary commits or included by a naive working-directory copy advertised as a complete export.
**ORNA-EMBED-001** Local database use MUST work without devtools, a VFS mount, or `orna serve`.
**ORNA-EMBED-002** The local CLI MUST be able to own embedded state in-process when no local writer exists.
**ORNA-EMBED-003** Concurrent local commands MAY use private IPC to the single owner, not an obligatory public daemon.
**ORNA-EMBED-004** Correctness MUST NOT depend on experimental unsynchronized multi-process SQLite/Turso access.
**ORNA-CLONE-001** Clones and worktrees MUST have independent workspace/runtime identities; database UUID alone MUST NOT identify a writer.
**ORNA-CLONE-002** Published synchronization MUST use Git commits/fetch/push/merge.
**ORNA-CLONE-003** Non-fast-forward or concurrent writes MUST be explicit conflicts, not hidden cross-clone locks.
**ORNA-CLONE-004** Independent clones MUST NOT claim exactly-once external ingestion without a connector-specific coordination or idempotency contract.

# 20. Embedded ownership and local durability {#embedded}


A long import, HTTP service, FUSE session or media decoder is not one open
SQLite write transaction. Capture/parse work is bounded and staged; a short
admission transaction publishes a logical state change.

**ORNA-TURSO-001** The local durable CWD MUST use embedded Turso/SQLite-compatible transaction storage, with no mandatory external server.
**ORNA-TURSO-002** Row writes and associated checkpoint/request outcomes MUST commit atomically in the owning local transaction.
**ORNA-TURSO-003** Implementations MUST document process/OS/power-loss guarantees, flush assumptions and measured settings.
**ORNA-TURSO-004** There MUST be at most one owning writable embedded state instance per worktree under the single-owner profile.
**ORNA-TURSO-005** Other local writers MUST coordinate with that owner or fail, never independently reopen an unsafe writable handle.
**ORNA-TURSO-006** Ownership acquisition/recovery MUST fence old writers before admitting a replacement.
**ORNA-IMPL-001** Git plumbing MAY use Git, gix or libgit2 while preserving native objects and observable Git semantics.

## CAPTURE-3: bytes to an admitted immutable value

1. Authorize source access and reserve storage/length/time quotas. Open one
   stable source handle; capture never follows a later pathname replacement.
2. Stream into a private object quarantine and/or disk-backed edit extents.
   Compute raw-byte SHA-256 and the pinned chunk profile; do not retain one
   giant byte vector. Explicit import declarations determine MIME/hint.
3. Finish the immutable graph, verify new object types/digests/lengths, flush
   objects/packs, and install a private native protection ref. Objects must
   survive Git GC before any dependent row is acknowledged.
4. Return an owner-scoped Blob. Insertion/update uses the normal transaction,
   storing its graph dependency with row mutation, request and checkpoint.
   Finishing capture alone does not insert a row or create a public commit.
5. Transfer pin ownership to durable pending state before releasing the
   capture owner. Cancellation frees only unadmitted/unreferenced scratch.
6. Publication makes the graph reachable from a commit. Release private pins
   only after the commit and local visibility journal make protection redundant.

**ORNA-CAP-001** Every acknowledged row containing new content MUST have durable verified new objects and native retention roots; a hash string in SQLite is insufficient.
**ORNA-CAP-002** Inherited promised objects MAY remain unhydrated only with recorded snapshot/promisor evidence; newly imported unknown bytes MUST NOT be acknowledged as merely promised.
**ORNA-CAP-003** Runtime pin refs MUST be local, owner-scoped and excluded from ordinary remote push; export of unpublished work MUST first freeze a recoverable consistent root.
**ORNA-CAP-004** Recovery MUST distinguish unaccepted scratch, accepted pending content, and published content; cleanup MUST NOT infer orphanhood from one absent in-memory handle.
**ORNA-CAP-005** Disk-full, cancellation, source mutation and failed flush MUST leave no falsely acknowledged dependent row; uncertain external effects MUST remain explicitly uncertain.
**ORNA-CAP-006** Git GC/repack MUST share the declared maintenance boundary. Concurrent external destructive prune that bypasses protection is unsupported and MUST NOT be advertised as safe.

A regular-file capture checks size/identity/change observations before and
after reading, but that is not a universal filesystem snapshot. A changing
source either fails under the strict capture profile or is supplied from a
source-snapshot mechanism; do not claim a pre-import version that was not
observed. Native content pins may survive a crash and be cleaned later.

# 21. Branching, staging and checkout {#branching}


Branches remain ordinary Git refs. Worktrees own pending state; a branch is
not a bag of unpublished changes. Mounts are projections of a worktree or a
pinned commit, not additional branch semantics.

**ORNA-BRANCH-001** Creating a branch at HEAD MUST preserve CWD/index and create no implicit commit.
**ORNA-BRANCH-002** Selecting a branch at the same commit MUST preserve pending data, staged/unstaged source differences and non-rewindable continuity state.
**ORNA-BRANCH-003** A different-commit checkout MUST run CHECKOUT-3 with exact target validation, conflict preservation and recoverable state transitions.

## Logical staging over a managed root

Source/resources use normal Git paths and index semantics. Rows no longer have
physical Git paths. `orna add --row TABLE KEY_TEXT` is the specified adapter
for staging a row's current stored value (or deletion) using a canonical
schema-typed key expression. `orna add --table TABLE` stages a whole selected
table only after showing/confirming its scope under the host's interaction
policy. These are metadata adapters, not new table representations.

The staging operation modifies a candidate `.orna/store` in the Git index and
records exact staged row generations. Later CWD mutations remain unstaged.
`orna restore --staged --row TABLE KEY_TEXT` restores that staged row to HEAD.
Do not claim ordinary `git add` can interpret virtual row paths.

**ORNA-STAGE-001** Staging MUST bind the exact selected source/schema/row/content candidate and preserve unrelated staged or partially staged paths.
**ORNA-STAGE-002** The index store MUST remain semantically complete and decode under its staged schema; missing dependencies or incompatible staged schemas MUST be rejected, not silently repaired using CWD.
**ORNA-STAGE-003** Repeated staging MUST be deterministic and metadata-only staging MUST NOT hydrate media payloads.
**ORNA-STAGE-004** Internal `.orna/store` paths MUST be managed atomically by Orna. Direct edits by other Git tools are detected as external candidate changes, not accepted without graph/schema validation.

## CHECKOUT-3

1. Resolve the selector once to an exact commit and, if applicable, an exact
   target ref. Acquire workspace mutation authority; fence new activations.
2. Wait for existing transactions to reach a boundary; cancellation joins owned
   children. Do not kill unrelated worktrees or quietly publish their work.
3. Admit target format, source graph, schema/catalogue and logical root.
   Verify available object structure; retain exact promises for lazy payloads.
   Validate affected row/reference/assertion obligations before visibility.
4. Compute staged/unstaged/pending carry-forward. Preserve safe independent
   edits; reject overlapping row/source/schema changes unless explicit reviewed
   resolution exists. A force token binds exact discarded-state identities.
5. Journal old/new HEAD/ref/index/worktree/store generations and continuity
   protection. Use Git-compatible index/ref locks and expected-old ref updates.
6. Install the coherent target and local base, leaving newer unrelated edits
   intact. Cache invalidation and VFS directory lookups switch by generation;
   existing read handles keep their pins. Unadmitted drafts are retained.
7. Complete recovery records before admitting new mutations. Do not rewind
   allocator authority, revive old sessions or rerun historical consumers.

**ORNA-CHECKOUT-001** A failed/stale checkout plan MUST leave current accepted state unchanged and preserve all drafts/unrelated changes.
**ORNA-CHECKOUT-002** Checkout recovery MUST inspect actual refs and journal progress; it MUST NOT force a ref back after another writer advances it.
**ORNA-CHECKOUT-003** A commit mount MUST be immutable; a branch-name snapshot mount MUST resolve once rather than move during reads.
**ORNA-CHECKOUT-004** A workspace mount MUST share the selected worktree's CWD, not create an untracked independent copy. Independent work requires another worktree/clone.

# 22. Publication and crash recovery {#publication}

The logical CWD combines committed rows with durable pending mutations. Publishing changes their representation and replication boundary: it makes an already committed local activation available through a Git commit. It is not a second execution of that activation.

## Pending state and policy

**ORNA-HOT-001** High-rate writes MUST become durable and queryable in embedded Turso before publication.

**ORNA-HOT-002** Status output MUST summarise pending high-rate changes rather than emit one path per pending row.

**ORNA-PUB-001** The default compact writer target is 16 MiB of compressed data, with an 8–32 MiB normal tuning range and the compact profile's 64 MiB file bound. An implementation MAY choose a target within that range but MUST expose the effective policy. A changed tuning value does not change logical rows or checkpoint meaning.

**ORNA-PUB-002** A maximum pending age MAY cause publication of a smaller complete batch; the profile default is 60 seconds.

**ORNA-PUB-003** Publication policy MUST NOT introduce a different table declaration kind.

**ORNA-PUB-004** Effective policy and pending/published state MUST be visible through `sys.Storage` and runtime maintenance metadata.

## Index and worktree invariants

Let **H** be the captured old commit tree, **N** the proposed publication tree, **I** the ordinary Git index, **W** the ordinary working tree, and **P** the frozen runtime mutation batch. The publisher builds **N** from **H + P**, not from **I** or **W**.

The publisher must also compute a reconciled index **I′** and worktree **W′**. Their purpose is to preserve the user's staged difference from the new `HEAD` and the user's unstaged difference from the new index. Merely changing `HEAD` while leaving **I = H** would stage the inverse of **P**. A later ordinary commit could then reverse the publication.

For every managed path changed by **P**, automatic publication requires that its existing index/worktree entries agree with the captured managed base or a journalled Orna projection of the same batch. A conflicting human edit pauses publication. For unrelated paths, staged and unstaged differences are preserved independently, including partially staged files, deletions, file modes and untracked paths. The publisher does not use a hard reset or a blanket `git add`.

**ORNA-PUB-010** A publication commit MUST use a private tree/index built only from the captured base and frozen runtime batch. It MUST NOT commit unrelated human edits.

**ORNA-PUB-011** Before publication is reported complete, the ordinary index MUST be reconciled so it contains no accidental staged reversal of published managed paths, while preserving every unrelated staged change and staged/unstaged boundary.

**ORNA-PUB-012** Unresolved merges, rebases, overlapping managed-path edits or unreconcilable index state MUST pause automatic publication. Durable ingestion may continue within configured storage limits; exhaustion fails explicitly rather than discarding the tail.

## Publication algorithm PUB-3

The following ordering specifies observable durability and recovery requirements. An implementation may use equivalent platform primitives but cannot omit ordinary-index reconciliation.

1. **Freeze.** In a short Turso transaction, select a contiguous committed mutation range, allocate its batch identity, and record the included source/checkpoint watermarks and base generation. Mark it `preparing`; do not remove the pending mutations. New activations may append after this range.
2. **Capture.** Under the worktree publication lock, capture the symbolic/detached `HEAD` identity and old object ID, normal index digest and relevant worktree states. Refuse conflicting merge/rebase state and managed-path edits. Resolve `.git` paths through Git's per-worktree administrative directory.
3. **Encode.** Produce immutable row/index/value nodes and the store root for the frozen batch. Fold repeated mutations in transaction order to one authoritative final mutation per key. Write and flush complete Git objects. Objects not subsequently referenced may remain unreachable.
4. **Build candidate.** With a private index, build **N = H + P** and a commit whose parent is the captured `HEAD`. Human staged or unstaged contents are not inputs. A table publication generation is bound to its captured root and schema.
5. **Plan reconciliation.** Compute **I′** using a Git-compatible two-tree carry-forward from **H** to **N**, preserving unrelated staged changes. Compute **W′** preserving unrelated unstaged changes. Reject managed-path overlaps. Validate staged and CWD database candidates separately; carrying human work cannot make either candidate silently invalid.
6. **Journal.** Durably record the batch, expected/target refs, old/new index bytes or immutable equivalents, affected old/new worktree entries, object hashes, generation and cleanup watermark. Journal data is local metadata, not ordinary committed source. It must be sufficient to distinguish every crash boundary below.
7. **Lock and revalidate.** Acquire the ordinary Git `index.lock` before exposing a changed ref. Hold it until the reconciled index is installed. Obtain the Git ref transaction/expected-old-value protection and recheck the captured symbolic `HEAD`, index and relevant files. Abort if they changed. Cooperating Orna worktree mutators remain excluded by the publication lock.
8. **Prepare files.** Write temporary replacement files and flush them. For each existing affected file, preserve the displaced bytes in the recovery journal/quarantine before installing a replacement. If an external editor changed the path after capture, retain those bytes and stop with a conflict rather than overwrite them. Filesystem writers outside the locking protocol cannot participate in the multi-file transaction. Their conflicting edits must be preserved for reconciliation.
9. **Advance and reconcile.** Advance the target ref using compare-and-set against the old object ID. While the ordinary index remains locked, atomically install **I′** and finish the planned worktree replacements, recording progress. If an external Git process moves a ref after the protected update, recovery treats it as a new state to reconcile; it never forces the ref back.
10. **Publish local boundary.** In Turso, record the publication commit and completion state, then consume only the batch's frozen pending range. Newer tail mutations remain. Logical readers switch using the batch watermark so a mutation is visible exactly once: either through the old commit plus tail, or through the new commit with that batch masked out of the tail.
11. **Complete.** Flush the final journal state, release the normal index lock and worktree lock, and mark the batch complete. Notify storage observers. Do not emit an insert/delete row delta merely because a row moved into a different physical node.

**ORNA-PUB-005** Complete objects and their required durability barriers MUST precede a visible ref that names them.

**ORNA-PUB-006** Failure before ref advancement MUST preserve the pending batch. Unreferenced objects are permitted; a false successful publication result is not.

**ORNA-PUB-007** Failure after ref advancement MUST recover index/worktree reconciliation as well as Turso cleanup. Recognising the batch in the committed manifest is necessary but not sufficient.

**ORNA-PUB-008** A reader MUST observe the old snapshot plus the unpublished tail or the new snapshot with the published batch excluded from the tail. It MUST NOT observe duplicate or missing logical rows.

**ORNA-PUB-009** Ref updates MUST use expected-old-object compare-and-set and revalidate the selected branch/`HEAD` relationship. Concurrent commits MUST NOT be overwritten.

**ORNA-PUB-016** Recovery MUST preserve unrelated staged and unstaged changes. An index lock left by a crashed publisher may be removed only after owner-liveness and journal checks establish that it is that publisher's abandoned lock.

**ORNA-PUB-017** A normal Git commit made after completed publication MUST not reverse published managed data unless the user explicitly staged such a reversal.

### Recovery states

| Observed state | Required action |
|---|---|
| Ref still at H; no candidate visible | Restore only journal-owned partial projections when their hashes match; keep P pending; release abandoned locks safely. |
| Ref at N; ordinary index still I | Complete I′ and W′ reconciliation from the durable journal before admitting further Orna commits; preserve unexpected edits as conflicts. |
| Ref at N; index I′; cleanup absent | Verify the batch manifest, apply the visibility watermark and finish idempotent tail cleanup. |
| Ref at N; cleanup complete | Verify journal completion; remove only temporary/quarantined data no longer needed and explicitly resolved. |
| Ref no longer H or N | Preserve journal and user files, inspect ancestry/batch presence and reconcile under the new ref. If safety cannot be proved, stop with a recovery conflict. Never overwrite the newer ref. |
| Index or affected path differs from both recorded states | Preserve that state; report a typed index/worktree conflict. Do not interpret it as permission to reset. |

A ref update and an index-file rename are not one filesystem transaction. The lock, journal and recovery procedure establish the safe boundary. A command that bypasses Git's index lock and writes arbitrary files is outside cooperative execution, but its unexpected contents must still be preserved when detected.

## Shutdown and transfer

**ORNA-PUB-013** Graceful writer shutdown SHOULD publish eligible batches after its children have terminated and open activations have reached a boundary. A pending publication conflict does not justify discarding the local tail.

**ORNA-PUB-014** Status MUST report remaining unpublished CWD changes.

**ORNA-PUB-015** Transferring resumable execution to another clone requires publishing and pushing the recoverable state, then fetching the resulting snapshot in the receiving clone. Loss of an unpublished local tail can be repaired only by replayable source data or an independently preserved copy of that tail.

Example publication message:

```text
orna: publish runtime data

sensors.Reading     48,120 rows
warehouse.Event       240 rows
```

## Format-3 graph publication

The encoder constructs one `.orna/store` root. It does not materialize VFS
row directories or generated Blob files in the backing worktree. CAPTURE-3
protects new content before row admission; PUB-3 verifies native reachability
and records the frozen store root. The private graph and actual index are
independently verified; hashes written in metadata alone are insufficient.

**ORNA-PUB-018** Pending graph objects MUST be native-pinned before mutation acknowledgement and remain protected through publication/recovery.
**ORNA-PUB-019** Auto-publication MUST be opt-in; the initial product defaults to explicit publication so successful saves do not silently create Git commits.
**ORNA-PUB-020** `.orna/store` index reconciliation MUST preserve separately staged logical changes and never stage the reverse of an automatically published batch.
**ORNA-PUB-021** Published system-watermark rows MUST be folded with the same batch without turning historical data into current operational authority.

For manual reviewed commits, the candidate is the exact admitted index tree,
not HEAD plus all current work. `review_commit` and `commit_reviewed` retain
their state-bound semantics and compare the index, source, schema, workspace
and ref observations under the publication locks. No stale review is refreshed
and retried automatically.

# 23. One indexed row store {#storage}


## One representation, not a placement menu

All new-format persisted rows use ORP-1, the immutable indexed row profile in
`profiles/store-format-3.md`. Small tables have shallow roots; large tables
add index levels. There are no editable/compact/hybrid new writers, no row
placement preference, and no size threshold that changes the table model.
Parquet/text are explicit interchange or legacy-reader formats only.

**ORNA-ROW-001** Every new-format table MUST have one ordered map from typed primary key to stored row value, bound to a stable table/schema identity.
**ORNA-ROW-002** Table lookup/order MUST use the language's logical key comparison, not filename order, CBOR byte order or hash order.
**ORNA-ROW-003** The canonical page/group algorithm, encoding and bounds MUST be those in ORP-1; runtime optimizations MUST produce identical canonical output for the same logical snapshot.
**ORNA-ROW-004** Blob fields MUST use small annotated-content references in stored rows; media bytes MUST never be expanded into row-page bytes or base64 row text.
**ORNA-ROW-005** Large rows/strings/collections MUST use the shared value-overflow graph so metadata pages and direct dependency fanout remain bounded.
**ORNA-ROW-006** Copy-on-write updates MUST reuse unchanged nodes where canonical boundaries permit it; changed indices and payload bytes MUST be measured separately.
**ORNA-ROW-007** Partial fetch MUST preserve exact schema/root pins and return unavailable rather than empty/successfully truncated results on missing objects.
**ORNA-ROW-008** Secondary indexes and long-key VFS name indexes MUST use the same ordered store profile and commit atomically with the rows they index.
**ORNA-ROW-009** An unindexed scan MAY require broad reads; explain/counters MUST distinguish estimated from actual transfer, decode and scan work.
**ORNA-ROW-010** Page rewrites/repacking/rechunking MUST NOT produce logical mutations when decoded values/annotations are unchanged.

## Representation versus interfaces

```text
Orna mutation             -> shared transaction -> stored map
Devtools edit             -> shared transaction -> stored map
VFS completed candidate   -> shared transaction -> stored map
```

A virtual `.orna` document is a schema-directed view, not another stored row.
Snapshot exports use a documented data/Blob manifest rather than guessing at
virtual paths. Opening data does not execute source scripts.

**ORNA-ROW-011** Mutation/assertion/reference validation MUST be shared across all entrypoints; no VFS-specific weaker rules are permitted.
**ORNA-ROW-012** Raw storage nodes MUST not be exposed as writable row files; external graph modification requires full admission before use.
**ORNA-ROW-013** Format-1/2 histories MUST retain versioned readers. Migration MUST NOT dual-write the old row layouts or silently upgrade a historical commit.
**ORNA-ROW-014** New `sys.Storage` metadata MUST describe root, schema, generation, row count when known, and observed bytes/availability; removed preference/placement modes MUST NOT survive as hidden toggles.
**ORNA-ROW-015** The native graph MUST connect all direct dependencies through real Git tree entries, use one object store, and reject mismatching, unused or type-invalid dependency edges.

## Physical paths

`.orna/store` is the current snapshot graph, not an append-only collection of
all historical chunks. Every node uses the same `data`/optional `refs` envelope;
raw terminal byte chunks are ordinary Git blobs. Git owns `.git/objects` fanout
and packfiles. Orna MUST NOT repartition Git's object directory, write a second
hash warehouse, or require a separate media daemon.

A million rows are a required performance test. Neither native Git trees nor
prolly-style partitioning is permission to claim zero index churn, constant
clone size or exact-byte network reads without measurements.

# 24. Git history, remotes and partial clones {#git-history}


**ORNA-GIT-001** A committed snapshot MUST identify all required local source, rows and content through native Git reachability and all dependencies through exact gitlinks/recorded origins.
**ORNA-GIT-002** Partial clones MAY defer blob payloads using ordinary filters and promisor remotes; unknown missing objects MUST be distinguished from promised objects.
**ORNA-GIT-003** A successful full/offline export MUST include all selected snapshot closure and recursive pinned dependencies, not just a pointer-only clone or mounted text files.
**ORNA-GIT-004** Fetch/push/clone MUST preserve Git compatibility and never require LFS to retrieve required content.
**ORNA-GIT-005** Sparse checkout MUST control materialized paths only; it MUST NOT be presented as an availability or confidentiality boundary.

## Lazy reads

Use blobless no-checkout cloning where the remote supports it, then fetch
needed metadata/rows/ranges in bounded batches. A server can refuse filtering;
the command MUST report fallback/estimated scope before silently transferring
an enormous repository. Native Git transfers complete objects, not sub-object
byte ranges. Read-ahead is separately bounded and observable.

**ORNA-GIT-006** Metadata-only queries and updates MUST read zero media payload bytes; metadata objects and tree traffic MUST be accounted separately.
**ORNA-GIT-007** A full mirror of the superproject is not a complete submodule copy; export verification MUST recursively account for dependency object databases.
**ORNA-GIT-008** Refs required for allocator/consumer continuity MUST be synchronized with their existing ordering/validation rules; private edit/capture pins MUST NOT be exposed by ordinary push.
**ORNA-GIT-009** Git host size/push/object limits MUST be reported as deployment constraints. Native format compatibility MUST NOT be advertised as unlimited GitHub storage.

## Retention and security

A metadata edit does not duplicate a recording. Re-encoding all its bytes can
cost another full recording. Chunk reuse is byte reuse, not semantic media
compression. Old reachable ancestry keeps its content; deleting the current
row does not erase its history. History rewrite/export-to-new-root is explicit,
reviewed and never a background prune policy.

**ORNA-GIT-010** Collection MUST preserve content reachable from retained commits, local pins, pending accepted changes, open pins and unfinished recovery journals.
**ORNA-GIT-011** Repository clone credentials MUST be treated as access to repository content; VFS path hiding, namespaces or filtered clone are not row-level permissions.
**ORNA-GIT-012** Offline failure MUST name unavailable scope without exposing secrets; it MUST NOT silently use a current host file or another snapshot.

Complete copy algorithm: freeze/pin a source or CWD root; enumerate its native
closure and dependency commits; hydrate/verify all selected objects; create a
portable archive/mirror plus explicit dependency map; run an independent
read-only reconstruction with the original remote disabled. No historical
application execution is required to recover typed records or bytes.

# 25. Schema evolution, semantic diff and merge {#evolution}


A physical object hash is not a row's identity. A row has a database/table/
typed-key address. Its stored fields have semantic values. Source and schema
revisions define how those values are interpreted; Git identifies the whole
published state.

**ORNA-EVOL-001** Stable table/field identities MUST survive supported renames; a textual name match alone MUST NOT establish continuity.
**ORNA-EVOL-002** Schema changes MUST be admitted against affected rows, references, assertions, indexes and stored codecs before visibility.
**ORNA-EVOL-003** Historical rows MUST be decoded under their historical schema/value profile; current source MUST NOT silently reinterpret them.
**ORNA-EVOL-004** Semantic diff MUST compare logical fields and Blob annotations/content, not storage page or Git-layout IDs.
**ORNA-EVOL-005** A three-way merge MUST validate the merged program/schema/data/content closure and constraints before publishing; a conflict-free Git text merge is not sufficient.
**ORNA-EVOL-006** Conflicting replacements of one Blob MUST be reported as value conflicts unless an explicit application merge function supplies a validated replacement; concatenating chunk lists is not a default merge.
**ORNA-EVOL-007** Explicit rekey MUST use normal immutable-key/reference rules and emit one logical rekey with old/new address, not a filename guess.
**ORNA-EVOL-008** A representation-only rewrite MUST preserve semantic rows and content commitments. Defaulting new annotations in migration is an explicit semantic migration, not falsely an empty semantic diff.
**ORNA-EVOL-009** Required introduction defaults/fallbacks MUST be frozen under the source/schema revision rather than reevaluated on every historical read.
**ORNA-EVOL-010** A failed migration/merge/checkout MUST preserve accepted state and conflicting user edits; no automatic ref reset, inferred winner or unknown-schema overwrite is allowed.

All current new-format writes use one profile. Existing old-format readers are
compatibility code, not user-selectable storage personalities. The precise
format-1/2-to-3 transition is in [upgrade](#upgrade).

# 26. Trust, isolation and extensions {#security}

Orna operates within a trusted environment. Host machines, OS accounts and repository access controls define the trust boundary.

**ORNA-TRUST-001** Local commands trust the invoking OS user. Orna v1 does not define principals, groups, grants, row ACLs, device roles or an enterprise policy language.

**ORNA-TRUST-002** `orna serve` binds to loopback by default. Remote deployment SHOULD use normal SSH, Tailscale, or a trusted authenticated reverse proxy/TLS boundary.

**ORNA-TRUST-003** Public anonymous mutation or arbitrary remote REPL execution is outside the trusted v1 profile.

## Extensions

Portable extensions use the WebAssembly Component Model/WIT where practical; system integrations may use a versioned out-of-process adapter; native extensions are explicitly trusted implementation-specific code.

**ORNA-EXT-001** Orna MUST NOT require a separate handwritten permissions/package manifest merely to restate a component's WIT imports and exports.

**ORNA-EXT-002** WIT imports/exports and the pinned Git object identify the portable interface and code. Installing/running the extension is the trust decision.

**ORNA-EXT-003** A Wasm extension MUST NOT receive raw pointers/handles to Turso or Git internals. Host calls use typed interfaces so a trap cannot corrupt repository/runtime memory.

**ORNA-EXT-004** Implementations SHOULD enforce coarse execution budgets and cancellation for portable components. These controls bound execution; they do not grant per-device permissions.

**ORNA-EXT-005** Native extensions run with the host process's trust and MUST be labeled as such. Orna does not sandbox native extensions.

## SOPS and repository privacy

SOPS-encrypted files may be committed normally. Decryption identities remain outside Git. Secret values are opaque/redacted by default across Inspect, Display, Present, codecs, diagnostics and traces.

Normal row deletion is versioned deletion, not erasure from Git history, remotes or backups. Explicit history rewrite/prune remains a separate destructive workflow.

## Public multi-user deployments

Multi-user authentication and authorisation are outside this profile. Trusted deployments do not require a separate multi-user authorisation service.

## Repository and binary access

Raw clone/fetch credentials imply trust with repository content; sparse checkout and partial clone are not row-level isolation. Content hashes and paths are identifiers, not grants. Blob import, decoding and serving follow the explicit root, quota, cancellation and historical rules in [ingestion](#ingestion). A decoder or UI cannot turn its inspection authority into arbitrary host execution. No new multi-user grant model is introduced in 1.1.

# 27. Secrets and encrypted files {#secrets}

## Principles

Credentials cannot be plaintext Git data, but encrypted secret documents may be versioned.

**ORNA-SECRET-001** Orna MUST NOT commit plaintext secret values by default.

**ORNA-SECRET-002** Secret values MUST be redacted from Inspect, Display, Present, diagnostics, traces and `sys` unless an explicitly privileged operation requests disclosure.

## SOPS profile

The recommended version 1.0 provider uses SOPS with `age`, PGP or a supported KMS.

```text
example/
├── .sops.yaml
└── secrets/
    └── personal.sops.yaml
```

The private `age` identity or KMS credential remains outside Git.

**ORNA-SOPS-001** An SOPS provider MUST decrypt in memory and SHOULD avoid writing plaintext temporary files.

**ORNA-SOPS-002** A clone without the decryption identity MUST remain able to inspect committed non-secret data and MUST report dependent runs as unavailable rather than corrupt.

## Secret references

```orna
let credential = std.secret.ref("messages.inbox");
google.mail(account: credential)
```

**ORNA-SECRET-003** A `SecretRef` may be displayed and serialized by stable name; the resolved secret value may not.

**ORNA-SECRET-004** `sys.Secret` MUST expose metadata such as name, provider and availability but MUST NOT expose secret contents.

# 28. Serving a database {#serving}

## Optional network host

`orna serve` provides network access to the current clone. Local commands use the [embedded runtime](#embedded) directly and do not require the server.

**ORNA-SERVE-001** `orna serve` provides Git transport, page/query endpoints and WebSocket presentation deltas for its clone.

**ORNA-SERVE-002** `orna serve` MUST NOT automatically invoke root `main()` or unrelated stream programs.

**ORNA-SERVE-003** A different clone running `orna serve` serves that clone's HEAD and CWD; CWD MUST remain local to that clone.

**ORNA-SERVE-004** The default listener MUST bind only to loopback unless the operator explicitly selects another address.

## Deployment trust {#serving-trusted-personal-deployment-model}

Host machines and local OS accounts are trusted. The [trust model](#security) defines the isolation boundary.

**ORNA-SERVE-005** Core Orna does not define principals, groups, grants, per-device roles or row-level permissions.

Remote exposure is expected to use existing boundaries such as Tailscale, SSH, host firewall rules or an authenticated reverse proxy. Git-over-SSH uses ordinary SSH keys. HTTPS deployments inherit authentication from the configured trusted proxy where desired.

**ORNA-SERVE-006** Binding beyond loopback MUST be explicit and MUST produce a clear status indication of the exposed interfaces.

Multi-user authorisation is outside this profile; local trusted operation MUST NOT depend on it.

## Git transport and browser frontend

**ORNA-SERVE-007** Git transport MUST remain usable when the optional Git/database web frontend is absent.

**ORNA-SERVE-008** The default frontend SHOULD be an ordinary, removable devtools application. An upstream `std.devtools` package or a vendored `devtools` module is permitted; neither name is privileged. The minimal source-hosted profile and exact source/resource ownership are defined in [source-hosted applications](#source-apps).

**ORNA-SERVE-009** When installed as `/`, the default frontend SHOULD lead with database tables and also expose files, commits, branches, functions, dependencies, storage and runtime state.

## Applications and renderers

An Orna application is a reachable collection of functions, pages, tables and assets. No `CREATE APPLICATION` grammar or application manifest is required merely to group code.

Presentation trees are renderer-neutral. Terminal, web and future native clients render the same value tree and fall back to Inspect-compatible nodes when needed.

## Source-hosted profile

[Source-hosted applications](#source-apps) define the Orna-authored serving and browser-library boundary. The first devtools application is intentionally request/response and manually refreshed; it does not remove existing live-protocol requirements or imply that a first milestone satisfies full Server conformance. Git data/source histories are not replaced by a UI database. Broken optional code must remain recoverable through local core commands.

# 29. Canonical values and schema descriptors {#formats}

This section retains the OVB-1 compatibility codec and defines its boundary with OVB-2/ROV-3/SOV-3. New-format stored values and legacy wire fingerprints use explicitly different coordinates. It does not replace the human-editable Orna text codec. Presentation strings, locale formatting and physical storage compression are not inputs to canonical logical identity.

## Frozen binary value profile OVB-1 (legacy and existing wire)

OVB-1 is an application profile of [CBOR](#ref-cbor). Every length and integer argument uses the shortest permitted head. Arrays, byte strings, text strings and maps have definite length. Maps have no duplicate keys and are ordered by unsigned lexicographic comparison of each key's complete deterministic encoding. Text must be well-formed UTF-8; ordinary string data is not normalised. Indefinite items, CBOR undefined, unsupported simple values and unregistered tags are errors.

Orna Float always uses a CBOR binary64 item, even when a shorter float could express the same number. This is an explicit application-level deterministic rule, not a claim to use CBOR's shortest-float representation. Negative zero retains its sign. All encoded NaNs use bits `0x7ff8000000000000`; infinities remain signed infinities.

Integers in the CBOR major-type 0/1 ranges use those forms. Larger positive integers use tag 2 with minimal unsigned big-endian magnitude bytes; larger negative integers use tag 3 with the minimal magnitude of `−1−n`. A bignum with a leading zero or a value representable directly in major type 0/1 is noncanonical. Zero is the single byte `00`, not an empty bignum. This rule completely defines arbitrary-size integer bytes without host endianness or signed-big-integer conventions.

**ORNA-FORMAT-001** Each canonical encoder MUST use the exact selected profile. OVB-1 legacy/wire encodings remain unchanged; OVB-2 uses the additions below. Strict decoders MUST reject aliases and foreign-profile tags before identity use.

### Type-preserving values

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

### Snapshot encoding

A snapshot is exactly one of:

```text
[0, database_uuid, runtime_uuid, generation_uint, snapshot_id_bytes]
[1, database_uuid, git_hash_algorithm, commit_oid_bytes]
```

For a CWD pin, `snapshot_id_bytes` is the 32-byte SHA-256 digest of ASCII `orna.snapshot.v1`, a zero byte and OVB-1 structural encoding of `[0, database_uuid, runtime_uuid, generation_uint]`. The digest does not include itself. A runtime generation must never identify two different logical states. `runtime_uuid` fences owner restarts. The mapping to the exact logical generation is retained for the pin's documented lifetime. For committed snapshots, `git_hash_algorithm` is `"sha1"` with a 20-byte object ID or `"sha256"` with a 32-byte object ID. An attached database uses its own database UUID in the same committed form, not a different ambiguous shorthand.

**ORNA-FORMAT-002** A CWD reference MUST identify its captured generation. A context-free `[0]` is invalid. A decoder unable to resolve a pin MUST report `sys.snapshot.expired` or `sys.snapshot.incomplete`; it MUST NOT rebind the reference to current CWD.

**ORNA-FORMAT-003** A referenced database, type, table and key schema MUST agree. Decoding data from another snapshot does not implicitly import its schema or replace the current execution context.

## Domain-separated digests

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

## Closed storage schema descriptor

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

## Canonical Orna text

The canonical text codec is schema-directed: `decode(input, as: T)` supplies the exact type and snapshot. It accepts only data constructors in the supplied type, never arbitrary function execution, imports or declarations. Row files are the same data subset with key and computed fields omitted. Loading a row does not execute user code other than deterministic declared construction/refinement validation.

Canonical output is UTF-8 without a byte-order mark, uses LF, two-space indentation for nonempty records, one space after `:`, comma-separated array/tuple elements, a trailing comma after each record field, and one final LF. Record field order is primary-key order followed by stored fields in stable field-ID order for table rows; structural records use NFC name order. Arrays and tuples retain value order. No comments or interpolation occur in canonical output.

Int uses minimal decimal digits with no `+`, leading zeros or separators. Decimal uses its normalised coefficient followed by `e`, a minimal signed decimal exponent (no `+`) and `.decimal`; zero is `0e0.decimal`. Finite Float uses exactly 17 significant decimal digits in scientific form with lowercase `e`, a minimal exponent and suffix `f`; signed zero is retained. Nonfinite Float uses `Float.nan`, `Float.infinity` or `-Float.infinity`. Strings use JSON-style quote/backslash/newline/carriage-return/tab escapes where shared by Orna; other control scalars and a literal opening brace use minimal lowercase `\u{...}`. Ordinary non-control Unicode scalars remain UTF-8.

Bool uses `true`/`false`, Option uses `null` or `Some(value)`, Unit uses `()`, and tuples use the comma distinction from the grammar. Uuid and opaque UUID-backed IDs use quoted lowercase canonical UUID text under their explicit type witness. Legacy bytes-only Blob uses quoted padded standard Base64 only in that legacy full-value export contract; this is not the format-3 row or annotated-value representation. Date/Instant use the validated literal forms; other time values use their OVB component structure rendered as data tuples under the type witness. Enums use the variant name resolved in the supplied type and an explicit record payload when needed. Nominal records use their declared field record; refined values use the base representation and are revalidated. Money and quantities use the exact numeric representation under their supplied type witness; no display symbol or locale is encoded.

Stored references use the target's schema-directed key representation under their declared database/table type. Independently pinned references include the snapshot descriptor and therefore cannot be confused with relative stored references. Generic standalone row encoding includes primary keys; a row-body encoding must be requested through the repository row writer. Unsupported values fail with a decode/encode diagnostic rather than losing type information or private data.

**ORNA-FORMAT-004** Canonical text decoding MUST be type-directed and non-executable. It MUST distinguish optional absence, scalar values, row references and nominal construction according to the supplied schema, and validate all refinements before returning a value.


## Portable opaque and contextual values

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

## Current value profiles and boundary

[The format-3 profile](profiles/store-format-3.md) is normative for OVB-2,
ROV-3, SOV-3 and tags 60110..60113. OVB-1 rejects those tags. Old consumers,
request fingerprints and saved protocol vectors retain their original domains.
New schema/row/argument/consumer identity uses the v3 domains with explicit
migration links. Runtime snapshot/reference identities retain their existing
context fencing and are not interchangeable with content hashes.

An explicit `std.encoding.ovb2` codec uses full annotated value encoding.
Reference-only inspection/row export is not full-value encoding. Byte download
exports just raw bytes and must be selected as such. Full annotated export may
stream; a limited sink rejects oversize before successful completion.

**ORNA-FORMAT-005** New annotated Blob values MUST preserve annotations through OVB-2 and MUST NOT silently lose them on old wire/export interfaces.
**ORNA-FORMAT-006** Stored and semantic reference tags MUST be rejected outside their recorded ROV-3/SOV-3 contexts; a raw SHA or Git OID is never permission to read content.

# 30. Live protocol and session recovery {#protocol}

## Transport and value profile

The live protocol uses the WebSocket subprotocol `orna.present.v1` over RFC 6455. One binary WebSocket message contains one complete OVB-1 CBOR envelope. Fragmented WebSocket frames are reassembled before decoding. Text application messages are rejected; ping, pong and close retain their WebSocket meanings. TLS is required outside an explicitly trusted loopback transport.

The [canonical value profile](#formats) defines integer, float, decimal, option, nominal-value, reference and snapshot bytes. Protocol structure uses the untagged CBOR maps/arrays specified here; application values use their OVB-1 typed representations. A protocol decoder must not confuse structural null fields with a typed optional result.

**ORNA-PROTO-001** An implementation MUST decode and validate the complete message, required fields, canonical forms and configured limits before admitting its operation. It MUST reject duplicate map keys and noncanonical integer/length forms; it MUST NOT execute a partially decoded request.

## Session creation and ownership

A trusted client creates a session with `POST /orna/session`, content type `application/json`, using UTF-8 JSON without duplicate members. The request is exactly `{ "database": "<database-uuid>", "protocol": "orna.present.v1" }`. The database must already be exposed by the host; this request does not attach arbitrary filesystem paths. The endpoint checks its authentication/perimeter policy and Origin before creating execution state.

A successful response is HTTP 201 with `session`, `database` and `runtime` UUID strings, `resume_token` (unpadded Base64url of 32 unpredictable bytes), `websocket_path` (`/orna/live/<session-uuid>`), `lease_ms`, and `limits`. The response sets an HttpOnly, SameSite=Strict session cookie scoped to that WebSocket path; Secure is required on TLS. The cookie authenticates the WebSocket upgrade. The session UUID alone is not a capability. Native clients may retain and send the same cookie through their HTTP/WebSocket stack.

A client resumes with `POST /orna/session/<session-uuid>/resume` and exactly `{ "resume_token": "...", "protocol": "orna.present.v1" }`. A live, unexpired session returns HTTP 200 with the same session identity, current runtime identity, a rotated token/cookie and limits. Only one active WebSocket is attached to a session; a successful resume replaces the old attachment, not its owned tasks. A failed or expired resume returns HTTP 410 and never fabricates a continuation of the old runtime.

`DELETE /orna/session/<session-uuid>` with `Authorization: Bearer <resume_token>` explicitly ends the session. The token must be the currently retained, unexpired token for that exact session. The WebSocket-path cookie alone is not sent to this HTTP endpoint and is not its authentication mechanism. Deletion also checks Origin under the same trusted-client policy. It stops new requests, cancels session-owned work, joins cleanup, and returns HTTP 204 after orderly termination. Loss of the WebSocket starts the advertised finite reconnection lease. No new client request is admitted on a lost connection. Expiry terminates the owner and cancels its children. The default lease is 30,000 ms; a host may advertise a different positive bounded value no greater than 300,000 ms. Explicit close or local REPL death does not wait for this network grace period.

Creation/resume errors use JSON `{ "code": "...", "message": "..." }` and an appropriate status: 400 malformed request, 401/403 unauthenticated or prohibited origin, 404 unavailable database, 409 incompatible protocol/runtime, 410 expired session, 413 limits exceeded, 503 temporarily unavailable. No error includes plaintext credentials or host-private paths. Session HTTP JSON permits no unknown members in version 1; an extension requires explicit negotiation.

**ORNA-PROTO-002** Session resumption MUST preserve request reservations and terminal outcomes for that session while its lease remains valid. A new runtime generation MUST invalidate operational handles from the old generation. Durable request recovery may report old outcomes as data without reviving old handles or owners.

## Envelope and field rules

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

## Complete message registry

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

### Shared structures

**DatabaseContext** is map `{0: database_uuid, 1: snapshot_or_null}`. Null selects the current CWD at admission. A supplied snapshot is exact and permits only read-only evaluation unless it denotes the admitted writable CWD generation. The host pins the admitted context once; retrying an identical reserved request does not resolve its null selector again against a newer CWD.

**PresentationContext** is map `{0: locale, 1: timezone_or_null, 2: width_or_null, 3: theme, 4: supported_kinds}`. Locale is BCP 47 text accepted by the configured presentation package; timezone is an IANA identifier; width is a positive integer in character columns for a terminal or CSS pixels for a browser, with renderer mode declared by `theme` prefix `terminal/`, `web/` or `native/`. Theme is a presentation hint, not executable code. `supported_kinds` is an array of kind identifiers. Unsupported locale/theme falls back to the host's declared default and is reported in snapshot metadata; it does not alter stored values or canonical hashes.

**Diagnostic** is tag 60011 around map `{0: code, 1: severity, 2: message, 3: spans, 4: notes, 5: causes, 6: redacted}`. Code and message are text; severity is 0 note, 1 help, 2 warning, 3 error, 4 fatal; notes are safe strings; causes are nested diagnostics subject to depth limits; redacted is Bool. Each span is `[snapshot, file_path, start_byte, end_byte]`, with a repository-relative UTF-8 path or explicit redacted marker, and half-open byte offsets. A missing span is represented by an empty spans array, not a fabricated file. Optional key 7 carries a stable diagnostic UUID when retained.

These are transport structures, not user-declarable records whose exact field names must be guessed from JSON. Their corresponding typed system metadata preserves the same source and causal semantics.

## Present nodes and patches

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

**ORNA-PROTO-003** The client MUST apply the ordered operations to a temporary tree and publish them atomically only if its current revision equals the declared base and every operation succeeds. Otherwise it MUST discard the patch and request resynchronisation. It MUST NOT leave a partially patched visible tree.

Fine-grained deltas are optional optimisations. Root replacement is always available. A host may coalesce visual updates for a slow client into its latest complete snapshot; this does not drop database rows or move source checkpoints.

## Actions, evaluation and request identity

A resource handle names a page/watchable value produced by the session's evaluation or configured entry function. An action handle is tied to the issuing watch and page revision. A stale action, wrong watch, incompatible typed input or unknown handle fails before any action execution; the host may send a new snapshot so the user can act on current state.

An eval source is exactly one REPL input. A watch source is exactly one expression whose resolved call graph is read-only and externally effect-free; its clock dependency is permitted and scheduled explicitly. Ordinary text in a page or event is never executable source.

For an event/eval, the fingerprint is SHA-256 of ASCII `orna.request.v1`, a zero byte, and canonical OVB encoding of `[session_id, message_type, envelope_watch, body_without_fingerprint]`. Identifiers in this structural tuple are byte strings. The host recomputes and compares it. Every supported optional extension field participates in these bytes. Non-event/eval requests obtain a fingerprint by the same rule using their entire body; clients need not send the redundant fingerprint field.

### Algorithm REQUEST-1

1. Decode, authenticate, enforce session ownership, validate the message and compute its fingerprint.
2. In a local durable reservation transaction, compare `(session, request_id)`. A different fingerprint fails. An existing terminal record returns the recorded outcome. An active record identifies that same operation; it does not schedule another copy.
3. Pin the requested database context and record admission with the safe operation identity. Resolve and type-check the source/action before its first user effect. Failure records one terminal diagnostic without writes.
4. Execute one activation with the reservation lease. Orna-controlled writes, the terminal claim and the compact terminal outcome are committed together on success. Failure/cancellation rolls back the user transaction and records terminal failure/cancellation separately. A read-only result also becomes a retained terminal outcome.
5. After a crash, a reserved operation without a terminal outcome is not automatically replayed. If it was proven to have performed only Orna-controlled transactional effects, recovery may establish rollback and mark it orphaned; if external effects may have occurred, report uncertainty. Neither case manufactures success or silently repeats external effects.
6. Send the terminal result if connected. A lost response does not undo a committed outcome. `request_status` reads the record without executing the request. Retention may prune rich output but keeps the identity/fingerprint and terminal disposition for the advertised reservation lifetime.

**ORNA-PROTO-004** A durable admission reservation and a terminal transactional claim are distinct records/states. The terminal claim and successful Orna writes MUST share one transaction. A reservation without a terminal claim MUST NOT be treated as permission to execute a potentially effectful request again.

## Limits, connection errors and reconnection

The `limits` JSON object contains `max_message_bytes`, `max_depth`, `max_nodes`, `max_collection_items`, `max_outgoing_bytes` and `request_retention_ms`, all positive integers. The minimum accepted profile is 16,777,216 encoded bytes, 64 nesting levels and 100,000 Present nodes. The host must advertise a bounded outgoing queue and a reservation-retention duration at least as long as the session lease. Compression, when negotiated, has separate decoded-size enforcement and cannot evade these bounds.

Malformed CBOR, an invalid envelope/version, unsupported mandatory extensions or invalid message direction closes the connection with code 1002 after a safe diagnostic where possible. Unsupported text application messages use 1003; excessive size uses 1009. A well-formed request with a stale handle, wrong fingerprint, invalid program or forbidden effect receives a correlated diagnostic without closing unrelated watches. Portable protocol codes are `wire.invalid_message`, `wire.unsupported`, `wire.limit`, `wire.request_mismatch`, `wire.stale_action`, `wire.unknown_handle`, `wire.read_only`, `wire.session_expired`, `wire.snapshot_expired` and `wire.outcome_unknown`.

On reconnect the host sends complete snapshots for resumed watches; delta history need not survive. A newly created session has new operational handles. A client must request retained status or inspect current state after uncertain delivery, not automatically resend a mutating action under a new request ID.

The message registry is also available as `profiles/live-messages.json`. The accompanying structural tests validate fields and bounds; independent client/server execution and malformed-frame fuzzing remain separate server-profile conformance obligations.



## Format-3 value compatibility

`orna.present.v1` remains frozen at its existing OVB-1 payload encoding. It
MUST NOT accept OVB-2/ROV-3/SOV-3 tags without a new explicitly negotiated
protocol. Typed inspector summaries can use safe legacy nodes/fields; exact
row/content references are validated against server-owned context. Full new
annotated values use explicit OVB-2 content/export responses. An operation
whose required argument cannot be represented fails before effects. A result
that cannot be richly encoded after commitment uses the existing retained
terminal-without-rich-value contract; it is never rerun under a fresh ID.
Legacy bytes-only values remain valid in their legacy context. An explicit
raw-byte conversion is not the identity operation for an annotated Blob.

# 31. Annotated immutable Blob values {#blob-values}


## One value; two identities

A `Blob` in this final 1.1.0 contract is immutable binary content with a
canonical declared media type and optional suffix hint. It has no intrinsic
path, owner row, mutable file identity or separate history. Every constructor
has a defined declaration: unknown content uses `application/octet-stream`.
A `Song(id)` row owns its application identity and may replace its Blob value.

**ORNA-BLOB-001** Blob MUST represent a finite octet sequence with canonical media type and optional suffix hint. The metadata MUST NOT alter the raw-byte content commitment.
**ORNA-BLOB-002** This release MUST NOT add intrinsic `File`, `Asset`, `Bytes` or `Clob` synonyms or mandatory tables. Existing `sys.File` source-catalogue objects remain distinct from media values.
**ORNA-BLOB-003** Blob MUST work in ordinary stored/nested values; Blob primary keys remain inadmissible.
**ORNA-BLOB-004** Content identity MUST be `(length, SHA256(raw bytes))`, independent of MIME, suffix, Git object header/algorithm, compression, chunking, path and snapshot.
**ORNA-BLOB-005** Blob value equality MUST compare canonical annotations and exact content. `same_content` ignores annotations. Equal digests MUST NOT override detected unequal bytes; independently supplied equal-digest content without a shared admitted witness requires exact streamed comparison or explicit unavailability.
**ORNA-BLOB-006** Detected content collisions MUST fail without overwriting or aliasing an existing content identity.

A Git OID identifies an encoding/layout, not the logical content. A descriptor
is a commitment, not evidence that every promised chunk has been downloaded.
Full verification checks all bytes; range verification checks the fetched
chunk bytes and the selected descriptor path only.

## End-to-end lazy materialization

**ORNA-BLOB-007** Loading a row, passing a same-database Blob reference, showing metadata, reading length/digest or changing MIME/title MUST NOT hydrate media just to form a `Vec<u8>`, encode a row or calculate its semantic digest.
**ORNA-BLOB-008** Held values MUST retain their exact resolution context for their owner/pin lifetime; unavailable/expired values MUST NOT rebind through a later row or HEAD.
**ORNA-BLOB-009** Structured portable annotated values MUST use negotiated OVB-2; old OVB-1 keeps its original bytes-only semantics. No implicit annotation-dropping or reference-in-byte-slot conversion is allowed.
**ORNA-BLOB-010** Inspectors MUST display type, length, declaration and content commitment without payload dumps or remote analysis. The VFS/row representation MUST use small references, not base64 media.

## Shared API

`api/sys.json` is the exact callable inventory. The existing storage functions
remain; these additions make the annotation contract explicit:

```orna
sys.blob.media_type(value)                 // Str, metadata only
sys.blob.suffix(value)                     // Str?, metadata only
sys.blob.annotate(value, media_type, suffix: null)
sys.blob.same_content(left, right)         // Bool, may read bytes
```

`annotate` returns a new annotated value sharing bytes; it never mutates a
content-global MIME registry. An invalid declaration/hint fails before change.
The old constructors keep their signatures and supply the octet-stream/null
annotation by default. An importer may explicitly annotate after capture.
A resource has package-owned metadata if declared; otherwise the same fallback.

**ORNA-BLOB-011** `read` MUST check nonnegative offsets/lengths, offset at/before EOF, checked arithmetic and at most 8 MiB per call; only known EOF clips a successful result.
**ORNA-BLOB-012** Range acquisition MUST fetch required descriptors/intersecting chunks and only separately bounded read-ahead. Whole Git-object overfetch MUST be measured, not called exact range transfer.
**ORNA-BLOB-013** BlobWriter MUST be owner/runtime-bound and nonserializable, with open/finished/aborted states, serialized append/finish/abort, idempotent terminal outcomes and cancellation cleanup.
**ORNA-BLOB-014** Begin/append/finish MUST enforce declared expected length, total quota and the format maximum 2^63−1. No unbounded buffer or temporary-file bypass is allowed.
**ORNA-BLOB-015** Exact snapshot hydration MUST respect offline/network policy and cancellation; host `capture_file` is external I/O and remains forbidden in historical/pure/presenter/assertion contexts.
**ORNA-BLOB-016** Format-3 row/argument/consumer semantic commitments MUST use the versioned SOV-3 domains, not Git layout OIDs or eager full-byte encodings.
**ORNA-BLOB-017** Cross-format comparison MUST compare decoded values under explicit migration semantics, not unrelated digest bytes.
**ORNA-BLOB-018** The existing live protocol MUST reject unnegotiated OVB-2/storage tags. Use typed safe inspector/reference responses and separately authorized content I/O, not automatic full-Blob result serialization.
**ORNA-BLOB-019** Copying into an independent database MUST establish and protect destination closure before acknowledging a dependent row, with no hidden dependency on the original user's path.
**ORNA-BLOB-020** Generic slicing/concatenation MUST produce octet-stream/null annotations unless it is an exact identity no-op; part of a JPEG does not automatically remain image/jpeg.
**ORNA-BLOB-021** Byte export MUST mean original raw bytes. Full annotated value export MUST preserve metadata explicitly; an export operation cannot silently switch between these two promises.

A 10 GB binary edit may require a whole-result streaming pass to calculate its
raw SHA-256. Bounded memory and storage reuse do not imply zero read cost.
Annotation-only edits MUST avoid this pass.

# 32. Unified native-Git graph and Blob chunks {#blob-storage}


## OGS-1: one object infrastructure

The complete binary encoding is in `profiles/store-format-3.md` and executable
reference constructors in `tools/final_model.py`. That model demonstrates
format algorithms, not a production transaction engine.

```text
Git commit
  source files and library gitlinks
  .orna/database.orna
  .orna/store          -> root/index nodes
                           -> row pages
                              -> overflow values / Blob descriptors
                                 -> byte index -> raw chunks
```

**ORNA-GRAPH-001** Each nonterminal node MUST be an ordinary Git tree with `data` and optional `refs`; `data` is one canonical bounded CBOR blob and `refs` maps native OID names to exact child object types.
**ORNA-GRAPH-002** The direct dependency inventory in decoded data MUST exactly match `refs`, deduplicated by OID; missing, unused, malformed or wrong-type edges MUST be rejected.
**ORNA-GRAPH-003** Terminal binary chunks MUST be raw Git blobs. No separate media server, pointer-only object or second hash warehouse is permitted.
**ORNA-GRAPH-004** Node metadata MUST be at most 64 KiB and direct fanout at most 256; larger values/indices MUST use the same bounded graph recursively.
**ORNA-GRAPH-005** Objects MUST be validated under their format, schema and context before use. A shared child MUST NOT bypass per-parent semantic constraints.
**ORNA-GRAPH-006** Git SHA-1 and SHA-256 modes MUST retain native OID widths and verification; neither native OID equals the raw-content SHA-256 by definition.
**ORNA-GRAPH-007** The store root MUST contain only snapshot-required reachable state. Retained history belongs to ancestry/pins, not a current-root catalogue of every historical object.

## Chunked content

OGB-2 retains the earlier draft's fixed Gear chunker parameters: reset after
cuts; min 64 KiB, mask 262143, max 1 MiB; table words are the first eight
big-endian bytes of SHA-256 of `orna.gear.v1` plus NUL plus the octet. Details
and vectors are packaged. Chunking never depends on MIME or file extension.

Content index grouping is changed: stable content-derived anchors partition
ordered child entries, rather than fixed ordinal batches that shift after a
prefix insertion. Spans in each node are local lengths, not global offsets.
Full algorithm/limits are fixed by ORP-1/OGB-2; no implementation may choose
its own chunk/page thresholds while writing the same format coordinate.

**ORNA-OGB-001** The root Blob descriptor MUST bind exact length, raw-byte SHA-256 and its native content graph. MIME and suffix MUST be stored with each referencing value, not as global facts about a hash.
**ORNA-OGB-002** Chunk entries MUST bind native object OID, byte length and independent chunk SHA-256; offset traversal MUST validate lengths, height and bounds.
**ORNA-OGB-003** Zero-length content MUST have one canonical empty descriptor; zero-length nonempty-index children, wrong total length and excessive height MUST be rejected.
**ORNA-OGB-004** Metadata-only access MUST not read chunks. Full verification MUST compare every chunk plus the complete raw-content digest before claiming full integrity.
**ORNA-OGB-005** The chosen partition rule MUST be reproducible; insertions and adversarial patterns MUST have explicit bounded-resource behaviour and measured index churn.
**ORNA-OGB-006** Native Git graph validation MUST not be replaced by checking a descriptor string. Missing promised data is unavailable; unknown missing data is corruption/incomplete closure.
**ORNA-OGB-007** All newly persisted Blob fields MUST use reference form regardless of payload size. Small Blobs may use one terminal chunk, not a second inline-row mode.
**ORNA-OGB-008** Cross-representation equality and semantic diff MUST ignore rechunking, graph OIDs and Git repacking while retaining annotation changes.

## Streamed integrity and retainability

CAPTURE-3 protects newly acquired objects before row acknowledgement; PUB-3
protects and publishes the coherent source/schema/store candidate. Node roots
are retained through actual native refs until public reachability is durable.
Do not run a second independent collector over chunks named in SQLite only.

Git content-defined reuse does not make compressed media edits cheap in every
case. New encodings may have almost no shared chunks. Partial clone defers
payloads, not necessarily all tree metadata. Compatible servers may impose
size/transport limits. These limits MUST be surfaced rather than used to
silently omit content from a snapshot or export.

# 33. Ingestion, inspection and derived knowledge {#ingestion}

## One way to put data into the database

Ingestion is an ordinary function that captures an input and performs ordinary table operations. There is no `ingest`, `asset`, `file` or `source` declaration kind. This extends the same pattern used by the retained sensor example, not a separate import pipeline language.

```orna
pub table Song(id: Str) {
    title: Str,
    content: Blob,
}

pub fn import_song(id: Str, title: Str, root: Str, path: Str) {
    let content = sys.blob.capture_file(root, path, max_bytes: 67108864);
    Song.insert({ id: id, title: title, content: content });
}

pub fn bytes_in_song(id: Str): Int {
    let song = Song | one(item => item.id == id);
    sys.blob.length(song.content)
}
```

A shipped standard library exposes an ordinary `std.io.fs.read_blob` wrapper; the example uses the core operation so it does not depend on an unavailable package pin. The example is valid source against the 1.1 API, not a claim that its engine implementation has executed.

**ORNA-INGEST-001** Import MUST capture the actual bytes, not a mutable source path or URL. Once the row commit is acknowledged, removing the source MUST NOT invalidate the imported Blob. Import MUST NOT automatically create a global media catalogue or unrelated derived records.

**ORNA-INGEST-002** File capture MUST require an authorised root and a safe relative path, resolve beneath the held root without traversal/symlink races, reject device/FIFO/socket/directory inputs, enforce the byte bound, and avoid implicit shell construction. Missing permission, changed source, quota exhaustion and cancellation MUST have distinct typed outcomes without leaking private path details.

**ORNA-INGEST-003** A regular-file capture MUST compare identity, size and modification observations before/after the streamed read and reject detected changes as `sys.blob.source_changed`. Without an OS snapshot or cooperating source lock it guarantees the exact observed byte stream, not an atomic historical photograph of a concurrently modified external file. Stronger source-snapshot claims require the relevant lock/snapshot witness.

**ORNA-INGEST-004** Long byte acquisition MUST NOT hold an exclusive database write lock or one open SQLite transaction for its duration. Its immutable result is installed through the existing optimistic/activation boundary. On conflict, Orna-controlled changes roll back; external reading is not undone. A retry uses the captured protected Blob where valid, not an unnoticed reread of a newer source.

**ORNA-INGEST-005** Programmatic/network ingestion MUST use the same owned Blob writer and atomic row/dependency boundary as regular-file import. Adapter staging is local temporary state, not another persistent content service. Quotas MUST include quarantined, protected, pending and retained objects as separate observable categories.

**ORNA-INGEST-006** Duplicate ingestion of identical bytes MAY share payload storage, but MUST NOT silently merge two distinct application rows or import events. Row deduplication follows the application's keys and transaction rules; content deduplication follows raw-byte identity, regardless of media annotations; annotated value equality remains separate.

## Properties without a second database

There are three layers: intrinsic content properties (length/digest); decoded facts from bytes (format, dimensions, duration, embedded tags); and authored/derived observations (labels, transcripts, visual matches, relationships). They may be displayed together, but their origin and certainty are different.

**ORNA-MEDIA-001** Default inspection MUST make Blob length and content identity available without a plugin or payload read. Common-format inspection SHOULD be supplied by bundled, version-pinned ordinary library functions, not require a separately administered extraction service. An unknown format remains storable, queryable by record metadata and retrievable.

**ORNA-MEDIA-002** A decoder MUST consume the selected immutable Blob through bounded reads, not reopen its import pathname. Its output MUST distinguish valid properties, absent embedded properties, unsupported formats, malformed content and resource/unavailable failures. File extensions and supplied MIME strings MUST NOT substitute for byte validation.

**ORNA-MEDIA-003** Decoding/sniffing MUST be side-effect-free with respect to database state; recording its results is an explicit ordinary transaction. Inspecting a Blob MUST NOT silently start network analysis, purchase inference, run embedded scripts/macros, execute source contained in a document or persist a catalogue of guesses.

**ORNA-MEDIA-004** A decoded property's cache key MUST bind content identity, decoder code/dependency revision, output schema and effective options/bounds that affect the result. A partial result obtained under a small budget MUST NOT masquerade as the complete result under another budget. Cache data belongs in local runtime/cache storage; it MUST NOT be required to interpret a committed record.

**ORNA-MEDIA-005** Native libraries and explicitly authorised decoder processes MAY implement codecs behind typed operations. They MUST use bounded time/memory/output, cancellation, argument-vector execution and protected temporary resources. Their presence MUST NOT create a separate file catalogue or bypass transaction, source-pin and privacy rules.

The first implementation milestone uses a PNG image and PCM RIFF/WAVE recording. JPEG, MP3, FLAC, MP4, EXIF and further properties are ordinary versioned decoder additions, not new Blob subtypes. The release does not promise every file format can be decoded. Browser playback support similarly depends on the selected renderer's actual codecs; failure to preview never prevents byte retrieval.

## Analysis and knowledge

A vision/transcription function is ordinary explicit Orna behaviour with declared effects, not special query syntax. Relational filters first narrow candidate records; an analysis function may examine their Blob values. Lack of an analysis index can require scanning all candidates and MUST be exposed in the plan/budget.

**ORNA-KNOW-001** A persisted analysis result MUST identify every input revision that affects it, function/dependency revision, provider/model coordinate, effective parameters/prompt, output schema, time and success/failure/uncertainty state. Source-byte identity alone is insufficient when related notes, prior outputs or external context participate.

**ORNA-KNOW-002** Persisted annotations/results MUST be ordinary typed records related to their inputs. User corrections and original bytes MUST be preserved independently of regenerated results. Re-analysis MUST NOT silently replace historical observations or mutate an older snapshot.

**ORNA-KNOW-003** External model use, disclosure of content and nontrivial cost MUST require explicit host/operator policy. Recorded provenance does not guarantee a changing external service can reproduce the result. Reproducibility MUST distinguish retained output replay from re-execution with retained model artifacts.

**ORNA-KNOW-004** Historical evaluation MUST use the selected program/data/result revisions. It MUST NOT silently run the host's newest decoder/model to fill a missing historical property. Explicit new analysis of old immutable content is a new operation with a new result, not a revision of history.

There is no mandatory `Asset`, `File`, `Embedding` or `Analysis` system relation. Applications use tables and relationships. A small shared library can define useful report/annotation types without giving its tables privileged storage semantics.

# 34. Source-hosted applications and minimal devtools {#source-apps}


## Scope first

Devtools, the Git viewer and playground are one optional application. The
first usable milestone is a table list, one input, Run, a typed result and an
explicit refresh. No Watch UI, saved-query store, theme engine, WPF object
system or new frontend framework is part of the initial acceptance boundary.
Existing language watches and live-protocol contracts remain available and
are not deleted merely because the first UI does not expose them.

**ORNA-APP-001** Devtools MUST be ordinary selected Orna source, not a privileged native Git homepage or separate playground product.
**ORNA-APP-002** Browse and console results MUST use shared typed inspection. Run MUST call the real parser/resolver/evaluator; simulated results are not completion.
**ORNA-APP-003** Portable UI meaning MUST not depend on HTML, CSS or DOM. The web renderer SHOULD use native semantic controls and absent/minimal optional CSS.
**ORNA-APP-004** The selected application source/resources MUST be the actual executed inputs; removal MUST not trigger hidden reinstallation or native fallback.
**ORNA-APP-005** Pinned `std` submodules MUST retain normal Git semantics. Editing a dependency requires its own commit then a parent gitlink update, or an explicit local vendor/fork; no silent mutable overlay.
**ORNA-APP-006** CSS, browser host bindings and Wasm outputs MUST belong to renderer-package resources or ordinary Blob fields. No compulsory File/Asset registry or `std.devtools.Asset` primitive is permitted.
**ORNA-APP-007** Package resources MUST resolve in the caller's admitted dependency/source snapshot. Host paths, newest installed libraries and mutable remote URLs cannot replace those inputs silently.
**ORNA-APP-008** Builds MUST bind source, dependencies, resources, toolchain and host ABI coherently; offline claims require complete dependency closure.

## Serving and native boundary

The default local `devtools/` source may be copied from a pinned distribution
while `stdlib/std` stays a submodule. This gives editable default application
code without pretending parent Git commits include dirty submodule files.

An explicit root `pub fn serve()` is the source-hosted entry selected by
`orna serve`. It runs as a supervised service, not the root `main()` or one
long database write transaction. Routing/HTML/response composition are Orna
library behaviour over public bounded socket/evaluator/transaction interfaces.
Missing entry/library support is a diagnostic, not a guessed code path.

**ORNA-APP-009** Source-hosted conformance MUST put routing, HTML generation, asset serving and application orchestration in replaceable Orna code; a wrapper around a complete native server fails this claim.
**ORNA-APP-010** CLI serving MUST invoke only the selected serving entry and retain loopback/trusted-perimeter defaults; full Server conformance still requires the existing exact protocol endpoints.
**ORNA-APP-011** Each mutation MUST use shared short activation/request transactions; listeners and browser waits are supervised lifetimes, not open database transactions.
**ORNA-APP-012** The first web implementation MAY use request/response HTML and forms. It MUST bound input, enforce origin/CSRF/session checks, and never blindly replay an uncertain mutation. This is not a full live-protocol claim.
**ORNA-APP-013** Blob summaries MUST be metadata-only. Explicit media/content responses MUST check authority and snapshot identity; inspection of HTML/SVG/JS MUST not execute it.
**ORNA-APP-014** Media responses SHOULD support RFC 9110 HEAD, single byte ranges and immutable validators through the shared Blob reader, with correct 206/416 and truncated-stream failure behaviour.

## Orna-authored browser profile (after the minimum)

**ORNA-APP-015** The later browser profile MUST run protocol/control/rendering code authored in Orna through a Wasm build of the shared evaluator initially; a direct compiler may replace it with equivalent semantics later.
**ORNA-APP-016** Generated JS MAY bind browser APIs only; no app-specific Git/table logic or second Orna language implementation. Server handles/closures MUST NOT become client authority.
**ORNA-APP-017** Page updates MUST use explicit immutable-model transitions. The console's scratch bindings MUST NOT overwrite the page's pinned application environment.
**ORNA-APP-018** Focus/caret/unsent drafts/local feedback MUST remain responsive and not be overwritten by unrelated server results.
**ORNA-APP-019** Connection resume, durable request recovery and page-state restoration MUST be separate promises; expired sessions MUST NOT replay mutation transcripts.
**ORNA-APP-020** Source/runtime upgrades MUST switch coherent generations for new sessions; old actions MUST NOT silently acquire new meanings. Invalid app source MUST leave local core recovery usable.
**ORNA-APP-021** Replaceability MUST be demonstrated by a second ordinary application and an independent minimal Orna renderer/HTTP composition using only public interfaces.
**ORNA-APP-022** The minimum demonstration MUST use actual row/Blob storage: import, query, retrieve/play, metadata edit, historical read and complete offline reopen. A mock HTML page is not engine evidence.

No CSS-to-terminal translation exists. Web CSS lives in a selected package;
text/native renderers consume the same typed intent and use their own platform
rendering. A view or mounted path is not a permission boundary.

# 35. Editable row-directory filesystem {#vfs}


## One interface to the stored database

VFS-1 is a case-sensitive Linux FUSE profile. Writable workspace mounts are
required for this profile; historical mounts are immutable. Other operating
systems require separately tested host adapters and filename rules. Portable
repository content does not imply identical host filesystem capabilities.

```text
Music/london/
  data.orna
  content.mp3
  cover.jpg
```

```orna
{
    title: "Live in London",
    artist: "Kieran",
    content: { path: "./content.mp3", media_type: "audio/mpeg" },
    cover: { path: "./cover.jpg", media_type: "image/jpeg" },
}
```

**ORNA-VFS-001** A virtual row directory MUST address database/table/typed key; `data.orna` MUST encode stored non-key fields only, under that snapshot's schema.
**ORNA-VFS-002** Virtual row text and sibling content files MUST be projections of the shared row store, not materialized competing authoritative copies.
**ORNA-VFS-003** Unchanged schema-directed Blob references MUST resolve internally in the captured row/draft context without reopening the mount or reading payloads.
**ORNA-VFS-004** Relative paths MUST stay within that field's registered projection. Absolute paths, symlink/traversal escapes and implicit cross-row references MUST fail.
**ORNA-VFS-005** Commit selectors MUST pin one snapshot; a branch-name snapshot selector resolves once. Workspace mounts MUST share one selected worktree's durable CWD.
**ORNA-VFS-006** Every accepted edit MUST use normal transaction/assertion/reference/permission validation and never automatically create a Git commit.

## Save state machine: EDIT-1

States: `clean -> dirty -> validating -> accepted`, or `rejected/conflicted`.
The journal records target identity, baseline row/schema/code generation,
owner, draft sequence, extent map, quota and unique apply identity. A writer
sees its own draft; other opens see accepted state. Read handles and directory
enumeration cursors pin coherent generations.

`write`, `truncate`, sparse holes and out-of-order byte writes change only a
private disk-backed/COW candidate. A zero-length Blob can be legal; validity
comes from schema constraints, not guessing whether the user intended it.

1. `fsync`/`fdatasync` of a managed file captures one draft sequence under its
   handle lock, parses/captures the entire candidate, then validates in the
   current authority context against the expected baseline.
2. New chunks are durably native-pinned before admission. Row and apply outcome
   commit atomically. Successful sync acknowledges durable accepted state.
3. Repeated callbacks for the same sequence return the recorded outcome. Later
   writes begin a new candidate based on accepted state; no duplicated mutation.
4. Failure leaves accepted data unchanged and retains an explicit recoverable
   draft/diagnostic. Ordinary `release` performs cleanup only, not acceptance.
5. A supported close `flush` attempts the same operation for dirty state, but
   may repeat or not occur and applications may ignore its error. Unsynced
   abandoned state is retained according to the stated bounded draft policy;
   it MUST NOT be advertised as durably accepted.

**ORNA-VFS-007** FUSE `release` MUST NOT be the only rejection/durability point; callback behavior and close-error limitations MUST be disclosed.
**ORNA-VFS-008** Dirty sequence capture and acceptance MUST serialize with writes; `dup`/fork/repeated flush MUST not create a second acceptance of the same candidate.
**ORNA-VFS-009** Invalid/stale candidates MUST preserve original accepted values, protect retained draft bytes and return a stable error/diagnostic association.
**ORNA-VFS-010** A metadata-only save MUST not fetch or rehash sibling content. A binary save MAY require streaming the entire resulting bytes to establish its raw SHA-256; memory remains bounded.

## Temporary-save replacement

An unknown file inside a row directory is private editor scratch, not a new
column. Its creation captures that row's current baseline. `fsync` of scratch
persists scratch only. Rename onto a managed target admits one complete
replacement and its namespace update through the same journal and transaction.
A failed rename keeps both old target and scratch recoverable. Unsupported
cross-directory/`RENAME_EXCHANGE`/whiteout behavior fails explicitly.

This does not prove when the editor originally read the row. If it closed an
old input handle and creates a new temp output much later, the VFS cannot infer
that prior read version. `orna edit PATH` captures a read-to-save baseline and
runs `$EDITOR` against an explicit draft, providing stronger CAS protection.
No compulsory revision comments or hidden row fields are added to `data.orna`.

**ORNA-VFS-011** Normal save CAS MUST compare the captured write/temp-create baseline. Strong initial-read protection MUST be claimed only for a retained edit token/guarded operation.
**ORNA-VFS-012** Renaming a suffix MUST not transcode bytes, rename columns or rekey a row. Compatible aliases update only the explicit suffix hint through CAS.
**ORNA-VFS-013** Removing a required Blob MUST fail; optional Blob unlink MAY set that field to null after validation. Deleting `data.orna` MUST fail; row deletion is an explicit database operation.
**ORNA-VFS-014** Hard links, device nodes, symlinks and writable shared mmap are unsupported in VFS-1 and MUST fail explicitly. Private mmap/read copies do not modify the database.
**ORNA-VFS-015** Multiple file saves MUST NOT be guessed into one transaction. `orna edit --row PATH` provides explicit grouped row/content admission; separate updates may reject cross-field constraints.

## Paths, naming and metadata

`profiles/vfs-1.md` specifies canonical escaping, composite keys, long-name
aliases, nested collections, field collisions and long-text projection.
`profiles/media-1.json` fixes MIME/suffix normalization/preferences. Path names
are not authority. Table keys remain valid regardless of host name limits.

**ORNA-VFS-016** Reserved/colliding names MUST be escaped reversibly, not rejected as otherwise valid database keys or merged by case-folding.
**ORNA-VFS-017** Directory iteration MUST be lazy/paged and pinned; lookup of a known key MUST use the index. An exhaustive recursive scan can still fetch all requested data.
**ORNA-VFS-018** `stat`, directory listings and row documents MUST not sniff/decode/download Blob content; raw-file size comes from the exact content descriptor.
**ORNA-VFS-019** Schema changes, MIME path changes and API writes MUST invalidate directory/attribute caches coherently. Existing handles MUST not retarget to a different row or reordered collection element.
**ORNA-VFS-020** Source files MAY be edited through the shared workspace source path; complete candidate validation and explicit grouped source edits are required when several files change together. No evaluation occurs during speculative file writes.

## Failure/recovery and boundaries

Default mount permissions are owner-only with `nodev,nosuid,noexec`; broader
access is explicit and never expands core authority. `allow_other` is not a
new row-level security model. Return EINVAL for malformed candidates, EAGAIN
for stale CAS, EROFS for snapshot writes, EPERM/EACCES for denied operations,
ENOSPC/EDQUOT for resource limits, and EIO for corrupt/unavailable backing I/O.
Exact Orna codes remain available through `orna mount status --json`.

**ORNA-VFS-021** Sync/rename acknowledgement, new content pins, local row commitment and namespace recovery MUST be journalled so crashes cannot lose acknowledged content.
**ORNA-VFS-022** FUSE writeback caching MUST be off initially; supported caching/IO modes MUST preserve per-open generation semantics despite non-VFS database writers.
**ORNA-VFS-023** The adapter MUST NOT expose a fake `.git` directory or writable raw store. Git commands target the backing repository, never generated row documents.
**ORNA-VFS-024** Unmount MUST refuse dirty/unresolved accepted-state recovery unless an explicit policy retains it safely. It MUST NOT report discarded drafts as saved.

# 36. MIME declarations and deterministic suffixes {#media-policy}


MIME is a declared interpretation, not a content hash, format validator or
security guarantee. File extensions are a host-view preference, not a schema
column name. The shared MIME-1 policy lives in the profile, not `.orna/` or a
host OS database. Registered aliases/preferences are pinned for a mount.

**ORNA-MIME-001** Every new Blob MUST have a valid concrete declared MIME value; constructors without knowledge use application/octet-stream, never a wildcard or empty string.
**ORNA-MIME-002** Normalization MUST lowercase type/subtype/parameter names, sort parameters, normalize charset case, and preserve other case-sensitive parameter values; duplicate names, controls and malformed quoted strings MUST fail.
**ORNA-MIME-003** A preferred suffix MUST be chosen from MIME-1 by essence; a compatible explicit safe hint overrides it, otherwise unknown types use bin.
**ORNA-MIME-004** Explicit hints MUST be lowercase safe ASCII suffix segments of at most 32 bytes, without leading dot, slash, backslash, empty segments or traversal. Known-type incompatible hints MUST fail.
**ORNA-MIME-005** Defaults matching the preferred suffix MUST canonicalize to no hint; normal spelling/quoting/case variations MUST not create different annotated values.
**ORNA-MIME-006** Import resolution MUST be explicit declaration first, then bounded trusted detection, then a uniquely mapped suffix, then octet-stream. Conflicting observations MUST be disclosed; host-specific MIME registries cannot silently decide persistent metadata.
**ORNA-MIME-007** Detection MUST NOT run on listings/stat/metadata-only edits. Unsupported or damaged content MUST remain preservable unless an explicit schema constraint requires a valid format.
**ORNA-MIME-008** MIME changes MUST update projected filenames atomically while keeping existing content handles pinned. Renames do not transcode or grant executable status.
**ORNA-MIME-009** HTML/SVG/JS previews MUST not execute merely because the stored declaration requests it. Trusted application assets and arbitrary user-content downloads require different explicit serving policies.

Examples: audio/mpeg prefers mp3 but admits mp1/mp2; text/javascript prefers
js but admits mjs; image/jpeg prefers jpg but admits jpeg/jpe; gzip prefers
gz and uses tar.gz only when explicitly supplied. MIME alone cannot infer
MPEG layer, ECMAScript module mode, or an inner tar archive.

Detailed parameter grammar/bounds, aliases and exact preferred suffixes are
in `profiles/media-1.json` and the normative reference normalizer. The table
is an Orna projection policy, not an assertion that IANA mandates one filename
per media type. Changing this table requires a new profile and explicit view
upgrade; persistent content hashes do not change.

# 37. Compatibility, migration and verification {#upgrade}


## The explicit coordinates

The language/specification version is **1.1.0**. This final source publication
is identified by `final-2026-10-05` and the exact artifact checksum. It replaces
the supplied earlier 1.1.0 authoring draft rather than pretending those
incompatible drafts have identical normative content.

The new writer coordinate is **repository format 3**. Its single pinned set
is OGS-1 / ORP-1 / OGB-2 / ROV-3 / SOV-3 / OVB-2 / VFS-1 / MIME-1. These are
format names, not user-selectable per-table storage strategies.

**ORNA-UPGRADE-001** Implementations MUST dispatch readers by recorded repository/value formats, not the CLI version or the current host's defaults.
**ORNA-UPGRADE-002** Existing format-1/2 commits MUST remain immutable and readable under their original decoders and equality/digest semantics.
**ORNA-UPGRADE-003** New-format writes MUST use one canonical row store; loose/compact/hybrid dual-writing is forbidden.
**ORNA-UPGRADE-004** Legacy Blob annotation defaults MUST be explicit: octet-stream and no suffix, without downloading or decoding every payload during metadata migration.
**ORNA-UPGRADE-005** Annotation migration MUST be reported as a semantic profile transition; preservation of raw bytes is not proof of unchanged annotated value identity.

## UPGRADE-3 (explicit, no background conversion)

1. Inspect the actual checkout, uncommitted work, gitlinks, runtime tail and
   format readers. Do not reuse a stale commit hash from a previous audit.
2. Freeze/pin a coherent source/schema/data candidate. Preserve unrelated
   staged and unstaged source and dirty submodule work. Require explicit
   resolution of incompatible staged store/schema changes.
3. Build the new graph privately. Decode exactly one authoritative legacy row
   per key; validate continuity, schema, assertions and references. Reuse exact
   existing payload objects/chunks where compatible; rebuild metadata/index
   layout. Keep aliases/legacy roots until all needed data is protected.
4. Convert published system records into typed store relations. Preserve
   non-rewindable allocator refs and current ownership separately. Map
   persistent consumer/checkpoint identities with recorded predecessor links
   so codec changes cannot silently rerun an already processed stream.
5. Compute format-3 schema/row/consumer domains and explicit old-to-new value
   comparison. Write `database.orna` and the new root; no `format.orna` or old
   current authoritative row folders remain in the candidate.
6. Validate closure and pure decoding. If old layout transformation needs
   unavailable row bytes, report them and stop that migration; lazy payloads
   may remain promised only with inherited verified descriptor/provenance.
7. Prepare one reviewed migration Git commit through PUB-3. Journal both
   sides and preserve the original commit/tail for recovery. Do not auto-push.
8. Verify old and new snapshots independently. Install the new writable
   reader/root only after the protected transition completes. Rollback means
   selecting an old compatible snapshot, not pretending format-3 is format-1.

**ORNA-UPGRADE-006** A migration failure MUST preserve the old authoritative snapshot/tail and all user edits; it MUST NOT partially change the active writer.
**ORNA-UPGRADE-007** Source and API changes MUST be atomic with the format transition; removed storage-preference APIs MUST not secretly choose legacy layouts.
**ORNA-UPGRADE-008** Consumers with Blob-containing arguments MUST not reset progress because the argument encoding domain changed; require an explicit validated migration map or fail closed.
**ORNA-UPGRADE-009** New protocols MUST explicitly negotiate new value codecs; old live endpoints and fingerprints MUST remain byte-for-byte legacy-compatible for their admitted values.
**ORNA-UPGRADE-010** Unsupported annotated values on legacy wire MUST fail before effects or use the specified retained-without-value result after an already committed operation. No implicit downcast to bytes.

## Upgrade of the implementation itself

Keep current parser/evaluator/embedded ownership/partial-clone code where
correct. Move production work out of conformance-only wrappers when necessary,
not into another parallel engine. Archive old write formats but retain their
readers and golden vectors. The current `api/sys.json` is the final contract;
its retired storage operations are recorded in `api/retired.json`.

An independent read-only extractor MUST recover modules, schema, rows and raw
content from a complete snapshot without running old application code or
contacting an AI provider. Compile/cache artifacts are optional for recovery.
Ten-year recoverability is a design/test obligation, not a promise that old
native executables will run forever.

# 38. Worked reference database {#examples}

The reference database demonstrates book lending, stock transfers, exact values and resumable sensor ingestion. Its five modules run with local data and the intrinsic Orna environment. The source files are available in [`examples/reference/`](examples/reference/README.md).

## Project layout

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

## Lending: keys and assertions

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

## Inventory: one atomic transfer

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

## Sensors: resumable finite input

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

## Values: refinement, variants and option

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

## Interactive inspection

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

## Cancellation without changing result types

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

## Testing these examples

Each example case runs in a disposable database with empty tables. The distributed expectations specify initial state, inputs, exact resulting rows and failure outcomes. Parser acceptance is separate from resolver/type/effect checking, and both are separate from execution. A conforming implementation must run all three stages before claiming the example project passes.



## Storage status of example rows

Example row text illustrates logical data or explicit import. In format 3,
these rows are stored in ORP-1, not discovered by recursively reading `.orna`
files from a table folder. VFS-1 can project them as row directories. Historical
fixture row-load stages remain parser/decoder tests and do not reinstate a
loose-row writer.

# 39. Conformance cases and evidence {#conformance}

## Test corpus {#conformance-conventional-conformance-tests}

The conformance corpus contains source fixtures, complete projects, behavioural scenarios and value vectors:

```text
examples/valid/      modules/rows that must parse, resolve and type-check
examples/invalid/    source that must fail in the stated phase with the stated diagnostic
examples/reference/       complete reference database project
tests/               language-independent cases and vector oracles
tools/               selected executable reference checks
```

A generated JSON index describes the fixtures and their expected outcomes for use by implementation test runners.

**ORNA-TEST-001** Every valid language fixture requires successful parse, name resolution and type checking. Parser-only success MUST NOT be presented as full validity.

**ORNA-TEST-002** Every invalid fixture identifies the expected failing phase and stable diagnostic code.

**ORNA-TEST-003** The reference `examples/reference/` project is loaded as one complete database project: parse reachable modules, resolve/type-check them with its declared intrinsic environment and supplied modules, admit every provided legacy row fixture through its declared input context, reconstruct its typed key and validate fields/units before importing it into the current row engine. Do not silently reinterpret fixture paths as format-3 storage.

**ORNA-TEST-004** Bundle validation distinguishes: a test is specified, a test exists in an implementation, and a test passed. Document/index checks MUST NOT claim that Orna source executed when no implementation was run.

**ORNA-TEST-005** The corpus covers each normative syntax alternative, precedence boundary and deliberately invalid ambiguous form. It need not manufacture a positive and negative file for every mechanical EBNF helper production.

**ORNA-TEST-006** A generated requirement-to-test coverage report SHOULD expose untested normative behaviour, but it MUST remain a reporting layer over ordinary tests rather than a new test language or CI system. Informative rationale is excluded from the denominator.

**ORNA-TEST-009** `tests/conformance-manifest.json` MUST index valid, invalid and complete-project fixtures. A complete-project fixture MUST include `load_rows`, which admits the supplied fixture rows through their declared legacy input reader, validates them against the resolved schema/units, then imports them into the current row engine; fixture files do not enable a second new-format row writer.

**ORNA-TEST-010** `tests/requirement-evidence.json` MUST attach a non-empty `tests` list to every numbered requirement. Entries may name ordinary source fixtures, project fixtures, vector suites, behavioural/fault scenarios, interoperability suites, benchmark suites or an explicit inspection requirement. The mapping is a test plan, not evidence that an implementation has executed it.

**ORNA-TEST-011** Machine-testable normative behaviour MUST be identified as requiring an executable fixture, vector, project, scenario or implementation suite. A plan without an implemented test remains unexecuted. Bundle validation MUST fail when a requirement has no recorded evidence obligation; production conformance additionally requires the applicable implementation evidence to pass.

## Branch-based isolation

A branch provides a natural isolated database snapshot for tests and experiments. Fixtures may be inserted into a temporary branch; teardown may be deleting that branch rather than truncating a shared database.

**ORNA-TEST-007** Test tooling SHOULD support temporary branch/worktree isolation without requiring a separately administered test server.

**ORNA-TEST-008** Branch-based tests MUST NOT imply that mocks or conventional unit tests are forbidden; the branch model is an integration-state primitive, not a ban on other testing methods.

## Mandatory behavioral scenarios

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
- partial clone fetches only the required bounded row/index objects and requested content chunks, with actual tree and payload transfer measured separately.


## Evidence levels

A fixture is a proposed test input and oracle. A syntax probe checks only its declared parsing expectation. A semantic reference model tests a selected algorithm outside an Orna implementation. A full implementation test additionally resolves, checks effects/types, executes and verifies results in the specified environment. These evidence levels MUST NOT be conflated.

The machine-readable evidence register maps each executed check to its actual subject, command and result. A requirement-family label is organisational metadata, not proof that every requirement in that family was exercised. Unexecuted implementation and interoperability obligations remain explicitly unexecuted.

**ORNA-EVIDENCE-001** A release report MUST distinguish authored conformance cases from executed implementation evidence and MUST NOT infer a pass from a filename, requirement association, file count or manifest validity.

**ORNA-EVIDENCE-002** A complete example project MUST include every required module/schema or pin an available dependency with an exact interface. Placeholder connector attachment points do not make an example self-contained.


## Scope of conformance

**ORNA-CLOSURE-002** Benchmark, cross-platform, interoperability, security and fault obligations determine an implementation's evidence for its claimed profile. They MUST NOT be used as permission to choose different observable semantics.

**ORNA-CLOSURE-003** Features outside the declared language/profile are absent, not implicitly implementation-defined. A source program relying on such a feature MUST receive an unsupported-feature or resolution diagnostic rather than silently acquiring vendor-specific behaviour under the same portable coordinate.

Conformance requires agreement with all applicable normative provisions. Report conflicting provisions as specification defects.

## 1.1 binary and source-hosted profiles

The active source/API/profile and `tests/final-scenarios.json` define the
current 1.1.0 final obligations. `tests/legacy-scenarios.json` and the exact
compatibility ZIP preserve historical test inputs, not current results.
`tools/run_checks.py` executes structural/model/native-Git publication checks;
these do not establish Orna engine, FUSE, crash-safety or scale conformance.
Profile claims must identify the artifact hash and actual executed tests.

# 40. Diagnostic reference {#diagnostics}

## Invalid forms and corrections

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



## Automated corrections

**ORNA-MIGRATE-003** Removal of postfix `?`, replacement of a function return arrow, and `var` to `let` are mechanical only when token-aware parsing proves the context.

**ORNA-MIGRATE-004** A tool MUST NOT rewrite anonymous-function `=>` while migrating case arms.


## Diagnostic classifications

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

# 41. System API reference {#system-reference}

This reference defines system names, field types and callable interfaces. The [system model](#system) explains identity, snapshots, reflection and redaction; [administration](#administration) specifies state-changing preconditions. A machine-readable schema is available in [`api/sys.json`](api/sys.json).

Signatures use qualified callable names and type parameters such as `T`. Source declaration syntax is defined by the [grammar](#grammar). Optional types use `?`; their absence value is `null`. All relation rows are read-only.

Field types and declared invariants apply together. Source, host-path and payload fields also follow availability and redaction rules. `null` represents defined absence; unavailable mandatory data produces the specified failure.

## Namespace index

| Group | Use |
|---|---|
| `sys.database`, `sys.current`, `sys.rt`, `sys.repl` | Current attachment, activation, runtime and optional REPL observations. |
| `sys.catalog` | Declarations, types, source and dependencies. |
| `sys.history`, `sys.git` | Snapshots, semantic changes and Git objects. |
| `sys.storage`, `sys.build` | Single-store physical observations and build/test metadata. |
| Root functions | Typed resolution, inspection, invocation and execution control. |
| `sys.admin` | Explicit state transitions; never row setters. |

## Singleton views

| Name | Type | Availability |
|---|---|---|
| `sys.database` | `sys.DatabaseView` | always while a database is attached |
| `sys.current` | `sys.CurrentContext` | every activation |
| `sys.rt` | `sys.RuntimeView` | every command/runtime owner |
| `sys.repl` | `sys.ReplView` | REPL only; otherwise sys.context.repl_unavailable |

## Opaque identifiers

An opaque identifier cannot be implicitly substituted for a string, UUID or another identifier. Its domain and canonical encoding remain part of the named type.

### `sys.DatabaseId` {#api-type-sys-databaseid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.FileId` {#api-type-sys-fileid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.DefinitionId` {#api-type-sys-definitionid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.ObjectId` {#api-type-sys-objectid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.RevisionId` {#api-type-sys-revisionid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.SnapshotId` {#api-type-sys-snapshotid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.RuntimeId` {#api-type-sys-runtimeid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.TransactionId` {#api-type-sys-transactionid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.InvocationId` {#api-type-sys-invocationid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.RunId` {#api-type-sys-runid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.QueryId` {#api-type-sys-queryid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.SessionId` {#api-type-sys-sessionid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.ClientId` {#api-type-sys-clientid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.TraceId` {#api-type-sys-traceid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.SpanId` {#api-type-sys-spanid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.CheckpointVersion` {#api-type-sys-checkpointversion}

Nominal identity value; see [identity and encoding](#formats).

### `sys.SegmentId` {#api-type-sys-segmentid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.BuildId` {#api-type-sys-buildid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.GitOid` {#api-type-sys-gitoid}

Nominal identity value; see [identity and encoding](#formats).

### `sys.FailureVersion` {#api-type-sys-failureversion}

Nominal identity value; see [identity and encoding](#formats).

### `sys.ConsumerIdentity` {#api-type-sys-consumeridentity}

Nominal identity value; see [identity and encoding](#formats).

## Typed row-reference aliases

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

## Closed enumerations

### `sys.RuntimeMode` {#api-type-sys-runtimemode}



Values: `repl`, `run`, `serve`, `command_owner`, `embedded`.

### `sys.ObjectKind` {#api-type-sys-objectkind}



Values: `namespace`, `module`, `table`, `column`, `key`, `assertion`, `function`, `parameter`, `type`, `field`, `variant`, `protocol`, `implementation`, `page`, `dimension`, `unit`, `currency`, `secret_requirement`, `extension`.

### `sys.Visibility` {#api-type-sys-visibility}



Values: `private`, `module`, `package`, `public`.

### `sys.Severity` {#api-type-sys-severity}



Values: `note`, `help`, `warning`, `error`, `fatal`.

### `sys.SnapshotKind` {#api-type-sys-snapshotkind}



Values: `commit`, `logical_cwd`, `synthetic_merge`, `build_input`.

### `sys.ChangeKind` {#api-type-sys-changekind}



Values: `add`, `modify`, `delete`, `rename`, `rekey`, `rewrite`.

### `sys.ChangeArea` {#api-type-sys-changearea}



Values: `cwd`, `staged`, `commit`, `merge`, `publication`, `storage`.

### `sys.RunStatus` {#api-type-sys-runstatus}



Values: `starting`, `running`, `completed`, `failed`, `cancelled`, `orphaned`.

### `sys.InvocationStatus` {#api-type-sys-invocationstatus}



Values: `queued`, `running`, `succeeded`, `failed`, `cancelled`, `orphaned`.

### `sys.TransactionStatus` {#api-type-sys-transactionstatus}



Values: `active`, `committing`, `committed`, `rolling_back`, `rolled_back`, `failed`.

### `sys.StreamStatus` {#api-type-sys-streamstatus}



Values: `starting`, `running`, `paused`, `backing_off`, `completed`, `failed`, `cancelled`, `orphaned`.

### `sys.FailureStatus` {#api-type-sys-failurestatus}



Values: `open`, `retrying`, `recovered`, `skipped`, `replaying`, `replayed`, `resolved`.

### `sys.LeaseStatus` {#api-type-sys-leasestatus}



Values: `acquiring`, `held`, `releasing`, `expired`, `lost`.

### `sys.BuildStatus` {#api-type-sys-buildstatus}



Values: `queued`, `running`, `passed`, `failed`, `cancelled`, `orphaned`.

### `sys.TestStatus` {#api-type-sys-teststatus}



Values: `not_run`, `running`, `passed`, `failed`, `skipped`.

### `sys.AssertionOwnerKind` {#api-type-sys-assertionownerkind}



Values: `executable`, `refined_type`, `table`, `module`.

### `sys.AssertionScope` {#api-type-sys-assertionscope}



Values: `activation`, `value_construction`, `transaction_candidate`, `snapshot_candidate`.

### `sys.ClientKind` {#api-type-sys-clientkind}



Values: `cli`, `repl`, `server`, `renderer`, `embedded`, `tool`.

### `sys.DependencyConfidence` {#api-type-sys-dependencyconfidence}



Values: `exact`, `conservative`, `possible`.

### `sys.DependencyKind` {#api-type-sys-dependencykind}



Values: `import`, `type_reference`, `call`, `table_read`, `table_write`, `assertion`, `implementation`, `page_entry`, `renderer_requirement`, `extension_import`, `storage_projection`.

### `sys.DiffScope` {#api-type-sys-diffscope}



Values: `all`, `semantic`, `source`, `rows`, `storage`.

### `sys.EffectKind` {#api-type-sys-effectkind}



Values: `table_read`, `table_write`, `network`, `filesystem`, `process`, `clock`, `randomness`, `secret_access`, `ui`, `git`, `storage_admin`, `runtime_admin`.

### `sys.ExtensionTrust` {#api-type-sys-extensiontrust}



Values: `sandboxed`, `trusted`.

### `sys.FileKind` {#api-type-sys-filekind}



Values: `module`, `row`, `manifest`, `package_manifest`, `storage_manifest`, `compact_segment`, `asset`, `secret_document`, `generated`, `other`.

### `sys.GitObjectKind` {#api-type-sys-gitobjectkind}



Values: `commit`, `tree`, `blob`, `tag`.

### `sys.HostAccess` {#api-type-sys-hostaccess}



Values: `filesystem_read`, `filesystem_write`, `network`, `process`, `environment`, `clock`, `device`.

### `sys.IndexKind` {#api-type-sys-indexkind}



Values: `primary`, `btree`, `hash`, `full_text`, `vector`, `provider`.

### `sys.InvokeTransaction` {#api-type-sys-invoketransaction}



Values: `inherit`, `separate`, `read_only`.

### `sys.ListenerKind` {#api-type-sys-listenerkind}



Values: `git_http`, `query_http`, `websocket`, `renderer`, `admin`, `custom`.

### `sys.MaintenanceKind` {#api-type-sys-maintenancekind}



Values: `flush`, `compact`, `verify`, `hydrate`, `prune`, `checkpoint_publish`, `storage_rewrite`.

### `sys.PlanNodeKind` {#api-type-sys-plannodekind}



Values: `scan`, `index_lookup`, `filter`, `project`, `join`, `aggregate`, `sort`, `limit`, `materialize`, `invoke`, `assertion_validate`, `checkpoint_update`, `external`.

### `sys.ProtocolMemberKind` {#api-type-sys-protocolmemberkind}



Values: `function`, `static_property`.

### `sys.ReferenceAction` {#api-type-sys-referenceaction}



Values: `restrict`, `cascade`, `set_none`.

### `sys.SettingScope` {#api-type-sys-settingscope}



Values: `runtime`, `database`, `repository`, `session`.

### `sys.SettingSource` {#api-type-sys-settingsource}



Values: `default`, `manifest`, `environment`, `command_line`, `local_config`.

### `sys.StatisticKind` {#api-type-sys-statistickind}



Values: `min`, `max`, `null_count`, `distinct_count`, `bloom`, `histogram`.

### `sys.StorageFileKind` {#api-type-sys-storagefilekind}



Values: `cwd_database`, `wal`, `lock`, `socket`, `segment_cache`, `temporary`, `identity`, `other`.

### `sys.StorageProfile` {#api-type-sys-storageprofile}



Values: `indexed`.

### `sys.TypeKind` {#api-type-sys-typekind}



Values: `alias`, `nominal`, `refined`, `enum`, `protocol`, `table_row`, `generic_parameter`, `constructed`.

### `sys.VerificationStatus` {#api-type-sys-verificationstatus}



Values: `unknown`, `pending`, `valid`, `invalid`.

### `sys.VerifyScope` {#api-type-sys-verifyscope}



Values: `database`, `repository`, `storage`, `history`, `checkpoints`, `all`.

### `sys.ChangeTargetKind` {#api-type-sys-changetargetkind}



Values: `object`, `file`, `row`.

## Supporting value types

### `sys.RowRef<T>` {#api-type-sys-rowref-t}

Snapshot/runtime-pinned reference to one canonical system-relation row.

This type has no public structural fields; use the operations that explicitly accept it.

Invariant: preserves the row natural key and resolution context.

Invariant: does not grant authority.

Invariant: live references are runtime-generation-bound.

### `sys.Value` {#api-type-sys-value}

Explicit existential envelope for a value crossing a reflective system boundary.

| Field | Type |
|---|---|
| `type` | `sys.TypeRef` |
| `redacted` | `Bool` |
| `canonical_digest` | `Digest?` |

Invariant: is not a dynamic Any fallback.

Invariant: retains the exact originating static type.

Invariant: cannot be implicitly unboxed or converted.

### `sys.Argument` {#api-type-sys-argument}

One named typed reflective-call argument.

| Field | Type |
|---|---|
| `name` | `Str` |
| `value` | `sys.Value` |

### `sys.ArgumentMap` {#api-type-sys-argumentmap}

Immutable ordered collection of uniquely named reflective-call arguments.

| Field | Type |
|---|---|
| `entries` | `[sys.Argument]` |

Invariant: names are unique.

Invariant: canonical order is Unicode scalar-value order by name.

Invariant: record literals are boxed only under an ArgumentMap expected type.

### `sys.ValueMetadata<T>` {#api-type-sys-valuemetadata-t}

Safe type, protocol and codec metadata for a value without private-field disclosure.

| Field | Type |
|---|---|
| `static_type` | `sys.TypeRef` |
| `nominal_type` | `sys.TypeRef?` |
| `protocols` | `Relation<sys.Protocol>` |
| `codecs` | `[Str]` |
| `redacted` | `Bool` |

### `sys.InvocationHandle<T>` {#api-type-sys-invocationhandle-t}

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

### `sys.InvocationResult<T>` {#api-type-sys-invocationresult-t}

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

### `sys.ExpressionRef` {#api-type-sys-expressionref}

Snapshot-pinned reference to a resolved expression in a definition.

| Field | Type |
|---|---|
| `owner` | `sys.ObjectRef` |
| `definition` | `sys.DefinitionRef` |
| `span` | `sys.SourceSpan` |
| `semantic_hash` | `Digest` |

### `sys.SourceSpan` {#api-type-sys-sourcespan}

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

### `sys.SourceMapEntry` {#api-type-sys-sourcemapentry}

Mapping from a generated span to an authored span and optional original name.

| Field | Type |
|---|---|
| `generated` | `sys.SourceSpan` |
| `original` | `sys.SourceSpan?` |
| `original_name` | `Str?` |

### `sys.DiagnosticLabel` {#api-type-sys-diagnosticlabel}

One source label attached to a structured diagnostic.

| Field | Type |
|---|---|
| `span` | `sys.SourceSpan` |
| `message` | `Str` |
| `primary` | `Bool` |

### `sys.SourceDocument` {#api-type-sys-sourcedocument}

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

### `sys.Attribution` {#api-type-sys-attribution}

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

### `sys.PersonIdentity` {#api-type-sys-personidentity}

Git-compatible authored identity without signing or authorization semantics.

| Field | Type |
|---|---|
| `name` | `Str` |
| `email` | `Str?` |

### `sys.ObjectDescription` {#api-type-sys-objectdescription}

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

### `sys.Explanation` {#api-type-sys-explanation}

Structured causal explanation of a diagnostic.

| Field | Type |
|---|---|
| `diagnostic` | `sys.Diagnostic` |
| `summary` | `Str` |
| `causes` | `[sys.Diagnostic]` |
| `suggestions` | `[Str]` |
| `related_objects` | `[sys.ObjectRef]` |
| `plan` | `sys.PlanRef?` |

### `sys.CheckpointPosition` {#api-type-sys-checkpointposition}

Provider-specific replay position with portable equality and canonical encoding.

| Field | Type |
|---|---|
| `format` | `Str` |
| `digest` | `Digest` |

Invariant: payload is not generically orderable.

Invariant: equality includes format and canonical payload.

Invariant: provider APIs own construction and comparison beyond equality.

### `sys.CompatibilityInfo` {#api-type-sys-compatibilityinfo}

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

### `sys.RuntimeInfo` {#api-type-sys-runtimeinfo}

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

### `sys.FlushResult` {#api-type-sys-flushresult}

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

### `sys.CompactionResult` {#api-type-sys-compactionresult}

Physical maintenance result for the one indexed profile; segment-named fields refer to observed bounded storage pages.

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

### `sys.VerificationReport` {#api-type-sys-verificationreport}

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

### `sys.CancellationView` {#api-type-sys-cancellationview}

Immutable activation-local cancellation observation.

| Field | Type |
|---|---|
| `requested` | `Bool` |
| `reason` | `Str?` |
| `requested_at` | `Instant?` |

### `sys.PresentationOverride` {#api-type-sys-presentationoverride}

Session-local presentation selection for one type.

| Field | Type |
|---|---|
| `type` | `sys.TypeRef` |
| `renderer` | `Str?` |
| `mode` | `Str` |
| `formatter` | `sys.FunctionRef?` |

### `sys.Watch` {#api-type-sys-watch}

REPL watch expression and its most recent safe observation.

| Field | Type |
|---|---|
| `expression` | `sys.ExpressionRef` |
| `label` | `Str?` |
| `last_value` | `sys.Value?` |
| `last_diagnostic` | `sys.Diagnostic?` |
| `updated` | `Instant?` |

### `sys.DatabaseView` {#api-type-sys-databaseview}

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

### `sys.CurrentContext` {#api-type-sys-currentcontext}

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

### `sys.RuntimeView` {#api-type-sys-runtimeview}

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

### `sys.ReplView` {#api-type-sys-replview}

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

### `sys.ChangeTarget` {#api-type-sys-changetarget}

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

### `sys.CheckpointConflict` {#api-type-sys-checkpointconflict}

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

### `sys.RowConflict` {#api-type-sys-rowconflict}

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

### `sys.CheckoutPlan` {#api-type-sys-checkoutplan}

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

### `sys.BlobWriter` {#api-type-sys-blobwriter}

Nonserialisable owner/runtime-bound incremental Blob capture.

This type has no public structural fields; use the operations that explicitly accept it.

Invariant: states are open, finished, aborted.

Invariant: a handle is not a stored Blob or a portable reference.

Invariant: declared total and per-call limits are enforced before mutation.

Invariant: finish protects and returns immutable content; no user row or branch is implicitly created.

### `sys.CommitReview` {#api-type-sys-commitreview}

Non-authorising, state-bound review of the exact staged Git candidate.

| Field | Type |
|---|---|
| `token` | `Digest` |
| `head` | `sys.GitOid?` |
| `head_ref` | `Str?` |
| `index_tree` | `sys.GitOid` |
| `index_digest` | `Digest` |
| `worktree_digest` | `Digest` |
| `cwd` | `sys.SnapshotRef` |
| `pending_generation` | `Int` |

Invariant: all fields and the exact format coordinate enter the review token.

Invariant: supplying the record does not grant administrative authority.

Invariant: an altered or stale review fails before ref visibility.

## Canonical system relations

### `sys.Database` {#api-type-sys-database}

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

### `sys.Namespace` {#api-type-sys-namespace}

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

### `sys.Module` {#api-type-sys-module}

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

### `sys.Import` {#api-type-sys-import}

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

### `sys.File` {#api-type-sys-file}

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

### `sys.Object` {#api-type-sys-object}

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

### `sys.Revision` {#api-type-sys-revision}

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

### `sys.Definition` {#api-type-sys-definition}

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

### `sys.Type` {#api-type-sys-type}

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

### `sys.TypeParameter` {#api-type-sys-typeparameter}

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

### `sys.Field` {#api-type-sys-field}

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

### `sys.Variant` {#api-type-sys-variant}

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

### `sys.Protocol` {#api-type-sys-protocol}

Protocol declaration metadata.

**Grouped handle:** `sys.catalog.protocols`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.ProtocolRef`.

| Field | Type |
|---|---|
| `reference` | `sys.ProtocolRef` |
| `object` | `sys.ObjectRef` |
| `parameters` | `Relation<sys.TypeParameter>` |
| `members` | `Relation<sys.ProtocolMember>` |
| `docs` | `Str?` |

### `sys.ProtocolMember` {#api-type-sys-protocolmember}

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

### `sys.Implementation` {#api-type-sys-implementation}

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

### `sys.Function` {#api-type-sys-function}

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

### `sys.Parameter` {#api-type-sys-parameter}

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

### `sys.Effect` {#api-type-sys-effect}

Conservative static effect metadata.

**Grouped handle:** `sys.catalog.effects`. **Availability:** catalogue. **Natural key:** `function + kind + target`. **Reference:** `sys.EffectRef`.

| Field | Type |
|---|---|
| `reference` | `sys.EffectRef` |
| `function` | `sys.FunctionRef` |
| `kind` | `sys.EffectKind` |
| `target` | `sys.ObjectRef?` |
| `conditional` | `Bool` |

### `sys.Table` {#api-type-sys-table}

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

### `sys.Column` {#api-type-sys-column}

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

### `sys.Key` {#api-type-sys-key}

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

### `sys.Assertion` {#api-type-sys-assertion}

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

### `sys.Reference` {#api-type-sys-reference}

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

### `sys.Page` {#api-type-sys-page}

Function-derived page entry metadata.

**Grouped handle:** `sys.catalog.pages`. **Availability:** catalogue. **Natural key:** `function`. **Reference:** `sys.PageRef`.

| Field | Type |
|---|---|
| `reference` | `sys.PageRef` |
| `function` | `sys.FunctionRef` |
| `path` | `Str` |
| `dependencies` | `Relation<sys.Dependency>` |
| `definition` | `sys.DefinitionRef` |

### `sys.Dimension` {#api-type-sys-dimension}

Reduced physical dimension declaration.

**Grouped handle:** `sys.catalog.dimensions`. **Availability:** catalogue. **Natural key:** `object`. **Reference:** `sys.DimensionRef`.

| Field | Type |
|---|---|
| `reference` | `sys.DimensionRef` |
| `object` | `sys.ObjectRef` |
| `exponents` | `sys.Value` |
| `definition` | `sys.DefinitionRef` |

### `sys.Unit` {#api-type-sys-unit}

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

### `sys.Currency` {#api-type-sys-currency}

Nominal type implementing Currency.

**Grouped handle:** `sys.catalog.currencies`. **Availability:** catalogue. **Natural key:** `type`. **Reference:** `sys.CurrencyRef`.

| Field | Type |
|---|---|
| `reference` | `sys.CurrencyRef` |
| `type` | `sys.TypeRef` |
| `code` | `Str` |
| `minor_digits` | `Int` |
| `definition` | `sys.DefinitionRef` |

### `sys.SecretRequirement` {#api-type-sys-secretrequirement}

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

### `sys.Extension` {#api-type-sys-extension}

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

### `sys.Dependency` {#api-type-sys-dependency}

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

### `sys.Diagnostic` {#api-type-sys-diagnostic}

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

### `sys.Snapshot` {#api-type-sys-snapshot}

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

### `sys.SnapshotObject` {#api-type-sys-snapshotobject}

Object revision membership in a snapshot.

**Grouped handle:** `sys.history.snapshot_objects`. **Availability:** catalogue. **Natural key:** `snapshot + object`. **Reference:** `sys.SnapshotObjectRef`.

| Field | Type |
|---|---|
| `reference` | `sys.SnapshotObjectRef` |
| `snapshot` | `sys.SnapshotRef` |
| `object` | `sys.ObjectRef` |
| `revision` | `sys.RevisionRef` |
| `reachable` | `Bool` |

### `sys.Change` {#api-type-sys-change}

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

### `sys.DiffEntry` {#api-type-sys-diffentry}

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

### `sys.FileVersion` {#api-type-sys-fileversion}

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

### `sys.GitRepository` {#api-type-sys-gitrepository}

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

### `sys.Commit` {#api-type-sys-commit}

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

### `sys.TreeEntry` {#api-type-sys-treeentry}

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

### `sys.Ref` {#api-type-sys-ref}

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

### `sys.Branch` {#api-type-sys-branch}

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

### `sys.Tag` {#api-type-sys-tag}

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

### `sys.Remote` {#api-type-sys-remote}

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

### `sys.Stash` {#api-type-sys-stash}

Git stash projection.

**Grouped handle:** `sys.git.stashes`. **Availability:** catalogue. **Natural key:** `index`. **Reference:** `sys.StashRef`.

| Field | Type |
|---|---|
| `reference` | `sys.StashRef` |
| `index` | `Int` |
| `commit` | `sys.CommitRef` |
| `message` | `Str` |
| `created` | `Instant?` |

### `sys.GitObject` {#api-type-sys-gitobject}

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

### `sys.WorktreeEntry` {#api-type-sys-worktreeentry}

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

### `sys.Query` {#api-type-sys-query}

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

### `sys.Plan` {#api-type-sys-plan}

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

### `sys.PlanNode` {#api-type-sys-plannode}

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

### `sys.Invocation` {#api-type-sys-invocation}

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

### `sys.InvocationArgument` {#api-type-sys-invocationargument}

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

### `sys.Run` {#api-type-sys-run}

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

### `sys.Transaction` {#api-type-sys-transaction}

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

### `sys.Trace` {#api-type-sys-trace}

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

### `sys.Span` {#api-type-sys-span}

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

### `sys.TraceEvent` {#api-type-sys-traceevent}

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

### `sys.Stream` {#api-type-sys-stream}

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

### `sys.Checkpoint` {#api-type-sys-checkpoint}

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

### `sys.CheckpointUpdate` {#api-type-sys-checkpointupdate}

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

### `sys.Failure` {#api-type-sys-failure}

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

### `sys.Session` {#api-type-sys-session}

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

### `sys.Client` {#api-type-sys-client}

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

### `sys.Lease` {#api-type-sys-lease}

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

### `sys.Listener` {#api-type-sys-listener}

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

### `sys.Storage` {#api-type-sys-storage}

Observed single indexed row-store root, generation and availability; not a placement policy.

**Grouped handle:** `sys.storage.objects`. **Availability:** local-durable. **Natural key:** `object`. **Reference:** `sys.StorageRef`.

| Field | Type |
|---|---|
| `reference` | `sys.StorageRef` |
| `object` | `sys.ObjectRef` |
| `profile` | `sys.StorageProfile` |
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
| `root` | `sys.GitOid?` |
| `schema_digest` | `Digest` |

### `sys.Segment` {#api-type-sys-segment}

Compatibility-named bounded immutable row/index-page observation; no separate compact storage writer.

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

### `sys.Statistic` {#api-type-sys-statistic}

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

### `sys.Index` {#api-type-sys-index}

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

### `sys.Materialization` {#api-type-sys-materialization}

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

### `sys.Allocator` {#api-type-sys-allocator}

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

### `sys.Hydration` {#api-type-sys-hydration}

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

### `sys.Compaction` {#api-type-sys-compaction}

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

### `sys.MaintenanceJob` {#api-type-sys-maintenancejob}

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

### `sys.StorageFile` {#api-type-sys-storagefile}

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

### `sys.Secret` {#api-type-sys-secret}

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

### `sys.Setting` {#api-type-sys-setting}

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

### `sys.Build` {#api-type-sys-build}

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

### `sys.Test` {#api-type-sys-test}

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

## Callable operations

Read operations do not mutate database state. Invocation/start operations follow [INVOKE-1](#system); admin operations follow [administrative transitions](#administration). Resolution and retained-data failures are ordinary typed failures, not missing/empty successful results.

### `sys.meta` {#api-call-sys-meta}

Return safe static/nominal/codec/protocol metadata for a value.

```text
fn sys.meta<T>(value: T): sys.ValueMetadata<T>
```

**Effect:** read.

### `sys.object` {#api-call-sys-object}

Look up a stable object at a snapshot.

```text
fn sys.object(id: sys.ObjectId, at: sys.SnapshotRef = sys.current.snapshot): sys.ObjectRef
```

**Effect:** read.

### `sys.resolve` {#api-call-sys-resolve}

Resolve a semantic name with ordinary visibility/import rules.

```text
fn sys.resolve(name: Str, kind: sys.ObjectKind? = null, at: sys.SnapshotRef = sys.current.snapshot, from: sys.ModuleRef? = null): sys.ObjectRef
```

**Effect:** read.

### `sys.resolve_function` {#api-call-sys-resolve-function}

Resolve a function reference.

```text
fn sys.resolve_function(name: Str, at: sys.SnapshotRef = sys.current.snapshot, from: sys.ModuleRef? = null): sys.FunctionRef
```

**Effect:** read.

### `sys.describe` {#api-call-sys-describe}

Return structured object description.

```text
fn sys.describe(object: sys.ObjectRef): sys.ObjectDescription
```

**Effect:** read.

### `sys.source(ObjectRef)` {#api-call-sys-source-objectref}

Return retained exact source/source maps for an object subject to redaction.

```text
fn sys.source(object: sys.ObjectRef): sys.SourceDocument
```

**Effect:** read.

### `sys.source(FileRef)` {#api-call-sys-source-fileref}

Return retained exact source/source maps for a file subject to redaction.

```text
fn sys.source(file: sys.FileRef): sys.SourceDocument
```

**Effect:** read.

### `sys.history(ObjectRef)` {#api-call-sys-history-objectref}

Return stable semantic-object revision history.

```text
fn sys.history(object: sys.ObjectRef): Relation<sys.Revision>
```

**Effect:** read.

### `sys.history(FileRef)` {#api-call-sys-history-fileref}

Return retained versions of one repository file.

```text
fn sys.history(file: sys.FileRef): Relation<sys.FileVersion>
```

**Effect:** read.

### `sys.blame(FileRef)` {#api-call-sys-blame-fileref}

Return source attribution.

```text
fn sys.blame(target: sys.FileRef): Relation<sys.Attribution>
```

**Effect:** read.

### `sys.blame(RowRef)` {#api-call-sys-blame-rowref}

Return semantic row/field attribution.

```text
fn sys.blame<T>(target: sys.RowRef<T>): Relation<sys.Attribution>
```

**Effect:** read.

### `sys.dependencies` {#api-call-sys-dependencies}

Traverse outgoing dependency edges.

```text
fn sys.dependencies(object: sys.ObjectRef, transitive: Bool = false, kinds: [sys.DependencyKind]? = null): Relation<sys.Dependency>
```

**Effect:** read.

### `sys.dependents` {#api-call-sys-dependents}

Traverse incoming dependency edges.

```text
fn sys.dependents(object: sys.ObjectRef, transitive: Bool = false, kinds: [sys.DependencyKind]? = null): Relation<sys.Dependency>
```

**Effect:** read.

### `sys.snapshot(SnapshotRef)` {#api-call-sys-snapshot-snapshotref}

Return an already resolved snapshot reference.

```text
fn sys.snapshot(reference: sys.SnapshotRef = sys.database.cwd): sys.SnapshotRef
```

**Effect:** read.

### `sys.snapshot(CommitRef)` {#api-call-sys-snapshot-commitref}

Resolve a commit snapshot.

```text
fn sys.snapshot(reference: sys.CommitRef): sys.SnapshotRef
```

**Effect:** read.

### `sys.snapshot(BranchRef)` {#api-call-sys-snapshot-branchref}

Resolve the branch target observed by the reference.

```text
fn sys.snapshot(reference: sys.BranchRef): sys.SnapshotRef
```

**Effect:** read.

### `sys.snapshot(TagRef)` {#api-call-sys-snapshot-tagref}

Resolve the peeled tag target.

```text
fn sys.snapshot(reference: sys.TagRef): sys.SnapshotRef
```

**Effect:** read.

### `sys.snapshot(GitOid)` {#api-call-sys-snapshot-gitoid}

Resolve an exact Git object identifier.

```text
fn sys.snapshot(reference: sys.GitOid): sys.SnapshotRef
```

**Effect:** read.

### `sys.snapshot(Str)` {#api-call-sys-snapshot-str}

Resolve a Git revision expression in the attached repository.

```text
fn sys.snapshot(reference: Str): sys.SnapshotRef
```

**Effect:** read.

### `sys.diff` {#api-call-sys-diff}

Compute semantic/source/row diff.

```text
fn sys.diff(from: sys.SnapshotRef, to: sys.SnapshotRef, scope: sys.DiffScope = sys.DiffScope.all): Relation<sys.DiffEntry>
```

**Effect:** read.

### `sys.changes` {#api-call-sys-changes}

Report changes represented by/relative to a state.

```text
fn sys.changes(snapshot: sys.SnapshotRef = sys.database.cwd, area: sys.ChangeArea? = null): Relation<sys.Change>
```

**Effect:** read.

### `sys.explain(Query)` {#api-call-sys-explain-query}

Return structured query plan.

```text
fn sys.explain<T>(query: Query<T>): sys.Plan
```

**Effect:** read.

### `sys.explain(FunctionRef)` {#api-call-sys-explain-functionref}

Return structured function/effect plan.

```text
fn sys.explain(function: sys.FunctionRef): sys.Plan
```

**Effect:** read.

### `sys.explain(Diagnostic)` {#api-call-sys-explain-diagnostic}

Return structured causal explanation.

```text
fn sys.explain(diagnostic: sys.Diagnostic): sys.Explanation
```

**Effect:** read.

### `sys.checkpoint` {#api-call-sys-checkpoint}

Read a checkpoint snapshot.

```text
fn sys.checkpoint(consumer_identity: sys.ConsumerIdentity, source_identity: Str, partition: Str? = null): sys.Checkpoint?
```

**Effect:** read.

### `sys.render_diagnostic` {#api-call-sys-render-diagnostic}

Render without altering structured diagnostic.

```text
fn sys.render_diagnostic(diagnostic: sys.Diagnostic, context: PresentContext = sys.current.present_context): PresentTree
```

**Effect:** read.

### `sys.rt.info` {#api-call-sys-rt-info}

Return exact language/sys/storage/protocol compatibility coordinates.

```text
fn sys.rt.info(): sys.RuntimeInfo
```

**Effect:** read.

### `sys.invoke(Value)` {#api-call-sys-invoke-value}

Reflectively invoke and return an explicitly erased value envelope.

```text
fn sys.invoke(function: sys.FunctionRef, arguments: sys.ArgumentMap, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.inherit, idempotency_key: Str? = null): sys.Value
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.invoke<T>` {#api-call-sys-invoke-t}

Reflectively invoke after validating the declared result against an explicit type witness.

```text
fn sys.invoke<T>(function: sys.FunctionRef, arguments: sys.ArgumentMap, as: T, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.inherit, idempotency_key: Str? = null): T
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.start(Value)` {#api-call-sys-start-value}

Start an awaitable invocation with an explicitly erased result.

```text
fn sys.start(function: sys.FunctionRef, arguments: sys.ArgumentMap, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.separate, idempotency_key: Str? = null): sys.InvocationHandle<sys.Value>
```

**Effect:** invoke.

Ownership: Current operation owns the child, except a direct REPL sys.start expression/binding is session-owned. Inherit transaction mode is rejected; separate/read_only are permitted.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.start<T>` {#api-call-sys-start-t}

Start an awaitable invocation after validating an explicit result type witness.

```text
fn sys.start<T>(function: sys.FunctionRef, arguments: sys.ArgumentMap, as: T, at: sys.SnapshotRef? = null, transaction: sys.InvokeTransaction = sys.InvokeTransaction.separate, idempotency_key: Str? = null): sys.InvocationHandle<T>
```

**Effect:** invoke.

Ownership: Current operation owns the child, except a direct REPL sys.start expression/binding is session-owned. Inherit transaction mode is rejected; separate/read_only are permitted.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.await` {#api-call-sys-await}

Wait for a terminal result; timeout does not cancel.

```text
fn sys.await<T>(invocation: sys.InvocationHandle<T>, timeout: Duration? = null): sys.InvocationResult<T>
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.cancel` {#api-call-sys-cancel}

Idempotently request invocation cancellation.

```text
fn sys.cancel<T>(invocation: sys.InvocationHandle<T>, reason: Str? = null): Bool
```

**Effect:** invoke.

Binding, snapshot and result rules: [reflective invocation](#system). Owner lifetime: [execution](#execution).

### `sys.admin.commit` {#api-call-sys-admin-commit}

Commit the validated staged state, preserving unstaged CWD changes and unpublished tail.

```text
fn sys.admin.commit(message: Str, author: sys.PersonIdentity? = null): sys.CommitRef
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(SnapshotRef)` {#api-call-sys-admin-checkout-snapshotref}

Validate and select an already resolved snapshot.

```text
fn sys.admin.checkout(target: sys.SnapshotRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(CommitRef)` {#api-call-sys-admin-checkout-commitref}

Validate and select a commit snapshot.

```text
fn sys.admin.checkout(target: sys.CommitRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(BranchRef)` {#api-call-sys-admin-checkout-branchref}

Validate and select a branch target.

```text
fn sys.admin.checkout(target: sys.BranchRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(TagRef)` {#api-call-sys-admin-checkout-tagref}

Validate and select a peeled tag target.

```text
fn sys.admin.checkout(target: sys.TagRef, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(GitOid)` {#api-call-sys-admin-checkout-gitoid}

Validate and select an exact Git object.

```text
fn sys.admin.checkout(target: sys.GitOid, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.checkout(Str)` {#api-call-sys-admin-checkout-str}

Resolve, validate and select a Git revision expression.

```text
fn sys.admin.checkout(target: Str, force: Bool = false, expected_plan: Digest? = null): sys.SnapshotRef
```

**Effect:** admin.

Preconditions: Carry nonconflicting staged, unstaged and pending database changes. Refuse overwrite by default. force requires a matching plan token and explicit host consent; invalid target assertions are never bypassed.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(SnapshotRef)` {#api-call-sys-admin-create-branch-snapshotref}

Create a branch at the supplied committed snapshot or current HEAD; do not switch or commit pending changes.

```text
fn sys.admin.create_branch(name: Str, at: sys.SnapshotRef? = null): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(CommitRef)` {#api-call-sys-admin-create-branch-commitref}

Create a Git branch at a commit.

```text
fn sys.admin.create_branch(name: Str, at: sys.CommitRef): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(BranchRef)` {#api-call-sys-admin-create-branch-branchref}

Create a Git branch at another branch target.

```text
fn sys.admin.create_branch(name: Str, at: sys.BranchRef): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(TagRef)` {#api-call-sys-admin-create-branch-tagref}

Create a Git branch at a peeled tag target.

```text
fn sys.admin.create_branch(name: Str, at: sys.TagRef): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(GitOid)` {#api-call-sys-admin-create-branch-gitoid}

Create a Git branch at an exact Git object.

```text
fn sys.admin.create_branch(name: Str, at: sys.GitOid): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.create_branch(Str)` {#api-call-sys-admin-create-branch-str}

Resolve a Git revision expression and create a branch there.

```text
fn sys.admin.create_branch(name: Str, at: Str): sys.BranchRef
```

**Effect:** admin.

Preconditions: Target must resolve to a commit; name must be a valid non-existing Git branch. An unborn HEAD or uncommitted CWD target fails without mutation.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.flush` {#api-call-sys-admin-flush}

Durably seal pending rows without changing logical contents.

```text
fn sys.admin.flush(table: sys.TableRef? = null): sys.FlushResult
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.compact` {#api-call-sys-admin-compact}

Maintain/repack the current indexed profile without logical mutation or selecting a row mode.

```text
fn sys.admin.compact(table: sys.TableRef? = null): sys.CompactionResult
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.verify` {#api-call-sys-admin-verify}

Verify repository, metadata, storage and checkpoint invariants.

```text
fn sys.admin.verify(scope: sys.VerifyScope = sys.VerifyScope.database): sys.VerificationReport
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.cancel_run` {#api-call-sys-admin-cancel-run}

Cancel a durable program run.

```text
fn sys.admin.cancel_run(run: sys.RunRef, reason: Str? = null): Bool
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.pause_stream` {#api-call-sys-admin-pause-stream}

Pause at an item/batch transaction boundary.

```text
fn sys.admin.pause_stream(stream: sys.StreamRef, reason: Str? = null): Bool
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.resume_stream` {#api-call-sys-admin-resume-stream}

Resume a paused stream.

```text
fn sys.admin.resume_stream(stream: sys.StreamRef): Bool
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.reset_checkpoint` {#api-call-sys-admin-reset-checkpoint}

Compare-and-set checkpoint reset.

```text
fn sys.admin.reset_checkpoint(checkpoint: sys.CheckpointRef, expected_version: sys.CheckpointVersion, expected_position: sys.CheckpointPosition, to: sys.CheckpointPosition, reason: Str): sys.Checkpoint
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.retry_failure` {#api-call-sys-admin-retry-failure}

Lease and retry the same blocked delivery; keep identity stable across failed attempts.

```text
fn sys.admin.retry_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus = sys.FailureStatus.open): sys.InvocationHandle<sys.Value>
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.skip_failure` {#api-call-sys-admin-skip-failure}

Atomically skip a supported failed position and move progress.

```text
fn sys.admin.skip_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus, reason: Str): sys.Checkpoint
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.replay_failure` {#api-call-sys-admin-replay-failure}

Reprocess a preserved skipped delivery without rewinding the live checkpoint.

```text
fn sys.admin.replay_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus = sys.FailureStatus.skipped): sys.InvocationHandle<sys.Value>
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.resolve_failure` {#api-call-sys-admin-resolve-failure}

Resolve a preserved failure without marking its processing as successful.

```text
fn sys.admin.resolve_failure(failure: sys.FailureRef, expected_version: sys.FailureVersion, expected_status: sys.FailureStatus, reason: Str): sys.Failure
```

**Effect:** admin.

Preconditions: Atomically compare stable delivery identity, expected_version and expected_status. A stale observation fails before work or checkpoint movement.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.consumer_identity` {#api-call-sys-consumer-identity}

Derive durable consumer identity from database identity, stable function ObjectId and canonical typed arguments. Reject secret plaintext and unsupported argument values.

```text
fn sys.consumer_identity(function: sys.FunctionRef, arguments: sys.ArgumentMap): sys.ConsumerIdentity
```

**Effect:** read.

### `sys.admin.plan_checkout(SnapshotRef)` {#api-call-sys-admin-plan-checkout-snapshotref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.SnapshotRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.plan_checkout(CommitRef)` {#api-call-sys-admin-plan-checkout-commitref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.CommitRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.plan_checkout(BranchRef)` {#api-call-sys-admin-plan-checkout-branchref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.BranchRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.plan_checkout(TagRef)` {#api-call-sys-admin-plan-checkout-tagref}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.TagRef): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.plan_checkout(GitOid)` {#api-call-sys-admin-plan-checkout-gitoid}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: sys.GitOid): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.plan_checkout(Str)` {#api-call-sys-admin-plan-checkout-str}

Compute a nonmutating, state-bound checkout preview, preserving whether a branch or detached snapshot is selected.

```text
fn sys.admin.plan_checkout(target: Str): sys.CheckoutPlan
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.blob.length` {#api-call-sys-blob-length}

Return the exact byte count without hydrating payloads.

```text
fn sys.blob.length(value: Blob): Int
```

**Effect:** read.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.digest` {#api-call-sys-blob-digest}

Return the raw-content SHA-256 commitment, independent of layout; not a full-verification claim.

```text
fn sys.blob.digest(value: Blob): Digest
```

**Effect:** read.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.read` {#api-call-sys-blob-read}

Return bounded raw-byte slice with octet-stream/null annotation; no media decoding.

```text
fn sys.blob.read(value: Blob, offset: Int, length: Int): Blob
```

**Effect:** read.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.verify` {#api-call-sys-blob-verify}

Incrementally verify the complete content and its native dependency structure.

```text
fn sys.blob.verify(value: Blob): Unit
```

**Effect:** read.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.from_bytes` {#api-call-sys-blob-from-bytes}

Construct at most 8 MiB of bytes from integers in 0..255. The fallback annotation is application/octet-stream with no suffix; explicit annotate changes metadata only.

```text
fn sys.blob.from_bytes(values: [Int]): Blob
```

**Effect:** read.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.to_bytes` {#api-call-sys-blob-to-bytes}

Explicit bounded materialisation of at most 8 MiB.

```text
fn sys.blob.to_bytes(value: Blob): [Int]
```

**Effect:** read.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.begin` {#api-call-sys-blob-begin}

Begin an owned capture with a checked total bound.

```text
fn sys.blob.begin(expected_length: Int? = null, max_bytes: Int): sys.BlobWriter
```

**Effect:** invoke.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.append` {#api-call-sys-blob-append}

Append at most 8 MiB in order; enforce the total bound before acceptance.

```text
fn sys.blob.append(writer: sys.BlobWriter, part: Blob): Unit
```

**Effect:** invoke.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.finish` {#api-call-sys-blob-finish}

Seal, verify and protect the captured value without implicitly inserting a row. The fallback annotation is application/octet-stream with no suffix; explicit annotate changes metadata only.

```text
fn sys.blob.finish(writer: sys.BlobWriter): Blob
```

**Effect:** invoke.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.abort` {#api-call-sys-blob-abort}

Abort unfinished capture; never delete a returned value.

```text
fn sys.blob.abort(writer: sys.BlobWriter): Unit
```

**Effect:** invoke.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.capture_file` {#api-call-sys-blob-capture-file}

Capture an authorised regular file using the shared Blob writer and protection boundary. The fallback annotation is application/octet-stream with no suffix; explicit annotate changes metadata only.

```text
fn sys.blob.capture_file(root: Str, path: Str, max_bytes: Int): Blob
```

**Effect:** invoke.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.resource` {#api-call-sys-blob-resource}

Read a safe package-relative resource in the caller module's admitted source snapshot. The fallback annotation is application/octet-stream with no suffix; explicit annotate changes metadata only.

```text
fn sys.blob.resource(path: Str): Blob
```

**Effect:** read.

Contract: [Blob value and storage contract](#blob-values).

### `sys.admin.review_commit` {#api-call-sys-admin-review-commit}

Validate and bind the exact staged candidate without changing index, branch or user data.

```text
fn sys.admin.review_commit(): sys.CommitReview
```

**Effect:** read.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.admin.commit_reviewed` {#api-call-sys-admin-commit-reviewed}

Revalidate a reviewed candidate at protected publication and commit exactly that candidate.

```text
fn sys.admin.commit_reviewed(review: sys.CommitReview, message: Str, author: sys.PersonIdentity? = null): sys.CommitRef
```

**Effect:** admin.

Contract: [Administrative transition contract](#administration).

State, cancellation, compare-and-set, audit and rollback rules: [administrative operations](#administration).

### `sys.blob.media_type` {#api-call-sys-blob-media-type}

Read canonical declared MIME without payload hydration.

```text
fn sys.blob.media_type(value: Blob): Str
```

**Effect:** read.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.suffix` {#api-call-sys-blob-suffix}

Read the canonical optional suffix hint without payload hydration.

```text
fn sys.blob.suffix(value: Blob): Str?
```

**Effect:** read.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.annotate` {#api-call-sys-blob-annotate}

Return the same immutable bytes with MIME-1-validated canonical annotations.

```text
fn sys.blob.annotate(value: Blob, media_type: Str, suffix: Str? = null): Blob
```

**Effect:** read.

Contract: [Blob value and storage contract](#blob-values).

### `sys.blob.same_content` {#api-call-sys-blob-same-content}

Exact content-only equality under admitted-witness/collision rules; may stream bytes and fail unavailable.

```text
fn sys.blob.same_content(left: Blob, right: Blob): Bool
```

**Effect:** read.

Contract: [Blob value and storage contract](#blob-values).

## Portable failure codes

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
| `sys.storage.unrepresentable_key` | The selected input/format operation cannot encode the key within its declared bounds; a VFS filename limit alone must not reject a valid stored key. |
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
| `sys.blob.bounds` | Negative/out-of-range offset, length or octet value. |
| `sys.blob.limit` | Per-call or total capture/materialisation quota would be exceeded. |
| `sys.blob.length_mismatch` | Observed bytes do not match the expected complete length. |
| `sys.blob.unavailable` | A required promised object cannot be obtained under the selected policy. |
| `sys.blob.corrupt` | Descriptor, tree, chunk bytes, length or content digest contradict the selected profile. |
| `sys.blob.collision` | Unequal contents would otherwise receive the same content commitment. |
| `sys.blob.writer_state` | An operation is invalid for an aborted or finished writer. |
| `sys.blob.source_changed` | Detected change to the external input during capture. |
| `sys.blob.source_denied` | The selected host source/resource is not permitted or not a regular safe input. |
| `sys.blob.reference_context` | A repository reference is decoded outside an admitted repository Blob slot. |
| `sys.git.stale_review` | The supplied review token or candidate state no longer matches at protected publication. |
| `sys.blob.invalid_media_type` | The declaration is malformed, non-ASCII or exceeds MIME-1 bounds. |
| `sys.blob.invalid_suffix` | The suffix contains prohibited characters or exceeds the pinned safe suffix bounds. |
| `sys.blob.incompatible_suffix` | The suffix is not permitted by MIME-1 for the known declared type. |
| `sys.vfs.name_collision` | The selected portable projection would produce duplicate or ambiguous path names. |
| `sys.vfs.invalid_document` | The complete editable document fails schema-directed data decoding or row validation. |
| `sys.vfs.stale_edit` | The write candidate no longer matches its captured row, schema or field baseline. |
| `sys.vfs.unsupported` | The filesystem operation is outside the explicitly supported EDIT-1 contract. |
| `sys.vfs.draft_retained` | An edit was not accepted and its candidate is retained in bounded recovery storage. |
| `sys.format.unsupported` | The repository header selects a format for which no admitted reader/writer is available. |
| `sys.codec.unsupported_value_profile` | The value profile is not accepted by the selected encoding or wire context. |

# 42. Complete grammar {#grammar}

The grammar has three entry points: `module_unit`, `row_unit` and `repl_input`. It specifies syntactic structure; contextual lexical classification, type/effect checking, value validity and resource limits remain additional requirements. The complete grammar is included below and also distributed as `grammar/orna.ebnf`.

```ebnf
(* Orna 1.1.0 syntactic grammar. Lexical classification and contextual
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

# 43. Normative dependencies {#references}

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
| [Apache parquet-format 2.13.0](https://github.com/apache/parquet-format/tree/apache-parquet-format-2.13.0) | Legacy compact-input interpretation only. The new writer uses ORP-1, not Parquet; retained numeric vectors do not establish a new columnar writer. |
| [Git reference updates](https://git-scm.com/docs/git-update-ref), [index tree updates](https://git-scm.com/docs/git-read-tree), [branch](https://git-scm.com/docs/git-branch) and [switch](https://git-scm.com/docs/git-switch) | External command behaviour is informative background; the adopted branch, staging, ref-CAS and publication rules are fully stated in [branching](#branching) and [publication](#publication), not delegated to an unspecified future Git feature. |

IANA timezone data and optional codec/connector packages change independently of the language. Operations whose result depends on those datasets must identify the selected dataset/package snapshot. The library reference specifies how that context is captured. A historical program must not silently replace it with the host's newest dataset.

A build records the versions of its implementation dependencies, including Turso, SOPS, Git libraries and renderer frameworks. These dependencies implement the observable behaviour specified here; their internal APIs and additional features are not part of the Orna interface.

## Background checked for the 1.1 design

The following primary documentation informed this edition. Its implementation details are not substituted for the fully stated Orna profile algorithms.

| Source | Relevant boundary |
|---|---|
| [Git object model](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects) | Blobs, trees, native graph reachability and content-addressed storage. |
| [Git partial clone](https://git-scm.com/docs/partial-clone) | Promised missing objects, demand fetching, filters, metadata and over-fetching limitations. |
| [Git GC](https://git-scm.com/docs/git-gc) | Reachability, pruning and concurrent-writer limitations; content IDs in application files are not roots. |
| [Git submodules](https://git-scm.com/docs/gitsubmodules) | A gitlink pins another repository's commit; complete export includes that repository. |
| [Git namespace security](https://git-scm.com/docs/gitnamespaces) | Repository views are not an isolation boundary for untrusted raw-Git clients. |
| [bup](https://github.com/bup/bup) | Prior art for chunked binary backups using Git-format storage; OGB-2 is not the bup format. |
| [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html) | Adopted HTTP conditional and byte-range response semantics for the optional web Blob response. |
| [WebAssembly concepts](https://developer.mozilla.org/en-US/docs/WebAssembly/Guides/Concepts) | Browser platform access through host bindings; Wasm is not a DOM or networking standard library. |

Historical reference sources were checked on 2026-10-04; final-profile rationale is also recorded in implementation/SOURCES.md. No external service, host or commercial storage plan is required by OGB-2.
