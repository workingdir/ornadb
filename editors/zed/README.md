# Orna for Zed

Minimal Zed integration for the Orna language: language detection, comment
tokens, bracket matching, tree-sitter highlighting and the `orna-lsp`
language server.

There are two ways to use it:

1. **Manual settings.json** (recommended, works today, no build step).
2. **The bundled extension** (requires a Rust shim for the LSP command in
   current Zed, see below).

## 1. Manual settings.json

Add this to `~/.config/zed/settings.json` (adjust the `tree_sitter_path` to
your checkout of the OrnaDB workspace):

```json
{
  "languages": {
    "Orna": {
      "path_suffixes": ["orna"],
      "tree_sitter_path": "/path/to/ornadb/work/editors/tree-sitter-orna"
    }
  },
  "lsp": {
    "orna-lsp": {
      "binary": {
        "path": "orna-lsp",
        "arguments": ["--stdio"]
      }
    }
  }
}
```

- `languages.Orna.path_suffixes` associates `.orna` files with the language.
- `languages.Orna.tree_sitter_path` points Zed at the vendored grammar in
  `editors/tree-sitter-orna` for syntax highlighting. The exact key name can
  vary slightly between Zed versions; check Zed's "languages" settings
  documentation if it is not accepted.
- `lsp.orna-lsp.binary` starts the language server. `orna-lsp` must be on
  `$PATH` (build it with `cargo build -p orna-lsp --release` from the
  workspace root), or give an absolute path.

## 2. Bundled extension

Extension layout:

```text
editors/zed/
├── extension.toml                              # manifest + server registration
└── languages/
    └── orna/
        ├── config.toml                         # name, grammar, suffixes, comments, brackets
        ├── highlights.scm                      # tree-sitter highlight query
        └── language_servers/
            └── orna_lsp/
                └── config.toml                 # legacy per-language server definition
```

- `extension.toml` registers the language server under
  `[language_servers.orna-lsp]` with `languages = ["Orna"]` (matching the
  `name` in `languages/orna/config.toml`).
- The tree-sitter grammar is vendored at
  `editors/tree-sitter-orna`; it is not bundled. Once the grammar is
  published, add a `[grammars.orna]` entry to `extension.toml` (see the
  comment there).
- Current Zed resolves the server executable from the extension's
  `src/lib.rs` (a Rust shim that returns the `orna-lsp` command) or from
  the `lsp.orna-lsp.binary.path` settings override. Until a Rust shim is
  added, keep the settings override above in place — it always takes
  precedence and needs no build.

To load the extension during development, use the Extensions panel's
"Install Dev Extension" action and select `editors/zed/`. Check the log
(`zed: open log`) if the server does not start.
