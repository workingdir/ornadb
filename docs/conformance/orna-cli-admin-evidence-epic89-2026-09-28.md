# ORNA-CLI and ORNA-ADMIN execution evidence (epic #89)

## Verified clauses

- **ORNA-CLI-001–006** are defined in `source/18-cli.md:12,31,56–58,87,89`: Git-compatible operation behavior (including identity-preserving `mv`), typed REPL/devtools information, stable porcelain output, compact status codes, non-terminal progress suppression, and supported terminal hyperlinks.
- **ORNA-ADMIN-001/002** are in `source/16-administration.md:9–11`; they require retained invocation/audit outcomes and reject reentrant administration during an activation with `sys.admin.busy`.
- **ORNA-ADMIN-003** is in `source/16-administration.md:65–67`; state-changing operations expose admission/recovery/terminal status and unknown reconnect outcomes are inspected through retained invocation/journal identity.

Frozen `tests/requirement-evidence.json` entries for CLI-001–006 (lines 10160–10273) and ADMIN-001–003 (lines 10025–10063) contain only planned implementation-conformance obligations; every entry remains `implementation_result: not executed`, with full implementation coverage false. They do not identify concrete test paths. The register is unchanged.

## Executed evidence

Every command used `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0`. The full captured stdout/stderr, including the failure verbatim, and each exit code are in `docs/conformance/orna-cli-admin-cargo-transcripts-epic89-2026-09-28.log`.

| Command | Actual result | Bounded evidence |
| --- | --- | --- |
| `cargo test -p orna-cli-v1 --test git_diff` | Exit 0; 1 passed | Raw Git diff command output and exit-code parity for a path with spaces; does not test `orna mv` semantic identity. |
| `cargo test -p orna-cli-v1 --test project_runtime binary_status_porcelain_preserves_git_worktree_bytes_and_hides_discovery_paths` | **Exit 101; 0 passed, 1 failed** | Verbatim failure: CLI stdout included an extra `RM 0 runtime mutations` row after Git's porcelain bytes; the test expected Git bytes alone. This records the test outcome and does not by itself establish a normative violation of the stable-output contract. |
| `cargo test -p orna-cli-v1 --test status_short_runtime` | Exit 0; 1 passed | Git short rows are preserved and runtime summary appended. Uses the checked-in `status-short.orna` fixture. |
| `cargo test -p orna-cli-v1 --test status_hyperlinks` | Exit 0; 1 passed | OSC 8 hyperlinks emitted only for supported terminal stdout. Uses the checked-in `.orna` path-with-spaces fixture. |
| `cargo test -p orna-conformance-v1 --test repl_admission` | Exit 0; 7 passed | Fixture-backed REPL admission, preview, binding and typed-result boundaries. It does not establish availability of all requested typed sys runtime/checkpoint/failure details. |
| `cargo test -p orna-conformance-v1 --test admin_fixture` | Exit 0; 1 passed | The checked-in `stream-admin-repl.orna` source passes parse, resolve and typecheck. The test itself explicitly says it does not exercise runtime admin dispatch or `sys.admin.busy`. |
| `cargo test -p orna-runtime-v1 --test activation_owner_fence` | Exit 0; 2 passed | Reentrant pause returns `sys.admin.busy`, leaves runtime rows/pending writes unchanged, and records failed and successful invocation outcomes. |
| `cargo test -p orna-runtime-v1 --test checkpoint_reset_provider generic_admin_audit_is_redacted_atomic_and_reopenable` | Exit 0; 1 passed | Checks redacted admin arguments, owner/generation/outcome, atomic audit persistence across reopen, and retained failed reset outcome. Does not exercise a lost response followed by unknown-outcome reconnect inspection. |

No result is promoted into the frozen register. This evidence does not establish all CLI/ADMIN clauses: CLI-002 typed diagnostic/recovery detail, CLI-005 dynamic progress suppression, `orna mv` identity semantics, and ADMIN-003 unknown-outcome reconnect recovery remain unproven by these selected targets. The complete relevant failure output is retained in the transcript.
