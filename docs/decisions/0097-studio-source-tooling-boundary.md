# Work ADR 0097: Studio Source Tooling Boundary

**Status:** Accepted deferral for OrnaDB 1.0

## Decision

This decision resolves the Orna 1.0 scope of Studio source tooling requested in
GitHub issue #7.

Orna 1.0 accepts its portable semantic-diff, raw Git-diff, and retained
source/revision inspection contracts. It does not define a Studio source
editor, Studio apply workflow, or revision-browser interface. Those Studio
product surfaces, including any public revision activation or restoration
behavior, are deferred until a versioned contract defines them. This ADR does
not add commands, mutation semantics, or UI behavior beyond the frozen
reference.

The defined v1 boundary is:

* `orna diff` preserves the corresponding Git operation's observable semantics
  (**ORNA-CLI-001**). Semantic diff SHOULD report schema, keyed-row, result,
  and dependency changes where available; raw Git diff remains available; and
  physical-only representation changes remain distinguishable
  (**ORNA-DIFF-001** through **ORNA-DIFF-003**).
* A stable object identity survives ordinary edits and semantic renames, a
  revision identifies one immutable definition, and snapshot-pinned references
  resolve against their pinned snapshot (**ORNA-SYS-017**, **ORNA-SYS-019**).
  Historical system queries remain snapshot-pinned and expose retained
  observations without fabricating missing data (**ORNA-SYS-011**,
  **ORNA-SYS-012**). Semantic hashes exclude formatting while source hashes
  identify retained source bytes (**ORNA-SYS-034**). A semantic diff classifies
  rename/rekey only when identity evidence supports it (**ORNA-SYS-051**).
* The portable system reference exposes read-only `sys.source` and `sys.history`
  operations. They return retained source/maps subject to redaction and stable
  object/file revision history; they do not define a Studio browser or apply
  operation.
* Any defined administrative state change still follows its declared
  transaction, precondition, failure, audit, validation, and failed-operation
  boundary (**ORNA-ADMIN-001** through **ORNA-ADMIN-003**;
  **ORNA-SYS-085** through **ORNA-SYS-087**). These general rules do not define
  a Studio source-apply API.

## Search evidence

The frozen corpus was searched across `Orna-1.0.0.md`, `source/`, `grammar/`,
`tests/`, `examples/`, and `api/` with:

```text
rg -n -i '\bStudio\b|source apply|interactive apply|revision browser|source editor|apply source' Orna-1.0.0.md source/ grammar/ tests/ examples/ api/
```

The search returned no matches. The relevant positive matches are the
requirements cited above in `source/18-cli.md`, `source/25-evolution.md`,
`source/15-system.md`, `source/16-administration.md`, and the read-only API
entries in `source/34-system-reference.md`. The reference therefore supports
the portable CLI/API contracts while leaving Studio-specific apply and browsing
behavior unspecified.

## Relationship to existing decisions

Work ADR 0003 keeps retained revision-pair listing internal and explicitly
defers a public revisions-list command and Studio revision browser. Work ADR
0038 accepts the installed local `orna source apply` command against an
expected active base; work ADR 0066 accepts read-only semantic source diff and
explicitly defers interactive apply. This ADR links issue #7 to those existing
boundaries and does not duplicate or change their CLI contracts. Existing
Studio presentation decisions do not create a source editor, Studio
source-apply workflow, or public revision-activation contract.
