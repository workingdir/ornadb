# ADR 0115: Single-source editor syntax artifacts

**Status:** Superseded by the `orna-syntax-v1` editor generator

This record describes the retired pre-1.0 editor pipeline. The current
artifacts are generated from `orna-syntax-v1`; see [Editor syntax support](../editor-tooling.md).

## Decision

Use `orna-syntax` as the source for syntax vocabulary and token presentation.
The `orna-syntax::grammar` module owns shared delimiters, operators,
punctuation, language metadata, semantic-token order, and the mapping from each
`HighlightKind` to its TextMate scope and optional semantic-token type. The
parser-backed classifier remains authoritative for contextual token roles.

Generate the TextMate grammar, VS Code language metadata, Tree-sitter keyword
rules and captures, and Vim, Emacs, and Sublime lexical packages from this
metadata. The structural Tree-sitter productions live in a template under
`orna-syntax`; its keyword inventory is injected from the same shared source
as the fallback grammars. Check in every generated file and require the
generator's check mode to match them byte-for-byte. `orna-lsp` must use the
shared table for its legend and token indices rather than maintain a duplicate
mapping.

Static editor highlighting is a lexical fallback. Since these packages cannot
use the Rust parser's CST context, ordinary identifiers receive generic scopes
where their formats do not support Tree-sitter captures; clients that support
LSP semantic tokens receive the classifier's contextual type, function,
property, namespace, and variable roles. The Tree-sitter query adds contextual
captures over its own syntax tree.

## Context and pragmatic choices

The frozen `/home/pbox/dev/ornadb/reference/Orna-1.0.0` reference recommends
syntax highlighting while typing in `source/17-repl.md:35`, and defines source
syntax in `source/34-grammar.md`. It does not specify editor artifact formats,
TextMate scopes, LSP semantic-token names or order, or package metadata. These
names, scopes, output paths, and the generic TextMate identifier scope are
therefore implementation choices; they do not claim normative editor
semantics. The earlier qualified-name capture question recorded in work ADR
0102 remains deferred where its exact capture contract is concerned.

The generated files are maintained with:

```text
just editor-artifacts
just editor-artifacts-check
```

Focused fixture tests live inside `orna-syntax` and load `.orna` inputs with
`include_str!`. No editor test reads from the reference checkout. The drift
check compares all package files and verifies Tree-sitter grammar JavaScript
syntax with Node; it does not launch editor hosts.

## Consequences

Changing accepted keyword, scalar-type, delimiter, operator, or punctuation
metadata produces one consistent update for the generated fallback grammar
and editor configuration. Changing token presentation updates the LSP legend
and generated mapping together. Contextual highlighting remains implemented
once in the syntax classifier, with semantic-token capable clients consuming
that result through `orna-lsp`.
