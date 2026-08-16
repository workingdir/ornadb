# ADR 0057: Terminal Documents, JSON Output, and the TTY Runtime

**Status:** Accepted

## Decision

The first output pipeline for `orna invoke` is terminal documents and JSON
byte streams. This decision accepts:

1. `std.terminal.Document` as a standard-library transient opaque value type
   with a fixed canonical codec;
2. `std.io.ByteStream` as a standard-library transient opaque value type
   carrying bytes with a declared media type;
3. a presenter registry of ordinary standard-library objects
   (`std.present.Presenter`) with alias, function, input type pattern,
   output type, media type, streaming flag, and priority;
4. two initial presenters: `std.json.encode` (canonical values to a JSON
   byte stream) and `std.terminal.present_table` (relational rows to a
   terminal document);
5. `--output <alias|media-type|type-name>` resolution through the presenter
   registry in the sealed `sys.invoke` route;
6. `orna-runtime-tty`, the first runtime, which consumes
   `std.terminal.Document` and `std.io.ByteStream` and renders them on the
   local terminal.

This is the first milestone-6 slice. CSV, XML, lossless Orna JSON,
streaming JSON Lines, presenters that run on the CLIENT side, and graphical
runtimes are later ADRs. The target function stays unchanged: it returns its
canonical typed value, and presentation is a separate registered step.

## Value types and codecs

`std.terminal.Document` is a standard-library `VALUE OPAQUE IMMUTABLE
TRANSIENT` type following the `opaque_token` precedent (work ADR 0034). Its
canonical payload is the terminal document in a fixed UTF-8 text layout:

```text
ORNA-TERMINAL-DOCUMENT/1 <len:u32 be> <utf-8 bytes>
```

The layout is plain text with `\n` line separators and a final newline. It
carries no control codes. The document is produced by presenters and
consumed by `orna-runtime-tty`.

`std.io.ByteStream` is likewise `VALUE OPAQUE IMMUTABLE TRANSIENT` with the
canonical payload:

```text
ORNA-BYTE-STREAM/1 <media-type-len:u32 be> <media-type> <len:u32 be> <bytes>
```

Both types receive stable identities in the standard manifest and codec
registrations in `orna_standard::registered_opaque_codecs`.

## Presenter registry

`std.present.Presenter` is an ordinary standard-library object type
registered as standard-library metadata, not a core definition kind. A
presenter record has:

| Field | Meaning |
| --- | --- |
| `alias` | stable CLI name, e.g. `json`, `table` |
| `function` | the presenter function reference |
| `input_type_pattern` | accepted result type pattern |
| `output_type` | produced value type (Document or ByteStream) |
| `media_type` | optional media type, e.g. `application/json` |
| `streaming` | whether output streams |
| `priority` | deterministic selection priority |

The registry is a standard-library relation of ordinary objects, queryable
through the standard catalogue. `--output json` resolves alias `json` to the
`std.json.encode` presenter; `--output table` resolves alias `table` to
`std.terminal.present_table`; `--output application/json` resolves by media
type. An unresolved alias, media type, or type name is a presentation error
(spec exit 5, `ORNA0702`).

## Presenter functions

`std.json.encode` is a SERVER function with this closed signature:

```text
std.json.encode(p_value std.json.Value) RETURNS std.io.ByteStream
SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
```

The compiler checks the exact shape. The first slice accepts one typed
value argument whose runtime value converts to JSON without loss: integers,
bigints, floats, booleans, text, bytes (base64), references (explicit
`$ref`/`$type` object), lists, maps, and null. The artifact is a closed
server plan that encodes the bound value to the `std.io.ByteStream` payload.
It does not read the catalogue or the database.

`std.terminal.present_table` is a SERVER function with:

```text
std.terminal.present_table(p_rows std.data.Rows) RETURNS std.terminal.Document
SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
```

The first slice accepts a bounded row batch (the `ResultRows` model) and
renders a fixed plain-text table: column headers, aligned values, and a
trailing row count. It does not stream or virtualise yet.

## `--output` resolution in the sealed route

`orna invoke` already carries an `InvocationOutputRequirement` in the sealed
request. This decision wires it: after the sealed boundary resolves the
target and obtains the canonical result, it looks up the presenter registry
for a presenter whose input pattern accepts the result type and whose output
type/media type satisfies the requirement. The selected presenter executes
on the server, and the resulting opaque value (Document or ByteStream) is
returned in the event stream as the final `ValueBatch`.

`--output json` therefore produces one `std.io.ByteStream` value whose
payload is the JSON bytes. `--output table` produces one
`std.terminal.Document`. A result type with no matching presenter is
`ORNA0701` (no path from result type to offered sink).

## `orna-runtime-tty`

`orna-runtime-tty` is a new crate (or a module in `orna-client` if the
runtime boundary is not yet split — decide by reading the crate layout;
prefer a separate crate `crates/orna-runtime-tty` to honour the spec's
runtime model) that:

- consumes `std.terminal.Document` payloads and writes them to stdout;
- consumes `std.io.ByteStream` payloads and writes the bytes to stdout;
- reports no interactive surface beyond those two sinks in this slice.

The `orna` client selects it automatically when the sealed request's client
offer names the `std.terminal.Document` and `std.io.ByteStream` sinks. The
first selection rule is deterministic: if stdout is a terminal and the
result is a Document or ByteStream, `orna-runtime-tty` consumes it.

## Clean channels

`orna invoke` keeps the milestone-5 rules: result values on stdout, progress
and diagnostics on stderr, `--no-progress` suppresses progress. This slice
adds: when the result is a ByteStream, the stream bytes go to stdout with no
envelope or progress interleave; when it is a Document, the document text
goes to stdout with a final newline.

## Required implementation order

1. `docs(output): define terminal documents and JSON output` — this ADR and
   the work-ADR index only.
2. `feat(std): register terminal and byte-stream value types` — the two
   opaque value types with manifest identities, retained source units, and
   codec registrations; existing V1/V2 digest goldens recomputed.
3. `feat(compiler): check terminal and json presenters` — closed
   exact-shape checks for `std.json.encode` and `std.terminal.present_table`
   declarations, rejecting every variation.
4. `feat(artifact): encode terminal and json presenter plans` — two closed
   server artifacts (`orna.server-json-encode` and
   `orna.server-terminal-table` versions 1) with decoders and exhaustive
   rejection tests.
5. `feat(postgres): execute terminal and json presenters` — decode and
   execute both artifacts after typed binding; JSON encoding with the lossless
   rules above; table rendering from `ResultRows`.
6. `feat(core): model presenter registry and output resolution` — the
   standard-library presenter metadata model and the registry lookup that
   maps `--output` aliases/media types to presenter functions.
7. `feat(postgres): resolve output through the sealed route` — after target
   execution, resolve the output requirement against the registry and emit
   the presented opaque value in the final `ValueBatch`.
8. `feat(runtime-tty): render terminal documents and byte streams` — the
   first runtime crate and its sink consumption.
9. `feat(client): select the tty runtime automatically` — the client offer
   names the two sinks and selects `orna-runtime-tty` deterministically.
10. `test(server): prove output through orna invoke` — a live proof that
    invokes with `--output json` and `--output table`, asserts exact bytes on
    stdout, progress on stderr, and no presenter for an unmatchable type.

Each commit changes one to three files, has a signed Conventional Commit, and
keeps the workspace buildable.

## Deferred surface

CSV and XML encoders, lossless Orna JSON, JSON Lines streaming, CLIENT-side
presenters, presenter graph search with ranking and cycles, `--pretty`/
`--compact`/`--schema` flags, `--output-file`, `--trace`, graphical runtimes,
and runtime preference policy are later ADRs.

## Precedence

This decision implements the output half of spec milestone 6. Work ADR 0056
remains authoritative for the `orna invoke` CLI and the sealed request
surface. Work ADR 0054 remains authoritative for the sealed event stream.
The canonical specification remains authoritative outside this accepted
implementation scope.
