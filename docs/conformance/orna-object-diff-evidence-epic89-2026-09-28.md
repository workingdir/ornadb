# ORNA-OBJECT and ORNA-DIFF execution evidence (epic #89)

## Verified clauses

- **ORNA-OBJECT-001–005** are defined in `source/07-tables.md:440–448`: stable IDs across persistent definitions; canonical committed metadata; identity-preserving CLI/LSP rename; conservative delete/create handling for unassisted renames; and no reuse of retired IDs.
- **ORNA-DIFF-001–003** are defined in `source/25-evolution.md:118–122`: semantic changes where available; raw Git diff remains available; physical-only changes remain distinguishable from logical data changes.
- Neighboring rename/re-key rules **ORNA-MUT-010/011** (`source/07-tables.md:432–434`) say explicit re-key appears as one semantic re-key change even if physical storage moves, while an unintentional path rename is not proof of semantic continuity.

The frozen `tests/requirement-evidence.json` entries for OBJECT-001–005 (lines 5210–5282) and DIFF-001–003 (lines 12290–12332) each contain only a planned `implementation-conformance obligation`; all say `implementation_result: not executed` and `full_implementation_coverage_claimed: false`. None names an executable test path. I left this frozen register unchanged.

## Executed evidence and limits

All four commands used `ORNA_REFERENCE_DIR=/home/pbox/dev/ornadb/reference/Orna-1.0.0`. Complete captured output and exit codes are in `docs/conformance/orna-object-diff-cargo-transcripts-epic89-2026-09-28.log`.

| Command | Actual result | Bounded evidence |
| --- | --- | --- |
| `cargo test -p orna-cli-v1 --test init` | Exit 0; 10 passed, 0 failed | Includes `init_directory_creates_a_git_repository_and_persists_one_uuid_identity`, checking stable database identity in `.orna/database.orna`, and `init_then_check_accepts_unchanged_authoritative_reference_sources`. It does not establish all ObjectId categories or identity-preserving rename. |
| `cargo test -p orna-evolution-v1` | Exit 0; 22 passed, 0 failed; doc tests 0 | Model-level evolution tests include stable table/field identity rename planning and delete/create for tables without stable identity. They do not exercise `orna mv`, semantic LSP rename, or retired-ID reuse. |
| `cargo test -p orna-core catalogue_diff::tests` | Exit 0; 21 passed, 0 failed; 568 filtered | Snapshot diff tests cover identity-keyed schema/catalogue changes and rename reporting. They do not demonstrate all semantic diff domains or distinguish pure physical-only from logical data changes. |
| `cargo test -p orna-cli-v1 --test git_diff` | Exit 0; 1 passed, 0 failed | The integration test loads the real `crates/orna-cli-v1/tests/fixtures/git-diff/changed path.orna` fixture with `include_bytes!` and verifies CLI raw diff output and exit-code parity with Git, including a changed path containing spaces. It is bounded evidence for raw diff availability. |

No test failures occurred in these runs. In particular, ORNA-OBJECT-003/004/005 and the physical-versus-logical distinction in ORNA-DIFF-003 remain unproven by this bounded run. No clause-family pass is claimed; the frozen register remains not executed.
