# tree-sitter-orna

A [tree-sitter](https://tree-sitter.github.io/tree-sitter/) grammar for the
Orna language — the DDL/DML/procedural language of the OrnaDB database.

The grammar provides syntax highlighting (and a concrete syntax tree) for
Neovim, Helix, Zed, Emacs and other tree-sitter-based editors. It is written
from `spec/spec/orna.ebnf` and is validated against executable sources in
`crates/` and `stdlib/`. Proposal-only examples that use deferred language
surfaces are excluded from the executable tooling gate.


## Building

The generated parser (`src/`) is committed, so most editors can use the
grammar as-is. To regenerate it after editing `grammar.js`:

```sh
tree-sitter generate
```

This requires the `tree-sitter-cli` (see `package.json`).

## Testing

```sh
npm install          # installs tree-sitter-cli
tree-sitter generate # rebuild the parser if grammar.js changed
tree-sitter test     # run the corpus tests in test/corpus/
```

To check that a file parses without error:

```sh
tree-sitter parse path/to/file.orna
```

The grammar is expected to produce zero `ERROR` nodes on every executable
`.orna` file under `crates/` and `stdlib/`. The proposal-only UI examples
`03_client_ui.orna`, `04_studio_shell.orna`, and `05_security_admin.orna` use
deferred language surfaces and are excluded from this gate.


## Editor setup

### Neovim

With `nvim-treesitter`, add the grammar directory to the runtime path and
install it:

```lua
vim.opt.runtimepath:append('/path/to/editors/tree-sitter-orna')
require('nvim-treesitter.configs').setup {
  ensure_installed = { 'orna' },
  highlight = { enable = true },
}
```

The language name is `orna` and the scope is `source.orna`.

### Helix

Helix 23.10+ loads grammars from the `runtime/grammars` directory and queries
from `runtime/queries`. Add the grammar and query files there (or use a
language configuration that points at this directory):

```
runtime/grammars/orna.so          # built from this grammar
runtime/queries/orna/highlights.scm
```

with the following entry in `languages.toml`:

```toml
[[language]]
name = "orna"
scope = "source.orna"
file-types = ["orna"]
comment-tokens = ["--"]
```

### Zed

Zed discovers tree-sitter grammars from its `languages` directory. Add a
`languages/orna` folder containing `grammar.json`, `parser.wasm` (built from
this grammar) and `highlights.scm`, with a matching language declaration:

```json
{
  "name": "Orna",
  "grammar": "orna",
  "scope": "source.orna",
  "path_suffixes": ["orna"],
  "line_comments": ["--"]
}
```

## Grammar notes

- Keywords are case-insensitive; identifiers are case-sensitive.
- `RETURNS ROWS (...)` and `RETURNS TABLE (...)` are both accepted.
- `EXPORT TYPE ... AS ... [TO PRELUDE AS ...]` is supported, including
  multi-word prelude names such as `CHARACTER LARGE OBJECT`.
- SERVER function bodies can be `AS` expressions, `AS` SQL statements, or
  `IS ... BEGIN ... END` procedural blocks with `LET`/`CONST`/`STATE`
  declarations and `IF`/`WHILE`/`FOR` statements.
- Accepted CLIENT bodies use the closed expression and state/procedural subsets
  covered by the CLIENT corpus tests; deferred UI proposal syntax is not part
  of the executable grammar contract.
- Keywords are usable as name components (`std.types.DATE`, `filter.SET`),
  and a few keywords (`security`, `rows`) also appear as name-initial
  components in real code; other keywords are reserved in name-initial
  position so that statement terminators such as `END` cannot be mistaken for
  expressions.
