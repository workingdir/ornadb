# Fallback target highlight disposition (#379)

## Accepted behavior

Issue #379 asks the Tree-sitter and Zed fallback queries to classify a resource/action target's final `client_field_path` member as a function capture, while ordinary and intermediate path members remain properties. It also excludes GUI/VSIX and Neovim/Vim runtime claims.

## Frozen reference boundary

The reference says the REPL SHOULD provide syntax highlighting while typing and matching-brace highlighting (`source/17-repl.md:33–39`). **ORNA-FN-002** (`source/06-expressions.md:63`) says a qualified function name without call parentheses denotes a function value and does not invoke it. Neither defines a Tree-sitter/Zed capture class, a target-final-member query rule, or property/function capture precedence.

The captured exact search at `docs/conformance/fallback-target-highlight-deferral-search-379-2026-09-28.log` found only those general highlighting mentions; the search for `client_field_path`, `FunctionName`, `@property`, Tree-sitter and Zed in reference source/grammar/tests/examples returned no matches (exit 1). No additional capture semantics are inferred from the general syntax-highlighting statement.

## Live implementation state

Live `origin/main` contains the earlier checker parity commit `125ce896a2bdf855d7234e5485a07fd95504e7af`. The exact child task `ornadb-1787968157049-281-a3974243.1` is closed and records that target-final function captures and ordinary/intermediate property behavior were already present at `221a5247`; its recorded proof is checker suite 16/16 and a focused LSP target-completion pass, with no files changed.

Subsequent main commit `d0dbd7a590b5059d0aa652f1366335708aff4466` removed `editors/tree-sitter-orna`, `editors/zed`, their highlight corpus and `scripts/check-editor-tooling.py` plus its tests. These files are absent from current main, leaving no live fallback query or corresponding Cargo test target for this accepted slice.

## Disposition

Record this slice as a deferral: preserve the historical issue evidence, but do not recreate removed editor tooling or invent additional capture rules without a current implementation surface and a normative capture contract. **Cargo tests were not run** because the current main tree has no Tree-sitter/Zed fallback or checker test files to execute. No GUI, VSIX, Neovim or Vim runtime proof is claimed.
