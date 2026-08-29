# Compiler diagnostics plan

## Goal

Give Orna compiler failures the same practical quality as Rust, SQLite, Turso, PostgreSQL, and Cargo:

- point at the exact source location
- show the relevant source line
- explain what the compiler expected or found
- provide a useful next action when the compiler can state one
- keep wording stable, direct, and free of internal implementation terms
- keep machine-readable output separate from human terminal output

## Current boundary

`CompilerDiagnostic` contains a stable code, a raw message, a logical path, and an exclusive UTF-8 byte span. `ParseReport` retains the exact source text in each `ParsedSourceUnit`. The server currently discards that text before rendering and emits one escaped line:

```
path:start..end: ORNA0001: message
```

The LSP maps the same raw message and byte span to an LSP diagnostic. The raw-call protocol is not a compiler-diagnostic transport and should not be changed for this work.

The current one-line output is an established contract in ADR 0018 and in the source-check, editor-tooling, and LSP tests. A human formatter must therefore be additive or explicitly versioned. It must not silently change the machine contract.

## Proposed phases

### Phase 1: source-aware presentation layer

Add a renderer-facing source context type. It should derive, without rereading files:

- one-based line and column for display
- the complete source line containing the start of the span
- a UTF-8-safe underline range
- a bounded set of adjacent lines for multi-line spans
- an explicit EOF marker for zero-width end-of-file diagnostics

Keep the original byte offsets and raw message unchanged. Use `ParsedSourceUnit::source_text()` and avoid `syntax_text()`, which allocates a new string.

Add focused tests for:

- ASCII source
- CRLF source
- multibyte and combining characters
- tabs
- zero-width EOF spans
- multiline spans
- a diagnostic whose path is not found in the supplied report

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

### Phase 3: human terminal formatter

Add an explicit human format for source commands. The formatter should follow the useful parts of rustc and database CLIs:

```
error[ORNA0001]: expected a schema name after CREATE SCHEMA
  --> broken.orna:1:15
   |
 1 | CREATE SCHEMA ;
   |               ^ schema name is missing
   |
   = help: write a schema name before the semicolon
```

Rules:

- plain output remains safe when stderr is not a terminal
- colour follows `--color auto|always|never`
- no colour appears in machine output
- diagnostics remain on stderr; successful result data remains on stdout
- multiple diagnostics retain compiler order
- long lines and multiline spans are bounded and readable
- control characters in source excerpts are escaped or displayed safely

Possible command shape: retain the current machine output as the default for compatibility and add `--format human|machine`, or make human output the terminal default while requiring `--format machine` for scripts. This choice needs an ADR update and migration tests.

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
- no silent change to existing one-line source-check output
- no speculative suggestions based on fuzzy or incomplete catalogue data

## Validation

For each phase:

- run the focused syntax/compiler/server tests for the changed contract
- run the source-check integration tests
- run the LSP parity tests when raw diagnostic fields change
- run `cargo fmt --all -- --check`
- run the relevant package check
- inspect plain, colour-disabled, and colour-enabled output manually

Before completion, update ADR 0018 and the source/apply/diff/editor-tooling documentation to describe the chosen formats and compatibility rules.
