# ADR 0115: Single-source editor syntax artifacts

**Status:** Accepted for generated lexical and semantic-token metadata

## Decision

Use `orna-syntax-v1` as the source for the Orna 1.0.0 syntax vocabulary and
token presentation. `Keyword::ALL`, the v1 lexer, and the editor presentation
table own keyword spellings, token classes, semantic-token order, and mapping
to TextMate scopes and optional semantic-token types.

Generate the TextMate grammar, VS Code language metadata, Tree-sitter grammar
and captures, and Vim, Emacs, and Sublime lexical packages from this metadata.
The generator and its Tree-sitter productions live in `orna-syntax-v1`. Check
in every generated file and require the generator's check mode to match them
byte-for-byte. `orna-lsp` uses the shared v1 table for its legend and token
indices rather than maintaining a duplicate mapping.

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

Focused fixture tests live inside `orna-syntax-v1` and load `.orna` inputs with
`include_str!`. No editor test reads from the reference checkout. The drift
check compares all generated package files and verifies Tree-sitter grammar
JavaScript syntax with Node; it does not launch editor hosts. A deterministic
grep guard rejects pre-1.0 syntax tokens in editor and packaging trees.

## Consequences

Changing accepted keyword, delimiter, operator, or punctuation metadata
produces one consistent update for the generated fallback grammar and editor
configuration. Changing token presentation updates the LSP legend and
generated mapping together. Contextual highlighting is implemented once in
the v1 syntax classifier, with semantic-token capable clients consuming that
result through `orna-lsp`.
