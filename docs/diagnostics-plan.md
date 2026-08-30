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

`CompilerDiagnostic` contains a stable code, raw message, severity, primary
label, optional help, notes, related source labels, logical path, and exclusive
UTF-8 byte span. `ParseReport` retains the exact source text in each
`ParsedSourceUnit`. Error and warning counts are available on every compiler
report.

The server exposes two diagnostic renderings:

- the established machine format, which preserves byte spans and escaped
  messages; and
- a Rust-style human format with severity-aware colour, line/column locations,
  bounded single- and multiline source context, labelled underlines, related
  locations, help, notes, and an error/warning summary.

Source commands keep the machine format for non-terminal output. Human output
is selected for terminal diagnostics without changing the machine contract.
Warnings are printed but do not block checking, preparation, apply, or diff.
The LSP publishes the same severity, code, range, help, notes, related
locations, primary label, and warning tags. The raw-call protocol is not a
compiler-diagnostic transport and is unchanged.

## Implementation phases

### Phase 1: source-aware presentation layer (implemented)

The renderer derives, without rereading files:

- one-based line and column for display
- bounded source context across single- and multiline spans
- labelled primary and related markers
- explicit EOF handling
- a fallback byte location when the diagnostic path is absent from the report

The implementation keeps the original byte offsets and raw message unchanged.
It uses `ParsedSourceUnit::source_text()` and avoids `syntax_text()`, which
allocates a new string.

Focused regression coverage includes ASCII, Unicode, CRLF, tabs, control
characters, long lines, multiline context, EOF, related definitions, colour,
summaries, warnings, and the unchanged machine-format contract.

### Phase 2: structured diagnostic metadata (implemented)

Compiler diagnostics carry:

- severity
- primary label
- help text
- notes
- related source labels

Rendered snippets and ANSI sequences remain presentation-layer data and never
enter compiler messages.

`DiagnosticCode` is the central wording catalogue for parser and semantic
diagnostics. Each entry defines its code, severity, short title, stable
explanation, and optional default help. Individual diagnostics add precise
expected/found wording and source labels when the compiler has that evidence.
Wording remains direct and does not mention parser state, implementation gaps,
or internal phase names.

### Phase 3: human terminal formatter (implemented)

Source commands expose a human format that follows the useful parts of rustc
and database CLIs:

```
error[ORNA0103]: duplicate schema definition app
  --> broken.orna:2:15
   |
 2 | CREATE SCHEMA app;
   |               ^^^ redefined here
  ::: broken.orna:1:15
   |
 1 | CREATE SCHEMA app;
   |               --- first defined here
   |
   = help: rename one of the definitions or remove the duplicate

error: aborting due to 1 previous error
```

The implementation:

- selects human output for terminal diagnostics and preserves machine output
  for the non-terminal default
- follows `--color auto|always|never`
- emits no colour in machine output
- keeps diagnostics on stderr and successful result data on stdout
- retains compiler order for multiple diagnostics
- prints warning diagnostics while returning success when no error exists
- emits plural-aware error and warning summaries

The compatibility rule is terminal-sensitive rather than a new `--format`
flag: existing machine output remains available for scripts, while terminal
users receive the human presentation.

### Phase 4: semantic precision and intelligent suggestions (in progress)

Completed:

- duplicate-definition diagnostics label both the redefinition and the first
  definition
- unreachable procedural statements emit non-blocking `ORNA0401` warnings
- compiler severity and related locations are preserved by the LSP

Remaining improvements must be made at their source, not in the formatter:

- replace remaining whole-function spans with the offending keyword,
  expression, or return type
- use catalogue data for unknown names and suggest the closest valid name only
  when the match is unambiguous
- distinguish remaining domain restrictions from type mismatches
- suppress cascaded delimiter errors after an unterminated token
- merge lexer and parser diagnostics by source span before exposing source order
- make an unknown syntax-code mapping a controlled internal diagnostic instead
  of silently collapsing it to `ORNA0001`

Each improvement needs a focused regression test and must preserve LSP raw
message/range parity unless the public contract changes intentionally.

## Non-goals for the first implementation slice

- no miette, ariadne, or other renderer dependency until the output contract is proven
- no compiler protocol changes
- no rewrite of the resolver
- no changes to runtime invocation failure messages
- no change to the machine-readable one-line source-check output
- no speculative suggestions based on fuzzy or incomplete catalogue data

## Validation

For each phase:

- run the focused compiler, server, and LSP tests for the changed contract
- run source-check integration coverage for error status `1` and warning-only
  status `0`
- prove warning-only preparation, apply, and diff remain successful
- run `cargo fmt --all -- --check`
- run workspace checks and Clippy with warnings denied
- inspect terminal output with colour disabled and enabled while preserving the
  redirected machine format

ADR 0018 and editor-tooling document the current compatibility rules. Update
them, and any source/apply/diff documentation added later, whenever a remaining
phase changes the diagnostic format.
