# Module-qualified access without `use`

Status: decision record for bead ornadb-5il1.199 residual (W03-03).
Spec authority: `reference/Orna-1.1.0/`.

## Question

A reference such as `telemetry.rig.trips()` (a full module path, no `use`
in the referring module) fails with `ORNA-S012-UNRESOLVED`, while
`use telemetry.rig;` followed by `rig.trips()` checks clean. Must the full
path resolve from any module without an import?

## Reproduction

Project: `telemetry/rig.orna` declares `pub fn trips()`; `main.orna` contains
`pub fn main() = telemetry.rig.trips();` and nothing else.

Built from origin/main `aaa8b4404` (`cargo build --locked -p orna-cli --bin
orna`, `orna 0.1.0`):

- `orna check` with the full-path body: `ORNA-S012-UNRESOLVED: name cannot be
  resolved`, exit 1. The same failure occurs for `telemetry.rig.Reading |
  count()` and for the bare value `telemetry.rig.trips`.
- `main.orna` = `use telemetry.rig;` + `pub fn main() = rig.trips();`:
  `project valid`, exit 0.

## Root cause

The diagnostic is emitted by the HIR layer, before the checker runs.
`crates/orna-hir/src/lower/imports.rs` `Builder::resolve_path` resolves a
qualified path only when its first segment is a module root bound in the
referring module's scope (`scope.module(first)`), or a value. The implicit
roots are only `sys` and `std` (`implicit_roots`). For `telemetry.rig.trips`
the first segment `telemetry` is unbound, so the result is
`PathKind::NotAModule`; `crates/orna-hir/src/lower/calls.rs` `classify_path`
then reports the unresolved name, which `crates/orna-check/src/hir_check.rs`
surfaces as `ORNA-S012-UNRESOLVED`.

`use telemetry.rig;` binds the final component only (ORNA-IMPORT-001), so the
path must be spelled `rig.trips`. That is why the `use` form checks clean.

## What the spec says

Normative clauses that govern how a namespace becomes reachable from source:

- `source/03-source-modules.md` ORNA-IMPORT-001: `use a.b;` MUST make
  namespace `a.b` available under its final component unless it conflicts.
  This is the only clause that states how a loaded namespace becomes
  available to a module.
- ORNA-IMPORT-002 / 003 / 004: alias, wildcard and selective forms bind names
  into the importing module's scope.
- ORNA-IMPORT-007: wildcard imports exposing the same otherwise-unresolved
  name produce an ambiguity diagnostic; import order MUST NOT select a winner.
- `source/17-repl.md` ORNA-REPL-001: the REPL MUST use ordinary `use` syntax
  for namespace access.
- ORNA-NS-001 / ORNA-NS-002 / ORNA-NS-003: these define a namespace's
  identity from file and directory paths. They state what a namespace is
  named, not whether it is visible from another module.
- ORNA-NS-005 / ORNA-NS-006 / ORNA-NS-007: `sys` and `std` are the reserved
  top-level namespaces; `sys` MUST be built into every implementation; `std`
  is optional and replaceable.

Clauses that mention qualified names but do not decide this question:

- ORNA-FN-002 (`source/06-expressions.md`): a qualified function name without
  parentheses denotes the function value.
- ORNA-RUN-001 (`source/06-expressions.md`): `orna run <qualified-function>`
  MUST invoke the named public function. This governs the CLI entry point.
- ORNA-LEX-008 (`source/04-lexical.md`): enum variants remain
  namespace-qualified in patterns unless explicitly imported. Stated for enum
  variants in patterns only.
- ORNA-REF-002 (`source/07-tables.md`): `vehicle.owner.key` access through a
  reference field. It concerns row access, not module scope.

No normative clause states that a loaded module is addressable by its full
path without `use`.

## Non-normative and conflicting evidence

- `examples/` are informative (`01-scope.md`). The single-file examples
  `examples/valid/parallel-streams.orna`, `function-values-and-pipelines.orna`,
  `parallel-function-values.orna` and `examples/invalid/wrong-field-type.orna`,
  `ambiguous-consumer.orna` reference `mail.google.sync`,
  `vehicle.corsa.freematics.sync` or `vehicle.corsa.Reading` with no `use`,
  and no fixture in those files provides the named module. They are syntax
  examples; they cannot establish semantic resolution of a full path.
- `tests/conformance-manifest.json` has `resolve: pass` entries for the valid
  examples above. These are conformance expectations, not normative clauses,
  and they cannot override the `use` rules.
- The complete example projects import every module they reference before use.
  `examples/reference/main.orna` begins `use library; use warehouse; use
  sensors; use values;`.
- Code comments that describe full-path resolution from any module are stale
  with respect to the default path:
  `crates/orna-check/src/calls.rs` `module_member_namespace` (doc at ~2276-2284,
  "with or without `use mail.google;`") and
  `crates/orna-conformance/src/semantic_adapter/project_graph.rs:62-64`. For an
  unbound root, the HIR `resolve_path` returns before the checker's unbound
  branch is reached, so that branch is not exercised by `orna check`.
- `CHANGELOG.md` `## Unreleased`, line 784, records the project rule: "Module
  names are resolved through `use` only (ORNA-IMPORT-001, ORNA-REPL-001). A
  module namespace that is reachable only through another module's `use` is
  not in scope; write `use telemetry.rig;` before `rig.Reading`."
- `crates/orna-cli/tests/module_tables.rs` pins the `use`-bound form and the
  CLI `run telemetry.rig.trips` entry point.

## Decision

The current behaviour is kept: a module namespace reached by a full path is in
scope only after a `use` in the referring module binds its final component.
Full-path references without an import continue to produce
`ORNA-S012-UNRESOLVED`.

Basis: ORNA-IMPORT-001 is the only clause that makes a loaded namespace
available to a module, and ORNA-REPL-001 requires namespace access through
`use`. The complete example project follows the same pattern. No checker,
project-loader or evaluator change is made under this decision.

## Follow-up (not done here)

- The stale doc comments listed above should be corrected to describe the
  `use`-bound rule, or the code should be aligned with them if the project
  later decides on the full-path rule. Either change is out of scope for this
  decision.
- If the spec is later amended to allow unimported full-path access, the
  change lives in `crates/orna-hir/src/lower/imports.rs` (`resolve_path`), with
  checker support in `crates/orna-check/src/calls.rs` and project-graph support
  in `crates/orna-project`. The `use`-only pin in
  `crates/orna-cli/tests/module_tables.rs` must change with it.
