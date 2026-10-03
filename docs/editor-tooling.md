# Editor syntax support

`orna-syntax-v1` owns the 1.0.0 lexical vocabulary used by editor tooling.
`Keyword::ALL`, the lexer token classes, and the editor presentation table are
the single source for static grammars and the LSP semantic-token legend.

The checked-in editor files are generated from this metadata:

| Artifact | Purpose |
|---|---|
| `editors/textmate/orna.tmLanguage.json` | Generic TextMate fallback grammar |
| `editors/vscode/syntaxes/orna.tmLanguage.json` | Same generated grammar for VS Code |
| `editors/vscode/package.json` | `.orna` association and grammar registration |
| `editors/vscode/language-configuration.json` | Comments, brackets, and quote pairs |
| `editors/semantic-token-legend.json` | LSP token order and classifier-to-editor mapping |
| `editors/tree-sitter-orna/grammar.js` | Parser grammar with keyword tokens derived from `Keyword::ALL` |
| `editors/tree-sitter-orna/queries/highlights.scm` | Tree-sitter token captures for the v1 lexical classes |
| `editors/tree-sitter-orna/{tree-sitter.json,package.json}` | Tree-sitter package registration and release metadata |
| `editors/generated-artifacts.json` | Complete manifest of the files owned by the generator |
| `editors/vim/` | Vim syntax groups and `.orna` file detection |
| `editors/emacs/orna-eglot.el` | Emacs font-lock mode and Eglot setup |
| `editors/sublime/Orna.sublime-syntax` | Sublime Text lexical scopes |

Regenerate the files after changing syntax metadata, and check for drift with:

```bash
just editor-artifacts
just editor-artifacts-check
```

`just check` includes the drift check and a grep-based guard for legacy syntax
tokens in editor and packaging trees. The drift check compares every
generated artifact byte-for-byte with the Rust renderer and runs Node's parser
check on the generated Tree-sitter grammar. Focused `orna-syntax-v1` proof
tests load `.orna` sources from `crates/orna-syntax-v1/tests/fixtures/` with
`include_str!` and prove that the lexer and generated editor grammars match
ORNA-LEX-007.

The Tree-sitter parser shape and all lexical editor renderers live in
`orna-syntax-v1`. Keyword rules come from `Keyword::ALL`; captures and semantic
token presentation come from the same v1 lexer classes used by `orna-lsp`.

TextMate, Vim, Emacs, and Sublime fallback grammars recognize lexical patterns
but do not have the Rust parser's context for distinguishing a declared type,
function, property, namespace, or variable. These fallback grammars therefore
scope ordinary identifiers generically; clients with semantic-token support
receive contextual classifications from `orna-syntax-v1` through `orna-lsp`.
This is a pragmatic editor mapping because the frozen Orna 1.0.0 reference
defines source syntax, but does not prescribe editor scopes, semantic-token
names, or package metadata. This work does not claim that a static grammar
provides parser-equivalent context.

The generated VS Code package contributes syntax association only. Configure
`orna-lsp` separately in the editor to receive diagnostics, navigation, and
semantic tokens. Focused checks do not launch editor hosts; Tree-sitter
generation is structurally checked by Node, while the grammar/query package is
drift-checked as generated text.
