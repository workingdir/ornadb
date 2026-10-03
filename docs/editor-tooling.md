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

Regenerate the files after changing syntax metadata, and check for drift with:

```bash
just editor-artifacts
just editor-artifacts-check
```

`just check` includes the drift check. The focused `orna-syntax` proof tests
load their `.orna` source from `crates/orna-syntax/tests/fixtures/` with
`include_str!` and compare every generated JSON artifact byte-for-byte with the
renderer.

TextMate grammars recognize lexical patterns but do not have the parser's
context for distinguishing a declared type, function, property, namespace, or
variable. The fallback grammar therefore scopes ordinary identifiers
generically; clients with semantic-token support receive the contextual
classifications from `orna-syntax` through `orna-lsp`. This is a pragmatic
editor mapping because the frozen Orna 1.0.0 reference defines source syntax,
but does not prescribe TextMate scopes, semantic-token names, or editor package
metadata. This work does not claim that a static TextMate grammar provides
parser-equivalent context.

The generated VS Code package contributes syntax association only. Configure
`orna-lsp` separately in the editor to receive diagnostics, navigation, and
semantic tokens. No editor host is launched by the focused proof tests.
