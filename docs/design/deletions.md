# Deletion candidates (Stream A)

Status: investigation record. No code is deleted by this document. Every
candidate below has at least one consumer that is either a spec-visible
surface, a gate selector, a budget or manifest row, or a structural decision
owned by the OrnaDB owner. Each entry lists the consumers found, the spec
clauses involved, the gate impact, the measured size, and a verdict.

Measurement basis: tree at origin/main 026faee90 (worktree
`trees/deletions-streamA`). Consumers were found by `grep` over `crates/`,
`scripts/`, `docs/`, and `.github/`, and by `cargo metadata` reverse
dependencies (normal, dev, and build edges). Line counts are `wc -l` over
`.rs` files unless stated otherwise.

## Summary

| # | Candidate | Size | Verdict |
|---|-----------|------|---------|
| 1 | sys.explain planner: `orna-sys` `introspection.rs` and 74 `planner_*` tests | 17,855 + 47,981 lines | Needs owner decision |
| 2 | `crates/archive/orna-compat-v1` (1.0.0 archive) | 62,108 lines, 50 files | Needs owner decision |
| 3 | `orna-table` (normal dependent: `orna-conformance` only) | 2,829 lines | Needs owner decision (layering stream) |
| 4 | 104 `repl-inline-*.orna` fixtures | 104 files, 657 bytes | Keep |
| 5 | Filler std modules in pinned `stdlib/std` | about 932 lines in 10 files | Needs owner decision and std-repo change |
| 6 | Dead code: `allow(dead_code)` sites, `sys.admin.*` retired descriptors | 24 sites | Keep (retired descriptors are spec-visible) |
| 7 | `orna-sys-codegen` | 4,144 lines | Keep (build dependency) |
| 8 | Binaries with no normal dependents: `orna-cli`, `orna-lsp`, `orna-traceability` | n/a | Keep (they are the shipped tools) |

Delete-now set: **empty**. No candidate is safe to remove without an
owner decision, because each one either changes a spec-visible surface, drops
gate selectors or budget rows, or removes evidence the owner has to accept.

## 1. sys.explain planner

### Consumers (production)

- `crates/orna-evaluator/src/explain_bindings.rs` (369 lines) is the only
  production caller of `orna_sys::explain_query` and
  `orna_sys::explain_function` (imports at lines 24-27; call sites at 154 and
  181).
- `explain_query_plan` (lines 128-160) lowers only the `distinct` and `take`
  operators. Every other plan node returns `VersionIncompatible`. It calls
  `explain_query` with `ExplainInputs::default()`, so the fold and chain
  inputs are never set in production (`introspection.rs:1683-1692`).
- `explain_function_plan` (lines 162-181) is unreachable in practice. The only
  `ExplainCatalogue` implementation is `ActivationExplainCatalogue`, and its
  `resolves_functions()` returns the default `false`, so every function
  explanation returns `VersionIncompatible`.
- `ExplainInputs` (`introspection.rs:1603`) is constructed only with
  `default()` in production. Fold and chain inputs are constructed only in
  the `orna-sys` `planner_*` tests.
- No other production crate calls `introspection` items. `cargo metadata`
  reverse dependencies on `orna-sys` do not show a planner consumer outside
  `orna-evaluator`.

### Consumers (tests and evidence)

- 74 `crates/orna-sys/tests/planner_*.rs` files (47,981 lines) plus
  `query_plan_hint_statistics_azb4m.rs`. These exercise the fold, chain,
  spill, window, decorrelation, and disjunct/conjunct limit paths.
- `crates/orna-check/tests/planner_dependency_introspection.rs` (cites
  ORNA-PLAN-002 and ORNA-PLAN-003).
- `crates/orna-evaluator/tests/explain_runtime.rs` (cites both IDs),
  `table_relation_streaming_audit.rs` (cites both IDs),
  `sys_provider_linkage.rs`, `standard_profile_v1.rs`.
- `crates/orna-cli/tests/sys_explain_run.rs` with
  `crates/orna-cli/tests/fixtures/sys-explain-run.orna`. The fixture uses
  `sys.Storage|distinct|take`, and the filter expects failure.
- `docs/conformance/validation-pass-2026-11-03.md` names
  `planner_decorrelated_anchor_fold_identity_wj87v` as a passing row.

### Spec impact

- `reference/Orna-1.1.0/source/08-relations.md` (ORNA-PLAN-002, line 91;
  `Orna-1.1.0.md` line 1946): planner choices MUST be introspectable through
  `sys.Plan`, `sys.Storage`, `sys.Materialization`, and `orna explain`.
- ORNA-PLAN-003: every optimisation MUST have a correct scan or reference
  fallback.
- `reference/Orna-1.1.0/tests/requirement-evidence.json` (around line 6441
  and 6457): both IDs are `implementation_result: not-executed`, with
  `full_implementation_coverage_claimed: false`. The obligation text is the
  spec requirement above.
- `api/sys.json` and `source/34-system-reference.md` (around lines
  2672-2700) define three `sys.explain` overloads. The function overload is
  the one that can never succeed in production.

### Gate and citation impact

- `crates/orna-traceability/citation-baseline.json` ratchet: `dropped_ids`
  fails if a cited ID disappears from the scan. ORNA-PLAN-002 and -003 are
  also cited from the orna-check, orna-cli, and orna-evaluator test files
  above, so deleting only the orna-sys planner tests would not drop the
  IDs. Deleting every citing file would.
- `scripts/architecture-budget.toml` line 131 has a budget row for
  `src/physical/tests.rs::planner_checks_base_drops_existing_projections...`.
  That row is in a different crate, so it is not affected by the orna-sys
  planner deletion.

### Options (owner decision required)

- **A. Honest minimal explain (recommended to decide on).** Keep `sys.explain`
  for `distinct` and `take` and keep `sys.Plan` and `sys.Storage`
  introspection on the real executor. Remove the fold and chain paths from
  `ExplainInputs` and the 74 planner tests that only drive them. Keep
  `explain_function` only if the owner accepts that its overload stays a
  constant `VersionIncompatible`; otherwise remove the function overload and
  its `sys.json` entry (spec change). Requires an owner ruling on whether the
  fold and chain paths are required by ORNA-PLAN-003 fallback evidence.
- **B. Keep the planner.** Accept 17.8k lines of planner code plus 48k test
  lines as the spec's planner evidence even though production uses only two
  operators. Costs maintenance; no behaviour change.
- **C. Move the planner tests to an archive branch.** Removes 47,981 test lines
  from the workspace. Requires a selector change for every named planner test
  in the validation documents.

Lines reachable only through the fold and chain inputs were not measured; that
needs a call-graph tool. The 17,855 figure is the whole file.

## 2. crates/archive/orna-compat-v1 (1.0.0 archive)

### Consumers

- `cargo metadata`: normal dependents none; the only edge is the
  `orna-conformance` dev-dependency (`crates/orna-conformance/Cargo.toml:40`).
- `crates/orna-conformance/tests/sys_api_compatibility.rs` uses
  `orna_compat_v1::revision::ArtifactCompatibilityCoordinates`.
- `crates/orna-conformance/tests/traceability_diff_trust_secret_batch_three.rs`
  uses `CatalogueRevisionId`, `SchemaId`, `catalogue::CatalogueSnapshot`,
  `catalogue::QualifiedSemanticName`, `catalogue::SchemaDefinition`, and
  `catalogue_diff::{SemanticChange, catalogue_diff}`.
- The crate's own test files (`inspect_contract`, `sealed_engine_registry`,
  `security_execute_denial`, `semantic_diff`, `types_conformance`) run only
  under the workspace test command.
- The crate's README states it is the frozen 1.0.0 reference model, "not a
  production component", and that "nothing in the shipped binary may depend
  on it".

### Gate and manifest impact

- `scripts/architecture-budget.toml` line 101: a `[[crate]]` block for
  `orna-compat-v1` with about 50 per-file line budgets.
- `scripts/architecture-budget-raises.toml` line 59: a raise for
  `crates/archive/orna-compat-v1/src/security.rs`.
- `scripts/fixture-manifest.sha256`: about 50 manifest rows under
  `crates/archive/orna-compat-v1/tests/fixtures/`.
- `docs/design/layers.toml` line 35 and line 170: layer 15, role `archive`.
  Line 20 states "nothing may depend on it".
- `docs/ARCHITECTURE.md` line 47 and 55-59: the crate is described as the
  archive layer.
- `reference/Orna-1.1.0` has no clause that requires the crate, but the spec
  treats compatibility archives as old-reader inputs
  (`Orna-1.1.0.md`, "Persisted rows and path projections").

### Options

- **A. Move to an archive tag or branch.** Remove the crate from the workspace
  and from `layers.toml`, budgets, and manifest rows. Move the two conformance
  tests' needs (artifact coordinates, catalogue snapshot and diff) either into
  conformance fixtures or into a tag the tests fetch. Requires updating the
  conformance selector list for the two test files named above. Risk: the
  catalogue diff and artifact coordinate logic has no production equivalent;
  if the owner keeps the two conformance tests, they need a replacement model.
- **B. Keep** until the 1.0.0 archive is no longer used as an old-reader
  reference.

Verdict: needs owner decision. The crate is not shipped, so there is no
runtime risk from keeping it, but it is 62k lines of maintenance and budget
noise.

## 3. orna-table

- `cargo metadata`: the only normal dependent is `orna-conformance`
  (`src/semantic_adapter.rs:65`; `tests/table_transaction.rs`). No shipped
  crate depends on it.
- `docs/design/layers.toml` V3 says merge `orna-table` into conformance at
  stage 13. That restructuring belongs to the layering stream and is not a
  deletion.

Verdict: needs owner decision and is owned by the layering stream. Not
deleted here.

## 4. repl-inline-*.orna fixtures

- 104 files: 38 in `crates/orna-evaluator/src/fixtures/`, 3 in
  `crates/orna-evaluator/tests/fixtures/`, and 63 in
  `crates/orna-project-repl/src/fixtures/`. Each is one line, 657 bytes in
  total.
- Every name is referenced by `include_str!` in the crate sources (104 of 104
  names referenced; 0 unreferenced).
- No two files share content (104 distinct SHA-256 digests).
- Each corresponds to a manifest row in `scripts/fixture-manifest.sha256`.

Verdict: keep. The only deletion lever would be folding the 104 one-line files
into one table-driven fixture. That would drop about 103 manifest rows, but
saves 657 bytes and changes the test shape. Low value.

## 5. Filler std modules (pinned stdlib/std, read-only)

The std repository is separate. This section lists modules and does not edit
them. The `stdlib/std` gitlink must stay at main's pin.

| Module | Lines | Consumers |
|--------|-------|-----------|
| `generics.orna` | 26 | `orna-standard-sources/src/lib.rs`; `orna-evaluator/tests/standard_library_generics_type_utils.rs`; `standard_profile_v1.rs`; 2 fixtures |
| `memo.orna` | 37 | `orna-standard-sources`; `standard_library_memoized_functions_82yit.rs`; `standard_profile_v1.rs` |
| `lazy.orna` + `lazy/adapters.orna` | 34 + 59 | `orna-standard-sources`; about 14 fixtures under `orna-evaluator/tests/fixtures/` |
| `iteration.orna` | 111 | `orna-standard-sources`; `standard_library_iteration_traversal_ptr1b.rs`; `docs/design/std-native.md` |
| `iterator.orna` + `iterator/adapters.orna` + `iterator/consumers.orna` | 248 + 77 + 143 | `orna-cli/tests/project_runtime.rs`; about 35 references outside the std repo; orna-cli fixtures |
| `type_utils.orna` | 50 | `orna-standard-sources`; `standard_library_generics_type_utils.rs`; `standard_profile_v1.rs`; 2 fixtures |
| `result.orna` | 147 | 28 references outside the std repo, including `orna-check/tests/semantic_graph.rs`, `intrinsic-diagnostics.snapshot`, and conformance expected-diagnostic fixtures |

Spec notes:

- ORNA-TYPE-006 and ORNA-ERR-009 (`source/05-types.md:55`,
  `source/06-expressions.md:409`) say `Result`, `Ok`, and `Err` are not reserved
  core constructors and that there is no intrinsic `Result<T,E>`. A library
  `result.orna` is not forbidden by those clauses, but it is a public std
  surface and the owner must decide whether it stays.
- `iteration.orna` and `iterator.orna` overlap by name. Overlap of contents
  was not measured.

Verdict: needs owner decision, then a change in the std repository and a
gitlink bump in this repo. Deleting these modules also changes the
`orna-standard-sources` embed list (one `include_str!` macro site that lists
every module).

## 6. Dead code

- The compiler and clippy were not run for this document; a dead-code sweep
  needs a build, which was out of scope for an investigation.
- `allow(dead_code)` appears at 24 sites. Most are test-support modules or
  platform-gated items (`cli_mount.rs`, `media_range.rs`, `host_filesystem.rs`,
  `repository` test support, LSP and CLI test helpers) and are kept.
- `crates/orna-sys/src/system_api.rs` lines 858-885:
  `sys_admin_set_storage_preference` and `sys_admin_rewrite_storage` are
  `#[allow(dead_code)]` functions whose bodies panic. They are registered
  through `#[ornasys]`, which generates
  `crates/orna-check/generated/sys_api.json` lines 15 and 20. The spec names
  both in `reference/Orna-1.1.0/api/retired.json`, and
  `crates/orna-check/tests/system_storage_contract.rs:56` tests the retired
  name through `fixtures/publication/sys-admin-retired-storage-preference.orna`.
  Verdict: keep. Removing them changes the generated surface and the
  retired-name diagnostics.
- `crates/orna-evaluator/src/regex_lite.rs` has a crate-level
  `#![allow(dead_code)]` at line 14. Not examined further in this pass.

## 7. orna-sys-codegen

- `crates/orna-sys/build.rs:6` uses `orna_sys_codegen::{build_host, build_provider,
  build_support}`. The crate is a build-time dependency of `orna-sys`, and
  `orna-sys` tests also use it.

Verdict: keep. `crates/orna-sys/Cargo.toml` lists `orna-sys-codegen` under both
`[build-dependencies]` (used by `build.rs`) and `[dev-dependencies]`, so it has a
live build edge, not only a test edge.

## 8. Binaries with no normal dependents

- `orna-cli` (binary `orna`), `orna-lsp` (binary `orna-lsp`), and
  `orna-traceability` (binary `orna-traceability`) have no normal dependents
  because they are leaf tools. Verdict: keep.

## Verification performed

- Repl fixture reference count: `grep -rhoE` over `crates/` against the
  `find` listing (104 files, 104 referenced, 0 duplicates).
- `cargo metadata` reverse-dependency listing over workspace members (normal,
  dev, and build kinds).
- Spec reads: `reference/Orna-1.1.0/tests/requirement-evidence.json`,
  `source/05-types.md`, `source/06-expressions.md`, `api/retired.json`.
- Gate and manifest reads: `scripts/architecture-budget.toml`,
  `architecture-budget-raises.toml`, `fixture-manifest.sha256`,
  `docs/design/layers.toml`, `crates/orna-traceability/citation-baseline.json`.

No cargo build, test, or clippy was run; this is an investigation document.

## Open questions for the owner

1. Option A, B, or C for the sys.explain planner (section 1), including whether
   the fold and chain paths in `ExplainInputs` are required by ORNA-PLAN-003
   fallback evidence.
2. Whether `sys.explain` as a function overload stays in the spec or is
   retired. Today it can never succeed in production.
3. Whether the 1.0.0 compat archive moves to a tag (section 2 option A) and
   what replaces the two conformance tests' needs.
4. Whether `orna-table` merges into conformance now or waits for the layering
   stream.
5. Whether the filler std modules (section 5) are removed in the std repo,
   and whether a library `Result` remains a supported std surface.
