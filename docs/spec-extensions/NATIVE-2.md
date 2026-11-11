# NATIVE-2: native types, native statics, doc comments and interface clauses

Amendment to the Orna 1.1.0 specification (`final-2026-10-05`), following
[NATIVE-1](NATIVE-1.md).
Status: proposed. Design rationale: sys-as-native.md (sections 2.4 and 5, stage SN1, on PR #9888
branch docs/sys-as-native), and [`docs/design/native-declarations.md`](../design/native-declarations.md).
Decision record: D19 and ADR 0123.

This document adds two declaration forms, doc comments and a set of interface clauses.
Native types, native statics and interface clauses are legal only in engine-embedded
interface modules. Doc comments are legal in every module. The amendment adds no
keyword, changes no lexical rule of ORNA-LEX-007, and changes no existing production
of NATIVE-1.

## 1. Scope of the amendment

Added: the grammar productions of section 4, the normative clauses of section 2, and
the diagnostic of section 3.

Not added: lowering of the `spelling` clause into an index, and lowering of the `key`
and `availability` clauses into relation metadata. Those are a later stage of the
sys-as-native migration. This amendment defines only that the parser accepts the
clauses and that lowering carries them on the definition they annotate.

## 2. Normative clauses

**ORNA-NATIVE-016** A native type declaration has the form

```orna
pub native type Name<T>;
```

It MUST NOT have a body. It declares an opaque nominal type that the engine supplies.
Its type parameters are optional. It is legal only in an engine-embedded interface
module; elsewhere it is reported under `ORNA091-E-NATIVE-PLACE`, as NATIVE-1 section 3
states for a misplaced native declaration.

**ORNA-NATIVE-017** A native static declaration has the form

```orna
pub native static name: Type effect set;
```

It MUST NOT have a body and MUST state a type. It declares a singleton value that the
engine provides. It MUST state its `effect` clause as ORNA-NATIVE-009 requires for a
native function. It is legal only in an engine-embedded interface module, under the
same diagnostic as ORNA-NATIVE-016.

**ORNA-NATIVE-018** A doc comment is a line that begins with exactly three slashes,
`///`. A line that begins with four or more slashes is an ordinary comment. A doc
comment documents the declaration that follows it. The text after `///` is kept as
written, without one leading space. The compiler MUST keep the doc lines with the
declaration in the HIR. It MUST NOT interpret them, and it MUST NOT reject them for
their content. Doc comments are legal in every module.

**ORNA-NATIVE-019** An interface clause is a contextual word after a declaration's
head and before its `;` or `{`. The clause words are `class`, `fails`, `since`,
`removed`, `formerly`, `deprecated`, `spelling`, `key` and `availability`. None of
them is a keyword, so each remains a valid name, as NATIVE-1 states of `effect`.

**ORNA-NATIVE-020** Each clause MUST appear only on the declaration forms that the
table in this section names. A clause on any other form is a parse error.

| Clause | Form | Meaning |
|---|---|---|
| `class read\|invoke\|admin` | `native fn` | the catalogue class of the call |
| `fails a, b` | `native fn` | the failure codes the call may raise, as enum names |
| `since "version"` | `native fn` | the first language version that has the declaration |
| `removed "version" [use "path"] [reason "text"]` | any declaration | gone from `version` |
| `formerly "name" [diagnostic "code"]` | `native fn`, `native static` | the name that this declaration replaced |
| `deprecated "version" [use "path"]` | any declaration | still works; tools warn |
| `spelling "label"` | `native fn` | the 1.1 label that the path cannot give |
| `key a, b` | row `type` | the key columns of a relation row type |
| `availability "word"` | row `type` | one of the five availability words of a relation row type |

**ORNA-NATIVE-021** A clause string is a double-quoted literal. A name list is one or
more names separated by commas. A clause word that is not in the table of ORNA-NATIVE-020
for its form MUST be reported as an error.

**ORNA-NATIVE-022** Interface clauses are legal only in an engine-embedded interface
module. A user module that writes an interface clause MUST be rejected with the
diagnostic of section 3.

**ORNA-NATIVE-023** Lowering MUST copy each declaration's doc lines and clauses onto
the definition it annotates. Lowering MUST NOT drop a clause and MUST NOT change a
clause's text. A definition with no doc lines and no clause carries an empty doc block
and an empty clause set.

## 3. Diagnostics added

| Code | Condition |
|---|---|
| `ORNA091-E-INTERFACE-CLAUSE` | an interface clause is written in a user module, or on a form that the table of ORNA-NATIVE-020 does not allow for it |

## 4. Grammar delta

The following productions extend NATIVE-1 section 6. Every other production is unchanged.

```ebnf
native_declaration   = native_function | native_type | native_static ;
native_type          = "pub" , "native" , "type" , identifier , [ type_parameters ] , { interface_clause } , ";" ;
native_static        = "pub" , "native" , "static" , identifier , ":" , type , effect_clause , { interface_clause } , ";" ;
native_function      = /* NATIVE-1 section 6 */ , { interface_clause } ;
interface_clause     = clause_word , clause_argument ;
clause_word          = "class" | "fails" | "since" | "removed" | "formerly"
                     | "deprecated" | "spelling" | "key" | "availability" ;
doc_line             = "///" , line_text , newline ;
```

`static` is already a keyword, through its use in `impl`. `type`, `native` and `pub`
are unchanged. The clause words are contextual, as ORNA-NATIVE-019 states.

## 5. Conformance

A conforming parser:

1. accepts each form and clause of section 2 in an engine-embedded module, and keeps
   the clause values on the parsed declaration;
2. rejects each interface clause written in a user module with `ORNA091-E-INTERFACE-CLAUSE`;
3. rejects a clause written on a form that its table does not name;
4. rejects a native type or native static in a user module with `ORNA091-E-NATIVE-PLACE`;
5. keeps every doc line as written, does not interpret it, and does not reject it for its content;
6. treats every clause word as a name outside a clause position.

A conforming lowering carries each doc line and clause onto its definition (ORNA-NATIVE-023).
