# Orna language decisions (D1 to D13): decided

Status: decided by the owner's delegation on 2026-11-10 ("figure out everything else"), reasoned from the
original plan (`reference/Orna-1.1.0/PLAN.md`, `source/01-scope.md`, `source/02-concepts.md`) and engineering
common sense. The option analysis behind each decision is in `DECISIONS.md`. Any decision can be reopened by the
owner; each says what reopening would cost.

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
5. **No silent magic.** Effects, failures, mutation, overloads and units are visible to the reader and the tools.

## Decisions

| Id | Decision |
|---|---|
| D1 | Real unification-based inference inside bodies and lambdas. `pub` functions need full parameter and result annotations. Private functions may omit parameter types; an unresolved type variable is an error that asks for an annotation. No syntactic guessing. |
| D2 | Effects are inferred, published in signature metadata and shown by the tools; legality is checked at the use site against the inferred set. No effect syntax for user functions, except an optional `effects(...)` assertion the checker verifies. Body-less native declarations must declare theirs. The vocabulary is `sys.EffectKind`. |
| D3 | One absent-value representation: flat `T?` with `null` as its only absent value. `T??` is a type error (no nested options). `Some(x)` stays as the explicit constructor, implicit lifting `T` to `T?` stays. No evaluator mode changes a value's shape. |
| D4 | Unchecked failures with a typed `Error` and rollback at the activation boundary, as the spec says. Inferred failure sets become tooling metadata, not normative. `std.result` is deleted. Cancellation is its own completion (never catchable by `\|?`). |
| D5 | No overloading, for users or for the platform. Arity variants become default parameters; type-directed variants become distinct functions. A type is passed only as a type argument (`decode<T>(raw)`), never as a value argument. The 1.1 spellings stay as deprecated aliases in 1.2. |
| D6 | Add core `Map<K, V>`, `Set<T>` and `Bytes`, immutable and persistent, keys under the canonical total order so iteration is deterministic. `List` gets `push` and `concat`. Blob reads return `Bytes`. Each needs canonical value tags (format work). |
| D7 | `Int` stays arbitrary precision. The digit cap is a resource quota reported as a quota error, not a language rule. No fixed-width integer types until a stated need (`Bytes` indexing yields `Int` in 0..255). The numeric tower has no implicit conversions between `Int`, `Decimal` and `Float`; conversions are explicit and total or failing by name. |
| D8 | Cut user-declared units and dimensions from 1.x (reserved syntax). `Duration`, `Instant`, `Decimal` and `Money<C>` stay as built-in types with their literal postfixes (`1.day`). The checker's dimensional algebra is deleted. |
| D9 | Default key is `Uuid` (`uuid7()`), because the product's core workflow is branch and merge. Auto-increment integer keys remain available by explicit declaration with the merge-conflict caveat documented. Add `unique` and `index` declarations (syntax reserved now, implemented in the table engine). An assertion that is exactly a declared uniqueness is recognised as one. |
| D10 | Keep `let` reassignable within its own scope (the 1.1 decision stands). Closures capture by value, and assigning to a captured binding inside a closure is a compile error, so a copy is never updated silently. |
| D11 | Add `pub use path;` and `pub use path as name;`. `std` is replaceable ordinary Orna. A package may not ship native code. |
| D12 | Host functions live in an engine-versioned `core` and `sys`, declared body-less in embedded `core.orna` and `sys.orna` and bound by `DefId` to implementations in the engine. `std` is pure Orna on top and re-exports `core` where it wants stable paths (`std.bits.bit_or` is `pub use core.bits.bit_or`). Natives are declared only in engine-embedded modules; this supersedes the earlier native-declarations decision that allowed them in pinned std modules. Old pinned std keeps working through an engine `NativeAbi` version. |
| D13 | Write a language reference beside the frozen 1.1.0 publication: grammar, typing rules, effect rules, evaluation rules, numeric tower, strings, modules. Format and storage specs and product requirements are separate documents. Orchestrator text and the vendor name leave the normative corpus (the store requirement becomes a transactional contract). Fossils of unshipped syntax move to a migration appendix. The frozen 1.1.0 stays hash-pinned as history; the new reference is 1.2. |

## Answers to the owner questions

1. D1: terse private code is wanted; public signatures are API and must be written.
2. D2: the tools show the twelve `sys.EffectKind` values; checking uses the same set.
3. D3: `null` stays as the literal; nested options are not allowed.
4. D4: users do not see `Result`; handlers match on the error code; cleanup observes cancellation as a completion, not as an error.
5. D5: neither user overloading nor types-as-values; generics are the one mechanism.
6. D6/D7: yes to `Map`, `Set`, `Bytes`; no fixed-width integers yet.
7. D8: a closed set (time, size, money) is enough for 1.x.
8. D9: branch-and-merge is the default workflow; the default key is merge-safe.
9. D10: `var` was removed to keep one binding form; that reason does not require `let mut`, so it is not added.
10. D11/D12: `std` is replaceable; no third-party native code.
11. D13: 1.1.0 is frozen history; the language reference is new.

## What this unblocks

- HIR stages H1 onward: D1, D3, D4, D5 give the type and call model; D2 the effect field; D6 to D8 the value model;
  D9 the table-op and index model; D12 the host-function declaration form and the `Callee` set.
- The native-declaration work resumes in a changed form: body-less `native fn` is parsed everywhere (N1 stands) but
  accepted only in engine-embedded `core.orna` and `sys.orna`.
- Typed sys ids resume as the generated binding of `sys.orna`, not as a second source of truth.

## Follow-up work items created from these decisions

Spec work (language reference chapters), parser work (`pub use`, `unique`/`index`, type arguments, `effects(...)`,
`Map`/`Set`/`Bytes` literals), checker work, canonical tags for the new collections, and the deprecation aliases are
tracked as beads under `ornadb-csenl` (see the bd comments on `ornadb-csenl.1` to `.13`).
