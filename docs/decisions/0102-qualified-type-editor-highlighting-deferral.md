# ADR 0102: Qualified Type Editor Highlighting Deferral

**Status:** Deferred pending restored editor tooling and an accepted capture contract

## Decision

Do not add or maintain a tree-sitter capture rule for lowercase qualified type
references while the editor and tree-sitter tooling is absent from `origin/main`.
The current task's intended distinction between type references and namespace
members is not fully specified by the Orna 1.0.0 reference. Do not infer a
capture policy from capitalization or parser shape alone.

Reopen this slice after editor tooling is restored and its capture expectations
are accepted. At that point, implement the accepted behavior with a real
`.orna` highlight fixture and focused query evidence.

## Context

The issue requests type highlighting for lowercase qualified names such as
`product_test.probe` in type positions while preserving namespace-member
highlighting elsewhere. The earlier implementation was integrated as commit
`de29eed8c` for GitHub issue #326. Current `origin/main` later removed the
top-level editor and tree-sitter tooling in `d0dbd7a59`; the current tree has no
tree-sitter grammar, highlight query, accepted highlight fixture, or editor
tooling checker to update.

## Reference evidence

The frozen `/home/pbox/dev/ornadb/reference/Orna-1.0.0` was searched for
`highlight`, `qualified_name`, `type_expression`, `type_spec`, and
`namespace`, including the normative document and source chapters 04, 05, 17,
and 34.

* `source/17-repl.md:31-41` says the REPL SHOULD provide syntax highlighting
  while typing. It does not define type-versus-namespace token captures or
  lowercase qualified-name behavior. No ORNA-* requirement ID was found for
  that classification.
* `source/34-grammar.md:38-40` defines `qualified_name` and
  `qualified_variant`; lines 116-130 define type expressions and allow a
  `qualified_name` as `type_primary`. These clauses define syntax, not editor
  capture names or namespace-member precedence.
* `source/04-lexical.md:27` (`ORNA-LEX-006`) requires case-sensitive
  namespace and definition-name comparison. Line 33 (`ORNA-LEX-008`) defines
  qualified enum variants in patterns. Neither requirement defines editor
  token classification. Line 142 says qualified names can construct nominal
  types or enum payloads; that is source resolution behavior, not highlighting.
* `source/05-types.md` defines type semantics. A targeted search of the chapter
  found no editor highlighting, qualified-name capture, or namespace
  classification rule.

The reference therefore supports qualified names in type positions and
recommends syntax highlighting in the REPL, but does not define the exact
editor classification requested by this issue. The missing current editor
target independently prevents a safe implementation on `origin/main`.

## Reopening condition

Reopen when the editor/tree-sitter assets return to `origin/main` and an
accepted contract specifies how type-position qualified names are captured
without changing namespace-member classification. Do not treat this deferral
as a claim that the removed editor tooling is currently supported.
