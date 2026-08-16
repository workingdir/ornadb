# Helix Orna support

Helix configuration for the Orna language: language detection, comment
tokens, tree-sitter highlighting and the `orna-lsp` language server.

## Installation

1. Copy `languages.toml` to your Helix config directory:

   ```bash
   cp editors/helix/languages.toml ~/.config/helix/languages.toml
   ```

2. The grammar is vendored in this repository at
   `editors/tree-sitter-orna`. Point the `[[grammar]]` `source.path` at your
   checkout. Relative paths resolve from the directory containing
   `languages.toml` (the Helix config directory); absolute paths always
   work. For example, if the OrnaDB workspace is at `~/dev/ornadb/work`:

   ```toml
   [[grammar]]
   name = "orna"
   source = { path = "/home/kieran/dev/ornadb/work/editors/tree-sitter-orna" }
   ```

3. Build the grammar:

   ```bash
   hx --grammar build
   ```

4. Make sure `orna-lsp` is on `$PATH` (build it with
   `cargo build -p orna-lsp --release` from the workspace root).

## Layout

- `[[language]]` — registers the `orna` language: `source.orna` scope,
  `.orna` file extension, comment tokens (`--`, `/*`, `*/`) and the
  `orna-lsp` language server.
- `[[language-server]]` — declares the `orna-lsp` server binary.
- `[[grammar]]` — registers the vendored `tree-sitter-orna` grammar. A
  published git source can replace the local path once the grammar is
  released (see the commented-out block in `languages.toml`).

## Notes

- Helix needs a working grammar build toolchain (`cc`) when running
  `hx --grammar build` from source; the prebuilt grammar approach also works
  for distributions that ship grammars.
- Highlight queries for the grammar are defined inside
  `editors/tree-sitter-orna` (standard captures: `@keyword`, `@type`,
  `@function`, `@variable`, `@property`, `@string`, `@number`, `@comment`,
  `@operator`, `@punctuation`, `@namespace`, `@parameter`).
