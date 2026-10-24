# ornadb-5il1.429: format-3 loose-row fixtures audit

Status: step 1 only. Step 2 (`ORNA-REPO-CONTEXT-004` refusal in the
`orna-project-v1` format-3 loader) is NOT implemented. The gate was
"fixtures needing migration <= 6"; the audit found more than 6, so the
loader change and fixture migration are deferred to the owner.

## Why the gate failed

The format-3 test helpers in `crates/orna-cli-v1/tests/support/format3.rs`
commit loose rows into format-3 repositories:

- `format3_repository_for_table_with_source_rows` (L425) commits each
  caller-supplied row path such as `Music/london.orna` or `media/image.orna`.
- `format3_repository_for_table_with_source_row` (L412) wraps the above.
- `object_format3_repository` (L205) commits `Object/{song,image,overflow}.orna`.
- `commit_source_row_to_native_graph` (L235) writes and stages `Object/<key>.orna`.
- `format3_repository_parts` (L449) commits `.orna/object-identities.cbor`
  (sidecar, L482). Every format-3 fixture built on it carries the sidecar.

## Table: test files committing loose rows into format-3 repos

| Test file | Helper call sites (approx. line) | Loose rows committed |
|---|---|---|
| `edit_cli.rs` | L90, L142 | `Music/london.orna`, `media/image.orna` |
| `history_diff_snapshots.rs` | L58, L99, L241 | `Object/*.orna` via `object_format3_repository` and `commit_source_row_to_native_graph` |
| `publication_crash_matrix.rs` | L236, L1093 | `Object/*.orna` |
| `publish_no_visible_change.rs` | L192, L290, L364, L502 | `Object/*.orna` |
| `vfs_mount_integration.rs` | L261, L540, L694, L959, L1064, L1206, L1365, L1644, L1744, L1862, L1970, L2070, L2291, L2440 | `Music/london.orna` and table rows |
| `vfs_mount_sessions.rs` | L738 | `Music/*.orna` (multiple rows) |
| `vfs_unsupported_ops.rs` | L104, L238 | `Music/london.orna` |
| `walkthrough_music.rs` | L502 | `music/london.orna` |
| `support/editor_mount.rs` (included by test binaries) | L295, L347 | `Music/<key>.orna` |

Total: 9 test files, about 30 call sites, plus the identity sidecar that
every `format3_repository_parts` user commits.

## Not in scope (legacy format, not format-3)

- `row_stage_review.rs`: uses `init_repository`, legacy repo, loose rows are
  the intended format.
- `object_identity_mv.rs`: uses `orna init` on a legacy source project.

## Loader facts (for the owner)

- `crates/orna-project-v1/src/lib.rs`
  - `load_committed_snapshot_with_standard_profile` (~L1024-1100): for
    format 3 it returns `Ok(Vec::new())` and ignores committed rows (~L1091).
  - `load_private_candidate` (~L1140-1203) calls `refuse_written_format3_loose_rows`
    (~L2215), which only checks rows changed versus HEAD and raises `MixedFormats`.
  - `ProjectLoader::load` (~L954) to `load_with_standard_profile` (~L994)
    reads the worktree with no format-3 check.

## Decision needed from the owner

1. Schedule migration of the ~9 test files above (plus the sidecar) to
   product commands, then implement the 004 refusal in step 2.
2. Confirm that the committed-snapshot path should refuse loose rows with
   `ORNA-REPO-CONTEXT-004` naming the path, and that the worktree path should
   too.
