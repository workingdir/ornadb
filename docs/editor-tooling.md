# Editor syntax support

`orna-syntax` is the source for editor keyword, scalar-type, delimiter,
operator, punctuation, and language metadata. The generated editor packages
and LSP metadata describe the Orna 1.1.0 vocabulary and record that version as
`languageVersion`.

The checked-in editor files are generated from this metadata:

| Artifact | Purpose |
|---|---|
| `editors/textmate/orna.tmLanguage.json` | Generic TextMate fallback grammar |
| `editors/semantic-token-legend.json` | LSP token order and editor mapping |
| `editors/tree-sitter-orna/grammar.js` | Generated Tree-sitter grammar |
| `editors/tree-sitter-orna/queries/highlights.scm` | Generated Tree-sitter token captures |
| `editors/tree-sitter-orna/{tree-sitter.json,package.json}` | Tree-sitter package registration and release metadata |
| `editors/vscode/extension.js` | Plain-JavaScript `orna-lsp` language client |
| `editors/vscode/{package.json,language-configuration.json}` | `.orna` association, grammar, language configuration, and client setting |
| `editors/vscode/syntaxes/orna.tmLanguage.json` | Same generated grammar for VS Code |
| `editors/neovim/lua/orna/init.lua` | Native `.orna` LSP attachment |
| `editors/vim/` | Vim syntax, filetype detection, and `vim-lsp` registration |
| `editors/emacs/orna-eglot.el` | Emacs font-lock mode and Eglot attachment |
| `editors/sublime/Orna.sublime-syntax` | Sublime Text lexical scopes |
| `editors/sublime/LSP-orna.sublime-settings` | Sublime Text LSP client settings |

Regenerate the files after changing syntax metadata, and check for drift with:

```bash
just editor-artifacts
just editor-artifacts-check
```

The check compares every checked-in artifact byte-for-byte with the Rust
renderer, syntax-checks the generated JavaScript — the Tree-sitter grammar and
the VS Code extension — with `node --check`, and drives the generated extension
through its launch contract (`scripts/check_vscode_extension.js`): the default
`orna-lsp` argv, an override split into executable and arguments, an empty
override meaning the default, a missing command reported once without failing
activation, and a setting change restarting the client. With
`npm install` in `editors/vscode`, `node scripts/check_vscode_extension.js
--real` also constructs a client with the installed `vscode-languageclient`,
which proves the generated launch options are the shape that version expects.
Focused tests in `orna-syntax` load `.orna` fixtures from that crate with
`include_str!`, validate the generated metadata, check for v1-only lexical
classes, and prove byte stability and complete editor-tree coverage. The
coverage test fails for a file under `editors/` that the generator does not
produce, for an editor directory that holds no generated artifact, and for a
listed artifact whose checked-in bytes differ from the renderer's output, so an
editor surface cannot be added without joining the emitter. `npm install` in
`editors/vscode` writes `node_modules` and `package-lock.json`, which the
coverage test skips as editor build output rather than governed artifacts.

TextMate, Vim, Emacs, and Sublime grammars recognize lexical patterns but do
not have parser context for distinguishing declared types, functions,
properties, namespaces, and variables. Every generated client launches
`orna-lsp` (see below), so those hosts receive diagnostics, navigation,
completion, and semantic tokens from the server; a host that runs only the
grammar receives colouring and no server features. Static editor grammars do
not claim parser-equivalent context.

## One server command

`orna-lsp` is the product: one binary that speaks the Language Server Protocol
over standard input and output and is the only place language behaviour lives.
Every editor integration is a thin client that launches it — no editor package
carries its own parser, its own diagnostics, or its own completion logic, and
the syntax definitions only colour text until the server attaches.

The start contract is therefore one command with one default and one override
per editor:

| Editor | Generated client | The one override | Default |
|---|---|---|---|
| VS Code | `editors/vscode/extension.js` | `orna.lsp.serverCommand` | `["orna-lsp"]` |
| Sublime Text | `editors/sublime/LSP-orna.sublime-settings` | `command` | `["orna-lsp"]` |
| Vim | `editors/vim/plugin/orna-lsp.vim` | `g:orna_lsp_command` | `['orna-lsp']` |
| Neovim | `editors/neovim/lua/orna/init.lua` | `options.cmd` | `{ "orna-lsp" }` |
| Emacs | `editors/emacs/orna-eglot.el` | `orna-eglot-server-command` | `("orna-lsp")` |

The semantics are identical in every row: the override is an **argv list**
whose first element is the executable and whose remaining elements are its
arguments, so a server outside `PATH` is spelled as a one-element list such as
`["/opt/orna/bin/orna-lsp"]`. An unset or empty override means `orna-lsp`,
resolved through `PATH`. No client passes a transport argument, because stdio
is the only transport the server supports. The generator renders the default
from one constant and the drift gate checks each client's override name and the
default against the table above, so a client cannot invent a second variable or
a different command.

When the command is missing — `orna-lsp` is not on `PATH` and no override names
it — an editor reports the failure and keeps the generated syntax definition
working; no client silently degrades into a second, editor-local language
implementation. VS Code shows an error notification naming the command and the
setting to fix, and leaves the window open: activation succeeds, `.orna` files
stay highlighted by the contributed grammar, and the server-backed features
return when the setting is corrected (which restarts the client) or `orna-lsp`
is put on `PATH` and the window is reloaded. A client that cannot reach the
server is a configuration error, not a reason to fail the editor.


## Attach the language server

VS Code loads `editors/vscode` as an extension: the manifest declares
`extension.js` as its entry point, activates on the `orna` language, and starts
the server through `vscode-languageclient`. Install the extension directory as a
development extension (`code --extensionDevelopmentPath=editors/vscode`) after
`npm install` in that directory, which installs the client library; there is no
build step, because the entry point is plain JavaScript that runs from the
tree. Diagnostics appear in the Problems panel, hover, definition, references,
rename and semantic tokens come from the attached server, and the setting
`orna.lsp.serverCommand` overrides the command. Changing that setting restarts
the client; there is no second setting and no editor-local fallback.

Sublime Text uses the [`LSP`](https://lsp.sublimetext.io/) package. Copy the
generated `LSP-orna.sublime-settings` into `Packages/User` (Preferences >
Browse Packages...) with the generated syntax definition, and the package
registers the `orna-lsp` client for `source.orna`; no separate server
configuration is needed. `command` is the only override, and the client
attaches to saved files.

Neovim uses its native LSP client. Add `editors/neovim` to `runtimepath` and
call `require("orna").setup()`; pass `cmd = { "/path/to/orna-lsp" }` when the
server is not on `PATH`. The setup registers `.orna` filetype detection and
attaches those buffers to `orna-lsp`. A completion plugin can use the attached
client's LSP completion provider. The native client also exposes server hover
through `vim.lsp.buf.hover()`, rename through `vim.lsp.buf.rename()`, references
through `vim.lsp.buf.references()`, and syntax-v1 semantic tokens.

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
Eglot provides rename through `eglot-rename` and references through its xref
backend.

The `orna-lsp` integration suite exercises hover, rename, references, and six
lexical semantic-token classes through Neovim's attached client. Emacs/Eglot
proves hover, rename, and references through its attached server, plus
semantic-token fontification when that Eglot feature is available. It also has
optional Vim/`vim-lsp` completion probes. Host probes report `SKIP` when an
editor, client runtime, or optional semantic-token support is unavailable.

Generated artifact checks do not launch editor hosts. Tree-sitter generation
is structurally checked by Node, and the grammar/query package is drift-checked
as generated text. The `orna-lsp` integration tests separately launch supported
editor hosts when their client dependencies are available.

## Language server behaviour

Inside an Orna project (a Git worktree whose root holds `main.orna`),
`orna-lsp` checks the open buffers exactly as `orna check` checks the saved
files and publishes the same diagnostics. Names resolve through the project's
`use` imports, so definition, references, rename, call and type hierarchies
and code lenses follow the module graph rather than matching names as text.
Rename edits only the occurrences that resolve to the renamed declaration, and
it is refused with a reason (`RequestFailed`) when an occurrence of the name
cannot be resolved, when the new name is not a valid identifier, or when
`orna check` would report a new diagnostic afterwards.

Go to definition on a standard-library name opens a read-only copy of the
standard source the analysis used: the project's captured `stdlib/std`
snapshot, or for documents outside a project the reference library the server
ships. The copies live below `$XDG_CACHE_HOME/orna/std/<digest>/` (by default
`~/.cache/orna/std`), one directory per source set. A project without a
captured standard library has no standard definitions, as `orna check`
rejects its standard imports.

Each document version is parsed and indexed once and shared by every request.
Requests run on worker threads, so a slow request does not delay edits or
other requests, and `$/cancelRequest` answers a cancelled request with
`RequestCancelled`. Semantic tokens classify names by what they resolve to:
modules are `namespace`, fields `property`, enum variants `enumMember`, and
names from the standard library or the intrinsic type environment carry the
`defaultLibrary` modifier.
