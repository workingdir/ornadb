# Writer uniqueness and legacy rejection at the public write entry points

Evidence for Beads `ornadb-5il1.99` (Gap A, found in the `ornadb-5il1.55` review). That review named about 25 public write entries with "no test asserting writer uniqueness or legacy-format rejection" and asked for either real tests that fail on the exact clause violation, or a justification for each. This document is that enumeration. Inspected at `fc11c3acd`.

Two of the families in the original list no longer exist or were never repository writers:

- `compact.rs commit` / `publish_compact_repository_boundary` is gone. The retired compact writer is compiled out of the shipped crate behind the `compact-writer-fixtures` test feature, and `scripts/check_retired_row_writers.sh` fails the nightly when the default build exposes any of its symbols. There is nothing left to test.
- `write_offline_copy`, `write_offline_copy_to_writer` and `write_offline_copy_limited` are not repository write entry points. They take `OfflineRow` values and write a bundle directory (`index.tsv` plus `media/<sha256>`). They cannot reach `.orna/store` or a loose managed path, so the writer-uniqueness clause does not bind them. Their own contract, that the target directory is empty and every payload matches its row metadata, is tested in `export_edit_cycle_manifest_eq.rs`, `export_import_manifest_eq.rs` and `import_streaming_verification.rs`.

The remaining entry points share one implementation chain and are covered:

| Entry point | Clause it can violate | Evidence |
|---|---|---|
| `Repository::publish_candidate` | Format-3 rows have exactly one writer (`.orna/store`, ORP) | `publish_format3_orp_only` asserts the published tree contains `.orna/store/` and no `media/` row files; `git_repository.rs` drives the entry point directly 22 times, including journal-stage and identity checks |
| `Repository::publish_candidate_with_stage_hook` | Same, plus the CAS refusal | Same implementation (`publish_candidate_impl_with_migration` with the same flags); used by the CLI path that `publication_crash_matrix.rs` kills at every reachable boundary |
| `Repository::publish_legacy_format_migration` | Legacy rejection: never converts or relabels legacy data | `git_repository.rs:5140-5650` asserts `InvalidFormatMigration` for a missing continuity record, a missing annotation default, a non-legacy captured HEAD, and a candidate that does not admit as format 3, each time leaving `HEAD` and the journal untouched |
| `RuntimePublicationCoordinator::publish` | Delegates to `publish_candidate_with_stage_hook` | No separate write path; `req_publication_cas.rs` drives it through CAS races, `req_upgrade.rs` through migration, `publication_crash_matrix.rs` through kill and recovery |
| `RuntimePublicationCoordinator::publish_with_stage_hook` | Same | Same delegation; the stage hook is exercised by every kill-boundary test in `publication_crash_matrix.rs` |

Legacy rejection for the ordinary path is enforced before any of these run: `orna publish` calls `refuse_legacy_head` before recovery, runtime access or a freeze, and the coordinator's `prepare_*` constructors require a format-3 context. That ordering is covered by the Gate A item whose selector is `test:publish_format3_orp_only` and by `req_upgrade.rs`.

## Conclusion

No live entry point lacks coverage of the clause it can violate, so this bead adds no test. The two exclusions above are not deferrals: one family was compiled out and the other was never a repository writer. If a future change reintroduces a repository writer, `scripts/check_retired_row_writers.sh` fails first, and the table above is the checklist to extend.
