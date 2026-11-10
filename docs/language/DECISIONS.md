# Orna language decisions: option analysis for the owner

Status: draft for the owner. Epic: `ornadb-csenl` (pivot). Input: the expert
critique of 2026-11-10 (findings F01 to F46) and the design documents under
`docs/design/`.

This document does not recommend anything. For each of thirteen decisions it
lays out the whole decision surface: what the specification says, what the
implementation does, why the two disagree or why the question is open, two to
four options with prior art, what each option costs the existing corpus and
standard library, how reversible it is, and what the typed intermediate
representation (HIR, see `docs/design/hir.md` when it lands) needs from the
answer. The owner decides; the analysis is meant to make any choice
defensible, including the choice to defer.

## How to read this document

Evidence tags used in every section:

| Tag | Meaning |
|---|---|
| `[F]` | Fact. Read directly in the cited file at `origin/main` `026faee90`, or in the spec at `reference/Orna-1.1.0/source/`. |
| `[M]` | Measured by a command run for this document (counts over files). The command is described next to the number. |
| `[J]` | Judgement. A weighing of trade-offs that a reasonable reviewer could weigh differently. |
| `[I]` | Inference. Believed true from reading, not observed by running anything. |
| `[C]` | Claim taken from the critique and not re-verified here. |

Citation conventions:

- Spec clauses are cited as `chapter-file:line`, relative to
  `reference/Orna-1.1.0/source/` (for example `05-types.md:311`). The clause id
  (for example ORNA-INFER-001) is given where one exists. Spec chapter numbers
  in titles are not stable; use file names.
- Code paths are relative to the repository root at `026faee90`. The critique
  cites `09546575e`; PR #9824 moved the std handlers out of
  `crates/orna-evaluator/src/lib.rs`, so evaluator line numbers here may differ
  from the critique's by a few hundred lines. Every code line cited below was
  re-read at `026faee90`.
- Prior-art URLs point at the language's or system's own documentation.
  Statements about prior art are summaries, not quotations.

Every decision has the same sections: **Spec**, **Implementation**, **Problem**,
**Options**, **Migration cost**, **Reversibility**, **Assumptions**, **What the
HIR design needs**, and **Questions only the owner can answer**.

Decisions interact. The interactions that matter are collected in the last
section, "Dependencies between decisions".

## Index

| Id | Decision | Spec clauses mainly involved | HIR impact |
|---|---|---|---|
| D1 | Type inference: real constraint inference or required annotations | INFER-001..014, INFER-1 | shapes `Ty` |
| D2 | Effects: visible in signatures, hidden, or a closed split | LIB-004, FIELD-008, ASSERT-011/039, FAILURE-002, TXN-008 | shapes function signatures |
| D3 | Null and Option | TYPE-001, LEX-008 | shapes `Ty` and value repr |
| D4 | Failure and cancellation | FAILURE-001..004, ERR-009, TYPE-006, CANCEL-001 | shapes the completion type |
| D5 | Overloading and types as value arguments | SYS-136, chapter 9 collection table | shapes call classification |
| D6 | Collections, byte strings, appending | core type list, BLOB-*, chapter 9 | additive |
| D7 | Integers and the numeric tower | NUM-001..005, LIT-001/002 | shapes `Ty` for numerics |
| D8 | Units of measure | UNIT-001..008 | shapes `Ty` if kept |
| D9 | Table keys and indexes | AUTOID-001..006, ROW-008, ASSERT-058 | additive to table definitions |
| D10 | Bindings and closures | VALUE-001..007, CFLOW-005/006/011, LAMBDA-003 | shapes lowering and slots |
| D11 | Modules, packages, re-export | NS-007, LIB-001..003, IMPORT-* | shapes name resolution |
| D12 | Where host functions live | LIB-002, LIB-003, NS-007, SYS-* | shapes `DefId` and `Callee` |
| D13 | Specification hygiene | TURSO-*, HOT-001, PUB-001, UPGRADE-* | none |

---

## D1. Type inference: real constraint inference or required annotations

### Spec

- ORNA-INFER-001 to 010 (`05-types.md:311-329`): omitted annotations MUST be
  inferred to one static type; no fallback to a dynamic `Any` (INFER-002);
  parameter and return annotations are optional when "unambiguously
  inferable" (INFER-003); public functions SHOULD annotate (INFER-004); the
  inferred exported signature is exposed exactly as if written (INFER-005);
  inference is independent of runtime data (INFER-006); an underconstrained
  case must name "the smallest useful annotation site" (INFER-007); adding a
  redundant annotation must not change behaviour (INFER-010).
- Algorithm INFER-1 (`05-types.md:456-466`): create type variables for omitted
  parameter, local and return types; add constraints from literals, patterns,
  calls, operators, assignments, fields, case arms, returns, table schemas,
  protocol requirements and contextual types; solve "to a principal type where
  one exists"; iterate strongly connected components for recursion; persist
  the inferred public signature.
- ORNA-INFER-011 to 014 (`05-types.md:467-473`): errors show conflicting
  constraints and spans; refactoring a private helper from explicit to inferred
  must not change exported identity; historical execution uses captured
  signatures.
- ORNA-TYPE-008 (`05-types.md:59`): absence of an annotation does not make a
  value dynamically typed.
- The spec's own examples rely on unannotated parameters:
  `examples/valid/inference-first.orna` has `fn square(x) = x * x;` and
  `pub fn names(users) = users | map(user => user.name);` `[F]`. The second
  needs the field name `name` to constrain `users`; ordinary
  Hindley-Milner inference has no record-field constraint without row types or
  a rule that resolves `.name` against a known nominal or table type.
- The spec defines no typing rules (no judgement forms, no subtyping relation).
  The checker lets `Int` flow to `Int?` (see D3), and no clause defines that.
  `[F]` for the absence; the checker behaviour is cited under D3.

### Implementation

- `crates/orna-check/src/types.rs:6-71`: the `Type` enum has no type-variable
  variant `[F]`. `Type::Error` and `Type::Bottom` exist; `Type::Error` is
  documented as "never a dynamic Any".
- `crates/orna-check/src/inference.rs:1455-1533`
  (`lambda_numeric_parameter_usage`): an unannotated parameter is "numeric" if
  its name appears directly (through groups) as either operand of any of
  `+ - * / % < <= > >= == !=`, anywhere in the body `[F]`.
- `crates/orna-check/src/inference.rs:1535-1559`
  (`inferred_function_parameter_type`): the order of attempts is (1) the type
  of the default expression, (2) `Int` if the numeric usage test holds,
  (3) a relation element type from usage, (4) `Type::Error` if the parameter is
  compared at all. The numeric test never looks at the other operand `[F]`.
- `crates/orna-check/src/inference.rs:1561-1692` handles lambda parameters by
  a different ordered list (comparison against a typed other side, then the
  numeric test, then record usage) `[F]`.
- Observed by the critique with a branch build, not re-run here `[C]`:
  `fn scale(x) = x * 2.5;` is rejected ("operator `*` is not defined for Int
  and Decimal"); `fn same(name) = name == "admin";` is rejected ("expected
  Int, found Text"); `let xs = []; union(xs, [1])` is rejected ("empty list
  needs an expected element type").
- The checker admits std items it cannot decide
  (`crates/orna-check/src/lib.rs:1725-1760`, `UNDECIDED_ITEM_CODES`): a std
  declaration that raises an import, unresolved, annotation or unsupported
  diagnostic is trusted on its declared signature `[F]`.
- Corpus facts `[M]`: in `stdlib/std` (81 files, 5,642 lines) there are 809
  `fn` declarations; of the 666 whose parameter list is on one line, none has
  an unannotated parameter other than `self` (regex over the checkout; 143
  declarations span several lines and were not measured). In the spec's valid
  examples (`examples/valid`, `final`, `reference`, `blob-library`) 5 of 118
  `fn` declarations have an unannotated non-`self` parameter
  (`function-default.orna:1`, `inference-first.orna:1,3`,
  `nominal-type-nested-impl.orna:5,17`). Lambdas with unannotated parameters:
  35 `name =>` occurrences in std, 42 in the examples (these are lambdas, not
  `fn`).

### Problem

The spec promises Hindley-Milner-style inference with principal types
(INFER-1); the product implements an order-dependent syntactic guess that is
unsound in both directions: it rejects valid programs when the guess is wrong
and its accept set is not describable as a rule. `[F]`. Separately, the
promise itself has costs the spec does not discuss: the algorithm lists
protocol bounds, nested implementations, table schemas, units and contextual
numeric typing as constraint sources, which is a research-grade combination
(see D8 for units). A decision is needed on whether the promise stands, is
narrowed, or is replaced.

### Options

**D1-A. Implement constraint inference as written (INFER-1).** Type variables
and unification in the checker; principal types for the supported fragment;
annotation requests only where no principal type exists (INFER-007).

- Pros `[J]`: matches every INFER clause as written; unannotated examples in
  the spec keep working; refactors between explicit and inferred forms are
  free (INFER-012); the language reads lightly for small programs and for the
  REPL.
- Cons `[J]`: the largest implementation cost of the four options; principal
  types are not guaranteed once protocol bounds, overlapping implementations,
  field selection (`user.name`), numeric contextual typing (INFER-008) and
  subtyping-like `T` to `T?` lifting (D3) are all present; error messages from
  unification are famously hard (INFER-011 requires showing conflicting
  constraints with spans); the semantic identity of a public function now
  depends on inference output (INFER-005, INFER-014), so the inference
  algorithm becomes part of the persisted format and must be versioned.
- Needs a decision on how `x.field` is typed without a known type for `x`:
  row polymorphism (as in Elm records and OCaml object/polymorphic variants),
  a rule that requires the type to be known at that point (bidirectional), or
  a "unique table or nominal with that field" rule.
- Prior art: OCaml and Elm infer principal types with Hindley-Milner and
  annotate by convention (Elm compilers warn on missing top-level
  annotations; Elm's records use row-like extensible record types).
  Koka infers effects and types together. PostgreSQL resolves operators and
  functions by a documented overload-resolution procedure rather than
  unification. URLs:
  https://ocaml.org/manual/5.2/types.html ,
  https://guide.elm-lang.org/types/reading_types.html ,
  https://koka-lang.github.io/koka/doc/book.html ,
  https://en.wikipedia.org/wiki/Hindley%E2%80%93Milner_type_system .

**D1-B. Require parameter annotations; infer locals and return types
(bidirectional).** Function parameters (and optionally public return types)
must be written; everything else is inferred forward from known types.
INFER-003/004 and the type-variable parts of INFER-1 are rewritten; lambdas get
their parameter types from the call context (the expected callback type of
`filter`, `map`, etc.).

- Pros `[J]`: removes the heuristic and the need for a unifier over function
  boundaries; error messages point at a missing annotation; the exported
  signature is by construction explicit (INFER-005 becomes trivial);
  inference is local so it cannot change across modules (INFER-014 becomes
  trivial); recursion needs no component iteration.
- Cons `[J]`: breaks the spec's own examples (5 of 118 example functions
  today) and every user function that omits a parameter type; `let xs = [];`
  still needs an expected element type (see below); lambda parameters still
  need context-directed typing, so a limited bidirectional typing is needed
  anyway; the REPL gets heavier for one-liners; "inference-first" is how the
  spec describes the language.
- Sub-choices: are lambda parameters exempt (inferred from the expected
  function type)? Is the return type required for recursive functions only?
  Are generics' bounds written or inferred?
- Prior art: Rust requires parameter and return types on `fn` and infers
  locals and closures; Gleam requires no annotations but its type system is
  HM so annotations are optional (see D1-A); Zig requires parameter types.
  URLs:
  https://doc.rust-lang.org/book/ch03-03-how-functions-work.html ,
  https://doc.rust-lang.org/reference/types/function-item.html ,
  https://tour.gleam.run/functions/functions/ ,
  https://ziglang.org/documentation/master/#Functions ,
  https://arxiv.org/abs/1908.05839 (bidirectional typing survey).

**D1-C. Hybrid: infer only within a stated, finite rule set.** Keep inference
for a documented fragment (for example: a parameter's type is determined by
its first use as an argument to an annotated function, an operator whose other
operand has a known type, or a default value), and require an annotation
otherwise, with a diagnostic that names the site (INFER-007).

- Pros `[J]`: small implementation; no unifier; existing `fn square(x) = x * x`
  style survives when the rule set covers it; diagnostics can be exact.
- Cons `[J]`: the rule set is a second place where behaviour is defined by
  procedure rather than by principle; each added case widens the surface;
  principality is not claimed, so INFER-1 step 5 must be dropped from the spec
  and the wording of INFER-006 and INFER-012 narrowed; it is the closest to
  the current heuristic, so there is a risk it reproduces its defects with a
  nicer name.
- Prior art: TypeScript and Swift infer from an initialiser and from context
  with documented but procedural rules; Go infers local `:=` only and requires
  parameter types. URLs:
  https://www.typescriptlang.org/docs/handbook/type-inference.html ,
  https://go.dev/ref/spec#Short_variable_declarations .

**D1-D. Declared types everywhere on public API, inference for everything
private.** Same as D1-B, except unexported functions and lambdas may omit
types and have them inferred (D1-A machinery on a private, non-persisted
fragment).

- Pros `[J]`: public metadata (INFER-005, INFER-014, `sys.Function`) stays
  explicit and stable, which suits a database whose historical programs must
  run against captured signatures; private helpers stay light.
- Cons `[J]`: needs the full D1-A machinery anyway; two regimes to explain.
- Prior art: Elm and OCaml convention (not rule); Haskell's top-level
  signature convention. URLs: https://wiki.haskell.org/Type_signatures_as_good_style (community convention),
  https://ocaml.org/manual/5.2/moduleexamples.html (interfaces).

### Migration cost

- Corpus `[M]`: std has 0 single-line unannotated non-`self` parameters (143
  multi-line declarations unmeasured), so D1-B and D1-D cost std essentially
  nothing in parameter annotations; lambdas in std (35) are the only open
  question and are covered by context typing in any option.
  Spec examples: 5 functions to annotate.
- Checker: D1-A is a new unifier plus an answer for field selection and
  numeric contextual typing; D1-B/C/D delete the three heuristic paths in
  `inference.rs:1455-1692` and add a bidirectional expected-type pass.
- Evaluator: unaffected (it consumes no inferred types today `[C]`).
- Spec: D1-A keeps INFER-1 and adds typing rules; D1-B rewrites INFER-003,
  INFER-004, INFER-007, INFER-012 and INFER-1 steps 2, 5 and 7.
- Tests: `orna-conformance/tests/valid_fixtures.rs:395-427` pins four valid
  spec fixtures as rejected (critique F31 `[C]`); they change status under D1-A.

### Reversibility

- A -> B is a breaking tightening of accepted programs. B -> A is
  non-breaking for accepted programs (it only accepts more), provided the
  persisted metadata format already carries explicit signatures.
- A persisted inferred signature (INFER-005/014) ties stored historical
  semantics to the inference algorithm; choosing A makes later algorithm
  changes a format question. `[J]`

### Assumptions

- Users write small programs and use the REPL (assumption `[J]`); whether
  unannotated one-liners matter to them is an owner question, not a fact.
- The persisted semantic catalogue records the exported signature for each
  function (INFER-1 step 8) `[F]` as spec text; whether the current
  implementation does is not verified here.

### What the HIR design needs

- A single `Ty` with or without inference variables. D1-A requires variables,
  substitution and a place to hold unsolved constraints per function; D1-B/C/D
  can lower directly to ground types, with variables confined to lambda
  context typing.
- `x.field` on a parameter of unknown type needs a chosen representation:
  row variable in `Ty`, or an error.
- Where inferred signatures are persisted (INFER-005): a `FnSig` in HIR that is
  identical whether written or inferred.

### Questions only the owner can answer

1. Is `fn square(x) = x * x;` a feature the language must keep?
2. Is "public signatures are always explicit" acceptable for historical
   execution (INFER-014) as a reason for D1-B or D1-D?
3. Should lambdas ever need annotations?

---

## D2. Effects: visible in signatures, hidden whole-program, or a closed split

### Spec

- Effects decide legality: read-only query callbacks "MUST be effect-checked
  before optimisation" (ORNA-LIB-004, `09-standard-library.md:44`); computed
  fields MUST NOT mutate, query unrelated tables, open streams, access secrets,
  perform I/O or use ambient time or randomness (ORNA-FIELD-008,
  `07-tables.md:132`); table assertions MUST NOT perform network, process, UI or
  arbitrary filesystem effects or observe uncaptured wall clock
  (ORNA-ASSERT-011, `07-tables.md:192`); an effectful or nondeterministic
  declaration assertion receives `ORNA-A091-007` (ORNA-ASSERT-039,
  `07-tables.md:408`).
- Effects have no syntax: "The compiler MUST track whether reachable operations
  may fail sufficiently to preserve control flow, rollback and diagnostics,
  without requiring source-visible effect annotations in 1.0" (ORNA-FAILURE-002,
  `06-expressions.md:440`); "User-visible effect annotations are not required
  for Orna to identify table mutations it owns. Internal effect information MAY
  be inferred for diagnostics, planning and introspection" (ORNA-TXN-008,
  `10-execution.md:33`).
- Function types are `fn(T): U` with no effect component
  (`09-standard-library.md:19`).
- The closed vocabulary exists in `api/sys.json` as `sys.EffectKind` (spec
  chapter 15).

### Implementation

- `crates/orna-check/src/types.rs:73-`: `EffectSummary` carries a set of
  `EffectKind`, an `opaque` flag and `may_fail` `[F]`
  (`crates/orna-check/src/lib.rs:386-391`: `is_effectful`,
  `forbidden_for_assertion`).
- `crates/orna-check/src/effects.rs:1-64`: the host vocabulary is twelve kinds
  `table_read, table_write, network, filesystem, process, clock, randomness,
  secret_access, ui, git, storage_admin, runtime_admin` `[F]`; `host_effect(path)`
  maps a callee path to at most one kind.
- Effects of std functions are computed by summarising their bodies
  (`crates/orna-check/src/std_support.rs:39`,
  `crates/orna-check/src/lib.rs:1191`); effects of intrinsics by
  `intrinsic_call_effects` (`lib.rs:5716`) `[F]`.
- Assertions are rejected by `forbidden_for_assertion`
  (`crates/orna-check/src/assertions.rs:37-45`, code `ORNA-A091-007`) `[F]`: any
  effect or `opaque` makes an assertion illegal.
- The native-declarations draft (`docs/design/native-declarations.md`, section 0
  and 3.8) introduces the first effect syntax, `effect none;`, on
  body-less native functions only, "because a body-less declaration has no call
  graph to derive effects from" `[F]`.

### Problem

Legality of a call site (assertion, computed field, query callback, REPL
preview) depends on a property that no signature shows and that changes when a
helper several calls away changes. `[F]` for the mechanism; the
maintainability consequence is `[J]`. The moment a function has no analysable
body (native, or an imported function from another package) the effect has to
be stated somewhere, and the native-declarations draft already does so for one
class of function.

### Options

**D2-A. Keep effects hidden; infer whole-program (status quo, made precise).**
Effects are computed per function from bodies and callee summaries; stored in
metadata (`sys.Function` effects), never written in source. Native functions
declare theirs on the declaration (the native-declarations `effect` clause).

- Pros `[J]`: no new syntax for users; existing spec text (FAILURE-002,
  TXN-008) stands; programs stay terse.
- Cons `[J]`: a library author cannot state or freeze the effect contract of a
  public function; an edit to a private helper changes the legality of public
  callers (a silent breaking change across packages); inference across package
  boundaries needs callee bodies or persisted summaries, which makes the
  summary part of the interface in practice; function types carry no effects,
  so higher-order functions (`filter(rows, f)`) cannot state "f must be pure"
  in their own types and instead rely on an out-of-band check per call site.
- Needs rules for recursion (least fixed point), for effects through
  function-typed arguments, and for `opaque`.
- Prior art: Haskell (IO monad) makes effects visible in types, so this
  option is the opposite; PostgreSQL has function volatility
  (`IMMUTABLE`/`STABLE`/`VOLATILE`) which is declared, not inferred (see
  D2-C). Hidden inferred effects have mainstream precedent in Java's
  unchecked exceptions and in most dynamic languages. URLs:
  https://www.postgresql.org/docs/current/xfunc-volatility.html .

**D2-B. Effects in signatures (effect rows).** Every function type carries an
effect set (`fn(T): U ! {table_read}`); effects are inferred for definitions
and checked against declared ones; higher-order functions are polymorphic over
the effect of their callback.

- Pros `[J]`: the legality rules (LIB-004, FIELD-008, ASSERT-011) become type
  rules; public contracts are explicit and checkable; assertion admission is a
  subtype check, not a special pass; natives and imported functions state their
  effect with the same syntax as everything else; `sys.Function` metadata is
  exactly the signature.
- Cons `[J]`: the largest language-design step; requires effect polymorphism
  for `map`/`filter`/`sort_by` and every combinator, which interacts with D1
  (effect variables join type variables); error messages involve rows;
  spec rewrite of FAILURE-002 and TXN-008 (the "no source-visible
  annotations" promise is removed); failure (D4) is also an effect in Koka and
  this option invites modelling it as one.
- Sub-choices: coarse (the twelve kinds as a set) versus structural rows;
  whether `fail` is an effect; whether effect annotations are mandatory or
  inferred-with-checked-optional-annotation.
- Prior art: Koka (effect rows in function types, inferred, optional
  annotation), OCaml 5 (effect handlers, not tracked in types), Unison
  (abilities in types). URLs:
  https://koka-lang.github.io/koka/doc/book.html#sec-effect-types ,
  https://arxiv.org/abs/1406.2061 ,
  https://ocaml.org/manual/5.2/effects.html ,
  https://www.unison-lang.org/docs/language-reference/abilities-and-ability-handlers/ .

**D2-C. Closed pure / effectful split with syntax.** Two function kinds,
decided by syntax or a keyword (for example a distinct declaration form for
functions that may touch tables or the host), with a closed rule: a pure
function may call only pure functions; assertions, computed fields and query
callbacks accept only pure functions. Individual effect kinds (the twelve in
`sys.EffectKind`) stay metadata and are not in types.

- Pros `[J]`: the legality rules become a single syntactic check; no effect
  polymorphism (a callback type is `fn(T): U` or `proc(T): U`, two function
  types); the contract is visible at the declaration; natives declare one bit
  (the native-declarations `effect none;` generalises to "pure" or not).
- Cons `[J]`: coarse: "may read tables" and "may use the network" are the same
  bit, so an assertion that legitimately reads a second table (ASSERT-021)
  needs a middle category or a special rule; two parallel function types
  and a coercion rule (pure to effectful) are needed; helpers must be declared
  in the right kind, which is friction for library authors.
- Sub-choice: a third "reads tables only" kind (a read/write/host trichotomy)
  versus two kinds.
- Prior art: Elm (functions are pure; effects are data returned to the
  runtime), Haskell (IO versus pure), PostgreSQL (declared volatility
  categories), Zig and Rust (no effect system; `const fn`/`comptime` is a
  closed pure subset). URLs:
  https://guide.elm-lang.org/architecture/ ,
  https://www.postgresql.org/docs/current/xfunc-volatility.html ,
  https://doc.rust-lang.org/reference/const_eval.html ,
  https://ziglang.org/documentation/master/#comptime .

**D2-D. No effect checking of user functions; contexts check at the
primitive.** Assertions, computed fields and query callbacks are run in a
restricted evaluation context in which effectful primitives fail at run time
with a typed error (a dynamic capability check), and nothing is checked
statically.

- Pros `[J]`: smallest implementation; no analysis, no metadata.
- Cons `[J]`: contradicts LIB-004 ("effect-checked before optimisation") and
  ASSERT-039 ("receives `ORNA-A091-007`", a compile-time diagnostic); an
  optimiser cannot rely on purity without the analysis; failures appear at
  commit time rather than at load time.
- Prior art: PostgreSQL documents that CHECK expressions are assumed to be
  immutable and does not verify it; Lua has no checking. URLs:
  https://www.postgresql.org/docs/current/ddl-constraints.html ,
  https://www.lua.org/manual/5.4/manual.html#2.4 .

### Migration cost

- Spec: A leaves FAILURE-002 and TXN-008 and adds a metadata clause; B and C
  rewrite them and the chapter-9 function-type description; D deletes
  LIB-004 and ASSERT-039 static wording.
- std `[M]`: 809 `fn` declarations; under B or C every std function that is
  not pure needs a signature marker; the std natives (113 per the
  native-declarations doc `[C]`) are marked once, by declaration.
- Checker: A is today's code made precise; B needs effect variables in the
  unifier (D1); C needs a function-kind field and a coercion rule; D deletes
  `effects.rs` static use.
- Corpus: valid fixtures that use assertions and computed fields are
  unaffected under A; under C each user helper called from an assertion needs
  the pure marker.

### Reversibility

- A -> B/C adds syntax later and is non-breaking if the new syntax is
  optional with inferred defaults. B -> A removes syntax and is a breaking
  change for any source that used it. C -> B is a refinement (add rows later)
  provided pure/effectful maps onto `{}` versus "some". `[J]`
- Whatever is chosen, the exported effect set of public functions is stored in
  metadata (spec chapter 15); changing the vocabulary later is a format
  change.

### Assumptions

- The effect vocabulary (twelve kinds) is stable. `[F]` that it is closed in
  `api/sys.json`; stability is an assumption `[J]`.
- Cross-package calls to functions whose bodies are not available at check time
  occur (natives now; packages in future, see D11).

### What the HIR design needs

- A field on every function signature: nothing (A, only metadata), an effect
  set (B), or a pure flag (C).
- A decision on whether callback-typed parameters carry an effect bound (B/C)
  or are checked at the call site (A).
- Per-call-site classification already requires `Callee` (source, native,
  sys, table op, relational); under every option the HIR stores the callee's
  effect summary on the call node or its `DefId`.

### Questions only the owner can answer

1. Should a library author be able to promise that a public function is pure?
2. Is "table read" in a position where assertions may use it (ASSERT-021
   requires at least two tables) a purity category of its own?
3. Is failure (D4) an effect for this purpose?

---
