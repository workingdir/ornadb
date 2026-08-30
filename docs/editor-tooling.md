# Editor tooling for `.orna` source

OrnaDB ships three layers of editor support for `.orna` files:

1. **`orna-lsp`** — a language server binary that works with any LSP
   client. It reuses the offline Orna compiler, so it needs no running
   database and never writes to disk.
2. **`tree-sitter-orna`** — a tree-sitter grammar with corpus tests and
   standard highlight captures, consumed natively by Neovim, Helix, and Zed.
   Emacs uses the package's font-lock fallback; its Eglot integration does
   not wire tree-sitter.
3. **A TextMate grammar** plus small per-editor integration packages,
   for editors that use static grammars (VS Code, Sublime Text).

## Features

The language server provides:

- Compiler diagnostics from the same checker as `orna source check`, with
  stable `ORNAxxxx` codes, raw messages, and byte spans. Both push
  (`textDocument/publishDiagnostics`) and pull (`textDocument/diagnostic`) modes
  are supported.
- Semantic highlighting through the standard LSP semantic-token API:
  keywords, types, functions, namespaces, properties, strings, numbers,
  comments, and operators.
- Document symbols (schemas, types, functions, fields, parameters).
- Hover for declared names and standard scalar types.
- Rich hover documentation: full signatures, per-parameter and
  per-field detail with modifiers, `DOCUMENTATION` clause text, usage
  examples, and a link to the grammar specification when the spec
  bundle is reachable from the document.
- Keyword hovers: every language keyword has a reference entry with a
  summary, grammar context, and a source example.
- Go-to-definition and find-references within the open document.
- Completion for keywords, scalar types, standard-library types, and
  names declared in the document.

Highlighting degrades gracefully. LSP clients that support semantic
tokens get the best result; tree-sitter editors use the grammar;
everything else can fall back to the TextMate grammar.

## Grammar and evidence boundary

The canonical EBNF (`spec/spec/orna.ebnf`) and the grammar/AST discussion
in `spec/docs/28-ebnf-ast.md` are **current proposal** text.
`spec/docs/25-source-compiler-ir.md` and `spec/docs/39-testing.md` combine
locked requirements with current proposal material. Together, these sources
describe intended language shape and testing strategy; they do not establish
editor, compiler, or runtime parity for every form shown there. In particular,
broader SQL/TABLE forms that appear only in proposal or deferred grammar are
not proof of an accepted surface.

Keep the evidence classes separate:

- **Static editor evidence** is the explicit accepted corpus named by
  `editors/tree-sitter-orna/test/accepted-corpus.txt`. The companion
  `test/deferred-corpus.txt`, the full corpus, and canonical `spec/examples/`
  files are proposal/deferred material, not accepted-contract evidence. A
  full-corpus `tree-sitter test` pass therefore does not prove full EBNF
  parity; this includes `RETURNS TABLE` or other broader SQL examples unless
  the accepted manifest names them.
- **Runtime/contract evidence** is limited to the accepted Rust parser/compiler
  and `orna-lsp` contracts and their Rust tests. This is offline implementation
  evidence, not host-editor launch evidence or proof of graphical/runtime ABI
  behavior.

## Building the language server

```bash
cargo build -p orna-lsp --release
```

The binary is `target/release/orna-lsp`. Install it somewhere on
`PATH`, or point your editor at the path directly.

Run the protocol tests:

```bash
cargo test -p orna-lsp
```

A manual smoke test of the framed lifecycle (the helper computes each JSON body's exact UTF-8 byte length and checks that the binary exits successfully):

```bash
python3 - <<'PY'
import json
import subprocess

messages = [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"processId": None, "rootUri": None, "capabilities": {}}},
    {"jsonrpc": "2.0", "method": "initialized", "params": {}},
    {"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": None},
    {"jsonrpc": "2.0", "method": "exit", "params": None},
]

def frame(message):
    body = json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return f"Content-Length: {len(body)}\r\n\r\n".encode("ascii") + body

wire = b"".join(frame(message) for message in messages)
process = subprocess.Popen(
    ["target/release/orna-lsp"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
)
process.communicate(wire)
if process.returncode != 0:
    raise SystemExit(f"orna-lsp exited with status {process.returncode}")
PY
```

## Layout

```text
crates/orna-lsp/              the language server crate
  src/documents.rs            document store, byte-to-position mapping
  src/analysis.rs             compiler checks, symbols, hover, navigation
  src/semantic.rs             semantic-token legend and encoding
  src/server.rs               stdio loop and request dispatch
  tests/lsp_e2e.rs            framed JSON-RPC end-to-end tests
editors/tree-sitter-orna/     tree-sitter grammar, corpus, highlight queries
editors/textmate/             canonical TextMate grammar
editors/vscode/               VS Code extension (plain JavaScript)
editors/neovim/               Neovim plugin (ftdetect + vim.lsp)
editors/vim/                  vim syntax and filetype detection
editors/helix/                Helix languages.toml
editors/zed/                  Zed extension
editors/emacs/                Emacs eglot integration
```

## Editor setup

### Visual Studio Code

Open the `editors/vscode/` folder in VS Code and run the extension from
the Run view, or package it:

```bash
cd editors/vscode
npx @vscode/vsce package
code --install-extension orna-vscode-0.1.0.vsix
```

The extension launches `orna-lsp` from `PATH`. To use a specific
binary, set `orna.lsp.path` in settings. The extension bundles the
TextMate grammar, so highlighting works even before the server starts.

### Neovim

Install the plugin from `editors/neovim/` (lazy.nvim example):

```lua
{
  dir = "/path/to/ornadb/work/editors/neovim",
  config = function()
    require("orna").setup({ cmd = { "orna-lsp" } })
  end,
}
```

The plugin registers `.orna` as the `orna` filetype and enables the
native LSP client. For tree-sitter highlighting, point nvim-treesitter
at the grammar:

```lua
require("nvim-treesitter.configs").setup({
  parser_install_dir = vim.fn.stdpath("data") .. "/treesitter",
})
-- then build it once:
-- :TSInstallFromGrammar orna /path/to/ornadb/work/editors/tree-sitter-orna
```

### Vim

Copy `editors/vim/syntax/orna.vim` and `editors/vim/ftdetect/orna.vim`
into your `~/.vim` tree, or use any package manager with the
`editors/vim` directory as a plugin root. For LSP features, point
vim-lsp or coc.nvim at `orna-lsp` using the snippet in the package
README.

### Helix

Merge `editors/helix/languages.toml` into your Helix config. It
registers the `orna` language, the `orna-lsp` language server, and the
`tree-sitter-orna` grammar. The README in that directory explains how
to point the grammar at the vendored `editors/tree-sitter-orna`
directory.

### Zed

Either install the extension from `editors/zed/` or use the manual
settings snippet in the README. Both register `orna-lsp` as the
language server and the tree-sitter grammar for syntax highlighting.

### Emacs

Use the `orna-eglot.el` package:

```elisp
(use-package orna-eglot
  :load-path "/path/to/ornadb/work/editors/emacs"
  :config (orna-setup-eglot))
```

The package registers `.orna` files with eglot and provides a
font-lock keyword set for buffers without the LSP running.

### Sublime Text

Sublime Text 4 supports tree-sitter grammars directly, or can use the
canonical TextMate grammar:

1. Copy `editors/textmate/orna.tmLanguage.json` into
   `Packages/User/` (or a `.sublime-package` with scope
   `source.orna`).
2. Add an LSP client for the `source.orna` scope pointing at
   `orna-lsp` (for example via the LSP package).

### Any other LSP client

Point the client at the `orna-lsp` binary with language id `orna`,
file patterns `["**/*.orna"]`, and UTF-16 position encoding (the
default). The server needs no configuration and no workspace
initialization beyond the standard handshake.

## Writing documentation for hovers

Attach `DOCUMENTATION '...'` clauses to object and value types, fields,
and parameters. The language server renders the text in hovers:

```sql
CREATE TYPE tasks.task AS OBJECT (
    title TEXT NOT NULL DOCUMENTATION 'the task title'
) DOCUMENTATION 'a durable task';
```

The clauses are part of the grammar (`spec/spec/orna.ebnf`) and are
captured by the lossless parser; they never change runtime behaviour.

## Language reference for tooling authors

- Line comments: `--` to end of line. Block comments: `/* ... */`.
- Strings: single quotes with doubled-quote escape (`'it''s'`).
- Quoted identifiers: double quotes with doubled-quote escape.
- Keywords and type names are case-insensitive.
- The authoritative keyword and scalar-type lists live in
  `crates/orna-syntax/src/highlight.rs` (`KEYWORDS`, `SCALAR_TYPES`).
- `spec/spec/orna.ebnf` is the current proposal grammar; do not infer
  accepted parity from it. For accepted editor evidence, use
  `editors/tree-sitter-orna/test/accepted-corpus.txt` and the Rust parser/LSP
  contracts described above.
