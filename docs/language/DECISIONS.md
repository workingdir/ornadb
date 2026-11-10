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
