# Editor tooling for `.orna` source

OrnaDB ships three layers of editor support for `.orna` files:

1. **`orna-lsp`** — a language server binary that works with any LSP client
  that supports UTF-16 position encoding. It reuses the offline Orna compiler,
  so it needs no running database and never writes to disk.
2. **`tree-sitter-orna`** — a tree-sitter grammar with corpus tests and
   standard highlight captures, consumed natively by Neovim, Helix, and Zed.
   Emacs uses the package's font-lock fallback; its Eglot integration does
   not wire tree-sitter.
3. **A TextMate grammar** plus small per-editor integration packages,
   for editors that use static grammars (VS Code, Sublime Text).

## Fresh-checkout prerequisites and evidence

Run the static gate from the repository root after the locked Cargo cache has
been provisioned and the embedded-engine prerequisite is available:

```bash
cargo fetch --locked
cargo fetch --locked --manifest-path editors/zed/Cargo.toml
CARGO_NET_OFFLINE=true just editor-tooling-check
```

The two `cargo fetch --locked` commands are the networked dependency bootstrap
for the root workspace and separate `editors/zed` workspace. The editor gate
requires Python 3.11 or newer (for TOML validation), `tree-sitter-cli` 0.26.5,
Node, Cargo, Git, and the checked-in `editors/` tree. Its Cargo checks and
tests use `--locked --offline`; the source-check parity phase also invokes
`orna-server`, whose embedded-engine build requires a Linux x86_64 host plus
either `ORNA_POSTGRES_ENGINE_OUTPUT` naming a complete prebuilt engine output
directory with an **absolute** path (for example,
`$PWD/target/postgresql-embedded-native-one/output` after the native lifecycle
recipe has produced it) or the environment-dependent Docker-backed build.
Cargo offline mode does not disable that build script's host/network work.
The script generates Tree-sitter output in a temporary directory and does not
rewrite the checkout.

The canonical `spec` bundle is not part of this checkout: both `./spec/` and
the sibling `../spec/` are absent. The static gate can still validate the
checked-in accepted corpus and logs `../spec/examples` as skipped when that
optional proposal/deferred directory is absent. Neither that skip nor a
successful static gate proves parity with the missing canonical bundle.

`emacs` is optional. If `emacs` and its Eglot package are available, the gate
batch-loads `editors/emacs/orna-eglot.el`; otherwise it records the Emacs
runtime check as unavailable while still requiring the checked-in integration
file. Neovim, Vim, Helix, Zed, VS Code, and Sublime host processes are not
started by this gate.

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
- Signature help (`textDocument/signatureHelp`) for declared server and client
  functions and recognized standard-library functions for calls whose names use
  ASCII identifier/name matching, including parameter details, the active
  parameter, and the return type.
- Workspace symbols (`workspace/symbol`) across all open documents, filtered by
  a substring query on the symbol name, with ASCII letters matched
  case-insensitively.
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

The paths in this section refer to the external canonical bundle. This
checkout currently has neither `./spec/` nor `../spec/`; work ADRs in
`docs/decisions/` are not a substitute for that source authority. Do not turn
an absent canonical input into a parity claim.

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
cargo build --locked --offline -p orna-lsp --release
```

The binary is `target/release/orna-lsp`. Install it somewhere on
`PATH`, or point your editor at the path directly.

Run the protocol tests:

```bash
cargo test --locked --offline -p orna-lsp
```

For the complete static gate, run `just editor-tooling-check`. A zero exit
status is evidence for the checked-in JSON, grammar generation, accepted
corpus, parser/LSP contracts, Zed extension, fallback parity, and VS Code
syntax only. The script reports optional Emacs runtime unavailability and
skips absent `../spec/examples`; those messages are not failures, but they
must remain in the evidence log. Any missing required file, missing CLI
prerequisite, Cargo cache miss, parser error, or non-zero subprocess is a gate
failure and must not be relabelled as a skip.

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
code --install-extension orna-vscode-1.0.0.vsix
```

Packaging is a separate network/editor-host gate: `npx` may download
`@vscode/vsce`, and `code --install-extension` requires a VS Code host. The
expected artifact is `editors/vscode/orna-vscode-1.0.0.vsix`; this checkout has
no such artifact until the packaging command succeeds. Neither packaging nor
installation is executed by `just editor-tooling-check`. Record a missing npm
network, `npx`, or VS Code host as unavailable, not as static editor evidence.

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

## Gate result and host-runtime boundary

Capture the static result when it is used as release evidence:

```bash
mkdir -p ci-evidence
set -o pipefail
just editor-tooling-check 2>&1 | tee ci-evidence/editor-tooling.log
```

The static gate passes only when it exits zero. It validates checked-in
metadata, generated grammar bytes, accepted/deferred manifest classification,
accepted parser and LSP tests, fallback grammar parity, the Zed extension's
locked offline Cargo check, and the VS Code JavaScript syntax with
`node --check`. It does not launch an editor, open a buffer, install an
extension, or prove host-session behavior. A missing required CLI prerequisite
or file, an offline Cargo cache miss, parser error, or non-zero child process
is a failure; do not relabel it as an expected skip.

The only expected static-gate skip is an absent optional `../spec/examples`
directory, which is logged as proposal/deferred input. Emacs runtime loading
is separately **unavailable** when `emacs` or Eglot is absent, while the
checked-in Emacs source remains required. Neovim/Vim/Helix/Zed/VS Code/Sublime
launches are host gates outside this command and must be recorded separately
with the host version and configuration if performed.

## Writing documentation for hovers

Attach `DOCUMENTATION '...'` clauses to object and value types, fields,
and parameters. The language server renders the text in hovers:

```sql
CREATE TYPE tasks.task AS OBJECT (
    title TEXT NOT NULL DOCUMENTATION 'the task title'
) DOCUMENTATION 'a durable task';
```

The clauses are implemented by the lossless parser; the referenced canonical
grammar is proposal text and is absent from this checkout. They never change
runtime behaviour.

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
