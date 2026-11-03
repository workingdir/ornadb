# Validation pass, 2026-11-03

A whole-repository check run against main `bc8120209` (the merge of `#9523`).
Every claim below names the command that produced it. Anything not run is listed
in "Not verified" rather than assumed.

## What main does now

| Surface | Evidence | Result |
|---|---|---|
| Formatting | `cargo fmt --all -- --check` | exit 0 |
| Clippy, `orna-repository-v1` | `cargo clippy -p orna-repository-v1 --all-targets --locked -- -D warnings` | finished, no diagnostics |
| Evidence tooling | `python3 -m unittest scripts.test_gate_evidence` | 47 tests pass |
| Manifest consistency | `python3 scripts/gate_evidence.py check` | 39 items, 4 known-failure entries |
| Standard library | `cargo test -p orna-evaluator-v1 --test standard_profile_v1 --test standard_snapshot_replay --test standard_library_format_contracts_wi489 --test standard_library_networking_url` | 118 tests pass (54 + 61 + 2 + 1) |

The standard library is not a separate build: `stdlib/std` holds 81 `.orna`
files that the evaluator compiles through `include_str!` (801 such sites), so the
evaluator test targets above are the coverage for it.

## CI on main

Push run at `bc8120209`, started 12:57 UTC:

| Job | Conclusion |
|---|---|
| Lint | success |
| Source audits (fixture-and-editor-artifacts, rollback-proof) | success, `focused-consumer` skipped |
| Semantic v1 tests | success |
| Live v1 tests | success |
| Conformance v1 tests | success |
| Gate C scale (bounded) | success |
| CLI tests | in progress at the time of writing |
| CLI Gate B | in progress at the time of writing |

## Gate evidence, from the nightly record

Run `41614410003` on `16bf94193`, summarised in
`gate-evidence-summary.json` (downloaded, not remembered):

| Gate | State | Items |
|---|---|---|
| A | failed | passed A3–A7, failed A2, not-executed A1 |
| B | not-executed | B1–B6 all passed |
| C | not-executed | C1–C6 all passed |
| D | not-executed | D1–D5 all passed |
| E | not-executed | E1–E10 all passed |
| F | not-executed | F1–F5 all passed |
| G | not-executed | no items |
| H | not-executed | no items |

A2's failure in that run was `check:retired-row-writers` exiting 2, which `#9520`
fixes; the same step passes locally now and records `passed`, 1 test.

Known-red baseline is four entries, all mapped to beads: `req_vfs` mount
detach (`ornadb-9aj6`), `runtime_owner_chain_survives_repeated_startup_crashes`
(`ornadb-mmsa.37`), and two `publication_crash_matrix` cases (`ornadb-aflz`).

## A temporary database, driven by hand

Built a repository outside the test suite and drove the product surface:

```
git init -q -b main . && orna init .
printf 'pub table Note(id: Str) {\n    body: Str,\n}\n' > main.orna
git add main.orna .orna/database.orna && git commit -m staged
orna commit -m "Declare Note"      # admitted table Note as relation 0af5bbce...
orna repl                          # two inserts, published
orna publish --message "Add two notes"   # Note 2 rows
orna query <relation> --format json      # rows 2
orna query <relation> --key ridge        # rows 1
orna export /tmp/archive --at HEAD       # 65 objects
orna export /tmp/archive --check         # complete copy
orna export /tmp/archive --restore /tmp/restored
orna query <relation> --format json      # rows 3 in the restored copy
orna check                                # syntax error reported as main.orna:2:10
```

Two defects came out of it, both now filed:

- `orna history <relation> ridge` fails with `DescriptorNotInRow` for every
  inline row, so revision history exists only for rows with a descriptor tree
  (`#9525`, bead `ornadb-0cq2`).
- `orna export ARCHIVE --check` called a bundle truncated to a quarter of its
  length a complete copy (`#9526`). Fixed in `#9527`: the check now reconstructs
  into a throwaway directory, so it exits 1 with "A recorded object failed
  verification" for that archive. The new test in `offline_copy_cli_read` fails
  without the fix and passes with it.

## Issues closed with evidence

| Issue | Evidence |
|---|---|
| `#9135` planner anchor-fold identity | `planner_decorrelated_anchor_fold_identity_wj87v::paired_decorrelation_folds_keep_anchor_and_child_identity_through_sparse_reordering` passes, 1 passed, 0 failed |
| `#9236` runtime stream tests | `durable_stream_control_reopens_paused_and_resets_opaque_position` and `stream_observation_projects_nullable_partition_and_failure_sys_dto_across_reopen` both pass, 2 passed, 464 filtered out |

## Not verified

- The full workspace test suite. Only the targets named above were run.
- `cli-tests` and `CLI Gate B` on `bc8120209`: still running.
- Editors (Emacs, Vim, TextMate, Sublime, tree-sitter): `#9450` stays open, no
  highlighter test was run here.
- Mount, VFS, live server, media and upgrade paths: no manual exercise.
- The C2, C3 and C4 limitations that keep Gate C at `not-executed`, beyond the
  two manifest corrections merged in `#9522` and `#9523`.
