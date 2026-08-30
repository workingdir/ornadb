# Compiler diagnostics plan

## Goal

Give Orna compiler failures the same practical quality as Rust, SQLite, Turso, PostgreSQL, and Cargo:

- point at the exact source location
- show the relevant source line
- explain what the compiler expected or found
- provide a useful next action when the compiler can state one
- keep wording stable, direct, and free of internal implementation terms
- keep machine-readable output separate from human terminal output

## Current implementation

`CompilerDiagnostic` contains a stable code, raw message, logical path, and
exclusive UTF-8 byte span. `ParseReport` retains the exact source text in each
`ParsedSourceUnit`.

The server exposes two diagnostic renderings:

- the established machine format, which preserves byte spans and escaped
  messages; and
- a Rust-style human format with line/column locations, source context,
  underlines, optional colour, and bounded help text.

Source commands keep the machine format for non-terminal output. Human output
is selected for terminal diagnostics without changing the machine contract.
The LSP maps the same raw message and byte span to an LSP diagnostic. The
raw-call protocol is not a compiler-diagnostic transport and is unchanged.

## Implementation phases

### Phase 1: source-aware presentation layer (partially implemented)

The initial renderer derives, without rereading files:

- one-based line and column for display
- the complete source line containing the start of the span
- a caret range for the source line
- a fallback location when the diagnostic path is absent from the report

The implementation keeps the original byte offsets and raw message unchanged.
It uses `ParsedSourceUnit::source_text()` and avoids `syntax_text()`, which
allocates a new string.

Focused regression coverage currently covers:

- an ASCII source diagnostic with line, column, source context, and help
- tabs
- the machine-format contract
- opt-in ANSI output

Bounded long-line rendering, explicit EOF handling, and readable multiline
spans remain open work.

### Phase 2: structured diagnostic metadata

Add optional metadata without forcing every existing diagnostic to change at once:

- severity
- primary label
- help text
- notes
- related source labels

Store this separately from the stable raw message until the wording catalogue is complete. Do not put rendered snippets or ANSI sequences in compiler messages.

Create a central wording catalogue for parser and semantic diagnostics. Each entry should define:

- code
- short title
- stable explanation
- optional help text
- examples of expected and found values where the compiler knows them

Use direct wording. Do not mention parser state, implementation gaps, or internal phase names.

### Phase 3: human terminal formatter (initial implementation)

The source commands expose an initial human format that follows the useful
parts of rustc and database CLIs:

```
error[ORNA0001]: expected a schema name after CREATE SCHEMA
  --> broken.orna:1:15
   |
 1 | CREATE SCHEMA ;
   |               ^ schema name is missing
   |
   = help: write a schema name before the semicolon
```

The current implementation:

- selects human output for terminal diagnostics and preserves machine output
  for the non-terminal default
- follows `--color auto|always|never` where coloured human output is exposed
- emits no colour in machine output
- keeps diagnostics on stderr and successful result data on stdout
- retains compiler order for multiple diagnostics

The compatibility rule is terminal-sensitive rather than a new `--format`
flag: existing machine output remains available for scripts, while terminal
users receive the human presentation. Bounded long-line and multiline
rendering, and safe control-character presentation, remain open work.

### Phase 4: semantic precision and intelligent suggestions

Improve diagnostics at their source, not in the formatter:

- replace whole-function spans with the offending keyword, expression, or return type
- add secondary labels for related declarations and conflicting definitions
- use catalogue data for unknown names and suggest the closest valid name only when the match is unambiguous
- distinguish domain restrictions from type mismatches
- suppress cascaded delimiter errors after an unterminated token
- merge lexer and parser diagnostics by source span before exposing source order
- replace the panic on unknown syntax diagnostic codes with a controlled internal error path

Each improvement needs a focused regression test and must preserve LSP raw message/range parity unless the public contract changes intentionally.

## Non-goals for the first implementation slice

- no miette, ariadne, or other renderer dependency until the output contract is proven
- no compiler protocol changes
- no rewrite of the resolver
- no changes to runtime invocation failure messages
- no change to the machine-readable one-line source-check output
- no speculative suggestions based on fuzzy or incomplete catalogue data

## Validation

For each phase:

- run the focused syntax/compiler/server tests for the changed contract
- run the source-check integration tests
- run the LSP parity tests when raw diagnostic fields change
- run `cargo fmt --all -- --check`
- run the relevant package check
- inspect plain, colour-disabled, and colour-enabled output manually

ADR 0018 and editor-tooling document the current compatibility rules. Update
them, and any source/apply/diff documentation added later, whenever a remaining
phase changes the diagnostic format.
