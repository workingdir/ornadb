# Kernel and library: language features written in Orna

Status: design. Five decisions, D14 to D18, were recorded on 2026-11-10 under the owner's delegation (section 11 and
[DECIDED.md](../language/DECIDED.md)). Everything else is a proposal for review. Changes no production code. Docs only.

Owner direction, quoted (2026-11-10):

> make as many core language USPs from the primitives we have in the Orna language. If you can figure out some way of
> using enum or something for units that would be insane. I want people to use the language and think it is magic,
> and then when they open the black box and look inside how the language features actually work (as they are
> dogfooded and written in Orna) they think how amazing that is.

Input documents: [hir.md](hir.md), [native-declarations.md](native-declarations.md),
[DECIDED.md](../language/DECIDED.md), [DECISIONS.md](../language/DECISIONS.md), the language reference
([README](../language/README.md)), the frozen 1.1.0 text under `reference/Orna-1.1.0/source`, and the units design on
PR #9854 (file docs/design/units.md on branch docs/units-ordinary-declarations, head `9cd0fddfc`).

Evidence base:

* Implementation evidence is `origin/main` = `a4cf733b1`. A bare `file:line` is relative to `crates/` at that commit.
  The path prefixes `C:`, `E:` and `H:` in tables stand for `orna-check/src/`, `orna-evaluator/src/` and
  `orna-hir/src/`.
* The pinned standard library is `stdlib/std` at `fdf63e87`. Counts of its contents are in Appendix A.
* Two facts come from open PRs and are marked: #9850 (HIR typeck fixes, head `cc718ae26`) and #9854 (units, head
  `9cd0fddfc`; its section and rule numbers were read at `86be7f979`, and the later commit changed one line).
* A claim marked `[probe]` comes from running a prebuilt `orna check` binary on a small module. The binary is
  `target/FixAppLive/debug/orna`, built on 2026-11-09 from a lane branch. It is not a build of main. Appendix B lists
  every probe with its output. Re-run a probe before you rely on it.
* A number that no command produced is marked `[INFERENCE]`.

Reading rules: every section starts with its conclusion. Option analysis is neutral: it lists what each option
costs and buys. A decision appears as "Decided (Dn)" and is recorded in DECIDED.md. A default that nobody has decided
appears only as "What I do until told otherwise".

--------------------------------------------------------------------------

## 0. Conclusions

**The answer.** Most of what users meet as "the language", beyond the type system itself, is Rust special cases that a
library could be. Section 3 inventories 26 of them. Twelve can be ordinary Orna: one today, eleven with the primitives
of section 4. Five split into a small kernel core and a library rest. Nine stay kernel. The special cases measured for
Money, time, suffixes and units are 2,385 lines of Rust (Appendix A).

**The kernel after the change** is what no user can write: evaluation, the type solver and the effect pass, the
representations and canonical bytes of values, tables and Git, the natives, resource accounting, and a short list of
tables (lang items, wire registry). Section 9.3 lists it. Everything else is `core` and `std`, written in Orna.

**Ten primitives close the gaps** (section 4). Each is smaller than what it replaces.

| Id | Primitive | One line |
|---|---|---|
| P1 | Operator protocols | `a + b` calls an instance of `Add`; step zero is that protocol member calls resolve |
| P2 | Literal suffixes | `n.name` finds an enum variant or a target type, not a hard-coded list |
| P3 | Member-surface gaps | impls in enums; variant by expected type; refined `base`; statics via `T`; variant types |
| P4 | Unit algebra | a unit-key kernel (PR #9854), or a pure fragment of Orna evaluated at check time |
| P5 | `Display` and interpolation | `{x}` calls `Display` for any type |
| P6 | Derive | none in the first cut |
| P7 | Library-worded diagnostics | message templates as statics; the kernel keeps codes and spans |
| P8 | Executor contract | resolve, monomorphise, inline, fold: library arithmetic costs what the special case cost |
| P9 | `native type` and a wire registry | library types keep today's canonical bytes |
| P10 | Lang items | each piece of sugar names its library target by a path in `core` |

**Units** (section 5). The existing checker already accepts `5.kilometre + 200.metre` and rejects `5.metre + 3.second`
with `cannot add Length and Time` `[probe]`. The evaluator cannot run any of it. Three decisions shaped the design:
whether the unit is in the type or the value, who computes a product of units, and what the surface is. All three are
decided. The unit is in the type and erased at run time (the D8 amendment on PR #9854). The algebra is PR #9854's
unit-key kernel now (P4-A, D14) and Orna evaluated at check time later (P4-B). The surface is enum-shaped families
(D15): `Length.kilometre`, `5.kilometre` and user-declared families, one declaration form. Both algebras sit behind a
four-item kernel interface, so the move from A to B changes no library or user source. Section 10 lists twelve changes
to units.md and accepts each; none overwrites it.

**What lands when** (section 8.2). H1 takes the grammar and resolver work. Impls in enums (P3a) are the first parser
change (D15). H2 takes protocol member calls, operators, suffixes, interpolation and the unit-key kernel. H3 takes the
diagnostics and the effect chain. H4 takes the executor rules and the step charge of D18. H5 moves bytes and natives:
`core.orna` as source, the wire registry, `native type` (D17). The check-time evaluator (P4-B) starts after H4 and H5
(D14).

**Findings the owner may not expect.**

* Enums cannot own `impl` or `static` members. The grammar and the parser agree (`ORNA-PARSE-003` `[probe]`).
* Protocol member calls do not resolve in the probed build, and the HIR typeck defers them. The pinned std declares 11
  protocols and has no `impl` block (Appendix A).
* `"{n}"` with an `Int` fails in the checker and the evaluator. Three components disagree about interpolation.
* `1.day` is a calendar `Period` in the checker and the evaluator, and a `Duration` in reference chapters 1 and 2.
* A generic nominal type cannot use its parameter as a field type in the probed checker (`type Box<T> { value: T }`).
* `std.option.flatten` takes `T? ?`, which D3 makes an error. `pub use` is decided but not yet in the parser.
* Money, Duration, Instant, Range and Quantity have fixed canonical tags. A library definition must keep them or the
  format changes (P9).

**Decided 2026-11-10, under the owner's delegation** (section 11 and [DECIDED.md](../language/DECIDED.md)):

1. D14: compile-time evaluation of a pure fragment of Orna is the destination, staged: the unit-key kernel now (P4-A),
   the HIR executor as the check-time evaluator after H4 and H5 (P4-B).
2. D15: units are enum-shaped families; the keyword forms are not shipped; impls in enums (P3a) come first.
3. D16: operator protocols are P1-A with implicit impl variables; instance selection is within D5.
4. D17: library types keep their bytes through the Rust registry (P9-A) and `native type`; a format change is rejected
   for now; the `core` paths the registry names are ABI.
5. D18: a call to a loop-free pure `core` function costs one step (P8 R5 option ii).

--------------------------------------------------------------------------

## 1. The principle and three tests

Conclusion: a feature is library when a user could write it with the declarations they can write, and the library
version keeps the typing, effects, errors, bytes and speed of the special case. The kernel is what remains. Section 3
applies this test to every special case in Rust today.

### 1.1 Definitions

* **Kernel**: Rust code the engine owns. It has six parts: the evaluation rules of syntax; the type-system rules
  (unification, protocol selection, exhaustiveness, effect inference); the representation and canonical bytes of
  values; natives ([D12](../language/DECIDED.md)); tables, transactions, assertions and Git; resource accounting.
* **Library**: Orna source in `core` and `std`. `core` is embedded in the engine and versioned with it (D12). `std` is
  pinned to a Git commit and replaceable (D11). Library code uses only declarations a user can write: `type`, `enum`,
  `protocol`, nested `impl`, `fn`, `static`, `use`, `pub use`. The one exception is `native fn`, which is accepted
  only in `core` and `sys`. This document proposes `native type` as a second exception (section 4, P9).
* **Glass box**: go-to-definition from any name a user can write ends in Orna source, except at a `native`
  declaration, where it ends at a signature, an effect clause and the name of the Rust function bound to it. The
  editor and the engine share one resolver (hir.md section 3.4), so this holds by construction once `core` is
  ordinary HIR.

### 1.2 Three tests

A candidate feature is library when all three hold.

1. **Writable.** A user can write it in their own module with the current grammar plus the primitives this document
   proposes, and the proposed primitives are general (they serve more than this one feature).
2. **Equal.** It has the same typing rules, effect set, error codes, canonical bytes and cost as the special case it
   replaces. Where it cannot, the difference is listed and the owner decides.
3. **Readable.** A reader who opens the source learns how the feature works. Source that only forwards to a native
   fails this test: it is a declaration, not an explanation.

### 1.3 How this sits with the decisions

| Decision | Relation to this document |
|---|---|
| D3 flat options | Unchanged. `T?` and lifting stay kernel. `std.option` already is library. |
| D5 no overloading | Operator protocols are not overloading (D16). Section 4, P1 states why and what it costs. |
| D8 units | Amended by owner direction on PR #9854. D15 sets the surface: enum-shaped families. Sections 5 and 10. |
| D11 `pub use`, replaceable std | Used for re-exports such as `pub use TimeUnit.hour as hours;`. |
| D12 natives in `core` and `sys` | Extended to `native type` and to body-less impl members in `core` (P9). |
| D14 to D18 | Decided on 2026-11-10 under the owner's delegation. Section 11 states each; sections 4 and 5 use them. |

Principle 4 of DECIDED.md says: "The core is small and ships with the engine; the library is ordinary code." This
document is its long form.

### 1.4 What "magic" means here

A feature is magic to use when its rule fits one sentence in the manual and the checker has no exception for it. It
is amazing to open when the source is short, uses only what the reader knows, and says why. A feature that is magic to
use and opaque to open is a special case with good manners. The inventory in section 3 lists those.

--------------------------------------------------------------------------

## 2. The primitives the language has today

Conclusion: the language has twenty primitives a library can use. Most library features need only these. Thirteen gaps
stop the rest. Two (G7 and G13) are in the checker, not in the design: protocol members are declared and checked but a
call such as `t.show()` does not resolve in the build probed, and a generic type cannot use its parameter as a field
type. The HIR typeck reports the first gap as well.

### 2.1 Primitives (U)

Sources: reference chapters 1 to 4 under [../language/reference/](../language/reference/00-overview.md), DECIDED.md.

| Id | Primitive | Specified in | A library can build |
|---|---|---|---|
| U01 | Nominal type, private fields, nested `impl` | 02-types "Nominal, alias and refined" | Money, Range, Duration |
| U02 | Refined nominal type `type A = B { assert ..; }` | 02-types; ORNA-REFINE-001..005 | Percent, `Money<C>` |
| U03 | Transparent alias `type A = B;` | 02-types | names for keys |
| U04 | Enum: closed, nominal, payload variants, `case` | 02-types "Enums" | Rounding, Ordering, TimeUnit |
| U05 | Protocol: functions, `static` properties, `Self` | 02-types "Generics and protocols" | Currency, Display, Add |
| U06 | Generics with `impl` bounds, explicit type arguments | 02-types; D5 | `sum<T>`, `Range<T>` |
| U07 | `From<S>` and `Target.from(source)` | 02-types; ORNA-CONVERT-001..010 | Email, `12.34.GBP` |
| U08 | `static` read `Type.name` | 1.1.0 ORNA-MEMBER-006 | currency code |
| U09 | Flat option `T?`, `Some`, `null`, `??`, null patterns | 02-types; D3 | `std.option` |
| U10 | Pipes, lambdas, closures, function values | 03 | table helpers |
| U11 | Tables: computed fields, defaults, assertions | 00-overview | computed helpers |
| U12 | Inferred effects, restricted contexts | 04 | pure operators in assertions |
| U13 | `fail`, `\|?`, typed `Error` | 04 | validation |
| U14 | Ranges `a..b`, `a..=b`, `x in r` | 02-types (kernel type today) | uses only |
| U15 | String interpolation `{expr}` | 01 | formatting |
| U16 | Modules, `use`, `pub use`, reserved namespaces | 01; D11 | replaceable std |
| U17 | `native fn` in `core` and `sys` only | 01; D12 | the kernel boundary |
| U18 | Default and named arguments | 03 | `divide(scale:, rounding:)` |
| U19 | Int unbounded, Decimal exact, Float; no implicit conversion | 02-types; D7 | exact money |
| U20 | `Instant`, `Duration`, `Period`, `Date` (kernel types) | 02-types | uses only |

Two kernel facts the rest depends on:

* `core.orna` already declares a library type: `pub type Error { code, message, cause }`
  (`orna-hir/interface/core.orna`). The pattern "library-declared type, kernel-raised value" exists.
* The 1.1.0 text says `Currency` "is a core protocol because `Money<C>` must remain meaningful without `std`"
  (`reference/Orna-1.1.0/source/09-standard-library.md:136`). The checker holds that protocol as a Rust string
  (`C:lib.rs:2904-2923`).

### 2.2 Gaps (G)

| Id | Gap | Evidence | Closed by |
|---|---|---|---|
| G1 | `a + b` on a library type is a type error | `[probe]` `E2001: operator + is not defined` | P1 |
| G2 | Suffixes beyond the built-in names | `[probe]` `5.metre`: `unit suffix cannot be resolved` | P2 |
| G3 | An enum cannot carry `impl` or `static` | `[probe]` `ORNA-PARSE-003`; `orna-syntax/src/parser.rs:279-283` | P3a |
| G4 | `C.code` through a type parameter does not resolve | `[probe]` `ORNA-S012-UNRESOLVED` | P3d |
| G5 | A refined value has no way to read its base | `C:lib.rs:5224-5230` offers only `from` | P3c |
| G6 | `rounding: half_even` is a name-based special case | `C:calls.rs:2701-2702`; DECISIONS.md D5 text | P3b |
| G7 | A protocol member call `t.show()` does not resolve | `[probe]` `field access requires a record` | P1 step 0 |
| G8 | Interpolation takes `Str` only | `C:lib.rs:3718`; `E:lib.rs:6706`; `H:typeck/mod.rs:715` (a fixed list) | P5 |
| G9 | No compile-time evaluation, no type-level arithmetic | no code | P4 |
| G10 | A library type cannot keep a legacy canonical tag | `reference/Orna-1.1.0/source/29-formats.md:24-29` | P9 |
| G11 | A library type cannot word its own diagnostics | strings live in Rust (section 3, F26) | P7 |
| G12 | The pinned std never exercises protocols | 11 protocols, 0 `impl` blocks, 0 `static` (Appendix A) | adoption |
| G13 | A generic type cannot use `T` as a field type | `[probe]` `type Box<T> { value: T }` fails | checker |

Note on G3: some working notes list "enums with `impl` and `static` members" as existing. They do not. The 1.1.0
grammar (`reference/Orna-1.1.0/grammar/orna.ebnf:95-98`) and the 1.2 reference grammar give `enum_declaration`
variants only. Types, refined types and tables own `impl` blocks. Enums do not.

Note on G7: the probe declared a protocol, implemented it in a nominal type and called the member on a value and on a
bound type parameter. Both calls failed `[probe]`. The source agrees. `infer_field` tries six name-based member probes
(text, numeric, currency suffix, numeric postfix, unit suffix, refined `from`; `C:inference.rs:143-185`), then reads
record fields, and otherwise reports `field access requires a record` (`:214`, `:220`). It never looks up an impl
member. Operators, `Display` and suffixes all need protocol member calls to resolve, so this is step zero of P1 and P5.

--------------------------------------------------------------------------

## 3. What is hard-coded in Rust today

Conclusion: 26 features that users meet as "the language" are Rust special cases. Nine are kernel work and stay so. Five
split into a kernel core and a library rest. Twelve move to library Orna: one with the primitives that exist, eleven
with the primitives proposed in section 4. The functions measured for Money, time, suffixes and units alone are 2,385
lines of Rust (Appendix A).

### 3.1 Where each one lives

Prefixes: `C:` = `orna-check/src/`, `E:` = `orna-evaluator/src/`, `H:` = `orna-hir/src/`. `I:` = the interface files in
`orna-hir/interface/`. Line numbers are at `a4cf733b1`.

| Id | Feature | Evidence |
|---|---|---|
| F01 | Postfix literals (`1.day`, `3.hour`) | `C:lib.rs:5320-5399`; `E:lib.rs:5885-5972`; #9850 `H:lower/literals.rs` |
| F02 | `Currency` protocol, held as a Rust string | `C:lib.rs:2904-2923`; `C:item_checks.rs:15`; `C:scope.rs:75` |
| F03 | `Money<C>`, `12.34.GBP`, `Money<C>(x)` | `C:lib.rs:5274-5309`, `:5499-5558`; `C:item_checks.rs:1570-1600` |
| F04 | Money arithmetic, mismatch | `C:inference.rs:1086-1113`; `C:lib.rs:5326`; `E:lib.rs:7067-7092`, `:7616-7664` |
| F05 | Money quantize, allocate, format | `orna-intrinsics/src/modules/money.rs`; `E:lib.rs:10354`, `:10732-10769` |
| F06 | `Duration`, `Instant`, `Period` operators | `C:inference.rs:1034`; `E:lib.rs:7097-7288`; `H:ops.rs:103-146` |
| F07 | Four Duration formatters | `I:core/time.orna` (4 natives); `stdlib/std/time/duration/*.orna` |
| F08 | Rounding modes, `.divide`, bare words | `E:lib.rs:7698-7742`, `:9611`, `:10165-10180`; `C:calls.rs:2701-2702` |
| F09 | Option lifting, `Some`, `null` | `C:calls.rs:2525`, `:2532`; `E:lib.rs:3608`, `:3677`, `:9704-9708` |
| F10 | `Range`, `..`, `..=`, `in` | `C:inference.rs:723-747`; `E:lib.rs:3647-3651`, `:7480-7517`; `H:ty.rs:111` |
| F11 | 25 relation stage names | `H:lower/classify.rs:62-91`; `H:call.rs:137-191`; `E:relation.rs:645-671` |
| F12 | 9 table operation names, six copies | `H:lower/classify.rs:46-59`; `C:calls.rs:1939`; `C:item_checks.rs:1269` |
| F13 | Interpolation formatting | `C:lib.rs:3718-3731`; `E:lib.rs:6691-6717`; `H:typeck/mod.rs:715-732` |
| F14 | Display, Inspect, Present, `std.ui` | `stdlib/std/ui.orna:5-105`; `E:ui.rs` (482 lines); `E:lib.rs:9651` |
| F15 | Units and dimensions | `C:units.rs` (1,503 lines); `C:types.rs:38-51`; `C:inference.rs:1117-1440` |
| F16 | Equality, order, hash | `E:lib.rs:2879`, `:12033-12274`; `H:ops.rs:77-92`; `C:calls.rs:1613-1640` |
| F17 | Refined types | `C:scope.rs:71-74`; `C:lib.rs:2623-2650`, `:5224-5230`; `C:assertions.rs:70-90` |
| F18 | Enum identity and construction | `E:source_enums.rs:1-102`; `E:lib.rs:3672-3676` |
| F19 | Aggregates and statistics | `I:core/stats.orna` (14); `I:core/collection.orna` (27); `intrinsics` `stats.rs` |
| F20 | Calendar and time zones | `E:timezone.rs` (399 lines); `I:core/time.orna`; `stdlib/std/time/calendar.orna` |
| F21 | Text, bits, hash, codec natives | `I:core/text.orna` (10), `bits.orna` (6), `hash.orna` (5), `encoding/*` (9) |
| F22 | `fail`, `parallel`, `race`, `timeout`, `merge_streams` | `H:lower/classify.rs:30-43`; `H:call.rs:202-222` |
| F23 | Root natives `Some`, `now`, `uuid7`, `error` | `I:core.orna`; `E:lib.rs:9700-9710` |
| F24 | Type-directed codecs `decode<T>` | `E:lib.rs:11659-11662`; `I:core/encoding/json.orna` |
| F25 | Stored-reference `.key`, `std.ui.action` routes | `E:lib.rs:5858-5861`, `:9651-9654` |
| F26 | Diagnostic wording for F03 to F15 | strings in the functions above, e.g. `C:inference.rs:1006-1010` |

Five more facts about the special cases:

* The checkpoint code has one boundary kind per applied type argument and two more that exist only for Money
  (`MoneyCurrency`, `MoneyUnit`, `C:lib.rs:4363-4366`). A library `Money<C>` is an ordinary applied type, so those two
  kinds would go.
* The evaluator has a separate `Value` variant for each of Money, Instant, Duration, Period and Range
  (`E:lib.rs:3613-3651`). It has two absent values side by side, `Null` and `Option` (`:3608`, `:3677`). D3 and H4
  remove the second.
* The HIR already knows its own table is temporary. #9850 adds `builtin_postfix` and says "Declared unit families
  replace it when stage D8 resolves postfixes against them".
* The nine table-operation names exist in six copies: `H:lower/classify.rs:46`, `C:calls.rs:1939`,
  `C:inference.rs:573`, `C:item_checks.rs:1269`, `orna-application/src/lib.rs:2800` and
  `orna-application/src/repl_transaction.rs:637`.
* `std` already shows both sides. `stdlib/std/option.orna` (115 lines) is a complete library over `T?`. The four
  `time/duration/*.orna` files are 12 to 14 lines of forwarding to natives.

### 3.2 What each one needs

Verdict codes: `L0` = library today, with primitives that exist. `L+` = library with the primitives of section 4.
`K` = stays kernel. `K+L` = a kernel core with a library rest.

| Id | Verdict | Needs | What stays in Rust |
|---|---|---|---|
| F01 | L+ | P2; P3a for enum families; P8 folding | the suffix lookup rule |
| F02 | L0 | none: declare it in `core` | nothing |
| F03 | L+ | P2 (type target), P3d, P9 | nothing; the bound `C impl Currency` is ordinary (U06) |
| F04 | L+ | P1, P7 | nothing |
| F05 | L+ | one kernel `Decimal` rounding primitive; P3a, P3d | the rounding primitive |
| F06 | L+ | P1, P9 (`native type Instant`) | representation and bytes (tags 60002, 60005) |
| F07 | L+ | P5; a nanosecond accessor | nothing |
| F08 | L+ | P3b; the rounding primitive of F05 | the rounding primitive |
| F09 | K | none | representation and lifting rule (D3) |
| F10 | L+ | P10, P1 (`Contains`), P9 (tag 60019) | the `..` token |
| F11 | K+L | P1 for the library half | 13 kernel variants; the operator tree |
| F12 | K | the name list becomes one `core` declaration (H5) | the transactional write path |
| F13 | L+ | P5 | the `{ }` token; scalar instances |
| F14 | K+L | P5 for `Display`; `std.ui` needs none | action handles; the Inspect fallback |
| F15 | K+L | section 5 | unit-key algebra now (A); the const evaluator later (B; D14) |
| F16 | K | none | canonical order, equality, hash |
| F17 | K+L | P3c for base access | assertion evaluation at construction |
| F18 | K | none | identity derivation; `case` exhaustiveness |
| F19 | L+ | P1 (`Add`, `Zero`, `Order`) | exact-decimal primitives |
| F20 | K+L | none | zone database and resolution |
| F21 | K | none; four text rows are `SRC?` (native-declarations.md Q3) | Unicode, bits, SHA-256, codecs |
| F22 | K | none | the non-call evaluation rules |
| F23 | K | none | all of it |
| F24 | K | none | type-directed natives (D5 type arguments) |
| F25 | K | none | all of it |
| F26 | L+ | P7 | codes, spans, printing |

Counts, produced by counting the verdict column: `K` 9 (F09, F12, F16, F18, F21 to F25), `K+L` 5 (F11, F14, F15, F17,
F20), `L0` 1 (F02), `L+` 11 (F01, F03 to F08, F10, F13, F19, F26). Total 26.

### 3.3 The split inside F11 and F19

The relation operators are the largest "split" row, so the cut is stated here. It rests on one distinction. A
*terminal* consumes a relation and returns a value. A library function can do that today with `for`, which pulls rows
(ch. 3). A *stage* returns a relation. A library function has no way to build a lazy relation, so a stage stays a
kernel stage unless a primitive for that exists.

* **Stay kernel (13 of the 26 variants):** scan, `filter`, `map`, `sort_by`, `bucket_by`, `take`, `drop`, `flat_map`,
  `union`, and the terminals that gain from early exit or ordered sources: `count`, `first`, `last`, `one`. They shape
  the operator tree that `sys.explain` prints (hir.md sections 3.5 and 5.4).
* **Move to library now (6 variants and 2 natives):** the terminals `sum`, `min`, `max`, `every`, `exists`, and the
  alias `project`. `every` is `first(rows | filter(x => !test(x))) == null`, which still exits early. Two natives
  outside `RelOp`, `rank` and `group_by`, return lists and also move. `sum`, `min` and `max` need `Add`, `Zero` and
  `Order` bounds (P1).
* **Open (7 variants):** `distinct`, `unique`, `pairs`, `window`, `zip`, `zip_exact`, `asof_join` are stages. They move
  only if the kernel gains a way to build a lazy relation from a pull function (call it `unfold`). That is a possible
  eleventh primitive. It is not proposed here: the plan must accept a user pull function, and the effect and
  explain rules for it need their own design.
* **Existing evidence for "expressible":** the catalogue already marks 10 collection rows and 6 `std.math` rows
  `SourceExpressible` (`orna-intrinsics/src/modules/collection.rs`, `math.rs`). native-declarations.md Q3 names 14 rows
  (`SRC?`): four `std.text`, eight `std.stats`, two `std.money`.
* **Cost of the cut:** `sys.explain` prints a fold where it printed `sum`. Option: a library function may name its plan
  in a doc comment that `explain` reads. This is a tooling decision, not a language one.

### 3.4 What the table does not say

* It does not say the library version is faster. Section 4, P8 says what the executor must do so it is not slower.
* It does not say the Rust is deleted at once. Under the strangler plan (hir.md section 6) each row moves behind the
  differential harness, one family per PR.
* Row F15 is not decided here. Section 5 lays out the options.

--------------------------------------------------------------------------

## 4. The primitives to add

Conclusion: ten primitives close the gaps of section 2. Each is smaller than the special cases it replaces. P1 to P5
and P10 change the checker or the grammar. P6 to P9 are rules for diagnostics, the executor and the formats. Every
primitive below states its options neutrally. Where the owner's delegate has decided, it says "Decided (Dn)". Where
nobody has, it gives a default only as "What I do until told otherwise".

| Id | Primitive | Closes | Touches | Decision or default |
|---|---|---|---|---|
| P1 | Operator protocols | G1, G7 | checker, lowering | decided D16: output as a parameter; implicit impl variables |
| P2 | Literal suffixes | G2 | checker, lowering | follows D15: value suffix plus type target |
| P3 | Member-surface gaps, parts a to e | G3 to G6 | grammar, checker | a and e needed by D15; b to d default yes |
| P4 | Unit algebra or compile-time evaluation | G9 | checker; exec for B | decided D14: A now; B after H4 and H5 |
| P5 | `Display` and interpolation | G8 | checker | default: protocol call with the spec signature |
| P6 | Derive and delegation | none | none | default: none in the first cut |
| P7 | Library-worded diagnostics | G11 | checker diagnostics | default: message templates as statics |
| P8 | Executor contract | none | executor | decided D18: R1 to R4; one step per core call |
| P9 | `native type` and the wire registry | G10 | `core` grammar, engine | decided D17: P9-A plus `native type` |
| P10 | Lang items | none | engine | default: path table in Rust |

--------------------------------------------------------------------------

### 4.1 P1: operator protocols

Conclusion: `+ - * / % ^`, unary `-`, `<`, `<=`, `>`, `>=` and `in` call protocol members found through the type of
the left operand. Kernel scalar pairs keep their direct path. This is type-directed selection of an instance of one
protocol member. It is not overloading by D5's wording, and D16 records that reading. A stricter reading of D5 would
call it overloading; option C is the fallback if D16 is reopened.

**Step zero.** Protocol member calls must resolve first. Chapter 3 rule 9 already says `value.member(args)` is allowed
for a member that "the receiver's type declares or its protocol supplies". `[probe]` shows that a call on a value and a
call through a bound `T impl P` both fail today, and the HIR typeck reports "a protocol member that H2a does not
resolve" (`H:typeck/expr.rs:167`). Primitives P1, P5 and P10 all need this.

**The protocols** (names in `core.ops`; shown for option A below):

```text
Add<Rhs, Out>   fn add(self, right: Rhs): Out        a + b       Sub, Mul, Div, Rem, Pow likewise
Neg<Out>        fn neg(self): Out                    -a
Order           fn compare(self, other: Self): Ordering   a < b, a <= b, a > b, a >= b
Contains<T>     fn contains(self, value: T): Bool    x in r
```

`std.numeric` already declares homogeneous `Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg`, `Compare` and four more protocols
(`stdlib/std/numeric.orna:108-153`). Nothing implements them (Appendix A). A name denotes one declaration (D5), so
`core.ops.Add<Rhs, Out>` replaces `std.numeric.Add`, and `std.numeric` re-exports it with `pub use` (D11).

`==` and `!=` stay canonical equality for every type. Equality must agree with canonical bytes and with `Map` and `Set`
keys (D6). `Order` is derived by the engine for every type that has the canonical total order (all types except those
that contain a `Float`). A user instance is accepted only where no canonical order exists. That removes the case of two
different orders for one type.

**Reconciling with D5, explicitly.**

1. D5 says: "No overloading, for users or for the platform. Arity variants become default parameters; type-directed
   variants become distinct functions." Its stated reason is that "users cannot write what the library uses, and a
   reader must memorise which names are which" (DECISIONS.md, D5, problem statement).
2. With P1 a name never denotes several functions. `a + b` names no function. It selects the one instance of `Add.add`
   nested in the type of `a` whose protocol arguments match the type of `b`. Two instances that could both apply are an
   error, whatever their order (ORNA-GENERIC-014). `From` already follows this rule (ORNA-CONVERT-003).
3. The reference already allows several instances of one generic protocol in one type: `From<Str>` and
   `From<EmailHeader>` (ORNA-GENERIC-013). `impl Mul<Int, Duration>` and `impl Mul<Decimal, Duration>` in `Duration`
   have the same shape.
4. The test behind D5 holds: a user writes `impl Mul<Int, Duration>` exactly as `core` does.
5. What a strict reading may reject: the selection is implicit at the call site and depends on an argument type.
   `Target.from(source)` names its target. `a * b` does not. Mitigations: hover and go-to-definition show the instance
   (section 9.4), and the effect chain names it (section 6.1).

**Options.**

* **P1-A, output as a protocol parameter.** Selection uses `(Self, Rhs)`. `Out` is read from the one matching
  instance. Buys: no new type-system concept; the grammar already allows protocol parameters
  (`protocol_declaration ... [generic_parameters]`). Costs: one new rule (two instances that differ only in `Out`
  overlap); long impl headers (`impl Add<Money<C>, Money<C>>`); `Out` acts as an associated type in disguise.
* **P1-B, associated `Out` type.** `protocol Add<Rhs> { type Out; ... }`. Buys: shorter headers; the usual shape
  (Rust's `Add` has `type Output`). Costs: reopens ORNA-GENERIC-004 and the sentence "no associated type" in chapter 2;
  needs projection types in the checker and a way to state `Out = T` in a bound.
* **P1-C, homogeneous protocols only.** `Self op Self -> Self`, as `std.numeric` declares today, plus named functions.
  Buys: no change to the type system. Costs: `Instant - Instant`, `Instant + Duration`, `Money * Decimal` and every unit
  product cannot be operators. They stay special cases or become `start.until(end)`. `5.kilometre / 2.hour` is lost.

**Impl variables** (needed only for heterogeneous generic operands such as `Quantity<N, V>`):

* (i) implicit: free capitalised names in an impl's protocol arguments are bound by the impl;
* (ii) explicit: `impl<E> Mul<...>`, new syntax;
* (iii) none: only the owner type's own parameters.

`[probe]` `impl Mul<Q<E>, Q<D>>` with a free `E` gives `generic protocol instantiation cannot be validated`. Money,
Duration and Range need no impl variables. Quantity products need (i) or (ii).

**Decided (D16):** P1-A with implicit impl variables (i). A free capitalised name in an impl's protocol arguments is
bound by the impl. P4-A needs none; the unit algebra under P4-B is the first user. Selecting an instance by the types is
within D5. Two applicable instances are an error. `==` and `!=` stay canonical equality, `Order` is derived by the
engine where a canonical order exists, and scalar pairs keep the direct path. Step zero is scheduled in H2. A private
function whose numeric operand type stays unresolved is a diagnostic that asks for an annotation (D1 unchanged). There
is no defaulting.

**Cost** `[INFERENCE]`: instance selection is the CONVERT-1 algorithm (`C:calls.rs:2495-2583` shows today's matcher)
applied to protocol instances. A few hundred lines in `orna-hir` typeck, one lowering rule per operator, and the
arithmetic table in `H:ops.rs:98-146` shrinks to the scalar fast path.

**Prior art:** Rust `std::ops` ("Only operators backed by traits can be overloaded";
https://doc.rust-lang.org/std/ops/index.html). Kotlin turns `a + b` into `a.plus(b)`, `a..b` into `a.rangeTo(b)` and
`a in b` into `b.contains(a)` (https://kotlinlang.org/docs/operator-overloading.html).

--------------------------------------------------------------------------

### 4.2 P2: literal suffixes

Conclusion: one resolution rule replaces the hard-coded suffix lists. `n.name`, where `n` is a numeric literal, resolves
`name` statically and without ambiguity to a suffix declared in scope. The kernel keeps the rule. The names live in
`core`.

**Today.** Chapter 1 rule 6 names three families: `decimal`, a currency type and the built-in duration names. The
checker holds two string lists (`C:lib.rs:5320-5322`, `:5344`). The evaluator holds a `match` on the last path segment
(`E:lib.rs:5885-5972`). Any other name is `ORNA-EVAL-TYPE` at run time even where the checker accepted it
(`E:lib.rs:5916`). The HIR will hold a third table: #9850 adds `builtin_postfix` and notes that declared unit families
replace it.

**The rule.** Look up `name` in the import context, in this order. Two hits in one step are an ambiguity error.

1. A payload-free enum variant whose enum implements `Suffix<N, R>`, where `N` is the literal's numeric type. The
   result is `variant.apply(n)`. A variant of an enum that implements `Unit` (section 5.5) is also a suffix. Its result
   is a `Quantity` of that unit's key, and `5.kilometre` is `of<Length.kilometre>(5)`.
2. A type `T` with a registered literal target (P10). `T impl Currency` gives `Money<T>.from(n)`. This is the
   explicit-target syntax that ORNA-CONVERT-006 already allows ("such as `12.34.GBP`"). Any other `T impl From<N>` gives
   `T.from(n)`, so `50.Percent` works.
3. Otherwise the error is `unit suffix cannot be resolved`, as today.

**Options.**

* **P2-A, value suffix and type target** (the rule above). Buys: no boilerplate per unit; the enum is the list of
  suffixes. Costs: needs P3a (impl in enums); two lookup cases; plural spellings need re-exports
  (`pub use TimeUnit.hour as hours;`).
* **P2-B, a `suffix fn` declaration.** `pub suffix fn kilometre(n: Decimal): Length = ...;`. `suffix` is a contextual
  word recognised by two-token lookahead, as `native` is (native-declarations.md, ORNA-NATIVE-002). Buys: explicit,
  greppable, needs no enum. Costs: one declaration per spelling and per currency; `1.hour` and `1.hours` are two lines.
* **P2-C, keep the fixed list.** The kernel holds about ten names. Buys: nothing to build. Costs: library authors cannot
  add a suffix; `3.GiB` and every user unit need the unit-key kernel to resolve them (section 5).

`decimal` names a numeral form, not a unit. It stays kernel under A and C. Under B it is one line in `core`.

**Open point inside P2.** `day` and `days` are a calendar `Period` in the checker (`C:lib.rs:5344`, `:5389`) and in the
evaluator (`E:lib.rs:5891`). Chapter 1 and chapter 2 of the reference say `1.day` is a `Duration`. units.md (PR #9854,
Q1) follows the checker. Under P2 the answer is one library line, so the owner can decide without touching the kernel.

**Follows from D15:** P2-A. Unit variants are suffixes (rule 1), and P3a is accepted, so P2-B is not needed. Section
5.5.3 says which families are in scope and how an ambiguity is resolved.

**Cost** `[INFERENCE]`: about the size of today's `infer_unit_suffix` plus `infer_currency_suffix` (80 lines in
`C:lib.rs:5278-5399`), and it deletes the evaluator functions `numeric_postfix` and `numeric_decimal_postfix` (88 lines,
`E:lib.rs:5885-5972`).

**Prior art:** C++ user-defined literals "produce objects of user-defined type by defining a user-defined suffix"
(https://en.cppreference.com/w/cpp/language/user_literal).

--------------------------------------------------------------------------

### 4.3 P3: the member-surface gaps

Conclusion: five small changes let enums, refined types and generic types carry the members library code needs. Parts
a to d are needed by Money, Duration, rounding modes and suffixes. Part e is needed because unit families are enums
(D15), and part a is the first parser change.

**P3a. `impl` blocks inside enums.**

```text
enum_declaration = visibility , "enum" , identifier , [ generic_parameters ] ,
                   "{" , [ enum_variants ] , { nested_impl_declaration } , "}" ;
```

A variant may be named `impl` (`contextual_name` admits keywords). The parser reads `impl` followed by an identifier
as an impl block, and `impl` followed by `,`, `}` or `{` as a variant. `[probe]` today: `ORNA-PARSE-003`.

* Option i, as above. Buys: enums behave like types. Costs: one grammar production, one lookahead rule, formatter and
  highlighter updates.
* Option ii, no impls in enums; use a nominal type with `static` constants. Buys: no grammar change. Costs: no
  exhaustive `case`, no variant list, and nothing like `Length.kilometre` as a pattern.

**P3b. A variant by expected type.** Today `rounding: half_even` works because the checker types the bare word
`half_even` as `std.Rounding` (`C:calls.rs:2701-2702`) and the evaluator accepts a fixed word list when the module is
`std.stats` and the argument is named `rounding`, `interpolation` or `method` (`E:lib.rs:10165-10180`; DECISIONS.md
D5 describes it). A `Rounding` enum needs one rule instead.

* Option i, expected-type resolution: when the expected type is a payload-free enum `E` and a bare name is unbound,
  it means `E.name`. Buys: the example in the reference works for every enum, with no argument-name test. Costs: a typo
  needs a good message that lists the variants; a local with the same name wins silently.
* Option ii, leading dot: `.half_even`. Buys: explicit. Costs: new syntax; the reference examples change.
* Option iii, import the variants by name (`use core.numeric.Rounding.{half_even}`; chapter 3 patterns rule 2 already
  allows importing a variant by name). Buys: no new rule. Costs: common words enter every module that imports them.

**P3c. The base of a refined type.** A refined type is "represented by" its base, but the checker offers only
`Port.from(value)` (`C:lib.rs:5224-5230`) `[probe]`. A library type such as `Duration = Int { ... }` cannot read its own
integer. The 1.1.0 text itself assumes it can: `std.time.duration.compact(self, context)` inside
`type FriendlyDuration = Duration { impl Display ... }` (`reference/Orna-1.1.0/source/13-presentation.md:74-79`).

* Option i, an implicit private field `base`, readable in the type's own nested impls (as private fields already are,
  ORNA-NOMINAL-006). Buys: matches existing privacy. Costs: `base` is a keyword; it is allowed after `.` already.
* Option ii, built-in conversions out of a refined type (`Int.from(p)`). Buys: no new member. Costs: `Int` owns no
  impls, so this is a checker rule.
* Option iii, a public `base` for `pub` refined types. Buys: simple. Costs: a public type exposes its representation.

**P3d. A static through a type parameter.** `C.code` with `C impl Currency` does not resolve `[probe]`. `Money<C>` has
no value of type `C`, so a value witness cannot work. D5 forbids passing a type as a value.

* Option i, `T.name` is allowed when the bound of `T` declares `static name`; it resolves at instantiation (P8).
* Option ii, only instance functions on bounds. Buys: nothing new. Costs: phantom-parameter types (`Money<C>`,
  `Quantity<N, U>`) cannot read their parameter at all.

**P3e. A payload-free variant as a type argument.** Needed by enum-shaped unit families (D15), where
`Quantity<Decimal, Length.kilometre>` names a unit. Option i: yes, a variant is a singleton type
(other languages do this, for example Scala 3 enum cases and TypeScript enum members `[INFERENCE: no page fetched]`).
Option ii: no; the unit is a name in the type namespace, as PR #9854 declares it. Option iii: the unit slot of
`Quantity`, and of the numeric shorthand `Decimal<U>`, accepts a variant of a unit family, and no other type position
does. Buys: no general singleton types, so no subtyping question. Costs: one resolver rule that is specific to units.

**Decided (D15):** part a, as the first parser change. Part e is needed.

**What I do until told otherwise:** b (option i), c (option i) and d (option i). For e, option iii.

**Cost** `[INFERENCE]`: a is a grammar and lowering change of about 100 lines. b is a small resolver rule. c and d are
member-lookup rules in typeck.

--------------------------------------------------------------------------

### 4.4 P4: the algebra of units, three ways

Conclusion: dimensional analysis needs a type whose parameter is computed from the types of the operands. There are
three ways to give the checker that: a unit-key algebra written in Rust (PR #9854), compile-time evaluation of a small
pure fragment of Orna, or computation at the type level through protocols. Section 5 compares them for units. This
section defines them.

* **P4-A, unit-key algebra in the kernel.** The checker owns `Quantity<N, U>`, where `U` is a key (exponents over base
  axes and an exact scale; PR #9854 adds an offset and an affine flag, which C6 moves to the library). It implements the
  typing rules for `+ - * / ^` over keys. Units are erased at run time. PR #9854 specifies this in rules `ORNA-UNIT-001`
  to `-019` and estimates 900 to 1,200 lines in one HIR module.
* **P4-B, compile-time evaluation.** A pure `core` function (an empty inferred effect set) takes and returns canonical
  value types, and runs at check time under a step quota. The earlier draft marked such a function `const fn`; D14 drops
  the marker. A type may take a value parameter. A type argument may be a call to a pure `core` function. The checker
  evaluates it when the arguments are concrete. Inside a generic body the call is an opaque term, equal only to itself.
  The unit algebra then becomes Orna functions over a `Key` record (listing 2).
* **P4-C, type-level computation.** The key is encoded in nominal types (for example unary naturals for exponents), and
  products are computed by protocol instance resolution. It needs associated types or output parameters (P1) and
  recursive instance search.
* **P4-D, literal statics read by the checker** (a small helper, not a full option). The checker reads `static` data
  whose initialiser is a literal, a variant, or a record of those, without evaluating calls. It is enough to read a
  unit's scale from an enum, and not enough to compute a product.

**The fragment for P4-B**, stated so its size can be judged:

* purity: the function and everything it calls have an empty inferred effect set; no table, clock, randomness or I/O;
* types: arguments and results are `Int`, `Decimal`, `Bool`, `Str`, tuples, records, enums, lists and `Map` of those;
* limits: the existing call depth of 64 and a step quota (a default of 10,000 is `[INFERENCE]`); exceeding either is a
  check-time error that names the call;
* determinism: evaluation reads only the resolved source and the dependency snapshot (as inference does,
  ORNA-INFER-006); the same inputs give the same result on every machine;
* layering: `orna-hir` defines a `ConstEval` trait; `orna-exec` implements it; `orna-check`, which sits above both,
  wires them. This avoids a second evaluator (hir.md risk K3 is the same worry).

**Costs** `[INFERENCE]` unless a number comes from PR #9854:

| Option | Rust to write | Orna a reader sees | Main risk |
|---|---|---|---|
| A | 900 to 1,200 lines (PR #9854) | declarations only; algebra is Rust | fixed algebra; later change needs Rust |
| B | 2,000 to 4,000 lines and a layering hook | declarations and algebra | equality of computed terms; compile time |
| C | recursive instance search | encodings, not units | unreadable errors; slow checks |
| D | about 200 lines | scale data only | does not compute products |

**Decided (D14):** A now, B after H4 and H5. A is built for H2c-1 with its interface frozen as the four items of section
5.6, so library source never changes. B is the destination: the check-time evaluator is the HIR executor running pure
`core` bodies (an empty inferred effect set). It then takes over interface items 2 and 3. D is the reader for any
declared data in the meantime. The `const fn` marker above is dropped: purity is inferred (D2) and is the criterion.
Whether a type takes a value parameter with the word `const` is part of the P4-B design, made when P4-B starts.

**Prior art:** Rust constant evaluation computes "the result of expressions during compilation. Only a subset of all
expressions can be evaluated at compile-time" (https://doc.rust-lang.org/reference/const_eval.html).

--------------------------------------------------------------------------

### 4.5 P5: `Display` and interpolation

Conclusion: `{x}` inside a string calls `Display.display` on the static type of `x`. The kernel supplies instances for
its scalar types. Everything else is library. This closes three disagreeing behaviours.

**Today.**

* Chapter 1 says `{ expression }` "interpolates the expression's display text", and chapter 0 uses `"{sensor}: {value}"`
  with a `Decimal` field.
* The checker accepts `Str` only (`C:lib.rs:3718-3731`). The evaluator accepts `Str` only (`E:lib.rs:6706`). `[probe]`
  `"{n}"` with `n: Int` and `"{t}"` with a `Display` impl both fail with `ORNA-S021-TYPE`.
* The HIR accepts a fixed list: Bool, Int, Decimal, Float, Str, Uuid, Date, Instant, Duration, Period, Digest
  (`H:typeck/mod.rs:715-732`) and defers the rest ("needs a protocol member that H2a does not resolve",
  `H:typeck/expr.rs:167`).
* The 1.1.0 protocol is `fn display(self, context: DisplayContext): Str`
  (`reference/Orna-1.1.0/source/06-expressions.md:136-138`). A fixture uses `fn display(self): Str`
  (`orna-check/tests/fixtures/refine-evidence-yhre/ports.orna`).

**Options.**

* **P5-A, protocol call.** Interpolation lowers to `Display.display(value, context)`. The context is the activation's
  default `DisplayContext` (locale and zone from the snapshot, never from the host). Buys: one rule; library types
  format themselves. Costs: needs P1 step zero; the signature question below.
* **P5-B, keep `Str` only.** Authors call `x.display()` or a formatter. Buys: nothing to build. Costs: the reference
  text and the overview example must change; every message needs ceremony.
* **P5-C, scalars only.** The HIR list stays; other types are an error. Buys: small. Costs: a library `Money` cannot be
  interpolated, which defeats the feature.

**Signature question.** D5 allows one arity per protocol. The spec form takes a context. A form without it is simpler.
With a context, a formatter can honour locale and width. Without one, locale must come from elsewhere.

**Rules that hold in every option.** `Display` is read-only and deterministic relative to its immutable context
(ORNA-PRES-008). It never changes equality, hashing, storage or codec output (ORNA-PRES-004). The derived Inspect
fallback stays kernel (ORNA-PRES-002 and -006).

**What I do until told otherwise:** P5-A with the spec signature `display(self, context: DisplayContext)`.

**Cost** `[INFERENCE]`: replaces `displayable` (`H:typeck/mod.rs:715-732`) with instance lookup, plus one kernel
instance table for scalars; deletes the evaluator's `Str`-only check.

--------------------------------------------------------------------------

### 4.6 P6: derive and delegation

Conclusion: add no derive syntax in the first cut. The engine already derives what users would derive: equality, order,
hash, the Inspect fallback and codecs. Chapter 2 says so: equality, order, hash and display of a nominal type "come from
the engine's rules or from `impl` blocks". What remains is operator forwarding on newtypes, and it is short.

How short: a refined type over `Decimal` with `Add`, `Sub`, `Mul<Decimal>`, `Mul<Int>`, `Div<Decimal>` and `Neg` is six
impls of three lines each (listing 4). A type with one operator needs three lines.

**Options.**

* **P6-A, none.** Write the impls. Buys: nothing to build; the source says what happens. Costs: repetition for every
  newtype a user declares.
* **P6-B, a delegation clause.** For example `impl Add via base`: forward to the base type's operator and re-wrap with
  `from`. Buys: one line per operator. Costs: new syntax; the re-wrap runs the type's assertions again, so
  `Percent + Percent` can fail at 101. That is correct but surprising; the clause needs a rule for it.
* **P6-C, derive written in Orna.** A library function generates impls from a description of the type, using P4-B and a
  reflection module over fields. Buys: derive is itself dogfooded. Costs: needs the const evaluator and a reflection
  API; large.

**What I do until told otherwise:** P6-A.

**A related point.** P2-A avoids a generated declaration per unit: the enum is the suffix table. P2-B would need one
`suffix fn` per spelling. If P2-B is chosen, P6-C becomes more attractive.

--------------------------------------------------------------------------

### 4.7 P7: library-worded diagnostics

Conclusion: a library type words its own errors as data. The kernel keeps codes, spans and printing. Text from a library
never shows library internals.

**Today.** The messages users see for these cases are good and are written in Rust, one per feature `[probe]`:

| Input | Code and message today |
|---|---|
| `5.metre + 3.second` | `E5001: cannot add Length and Time` |
| `12.34.GBP + 1.USD` | `E5003: cannot add different currencies without conversion` |
| `a + b` on two nominal values | `E2001: operator + is not defined for Meters and Meters` |

Once operators are protocol calls, the kernel alone can say the third form. It would say it for Money as well, and that
is less specific than the second form. The library knows the right words.

**Rules in every option.**

1. The span is the user's expression, not a line inside `core`.
2. Types print in user terms: the shortest unambiguous path in the user's import context. The user reads `Money<GBP>`,
   never `core.money.Money<app.GBP>`.
3. Library frames are collapsed. A diagnostic raised while checking an instance reports the outermost user span. The
   instance chain shows under `orna explain CODE` or a verbose flag.
4. Codes are stable and kernel-owned. Library text fills the message and may add a note.
5. Library text is marked `source: library` in machine output so tools can style it.

**Options.**

* **P7-A, message templates as statics.** A core protocol `Explain` declares `static mismatch: Str`, for example
  `"cannot $op $left and $right without a conversion"`. The type of the left operand supplies it. The kernel fills
  `$op`, `$left` and `$right` with printed types. The holes use `$` because `{` would start an interpolation inside a
  string. Buys: data only; deterministic; no evaluator at check time. Costs: one template per type unless several
  statics are declared; no computed text.
* **P7-B, a check-time failure from a pure function.** A library function calls `compile_error("...")` when a pure
  predicate fails (needs P4-B). Buys: any message, computed from the types. Costs: depends on P4-B; unbounded text; the
  failure origin is a library frame (rule 3 must hide it).
* **P7-C, kernel text only.** No library words. Buys: nothing to build; uniform wording. Costs: `E5001` and `E5003`
  lose their specific text.
* **P7-D, failure at run time.** The impl calls `fail(error(...))`. Buys: always possible. Costs: the error moves from
  check time to run time.

**Compatibility.** `E5001` and `E5003` are published codes. A new "no instance" code must keep them for the same inputs,
or the change goes in the migration appendix.

**What I do until told otherwise:** P7-A, with P7-C as the text when a type declares no template.

**Cost** `[INFERENCE]`: one new code, one substitution routine, one note line; about 100 lines in the diagnostics layer.

**Prior art:** Rust's `diagnostic::on_unimplemented` is "a hint to the compiler to supplement the error message that
would normally be generated in scenarios where a trait is required but not implemented on a type"
(https://doc.rust-lang.org/reference/attributes/diagnostics.html).

--------------------------------------------------------------------------

### 4.8 P8: the executor contract for library code

Conclusion: library arithmetic must cost what the special case costs. Four executor rules give that without any new
language feature: static resolution, monomorphisation, inlining of small pure bodies, and constant folding. One
decision, step accounting, is observable by programs. It is decided (D18): option (ii) of R5.

Evidence for why it matters. The old evaluator picks arithmetic by matching the operator string and then the operand
tags on every evaluation (hir.md section 7.1). The HIR executor picks `AddInt` or `AddDecimal` once, when it builds the
operator tree (hir.md section 7.2). A library `Money` add must not drag that back to a slow path.

**Rules.**

* **R1, static resolution.** `a + b` lowers to `Callee::Source { def: <instance method> }` during typeck (hir.md 2.6).
  There is no run-time lookup.
* **R2, monomorphisation.** A generic body is instantiated once per distinct type-argument tuple when the operator tree
  is built, so a protocol call through a bound resolves to one body. The alternative is to pass instance tables as
  hidden arguments (dictionary passing). Buys of monomorphisation: speed and inlining. Cost: tree size and build time.
* **R3, inlining.** A call to a pure, non-recursive body of at most N nodes is inlined when the tree is built (N = 32 is
  `[INFERENCE]`). A refined type over a scalar has the scalar's run-time representation, so after inlining `Duration +
  Duration` is one integer add. `Duration.from(x)` with no assertions compiles to nothing. With assertions it compiles
  to the check.
* **R4, constant folding.** A call whose callee has an empty effect set and whose arguments are constants is evaluated
  once at build time. `3.hour`, `12.34.GBP` and `Percent.from(50)` become constants. A call that would fail is not
  folded. It fails at run time with the same error. When P4-B exists, the checker may also report it at check time.
* **R5, step accounting.** Step counts are observable: programs hit `ORNA-EVAL-LIMIT` at a count, tests assert counts,
  and hir.md 6.4 sets exact parity as the default. The default budget is 419,600 steps (hir.md 1.7). Options:
  * (i) charge every node honestly. A `Money` add costs several steps where it cost one. Quotas hit sooner;
    nothing is hidden.
  * (ii) charge one step for a call to a loop-free, pure `core` function. Parity with the special case; inlined work is
    not charged, so a long `core` body costs the same as a short one.
  * (iii) a written `weight` per `core` function, as `Expr.weight` does for lowering (hir.md 6.4). Explicit and exact.
    It adds an annotation that readers must trust.
  hir.md 6.4 already plans one re-baseline at H9. All three options can be adopted at that point.
* **R6, benchmarks.** Add two to the three of hir.md 7.4 before P1 lands: B4, a `Money` sum over 10^5 rows; B5, a
  `Duration` arithmetic loop of 10^6 steps. Gate the deterministic counters the same way: zero heap allocations per
  operator call in steady state, and a step count equal to the baseline under option (ii).

**Decided (D18):** R1 to R4 stand as written. For R5, option (ii): a call to a pure, loop-free `core` function costs one
step. "Loop-free" means the body has no `for`, `while` or `loop` and calls only loop-free `core` functions; lowering
computes it once per function. Every other call is charged per node, as user code is. Explicit weights (option iii) are
added only where a measured need appears. The one re-baseline is at H9.

**Cost.** All of it lands in the operator-tree builder of H4. None of it needs a language change.

--------------------------------------------------------------------------

### 4.9 P9: `native type` and the wire registry

Conclusion: library types keep today's bytes through an engine-side registry keyed by `DefId` and guarded at build time.
`core` declares kernel representation types with `native type`, so impls can be nested in them.

**Facts** (`reference/Orna-1.1.0/source/29-formats.md`):

| Tag or code | Value | Shape |
|---|---|---|
| 60002 | `Instant` | `[unix_seconds, nanosecond]` |
| 60005 | `Duration` | `[seconds, nanosecond]` |
| 60006 | `Quantity` | `[number, unit_object_id]`; the type descriptor holds dimension, scale, offset |
| 60007 | `Money` | `[Decimal, currency_object_id]` |
| 60019 | `Range` | `[lower, upper, upper_inclusive]` |
| 60008, 60009, 60020 | generic enum, record, refined | `[type_id, ...]`; refined: `[type_object_id, base_value]` |
| type codes 7, 8, 9 | Quantity, Money, Range | descriptors of the schema fingerprint |

Enum and variant identities are derived from the qualified name with domain separation (`E:source_enums.rs:23-60`). A
library type that moves or is renamed therefore changes identity and bytes. The 16 golden vectors under
`reference/Orna-1.1.0/tests/*.json` are the byte oracle (hir.md 6.2).

**Two problems.**

1. A library `Money<C>` is a refined nominal value (tag 60020, `[type id, decimal]`). Stored Money is tag 60007
   (`[decimal, currency id]`). The bytes and the schema fingerprint would change.
2. `Instant` is a kernel type, so it cannot own `impl Add<Duration, Instant>`. Implementations live in the type that
   owns them (ORNA-GENERIC-003).

**Options for problem 1.**

* **P9-A, a registry in Rust.** The engine maps `core.money.Money`, `core.time.Duration`, `core.range.Range` and the
  quantity type to their legacy adapters. The library has no syntax for it. A build script checks that each registered
  path exists and has the expected fields and parameters; a mismatch fails the build. Buys: bytes unchanged; no grammar.
  Costs: a hidden coupling; a `core` rename breaks bytes unless the guard runs; four or five adapters.
* **P9-B, a `wire` clause legal only in `core`.** For example `pub type Duration = Int wire 60005 { ... }`. PR #9854
  already proposes this shape for units as `repr NAME` on `dim` and `alias` declarations, "checked against the wire-form
  table and nothing else" (units.md section 5). Buys: the coupling is visible in source. Costs: new syntax; Money still
  needs a Rust adapter because its shape differs. A clause would have to serve every legacy-tagged type, not only
  quantities.
* **P9-C, no registry.** Library types use the generic encodings. Buys: simplest and most honest. Costs: a format change
  for Money, Duration, Range and Quantity: new descriptors, new fingerprints, new golden vectors, and a migration in
  `orna-evolution` for stored data.

**Options for problem 2.**

* **`native type` declarations in `core`.** `pub native type Instant { impl Add<Duration, Instant> { ... } }`. The
  engine owns the representation and the codec. The impls are ordinary Orna. PR #9854 already declares `Duration` "as a
  body-less native type, bound by DefId (D12)" (units.md section 8.4). Scalars get the same treatment with body-less
  members, so that go-to-definition on `+` for `Int` lands on `native fn add(self, right: Int): Int effect none;` and
  not in Rust. Buys: the whole operator table is readable. Costs: one new declaration form, legal only in `core` (D12
  pattern).
* **Relax ownership** so an impl may nest in any type named in the protocol arguments. Buys: `Duration` could host both
  sides. Costs: it needs a `for` form that 1.2 removed (ORNA091-E-IMPL-FOR) and breaks the one-owner rule.

**Decided (D17):** P9-A and `native type`. P9-C is rejected for now; it may be revisited only with a deliberate storage
format version. P9-B is not built, so no `repr` or `wire` clause exists (C2, C9). The `core` paths that the registry
names are part of the engine ABI: renaming one bumps the `NativeAbi` number, and the build guard checks it.

**Cost** `[INFERENCE]`: about 100 lines of build-script check and one adapter per registered type.

--------------------------------------------------------------------------

### 4.10 P10: lang items

Conclusion: each piece of sugar names its library target by a path in `core`. The engine checks at build time that every
target exists and has the expected shape. No attribute syntax is needed.

| Sugar | Target in `core` |
|---|---|
| `a + b`, `-a`, `a < b` | the `core.ops` protocols (P1) |
| `a..b`, `a..=b` | `core.range.Range` constructors |
| `x in r` | `core.ops.Contains<T>.contains` |
| `"{x}"` | `core.display.Display.display` (P5) |
| `n.name` | `Suffix`, `From`, literal targets (P2) |
| `12.34.GBP` | `Currency` pairs with `Money` |
| `for p in e` (optional) | `core.iter.Iterable<T>` |
| `Some`, `null`, `??`, `T?` | stay kernel (D3) |

**Mechanism.** The table lives in Rust, keyed by `core` path. The H5 build script validates it the way it validates
natives: "a declaration without an implementation does not compile" (hir.md section 4.3).

**Options.**

* **P10-A, a path table in Rust.** Buys: no syntax. Costs: the link is invisible in source; a rename breaks it (the
  guard catches that).
* **P10-B, a `lang` modifier in `core` source** (`pub lang protocol Add<...>`). Buys: visible to readers and to tools.
  Costs: new syntax, legal only in `core`.
* **P10-C, attributes.** Rejected: the grammar has none, and native-declarations.md section 3.1 rejected them for
  `native`.

**Iteration.** The pinned std already has a library pull iterator:
`pub type Iterator<T> { pub pull: fn(): (T?, Iterator<T>) }` (`stdlib/std/iterator.orna:8-10`). A protocol
`Iterable<T> { fn iter(self): Iterator<T>; }` needs no associated type because `T` is a parameter. `for` over a library
type would then be sugar for it. This is a candidate (section 8, feature 14), not part of the default.

**What I do until told otherwise:** P10-A.

**Cost** `[INFERENCE]`: a table of about 15 rows and its validator.

--------------------------------------------------------------------------

## 5. Dimensional algebra

Conclusion: the owner wants `5.kilometre + 200.metre`, `Speed = Length / Time` and `5.metre + 3.second` as a check-time
error, with units as enum-style families. The existing checker already does all three `[probe]`. The evaluator cannot
run them. PR #9854 re-specifies them with a unit-key kernel. Three decisions shaped the design: whether the unit lives
in the type or in the value, who computes the product of two units, and what the surface looks like. All three are
decided. The unit is in the type and erased at run time (the D8 amendment on PR #9854). The algebra is the unit-key
kernel now and evaluated Orna after H4 and H5 (D14). The surface is enum-shaped families (D15). Both algebras sit behind
one four-item interface (section 5.6), so the move from one to the other changes no source. Sections 5.3 to 5.5 keep
the options as the record. Section 5.5 spells out the surface.

### 5.1 What exists

* The checker has a quantity type with exponents, scale and offset (`C:types.rs:38-44`), 1,503 lines in `units.rs`, the
  operator tables (`C:inference.rs:1117-1440`) and the suffix rule (`C:lib.rs:5352-5399`).
* `[probe]`, with a module that declares `Length`, `Time`, `Speed = Length / Time`, `metre`, `kilometre`, `second`,
  `hour` and `kmh`:
  `5.kilometre + 200.metre` is accepted; `5.kilometre / 2.hour` is accepted; a function typed
  `(Decimal<kilometre>, Decimal<hour>): Decimal<kmh>` is accepted; `5.metre + 3.second` gives
  `E5001: cannot add Length and Time`; the same function typed to return `Decimal<metre>` is rejected.
* The evaluator has no quantity value (`E:lib.rs:3606-3683`). A suffix outside its fixed list is `ORNA-EVAL-TYPE` at run
  time (`E:lib.rs:5916`). DECISIONS.md D8 puts it plainly: "a headline specified feature type-checks and cannot run".
* The HIR has `DefKind::Dimension` and `DefKind::Unit` (`H:def.rs:51-56`, commented "if decision D8 keeps dimensions")
  and no type for a quantity (`H:ty.rs:65-126`). `H:ops.rs:19-22` says units are outside its table.
* The 1.1.0 text has `ORNA-UNIT-001` to `-008` (`reference/Orna-1.1.0/source/05-types.md:189-252`), tag 60006 and the
  descriptor `[7, numeric_type, unit_id]`.
* PR #9854 re-specifies the feature as `ORNA-UNIT-001` to `-019`, with the algebra in one HIR module of 900 to 1,200
  lines (its estimate).

### 5.2 What a unit system needs, and who provides it

| Need | A: unit-key in Rust | B: evaluated Orna | D: declared relations | E: run-time checks |
|---|---|---|---|---|
| N1 families with exact factors | Orna declarations | Orna declarations | Orna | Orna |
| N2 a quantity value | kernel type, erased | library type, const param | type per dimension | record with unit |
| N3 `+ - <` need equal dimension | kernel rule | library impl with a `where` | type mismatch | run-time check |
| N4 products and powers | kernel rule | pure fn in Orna | one impl per product | run-time vector add |
| N5 literals `5.kilometre` | P2 | P2 | P2 | P2 |
| N6 messages in user terms | P7-A | P7-A or P7-B | P7-A | `fail` text |
| N7 printing | library `Display` with a key query | library | library | library |
| N8 storage and bytes | type holds the key; value is a number | same | same | value holds the unit id |

Option C (type-level computation, section 4.4) is left out of the table. It shares B's needs and adds recursive instance
search. Its errors would show encodings, not units.

### 5.3 Decision 1: is the unit in the type or in the value?

* **World 1, in the type and erased at run time** (PR #9854; F# measures). A quantity is its number at run time. Buys:
  static errors; `Int`, `Decimal` and `Float` quantities stay (`Float<kWh>` is in the 1.1.0 text); zero run-time cost;
  exact conversion folded at check time. Costs: the checker computes keys, scales and exactness; a dynamic boundary
  (`sys.invoke`, JSON decode) needs a typed schema (PR #9854, `ORNA-UNIT-019`).
* **World 2, dimension in the type, unit in the value.** `Quantity<Length>` holds a number and a unit variant. Buys:
  enum-shaped families with no variants-as-types; the stored shape equals tag 60006 `[number, unit_object_id]`; only
  dimensions need algebra. Costs: conversions at run time over exact rationals; `60.kmh + 10.mph` may fail at run time,
  not check time; the numeric type collapses to one exact type, so `Float<kWh>` is lost; values are larger.
* **Canonical unit, the `Duration` model.** The value is stored in the dimension's canonical unit; units are
  constructors and views. Buys: simple and fast. Costs: derived units with non-decimal scales are inexact (`60.kmh` is
  `50/3` m/s); the unit a user wrote is lost on print unless `Display` rescales.

Prior art exists for each. F# measures are erased
(https://learn.microsoft.com/en-us/dotnet/fsharp/language-reference/units-of-measure, as cited in DECISIONS.md D8). The
Rust `uom` crate does "automatic type-safe zero-cost dimensional analysis" (https://docs.rs/uom/latest/uom). Julia's
Unitful keeps dimensional information as a parameter of the unit type: "`D` keeps track of dimensional information"
(https://engee.com/helpcenter/stable/en/julia/Unitful/types.html).

**Decided (the D8 amendment on PR #9854):** World 1. The unit is in the type and erased at run time. The typing rules of
units.md section 6 are World 1.

### 5.4 Decision 2: who computes the product of two units?

Within World 1 the options of section 4.4 apply. A and B are the main options. D and E are fallbacks if units must
ship before H2c is done.

* **A, the unit-key kernel.** Small, known, fast, with exact messages. The algebra is Rust. A reader sees declarations,
  not the algebra.
* **B, evaluated Orna.** The algebra is about 30 lines of Orna (listing 2). It needs value parameters on types,
  implicit impl variables (P1 (i)) and a `where` on an impl. That is a language feature with its own design, risk and
  compile-time cost. It also serves refined-literal checks, library diagnostics and `Money` currency checks.
* **D, declared relations (closed world).** The library declares `Quantity<Length>`, `Quantity<Time>` and every allowed
  product as an impl (`impl Div<Quantity<Time>, Quantity<Speed>>`). It needs P1 only and no kernel code. There is no
  `Area / Length = Length` unless someone writes it, the impls grow with the square of the dimensions, and scale must
  live in the value (World 2) or be fixed per family.
* **E, run-time checks.** The library stores a dimension vector in the value. It needs P1 only. The error for `5.metre +
  3.second` appears at run time, except for literals folded under P8. That does not meet "is an error" at check time.

**Decided (D14):** A first, then B. A is built for H2c-1. B starts after H4 and H5 and takes over interface items 2 and
3. D and E stay fallbacks and are not planned. B needs no `const fn` marker: the check-time evaluator is the HIR
executor and runs any pure `core` body.

### 5.5 Decision 3: the surface (decided: enum-shaped families, D15)

**Decided (D15):** S2. S1 and S3 stay below as the record of the options. S1 is not shipped.

**S1, keyword forms** (PR #9854, which extends the 1.1.0 grammar with `canonical` and `repr` clauses and an `alias`):

```orna
pub dim Length;
pub unit metre : Length base;
pub unit kilometre : Length = 1000.metre;
pub dim Speed = Length / Time;
```

Buys: short; the parser has it (`Declaration::Dimension` and `Declaration::Unit`, `orna-syntax/src/parser.rs:284-292`);
it reads like the spec. Costs: five words (`dim`, `unit`, `base`, `offset`, `affine`) stay meaningful; it is one more
declaration the library cannot imitate; the owner's enum idea is unused.

**S2, enum-shaped families** (decided). A family is an enum and a unit is a variant. A definition after `=` gives the
scale, and the definition is an ordinary expression that names a quantity: `kilometre = 1000.metre` says that one
kilometre is 1000 metres. Section 5.5.1 gives the exact form.

Buys: the family is a list that `case` and tools can walk; user code can match on units; the declaration form is one the
user already knows. Needs P3a and, where a unit appears in a type, P3e. Costs depend on the algebra: under P4-A the
checker accepts a small set of expression forms after `=` (rule 5 of 5.5.1); under P4-B it accepts any pure
expression. The source is the same under both.

**S3, a marker type with an impl.** `pub type Kilometre { impl Scale { static scale = ...; } }`. Buys: no new syntax at
all. Costs: no unit list and no `case`; one type per unit.

#### 5.5.1 The declarations

A unit family is an enum that implements the core protocol `Unit`. Built-in and user families use this one form.

```orna
// A base family. The one variant without a definition is the base unit. It starts a new axis.
pub enum Length {
    metre,
    kilometre = 1000.metre,
    mile = 1609.344.metre,

    impl Unit {}
}

// A derived family. The header gives the dimension. Each definition is checked against it.
pub enum Energy = Mass * Length^2 / Time^2 {
    joule = 1.kilogram * 1.metre^2 / 1.second^2,
    kilowatt_hour = 3_600_000.joule,

    impl Unit {}
}
```

The rules restate ORNA-UNIT-009 and -010 for enums.

1. A family is a payload-free enum that implements `Unit`. Under P4-A `Unit` has no members: the line is a marker. It
   links the enum to its explanation, because go-to-definition on `Unit` lands on the doc comment in `core`. `Currency`
   marks a currency type the same way.
2. A variant without `=` is a base variant. A family has at most one. If it has one, it is a base family: the variant
   starts a new axis, and every other variant must reduce to that axis alone (ORNA-UNIT-010).
3. A family with no base variant is a derived family. It needs a header, `= dimension_expression`, which names families
   and uses `*`, `/` and `^` with an integer literal. Each definition must have that dimension. If one does not, the
   error is ORNA-UNIT-DIM and names both printed dimensions. A base family has no header.
4. A definition `v = e` says that one `v` equals the value of `e`. The expression `e` has a quantity type. The key of
   `v` takes its exponents from that type, and its scale from the scale of that type times the value of `e`. So
   `kilometre = 1000.metre` has scale 1000, and `joule = 1.kilogram * 1.metre^2 / 1.second^2` has scale 1.
5. Under P4-A a definition may use only numeric literals (exact decimals), unit suffixes, `*`, `/`, `^` with an integer
   literal, and parentheses. Anything else is an error that names the allowed forms. Under P4-B any expression with an
   empty effect set is allowed. The text that P4-A accepts is also valid P4-B text with the same meaning, so the move
   changes no source (D14).
6. A definition cycle is ORNA-UNIT-CYCLE. A definition on a variant of an enum that does not implement `Unit` is an
   error, because the clause has no meaning there. A variant with a payload cannot be a unit.
7. A definition has no offset, so there are no affine variants. Temperature scales are library types (C6, listing 10).

#### 5.5.2 Where a unit appears

* **As a suffix.** `5.kilometre` follows the P2 rule: a variant of a unit family in scope. The result is a `Quantity`
  of that unit's key. The qualified form is `core.units.of<Length.kilometre>(5)`, a generic call in which the unit is a
  type argument (D5). Symbols are re-exports, not second variants: `pub use Length.kilometre as km;` (D11).
* **In a type.** `Decimal<Length.kilometre>` is `Quantity<Decimal, Length.kilometre>`. A product or quotient of units is
  a key product: `Decimal<Length.kilometre / Time.hour>`. The type grammar has `*` and `/` already. Only the unit slot
  of `Quantity` and of the numeric shorthand accepts a unit. This is P3e in its narrowest form (section 4.3, option
  iii).
* **As a value.** `Length.kilometre` is an ordinary enum value. It can be matched with `case`, stored and put in a list.
  It carries no number.

#### 5.5.3 Which names are in scope

The suffix `5.m` looks for `m` among the variants of the unit families in scope, and among the names that the module
imports and that denote such variants.

* A family is in scope when the module declares it or imports it. The families of `core` are in scope everywhere: they
  are the intrinsic environment, the outermost level.
* Two families at the same level that both define the suffix are an ambiguity error, ORNA-S011. There is no
  last-import-wins rule, so the result never depends on the order of imports. The qualified form resolves it.
* A name that the module declares or imports shadows the same name in the intrinsic environment, as every `core` name
  is shadowed (R11). A shadowed family is not in scope for that name. So a module that imports the physics `Time`
  family of listing 1 gets `3.hour` as a time quantity. A module that does not gets the `Duration` of listing 3.

#### 5.5.4 Derived dimensions: what was considered

`dim Speed = Length / Time;` of 1.1.0 becomes the header of the family `Speed`. A dimension with no unit cannot be
declared. Nothing could have its type.

* **H0, no header.** The dimension is inferred from the definitions. Buys: nothing extra to write, and the closest shape
  to a plain enum. Costs: a family with one variant and a wrong definition names the wrong dimension without any error,
  and ORNA-UNIT-010 has nothing to check against.
* **H1, a header** (the form above). Buys: the declaration reads like the owner's `Speed = Length / Time`; each
  definition is checked against a stated dimension; it reuses the `dimension_expression` production of the 1.1.0
  grammar. Costs: one optional production in the enum header, and a derived family states its dimension twice (header
  and definitions). The second statement is the check.
* **H2, a static in the impl.** `impl Unit { static dimension = ...; }`. Buys: no header syntax. Costs: a dimension is
  not a value, so the static would need a type for dimensions. That type does not exist.

**What I do until told otherwise:** H1.

#### 5.5.5 Grammar impact

Against the frozen 1.1.0 grammar (`reference/Orna-1.1.0/grammar/orna.ebnf`):

| Change | Production | Needed by |
|---|---|---|
| Add | `enum_declaration` takes nested impls after the variants | P3a, `impl Unit` |
| Add | `enum_variant`: an optional `"=" , expression` after the name | the definition clause |
| Add | `enum_declaration`: an optional `"=" , dimension_expression` after the name | derived families |
| Reuse | `dimension_expression` and `dimension_term`, now used by the header | the header |
| None | `type_product` has `*` and `/`; `Length.kilometre` is a `qualified_name` | unit types |
| Not shipped | `dimension_declaration` and `unit_declaration` | S1 |

D15 adds no keyword. `dim`, `unit`, `base`, `offset` and `affine` keep their present status in the lexical chapter. A
later release may free them. Beyond the grammar the change touches the resolver (the suffix scope of 5.5.3, and a
variant in the unit slot), the checker (the family rules of 5.5.1) and `core` (the `Unit` protocol, `Quantity` and
`of`).

Cost `[INFERENCE]`: the enum changes are the P3a estimate of section 4.3, about 100 lines of grammar and lowering, plus
a small addition for the clause and the header. PR #9854 no longer needs its declaration parser.

### 5.6 The interface the kernel keeps in every option

Four items. They are the same under A and B. Under B items 2 and 3 move into `core/units/algebra.orna`, and nothing a
user or library author writes changes.

1. **`Quantity<N, U>`**: a type constructor over `N` in `{Int, Decimal, Float}` and a key `U`. Erased at run time.
2. **Keys**: an exponent map over base axes and an exact rational scale. An axis is a base variant of a unit family or a
   type that implements `Currency`. Equal keys are one type. Key arithmetic for `*`, `/` and `^n` with an integer
   literal exponent. The family rules of section 5.5.1 are checked here.
3. **Exactness and ratio queries**: for two keys of one dimension, the finer key, and the exact ratio or "inexact". Used
   by mixed-unit `+`, `-` and comparison, and by `to`.
4. **Suffix resolution**: the P2 rule yields a key for a unit variant.

Everything else is library: unit data, `Duration`, `Money`, size units, affine types (listing 10), `to`, aggregates,
`Display` and diagnostic text.

### 5.7 Money and the currency axis

PR #9854 (`ORNA-UNIT-017`) makes `Money<C>` equal to `Quantity<Decimal, C>`, so a currency is a base axis. MONEY-003
then follows from the algebra: `Decimal<kWh> * (Money<GBP> / kWh)` reduces to `Money<GBP>`. The other shape keeps
`Money<C>` a library type (listing 4) and adds a library `Rate<C, U>`.

| | Currency as an axis | Money as a library type |
|---|---|---|
| MONEY-003 | falls out of the algebra | a `Rate<C, U>` impl, plus the ratio query for `kWh` against `J` |
| `Money<GBP> * Money<GBP>` | valid as `GBP^2` unless a rule forbids it | no such impl, so an error |
| Kernel size | one more rule (PR #9854 O4) | none |
| Tag 60007 | kept by `repr Money` (PR #9854) | kept by P9 |

This document does not decide it. Both keep tag 60007.

### 5.8 Staging (decided: D14 and D15)

1. **H2c-1.** The P4-A unit-key kernel behind the four-item interface. It needs none of P1 to P3.
2. **H2c-2, after P1, P2 and P3.** The unit families as enums (listing 1), then `Duration`, size units, affine types and
   `to` as library (listings 3 and 10). The surface is S2 (section 5.5). It needs P3a, which is the first parser
   change, then P3e and the `Unit` protocol. Keep the unit data in a `core.units` module.
3. **After H4 and H5.** Build P4-B and move items 2 and 3 of the interface into Orna. Library source and user source do
   not change.

D8 removed units from 1.x, so no program declares one yet, and the surface was free to change. It is fixed now. The
keyword forms `dim` and `unit` of PR #9854 are not shipped.

### 5.9 What a reader sees when opening `5.kilometre`

| Design | Go-to-definition on `kilometre` lands on | The algebra is |
|---|---|---|
| P4-A with S2 (now) | `kilometre = 1000.metre,` in an enum | in Rust |
| P4-B with S2 (after H4 and H5) | the same enum line, then `times` in `algebra.orna` | in Orna (listing 2) |
| S1 keyword forms (not shipped) | `pub unit kilometre : Length = 1000.metre;` | in Rust |

--------------------------------------------------------------------------

## 6. How library code flows through the engine

Conclusion: library code uses the same effect inference, the same type solver and the same executor as user code. Three
things need a stated rule: how an implicit call shows in an effect chain, how the solver defers a protocol obligation,
and what the browser profile must carry. Nothing here needs a new language feature.

### 6.1 Effects and typing

* **Effects.** An operator instance is an ordinary function with an inferred effect set. `a + b` lowers to
  `Callee::Source { def }` (hir.md 2.6), so the effect pass joins the instance's set (hir.md 2.5). A native member
  declares its effects in `core` (D12).
* **Restricted contexts.** `a + b` inside a computed field or an assertion is legal exactly when the instance has an
  empty effect set (chapter 4). A user instance that reads a table makes `+` illegal there. The diagnostic uses the
  existing rule, "the first forbidden kind found and the call chain", and the chain names the operator and the instance:
  the operator `+` on `Money<GBP>` calls `Add.add` at `core/money.orna:31`, which has `table_read`.
* **Core is pure.** A CI check asserts that every operator instance, formatter and constructor in `core` has an empty
  inferred effect set. A pure `core` is what lets operators run in assertions and watches.
* **Obligations.** D1 rule 4 says operators are "checked after their operands are solved". With P1 the solver records
  the obligation "`typeof(a)` has an instance of `Add<typeof(b), ?Out>`" and solves it after unification. An operand
  type that stays unresolved, with two candidate instances, is an error that asks for an annotation, for example on a
  private function `fn double(x) = x * 2`. There is no guessing and no defaulting to `Int` or `Decimal` (D1, D16).
* **Literals.** Expected types flow inward only for literals and `null` (D1 rule 5). `price * 2` selects `Mul<Int, _>`.
  `price * 2.5` selects `Mul<Decimal, _>`. `price * 2.5f` has no instance, so it is an error, as ORNA-NUM-003 requires.
* **Options.** The lift from `T` to `T?` is a kernel rule. It does not apply to operands: `Int?` plus `Int` is still an
  error.
* **Generics.** In `fn total<T impl Add<T, T> + Zero>(xs: [T]): T`, `a + b` resolves through the bound. At instantiation
  the instance body is selected (P8 R2).
* **Failure and cancellation.** An instance may call `fail`. The failure propagates like any call (D4), and the
  statement savepoint rules are unchanged. A library loop is a cancellation point like any loop.

### 6.2 Determinism and canonical bytes

* Library code is deterministic when it is pure. The const evaluator of P4-B has the same limit: it reads only the
  resolved source and the dependency snapshot.
* Existing bytes are kept by P9. New library types use the generic encodings (tags 60008, 60009, 60020). The 16 golden
  vectors are the oracle.
* `core` paths derive object identities (`E:source_enums.rs:23-60` shows the scheme for enums). Moving or renaming a
  `core` item changes bytes. So the `core` paths that the wire registry names are ABI (D17), and the rest follow
  section 9.5.
* A change to `Display` or to a formatter never changes equality, hashing, storage or codec output (ORNA-PRES-004).
* Library arithmetic on `Int` and `Decimal` calls the kernel scalar operators, so the quota failure (`ORNA-EVAL-LIMIT`)
  happens at the same value. Step counts follow D18 (P8 R5).

### 6.3 The browser and Wasm profile

* The library is Orna. It runs wherever the executor runs. Only natives touch the host, and each declares its effects.
  The browser build is a `Host` with `io() = None` (hir.md 4.4), so features built on pure `core` work unchanged. The
  wasm parity workflow runs from H4 (hir.md 10.3).
* Two costs belong to library-defined features. (1) Start-up: `core` is parsed, lowered and checked at start, once per
  page load in a browser. (2) Size: the embedded source. Neither is measured. Options for (1): ship a pre-lowered
  `Program` for `core` built at compile time and checked against the source digest; or lower `core` modules lazily on
  first use. Measure both in H0 and H4 `[INFERENCE]`.
* No library feature may read the host locale or clock. `DisplayContext` comes from the snapshot (P5). Time-zone data is
  embedded (`E:timezone.rs`).

--------------------------------------------------------------------------

## 7. The tour: opening the black box

Conclusion: ten listings show what a curious user would read. Each is under 40 lines. Each is an **illustration** except
listing 7, which is real. Each names the primitives it uses: `U` for what exists (section 2), `P` for what is proposed
(section 4). Section 7.11 reports how each fits the grammar and what the probed checker says.

### 7.1 Listing 1: a unit family and its derived dimension

Uses: enums with `impl` and a definition clause (P3a, D15), and the P4-A kernel behind them. The shape is that of
section 5.5. The user types `5.kilometre + 200.metre` and never sees this file until the first go-to-definition.

```orna
// std/units.orna (excerpt). ILLUSTRATION. Enum-shaped families (D15). The algebra is the kernel's (P4-A).
/// Length. `metre` has no definition: it is the base unit and starts the axis.
pub enum Length {
    metre,
    kilometre = 1000.metre,              // one kilometre is 1000 metres
    mile = 1609.344.metre,

    impl Unit {}
}

/// Physical time. Importing it shadows the core `second`, `minute` and `hour`, which return `Duration`.
pub enum Time {
    second,
    minute = 60.second,
    hour = 60.minute,

    impl Unit {}
}

/// A derived family states its dimension in the header. Each definition is checked against it.
pub enum Speed = Length / Time {
    kilometre_per_hour = 1.kilometre / 1.hour,
    mile_per_hour = 1.mile / 1.hour,

    impl Unit {}
}

// User code, same forms:
//   5.kilometre + 200.metre    a Length: 5200 metre (the finer unit)
//   5.kilometre / 2.hour       a Speed, with this family's Time in scope
//   5.metre + 3.second         error: cannot add Length and Time
```

### 7.2 Listing 2: the unit algebra written in Orna

Uses: U01, U06; P1 (impl variables), P4-B. This is option B: the part that PR #9854 keeps in Rust, written as Orna. It
does not fit today's grammar (section 7.11).

```orna
// core/units/quantity.orna. ILLUSTRATION. Needs value parameters on types (P4-B) and operators (P1). Pure by inference.
/// An element of the unit group: exponents of the base units and an exact scale.
pub type Key {
    pub axes: Map<Str, Int>,            // "metre" -> 1, "second" -> -1
    pub scale: Rational,                // exact: 1000, or 5/18
}

fn times(a: Key, b: Key): Key = Key {
    axes: merge(a.axes, b.axes, (x, y) => x + y),
    scale: a.scale * b.scale,
};

/// A number whose unit is part of its type. At run time it is just the number.
pub type Quantity<N impl Number, const U: Key> {
    pub value: N,

    impl Add<Quantity<N, U>, Quantity<N, U>> {
        fn add(self, other: Quantity<N, U>): Quantity<N, U> =
            Quantity { value: self.value + other.value };
    }

    // The type of a product is computed by `times`; the value is one multiplication.
    impl Mul<Quantity<N, V>, Quantity<N, times(U, V)>> {
        fn mul(self, other: Quantity<N, V>): Quantity<N, times(U, V)> =
            Quantity { value: self.value * other.value };
    }
}
```

`Number`, `Rational` and `merge` are library items that do not exist yet. `V` is an impl variable (P1 (i)). `times` has
no marker: the check-time evaluator runs any pure `core` body (D14). How a type takes a value parameter (`const U`) is
part of the P4-B design.

### 7.3 Listing 3: `Duration` with its built-in units

Uses: U01, U02, U04; P1, P2, P3a, P3c, P9. The enum is the suffix table: `3.hour` is `TimeUnit.hour.apply(3)`.

```orna
// core/time.orna. ILLUSTRATION. The wire form [seconds, nanosecond] is kept by the P9 registry.
/// Elapsed time in whole nanoseconds. A calendar day is a `Period`, not a `Duration`.
pub type Duration = Int {
    impl Add<Duration, Duration> {
        fn add(self, other: Duration): Duration = Duration.from(self.base + other.base);
    }
    impl Sub<Duration, Duration> {
        fn sub(self, other: Duration): Duration = Duration.from(self.base - other.base);
    }
    impl Mul<Int, Duration> {
        fn mul(self, count: Int): Duration = Duration.from(self.base * count);
    }
    impl Neg<Duration> {
        fn neg(self): Duration = Duration.from(-self.base);
    }
    impl Nanoseconds {
        fn nanoseconds(self): Int = self.base;
    }
}

/// The suffixes. The enum is the list; `case` makes it exhaustive.
pub enum TimeUnit {
    nanosecond,
    second,
    minute,
    hour,

    impl Suffix<Int, Duration> {
        fn apply(self, count: Int): Duration = Duration.from(count * case self {
            TimeUnit.nanosecond: 1,
            TimeUnit.second: 1_000_000_000,
            TimeUnit.minute: 60_000_000_000,
            TimeUnit.hour: 3_600_000_000_000,
        });
    }
}
pub use TimeUnit.hour as hours;      // plural spellings are re-exports (D11)
```

### 7.4 Listing 4: `Money<C>` with a currency protocol

Uses: U02, U05, U07, U08; P1, P2 (currency target), P3c, P3d, P5, P7, P9. The currency is part of the type. `12.34.GBP`
is `Money<GBP>.from(12.34)`.

```orna
// core/money.orna. ILLUSTRATION. The wire form [Decimal, currency id] (tag 60007) is kept by the P9 registry.
pub protocol Currency {              // today a Rust string in the checker (C:lib.rs:2909)
    static code: Str;
    static minor_digits: Int;
}

/// An exact amount in currency `C`. The currency is in the type, not in the value.
pub type Money<C impl Currency> = Decimal {
    impl Add<Money<C>, Money<C>> {
        fn add(self, other: Money<C>): Money<C> = Money.from(self.base + other.base);
    }
    impl Sub<Money<C>, Money<C>> {
        fn sub(self, other: Money<C>): Money<C> = Money.from(self.base - other.base);
    }
    impl Mul<Decimal, Money<C>> {
        fn mul(self, factor: Decimal): Money<C> = Money.from(self.base * factor);
    }
    impl Explain {                   // P7-A: the words for `GBP + USD`
        static mismatch = "cannot $op $left and $right without a data-backed conversion";
    }
    impl Display {
        fn display(self, context: DisplayContext): Str = "{C.code} {self.base}";
    }
}

// A currency is any type that implements `Currency`.
pub type GBP {
    impl Currency {
        static code = "GBP";
        static minor_digits = 2;
    }
}
```

### 7.5 Listing 5: a `Range` type behind `..`

Uses: U01, U04, U05, U06, U09; P1, P9, P10. It needs G13 (a generic type with `T` fields) fixed in the checker. `a..b`
desugars to the constructor; `x in r` calls `contains`.

```orna
// core/range.orna. ILLUSTRATION. `a..b` is Range { lower: a, upper: b, inclusive: false }. Tag 60019 kept by P9.
pub enum Ordering { less, equal, greater }

pub protocol Order {
    fn compare(self, other: Self): Ordering;
}

pub type Range<T impl Order> {
    pub lower: T?,
    pub upper: T?,
    pub inclusive: Bool,                 // true for a..=b

    impl Contains<T> {                   // `x in r` calls this
        fn contains(self, value: T): Bool =
            above(self.lower, value) && below(self.upper, self.inclusive, value);
    }
}

fn above<T impl Order>(lower: T?, value: T): Bool = case lower {
    Some(bound): value.compare(bound) != Ordering.less,
    null: true,
};

fn below<T impl Order>(upper: T?, inclusive: Bool, value: T): Bool = case upper {
    Some(bound): case value.compare(bound) {
        Ordering.less: true,
        Ordering.equal: inclusive,
        Ordering.greater: false,
    },
    null: true,
};
```

`Order` is derived by the engine for every type that has the canonical order (P1). The `Order` declared here shows the
shape. In `core` it is the lang item.

### 7.6 Listing 6: a refined type

Uses: U02, U07; P1, P2 (type target), P3c, P5. The declaration follows the reference's own `Port` example
(`reference/Orna-1.1.0/examples/valid/refined-type-assertions.orna`) and fits today's grammar. `50.Percent` is
`Percent.from(50)`.

```orna
// std/percent.orna. ILLUSTRATION.
/// A whole percentage from 0 to 100.
pub type Percent = Int {
    assert >= 0;
    assert <= 100;

    impl Mul<Decimal, Decimal> {
        fn mul(self, total: Decimal): Decimal = total * Decimal.from(self.base) / 100.decimal;
    }
    impl Display {
        fn display(self, context: DisplayContext): Str = "{self.base}%";
    }
}

// User code:
//   let tax = 20.Percent * 80.decimal;    // 16
//   let bad = 150.Percent;                // fails the assertion `<= 100` at construction.
//                                         // A check-time error once P8 folds it (or P4-B evaluates it).
```

### 7.7 Listing 7: an `Option` helper (real)

Uses: U09, U10. **REAL**: `stdlib/std/option.orna:15-23` and `:31-34` at the pin `fdf63e87`. `T?` is built in.
Everything a user does with it is library.

```orna
pub fn map<T, U>(value: T?, transform: fn(T): U): U? = case value {
    Some(inner): first([transform(inner)]),
    null: null,
};

pub fn and_then<T, U>(value: T?, transform: fn(T): U?): U? = case value {
    Some(inner): transform(inner),
    null: null,
};

pub fn unwrap_or<T>(value: T?, fallback: T): T = case value {
    Some(inner): inner,
    null: fallback,
};
```

Line 2 shows a workaround. `first([x])` builds the option without nesting one when `U` is itself an option. D3 makes
`Some(x)` the explicit constructor and makes `T??` an error, so `Some(transform(inner))` is enough. The same file has
`flatten<T>(value: T? ?)` (`option.orna:50`), whose parameter type is `T??`. D3 makes that type an error, so the
function needs a decision (delete it or retype it).

### 7.8 Listing 8: table helpers

Uses: U10, U11. Only today's primitives. The probed checker accepts this module.

```orna
// std/table.orna. ILLUSTRATION. Passes today `[probe]`.
pub table Reading(id: Uuid = uuid7()) {
    taken: Instant,
    value: Decimal,
}

/// The newest row by a timestamp, or null for an empty table.
pub fn latest<T>(rows: Relation<T>, stamp: fn(T): Instant): T? =
    rows | sort_by(stamp) | last();

/// One page of a stable order. Pages count from 1.
pub fn page<T, K>(rows: Relation<T>, order: fn(T): K, number: Int, size: Int = 50): Relation<T> =
    rows | sort_by(order) | drop((number - 1) * size) | take(size);

pub fn newest_reading(): Reading? = latest(Reading, r => r.taken);
pub fn second_page(): Relation<Reading> = page(Reading, r => r.taken, 2, size: 20);
```

### 7.9 Listing 9: a formatter in Orna

Uses: U15, U19; P1 (`Neg`), P5. It replaces one of the four natives `duration_*_format`
(`orna-hir/interface/core/time.orna`). The native context argument is only validated, so none is needed here.

```orna
// std/time/duration/compact.orna. ILLUSTRATION. 8_040 seconds is "2h 14m".
pub fn format(duration: Duration): Str {
    if duration < 0.seconds {
        "-{format(-duration)}"
    } else {
        let seconds = duration.nanoseconds() / 1_000_000_000;
        let hours = seconds / 3600;
        let minutes = seconds % 3600 / 60;
        let rest = seconds % 60;
        if hours > 0 {
            "{hours}h {minutes}m"
        } else if minutes > 0 {
            "{minutes}m {rest}s"
        } else {
            "{rest}s"
        }
    }
}
```

### 7.10 Listing 10: temperature scales as library types

Uses: U01; P1 only. This is the affine point and vector rule of `ORNA-UNIT-005` as two types. There is no affine flag in
the kernel. `Temperature + Temperature` has no instance, so it is an error.

```orna
// std/units/temperature.orna. ILLUSTRATION. Kelvin inside; `20.celsius` would be a suffix (P2) that adds 273.15.
pub type Temperature {
    pub kelvin: Decimal,

    impl Sub<Temperature, Delta> {
        fn sub(self, other: Temperature): Delta = Delta { kelvin: self.kelvin - other.kelvin };
    }
    impl Add<Delta, Temperature> {
        fn add(self, change: Delta): Temperature = Temperature { kelvin: self.kelvin + change.kelvin };
    }
}

pub type Delta {
    pub kelvin: Decimal,

    impl Add<Delta, Delta> {
        fn add(self, other: Delta): Delta = Delta { kelvin: self.kelvin + other.kelvin };
    }
    impl Mul<Decimal, Delta> {
        fn mul(self, factor: Decimal): Delta = Delta { kelvin: self.kelvin * factor };
    }
    impl Add<Temperature, Temperature> {
        fn add(self, point: Temperature): Temperature = Temperature { kelvin: point.kelvin + self.kelvin };
    }
}
```

### 7.11 Fit against the reference grammar

I checked each listing by reading it against the reference grammar (chapter 1) and by running it through the prebuilt
`orna check` `[probe]`. "Parses" means the probed parser accepts the text. Any failure after parsing comes from a gap of
section 2, not from the grammar.

| Listing | Parses today | What stops it | Gap |
|---|---|---|---|
| 1 units | no | enum `impl` (`ORNA-PARSE-003`); the `=` clause; the family header | P3a, D15 |
| 2 algebra | no (`ORNA-PARSE-001`) | a value parameter on a type; impl variable `V` | P4-B, P1 |
| 3 Duration | type yes, enum no | enum `impl` (`ORNA-PARSE-003`); `self.base`; operators; `pub use` | G3, G5, G1, D11 |
| 4 Money | yes | `C.code`; `Display` and `Explain` calls; `Decimal` interpolation | G4, G8 |
| 5 Range | shape yes | generic type with `T` fields; the name `Range`; `value.compare(..)` | G13, G7 |
| 6 Percent | yes | `self.base`; `Mul` instance; `50.Percent` | G5, G1, G2 |
| 7 Option | real | none | none |
| 8 Table helpers | yes; checks today | none | none |
| 9 Formatter | yes | `duration.nanoseconds()`; `Int` interpolation | G7, G8 |
| 10 Temperature | yes | instance resolution; forward reference to `Delta` | G1, G7 |

The enum form of listing 1 does not parse today. Probes 12, 13 and 21 ran the 1.1.0 keyword form of the same
module, which D15 does not ship. The probed checker accepted it and the first two user lines, and rejected the third
with `E5001`. That shows the algebra exists. It says nothing about the enum surface.

Three listings could not be made to fit: listing 1 (enums cannot own impls and have no `=` clause), listing 2 (it needs
syntax that does not exist) and the `TimeUnit` half of listing 3 (enums cannot own impls). All three are the point: they
show what P3a, the definition clause and P4-B add.

`pub use` itself is decided (D11) but the parser at `a4cf733b1` has no code for it (no `pub use` in
`orna-syntax/src/parser.rs`), and the probed binary rejects it.

--------------------------------------------------------------------------

## 8. Candidate "magic" features

Conclusion: sixteen candidates, listed by how often a user meets the feature, then how readable the library source is,
then (in the other direction) how much kernel work it needs. The order is **not a recommendation**. Two candidates need
no new primitive (5 and 16). Candidate 15 needs only candidate 1. The order that fits the HIR stages is in section 8.2.

Cost words: **S** is none, or under 200 lines of Rust. **M** is 200 to 1,000. **L** is more than 1,000. All are
`[INFERENCE]` except where a number is cited.

### 8.1 The list

**1. Money that reads like arithmetic.**

* The user sees: `12.34.GBP`, `price * 3`, `a + b`, `"{total}"`, and a plain-words error for `GBP + USD`.
* Library: listing 4 (`Money<C> = Decimal { ... }`, `Currency`, `Explain`).
* Needs: step zero, P1, P2, P3c, P3d, P5, P7, P9.
* Cost: M. It deletes about 260 lines of Rust: 185 in the checker and 75 in the evaluator (Appendix A).
* Risk: tag 60007; step counts; `Float` must stay refused (ORNA-NUM-003).

**2. Time that reads like arithmetic.**

* The user sees: `1.hour + 30.minutes`, `now() - 7.days`, `deadline - now()`, `"{elapsed}"` printing `2h 14m`.
* Library: listings 3 and 9.
* Needs: step zero, P1, P2, P3a, P3c, P5, P9; D11 for plural spellings; PR #9854 Q1.
* Cost: M. It deletes about 190 evaluator lines (`E:lib.rs:7097-7288`), a checker table of 17 lines and four natives.
* Risk: tags 60002 and 60005; hosting `Instant` (P9); the nanosecond-grid rules of `E:lib.rs:5919-5972` must be copied
  exactly.

**3. Interpolation of anything displayable.**

* The user sees: `"{when} paid {price} in {elapsed}"`.
* Library: a `Display` impl in each type.
* Needs: step zero, P5.
* Cost: S to M.
* Risk: the signature question (P5); the reference text and the overview example change.

**4. Units of measure.**

* The user sees: `5.kilometre + 200.metre`, `Speed = Length / Time`, and `5.metre + 3.second` as an error.
* Library: listing 1 with P4-A now, and listing 2 when P4-B takes over (D14).
* Needs: P2, P4-A, P1, P7; P3a and P3e for the enum surface (D15); P4-B later.
* Cost: L for A (PR #9854 estimates 900 to 1,200 lines); larger for B.
* Risk: exactness rules; `Float` quantities; dynamic boundaries; time suffixes are `Duration`, not quantities (C11).

**5. Table helpers.**

* The user sees: `Reading | latest(r => r.taken)`, `page(rows, order, 2)`.
* Library: listing 8.
* Needs: nothing.
* Cost: S.
* Risk: `sys.explain` prints kernel stages, not helper names; a user function named `one` collides with the kernel
  relation `one` today `[probe]`.

**6. Ranges as values.**

* The user sees: `1..=5`, `x in r`, `r.overlaps(s)`, `clamp(x, r)`.
* Library: listing 5 and helpers.
* Needs: P1, P9, P10; G13 fixed.
* Cost: M.
* Risk: tag 60019; the name `Range` is taken in the probed checker; ordering of ranges (ORNA-RANGE-005); `for` over a
  range stays kernel.

**7. Refined values with literals.**

* The user sees: `50.Percent`; `150.Percent` fails before the program runs.
* Library: listing 6.
* Needs: P2, P3c. The check-time error needs P8 folding with a check-time report, or P4-B.
* Cost: S at run time; M at check time.
* Risk: error timing. A folded failure must match the run-time error (P8 R4).

**8. Rounding modes as an enum.**

* The user sees: `rounding: half_even`, `Rounding.floor`, and a list of modes in the editor.
* Library: `pub enum Rounding { half_even, half_up, toward_zero, away_from_zero, floor, ceil }` in `core`; natives take
  `Rounding`, not `Str`.
* Needs: P3b; P3a if the enum has impls.
* Cost: S. It deletes the name-based cases (`C:calls.rs:2701-2702`, `E:lib.rs:10165-10180`).
* Risk: native signatures change (`rounding: Str?` becomes `Rounding?`), so the ABI number moves.

**9. Generic aggregates.**

* The user sees: `sum`, `mean`, `min`, `max` over `Money`, `Duration` and quantities.
* Library: generic functions bounded by `Add`, `Zero`, `Order`; `mean` also needs a divide-by-`Int` instance.
* Needs: P1, P3d (a `static zero` through the parameter).
* Cost: M. It removes part of the 14 `stats` natives and the `SRC?` rows.
* Risk: exactness and rounding rules of `mean`, `median`, `percentile` must match byte for byte; step parity.

**10. Duration formatters in Orna.**

* The user sees: `compact`, `clock`, `words`, `iso` output, unchanged.
* Library: listing 9, four times.
* Needs: P5 for `Int` display; an accessor (listing 3's `Nanoseconds`).
* Cost: S. It removes four natives.
* Risk: output must equal today's natives for every input; the `{locale, time_zone}` context.

**11. Data sizes.**

* The user sees: `3.GiB + 512.MiB`, printed as `3.5 GiB`.
* Library: a size family, plus a `Display` that picks the largest unit giving a value of at least 1.
* Needs: as candidate 4.
* Cost: S on top of 4.
* Risk: SI against binary prefixes in display.

**12. Temperature and other affine scales.**

* The user sees: `20.celsius + 5.delta_celsius`; `20.celsius + 5.celsius` is an error.
* Library: listing 10.
* Needs: P1, P2.
* Cost: S.
* Risk: the aggregation matrix of `ORNA-UNIT-007` (the mean of points) becomes library functions.

**13. Calendar arithmetic.**

* The user sees: `2026-09-01 + 3.months`, `date.next_business_day()`.
* Library: `Period` and `Date` instances over `std.time.calendar` (212 lines of Orna today).
* Needs: P1, P2; `Date` as a `native type` (P9).
* Cost: M.
* Risk: month-end and zone rules (ORNA-TIME-002 to -004); the Period against Duration decision.

**14. Custom iterables.**

* The user sees: `for day in days_between(a, b) { ... }`.
* Library: `Iterable<T>` over `std.iterator.Iterator<T>` (`stdlib/std/iterator.orna:8-10`).
* Needs: step zero, P10.
* Cost: M.
* Risk: cancellation points and step charge for the library `pull`; chapter 3 lists the iterable kinds today.

**15. Currency conversion as data.**

* The user sees: `rates.convert(total, GBP, on: date)` (ORNA-MONEY-005).
* Library: a table of rates and one function.
* Needs: candidate 1.
* Cost: S.
* Risk: the rounding policy must be a named operation (ORNA-CONVERT-008).

**16. Validated strings.**

* The user sees: `Email.from(raw)` failing with a plain code.
* Library: a nominal type with `impl From<Str>` (the reference example).
* Needs: nothing.
* Cost: none.
* Risk: none. It is the pattern users copy for their own types.

### 8.2 An order that fits the HIR migration

Stages are those of hir.md section 6.3. "Lands" means the work can merge behind the differential harness. Switching the
default consumer follows the harness rules of hir.md 6.1. The decisions D14 to D18 fix the order below.

* **Any time, no HIR dependency:** candidates 5 and 16, and a `Rounding` enum declared in std without shorthand.
* **H1 (lowering, resolver):** P3a grammar and lowering, the first parser change (D15), with the variant definition
  clause `= expression` and the family header; the P3b resolver rule; `native type` parsing; `pub use` (D11).
* **H2 (typing):** step zero, protocol member calls resolve (D16), and G13 (H2b); P1 selection with implicit impl
  variables; P2 resolution in place of `builtin_postfix`; P3c, P3d and P3e; P5 in place of `displayable`. H2c-1, the
  P4-A unit-key kernel behind the four-item interface (D14), needs none of the above and can land first. H2c-2, the
  unit families, needs P2, P3a, P3e and the `Unit` protocol. Waits: P4-B.
* **H3 (checks):** P7 diagnostics; the effect chain naming implicit calls; the `core` purity check; the refined-literal
  report. Waits: const-based checks (P4-B).
* **H4 (executor):** P8 R1 to R4 in the operator-tree builder; the step charge of D18 (one step per call of a loop-free
  pure `core` function); benchmarks B4 and B5. When H4 and H5 are done, P4-B starts: the executor becomes the
  check-time evaluator (D14).
* **H5 (host, link):** `core.orna` as source of truth; the P9 registry and `native type` (D17); the P10 validator;
  natives out (Money, time formatters, `SRC?` rows). Waits: library `Range` until G13, P9 and P10.
* **After H4 and H5:** P4-B takes over interface items 2 and 3 (D14); P7-B; the check-time half of candidate 7.
* **H9:** the one step re-baseline (D18; hir.md 6.4).

By candidate:

* **Lands with H2:** 3 (interpolation), 7 at run time (P2), 8 (resolver rule), 12 (after P1), 4 and 11 (units and data
  sizes: the kernel in H2c-1, the enum families in H2c-2), and the typing half of 1, 2, 9.
* **Lands with H3:** the check-time half of 7; the user-term messages of 1, 2, 4.
* **Lands with H5:** 1, 2, 10 and 13 as library (the kernel types and natives leave in the same PR family); 6; 9.
* **Waits:** 4 in the P4-B form, until after H4 and H5 (the source does not change); 14 (after P10 and the iteration
  rule).

The reason for the split: H2 changes what the checker accepts and can be proved by the harness. H5 changes where bytes
and natives live. Moving a feature to library in H2 and switching its bytes in H5 keeps each proof small.

--------------------------------------------------------------------------

## 9. Risks, rejected alternatives, tooling, versioning

Conclusion: the main risks are step counts, bytes, error wording and unreadable sugar. Each has a rule above. The things
that stay in Rust are listed in 9.3. Tooling gets the open-the-box experience for free once HIR is the one resolver
(9.4). The embedded core needs a versioning rule that does not exist yet (9.5).

### 9.1 Risks

* **R1. Sugar becomes unreadable** (long impl headers, copied boilerplate). Mitigation: P6 default none; revisit
  delegation after real use; review gate: a `core` listing reads at a glance.
* **R2. Compile time** (instance lookup, generic instantiation, const evaluation). Mitigation: index instances by
  protocol and head type; cache per `Program`; scalar fast path; quota on const evaluation; measure in H2.
* **R3. Library internals leak into errors.** Mitigation: P7 rules 1 to 5; snapshot tests of user-term messages per
  feature.
* **R4. Step counts change.** Mitigation: D18 (one step per call of a loop-free pure `core` function); one re-baseline
  at H9 (hir.md 6.4).
* **R5. Bytes drift.** Mitigation: the P9 guard; golden vectors in CI; the `core` paths that the registry names are ABI
  (D17).
* **R6. A second evaluator appears.** Mitigation: one `ConstEval` trait implemented by `orna-exec`; no interpreter in
  `orna-hir`.
* **R7. Bootstrap cycles** (`core` uses operators it defines). Mitigation: layering of kernel scalars, then `core.ops`,
  then `core.money`, `core.time` and `core.range`, then `std`; the build script rejects a cycle.
* **R8. Two orders or two equalities for one type.** Mitigation: `Order` derived for canonical types; `==` stays
  canonical.
* **R9. A `core` change silently alters old programs.** Mitigation: section 9.5.
* **R10. The box opens onto a puzzle** (type-level encodings). Mitigation: reject P4-C; the readability test T3 in
  review.
* **R11. Names collide with kernel names** (`one`, `Range`) `[probe]`. Mitigation: the lexical shadowing rule applies to
  every `core` name (hir.md 2.6 does it for `Some`).
* **R12. Library arithmetic is slower.** Mitigation: the P8 rules and the B4 and B5 gates.

### 9.2 Alternatives considered and rejected

* **Keep everything in Rust.** Rejected: the owner direction, and 26 special cases with 2,385 measured lines.
* **A macro system** for derive and units. Rejected for now: it adds a second language, hygiene rules and tooling work.
  P4-B is the bounded form of the same need.
* **Dependent types.** Rejected: P4-B is the bounded form.
* **Dynamic `Any` quantities.** Rejected: chapter 0 says there is no dynamic `Any`.
* **UFCS for literals** (`5.f` calls any `f(5)`). Rejected: chapter 3 rule 9 says a free function is not a method
  because its first parameter matches.
* **Implicit `Int` to `Decimal` widening for quantities.** Rejected by D7. Where PR #9854 widens, it is by a stated rule
  (`ORNA-UNIT-016`).
* **Attributes for lang items.** Rejected: the grammar has none (native-declarations.md 3.1).
* **`native fn` in `std`.** Rejected by D12.
* **Several functions with one name.** Rejected by D5. P1 selects an instance of one member instead.
* **New operator symbols.** Rejected. Rust's operator module says it "does not provide any mechanism to create new
  operators"; the set stays fixed.
* **A library `Option`.** Rejected by D3. `T?` is kernel; its helpers are library already.
* **Keyword unit declarations (`dim`, `unit`, `alias`).** Not shipped (D15). The enum is the one form.
* **A format change for library types (P9-C).** Rejected for now (D17). Revisit only with a storage format version.
* **A `const fn` marker.** Not needed (D14). The check-time evaluator runs any pure `core` body.

### 9.3 What stays in Rust forever

* Evaluation of control flow, assignment, closures and pattern matching.
* Unification, protocol selection, exhaustiveness, effect inference, the lowering and the one resolver.
* The representation of `Int`, `Decimal`, `Float`, `Str`, `Bytes`, `Blob`, `Uuid`, `Digest`, `Date`, `Instant`, `T?`,
  lists, maps and sets, tuples, records and function values.
* The canonical encoding, the canonical total order, equality and hash.
* Tables, transactions, assertions and the candidate-relation model, the relation operator tree, Git and storage.
* Resource accounting and cancellation.
* The time-zone database, Unicode tables, SHA-256, the codecs, and `sys` providers.
* The table of lang items (P10), the wire registry (P9), and the unit-key algebra (P4-A) until P4-B takes over interface
  items 2 and 3 (D14). P4-B adds no second evaluator: the check-time evaluator is the HIR executor.
* Diagnostic codes, spans and printing. The library supplies words, never codes.
* The formatter and the parser.

### 9.4 Tooling: hover and go-to-definition are the open-the-box experience

The editor and the engine share one resolver once HIR lands (hir.md 3.4). A name resolves to a `Def`, and a library
`Def` has a span in embedded source. So opening the box is an ordinary go-to-definition. What the tooling needs:

1. **Addressable embedded sources.** `core` is served from the binary under a read-only virtual path such as
   `orna-core:/money.orna`. The editor opens it like a file.
2. **Doc comments** (`///`) become hover text. Listing 3's header comment is what a user reads first.
3. **Natives** show the signature, the effect clause and the bound Rust function from the link table (H5), for example
   "implemented by `natives::money::quantize`".
4. **Implicit calls resolve visibly.** Hover on `+` shows the instance, `Add<Money<GBP>, Money<GBP>>` at
   `core/money.orna:12`. Go-to-definition on `+` lands on that method. On `kilometre` in `5.kilometre` it lands on the
   variant. On `{x}` in a string it lands on the `Display` instance for the type of `x`.
5. **Inlay hints** show the instantiated types of generic library calls.
6. **References** work: "find all uses of `Duration.add`" finds every `+` on durations, because each is a
   `Callee::Source` (hir.md 2.6).

Today the language server has its own resolver and `Hint` approximations (hir.md 1.2, rows R24 to R26). Until H1 removes
them, the experience is partial: names resolve, operators and suffixes do not. The experience starts at H1 for names and
at H2 for operators, suffixes and interpolation.

### 9.5 Versioning of the embedded core

Facts: D12 versions natives by `NativeAbi(u32)`, each `core.orna` declaration carries `since` and optionally `until`, a
captured program records its ABI, and a retired native stays under a retired name (hir.md 4.5). ORNA-LIB-001 says "a
program's library behaviour MUST be determined by its captured dependency snapshots".

New with Orna-bodied `core`: the behaviour of `Money + Money` is now an ABI fact, and identity derives from `core` paths
(`E:source_enums.rs:23-60`). Options:

* **(A) Frozen generations.** The engine embeds the source of each ABI generation. A snapshot selects its generation. A
  fix that changes results starts a new generation. Buys: exact history. Costs: binary size and a test matrix.
* **(B) Additive only.** A `core` item never changes behaviour. A fix is a new item and the old one stays under `until`.
  Buys: one copy. Costs: an instance cannot be versioned by name, because it is chosen by the type. A changed instance
  needs a new type.
* **(C) Errata policy.** The reference fixes the behaviour. A bug fix that changes results is listed in the changelog
  and historical programs see it. Buys: simplest. Costs: weaker than ORNA-LIB-001 gives `std`.

Paths are ABI in every option. `std` may rename freely, because it re-exports `core` paths with `pub use` (D11). `core`
may not. D17 fixes the part that is certain: the `core` paths that the wire registry names are ABI, renaming one bumps
the `NativeAbi` number, and the build guard checks it. For every other `core` path the default below stands.

**What I do until told otherwise:** (B), with (A) as the way out for an instance that must change, and generation 1
frozen at the first release that ships `core` as source.

--------------------------------------------------------------------------

## 10. Changes to the units design (PR #9854): status and concrete list

Conclusion: all twelve changes are accepted under D14 to D18. This is the concrete list that the author of PR #9854
applies to `units.md`. This document edits none of that file. Section and rule numbers are those of `units.md` at head
`9cd0fddfc` (file docs/design/units.md on branch docs/units-ordinary-declarations). The only change from `86be7f979`,
where this review began, is the colon bound in C3. Where the delegation did not name an item, the rule used is "keep one
declaration form and fewer keywords", and the item says so.

After these changes `units.md` section 12 keeps Q1 (`day` and `days`) as open. Q2 and Q3 are answered by C7. Q4 goes
with C5. Q5 and Q6 go with C11.

* **C1. Surface (section 2). ACCEPTED (D15).** Replace the declaration forms and their grammar with the enum-shaped
  surface of section 5.5 here. Delete `dim_decl`, `unit_decl`, `alias_decl`, `clause`, `offset_clause` and `affine`.
  Restate ORNA-UNIT-009 and -010 for families (section 5.5.1). Rewrite the examples of sections 7 and 8 in the enum
  form; listing 1 is the model. Reason: one declaration form, and no program declares a unit yet, so nothing migrates.
  The frozen 1.1.0 examples `examples/valid/units.orna` and `examples/valid/affine-unit.orna` use the keyword forms.
  They stay frozen and go to the 1.2 migration appendix (D13).
* **C2. `alias_decl` (section 2). ACCEPTED.** Delete the `alias` declaration. Write
  `pub type Money<C impl Currency> = Quantity<Decimal, C>;` with the existing alias form (U03). Reason: one way to do a
  thing (DECIDED principle 1), and `repr` is gone (D17), so `alias` has nothing left to carry. Of the two options this
  is the one with fewer keywords.
* **C3. Colon bound (section 8.3). ACCEPTED and done.** The author changed `Money<C: Currency>` to
  `Money<C impl Currency>` in `9cd0fddfc` after this review. Reason: 1.1.0 removed the colon bound (ORNA-GENERIC-006,
  `ORNA091-E-BOUND-COLON`).
* **C4. Postfix lookup (section 5 item 3; ORNA-UNIT-008). ACCEPTED.** State the lookup as the P2 rule: a variant of a
  unit family in scope, then a type target. "Then as a currency, then as a calendar period" become library lines (a
  `Suffix` enum for `Period`). Two hits in one step are ORNA-S011, and the qualified form resolves it (section 5.5.3).
  Reason: the kernel keeps one rule, and `day` is a declaration returning `Period`, so Q1 is one line to change.
* **C5. `canonical` (ORNA-UNIT-018; Q4; `DimensionDef.canonical`). ACCEPTED: removed.** Delete the clause, the field and
  the clause number 018 (leave the number unused, so 019 keeps its number). A library suffix normalises where a family
  wants it (listing 3). Reason: only time declared one, and C11 moves time out of the algebra. One clause and one field
  fewer in the kernel, and no new word in the grammar.
* **C6. Affine units (ORNA-UNIT-005 and -007; section 3.3). ACCEPTED.** Realise the point and vector rules as library
  types (listing 10). Drop `offset` and `affine` from the key (`UnitKey` loses `o` and `a`; sections 3.3 and 3.4
  shrink). Reason: D15 makes a definition a plain quantity expression, which has no offset form, and the other way
  needs two more clauses. Cost: affine units no longer share `Quantity`, and the aggregation matrix becomes library
  functions. The results of ORNA-UNIT-005 and -007 do not change.
* **C7. `to` (ORNA-UNIT-015; Q2; Q3). ACCEPTED, with one form.** Realise it as a library function over two kernel casts
  (erase, unerase) and the ratio query. The target unit is a type argument: `to<Length.mile>(distance)`. D5 allows a
  type only as a type argument, and a unit passed as a value would make the result type depend on a value. The name
  `to` stays. The rounding form takes a `Rounding` enum (candidate 8). Reason: conversion is data and arithmetic, not a
  typing rule.
* **C8. Currency axis (ORNA-UNIT-017; O4). ACCEPTED.** Keep the axis as the default. Add "Money as a library type plus
  `Rate<C, U>`" (section 5.7 here) to O4 as a second option. Which one ships is still open (section 11). Reason: the
  reader sees both with their costs.
* **C9. Native minimum (section 5). ACCEPTED, corrected.** Restate it as the four-item interface of section 5.6 here.
  Item 5 (`canonical` and `repr`) leaves the list (C5, D17). The currency axis stays: an axis is a base variant or a
  type that implements `Currency`, and that is part of interface item 2. Reason: a frozen, minimal interface lets P4-B
  replace items 2 and 3 later with no change to any source.
* **C10. Stage plan (section 11.2). ACCEPTED.** Split H2c into H2c-1 (the kernel interface; needs none of P1 to P3) and
  H2c-2 (the `core.units` listings; needs P2, P3a, P3e and the `Unit` protocol). Reason: the kernel can land first.
* **C11. `Duration` (section 5 item 5; O6; Q5; Q6). ACCEPTED.** Make `Duration` a library type over a kernel integer,
  registered to tag 60005 (D17; listing 3). The time suffixes return `Duration`. Replace the "time view" rule with
  explicit conversions. Q5 is answered (a separate type, O6's default). Q6 disappears: `1.hour + 30.minute` is a
  `Duration`, so a `Duration` annotation fits. Reason: it matches O6's default and removes a checker rule. Consequence:
  time suffixes are not quantities, so `5.kilometre / 2.hour` with the core `hour` is an error (a length divided by a
  `Duration`). Physics code imports a `Time` family (listing 1), and an import shadows the intrinsic suffixes
  (section 5.5.3).
* **C12. Questions (section 12). ACCEPTED.** Replace Q2 to Q6 by the answers above and keep Q1. Add a pointer to D17 for
  wire forms. Reason: one decision about wire forms for all legacy-tagged types.

--------------------------------------------------------------------------

## 11. Decided 2026-11-10 (owner delegation)

Conclusion: the owner delegated the five questions of the first revision of this document ("i will let you figure out
those decisions"). They are decided as D14 to D18. [DECIDED.md](../language/DECIDED.md) records each with its reasoning
and what reopening costs. This section states each decision and what it unblocks.

| Id | Was | Decision |
|---|---|---|
| D14 | Q1, compile-time evaluation | Yes, staged: P4-A now; P4-B after H4 and H5 |
| D15 | Q2, the surface of unit families | Enum-shaped families; keyword forms not shipped; P3a first |
| D16 | Q3, operator protocols | P1-A; implicit impl variables; instance selection is within D5 |
| D17 | Q4, bytes of legacy types | P9-A registry plus `native type`; P9-C rejected for now |
| D18 | Q5, step accounting | P8 R5 option (ii): one step per loop-free pure `core` call |

**D14. Compile-time evaluation.** Yes, as the destination, in two stages.

* Now: P4-A. The unit-key kernel stays behind the four-item interface of section 5.6, so library source never changes.
* After H4 and H5: P4-B. The check-time evaluator is the HIR executor itself, running pure `core` bodies, that is,
  bodies whose inferred effect set is empty (D2). Purity is the criterion, so there is no `const fn` marker. P4-B then
  takes over interface items 2 and 3.
* Unblocks: listing 2, P7-B and the check-time half of candidate 7. Blocks nothing in H2c.

**D15. The surface of unit families.** Enum-shaped families.

* `Length.kilometre`, `5.kilometre` and user-declared families share one declaration form, the enum.
* The keyword forms `dim` and `unit` of PR #9854 are not shipped. No program declares a unit yet, so nothing migrates.
* The text after `=` in an enum body is a unit expression under P4-A and an ordinary Orna expression under P4-B.
* Dimensional algebra is kept (D8, as amended on PR #9854).
* P3a, `impl` and `static` members in enums, is the first parser change. Part e of P3 follows from it.
* Section 5.5 spells out the exact surface, derived dimensions included, and lists the grammar impact.

**D16. Operator protocols.** P1-A.

* The output type is a protocol parameter. Impl variables are implicit (P1 (i)): a free capitalised name in an impl's
  protocol arguments is bound by the impl.
* Type-directed selection of an instance of one protocol member is within D5. D5 forbids a name that denotes several
  functions. `a + b` names one protocol member, and the types select the instance, exactly as `From<Str>` and
  `From<X>` already are. Two applicable instances are an error.
* `==` and `!=` stay canonical equality. `Order` is derived by the engine where a canonical order exists. Scalar pairs
  keep the direct fast path.
* Step zero, protocol member calls resolve, is scheduled in H2.

**D17. Bytes of library-defined legacy types.** P9-A, a registry in Rust, plus `native type` declarations in `core` for
`Instant` and the like.

* P9-C, a format change, is rejected for now. It may be revisited only with a deliberate storage format version.
* P9-B is not built, so no `repr` or `wire` clause exists.
* The `core` paths that the registry names are part of the engine ABI. Renaming one bumps the `NativeAbi` number, and
  the build guard checks it.

**D18. Step accounting.** Option (ii) of P8 R5.

* A call to a loop-free pure `core` function costs one step. Rules R1 to R4 stand as written.
* Explicit weights (option iii) are added only where a measured need appears.
* The one re-baseline stays at H9 (hir.md 6.4).

**Two further resolutions.**

* Two unit families in scope that both define the same suffix are an ambiguity error (ORNA-S011 style). The qualified
  form resolves it. There is no last-import-wins rule, so a result never depends on import order. Section 5.5.3 says
  what "in scope" means.
* A private function with an unresolved numeric operand type stays a diagnostic that asks for an annotation (D1
  unchanged). There is no defaulting to `Int` or `Decimal`.

**The twelve changes to units.md** are all accepted (section 10). C2, C5, C7 and C11 carry a stated form.

**Proposed, not decided.** D15 fixes the principle. Section 5.5 proposes the spelling: the derived-dimension header
(5.5.4), the `Unit` marker (5.5.1), the narrow form of P3e (4.3) and the shadowing rule (5.5.3). Each states its default
as "what I do until told otherwise".

**Still open.** These are smaller questions. Each has a default in the section named, and none blocks a stage.

* The `Display` signature, with or without `DisplayContext`, and interpolation of every displayable type (P5).
* `day` and `days` as `Period` or `Duration` (PR #9854 Q1; P2).
* The currency axis against Money as a library type (section 5.7).
* `base` access for refined types (P3c) and static access through a type parameter (P3d).
* Whether stage-producing relation operators need a lazy-relation primitive (section 3.3).
* `std.option.flatten<T>(value: T? ?)` against D3 (listing 7).
* When `pub use` lands (D11 is decided, the parser at `a4cf733b1` has no code for it).

--------------------------------------------------------------------------

## Appendix A. Evidence and counts

### A.1 Rust function sizes

A script found each `fn name(` line and counted to the matching closing brace, at `a4cf733b1`.

| Feature | Function | At | Lines |
|---|---|---|---|
| suffix | `numeric_postfix` | `E:lib.rs:5885` | 34 |
| suffix | `numeric_decimal_postfix` | `E:lib.rs:5919` | 54 |
| suffix | `infer_unit_suffix` | `C:lib.rs:5352` | 48 |
| suffix | `is_calendar_period` | `C:lib.rs:5402` | 11 |
| Money | `infer_currency_suffix` | `C:lib.rs:5278` | 32 |
| Money | `reduce_exact_money_rate` | `C:lib.rs:5431` | 42 |
| Money | `infer_money_constructor_type` | `C:lib.rs:5531` | 28 |
| Money | `money_generic_constructor_currency` | `C:lib.rs:5499` | 13 |
| Money | `incompatible_addition_message` | `C:lib.rs:5326` | 14 |
| Money | `money_operator_type` | `C:inference.rs:1086` | 28 |
| Money | `validate_money_currency` | `C:item_checks.rs:1573` | 28 |
| Money | `money_pair_binary` | `E:lib.rs:7067` | 26 |
| Money | `money_binary` | `E:lib.rs:7616` | 33 |
| Money | `money_scalar_binary` | `E:lib.rs:7649` | 16 |
| time | `elapsed_time_operator_type` | `C:inference.rs:1034` | 17 |
| time | `temporal_binary` | `E:lib.rs:7097` | 25 |
| time | `instant_binary` | `E:lib.rs:7125` | 51 |
| time | `duration_binary` | `E:lib.rs:7179` | 110 |
| units | `quantity_operator_type` | `C:inference.rs:1117` | 46 |
| units | `quantity_sum_type` | `C:inference.rs:1168` | 79 |
| units | `temporal_quantity_mismatch` | `C:inference.rs:1264` | 32 |
| units | `dimension_name` | `C:inference.rs:1312` | 42 |
| units | `quantity_product_type` | `C:inference.rs:1357` | 42 |
| units | `unit_suffix_operator_type` | `C:inference.rs:1410` | 31 |
| units | the whole file `units.rs` | `C:units.rs` | 1,503 |

Totals: suffix 147, Money 260, time 203, units 1,775. Sum 2,385. The sum leaves out the 25 relation names, the 9 table
names, message strings, the range code and the interpolation code.

### A.2 Standard library and natives

Commands ran from the repository root at the pin `fdf63e87`.

| Quantity | Value | Command |
|---|---|---|
| `.orna` files in `stdlib/std` | 79 | `find stdlib/std -name '*.orna' -not -path '*/.orna/*'` |
| `pub fn` | 630 | `grep -rh -E '^pub fn '` |
| protocols | 11, all in `numeric.orna` | `grep -rh -E '^(pub )?protocol '` |
| `impl` blocks | 0 | `grep -rh -E '^\s+impl [A-Za-z]'` |
| `static` members | 0 | `grep -rh -E '^\s+static '` |
| enums, types | 7, 1 | `grep -rh -E '^(pub )?enum '`, `'^(pub )?type '` |
| marker bodies "is a native intrinsic" | 70 | `grep -rh 'is a native intrinsic'` |
| lines | 5,959 | `cat` of the files, `wc -l` |
| `native fn` in `orna-hir/interface` | 81 in `core`, 82 in `sys.orna` | `grep -rh -E '^(pub )?native fn '` |

`core/` by file: collection 27, stats 14, text 10, bits 6, time 6, hash 5, json 3, base64 2, orna 2, ovb 2.

### A.3 Other figures cited

* `orna-check` 36,372 lines; `orna-evaluator` 31,303; `orna-hir` `src` 12,274; `orna-exec` 592 (at `a4cf733b1`).
* Canonical tags and type codes: `reference/Orna-1.1.0/source/29-formats.md:23-44` and `:110-112`.
* Golden vectors: 16 files under `reference/Orna-1.1.0/tests` (hir.md section 6.2).
* Default budget 419,600 steps (hir.md section 1.7).

--------------------------------------------------------------------------

## Appendix B. Probes

The binary is `target/FixAppLive/debug/orna`, built 2026-11-09 from a lane branch. The command was `orna check` in a
fresh git directory holding one `main.orna`. It is not a build of main. Re-run before you rely on a row.

| # | Module (summary) | Result |
|---|---|---|
| 1 | `a + b` on two nominal values with `impl Add2` | `E2001: operator + is not defined for Meters and Meters` |
| 2 | `5.metre` with no declared unit | `ORNA-S012-UNRESOLVED: unit suffix cannot be resolved` |
| 3 | `impl Display { ... }` inside an `enum` | `ORNA-PARSE-003: unterminated enum body` |
| 4 | `fn f<C impl Currency>(..) = C.code` | `ORNA-S012-UNRESOLVED: name cannot be resolved` |
| 5 | `"{n}"` with `n: Int` | `ORNA-S021-TYPE: static types are incompatible` |
| 6 | `"{t}"` with `t` a type that has `impl Display` | `ORNA-S021-TYPE: static types are incompatible` |
| 7 | `t.show()` through a user protocol impl, on a value | `ORNA-S021-TYPE: field access requires a record` |
| 8 | the same call on `t: T` with `T impl Show2` | `ORNA-S021-TYPE: field access requires a record` |
| 9 | `fn raw(p: Percent): Int = p;` | `ORNA-S021-TYPE: static types are incompatible` |
| 10 | `type Box<T> { value: T }` | `ORNA-S012-UNRESOLVED: type name cannot be resolved` |
| 11 | `type Cash<C impl Currency> = Decimal { assert >= 0.decimal; }` | valid |
| 12 | units module, `5.kilometre + 200.metre` and `5.kilometre / 2.hour` | valid |
| 13 | units module, `5.metre + 3.second` | `E5001: cannot add Length and Time` |
| 14 | `12.34.GBP + 1.USD` | `E5003: cannot add different currencies without conversion` |
| 15 | `impl Mul<Q<E>, Q<D>>` with a free `E` | `ORNA-S021-TYPE: generic protocol instantiation cannot be validated` |
| 16 | impl says `Meters`, protocol says `Self` | `protocol implementation function signature is incompatible` |
| 17 | `pub use TimeUnit.hour as hours;` | `ORNA-PARSE-001: module top level accepts declarations only` |
| 18 | a user function named `one` | `finite-list one arguments do not match its static signature` |
| 19 | a parameter named `unit` | `ORNA-PARSE-001: expected pattern` |
| 20 | user `enum Ordering` and `protocol Order` with a bound | valid |
| 21 | `pub dim`, `pub unit` declarations | valid |
| 22 | the table helpers of listing 8 | valid |

--------------------------------------------------------------------------

## Appendix C. Prior art

Fetched during this work:

* Rust operator traits: https://doc.rust-lang.org/std/ops/index.html
* Kotlin operator conventions: https://kotlinlang.org/docs/operator-overloading.html
* C++ user-defined literals: https://en.cppreference.com/w/cpp/language/user_literal
* Rust constant evaluation: https://doc.rust-lang.org/reference/const_eval.html
* Rust diagnostic attributes: https://doc.rust-lang.org/reference/attributes/diagnostics.html
* The Rust `uom` crate: https://docs.rs/uom/latest/uom
* Julia Unitful types: https://engee.com/helpcenter/stable/en/julia/Unitful/types.html
* F# units of measure (cited in DECISIONS.md D8, not fetched here):
  https://learn.microsoft.com/en-us/dotnet/fsharp/language-reference/units-of-measure
