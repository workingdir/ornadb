# vim-orna

Classic Vim integration for the Orna language: syntax highlighting and
optional LSP support via [vim-lsp](https://github.com/prabirshrestha/vim-lsp)
or [coc.nvim](https://github.com/neoclide/coc.nvim). No plugin manager
required.

## Installation

### Without a plugin manager

Copy the files into your `~/.vim` directory:

```bash
cp -r syntax ftdetect ~/.vim/
```

### With a packpath

```bash
mkdir -p ~/.vim/pack/orna/start
git clone <this-repo> ~/.vim/pack/orna/start/vim-orna
```

Or, using a local checkout:

```bash
ln -s /path/to/ornadb/work/editors/vim ~/.vim/pack/orna/start/vim-orna
```

Vim will source `ftdetect/orna.vim` at startup and highlight `.orna` files
automatically. To enable the syntax file immediately, add
`syntax on` to your `~/.vimrc` (usually already present).

## Language server

The Orna language server binary is `orna-lsp`. Install it on `$PATH`
(e.g. `cargo build -p orna-lsp --release` from the workspace root) and
wire it into your LSP client of choice.

### vim-lsp

```vim
function! s:setup_orna_lsp() abort
  if !exists('g:loaded_vim_lsp')
    return
  endif
  call lsp#register_server({
        \ 'name': 'orna-lsp',
        \ 'cmd': {server_info -> ['orna-lsp']},
        \ 'allowlist': ['orna'],
        \ 'workspace_config': {},
        \ })
endfunction

augroup orna_lsp
  au!
  au FileType orna call s:setup_orna_lsp()
augroup END
```

### coc.nvim

Add to your `coc-settings.json`:

```json
{
  "languageserver": {
    "orna": {
      "command": "orna-lsp",
      "filetypes": ["orna"],
      "rootPatterns": [".git", "Cargo.toml"]
    }
  }
}
```

## Highlight groups

| Group                  | Linked to  | Matches                                |
| ---------------------- | ---------- | -------------------------------------- |
| `ornaStatement`        | `Statement`| Keywords (case-insensitive)            |
| `ornaBoolean`          | `Boolean`  | `TRUE`, `FALSE`, `NULL`                |
| `ornaType`             | `Type`     | Scalar types (`INT`, `TEXT`, ...)      |
| `ornaString`           | `String`   | `'...'` literals (`''` escape)         |
| `ornaQuotedIdentifier` | `Identifier` | `"..."` quoted identifiers (`""` escape) |
| `ornaComment`          | `Comment`  | `--` line and `/* ... */` block        |
| `ornaNumber`           | `Number`   | Integer, decimal, exponent literals    |
| `ornaOperator`         | `Operator` | `=`, `<>`, `::`, `||`, arithmetic      |
| `ornaFunction`         | `Function` | Identifiers followed by `(`            |
| `ornaIdentifier`       | `Identifier`| All other identifiers                |
