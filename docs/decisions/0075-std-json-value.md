# ADR 0075: `std.json.Value` Standard Value Snapshot

**Status:** Accepted

## Decision

Add `std.json.Value` to a new append-only `orna.std/5` standard-library
snapshot. Retain every V4 source unit and add `std/json.orna`; do not mutate
V4 or fabricate a same-major predecessor edge.

The new value uses these fixed identities:

| Item | Identity |
| --- | --- |
| V5 standard revision | `...05` |
| V5 catalogue revision | `...05` |
| V5 source bundle | `...05` |
| V5 source revision | `...05` |
| `std/json.orna` source unit | `...06` |
| `std.json` schema | `...06` |
| `std.json.Value` type | `...11` |

The source declaration is:

```sql
CREATE SCHEMA std.json;

CREATE TYPE std.json.Value AS VALUE
    OPAQUE
    KERNEL CONTRACT 'orna.std.value.json@1'
    IMMUTABLE
    TRANSIENT;

EXPORT TYPE std.json.Value AS std.JsonValue;
```

The existing `std.json.encode` function identity and sealed presenter plan do
not change. The installed V5 catalogue now provides its parameter type rather
than relying on a presenter-only fixture identity.

## Canonical payload

The opaque value codec accepts exactly:

```text
ORNA-JSON-VALUE/1 <len:u32 be> <canonical JSON UTF-8 bytes>
```

The body must be valid canonical JSON. It has no insignificant whitespace,
unique object keys sorted by UTF-8 bytes, arrays in source order, finite
numbers in the repository encoder's canonical form, and no trailing bytes.
The codec parses and re-serialises the body, then accepts it only when the
bytes are identical. The body length is bounded by the existing opaque payload
limit and the frame length must match.

The accepted `std.json.encode` conversion matrix remains ADR 0057's matrix:
null, booleans, integers, bigints, finite floats, text, base64 bytes, explicit
reference objects, lists, and maps. Enums, records, opaque values other than
`std.json.Value`, options, invocation carriers, and unknown constructed values
remain rejected as lossy.

## Implementation order

1. Add semantic canonical-JSON validation to the existing opaque codec contract.
2. Add `std/json.orna` and the V5 retained snapshot, identities, and digests.
3. Register the V5 JSON codec and prove V4 remains unchanged.
4. Bind the existing compiler presenter checker to the installed JSON value.
5. Add artifact and installed-runner proofs.

Each increment must change one to three files and keep the workspace buildable.

## Consequences

- JSON output keeps its existing `std.io.ByteStream` representation.
- `std.data.Rows` remains reserved and unregistered.
- A malformed or non-canonical JSON value fails before it enters an active
  revision.
- The standard upgrade remains compiler-backed and fail-closed.
- GUI runtimes, resource inspection, and reflective gateways remain outside
  this decision.

## Precedence

The canonical contract is `/home/kieran/dev/ornadb/spec/docs/50-json-value-type.md`.
ADR 0057 remains authoritative for presenter conversion and output routing.
ADRs 0058 and 0062 remain authoritative for V3 and V4 snapshots.
