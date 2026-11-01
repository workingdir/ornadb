# Changelog

This file is the user-facing inventory for the OrnaDB source tree. It is not a
Debian changelog, a package manifest, or proof that a production artifact has
been published.

## Unreleased
- Known limits for row maps and Git maintenance (no product behaviour change). Fixed-row batching is not content-stable: ORP row pages close at content-defined boundary anchors, with a maximum entry count as a backstop (`boundary_anchor` in `crates/orna-repository-v1/src/orp.rs`). Concurrent uncoordinated `git prune --expire now` is unsupported and is not advertised as safe (ORNA-CAP-006); this build exposes no `prune` command.
- CWD observation reads each dirty or untracked file once per settled stamp (ornadb-5il1.156; product change). Commit,
  verify and publish observations reuse a regular file's digest section while its stamp is unchanged and its mtime and
  ctime are at least two seconds before the read (racy-clean). A file written within that window is read again on every
  observation, so a same-size rewrite is still detected; this is a documented limit, not a single-read guarantee for a
  file edited seconds before commit. Git-ignored files stay out of the CWD observation (unchanged). The worktree content
  digest format is now `ORNA-WORKTREE-CONTENT-V2`, so digests computed before this change differ from new ones.
  Tests: `crates/orna-repository-v1/src/lib.rs` (`settled_dirty_file_is_read_once_across_cwd_observations`).
- Literal list streams evaluate each item's row expression under the bound item parameter (ORNA-A12-01), so helper calls
  and arithmetic in the item expression produce the row instead of only literal items. An item whose evaluation fails
  fails the stream with ORNA-LIST-STREAM-ITEM, naming the owning source (ORNA-ASSERT-051); the failed item is not
  committed and the checkpoint does not advance past it (ORNA-ASSERT-047). Items committed before the refusal keep their
  own checkpointed rows. Partial: whole-batch rollback is not implemented.
- Float literals and scalar `min`/`max` follow IEEE-754 (ornadb-5il1.188; product change). An `f`-suffixed numeral is
  correctly rounded to nearest (ORNA-LIT-002, ORNA-NUM-003), so `1.0e400f` evaluates to a signed infinity and an
  underflowing numeral to a signed zero, where it previously failed with ORNA-EVAL-VALUE. Scalar `std.math.min` and
  `std.math.max` over two Floats follow ORNA-FLOAT-007: a NaN operand yields the canonical NaN, `min` prefers `-0.0` and
  `max` prefers `+0.0`, and neither fails with ORNA-EVAL-VALUE any more. Tests: `crates/orna-evaluator-v1/tests/float_ieee.rs`.
  Open: scalar `std.math.clamp` still rejects a NaN operand with ORNA-EVAL-VALUE; the spec does not name clamp's NaN
  behaviour (ORNA-FLOAT-007 covers `min` and `max` only).
- Nonfinite Float text decodes (ornadb-5il1.188; product change). `Float.nan`, `Float.infinity` and `-Float.infinity`
  (29-formats.md:125) decode to the IEEE-754 values they name, and every nonfinite Float re-encodes to one of those
  spellings (ORNA-CODEC-003). Decode admits Float data only as literals and these spellings, so any other Float path,
  such as `Float.pi`, is refused with ORNA-EVAL-VALUE (ORNA-FORMAT-004, non-executable). Tests:
  `crates/orna-evaluator-v1/tests/float_ieee.rs`. Open: decode does not yet check the `as:` type against the decoded
  value (ORNA-FORMAT-004 type-directed decoding).
- Project admission uses the production catalogue (ornadb-5il1.384, partial; product change, not test-only).
  `orna check`, `orna run`, commit and serve admission, the VFS source-write check and the LSP project loader resolve
  `LoadedProject::production_catalogue`: the authoritative core, extended by the pinned standard profile and its verified
  sources when the project captured one. CLI project loading (`load_project`) refuses a reachable import of a standard module
  when the project captured no standard dependency, with ORNA-S010-IMPORT. `admit_transaction_project` refuses a project whose
  environment expectations declare a `stdlib` entry, and refuses a standard source unit with ORNA-STANDARD-PROFILE when the
  catalogue has no pinned profile or the profile rejects the source. It takes the catalogue as an explicit argument and does not
  rebuild one. Table-write validation (`transactional_loaded_project_candidate_validator`) admits against `production_catalogue`.
  `TransactionalEvaluator::new` admits source against `AdmissionCatalogue::Production`, the authoritative core without a project's
  pinned profile; only the conformance harness opts into the fixture catalogue (`with_fixture_catalogue`).
  Tests in `crates/orna-cli-v1/tests/project_runtime.rs`: `binary_run_admits_a_standard_import_alongside_a_table_assertion`,
  `binary_run_refuses_an_assertion_naming_an_undeclared_table`, and
  `binary_run_refuses_a_committed_row_violating_a_table_assertion_and_keeps_rows_unchanged`, which pins a refused violating
  run (non-zero exit, empty stdout, a diagnostic on stderr; the internal code is not asserted), byte-identical committed rows,
  and a later valid run still admitted.
  Open: the one-shot invocation still builds a second standard catalogue, used only as a verification gate. `BoundedEvaluator::load_standard_sources`
  (conformance-owned, `orna-conformance-v1` `semantic_adapter.rs:757-764`) verifies the captured sources and calls
  `Catalogue::from_standard_sources`, after analysis resolved through `production_catalogue`. `orna-cli-v1` takes
  `orna-conformance-v1` as a normal dependency for this path, the REPL and the table validator. Not changed here.
  Open (ornadb-a3jt, P2): a false committed-row table assertion reports `E2200` on the run path; the internal code
  `ORNA-EVAL-TABLE-ASSERT` appears only under `--debug`, and the default output names neither the violated assertion nor a
  deterministic safe witness. The Orna 1.1.0 diagnostics reference assigns `ORNA-A091-009` to a false table or cross-table
  assertion and the REPL path emits that code. Aligning the run path is not done here.
- REPL write gate (ornadb-5il1.384 S3, partial; product change): in a project that declares a table or module assertion, each REPL write rebuilds the table validator from the loaded project and checks the post-mutation candidate before any row, checkpoint or terminal is published (ORNA-ASSERT-042). The assertion path commits through the validated request activation, which takes the request identity, fingerprint, terminal outcome and validator, so they publish with the rows. A project that declares no assertion keeps the unvalidated activation path. A false assertion with no external binding dispatched during evaluation is refused with ORNA-A091-009 and leaves the store unchanged. A validation that cannot complete, and any refusal before publication, with no external binding dispatched is ORNA-REPL-EFFECT; once an external binding was dispatched, and for any commit error, the outcome is ORNA-REPL-EFFECT-UNCERTAIN and the request is not replayed. Partial (INTERIM, ornadb-gq4g, P2): the transactional candidate seam (`orna-conformance-v1` `semantic_adapter.rs`) builds its key map from every table in the project and accepts only scalar Bool, Int, Decimal and Str primary-key components (`TransactionKeyType::from_syntax`, propagated with `?` across all tables). So in a project that declares assertions, a REPL write is refused with ORNA-REPL-EFFECT, and the store is left unchanged, when any table in that project has a primary key of another type: an enum key, or a Uuid, Date, Instant or tuple key. That includes a valid write to a scalar-keyed table with no assertion of its own. Pinned by `unrelated_enum_keyed_table_refuses_a_valid_write_in_an_asserting_project` (INTERIM; the read-only count still answers there). ORNA-ENUM-005 and ORNA-KEY-005 permit enum keys (ORNA-KEY-003 forbids only Float). [INFERENCE] With an external binding dispatched during evaluation, the same refusal would be ORNA-REPL-EFFECT-UNCERTAIN and the row would stay Running; not tested. Tests: `repl_enum_keyed_assertion_write_is_refused_as_repl_effect` (`crates/orna-cli-v1/src/main.rs`; enum case only, a table-reference key is outside the same types and is untested), `enum_keyed_assertion_write_is_refused_and_leaves_the_store_unchanged`, `unrelated_enum_keyed_table_refuses_a_valid_write_in_an_asserting_project` and `enum_keyed_table_without_assertion_admits_the_write` (`crates/orna-cli-v1/tests/repl_table_assert.rs`). Not done: `AdmittedReplSession::from_loaded_project` still takes the caller's standard-source set; switching only its catalogue would split the semantic and runtime catalogues, so that part stays open.
  Assert-free admission of Date, Instant and composite (Int, Instant) keys is pinned by `date_keyed_table_without_assertion_admits_the_write`, `instant_keyed_table_without_assertion_admits_the_write` and `composite_keyed_table_without_assertion_admits_the_write` (same file). A Uuid-keyed table cannot be written in the REPL yet: `uuid7()`, as a table default or inline, fails with `ORNA-EVAL-NAME`, although 09-standard-library.md:7 lists `uuid7` in the root intrinsic environment. That is a product gap outside this slice, recorded under ornadb-gq4g and not fixed, so no Uuid case is tested. Refusal of Uuid, Date, Instant and composite keys in an asserting project follows from the seam's scalar-only key types (`TransactionKeyType::from_syntax`) by code reading; only the enum case is tested.
- Served table writes are gated on the table assertion (ornadb-5il1.384 S4, partial; product change). A live Eval transaction whose staged table commit fails the table assertion validator is refused before its row is published. The refusal answers the request with a correlated diagnostic (`Message::Diagnostic`, 30-protocol.md) that carries ORNA-A091-009 and the request's correlation ID, and the socket stays open: the next request on it commits as usual. Test: `served_eval_refused_by_table_assertion_reports_its_diagnostic` in `crates/orna-cli-v1/src/cli_serve.rs` checks those points and that the refused write commits no row. Not covered by that test: a replay of the refused request ID, and whether unrelated watches stay open. Open: the gate seam supports scalar Bool, Int, Decimal and Str primary keys only (ornadb-gq4g).
- Publish gate (ornadb-5il1.384 S7, partial; product change): `orna publish` runs the shared assertion gate (`crates/orna-cli-v1/src/assertion_gate.rs`, also used by `orna commit`) on the pending candidate before the storage publish call, so no ref, pin or journal effect precedes it (ORNA-ASSERT-045). The order is by code; no test asserts the absence. A false table assertion is refused with ORNA-A091-009; a false module assertion carries its owner and span; a candidate the gate cannot validate keeps its cause. A refused publish leaves HEAD unchanged and the pending range pending for a retry after the fix. Tests: `crates/orna-cli-v1/tests/publish_assertion_gate.rs` (an assert-free project publishes; a violating pending row is refused with ORNA-A091-009 and HEAD unchanged; a refused range stays pending; a holding pending row publishes under its assertion).
  Behaviour change, not covered by a test: publish now loads the project before publishing, so an assert-free candidate whose project does not load is refused with E2300 where it previously published.
  Open (ORNA-ASSERT-045, 048, 050): `orna migrate` creates a new physical root through `publish_copy_noreplace` (`migration_copy.rs`) from a metadata-only candidate without the gate. The repository entry points (`publish_candidate`, `publish_candidate_with_stage_hook`, `publish_legacy_format_migration`) and the conformance `publish_pending` remain ungated library API.
- Checkout assertion gate (ornadb-5il1.384 S8, partial; product change): `Repository::execute_nonconflicting_git_checkout` takes a `CheckoutAssertionGate`. It takes the coordination lock, re-verifies the preflight, and runs the gate before any `git switch`; a refusal maps to `RepositoryError::CandidateAssertionRefused` (ORNA-ASSERT-044). The CLI gate (`crates/orna-cli-v1/src/checkout_assertion_gate.rs`) validates the target commit with the assertion gate that `orna commit` and `orna publish` use. Tests in that file: a target whose assertion is false over its changed rows is refused; a target without changed rows is admitted without evaluating it; a target whose assertion holds over its changed rows is admitted.
  `orna checkout` to a different commit reaches this gate (ornadb-5il1.141) only when no draft is pending and no uncommitted change touches a path the target changes; otherwise the checkout is refused first. Open: no repository merge or reset executor exists, and `validate_checkout_discard_set` takes the same gate but nothing consumes its result, so merge and reset are not gated (ORNA-ASSERT-043; gap bead ornadb-t2nk).
- Stream assertion refusal attribution (ornadb-5il1.384 S10; product change): a literal list stream item whose
  table or module assertion refuses, or cannot be evaluated, fails with ORNA-LIST-STREAM-DELIVERY. The diagnostic
  names the owning assertion (owner kind, owner name, source span), the item position and the source identity
  (ORNA-ASSERT-051). Each literal list item is one activation (ORNA-STREAM-004, ORNA-TXN-004): the refused item
  publishes no row and its checkpoint does not advance (ORNA-ASSERT-047, ORNA-SYS-061, ORNA-SYS-062), while earlier
  items keep their own checkpointed activations. No batch-wide validator is introduced. Tests:
  `refused_table_assertion_item_names_its_owner_span_and_position` and
  `refused_stream_item_names_its_owner_and_does_not_advance_checkpoint` in
  `crates/orna-conformance-v1/tests/streams_pa0p_conformance_audit.rs`.
- Branch and checkout verbs (ornadb-5il1.141, partial; product change): `orna branch NAME` creates a branch at HEAD with no commit, and the index, worktree and CWD are unchanged (ORNA-BRANCH-001). `orna checkout NAME` to a commit that HEAD already has keeps pending and staged state (ORNA-BRANCH-002). A different-commit checkout runs the CHECKOUT-3 plan under the runtime owner lock and validates the target through the gate above before `git switch`; it is refused, leaving HEAD, index, worktree and drafts unchanged, while a draft is pending or a path the target changes has uncommitted changes (ORNA-CHECKOUT-001). Not built: carry-forward of pending drafts, checkout journal and recovery records, and `orna add --row`; ornadb-5il1.141 stays open. `switch`, `merge`, `reset`, `pull` and `stash` remain refused.
- Publish-time schema check test (ornadb-eufo.2.2; test-only, no product behaviour change): `a_pending_row_saved_before_a_schema_commit_lands_is_refused_at_publish_under_that_schema` in `crates/orna-cli-v1/tests/schema_evolution_e2e.rs` saves a row under a schema, lands a schema commit from a second clone that fast-forwards into the worktree, and checks that `orna publish` re-validates the pending row against the new HEAD: a schema the row does not fit is refused on both attempts, HEAD stays on the owner's commit, and no row is committed under the stale schema. The positive branch (a row the new schema accepts is published) is not covered here; see the gap noted under ornadb-eufo.2.2.
- ORNA-VFS-020 source modules are projected in the live `--cwd` mount and in `--at` snapshot mounts (ornadb-9npn.1). A reachable ordinary source module (`main.orna` or `notes.orna` at the root, or `Music/extra.orna` inside a directory) appears at its logical path beside the table directories and reads its committed bytes, read when the mount opens. On a name collision the source path keeps its plain name and the colliding table directory is projected under a first-byte escape (`Music` becomes `%4Dusic`), with its rows intact, rather than being dropped; a name that still collides after escaping is refused. A write-intent write to a source module on the live `--cwd` mount is accepted: a synced write is visible through a new open (`vfs_020_source_module_write_is_accepted_and_visible_after_sync`), and the `--at` snapshot mount remains read-only. Uncommitted bytes stay invisible to a new open until sync (`vfs_020_speculative_source_write_is_not_visible_until_sync`); a truncating or append open continues from the committed bytes (`vfs_020_source_module_append_open_continues_committed_bytes`). Source modules synced together in one group are admitted as a unit, and a malformed member refuses the whole group with neither committed module changed (`vfs_020_source_module_group_is_admitted_together_or_not_at_all`). Group admission is spec-silent in 41-vfs.md and is a conservative choice of this build, flagged for review. A malformed candidate is refused at sync with EINVAL and the committed bytes are kept (`vfs_020_invalid_source_candidate_is_refused_with_einval_and_committed_bytes_kept`); a candidate whose disk baseline changed since its open is refused with EAGAIN, and the mount keeps the other writer's bytes (`vfs_020_stale_source_candidate_is_refused_with_eagain`). Not covered: the no-evaluation-during-speculative-writes check (ORNA-VFS-020 proper; the speculative test shows visibility only, not that a draft is never evaluated) and recovery of an unsynced source draft across a remount; both remain under ornadb-9npn.
- ORNA-VFS-020 source group commit rule (ornadb-9npn.1.1; spec-silent in 41-vfs.md and profiles/vfs-1.md, a conservative choice of this build, flagged for review): an fsync or close of a source handle is the trigger, and it commits every dirty source candidate as one group. A member that fails on its own is refused and retained with its own cause while the rest of the group stays pending; when no member fails on its own, the whole group is refused. The trigger's errno reports only its own candidate: its own refusal (EINVAL, EAGAIN or the retention errno) if it is refused; EIO if it is accepted but a sibling was refused, since the sibling's errno is not reported on this sync; and success if it is not a group member, so another module's refusal is never reported on its sync (`vfs_020_sibling_refusal_is_not_reported_on_another_modules_sync`). Group admission and refusal are covered by `vfs_020_source_module_group_is_admitted_together_or_not_at_all`. A committed module larger than one range reads exactly by range (`vfs_020_large_source_module_reads_committed_bytes_by_range`).
- ORNA-VFS-020 grouped source validation (ornadb-5il1.372, partial): the real-mount test `multi_file_source_edit_requires_grouped_validation` in `vfs_mount_cases.rs` checks that renaming a function an importer calls is refused when only the callee is saved (both modules keep their bytes and the candidate is retained as a draft), that the same rename saved with its importer commits both files as one group, that a group whose two candidates both fail validation on their own is refused, and that a group whose candidates each pass alone but fail together is refused as a unit (the callee drops `spare`, which the importer's candidate calls). The source projection and group commit rule already existed; this change adds the cross-module test only. Partial: one import edge and one rename are covered; the EIO sibling-refusal errno and draft recovery beyond the retained-draft checks are not.
- The read-side `sys.blob` operations `length` and `digest` run natively from the Blob's stored identity. `read`, `verify` and `to_bytes` run natively on Blobs whose content is held in the value; a Blob that decodes to descriptor content (stored or captured ROV-3 values) fails with `sys.blob.unavailable` until its descriptor is bound (ornadb-55op). `digest` returns the raw-content SHA-256 commitment (`Digest`, tag 60025) from the Blob's stored content identity, without reading content. `read` returns a bounded Blob slice; `to_bytes` and `read` refuse more than 8 MiB in one call with `sys.blob.limit`. `verify` streams the content in bounded chunks and checks its SHA-256; it is not a full-verification claim. `from_bytes` builds a Blob from integers in `0..=255` with the `application/octet-stream` fallback, refusing an out-of-range integer with `sys.blob.bounds` and more than 8 MiB with `sys.blob.limit`. `sys.blob.media_type` and `sys.blob.suffix` are native metadata reflection. `sys.blob.same_content` returns false from the stored identities when they differ, returns true at once when both sides share a content source or descriptor witness, and otherwise streams both sides in bounded chunks and compares exact bytes (ORNA-BLOB-005). `sys.blob.capture_file` is invoked as an effect, not read.
- Known gap: `sys.blob.verify` does not check the native dependency structure. Orna-1.1.0 `34-system-reference.md` (`sys.blob.verify`) requires "complete content and its native dependency structure"; this build checks content only. The gap stays open under ornadb-5il1.120.
- Release scenario oracles (ORNA-TEST-011): `crates/orna-cli-v1/tests/scenario_oracles` runs every FINAL-* and V110-* oracle and every §39 mandatory scenario in a fresh `orna init` database through the product binary and Git. A scenario whose product behaviour is still missing is recorded as a known gap against its blocking bead, with a probe that must observe that blocker; a probe that finds the blocker closed fails the run (FAIL), and a probe that cannot establish its own precondition fails the run as a harness error. STALE gaps are ratcheted by `stale-entries.txt` (bead ornadb-ksrh). Test-only; no product behaviour change.
- VFS mount cases (ornadb-5il1.63 remainder): `crates/orna-cli-v1/tests/vfs_mount_cases.rs` adds ORNA-VFS-015 row-document coverage (two row saves admitted in order, with no cross-file rollback; an invalid later save is refused with EINVAL and keeps the earlier accepted row; grouped source edits are covered in part, one import edge, one rename and one group-only refusal (ornadb-5il1.372, partial; see the ORNA-VFS-020 bullet above); no-evaluation-during-speculative-writes, ORNA-VFS-020 proper, is not covered; ornadb-9npn), the first half of ORNA-VFS-019 (an open handle does not retarget after a sibling row write), and multi-page listing consistency for ORNA-VFS-017 over a 512-row Music table (laziness is proven by the unit test `workspace_readdir_uses_lazy_indexed_pages`). The shared fixture helpers widen the row ordinal from `u8` to `u16`, which had stopped at 112 rows (ornadb-5il1.465). Not covered: the cache-invalidation and reorder halves of VFS-019, the open-handle remount half (schema change followed by remount; ornadb-eufo), and nested-collection directories, because the format-3 fixture cannot build a nested module (ornadb-5il1.464). Test-only; no product behaviour change.
- FUSE mount integration coverage for the VFS-1 refusals and key projection (ornadb-5il1.63): symlinks and hard links (ORNA-VFS-014); unlink, mkdir and document rename on `data.orna` (ORNA-VFS-013, with mkdir and the `data.orna` rename also refused by EPERM as conservative choices the spec does not name); chmod of a document (profiles/vfs-1.md "Editable candidate and syscall rules"; refused with EPERM, a conservative choice because the spec does not name chmod); a Blob suffix rename (content.mp3 to content.wav) refused with EPERM (ORNA-VFS-012, a conservative choice the spec does not name); and case-distinct Str keys projecting to distinct rows (ORNA-VFS-016, case-folding half only). Test-only; no product behaviour change. Known gap: the product refuses creating scratch files (`.content.wav.replace`) inside a Blob-field row directory with EPERM, but 41-vfs.md "Temporary-save replacement" requires that scratch to be supported; the test is `#[ignore]` pending ornadb-5il1.463.
- FUSE coverage: a Str of 300,000 bytes saved through a mounted row document is stored in full and read back through the document. This does not exercise ORNA-VFS-010 or ORNA-VFS-018. Test-only; no product behaviour change.
- VFS projection coverage for module paths and key escaping (ornadb-5il1.464, ornadb-5il1.462, ornadb-5il1.372). Test-only for the .464 and .372 cases; the .462 cases change product behaviour and are listed under Fixed. New file `crates/orna-cli-v1/tests/vfs_projection_features.rs`, with the `format3.rs` fixture extended to write nested module trees (`git mktree` takes one directory level per tree). A reachable source module at a nested logical path (`lib/inner/deep.orna`) projects as its directory chain beside the table directory and reads its committed bytes (profiles/vfs-1.md Row/key directory mapping). Key components containing UTF-8, `%`, `~` and `/` project as uppercase `%HH` (profiles/vfs-1.md:44-50) and each row reads its own title; the fixture keys rows with the source path encoder. Ignored with bead id and red output recorded: a Str over 64 KiB is not projected as `<field>.txt` (ornadb-5il1.372); a row with nested record and list fields is refused at source admission before the mount is reached (ornadb-5il1.372). Not covered: create, rename and unlink inside nested collection directories (profiles/vfs-1.md:71-74).

- Gate E media reader evidence (ornadb-5il1.369, ornadb-5il1.245): `crates/orna-cli-v1/tests/vfs_media_reader.rs` mounts a captured 64 MiB WAV Blob with `mount --cwd` and checks byte-exact sequential, boundary, EOF, seek, random, readahead-hinted and concurrent reads, a mount-daemon peak RSS below the Blob size, and ORNA-VFS-018 (stat and readdir succeed over a corrupt loose chunk; the first read fails with EIO). mpv plays the mounted WAV where it is installed; CI requires it (`ORNA_REQUIRE_MEDIA_PLAYER=1`) and fails on any mpv error output. `cli-tests.yml` installs mpv and vim and records the Gate E media reader run as an uploaded diagnostic (command, `uname -r`, libfuse, mpv, emacs and vim versions) named `gate-e-evidence-media-reader`; no gate item selects it. The crate-wide step excludes the suite (`--exclude-target`), so it runs once. The nightly cli shard records mpv and vim versions too. Test and CI only; no product behaviour change. Evidence: `docs/conformance/gate-e-evidence-2026-10-29.md`.
- VFS-019 schema change under a live `--cwd` mount (ornadb-eufo): `crates/orna-cli-v1/tests/vfs_schema_change.rs` checks that an open handle keeps its captured row bytes, that live attributes and the listing stay on the pinned v1 generation, and that a v2-only optional field is refused with EINVAL on the live mount. The test file adds no product behaviour. The scratch-record change is covered by unit tests in `fuse_mount` (creation schema identity round-trips; a legacy record without identity is read; a corrupt record is reported and left on disk) and by `vfs_019_legacy_scratch_record_is_retained_not_bound_to_live_schema` and `vfs_019_unreadable_persisted_scratch_is_reported_and_left_on_disk`. The restarted-mount proofs `vfs_019_remount_after_schema_change_shows_new_shape` and `vfs_019_persisted_scratch_is_restored_after_schema_change` are un-ignored under ornadb-eufo.2.1 (see Fixed).
- Gate E mount-level evidence for items E2, E3, E6, E7, E8 and E9 (ornadb-5il1.245). Test-only; no product behaviour change. New tests: `rejected_sync_retains_recoverable_draft_listed_by_id` (`vfs_mount_sync_semantics`), `reserved_key_components_escape_first_byte` and `reserved_spelling_beyond_vfs_1_text_is_conservative` (`vfs_projection_features`; the second is spec-silent and pins conservative choices). The E2, E6, E7 and E9 selectors are existing mount tests; the E6 selector now pins the current stale-baseline limitation (a late temp replaces an owner change while `orna edit` refuses with E2000), not a protection. `vfs_mount_isolation` now waits for the projected row after its SIGKILL remount before reading it, because a readiness wait is needed there. Registered in `scripts/gate-evidence-items.toml`; per-item claims and limitations are in `docs/conformance/gate-e-evidence-2026-10-29.md`.
- Test support change (ornadb-5il1.245): `await_path` in `crates/orna-cli-v1/tests/support/editor_mount.rs` waits for the projected path and its mount to answer. It is shared by every `vfs_*` integration suite that calls it (`vfs_media_reader`, `vfs_mount_cases`, `vfs_mount_isolation`, `vfs_mount_sessions`, `vfs_projection_features`, `vfs_schema_change`), so the change affects all of them, not only `vfs_mount_isolation`. Behaviour of product code is unchanged.
- Gate E mount evidence, partial (ornadb-5il1.245): `crates/orna-cli-v1/tests/vfs_composite_key_mount.rs` mounts a committed table with a composite key and checks the nested directory projection (`Stock/north/widget%20001/data.orna`, `a%2Fb`, `south/c.d`). This is coverage, not a fix: it passes on origin/main (5ec573fea), so no product behaviour changes. E8 also selects existing nested-collection, long-key, large-Str, grouped-source, chmod and multi-file tests; E3 adds the SIGTERM draft-retention selector; E9 adds the `bin:orna` open-flag unit test, a constant check only. Remaining limitations (namespace collisions, the ignored empty-key test, ORNA-VFS-020 proper, mount-level kernel-cache observation) are named in `scripts/gate-evidence-items.toml` and `docs/conformance/gate-e-evidence-2026-10-29.md`.
- Gate B range-read overfetch evidence (ORNA-BLOB-012): `range_reads_report_whole_chunk_overfetch_against_requested_bytes` (`format_context.rs`) reads a Gear-chunked committed Blob through `read_blob_range` over ranges that start, end and straddle chunk boundaries. It checks that the payload bytes charged equal the intersecting chunks, that overfetch equals the whole-chunk overhang at both ends and stays below two `GEAR_MAXIMUM` chunks, and that the whole-blob read reports zero overfetch. Each range prints a `Gate B: range` line that gate item B4 records as a counter. Test-only; the payload-byte counter in the read path already existed and is unchanged.
- Gate B rehash-cost evidence (ORNA-VFS-010): `binary_save_and_metadata_save_report_rehash_cost` (`format_context.rs`) measures raw-content bytes digested in process. ORNA-VFS-010 permits a binary save to stream the entire resulting bytes; the test asserts only that at least the whole blob is digested. Measured in one run: a binary save of a 2,097,225 B Blob (capture, protect and accept together) digested 12,583,350 B, which is 6x the blob length. A metadata-only row insert fetched 0 payload B and digested 0 B of sibling content, as ORNA-VFS-010 requires. Product change: `ContentDigest::update` (`blob_store.rs`) adds each digested byte count to a thread-local counter that only a `#[cfg(test)]` accessor reads; the schema-identity digest is excluded. Not counted: object-id hashing by child `git hash-object` processes, which runs outside this process. Each run prints `Gate B: rehash` lines that gate item B6 records as counters.

### Fixed
- Partial (ornadb-5il1.366): a path truncate of a file with a pending cut is not lost or retained early. A read-only open shows the truncated view and leaves the cut pending, so it neither retains a draft at release nor consumes the cut; the cut is retained once when the mount stops (ORNA-VFS-009, ORNA-VFS-024; 41-vfs.md truncate clause). An accepted atomic replacement clears the pending path truncate of the file it replaces (ornadb-5il1.366.5, ORNA-VFS-024). The clear runs right after the commit succeeds, before any later post-commit step can fail. A replacement refused before commit keeps the cut. Parent run (origin/main 5ec573fea, `ORNA_REQUIRE_FUSE=1`): `cwd_mount_atomic_replacement_clears_pending_path_truncate` fails (the read-back shows the 13-byte truncated prefix) and `cwd_mount_read_only_open_after_path_truncate_retains_nothing_early` fails (a read-only open retained a draft before the mount stopped); both pass on this head. Coverage, not a fix: `cwd_mount_stale_replacement_keeps_pending_path_truncate`, `cwd_mount_truncate_after_failed_retention_keeps_one_truncated_record` and `cwd_mount_truncate_after_failed_release_retains_one_record` pass on the parent. Not covered by a test: the post-commit failure branches (the mount has no fault hook for them) and an open writer on the destination at an accepted replacement (spec-silent; flagged for review). Not done here: the duplicate Draft and Truncated record when a same-inode refusal is cut by truncate (ornadb-5il1.366.6), the same-inode overwrite in the unresolved map (ornadb-5il1.366.4), and reopen-after-truncate baseline seeding (ornadb-5il1.366).
- Partial (ornadb-5il1.366.4): two refused drafts of one inode stay recoverable through fsync, flush, release and mount stop. The second refusal no longer overwrites the first's unretained source (`retain_candidate` in `fuse_mount.rs` merges it), and the stop loop retries every unretained source of an inode, not only one without a retained draft id. A later path truncate cuts the live draft rather than the accepted baseline (ORNA-VFS-024, ORNA-VFS-009, 41-vfs.md EDIT-1). Residual: a retained draft id replaced by a later refusal of the same inode still drops out of the stop-time report (pre-existing; the record stays on disk). Regression tests `cwd_mount_release_after_same_inode_refusal_keeps_both_drafts` (release-time merge; new) and `cwd_mount_truncate_after_same_inode_refusal_keeps_both_drafts` (stop-time): on origin/main (b51af4f91) both fail (release: one body `"{ title: \"second draft is kept\",\n"` where two are expected; truncate: one body `"{ title: \"unsynced writes kept\",\n"` where two are expected, the 13-byte `"{ title: \"sec"` draft missing); on this branch both pass.
- Partial (ornadb-s5gh; ORNA-VFS-009; 41-vfs.md EDIT-1 step 4): a second path truncate that lands while a stop-retry is pending cuts the same candidate again. The first truncate's refusal held no candidate (its retention failed at stop), so the second truncate found nothing to cut, and the untruncated copy was kept as a second record beside the newest cut. A refusal is now superseded by a truncate that cuts its candidate, and a candidate-less refusal is cut by a truncate when no writer holds the file, so exactly one record (the newest cut) is retained. Regression test `cwd_mount_truncate_during_pending_stop_retry_keeps_one_record` (`vfs_mount_integration.rs`) retained two records before the fix and one after it. Flagged for review: the spec does not state the candidate-less case explicitly; this supersession rule is a conservative choice inferred from the truncate clause and EDIT-1 step 4.
- 64 MiB Blob candidate accept RSS (ornadb-5il1.366, partial): the candidate write and accept path makes fewer full-size copies. `cwd_mount_blob_64_mib_candidate_streams_in_bounded_rss` measures mount-daemon peak RSS growth above the post-mount baseline. One run each: on origin/main 9eab295fd, write and read-back 3,231,744 B and accept 88,264,704 B (84.2 MiB); on 7fa27a6eb, accept 20,045,824 B (19.1 MiB); on d5afa463d (streaming commit 8f1cfe389 included), write and read-back 3,432,448 B (3.3 MiB; 16 MiB bound) and accept 19,697,664 B (18.8 MiB; 32 MiB regression ceiling, lowered from 96 MiB). Two later runs of the same code, with the ceiling change aside, measured accept 21,106,688 B (20.1 MiB) and 20,283,392 B (19.3 MiB), with write 3,350,528 B. The accept growth is still above the 16 MiB goal, so this bead stays partial; the source of the remaining growth was not profiled [INFERENCE: not measured], and ornadb-u715 tracks the remaining copy sites. The 32 MiB ceiling is a regression bound with about 11 MiB headroom over the measured accept growth, not proof that no whole-candidate copy remains. The figures come from single runs each and are not averaged. Restored Blob read is 15.2 MiB.
- Restored 64 MiB Blob read-back RSS bound (ornadb-5il1.366.3.1): `cwd_mount_blob_64_mib_committed_restore_streams_in_bounded_rss` raises its mount-daemon RSS growth bound from 16 MiB to 24 MiB. Five measured runs grew 15,912,960, 15,015,936, 15,912,960, 15,912,960 and 14,950,400 B, so the 16 MiB bound left 0.8 MiB of margin. A non-streaming restore would grow by about 64 MiB [INFERENCE: not measured], which would fail the 24 MiB bound. `cwd_mount_truncate_after_failed_retention_keeps_one_truncated_record` replaces its fixed 500 ms stop wait (ornadb-5il1.366.6) with a wait for the mount's `retrying draft retention before unmount` stderr report, so the check runs once that failed retention is reported.
- A REPL write refused because a live writable mount holds the writer lease reports `ORNA-REPL-EFFECT` with its cause (the repository's runtime owner lock is held by another live process, so this REPL is a secondary writer and cannot take the writer lease) and the remedy (REPL database writes need the mount stopped: `orna unmount DIR`, then rerun the REPL; file writes while mounted go through `orna edit`). A refusal where this process holds the runtime owner lock reports only its cause, since no mount is implicated. Before, the cause was discarded at the lease acquisition and the refusal named only the code (ornadb-5il1.64; ORNA-TURSO-005, 20-embedded.md). The spec does not define a REPL write routed through the owner while mounted, so that route is not implemented (ornadb-5il1.64.1).
- A document write whose handle opened before the table's schema changed is refused instead of being accepted (ornadb-eufo.1, profiles/vfs-1.md:91-92 and :138). Each document row save carries the table schema identity it was admitted under: a SHA-256 over `orna.vfs.table-schema.v1` and the committed table's schema descriptor, captured when the handle's row save context is built. The runtime reads the live identity from the committed git `HEAD` inside the SQLite writer transaction, after the owner check, and compares it with the admitted identity twice: before the candidate is validated (`lib.rs` 7141-7144) and immediately before COMMIT, after the transaction's writes (`lib.rs` 7281-7285; `with_live_schema_identity` in `orna-runtime-v1` `vfs.rs`). A mismatch at either read fails with `RuntimeError::StaleTableSchema`; the VFS reports it as a stale save and retains the candidate for retry, and the ordinary write path replies `EAGAIN` (`fuse_mount.rs` `DocumentCommitError::Stale`), as profiles/vfs-1.md:138 names. On the `orna edit` xattr commit path the reply stays `ESTALE` (`fuse_mount.rs` near line 4526), as on origin/main; that is a known deviation from profiles/vfs-1.md:138, recorded below, and this change does not alter it. A schema change committed before either read is refused. Residual window: between the second committed-schema check and the transaction commit, a schema change is not refused. Schema changes are git commits made outside orna and take no orna writer lease; no SQLite lock covers git refs, so this window is not closed and the identity is not pinned for the whole writer transaction. Lock order (ornadb-eufo.2.2): the VFS operation gate and the projection epoch lock (`cache_epoch.generation`) are taken before the writer transaction, as documented on `commit_validated_table_activation`. The live identity is read twice per save. A commit that leaves the table's schema unchanged does not refuse other open handles. An `edit --row` whose pinned row no longer matches a changed Music declaration (for example `artist` replaced by `subtitle`) is refused with `EIO`, not `EAGAIN`, because the mount cannot read the row under the new declaration and no expected version exists to be stale (profiles/vfs-1.md:134 and :140; test `cwd_mount_edit_row_refused_after_head_changes_music_schema`). This entry does not close ornadb-eufo.2, and ornadb-eufo.2.2 stays open for the residual window.
- ornadb-eufo.2.2 is partial. The second committed-schema check is in place. It reuses the first check's identity while the committed `HEAD` oid is unchanged: the identity is a pure function of that commit, so the reuse costs one `HEAD` lookup (`git rev-parse`) and no second committed-project load. Measured once in the test repository (`cwd_mount_optional_null_blob_replacement_survives_remount`, one save): a committed-project load took about 78 ms and a `HEAD` lookup about 3 ms, and the reuse avoids a second load of that cost under the SQLite writer lock. The test repository is small; the load cost was not measured on a larger project. A moved `HEAD` is resolved again (`LiveTableSchemaIdentity` in `orna-cli-v1` `table_schema.rs`). The second check takes the reuse whenever `HEAD` is unchanged between the two reads, but no assertion targets the reuse path itself. `schema_commit_landing_between_identity_reads_refuses_the_save` covers a schema commit that lands after the first check and before the second, and asserts that the committed rows and CWD capture are unchanged after the refused save. The window between the second check and COMMIT is not closed: schema changes are git commits made outside orna with no writer lease, and no SQLite lock covers git refs. ornadb-eufo.2.2 stays open for that residual window.
- Known limit, not changed by this change (ornadb-eufo.1.1; profiles/vfs-1.md:138 names `EAGAIN`): the kernel rewrites an `EAGAIN` setxattr reply to `EOPNOTSUPP` (errno 95; fs/xattr.c:245-246 on v6.19), so the `orna edit` xattr commit path cannot carry `EAGAIN`. It keeps `ESTALE` (errno 116, which reaches the client unchanged; the kernel retries it once, so the daemon sees two calls) as its stale signal, and `orna edit` reports `E2000` for a stale baseline. An `EAGAIN` reply would surface as errno 95, which `orna edit` reports as `E2010` ("outcome unknown"). Kernel-limit proof: `crates/orna-cli-v1/tests/vfs_setxattr_errno_kernel_limit.rs`; observed on kernel 6.19.10: `EAGAIN` gives 1 setxattr call and errno 95, `ESTALE` gives 2 calls and errno 116. Production E2000 coverage: `crates/orna-cli-v1/tests/vfs_mount_isolation.rs` lines 446 and 824. A spec amendment naming the stale signal for edit commits on setxattr is recommended (ornadb-eufo.1.2).
- An exact replay of a document save the relation already holds is accepted after a table schema change instead of being refused as stale: the already-durable check runs before the schema identity check, so a retry of a published save is not reported as `StaleTableSchema` (ornadb-eufo.1, ORNA-VFS-019). A committed table schema that cannot be resolved at the live head counts as a mismatch and is refused with `StaleTableSchema`; it is never accepted.
- Scratch records carry the table schema identity they were created under, in the ORNAVFS2 record format (ornadb-eufo.2). Restoring an ORNAVFS2 record keeps that creation identity and does not read the live identity at restore, so it is not rebound to the current schema. An ORNAVFS1 record, written before that identity existed, has no creation identity and is not bound to the live schema: on restore its candidate is kept as a retained draft (cause `sys.vfs.stale_edit`), the record is removed, and no scratch node is mounted, so the candidate bytes stay recoverable and are re-applied through a normal write. Mount stderr reports `legacy scratch record (ORNAVFS1) kept as retained draft`. This is a conservative choice the spec does not name, and is flagged. A persisted record that cannot be read, or whose identity does not match its filename, is reported on mount stderr as `unreadable persisted temporary scratch` and left on disk; the mount still opens. Tests: unit `legacy_scratch_record_is_read_without_schema_identity` and `corrupt_scratch_record_is_reported_and_left_on_disk`; integration `vfs_019_legacy_scratch_record_is_retained_not_bound_to_live_schema` and `vfs_019_unreadable_persisted_scratch_is_reported_and_left_on_disk`. `vfs_019_persisted_scratch_is_restored_after_schema_change` restores the scratch on a restarted v2 mount once the row is admitted (ornadb-eufo.2.1, next entry).
- A restarted `--cwd` mount admits a CWD row written under an earlier committed schema generation instead of failing validation with `InvalidValue` and reading EIO (ornadb-eufo.2.1, ORNA-VFS-019, ORNA-EVOL-002). The stored row must validate under exactly one generation found on the `HEAD` lineage (`committed_history`, bounded at 1024 commits, newest first). It is then re-encoded to the mounted generation through `admit_row_under_descriptor` (`orna-repository-v1` `schema_evolution`), and the admitted bytes are what the mount exposes. A field that schema evolution introduced takes the constant default frozen by the commit that introduced it, read from that commit's own source (ORNA-FIELD-003, ORNA-EVOL-009); `compact.rs:1701` diverges from this rule and is flagged for review, not changed here. A table's committed history is scanned on a background thread outside the FUSE state lock and cached per table for the mount's lifetime. Each pass runs for at most 10 seconds (`HISTORICAL_RESOLVE_BUDGET`); the budget is checked before each commit, so a pass can overrun it by one commit's read. A pass that runs out keeps its progress, and the next row request for that table resumes from the commit where it stopped. While a scan is pending or paused, a row read fails transiently. A failed scan (a history longer than 1024 commits, a commit whose identities or committed table schema cannot be read, or a scan thread that cannot start) also fails transiently and is retried from the start once 5 seconds have passed (`HISTORICAL_SCAN_RETRY`). It is not cached as a refusal, so reads of that table stay EIO until a retry succeeds. A row that matches no generation, more than one, or a refused admission is recorded as refused and reads EIO for the mount, with the reason on mount stderr (`FUSE row projection failed:`). A commit without an identity manifest, or without a table record for the table, is skipped (F7). A `HEAD` that moves after a table's first scan is not rescanned for the mount's lifetime (F8). Persisted temporary scratch is restored at mount start. The scans of its tables share one startup budget of 10 seconds, advanced before the mount serves requests. A table whose scan does not finish within that budget keeps its progress, and its persisted scratch is left on disk and reported on mount stderr (`skipped unresolved persisted temporary scratch`); a later mount restores it once that table's scan completes within the budget. A null optional value made required converts by type but has no lossless value, so an owner commit of that change is refused by the commit-time check over committed rows (E2101, ORNA-EVOL-002) and HEAD does not move. The mount-side refusal of such a row is therefore defensive: no owner commit reaches it, and it has no end-to-end test. The skip rule, the 1024-commit bound, the exactly-one-generation rule, the scan-once cache, the time budgets and the startup scratch deferral are conservative choices the spec does not name and are flagged for review. Tests: integration `vfs_019_remount_fills_introduced_field_with_default_frozen_at_introduction`, `vfs_019_commit_the_row_cannot_admit_is_refused_and_mount_keeps_row` and `vfs_019_persisted_scratch_is_restored_after_schema_change`; unit `historical_scan_budget_tests` (`a_pass_without_budget_visits_no_commit`, `a_paused_pass_resumes_where_it_stopped_and_matches_an_uninterrupted_pass`). The budget expiry paths themselves (lazy pause and startup deferral) have no direct test.
- The committed-schema scan of a CWD table counts commits that carry no committed identities for the table (ornadb-eufo.2.1 gap 3; ORNA-EVOL-002, F7). Such a commit cannot declare a generation, so it is still skipped, but it is no longer skipped silently: the scan's settled result records the count and the mount reports it on stderr when it is non-zero (`committed schema scan of table ...: skipped N commit(s) without committed identities`). Test-only coverage added in `fuse_mount` (`identity_less_commits_are_counted_and_declare_no_generation`); the count is new API surface, not a spec-stated rule. Open and documented, not changed: a failed scan is reported on stderr and retried, not returned as a typed EIO reason (no `sys.vfs` error code exists for it in the catalog); a moved `HEAD` is not rescanned until remount, because the scan pins the lineage at the first pass (spec-silent for this case; flagged).
- An explicit `orna edit PATH` opened after an owner plain write starts from the fresh accepted row instead of being refused with `E2000` (ornadb-5il1.471; profiles/vfs-1.md:118-119 "`orna edit` supplies a strong read baseline"). The editor's view digest (SHA-256 of the bytes the editor was given) is checked against the owner node bytes before commit; a mismatch is refused with `EditError::Stale`. For a plain edit the commit's compare-and-swap baseline is the live row read at commit time. A grouped `edit --row` keeps the recorded baseline, so it is still refused as stale when another writer advanced the row. Known residual: the digest check and the commit read are separate reads under the owner's writer lease, so a change between them is not excluded by the digest alone.
- An fstat on a clean handle opened before an owner-routed `edit --row` reports the size of the generation that handle pinned, not the accepted size (ornadb-5il1.470; 41-vfs.md:38-40, profiles/vfs-1.md:126-133). Partial: a superseded node keeps its captured bytes and save context, so such a handle keeps its pinned size across an owner-routed accept. A temp-file rename over `data.orna` now also replaces the live node with a new generation when an open handle pins it, so a handle opened on the live row keeps its pinned size and bytes across the rename (ornadb-5il1.470.3, 41-vfs.md:38-40). `reader_opened_on_live_row_survives_temp_rename_over_row` fails on the pre-fix parent (`fstat` reports the admitted replacement's 93 bytes where the pinned size is 49) and passes after the change. Tests `reader_opened_before_owner_write_keeps_fstat_size` (un-ignored) and `reader_opened_before_owner_write_survives_temp_rename_over_row` in `vfs_mount_isolation`. The same replacement re-projects a row whose Str over 64 KiB shrinks below the bound: the pinned and unpinned arms of `fuse_mount.rs` re-project the row's `<field>.txt` from the accepted document, so the stale `.txt` leaves the row directory and a pinned reader keeps its bytes (ornadb-5il1.470.3; profiles/vfs-1.md:63-80). Tests `pinned_reader_survives_temp_rename_shrinking_large_str_row` and `temp_rename_shrinking_large_str_row_removes_text_sibling` in `vfs_mount_isolation`. Mutation on origin/main (5bf8a8932): `pinned_reader_survives_temp_rename_shrinking_large_str_row` and `temp_rename_shrinking_large_str_row_removes_text_sibling` fail ("the stale title.txt sibling leaves the row directory"), and `reader_opened_on_live_row_survives_temp_rename_over_row` fails (`fstat` 93, pinned 49). `writer_handle_pinned_before_owner_rename_over_is_refused_and_candidate_retained` passes on origin/main, so it is coverage, not a fix. A failed `.txt` re-projection is logged with `eprintln!` and does not fail the accepted rename; that outcome is spec-silent and flagged for review. `path_stat_after_owner_write_reports_accepted_size_in_loop` (ornadb-5il1.470.4) passes on the pre-fix parent as well, so it is coverage, not a fix; the same-jiffy stale attribute window it targets was not reproduced and ornadb-5il1.470.4 stays open.
- A writable document handle's close flush compares against the row it captured at open (ORNA-VFS-011, 41-vfs.md:80). A first writer that closes after a second writer's accepted commit, or after a grouped edit, is refused as stale and retained, and the owner's row is kept (ornadb-5il1.470.1). Partial: `cwd_mount_close_flush_after_grouped_edit_is_stale` and `cwd_mount_first_writer_close_after_second_writer_commit_is_stale` also pass on the pre-fix parent, so they do not reproduce the shared-baseline defect the bead describes; no reproducing scenario was found, so F1 is unproven and flagged. A handle pinned to a superseded generation is refused as stale and its candidate retained when a later grouped edit returns the row to the bytes it captured, a conservative choice the spec does not name (ornadb-5il1.470.2; 41-vfs.md:38-40). A superseded handle with no unsynced candidate is acknowledged on fsync or close instead of refused (`cwd_mount_clean_fsync_on_superseded_generation_is_acknowledged`; it passes on the pre-fix parent too and guards against an unconditional superseded refusal). `cwd_mount_close_flush_on_superseded_generation_is_stale_when_row_returns_to_capture` fails on the pre-fix parent at its restore grouped save (E2000), before its refusal assertion, so it does not isolate the dirty refusal on the pre-fix parent; the dirty refusal is kept and is not separately shown new. Partial: a plain owner-routed `orna edit` does not advance the recorded baseline, so a handle opened after it still gets a stale first flush (commit 2704cfac4; flagged for review).
- Live mount directory listings keep their readdir offsets across removal, replacement and row refresh (ornadb-5il1.372, ORNA-VFS-017, 41-vfs.md; profiles/vfs-1.md). Each directory entry takes a cookie when it is added and never reuses it within that directory; its readdir offset is the cookie plus 3, since `.` and `..` take 1 and 2. A listing resumed from a kernel cursor continues after the entry that cursor names, so removing an earlier sibling neither skips nor repeats entries. A surviving sibling keeps its offset when its node is swapped by a Blob replacement or a row refresh, and a stale-row refresh reconciles the row's projected files by name instead of recreating them. A replaced or removed directory stays loaded while a handle opened with `opendir` or a stateless `open` is held at or below it, so that listing keeps enumerating and its readdir does not fail with ENOENT; the last release removes it with its children (41-vfs.md:39-40). A Blob opened with no write intent is tracked the same way, so a replacement leaves its bytes readable to that handle. This is not a pinned snapshot: the listing is live, so an entry removed after the handle was opened is no longer listed. The spec text cited here pins open handles to their generation (41-vfs.md:39-40) and names no pinned directory listing, so the live listing is a spec gap flagged for review. Covered by the unit tests in `readdir_cursor_tests` in `crates/orna-cli-v1/src/fuse_mount.rs`; the row-refresh path and a FUSE-level held-handle test have no direct coverage in this change.
- Nested record, tuple and list fields project as directories named by their escaped field name (ornadb-5il1.372; profiles/vfs-1.md:71-75; ORNA-VFS-016). A field named like a reserved host name projects as its escaped spelling (`con` becomes `%63on`), and the directory takes the same collision refusal as a sibling file. Before, the raw field name was used. Before, every accepted save with nested directories left the previous subtree in memory until unmount; now a retired nested subtree is removed with its children unless an open handle is at or below it (see the readdir entry above). Known: retention follows tracked handles, not kernel lookup counts (see `retire_node`). Known: an accepted `data.orna` save of a flat row gives its Blob children new inode numbers; held handles keep their captured generation, so content is unaffected (profiles/vfs-1.md:128-129 does not specify inode identity across saves).
- The live FUSE mount projects a Str field over 64 KiB (65536 bytes) as a `<field>.txt` file beside `data.orna` (ornadb-5il1.372, profiles/vfs-1.md:63-80, 41-vfs.md VFS-010/013/018). `data.orna` names the file as `{text: "./<field>.txt", encoding: "utf-8"}` instead of carrying the text, and a Str of exactly 65536 bytes stays inline. Reading the `.txt` returns the stored Str bytes exactly. A write to the `.txt` replaces the Str through the row's CAS boundary when its bytes are valid UTF-8; bytes that are not UTF-8 are refused with `EINVAL` at fsync and the accepted row and its `.txt` stay unchanged. Unlinking a required Str `.txt` is refused with `EPERM`.
- An accepted `--row` edit of a mounted row that holds a Str over 64 KiB re-projects the row's `data.orna` and `<field>.txt` from the accepted document, so the edited row is read back without remount (ornadb-5il1.372). A Str field whose inline string literal is at most 64 KiB stays inline when another field of the row is replaced, instead of being refused as over the bound. The `\0` escape in an Orna string literal decodes to NUL in the evaluator, matching the lexer (`orna-evaluator-v1`).
- Known limits of the Str `.txt` projection (ornadb-5il1.372): a read or a write copies the whole Str (`NodeKind::Text` owns its bytes, and the replacement path copies them into an `Arc`), and each accepted save re-projects the row's `data.orna` and `<field>.txt` from the accepted document. Range-wise projection of a large Str is not implemented.
- Source rows decode an Orna string literal with the Orna escape table through `orna-syntax-v1::unescape_string_literal`, the decoder the evaluator uses, instead of JSON (ornadb-5il1.372). A row whose literal holds an Orna escape JSON does not know (`\0`, `\u{...}`) or a raw control character is admitted with its exact text; before, the literal failed to decode and the row was not admitted.
- Source rows refuse the escapes that only JSON spells (`\/`, `\b`, `\f`, `\uXXXX` without braces) that `serde_json` admitted before (ornadb-5il1.372). The spec escape table lists only `\"`, `\\`, `\n`, `\r`, `\t`, `\0` and `\u{hex}` (Orna-1.1.0.md string escapes), so such a literal is refused rather than decoded as JSON.
- A stale Text or Blob candidate committed through a write-open handle is kept in recovery storage before its fsync or close replies, as a refused candidate is (ORNA-VFS-009, ornadb-5il1.372). Before, it was recorded as refused without being retained.
- Source admission decodes a row's key components by the VFS-1 spelling, so the rows the mount projects are admitted (ornadb-5il1.462, ORNA-VFS-016, profiles/vfs-1.md:44-60). Each component decodes from its `%HH` escapes, `~empty`, or a `~key-<64 lowercase hex>` alias. An alias resolves only through a supplied pair of key text and canonical OVB-1 encoding: the encoding must digest to the alias (SHA-256 of the encoding), and the text must be the canonical text of the typed key that the encoding decodes to (`decode_canonical(encoding).component_text() == text`); a pair that fails either check is refused with `InvalidSourceRow`. The mount supplies the pair from the committed key. The legacy key-path-v1 spelling (`~xx`, `~ff`) is refused with `InvalidSourceRow`, because the spec admits legacy rows only through a recorded import context (Orna-1.1.0 "File extension"); no loader in this build records one, and ORNA-REPO-CONTEXT-004 still refuses loose rows. `.`, `%`, a literal `~empty` key, a key whose encoding exceeds 200 bytes, and a key of 300 or more characters are admitted and project under their VFS-1 names. The empty key is not admitted by this build: a text primary key of `""` maps to an empty `table_row.row_key`, which the table schema refuses (`CHECK (length(row_key) > 0)`, orna-runtime-v1 lib.rs:174), so the empty-key case is blocked and tracked in ornadb-5il1.462.1. `admit_source_row` takes no key components, so it admits canonical spellings only and refuses an alias component. Test-only changes: `media_commit` passes each key once to the component helper (no double encoding), and `publication_crash_matrix` and `publish_no_visible_change` pass the new argument. In `vfs_projection_features.rs`, `dot_percent_and_literal_tilde_keys_are_admitted_and_escaped`, `long_key_over_200_encoded_bytes_projects_as_sha256_key_alias` and `key_over_300_characters_is_admitted_and_projects_as_alias` are un-ignored; `empty_key_is_admitted_as_tilde_empty` stays `#[ignore]`d against ornadb-5il1.462.1. The long-key test asserts the alias, the row's title and artist, and the exact listing, not the whole rendered document. The `.372` cases stay ignored.
- A stored record field inside a row is admitted in canonical encoded-key order (ornadb-5il1.372; Orna-1.1.0.md:4015 "Encoders sort by the encoded field key"; implementation/29-formats.md:46). The stored-row validator matches fields by key rather than declaration position, so a nested record row whose declared order differs from encoded key order is admitted rather than refused as InvalidSourceRow. Nested record, tuple, list and text-keyed map values, and present optionals, project as directories in the live mount (ornadb-5il1.372; profiles/vfs-1.md:71-75). Each nested directory lists one level in pages of at most 256 entries (profiles/vfs-1.md:75): a readdir lists the next page, and a lookup lists pages until its name is listed. A lookup of an absent name lists the whole level, since no page can be ruled out; this is a conservative choice the spec does not name, flagged for review. A refresh after another writer's accepted row replaces the row's nested directories with that row's. The case `nested_record_and_list_fields_project_as_pinned_directories` in `vfs_projection_features.rs` is un-ignored. Not projected as directories: map keys that are not text and set values, which stay inside `data.orna` (ornadb-5il1.372 stays open for them).
  Nested leaf files (record fields, tuple and list elements, optional values) are read-only (mode 0444); a write open of one is refused with `EPERM` (ornadb-5il1.372).
- PARTIAL (ornadb-5il1.372, profiles/vfs-1.md:71-75, 81-83): an accepted `data.orna` save reconciles the row's projected child files by name, nested record and list directories and their leaves included, so a new open shows the accepted nested value. Before, only a `.txt` sibling was reconciled on an accepted save and a nested leaf kept the value from its last load. Not covered: writable-baseline invalidation after a containing collection changes (profiles/vfs-1.md:73-74). Lazy paging of large nested enumerations (profiles/vfs-1.md:75) is implemented (see the nested directory entry above); a page is recorded as listed only after all its children are inserted, so a failed page is retried by the next lookup or readdir.
- Path `stat` after an owner write reports the accepted row's size (ornadb-5il1.468, ORNA-VFS-019, 41-vfs.md:38-39, profiles/vfs-1.md:133). A path `stat`, `lookup` and `setattr` without a handle take a document's size from the accepted image (`ManagedFile::stat`) rather than from the node cached at the last save, so `stat` agrees with a new open after an owner write. A writer's own path `stat` reports its own draft size, and another open's path `stat` reports the accepted size after a refused writer. Open handles keep their captured bytes, and a writer opened before the owner write is still refused with `EAGAIN` on fsync. Known residuals, with the violated clause named: (1) FUSE passes no handle to `fstat`, so `fstat` on a clean handle opened before an owner write reports the accepted size instead of its pinned size (41-vfs.md:39-40, profiles/vfs-1.md:128; ornadb-5il1.470). (2) An `orna edit` started after an owner-routed plain edit is refused with `E2000`, because the owner's explicit-edit compare-and-set uses the mount's cached baseline rather than the editor's read-to-save baseline (41-vfs.md:75-77; ornadb-5il1.471). (3) The cached node bytes are not refreshed for an open-time truncate or rename, which still uses them; no test covers this yet. Tests in `crates/orna-cli-v1/tests/vfs_mount_isolation.rs`: `stat_after_owner_write_reports_fresh_size`, `writer_stat_reports_its_own_draft_size`, `writer_opened_before_owner_write_is_refused_with_eagain`, `reader_opened_after_owner_write_sees_fresh_bytes_and_size`, `reader_opened_before_owner_write_keeps_old_bytes`, `reader_opened_before_shrinking_owner_write_keeps_old_bytes`, `other_open_stat_after_refused_writer_reports_accepted_size`; ignored with their beads: `reader_opened_before_owner_write_keeps_fstat_size` (.470) and `orna_edit_started_after_owner_write_starts_from_fresh_row` (.471).
- The live FUSE mount applies 41-vfs.md VFS-012 and VFS-013 to Blob fields (ornadb-5il1.370). Renaming a Blob to a suffix its MIME admits (for example `content.mp3` to `content.mp1` under `audio/mpeg`) commits only the descriptor's suffix hint through the row's CAS boundary; the bytes are not transcoded and the field is not renamed. A suffix the MIME does not admit is refused with EPERM. Unlinking an optional Blob field commits its descriptor as `null` and removes its file; unlinking a required Blob field is refused with EPERM. An unlink whose `null` descriptor the table's schema refuses is refused with EINVAL, and the file and `data.orna` stay unchanged. `data.orna` remains non-unlinkable. A rename or unlink refused as stale (`EAGAIN`, because another writer advanced the row) refreshes `data.orna` to the current row, and the retried rename or unlink commits against that row. The rename and unlink hold the mount's namespace guard across the baseline capture, the commit and the directory update, so no other namespace operation on the mount can claim the new name in between. Known residuals, not fixed here: on the save path the baseline is recorded before the node bytes update under the lock, so a concurrent save can interleave between the two; and readdir offsets shift when an entry is removed (LOW-2), so a listing held across an unlink can skip or repeat an entry. These are proven by focused integration and unit tests, not by the full mount suite.
- FUSE isolation coverage for live-mount editor and REPL coherence (ornadb-5il1.368). Test-only; no product behaviour change. An owner write is visible to a new open while a handle opened before it keeps its generation. A stale `orna edit` after a concurrent row change is refused and keeps its draft (shown with `--debug`, ORNA-UX-004). A REPL write while a live editor mount holds the writer lease is refused with ORNA-REPL-EFFECT (ORNA-TURSO-005). A blind replacement (an editor that writes without a read baseline overwrites a concurrent owner change) is a demonstrated limitation, not a fix (profiles/vfs-1.md:118-119; 41-vfs.md:65-77). Known gap, fixed for path `stat` under ornadb-5il1.468: refreshing the node from `stat` re-baselines open writers and was reverted because it defeats the stale-save refusal.
- A Blob-field row directory accepts editor scratch as 41-vfs.md "Temporary-save replacement" (line 67, ORNA-VFS-011) describes (ornadb-5il1.463). This applies to every row directory, including those in the `vfs_unsupported_ops` and `vfs_mount_sessions` tests. An unknown name is a name that is not an existing child of the row directory (not `data.orna` and not an existing Blob field file); it is private scratch, not a new column. Creating it succeeds and captures the row baseline; its fsync persists scratch bytes only. Renaming a Blob-bound scratch onto its Blob replaces the Blob through the baseline CAS; a stale baseline is refused with `EAGAIN` and the scratch stays retained (ORNA-VFS-009). An unbound scratch commits to the destination its rename names, the row document or a Blob field, through the same fresh image target as a bound scratch; after a Blob rename `data.orna` is served from the accepted row. Each mount admits at most 16 unbound scratch records it created itself; a create past that cap fails with `EDQUOT`, and unlinking or renaming one frees its slot. Records restored from an earlier mount are retained drafts and are not counted. A name that already exists fails with `EEXIST` before the cap is checked. Names whose Blob value is NULL are accepted as scratch, and renaming onto them is refused with `EPERM`. Storage failures in these paths reply `EIO`.
- The live FUSE mount no longer copies a whole EDIT-1 candidate to serve a write, a size query, a read or a draft retention (ornadb-5il1.366.1). `read` on a candidate-backed handle reads only the requested range from the spool, and a document or temporary candidate's size comes from its length rather than its bytes. Retention of a refused or unlinked candidate from the live mount streams the candidate through a `CANDIDATE_CHUNK_BYTES` (1 MiB) window (`retain_refused_scratch`), digest pass then write pass, so the mount-side retention holds one window of a 64 MiB candidate at a time. The vfs-layer retention paths (`retain_rejection`, `retain_retryable_conflict`) no longer take a whole copy through `candidate_arc()`: a retained record takes over the spool-backed candidate and its `DraftQuota` charge rather than copying it into memory. A refusal whose retention fails keeps its live candidate spool, still charged to `DraftQuota`. `fsync_with` still copies the whole candidate through `candidate_arc()` (ornadb-5il1.366.1.5), and the Document and text-Blob commit paths still take whole-value copies through `candidate_bytes()` (`fuse_mount.rs`).
- The live FUSE mount streams chunked candidates through persist, restore and retarget without a whole-value copy on those steps (ornadb-5il1.366.1). A Blob temporary commit computes its SHA-256 over the spool in `CANDIDATE_CHUNK_BYTES` windows (`commit_blob_temporary`), so the digest does not hold the candidate in memory. The digest is taken outside the scratch operation lock: a scratch write that lands before admission is refused by the vfs identity check before any row is touched, and the commit retries the digest once and then refuses with a save-again error. It never commits bytes that differ from the digest. Persisting a retained candidate, restoring a retained record and retargeting a pending truncate move the candidate in 1 MiB windows, and `CandidateSnapshot` serves ranged reads from the scratch spool. Tests: `draft_spool_memory` bounds the runtime live heap for a 64 MiB candidate under 8 MiB; `cwd_mount_blob_64_mib_candidate_streams_in_bounded_rss` bounds the mount daemon's RSS growth below 16 MiB across a 64 MiB Blob candidate's write and read-back, measured before the accept (ornadb-5il1.366.3); `cwd_mount_blob_64_mib_committed_restore_streams_in_bounded_rss` bounds the restarted daemon's RSS growth below 16 MiB while a 64 MiB committed Blob is read back in 1 MiB ranges after a remount; `cwd_mount_blob_racing_sibling_accepts_never_commit_mixed_bytes` races sibling handles' syncs on one Blob and asserts the committed bytes equal one accepted candidate or the original. The persist, restore-record and retarget windows are not RSS-measured by any test. Residual whole-value copies, each tracked under ornadb-5il1.366.1: the Document commit and accept bytes (ornadb-5il1.366.1.1), the Temporary baseline and the Blob `replacement_bytes` (ornadb-5il1.366.1.2), the Blob commit bytes (ornadb-5il1.366.1.3), the `fsync_with` copy (ornadb-5il1.366.1.5) and the text-Blob commit copy (ornadb-5il1.366.1.6).
- A path `stat` on the live mount (getattr without a handle) while a Document write is open reports the open candidate's length rather than the accepted baseline's, matching the handle-based size (ornadb-5il1.366.1). A path `truncate` while a Document writer is open, or after that writer was released unretained, is recorded as pending and cut against the newest writer candidate when retention runs, not against the accepted baseline. Known residuals: a handle released unretained keeps its spool charged to the shared `DraftQuota` until its refusal is resolved, and a reopen after a pending truncate still applies the truncate to the accepted baseline.
- A pending path truncate is cleared when a later Document write on the same file is accepted on the live mount, so it no longer applies to an unrelated later candidate (ornadb-5il1.366.5). A Document accept applies a pending path truncate to the candidate before it commits. The spec does not name the order of a path truncate and an accept, so this build applies the truncate first and flags it for review. When retention applies a pending truncate to a released candidate, the superseded retained record is removed only after the truncated candidate is retained (the retention-order part of ornadb-5il1.366.6; the same-inode duplicate-record part is fixed in the entry below). If that retention is refused, the superseded record is kept, so the only copy of the candidate is not dropped. Retained drafts already on disk are charged to the shared `DraftQuota` when the recovery store opens and released when a record is removed; `DraftQuota::release` saturates at zero, so an over-release cannot wrap the budget. Known residual: a reopen after a pending truncate still applies the truncate to the accepted baseline (ornadb-5il1.366, not fixed here). Test-only addition: `draft_spool_memory` streams a 64 MiB candidate through `EditDraft` in 1 MiB chunks and asserts the peak live heap stays under 8 MiB (ornadb-5il1.366.3; the 64 MiB RSS bound itself is ornadb-5il1.366.3.1, still open).
- A path truncate of a file whose unretained candidate was refused no longer retains a duplicate record (ornadb-5il1.366.6, ORNA-VFS-009, ORNA-VFS-024, 41-vfs.md EDIT-1 step 4). `retain_all_drafts` skipped an unretained candidate cut by a path truncate only for the first pass, so the same inode could be retained twice, once as a Draft and once as a Truncated record. Only the Truncated record is retained now. Covered by `cwd_mount_truncate_after_refused_unretained_candidate_retains_one_record`, which fails on the prior code and passes on this one.
- Partial (ornadb-5il1.366.6, part b; no behaviour change): a reopen for write after a path truncate starts from the accepted baseline cut to the truncated size (ORNA-VFS-024, EDIT-1 steps 1-2, 4). Pinned by `cwd_mount_reopen_after_path_truncate_applies_truncate_to_accepted_baseline`, which makes the baseline with an explicit fsync and then closes a clean handle; the pin does not depend on the close-time flush. The residual above stays open. Spec reading [INFERENCE, flagged for review]: EDIT-1 step 5 lets a close-time flush accept an unsynced candidate or not, and this build accepts it. The residual is whether a reopen should apply a pending truncate to the accepted baseline at all, which this entry does not settle.
- A writable VFS-1 mount no longer lets an unsynced draft escape to other opens. The kernel sends FLUSH when a descriptor inherited through `exec(2)` is closed by the child process; that FLUSH carries the child's lock owner, and the mount previously accepted the draft on it, making the unsynced write visible to a second open and to a remount after SIGKILL. The mount now records the pid that opened each handle, and a FLUSH from any other process acknowledges without accepting the draft; the opener's own close commits it. This is this build's PR policy (ornadb-5il1.64), not the EDIT-1 step 5 text. Not covered by this gate: a FLUSH from a duplicate of the writer's own descriptor in the same process (same lock owner, so it still accepts mid-write), and `fsync` from another process, which still accepts as EDIT-1 steps 1-2 require. Known gap: a temp created before a peer's accepted owner change is still refused with `EAGAIN` on rename over the row, so `read_handle_across_owner_edit_temp_created_after_it_renames_f2` is `#[ignore]`d against ornadb-5il1.466. Tests: `crates/orna-cli-v1/tests/vfs_mount_isolation.rs`.
- Known limit of the owner gate (ornadb-5il1.64): the owner is the pid that opened the handle, not the process that writes through it. If that descriptor is shared with another writer (a `fork(2)` without `exec(2)`, or `SCM_RIGHTS` passing) and the opener closes or exits first, the opener's FLUSH is the owner FLUSH and commits whatever is dirty at that moment, while the other process still holds the descriptor. Writes that process makes afterwards are acknowledged on close without acceptance and stay unresolved until release keeps them as drafts. No test covers this case.
- `orna mount status` lists each writable `--cwd` mount with its mode, mountpoint and view, and each unresolved or rejected draft of that mount with its exact Orna cause code (`sys.*`, e.g. `sys.vfs.stale_edit`, or `sys.vfs.draft_retained` for a kept candidate with no other cause) and retained draft id. The human line `  unresolved draft PATH code CODE draft ID: DETAIL` is this implementation's choice; the spec fixes only the codes and ids (41-vfs.md:106). `--json` carries `code`/`draft` on each `unresolved_drafts` object, plus `draft_source` (`fields` or `list`), `drafts_truncated` and `drafts_omitted` (ORNA-VFS-011/024; profiles/vfs-1.md). A draft with no profile code (backing I/O, unsynced write) reports an empty code (`-` in the human form, `null` in JSON). The daemon lists at most 64 drafts within 32 KiB; the rest are counted, never drawn as a placeholder draft (ornadb-5il1.365).
- `orna mount status` against a daemon that predates the `user.orna.unresolved-draft-fields` xattr (ENODATA) no longer fails the listing: it prints each draft from the two-column `user.orna.unresolved-draft-list` with empty `code` and `draft` columns (`-`), reports `draft_source` `list` in JSON, and prints a `note:` line saying the daemon predates them (ornadb-5il1.365).
- EDIT-1 candidate drafts in the runtime VFS layer (ORNA-VFS-1, 41-vfs.md EDIT-1, `EditDraft`) are backed by a private, unlinked, 0600 spool file in a `0700` `vfs-draft-spool-v1` directory under the runtime root rather than a RAM vector. The spool directory must be a real directory owned by this user (symlinks are refused), and stale spool files from an earlier crash are swept at startup. Sparse writes near the draft bound do not allocate their holes on disk, and ranged reads and writes stay on the spool. Every logical byte of every open runtime candidate, and every retained rejected copy, is charged to a shared `DraftQuota`; a growth that exceeds the quota fails with `EDQUOT` and changes nothing. The process default is 1 GiB (eight 64 MiB drafts fit); the spec names an aggregate quota without a value, so this bound is conservative and flagged for review. A spool I/O failure returns `EIO`. The live FUSE mount uses the spool for scratch writes, size queries and ranged reads (ornadb-5il1.366.1). The on-disk `VfsRetainedDraftStore` is quota charged: a new retained record absorbs its candidate's charge, and removing the record releases it. Whole-candidate copies remain at `fsync_with`, the Document commit and accept, and the text-Blob commit (ornadb-5il1.366.1.1, .5 and .6; ornadb-5il1.366).
- Bare `orna run` (ORNA-RUN-002) invokes the root `main()` of a project, including a project committed with `orna commit` over tables. A project with no root `main()` now reports `E2103 no root main(): main.orna declares no \`main()\` function` with the remedy to declare `main()` or run a qualified function, instead of the generic `E2000 durable project invocation is not available`. A qualified function in an `ingest/main.orna` module runs exactly like the same function in another file (ORNA-RUN-003): the path carries no special meaning, and the module is reached through its namespace once the root imports it (ornadb-5il1.122).
- Draft retention failures (ORNA-VFS-024, ornadb-5il1.367) are no longer swallowed. A candidate that recovery storage cannot keep is refused with its retention errno: `EDQUOT` when the store is full (256 drafts), `EIO` for any other storage failure. This covers a rename whose replacement cannot be retained, a refused scratch unlink, and a document accept, fsync or flush whose candidate cannot be retained. The write stays an unresolved draft and unmount with unresolved drafts stays blocked. A stop whose draft retention fails keeps the mount up and retries retention, logging the unresolved draft count and cause at most every 5 seconds. A second stop signal (SIGINT, SIGTERM or SIGHUP) ends the wait: the stop prints each unretained draft as lost, exits non-zero, and leaves the mount up rather than unmounting over unresolved drafts. A mount-error stop with unresolved drafts takes the same path. `user.orna.retain-drafts` replies with the retention errno (`EDQUOT` or `EIO`).
- A draft-quota refusal at save is EDQUOT, not EIO (ornadb-5il1.367). When the aggregate draft quota refuses a document, fsync or flush candidate (`EditDraftError::QuotaExceeded` or `FileTooLarge` from a baseline-only candidate's reservation), the save fails with EDQUOT and the candidate stays an unresolved draft, as 41-vfs.md:105 requires for resource limits. Before this change the save mapped to EIO. The unit test covers the save-error classification that the document, fsync and flush replies share; it does not exhaust the 1 GiB process-wide draft quota through a live mount, which has no test seam.
- Unsupported mutations on the live writable VFS-1 mount return the profile errno (EPERM; profiles/vfs-1.md:138-140, ORNA-VFS-014) and log the stable cause `sys.vfs.unsupported` to the mount's stderr. Hard links, symlinks and device nodes (`link`, `symlink`, `mknod`) previously returned `EOPNOTSUPP`; `setxattr` for any name other than the mount control names and `removexattr` previously returned `ENODATA`/fuser's default `ENOSYS`. Both now return `EPERM`. `mkdir`, `rmdir`, cross-directory rename and rename flags are refused the same way with the cause logged (ornadb-5il1.364).
- A projected Blob opened for writing (ORNA-VFS-1, 41-vfs.md EDIT-1) now gets a private candidate seeded from the current bytes; truncating the open empties it before any write, and writes through the handle are accepted on fsync or close as the row's replacement. A later write on the same handle starts a new candidate from the accepted bytes. A write-open captures the row as it was when opened, so an fsync after another handle has already replaced the row is refused, and its bytes are kept as a draft that blocks unmount until resolved. Blob candidates are spool-backed and charged to the shared `DraftQuota` (see the draft-spool entry above; ornadb-5il1.366). Previously every write-intent open of a Blob was refused with `EPERM`. Fstat on a Blob with an open candidate reports the candidate size.
- `orna run` over a project committed with plain git (without `orna commit`) now reports `E2200 committed table identities are unavailable` with the remedy `run \`orna commit -m MESSAGE\``, instead of the misleading `E2200 committed table rows could not be read`. The refusal itself is not new: such a commit has no object identity inventory and was already refused; this change only makes the diagnostic accurate (ORNA-RUN-001/002, ornadb-4w8z). An unreadable `HEAD` now fails closed with `E2200 committed HEAD is unreadable`; an unborn `HEAD` still passes through. `ObjectIdentityManifest::read_committed_if_present` distinguishes an absent inventory from an unreadable one.
- The REPL admits module-level cross-table assertions (`assert every(Loan, loan => exists(Book, book => ...));`, ORNA-ASSERT-021..030) instead of refusing the whole session with `E2200`. A REPL write that changes a table in the assertion's dependency set evaluates it against the candidate database before publication; a false result rejects the write with `ORNA-A091-009` and leaves the store, the Git HEAD and the Git status unchanged (ORNA-ASSERT-029, ORNA-A091-009). Keyless tables (ORNA-AUTOID-001) carry their implicit `id: Int` key into the assertion check. A module assertion over one table is refused by `orna commit` with `E2101` whose detail names `ORNA-A091-003` (ORNA-ASSERT-022); one with no table dependency is refused with `E2101` naming `ORNA-A091-012` (ORNA-ASSERT-023). Regression test: `repl_module_assert`.
- REPL module-assertion gating (ORNA-ASSERT-029) no longer skips a module assertion whose dependencies are not all visible to the gate. The dependency analysis now visits every expression form, including range bounds, and treats a helper used as a function value (ORNA-LAMBDA) through its summary; a dependency set that cannot be proven complete evaluates the assertion on every write. Module tables are identified by their qualified name (`a.Item`), so a write to a non-root module table evaluates its assertion. Declaration assertions that read tables through a helper or `count()` are refused with `ORNA-A091-007` (ORNA-ASSERT-039), so no such dependency is admitted.
- The REPL evaluates `main.<Table>` against the CWD, not HEAD: its evaluator rows and Blob-restore base are the activation snapshot's rows (committed `HEAD` overlaid by uncommitted local rows), so an uncommitted REPL insert is counted by `main.Contact | count()` (Orna-1.1.0 `19-repository.md`, ORNA-STATE-002, ornadb-5afo). The HEAD half of STATE-001 is checked through `orna query --at HEAD`; `as_of(HEAD)` is unexecuted in the REPL and is not claimed there. Scenario `STATE-001` is now a passing oracle.
- Publication recovery for a runtime-pending publication whose HEAD has moved to a descendant of the candidate now finalizes the runtime tail without rewriting HEAD, index or worktree, as ORNA-PUB-009's recovery table requires ("Ref no longer H or N": inspect ancestry, reconcile, never overwrite) (ornadb-5il1.47). `RuntimePublicationCoordinator::complete` and `recover` accept such a HEAD (previously `RefConflict`) when the candidate is an ancestor, and `Repository::mark_runtime_complete` verifies the reconciled index against the journal before finalizing: a managed entry that the index no longer carries returns `StaleIndex` and keeps the journal. `recover` returns `RuntimeCompletionRequired` for runtime-pending journals and the caller finalizes through `mark_runtime_complete`. A HEAD that does not contain the candidate is still refused with `StaleHead`, and the journal is kept.
- Runtime `reserve_request` now refuses a secondary handle that does not own the worktree lock with `LeaseHeld` before inserting a row (ORNA-TURSO-005, ornadb-f69b). Test setup that reserved through a secondary handle now reserves through the host's owner-bound runtime, or releases the owner handle first.
- Secondary runtime handles (another live runtime holds the worktree owner lock) are refused with `LeaseHeld` by the writer-lease entry points `begin_observed_request` (with admission), `acquire_lease`, `current_lease`, `takeover_lease`, `recover_abandoned`, and by every owner-fenced write: the central owner check (`require_current_owner`, used by `require_owner` and `require_recovery_owner`), the legacy request transition (`transition_request`: start, complete, cancel) and `register_stream_observation` (ORNA-TURSO-005, ornadb-3c2s). A secondary can no longer read, take over or reclaim the writer lease, nor commit an owner-fenced write, so a second local writer must coordinate with the owner or fail. Tests that relied on a second handle performing this setup now use the owner handle, a handle outside the owner lock for compare-and-swap races, or a real secondary for `LeaseHeld` assertions. Publication-intent writers (`bind_compact_publication` and its receipt and completion transactions) are not yet lease-fenced and remain a tracked residual.
- One-shot `invoke` and `run` over committed tables (ornadb-03ol) now convert the merged activation snapshot (committed base plus the CWD tail) to evaluator records through the single stored-to-evaluator conversion `orna_project_v1::evaluator_rows_from_stored`, keyed by table name and ordered by storage order. Committed rows are no longer converted separately from the tail. A live-row test (`invoke_reads_every_scalar_field_type_of_a_committed_row`) reads Int, Str, Bool, Float and Decimal fields back from a committed row.

- Private staged identity carriers (the unreachable commit that `capture_staged_candidate` names for loading) now set a fixed internal author and committer identity through the environment, so they no longer depend on the ambient `user.name`/`user.email`. A machine or CI runner without a git identity can capture them (bead ornadb-36nj). Publication candidates ref-advance the user's branch and keep the configured identity, so a published branch tip records the configured author. Reviewed user commits are unchanged.
- The `replacement_runtime_fences` embedded test (ORNA-TURSO-005/006) now expects `LeaseHeld` for the Secondary handle that does not own the worktree lock, and proves TURSO-006 fencing on the replacement owner, which refuses the stale owner's lease with `OwnerLost` (ornadb-olxr).
- The `mv_restores_the_source_when_staging_the_inventory_fails` test no longer compares the source's change time or inode number; restore is observed by the refused output, the unchanged index bytes, the publication journal, and the staged identities. Inode numbers are reused on tmpfs, and change time is not specific to restore (ornadb-srco).
- The `writes_after_sync_start_a_new_candidate_until_the_next_sync` VFS test (ORNA-VFS-008) no longer asserts that the unsynced candidate is absent before the next sync. A close-time flush (EDIT-1 step 5) accepts the dirty candidate whenever the kernel sends FLUSH, including the close that a child process spawned by another test performs at exec, so before the next sync the accepted view is one of the two complete candidates; the test now checks exactly that (ornadb-rw8g). Test-only; no product behaviour change.
- A temporary file created in a row directory captures that row's current durable baseline (41-vfs.md Temporary-save replacement, ORNA-VFS-011), not the baseline the mount recorded when it opened the row. Creation reads that live row with one keyed lookup; if the lookup fails, the create returns EIO. A temp created after another writer's owner-routed commit (`orna edit`) therefore renames over the row instead of being refused with EAGAIN (ornadb-5il1.466). A temp created before that commit keeps the older baseline and is still refused. Known lost-update case: an editor that read the row before the owner's commit and creates its temp after that commit renames over the owner's change and replaces it, because the VFS cannot tell what the editor read (41-vfs.md:74-77). `orna edit PATH` is the guarded path for that change.
### Breaking / API
- Persisted-format change: table identities in the project module graph are module-qualified (`a.Item`; bare names only for root-module tables). Legacy project module rows written with bare module identities are refused with `E2200` and are not migrated. Re-create those rows under the qualified identity (ornadb-5il1.199).
- Known gap: qualified table identities are stored and read by their qualified name, but binding a qualified table identity to its native oid is pending ornadb-5il1.262 and ornadb-5il1.121. Legacy bare-name module rows are refused with `E2200` (above), not migrated.
- Module names are resolved through `use` only (ORNA-IMPORT-001, ORNA-REPL-001). A module namespace that is reachable only through another module's `use` is not in scope; write `use telemetry.rig;` before `rig.Reading`. Unresolved references that name a loaded module suggest the missing `use` (ornadb-5il1.199).
- `ActivationEffectReport` is built only through `ActivationEffectReport::declare(DeclaredEffectSet)`, which yields a `ControlledEffectToken` for `DeclaredEffectSet::None`. `ActivationWork::with_effects` takes this report.
- `RuntimeState::record_controlled_transaction` now requires `&ControlledEffectToken` as its final argument.
- `SysHostBindingRegistry` reports `ExternalPossible` from `may_reach_external_state()`, so bound-registry evaluations no longer declare `None`; `declared_effects` takes that boolean. The REPL declaration is now truthful. REPL table inputs reserve and start their request row in the runtime ledger before evaluation, so an external effect dispatched during evaluation is covered by a durable Running row. An input that stages and then fails during evaluation with no external binding run has its row cancelled. A failure after evaluation, including stage failures, is reported as UNCERTAIN and leaves the row Running, because `ActivationEffectReport` is always `ExternalPossible` (`ENVIRONMENT_READS_ARE_EXTERNAL`); the cancel branch for those is unreachable until that constant changes. A failed commit also leaves the row Running until fenced recovery settles it. A parse-, type-, or pure-failed input stages before reserve, so it records no request row and no terminal diagnostic (ornadb-od61). A panic between reservation and settlement leaves the row Running, which is the same state as a crash.
- `ControlledEffectToken` is a proof a declaration was made, not a verification; trust rests on callers declaring truthfully.
- `commit_table_request_activation` gains a final `effects: ActivationEffectReport` argument; callers must pass a report built with `ActivationEffectReport::declare`.
- `StagedTableActivation::from_source` gains a final `effects: ActivationEffectReport` argument; callers must pass a declared report.
- `ActivationWork::new` now defaults to `ExternalPossible`; callers that relied on the implicit controlled default must declare effects with `ActivationWork::with_effects`.

### Added
- Partial (ornadb-5il1.384, slices S1-S3 and S6; ORNA-ASSERT-026, ORNA-ASSERT-042, ORNA-ASSERT-048, ORNA-ASSERT-049, ORNA-ASSERT-050, ORNA-ASSERT-051): REPL writes and `orna commit` evaluate table assertions against the candidate's own rows before the commit becomes visible. `orna commit` does this through the assertion gate (`assertion_gate.rs` in `orna-cli-v1`): a false assertion refuses the commit with `ORNA-A091-009`, and a refused `orna commit` leaves HEAD unchanged. REPL writes use the runtime validator (the REPL gate), not `assertion_gate.rs`: a false table assertion refuses the write with `ORNA-A091-009`, and other validator failures keep their diagnostic in the detail. `orna commit` evaluates a module assertion only when the commit changes a table in its dependency set (ORNA-ASSERT-026), comparing the candidate's evaluator rows with HEAD's. A false module assertion refuses the commit with `ORNA-A091-009` and reports its source span rather than a table assertion (ORNA-ASSERT-051); the owner module name is not pinned by a test. Interim limits of the commit path: the gate runs only in `commit_reviewed_candidate_with_stage_hook` and `commit_reviewed_root_candidate`; the CLI publish gate is described in the S7 entry above; the library publish entry points are ungated by decision; no ASSERT-053 witness is produced; only the first refusal is reported, with no multi-assertion ordering; a candidate that changes only schema or only module-assertion source, with identical rows, does not trigger a module assertion; `head_table_rows` reads every admitted table at HEAD on each commit. The seam reads only scalar primary keys (bead ornadb-gq4g): a check that reads a table with any other key refuses the commit whole with `E2300`, and no subset of the assertions is validated (ORNA-ASSERT-049, ORNA-ASSERT-050). Checks over scalar-key tables still evaluate; `tests/commit_assertion_gate.rs` pins this (`scalar_table_assertion_beside_enum_keyed_table_is_still_evaluated`, `enum_keyed_table_assertion_refuses_the_commit_with_its_cause`, `assert_free_project_with_non_scalar_key_still_commits`, `false_module_assertion_refuses_the_commit_with_its_span`, `module_assertion_over_unchanged_tables_is_skipped_by_the_commit`). The REPL gate has the seam limit in full: in a project that declares a table assertion and also a non-scalar-key table, REPL writes are refused project-wide, including a valid write to a scalar-key table (`tests/repl_table_assert.rs`: `enum_keyed_assertion_write_is_refused_and_leaves_the_store_unchanged`, `unrelated_enum_keyed_table_refuses_a_valid_write_in_an_asserting_project`). On the run path a false committed-row table assertion reports `E2200`; the internal code `ORNA-EVAL-TABLE-ASSERT` appears only under `--debug` (bead ornadb-a3jt).
- Follow-up (ornadb-t5tz): the commit gate's interim HEAD read (the `head_table_rows` limit disclosed in the S6 entry above) stays until a cheaper change signal is proven equal to the changed-table predicate. Until then each gated `orna commit` or `orna publish` in a project that declares assertions reads every admitted table at HEAD, bounded by `committed_read_limits`; past a limit, or when HEAD cannot be read, every candidate table counts as changed, so no module assertion is skipped.
- Format-2 Blob read API (not yet reachable from a production row read path): `Repository::read_format2_orr_blob` and `Repository::read_format2_rov_blob` pin one selector, admit only a repository-format-2 commit, decode an ORR-2 inline value or record or a ROV-2 CBOR reference (authority `ORNA-ROW-013`, `ORNA-UPGRADE-001`/`002`), and return bytes only after the OGB-1 content root under `.orna/blobs/<2>/<62>/` verifies against the recorded digest and length. Content per call is capped at 8 MiB (`ORNA-BLOB-011`); the effective limit is the smaller of the caller's limit and that cap. Format-1 and format-3 pins are refused. An absent root is reported as `RootMissing`, not as corruption; Git read failures are reported as `Git`. The ORR-2, ROV-2 and OGB-1 clause IDs in the code are superseded draft IDs, not final-spec clauses. Loose-row and compact-v2 physical row decoders are not part of this change. `ornadb-nne7` remains open.
- Row-map resume paging (ORNA-ROW-002): a resume cursor skips exactly the keys at or below it under the shared `Int`/`UInt` rank, is intersected with the range's lower bound, and is refused at or above the upper bound. Composite (tuple) predicate scans page every row in key order across page boundaries; a 300-row composite fixture is checked key by key by the native graph and CLI e2e tests.
- `orna serve` reads tables (ornadb-03ol, ORNA-SERVE-003 / ORNA-TXN-005): each `GET /` re-reads this clone's committed `HEAD` `main.orna`, binds its tables to their committed identities, and overlays the committed rows with the CWD generation captured for that one activation. A `HEAD` commit is visible to the next request, and a durable CWD row is served; the partial writes of an activation that is not committed are not. Errors are `serve.entry_unavailable` (no readable `HEAD` entry, including an unborn `HEAD`), `serve.table_identity_unavailable`, `serve.mixed_table_identity` (bound and unbound tables together), `serve.table_read_unsupported`, `serve.table_rows_unavailable`, `serve.table_read_limit` (per-table or total committed read exceeds the source/collection caps), `serve.evaluation_failed`, and `serve.write_refused` (a `serve` that stages a table write is refused and nothing is committed). Trust: `serve` table data is reachable by any client of the listener. The default bind is loopback (ORNA-SERVE-004/005); a non-loopback bind exposes the tables. Known cost: each request opens a runtime handle and reloads the committed rows (`ornadb-n4m5`).
- `orna serve` committed table reads share one bound with `run` and the REPL: a table may hold at most the evaluator collection limit of rows, and the rows' key and stored bytes across the read may not exceed the project source byte limit. A read past either bound refuses with `serve.table_read_limit` (`run`/REPL: `E2200`). A committed row holding a Blob is refused with `serve.table_read_unsupported` rather than serving its placeholder.
- REPL request-row ordering proof (ornadb-5xrk): `orna-application-v1` gains a `test-support` feature that exposes a dispatch observer on the REPL transaction handler. A regression test records the running request-row count at the first `sys.blob.capture_file` dispatch and asserts the row is already reserved (REQUEST-1, Orna-1.1.0 30-protocol.md:104). The observer is absent unless the feature is enabled.

### Changed
- Test-expectation migration (ornadb-0im9): bare root intrinsic names (`sort_by`, `every`, `sum`, `min`, `max`, ...) resolve without a standard profile (09-standard-library L7), so the five finite-list tests that expected them unresolved now assert `is_ok`; their qualified `std.collection` assertions remain unresolved without a profile. Added signature regression table `root_intrinsic_calls_are_checked_against_their_signatures` (arity, argument types, callback arity/return; bad calls report `ORNA-S021-TYPE`). Known-red entries for those tests are removed.

### Fixes
- Test expectation realigned with shipped behaviour (nightly triage, no product change): `trace_event_accessors_expose_the_closed_facts` now expects the redacted observation purpose `<redacted>`, as documented on `InspectTraceEvent::purpose` (secret-trace redaction, #2204).
- REPL results carrying a format-3 Blob (`ROV3`) now present its payload-free summary (`type`, `length`, `sha256`, `suffix`, `media_type`) at the REPL canonical boundary. Blob capture followed by an insert no longer fails with `ORNA-EVAL-VALUE` and then `ORNA-REPL-EFFECT-UNCERTAIN` (tracked by ornadb-rg5g). The descriptor OID and payload bytes do not cross the boundary. The summary is a metadata Map, not a Blob: `$_` after such a line holds that Map, so `sys.blob.media_type($_)` or `sys.blob.annotate($_, ...)` on the next line is a type error.
- An uncertain REPL outcome, where external effects may already have happened, keeps `ORNA-REPL-EFFECT-UNCERTAIN` as its code and names the evaluator cause in the located detail instead of dropping it.
- REPL module-level assertions (ORNA-ASSERT-026) are gated by their table dependencies: a REPL write whose changed tables do not intersect an assertion's dependency set no longer evaluates that assertion, so an unrelated write is no longer refused by a false assertion over other tables (ornadb-exwj). Tests cover multi-statement rollback (ORNA-ASSERT-029), several false assertions in one boundary, dependency-table writes still refused, and unchanged store, HEAD and status after rejection. The REPL's non-validated commit failure now names its cause in the `ORNA-REPL-EFFECT-UNCERTAIN` detail.
- Known limitation: carrying the live context result through `$_` (preserving `ORNA-BLOB-008` held-context behaviour) is not in this change; it is tracked as P2 follow-up `ornadb-ssrk`. Until it lands, `$_` after a Blob result is the payload-free Map described above. `SOV3` `ContextualBlob` values still fail canonical validation at the REPL boundary.
- Known limitation: `ROV3_OVERFLOW_TAG` (60113) blob references are refused fail-closed by the evaluator (test `overflow_reference_is_refused_by_the_evaluator`). No end-to-end test proves that a REPL capture produces this shape.
- Row admission accepts the specification tuple encoding (`Tag(60015)` over an array)
  for tuple-typed fields, where it previously failed with `ORNA-CONFORMANCE-ROW-TYPE`.
  Stale conformance and application test expectations were migrated to
  the current specification: semantic-adapter and valid-fixture diagnostics, money and
  multiple-root codes (`E5003`/`E9102`), the computed-field committed-row schema digest
  (`orna.schema.v3` domain, store-format-3 section 5), primary-key patch `E3002`, txn key and
  decimal pins, the partial-mutation rollback entry, and `ORNA-ASSERT-021`/`030` catalogue
  resolution. The `sys.SnapshotRef` semantic-adapter assertions are restored to the specification
  `Named` result. That pair (`sys_snapshot_string_as_pinned_read`,
  `sys_database_cwd_as_snapshot_ref_read`) stays red until `ornadb-hzt6` lands and is registered in
  the conformance known-red gate of PR 9369 (merge order: PR 9370 first). The valid-fixture gaps
  are tracked product gaps in `KNOWN_PRODUCT_GAPS` (`valid_fixtures.rs`), not known-red: each entry
  records today's product diagnostics and its bead, and the test fails when product output changes
  in either direction. Tracked product gaps: `jnf1` (failure-natural-key, stream-admin-repl,
  finite-stream), `7owc` (table-key-default field access), `qx8l` (owner type-mismatch emits `E2001`
  before `A091-004`), `yu0h` (unparenthesised pipeline lambda). `6aqk` (implicit
  conversion chain reports `ORNA-S012-UNRESOLVED` instead of `ORNA091-E-CONVERSION-CHAIN`) is an
  open product gap and is excluded from the closed-pass list.
- REPL table inserts now run through the runtime request ledger: the request row is reserved and started before evaluation with a request-scoped fingerprint, and the commit records a terminal outcome. A failure after an external sys binding was dispatched, including an evaluation failure that follows a write, finishes the REPL query as failed with `ORNA-REPL-EFFECT-UNCERTAIN`, because external effects may already have happened; the request row stays Running and is settled only by fenced takeover recovery, and the input is never replayed. An evaluation failure before any external binding keeps its evaluator code and cancels the reserved row. A failure before the request starts leaves its row Reserved, and recovery does not settle that row (ornadb-buat). A staged input with no table mutations completes its row with a read outcome.
- The controlled-transaction marker is now written only for activations whose
  effects are proven controlled. `ActivationEffectReport::ControlledOnly` is built
  only by `ActivationEffectReport::declare(DeclaredEffectSet::None)`, which yields the
  `ControlledEffectToken`; `ExternalPossible` (the `ActivationWork::new` default)
  records external-effect uncertainty instead. A
  faulted `ExternalPossible` activation is not reported as rollback-proven
  (`ORNA-PROTO-004`, `ACCEPTANCE.md:33` Gate D; `W118` M1).
- Conformance v1 test gate: `scripts/check-conformance-known-red.sh` (workflow `conformance-v1-tests.yml`) runs every `orna-conformance-v1` test target. Five product-gap tests are listed in `scripts/conformance-known-red.txt` against beads `ornadb-ptqm`, `ornadb-8rfu` and `ornadb-hzt6`; each must still fail with its expected message. The row admission tuple arm now accepts canonical tagged tuple fields (ORNA-CONFORMANCE-ROW-TYPE no longer fires for `Raw::Tag(60015, Array)`). The stale cross-table and durable-consumer conformance expectations were migrated to the spec clauses they cite (ornadb-3ybj part B).
- Conformance gate coverage: `scripts/check-conformance-known-red.sh` now also runs the `orna-conformance` bin's unit tests (`--bin orna-conformance`, the `cfg(test)` tests in `src/main.rs`), and fails closed when one test name appears in more than one target. Changes to its cap and allowlist need owner review; the repository has no CODEOWNERS file.
- A project that imports `std` and whose source text contains `sys.Storage` or
  `sys.MaintenanceJob` now runs `use std.<module>.{name}` calls through the production
  authority route (`orna invoke`). The gate is a substring match on the admitted source
  text (`requires_publication_metadata`); a project that imports std but does not contain
  either name keeps its previous path, unchanged. The authority admits the captured
  standard dependency's function bodies, so a standard function is callable from the
  importing module instead of failing with `E2200` (bead `ornadb-5il1.430`).
  Glob imports (`use std.<module>.*`) bind every public function of the module
  (ORNA-IMPORT-003). `use std.<module> as _` binds only the names the module records in its
  prelude. A module alias (`use std.<module> as <name>`, or a bare `use std.<module>;`
  bound to a name) is refused at admission with `UnsupportedImport` ("qualified std module
  alias not supported on this route"). A standard body calls its own module helpers, never a
  user function of the same name.

- Git probes that decide publication admission, blob verification, and
  migration index pins now stop repository discovery at the probed directory's
  parent only when that directory carries its own `.git` entry. A nested
  repository with a corrupt `HEAD` fails closed instead of reading an enclosing
  repository. Migration Git commands always stop discovery at their directory's
  parent, so a plain subdirectory cannot reach an enclosing repository's refs or
  index. A plain subdirectory of another repository is no longer adopted as a
  dependency checkout (`DependencyUnavailable`); `is_repository` is now a plain
  `.git` existence check and makes no Git call. Exporting from a subdirectory of
  a worktree and porcelain status again discover the enclosing repository
  (ornadb-6vxj).
- `orna query` with a predicate now reads the whole `--from`/`--until` range in
  ordered pages instead of evaluating only the first 256 rows (ORNA-ROW-009);
  `--from`/`--until` accept `int:N` (on an `Int` key), `uint:N` (on a `UInt` key)
  and `text:KEY` (on a `Str` key), and `--key` tries integer spellings as integer
  keys after text and bytes. `orna history KEY` shares that spelling resolution,
  so an integer spelling also names the integer row there.
  A predicate scan pages the whole `--from`/`--until` range by an exclusive resume
  cursor (the last key seen), so it needs no successor of a key and works for every
  primary-key type, composite (multi-component) keys included; a page that ends at
  the greatest key of a type, including `u64::MAX`, continues rather than being
  refused. A bound whose spelling does not match a single `Int`, `UInt` or `Str`
  primary key is refused naming the valid spelling (`bytes:HEX` on a `Str` key is
  refused; use `text:KEY`) instead of answering empty. Any `--from`/`--until` bound
  on a composite key is refused, naming bead ornadb-5il1.461, because a
  single-component bound has no spelling over the tuple. `Bool`, `Decimal`, `Uuid`,
  `Date`, `Instant`, enum and table-ref keys are not addressable by `--from`,
  `--until` or `--key`; those spellings are refused naming bead ornadb-5il1.461 and
  this change does not implement them.
  The `--format json` output of `orna query` with a predicate reports `rows` as
  the number of committed rows read in the bounded range, and `blobs` as the
  number of listings after the predicate; `plan.candidates` counts Blob-field
  coordinates before the predicate and `plan.matched` counts those that passed.
  Refusal and `orna history` error titles now carry their detail text
  (`key_error` and `history_error`), so the human renderer shows the valid
  spelling or bead named in the title; the title wording of those refusals changed.
- `orna-repository-v1::format2_rov::decode_rov2_blob` decodes one superseded-draft format-2 ROV-2 storage reference (CBOR tag 60100 around `[sha256, length]`; superseded draft ID ORNA-OGB-008) with shortest-form, width and 2^63-1 bounds. Its inline golden vectors were hand-copied from the draft archive and compared manually; the archive comparison test runs only when `ORNA_DRAFT_ZIP` is set (it prints SKIPPED otherwise, and fails when `ORNA_REQUIRE_DRAFT_ZIP=1`). Read-only; consumed only by `read_format2_rov_blob`, not by a production row read path. Final-spec authority for reading retained bytes is ORNA-ROW-013 and ORNA-UPGRADE-001/002.
- `orna-repository-v1::format2_orr::decode_orr2_blob_field` decodes one superseded-draft format-2 ORR-2 Blob field value (superseded draft ID ORNA-OGB-007): an exact `{ blob: "sha256:<64 lowercase hex>", length: <decimal> }` record (no empty or trailing fields) or a legacy inline padded-Base64 string. Read-only; consumed only by `read_format2_orr_blob`, not by a production row read path; does not resolve `.orna/blobs` or produce an OVB-1 Blob.
- `orna-repository-v1::format2_ogb::verify_ogb_root` and `read_ogb_root` read-only verify a superseded-draft OGB-1 descriptor/index/chunk tree (superseded draft IDs ORNA-OGB-004/005): exact tree shape, modes, names, canonical CBOR, height/fanout bounds, chunk digests and content commitment, pinned to the draft `abc` vector. The caller supplies a verified object store; Git object IDs are not re-hashed. Missing objects are reported distinctly from corruption. `read_ogb_root` refuses content above 8 MiB per call (`ORNA-BLOB-011`). Consumed only by the Blob-field read API; no writer is activated.
- The language server's semantic rename keeps a table or field's object identity.
  Owner-visible behaviour: on save, the editor writes to the git index. The index
  entry for `main.orna` is replaced with the whole saved file (not only the renamed
  hunk), and the identity inventory and `.orna/store` are staged too, but only when
  the `main.orna` index entry still equals HEAD's or the text the edit started from.
  Otherwise nothing is staged and the warning asks for manual staging, so partial
  `git add -p` work is never overwritten. Nothing is committed; `orna commit` then
  keeps the identity, like `orna mv`. A field rename is not staged: it is refused with
  the warning `ORNA-LSP-RENAME-001` ("schema evolution is not admitted") until schema
  evolution is implemented (ORNA-EVOL-002). An identity that cannot be carried leaves
  the rename applied and shows a warning with code `ORNA-LSP-RENAME-001` and the remedy
  `orna mv` (`ornadb-5il1.414`). A rename applied to the buffer but not saved before
  the document closes or reopens is reported as "the rename was not saved". Concurrent
  inventory changes are guarded by the staged-store `StaleIndex` check. An outer
  coordination lock was not added because the install path takes the same lock
  non-blockingly: a re-entrant attempt fails with `RepositoryBusy`, so the install
  would fail rather than deadlock.
- Publication recovery re-verifies `.orna/store` payloads at the `IndexReconciled` stage before worktree materialisation, so a managed store edit made inside the `index` kill window is retained (E2300) and HEAD is not rewound. `publication_crash_matrix` gains a later-tail leg at every kill boundary (`prepared` included) and a managed-store kill-window leg (ornadb-5il1.406, ornadb-5il1.407).
- `orna serve` HTML pages (`/tree/`, `/blob/`) now send a
  `Content-Security-Policy` (`default-src 'none'`, inline styles only,
  `frame-ancestors 'none'`), `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY` and `Referrer-Policy: no-referrer` (ORNA-APP-012,
  ORNA-APP-013). Oversize bodies on non-`/orna` routes answer 413 instead of
  400, and the Origin/Host refusal now runs before the size check (bead
  ornadb-5il1.387).
- `orna mount status --json` now reports `writable`, `read_only`, `unresolved_drafts`
  (each `{path, detail}`), and `draft_error`. The record of a `--cwd` mount now
  stores `read_only false`, matching the FUSE session that was already writable;
  the record and status previously reported it read-only. Records written by this
  build carry a `read_only` value that older binaries reject as invalid (RecordInvalid).
  The `--read-only` flag from profiles/vfs-1.md:20 is not implemented; every `--cwd`
  mount is writable.
- `orna publish` gains four crash barriers behind the `crash-barriers` test
  feature: `ref-cas-pre-journal`, `store-worktree`, `worktree` and
  `runtime-complete`. `publication_crash_matrix` SIGKILLs the publisher at each
  barrier and asserts, after `orna publish` recovery: HEAD equals the base at the
  pre-CAS barriers (`ref-cas-pre-journal`, `store-worktree`) and equals the
  journaled candidate at the post-CAS barriers (`worktree`, `runtime-complete`)
  while the journal is present; the recovered candidate is checked to be parented
  on the captured HEAD only at the pre-CAS barriers; exactly one publication commit
  exists; a second publish is a no-op; staged and unstaged user work, pins, the
  runtime tail, the writer lease and locks are preserved or released; and
  `git fsck` passes. Managed-worktree and index reconciliation checks run only for
  non-store paths, and format-3 candidates have none, so they are vacuous here.
  No production behaviour changes.
- A complete-copy export of a workspace with a legacy format-1/2 snapshot now
  admits that snapshot by its recorded format record, and an offline restore
  re-admits each member (Gate F, `ACCEPTANCE.md:63-68`,
  `24-git-history.md:19`). Format-3 members still require their schema and
  store roots. Earlier builds refused every non-format-3 snapshot as a complete
  copy. The migrated-member leg (`migrate copy` publication) remains open under
  ornadb-5il1.192, and a gitlink whose dependency repository is absent is still
  refused.
- Runtime admission refuses a `HEAD` whose `.orna` metadata mixes layouts (a
  legacy `.orna/format.orna` or 1.0 sidecar beside `.orna/store`) with
  ORNA-REPO-CONTEXT-004 `MixedFormats`, naming `.orna/store`. It used to report
  ORNA-REPO-CONTEXT-003 (unknown format) because the gate reclassified the
  mixed snapshot (ORNA-META-001, ORNA-META-002; Gate A).
- `mkdir` and `rmdir` on the live writable mount now return `EPERM`, the same
  errno as every other refused mutation there (profiles/vfs-1.md errno table;
  41-vfs.md:103-105). They previously fell through to the fuser default, ENOSYS,
  which is not in the errno table. The `EXDEV` once considered for cross-directory
  rename is not in the reference spec, so it is not used; that rename keeps `EPERM`.
- One-shot `orna invoke` reads committed table rows at `HEAD`, binding each table to its committed identity (`10-execution.md`). A table-reading entry requires a committed project; without one it fails with `table reads need a committed project`. A mutating one-shot `invoke` fails closed with `table mutations`. `sys.rt.info` and `sys.Storage` are available inside table-reading entries. A one-shot read completes while another process holds the runtime owner lock, as a secondary participant. REPL expressions over committed table rows (for example `main.music | count()`) read the committed rows at `HEAD`.
- Known limit: a one-shot `orna invoke` in a multi-module project that declares a table fails closed with `E2200` (`table reads in multi-module projects are not supported by one-shot invocation; use the REPL or orna run`). This wording is a spec-silent choice of this build. Any table-declaring module is refused, reached or not; the committed-table route admits a single module only. Extending one-shot reads to multi-module projects is tracked as follow-up work. Before this entry, the same projects were refused by the bounded evaluator with the generic `one-shot invocation requires an executable function-only project`; the outcome is unchanged, only the diagnostic is named.
- Known limit: a committed table read pages its relation scan with an ordinal cursor recomputed per page, so each page costs O(N log N) and a full scan with a one-row page size is O(N^2 log N). Carrying a keyed cursor is tracked as follow-up work.
- Known limit: a successful one-shot `orna invoke` prints only `invocation completed` and does not print the returned value. No one-shot invoke clause was located in the reference source (`reference/Orna-1.1.0/source/*.md`), so this output route is spec-silent in this build.
- A FUSE editor handle whose `close(2)` finds unsynced writes attempts the same
  acceptance as `fsync`, once (ORNA-VFS-008, `41-vfs.md:34-58`). A close whose
  baseline a peer has since accepted is refused: the peer's row stays and the
  candidate is kept in recovery storage (ORNA-VFS-009). Unmount refuses, rather
  than drops, a valid dirty draft that retention cannot keep, and a released
  handle does not re-admit a refusal that a later accepted save resolved.
  Earlier builds did not attempt acceptance at close and did not keep unsynced
  drafts on release.
- A FUSE write-intent open of a Blob projection (any write access mode, or
  `O_TRUNC`) now fails at open with `EPERM` and leaves the blob unchanged. It
  used to succeed or be ignored, and the later write failed with `EBADF`
  (profiles/vfs-1.md:104-106). Tools that write a Blob in place, such as `cp`
  onto `content.mp3` or an editor with `backupcopy=yes`, now see the error at
  open.
- The errno rule for the live writable mount is now uniform: every refused
  mutation there returns `EPERM` (41-vfs.md:103-105; profiles/vfs-1.md:139).
  This covers Blob write-open, Blob unlink and same-row rename, create in a
  table directory or in a row with a non-blob name, unlink of a row's
  `data.orna`, rename with flags, rename to a name other than the row's
  `data.orna` or Blob field, rename of a non-temporary node, and rename across
  rows. A cross-row rename is `EPERM`, not `EXDEV`: `EXDEV` would make `mv` and
  `cp` fall back to copy-then-unlink, which writes the bytes into the other row
  before the refused source unlink leaves the original behind. `EROFS` is
  reserved for the kernel read-only historical mount (`mount --at`), which never
  reaches these handlers. Rename with flags, rename of a row's `data.orna`, and
  path-truncate of a Blob are covered by `live_mount_structural_refusals_are_eperm`.
  Rename with flags uses `EPERM` where rename(2) would use `EINVAL`; `EPERM`
  follows the unsupported-mutation rule (41-vfs.md:103-105). Zero-length and
  sparse-hole Blob coverage remains open.
- A malformed format-3 native graph now reports a typed `GraphMalformed` cause
  under `ORNA-REPO-CONTEXT-017` when loaded through `load_row_map` for shape
  faults such as overlapping child ranges, broken counts, over-wide fanout, and
  a wrong schema domain. Operational graph failures (quota, cancellation,
  durability) are not reported as malformation.
- Runtime admission reads the format recorded at the committed `HEAD` before
  any worktree metadata or runtime state. A `HEAD` that records a format this
  build does not know, whether `database.orna` names another `repository_format`
  integer with any other fields or `format.orna` names one, is refused with
  `ORNA-REPO-CONTEXT-003` (ORNA-UPGRADE-001, ORNA-META-002). The refusal is a
  new `RuntimeError::UnknownHeadFormat`, and its persisted admin failure class
  is `unknown_head_format`. Other `HEAD` metadata failures keep the existing
  path.
- A format-3 snapshot that tracks a loose row file (an unreached
  `<table>/<key>.orna`) no longer loads with the row silently skipped:
  loading the committed snapshot or a commit candidate fails with
  `ORNA-REPO-CONTEXT-004` naming the file (ORNA-REPO-004, ORNA-UPGRADE-003).
  A module a `use` reaches under a table's directory is still a module.
  `orna commit` now also refuses a row file that `HEAD` already tracks, not
  only one the commit adds or changes. The CLI test fixtures no longer commit
  loose rows into format-3 repositories; row sources go straight into the
  store.
- Format-3 schema nodes record the Orna 1.1.0 schema identity, SHA-256 of
  `orna.schema.v3`, NUL and the closed storage descriptor, and every row map
  binds it. Earlier development builds recorded the descriptor's plain
  SHA-256, so the row maps of format-3 stores they wrote no longer open;
  re-create such a store with `orna init`, `orna commit` and `orna publish`.
  The repository API also gives each admitted row its `orna.row.v3`
  identity, which binds content and Blob annotations but no descriptor, page
  or overflow object ID.
- `orna publish` rewrites only the row pages a change touches. Maintenance
  reads the path to each changed key and the pages beside it, rewrites those
  pages and the branches above them, and keeps every other page by object ID
  without reading it; the result is the same root a complete rebuild gives.
  Earlier builds read and decoded every row of the table on each publish. A
  row written back with the value it already holds is no change, so such a
  publish reports nothing to publish.
- `orna publish` stores rows larger than a row page holds inline. A value
  whose stored encoding is over 8 KiB, or that holds more than 32 Blob
  references of its own, is written to the shared overflow graph (a kind-3
  node over an OGB-2 byte index, with a kind-7 index of the objects it
  references) and its row names it with tag 60113. Large fields are lowered
  first, so a row with one long text field keeps its other fields inline.
  The committed rekey log lowers its rows the same way, so a rekey between
  long keys stores each long address on its own instead of the whole row.
  `orna query`, `orna history`, mounts and the REPL read such rows back as
  ordinary values; earlier builds refused to publish them.
- Format-3 row maps and the relation map cut pages by the exact ORP-1 rule:
  the encoded node bytes, including the domain and entry-count heads, and the
  page's distinct native refs. The same rows now give the same root as the
  Orna 1.1.0 reference model. Earlier builds estimated sizes and summed refs,
  so they cut pages early for large rows and for rows sharing Blob
  descriptors.
- A format-3 project whose `main.orna` is larger than 64 KiB can query,
  publish, show history and mount its rows. Opening a row map pins the
  committed `main.orna` by its blob OID instead of reading the source under a
  64 KiB cap that store format 3 sets only for `database.orna`.
- `orna mv OLD NEW` works in a database made by `orna init` once its first
  `orna commit` exists. Like `git mv`, it stages the rename and commits
  nothing. The table's declaration is renamed in the worktree module and in
  its staged copy, and the store's identity inventory is restaged in the same
  index update, so the table and its fields keep their IDs. Other unstaged
  edits to the module stay unstaged. `orna commit` right after `mv` keeps the
  table's relation and its published rows (ORNA-OBJECT-003, ORNA-CLI-001).
  `orna mv` no longer reads or writes `.orna/object-identities.cbor` and no
  longer moves loose-row directories, which format 3 does not have. Before the
  first commit it refuses with E2101 because no identity is staged yet. While an
  interrupted publication is pending it refuses with E2100 ("an interrupted
  publication has not been recovered") before reading the index; run
  `orna publish` first. A rerun after an interrupted `mv` resumes: when the
  worktree already declares the new name, only the index is restaged. If the
  index update is refused, the source edit is rolled back. The Rust API adds
  `Repository::stage_object_identities`,
  `RelationAdmissionError::SourceNotStaged` and
  `ObjectIdentityManifest::with_renames`. `ObjectIdentityManifest::stage_renames`
  takes the staged module sources as an extra `staged_sources` argument.
- Deleted the old-stack island crates `orna-client`, `orna-artifact`, `orna-protocol`, `orna-standard` and `orna-runtime-tty` (deletion programme step 7). None had a production caller: their only remaining dependents were the `orna-application-v1` dev-dependency (now removed) and each other. The `orna-standard` snapshot oracle `v1_profile.rs` is ported to `crates/orna-evaluator-v1/tests/standard_profile_v1.rs` (imports rewired to `orna-standard-sources`, fixtures moved alongside); `orna-standard-sources` remains the canonical source. The `just` recipes for the removed demos, Qt runtime and Studio shell (`runtime-tty-demo`, `client-*-demo`, `demo-suite`, `runtime-qt-*`, `studio-qt-*`, `runtime-suite`, `runtime-display-suite`, `runtime-abi-header-check`, `runtime-abi-parity`) were removed with them.
- The `orna` CLI no longer links `orna-client` (and its HTTP, WebSocket and Qt-ABI dependencies) for the `:watch` types. `live_presentation` moved unchanged from `orna-client` into `orna-live-v1`, which the CLI already depends on (deletion programme step 4). The public API of `orna-client` no longer exports `live_presentation`; import it from `orna-live-v1` instead.
- A committed source tree whose walk would visit more than 2^20 entries (every interior tree and every listed entry, counting each occurrence of a shared subtree), list more than 2^27 path bytes, or nest deeper than 256 levels is refused before git expands it. A project loaded from such a snapshot reports `ProjectLoadError::SourceTreeLimit` (CLI E2100, titled as a walk-limit breach) instead of spending unbounded time and memory on the expansion (W46 F1).
- Managed paths refuse a `.git` component at any depth and in any letter case,
  including the Windows and macOS spellings Git refuses (trailing dot or space,
  `git~1`, a `:` stream suffix and HFS ignorable characters), on write and when
  a committed tree is loaded or listed. Before, only an exact `.git` first
  component was refused. Path length is not capped, because Git stores names of
  any length. Untracked user paths such as `Sub/.GIT/notes` still appear in
  status and checkout plans, and a forced checkout leaves them in place.
  `orna export --check` and `--restore` refuse an archive whose dependency path
  names Git metadata before anything is restored. Managed paths also refuse a
  backslash, a NUL byte, an empty component (`a//b`, `a/`), and `..`, absolute
  and other non-normal components, on write, in publication and checkout
  journals, and in compact manifests. The system worktree listing skips a
  Git-metadata-named entry (`.git:x`, `a\.git`, `.git.`) instead of reporting it
  as a user path.
- Project path validation, worktree discovery, the VFS reserved-name check and the legacy migration clear, copy and hash
  paths treat every Git-metadata spelling (`.GIT`, `git~1`, `.git. `) as Git metadata through
  `orna_foundation_v1::is_git_metadata_component`, not only the exact `.git`. A directory with such a name no longer fails
  project loading as a non-portable path, no longer produces worktree rows, is reserved in the VFS, and is no longer
  copied, cleared or counted by a legacy migration. The exact nested `.git` remains hashed as repository state.
- `scripts/orna_extract.py` is an independent extractor. It reads a format-3
  repository's Git objects at a commit through `git cat-file --batch`, without
  the engine's readers, and writes the modules, the schema, the rows and each
  Blob's bytes. It refuses any tracked `.orna/` entry other than `database.orna`
  and `store` (ORNA-META-001).
- Work ADRs that Orna-1.1.0 supersedes, or that no longer constrain code, are
  archived under `docs/decisions/archive/1.0/` with an evidence table in
  `STATUS.md`. Duplicate ADR numbers are renumbered: the offline LSP ADR is
  0119 (was 0059) and the CLIENT VM trust boundary ADR is 0120 (was 0101).
- The live WebSocket handshake no longer accepts URL-alphabet or mixed-alphabet `Sec-WebSocket-Key` values; only padded standard base64 is accepted (RFC 6455).
- `orna --help` names the command `orna`. It says that `unmount DIR` detaches
  the kernel mount, including an orphaned Orna mount, and releases its view
  record. `annotate` help no longer names a wire encoding (ORNA-UX-002). The
  `migrate copy` help says that `--no-persistent-consumers` confirms that
  consumer checkpoints are carried, not reset (ORNA-UPGRADE-008).
- The Linux package archive holds only `usr/bin/orna` and its distribution
  manifest. The retired embedded-engine manifest, its builder image and its
  predecessor-engine fields are gone, so a default `packaging/linux/package.sh
  build` no longer needs an input that nothing produces.
- `orna commit` stages the format-3 store: a database made by `orna init`
  gets its `.orna/store` on the first commit, which creates the branch on an
  unborn HEAD. Each newly declared table is admitted with its schema node, an
  empty row map and a relation entry. Stable object IDs live in the store as a
  sealed identity relation keyed by UUID rather than
  `.orna/object-identities.cbor`, which is read only from older commits. A
  commit that fails leaves its newly staged tables free to change or drop, and
  `orna commit` refuses to stage while an interrupted publication awaits
  recovery. `orna publish` refuses rows whose table has no relation in the
  committed store. Identity rows record each object's kind as the
  specification's `sys.ObjectKind` ordinal (table 2, field 3, unit 15, type 8,
  variant 10, type and variant fields 9), not a private integer. Stores written
  with the private integers are not readable, and the sidecar's version 1 codes
  are mapped only when reading an older commit.
- `orna commit` evolves a format-3 table whose declaration changed instead of
  refusing it. Fields are matched by their stable ObjectId, never by name, and
  every committed row is rewritten to the staged schema and admitted against it
  in the same commit: a renamed field keeps its values, a new optional field is
  absent, a new required field takes its constant default (frozen into the
  rows at that commit), a removed field is dropped, and `T` to `T?`, `T?` to
  `T`, `Int` to `Decimal` and exact `Int` to `Float` convert. A new required
  field without a constant default, a changed primary key, a lossy type change
  or a row that does not fit refuses the commit and stages nothing. A removed
  table's relation leaves the store. Schema changes wait until unpublished
  runtime rows are published. The commit reports each evolved and removed
  table (ORNA-EVOL-001, ORNA-EVOL-009, ORNA-FIELD-003, ORNA-FIELD-005). A
  table with a secondary index is refused (E2101, nothing staged): index-aware
  evolution is not implemented (ORNA-EVOL-002 gap, `ornadb-7f9c`).
- `orna mv OLD NEW` renames a table, table field, type, type field, enum
  variant, variant field or unit of a format-3 database by its qualified name
  (such as `Music.title`): it edits the declaration and stages the ObjectId
  move in the index's `.orna/store`, committing nothing, so the next
  `orna commit` keeps the identity and the field's values (ORNA-OBJECT-003).
  Editors can stage the same move through
  `ObjectIdentityManifest::stage_renames`.
- Automatic table IDs are never reused: each automatic-key table's highest
  reserved ID lives in the hidden ref `refs/orna/ids/<table ObjectId>` outside
  every branch, reserved before the rows that use it commit, so deleting rows,
  `git reset`, checkout and branch switches no longer hand out an ID again
  (ORNA-AUTOID-004). A database created before `refs/orna/ids/<table>` existed
  has no watermark and can reuse an ID deleted before its first ref was written
  (ORNA-AUTOID-002 gap, `ornadb-ehgy`). Every `orna commit` and every
  publication that changes rows records each automatic-key table's watermark
  in a sealed allocator summary relation of `.orna/store` for history;
  checking out an older commit never rewinds the allocator (ORNA-AUTOID-005,
  ORNA-META-004).
- Row values are stored in the encoding their declared type requires: REPL
  inserts of optional, record and `Float` fields, and row documents naming enum
  variants, nominal records, refined values, money, quantities or stored
  references, now publish. A row document that omits a field with a constant
  default stores the default. Refined nominal values are stored as tag 60020
  `[type, base value]` (ORNA-FORMAT-004, ORNA-FIELD-001).
- `orna commit` refuses a staged index whose `.orna/store` is partial after `git rm --cached .orna/store`, leaving skip-worktree payload entries behind, with `E2302` naming the unstaged committed relation and its `git restore --staged .orna/store` remedy, instead of a generic staging failure.
- A live eval or event whose CWD snapshot pin the runtime cannot resolve is answered with a correlated `wire.snapshot_expired` diagnostic before any application effect runs. The pin is never rebound to the current generation.
- A live `cancel` whose target names no admitted request now acknowledges with a correlated Unit result and keeps the connection open. Before, the host answered `Denied` and dropped the socket. The application is not invoked for an unknown target, in memory or durable.
- `orna serve` hosts the clone's durable runtime. A live eval that inserts
  into a project table commits the row together with its terminal outcome,
  and resending the same request ID returns the recorded outcome without
  running again; before, every writing eval was refused. Each eval is one
  REPL input: a declaration, binding or import answers with typed Unit and
  persists for the session, and an input that declares a function named
  `serve` or `main` is an ordinary declaration rather than a one-shot module
  run. An explicit resync is answered with the watch's latest full snapshot
  under its own request ID; before, a changed value produced a delta with no
  request ID.
- `orna invoke` attaches the runtime host context before it evaluates, so `sys.rt.info()` completes in a project and fails closed outside one. The record is computed once per attachment; an incompatible system version reports `sys.version.incompatible`, an unreadable standard snapshot pin reports `sys.snapshot.incomplete`, and the build identity reads git HEAD only from the crate's own repository, otherwise `+nogit`.
- Semantic analysis keeps described `sys` member types nominal: `checkpoint.position` is a `sys.CheckpointPosition` that matches annotations and `sys.admin` signatures, and `storage.publication_policy` stays the opaque `sys.Value`.
- A qualified `std.collection` `sum`, `min` or `max` call is checked against a monomorphic pinned export's element type and takes its declared result.
- A table key whose type annotation does not resolve reports only the unresolved name, without a second primary-key encoding error.
- Enum values can be written in source: `Level.low` references a payload-free
  variant and `SyncResult.ok { count: 3 }` constructs a payload variant, also
  through named imports and module bindings. `orna check` reports an unknown
  variant as unresolved and unknown, missing and mistyped payload fields as
  `E2003`, `E2002` and `E2001`. The project REPL and `orna run` evaluate the
  constructed values, so `case` matches them and tables store them.
- `std.io.fs` operations resolve every path from the root directory handle held
  since the host allowed it, one component at a time without following
  symbolic links, and act on the final entry through its open parent directory.
  Parent traversal, absolute paths, symbolic links and `.git` metadata are
  refused as `sys.host.filesystem.denied`, while a missing target reports
  `sys.host.filesystem.not_found`; only `symlink_metadata` still describes a
  final link, without following it. Writes without overwrite create
  exclusively, moves without overwrite never replace a destination, and
  listings omit `.git`. Platforms other than Linux refuse these operations as
  unavailable.
- `sys.explain` works in `orna run`, `orna serve` and the REPL. Each host pins
  it to its activation snapshot: the admitted project tables, plus
  `sys.Storage` and `sys.MaintenanceJob` where the host serves their rows
  (`orna run` and `orna serve`; the REPL serves neither yet). A query returns its
  structured `sys.Plan`. A `sys.Diagnostic` returns a `sys.Explanation`: the
  row's own message as the summary (the explicit marker only when the row is
  redacted), its causes, its help as suggestions and the objects it names.
  `sys.explain(FunctionRef)` is dispatched. The production explain catalogue
  does not resolve function references (`resolves_functions` defaults to
  false), so every function reference reports `sys.version.incompatible`.
  `sys.object.not_found` applies to an unknown source, or to an unknown function
  only when the catalogue resolves functions.
- Changed failure codes, all now taken from the portable failure table:
  - `sys.explain`: `sys.abi.unavailable` becomes `sys.version.incompatible`.
    `sys.abi.provider_failed` becomes `sys.object.not_found` for an unknown
    source, `sys.version.incompatible` for a stage or source the
    planner cannot represent, and `sys.object.wrong_kind` for a reference to
    another relation. `ORNA-EVAL-TYPE` no longer rejects function and
    diagnostic arguments.
  - `sys.blob.capture_file`: `ORNA-INGEST-001`, `-002` and `-003` become
    `sys.blob.source_denied`; `ORNA-INGEST-004` becomes `sys.blob.limit`; and
    `ORNA-INGEST-005` becomes `sys.blob.source_changed`. A negative or
    unrepresentable `max_bytes` is `sys.blob.bounds`, and a repository capture
    quota is `sys.blob.limit`. A host without a filesystem provider reports
    `sys.blob.source_denied`, and one without a repository Blob writer reports
    `sys.version.incompatible`, instead of `ORNA-EVAL-UNSUPPORTED`. The native
    host operation declares `sys.blob.source_denied`, `sys.blob.bounds`,
    `sys.blob.limit` and `sys.blob.source_changed` instead of
    `sys.blob.capture.*`; the binding raises only these declared codes and the
    global `sys.version.incompatible`, and the portable operation declares the
    same set plus that global code, which every callable operation carries.
    `orna annotate` source errors cite the portable code.
  - `sys.blob.annotate`: an OVB-1 value is
    `sys.codec.unsupported_value_profile`; arguments that are not annotated
    Blobs are `ORNA-EVAL-TYPE`; a host without the generated provider is
    `sys.version.incompatible`. No `sys.abi.*` code reaches programs.
  - `orna serve` stream administration: `sys.admin.unavailable` becomes
    `sys.admin.missing_object`, `sys.admin.busy`, the specific checkpoint or
    snapshot code, or `sys.storage.unavailable`. A dispatcher without resume
    reports `sys.admin.read_only`, unavailable publication metadata reports
    `sys.storage.unavailable` instead of `sys.storage.metadata_unavailable`, and
    `sys.admin.result` is gone.
- Every portable `sys` operation now declares its failure codes explicitly
  through `#[ornasys(failures = ...)]`, replacing the namespace-prefix guess.
  The build checks that every published code is declared by an operation, is
  global, or belongs to a non-callable surface (the `sys.repl` singleton or the
  VFS).
- A failed stream administration call (pause, resume or checkpoint reset)
  records a stable failure class in its invocation audit row, with the
  generation its writer fence observed. A retry under the same invocation ID
  returns the recorded error instead of a storage-unavailable error. A stale
  capture stores the capture that rejected the call and replays that same
  error, even after later commits advance the CWD. A rejected stream-reference
  resume now records its failure like a rejected pause.
- When the CWD generation cannot be read, the row records no generation (NULL)
  with a `failure:unobservable:<class>` outcome, and the first call still
  returns its original error. A retry replays `<class>` as the same error when
  the class is payload-free. The payload-carrying classes `stale_capture`,
  `fault_injected` and `legacy_head_read_only`, and a bare legacy
  `failure:unobservable` row, replay as `RecoveryInvalid`, because their
  payload was never stored. The observed generation is nullable in the audit
  schema, migrated in place for existing stores; unobservable rows are
  excluded from the checkpoint-reset generation ordering.
- A skip lease refused because the failure is no longer open is
  `RejectReason::RetryNotAllowed` at the stream layer. No production path maps
  it to a public `sys.failure.*` code: the `RuntimeError::StreamFailure*`
  variants keep their `public_code` strings for round-trip but nothing
  constructs them. The unreached `failure_transition_error` mapping is removed
  until `sys.admin.{retry,skip,resolve}_failure` exists (ornadb-5il1.296).
- Removed the unreachable duplicate explain implementations:
  `orna_sys_v1::ExplainQueryProvider`, `QueryPlanResolver` and their constants,
  `orna_sys_v1::explain_diagnostic` with `Explanation` and
  `DiagnosticExplanationError`, and the `orna_foundation_v1` explanation
  envelope (`SystemDiagnosticExplanationInput`,
  `SystemDiagnosticExplanationOutput`, `system_diagnostic_explanation_input`
  and `SystemDiagnosticAdapterError`). Also removed
  `SysHostBindingRegistry::with_explain_catalogue` and
  `CaptureFileError::code`.
- Removed the unreachable `orna-core` catalogue bridge from `orna-runtime-v1`:
  `CatalogueAdmission::from_artifact`,
  `RuntimeState::commit_validated_catalogue_table_request_activation_with_revision_pair`
  and `RuntimeState::catalogue_revision_pair_at`. Nothing called them, so every
  catalogue admission already recorded no revision pair. `orna-runtime-v1` no
  longer depends on `orna-core`. The `runtime_catalogue_revision_binding` table
  stays in the state schema, so existing `state.db` files open unchanged.
- Opening a repository format context reads only the root tree, the `.orna`
  tree and its metadata blobs, through one long-lived `git cat-file
  --batch-command` reader per repository handle. It no longer lists the whole
  commit, `.orna/store` included, with a Git process per entry, so point reads
  no longer slow down as the store grows, and a snapshot with more than 65,536
  tree entries opens.
- The project loader's 4,096-entry limit is now `ProjectLimits::max_source_modules`
  (formerly `max_repository_entries`) and counts source modules only. The
  loader never walks `.orna/store`, in the worktree or in a committed snapshot,
  so `orna check`, `orna run` and `orna repl` load databases with large stores.
- The evaluator's depth limit (`Limits::max_depth`, default 64) now counts
  active calls: function, closure and callback bodies. Expression nesting inside
  one body no longer consumes it, so recursion depth no longer shrinks as bodies
  nest deeper. The parser bounds expression nesting. Pattern and decoded-value
  nesting are checked from their own roots against the same limit. Exceeding
  any of these fails with `ORNA-EVAL-LIMIT` and never returns a partial result
  (ORNA-CLOSURE-004).
- An assignment inside a `case` arm now replaces the enclosing `let` slot, as
  assignments inside blocks and loops already did (ORNA-CFLOW-005). Names bound
  by the arm pattern stay local to the arm.
- Counting an in-memory list costs one evaluator step instead of one step per
  element. A plain `=` assignment no longer copies the value it replaces.
  Callback calls no longer copy the callee's syntax tree. Block, loop and `case`
  write-back moves values instead of copying them.
- Evaluator list values share their elements between copies. Appending to the
  newest list reuses spare capacity, so building a list one element at a time
  costs amortised constant work per element instead of copying the whole list
  each time. `union` and `__list_concat` charge steps only for the elements
  they actually write.
- Ordered comparisons (`<`, `<=`, `>`, `>=`) between two Decimals evaluate
  exactly across scales (ORNA-COMPARE-002). They previously failed with
  `ORNA-EVAL-UNSUPPORTED`.
- Decimal `%` evaluates as the exact truncated remainder, matching Int `%`: the
  result keeps the dividend's sign. It previously failed with
  `ORNA-EVAL-UNSUPPORTED`.
- Exponent-only Decimal numerals such as `1e3`, `1E-2` and `2e-30` evaluate as
  exact Decimals (ORNA-LIT-002). They previously failed with `ORNA-EVAL-VALUE`.
- Relation scans charge each streamed row to the step budget. Only values that a
  stage or terminal keeps (sort buffers, distinct sets, collected results) count
  against `max_collection_items`. `count`, `exists`, `every`, filtered or
  dropped counts, `sum`, `min` and `max` now work on relations larger than the
  item budget. Sorting more rows than the budget still fails with
  `ORNA-EVAL-LIMIT`.
- `sort_by`, `rank` and relation sorts use a stable merge sort, with
  O(n log n) comparisons that each cost one step, instead of an insertion sort.
  The default evaluator step budget is now 200 steps per item plus 10,000
  (419,600), down from 2,516,752. The pinned std helpers stay within it at the
  2,048-item limit. A runaway loop such as `while true {}` now fails with
  `ORNA-EVAL-LIMIT` six times sooner. Std bodies that are inherently quadratic,
  such as the Orna `std.collection.distinct`, reach the limit at a few hundred
  elements.
- `std.collection.map` evaluates its pinned Orna while-loop body instead of the Rust binding when the pinned `map_from` is a loop. Historical pins whose `map_from` is recursive keep the native binding. Other list-input collection operations already take their Orna bodies; relation inputs stay on the Rust relation path.
- `orna publish` says what happened when every pending runtime mutation
  already matched the committed snapshot: it reports how many mutations made
  no visible change and were cleared, per table, instead of calling them
  already committed.
- The CLI test suite runs the Orna 1.1.0 release scenario oracles from the
  release's own `tests/scenarios.json`: every FINAL and V110 oracle and the
  mandatory behavioural scenarios start from `orna init` and use only product
  commands. Each is an executor that must pass or a known gap that names its
  blocking work item and a probe that still reproduces the blocker. Gaps on
  export of a tampered store, the REPL effect a scenario needs, and a 3 MiB
  REPL capture are blocked by ornadb-z7v3, ornadb-k2wq and ornadb-9hjd.
- Format-3 transaction validation maps positional row values by stable field-ID order before evaluating row assertions.
- `orna serve` no longer installs a host-generated devtools table browser; local query, mount, and content workflows remain independent.
- Remounted writable workspace views capture one runtime activation and project
  current row values only when table identity, exact key, and ordered stored
  field layout match. Rekeys without row-lineage metadata stay pinned; Blob
  bindings remain pinned.
- Blob annotation binding canonicalizes caller-supplied MIME-1 values (including
  the preferred suffix) before storing the canonical wire annotation.
- JSON encoding supports Money values when their registered nominal Currency metadata is available. It preserves the exact decimal amount as a string and emits the registered currency code.
- Semantic quantities retain reduced dimension exponents, exact rational scale,
  and affine metadata through numeric aggregates. Integrating speed by elapsed
  time adds the Time exponent and returns a length quantity. Dimension expressions
  parse qualified names without consuming multiplication or division operators.

- `orna publish` refuses a format-1/2 `HEAD` before recovery, runtime access or
  any freeze, with `E2300`; it writes nothing and the pending range stays. The
  help says this build has no writer or in-place upgrade for format-1/2
  snapshots and names the upgrade work item, ornadb-5il1.184. Inside a prepared
  migration copy it says the format-3 candidate is prepared but not activated,
  and that activation is tracked as ornadb-5il1.192. The loose and compact
  (Parquet) row writers, their recovery paths and the CLI compact-journal
  recovery are retired. Format-1/2 commits keep their library readers, but no
  CLI verb reads them yet (ORNA-UPGRADE-003, ORNA-ROW-013).

- The `format.orna` records that the legacy writers produced are now read as
  read-only format-1/2 inputs: OrnaDB 1.0's `{repository_format: 1,
  storage_profile: "compact-storage-v1"}` and the previous draft's closed
  format-2 record. They were rejected as malformed, so `orna migrate copy`
  failed on every real legacy repository. Runtime writes now follow the format
  recorded at `HEAD`. A format-1/2 `HEAD` refuses them with
  `ORNA-REPO-CONTEXT-015` even when the worktree holds format-3 metadata, and
  `orna run` and the other write verbs exit with `E2200`. The one exception is
  a prepared migration copy (ORNA-UPGRADE-001).

- `orna migrate copy` reports a source whose format context cannot be admitted as `E2100` titled `migration source could not be admitted: <cause>`, with the cause's `ORNA-REPO-CONTEXT-*` code, and creates no destination.

- `orna run` and the streaming project invocation keep the underlying failure on `E2200`. A runtime error carries its `sys.*` public code when it has one, otherwise its non-disclosing display text; a legacy read-only head refusal carries its `ORNA-REPO-CONTEXT-*` code and names its remedy in the help line. A streaming failure carries the runtime stream-failure label and no evaluator nested causes. Nested cause codes apply to `orna run` only: an evaluator rejection lists each nested cause code the evaluator itself emits, and redacts any other code, including a user-authored `ORNA-*` code. These details render with `--debug` only, per ORNA-UX-004. Evaluator spans and the shared source diagnostic remain open under ornadb-5il1.152 and ornadb-5il1.101.

- Format-3 store-only publication validates canonical row keys without legacy
  loose-row path resolution, including no-op preparation.

- Explicit format-3 row rekeys retain their relation identity and canonical old/new
  addresses in the publication journal so replay and recovery do not infer a move
  from filenames or collapse it into delete-plus-insert.

- Missing unrooted prepared publication commits can be discarded during recovery
  only when HEAD and the base index still match, preserving frozen runtime
  mutations for candidate rebuild and retry after Git garbage collection.

- Publication reconciliation keeps bounded OGS node metadata materialized while
  leaving OGB-2 raw chunks in Git's object store, avoiding a second worktree copy
  of imported media.

- Citation scanning counts active test operands and referenced test helpers/constants while excluding diagnostic text, format placeholders, unused helpers/constants, and non-test literals. The optional `--locations-output` flag emits deterministic per-ID source locations.

- Replacing a mounted top-level Blob file updates that field's immutable
  descriptor through the durable row-save transaction while retaining the
  existing row key and the previous content for already-open handles.
- The CLI `add --row TABLE KEY` stages one selected logical row into the index. `commit -m MESSAGE` reviews that exact staged candidate and publishes it through the protected reviewed-candidate repository path; runtime-tail publication remains the separate `publish` command.
- Source identity reconciliation accepts explicit rename mappings and preserves
  existing IDs across authorized name changes. Unassisted renames remain
  delete-plus-create, and retired IDs are never reused.
- LSP declaration rename identity follows only WorkspaceEdits with exact resulting
  document text observed at a newer didChange version; saving requires disk text
  to match current document text. If one didChange combines the returned edit with
  unrelated edits so the exact result is not observed, that step is not persisted
  (fail closed); reusing a renamed-away name before save may lose identity continuity.
- The `mv` CLI command renames committed tables and retains their table and
  child-field IDs in uncommitted source and identity-manifest changes. It can
  resume after a source rename is interrupted before the manifest update. The
  format-3 store keeps rows under the retained table ID, so `mv` moves no row
  files. If a legacy format-1/2 loose-row directory exists, `mv` still renames
  it. That is legacy-layout handling, not a format-3 feature: Orna 1.1.0
  forbids loose-row writes for the new format (ORNA-UPGRADE-003).

- Optional Blob fields preserve `Some` bindings through source-row admission,
  metadata queries, history diffs and offline export; overflow verification counts
  nested Blob references through `Some` without reading payload bytes.
- FUSE workspace mounts page through the pinned row index on listing, resolve
  known keys by the index, and isolate projection errors as EIO so one corrupt
  row does not block neighboring rows.
- FUSE workspace mounts hold a private, per-worktree reference to their pinned
  snapshot so reads survive HEAD changes and garbage collection. SIGINT,
  SIGTERM, and SIGHUP clean up mounts, while orna unmount DIR can detach
  orphaned mounts when DIR is relative to the command's current working
  directory.
- `mount DIR --at SELECTOR` now starts a read-only FUSE session pinned to the
  resolved commit, so its mounted paths continue to read that historical state
  after workspace HEAD advances.
- FUSE teardown serializes daemon and CLI cleanup so concurrent detach paths do
  not race to remove the same view pin and record.
- The trusted REPL exposes the root namespace as `main` unless a user module named
  `main` exists, and admits `main.add` Blob rows using typed keys against the
  committed format-3 schema. A semicolon-terminated capture/add submission
  suppresses the row result while retaining one durable runtime transaction.
  Captured scratch refs share the runtime owner and are reclaimed when a later
  runtime reopen recovers a crashed session.

- Decimal division preserves exact terminating quotients and provides explicit
  scale with half-even rounding.

- Decimal currency suffixes construct Money only when the selected nominal
  implements Currency; a bare uppercase code is no longer sufficient.

- `Money<C>` requires a currency type: a nominal type that implements the core
  `Currency` protocol, or a generic parameter bounded by it. A declared or
  imported `Currency` counts only when its surface is exactly the core one.
  `Money<Int>`, table rows, types without `Currency` and undeclared names are
  rejected, and three-letter names are no longer assumed to be currencies.
  Imported currencies now construct Money from `12.34.GBP`. A Float currency
  literal such as `12.34f.GBP` is rejected with E5004.


- Format-3 table schemas encode defaulted fields and keys as stored or key
  fields, computed fields as non-stored role-2 selectors with an expression
  digest, table-typed fields as stored references, and enum, nominal record,
  refined, transparent-alias and `Money` field types with their closed
  definitions. The identity manifest issues stable IDs for nominal types, enum
  variants and their fields. Explicit type and variant renames passed to the
  manifest reconciliation API carry those IDs; `orna mv` and LSP rename still
  rename only tables and fields. A manifest that records these new identity
  kinds cannot be read by older binaries, which reject unknown kinds.
- Format-3 rows admit every registered value tag. Tables keyed by Uuid, Date,
  Instant, Decimal, enum, reference or tuple values store and order rows by
  logical key, and Int and UInt spellings of one integer are one key.
- MIME and file-name code reads one MIME-1 registry pinned to the Orna 1.1.0
  `profiles/media-1.json`. Octet-stream and unregistered media types accept any
  safe suffix hint, and stored Blob annotations with quoted parameters or `_`
  hints read back. `sys.blob.capture_file` now supplies the
  `application/octet-stream` annotation with no suffix instead of guessing from
  the file extension; declare the media type with `sys.blob.annotate`.

- The parser no longer changes valid source silently: a comparison followed
  later by `> (` keeps all of its tokens; a braced control statement (`if`,
  `case`, `for`, `while` or `loop`) ends before a following `(`, `[`, `-`, `+`
  or `..` item; control headers never read `Name {` as a constructor (an
  unparenthesised one before the body is reported); table subjectless
  predicates keep their operator; and spaceless arithmetic such as
  `1+10-20-30` is not a date.
- Case guards such as `x if x < limit:`, `assert` as a variant, label, field or
  member name, and default parameters of generic functions parse in modules as
  in the REPL. Legacy assertion, colon-bound and assignment-expression
  diagnostics now come from the failing production in every entry point.
- Publication refuses a detached HEAD, a stale reviewed candidate, managed-file
  edits and store conflicts before it writes its prepared journal, and rechecks
  the captured index immediately before the ref compare-and-set. A refused or
  lost compare-and-set discards that journal, so the pending batch can be
  retried. Recovery likewise discards a prepared journal when neither HEAD nor
  any local branch contains its candidate, even if the candidate object
  survives or the index changed since; legacy-migration journals stay
  resumable. Recovery also recognises a reconciled `.orna/store` index
  installed before its journal stage. New candidate objects are flushed before
  the ref names them, and the ref is flushed before the runtime tail is
  consumed.

- `orna serve` no longer holds its shared session state while a WebSocket
  waits on its peer, so open sockets do not block other sockets or
  `/orna/session` requests. A resume replaces and closes the old socket, idle
  sockets are pinged and silent peers are disconnected, and request heads are
  bounded in time and size. Requests whose Host is not the listener's loopback
  authority, and cross-origin writes, get 403. Smart-HTTP push now requires
  `serve --allow-push`.

- `orna serve` ends live sessions in order. `DELETE /orna/session/<id>` cancels
  and joins the session's application work and closes its socket before it
  answers 204, including while the socket is attached; it used to answer 503
  then, and 400 once the session had evaluated anything. A sweep ends sessions
  whose lease has elapsed in the same way.

- `orna serve` serves at most 64 connections at once. The accept loop answers
  any further connection with 503 straight away, without reading its request.

- `orna serve` bounds each HTTP request body and each response with one
  deadline for the whole transfer, 15 s plus the time it takes at 64 KiB/s,
  instead of bounding each blocked read or write. A peer that sends or reads a
  few bytes at a time can no longer keep a connection busy.

- Live session HTTP errors use the statuses `30-protocol.md` assigns: a
  WebSocket upgrade from another origin gets 403 instead of 410, and a DELETE
  carrying a token other than the live session's current one gets 401 instead
  of 410. A child-free DELETE that would first have to drain application work
  gets 503 instead of 400. `Content-Type: application/json; charset=utf-8` is
  accepted.
- Semantic checking types `+ - * / % ^`, unary signs and compound
  assignment through one operator table: Int, Decimal and Float never mix,
  Money scales only by exact numbers in one currency, declared-unit
  quantities follow dimension and affine algebra, and other operand pairs
  report `E2001` (`E5001` for incompatible dimensions). Every reachable
  `return`, including inside anonymous functions, is checked against the
  declared result or joined into the inferred one. Blocks without a final
  expression, bare `return;`, bare `break` and `T.delete` have type `Unit`.
- `sys.rt.info()` is linked into the production sys provider, so it evaluates
  to a `sys.RuntimeInfo` value carrying the runtime's real coordinates instead
  of an unlinked callable. Admission no longer reports a version diagnostic for
  it (ORNA-SYS-006).
- The CLI's `migrate abort DEST` discards a prepared format-3 migration copy only
  after verifying that DEST is the copy prepared from the current source. The
  source is unchanged. Until the verified copy is renamed to its reserved
  sibling quarantine, a change to the source commit, index, or working tree
  since preparation makes the command refuse and remove nothing. That rename is
  the commit point: a repeated `migrate abort DEST` finishes an interrupted
  removal without rechecking the source, reports nothing to discard once nothing
  remains, and `migrate copy` refuses the destination while the quarantine
  exists.
- The command-line executable is now `orna`; the package and crate remain
  `orna-cli-v1`. The former `orna-cli-v1` executable name is removed with no
  alias or shim. Build output is `target/<profile>/orna`, and the Linux package
  and release paths, CI, and the command examples in the README and docs name
  `orna`.
- `TransactionalEvaluator` and `DurableTransactionalEvaluator` now admit
  transactions against the production catalogue by default, so a user module
  whose path matches a conformance fixture namespace, such as
  `vehicle/corsa.orna`, is no longer rejected with `ORNA-S002-NAMESPACE`. The
  conformance fixture catalogue is an explicit harness opt-in only, through
  `TransactionalEvaluator::with_fixture_catalogue`.

## 1.0.0 — release inventory (publication pending)

> **Historical inventory (note added 2026-10-21).** This section was written
> for an Orna 1.0.0 release that was never published. Most surfaces that it
> lists were removed from the tree before any release. Do not read it as a
> description of the current product. The current target is Orna 1.1.0
> (publication `final-2026-10-05`, repository writer format 3). Its reference
> bundle `../reference/Orna-1.1.0` has 947 requirements. The Unreleased section
> above describes current behaviour.
>
> These surfaces were removed. No release shipped them.
>
> - The previous `orna` binary of the `orna-server` package. It provided
>   `orna source check`, `source diff` and `source apply`, daemon and server
>   administration, runtime description, raw calls, USER state, inspection,
>   security administration, the Inspector, `--explain`, argument files,
>   canonical output, the JSON, table and CSV presenters, invocation tracing
>   and the managed Unix-socket `invoke` route. Commit `4777f6213` removed it
>   (issue #603). The current `orna` command is the `orna-cli-v1` binary. It
>   rejects `orna+unix://` and TLS endpoints.
> - The direct SQLite adapter that a filesystem `--db` path selected. Commits
>   `b0f4c4f76` (issue #285) and `3dc8360b5` (issue #603) removed it. `--db
>   PATH` now selects a local Git worktree. SQLite remains only as private
>   runtime state.
> - The PostgreSQL engine, its generated SQL migrations and the `_orna_kernel`
>   tables. Commits `3dc8360b5` (issue #603) and `f1fb5684c` (issue #889)
>   removed them.
> - The accepted demo inventory `examples/accepted-demos.toml`. Commit
>   `d0dbd7a59` removed it.
> - ORV5 and ORV6 as value contracts. Orna 1.1.0 defines OVB-1, OVB-2, ROV-3
>   and SOV-3. The CLI does not depend on the `orna-runtime-tty` crate.
>
> Four bullets below describe surfaces that are still in the tree. They were
> corrected in place on 2026-10-21: `mount --at`, the system API publication
> fields, `migrate copy`, and loose-row paths.

**Status:** The Cargo workspace currently identifies itself as `1.0.0`, but this
checkout is not a published production release. The audited immutable
normative reference bundle is maintained outside this checkout at
`../reference/Orna-1.0.0`; its requirement ledger still records all 870
requirements as not executed. This inventory must not be read as a conformance
claim.

The accepted release-mechanics decision reserves a future Debian 12 amd64
release identity of `orna 1.0.0-1` with a signed `v1.0.0` source tag. That
decision does not declare the product baseline complete. This checkout now has
a minimal `packaging/linux/` provenance/install artifact recipe and focused
tests, but it has no Debian changelog, signed artifact, or clean-host
production-package evidence.
See the [release-mechanics decision](docs/decisions/archive/1.0/0047-first-one-zero-release.md)
for the required release authority and the
[maintainer runbook](docs/maintainer-runbook.md) for evidence rules.

### Product boundary

- OrnaDB is a backend-neutral `.orna` source language and typed application
  model. A source file does not select a backend dialect, and the public product
  does not ask users to write backend-specific SQL.
- A filesystem database path selects the direct SQLite adapter. That adapter is
  a bounded implementation/runtime route for the same source model; it does
  not define a second source language. Physical operations the adapter cannot
  represent are rejected rather than given backend-specific semantics.
- `orna://` and `orna+unix://` endpoint forms are parsed. The bounded installed
  `invoke` route supports only the current managed Orna Unix socket through
  local authentication; remote TLS and other explicit-endpoint commands remain
  unavailable in this checkout and fail closed.

### Implemented source and server surface

The following behaviors are present in the source tree and are exercised by
checked-in unit, integration, or accepted-fixture coverage. The list describes
implementation boundaries, not a claim that every environment-dependent
release gate has passed.

- `orna source check <file.orna>` checks one regular UTF-8 source file without a
  database, network access, configuration, child process, or write. It is a
  standalone check against the empty application catalogue; it does not prove
  continuity with an installed revision.
- `orna source diff <file.orna>` prepares a read-only semantic comparison with
  the active application revision. `orna source apply <file.orna>` checks and
  prepares one source file, records a typed application migration, and commits
  the candidate atomically. Diagnostics, an active-base race, an unsupported
  physical operation, or failed post-apply hash reproduction fails closed.
- The source compiler and accepted fixtures cover bounded server functions,
  typed scalar/reference/record values, identity-selected mutations, client
  expression bodies, local/session/user state declarations, resources and
  actions, JSON, bounded table presentation, and the accepted structural UI
  constructors. The [accepted demo inventory](examples/accepted-demos.toml)
  is the authoritative list of checked-in examples; it is not a replacement
  for the missing canonical spec.
- The CLI exposes function invocation, a bounded local function-backed REPL
  route, daemon/server administration, runtime description, source
  check/diff/apply, raw calls, USER state, inspection, and security
  administration. A local format-3 REPL with writable, identity-admitted source
  matching `HEAD` can commit a Blob capture and its source-row table insert as
  one durable runtime transaction without publishing a Git commit. Other
  effectful REPL submissions fail closed when matching format-3 transaction
  authority is unavailable. This does not establish engine
  execution for the canonical `ORNA-REPL-001..006` requirements. `--explain`,
  argument files, canonical output, JSON/table/CSV presenters, and bounded
  invocation tracing are implemented where the selected route supports them.
- On Linux, `orna mount DIR --cwd` mounts the current committed workspace
  through host FUSE and allows validated in-place row-document saves, including
  editors that open with `O_TRUNC`. Existing rows are validated and replaced
  before durable data changes; sibling temporary files renamed over `data.orna`
  remain supported. `orna unmount DIR` releases the kernel mount and its view
  record. `orna mount DIR --at SELECTOR` starts a read-only FUSE session pinned
  to one resolved commit; see the Unreleased section.
- Committed snapshot loading preserves nested module import candidates while
  keeping format-3 row documents out of loose-row source discovery.
- USER state is principal-scoped and typed. Local peer authentication supplies
  the principal; optimistic revision conflicts and invalid expected types fail
  closed. State values use the canonical ORV5 value representation.
- The system API exposes publication metadata as typed records. `sys.Storage`
  declares `pending_rows`, `pending_bytes`, `last_publication` and a
  `publication_policy` field of type `sys.Value`. Orna 1.1.0 defines no
  `sys.PublicationPolicy` type, and `sys.MaintenanceJob` has no row or byte
  counts.
- The Inspector exposes immutable invocation epochs, eight closed projections,
  redacted-by-default records, privilege checks, and sequence-addressable
  traces. Local SQLite inspection is narrower and stores only bounded,
  redacted invocation/inspection metadata; it does not expose durable resource
  payloads or arbitrary stored values.
- The installed `tty` runtime renders terminal documents and byte streams. The
  runtime/value boundary uses canonical ORV5 values and ORV6 for checked sets;
  output framing is validated before bytes are written. The Qt path is not a
  production-support claim for this inventory: its ABI/native checks require
  the absent canonical header and separately gated native dependencies.
- `orna-lsp`, the Tree-sitter grammar, the TextMate grammar, and editor
  integration packages provide the checked-in static tooling surface. LSP
  diagnostics, symbols, hover, references, completion, semantic tokens, and
  pull/push diagnostics are implemented. Static gates do not launch editor
  hosts and do not prove parity with the absent spec or host-session behavior.

### Storage and compatibility contracts

- Internal application migrations use a closed, typed, backend-neutral
  migration representation. The source is lowered by each storage adapter;
  users must not edit generated SQL or internal `_orna_kernel` tables. The
  application migration ledger retains expected and candidate source/catalogue
  revision pairs, canonical bytes, and digests.
- Revision recovery validates lineage, active pointers, ledger order, canonical
  digests, and post-apply reproduction. Historical source and standard
  snapshots are immutable inputs to this verification; hand-editing retained
  rows is unsupported.
- The CLI's `migrate copy` command prepares or recovers a journalled candidate
  in a separate directory from a local format-1/2 repository, leaving the
  source commit and workspace unchanged. It requires a decoded annotation-default
  count (`--annotation-defaults N`) and the `--no-persistent-consumers` flag.
  Despite its name, that flag confirms that persistent consumer checkpoints are
  carried, not reset. Each carried checkpoint must match a consumer identity
  correspondence recorded in the candidate before any row is imported. A source
  that holds live stream state (leases, claims, failures or receipts) is
  refused. This follows ORNA-UPGRADE-008. This preparation step does not
  convert legacy rows into a format-3 store or activate a writable format-3
  repository.
- Format-3 committed Blob rows require exact canonical MIME-1 media types and
  suffixes. Readers reject noncanonical stored annotations instead of
  normalizing them during metadata projection or SOV-3 conversion.
- A prepared Blob row binds its stored annotation to the annotation its pin was
  admitted with. The candidate metadata lookup rejects a row whose spelling is
  not canonical MIME-1, or that differs from the admitted pair, so a transfer
  record cannot report an annotation the pin never admitted.
- The CLI publication boundary validates format-3 rows against the pinned
  committed table binding and writes them to the committed format-3 store. It
  derives no loose-row paths. Loose-row paths are a format-1/2 layout only;
  Orna 1.1.0 forbids them for new-format writes (ORNA-UPGRADE-003). Invalid
  rows fail closed while preserving the pending range.
- `orna history <relation> --diff <from> <to>` decodes source-owned Object row
  fields against each endpoint's committed schema and compares stored Blob
  annotations without fetching payloads.
- Product selection retains standard-library snapshots through `orna.std/9`.
  The pre-1.0 V10 `std/cli.orna` source and V9-to-V10 upgrade constructor are
  retired; a stored V10 revision can only be verified when supplied directly
  and is never reconstructed or substituted. The pre-1.0 V11 SQL-shaped math
  addition is also retired; pure math comes from the pinned Orna 1.0 source.
- Local server sockets use the private Orna protocol handshake (versions 1–5,
  with bounded fallback behavior where an opaque codec registry is unavailable)
  and local peer authentication. These are private Orna contracts, not a
  public SQL or remote transport protocol.

### Explicitly deferred, unavailable, or not promised

The following boundaries are intentional and must not be inferred as shipped
from the implementation inventory:

- Full language/spec conformance, canonical examples, and ABI parity: the
  canonical bundle is absent, so no conformance or ABI result is claimed.
- The checked-in accepted corpus is intentionally bounded. Examples recorded
  as deferred include broader ALTER/drop/rename forms, procedural declarations,
  top-level INSERT, list/map/record literals and lambdas, broader SERVER
  expression calls/operators, additional CLIENT return forms, and
  value-type/user/role/grant/revoke declarations.
- Remote Orna TLS sessions, explicit Unix-socket routing beyond the bounded
  installed `invoke` route, reflective JSON-RPC/MCP gateways, a production
  CLIENT VM/sandbox, arbitrary toolkit or browser runtimes, and richer Studio
  workflows.
- SQLite physical/runtime parity beyond the bounded adapter capabilities
  described above. Unsupported SQLite physical shapes fail closed; the source
  language remains backend-neutral.
- Native Qt/ABI, editor-host, and clean-host distribution checks unless a named
  command has been run with retained evidence. `CARGO_NET_OFFLINE=true` only
  makes Cargo dependency resolution fail closed; it does not make a native
  build host- or network-free.
- A production package, repository publication, signing, SBOM/package
  inventory, support period, SLA, backup/DR policy, or recovery guarantee.
  CI retention artifacts and local build outputs are not production
  distribution authorities.
- The canonical live `sys.Session` relation, `sys.SessionRef`,
  `sys.rt.sessions`, and `sys.RuntimeView.sessions` projection. Commit
  `78301153` pins that schema in `sys_api_contract`, but it does not supply
  authoritative session-field provenance or runtime projection evidence.
- An in-place upgrade from a 0.x or development engine. The first-release
  decision defines empty accepted predecessor/forward-edge sets; no package
  predecessor migration is implemented. There is no `orna server upgrade`
  command in the current CLI.
- RPM packaging or cross-format upgrade support.

For migration steps and compatibility cautions, see the
[1.0.0 migration guide](docs/migration-1.0.0.md). For the commands, prerequisites,
and evidence vocabulary used by maintainers, see the
[maintainer runbook](docs/maintainer-runbook.md) and
[editor tooling guide](docs/editor-tooling.md).
