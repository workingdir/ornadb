# TextMate grammar and host coverage: bounded deferral for #40

This records the accepted TextMate/editor-host slice of Beads `ornadb-1787784779726-40-30adabd4` (GitHub #40), checked on `origin/main` at `6b958d06299859a97473fe43a29acb23068787ef`.

## Result

No TextMate parity or editor-host coverage claim can be made from the current main tree. The implementation assets and their checker were removed by `d0dbd7a590b5059d0aa652f1366335708aff4466` (`Remove obsolete top-level directories`): this includes `editors/textmate/orna.tmLanguage.json`, `editors/vscode/syntaxes/orna.tmLanguage.json`, `scripts/check-editor-tooling.py`, and `scripts/test_check_editor_tooling.py`. Each is absent at the checked main SHA. Consequently there is no current TextMate grammar pair or parity assertion to run or extend. Recreating these deleted assets would exceed this evidence slice and has no editor-specific normative contract to guide it.

The frozen reference does define language requirements that an editor grammar could represent: `ORNA-LEX-007` gives the exact keyword set and `ORNA-LEX-009` specifies indivisible longest-match operators (`source/04-lexical.md:29,35`); `ORNA-PARSE-001` and `ORNA-PARSE-002` require equivalent parse outcomes for the specified constructs (`source/06-expressions.md:235,267`). The grammar chapter identifies `grammar/orna.ebnf` as the distributed syntactic grammar and names its `module_unit`, `row_unit`, and `repl_input` entry points (`source/34-grammar.md:1-9`; `README.md:30-32`). The reference search found no TextMate, Vim, Neovim, editor-host, or highlight-capture requirement. These language rules do not define TextMate scopes, capture names, editor packaging, or host launch behavior.

No editor executable (`tree-sitter`, `zed`, `vim`, `nvim`, `code`, `subl`, or `emacs`) is available in this environment. Host runtime checks are therefore unavailable, and no host pass is claimed. The broader issue's Vim/Neovim and Tree-sitter/Zed coverage remains outside this component-isolated result.

## Bounded proof

The repository's Rust parser suite was run as a language-level check only; it does not prove TextMate parity or host integration. The exact CLI transcript and exit code are in `textmate-editor-host-coverage-40-2026-09-28.log`.

```text
cargo test --locked --offline -p orna-syntax-v1
EXIT_CODE=0
```

The suite passed 104 Rust tests (including authoritative valid/invalid parser fixture corpus checks). No editor-specific tests ran because their assets and checker are absent from current main.

## Search and checkout evidence

The audit searched the frozen reference tree for `TextMate`, `Vim`, `Neovim`, `editor host`, `host editor`, `highlight capture`, and `editor grammar`; there were no matches. A separate search found only the lexical requirements and parser clauses listed above. Current-main path checks found the TextMate grammar, VS Code grammar copy, editor-tooling checker/tests, Tree-sitter grammar, and Zed integration absent. The host executable probe reported all seven tools unavailable. Full captured parser output follows in the adjacent log.
