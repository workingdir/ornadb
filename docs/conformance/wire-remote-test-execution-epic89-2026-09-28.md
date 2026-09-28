# ORNA-WIRE / ORNA-REMOTE linked test execution (epic 89)

Date: 2026-09-28. Tracking epic: `ornadb-1787968123319-16-24513f57` (GitHub #89).

## Normative scope checked

The frozen reference at `/home/pbox/dev/ornadb/reference/Orna-1.0.0` was checked before running tests:

- `source/14-pages.md:46-56` defines ORNA-WIRE-001 through ORNA-WIRE-006 (watch snapshots/revisions/patches, stable identity, bounds and reconnect behavior).
- `source/14-pages.md:62-64` defines ORNA-WIRE-007 and ORNA-WIRE-008 (stale action handling and action transaction semantics).
- `source/14-pages.md:98-104` defines ORNA-WIRE-009 through ORNA-WIRE-012 (binary/value encoding, secret redaction, WebSocket/origin boundary and protocol failure presentation).
- `source/24-git-history.md:44-52` defines ORNA-REMOTE-001 through ORNA-REMOTE-005 (ordinary Git host migration, no redundant migration command, synchronization of `refs/orna/*`, allocator ordering, and diagnosis of missing/stale continuity refs).

These locations are the checked source clauses; this note does not assert that every clause has a passing linked test.

## Frozen register status

`tests/requirement-evidence.json` in the reference was inspected, not edited. The WIRE-001..012 and REMOTE-001..005 entries still say `implementation_result: not executed`, their linked test status is `planned`, and `full_implementation_coverage_claimed` is `false`. The register remains frozen; the runs below are bounded evidence recorded separately and do not change its assertions or status.

## Executed tests and captured results

Commands were run with `--locked --offline`; output was captured in `/var/tmp/orna-wire-remote-*.log` on the execution host. The result lines below are copied from those actual Cargo outputs.

1. `cargo test --locked --offline -p orna-protocol-v1 -- --nocapture` — **exit 0**.
   Captured suite results: `test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`; `test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`; doctests: `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`.
2. `cargo test --locked --offline -p orna-client --test v1_watch_review_regressions -- --nocapture` — **exit 0**.
   Captured result: `test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`. The test file identifies its bounded requirement links as ORNA-WIRE-001/002/003/006 in its header.
3. `cargo test --locked --offline -p orna-client --test v1_http_review_regressions -- --nocapture` — **exit 0**.
   Captured result: `test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`. This is HTTP-boundary evidence; the test header explicitly links same-origin behavior to ORNA-WIRE-011.
4. `cargo test --locked --offline -p orna-repository-v1 --test git_transport -- --nocapture` — **exit 101**.
   Captured result: `test result: FAILED. 2 passed; 24 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.81s`.

   The transport suite failure was preserved. Test setup attempted to configure a synthetic email, and the repository proxy rejected it. Verbatim captured diagnostic:

   ```text
   git ["config", "user.email", "transport@example.invalid"]: [git-commit-proxy] blocked: user.email value 'transport@example.invalid' is not in the identity allow-list
   ```

   The test helper then panicked with that same diagnostic. No identity override or proxy bypass was attempted. Failed tests (as printed by Cargo):

   ```text
   clone_preconditions_ignore_inherited_git_routing
   clone_synchronizes_internal_refs_without_checking_out_or_mutating_worktree
   fetch_after_remote_set_url_updates_requested_ordinary_and_internal_refs
   fetch_does_not_dereference_a_symbolic_destination
   fetch_errors_are_redacted_and_request_names_are_validated_before_git
   fetch_preconditions_ignore_inherited_git_routing
   fetch_preserves_materialized_promised_and_unavailable_object_states
   fetch_rejects_a_force_rewound_remote_branch_without_mutating_local_state
   fetch_rejects_a_local_change_to_an_unchanged_destination
   fetch_rejects_a_newer_local_tracking_ref_without_overwriting_it
   fetch_rejects_a_non_commit_branch_target_without_installing_it
   fetch_rejects_a_remote_change_between_advertisement_and_install
   fetch_reports_missing_internal_ref_without_fabricating_it
   fetch_reports_stale_internal_ref_and_preserves_the_local_ref
   fetch_synchronizes_matching_internal_refs_when_another_witness_is_missing
   fetch_synchronizes_matching_refs_while_missing_and_stale_witnesses_fail_closed
   fetch_updates_branch_and_internal_refs_without_mutating_local_state
   hydrate_fails_closed_with_an_unusable_promisor_without_changing_promises
   hydrate_materializes_only_the_requested_promised_object
   hydrate_rejects_unpromised_or_malformed_objects_before_fetch
   hydration_and_fetch_ignore_unavailable_submodules
   push_does_not_treat_allocator_shadow_as_allocator
   push_keeps_second_phase_atomic_after_allocator_first
   push_rejects_unsupported_atomic_before_allocator_advance
   ```

## Evidence boundary

The successful protocol-v1 and client runs are bounded evidence for the exercised protocol, watch and HTTP-boundary cases only. The attempted repository transport run produced **no passing behavioral evidence** for its 24 failed cases because setup was blocked before those cases could complete; only two unrelated tests passed. This note does not claim full conformance, server interoperability, engine behavior, or complete coverage of all 17 WIRE/REMOTE clauses. In particular, ORNA-REMOTE-001/002 and WIRE clauses without a direct mapping in the selected test headers are not individually certified here. The register must not be changed based on these results alone.

No Orna source was added or embedded in Rust tests, so this slice required no `.orna` fixture.
