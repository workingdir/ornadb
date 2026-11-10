# Orna language decisions D1 to D13

Status: option analysis for the owner. Nothing here is decided, and this
document does not recommend an option. Each decision has a bead under the
pivot epic `ornadb-csenl`; the bead id is given in each section.

Companion: [README.md](README.md) outlines the language reference that will
be written once these are answered, and says which chapters can be written
before which decision.

## How to read this document

Sources and pins:

| What | Where | Pin |
|---|---|---|
| Implementation | paths relative to `crates/` | `origin/main` = `026faee90` |
| Specification | paths relative to `reference/Orna-1.1.0/source/` unless another directory is named | publication `final-2026-10-05` |
| Standard library | paths relative to `stdlib/std/` | gitlink `fdf63e87` |
| Critique | the expert critique of 2026-11-10 (findings F01 to F44) | written against `09546575e` |

The critique's evaluator line numbers have drifted; every `file:line` below
was re-read at `026faee90`. Nothing was built or run for this document. A
statement about behaviour is a statement about what the code says.

Fact and judgement are kept apart:

* The subsections "Specification today", "Implementation today" and
  "Corpus" are facts and carry citations.
* "The problem" is the critique's argument restated; where it is the
  critique's judgement and not a contradiction between two cited texts, it
  says so.
* Inside an option, "What it is" and "Prior art" are facts (URLs cited).
  "For", "Against", "Risks" and "Reversibility" are judgement unless they
  carry a citation. "Rests on" lists the assumptions that must hold for the
  option to be a good choice; if an assumption is false the option is
  weaker than it looks.
* An option that is dominated by another on every axis I could find is
  marked "Weak" with the reason. That is the only steering in the document.
* `[INFERENCE]` marks a claim I derived and did not observe directly.

Migration cost is measured against three bodies of Orna source:

| Corpus | Size | How counted |
|---|---|---|
| Pinned std | 80 `.orna` files, 5,961 lines, 830 `fn` declarations, 1,700 parameters | `git ls-tree` and a regex over `git show fdf63e87:<file>` |
| Spec examples | 92 valid and 80 invalid `.orna` files under `examples/` | file count |
| Repository fixtures | 5,621 `.orna` files under `crates/` with 4,758 `fn` declarations | regex; Orna embedded in Rust string literals is not counted |

The regex counts are approximate (a `fn` followed by a balanced parameter
list); they are good to a few percent, not exact.

## Summary

| Id | Bead | Question | Options | Coupled with |
|---|---|---|---|---|
| D1 | `ornadb-csenl.1` | How are omitted types inferred? | A constraint inference; B annotations required on declared functions; C annotations required on `pub` only | D2, D5, D8 |
| D2 | `ornadb-csenl.2` | Where do effects live? | A effect sets in signatures; B inferred and published, never written; C closed pure/effectful split | D1, D4, D12 |
| D3 | `ornadb-csenl.3` | What is an absent value? | A ordinary enum, no lifting; B `T?` with lifting, one representation; C flat nullable | D5, D6 |
| D4 | `ornadb-csenl.4` | What is a failure, and what is cancellation? | A unchecked failure with a typed code; B inferred failure sets made normative; C failures as `Result` values | D2, D3 |
| D5 | `ornadb-csenl.5` | Who may overload, and how is a type passed? | A no overloading; B one overloading rule for everyone; C a closed platform list | D1, D3, D12 |
| D6 | `ornadb-csenl.6` | Which collections are core? | A core `Map`, `Set`, `Bytes`; B library types on a native core; C defer, fix `List` only | D7, D12 |
| D7 | `ornadb-csenl.7` | What is `Int`, and what is the numeric tower? | A unbounded with a stated cap; B add fixed-width types; C `Int` is 64-bit, `BigInt` separate | D6, D8 |
| D8 | `ornadb-csenl.8` | Units of measure in 1.x? | A implement with runtime quantities; B implement erased; C restrict; D cut | D1, D7 |
| D9 | `ornadb-csenl.9` | Default key, indexes, uniqueness? | Key: A keep, B `Uuid` default, C key required. Index: X assert-only, Y `unique` and `index` declarations, Z key-shaped assertions only | D2 |
| D10 | `ornadb-csenl.10` | Are bindings mutable, and what do closures capture? | A status quo plus a capture rule; B `let` and `let mut`; C no reassignment | D2 |
| D11 | `ornadb-csenl.11` | Re-export, packages, std replaceability? | Re-export: A `pub use`, B none. Std: P pinned and privileged, Q replaceable and pure | D12 |
| D12 | `ornadb-csenl.12` | Where do host functions live? | A `core` + `sys` with the engine, pure std; B std with natives; C natives in any pinned package | D2, D11 |
| D13 | `ornadb-csenl.13` | What is the specification? | A three documents; B one document, cleaned; C reference beside the frozen 1.1.0 | all |

The order in which answers unblock work: D12 and D2 gate the host-function
declaration form; D1, D3, D4 and D5 gate the HIR type and call model; D6, D7
and D8 gate the value model; D9 gates the table engine; D10 and D11 are
local; D13 decides where the answers are written down.

--------------------------------------------------------------------------

## D1. Type inference

Bead `ornadb-csenl.1`. Critique F02, section 3.2.

### Specification today

* `05-types.md:302`: "The compiler infers omitted type annotations from
  literals, operators, calls, fields, control-flow joins, assignments,
  protocol selection, return paths and contextual expectations." The
  example at `:305` is `fn square(x) = x * x;`.
* ORNA-INFER-001 (`:311`): omitted annotations "MUST be inferred to one
  static type before evaluation". INFER-002 (`:313`): no fallback to a
  dynamic `Any`. INFER-003 (`:315`): parameter and return annotations are
  optional "when the signature is unambiguously inferable". INFER-004
  (`:317`): public functions SHOULD annotate, but absence "is not a syntax
  error when inference succeeds". INFER-007 (`:323`): the diagnostic "MUST
  identify the smallest useful annotation site". INFER-010 (`:329`): a
  redundant annotation must not change behaviour.
* Algorithm INFER-1 (`:456-465`): step 2 "Create type variables for omitted
  parameter, local and return types"; step 5 "Solve constraints to a
  principal type where one exists"; step 7 iterates strongly connected
  declaration components for recursion.
* ORNA-GENERIC-001 (`06-expressions.md:161`): "Generic parameters are
  explicit in public source when a generic abstraction is intended; local
  type arguments may be inferred."
* The specification gives no typing rules, no definition of "principal",
  no generalisation rule (when an inferred type becomes generic) and no
  subtyping relation, although the checker has one (below).

### Implementation today

* `orna-check/src/types.rs:6-70`: `enum Type` has no type-variable
  variant. `Error` (`:68-69`) is "never a dynamic Any" and matches every
  type in `types_match` (`orna-check/src/calls.rs:2509`).
* A declared function's parameter type is the annotation, or else a
  syntactic guess (`orna-check/src/flow.rs:296-300`). The guess is
  `inferred_function_parameter_type` (`orna-check/src/inference.rs:1535-1559`):
  1. if the parameter has a default, the type of the default (`:1542-1553`);
  2. else if the name appears directly beside one of
     `+ - * / % < <= > >= == !=` anywhere in the body, `Int`
     (`:1554-1556`, operator list at `:1468`);
  3. else `Relation<{fields}>` if the whole body is `name | filter(x => ..)`
     or `name | map(x => ..)`, with the record built from the fields the
     lambda reads (`:1726-1760`; the callee is matched by the spelling
     `filter` or `map`);
  4. else the diagnostic "function parameter needs a static annotation"
     (`flow.rs:301-305`).
* So `fn square(x) = x * x` is typed `fn(Int): Int`. `fn scale(x) = x * 2.5`
  also gets `x: Int`, and then fails the arithmetic table, because `Int`
  and `Decimal` never mix (`inference.rs:1062-1065`). A function that is
  only ever called with `Decimal` arguments is still `Int`: call sites
  contribute nothing.
* Lambda parameters use a second heuristic
  (`inferred_lambda_parameter_type`, `inference.rs:1561-1573`): the type of
  the other side of a comparison, else `Int` by operator adjacency, else a
  record shape built from the fields the body reads, else "lambda parameter
  needs a static annotation" (`:250-253`). Where a callee's signature is
  known the lambda is checked against the expected parameter instead
  (`calls.rs:1708-1745`), which is ordinary bidirectional checking.
* The same guess is called from two more places
  (`orna-check/src/units.rs:629`, `orna-check/src/semantic_payload.rs:381`),
  so the inferred signature that reaches metadata (INFER-005) is the guess.
* There is an implicit subtyping relation that no clause defines: a value
  of type `T` is accepted where `T?` is expected
  (`calls.rs:2530-2532`), `null` is accepted where `T?` is expected
  (`:2525`), and `if`/`case` branches of `T` and `T?` join to `T?`
  (`flow.rs:208-241`, comment at `inference.rs:2698-2699`). See D3.
* A generic function cannot be declared at the REPL: the test
  `generics_fail_explicitly_in_the_bounded_runtime`
  (`orna-project-repl/src/lib.rs:1564`) pins `ORNA-EVAL-UNSUPPORTED`.

### Corpus

* Pinned std: 830 `fn` declarations, 1,700 parameters, 0 unannotated.
  Every return type I sampled is annotated as well.
* Spec `examples/valid`: 7 of 100 `fn` declarations have an unannotated
  parameter (8 parameters). Three are protocol members whose types follow
  from the protocol (`from(value)`, `display(self, context)` in
  `nominal-type-nested-impl.orna`); one takes its type from a default
  (`function-default.orna:1`, `duration = 7.days`, but `rows` is bare);
  `inference-first.orna` has `square(x)` and `names(users)`.
* Repository fixtures: 124 of 4,758 `fn` declarations have an unannotated
  parameter (142 of 3,624 parameters).

### The problem

Fact: INFER-1 specifies type variables and principal types; the checker has
neither. Fact: the result for an unannotated parameter depends on which
operator token is adjacent to it, not on the types of the other operands
or of any call site. Judgement (critique): a language that says
"inference-first" and ships a guess is worse off than one that asks for
annotations, because the guess is silent when it is wrong.

A second, independent problem: the specification never says what the
inference is over. Units of measure, `Money<C>`, protocol bounds, the
`T`/`T?` relation and overloading (D5) each complicate principal types, and
the combination has no published rules.

### Options

#### D1-A. Constraint inference as INFER-1 says

What it is. Give `Ty` inference variables; generate equality (and, if D3-B
is kept, `T <: T?`) constraints per function; unify; generalise at
declaration level; report unsolved variables at the smallest annotation
site. Publish the rules.

Prior art.
* Elm: full inference, annotations optional and conventional on top-level
  definitions (https://guide.elm-lang.org/types/reading_types.html).
* OCaml: Hindley-Milner with the value restriction for generalisation
  (https://ocaml.org/manual/5.3/polymorphism.html).
* Gleam: full inference, annotations optional
  (https://tour.gleam.run/basics/type-annotations/).
* F#: inference plus units of measure, a research result in its own right
  (https://learn.microsoft.com/en-us/dotnet/fsharp/language-reference/units-of-measure).

For.
* Keeps the specification's promise and its headline example unchanged.
* REPL and one-off scripts stay terse.
* The HIR gets a real unifier, which lambdas, generics and `case` joins
  need under every option.

Against.
* Elm, OCaml and Gleam infer over a small, regular type language. Orna's
  has an implicit `T` to `T?` relation, arithmetic that is defined per
  type pair with no numeric class (`inference.rs:1054-1082`), units,
  `Money<C>`, and (today) overloaded platform functions. `fn double(x) =
  x + x` has no principal type without either a numeric protocol
  (`<T impl Add>`) or a defaulting rule; both are new language design that
  the spec does not contain.
* Inferred public signatures change when a body changes. INFER-012 to
  INFER-014 already try to contain that; they are obligations on tooling
  that do not exist yet.
* Error messages for inference failures across function boundaries are the
  known weak point of this family; INFER-011 requires them to be good.

Risks. Scope: this is the largest option (critique estimate: L, one to two
months, on top of the HIR). Soundness: every interaction listed above needs
a rule and tests. Schedule: the HIR cannot fix `Ty` until the rules exist.

Reversibility. Easy to tighten later (requiring annotations is a
mechanical migration, and a formatter can write the inferred ones). Hard
to loosen back.

Rests on.
* Users write enough unannotated declared functions for it to matter (the
  corpus says 0 percent in std and under 3 percent in fixtures; there is no
  corpus of user programs).
* An answer exists for arithmetic on an unconstrained variable.
* D3 and D5 do not add subtyping or overloading that breaks principality,
  or the owner accepts "no principal type, annotate" in those cases.

#### D1-B. Annotations required on declared functions; infer the rest

What it is. A `fn` declaration (top level, nested in a type, or a protocol
implementation member not covered below) must annotate every parameter.
Return types, `let` bindings and generic type arguments at call sites are
inferred. Lambda parameters are inferred from the expected type at the use
site and need an annotation only when there is none. A protocol
implementation member takes its parameter types from the protocol
signature. The heuristics are deleted. INFER-003, INFER-004, INFER-008 and
INFER-1 are rewritten as local (bidirectional) inference.

Sub-choice B1: return annotations required on `pub` functions (signature
stable against body edits) or B2: inferred everywhere.

Prior art.
* Rust: parameter and return types are mandatory on `fn` items; locals and
  closures are inferred
  (https://doc.rust-lang.org/reference/items/functions.html).
* Gleam and Elm in practice: the compiler does not require annotations but
  the style guides and the standard libraries annotate every top-level
  function, which is what the pinned Orna std already does.
* Zig: parameter types are always written; `anytype` is the explicit
  opt-out (https://ziglang.org/documentation/master/#Functions).

For.
* Matches what the corpus already does: zero changes in the pinned std.
* The typing rules are short enough to write in the reference now.
* Generic functions are explicit (`<T>`), which GENERIC-001 already asks
  for in public source; there is no generalisation rule to specify.
* A signature never depends on a body, so effects (D2) and failure sets
  (D4) are the only inferred parts of a function's interface.

Against.
* The specification's own first example (`fn square(x) = x * x`) becomes
  an error. "Inference-first" stops being a headline.
* REPL one-liners need `fn square(x: Int) = x * x`.
* Lambdas passed to a generic or overloaded callee can still need
  annotations; the rule for "expected type known" must be written
  carefully or users will see the same diagnostic the heuristic produces
  today.

Risks. Low technical risk. The product risk is the owner's intent for the
language's feel, which only the owner can weigh.

Reversibility. High. Moving later from B to A is additive: every program
valid under B stays valid under A.

Rests on.
* Terse unannotated declarations are not a product requirement.
* Bidirectional checking is enough for lambdas in pipelines (it is what
  `calls.rs:1708-1745` already does for known callees).

#### D1-C. Annotations required on `pub` functions only; private functions inferred

What it is. D1-B at module boundaries, D1-A inside a module.

Prior art. Haskell and OCaml community practice (not compiler rules);
OCaml enforces it structurally when a module has an interface file
(https://ocaml.org/manual/5.3/modules.html).

For. Public signatures are stable and documented; private helpers stay
terse; inference never crosses a module boundary, so recursion groups stay
small.

Against. Needs everything D1-A needs (variables, unification,
generalisation, the arithmetic question), only with a smaller blast
radius. Two rules to explain.

Risks. As D1-A, reduced.

Reversibility. As D1-A.

Rests on. The same assumptions as D1-A, plus: the saving over D1-B (private
helper parameters) is worth the full inference engine.

Weak as a first step: it costs what A costs. It is a reasonable end state
reached from B.

#### Status quo (heuristic) is not offered as an option

Keeping the heuristic contradicts INFER-1 as written and gives wrong
answers silently. If the owner wants it kept for a transition period, that
is a migration question under A, B or C, not a design.

### Migration cost

| Option | Pinned std | Spec examples | Repository fixtures | Spec text |
|---|---|---|---|---|
| A | none | none | none expected; some fixtures that pin the heuristic's wrong answers change outcome | add typing rules; INFER-1 stays |
| B | none | 2 to 4 valid examples gain annotations (`square`, `names`, `recent`), protocol members unchanged | about 124 declarations to annotate, mechanically, by printing the type the checker assigns today | rewrite INFER-003/004/008 and INFER-1; `05-types.md:300-309` example |
| C | none | 0 to 1 (`pub` only) | about 23 `pub` declarations | as A plus the boundary rule |

### What the HIR needs from the answer

* A: `Ty::Var` with a substitution, a generalisation step per declaration
  group, and a place to store the solved scheme on each `DefId`. The
  checker's diagnostics must carry constraint provenance (INFER-011).
* B: no inference variables that outlive one function body; a
  bidirectional `check(expr, expected)` / `infer(expr)` pair; function
  signatures are resolved before any body is typed, which makes HIR
  lowering a two-pass affair with no fixpoint.
* C: both.
* Under every option: one `Ty`, no `Type::Error`-matches-everything
  escape that survives into the HIR, and an explicit node for the `T` to
  `T?` coercion if D3 keeps it.

--------------------------------------------------------------------------

## D2. Effects

Bead `ornadb-csenl.2`. Critique F11, section 3.3.

### Specification today

Effects decide legality in at least five places:

* ORNA-LIB-004 (`09-standard-library.md:44`): "Read-only query callbacks
  MUST be effect-checked before optimisation."
* ORNA-FIELD-008 (`07-tables.md:132`): a computed field "MUST be
  deterministic and row-local"; no mutation, unrelated tables, streams,
  secrets, I/O, ambient time or randomness.
* ORNA-ASSERT-011 (`07-tables.md:192`) and ASSERT-039 (`:408`): assertions
  must not perform network, process, UI or filesystem effects, randomness
  or uncaptured time; a violation is `ORNA-A091-007` "identifying the
  forbidden effect".
* `09-standard-library.md:9`: "A user-defined function of the same name
  remains an ordinary function and is checked for the effects required by
  its use."
* `09-standard-library.md:128` and `:152`: `sleep`, `std.io` and `std.net`
  are unavailable to assertions, read-only watches, presenters and
  historical execution.

Effects have no syntax:

* ORNA-FAILURE-002 (`06-expressions.md:440`): the compiler must track
  whether operations may fail "without requiring source-visible effect
  annotations in 1.0".
* ORNA-TXN-008 (`10-execution.md:33`): "User-visible effect annotations
  are not required for Orna to identify table mutations it owns. Internal
  effect information MAY be inferred for diagnostics, planning and
  introspection."
* A function type is `fn(T1, T2): R` (`04-lexical.md:159`): "Failure
  information remains a separate inferred channel". Nothing is said about
  effects in a function type.

There is one machine-readable vocabulary, `sys.EffectKind`, with twelve
values (`api/sys.json`, enums).

### Implementation today

* `orna-check/src/effects.rs:11`: `EffectKind` mirrors the twelve values
  (`table_read`, `table_write`, `network`, `filesystem`, `process`,
  `clock`, `randomness`, `secret_access`, `ui`, `git`, `storage_admin`,
  `runtime_admin`).
* `orna-check/src/types.rs:72-80`: `EffectSummary { effects, opaque,
  may_fail }`. `opaque` means "effectful, kind unknown; fails closed".
* The summary travels with the inferred expression and with the `Symbol`
  (`types.rs:107`), not with the type: `Type::Function` (`types.rs:52-62`)
  has parameters, names, defaults and a result, and no effect component.
* Naming a function yields that function's effects as the effects of the
  naming expression (`orna-check/src/calls.rs:2333-2334`), and a lambda
  expression has the effects of its body (`orna-check/src/inference.rs:306`).
  So the approximation is "mentioning or constructing a function value
  counts as running it". That is conservative for arguments passed
  directly. It says nothing about a call through a parameter of function
  type: inside `fn apply(f: fn(Int): Int, x: Int) = f(x)` the call `f(x)`
  contributes no effect, because `f` is a local symbol with an empty
  summary `[INFERENCE: I read the binding path, I did not run a probe]`.
* The legality checks:
  assertions reject any effect (`orna-check/src/lib.rs:386-391`,
  `orna-check/src/assertions.rs:37-45`); computed fields reject any effect
  or possible failure (`lib.rs:2592-2598`, diagnostic `E3012`); relation
  callbacks reject everything except `table_read`
  (`calls.rs:1767-1774`, reported under the generic type diagnostic).
* The native-declarations design adds the first effect syntax, on
  body-less declarations only: `pub native fn now(): Instant effect clock;`
  (`docs/design/native-declarations.md`, section 3.8).

### The problem

Fact: whether `rows | filter(f)` or `assert every(p)` is legal depends on a
property of `f` and `p` that appears in no signature, and that property is
the union over the whole call graph. Fact: the function type cannot express
it, so a function-typed parameter, field or return value has no stated
effect. Fact: the only place the project has needed to write an effect down
(a function with no body) it invented syntax for it.

Judgement (critique): a public function in another package is in the same
position as a native, from the caller's side; adding a log line to a
private helper three calls away can make a distant assertion illegal, and
the author of that edit gets no local signal.

Separate from visibility: `may_fail` is tracked in the same summary as
effects. Whether failure is an effect is D4's question; computed fields
already treat it as one.

### Options

#### D2-A. Effect sets are part of function types and are written in signatures

What it is. `fn(T): U` gains an effect set; a declaration may (or must,
for `pub`) state it; the checker infers the set from the body and verifies
the statement. Function-typed parameters state the effects they accept,
and effect-polymorphic higher-order functions (`map`, `filter`, `apply`)
need an effect variable. The vocabulary is the twelve `sys.EffectKind`
values, or a coarser grouping of them.

Possible surface (illustrative only):
`pub fn sync(account: Account): Int effect network, table_write` and
`fn apply<T, U, e>(f: fn(T): U effect e, x: T): U effect e`.

Prior art.
* Koka: effect rows in every function type, inferred, with row
  polymorphism (https://koka-lang.github.io/koka/doc/book.html, "Effect
  types").
* Java checked exceptions are the cautionary relative: declared sets
  without polymorphism (https://docs.oracle.com/javase/specs/jls/se21/html/jls-11.html).
* Zig error sets: declared or inferred per function, part of the type
  (https://ziglang.org/documentation/master/#Errors). Errors only, but the
  same mechanism.

For.
* The interface says what the checker enforces; an effect change in a
  dependency is a visible signature change.
* One mechanism for sources, natives, `sys` and packages: the `effect`
  clause in the native-declarations design becomes the general rule
  instead of a special case (critique 4.2, objection 3).
* Function values get a sound story: the type carries the set.

Against.
* Effect polymorphism is required on day one: `filter`, `map`, `sort_by`,
  `for_each`, `parallel` and every user combinator take callbacks. Without
  effect variables each needs a pure and an effectful version (D5) or
  rejects effectful callbacks.
* Twelve kinds is a wide row to write. A table-reading helper called from
  an HTTP handler shows `effect table_read, network` everywhere above it.
* Annotation burden lands on exactly the code the corpus does annotate
  (std) and on user code.

Risks. Design risk in the polymorphism rule and in choosing the grain
(twelve kinds, or fewer). Usability risk known from checked exceptions:
signatures churn as leaf effects change. Implementation is moderate once
the HIR exists: the inference is already there.

Reversibility. Low once user code and packages carry annotations.
Starting with "inferred, optional to write, verified when written" keeps
the door to B open.

Rests on.
* Users will read and maintain effect sets rather than suppress them.
* The effect vocabulary is stable enough to appear in source.
* D1 does not go to full inference of function types without a matching
  rule for effect variables.

#### D2-B. Effects stay inferred and unwritten, but become a published, specified part of a function's interface

What it is. No effect syntax in ordinary source. The reference defines
the effect lattice, the inference rule (including the rule for calls
through function values), and the legality table (which contexts admit
which effects). The inferred set of every `pub` function is part of its
semantic signature, exposed through `sys.Function`, hover, `orna check`
output and the semantic diff, so a change is reported as an interface
change (the spec already asks for that for types: INFER-005, INFER-013).
Body-less declarations (natives, `sys`) carry a mandatory clause because
there is nothing to infer from.

Prior art.
* PostgreSQL and SQLite sit near this point with the polarity reversed:
  volatility (`IMMUTABLE`, `STABLE`, `VOLATILE`) is declared, defaults to
  the unsafe end, is not verified, and decides whether a function may be
  used in an index or generated column
  (https://www.postgresql.org/docs/current/xfunc-volatility.html,
  https://www.sqlite.org/deterministic.html).
* OCaml 5 effect handlers are untyped by design: "no static effect
  safety" (https://ocaml.org/manual/5.3/effects.html).
* Rust infers `Send`/`Sync` structurally through types without the user
  writing them; a private field change can break a downstream use. That
  is the same failure mode, and it is tolerated there.

For.
* No change to any existing source. Smallest specification and
  implementation delta: the mechanism exists; it needs rules and exposure.
* No polymorphism syntax; the inference handles callbacks by flow.

Against.
* The critique's objection stands: legality depends on something not on
  the page. Tooling mitigates; it does not remove it.
* Function types still cannot state an effect, so the rule for a call
  through a stored function value must be either "assume the worst"
  (rejects pure callbacks stored in records) or flow-sensitive tracking
  (complex, and whole-program).
* Two mechanisms remain: written for natives and `sys`, inferred for
  everything else.

Risks. The hole described under "Implementation today" (calls through
function-typed parameters) must be closed by a rule, and the sound rule is
restrictive. Cross-package checks need the dependency's inferred sets
stored with its pin (INFER-014 covers types, not effects).

Reversibility. High. B is the status quo with rules; moving to A or C
later adds syntax and can be automated by writing out what was inferred.

Rests on.
* Tooling (diff, hover, `sys.Function`) is actually built and used.
* The owner accepts that an interface has an unwritten part.
* Stored function values are rare enough that "assume the worst" is
  tolerable, or flow tracking is affordable.

#### D2-C. A closed two-way split with syntax: pure functions and effectful functions

What it is. Every function is one of two kinds, marked in its declaration
and in its type (illustratively `fn` and `proc`, or `pure fn` and `fn`).
A pure function may call only pure functions; it is deterministic and
performs no host effect. Whether it may read tables, and whether it may
fail, are sub-choices (see below). Assertions, computed fields, refined
types, key defaults that must be deterministic, and relational callbacks
require pure. The twelve kinds stay as metadata on effectful functions for
`sys`, capability checks and diagnostics; they are not in the type.

Sub-choices that must be fixed: C1 two levels (pure, effectful); C2 three
levels (pure; reads committed snapshot; effectful), which is what the
legality checks distinguish today (assertions and relation callbacks admit
table reads; computed fields do not).

Prior art.
* Elm: functions are pure; effects are values (`Cmd`, `Sub`) returned to
  the runtime (https://guide.elm-lang.org/effects/). The strictest form.
* Rust `const fn`: a marked subset callable in restricted contexts
  (https://doc.rust-lang.org/reference/const_eval.html).
* PostgreSQL `IMMUTABLE`/`STABLE`/`VOLATILE` is a declared three-level
  split of exactly the C2 shape
  (https://www.postgresql.org/docs/current/xfunc-volatility.html).
* Datomic: transaction functions and query functions are documented as
  pure (https://docs.datomic.com/transactions/transaction-functions.html).

For.
* One bit (or three levels) is easy to read, write and explain. It is
  the distinction every legality rule above actually uses.
* Function types carry it cheaply: `fn(T): Bool` versus the effectful
  form; callbacks in `filter` are simply pure.
* Effect polymorphism can be avoided: a higher-order function is as
  effectful as its declaration says, and a pure function cannot accept an
  effectful callback.

Against.
* Without polymorphism, `map(xs, f)` with an effectful `f` needs either a
  second function or the rule "pure-typed parameters accept only pure
  arguments; effectful-typed parameters accept both, and the caller is
  then effectful". That rule is simple but makes every generic combinator
  choose.
* Colouring: marking a helper effectful forces the mark up the call chain.
  That is the point, and it is also the cost.
* Loses the per-kind precision in types (a function that only reads the
  clock and one that sends e-mail are the same colour). `sys` metadata
  keeps the detail but not at the type level.

Risks. Getting the levels wrong is expensive: `now()` is fixed per
activation (TXN-005), so is it pure? Table reads are deterministic for a
snapshot; are they pure? Each answer moves the line for assertions.

Reversibility. Medium. The marker is in every signature; refining two
levels into three later is additive, collapsing is not.

Rests on.
* The legality rules really need only the split, not the kinds.
* Users accept the colouring.
* The default colour is chosen well (unmarked means pure, or unmarked
  means effectful); the corpus cost depends on it entirely.

#### Elm-style "effects as data" is not offered

It would replace `sys`, activations and table mutation as specified. It is
a different language.

### Migration cost

| Option | Pinned std | Spec examples and fixtures | Spec text |
|---|---|---|---|
| A, optional to write | none until annotations are required on `pub`; then up to 630 `pub fn` signatures, generated mechanically from the inferred sets | none | new chapter; function type grammar; FAILURE-002 and TXN-008 reworded |
| A, required | 630 `pub fn`, plus callback parameter types in every higher-order function | every `pub fn` in fixtures | as above |
| B | none | none | new chapter (lattice, inference, legality table); `sys.Function` gains the field if absent |
| C, unmarked = pure | every effectful function is marked: the `std.io`, `std.net`, `std.concurrent`, `std.random`, `std.time` clock functions, `std.ui` and their callers | every table-writing or host-calling function in fixtures | new chapter; grammar; function type |
| C, unmarked = effectful | every function used in an assertion, computed field or query callback is marked pure; most of `std.collection`, `std.text`, `std.math` to be useful there | assertion and computed-field helpers | as above |

I did not count effectful functions in the corpus; under C the count can
be produced from the checker's existing summaries once a default is
chosen.

### What the HIR needs from the answer

* Under every option: an effect set per `DefId`, computed once, and the
  legality contexts as an enum on the HIR node that imposes them
  (assertion, computed field, relation callback, refined type, read-only
  watch). The checker's three ad hoc checks become one table.
* A: `Ty::Fn` carries an effect row with variables; unification over rows;
  declared-versus-inferred comparison per `DefId`.
* B: `Ty::Fn` has no effect; the HIR needs a defined rule for indirect
  calls (`Callee::Value`) and must persist inferred sets in the module
  interface it writes for dependents.
* C: `Ty::Fn` carries a two- or three-valued purity; no variables; the
  twelve kinds stay as a side table keyed by `DefId`.
* D12 depends on this: the `effect` clause on a native declaration is
  either the general syntax (A), a clause unique to body-less declarations
  (B), or the purity marker plus metadata (C).

--------------------------------------------------------------------------

## D3. Null and Option

Bead `ornadb-csenl.3`. Critique F12, section 3.5 "Options".

### Specification today

* The core type list has `Option<T>     written T?` (`05-types.md:28`).
* ORNA-TYPE-001 (`05-types.md:45`): "`T?` MUST mean `Option<T>` and MUST
  NOT mean an unchecked nullable value."
* The root environment provides `Some` and `null`
  (`09-standard-library.md:7`), and "Ordinary lexical declarations may
  shadow unqualified helpers".
* `first(rows)` returns "`Some(first value)` or `null`"
  (`09-standard-library.md:35`); empty `min`, `max`, `mean` return `null`
  (`05-types.md:111`, `09-standard-library.md:144`).
* ORNA-COALESCE-001 (`06-expressions.md:344`): "If `a` is `Some(value)`,
  it yields that value".
* `all_unique`: "For optional selected keys, `null` equals `null`"
  (`09-standard-library.md:52`).
* `sys.await`: "A successful result containing an optional `T` retains the
  outer success wrapper, so success with `null` is distinguishable from
  absence of a result." (`10-execution.md:61`).
* No clause says that a `T` is accepted where a `T?` is expected. No
  clause defines `T??`.

### Implementation today

* Checker: `Type::Null` (`orna-check/src/types.rs:14`) and
  `Type::Optional` (`:28`) are distinct. `null` is accepted for any
  `Optional` (`orna-check/src/calls.rs:2525`). A bare `T` is accepted for
  `T?`: "Optional parameters accept their non-null payload directly;
  callers do not need to wrap ordinary values in a nullable constructor"
  (`calls.rs:2530-2532`). Branch joins widen `T` and `null` to `T?`
  (`orna-check/src/flow.rs:208-227`).
* Evaluator: two values. `Value::Null` (`orna-evaluator/src/lib.rs:3590`)
  is what the literal `null` evaluates to (`:6623`). `Value::Option`
  (`:3659`) is what `Some(x)` constructs (`:9711`) and what `first`
  returns, including `Option(None)` for an empty input (`:8155`).
* Consumers accept both spellings of absence in some places and one in
  others: `??` takes `Option(None) | Null` (`:6972`); a `null` pattern
  matches `Option(None)` by a special arm and `Null` by literal equality
  (`:10898-10902`); a bare `T` value flowing into a `T?` parameter is not
  wrapped, so a present optional is sometimes `Option(Some(v))` and
  sometimes `v` `[INFERENCE from the checker rule at calls.rs:2530 and the
  absence of a wrapping step at the call boundary; I did not trace every
  path]`.
* Mode dependence: for ten statistics (`__mean`, `__median`, ...) the
  result is wrapped into `Value::Option` only when the evaluator is in
  REPL mode (`:10295-10300`; mode comment at `:5014-5017`: "a nullable
  statistic returns `T?` as an Option value (ORNA-TYPE-001)"). In module
  mode the same call returns a bare value or `Value::Null`.
* The pinned std builds a present optional with `first([x])` instead of
  `Some(x)` in its generic helpers (`option.orna:16`: `Some(inner):
  first([transform(inner)])`; `type_utils.orna:45`: `some(value) =
  first([value])`). The files do not say why. `option.orna:50` declares
  `flatten(value: T? ?)`, so nested options are in use.

### The problem

Fact: there are two runtime representations of "absent" and two of
"present", and which one a program observes depends on the producer and,
for statistics, on the activation mode. Fact: the checker has a subtyping
rule (`T` fits `T?`) that the specification does not state. Consequence:
with implicit lifting, `T??` is ambiguous at the value level (is a `null`
the outer or the inner absence?), which is why `sys.await` needs its
"outer success wrapper" sentence.

Judgement (critique): this is one type with two half-implementations; a
language should pick "option is an ordinary enum" or "types are nullable"
and say so.

### Options

#### D3-A. Option is an ordinary generic enum; no implicit lifting

What it is. `Option<T>` is a core enum with variants `Some { value }` (or
positional `Some(value)`) and `None`; `T?` is sugar for the type. A `T` is
not a `T?`: the program writes `Some(x)`. `null` either disappears or
becomes the spelling of `None`. `T??` is two layers and means what it
says. `??`, `case` and optional chaining (if any) are defined on the enum.

Prior art.
* Elm `Maybe` (https://guide.elm-lang.org/error_handling/maybe.html).
* OCaml `option` (https://ocaml.org/docs/options).
* Rust `Option<T>` (https://doc.rust-lang.org/std/option/enum.Option.html).
* Gleam keeps it out of the language entirely: `Option` is a library enum
  (https://hexdocs.pm/gleam_stdlib/gleam/option.html).

For.
* One representation, ordinary pattern matching, no special arms.
  Nested options are sound; the `sys.await` paragraph is unnecessary.
* No subtyping in the type system, which keeps D1-A principal and D1-B
  simple.
* JSON and row codecs can distinguish "missing" from "explicit null"
  (`09-standard-library.md:148` requires that) with types.

Against.
* Every call that passes a present value to an optional parameter gains
  `Some(...)`: `Contact.insert({ email: Some(address) })`,
  `sys.await(job, timeout: Some(5.seconds))`. For a database language with
  many optional columns that is a visible tax.
* `null` literals in row documents and JSON need a defined mapping.
* Breaks existing source that relies on lifting.

Risks. Ergonomic: users of SQL and TypeScript expect nullable fields to
accept plain values. Migration: finding every lifted site needs the
checker to report them (it can: the rule is one line).

Reversibility. Medium. Adding lifting later (A to B) is backwards
compatible for programs; removing it later (B to A) is not.

Rests on.
* The owner values type-system regularity over terseness at call sites.
* Record-literal and named-argument ergonomics are acceptable with
  explicit `Some`, or a narrower sugar is acceptable (for example lifting
  only in record fields and named arguments, which is still a rule to
  specify).

#### D3-B. Keep `T?` with implicit lifting, with one runtime representation and a written rule

What it is. `T?` is `Option<T>`. The reference states the coercion: where
a `T?` is expected and a `T` is supplied, the compiler inserts `Some`.
The coercion is an explicit node after type checking, so at run time
there is exactly one representation: `Option(Some(v))` or `Option(None)`.
`null` is the literal for `None`. `Value::Null` is removed. The evaluator
has no mode. Nested options: either forbidden (`T??` is a type error) or
allowed with the rule "lifting inserts exactly one layer and never applies
to a value that is already optional".

Prior art.
* Swift optionals: `T?` is `Optional<T>`, with implicit promotion of `T`
  to `T?`; nesting is legal
  (https://docs.swift.org/swift-book/documentation/the-swift-programming-language/thebasics/).
* Zig `?T` coerces `T` to `?T`
  (https://ziglang.org/documentation/master/#Optionals).
* Kotlin and TypeScript use flat nullable types instead (see C); they are
  prior art for the call-site ergonomics, not for the representation.

For.
* Source stays as written today. Smallest migration.
* The fix is where the defect is: one representation and one elaboration
  step, both natural products of the HIR.

Against.
* Subtyping (or coercion) is in the type system for good. With D1-A,
  inference must decide where coercions go; "principal type" needs care
  (`if c { 1 } else { null }` is fine; `let f = x => x; f(1); f(null)`
  is the kind of case that needs a rule).
* Generic code can observe the rule: with `fn wrap<T>(x: T): T? = x`,
  `wrap(null)` has type `Null??` or `T?` depending on the rule chosen for
  already-optional values.
* `all_unique`'s "null equals null" and the JSON "missing versus null"
  requirement still need separate statements.

Risks. The nested-option rule is easy to get subtly wrong; forbidding
`T??` is the safe form but std already declares `flatten(value: T? ?)`.

Reversibility. High in the direction of A (tighten by requiring `Some`),
with a mechanical rewrite because the coercion sites are explicit in HIR.

Rests on.
* A precise coercion rule can be written and holds under the D1 choice.
* Either nested options are not needed, or the owner accepts the stated
  one-layer rule.

#### D3-C. Flat nullable types: `T?` is "T or null", no wrapper, no nesting

What it is. `T?` is the union of `T` and the single value `null`.
`Some` is removed (or is the identity). `(T?)?` is `T?`. Flow typing
narrows after a null test. ORNA-TYPE-001 is reversed.

Prior art.
* SQL: every column type is nullable unless declared otherwise; three-valued
  logic follows (https://www.postgresql.org/docs/current/functions-comparison.html).
* Lua `nil` (https://www.lua.org/manual/5.4/manual.html).
* Kotlin nullable types (https://kotlinlang.org/docs/null-safety.html) and
  TypeScript under `strictNullChecks`
  (https://www.typescriptlang.org/tsconfig/strictNullChecks.html).

For.
* Matches the storage model (a column is present or absent) and JSON.
* No lifting rule and no wrapper: the checker's current behaviour
  (`calls.rs:2530-2532`) is simply the definition.
* `Value::Null` stays and `Value::Option` goes.

Against.
* A generic container cannot distinguish "no entry" from "entry whose
  value is null": `first(rows)` over `[Int?]`, `Map.get` on a map of
  optionals (D6), `sys.await` on a function returning `T?`. Each needs a
  different return shape, which is the `sys.await` paragraph multiplied.
* Contradicts TYPE-001 as written and the std modules built on `Some`.
* Needs flow-sensitive narrowing to be pleasant, which is checker work of
  its own.

Risks. The container problem above recurs in every generic API added
later. It is the reason Kotlin and TypeScript libraries grow sentinel
wrappers.

Reversibility. Low. Going from flat to wrapped later changes the meaning
of every `T?` in a generic position.

Rests on.
* Generic code over optional element types is rare enough to special-case.
* The owner is willing to reverse TYPE-001.

Weak in this language: D6 (maps, `first`, `get`) and `sys.await` already
show the container problem, and the option has no advantage over B that
survives it.

### Migration cost

| Option | Pinned std | Corpus | Evaluator and codecs |
|---|---|---|---|
| A | `option.orna`, `type_utils.orna` rewritten more simply; every lifted call site in std gains `Some` (not counted; the checker can list them) | every lifted site; row documents with `null` | remove `Value::Null`; remove the REPL wrap |
| B | remove the `first([x])` workarounds; decide `flatten` | none at source level if nesting is allowed; fixtures that pin a mode-dependent or bare shape change | remove `Value::Null`; insert coercion nodes; remove the REPL wrap |
| C | rewrite `option.orna`; remove `Some` everywhere | every `Some(...)` and every `case` over `Some` | remove `Value::Option`; add narrowing |

### What the HIR needs from the answer

* One `Ty::Option(T)` (A, B) or `Ty::Nullable(T)` with normalisation (C).
* B: an explicit `Coerce::Some` expression node inserted by the checker,
  so the executor never sees an unwrapped present optional.
* A and B: `Some`/`None` are constructors resolved to `DefId`s, not a
  shadowable root function matched by name
  (`orna-evaluator/src/lib.rs:9675-9683`).
* No `ActivationMode` input to value construction under any option.
* The value codec needs one encoding of absence; the canonical format
  already has one for `Option` (tag 0 or 1 payload, decoded at
  `orna-evaluator/src/lib.rs:4597-4600`).

--------------------------------------------------------------------------

## D4. Failure and cancellation

Bead `ornadb-csenl.4`. Critique section 3.4; state machine 1 in section 5.

### Specification today

* ORNA-TYPE-002 (`05-types.md:47`): a failable expression has "a separate
  abrupt failure channel carrying an `Error`; failure is not represented
  by `Result<T, E>` in source". TYPE-006 (`:55`) and ORNA-ERR-009
  (`06-expressions.md:409`): no intrinsic `Result`, `Ok`, `Err`; the names
  are not reserved.
* ORNA-ERR-001 (`06-expressions.md:393`): failures propagate automatically
  to a `|?` boundary or the host. ERR-003 (`:397`): `fail(e)` has the
  bottom type. ERR-006 (`:403`) and ORNA-TXN-002 (`10-execution.md:9`): an
  escaping failure rolls back the activation's writes. ORNA-LIB-006
  (`09-standard-library.md:77-79`): each mutation has a statement
  savepoint, so a recovered failure leaves no partial write.
* Completion states (`06-expressions.md:428-436`): success, failure,
  cancellation.
* ORNA-ERR-004 (`:399`) and ORNA-CANCEL-001 (`10-execution.md:99`):
  cancellation is distinct; "`|?` cannot catch it".
* ORNA-FN-010 (`06-expressions.md:84`): "A function's inferred failure set
  is metadata on the callable". ORNA-FAILURE-003 (`:442`): "Public
  metadata SHOULD expose the inferred set of nominal failure types where
  knowable and `Error` where open-ended." FAILURE-004 (`:444`): a handler
  receives at least `Error`; a processor MAY infer a narrower type.
* `Error` is constructed with `error(code: ..., message: ..., cause: ...)`
  (examples at `05-types.md:350-353` and `06-expressions.md:475-479`). The code is a
  string. I found no clause that defines "nominal failure type" or how a
  user declares one.

### Implementation today

* The evaluator's completion is `Result<Value, EvaluationError>` plus a
  side field `Context.transfer` for `return`/`break`/`continue`
  (`orna-evaluator/src/lib.rs:5043`) plus a cancellation token
  (`:5046`, `orna-evaluator/src/cancellation.rs`).
* Cancellation surfaces as an ordinary `Err` with code
  `ORNA-EVAL-CANCELLED`, raised from the step counter (`lib.rs:5738-5740`).
  `|?` distinguishes it by consulting the token, not the error
  (`lib.rs:6949-6958`): if cancellation has been requested, any failure
  arriving at `|?` is passed through unhandled. So an ordinary failure
  that happens to arrive after a cancellation request is treated as
  cancellation, and the two are not separable from the value alone.
* The checker tracks one bit, `may_fail` (`orna-check/src/types.rs:79`).
  There is no failure set. Nothing produces the metadata FAILURE-003
  describes `[INFERENCE: I searched for a set-valued failure summary in
  orna-check and found only the bit]`.
* The pinned std ships `pub enum Result<T, E> { Ok { value: T }, Err {
  error: E } }` in a 147-line module of combinators (`result.orna:5-8`), with a
  header that says language failures are not wrapped. That is permitted by
  ERR-009 (the names are not reserved) and is a second error convention
  in the reference library.
* `error.orna:8-20` builds errors with string codes and matches them with
  `value.code == expected`.

### The problem

Fact: three conventions coexist: unchecked failure (the language), failure
sets as metadata (promised, SHOULD, not implemented, and undefined because
"nominal failure type" has no declaration form), and `Result` values (in
std). Fact: cancellation is specified as a completion state and
implemented as an error plus a flag.

Judgement (critique): the core model is defensible and close to SQL and to
Erlang; the other two should go, `Error` should get a typed code, and
cancellation should be a variant.

Cancellation is listed under every option below as the same change,
because the specification is already unambiguous about it; the open part
is only the implementation shape.

### Options

#### D4-A. Unchecked failure only; typed error codes; no failure sets; no std Result

What it is. Failure is an abrupt completion carrying an `Error`.
`Error.code` becomes a value of a declared type instead of a free string:
either (A1) a closed core enum plus an open namespaced identifier for
user codes, or (A2) user-declared error enums that implement an `ErrorCode`
protocol, with `Error` holding one. `|?` handlers match on the code with
`case`. FAILURE-003 and FN-010's "failure set" are removed; `may_fail`
stays as one bit and is part of the effect answer (D2). `std.result` is
removed from the reference library, or kept as an ordinary data structure
with no relation to failure and documented as such.

Prior art.
* SQL and PL/pgSQL: errors abort the statement, roll back to the
  subtransaction, and are identified by SQLSTATE codes
  (https://www.postgresql.org/docs/current/plpgsql-control-structures.html,
  https://www.postgresql.org/docs/current/errcodes-appendix.html).
* Erlang: errors are unchecked; a supervisor owns recovery
  (https://www.erlang.org/doc/system/errors.html,
  https://www.erlang.org/doc/system/design_principles.html).
* OCaml exceptions (https://ocaml.org/docs/error-handling).
* Lua `error` and `pcall` (https://www.lua.org/manual/5.4/manual.html).

For.
* It is what the evaluator does. Rollback semantics (TXN-002, LIB-006)
  are already written around it.
* One convention. Nothing for the HIR to infer beyond a bit.
* Typed codes remove string comparison from handlers and make codes
  renameable and listable.

Against.
* A caller cannot learn from a signature which failures to expect; that
  is the trade every unchecked-exception language makes.
* Removing FAILURE-003 gives up something the spec promised (as SHOULD).
* A1 versus A2 is a real sub-decision: A1 is small and stringly at the
  edges; A2 needs protocol dispatch on error values and a stable identity
  for codes across packages and history.

Risks. Low. The typed-code design can be deferred (keep `Str` in 1.x)
without blocking the HIR.

Reversibility. High. Failure sets (B) can be added later as inferred
metadata without changing source.

Rests on.
* Activation rollback plus `|?` is enough structure for user programs.
* Nobody depends on FAILURE-003 metadata (nothing produces it today).

#### D4-B. Unchecked failure, with inferred failure sets made normative

What it is. As A for propagation and rollback. In addition, define
"nominal failure type" (a declared error enum or a declared code), have
the compiler infer for each function the set of failure types that can
escape it, and make that set part of the function's published interface
(FAILURE-003 becomes MUST). `|?` handlers get a narrowed parameter type
(FAILURE-004). Still no annotations in source.

Prior art.
* Zig inferred error sets: `fn f() !T` infers the set from the body, and
  the set is part of the function type
  (https://ziglang.org/documentation/master/#Inferred-Error-Sets).
* Koka: `exn` is an effect in the row
  (https://koka-lang.github.io/koka/doc/book.html).
* Java checked exceptions for the declared (not inferred) variant
  (https://docs.oracle.com/javase/specs/jls/se21/html/jls-11.html).

For.
* Keeps the promise in FN-010 and FAILURE-003.
* Handlers can be exhaustive over a known set; tooling can show what a
  function may raise.

Against.
* It is an effect system for failures. Everything said under D2 about
  inference through function values, polymorphism over callbacks and
  whole-program dependence applies again: `map(xs, f)` raises what `f`
  raises.
* Host and native failures are open-ended, so most real functions infer
  "`Error`" and the set carries little.
* Needs the nominal-failure declaration form designed first.

Risks. High cost for information that is often the top element. Zig keeps
this tractable by having no closures that capture and no hidden host
failures; Orna has both.

Reversibility. Medium. Once handlers rely on narrowed types, widening a
function's set is a breaking change for its callers.

Rests on.
* D2 is answered with a mechanism (A or B there) that failure sets can
  share; under D2-C there is no row to put them in.
* Enough failures are nominal and closed for the sets to be informative.

Weak unless D2-A is chosen: under D2-B or D2-C it is a second inference
system with its own rules.

#### D4-C. Failures as values: Result in the language

What it is. Reverse TYPE-002: fallible operations return `Result<T, E>`;
propagation is explicit (an operator or `case`); `fail` and `|?` are
removed or become sugar. Activation rollback is triggered by returning
`Err` from the activation root, or by a separate abort.

Prior art.
* Rust (https://doc.rust-lang.org/book/ch09-00-error-handling.html).
* Gleam (https://tour.gleam.run/data-types/results/).
* Elm (https://guide.elm-lang.org/error_handling/result.html).

For.
* Failure is in the type; no hidden channel; no separate inference.
* `std.result` already exists.

Against.
* Contradicts TYPE-002, TYPE-006, ERR-001, ERR-008, ERR-009 and the whole
  of FAILURE-1; the spec rejects it three times and gives the postfix
  propagation operator its own diagnostic.
* Every table mutation, conversion, decode and arithmetic limit becomes a
  `Result`; pipelines need a propagation operator at every stage.
* Rollback on failure stops being automatic unless a rule ties `Err` at
  the root to rollback; statement savepoints (LIB-006) need a new trigger.
* Resource-limit and host failures still need an abrupt path (Rust has
  panics, Gleam has `panic`), so there are two channels anyway
  (https://tour.gleam.run/advanced-features/panic/).

Risks. A rewrite of the failure chapter, the evaluator's control flow and
every example.

Reversibility. Very low.

Rests on. The owner wanting to reverse a decision the spec makes
explicitly and repeatedly.

Weak for this product: the transactional model is built on abrupt failure,
and two channels remain regardless.

#### Cancellation, under every option

What it is. The executor's completion type has a `Cancelled` variant.
`|?` cannot observe it because it is not an `Error` value, not because a
flag is consulted. An ordinary failure that races with a cancellation
request stays an ordinary failure until the owner's termination algorithm
(TASK-END-1, `10-execution.md:63-71`) decides the reported outcome.

Prior art.
* PL/pgSQL: "The special condition name OTHERS matches every error type
  except QUERY_CANCELED and ASSERT_FAILURE"
  (https://www.postgresql.org/docs/current/plpgsql-control-structures.html).
* Erlang: an exit signal with reason `kill` cannot be trapped
  (https://www.erlang.org/doc/system/errors.html).

Open sub-question for the owner: whether cleanup code may observe
cancellation at all (a scoped "finally" form). ERR-005 says cleanup
"SHOULD use scoped functions rather than user finalizers" and nothing
defines the scoped form.

### Migration cost

| Option | Pinned std | Corpus | Spec text |
|---|---|---|---|
| A | delete or re-document `result.orna` (147 lines) and its dependants; `error.orna` moves to typed codes if A1/A2 is taken now | uses of `std.result`; string code comparisons if codes are typed | remove FAILURE-003 and the set wording in FN-010; define `Error` |
| B | as A, plus declared failure types for std codes | handlers may narrow | define nominal failure types and the inference |
| C | every fallible signature | everything | failure chapter rewritten |
| Cancellation | none | fixtures that pin `ORNA-EVAL-CANCELLED` as an error code at the recovery operator | none; the spec already says it |

### What the HIR needs from the answer

* Under every option: the executor returns
  `Completion { Value, Return, Break, Continue, Fail(Error), Cancelled }`
  (the critique's state machine 1); `transfer` and the dummy `Value::Null`
  returns go.
* A: a `may_fail` bit per `DefId`, folded into D2's answer; `Error` as one
  core type with a resolved code type.
* B: a failure-set slot on `Ty::Fn` or per `DefId`, with the same
  variable machinery as D2-A.
* C: no failure channel in the executor other than abort; `Result` is an
  ordinary enum `DefId` with a propagation operator lowered to `case`.
* `|?` lowers to a `Recover { body, handler }` node whose handler binds an
  `Error`; cancellation never reaches it by construction.

--------------------------------------------------------------------------

## D5. Overloading and type-as-value arguments

Bead `ornadb-csenl.5`. Critique F28, section 3.5 "Overloading" and "Types as
values".

### Specification today

* `09-standard-library.md:19`: "Each listed collection operation has those
  two explicit overloads", one for `[T]` and one for `Relation<T>`, and
  preserves the container kind.
* `09-standard-library.md:21`: "A pipeline supplies argument one: `rows |
  filter(test)` calls `filter(rows, test)`. It does not first call
  `filter(test)` and guess a missing collection. Predicate factories
  explicitly listed below are genuine one-argument calls."
* Arity overloads: `one(rows)` and `one(rows, predicate)` (`:36`);
  `every(rows, predicate)` (`:39`) beside the factory `every(test)` that
  builds a `Predicate<Relation<T>>` (`:50`), and `all_unique(selector)`.
* Type overloads in `sys`: ORNA-SYS-136 (`15-system.md:114`) "Snapshot
  selection is represented by explicit overloads for `SnapshotRef`,
  `CommitRef`, `BranchRef`, `TagRef`, `GitOid` and `Str`".
  `api/sys.json` lists 36 of its 82 function entries as members of an
  overload family (`sys.snapshot` x6, `sys.admin.checkout` x6,
  `sys.admin.create_branch` x6, `sys.admin.plan_checkout` x6, `sys.explain`
  x3, `sys.source`, `sys.history`, `sys.blame` x2 each, `sys.invoke` and
  `sys.start` x2 each).
* Types passed as values: `decode(raw, as: Message)`
  (`06-expressions.md:381`, also `09-standard-library.md:148`);
  `sys.invoke<T>(..., as: T)` and `sys.start(worker, arguments, as: Int)`
  (`10-execution.md:55`, ORNA-SYS-132 at `15-system.md:297`, "The result
  witness `as: T` selects a typed boundary" at `:254`). The same spec says
  generics are explicit and protocol dispatch is static
  (`06-expressions.md:161-167`).
* Bare words as values: `rounding: half_even` (`05-types.md:91-97`).
* No clause says whether a user may declare two functions with one name.

### Implementation today

* The checker special-cases the argument name `as`: it is read as a type
  expression, not a value (`orna-check/src/inference.rs:491-493`, also
  `orna-check/src/flow.rs:972` and `orna-check/src/system_calls.rs:240-499`).
  The evaluator has the same special case (`orna-evaluator/src/lib.rs:9060`).
  Nothing in the type system says why `as` is different; it is the
  spelling.
* The pinned std declares `pub fn decode<T>(input: Str): T` and documents
  the call form `decode(input, as: T)` in a comment
  (`encoding/json.orna:7-8,17`): the declaration has no `as` parameter, so
  the call form and the declaration disagree and only the special case
  reconciles them.
* Relation operators are resolved by the callee name at each call
  (`orna-evaluator/src/lib.rs:9675-9683` for root intrinsics); `std.query`
  redeclares 16 of them as marker-bodied functions because the language has
  no re-export (`query.orna:251-288`; see D11 and D12).
* A user-declared duplicate function name is a diagnostic in the REPL
  (`orna-check/src/repl.rs:253`) and a `DuplicateDeclaration` error for the
  standard catalogue (`orna-check/src/catalogue.rs:170`); I did not find an
  overload rule for user source.
* The `sys` overloads are not language overloads: the checker has a
  separate parser and type model for `api/sys.json` signatures
  (`orna-check/src/system_api.rs`), so the six `sys.snapshot` entries are
  six rows in a table the checker searches by argument type.
* The stats module accepts the bare words `half_even`, `linear`, `lower`,
  `higher`, `nearest`, `midpoint` as string values only when the call is a
  stats operation and the argument name is `rounding`, `interpolation` or
  `method` (`orna-evaluator/src/lib.rs:10136-10149`); decimal division has
  its own `half_even` case (`:7680-7683`).

### The problem

Fact: the platform overloads by arity and by type, users cannot declare
overloads (or the rule is unstated), and types appear as value arguments
through a name-based special case. Judgement (critique): users cannot write
what the library uses, and a reader must memorise which names are which.

Two questions are independent and are separated below:

* Q1. Is overloading a language feature, and for whom?
* Q2. How does a program pass a type to a function: type application only,
  or a value-level witness?

### Options for Q1: overloading

#### D5-A. No overloading anywhere

What it is. One name, one function in a scope. Arity variants get distinct
names (`one` and `one_where`), or one function with a default parameter.
Type variants use a generic (`snapshot<R impl SnapshotSelector>(reference:
R)`) or distinct names (`snapshot_commit`). `Collection<T>` for `[T]` and
`Relation<T>` is either a protocol bound or one type.

Prior art.
* Elm has no overloading; `Dict.map` and `List.map` are different names
  (https://package.elm-lang.org/packages/elm/core/latest/Dict).
* OCaml has none; modules separate namespaces
  (https://ocaml.org/manual/5.3/modules.html).
* Gleam has none; labelled arguments and defaults cover arity cases
  (https://tour.gleam.run/functions/labelled-arguments/).
* Rust has none for free functions; traits provide type-directed dispatch
  (https://doc.rust-lang.org/reference/items/traits.html).

For.
* A call resolves by name alone; the HIR call-resolution step has no
  candidate set. Pipelines (ORNA-PIPE-003) need no overload explanation.
* The ORNA-SYS-136 sentence ("no implicit `SnapshotLike` union") stops being
  an exception.

Against.
* The `sys` surface changes shape: 36 entries collapse into generics or new
  names, and the typed `sys.invoke` / `sys.start` pairs become one function
  with a type argument (Q2).
* The `[T]`/`Relation<T>` pair is not a type-parameter difference today;
  without a protocol or a unified type each operator needs two names or a
  new core type relationship (see D6).
* Breaks any program that calls the overloaded platform names by their
  current arity or type, unless the old names are kept.

Risks. API redesign of the largest family (`sys.admin.*`) is not mechanical.

Reversibility. High: adding overloading later is compatible with programs
that never relied on it.

Rests on.
* Protocol bounds (ORNA-GENERIC-005) are expressive enough for selector
  families, or distinct names are acceptable.

#### D5-B. One stated overloading rule for everyone

What it is. Users and the platform may declare several functions with one
name in one scope, distinguished by arity and/or the static types of
arguments. The reference states the resolution algorithm (exact match,
then at most one applicable candidate after the declared coercions, else an
ambiguity diagnostic) and forbids overloads that could overlap, matching
the style of ORNA-GENERIC-002 and ORNA-CONVERT-003.

Prior art.
* PostgreSQL function overloading with a documented resolution algorithm
  (https://www.postgresql.org/docs/current/typeconv-func.html,
  https://www.postgresql.org/docs/current/xfunc-overload.html). It is also
  the cautionary tale: the algorithm is long and implicit casts make it
  surprising.
* Swift, Java, C++ overload by type with ranking rules (not cited).

For.
* Preserves every current platform signature and call.
* Users get what the library has.

Against.
* Interacts with inference (D1-A): the type of an argument may depend on
  the chosen overload. With D3-B's `T` to `T?` coercion, `f(x)` and
  `f(Some(x))` may both apply.
* Every overload set is an ABI surface for D12: a native binds to a
  `DefId`, so overloads need distinct ids.
* Pipe stages (`x | f`) must resolve with the piped value as argument one.

Risks. The rule itself is the product: a PostgreSQL-sized algorithm to
write, test and keep stable.

Reversibility. Low: once users rely on it, removal breaks programs.

Rests on.
* No implicit coercions beyond D3's, or a ranking rule.
* Owner accepts the cost of an algorithm chapter in the reference.

#### D5-C. Overloading is a platform feature with a closed, declared list

What it is. Only declarations in `core`/`sys` interface files (D12) may
carry an `overload` marker; the set of overloaded names is part of the
engine; user declarations may not overload. The reference lists the
overloaded names and why.

Prior art. Lua's core functions behave this way by implementation
(`tostring`, `type`), and PostgreSQL built-in operators are overloaded in a
catalogue users can extend only through `CREATE OPERATOR`
(https://www.postgresql.org/docs/current/extend-type-system.html).

For. Smallest change from today; keeps `sys` as written; the HIR classifies
overloaded call targets by the declaration marker.

Against. Keeps the critique's objection exactly: users cannot write what
the library uses.

Weak as an end state, reasonable as a transition: it is the status quo with
a list.

Reversibility. High (opens to B later).

### Options for Q2: passing a type

#### D5-X. Type application only

What it is. A type is passed with angle-bracket arguments:
`decode<Message>(raw)`, `sys.invoke<Int>(f, args)`. `as: T` is removed; a
type is never a value argument. The declaration `decode<T>(input: Str): T`
already has the shape.

Prior art. Rust turbofish `parse::<i32>()`
(https://doc.rust-lang.org/reference/paths.html); OCaml uses annotations
(`(x : t)`) and first-class modules.

For. One rule, matches existing declarations, no special argument name;
ORNA-GENERIC-001 already says type arguments are explicit.

Against. Pipelines read less well: `raw | decode<Message>` or
`raw | std.encoding.json.decode<Message>()`; existing docs use the
witness form; every example and diagnostic about `as:` changes.

Reversibility. Medium.

#### D5-Y. Types are values with a stated rule

What it is. A type name in expression position is a value of a core type
`Type<T>`; `as: Message` is an ordinary named argument of parameter type
`Type<T>`; the checker resolves `T` from it. The reference defines what
operations exist on a `Type<T>` (none beyond passing it).

Prior art. Zig `comptime` type parameters
(https://ziglang.org/documentation/master/#comptime); Python classes as
values (not typed the same way).

For. Reads as today in pipelines (`| decode(as: Message)`); one mechanism
with `sys.invoke` witnesses.

Against. A new core type, and a rule that `T` in `Type<T>` is statically
known (otherwise generics become dependent); `Message` as both a type and
a value needs a resolution rule; nothing else in the language is
compile-time evaluated.

Reversibility. Low.

#### D5-Z. Keep `as:` as a declared parameter form

What it is. A declaration may mark a parameter `as: T`-style (for example
`witness as: T`) to say it names the result type; the checker treats
exactly that parameter as a type argument. No new core type; the special
case becomes a declared one.

For. Smallest change; the declaration and the call form agree.

Against. A third way to pass a type next to `<T>` and `Type<T>`; the
parameter is "not a value" without a type that says so.

Weak unless the owner wants the pipeline spelling and does not want X or Y.

### Bare words

`half_even`, `linear` and the other bare words have three possible
readings, independent of Q1 and Q2: ordinary enum variants of declared enum
types (`Rounding.half_even`, shortened by a scoped import), string
literals, or a keyword-argument sugar for a closed set. Only the first
needs no special case; it needs the owner to accept either qualified names
or a "variant of the parameter's enum type may be written bare" rule.

### Migration cost

| Option | Pinned std | `sys` | Spec text |
|---|---|---|---|
| A | every arity or type overload pair renamed or merged (`query.orna` has 16 re-declared operators; `one` and `every` forms) | 36 entries restructured | remove "two explicit overloads" and SYS-136 |
| B | none | none | add a resolution algorithm chapter |
| C | none | none | list overloaded names |
| X | every `decode(input, as: T)` call and comment (json, orna, ovb encodings) | `sys.invoke`, `sys.start` witness form | rewrite `as:` text in 06, 09, 10, 15 |
| Y | none at call sites | none | define `Type<T>` |
| Z | declarations gain a marker | none | grammar |

### What the HIR needs from the answer

* A: each call resolves to exactly one `DefId` by name and scope.
* B and C: call resolution yields a candidate set resolved after type
  checking; the HIR must store which candidate was chosen, because the
  executor must not re-resolve (critique F01, F17).
* X: calls carry explicit type arguments in a dedicated slot; no named
  argument is special.
* Y: `Ty::Type(T)` and a constant-type expression form.
* Z: a flag on a parameter that moves it from the value list to the type
  argument list during lowering.
* Bare words: either enum variant resolution against the expected type, or
  a literal form.

--------------------------------------------------------------------------

## D6. Collections

Bead `ornadb-csenl.6`. Critique F25.

### Specification today

* The core type list (`05-types.md:7-43`) has `List<T>` written `[T]`,
  `Range<T>`, `Relation<T>`, `Query<T>`, `Stream<T>`, records, tuples and
  `Blob`. It has no `Map`, no `Set` and no byte-string type apart from `Blob`.
* `Blob` is "immutable binary content with canonical declared media type
  and optional suffix" (`05-types.md:130`).
* ORNA-VALUE-001 (`05-types.md:65`): lists, records and rows have value
  semantics; ORNA-VALUE-004 (`:71`): cyclic ordinary values are not
  constructible.
* `union(left, right)` "Concatenate left then right; duplicates remain"
  (`09-standard-library.md:33`); there is no append or index-update
  operation in the core table (`:23-40`).
* ORNA-FLOAT-003 (`05-types.md:107`): `Float` is not accepted as a primary
  key component or default hash key type.
* `sys.blob.to_bytes(value: Blob): [Int]` and `sys.blob.from_bytes(values:
  [Int]): Blob` (`api/sys.json`); `sys.blob.read(value, offset, length):
  Blob`.

### Implementation today

* The evaluator's `Value` has `List` (shared storage; "clones and appends at
  the end of the longest view do not copy existing elements",
  `orna-evaluator/src/lib.rs:3634-3636`), `Record` as a `BTreeMap`
  (`:3649`), `Tuple`, `Blob(Vec<u8>)` (`:3601`) and no map or set variant.
* The pinned std fills the gap with lists. `std.map` is a list of pairs with
  at most one entry per key; "Lookup is linear" (`map.orna:1-7`), and
  `size` is `count(from_entries(entries))` (`:11-12`). `std.set` is `[T]`
  with duplicates removed and "Membership is linear" (`set.orna:1-4`).
* `std.bytes` is "immutable lists of unsigned octets, each in 0...255"
  (`bytes.orna:1`) and builds buffers with `output = union(output, [0])`
  inside `while` loops (`bytes.orna:24`); `std.map` does the same
  (`map.orna:41`); `list.append` is `union(values, [value])`
  (`list.orna:12-13`).
* The Blob bridge returns a `List` of `Value::Int(BigInt)`, one big integer
  per byte (`orna-evaluator/src/lib.rs:9431-9440`).
* Step budgets are per call and per element (`orna-evaluator/src/lib.rs:117-124`
  comment: appends reuse list storage, "No quadratic term is needed"), so
  the append idiom is made cheap by a storage optimisation in the evaluator,
  not by the language.

### The problem

Fact: associative lookup and set membership are linear in std; bytes are
lists of arbitrary-precision integers; the append idiom is a union. The
critique's argument (judgement): basic programs are quadratic by
construction, and the performance fix lives in an evaluator trick that a
second implementation would not have.

### Options

#### D6-A. Core `Map`, `Set` and `Bytes` types

What it is. Add `Map<K, V>`, `Set<T>` and `Bytes` (distinct from `Blob`, or
`Blob`'s in-memory form) to the core type list with defined literals or
constructors, operations (get, insert, remove, keys, merge, slice, append,
indexing), equality, order and canonical encoding. Key types must satisfy
the same lawful-equality rule `distinct` uses. Iteration order is defined
(insertion order, or key order).

Prior art.
* Elm `Dict` and `Set` are core, tree-based and ordered by `comparable`
  (https://package.elm-lang.org/packages/elm/core/latest/Dict), and
  `elm/bytes` has a `Bytes` type
  (https://package.elm-lang.org/packages/elm/bytes/latest/).
* Gleam has `Dict`, `Set` in the standard library and `BitArray` in the
  language (https://hexdocs.pm/gleam_stdlib/gleam/dict.html,
  https://tour.gleam.run/data-types/bit-arrays/).
* Erlang maps are a built-in type
  (https://www.erlang.org/doc/apps/stdlib/maps.html).
* OCaml: `Map.Make` and `Hashtbl` in the standard library, `bytes` in the
  language (https://ocaml.org/docs/maps,
  https://ocaml.org/manual/5.3/api/Bytes.html).

For.
* Fixes the asymptotics where it matters, in the specification.
* Gives `sys` and codecs a real type for maps and byte strings (JSON
  objects, headers, `sys.ArgumentMap`).
* Canonical encoding and equality become defined, not inherited from list
  shape; the value formats already have a map concept in the OVB wire
  format `[INFERENCE: std.map references std.encoding.ovb for map encoding
  at map.orna:22-35; I did not read the format chapter for a map tag]`.

Against.
* Largest specification, evaluator and format change of the three options:
  each new core type touches the checker, value codec, equality, order,
  hashing, presentation and storage types (critique F16, eight type
  representations).
* Generic key requirements need a protocol (`Hash`/`Order`), which today
  exists only as hard-coded rules for `Float` and keys.
* `Map<K, Option<V>>` meets D3 (get returns an option of an option).

Risks. Scope creep: every collection added becomes a permanent part of
the 1.x surface.

Reversibility. Low: types and their encodings are forever.

Rests on.
* Programs need these types for correctness or speed, not just
  convenience.
* D7 and D12 are settled enough to define element and native details.

#### D6-B. Library types over a small native core

What it is. `Map`, `Set` and `Bytes` are nominal types declared in std (or
`core`) as opaque wrappers whose operations are native functions (D12).
Only a minimal core capability is specified: an ordered key-value store
primitive and a byte buffer primitive with documented complexity. The core
type list does not change.

Prior art. Gleam (types in the standard library, implemented over Erlang or
JavaScript maps through `@external`
(https://tour.gleam.run/advanced-features/externals/)); Rust
(`HashMap` in `std`, `Vec<u8>` in `alloc`,
https://doc.rust-lang.org/std/collections/index.html).

For.
* Core stays small; the types can change between std pins without a
  language change.
* Matches how the owner's native-declarations design treats natives.

Against.
* Complexity guarantees live in a library, so a replaceable std (D11-Q,
  D12-A) could silently drop them.
* Needs a story for equality, ordering and codecs of opaque types
  (ORNA-NOMINAL-009 already says protocols supply them).
* Binds D6 to D12: it cannot be done until a native form exists.

Risks. The same asymptotics problem returns if the native implementation is
not specified.

Reversibility. High: types can be promoted into the core later.

Rests on. D12 answered with a native declaration form (either A or B there).

#### D6-C. Defer: fix `List` and the byte API only

What it is. No new collection types in 1.x. Add `List` operations (`append`,
`concat`, `get`, `set`, `slice`) to the core with stated complexity so the
append idiom is specified; give `Blob` a typed byte view (`Blob.bytes()`
returning a lightweight `Bytes`) so `sys.blob.to_bytes` stops producing
`[Int]`. `std.map`/`std.set` stay as documented linear structures.

For. Smallest change; removes the worst defect (bytes as big integers and
the `union` loop) without new equality rules.

Against. Maps and sets stay quadratic for programs that grow them; the
critique's claim that basic programs are quadratic stays true for lookup.

Reversibility. High.

Weak as a final answer for a database language, since a keyed lookup is the
native operation of the domain; reasonable as a first step before A or B.

### Migration cost

| Option | Pinned std | Evaluator and formats | Spec text |
|---|---|---|---|
| A | `map.orna` (181 lines), `set.orna`, `bytes.orna` (127) rewritten on core operations; many `union(x, [y])` loops removed | new `Value` variants, codecs, equality, canonical ordering | core type list, formats chapter |
| B | the same files become native declarations | native table plus opaque handle type | std contract |
| C | `list.orna` gains core-backed operations; `bytes.orna` shrinks | `Blob` byte view; list ops | core operation table |

### What the HIR needs from the answer

* A: `Ty::Map(K, V)`, `Ty::Set(T)`, `Ty::Bytes` as builtin constructors;
  literal forms if any; key-protocol bounds.
* B: opaque nominal types in the DefId table; nothing in `Ty`.
* C: list operations as core `DefId`s; `Ty::Blob` unchanged.
* All: element-type unification for generic builtin constructors (it is the
  same machinery as `Ty::List`); no `Value::Int(BigInt)` per byte at the
  boundary.

--------------------------------------------------------------------------

## D7. Integers and the numeric tower

Bead `ornadb-csenl.7`. Critique section 3.5 "Collections" (last sentence)
and section 3.7 (numeric tower).

### Specification today

* ORNA-NUM-001 (`05-types.md:81`): "`Int` MUST represent exact integers."
  Nothing bounds it. ORNA-LIT-001 (`04-lexical.md:57`): "Integer source
  numerals have arbitrary-precision parse semantics and are range-checked
  when checked against a bounded integer type." No bounded integer type is
  defined in the core list.
* `std.bits` is defined "over Int using an unbounded signed two's-complement
  model", "Right shift is arithmetic; left shift is exact subject to
  resource limits" (`09-standard-library.md:132`).
* `Decimal`: "finite exact base-10 values with implementation resource
  limits" (`05-types.md:83`); `Float`: IEEE-754 binary64 (`:85`); fractional
  literals default to `Decimal`, `f` suffix selects `Float`
  (`04-lexical.md:59`).
* ORNA-INFER-008 (`05-types.md:325`): contextual numeric inference "MUST
  preserve the exactness rules". ORNA-CONVERT-006 (`:442`): arbitrary `From`
  conversions are not implicit.
* The specification says nothing about `Int` division and remainder on
  negative operands, nothing about `Int + Decimal`, `Int * Float` or
  `Int / Int` result types, and states no digit cap or overflow behaviour
  beyond "implementation resource limit" for `Decimal`.

### Implementation today

* Evaluator: `Value::Int(BigInt)` (`orna-evaluator/src/lib.rs:3593`). After
  every operation `integer()` renders the result in base 10 and compares
  the length with `max_integer_digits` (default 1,024,
  `DEFAULT_INTEGER_DIGITS` at `:130`; check at `:255` and `:5955-5961`).
  Exceeding it is `ORNA-EVAL-LIMIT`.
* Integer division is `a / b` on `BigInt` and remainder `a % b`
  (`lib.rs:7549-7560`), which truncates toward zero in the `num-bigint`
  crate; division by zero is `ORNA-EVAL-DIVIDE-BY-ZERO`. That is the
  behaviour; no clause states it.
* Checker: `Int`, `Decimal`, `Float` "never mix implicitly" and each
  arithmetic operator is defined only for equal operand types
  (`orna-check/src/inference.rs:1062-1065`). `Int / Int` is `Int`
  (truncating). Money and quantities add their own rows
  (`:1066-1080`).
* `Decimal` division by a non-terminating result fails unless
  `.divide(scale:, rounding:)` is used (ORNA-NUM-005; the evaluator matches
  the field name `divide`, `lib.rs:9593-9594`).
* The integer step budget makes every `Int` operation O(digits), because of
  the decimal rendering (`:5955-5961`); the critique counts this under F26
  (interpreter overhead).
* Storage: keys permit `Int` (`07-tables.md:272`); the value codec encodes
  arbitrary precision integers `[INFERENCE: CBOR bignum tags; not read in
  full]`.

### The problem

Fact: the specification has one integer type of unbounded size and
mentions a bounded integer type that does not exist; `Int` division
semantics and the cross-type arithmetic table are only in code. Fact: the
implementation's digit cap is a resource limit, observable as an error.
Judgement (critique): a database wants fixed-width integers for storage and
interop (counts, ids, ports, protocol fields), and unbounded `Int` as the
only integer is unusual.

### Options

#### D7-A. Unbounded `Int` with a stated cap and stated operations

What it is. Keep one `Int`. The reference states: the cap (a minimum
guaranteed size and the failure code), `/` and `%` semantics (truncating or
floored, pick one), exponent and shift rules, and the numeric tower:
`Int`, `Decimal`, `Float` never mix implicitly; conversions are explicit
functions (`Decimal.from(i)`), and literal typing follows ORNA-LIT-002.

Prior art.
* Python `int` is unbounded (https://docs.python.org/3/library/stdtypes.html,
  https://peps.python.org/pep-0237/).
* Erlang integers are bignums
  (https://www.erlang.org/doc/system/data_types.html).
* Haskell `Integer` (https://www.haskell.org/onlinereport/haskell2010/haskellch6.html).

For.
* Matches the spec and the implementation; no new type; no overflow
  semantics to define.
* Money, counts and keys never silently wrap.

Against.
* No exact match for a 64-bit column or wire field: values outside the
  range must be checked at the boundary by a refined type
  (`type U16 = Int { assert >= 0; assert <= 65535; }` is already legal,
  ORNA-REFINE-001).
* Every operation is allocation and digit-bounded; a cap check by bit
  length (not decimal rendering) is an implementation detail but the
  1,024-digit cap is observable.

Risks. Low. The decision is mostly to write down what exists.

Reversibility. High: fixed-width types can be added later (B).

Rests on.
* Refined types over `Int` are an acceptable way to model bounded domains.

#### D7-B. Add fixed-width integer types beside `Int`

What it is. Core types `Int8` .. `Int64` and `UInt8` .. `UInt64` (or a
subset), with defined overflow behaviour (fail, wrap or saturate; pick one
per operator family) and explicit conversions to and from `Int`. `Int`
stays unbounded. Literal typing gets a contextual rule (ORNA-LIT-001 already
anticipates a "bounded integer type").

Prior art.
* Rust numeric types with checked, wrapping and saturating methods
  (https://doc.rust-lang.org/reference/types/numeric.html).
* PostgreSQL `smallint`, `integer`, `bigint` and `numeric`; overflow is an
  error (https://www.postgresql.org/docs/current/datatype-numeric.html).
* SQLite stores integers in 1 to 8 bytes by magnitude
  (https://www.sqlite.org/datatype3.html).
* Gleam deliberately has one `Int` per target
  (https://tour.gleam.run/basics/ints/); an example of the choice not to.

For.
* Exact storage and interop types; the checker can reject `Int` to
  `UInt8` without a conversion.
* Bounded arithmetic is O(1) and removes the digit-rendering check for
  those types.

Against.
* Many operator/type combinations to define (the checker has no numeric
  protocol, D1), and bitwise operations (`std.bits`) must be restated per
  width.
* A second set of literal and suffix rules; overlap with unit suffixes (D8).
* Every table, codec and `sys` signature gains a choice of integer type.

Risks. Large surface; each type is permanent.

Reversibility. Low once stored in tables.

Rests on.
* Programs need bounded semantics (wrapping, exact interop) that refined
  types over `Int` cannot give.

#### D7-C. `Int` is 64-bit; a separate `BigInt` for unbounded values

What it is. Reverse the default: `Int` is 64-bit signed with checked
arithmetic (overflow fails), `BigInt` is the unbounded type. Literals
default to `Int` and fail to check if they do not fit.

Prior art.
* Rust's `i64` default with a separate big-integer crate; Go `int` and
  `math/big` (https://go.dev/ref/spec); Lua 5.4 64-bit integers
  (https://www.lua.org/manual/5.4/manual.html).
* PostgreSQL `bigint` versus `numeric`
  (https://www.postgresql.org/docs/current/datatype-numeric.html).

For.
* Fast, bounded, matches storage engines; makes the digit cap unnecessary
  for the common type.

Against.
* Reverses ORNA-NUM-001 and every example that relies on exact integers;
  `sum` and factorial-style code now fail at 2^63.
* The existing `std.bits`, money allocation (exact integer weights,
  `09-standard-library.md:138`) and `Decimal` coefficients rely on
  unbounded integers; they move to `BigInt`.

Risks. Behaviour change in every program that exceeds 2^63 (rare, but
silent only until it fails).

Reversibility. Low.

Rests on. 64 bits suffices for the domain's ordinary integers.

### Numeric tower sub-decision (applies to every option)

Independent of the width: whether `Int` operands may meet `Decimal` or
`Float` operands. Current behaviour is never. Options: (T1) never, explicit
conversions only (status quo, what NUM-001..003 and CONVERT-006 imply);
(T2) `Int` widens to `Decimal` implicitly (exact, so no information is
lost) but never to `Float`; (T3) a numeric protocol with a defined result
type per pair. T1 needs no new rule; T2 and T3 need rules and interact with
D1 (principal types) and D5 (overload resolution).

### Migration cost

| Option | Pinned std | Corpus | Spec text |
|---|---|---|---|
| A | none | none | state division, shift, cap, conversions |
| B | `std.bits` and `std.bytes` gain width-specific forms; the byte API can use `UInt8` | none until adopted | new types, overflow rules |
| C | any code that needs unbounded values moves to `BigInt` (money, bits, stats exact sums) | fixtures that exceed 2^63 | reverses NUM-001 |

### What the HIR needs from the answer

* `Ty::Int(kind)` with a kind enum (A: one kind; B: many; C: two) so the
  checker's operator table is keyed by kind, not by spelling.
* A numeric-literal node that stores the exact value and is typed by
  context (ORNA-LIT-001/002), not a string.
* A binary-operator enum (not `op: String`, critique F36) whose typing rule
  is a table lookup.
* Resource limits as a runtime concern: the digit cap check by bit length
  at the executor, not in the type.

--------------------------------------------------------------------------

## D8. Units of measure
Status: SUPERSEDED by the amendment of 2026-11-10 below, AMENDED by owner direction
2026-11-10. The cut in this section is kept as the record of the earlier decision.


Bead `ornadb-csenl.8`. Critique F10, section 3.2.

### Specification today

* `05-types.md:189-252`: `dim` and `unit` declarations with base, scaled,
  composite and affine units (`unit C : Temperature = K offset 273.15
  affine;` at `:211`); eight clauses ORNA-UNIT-001 to 008:
  compatibility by reduced dimension exponents, rational scale and affine
  behaviour (`:215`); compile-time error on incompatible addition (`:221`);
  point/vector algebra for affine units (`:223-238`); integration reduces
  dimensions (`:240`); an aggregation matrix over affine values
  (`:242-250`); static, unambiguous unit suffix resolution that does not
  perform `From` (`:252`).
* `Quantity<N, U>` and the surface form `Float<kWh>` are core type forms
  (`05-types.md:34`, `:134`).
* Money interacts: ORNA-MONEY-003 (`05-types.md:280`) requires dimensional
  algebra to reduce `Decimal<kWh> * (Money<GBP> / kWh)` to `Money<GBP>`.
* The wire format has a Quantity value: tag 60006 `[number,
  unit_object_id]` and a type descriptor `[7, numeric_type, unit_id]`
  (`29-formats.md:28`, `:110`). Unit identities are stable ObjectIds
  (ORNA-OBJECT-001, `07-tables.md:350`).
* Time units (`1.day`, `5.minutes`) are described separately as Duration and
  calendar period (ORNA-TIME-001 to 004, `05-types.md:141-161`).

### Implementation today

* Checker: `Type::Quantity` carries numeric type, dimension basis, scale and
  offset (`orna-check/src/types.rs:38-44`); `units.rs` is 1,498 lines of
  declaration reduction, and `quantity_operator_type` /
  `quantity_sum_type` implement the operator tables
  (`orna-check/src/inference.rs:1117-1250`). The checker's own comment on
  money rates says `MoneyPerUnit` "is not a general dimensional-algebra
  representation: unsupported products still remain Error"
  (`types.rs:45-51`).
* Evaluator: there is no quantity value in `enum Value`
  (`orna-evaluator/src/lib.rs:3589-3665`). A numeric postfix is resolved by
  its last path segment against a fixed list: `decimal`, `day(s)`,
  `hour(s)`, `minute(s)`, `min`, `second(s)`, `s`; anything else is
  `ORNA-EVAL-TYPE` (`lib.rs:5867-5900`). So the declared `unit m` of the
  spec's own example parses and type-checks, and `1000.m` fails when run.
  Note also that `s` and `min` are hard-coded names, not the declared
  units.
* `orna-value` has a Quantity schema test
  (`orna-value/src/lib.rs:200`), so the canonical type descriptor exists;
  the evaluator never constructs one.

### Corpus

No unit or dimension declaration exists in the pinned std (0 matches over 80
modules). The spec examples `examples/valid/units.orna` and
`affine-unit.orna` are the only source declaring units; these are parsed and
checked, not executed (critique F31).

### The problem

Fact: a headline specified feature type-checks and cannot run. Judgement
(critique): F# units of measure were a research programme, specified here
in eight clauses; the checker's money-rate comment already records that the
general case was not built.

Note what is not in question: `Duration`, calendar periods and `Money` are
runtime values today. The question is user-declared dimensions and units.

### Options

#### D8-A. Implement with runtime quantities

What it is. Add `Value::Quantity { number, unit }` to the executor, with
arithmetic that carries units, conversion on comparison and sum of
compatible units, affine point/vector rules and the aggregation matrix.
Numeric postfix resolves against declared units (including time units as
declared in std or core), replacing the hard-coded list.

Prior art.
* F# measures are erased at run time and checked statically (see B).
  Runtime-carrying systems exist in libraries (Python `pint` is one)
  `[INFERENCE: no page fetched]`.

For. Matches the specification and the wire format (tag 60006 carries a
unit id), so a quantity read from storage or sent to a peer has a runtime
identity.

Against.
* A new value kind across equality, order, hashing, presentation, codecs,
  aggregation and every native that touches numbers.
* Every unit conversion is an exact rational (UNIT-001), so the executor
  needs exact scaled arithmetic over `Int`, `Decimal` and `Float`.
* Aggregates over affine values need the ORNA-UNIT-007 matrix per
  aggregate.

Risks. Largest of the four; critique estimate M for the evaluator part,
larger once codecs and storage are included.

Reversibility. Low.

Rests on.
* Quantities must exist as stored values (columns of type `Float<kWh>`), not
  only as checked expressions.

#### D8-B. Implement erased: units are checked, values are plain numbers

What it is. The checker keeps the full dimension and unit algebra; after
checking, a quantity is lowered to its numeric value expressed in a
declared canonical unit (scale and offset applied at elaboration time, by
the HIR). The executor has no quantity value. Storage keeps the unit
identity in the column type descriptor (`[7, numeric_type, unit_id]`), not
in each value.

Prior art.
* F# units of measure are erased after type checking
  (https://learn.microsoft.com/en-us/dotnet/fsharp/language-reference/units-of-measure).

For.
* Reuses the checker's 1,498 lines; the executor does not change.
* Compatible with the wire descriptor.

Against.
* Anything dynamic loses the unit: a value from `sys.invoke`, a JSON
  decode, or an aggregate over a polymorphic column needs a typed
  boundary; `sys.Value` cannot carry units unless the boundary conversion
  is specified.
* Conversion at elaboration needs the HIR to know every operand's unit, so
  it needs D1 (types known statically for every unit expression).
* The tag 60006 value (number plus unit id) either is produced at the
  boundary or is unused.

Risks. Medium: the elaboration pass is new; dynamic boundaries need rules.

Reversibility. Medium: moving to A later is compatible for programs.

Rests on.
* Programs use units in statically typed code; dynamic quantity values are
  rare or boundary-converted.

#### D8-C. Restrict: a fixed set of built-in dimensions

What it is. Keep `Duration`, calendar `Period`, `Money<C>`, and a closed set
of standard units declared in `core`/std as ordinary declarations (time,
information size, maybe length and mass) with fixed conversions. Users
cannot declare `dim` or `unit`; `Float<kWh>` is accepted only for the
listed units. Affine units are either limited to temperature or cut.

Prior art.
* Rust `std::time::Duration` is a single built-in quantity; units beyond
  time are left to crates (no page cited). SQL's `INTERVAL` is likewise a
  fixed built-in.

For. Covers the real uses in the corpus (time, size, money) with a bounded
implementation and no user-visible dimension algebra.

Against. Removes user-declared units (`dim`, `unit`) and the energy and
power examples in the spec; the checker's `units.rs` shrinks but is not
free; MONEY-003's `kWh` rate example needs `kWh` in the set.

Reversibility. High: the closed set can open into A or B later.

Rests on. The domain's units are few and known.

#### D8-D. Cut units from 1.x

What it is. Remove UNIT-001 to 008, `dim`/`unit` declarations and the
`Quantity` type form from 1.x; reserve the keywords. `Duration`, `Period`
and `Money` stay. Reintroduce in a later minor version.

For. Removes the largest unexecutable surface (critique F10: "Implement in
the evaluator or cut from 1.x"); `units.rs` and its tests leave the
critical path of the HIR.

Against. Loses a differentiator; `Money<GBP> / kWh` (MONEY-003) needs a
replacement story; wire tag 60006 stays reserved and unused.

Reversibility. Medium: re-adding a removed feature in a minor release is
allowed only if 1.x versioning permits additive features (an owner
question under D13).

### Migration cost

| Option | Pinned std | Spec examples | Checker and evaluator |
|---|---|---|---|
| A | time units re-declared as ordinary units in std or core | none | new `Value`, arithmetic, codecs |
| B | none | none | new elaboration pass; units erased after check |
| C | none | `examples/valid/units.orna` and `affine-unit.orna` removed or rewritten | delete user-declared unit machinery |
| D | none | same two examples removed | delete `units.rs` (1,498 lines) and tests |

### What the HIR needs from the answer

* A: `Ty::Quantity { numeric, unit }` with unit `DefId`s and a conversion
  table; executor value for quantities.
* B: `Ty::Quantity` in the HIR, and an elaboration step that rewrites
  unit arithmetic to plain numeric operations; no executor value.
* C: `Ty::Quantity` over a closed enum of units, no user `unit` `DefId`s.
* D: no quantity type; `dim`/`unit` are reserved words.
* Under every option, the numeric-postfix rule must resolve through
  declared names, not the hard-coded list at `lib.rs:5867-5900`.

### Amendment 2026-11-10: AMENDED by owner direction 2026-11-10

Status: AMENDED by owner direction 2026-11-10. This is a decision, not a proposal.
It supersedes the cut in the D8 row of `DECIDED.md` and in the D8 section above,
which is kept as superseded. Specification: `docs/design/units.md`.

The owner's direction: rewrite the unit system and make it better, rather than cut it.

What is decided:

* User-declared units and dimensions return in 1.x as enum-shaped families (D15, owner
  direction 2026-11-10): a family is an `enum`, a unit is a payload-free variant, and
  `kilometre = 1000 metre,` gives the scale. Grammar: units.md section 2. The keyword
  forms `dim` and `unit` of the first 2026-11-10 draft are withdrawn.
* Dimensional algebra is kept and rewritten: exponent vectors over base axes,
  rational scales, and the typing rules of units.md section 6.
* Surface prerequisites (units.md section 11.2): P3a, `impl` and `static` members in
  enums; P3e, a payload-free variant as a type argument (`Quantity<Decimal, Length.kilometre>`).
  Suffix ambiguity: the same suffix in two families in scope is ORNA-S011; the qualified
  form `Family.name(n)` resolves it.
* Built-in units are ordinary declarations in the embedded `core` (units.md
  section 8), in the same form a user writes: time postfixes (`.day`, `.seconds`),
  data size, and the currency family. `Duration`, `Instant`, `Decimal` and `Money<C>`
  remain built-in type names.
* The checker natively knows only the minimum listed in units.md section 5.
* `Band.cold` is cited by the owner as an example. units.md does not specify it, and
  this amendment does not decide its meaning.

Why. Language features are dogfooded: a reader who opens the source of `Duration`,
a unit family or `Money<C>` must find plain Orna, not Rust special cases. Ordinary
declarations plus a small native core let one rule serve built-in and user families
alike.

Consequences:

* The 1.1.0 clauses ORNA-UNIT-001 to 008 are carried forward with their meaning
  (units.md section 10.2). Numbers 009 to 019 are new.
* `Duration` stays the 1.1.0 primitive, wire tag 60005. The time dimension names
  it with the `repr` attribute (units.md O6).
* The finer-unit rule for mixed-scale add is the rule the 1.1.0 checker already
  implements (`inference.rs:1168`), restated as ORNA-UNIT-011.
* Stage H2c of `docs/design/hir.md` is rewritten to implement units.md, not deleted.

Open sub-questions. They stay open for the owner. Each has the default this design
uses until the owner answers (units.md section 12):

* Q1. `day` and `days`: default follows the checker, a calendar `Period`
  (ORNA-TIME-004). The reference `02-types.md` says elapsed `Duration`; to be corrected
  once the owner decides.
* Q2. Conversion name: default `q.to(U)`.
* Q3. Rounding: default a library function, `to(U, scale, rounding)`, not a rule.
* Q4. `canonical`: default a general dimension clause; only time declares one.
* Q5. Nominal form of `Duration`: default a separate primitive (units.md O6), not an
  alias of `Quantity<Decimal, s>`.
* Q6. `Int` time quantity under a `Duration` annotation: default no widening; the
  annotation is a type error (ORNA-UNIT-016 widens only for non-integer ratios).

--------------------------------------------------------------------------

## D9. Table keys and indexes

Bead `ornadb-csenl.9`. Critique F24, F41, section 3.5 "Tables".

Two questions with separate answers: Q1 the default key; Q2 how uniqueness
and indexes are declared.

### Specification today

* ORNA-AUTOID-001 (`07-tables.md:53`): a table without key parameters
  receives an implicit monotonically increasing `Int` key `id`.
  AUTOID-002 (`:55`): allocated ids are never reused. AUTOID-004 (`:59`):
  the allocator high-water mark is stored outside rewindable branch trees,
  suggested ref `refs/orna/ids/<table-object-id>`. AUTOID-006 (`:69`):
  "Independent offline clones have independent allocators and MAY allocate
  the same integer. A merge of different rows with the same automatic ID
  MUST create a typed row conflict and MUST NOT silently renumber".
* The spec's own answer for collision-free creation is an explicit key:
  `pub table Event(id: Uuid = uuid7())` (`:71-78`). ORNA-KEYDEF-004
  (`:97`): key defaults that inspect table state "MAY allocate colliding or
  different keys".
* Permitted key components: `Bool`, `Int`, canonical `Decimal`, `Str`,
  `Uuid`, `Date`, `Instant`, payload-free enums, references, tuples
  (`:268-281`); never `Float` (ORNA-KEY-003, `:283`).
* ORNA-MUT-009 (`:338`): the implicit integer key cannot be re-keyed.
* There is no index declaration. ORNA-ASSERT-018 (`:206`): dedicated
  `unique` and `check` field modifiers are not grammar; ORNA-ASSERT-057
  (`:448`): "Assertion failure MUST NOT be encoded as a hidden stored
  Boolean column, synthetic row or implicit index"; ORNA-ASSERT-058
  (`:450`): an optimizer MAY implement `all_unique`, `every` and
  cross-table predicates through indexes, anti-joins or incremental plans
  when results and witnesses equal the reference evaluation. The only
  mention of "secondary indexes" is ORNA-ROW-008 (`23-storage.md:19`):
  they must use the same ordered store profile and commit with their rows.
* Uniqueness is `assert all_unique(selector)` (`07-tables.md:152`); ORNA-
  ASSERT-004/005 (`:178`, `:180`) define the subject as the complete
  candidate relation. `null` equals `null` in `all_unique`
  (`09-standard-library.md:52`).

### Implementation today

* The automatic key allocator is implemented as a hidden Git ref prefix:
  `AUTOMATIC_ID_REF_PREFIX = "refs/orna/ids/"`
  (`orna-repository/src/allocator.rs:22`).
* Assertion validation reads whole tables. The commit gate's
  `head_table_rows` "encodes every admitted table at `HEAD`" for every
  gated commit in a project that declares assertions, bounded by a
  committed-read limit and past it "treats every candidate table as
  changed" (`orna-cli/src/assertion_gate.rs:77-90`, bead `ornadb-t5tz`).
  I did not find an index structure that serves `all_unique` (nothing in
  the checker or evaluator names one) `[INFERENCE: negative search only]`.
* The evaluator's relation scans cap at 2,048 collection items and 419,600
  steps by default (`orna-evaluator/src/lib.rs:124-126`); this is D9-adjacent
  because `all_unique` is evaluated by the same machinery (critique F03,
  F24).

### The problem

Q1. Fact: automatic keys are integers from a per-clone allocator, and the
spec itself says offline clones collide and a merge of different rows with
one id is a conflict. Judgement (critique): the default key breaks the
product's central workflow of branch and merge; Dolt documents UUID keys as
the answer for the same reason.

Q2. Fact: uniqueness is a full-relation assertion whose cost grows with the
table, and nothing in the language lets a user state "this lookup is
indexed". An optimiser is allowed to recognise `all_unique` but is not
required to, and users cannot tell whether one did.

### Options for Q1: the default key

#### D9-A. Keep the automatic `Int` key as the default

What it is. As specified.

Prior art. SQLite `AUTOINCREMENT` and rowid
(https://www.sqlite.org/autoinc.html); PostgreSQL identity columns
(https://www.postgresql.org/docs/current/ddl-identity-columns.html).

For. Compact, ordered, human-readable ids; matches single-writer SQL
expectations; implemented.

Against. The spec's own collision rule (AUTOID-006); the allocator needs a
hidden ref (AUTOID-004) that survives branch switches.

Reversibility. Low once rows exist with integer ids.

#### D9-B. Default to `Uuid = uuid7()`

What it is. A table without key parameters gets `id: Uuid = uuid7()`
(time-ordered, RFC 9562). The integer allocator becomes opt-in, or is
removed together with AUTOID-001..006 and `refs/orna/ids`.

Prior art.
* Dolt: UUID keys recommended over auto increment for a versioned database
  (https://www.dolthub.com/blog/2023-10-27-uuid-keys/;
  https://docs.dolthub.com/concepts/dolt/git/conflicts).
* PostgreSQL `uuid` type
  (https://www.postgresql.org/docs/current/datatype-uuid.html); UUIDv7 in
  RFC 9562 (https://www.rfc-editor.org/rfc/rfc9562).

For. Merge-safe by construction; removes the hidden ref, AUTOID-002 to 006
and the associated recovery surface; already the spec's own recommended
form.

Against. Ids are 16 bytes and not human friendly; `uuid7()` is an effect
(clock and randomness) evaluated inside the insertion transaction
(ORNA-KEYDEF-001, `:91`), so default key evaluation reaches the effect
system (D2); changes `table Note { ... }` from `id: Int` to `id: Uuid` in
every example and test.

Reversibility. Medium: a later switch of default changes the type of every
implicit key.

#### D9-C. No default key: every table declares its key

What it is. `table Note { .. }` without a key list is an error; a table
must write `table Note(id: Uuid = uuid7())` or another key. Automatic keys
are removed.

Prior art. SQLite `WITHOUT ROWID` tables
(https://sqlite.org/withoutrowid.html) and Datomic, where identity is
attributes you choose (https://docs.datomic.com/schema/identity.html).

For. No hidden allocator and no hidden default; every table says what
identifies it.

Against. More text for every table; breaks the minimal `table Note { }`
example; the REPL start-up story gets longer.

Reversibility. High: a default can be added later.

### Options for Q2: uniqueness and indexes

#### D9-X. Assertion only, as specified

What it is. Uniqueness and lookup constraints are `assert all_unique(..)`;
the optimiser MAY use indexes (ASSERT-058); no user-visible index
declaration.

For. One mechanism for integrity; no second vocabulary; matches ASSERT-018.

Against. Cost is a scan unless an optimiser recognises the form, and users
cannot require it; there is no way to ask for an index to speed a query.

Reversibility. High.

#### D9-Y. Add `unique` and `index` declarations

What it is. A table body gains declarations such as `unique(field, ..)`
and `index(field, ..)`; `unique` is sugar defined as the equivalent
`all_unique` assertion plus a required index; `index` is a performance
declaration with no semantic effect. Witnesses are unchanged (ASSERT-058).

Prior art.
* PostgreSQL `CREATE UNIQUE INDEX` and unique constraints
  (https://www.postgresql.org/docs/current/indexes-unique.html,
  https://www.postgresql.org/docs/current/sql-createindex.html,
  https://www.postgresql.org/docs/current/ddl-constraints.html).
* SQLite `CREATE INDEX`
  (https://www.sqlite.org/lang_createindex.html).
* Datomic schema attributes `:db/unique` and `:db/index`
  (https://docs.datomic.com/schema/schema-reference.html).

For. Users state the cost model; uniqueness checks can be incremental and
O(log n); matches ORNA-ROW-008's reference to secondary indexes.

Against. Reverses ORNA-ASSERT-018's "dedicated `unique` modifier" for
table-level declarations (it forbade field modifiers; a table-level form is
a new decision); adds storage objects (stable ObjectIds, ORNA-OBJECT-001),
merge rules for index definitions and an index maintenance obligation in
the store.

Reversibility. Low: declarations become part of schema history.

#### D9-Z. Key-shaped assertions only

What it is. Keep assertions as the only syntax but specify that
`assert all_unique(r => r.field)` over a single field or tuple of fields,
exactly in that shape, is required to be index-backed; other shapes are
allowed and scan. The reference states the recognised shapes.

For. No new declaration; the cost model is written down; reuses ASSERT-058.

Against. Costs depend on spelling (the critique's F17 complaint in another
place); every recogniser is a normative rule.

Weak: it recreates an index declaration through a syntactic pattern.

### Migration cost

| Option | Pinned std | Corpus | Spec text |
|---|---|---|---|
| Q1-A | none | none | none |
| Q1-B | none | every implicit-key table example and fixture (`id: Int` to `id: Uuid`) | rewrite AUTOID-001..006, remove ref layout |
| Q1-C | none | every keyless table gains a key | remove AUTOID, MUT-009 |
| Q2-X | none | none | none |
| Q2-Y | none | optional adoption | grammar, schema identity, storage |
| Q2-Z | none | none | recognised-shape clause |

I did not count keyless tables in the corpus.

### What the HIR needs from the answer

* `TableDef` with an ordered key list and, per key component, an optional
  default expression (all of Q1 reduces to this).
* Q1-A: a synthetic key component with allocator semantics; Q1-B/C: none.
* Q2-Y: index and unique declarations as `TableDef` members with
  `DefId`s; assertion lowering may reference them.
* Q2-X/Z: assertions lower to `Predicate<Relation<Row>>` expressions
  only; the planner (stream E) decides index use.

--------------------------------------------------------------------------

## D10. Bindings and closures

Bead `ornadb-csenl.10`. Critique section 3.5 "Bindings".

### Specification today

* `05-types.md:63`: "A local name introduced by `let` is a function-local
  slot whose binding may be reassigned by an assignment statement;
  reassignment replaces the entire immutable value". ORNA-VALUE-006
  (`:75`): reassigning a `let` MUST NOT mutate any value previously
  captured, returned, stored or observed through another binding.
* ORNA-VALUE-007 (`:77`) and ORNA-CFLOW-011 (`06-expressions.md:237`):
  `var` is not a declaration; `ORNA091-E-VAR` proposes `let`.
* ORNA-CFLOW-005 and -006 (`06-expressions.md:217-219`): `let` creates a
  slot; assignment is a statement targeting an existing local `let`
  binding, not an expression.
* ORNA-VALUE-003 (`05-types.md:69`): "Closures capture values at creation
  time." ORNA-LAMBDA-003 (`06-expressions.md:269`): "Anonymous functions
  capture immutable lexical values."
* Example at `06-expressions.md:203-206`: `let total = 0; for item in items
  { total = total + item; }`.

### Implementation today

* Checker: assignment looks up the target in the current local map and
  checks the value type (`orna-check/src/inference.rs:1972-2069`, lookup at
  `:1984`). A lambda body is checked with a clone of the enclosing locals
  (`:237`), so an assignment to an enclosing local inside a lambda
  type-checks against the clone. I found no diagnostic for "assignment to a
  captured variable" `[INFERENCE: negative search in inference.rs and
  flow.rs]`.
* Evaluator: closure creation clones the whole scope (`captured =
  scope.clone()`, `orna-evaluator/src/lib.rs:6149`), and calls clone the
  captured scope again (`:10040`), so a closure body that assigns to a
  captured name updates its own copy and the enclosing binding is unchanged
  `[INFERENCE from the clones; I did not run a probe]`.
* Closure scope is a `BTreeMap<String, Value>` that is cloned wholesale
  (`lib.rs:4795` per critique F26): capture cost grows with scope size.

### The problem

Fact: `let` is reassignable and closures capture by value, so an assignment
inside a closure to an outer binding compiles and is silently local to the
closure. Fact: there is no immutable binding at all; a reader cannot tell
from a declaration whether a name will change. Judgement (critique): Rust's
`let`/`let mut` or Gleam's no-reassignment are both more coherent.

### Options

#### D10-A. Keep reassignable `let`; add a rule for captured assignment

What it is. Status quo, plus one reference rule: assigning to a binding
from a scope outside the closure is either a static error, or defined as
assignment to the closure's private copy (documented as such). Optionally
add a lint.

For. No source changes; smallest specification delta.

Against. No immutable bindings; every `let` might be reassigned, so
nothing is "constant" for readers or optimisers (the HIR cannot treat
bindings as single-assignment).

Risks. Low. Reversibility: high (B or C are tightenings).

Rests on. Reassignment is a convenience for loops, not a safety concern.

#### D10-B. `let` is immutable; `let mut` is reassignable

What it is. Two declarations. Assignment targets only `let mut`; a closure
may not assign to a captured binding (static error), or captures `let mut`
by value with a documented copy. `var` stays unused (ORNA-VALUE-007 is
reworded).

Prior art.
* Rust `let` and `let mut`
  (https://doc.rust-lang.org/book/ch03-01-variables-and-mutability.html);
  closures borrow or move
  (https://doc.rust-lang.org/reference/types/closure.html).

For. Intent is in the declaration; the HIR has an immutable-binding fact
to optimise on; closure assignment becomes an obvious error.

Against. Reintroduces a second declaration form the owner removed (`var` was
deleted in 1.1; see ORNA-VALUE-007 and ORNA-CFLOW-011, which say a second
mutability system is unwanted); every existing counter loop changes.

Reversibility. Medium.

Rests on. The reason for removing `var` was spelling, not the existence of
a mutability distinction. `[INFERENCE: the clauses give no reason]`.

#### D10-C. No reassignment: loops use folds and recursion

What it is. `let` is the only binding and is immutable; assignment and
compound assignment are removed. Iteration uses `reduce`, `fold`, recursion
or `for` with a defined accumulator construct.

Prior art.
* Gleam: no reassignment; names can be shadowed by a later `let`
  (https://tour.gleam.run/basics/assignments/).
* Elm and OCaml (the immutable `let`; mutation only through explicit
  `ref`, https://ocaml.org/docs/mutability-imperative-control-flow).
* Erlang single assignment (https://www.erlang.org/doc/system/expressions.html).

For. Closure capture is trivially sound; the HIR bindings are
single-assignment; no mutable-capture rule.

Against. Removes `while` and `for` accumulation idioms used throughout the
pinned std (`bytes.orna:20-28`, `map.orna:35-50`, `list.orna:34-42` all
loop with `index += 1` and `result = append(result, ..)`), so every such
function is rewritten; stack depth for recursion is 64 active calls by
default (`orna-evaluator/src/lib.rs:125`), so loops cannot just become
recursion without raising the bound.

Risks. Large std rewrite, and a language-level answer to accumulation.

Reversibility. Low.

### Migration cost

| Option | Pinned std | Corpus | Spec text |
|---|---|---|---|
| A | none | none | add one captured-assignment rule |
| B | every reassigned local becomes `let mut` (count: not measured; at least the loop functions in `bytes`, `map`, `list`) | the same in fixtures | CFLOW-005/006, VALUE-006/007 |
| C | every loop with an accumulator | every loop | remove assignment from grammar |

### What the HIR needs from the answer

* A binding node with `mutable: bool` (A: always true; B: declared;
  C: always false), and an assignment node that references a binding
  `DefId` and is rejected during lowering if it targets a captured one.
* A closure node with an explicit capture list (names and modes), computed
  once, replacing the cloned-scope capture of today; this is needed under
  every option for performance (critique F26).
* Under C: no assignment node.

--------------------------------------------------------------------------

## D11. Modules, packages, re-export and std replaceability

Bead `ornadb-csenl.11`. Critique section 3.5 "Modules", F13, F14.

Two questions: Q1 is there a re-export form; Q2 what does "`std` is
replaceable" mean, and what is pinned.

### Specification today

* Imports (`03-source-modules.md:40-64`): `use a.b;`, `use a.b as x;`,
  `use a.b.*;`, `use a.b.{X, y};`, `use x as _;` (prelude). The grammar has
  no visibility on `use`: `use_declaration = "use" , namespace_path ,
  use_tail , ";"` (`grammar/orna.ebnf:27`); `visibility` appears only on
  other declarations. No clause defines re-export.
* Yet `09-standard-library.md:9` says "`std.query` and `std.stream` may
  re-export the intrinsic operators with identical identities and
  behaviour", and `:136` says "`std.money` re-exports that protocol".
* `sys` and `std` are the only reserved namespaces (ORNA-NS-005, `:32`);
  `sys` "MUST be built into every conforming implementation and MUST NOT be
  replaced or shadowed" (NS-006, `:34`); `std` "MUST remain optional and
  replaceable, although its top-level name is reserved when present"
  (NS-007, `:36`).
* ORNA-LIB-001 (`09-standard-library.md:11`): library behaviour is
  determined by captured dependency snapshots; ORNA-LIB-002 (`:13`): core
  integrity, transactions and assertions work without `std`, and "An absent
  optional module produces the ordinary import diagnostic; it cannot be
  filled by an undocumented host library with the same spelling." LIB-003
  (`:15`): portable operations have specified semantics "under the
  reference-library profile".
* ORNA-PACKAGE-001 (`08-relations.md:135`): version 1.0 MUST NOT require a
  package registry, semantic-version solver or lockfile where exact Git
  commit pins suffice; PACKAGE-003 (`:139`): `std` "remains an ordinary
  optional attached Orna database/library except for mandatory core
  language and `sys` functions". There is no package or version declaration
  in source.
* Chapter 9 specifies behaviour for `std.text`, `std.bits`, `std.stats`,
  `std.money`, `std.time`, codecs and others (`:128-152`), including
  operations a program cannot implement in Orna (D12).

### Implementation today

* No `pub use`. The pinned `std.query` instead declares 16 public
  functions with marker bodies (`fail(error(code: "ORNA-EVAL-UNSUPPORTED",
  message: "std.query.filter is the core relation intrinsic"))`,
  `query.orna:251-288`) that the runtime binds to the root intrinsic of the
  same name; the owner's native-declarations design counts 16 such
  re-exports (`docs/design/native-declarations.md:94-99`).
* The binding of a std declaration to a native is decided by names at
  load time: `orna-application/src/lib.rs:330-356` looks up
  `(module, leaf name)` in the intrinsic catalogue and requires
  `intrinsic.pins.contains(CURRENT_STD_PIN)`.
* `CURRENT_STD_PIN` is one hard-coded commit hash
  (`orna-intrinsics/src/pin.rs:7`) and `PinRange::contains` is string
  equality (`:38-40`), so a program whose captured std is any other commit
  loses native bindings even though ORNA-LIB-001 says its captured std
  decides its behaviour.
* The runtime checks `sys` shadowing by scope lookup at run time
  (`orna-evaluator/src/lib.rs:554`, `:9804`) though NS-006 forbids it
  `[INFERENCE: the checker side was not confirmed, critique F44]`.

### The problem

Fact: the specification uses re-export in prose and has no syntax for it;
the workaround costs 16 marker declarations. Fact: NS-007 and LIB-002 say
`std` is replaceable while chapter 9 specifies its native behaviour and the
engine binds natives to one hard-coded `std` commit; these cannot all hold
(critique section 3.6). Fact: there is no package or version model beyond
Git pins.

### Options for Q1: re-export

#### D11-A. Add `pub use`

What it is. `pub use a.b.{X, y};` (and `pub use a.b as x;`) re-exports
names with their original identity: the re-exported name resolves to the
same `DefId`, equal for identity and effects. The reference defines
visibility of re-exports and conflicts.

Prior art. Rust `pub use`
(https://doc.rust-lang.org/reference/items/use-declarations.html); OCaml
`include` and module aliases (https://ocaml.org/manual/5.3/modules.html).

For. Removes the 16 marker declarations; "identical identities"
(`09-standard-library.md:9`) becomes a property of the language, not a
runtime table; the HIR needs aliases anyway for imports.

Against. A grammar and name-resolution change (visibility on `use`,
cycle and glob re-export rules). Re-export of a native across a package
boundary interacts with D12.

Reversibility. High: adding it is additive.

#### D11-B. No re-export

What it is. State in the reference that the language has none and delete
"may re-export" from chapter 9. A module needing a second path declares a
forwarding function (an ordinary function, with its own identity).

For. No change. The spec becomes consistent by deleting two sentences.

Against. A forwarding function has a different identity from the original,
so `std.query.filter` is not `filter`; "identical identities" is lost,
which matters for the pinned-std historical story (ORNA-LIB-001) and for
the effect and plan treatment of the core operators (the evaluator today
needs the root-intrinsic identity to treat `std.query.filter` as a relation
stage, `orna-evaluator/src/lib.rs:1116-1135`).

Reversibility. High.

### Options for Q2: what "std is replaceable" means

#### D11-P. `std` is pinned and privileged

What it is. `std` is part of the language distribution in all but file
layout: it is a specific, engine-recognised snapshot (or a set of
supported snapshots) whose natives the engine binds. "Replaceable" in
NS-007 and "optional" in LIB-002 are rewritten to say: the program may omit
it, and may pin a different supported snapshot, but may not provide a
different library under the name `std`.

Prior art. Rust's `std` over `core` and `alloc`
(https://doc.rust-lang.org/core/index.html); Elm's kernel code is
reserved to its own packages
(https://guide.elm-lang.org/interop/limits.html describes the boundary; the
claim about package restrictions is the critique's).

For. Matches what is implemented; keeps natives in std (the
native-declarations design as written).

Against. Contradicts NS-007 as written; "replaceable" disappears from the
language's promises; whoever pins std acquires an ABI (D12).

Reversibility. Medium.

#### D11-Q. `std` is ordinary, replaceable Orna on top of a built-in core

What it is. `core` and `sys` are built in and versioned with the engine
(D12-A). `std` contains only Orna source (no natives); any module may
replace it. NS-007, LIB-002 and PACKAGE-003 are true as written. Chapter 9's
native-backed operations move to `core`.

Prior art. Rust `core`/`std` split; OCaml `external` declarations in the
standard library bind to compiler primitives
(https://ocaml.org/manual/5.3/intfc.html) while the library itself is
ordinary OCaml.

For. The specification is consistent; historical pins (LIB-001) become
tractable because the native ABI is the engine's, not a std commit's.

Against. Moves 113 natives (and the contract text for them) out of
`std`; the specification of chapter 9 splits into "core" and "std" halves;
users who pinned `std` for native behaviour see them in `core` instead.

Reversibility. Medium.

### Package and version declarations (applies to both)

Currently a dependency is a Git commit and nothing in source names it
(PACKAGE-001). The owner may keep that for 1.x or add a package or version
declaration (a manifest, or a module header). This is only a decision for
D11 if D12-C is considered, because natives in third-party packages need a
package identity and an ABI version.

### Migration cost

| Option | Pinned std | Spec text | Engine |
|---|---|---|---|
| Q1-A | delete 16 marker declarations in `query.orna` | grammar, resolution | alias resolution in the HIR |
| Q1-B | none | delete two sentences in chapter 9 | none |
| Q2-P | none | reword NS-007, LIB-002, PACKAGE-003 | none; ABI by pin as today |
| Q2-Q | natives leave `std`: 113 `native fn` become core declarations; `std` keeps only Orna | split chapter 9; NS-007 stays | native table moves to core |

### What the HIR needs from the answer

* Imports lower to alias `DefId`s that resolve to the target definition;
  with A, an alias can itself be exported.
* A `DefId` has a package identity (engine core, `sys`, or a dependency
  pin); the HIR checks `sys` shadowing at resolution (removing the runtime
  test).
* Q2-Q: `std` modules contain no body-less declarations; Q2-P: they may.

--------------------------------------------------------------------------

## D12. Where host functions live

Bead `ornadb-csenl.12`. Critique F13, F14, F15, F27, section 3.6, section
4.2; the owner's design `docs/design/native-declarations.md`.

### Specification today

* `09-standard-library.md:3`: "The language supplies a small intrinsic
  environment sufficient to evaluate source, query and update tables ...
  The optional `std` namespace contains ordinary, explicitly imported
  library code pinned to a Git snapshot." `:7`: the root environment has
  `Some`, `null`, `error`, `fail`, `now`, `uuid7` and the relational and
  stream operations; "Ordinary lexical declarations may shadow
  unqualified helpers; `sys` itself cannot be shadowed."
* `sys` is built in and not replaceable (ORNA-NS-006, `03-source-modules.md:34`);
  it is "the implementation-provided model of the attached database and
  its execution" (`15-system.md:5`). Its functions and relations are
  specified in `api/sys.json` (82 functions, 78 relations) and
  `34-system-reference.md`.
* Chapter 9 normatively specifies `std.text`, `std.bits`, `std.stats`,
  `std.money`, `std.time` and codec behaviour (`:128-152`), several of
  which cannot be written in Orna (D11).
* No clause defines how a function is declared as implemented by the host.

### Implementation today

Three surfaces answer "which functions does the runtime implement":

| Surface | Declared in | Shadowable | Overloads | Bound by |
|---|---|---|---|---|
| Root intrinsics (`Some`, `now`, `uuid7`, `error`, `fail`, relation operators) | Rust, matched by callee spelling (`orna-evaluator/src/lib.rs:9675-9683`) | yes | by arity | name at each call |
| `std` natives (113) | `.orna` with marker or self-call bodies plus the Rust catalogue `orna-intrinsics` (124 entries) | yes | no | catalogue lookup by `(module, leaf)` at admission (`orna-application/src/lib.rs:330-356`) |
| `sys` | `api/sys.json` with Orna signatures as strings, parsed by a second signature parser (`orna-check/src/system_api.rs`); generated ids (`orna-sys-id`, `orna-sys-codegen`, `orna-sys-macros`, `orna-sys-model`, `orna-sys`) | no (checked at run time) | by arity and type | JSON read at run time, then per-function Rust |

* `native-declarations.md:62-92` measures the std surface: 113 natives; 56
  dispatched through a private `__leaf` and 57 through the public name; 83
  top-level `fn __x` declarations in the pin, 68 with a `fail(error(..))`
  marker body; 88 forwarders; 12 public functions that call themselves;
  129 `native fn` declarations the migration would write.
* A call is routed by `route_call` (`orna-evaluator/src/lib.rs:9884`), which
  after resolving the callee tests `pins.contains(CURRENT_STD_PIN)`, a
  constant (D11).
* The owner's proposal: `pub native fn trim(value: Str): Str effect none;`
  admitted only in the pinned standard dependency, bound by `(module path,
  declared name)` at load, unbound is a load error, ABI versioned by the
  pin (`native-declarations.md:34-56`, `:686-705`, `:1019-1043`). It leaves
  `sys` as JSON and keeps `std` the only native carrier (Q5, `:1381-1385`).

### The problem

Fact: three mechanisms for one question, and the declaration in `std` can
lie (a marker body next to a real native). Judgement (critique): serious
languages give one answer, a privileged core versioned with the language
distribution and ordinary code on top (OCaml `external`, Rust `core`
intrinsics, Gleam `@external`, Lua's small C API), and the native
declarations design should (a) not admit natives only in `std`, (b) use an
engine-level ABI version and (c) not give `effect` to natives alone (D2),
and `sys` should use the same form.

### Options

#### D12-A. Built-in `core` and `sys` versioned with the engine; `std` is pure Orna

What it is. Two engine-shipped interface files, `core.orna` and `sys.orna`,
contain only body-less declarations (`native fn ..`, or `fn ..;`). They
parse and check through the ordinary front end; each declaration's
`DefId` is the native or `sys` id. `api/sys.json` becomes generated output
(the critique: invert the source of truth). The native ABI is a number on
the engine, not a std commit. `std` is Orna only and replaceable (D11-Q).
The 113 natives become `core` declarations (or move into Orna if the 14
rows `native-declarations.md:1365-1371` marks as expressible are
rewritten).

Prior art.
* OCaml `external` declarations bind to runtime primitives
  (https://ocaml.org/manual/5.3/intfc.html).
* Rust `core` intrinsics and `extern` blocks
  (https://doc.rust-lang.org/std/intrinsics/index.html,
  https://doc.rust-lang.org/reference/items/external-blocks.html).
* Gleam `@external` (https://tour.gleam.run/advanced-features/externals/).
* PostgreSQL `CREATE FUNCTION ... LANGUAGE internal AS 'symbol'`, checked
  at creation against the backend symbol table
  (https://www.postgresql.org/docs/current/xfunc-internal.html), and
  `extend` mechanisms for extensions
  (https://www.postgresql.org/docs/current/extend-extensions.html).
* Lua keeps a small host API and library tables
  (https://www.lua.org/manual/5.4/manual.html).

For.
* One declaration mechanism for core, `sys` and what are natives today;
  `std` is truly replaceable; ORNA-LIB-001 gets an engine-side ABI to
  version against.
* Retires `orna-intrinsics` tables, `sys-id`, `sys-codegen` and
  `sys-macros` (critique 4.3, F15, F27).

Against.
* The largest change: a second pin-bump story (users keep `std`, but
  functions move to `core`), a spec split of chapter 9, and a new
  load-time check for body-less declarations in `core`.
* The owner's native-declarations work (7 stages, PR #9828) is partly
  superseded: the declaration form stays, its admission rule (pinned std
  only) and ABI rule change.
* Whatever `core` exposes is permanently versioned with the engine; the
  engine cannot drop a native without a language version.

Risks. Scope; ordering against the HIR (the `DefId` of a declaration must
exist before a native binds to it).

Reversibility. Medium.

Rests on.
* The owner accepts that natives are part of the language distribution.
* D11-Q, D2 (a single effect syntax for declarations).

#### D12-B. `std` keeps natives; one declaration form; `sys` unchanged

What it is. The owner's draft design as written: `pub native fn ..;` in
the pinned standard dependency only, bound at load by `(module path,
declared name)`, ABI versioned by the pin, `effect` clause mandatory on
natives, unbound is a load error. `sys` stays in JSON. The 16 `std.query`
re-exports need their own `native fn` (no `pub use`, D11-B) or `pub use`
(D11-A).

For. Smallest delta from the in-flight design; corrects the lying marker
bodies; no change to the spec's "std" namespace.

Against. Two mechanisms remain (Orna declarations for std, JSON for `sys`);
`std` stays privileged, which contradicts NS-007 (D11-P resolves the
contradiction by rewording); the `effect` clause exists for natives only
(critique objection 3) unless D2 generalises it; the binding key is two
strings (`native-declarations.md:707-710`).

Risks. The design records its own open questions (language version
coordinate for the extension: 1.2.0 or errata; vocabulary; user natives).

Reversibility. Medium: moving natives from `std` to `core` later is a pin
bump plus declaration moves.

Rests on. D11-P and D2-B or D2-C (effects on natives only) or D2-A.

#### D12-C. Natives may be declared in any pinned package

What it is. As B, but the admission rule is "a package whose pin the
engine trusts and whose declarations match an ABI the engine implements",
so third-party packages can ship native operations. Needs package identity
and an ABI manifest (D11).

Prior art. PostgreSQL extensions
(https://www.postgresql.org/docs/current/extend-extensions.html) and
Erlang NIFs (https://www.erlang.org/doc/apps/erts/erl_nif.html); SQLite
loadable extensions are the same idea (no page cited).

For. Open ecosystem for host capabilities.

Against. Reverses the owner's draft decision (Q5 "no"); native code from a
package is arbitrary code in the engine, against the trust model in
`26-security.md` `[INFERENCE: not read in full]`; every native needs an ABI
version and a signature check.

Weak for 1.x: nothing in the corpus needs it and the owner's draft
deliberately excludes it.

### Migration cost

| Option | Pinned std | Engine | Spec text |
|---|---|---|---|
| A | 113 natives become `core` declarations; `std` loses its natives | `orna-intrinsics` tables, `sys` codegen and JSON-as-source retired | split chapter 9; define `core` and `sys` interface files |
| B | `std` rewritten as in the draft (130 `native fn` and removed markers) | intrinsics keep identity and dispatch, lose signatures | amend 09 and 03 for `native` |
| C | as B | plus package trust and ABI manifest | as B plus package model |

### What the HIR needs from the answer

* Every function in the HIR is `Source`, `Native` or `Sys` by declaration,
  never by name; `Callee` is computed once (critique F01, F17).
* A and C: natives and `sys` declared in interface modules that go through
  the front end; `DefId` is the id.
* B: natives are body-less declarations in std modules; `sys` is a separate
  table behind a lookup that yields a `DefId`.
* An ABI version carried by the engine (A), by a pin (B) or by a package
  (C), checked once at load.

--------------------------------------------------------------------------

## D13. Specification hygiene

Bead `ornadb-csenl.13`. Critique F04, F29, F30, section 3.1.

### Specification today (facts)

* `implementation/ORCHESTRATOR.md` (119 lines) is part of the specification
  tree and fixes tooling policy: "Root orchestrator: exactly
  `openai-codex/gpt-5.6-terra`" and worker model (`:25-27`), worker counts
  (`:72`), "Missing required model support is a checkpointed blocker, not
  permission to substitute another model" (`:30`).
* `source/40-upgrade-contract.md` is headed "# 37. Compatibility, migration
  and verification" (`:1`, the file is chapter 40), refers to "the supplied
  earlier 1.1.0 authoring draft" (`:8`) and instructs "Keep current
  parser/evaluator/embedded ownership/partial-clone code where" correct
  (`:56`). `START-HERE.md:5` and `CHANGELOG.md:4` say the same of the
  publication.
* Vendor and product names in normative clauses: ORNA-CP-001
  (`12-checkpoints.md:7`), ORNA-TURSO-001..006 (`20-embedded.md:8-13`),
  ORNA-HOT-001 (`22-publication.md:7`) require "embedded Turso"; 16 lines in
  `source/` mention it; `01-scope.md:5` says the specification "does not
  prescribe a compiler implementation language, a user-interface toolkit or
  a hosting profile".
* Internal contradiction: ORNA-PUB-001 (`22-publication.md:11`, default
  compact writer target 16 MiB) against ORNA-UPGRADE-003
  (`40-upgrade-contract.md:17`, one canonical row store, no compact or
  hybrid dual writing).
* Fossils: about 33 of the 80 files in `examples/invalid/` are named
  `legacy-*.orna`; 14 diagnostic codes in the `ORNA091-E-*` family
  (`BOUND-COLON`, `CONVERSION-CHAIN`, `CURRENCY`, `CURRENCY-SYMBOL`,
  `FIELD-CONSTRAINT`, `IMPL-FOR`, `MATCH`, `OPAQUE`, `POSTFIX-QUESTION`,
  `RESULT`, `RETURN-ARROW`, `STATIC-FN`, `TRYFROM`, `VAR`) describe syntax
  that did not ship; ten source chapters say "version 1.0" or "1.0";
  `grammar/orna.ebnf:1` says "Orna 1.0.0 syntactic grammar".
* Size: 947 distinct bold requirement ids and 840 occurrences of "MUST"
  across `source/*.md`; chapter 34 (system reference) is 3,568 lines.

### Implementation today

* Crate descriptions and types use 1.0 names (`FailureStatus::{Failed,
  Succeeded}` at `orna-stream/src/lib.rs:479` against the spec's `open`
  and `recovered`; `Type::Text` against `Str`; critique F43). Not
  re-verified here.

### The problem

Fact: one artefact is a language reference, a product requirements list and
an agent work order. Judgement (critique): nobody can implement or review
against it, it changes when the prompt changes, and the reader cannot
tell live rules from history.

### Options

#### D13-A. Split into three documents

What it is. (1) A language reference (the outline in
[README.md](README.md)); (2) format specifications (canonical value
encodings, row store, publication, protocol, VFS); (3) product
requirements (CLI, REPL, serving, repository behaviour, embedded runtime,
conformance gates). `ORCHESTRATOR.md` and the upgrade contract move out of
the specification into the repository's process documents. Vendor names
become contract statements (transaction, durability, single-owner). History
goes to a migration appendix. The `ORNA-` id space is kept for requirements
that survive.

Prior art.
* OCaml publishes the manual in separate parts
  (https://ocaml.org/manual/5.3/index.html).
* SQLite splits language documentation from the file format
  (https://www.sqlite.org/fileformat2.html).
* The Go specification is the language only (https://go.dev/ref/spec) with
  modules in a separate reference (https://go.dev/ref/mod).

For. Each document has one audience and can change at its own rate; a
second implementation can claim conformance to the part it implements.

Against. Large editing effort (critique estimate L); cross-references
between the three need maintenance; the 1.1.0 text is published and
pinned by artefact hash (`CHANGELOG.md:4`), so restructuring changes the
publication identity.

Reversibility. High: documents can be re-merged.

#### D13-B. One document, cleaned

What it is. Keep the single specification tree. Delete `ORCHESTRATOR.md`,
retitle chapters, fix the PUB-001 contradiction, move fossils to an
appendix, replace vendor names with contract text, and add the reference
content as new chapters.

For. Smallest structural change; keeps ids and cross-references.

Against. The three audiences stay fused; the language reference chapters
live beside 3,568 lines of system reference and product behaviour.

Reversibility. High.

#### D13-C. New language reference beside the frozen 1.1.0

What it is. Leave `reference/Orna-1.1.0` as the frozen historical
publication (hash-pinned). Write the reference as a new document under
`docs/language/` (and a new version line, for example 1.2 or 2.0), which
declares which 1.1.0 clauses it keeps, replaces or drops. The 1.1.0 text
gets an erratum list instead of edits.

For. No change to the pinned artefact; the new reference starts clean; the
native-declarations design's version question (Q1, 1.2.0 or errata,
`native-declarations.md:1350-1355`) is answered by the same mechanism.

Against. Two normative documents for a period; conformance claims must say
which one; "which clauses survive" is a large table to maintain.

Reversibility. Medium.

### Migration cost

| Option | Source text | Fixtures and tests | Publication |
|---|---|---|---|
| A | restructure 42 chapters into three documents | `examples/invalid/legacy-*` move to the migration appendix; conformance corpus references change | new artefact identity |
| B | edit in place | rename or move fossil fixtures | new artefact identity for the edited tree |
| C | add a new tree | none initially | 1.1.0 identity unchanged |

### What the HIR needs from the answer

* Nothing structural. The HIR design needs one thing: the document that
  states the typing, effect and evaluation rules must exist and be the
  authority (README.md), so that the HIR's rules are the specification's
  rules and not a transcription of the checker.

--------------------------------------------------------------------------

## Questions only the owner can answer

These are decisions about intent, not options that more analysis can rank:

1. D1: is terse unannotated declaration a product requirement, or only a
   consequence of INFER-1?
2. D2: which grain of effect do users need to read in a signature: twelve
   kinds, three levels, or two?
3. D3: is `null` a keyword for `None` kept in the language, and is a nested
   option something programs may build?
4. D4: is `std.result` something the language wants users to see? Should a
   handler narrow on a nominal error type? Can cleanup code observe
   cancellation?
5. D5: which of "user-declared overloading" and "types as arguments" does
   the language want, independent of today's platform needs?
6. D6 and D7: do tables need fixed-width integers and a native map or set
   for the domain's first programs, or is the corpus evidence (no
   corpus of user programs yet) enough to defer?
7. D8: are user-declared dimensions and units a 1.x promise, or is a closed
   set (time, size, money) enough?
8. D9: is the branch-and-merge workflow the default workflow, so that the
   default key should be merge-safe?
9. D10: why was `var` removed in 1.1, and does that reason forbid `let mut`?
10. D11 and D12: is `std` a replaceable library or a privileged part of
    the distribution, and may a third-party package ever ship native code?
11. D13: is the 1.1.0 publication frozen (hash-pinned) or editable?

--------------------------------------------------------------------------
