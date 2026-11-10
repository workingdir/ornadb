# Orna language reference: outline and status

Status: the decisions are made ([DECIDED.md](DECIDED.md)) and the first
chapters are written under [reference/](reference/00-overview.md). The outline
below says what each chapter must define precisely; the table "Written
chapters" says what exists.

## Why a new document

The 1.1.0 specification is 946 numbered requirements, 669 or more of them
MUSTs, and no typing rules, no effect rules and no evaluation semantics
(critique findings F04, F29, F30; section 3.1). A reference is the opposite
shape: few clauses, each defined precisely enough that two implementers
produce the same behaviour. It contains no history, no orchestration text and
no product names. Where to put it relative to the 1.1.0 text is decision D13.

## Conventions every chapter follows

* Normative text uses defined terms and rules, not outcome assertions. A rule
  is written as `premises => conclusion` (typing, effect) or as a numbered
  step list (evaluation), not as "MUST behave as".
* Every rule has at least one valid and one invalid example, and each
  example is an `.orna` file executed by the conformance suite (critique
  F31: valid fixtures today are parsed and checked but never run).
* Every diagnostic is listed with its code, the rule that produces it and a
  stable message shape.
* Resource limits are stated as limits with a failure code, never as
  semantics.
* No requirement describes syntax that did not ship. Removed forms go to a
  migration appendix.

## Chapters

| # | Chapter | Must define precisely |
|---|---|---|
| 1 | Lexical structure | Source encoding and NFC rules; token classes and the disjointness of Decimal, Float, Date and Instant literals; numeric literal forms and suffixes; string escapes and interpolation; comments; reserved words; the rule that makes `1000.m` a unit literal and `1.decimal` a decimal |
| 2 | Grammar | One complete grammar for module unit, row unit and REPL input, with precedence and associativity table, the statement/expression boundary (`;` and block tails), the parenthesis disambiguation rule, and every context-sensitive production stated as a production |
| 3 | Modules, names and packages | Filesystem-to-namespace mapping; `use` forms; visibility; re-export (if any); shadowing and ambiguity rules; the reserved namespaces and what each guarantees; what a dependency pin is and how it resolves |
| 4 | Types | Every type form; nominal versus alias versus refined; records, tuples, enums, ranges, functions; generics and protocol bounds; implementation selection and overlap; subtyping or coercion relations (exactly the ones that exist); equality, order and hash as protocol facts |
| 5 | Type checking | Typing rules for every expression and declaration form; the inference rules (what is inferred, from what, what is generalised); join rules for `if` and `case`; the exhaustiveness algorithm; every place an annotation is required |
| 6 | Numeric tower | The integer, decimal and float types; every operator on every type pair, including division and remainder on negatives; overflow or resource behaviour; conversions (all explicit, or which are implicit); literal typing; rounding modes as named values |
| 7 | Text and bytes | String model (scalar values, indexing cost, normalisation, collation); interpolation; byte strings; encoding functions' failure behaviour |
| 8 | Expressions and evaluation | Evaluation order; binding and assignment; closure capture; `if`, `case`, loops, `break` with a value; pipeline and recovery operators as defined rewrites; the operational semantics of calls |
| 9 | Effects | The effect lattice and vocabulary; how an effect set is computed for a function, lambda and call through a function value; the table of contexts that restrict effects (assertion, computed field, relation callback, refined type, read-only watch) with the diagnostic each produces |
| 10 | Failure and cancellation | The completion states; propagation; the recovery operator; typed error values; rollback boundaries and statement savepoints; what cancellation is and what can observe it |
| 11 | Tables and rows | Table declaration; key forms and permitted key types; defaults and computed fields; stored references; mutation operations with exact preconditions, results and errors; row identity and re-key; index and uniqueness declarations (if any) |
| 12 | Assertions and validation | The four assertion owners; the candidate-relation model; ordering and witness selection; validation boundaries; the reference evaluation that any optimisation must equal |
| 13 | Relations and queries | `Relation<T>` and `Query<T>` as values; the core operators with order, stability and failure rules; the callback restrictions; aggregates, nulls and empty inputs; the plan-independent semantics |
| 14 | Activations, transactions, tasks | Activation roots; the captured snapshot and activation time; commit and rollback rules; task ownership and termination algorithm; the concurrent combinators; cancellation checkpoints |
| 15 | Units, money and time | Dimensions and units (or their restriction, see D8); `Money<C>` and currency; elapsed versus calendar time; zone rules |
| 16 | The core environment and the standard library contract | What `core` and `sys` guarantee and how they are versioned with the engine; what makes a function native; the single declaration form for host functions; what `std` may and may not assume; how a program names its library snapshot |
| 17 | Diagnostics | Code space, severity, span and fix-it model, the full code table |
| 18 | Conformance | What a processor must implement, what is optional, how the corpus is run, how an expected-failure list is kept |
| A | Migration appendix | Every removed or legacy form with its rewrite and diagnostic (the ORNA091 family and the legacy fixtures live here, not in the chapters) |

Formats (canonical encodings, storage, publication, protocol), product
requirements (CLI, REPL, serving, repository behaviour) and the system
reference (`sys`) stay outside this document; D13 decides where they go.

## Status: what can be written before which decision

"Now" means the chapter content does not depend on any open decision and
the current implementation or the 1.1.0 text already gives a single answer.
"Partly" means a defined subset can be written now and the named sections
wait. "Waits" means the chapter's central rules depend on the decision.

| # | Chapter | Status | Waits for | What can be written now |
|---|---|---|---|---|
| 1 | Lexical structure | Now | none (suffix rules touch D7, D8 only at the edges) | Everything except unit-suffix and fixed-width literal suffixes |
| 2 | Grammar | Partly | D2 (effect syntax), D3 (`T?` and `null`), D10 (`let mut`), D12 (`native fn`), D8 (unit declarations), D11 (`pub use`) | The existing grammar as one document with its precedence table; the contextual rules |
| 3 | Modules, names and packages | Partly | D11, D12 | Filesystem mapping, import forms, wildcard ambiguity rules, namespace reservation text |
| 4 | Types | Partly | D3, D5, D6, D8 | Nominal, alias, refined, record, tuple, enum, range, generics, protocols, implementation selection |
| 5 | Type checking | Waits | D1, D3, D5 | Expression typing rules that do not involve inference, `T?` or overloading; exhaustiveness; the `if` and `case` join rule once D3 is answered |
| 6 | Numeric tower | Waits | D7 | Decimal and Float rules (NUM-002..005, FLOAT-001..007) |
| 7 | Text and bytes | Partly | D6 | String model and normalisation (already stated in 04-lexical and 09); byte strings wait |
| 8 | Expressions and evaluation | Partly | D10 | Evaluation order, `if`, `case`, loops, pipeline rewrites |
| 9 | Effects | Waits | D2 | The legality-context table can be written with its current five contexts; the rule for how effects are computed cannot |
| 10 | Failure and cancellation | Waits | D4, D2 | Rollback, savepoints, cancellation-as-completion-state (already unambiguous in the 1.1.0 text) |
| 11 | Tables and rows | Partly | D9 | Table declaration, key types, defaults, computed fields, references, mutation preconditions and results |
| 12 | Assertions and validation | Now | none; D9 only if indexes become declarations | The candidate-relation algorithm and the four owners |
| 13 | Relations and queries | Partly | D2 (callback restriction wording), D3 (`first`, `min` results), D5 (`one`, `every` forms), D6 | Operator order, stability and failure rules |
| 14 | Activations, transactions, tasks | Now | none | TXN, CONCUR, TASK-END-1 and cancellation checkpoints; the cancellation sentence waits for D4 only if it changes the observable form |
| 15 | Units, money and time | Partly | D8 | Money, currency, time and zone rules; unit rules wait |
| 16 | Core environment and std contract | Waits | D12, D11 | Nothing normative until the host-function declaration form is chosen |
| 17 | Diagnostics | Partly | every decision that adds or removes a diagnostic | Code space and message model |
| 18 | Conformance | Now | D13 for location | What a processor is, how the corpus runs, expected-failure lists |
| A | Migration appendix | Now | none | Moving the ORNA091 family and legacy fixtures out of the chapters can start immediately |

Counting the table: 4 chapters can start now (1, 12, 14, 18) plus the
appendix; 8 can be partly written; 5 wait on a decision (5, 6, 9, 10, 16).

## Written chapters

The reference files are numbered by writing order. One file can cover several
outline chapters above.

| File | Outline chapters covered | Status |
|---|---|---|
| [reference/00-overview.md](reference/00-overview.md) | introduction | written |
| [reference/01-lexical-and-grammar.md](reference/01-lexical-and-grammar.md) | 1, 2, and the import and native parts of 3 | written |
| [reference/02-types.md](reference/02-types.md) | 4, the typing rules of 5, 6, the `Bytes` part of 7 | written |
| [reference/03-bindings-and-expressions.md](reference/03-bindings-and-expressions.md) | 8 | written |
| [reference/04-effects-and-failure.md](reference/04-effects-and-failure.md) | 9, 10 | written |

Not yet written: tables and rows (11), assertions (12), relations and queries
(13), activations and tasks as a chapter of their own (14; rollback and
cancellation are in 04), units, money and time as a chapter (15; the types are
in 02), the core environment contract (16), diagnostics (17), conformance (18)
and the migration appendix.

## Order in which decisions unblock chapters

1. D1, D3, D5 unblock chapters 4 and 5 and the HIR type model.
2. D2 and D4 unblock chapters 9 and 10.
3. D12 and D11 unblock chapters 3 and 16 and the host-function declaration
   work (stream D).
4. D7 unblocks chapter 6; D6 unblocks chapter 7's byte part; D8 unblocks
   chapter 15's unit part.
5. D9 unblocks the index section of chapter 11 and the table engine
   (stream E); D10 is local to chapters 2 and 8.
6. D13 decides where all of the above is written and what is deleted from
   the 1.1.0 text.

## Relationship to other documents

* [DECISIONS.md](DECISIONS.md): the option analysis this outline waits on.
* `docs/design/native-declarations.md`, `docs/design/sys-identity.md`,
  `docs/design/std-native.md`, `docs/design/architecture.md`: paused reform
  designs kept as input; D12 and D2 decide how much of them survives.
* [../design/kernel-and-library.md](../design/kernel-and-library.md): design for which language features are engine
  kernel and which are ordinary Orna in `core` and `std`. Its five open questions are decided as D14 to D18
  ([DECIDED.md](DECIDED.md)). It bears on chapters 4, 5, 6, 7, 15 and 16.
