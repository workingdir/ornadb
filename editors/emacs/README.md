# orna-eglot

Orna language support for Emacs: a major mode (`orna-mode`) with syntax
highlighting and Eglot integration with the `orna-lsp` language server.

## Requirements

- Emacs 28.1+ (29+ recommended for the built-in Eglot).
- `orna-lsp` on `$PATH` (build it with
  `cargo build -p orna-lsp --release` from the workspace root).
- The tree-sitter grammar in `editors/tree-sitter-orna` (used by LSP
  tooling; not required by this package itself).

## Installation (use-package)

```elisp
(use-package orna-eglot
  :load-path "/path/to/ornadb/work/editors/emacs" ; absolute path to this directory
  :mode ("\\.orna\\'" . orna-mode)
  :hook (orna-mode . eglot-ensure)
  :custom
  (orna-lsp-command "orna-lsp")       ; or an absolute path
  :config
  (orna-setup-eglot))
```

What each piece does:

- `:mode` activates `orna-mode` for `.orna` files.
- `orna-setup-eglot` adds `((orna-mode) . ("orna-lsp"))` to
  `eglot-server-programs`, telling Eglot how to start the server.
- `:hook (orna-mode . eglot-ensure)` starts Eglot (and the server) when an
  Orna buffer opens.

## Manual install

```elisp
(add-to-list 'load-path "/path/to/ornadb/work/editors/emacs")
(require 'orna-eglot)
(orna-setup-eglot)
(add-hook 'orna-mode-hook #'eglot-ensure)
```

## Customization

- `orna-lsp-command` (default `"orna-lsp"`): the language server command.
  Set it to an absolute path if the binary is not on `PATH`.

## Notes

- `orna-mode` font-lock is case-insensitive and mirrors the keyword and
  scalar type lists from `crates/orna-syntax/src/highlight.rs`.
- Strings (`'...'`, `''` escape) and quoted identifiers (`"..."`, `""`
  escape) are handled via the syntax table plus a syntax-propertize rule
  for doubled-quote escapes.
- Eglot derives the LSP language id `orna` from the mode name, matching the
  id expected by `orna-lsp`.
