# Publication bundle validation audit

Issue: `ornadb-kpki` (GitHub #2109). Base: `origin/main` at `e1cc5a4d`. Authority: frozen `/home/pbox/dev/ornadb/reference/Orna-1.0.0`. This audit separates the generated reference publication bundle from the runtime Git publication protocol: `publication/api-index.json` and `publication/heading-index.json` describe the former; `source/22-publication.md` defines the latter.

## Bundle result

The frozen bundle contains 417 API name-to-anchor entries and 541 heading records. Direct audit found all 541 heading IDs unique; every heading source/line points to an ATX heading in its listed chapter; every source heading outside code fences is indexed; the API index key set exactly matches the names and overload aliases derivable from `api/sys.json`; and every indexed target exists uniquely in both `index.html` and `sys-reference.html`. All 17 ORNA-PUB identifiers in the source chapter are published as direct anchors.

`tools/build_publication.py:119–120` writes the two JSON indexes. `tools/check_publication.py:13–27` checks generated HTML duplicate IDs, local links, and external assets, but does not read either JSON index. The new integration checks in `crates/orna-conformance-v1/tests/publication_bundle_validation_kpki.rs` fill that validation-evidence gap without editing the frozen generator/checker or production code.

## Five added bundle checks

| Added test | Check |
|---|---|
| `heading_index_ids_are_unique_and_point_to_real_heading_lines` | Unique IDs, safe source paths, correct heading level and explicit anchors. |
| `heading_index_covers_every_source_heading_outside_code_fences` | No Markdown heading omitted from the generated index. |
| `api_index_names_match_the_declared_system_api_surface` | Exact API index key set against `api/sys.json`, including base names and grouped handles. |
| `api_index_targets_resolve_in_both_published_html_documents` | Every target resolves exactly once in each generated HTML document. |
| `publication_requirement_ids_are_published_as_direct_anchors` | Each normative ORNA-PUB identifier is addressable in the published book. |

These tests read the frozen Markdown, JSON and HTML corpus through `ORNA_REFERENCE_DIR`; they do not execute Orna source, so no `.orna` program fixture is applicable.

## ORNA-PUB implementation and proof map

Grades: **A** direct focused behavior proof; **B** partial or distributed proof; **C** no direct proof located. “C” records an evidence gap, not a claim that the behavior is absent. Clause IDs and anchors below are from `source/22-publication.md`.

| Requirement | Grade | Tip evidence and remaining gap |
|---|---:|---|
| ORNA-PUB-001, `:11` | A | `crates/orna-runtime-v1/tests/publication_metadata.rs::publication_metadata_tracks_frozen_prefix_and_matches_both_projections` checks target bounds, effective policy, and pending/published projection. |
| ORNA-PUB-002, `:13` | B | The same test checks the 60-second default metadata. The clause says a maximum age MAY publish a smaller complete batch; no timer-triggered publication proof was found, and the optional behavior is not claimed. |
| ORNA-PUB-003, `:15` | C | No focused test was found proving publication policy cannot change a table declaration kind. |
| ORNA-PUB-004, `:17` | A | Runtime metadata test checks `sys.Storage` and maintenance metadata projections and pending/published counters. |
| ORNA-PUB-005, `:49` | B | `crates/orna-repository-v1/tests/git_repository.rs::compact_publication_refuses_missing_or_corrupt_referenced_objects_before_ref_advance` rejects incomplete referenced objects; a direct durability-barrier-before-visible-ref test was not located. |
| ORNA-PUB-006, `:51` | A | `recovery_keeps_a_pre_ref_publication_pending` verifies the pre-ref batch remains recoverable. |
| ORNA-PUB-007, `:53` | B | `recovery_resumes_after_ref_and_index_boundaries`, `recovery_after_ref_advance_preserves_unrelated_partial_staging`, and `recovery_preserves_post_ref_external_conflict_and_can_resume` cover important post-ref paths; this is not an exhaustive crash-state matrix. |
| ORNA-PUB-008, `:55` | C | No focused logical-reader test was found proving the old-snapshot-plus-tail/new-snapshot-with-masked-batch handoff has neither duplicate nor missing rows. |
| ORNA-PUB-009, `:57` | A | `private_candidate_advances_current_branch_with_compare_and_set` verifies stale expected-head advancement is rejected. |
| ORNA-PUB-010, `:27` | A | `private_candidate_uses_head_and_preserves_ordinary_cwd_state` verifies candidate construction from the captured head without including ordinary staged/unstaged edits. |
| ORNA-PUB-011, `:29` | A | `published_candidate_reconciles_only_managed_index_entries` and `recovery_after_ref_advance_preserves_unrelated_partial_staging` verify index reconciliation and staged/unstaged preservation. |
| ORNA-PUB-012, `:31` | A | `publication_pauses_for_an_existing_git_index_lock_before_ref_change`, `publication_pauses_during_unfinished_merge_and_rebase`, and `publication_rejects_a_known_managed_edit_before_ref_advance` prove representative pause conditions. |
| ORNA-PUB-016, `:59` | B | Post-ref recovery preserves unrelated partial staging. The audit did not locate a direct test for every abandoned-index-lock owner-liveness and journal check. |
| ORNA-PUB-017, `:61` | B | `ordinary_commit_preserves_verified_compact_manifest_and_segment_identities` covers compact publication identities; a general managed-row reversal case was not located. |
| ORNA-PUB-013, `:78` | C | No focused graceful-writer-shutdown test was found for child termination, activation boundary and eligible-batch publication together. |
| ORNA-PUB-014, `:80` | B | Runtime metadata tests expose pending counts/bytes; a focused CLI status assertion for the required remaining unpublished CWD summary was not located in this audit. |
| ORNA-PUB-015, `:82` | C | Clone/transport tests cover Git transfer generally, but no end-to-end test was found for publishing recoverable runtime state, pushing, and fetching it into a receiving clone. |

This report does not convert the C/B items into implementation claims. The reference does not authorize replacing those proof gaps with inferred semantics.

## Focused proof

Command:

```text
ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0 cargo test --locked --offline -p orna-conformance-v1 --test publication_bundle_validation_kpki
```

Captured result:

```text
running 5 tests
test publication_requirement_ids_are_published_as_direct_anchors ... ok
test heading_index_covers_every_source_heading_outside_code_fences ... ok
test api_index_targets_resolve_in_both_published_html_documents ... ok
test api_index_names_match_the_declared_system_api_surface ... ok
test heading_index_ids_are_unique_and_point_to_real_heading_lines ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
CARGO_TEST_EXIT=0
```
