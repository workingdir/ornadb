# Work ADR 0087: Bounded `std.data.Rows` and the Retained Table Presenter

**Status:** Accepted

## Decision summary

Accept one bounded, materialised `std.data.Rows` value and make the existing
`std.terminal.present_table` input a retained standard executable in a new
append-only standard revision after `orna.std/7`. The value is immutable,
transient, non-persistable, and carries a complete ordered column shape plus a
finite ordered set of rows. It is a transport value for a completed result, not
a database relation, resource stream, or UI model.

The value crosses the sealed invocation boundary as one registered opaque value
whose payload is the canonical `ORNA-ROWS/1` frame below. Every cell is one
canonical ORV5 value and is checked against the declared column width, resolved
type, and nullability. The frame has no authority, credentials, model handles,
SQL, or caller-supplied security facts.

The sealed invocation path retains the complete `ResultRows` shape before it
presents a result. A multi-row or multi-column `FunctionReturn::Rows` result is
never converted into one value per row and never silently narrowed to its first
column. The route emits one `std.data.Rows` value in its final `ValueBatch`;
table and CSV presenters decode that value back to `ResultRows`. The existing
CSV presenter remains sealed. The retained table presenter wins only through
its explicit table/default-TTY selection; explicit CSV selection continues to
win for CSV. A bare Rows type selector remains ambiguous when table and CSV are
otherwise tied.

This decision does not accept Qt virtual model construction, model handles,
range or child requests, sorting, filtering, refresh/invalidation, incremental
updates, or model cancellation semantics.

## Context

The current repository has the pieces of a bounded result path, but not a
standard value that can carry its shape across `sys.invoke`:

* `orna-core` has `ResultColumn`, `ResultRow`, and `ResultRows`. Columns retain
  their declared order and contain a non-empty unique name, a `ResolvedType`,
  and a nullable flag. Rows retain query order. `ResultRows::new` checks exact
  row width, exact runtime type, nullability, finite FLOAT values, and rejects
  constructed values, opaque values, and sealed invocation carriers.
* `ResultRows` itself has no intrinsic count or byte limit. The PostgreSQL
  SERVER executor supplies the finite limits: `ROW_LIMIT = 10_000`,
  `CELL_LIMIT = 1_000_000`, and `PAYLOAD_LIMIT = 16 * 1024 * 1024` bytes.
* The protocol already has the canonical ORV5 value codec and registered opaque
  codec registry. ORV5 fixes a 25-byte value header, big-endian lengths and
  identities, active-catalogue validation, finite FLOATs, and bounded recursive
  values. It does not define a Rows envelope.
* The sealed table and CSV engines already render a `ResultRows`, but the
  current sealed route wraps a scalar as a synthetic one-column, one-row
  `ResultRows`. `FunctionReturn::Rows` is currently classified as a stream and
  `resource_values_from_server_result` requires one value per row, so a
  multi-column result loses its shape before presentation.
* `InvocationEventBody::ValueBatch` is a non-empty vector of typed values and
  has no row-boundary field. `ORNA-RESOURCE/1` is a bounded value stream
  protocol, not a materialised table contract.
* The resolver and presenter fixtures reserve `STD_DATA_SCHEMA_ID` and
  `STD_DATA_ROWS_TYPE_ID`, and the sealed table artifact checks those identities,
  but no V1 through V7 standard snapshot registers `std.data.Rows` or its
  codec. Fixture identities and the old `std/present.orna` test source are not
  source authority.
* V7 is already occupied by the accepted `std.ui.window` source unit and
  append-only snapshot. The Rows vehicle therefore starts at V8 rather than
  mutating V7.

The gap-research proposal (spec `docs/49-gap-research-and-contract-plan.md`)
correctly requires a separate Rows schema, codec, source owner, upgrade edge,
and retained-versus-sealed decision. Work ADRs 0057, 0058, 0062, 0067, 0075,
0082, and 0083 remain the relevant output, value, CSV, UI, and runtime
precedents.

## Scope and non-goals

This ADR accepts:

1. the `std.data.Rows` opaque value and its canonical payload;
2. the V8 retained standard source unit containing the Rows type and the
   `std.terminal.present_table` declaration;
3. the V7-to-V8 append-only standard upgrade and digest rules;
4. shape-preserving sealed invocation and table/CSV presentation of a complete
   bounded `ResultRows`; and
5. immutable ownership, active-revision validation, and redacted failure rules.

It does not accept a general relational algebra value, a durable table object,
`std.data.Resource<Rows>`, a `StreamResource<Rows>`, a Rows-specific
`ORNA-RESOURCE/1` transport, arbitrary opaque cell values, a presenter graph,
lossless Rows-to-JSON conversion, or any graphical table/grid function.

## Identity, version, and source facts

All IDs below are exactly 16 bytes in network order. The `...NN` notation means
fifteen zero bytes followed by byte `NN`; these are stable identity values, not
UUIDs.

| Fact | Exact value |
| --- | --- |
| Standard library version | `orna.std/8` |
| Language version | `orna.language/1` |
| `StandardLibraryRevisionId` | `...08` |
| Standard catalogue revision | `...08` |
| Standard source bundle | `...08` |
| Standard source revision | `...08` |
| Source-revision parent | V7 source revision `...07` |
| New source ordinal | `7` |
| New source logical path | `std/data.orna` |
| New source unit identity | `...09` |
| `std.data` schema identity | `STD_DATA_SCHEMA_ID = ...07` |
| `std.data.Rows` type identity | `STD_DATA_ROWS_TYPE_ID = ...12` |
| Rows semantic name | `std.data.rows` |
| Rows representation contract | `orna.std.value.rows@1` |
| Export binding | `std.Rows` → `...12` |
| Export `TypeBindingId` | `6d00a1809c45ec55e8b7de1f6e054f97` |
| `std.terminal` schema identity | `...04` (retained from V3) |
| `std.terminal.Document` type | `...0f` (retained from V3) |
| Retained table function | `std.terminal.present_table` → `...12` |
| Retained table parameter | `std.terminal.present_table.p_rows` → `...12` |
| Retained table function revision | `...12`, revision number `1` |
| Sealed CSV function | `std.csv.encode` → `...13` (ADR 0067) |
| Sealed CSV parameter/revision | `...13` / `...13` (ADR 0067) |

The V8 catalogue is V7 plus `std.data`, the Rows opaque value type, the
`std.Rows` qualified binding, and the retained table function. It has no new
object type and no `std.csv` source declaration. The V7 catalogue, all V1-V7
source units, and every historical executable remain unchanged.
The export identity is derived from `TypeBinding::qualified` over the exact
qualified lookup words `std` and `Rows`. It is independent of the target
`TypeId`; the implementation and its golden tests must compute this value
through the canonical constructor rather than copy a hand-authored identity.

An earlier draft of this ADR listed `0ec2755d6bfe08211cab49f69968e146`.
That value does not match the repository's canonical type-binding identity
algorithm and is not part of the accepted V8 contract.

### Exact retained source unit

The V8 source bundle retains V7 units at ordinals `0` through `6`, byte for
byte, and appends `std/data.orna` at ordinal `7` with source unit identity
`...09`. The appended unit contains exactly the following text after its final
newline:

```sql
CREATE SCHEMA std.data;

CREATE TYPE std.data.Rows AS VALUE
    OPAQUE
    KERNEL CONTRACT 'orna.std.value.rows@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.data.Rows AS std.Rows;

CREATE SERVER FUNCTION std.terminal.present_table(
    p_rows std.data.Rows
)
RETURNS std.terminal.Document
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT p_rows;
```

`std.terminal` is already declared by the retained output unit. The table
function is intentionally sourced in the appended data unit so the Rows type,
its export, and its only retained consumer have one source owner. Its body is
the closed presenter declaration body; the checked `orna.server-terminal-table`
artifact, not general SQL evaluation, supplies the rendering operation.

The source checker must require the exact source path, ordinal, unit identity,
source text, schema/type/export facts, table signature, declaration origins, and
cross-unit `std.terminal` reference. It must reject a source file that merely
has a matching name or fixture identity.

## `std.data.Rows` value and codec

### Value semantics

`std.data.Rows` is a `VALUE OPAQUE IMMUTABLE TRANSIENT` type. It owns a complete
snapshot of one bounded result and has no mutation operation. It is valid only
for the lifetime of the invocation, presenter operation, or other explicitly
owned in-memory value that contains it.

The outer Rows value is the only opaque value admitted by this contract. A
Rows frame contains no principal, grant, session, invocation authority,
revision claim, SQL text, source text, credential, runtime handle, model
handle, or cancellation token. The active revision and verified standard
registry are supplied by the trusted boundary and are never read from the
frame as authority.

At the current `ResultRows` boundary, cells admit the same useful result subset
as the SERVER executor: BOOLEAN, INTEGER, BIGINT, finite FLOAT, TEXT,
BYTES, active typed references, active enum values, and active immutable record
values, plus typed NULL where the column is nullable and the runtime can
represent that type. The exact `ResultRows` checks remain authoritative; this
ADR does not add coercion or compatibility matching.

Cells are rejected when they are:

* a `RuntimeValue::Constructed` value (OPTION, LIST, MAP, or SET), even though
  ORV5 can represent constructed values;
* an opaque value, including a registered opaque value (the outer Rows value is
  the exception); or
* an `InvokeValue`, `InvokeRequest`, or `InvokeEvent` carrier.

Consequently no unregistered opaque value, secret-bearing opaque value, sealed
authority carrier, or nested Rows value can enter a Rows frame. Ordinary TEXT
and BYTES remain authorized result data; the contract gives them no authority
meaning and does not pretend to classify their application content as a
credential.

### Canonical `ORNA-ROWS/1` frame

The opaque payload is one complete frame with no outer body-length prefix:

```text
magic[12]                 ASCII `ORNA-ROWS/1 `
frame_version             u16 big-endian, exactly 1
column_count              u32 big-endian
columns                   column_count column definitions
row_count                 u32 big-endian
rows                      row_count row records
```

Each column definition is ordered and encoded as:

```text
name_length               u32 big-endian
name                      exactly name_length UTF-8 bytes
type_form                 u8
type_id                   exactly 16 identity bytes
nullable                  u8, exactly 0 or 1
```

`type_form` is closed:

| `type_form` | Meaning | `type_id` rule |
| --- | --- | --- |
| `0x01` | `ResolvedType::Scalar` | The canonical standard identity for one of BOOLEAN `...01`, INTEGER `...02`, BIGINT `...03`, FLOAT `...04`, TEXT/CLOB `...06`, or BYTES/BLOB `...07`. |
| `0x02` | `ResolvedType::Named` | An active enum or immutable record type; the active catalogue must contain the exact identity. |
| `0x03` | `ResolvedType::Reference` | An active object type target; the reference is retained as an ID and is never dereferenced by the codec. |
| `0x04` | `ResolvedType::Value` | An active non-opaque value identity whose concrete runtime representation is admitted by `ResultRows`. |

No type names, aliases, inferred compatibility scalar, or alternate type
forms occur in the frame. A type identity is metadata, not authority; it must
be checked against the active application catalogue and the verified standard
catalogue before a cell is accepted.

Each row is encoded as:

```text
cell_count                 u32 big-endian, exactly column_count
cells                      cell_count length-delimited ORV5 values
```

Each cell is:

```text
value_length               u32 big-endian
value                      exactly value_length bytes, one canonical ORV5 frame
```

There is no row key, row sort key, cursor, page token, estimated count, or
child marker. Columns are emitted in the `ResultRows` declared projection
order. Rows are emitted in the SERVER query result order. The encoder does not
sort, deduplicate, or otherwise change either order. A zero-row Rows value is
valid and still retains its non-empty column definitions.

### ORV5 cell rules

A cell must start with the ORV5 marker and use the existing 25-byte ORV5
header:

```text
`ORV5` marker[4]
value_tag              u8
value_type_id          16 bytes
payload_length         u32 big-endian
payload                exactly payload_length bytes
```

The cell must contain exactly one complete ORV5 value. ORV6 SET frames, ORV1
through ORV4 frames, trailing bytes, malformed nested descriptors, and values
that cannot be re-encoded canonically as ORV5 are rejected. The decoder uses
the active revision and the V8 opaque registry, then re-encodes the decoded
value and requires byte-for-byte equality with the supplied cell.

The existing fixed-width and canonical rules remain in force: BOOLEAN payloads
are one byte `0` or `1`; INTEGER is four bytes; BIGINT and finite FLOAT are
eight bytes; a reference object identity is exactly 16 bytes; variable TEXT,
BYTES, enum, and record payloads use the existing bounded ORV5 rules; and
negative zero FLOAT is normalised to positive zero by the canonical encoder.
A typed NULL carries the exact declared type identity and is accepted only when
`nullable = 1`. A non-null cell carries the exact declared `ResolvedType` and
is not coerced to a compatible scalar.

Rows construction is two-phase:

1. the registered opaque codec checks the frame marker, version, counts,
   UTF-8, name uniqueness, field widths, lengths, ORV5 structural shape, and
   canonical byte consumption without allocating beyond the bounds below; and
2. the protocol/server boundary resolves every type against the active
   revision, decodes every ORV5 cell with the active registry, and constructs
   `ResultRows`, which performs the exact width, type, nullability, finite
   FLOAT, constructed, opaque, and carrier checks.

Both encoding and decoding are atomic. No partially decoded Rows value or
partially presented output is returned.

## Bounds and validation

The Rows-specific bounds are derived from the existing SERVER result and
runtime-value limits:

| Quantity | Bound and rule |
| --- | --- |
| Columns | `1..=1_000_000` (`CELL_LIMIT`); the count is checked before allocation. |
| Rows | `0..=10_000` (`ROW_LIMIT`). |
| Cells | `row_count * column_count <= 1_000_000` (`CELL_LIMIT`), with checked multiplication. |
| Complete Rows payload | At most `16 * 1024 * 1024` bytes (`PAYLOAD_LIMIT` and `MAX_OPAQUE_CODEC_PAYLOAD_LENGTH`), including magic, metadata, count fields, cell lengths, and cell bytes. |
| One cell frame | At most the same 16 MiB value-payload bound; its ORV5 decoder also enforces `MAX_RUNTIME_VALUE_NODES = 65_536` and existing descriptor/depth bounds. |
| Encoded lengths/counts | Every length and count is a big-endian `u32`; conversion to `usize` is checked before allocation or arithmetic. |
| Column name | Non-empty UTF-8 and exact-byte unique against earlier names, matching `ResultRows`. |
| Row width | Every `cell_count` equals `column_count`; missing or extra cells are invalid. |

The complete-frame byte bound includes framing overhead. The server encoder
must reserve that overhead rather than relying only on the existing logical
name-plus-value accounting. A result that fits the SQL executor's logical
payload but cannot fit its canonical Rows frame fails closed; it is not paged
or truncated.

Validation has a deterministic precedence:

1. reject an oversized input before count-derived allocation;
2. check the exact magic and frame version;
3. check column and row counts and their checked cell-product bound;
4. decode and validate all column names, type forms, identities, and nullable
   bytes;
5. check row `cell_count` before reading cells;
6. check each cell length and complete-frame boundary;
7. validate the ORV5 marker, structural payload, and canonical re-encoding;
8. resolve the active type and apply exact NULL/type/`ResultRows` checks; and
9. require the frame cursor to finish exactly at the input length.

Internal errors may retain the zero-based row and column position for audit and
engineering diagnostics. The closed error families are invalid magic/version,
truncation/trailing bytes, count/cell/payload limit, invalid UTF-8 or column
metadata, duplicate name, row-width mismatch, ORV5 failure, inactive type,
nullability mismatch, exact type mismatch, non-finite FLOAT, constructed or
opaque cell, and invocation-carrier cell. These details never cross the sealed
public failure boundary.

## Shape-preserving invocation and presentation

### Sealed invocation result

For a SERVER target whose declared return is `FunctionReturn::Rows`, the
sealed invocation executor retains the complete `ResultRows` produced under the
already-authorised active transaction. It encodes that set as one V8-registered
`RuntimeValue::Opaque` of type `STD_DATA_ROWS_TYPE_ID`.

The completed event sequence is therefore:

```text
InvocationStarted(0)
ValueBatch(1)          exactly one `std.data.Rows` value
InvocationCompleted(2)
```

The optional existing event schema field is not a second copy of the Rows
metadata; the opaque value is the typed result. This sequence is used for
zero-row, one-row, multi-row, one-column, and multi-column results alike. No
row is emitted as an independent event item, and an empty result still emits
one Rows value so its column shape is not lost.

A Rows result is a bounded materialised value, not an `ORNA-RESOURCE/1` stream.
The resource protocol continues to accept its existing scalar and
`STREAM<T>` shapes and rejects a Rows/Table resource request. `STREAM<T>`
targets retain their existing pull, credit, and cancellation behaviour.

The same complete-shape rule applies when a sealed invocation presents a
mutating target whose declared result is `ROWS`: the bounded result is
materialised and validated before the transaction commits. Cancellation that
wins before materialisation follows the existing transaction cancellation and
rollback rules. Once the immutable Rows value has been constructed, there is
no independent presenter or model cancellation phase.

`CALL_RAW` remains the lower-level typed path. It does not acquire presenter
semantics or a Rows event shape; its existing one-column flat-result boundary
continues to reject a multi-column result rather than silently dropping
columns.

### Retained table input and sealed CSV input

On V8, the canonical retained table presenter is:

```text
std.terminal.present_table(
    p_rows std.data.Rows
)
RETURNS std.terminal.Document
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
```

Its retained executable is the existing closed
`orna.server-terminal-table` version-1 artifact. That artifact pins
`std.terminal.present_table.p_rows` (`...12`) and `std.data.Rows` (`...12`)
and has the exact 44-byte `ORNATT\0\0`/version/parameter/type payload. The
engine decodes the Rows value and renders the existing aligned terminal table,
including its fixed header, separator, rows, row-count line, LF endings, and
control-character rejection.

`std.csv.encode` remains the sealed ADR 0067 function and
`orna.server-csv-encode` version-1 artifact. It consumes the same decoded
`ResultRows`, preserves the same column and row order, and produces the
existing `std.io.ByteStream` with media type `text/csv`. It is not added as a
source declaration or V8 retained executable.

The precedence is closed and deterministic:

1. an exact output alias wins: `table` selects retained
   `std.terminal.present_table`; `csv` selects sealed `std.csv.encode`;
2. an exact media type selects its sole compatible presenter:
   `text/plain` selects table and `text/csv` selects CSV;
3. a qualified `std.data.Rows` type selector considers both candidates. Since
   table and CSV have the same default priority, this selector is ambiguous
   and fails rather than choosing by source order; and
4. when no explicit output requirement is supplied, the existing interactive
   TTY policy selects the retained table presenter. A non-TTY caller does not
   trigger an implicit machine encoding; it must request an explicit format.

The alias-over-media-over-type ordering is unchanged for JSON and existing
presenter entries. On V1-V7, where the Rows type and retained table executable
are absent, the current sealed scalar compatibility path remains available
unchanged. On V8, a malformed retained table artifact or malformed Rows value
fails closed and does not fall back to the old scalar wrapper.

`std.json.encode` remains unchanged. A Rows result is not implicitly converted
to `std.json.Value`; a future lossless Rows/JSON presenter needs its own
contract and identity.

## Ownership, security, and redaction

* The authorised server executor owns `ResultRows` until encoding. The
  invocation event owns the resulting immutable opaque value. A presenter may
  borrow and decode it for one operation, then owns only its output value.
  No mutable alias crosses a boundary.
* `Rows` cannot be persisted in catalogue data, object fields, durable USER
  state, action storage, or a recovered revision. The transient persistence
  check fails before a durable write. It is not a cache key or a cross-principal
  durable snapshot.
* Encoding occurs only after the target has passed the existing authenticated
  `EXECUTE` decision and active-revision checks. Decoding checks the pinned
  verified standard registry and active catalogue; a type ID in a frame cannot
  grant access to an object or field.
* References are displayed as explicit typed references and are never
  dereferenced by the Rows codec or presenters. Record fields and enum labels
  must be active and exact. No principal, grant, policy input, credential,
  runtime callback, or model handle is representable.
* A target or Rows limit failure emits no `ValueBatch`. A malformed presenter
  input emits no partial document or byte stream. The public sealed result uses
  the existing redacted classes and messages (`INVOKE_BIND_FAILED`,
  `INVOKE_TARGET_FAILED`, `INVOKE_INTERNAL_FAILURE`) or the existing
  presentation failure (`ORNA0701`/`ORNA0702` at the CLI boundary). Row/column
  indices, expected and actual types, raw payloads, SQL, and codec error text
  remain internal.
* Security and invocation audit records retain stable outcome classes and
  required identity evidence only. They do not retain cell bytes, credentials,
  raw arguments, or unredacted Rows errors.

## Compatibility and migration

V1 through V7 standard snapshots are immutable. Their source units, catalogue
identities, executable records, codec registrations, digest goldens, and
recovery behaviour remain byte-for-byte valid. `STD_DATA_SCHEMA_ID` and
`STD_DATA_ROWS_TYPE_ID` are inactive in those snapshots; a caller cannot make
them active by supplying a fixture or an unverified payload.

V8 is installed only by the compiler-backed append-only edge
`orna.std/7 → orna.std/8`:

1. require the active verified standard to be exactly V7;
2. retain and verify the V7 parent and the complete V8 source bundle;
3. check `std/data.orna`, the Rows type, export, retained table signature,
   origins, artifact, and references against the V8 catalogue;
4. prepare the companion application revision against the V8 verified hash
   context; and
5. persist the complete V8 standard snapshot and companion application
   revision atomically, leaving all V7 records and historical application pins
   intact.

The existing standard-snapshot persistence relations are sufficient for the
additional schema, value type, binding, function, executable, origin, and
reference rows. No historical row is rewritten. If a database constraint needs
an accepted append-only extension for the new value kind, the extension must
preserve every pre-V8 row and its recovery interpretation.

A V8 decoder rejects a Rows payload under any standard snapshot other than the
verified V8 registry. A V7 application can continue to use its existing sealed
scalar table/CSV path, but it cannot receive a multi-column result by silently
narrowing it.

## Digest and golden rules

V8 reuses `StandardLibraryDigestVersion::Version2` and the exact domain
`ornadb.hash/standard-library/v2\0`. The implementation computes, rather than
copies, these values from the retained source and canonical records:

* the new `std/data.orna` source-unit content digest;
* the V8 source-bundle digest from the eight units in ordinal order;
* the V8 source-revision record digest using bundle `...08`, parent source
  revision `...07`, and the bundle digest;
* the V8 catalogue/standard-library digest over the V2 version number, V8
  identities, language version, complete catalogue, complete executable
  sequence, references, and origins in the existing canonical order; and
* the retained table artifact content digest and semantic digest, computed
  from the exact source declaration and `orna.server-terminal-table` version-1
  payload.

The V8 executable sequence appends the table executable after the retained V7
sequence (`std.invoke.echo`, `std.json.encode`, and `std.ui.window`). Existing
V1-V7 unit, bundle, source-revision, standard, artifact, and semantic goldens
are asserted unchanged. The sealed CSV artifact keeps its ADR 0067 golden and
is not copied into the V8 standard digest. Every V8 golden test recomputes its
value through the canonical encoders and compares it with the pinned compiled
golden; a hand-authored digest is not authoritative.

## Alternatives considered

### Keep `ResultRows` internal and pass it directly to the presenter

Rejected. An in-memory Rust value cannot cross the sealed invocation event
boundary, and the current `ValueBatch` has no row shape. This is the source of
the existing one-column narrowing bug.

### Encode Rows as `LIST<RECORD>` or another general constructed value

Rejected for this slice. It would replace the stable column shape with
application-specific descriptors, conflict with the current `ResultRows`
constructed-value rejection, and make presenter input depend on collection
semantics that have not been accepted for result tables.

### Treat Rows as a stream or resource

Rejected. A bounded complete result needs one immutable shape for table and CSV
presentation. Reusing `ORNA-RESOURCE/1` would require cursor, credit,
completion, cancellation, and lifetime rules and would encourage an unbounded
buffer or implicit paging.

### Construct a Qt table model

Rejected. The Qt v1 provider has no accepted model-construction operation and
its model handles, range/child requests, completion, sorting/filtering, and
cancellation semantics are explicitly outside the current runtime contract.
Materialised TTY/CSV output needs none of those semantics.

### Retain CSV in the same source unit

Rejected. ADR 0067 deliberately defines CSV as a sealed in-process presenter.
Retaining only `std.terminal.present_table` gives the source-authored table
input a standard identity without creating a second source of CSV authority.

### Mutate V7 or reuse the presenter fixture source identity

Rejected. V7 is an immutable accepted snapshot, and the fixture's
`std/present.orna` identity is not a manifest fact. V8 and source unit `...09`
are the append-only vehicle.

## Implementation files

The following files comprise the later implementation boundary; this ADR does
not assert that those changes or proofs are present:

* `stdlib/std/data.orna` — the exact appended source unit above.
* `crates/orna-standard/src/lib.rs` — V8 manifest, retained/verified snapshot,
  V7-to-V8 upgrade, Rows codec registration, and computed V8 goldens.
* `crates/orna-compiler/src/resolver/model.rs` — V8 and source/type identity
  constants, including the existing Rows and data-schema reservations.
* `crates/orna-compiler/src/resolver.rs` and `src/prepare.rs` — exact V8 source
  reconciliation, retained table checker integration, references, executable
  evidence, and application-upgrade preparation.
* `crates/orna-core/src/value.rs` — the checked Rows payload contract,
  structural frame validation, and opaque registration integration.
* `crates/orna-protocol/src/lib.rs` — canonical Rows encode/decode at the
  active-revision boundary and ORV5 cell round-trip checks.
* `crates/orna-core/src/presenter.rs` and
  `crates/orna-postgres/src/kernel/server_execution.rs` — retained table
  metadata, Rows decoding, table/CSV engines, output precedence, and bounded
  frame accounting.
* `crates/orna-postgres/src/kernel/security.rs` — complete `ResultRows`
  retention in sealed invocation, one Rows `ValueBatch`, transaction ordering,
  and redacted failure mapping.
* `crates/orna-server/src/invoke.rs` and related invocation integration tests —
  explicit table/CSV selection, stdout/stderr discipline, and CLI presentation
  mapping without implicit JSON conversion.

No Qt runtime source is part of this Rows slice.

## Sequential implementation checklist

1. Add the exact `std/data.orna` source and V8 identity table while retaining
   V1-V7 source bytes and catalogue facts unchanged.
2. Add the Rows frame model and registration with checked big-endian framing,
   count/byte/node limits, deterministic order, and atomic malformed-input
   errors.
3. Add active-revision ORV5 cell decoding and `ResultRows` construction;
   enforce exact width, type, nullability, finite FLOAT, active enum/record,
   reference, constructed, opaque, and invocation-carrier rules.
4. Reconcile the V8 source unit and retained table declaration through the
   compiler, construct the pinned table artifact/references, and compute all
   V8 source, artifact, semantic, and standard goldens canonically.
5. Extend the standard registry and compiler-backed upgrade from V7 to V8;
   persist and recover the complete snapshot atomically without changing
   historical snapshots.
6. Change sealed SERVER Rows execution to materialise and encode one complete
   Rows value before event construction, while leaving scalar, stream, and
   raw-call compatibility paths closed and explicit.
7. Route V8 table presentation through the retained table executable, route
   CSV through the sealed ADR 0067 executable, and enforce alias/media/type
   precedence without scalar fallback or implicit JSON conversion.
8. Add the focused codec, source/reconciliation, presenter, invocation, and
   redaction proofs below.
9. Add the Compose-backed install/reopen/presentation proof below, including
   the V7 historical-pin checks.

## Proof obligations

### Focused proof obligations

* Encode and decode a canonical one-column zero-row frame and a golden
  multi-column frame containing scalar, reference, enum/record, and typed-null
  cells; assert exact bytes, exact order, and immutable ownership.
* Reject every prefix truncation and trailing byte, wrong magic/version,
  invalid UTF-8, empty or duplicate name, unknown type form, invalid nullable
  byte, oversized column/row/cell/payload count, checked cell-product overflow,
  row-width mismatch, ORV6/ORV1-ORV4 cell, non-canonical ORV5, wrong active
  type, wrong nullability, non-finite FLOAT, constructed value, opaque value,
  and invocation carrier. Assert deterministic error precedence and no partial
  value.
* Recompute V1-V7 retained snapshot goldens and assert they are unchanged;
  validate the exact V8 source order, identities, export binding, table
  function signature, artifact bytes, references, and Rows registry set.
* Present zero-row, one-row, multi-row, one-column, and multi-column `ResultRows`
  values through table and CSV. Assert column/row order, null rendering,
  exact Document/CSV framing, and failure without partial output.
* Exercise sealed invocation for every Rows cardinality. Assert exactly
  `InvocationStarted`, one Rows `ValueBatch`, and `InvocationCompleted`, with
  the decoded envelope equal to the original `ResultRows`.
* Assert explicit alias/media precedence, bare Rows-selector ambiguity,
  retained-table selection for an interactive TTY, and no implicit
  Rows-to-JSON path. Assert raw-call and resource requests retain their
  documented closed boundaries.
* Inject target, codec, presenter, and limit failures. Assert the public
  result contains only the existing redacted failure class/code and no cell,
  SQL, argument, credential, expected/actual type, or internal codec detail.

### Compose/live proof obligations

Against the PostgreSQL Compose service, install V7 and then the compiler-backed
V8 child, reopen the database, and verify the active V8 standard/revision and
codec registry. Invoke a source-authored multi-column/multi-row SERVER function
through normal `sys.invoke`; decode the emitted Rows value and assert its exact
columns, order, rows, nullability, and active types. Repeat with zero rows and a
limit-boundary failure.

Run the same invocation with explicit `table` and `csv` output and assert the
complete terminal Document or `text/csv` ByteStream on stdout, with progress and
redacted diagnostics confined to stderr. Restart/recover and repeat the
presentation to prove the V8 pin and retained table executable survive reopen.
Finally verify a historical V7 application pin remains V7 and continues its
pre-V8 sealed behaviour; it must not acquire the Rows codec or a rewritten
source snapshot.

## Consequences and precedence

The accepted result path gains one explicit bounded value and one retained
source executable. Table/CSV presentation can preserve complete result shape
without introducing a database-backed UI model, but large or unbounded data
still requires a later resource/virtual-model contract. Callers must request
CSV explicitly when they need machine output; default interactive output is
human-oriented table text.

Within this bounded slice, this ADR is authoritative for the V8 Rows value,
`ORNA-ROWS/1`, the retained table input, and shape-preserving presenter
routing. Work ADR 0057 remains authoritative for terminal Documents and the
original sealed presenter boundary; ADR 0067 remains authoritative for sealed
CSV; ADR 0058/0062/0075/0079/0082/0083 remain authoritative for their
historical value, action, and runtime contracts. The canonical V1-V7 standard
snapshots and all existing raw/resource/Qt model boundaries retain their
previous meaning. Any future virtual model, Rows stream, lossless JSON, or
additional presenter must be a separate append-only contract.
