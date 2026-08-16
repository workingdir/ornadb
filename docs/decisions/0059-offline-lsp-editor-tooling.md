# 0059 — Offline LSP and Editor Tooling for `.orna` Source

**Status:** accepted

**Date:** 2026-08-16

## Problem

Editors and IDEs need syntax highlighting and language features for
`.orna` files. The `orna source check` command already proves exact
offline diagnostics, but no editor can run it per keystroke. The first
implementation must choose:

- one language server stack that every major editor can launch,
- one highlighting strategy that works in editors without an LSP, and
- one set of integration packages that can grow with the language.

## Decision

### The LSP server is a new `orna-lsp` binary

The server lives in `crates/orna-lsp/` and speaks the Language Server
Protocol over stdio. It uses `lsp-server` 0.10 and `lsp-types` 0.97,
the standard lightweight Rust LSP stack also used by rust-analyzer.

The server reuses the offline compiler directly:

- the retained standard library snapshot is verified once per process,
- each open document is checked with
  `orna_compiler::check_new_application`,
- diagnostics carry the stable `ORNAxxxx` codes and byte-exact spans.

It needs no running database, no network, and no filesystem writes.

### Highlighting is three layered mechanisms

1. **Semantic tokens via the LSP.** `orna-syntax` exposes a
   context-aware classifier (`Parse::highlight`) that walks the lossless
   CST and classifies every token: keywords, type names, function names,
   namespaces, properties, strings, numbers, comments, and operators.
   The server maps the classifier onto a fixed ten-entry semantic-token
   legend. Any LSP client that supports semantic tokens gets accurate
   highlighting with zero extra grammar files.
2. **A tree-sitter grammar.** `editors/tree-sitter-orna/` covers the
   current language surface with corpus tests and standard highlight
   captures. Neovim, Helix, Zed, and Emacs use it natively.
3. **A TextMate grammar.** `editors/textmate/orna.tmLanguage.json`
   covers VS Code and Sublime without any extension.

### Editor integrations are small, dependency-light packages

- `editors/vscode/` is a plain-JavaScript extension with no build step.
  It launches `orna-lsp` (configurable via `orna.lsp.path`), bundles the
  TextMate grammar, and declares language configuration.
- `editors/neovim/`, `editors/vim/`, `editors/helix/`, `editors/zed/`,
  and `editors/emacs/` provide native configs or plugins that wire the
  same `orna-lsp` binary and `tree-sitter-orna` grammar.

### The classifier stays in `orna-syntax`

The highlight API is part of the syntax crate, not the LSP crate, so any
future tool (documentation generators, diffs, CI screenshot checks) can
reuse it. The CST remains the single source of truth for what a token
is.

## Consequences

- Editors get full-fidelity diagnostics identical to `orna source check`.
- Highlighting degrades gracefully: semantic tokens first, tree-sitter
  next, TextMate last, plain fallback always.
- The LSP crate is a thin protocol layer over the compiler. It has no
  business logic that can drift from `orna source check`.
- Each editor integration is independent and can be removed or extended
  without touching the others.
- `lsp-types` 0.97 uses `Uri` (fluent-uri) rather than `url::Url`;
  position conversion must account for UTF-16 code units, which the
  `PositionMapper` in `orna-lsp` owns.

## Alternatives considered

- **tower-lsp.** Async and batteries-included, but pulls a large
  tokio/tower stack for a synchronous server. `lsp-server` keeps the
  binary small and the loop explicit.
- **Highlighting only from TextMate.** Simple, but every editor would
  need its own copy and the result would be static. Semantic tokens
  match the compiler's own understanding of the source.
- **A separate lexer in the LSP crate.** Avoids touching `orna-syntax`
  but duplicates token knowledge. The classifier lives beside the
  parser, where the CST is already private and tested.

## Verification

- `cargo test -p orna-lsp` drives the compiled binary through a framed
  JSON-RPC client and asserts the initialize handshake, pushed and pulled
  diagnostics, semantic tokens, document symbols, hover, definition,
  completion, and clean shutdown.
- `tree-sitter test` runs the grammar corpus; `tree-sitter parse`
  reports zero error nodes on every spec example and system-test
  fixture.
- All JSON grammar files are validated with `python3 -m json.tool`; the
  VS Code extension passes `node --check`.
