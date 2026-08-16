# orna.nvim

Neovim integration for the Orna language: filetype detection (`*.orna`) and a
native LSP client for the `orna-lsp` language server. Syntax highlighting is
provided by tree-sitter using the vendored grammar in
[`../tree-sitter-orna/`](../tree-sitter-orna/).

## Requirements

- Neovim >= 0.10 (0.11+ recommended; the modern `vim.lsp.config` path is
  used when available, with an automatic fallback for older versions).
- `orna-lsp` on `$PATH`. Build it from the workspace root with
  `cargo build -p orna-lsp --release`.

## Installation

### lazy.nvim

```lua
{
  dir = "/path/to/ornadb/work/editors/neovim",
  name = "orna",
  ft = "orna",
  opts = {}, -- e.g. { cmd = { "orna-lsp" } }
}
```

### packer.nvim

```lua
use {
  "/path/to/ornadb/work/editors/neovim",
  ft = "orna",
  config = function()
    require("orna").setup({ cmd = { "orna-lsp" } })
  end,
}
```

## Configuration

The plugin configures itself on load. To customise, set
`vim.g.orna_skip_auto_setup = true` before the plugin is sourced and call
`setup()` yourself:

```lua
vim.g.orna_skip_auto_setup = true
require("orna").setup({
  cmd = { "orna-lsp" }, -- or an absolute path to the binary
  settings = {
    -- Initialization options sent to orna-lsp (server-specific).
  },
})
```

## Tree-sitter

Build the vendored grammar and place the parser on the runtimepath so
nvim-treesitter picks it up:

```bash
cd editors/tree-sitter-orna
tree-sitter generate
tree-sitter build --output ~/.local/share/nvim/site/parser/orna.so
```

The grammar uses standard capture names (`@keyword`, `@type`, `@function`,
`@variable`, `@property`, `@string`, `@number`, `@comment`, `@operator`,
`@punctuation`, `@namespace`, `@parameter`), so `require("nvim-treesitter")
` highlighting works without extra configuration once the parser is
installed.

## Features

- Filetype detection for `.orna` files.
- LSP via `vim.lsp.*`: diagnostics, hover, go-to-definition, completion,
  and semantic tokens from `orna-lsp`.
- Project root detection (`.git` / `Cargo.toml` markers) with buffer
  directory fallback.
- Ready for tree-sitter highlighting with nvim-treesitter.
