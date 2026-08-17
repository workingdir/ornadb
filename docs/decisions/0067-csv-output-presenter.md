# ADR 0067: `std.csv.encode` — the Sealed CSV Output Presenter

**Status:** Accepted

## Decision

`--output csv` becomes a sealed output presenter for relational results,
closing the roadmap M6 remainder ("`std.json` and CSV encoders" — json is
built, CSV is the gap). The presenter renders a bounded `ResultRows` as a
`text/csv` byte stream through one sealed `std.csv.encode` function identity,
exactly mirroring the ADR 0057 json and terminal-table presenter pattern.

## Surface

```text
orna invoke <function> --output csv
orna invoke <function> --output text/csv
```

`csv` is an alias resolving to the media type `text/csv` in the sealed
presenter registry. The rendered bytes are one CSV document on stdout;
diagnostics and progress never interleave with it.

## Sealed registration

One new sealed standard function identity in the function-id space:

```text
std.csv.encode            FunctionId          ...13
std.csv.encode.p_rows     ParameterId         ...13
std.csv.encode revision   FunctionRevisionId  ...13
```

Byte `0x13` (19) is unused in the standard model's FunctionId space (echo
`...10`, json.encode `...11`, present_table `...12`) and is disjoint from the
sealed `sys.security.*` block (`...40`-`...4b`). Like `std.json.encode` and
`std.terminal.present_table`, the CSV function is a sealed in-process
identity: it is not a retained stdlib source unit, so no `orna.std/5`
snapshot upgrade is needed. The compiler model gains the three constants and
the sealed route constructs the definition and revision in-process.

## Engine

The engine accepts the bounded `ResultRows` the sealed route builds from the
canonical result (the same `sealed_result_rows` step the terminal-table
presenter uses) and renders RFC-4180-style CSV:

- one header row of column names;
- one row per result row;
- cells rendered with the closed terminal-cell rules (scalars, text, bytes,
  references, enums, records), then CSV-escaped: a cell containing a comma,
  double quote, CR, or LF is quoted, and embedded quotes are doubled;
- CRLF line endings are not used; the document uses LF (the writer's final
  newline matches the other presenters);
- a trailing newline terminates the document.

The rendered bytes are framed as a `std.io.ByteStream` opaque value with
media type `text/csv`, exactly like `std.json.encode` frames
`application/json`.

## Proof

The live proof (`proves_output_through_orna_invoke_against_postgres`) gains a
CSV case: an echo invocation with `--output csv` renders the canonical value
as a one-column, one-row CSV document with the exact `text/csv` byte-stream
media type, on stdout only. The registry unit test for the previously
rejected `csv` alias is updated to assert resolution.

## Consequences

- `crates/orna-compiler/src/resolver/model.rs` gains the three sealed CSV
  identities (no new dependencies).
- `crates/orna-artifact/src/server_csv_encode.rs` mirrors
  `server_json_encode.rs`: the canonical `orna.server-csv-encode` version 1
  artifact.
- `crates/orna-postgres/src/kernel/server_execution.rs` gains the sealed
  `std.csv.encode` definition/revision, the CSV rendering engine, and the
  `csv` entry in the sealed presenter registry.
- No owned-path changes; `Cargo.lock` untouched; the standard snapshot and
  migration set are unchanged (the presenter is sealed, like ADR 0057).
- Deferred (documented, not invented): streaming CSV for `STREAM<T>`
  results, a `std/csv.orna` source unit, and typed column schemas beyond the
  bounded `ResultRows` set.