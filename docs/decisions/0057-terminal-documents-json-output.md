# ADR 0057: Terminal Documents, JSON Output, and the TTY Runtime

**Status:** Accepted

## Decision

The first accepted output slice has fixed terminal/byte-stream sink values and
one fixed internal JSON presenter path. This decision accepts:

1. `std.terminal.Document` as a standard-library transient opaque value type
   with a fixed canonical codec;
2. `std.io.ByteStream` as a standard-library transient opaque value type
   carrying bytes with a declared media type;
3. the fixed `std.json.encode` SERVER function/artifact path from a typed value
   to an `application/json` byte stream;
4. `--output json` resolution through that fixed path in the sealed
   `sys.invoke` route; and
5. `orna-runtime-tty`, the first runtime, which consumes
   `std.terminal.Document` and `std.io.ByteStream` and renders them on the
   local terminal.

This decision does not accept a generic `std.present.Presenter` relation or
registry, generic presenter graph search, `std.data.Rows` table semantics, or
`std.terminal.present_table`. Those surfaces are deferred below. CSV, XML,
lossless Orna JSON, streaming JSON Lines, presenters that run on the CLIENT
side, and graphical runtimes are governed by later decisions. The target
function stays unchanged: it returns its canonical typed value, and the fixed
JSON presentation path is a separate sealed step.

## Value types and codecs

`std.terminal.Document` is a standard-library `VALUE OPAQUE IMMUTABLE
TRANSIENT` type following the `opaque_token` precedent (work ADR 0034). Its
canonical payload is the terminal document in a fixed UTF-8 text layout:

```text
ORNA-TERMINAL-DOCUMENT/1 <len:u32 be> <utf-8 bytes>
```

The layout is plain text with `\n` line separators and a final newline. It
carries no control codes. A fixed output path produces the document and
`orna-runtime-tty` consumes it.

`std.io.ByteStream` is likewise `VALUE OPAQUE IMMUTABLE TRANSIENT` with the
canonical payload:

```text
ORNA-BYTE-STREAM/1 <media-type-len:u32 be> <media-type> <len:u32 be> <bytes>
```

Both types receive stable identities in the standard manifest and codec
registrations in `orna_standard::registered_opaque_codecs`.

## Fixed JSON presenter path

The accepted JSON output implementation is a fixed internal path, not a
queryable or user-extensible presenter relation. The sealed route pins the
standard `std.json.encode` function shape and its version-1
`orna.server-json-encode` artifact.

The fixed function has this closed signature:

```text
std.json.encode(p_value std.json.Value) RETURNS std.io.ByteStream
SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE
```

The compiler and sealed execution path check the exact function, parameter,
value-type, return-type, security, transaction, volatility, and artifact
identities. The bound value converts to JSON without loss according to the
accepted conversion matrix (integers, bigints, finite floats, booleans, text,
bytes as base64, explicit reference objects, lists, maps, and null). The
artifact is a closed server plan; it does not read the catalogue or the
database. The result is one `std.io.ByteStream` with media type
`application/json`.

The `std.present.Presenter` relation, a generic presenter registry, and
user-extensible presenter metadata are not accepted by this ADR. Their
physical representation and ranking remain proposal-level work.

## `--output json` in the sealed route

`orna invoke` carries an `InvocationOutputRequirement` in the sealed request.
For the accepted JSON slice, `--output json` selects the fixed
`std.json.encode` path after the sealed boundary resolves the target and
obtains its canonical result. The fixed presenter executes on the server, and
the resulting `std.io.ByteStream` opaque value is returned in the event stream
as the final `ValueBatch`.

`--output json` therefore produces one `std.io.ByteStream` value whose payload
is the JSON bytes. This ADR does not define `--output table` or any other
relational/table presenter; the absence of a fixed accepted path is a
presentation error until a later decision accepts the required Rows/table
contract.

## `orna-runtime-tty`

`orna-runtime-tty` is a new crate (or a module in `orna-client` if the
runtime boundary is not yet split — decide by reading the crate layout; prefer
a separate crate `crates/orna-runtime-tty` to honour the spec's runtime model)
that:

- consumes `std.terminal.Document` payloads and writes them to stdout;
- consumes `std.io.ByteStream` payloads and writes the bytes to stdout;
- reports no interactive surface beyond those two sinks in this slice.

The `orna` client offers these sinks and selects the installed
`orna-runtime-tty` deterministically. Work ADR 0063 later resolves the
selection/sink boundary: while TTY is the only installed runtime, the default
(and an explicit `--runtime tty`) selects it regardless of whether stdout is a
terminal. Once selected, its sink map is unconditional: a `Document` or
`ByteStream` is consumed and written to stdout whether stdout is a terminal or
redirected. The caller context still records `CliTty` versus `CliPipe`; that
fact does not gate sink consumption.

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
3. `feat(compiler): check the fixed JSON presenter` — closed exact-shape
   checks for the `std.json.encode` declaration.
4. `feat(artifact): encode the JSON presenter plan` — the closed
   `orna.server-json-encode` version-1 artifact with its decoder and exhaustive
   rejection tests.
5. `feat(postgres): execute the fixed JSON presenter` — decode and execute the
   artifact after typed binding, with the lossless JSON rules above.
6. `feat(postgres): resolve JSON output through the sealed route` — after
   target execution, select the fixed JSON path for `--output json` and emit
   the presented `ByteStream` in the final `ValueBatch`.
7. `feat(runtime-tty): render terminal documents and byte streams` — the
   first runtime crate and its sink consumption.
8. `feat(client): select the tty runtime automatically` — the client offer
   names the two sinks and selects `orna-runtime-tty` deterministically.
9. `test(server): prove JSON output through orna invoke` — a proof that
   invokes with `--output json`, asserts exact bytes on stdout, progress on
   stderr, and no output path for an unmatchable type.

Each commit changes one to three files, has a signed Conventional Commit, and
keeps the workspace buildable.

## Deferred surface

The following remain deferred or are governed by later decisions:

- the generic `std.present.Presenter` relation/registry, user-extensible
  presenter metadata, graph search, ranking, and cycle handling;
- `std.data.Rows` and general Rows/object-value semantics;
- `std.terminal.present_table`, `--output table`, and generic table rendering;
- CSV and XML encoders, lossless Orna JSON, JSON Lines streaming, CLIENT-side
  presenters, `--pretty`/`--compact`/`--schema` flags, `--output-file`, and
  `--trace`;
- graphical runtimes and runtime preference policy.

In particular, this slice does not define a different automatic format for
non-terminal stdout, a fallback non-TTY runtime, or a runtime contract for
redirected output; the installed TTY sink map above remains the only accepted
behavior.

## Precedence

This decision implements the fixed terminal/byte-stream and JSON portion of
spec milestone 6. Work ADR 0056 remains authoritative for the `orna invoke`
CLI and the sealed request surface. Work ADR 0054 remains authoritative for
the sealed event stream. Work ADR 0063 is later and authoritative for
automatic runtime selection and the installed TTY sink map. It supersedes only
the initial terminal-gating wording in this ADR's `orna-runtime-tty` section;
this ADR remains authoritative for the sink value types, fixed JSON presenter
path, `--output json` resolution, and clean channels. The canonical
specification remains authoritative outside this accepted implementation
scope.
