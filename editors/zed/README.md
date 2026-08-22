# Orna for Zed

Minimal Zed integration for the Orna language: language detection, comment
tokens, bracket matching, tree-sitter highlighting and the `orna-lsp`
language server.

There are two ways to use it:

1. **Manual settings.json** (no build step).
2. **The bundled extension** (recommended for a reusable setup).

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
        "arguments": []
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

The bundled extension registers the `orna-lsp` language server and the
`tree-sitter-orna` grammar. Zed builds the small Rust shim as WebAssembly;
the shim resolves `orna-lsp` from the worktree shell `PATH`. The server
speaks LSP over stdio and takes no command-line arguments.

Extension layout:

```text
editors/zed/
├── extension.toml                              # manifest + registrations
├── Cargo.toml                                  # Zed WebAssembly shim
├── src/
│   └── lib.rs                                  # PATH-based LSP command
└── languages/
    └── orna/
        ├── config.toml                         # name, grammar, suffixes, comments, brackets
        ├── highlights.scm                      # tree-sitter highlight query
        └── language_servers/
            └── orna_lsp/
                └── config.toml                 # legacy per-language server definition
```

The grammar registration points at the repository root with
`path = "editors/tree-sitter-orna"` and a fixed Git revision, so the
extension uses the existing grammar asset without inventing a release process.
The `language_ids` mapping sends Zed's `Orna` language name as the LSP `orna`
language id expected by the server.

To load the extension during development, use the Extensions panel's
"Install Dev Extension" action and select `editors/zed/`. Check the log
(`zed: open log`) if the server does not start. The `orna-lsp` binary must
be on the worktree shell `PATH` (build it with
`cargo build -p orna-lsp --release`).
