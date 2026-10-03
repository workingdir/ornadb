# Editor syntax support

`orna-syntax` owns the source vocabulary used for syntax highlighting. Its
`grammar` module publishes comment/string delimiters, operators, punctuation,
keyword and scalar-type spellings, and one presentation entry for each
`HighlightKind`. The parser-backed classifier assigns contextual roles such as
function, property, namespace, and type; `orna-lsp` derives its semantic-token
legend and indices from that same presentation table.

The checked-in editor files are generated from this metadata:

| Artifact | Purpose |
|---|---|
| `editors/textmate/orna.tmLanguage.json` | Generic TextMate fallback grammar |
| `editors/vscode/syntaxes/orna.tmLanguage.json` | Same generated grammar for VS Code |
| `editors/vscode/package.json` | `.orna` association and grammar registration |
| `editors/vscode/language-configuration.json` | Comments, brackets, and quote pairs |
| `editors/semantic-token-legend.json` | LSP token order and classifier-to-editor mapping |
| `editors/tree-sitter-orna/grammar.js` | Parser grammar with keyword tokens derived from `KEYWORDS` and `SCALAR_TYPES` |
| `editors/tree-sitter-orna/queries/highlights.scm` | Tree-sitter token captures and contextual name roles |
| `editors/tree-sitter-orna/{tree-sitter.json,package.json}` | Tree-sitter package registration and release metadata |
| `editors/vim/` | Vim syntax groups and `.orna` file detection |
| `editors/emacs/orna-eglot.el` | Emacs font-lock mode and Eglot setup |
| `editors/sublime/Orna.sublime-syntax` | Sublime Text lexical scopes |

Regenerate the files after changing syntax metadata, and check for drift with:

```bash
just editor-artifacts
just editor-artifacts-check
```

`just check` includes the drift check. It compares every checked-in artifact
byte-for-byte with the Rust renderer and runs Node's parser check on the
generated Tree-sitter grammar. The focused `orna-syntax` proof tests load their
`.orna` source from `crates/orna-syntax/tests/fixtures/` with `include_str!`,
validate generated JSON manifests, and prove that Tree-sitter, Vim, Emacs,
Sublime, and TextMate contain the shared keyword and scalar-type inventory.

The Tree-sitter parser shape lives in a template inside `orna-syntax`; its
keyword token rules and highlight-query vocabulary are rendered from the same
`KEYWORDS` and `SCALAR_TYPES` tables used by the other editors. The query's
contextual captures use the `TOKEN_PRESENTATIONS` table for comments, literals,
names, operators, and punctuation.

TextMate, Vim, Emacs, and Sublime fallback grammars recognize lexical patterns
but do not have the Rust parser's context for distinguishing a declared type,
function, property, namespace, or variable. These fallback grammars therefore
scope ordinary identifiers generically; clients with semantic-token support
receive contextual classifications from `orna-syntax` through `orna-lsp`.
This is a pragmatic editor mapping because the frozen Orna 1.0.0 reference
defines source syntax, but does not prescribe editor scopes, semantic-token
names, or package metadata. This work does not claim that a static grammar
provides parser-equivalent context.

The generated VS Code package contributes syntax association only. Configure
`orna-lsp` separately in the editor to receive diagnostics, navigation, and
semantic tokens. Focused checks do not launch editor hosts; Tree-sitter
generation is structurally checked by Node, while the grammar/query package is
drift-checked as generated text.
