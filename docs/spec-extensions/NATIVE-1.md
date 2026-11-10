# NATIVE-1: native declarations for a pinned standard library

Amendment to the Orna 1.1.0 specification (`final-2026-10-05`).
Status: proposed. Owner sign-off is the order recorded in bead
`ornadb-yuzot` (2026-11-10). Design rationale:
[`docs/design/native-declarations.md`](../design/native-declarations.md).

This document contains the normative addition and its grammar delta. It adds
no other requirement, changes no existing production, and changes no lexical
rule.

## 1. Scope of the amendment

Added: a normative subsection for chapter 9 (core operations and the standard
library) and the grammar productions of section 6 below.

Touched, by narrowing one sentence: chapter 9's statement that the optional
`std` namespace "contains ordinary, explicitly imported library code pinned to
a Git snapshot" is read as *ordinary in every observable respect* - imported,
resolved by ordinary `use` rules and lexical scope, shadowable, pinned,
documented and statically typed - with the single exception that the
reference-library profile may state that an operation's implementation is
supplied by the runtime. ORNA-LIB-003's condition "when supplied under the
reference-library profile" is the phrase this amendment gives exact meaning.

Not touched: the keyword set (ORNA-LEX-007), ORNA-SOURCE-001/002/003,
ORNA-LIB-001/002/004/005/006/007, ORNA-NS-005 through ORNA-NS-008,
ORNA-TXN-007/008, ORNA-EVAL-004, and every definition of a type, expression,
statement, table, relation, stream, checkpoint, format or `sys` member.

## 2. Normative clauses

**ORNA-NATIVE-001** A native declaration has the form

```orna
pub native fn name<T, U>(parameter: Type, another: Type = default): Result effect effects;
```

with these requirements. It MUST NOT have a body. It MUST state a result type.
It MUST state a type for every parameter. It MUST NOT declare a `self`
parameter. It MUST state an effect clause. A native declaration whose result
type, any parameter type, or effect clause is absent is rejected.

**ORNA-NATIVE-002** `native`, `effect` and the effect class names are not
keywords and MUST NOT become keywords. The exact keyword set of ORNA-LEX-007 is
unchanged. At the top level of a module unit, the identifier `native`
immediately followed by the keyword `fn` begins a native declaration. No other
production begins with an identifier at that position, so recognition requires
two tokens of lookahead and no existing source changes meaning. `native`,
`effect` and every effect class name remain usable as identifiers, and
therefore as field names, member names, parameter names, function names and
namespace components, everywhere they are usable today.

**ORNA-NATIVE-003** A native declaration is permitted only as a top-level item
of a module unit. It is not permitted in `repl_input`, in a row unit, in a
block, or as a member of a `table`, `protocol`, `enum`, `type` or `impl` body.

**ORNA-NATIVE-004** Only a module of the admitted pinned `std` dependency may
contain a native declaration. A module is such a module when its logical path
is a standard module path and its bytes are the bytes verified against the
admitted standard dependency profile. A native declaration in project source,
in an attachment, in a generated stub, in a replacement `std` supplied outside
the admitted dependency, or through any interactive entry point is rejected at
load, naming the module path, the declaration name and the source position.

**ORNA-NATIVE-005** The binding key of a native declaration is the namespace
path of the module that declares it together with the declared name. Two native
declarations with the same binding key in one module are rejected. Where the
reference-library profile records two spellings of one identity in two modules
(chapter 9, re-export of the intrinsic operators), each module declares the
operation natively under the same key and the two declarations MUST have
structurally equal signatures.

**ORNA-NATIVE-006** A native declaration whose binding key has no
implementation in the evaluating build MUST be reported at load, before any
expression is evaluated, naming the module path, the declared name and the
pinned dependency identity. The report is an admission failure. There is no
substitute implementation, no failure-valued placeholder, no deferred run-time
error and no silent fallback to an unrelated operation.

**ORNA-NATIVE-007** An implementation whose binding key is admissible at the
admitted pin MUST have exactly one native declaration in the admitted snapshot.
At a pin that admits declaration binding, an implementation without a
declaration is reported at load with the module path, the declared name and the
pin. A pin that predates this amendment is governed by ORNA-NATIVE-011.

**ORNA-NATIVE-008** The signature of a native declaration is the type of the
declared function. Parameter names, parameter order, parameter types, default
values, admissible arities, generic parameters and the result type of a native
call are taken from the declaration and from nothing else. No implementation
table, registry, projection or generated artefact may define or restate them,
and none may be consulted to type a native call.

**ORNA-NATIVE-009** The effect clause states the host effects of the
implementation as a set drawn from the closed `sys.EffectKind` vocabulary, or
as `none` for the empty set. `none` is the only admissible spelling outside
that vocabulary. The clause is mandatory. Host effects that the vocabulary does
not name - waiting for and advancing a stream, and evaluating a relation plan -
are properties of the declared `Stream<T>` and `Relation<T>` types and are not
stated in the clause.

**ORNA-NATIVE-010** Where a native declaration exists, its effect clause is the
only admissible source of that operation's effect set. An implementation that
derives effects from the resolved call graph elsewhere MUST use the clause for a
native operation, because a native declaration has no body from which to derive
them.

**ORNA-NATIVE-011** The mechanism that binds an operation is determined by the
captured snapshot's own text, not by a version number. A captured snapshot that
contains no native declaration for an operation keeps the binding mechanism its
text supports, and a snapshot that contains one uses it. Removing an operation's
native declaration in a later pin changes that later pin's behaviour only and
MUST NOT change the behaviour of any earlier captured snapshot.

**ORNA-NATIVE-012** A native declaration is not part of a user program's own
source. A user program MUST NOT require, redeclare, extend, shadow or otherwise
obtain a native declaration outside an admitted pinned standard dependency. A
`std` dependency that is not the admitted pinned dependency (ORNA-NS-007) either
contains no native declaration, in which case it is ordinary source, or is
rejected under ORNA-NATIVE-004.

**ORNA-NATIVE-013** An operation is dispatched by the identity bound to its
resolved declaration at admission. An implementation MUST NOT dispatch an
operation because a spelling matches an entry in an internal table, and MUST NOT
run an implementation for a qualified spelling that resolved to no declaration.
This restates ORNA-LIB-002 for the evaluator.

**ORNA-NATIVE-014** A native declaration is a public declaration for name
resolution, import, documentation, hover, completion, signature help and
go-to-definition, and its declared signature and effect set are reported by
`sys` introspection. No member is added to the `sys` surface by this amendment.

**ORNA-NATIVE-015** The native application binary interface of a pin is the set
of `(module path, declared name, signature, effect set)` triples its native
declarations state. Changing the name, module path, signature or effect set of a
native declaration in a published pin is a different interface and requires a
different pin. A build that does not implement the interface of an admitted pin
MUST report the mismatch at load and MUST NOT adapt, substitute or partially
bind.

## 3. Diagnostics added

Load diagnostics:

| Code | Condition | Must name |
|---|---|---|
| `ORNA-STD-NATIVE-UNBOUND` | a native declaration has no implementation (ORNA-NATIVE-006), or the registry entry for the key is not implementation-bearing | module path, declared name, pin |
| `ORNA-STD-NATIVE-UNDECLARED` | an admissible implementation has no declaration (ORNA-NATIVE-007) | module path, declared name, pin |
| `ORNA-STD-NATIVE-SCOPE` | a native declaration is outside the admitted pinned dependency (ORNA-NATIVE-004, ORNA-NATIVE-012) | module path, declared name, file and position |
| `ORNA-STD-NATIVE-DUPLICATE` | two declarations share one binding key (ORNA-NATIVE-005) | module path, declared name, both positions |
| `ORNA-STD-NATIVE-BINDING` | the declared signature and the implementation's recorded signature disagree (ORNA-NATIVE-015) | module path, declared name, both signatures |

Parser diagnostics:

| Code | Condition |
|---|---|
| `ORNA091-E-NATIVE-BODY` | a native declaration has a body |
| `ORNA091-E-NATIVE-TYPE` | the result type or a parameter type is missing |
| `ORNA091-E-NATIVE-SELF` | a native declaration declares a `self` parameter |
| `ORNA091-E-NATIVE-EFFECT` | the effect clause is absent or names an unknown class |
| `ORNA091-E-NATIVE-PLACE` | a native declaration appears outside a module unit's top level |

## 4. Conformance

A conforming implementation satisfies, in addition to the clauses above:

1. ORNA-NATIVE-002 is demonstrated by a program that uses `native` and `effect`
   as identifiers on the same source line as a native declaration.
2. ORNA-NATIVE-003 is demonstrated by rejection of a native declaration in each
   position it forbids.
3. ORNA-NATIVE-004 and ORNA-NATIVE-012 are demonstrated separately for project
   source, a replacement dependency and an interactive entry point.
4. ORNA-NATIVE-006 and ORNA-NATIVE-007 are demonstrated by removing and by
   adding an implementation respectively, with the diagnostic naming the module
   path and the declared name in both directions.
5. ORNA-NATIVE-008 is demonstrated by a call that is accepted or rejected only
   because of a declared default value and a declared parameter type.
6. ORNA-NATIVE-009 and ORNA-NATIVE-010 are demonstrated by a native operation
   whose declared effect set forbids its use in an assertion, and by one whose
   `none` permits it.
7. ORNA-NATIVE-011 is demonstrated by the same program, under two captured
   snapshots that differ only in the presence of a native declaration, producing
   the two behaviours its two texts specify.
8. ORNA-NATIVE-013 is demonstrated by a qualified spelling that resolves to no
   declaration, which is a resolution failure and not a dispatch.

## 5. Relationship to existing clauses

| Clause | Relationship |
|---|---|
| `source/09-standard-library.md:3` | narrowed: the pinned reference dependency may state that an implementation is supplied by the runtime; everything else about it stays ordinary |
| `source/09-standard-library.md:9` (re-export) | satisfied: a native declaration is the only form in which Orna can express one identity under two spellings |
| `source/09-standard-library.md:11` (ORNA-LIB-001) | governs: ORNA-NATIVE-011 decides the mechanism from the captured text |
| `source/09-standard-library.md:13` (ORNA-LIB-002) | governs: ORNA-NATIVE-013 forbids a spelling-based dispatch |
| `source/09-standard-library.md:15` (ORNA-LIB-003) | satisfied: the reference-library profile's supplied set is its native declarations |
| `source/09-standard-library.md:19,21` | governs the declared types and the pipeline's first argument; no change |
| `source/09-standard-library.md:44` (ORNA-LIB-004) | served by ORNA-NATIVE-009/010 |
| `source/09-standard-library.md:46` (ORNA-LIB-005) | unchanged; a native operation's failure uses the ordinary failure channel |
| `source/10-execution.md:33` (ORNA-TXN-008) | served: the clause supplies the effect information the host otherwise infers |
| `source/14-pages.md:76` (ORNA-EVAL-004) | served: a body-less declaration cannot have its effects derived from a call graph |
| `source/03-source-modules.md:9` (ORNA-SOURCE-002) | unchanged: a native declaration is a top-level declaration |
| `source/03-source-modules.md:36` (ORNA-NS-007) | governs: ORNA-NATIVE-004 and -012 keep a replaced `std` ordinary |
| `source/04-lexical.md:29` (ORNA-LEX-007) | unchanged: ORNA-NATIVE-002 adds no keyword |

## 6. Grammar delta

Changes one production and adds four. Everything else in
`grammar/orna.ebnf` and in the grammar chapter of the specification is
unchanged, including the `contextual_name` production and the keyword set.

```ebnf
top_level_item    = use_declaration
                  | native_declaration
                  | table_declaration
                  | function_declaration
                  | protocol_declaration
                  | enum_declaration
                  | dimension_declaration
                  | unit_declaration
                  | type_declaration
                  | assertion_statement
                  ;

(* A native declaration states that the pinned dependency supplies the
   implementation. It has no body, so every type is written out. *)
native_declaration = visibility , "native" , "fn" , identifier ,
                     [ generic_parameters ] ,
                     "(" , [ native_parameters ] , ")" ,
                     ":" , type_expression ,
                     "effect" , native_effect_set , ";" ;
native_parameters  = native_parameter , { "," , native_parameter } , [ "," ] ;
native_parameter   = identifier , ":" , type_expression , [ "=" , expression ] ;
native_effect_set  = "none" | effect_class , { "," , effect_class } ;
effect_class       = identifier ;
```

`visibility`, `generic_parameters`, `type_expression` and
`function_parameters` are used as they are defined today. `native_parameter`
is `function_parameter` without its `self` alternative, written as its own
production so that the restriction ORNA-NATIVE-001 states is visible in the
grammar. `effect_class` is an identifier syntactically; which identifiers are
admissible is the semantic requirement of ORNA-NATIVE-009.

The two published copies of the grammar (`grammar/orna.ebnf` and the grammar
chapter of the specification) must receive this delta together. They currently
disagree about their own version in their header comments, which is recorded
here as a defect of the frozen release and is not caused by this amendment.
