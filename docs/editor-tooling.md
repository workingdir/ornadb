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
| `editors/neovim/lua/orna/init.lua` | Native `.orna` LSP attachment |
| `editors/vim/` | Vim syntax, filetype detection, and optional `vim-lsp` completion |
| `editors/emacs/orna-eglot.el` | Emacs font-lock mode and Eglot attachment |
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

## Attach the language server

Neovim uses its native LSP client. Add `editors/neovim` to `runtimepath` and
call `require("orna").setup()`; pass `cmd = { "/path/to/orna-lsp" }` when the
server is not on `PATH`. The setup registers `.orna` filetype detection and
attaches those buffers to `orna-lsp`. A completion plugin can use the attached
client's LSP completion provider. The native client also exposes server hover
through `vim.lsp.buf.hover()` and requests syntax-v1 semantic tokens.

Vim uses the optional [`vim-lsp`](https://github.com/prabirshrestha/vim-lsp)
client. Add `editors/vim` and the `vim-lsp` plugin to `runtimepath`; the Orna
plugin registers `orna-lsp` for the `orna` filetype and sets
`omnifunc=lsp#complete`. Vim's omni completion (`Ctrl-X Ctrl-O` in insert mode)
then requests completion items from the attached server. Set
`g:orna_lsp_command` to a command list to use a non-default server path.

Emacs uses Eglot. Load `editors/emacs/orna-eglot.el` and call
`(orna-setup-eglot)` once; Orna buffers then attach through `eglot-ensure` and
use Emacs's `completion-at-point` interface. Set `orna-eglot-server-command`
to a command list before calling the setup function when `orna-lsp` is not on
`PATH`. Eglot provides hover documentation through ElDoc and applies semantic
token faces when its `eglot-semantic-tokens-mode` support is available.

The `orna-lsp` integration suite exercises hover and six lexical semantic
token classes through Neovim's attached client, and hover plus semantic-token
fontification through Emacs/Eglot. It also has optional Vim/`vim-lsp`
completion probes. Host probes report `SKIP` when an editor, client runtime,
or Eglot semantic-token support is unavailable.

Generated artifact checks do not launch editor hosts. Tree-sitter generation
is structurally checked by Node, and the grammar/query package is drift-checked
as generated text. The `orna-lsp` integration tests separately launch supported
editor hosts when their client dependencies are available.
