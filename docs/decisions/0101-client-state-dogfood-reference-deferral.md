# ADR 0101: Defer CLIENT STATE Dogfood as Orna 1.0.0 Conformance

**Status:** Deferred

## Decision

The requested CLIENT STATE dogfood extension is deferred as an Orna 1.0.0
conformance slice. Do not claim that Orna 1.0.0 requires `STATE` declarations
on CLIENT functions, scope/default semantics, or V4 `StateClientPlan` slot
metadata.

This does not remove repository-local work ADR 0069 or the existing
`accepted-client.orna` fixture. It records that this fixture and its
CLIENT-state metadata are implementation extensions, not behavior established
by the frozen Orna 1.0.0 reference.

## Reference evidence

The frozen reference was searched across `Orna-1.0.0.md`, all `source/`
chapters, `grammar/orna.ebnf`, `tests/` (including requirement, evidence, and
scenario registries), `examples/`, and `api/`. Searches for
`CREATE CLIENT FUNCTION`, CLIENT `STATE` declarations with `LOCAL`, `SESSION`,
or `USER` scope, and `StateClientPlan` found no grammar production, ORNA
requirement, fixture contract, or API definition for this feature.

The closest related requirements are **ORNA-PAGE-001** and **ORNA-PAGE-002**
in `source/14-pages.md`: pages are ordinary values returned by functions, and
widgets/layouts are ordinary values composed by functions. They do not define
CLIENT function state declarations, scopes, defaults, or artifact slot
metadata. They therefore do not authorize treating the proposed dogfood case
as an Orna 1.0.0 conformance requirement.

Repository-local work ADR 0069 describes additional CLIENT state behavior.
That implementation decision is not an `ORNA-*` requirement in the frozen
reference and does not change this disposition.

## Current repository evidence

At the reviewed `origin/main` commit `bd24f99f7f1cfaec7bee5b16025bb87f0c1fd194`,
`crates/orna-syntax/testdata/accepted-client.orna` already contains a V4
state-bearing scalar function, and the LSP end-to-end test loads that fixture.
The offline compiler test for `StateClientPlan` metadata still embeds a
separate Orna source string. This slice adds no further fixture or metadata
test because the frozen reference does not define their contract. No Rust
tests were run for this documentation-only deferral.

Reconsider this slice only after the normative reference defines CLIENT state
syntax and the scope/default/slot metadata whose dogfood conformance is to be
asserted.
