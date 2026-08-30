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
`orna source apply` (work ADR 0038). It writes only its rendered report to the
caller-selected output stream; it never installs a candidate and never changes
the active revision pair.

## Surface

```text
orna source diff <file.orna>
```

Exit codes follow the spec CLI table (`0` success, `2` usage, other closed
failures as documented by the installed-source error classes). The rendered
report is one UTF-8 document on stdout; diagnostics and progress never
interleave with it.

## Semantics

The active catalogue is recovered from the selected backend. On the managed
local endpoint, the bundle is checked through the same standard-backed
application path as `orna source apply` (`check_standard_application` against
the pinned verified standard, then `prepare_standard_application` against the
active revision pair). On an explicit `LocalPath`, the SQLite adapter runs its
supported backend-neutral `check`/`prepare` path against the recovered active
revision and rejects unsupported catalogue or artifact capabilities before
mutation. In both cases the candidate's `CatalogueSnapshot` is then compared
against the active `CatalogueSnapshot`:

- **added** — a schema, object type, enum type, or function whose stable
  identity is absent from the active catalogue;
- **dropped** — a schema, object type, enum type, or function whose stable
  identity is absent from the candidate;
- **renamed** — in the semantic-diff model, a schema, object type, enum type,
  or function present in both by stable identity but with a different resolved
  qualified name; references survive because resolution uses stable IDs. The
  diff model also has
  `FunctionRenamed` and `ParameterRenamed` kinds for these identity-keyed
  catalogue comparisons. Those model-level kinds do not imply that the source
  language accepts a corresponding rename statement;
- **unchanged** — present in both with the same identity and name (reported
  only in a `--verbose` mode, not by default).

The model reports field-level changes inside a renamed or retained object type
as nested field additions/drops/renames using the same stable-ID comparison.
It likewise reports function parameter changes per parameter identity. The
currently accepted source evidence is narrower: source accepts the
identity-preserving `ALTER TYPE ... RENAME FIELD ... TO ...` form, while
additions, drops, and unchanged definitions come from comparing the candidate
and active declarations. Source-level function and parameter rename syntax is
deferred until a separate language contract defines its identity and
dependency rules.

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
application revision with an object type and two functions, and exercises
body-only, field-rename, broken-source, and identical candidates. The field
rename candidate uses the identity-preserving `ALTER TYPE ... RENAME FIELD`
form, adds one function, and drops another. The rendered report must show the
field rename with its stable identity preserved (so dependent references
survive), the addition, and the drop; the identical candidate must report no
semantic changes; and the active pair must be byte-identical before and after
the diff command. This proof does not cover function or parameter renames:
source syntax for those transitions remains deferred pending a separate
accepted language contract.

## Consequences

- `crates/orna-core` gains a small closed semantic-diff model (no new
  dependencies) with unit tests for the identity-keyed comparison.
- `crates/orna-server` gains `source_diff.rs` (installed runner) and one CLI
  branch `orna source diff`.
- No owned-path changes; `Cargo.lock` untouched; the standard snapshot and
  migration set are unchanged (the command is read-only).
- Deferred (documented, not invented): a `std.diff` source unit (would need an
  `orna.std/5` snapshot ADR), non-file diff inputs, and interactive apply.