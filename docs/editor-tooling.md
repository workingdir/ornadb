# Editor syntax support

`orna-syntax-v1` is the source for editor keyword, scalar-type, delimiter,
operator, punctuation, and language metadata. The generated editor packages
and LSP metadata use the frozen Orna 1.0 vocabulary.

The checked-in editor files are generated from this metadata:

| Artifact | Purpose |
|---|---|
| `editors/textmate/orna.tmLanguage.json` | Generic TextMate fallback grammar |
| `editors/vscode/syntaxes/orna.tmLanguage.json` | Same generated grammar for VS Code |
| `editors/vscode/package.json` | `.orna` association and grammar registration |
| `editors/vscode/language-configuration.json` | Comments, brackets, and quote pairs |
| `editors/semantic-token-legend.json` | LSP token order and editor mapping |
| `editors/tree-sitter-orna/grammar.js` | Generated Tree-sitter grammar |
| `editors/tree-sitter-orna/queries/highlights.scm` | Generated Tree-sitter token captures |
| `editors/tree-sitter-orna/{tree-sitter.json,package.json}` | Tree-sitter package registration and release metadata |
| `editors/vim/` | Vim syntax groups and `.orna` file detection |
| `editors/emacs/orna-eglot.el` | Emacs font-lock mode and Eglot setup |
| `editors/sublime/Orna.sublime-syntax` | Sublime Text lexical scopes |

Regenerate the files after changing syntax metadata, and check for drift with:

```bash
just editor-artifacts
just editor-artifacts-check
```

The check compares every checked-in artifact byte-for-byte with the Rust
renderer and runs Node's parser check on the generated Tree-sitter grammar.
Focused tests in `orna-syntax-v1` load `.orna` fixtures from that crate with
`include_str!`, validate the generated metadata, check for v1-only lexical
classes, and prove byte stability and complete editor-tree coverage.

TextMate, Vim, Emacs, and Sublime fallback grammars recognize lexical patterns
but do not have parser context for distinguishing declared types, functions,
properties, namespaces, and variables. Configure `orna-lsp` separately to
receive diagnostics, navigation, completion, and semantic tokens. Static
editor grammars do not claim parser-equivalent context.

Focused checks do not launch editor hosts. Tree-sitter generation is
structurally checked by Node, and the grammar/query package is drift-checked as
generated text.
