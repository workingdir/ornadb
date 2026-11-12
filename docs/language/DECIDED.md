# Orna language decisions (D1 to D19): decided

Status: decided by the owner's delegation on 2026-11-10 ("figure out everything else"), reasoned from the
original plan (`reference/Orna-1.1.0/PLAN.md`, `source/01-scope.md`, `source/02-concepts.md`) and engineering
common sense. The option analysis behind D1 to D13 is in `DECISIONS.md`. D14 to D18 were decided later the same day,
under the owner's delegation of the five open questions of `docs/design/kernel-and-library.md` ("i will let you figure
out those decisions"); their option analysis is in that document, sections 4 and 5. Any decision can be reopened by
the owner; each says what reopening would cost.
D19 was decided on 2026-11-11 under the owner's delegation of the `sys` source questions. The owner amended it the same
day: "no json for sys please. i do not want json for any of the api definitions please." Its option analysis is in
`docs/design/sys-as-native.md`.

## Principles used

1. **Conceptual integrity** (PLAN.md): one way to do a thing, one logical path for a mutation, one authority per fact.
   Two representations of the same idea (two nulls, two declaration mechanisms, two resolvers) are defects.
2. **Statically typed, honestly.** The spec calls Orna a statically typed language. Anything the checker accepts the
   evaluator must be able to run; anything the evaluator cannot run is not in the language yet.
3. **A database for programs whose code, data and history live in Git.** Determinism, exactness, merge safety and
   resource bounds beat convenience. Canonical bytes must exist for every value.
4. **The core is small and ships with the engine; the library is ordinary code.** The spec says the intrinsic
   environment is sufficient without `std` and that `std` is optional and replaceable. The design follows that
   sentence literally.
5. **No silent magic.** Effects, failures, overloads and units are visible to the reader and the tools.
6. **It is a scripting language.** A feature earns its place only if a script author needs it. Effect syntax, user-declared
   units and index declarations do not clear that bar today, so they are not in the language. Mutation is not ceremony:
   assigning to a variable just works.

## Decisions

| Id | Decision |
|---|---|
| D1 | Real unification-based inference inside bodies and lambdas. `pub` functions need full parameter and result annotations. Private functions may omit parameter types; an unresolved type variable is an error that asks for an annotation. No syntactic guessing. |
| D2 | Effects are inferred, published in signature metadata and shown by the tools; legality is checked at the use site against the inferred set. There is no effect syntax for user functions. Only body-less native declarations (engine-embedded) declare their effects. The vocabulary is `sys.EffectKind`. |
| D3 | One absent-value representation: flat `T?` with `null` as its only absent value. `T??` is a type error (no nested options). `Some(x)` stays as the explicit constructor, implicit lifting `T` to `T?` stays. No evaluator mode changes a value's shape. |
| D4 | Unchecked failures with a typed `Error` and rollback at the activation boundary, as the spec says. Inferred failure sets become tooling metadata, not normative. `std.result` is deleted. Cancellation is its own completion (never catchable by `\|?`). |
| D5 | No overloading, for users or for the platform. Arity variants become default parameters; type-directed variants become distinct functions. A type is passed only as a type argument (`decode<T>(raw)`), never as a value argument. The 1.1 spellings stay as deprecated aliases in 1.2. |
| D6 | Add core `Map<K, V>`, `Set<T>` and `Bytes`, immutable and persistent, keys under the canonical total order so iteration is deterministic. `List` gets `push` and `concat`. Blob reads return `Bytes`. Each needs canonical value tags (format work). |
| D7 | `Int` stays arbitrary precision. The digit cap is a resource quota reported as a quota error, not a language rule. No fixed-width integer types until a stated need (`Bytes` indexing yields `Int` in 0..255). The numeric tower has no implicit conversions between `Int`, `Decimal` and `Float`; conversions are explicit and total or failing by name. |
| D8 | **AMENDED by owner direction 2026-11-10.** Units of measure return in 1.x as ordinary Orna declarations. User-declared unit families return as enums (`enum Length { metre, kilometre = 1000 metre }`, D15); the built-in units (time postfixes such as `.day` and `.seconds`, data size, the currency family, `Duration`, `Instant`, `Decimal`, `Money<C>`) are declared in the embedded `core` in the same form a user writes. Dimensional algebra is kept and rewritten (exponent vectors, rational scales). The checker natively knows only the minimum in `docs/design/units.md` section 5. Open sub-questions Q1-Q6 are kept, each with its default. Superseded: the 1.x cut (no user units; dimensional algebra deleted), kept in DECISIONS.md D8 for the record. |
| D9 | Default key is `Uuid` (`uuid7()`), because the product's core workflow is branch and merge. Auto-increment integer keys remain available by explicit declaration with the merge-conflict caveat documented. No index or unique declarations: uniqueness stays an assertion, and the engine MUST recognise the canonical key-shaped forms and serve them from an index (the spec only allowed this before, ASSERT-058). |
| D10 | Mutation just works. `let` binds a variable and `x = value;` reassigns it, exactly as 1.1 allows; there is no `mut`, no `var`, and no immutability ceremony. Closures capture the variable itself (a shared slot within the activation), so assigning to an outer variable inside a closure updates that variable and the closure sees later assignments. Values stay immutable; only the variable slot changes. The one boundary: `parallel` and task bodies capture snapshots, and assigning to an outer variable from them is a compile error, so concurrent children cannot race on a slot. |
| D11 | Add `pub use path;` and `pub use path as name;`. `std` is replaceable ordinary Orna. ~~A package may not ship native code.~~ Superseded 2026-11-10 (see the amendment section). |
| D12 | **Amended 2026-11-10** (see the amendment section). A `native fn` may appear in any module; the engine registry binds `core` and `sys`, and a project-named shared library binds every other module. `std` stays pure Orna on top and re-exports `core` where it wants stable paths. The original D12 text is kept there, marked superseded. |
| D13 | Write a language reference beside the frozen 1.1.0 publication: grammar, typing rules, effect rules, evaluation rules, numeric tower, strings, modules. Format and storage specs and product requirements are separate documents. Orchestrator text and the vendor name leave the normative corpus (the store requirement becomes a transactional contract). Fossils of unshipped syntax move to a migration appendix. The frozen 1.1.0 stays hash-pinned as history; the new reference is 1.2. |
| D14 | Compile-time evaluation of a pure fragment of Orna is the destination, in two stages. Now: P4-A, the unit-key algebra in Rust behind the four-item interface of `docs/design/kernel-and-library.md` section 5.6, so library source never changes. After H4 and H5: P4-B, where the check-time evaluator is the HIR executor running pure `core` bodies (empty inferred effect set, no `const fn` marker); it then takes over interface items 2 and 3. |
| D15 | Units of measure are enum-shaped families (`Length.kilometre`, `5.kilometre`, user-declared families): one declaration form for built-in and user units. The keyword forms `dim` and `unit` of PR #9854 are not shipped; no program declares a unit yet, so nothing migrates. The text after `=` in an enum body is a unit expression under P4-A and an ordinary Orna expression under P4-B. Dimensional algebra is kept (D8, as amended by owner direction). `impl` and `static` members in enums (P3a) are the first parser change. Two unit families in scope that define the same suffix are an ambiguity error (ORNA-S011 style); the qualified form resolves it; there is no last-import-wins rule. The exact surface is in section 5.5 of the design. |
| D16 | Operator protocols use P1-A: the output type is a protocol parameter. Impl variables are implicit where the unit algebra needs them. Type-directed selection of an instance of one protocol member is within D5: D5 forbids a name that denotes several functions, and `a + b` names one protocol member while the types select the instance, as `From<Str>` and `From<X>` already do. Two applicable instances are an error. `==` and `!=` stay canonical equality. `Order` is derived by the engine where a canonical order exists. Scalar pairs keep the direct fast path. Step zero (protocol member calls resolve) is scheduled in H2. A private function whose numeric operand type stays unresolved is a diagnostic that asks for an annotation (D1 unchanged). |
| D17 | Library types keep today's canonical bytes through a registry in Rust (P9-A), with `native type` declarations in `core` for `Instant` and the other kernel representation types. A format change (P9-C) is rejected for now and may be revisited only with a deliberate storage format version. The `core` paths that the registry names are part of the engine ABI: renaming one bumps the `NativeAbi` number, and the build guard checks it. |
| D18 | A call to a loop-free pure `core` function costs one step (P8 R5 option ii), which keeps parity with the special cases. Rules R1 to R4 (static resolution, monomorphisation, inlining, constant folding) stand as written. Explicit per-function weights (option iii) are added only where a measured need appears. The one step re-baseline stays at H9. |
| D19 | The `sys` surface is declared in Orna, and no API definition is JSON. `sys.orna` and submodules, held by the new crate `orna-sys-interface`, and `core/*.orna` are the one hand-written source. SN1 deleted `sys-1.1.0.json` and `generated/sys_api.json` with no byte-equality gate; the frozen 1.1.0 publication stays as a conformance fixture at `crates/orna-conformance/tests/fixtures/reference/api/sys.json`. `engine-1.1.0.json`, `std_intrinsics.json` and their generated JSON are not yet removed. Every consumer reads the declarations. Effect classes, lifecycle (`since`, `removed`, `formerly`, `deprecated`) and overload spellings are declaration clauses. Prose is fixed doc-comment sections. Failure codes are an enum with `__` as the segment break. Typed ids are a small generated set of Rust names, and hosts register by qualified name at link. A deprecated overload spelling is an alias to the one declaration, with a warning (D5). Design: `docs/design/sys-as-native.md`; ADR 0123. |

## Answers to the owner questions

1. D1: terse private code is wanted; public signatures are API and must be written.
2. D2: the tools show the twelve `sys.EffectKind` values; checking uses the same set.
3. D3: `null` stays as the literal; nested options are not allowed.
4. D4: users do not see `Result`; handlers match on the error code; cleanup observes cancellation as a completion, not as an error.
5. D5: neither user overloading nor types-as-values; generics are the one mechanism.
6. D6/D7: yes to `Map`, `Set`, `Bytes`; no fixed-width integers yet.
7. D8: a closed set (time, size, money) is enough for 1.x.
8. D9: branch-and-merge is the default workflow; the default key is merge-safe.
9. D10: `var` was removed to keep one binding form; that is a reason for one form (`let`, reassignable), not for immutability. Mutation stays plain assignment.
10. D11/D12: `std` is replaceable. Amended 2026-11-10: any `.orna` may declare a native function; the implementation
    comes from the engine (`core`, `sys`) or from a shared library the project names.
11. D13: 1.1.0 is frozen history; the language reference is new.

## Answers to the five questions of the kernel and library design

Source: `docs/design/kernel-and-library.md`, section 11. The owner delegated these five questions ("i will let you
figure out those decisions").

1. D14 (compile-time evaluation): yes, as the destination. The unit-key kernel first, then the HIR executor as the
   check-time evaluator after H4 and H5.
2. D15 (units surface): enum-shaped families, `Length.kilometre` and `5.kilometre`. The keyword forms are not shipped.
3. D16 (operator protocols): the output is a protocol parameter. Choosing an instance by the types is within D5.
4. D17 (bytes of legacy types): a registry in Rust plus `native type`. A format change is rejected for now. The
   registered `core` paths are ABI.
5. D18 (steps): one step per call of a loop-free pure `core` function.

Two further resolutions:

* Two unit families in scope that define the same suffix are an ambiguity error (ORNA-S011 style). The qualified form
  resolves it. There is no last-import-wins rule, so a result never depends on import order.
* A private function whose numeric operand type stays unresolved is a diagnostic that asks for an annotation (D1 is
  unchanged). There is no defaulting.

## D14 to D19: reasoning and what reopening costs

### D14. Compile-time evaluation

Why. Principle 4 says the library is ordinary code, and a unit algebra written in Rust is a feature that a reader cannot
open. The destination is Orna that the checker runs. The executor it needs (H4) and `core` as source (H5) do not exist
yet, and units cannot wait for them. So the kernel interface is frozen first, as four items, and the algebra behind two
of them moves later. Library source does not change when it moves. Using the HIR executor as the check-time evaluator
avoids a second evaluator (hir.md risk K3).

Reopening costs. To drop P4-B: the algebra stays in Rust for good, and P7-B and the check-time half of refined literals
stay unavailable. Nothing already built is lost, because P4-A is built first in both cases. To demand P4-B first: 2,000
to 4,000 lines of Rust (an estimate) and a layering hook, before H4 and H5 exist, so H2c slips behind them.

### D15. The surface of unit families

Why. The owner asked for `Band.cold`-style names, `5.kilometre` and user-defined families, and wants the box to open
onto plain Orna. An enum is the declaration form that every user already knows. A keyword form is one more declaration
that the library cannot imitate (principle 1). D8 cut user units and the owner's direction of 2026-11-10 brought them
back, so no program declares one yet and nothing migrates. The text after `=` is a unit expression under P4-A, a new
production in the units design (PR #9854, section 2), and an ordinary Orna expression under P4-B. Whether the P4-A text
stays valid under P4-B is not settled: `docs/design/kernel-and-library.md` section 5.5.1 (point 2) gives the two ways.

Reopening costs. Nothing until H2c-2 ships. After it, every declared family needs a migration. The parser work for
`impl` in enums is not lost, because `Duration`, `Money` and the suffix enums need it anyway. The definition clause and
the family rules would be lost.

### D16. Operator protocols

Why. Without operator protocols, `Instant - Instant`, `Money * Decimal` and every unit product stay special cases
(P1-C). P1-A adds no type-system concept. P1-B, an associated `Out` type, reopens "no associated types". D5 says a name
must not denote several functions, because a reader would have to memorise which names are which. `a + b` denotes no
function. It selects the one instance of one protocol member by the types, the rule that `From<Str>` and `From<X>`
already follow (ORNA-GENERIC-013). Two applicable instances are an error, so there is no priority rule to learn.
Equality and order stay canonical, because `Map` and `Set` keys depend on them (D6).

Reopening costs. If a stricter reading of D5 rejects instance selection, the fallback is P1-C: no operators between
different types, and unit products return to the kernel as rules. Every listing that uses `Add<Rhs, Out>` changes. If
P1-B is wanted instead: projection types in the checker, a way to state `Out = T` in a bound, and a change to the
reference sentence "no associated types".

### D17. Bytes of library-defined legacy types

Why. The bytes are specified (16 golden vectors) and stored data exists. A registry keeps them with no grammar change. A
source clause (P9-B) would add syntax for a coupling that stays in Rust anyway, because the shape of `Money` differs. A
format change (P9-C) costs a migration for four types and gives users nothing. `native type` lets impls nest in kernel
types such as `Instant`. The registered paths are ABI because a rename changes bytes, and the build guard turns a
silent break into a build error.

Reopening costs. P9-C needs new descriptors, new fingerprints, new golden vectors and an `orna-evolution` migration for
stored `Money`, `Duration`, `Range` and `Quantity`, that is, a storage format version. P9-B needs a clause that only
`core` may use, and the registry stays for `Money`.

### D18. Step accounting

Why. Step counts are observable: programs hit `ORNA-EVAL-LIMIT` at a count, and tests assert counts. One step for a call
to a loop-free pure `core` function keeps parity with the special cases, so moving a feature into the library does not
make existing programs hit a quota sooner. The rule is short to state and to test, and it needs no annotation. A long
`core` body costs the same as a short one. That is accepted, because `core` bodies are small and reviewed.

Reopening costs. Option (i), every node: a library operation costs several steps, the default budget (419,600) and every
step-count test are re-baselined at H9, and programs that use `Money` or time hit quotas sooner. Option (iii), explicit
weights: one annotation per `core` function that readers must trust.

### D19. The `sys` surface is declared in Orna

Why. The owner rejected a JSON schema as the definition of `sys` and asked for something native. The owner then ruled
out JSON for every API definition, generated or not. Today nine files state the surface, four hand-written parsers read
its signature strings, and the one check on the generated `sys.orna` has no caller
(`docs/design/sys-as-native.md` section 1). Principle 1 (one authority per fact) and principle 5 (effects and failures
are visible to the reader and the tools) point at one source that the front end reads. Every consumer reads that
source, and a data format derived from it would be a second statement of the same facts. The proof is a test that
compares the typed model of the declarations with the typed model of the JSON, and the JSON is deleted only after each
consumer has flipped. Effect classes in the declaration are the only authority: the checker's `host_effect` table and
`sys.orna` agree on 3 of 36 non-read callables, so the table is a defect source, and it goes when the checker moves to
HIR (H3). Prose is fixed doc-comment sections because no code acts on it, and a clause per fact would add grammar for
no behaviour. Lifecycle facts are clauses because admission, diagnostics and the profile view act on them, and doc tags
are excluded. A small generated id set keeps Rust naming identities through typed ids, and hosts register by qualified
name at link, so no hot path looks up a string. A deprecated overload spelling is an alias to the one declaration with
a warning, which is D5 applied to `sys`. The frozen 1.1.0 publication is history, not a source: it stays as hash-pinned
test fixtures, nothing in the build reads it, and one conformance test compares the declarations with it. Sealed engine
ids stay with the archive as a Rust table until the archive decision, and host operations move at SN7.

Reopening costs. To keep JSON as a generated output: an exporter, its profile rules and a byte-equality check stay,
and readers can drift back to the generated file, which is how nine files came to state one surface. To keep JSON as
the source: the four signature parsers and the second copies stay, `sys.orna` stays a lossy projection, and the H5
acceptance sentence (byte-equal JSON from `sys.orna`) cannot be met. To declare the engine catalogue as `engine.orna`:
the sealed 16-byte ids need an identity token in the language, which section 3.6 of
`docs/design/native-declarations.md` rejects, or a second table. To keep the old three-class effect mapping: the
declarations misstate 33 of 36 non-read callables and `host_effect` survives. To use per-fact clauses for prose: five
more clauses for about 90 uses. To drop the generated ids: Rust names every `sys` function and failure code by string,
which undoes the sys-identity invariant. To check generated Rust in: a drift check that must be wired, and a generated
diff in every PR that touches `sys.orna`.

## What this unblocks

- HIR stages H1 onward: D1, D3, D4, D5 give the type and call model; D2 the effect field; D6 to D8 the value model;
  D9 the table-op and index model; D12 the host-function declaration form and the `Callee` set.
- The native-declaration work resumes in a changed form: body-less `native fn` is parsed everywhere and is accepted in
  any module (amended 2026-11-10, see the amendment section). The binding is at link: the engine registry for `core`
  and `sys`, a project-named shared library for every other module. Design: `docs/design/native-libraries.md`.
- Typed sys ids resume as the generated binding of `sys.orna`, not as a second source of truth. D19 records the source,
  the deletion of the JSON and the staging.
- HIR stage H2 (typing): step zero, protocol member calls resolve, then operator protocols (D16) and the H2c units work:
  the P4-A unit-key kernel behind a four-item interface (D14), then the enum-shaped families (D15).
- HIR stage H4 (executor): the step charge of D18. HIR stage H5 (host, link): the wire registry and `native type`
  (D17). The check-time evaluator (P4-B, D14) starts after H4 and H5.

## Follow-up work items created from these decisions

Spec work (language reference chapters), parser work (`pub use`, type arguments, the closure slot-capture rule,
`Map`/`Set`/`Bytes` literals), checker work, canonical tags for the new collections, and the deprecation aliases are
tracked as beads under `ornadb-csenl` (see the bd comments on `ornadb-csenl.1` to `.13`).

The work for D14 to D18 is ordered by HIR stage in `docs/design/kernel-and-library.md`, section 8.2. The first parser
change is `impl` and `static` members in enums.

## Amendment 2026-11-10: native code in any module (D11 and D12 amended)

The owner overruled the part of D11 and D12 that confined native code to the engine. The owner's words:

> any .orna should be able to use native functions, you just gotta load the .so with the declaration

New rule, replacing the superseded text below:

1. A body-less `native fn` declaration is accepted in any module unit of a project, a package or `std`.
2. Every native declaration is bound exactly once, at link and before evaluation, by its key (module path, name).
3. A key in the engine-embedded modules `core` and `sys` is bound from the engine registry, as before.
4. Any other key is bound from a shared library (`.so`, `.dylib` or `.dll`) that the root project names in its
   manifest, with a SHA-256 digest of the file pinned in Git. A library that cannot be found, does not match its
   digest, has the wrong ABI, or lacks a symbol is a distinct link error that names the declaration and the library.
5. Every native still declares its `effect` clause. The engine cannot verify a library's effect; a library native is
   labelled "host trust, not sandboxed" (ORNA-EXT-005).

Open owner decisions (options and trade-offs in `docs/design/native-libraries.md`, section 14): the manifest name and
format, whether only the root project may name libraries, in-process `.so` versus out-of-process execution given the
ORNA-MEDIA-005 time and memory MUST, the effect model for libraries, digest-only versus an operator allowlist, and
whether the browser profile rejects a program that uses a library native (the design proposes rejection at link).
This amendment changes no Rust code. The superseded text follows, verbatim, for the record.

Superseded D11 sentence: "A package may not ship native code."

Superseded D12 text (2026-11-10, before this amendment): "Host functions live in an engine-versioned `core` and `sys`,
declared body-less in embedded `core.orna` and `sys.orna` and bound by `DefId` to implementations in the engine.
`std` is pure Orna on top and re-exports `core` where it wants stable paths (`std.bits.bit_or` is
`pub use core.bits.bit_or`). Natives are declared only in engine-embedded modules; this supersedes the earlier
native-declarations decision that allowed them in pinned std modules. Old pinned std keeps working through an engine
`NativeAbi` version."

Superseded answer 10 text: "D11/D12: `std` is replaceable; no third-party native code."
Superseded "What this unblocks" text: "accepted only in engine-embedded `core.orna` and `sys.orna`."
