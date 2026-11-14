# HIR cutover plan (stage H9)

Status: plan, not yet executed. Bead `ornadb-csenl.24` (H9: "cutover and
deletion of the old pipeline; AST cleanup"), blocked by `ornadb-csenl.23` (H8),
which is blocked by `.22` (H7), which is blocked by `.21` (H6). All ten beads
`ornadb-csenl.15` (H0) to `.24` (H9) were `OPEN` when measured.

This document turns section 6.3 (H9) of [hir.md](hir.md) into measurable exit
criteria, a shadow-period procedure, an ordered cutover with a rollback path, a
deletion schedule, and the decisions the owner must make before deletion. It
does not delete anything and does not change behaviour.

Measured at `origin/main` `0f1e5961d` (this branch's base). Ratchet numbers come
from running the tests named in section 9. Line counts are `wc -l` or `cat | wc
-l` over the named paths. Figures taken from other documents are labelled with
their source. Anything not measured is marked `not measured`.

## 1. Where the ratchets stand

| Ratchet | Test | Measured now | Recorded in file | What the test enforces |
|---|---|---|---|---|
| Diagnostic codes, old checker vs HIR | `crates/orna-differential/tests/diagnostic_codes.rs`, 211 cases, 0 skipped | 104 cases differ | 104 rows under `## expected` in `crates/orna-differential/tests/diagnostic_codes_differing.txt` | the differing set equals the expected rows exactly; a listed case that now agrees fails as stale |
| Executor agreement (H4) | `crates/orna-differential/tests/exec_share.rs`, corpus of 226 cases | 54 of 70 run cases agree; 110 evaluable; 49 of 54 agreeing run cases have exact step parity | `agreed = 54`, `evaluable = 110`, `steps-exact = 49`, `old-harness-environment = 10`, and 15 allowances (10 `harness-allowance`, 1 `representation-allowance`, 4 `executor-gap-allowance`) in `crates/orna-differential/tests/differential/exec-share.toml` | `agreed` and `steps-exact` may only grow; `old-harness-environment` may only shrink; `evaluable` is the denominator |
| Typed share (H2) | `crates/orna-differential/tests/typed_share.rs` | 49 of 104 old-accepted cases typed; 14 not lowered; 108 old-rejected cases excluded | `typed = 35`, `lowered = 104` in `crates/orna-differential/tests/differential/typed-share.toml` | see the comparator note below |
| REPL inline (H8) | `crates/orna-differential/tests/repl_inline_differential.rs` | 15 of 48 runnable fixtures agree; 104 fixtures in total; 56 excluded; 33 expected disagreements | `AGREED_FLOOR = 15` in the test; 33 rows under `## expected` in `crates/orna-differential/tests/repl_inline_differing.txt`, each with a one-line reason; 56 excluded in `crates/orna-differential/tests/repl_inline_exclusions.txt` | `agreed >= 15`; the disagreement set equals the 33 rows |
| Expected differences (harness allowlist) | `crates/orna-differential/tests/differential/expected-differences.toml` | 98 `[[difference]]` and 9 `[[environment]]` entries; 856 lines | 107 `reason` and 107 `bead` fields | not checked: no test found that verifies each cited bead exists (gap G19) |

Notes on the table:

- **Executor arithmetic.** The 16 non-agreeing run cases (70 - 54) are the 15 ratchet allowances (10 + 1 + 4) and the one `currency-allowance` (`valid/currency.orna`), which the ratchet count does not include. The test's printed reason breakdown sums to 156, which matches neither 70 nor 226; the test does not state its denominator, and this doc does not reconcile it.
- **Typed-share comparator (fixed, gap G3).** `typed_share_ratchet` in
  `crates/orna-differential/src/compare.rs` fails when `typed` falls, when `typed`
  rises, and when `lowered` changes. The recorded `typed` is 51, the value measured
  at af9a80627 with the pinned stdlib (51 of 104 old-accepted cases typed). The
  earlier figure of 35 was stale.
- **Diagnostic-code rows carry a reason (fixed, gap G2).** Every row of
  `diagnostic_codes_differing.txt` is `<case> :: <checker codes> => <HIR codes> ::
  <reason>`, and the test fails on a row without a reason. Criterion C1(b) holds.
- **REPL rows carry reasons.** Each of the 33 rows in
  `repl_inline_differing.txt` has a one-line reason. That is the format the
  diagnostic file lacks.

## 2. Consumers and what they call today

From `cargo metadata` (normal dependencies only) and grep of `crates/`:

| Old crate | Normal reverse dependencies | Production call sites (verified) |
|---|---|---|
| `orna-check` | application, cli, conformance, differential, evaluator, lsp, project, project-repl | cli `assertion_gate.rs:66` (`orna_check::analyze_with_providers`); cli `main.rs`, `cli_commit.rs`, `cli_mv.rs`, `cli_serve*.rs`, `fuse_mount/source_write.rs`; lsp `project.rs`, `server/standard.rs`; project `attachments`, `committed_store`, `object_identity`, `table_storage`, `typed_row`; application `lib.rs` (`orna_check::Type`, `ModuleHeader`) |
| `orna-evaluator` | application, cli, conformance, differential, live, project-repl, runtime | application `lib.rs` (`EffectGate`, `ActivationExplainCatalogue`, `SysHostBindingRegistry`); runtime `lib.rs:6780` (`impl orna_evaluator::EffectHandler for QuerySessionEffects`) and `lib.rs:11304` (`EffectGate::admit_unbound_for_test`, in a test); live `lib.rs:30` (`ActivationEffectReport`) |
| `orna-project-repl` | application, cli, conformance, differential | cli `repl.rs`, `cli_serve_app.rs`, `main.rs` (`AdmittedReplSession`) |
| `orna-intrinsics` | application, check, evaluator | deleted with the evaluator (section 6.2) |
| `orna-exec` (HIR executor) | differential, project, repl | already a production dependency of project and repl |
| `orna-compat-v1` | none in production; dev-dependency of conformance only | `crates/archive/orna-compat-v1`: 50 files, 62,108 lines |
| `orna-table` | conformance only | `crates/orna-table`: 2,829 lines of `.rs` |

Consequences:

- The `orna-check` **types** (`Type`, `ModuleHeader`, `ModuleInput`,
  `TableSchema`) are used in production by project, application, lsp and cli.
  `hir.md` section 10.2 deletes `orna-check/src/types.rs` in H3, and section
  10.2 also says `orna-check` keeps its name and API. Both cannot hold without
  moving the types. This is decision Q6 (section 7).
- `orna-evaluator` cannot be removed while application, runtime, live and
  project-repl import it. Removal is a migration of those callers first (step
  PRs of section 5), not a Cargo edit.
- `orna-differential` depends on the old evaluator (normal dependency, with the
  `route-trace` feature, `crates/orna-differential/Cargo.toml:19`). Deleting the
  old evaluator ends the harness unless the old outputs are frozen first (gap
  G11).

### 2.1 Switches that exist today

Grep of `crates/` for `ORNA_CHECK_` finds two variables, both read in
`crates/orna-cli/src/cli_check_backend.rs`:

- No variable: the old checker runs (`Backend::Old`, the default).
- `ORNA_CHECK_HIR=1`: `orna check` runs HIR.
- `ORNA_CHECK_SHADOW=1`: both run. The old verdict is reported. Differences go
  to stderr as `orna check shadow: checker-only ...` and `orna check shadow:
  hir-only ...`. The command never fails for a difference. Shadow wins when both
  variables are set.

Discrepancies with `hir.md`:

- `hir.md` section 6.3 named an `ORNA_CHECK_LEGACY` variable that does not
  exist. Corrected in this PR: it names `ORNA_CHECK_HIR` and `ORNA_CHECK_SHADOW`,
  which exist. The old-path switch is decision Q7.
- `hir.md` section 6.6 said H1 (LSP) is gated by an environment variable. No gate
  exists in `crates/orna-lsp/src` (its `env::var_os` reads are `XDG_CACHE_HOME`
  and `HOME`, `standard.rs:290-292`). Corrected in this PR: the LSP gate is not
  yet implemented, tracked under `ornadb-csenl.16` (gap G8).
- No switch exists for the assertion gate, `orna run`, the REPL, `serve` or the
  mount (gaps G6, G7). The per-entry switches in section 4.1 are new work.

## 3. Exit criteria per consumer

A consumer's HIR path becomes the default when every criterion in its row holds.
Each criterion is a number that a command prints or a file records.

| Consumer | Stage | Criteria (all required) | Measured now |
|---|---|---|---|
| C1 `orna check` | H3 | (a) diagnostic-code differing rows = 0, or each remaining row names a bead and the owner accepts it as an intended difference (decision Q11); (b) every row has a reason field (gap G2, fixed in this PR); (c) the shadow report shows zero unlisted differences on two consecutive nightly runs (section 4); (d) criterion C6 holds | 102 rows, each with a reason field; no nightly shadow report |
| C2 assertion gate (`assertion_gate.rs`) | H9 step 1 | (a) a switch exists (gap G6); (b) the gate's own tests pass with the HIR backend; (c) zero shadow differences on two consecutive nightly runs | no switch; calls `orna_check::analyze_with_providers` directly |
| C3 `orna run` (executor) | H4 to H6 | (a) `agreed + allowances = evaluable` with every allowance naming a bead; (b) `steps-exact = agreed`; (c) zero shadow differences on two consecutive nightly runs; (d) the step re-baseline of `hir.md` section 6.4 is recorded in the flip PR | agreed 54; evaluable 110; allowances 10 + 1 + 4 = 15; steps 49 of 54 |
| C4 REPL | H8 | (a) `agreed` equals the 48 runnable fixtures (currently 15); (b) the 33 expected rows are empty, or each names a bead; (c) the 56 excluded fixtures stay listed in `repl_inline_exclusions.txt` with a reason; (d) zero shadow differences on two consecutive nightly runs | 15 of 48; 33 expected; 56 excluded |
| C5 `serve` and mount | H9 steps 4 and 5 | (a) a switch exists (gap G7); (b) the serve and mount tests pass with HIR; (c) zero shadow differences on two consecutive nightly runs. None of the three ratchet files of section 1 covers these two entry points (gap G7) | no switch; no ratchet |
| C6 typed share | H2 | `typed` equals the measured value on the old-accepted corpus, and `typed-share.toml` records it in the same PR | measured 51 of 104; file records 51; comparator exact both ways (gap G3, fixed in this PR) |
| C7 LSP | H1 | (a) the LSP conformance suite (`crates/orna-lsp/tests/ji3t0_conformance.rs`) passes against HIR; (b) no production import of `orna_check` remains in `crates/orna-lsp` | not verified; the LSP gate is not yet implemented (gap G8; `ornadb-csenl.16`) |
| C8 allowlist hygiene | H9 | every `expected-differences.toml` entry names a bead that exists; no `[[difference]]` lacks a reason; the entry count is recorded in the PR | 107 entries, 107 reason and 107 bead fields; bead existence not checked (gap G19) |
| C9 benchmarks | H9 | the benchmarks of `hir.md` section 7.4 meet their targets in CI | not measured here (gap G13) |
| C10 dependency | H9 | `cargo metadata` shows no `orna-evaluator`, `orna-intrinsics`, `orna-project-repl` or `orna-check` types needed by production (and `orna-compat-v1` removed from the production graph) | `orna-evaluator` present |

The H9 acceptance in `hir.md` section 6.3 is C1 to C10 together, plus "two
consecutive nightly runs with zero harness differences outside the allowlist".

## 4. Shadow period mechanics

### 4.1 Modes

Each entry point has three modes, with the same names:

- `old` (default before the flip): the old pipeline runs and is authoritative.
- `shadow`: both pipelines run on the same input. The old result is returned to
  the user unchanged. Each difference is reported. The command never fails
  because the pipelines disagree.
- `hir`: HIR is authoritative.

`orna check` already implements these modes with `ORNA_CHECK_SHADOW` and
`ORNA_CHECK_HIR` (section 2.1). The other entry points need the same pair. The
proposed names are `ORNA_<ENTRY>_SHADOW` and `ORNA_<ENTRY>_HIR` per entry, which
leaves `orna check`'s names unchanged. The naming is decision Q7.

### 4.2 Reporting

- Per invocation: the shadow report goes to stderr, one line per difference, in
  the shape `orna <entry> shadow: <side>-only <key>`. `orna check` already writes
  this shape. The key is the entry's natural unit: the diagnostic code with its
  logical path and line for check; the case name with the canonical result or
  error code for run and REPL.
- Per nightly run: the `shadow-report` job of `nightly-gates.yml` runs the `shadow_report` test (`crates/orna-differential/tests/shadow_report.rs`, ignored by default) over the whole corpus through the old and HIR pipelines, with `ORNA_SHADOW_REPORT` naming `shadow-report.md`. The test writes a markdown report, which the job uploads as the `shadow-report` artifact and appends to the job summary. To run it locally: `ORNA_SHADOW_REPORT=shadow-report.md cargo test -p orna-differential --test shadow_report --locked -- --ignored --nocapture`. A difference is unexplained when the matching ratchet test would fail on it: the diagnostic expected-row table, the executor allowance table, or the REPL expected-row file. A ledger row that matches no case is stale. The job fails on any unexplained difference or stale row. It is not part of the evidence summary.

What exists today: `.github/workflows/nightly-gates.yml` runs on `cron: "23 1 * * *"` under `on.schedule`. Its `workspace` job's `rest` matrix entry runs `cargo test --locked --no-fail-fast` over `--workspace` minus `orna-cli`, `orna-evaluator`, `orna-sys` and `orna-conformance`. The ratchet tests of section 1 therefore run nightly, through the `rest` shard (for `orna-differential` and `orna-repl`). The `shadow-report` job adds the whole-corpus comparison (gap G9). It has not yet run nightly, so criterion C1(c) is not yet met.

### 4.3 Triage

- Each difference is triaged by the owner of the stage that owns the checked
  behaviour: H3 for check, H4 to H6 for run, H8 for the REPL, H9 for the rest.
- Following `hir.md` section 9.1, a production difference first becomes a corpus
  case. The case then gets a fix, or an entry in the expected file with a reason
  and a bead. An allowlist entry without a bead fails review.
- The triage SLA is decision Q8.

### 4.4 Duration and exit

- An entry leaves shadow when criteria C1 to C10 for it hold and the nightly
  job reports zero unlisted differences on two consecutive nightly runs (the
  rule of `hir.md` section 6.3, stated per entry).
- `hir.md` sets no calendar minimum. Whether to add one is decision Q8.
- After the flip, shadow stays on until the old path for that entry is deleted,
  so a regression appears as a nightly difference rather than a user report.

## 5. Cutover order, default flip and rollback

### 5.1 Order

The order is `hir.md` section 6.3, with `orna check` first because its switch
already exists:

0. `orna check` (H3). Flip the default after C1 holds.
1. Assertion gate (`crates/orna-cli/src/assertion_gate.rs`). Switch needed (G6).
2. `orna run` (executor). C3 must hold. The executor is `orna-exec`, already a
   production dependency of project and REPL (section 2).
3. REPL (`repl.rs`, `cli_serve_app.rs`, `AdmittedReplSession`). C4 must hold.
4. `serve` (`cli_serve*.rs`). C5 must hold.
5. Mount (`fuse_mount/source_write.rs`, `SourceMutationEffectHandler`). C5 must
   hold.

Each step is its own PR. A step's PR adds the switch (if missing), runs in
shadow, and changes the default only after its criteria hold. The old path stays
compiled and reachable until the deletion PRs of section 6.

### 5.2 Default flip

A flip changes the default in the entry's backend selector. For `orna check` that
is `backend_from_env` in `crates/orna-cli/src/cli_check_backend.rs`, which returns
`Backend::Old` when neither variable is set. The flip PR:

- changes the default to HIR and keeps `old` selectable by an environment
  variable (`ORNA_<ENTRY>_OLD=1`, or the reverse of the default; decision Q7);
- updates the ratchet files in the same PR, with the measured values;
- records the nightly report in the PR description: two consecutive nightly runs
  with zero unlisted differences.

### 5.3 Rollback

- Before the old path for an entry is deleted: revert the flip PR. This restores
  the old default. The old code is not deleted, so the revert is a one-line
  change.
- After that entry's old path is deleted: rollback is a `git revert` of the
  deletion PR. Each deletion is its own PR, so a revert restores exactly one
  path.
- A tag on the last commit before the first deletion PR gives a fixed rollback
  point (decision Q10).

## 6. Deletion schedule

Sizes are measured at `0f1e5961d` with `wc -l` or `cat | wc -l` over the named
paths. Figures from `hir.md` section 10.2 (taken at `026faee90`) are shown for
comparison.

### 6.1 Prerequisites

- P1 (gap G11): freeze the old pipeline's outputs over the corpus into golden
  files, so that the ratchets and the differential harness can still run after
  the old crates are gone. Without this, deleting `orna-evaluator` ends the proof.
- P2 (gap G10): move the `orna-check` types used in production (decision Q6)
  before `orna-check` is reduced.
- P3: migrate every production caller of `orna-evaluator` and
  `orna-project-repl` (section 2) in the step PRs of section 5.

### 6.2 H9 deletions

| Item | Path | Lines measured now | `hir.md` 10.2 | Status |
|---|---|---|---|---|
| `orna-evaluator` (src) | `crates/orna-evaluator/src` | 31,317 | 30,818 at `026faee90` | deleted in step D2, after P1 and P3 |
| `orna-evaluator` (src and tests) | `crates/orna-evaluator` | 83,704 | not stated | the tests and fixtures go with it |
| of which `sys_bindings.rs` | `crates/orna-evaluator/src/sys_bindings.rs` | 2,942 | 2,942 | with the evaluator |
| of which `relation.rs` | `crates/orna-evaluator/src/relation.rs` | 1,375 | 1,375 | with the evaluator |
| `orna-intrinsics` (src) | `crates/orna-intrinsics/src` | 4,639 | 4,894 | deleted in step D3, after the evaluator |
| `SourceMutationEffectHandler` and its async wrapper | `crates/orna-application/src/lib.rs:1599-2963` (types at `:1630` and `:2471`) | 1,365 (range length) | about 1,360 | deleted with the mount flip (step 5) |
| `Functions`, `StandardLayer`, `with_standard_sources` | `crates/orna-application/src/lib.rs:294-406` (`with_standard_sources` at `:296`) | not measured | listed in 10.2 | deleted with the same step as their last caller |
| `contextual_snapshot.rs` handler | `crates/orna-application/src/contextual_snapshot.rs` | 668 | 668 | deleted with the REPL or mount flip, per last caller |
| Three effect-handler traits and adapters | `crates/orna-evaluator/src/lib.rs:1349` (`EffectHandler`), `:1566` (`Ovb2EffectHandler`), `:1687` (`ContextValueEffectHandler`); `crates/orna-application/src/lib.rs:1253` (`AsyncApplicationEffectDispatcher`) | not measured | listed in 10.2 | deleted with the evaluator |
| `orna-project-repl` (`.rs`) | `crates/orna-project-repl` | 2,611 (no separate tests tree) | 2,605 | deleted with the REPL flip (step 3), after its callers move |
| Old `orna-check` modules of `hir.md` 10.2 H3 | `inference.rs`, `calls.rs`, `intrinsics.rs`, `flow.rs`, `imports.rs`, `collections.rs`, `collection_calls.rs`, `scope.rs`, `types.rs`, `system_api.rs`, `system_calls.rs`, `checkpoint.rs`, `semantic_payload.rs`, `secrets.rs`, `located.rs` | `crates/orna-check/src` total 36,591; per-module counts not measured | H3 | `types.rs` blocked by Q6 |
| `route-trace` feature | `crates/orna-evaluator/Cargo.toml:13-18` (feature and `required-features` for a binary); consumer `crates/orna-differential/Cargo.toml:19` | not measured | H9 | located (was gap G14); removed when the harness no longer needs the old step counters |
| `Expr.weight` | lowered expression field (HIR side) | not measured | H9 (`hir.md` 6.4) | removed with the step re-baseline (gap G12) |
| Legacy `analyze*` bodies | `orna-check` | not measured | H9 | removed with the H3 modules |

Measured subtotal of the H9 items with a verified size: orna-evaluator src
31,317 + orna-intrinsics src 4,639 + application region 1,365 + orna-project-repl
2,611 = 39,932 lines, plus `contextual_snapshot.rs` 668 = 40,600. `hir.md` 10.2
says "about 38,000". The difference is source drift since `026faee90` and the
items marked "not measured". The total also excludes the `orna-check` modules,
`route-trace` and `Expr.weight`, none of which is sized.

### 6.3 Owner-pending deletions (need a ruling; not in the H9 totals)

These are in `docs/design/deletions.md`. That document is investigation only and
deletes nothing; its delete-now set is empty.

| Item | Lines measured now | Options | Decision |
|---|---|---|---|
| `sys.explain` planner: `crates/orna-sys/src/introspection.rs` | 17,855 | A: keep `sys.explain` for `distinct` and `take` only, remove the fold and chain paths. B: keep the planner. C: move the planner tests out of the workspace | Q1 |
| Planner tests: `crates/orna-sys/tests/planner_*.rs` | 47,981 in 74 files | as above | Q1 |
| `crates/archive/orna-compat-v1` (the 1.0.0 archive) | 62,108 in 50 files | A: move to a tag and remove from the workspace. B: keep | Q3 |
| `orna-table` (`crates/orna-table`) | 2,829 | merge into conformance now, or after the layering decision | Q4 |
| Filler std modules in the `std` repository | about 932 in 10 files (figure from `deletions.md`; not re-measured here, as `stdlib/std` is a pinned read-only checkout) | remove in the std repository, not here | Q5 |
| `repl-inline-*.orna` fixtures | 104 files: 38 in `crates/orna-evaluator/src/fixtures`, 3 in `crates/orna-evaluator/tests/fixtures`, 63 in `crates/orna-project-repl/src/fixtures` | keep (`deletions.md` recommends keep); they are the REPL corpus | none |

## 7. Decisions for the owner

Each decision is stated so that the answer is an option letter or a short phrase.

- **Q1.** `sys.explain` planner (section 6.3): option A, B or C. Option A removes
  about 48,000 test lines and the fold and chain paths of `introspection.rs`. It
  needs a ruling on whether ORNA-PLAN-003 fallback evidence requires those paths.
  Bead `ornadb-csenl.22` (H7).
- **Q2.** Does `sys.explain` as a function overload stay in the spec? If yes, its
  `VersionIncompatible` result stays. If no, `explain_function` and its
  `sys.json` entry are removed, which is a spec change.
- **Q3.** `orna-compat-v1`: move to a tag (A) or keep in the workspace (B). It has
  no production consumer; its only consumer is a dev-dependency of conformance.
- **Q4.** `orna-table`: merge into conformance now, or after the layering decision.
- **Q5.** Filler std modules: approve removal in the std repository. This plan
  does not touch that repository.
- **Q6.** `orna-check` types used by production crates (`Type`, `ModuleHeader`,
  `ModuleInput`, `TableSchema`): (a) move them to `orna-hir` and reduce
  `orna-check` to its remaining API, or (b) keep `orna-check` as the types crate
  and delete only its checker modules. `hir.md` 10.2 says `orna-check` keeps its
  name and API, yet its H3 list deletes `types.rs`. The two statements conflict,
  so the owner must pick one.
- **Q7.** Switch naming and form: per-entry `ORNA_<ENTRY>_HIR` and
  `ORNA_<ENTRY>_SHADOW` (proposed), or one `ORNA_BACKEND=old|shadow|hir`. The
  `hir.md` 6.3 reference to `ORNA_CHECK_LEGACY` is removed (corrected in this PR).
- **Q8.** Shadow triage: the SLA for a nightly difference, and whether a calendar
  minimum applies in addition to two consecutive clean nightly runs.
- **Q9.** Differential harness after deletion: freeze old outputs as golden files
  (P1, proposed), or keep the old crates in a tag and run the harness from it.
- **Q10.** Rollback point: create a tag on the last commit before the first
  deletion PR (proposed), and name who creates it.
- **Q11.** Diagnostic-code rows: fix all 104 before the `orna check` flip, or
  accept a named set as intended differences, each with a reason and a bead.
- **Q12.** Who approves each default flip, and whether a flip PR needs a second
  reviewer.

## 8. Gap table

| # | Criterion | Gap | Bead | Note |
|---|---|---|---|---|
| G1 | C1 | 104 differing diagnostic-code rows | `ornadb-csenl.18` (H3, "effects pass and checker rules on HIR; orna check on HIR") | open |
| G2 | C1(b), C8 | diagnostic-code rows had no reason field | none | fixed in this PR: each row carries a reason; the test fails on a row without one |
| G3 | C6 | `typed-share.toml` recorded 35; measured 51. The comparator did not fail when `typed` rose | `ornadb-csenl.17` (H2, "Ty, inference and expression typing on HIR, diffed against the checker") | fixed in this PR: file records 51; comparator exact both ways |
| G4 | C3 | exec agreement 54 of 110 evaluable; 15 allowances (10 harness, 1 representation, 4 executor-gap) | `ornadb-csenl.19` (H4, "orna-exec pure core"), `.20` (H5, "Host trait, core/sys native declarations"), `.21` (H6, "TableStore, cursors and one mutation path"), `ornadb-0xfiw` (executor gap) | each allowance names its bead in `exec-share.toml`; the harness count falls when the old observer gets a table store (`ornadb-cdi8u`) |
| G5 | C4 | REPL 15 of 48 runnable agree; 33 expected rows | `ornadb-csenl.23` (H8, "REPL as incremental module on HIR") | open |
| G6 | C2 | the assertion gate has no HIR switch; it calls `orna_check` directly | none | propose a bead under `ornadb-csenl.24` |
| G7 | C5 | `orna run`, REPL, serve and mount have no switch; serve and mount have no corpus ratchet | none | propose beads under `ornadb-csenl.24` |
| G8 | C7 | `hir.md` 6.6 said H1 LSP is env-gated; no gate exists; LSP HIR status unverified | `ornadb-csenl.16` (H1, "lowering, resolver and call classification; LSP on HIR") | `hir.md` corrected in this PR; LSP gate not yet implemented; verify the LSP path under `ornadb-csenl.16` before H9 |
| G9 | section 4.2 | closed by the `shadow-report` job of `nightly-gates.yml`: the whole corpus runs through both pipelines and writes a markdown report (section 4.2) | none | the job has no run history yet, so criterion C1(c) is not met |
| G10 | P2, section 2 | production types from `orna-check` (decision Q6) | `ornadb-csenl.18` (H3), `.24` (H9) | decision Q6 |
| G11 | P1 | the differential harness has a normal dependency on the old evaluator; no golden freeze exists | none | propose a bead under `ornadb-csenl.24` |
| G12 | C3 | step re-baseline and `Expr.weight` removal (`hir.md` 6.4) | `ornadb-csenl.19` (H4), `.24` (H9) | recorded in the H9 PR |
| G13 | C9 | benchmarks of `hir.md` 7.4 not measured in this plan | `ornadb-csenl.15` (H0, "orna-hir crate scaffold, differential harness, baseline benchmarks") | baselines are an H0 deliverable |
| G14 | section 6.2 | `route-trace` feature: located at `crates/orna-evaluator/Cargo.toml:13-18`; not sized | `ornadb-csenl.24` | size in the H9 PR |
| G15 | section 3 (bead hygiene) | `docs/language/DECIDED.md` decides D1 to D19, but beads `ornadb-csenl.1` (D1, "type inference: constraint inference or required parameter annotations") and the other language-decision beads are `OPEN` | owner | not an H9 blocker; misleading to the next reader |
| G16 | section 2.1 | `hir.md` 6.3 named `ORNA_CHECK_LEGACY`, which does not exist; `hir.md` 6.6 claimed H1 and H3 env gates, only H3 exists | none | fixed in this PR in `hir.md` |
| G17 | section 6.3 | `sys.explain` planner decision (Q1, Q2) | `ornadb-csenl.22` (H7) | owner |
| G18 | section 6.3 | `orna-compat-v1`, `orna-table`, filler std (Q3 to Q5) | none | owner |
| G19 | C8 | 107 expected entries each have a bead field; the cited beads are not checked | `ornadb-csenl.24` | add a check before H9 |
| G20 | `hir.md` 9.1, K6 | no production-path coverage check: a production path covered by no corpus case is not detected | none | the shadow period is the mitigation; propose a bead |

## 9. Verification performed

All runs at `0f1e5961d` in a worktree with a private `CARGO_TARGET_DIR`.

- `cargo test -p orna-differential --test exec_share --test typed_share --test diagnostic_codes`:
  exit 0 at af9a80627, with the pinned stdlib. Re-run in this PR: "102 cases differ
  (expected 102)" from `diagnostic_codes`; "typed share: 51/104
  old-checker-accepted cases (14 cases are not lowered)" from `typed_share`.
  Earlier figures, not re-run here, at 0f1e5961d: "executor agreement: 48/62 run
  cases agree with the old pipeline (226 cases in the corpus)"; "exact step parity:
  48/48".
- `cargo test -p orna-repl --test repl_inline_differential -- --nocapture`:
  exit 0. Output: "repl-inline differential: agreed 15 of 48 runnable, 104 total, 33
  expected disagreements".
- Merge with `origin/main` 710ac17ff: `cargo test --locked -p orna-differential` (whole crate): exit 0.
  `cargo test --locked -p orna-differential --test shadow_report -- --ignored --nocapture`
  with `ORNA_SHADOW_REPORT` set: exit 0; report "226 cases. Clean: yes. Unexplained
  differences: 0. Stale ledger entries: 0"; diagnostic codes 211 compared (109 agree,
  102 expected); executor 63 compared (49 agree, 14 expected); REPL inline 48 compared
  (15 agree, 33 expected).
- `cargo test -p orna-differential --test exec_share -- --nocapture` after the `sort_by` stage (H4): exit 0. Output: "old-harness environment: 10"; "executor agreement: 54/70 run cases agree with the old pipeline (226 cases in the corpus)"; "exact step parity: 49/54". Before the stage: 54/69, 49/54, 9.
- Counts: `wc -l` and `find ... -name '*.rs' | xargs cat | wc -l` for every line
  count in sections 1, 2 and 6. `awk` count of the `## expected` rows in both
  differing files (104 and 33). `grep -c` for the `[[difference]]`, `[[environment]]`,
  `reason`, `bead` and allowance entries.
- `grep` of `crates/` for `ORNA_CHECK_` and `ORNA_LSP`; `cargo metadata` for the
  reverse-dependency list (section 2).
- `bd show` for beads `ornadb-csenl.1` and `.15` to `.24`: all `OPEN`.

Not run: the benchmarks of `hir.md` 7.4, the nightly `shadow-report` job (no run history yet), and
any shadow comparison on the production entry points. The gates
`scripts/check_doc_links.py` and `scripts/check_architecture.py` are run as
described in the PR.
