# ADR 0066: `orna source diff` — Semantic Source Changes Without Apply

**Status:** Accepted

## Decision

`orna source diff <file.orna>` becomes a read-only command that checks one
source bundle against the active database revision, prepares the candidate
revision without applying it, and renders the semantic changes between the
candidate and active catalogues. It closes the spec's `source/revision/
semantic diff` checklist item (LOCKED: "human source plus resolved
stable-ID semantic graph").

The command runs in-process against the fixed private instance, exactly like
`orna source apply` (work ADR 0038). It never writes a standard stream, never
installs a candidate, and never changes the active revision pair.

## Surface

```text
orna source diff <file.orna>
```

Exit codes follow the spec CLI table (`0` success, `2` usage, other closed
failures as documented by the installed-source error classes). The rendered
report is one UTF-8 document on stdout; diagnostics and progress never
interleave with it.

## Semantics

The active catalogue is recovered from the fixed private instance. The bundle
is checked through the same standard-backed application path as `orna source
apply` (`check_standard_application` against the pinned verified standard,
then `prepare_standard_application` against the active revision pair). The
candidate's `CatalogueSnapshot` is then compared against the active
`CatalogueSnapshot`:

- **added** — a schema, object type, enum type, or function whose stable
  identity is absent from the active catalogue;
- **dropped** — a schema, object type, enum type, or function whose stable
  identity is absent from the candidate;
- **renamed** — a schema, object type, enum type, or function present in both
  by stable identity but with a different resolved qualified name; references
  survive because resolution uses stable IDs;
- **unchanged** — present in both with the same identity and name (reported
  only in a `--verbose` mode, not by default).

Field-level changes inside a renamed or retained object type are reported as
nested field additions/drops/renames using the same stable-ID comparison.
Function parameter changes are likewise reported per parameter identity.

Comparison keys are stable identities (`SchemaId`, `TypeId`, `FunctionId`,
`FieldId`, `ParameterId`), never name strings, matching the durable
`sys_definition_references` contract.

## Diagnostics and failures

Compiler diagnostics are rendered exactly as `orna source check` renders them
(no candidate is prepared). A bundle that fails checking exits with the
diagnostic document, not a diff. Installed-host failures (service identity,
package state, engine, missing instance) reuse the installed-source error
classes from work ADR 0038.

## Proof

A live proof (`source_diff_live.rs`) boots the standard chain, installs one
application revision with an object type and two functions, then diffs a
candidate that renames one field and one function, adds one function, and
drops another. The rendered report must show the rename with the stable
identity preserved (so dependent references survive), the addition, and the
drop — and the active pair must be byte-identical before and after the diff
command.

## Consequences

- `crates/orna-core` gains a small closed semantic-diff model (no new
  dependencies) with unit tests for the identity-keyed comparison.
- `crates/orna-server` gains `source_diff.rs` (installed runner) and one CLI
  branch `orna source diff`.
- No owned-path changes; `Cargo.lock` untouched; the standard snapshot and
  migration set are unchanged (the command is read-only).
- Deferred (documented, not invented): a `std.diff` source unit (would need an
  `orna.std/5` snapshot ADR), non-file diff inputs, and interactive apply.