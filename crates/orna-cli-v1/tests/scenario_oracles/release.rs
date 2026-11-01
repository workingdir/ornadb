//! Entries for the 130 FINAL-* and 27 V110-* release oracles.
//!
//! A FINAL oracle asks the product to exercise one requirement, its rejected
//! inputs and its concurrency or recovery boundary on a final-format
//! database. Executors check the requirement through product commands; gap
//! probes reproduce the first blocker on that path.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    sync::LazyLock,
};

use sha2::{Digest, Sha256};

use crate::{
    harness::{
        Check, Db, Entry, Failure, Probe, check_refuses, ensure, harness_ensure, hex,
        repl_refuses, unknown_command, vendored_fixture,
    },
    mandatory::{LIVE_BLOCKER as LIVE_WATCH, watches_receive_no_update},
};

/// Byte-identical copy of the release `tests/final-vectors.json`, vendored in
/// this crate and pinned to the release `SHA256SUMS` entry.
const FINAL_VECTORS_SHA256: &str =
    "985140787cde755ae53034de28d46cb4f5696d8bc95e385876e95ec0c953899d";

static FINAL_VECTORS: LazyLock<serde_json::Value> = LazyLock::new(|| {
    let text = vendored_fixture("tests/fixtures/reference/tests/final-vectors.json");
    assert_eq!(
        hex(&Sha256::digest(text.as_bytes())),
        FINAL_VECTORS_SHA256,
        "the vendored final vectors no longer match the release file"
    );
    serde_json::from_str(&text).expect("the release vectors are JSON")
});

const BLOB_OPERATIONS: &str = "the evaluator dispatches no Blob operation beyond file capture and annotation: `sys.blob` reads, comparisons, digests, writers and constructors are unsupported";
const SERVING_ENTRY: &str = "`orna serve` answers with its own native page and never runs the project's `serve()` entry, so no source-hosted application runs";
const NO_CLONE: &str =
    "there is no `orna clone`, so no second, partial or mirror clone of a database can be made";
const NO_UPGRADE: &str = "there is no `orna upgrade`, so no format-1/2 history can be opened through an upgrade candidate";
const RUN_ROWS: &str = "rows written by `orna run` are refused at publication, so no product path builds a multi-row table";
const TYPED_KEYS: &str = "the CLI names rows only by text or bytes: an Int-keyed row cannot be found with `orna query --key`";
const STORAGE_METADATA: &str = "storage metadata and work counters are unobservable: `sys.Storage` does not evaluate in the project REPL";
const CHECKOUT: &str = "`orna checkout` to a different commit is refused while drafts are pending or a changed path has uncommitted changes: pending drafts are not carried forward (CHECKOUT-3 step 4) and no checkout journal is written";
const STAGING: &str =
    "`orna add --row` cannot stage a published row: it reports that the logical row was not found";
const OVERFLOW: &str = "a row above the 64 KiB node bound cannot be published: publication cannot prepare the candidate";
const RUN_CAPTURE: &str = "`orna run` cannot capture a file into a row: the invocation is rejected";
const READ_ONLY_WORKSPACE: &str = "the recorded blocker that the workspace mount (`orna mount DIR --cwd`) is read-only is spec-false: profiles/vfs-1.md:20 makes --cwd writable unless --read-only; re-scope in ornadb-kfbk";
const CWD_MOUNT: &str = "the workspace mount does not project the CWD: a row inserted but not published is missing from it";
const DESCRIPTOR_EDITS: &str = "`orna edit` accepts Blob descriptor edits it must refuse or apply: a traversal path or a null required Blob is admitted";
const MEDIA_TYPE_EDITS: &str =
    "`orna edit` cannot change a stored Blob's media type: the edit is not applied";
const DECODED_MEDIA: &str = "no decoder runs for stored media: `orna query` reports no decoded properties such as image dimensions";
const NESTED_BLOBS: &str =
    "a row cannot hold an optional or nested Blob: inserting `Blob?` null and `[Blob]` is refused";

pub fn entries() -> Vec<Entry> {
    vec![
        // FINAL-ROW
        Entry::gap(
            "FINAL-ROW-001",
            "ornadb-5il1.180",
            TYPED_KEYS,
            typed_key_lookup_fails,
        ),
        Entry::gap(
            "FINAL-ROW-002",
            "ornadb-5il1.180",
            TYPED_KEYS,
            typed_key_lookup_fails,
        ),
        Entry::gap(
            "FINAL-ROW-003",
            "ornadb-5il1.122",
            RUN_ROWS,
            run_rows_are_not_published,
        ),
        Entry::pass("FINAL-ROW-004", blob_rows_hold_references),
        Entry::gap(
            "FINAL-ROW-005",
            "ornadb-5il1.129",
            OVERFLOW,
            large_row_is_not_published,
        ),
        Entry::gap(
            "FINAL-ROW-006",
            "ornadb-5il1.122",
            RUN_ROWS,
            run_rows_are_not_published,
        ),
        Entry::gap("FINAL-ROW-007", "ornadb-gov5.21", NO_CLONE, no_clone),
        Entry::gap(
            "FINAL-ROW-008",
            "ornadb-mmsa.28.16",
            STORAGE_METADATA,
            storage_unobservable,
        ),
        Entry::gap(
            "FINAL-ROW-009",
            "ornadb-mmsa.28.17",
            "`orna query` reports actual object reads but no estimate to compare them with",
            query_reports_no_estimate,
        ),
        Entry::pass("FINAL-ROW-010", repacking_changes_no_logical_rows),
        Entry::pass("FINAL-ROW-011", repl_enforces_table_assertions),
        Entry::gap(
            "FINAL-ROW-012",
            "ornadb-z7v3",
            EXPORT_COPIES_TAMPERED_STORE,
            row_012_blocker,
        ),
        Entry::gap("FINAL-ROW-013", "ornadb-5il1.184", NO_UPGRADE, no_upgrade),
        Entry::gap(
            "FINAL-ROW-014",
            "ornadb-5il1.123",
            STORAGE_METADATA,
            storage_unobservable,
        ),
        Entry::gap(
            "FINAL-ROW-015",
            "ornadb-z7v3",
            EXPORT_COPIES_TAMPERED_STORE,
            row_015_blocker,
        ),
        // FINAL-GRAPH
        Entry::gap(
            "FINAL-GRAPH-001",
            "ornadb-z7v3",
            EXPORT_COPIES_TAMPERED_STORE,
            graph_001_blocker,
        ),
        Entry::gap(
            "FINAL-GRAPH-002",
            "ornadb-z7v3",
            EXPORT_COPIES_TAMPERED_STORE,
            graph_002_blocker,
        ),
        Entry::pass("FINAL-GRAPH-003", terminal_chunks_are_raw_blobs),
        Entry::gap(
            "FINAL-GRAPH-004",
            "ornadb-5il1.129",
            OVERFLOW,
            large_row_is_not_published,
        ),
        Entry::gap(
            "FINAL-GRAPH-005",
            "ornadb-z7v3",
            EXPORT_COPIES_TAMPERED_STORE,
            graph_005_blocker,
        ),
        Entry::pass("FINAL-GRAPH-006", sha256_repositories_keep_native_widths),
        Entry::pass("FINAL-GRAPH-007", store_roots_hold_only_the_snapshot),
        // FINAL-BLOB
        Entry::pass("FINAL-BLOB-001", annotation_never_alters_raw_bytes),
        Entry::gap(
            "FINAL-BLOB-002",
            "ornadb-5il1.132",
            "`orna check` accepts intrinsic synonyms the release forbids, such as a `Clob` field",
            forbidden_blob_synonyms_are_accepted,
        ),
        Entry::gap(
            "FINAL-BLOB-003",
            "ornadb-5il1.109",
            NESTED_BLOBS,
            nested_blobs_are_refused,
        ),
        Entry::pass("FINAL-BLOB-004", content_identity_ignores_annotations),
        Entry::gap("FINAL-BLOB-005", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(
                db,
                "sys.blob.same_content(sys.blob.from_bytes([1, 2]), sys.blob.from_bytes([1, 2]))",
            )
        }),
        Entry::gap("FINAL-BLOB-006", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(
                db,
                "sys.blob.same_content(sys.blob.from_bytes([1]), sys.blob.from_bytes([2]))",
            )
        }),
        Entry::gap("FINAL-BLOB-007", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.length(sys.blob.from_bytes([1, 2, 3]))")
        }),
        Entry::gap("FINAL-BLOB-008", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.read(sys.blob.from_bytes([1, 2, 3]), 0, 2)")
        }),
        Entry::gap("FINAL-BLOB-009", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.from_bytes([1, 2, 3])")
        }),
        Entry::gap(
            "FINAL-BLOB-010",
            "ornadb-5il1.142",
            "the REPL cannot inspect a Blob value: evaluating a captured Blob fails",
            blob_value_cannot_be_inspected,
        ),
        Entry::gap("FINAL-BLOB-011", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.read(sys.blob.from_bytes([1, 2, 3]), 3, 1)")
        }),
        Entry::gap("FINAL-BLOB-012", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.read(sys.blob.from_bytes([1, 2, 3]), 1, 1)")
        }),
        Entry::gap("FINAL-BLOB-013", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.begin(null, 16)")
        }),
        Entry::gap("FINAL-BLOB-014", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.begin(4, 2)")
        }),
        Entry::gap(
            "FINAL-BLOB-015",
            "ornadb-5il1.120",
            "`orna check` admits host file capture inside a table assertion, a context where it is forbidden",
            capture_is_admitted_in_an_assertion,
        ),
        Entry::gap("FINAL-BLOB-016", "ornadb-5il1.130", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.digest(sys.blob.from_bytes([1, 2, 3]))")
        }),
        Entry::gap("FINAL-BLOB-017", "ornadb-5il1.130", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.digest(sys.blob.from_bytes([1]))")
        }),
        Entry::gap("FINAL-BLOB-018", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.from_bytes([0])")
        }),
        Entry::gap("FINAL-BLOB-019", "ornadb-gov5.21", NO_CLONE, no_clone),
        Entry::gap("FINAL-BLOB-020", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.read(sys.blob.from_bytes([1, 2, 3]), 0, 1)")
        }),
        Entry::gap("FINAL-BLOB-021", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.to_bytes(sys.blob.from_bytes([1, 2]))")
        }),
        // FINAL-OGB
        Entry::pass("FINAL-OGB-001", descriptors_match_the_release_graph),
        Entry::gap(
            "FINAL-OGB-002",
            "ornadb-z7v3",
            EXPORT_COPIES_TAMPERED_STORE,
            ogb_002_blocker,
        ),
        Entry::gap(
            "FINAL-OGB-003",
            "ornadb-z7v3",
            EXPORT_COPIES_TAMPERED_STORE,
            ogb_003_blocker,
        ),
        Entry::gap("FINAL-OGB-004", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.verify(sys.blob.from_bytes([1]))")
        }),
        Entry::gap(
            "FINAL-OGB-005",
            "ornadb-4ihd",
            STORAGE_METADATA,
            storage_unobservable,
        ),
        Entry::pass("FINAL-OGB-006", missing_chunks_are_not_empty_content),
        Entry::pass("FINAL-OGB-007", small_blobs_use_one_terminal_chunk),
        Entry::gap(
            "FINAL-OGB-008",
            "ornadb-5il1.139",
            MEDIA_TYPE_EDITS,
            media_type_edit_is_not_applied,
        ),
        // FINAL-MIME
        Entry::pass("FINAL-MIME-001", unknown_content_is_octet_stream),
        Entry::pass("FINAL-MIME-002", media_types_normalize),
        Entry::pass("FINAL-MIME-003", projected_suffixes_follow_the_registry).with_fuse(),
        Entry::pass("FINAL-MIME-004", suffix_hints_are_validated),
        Entry::pass("FINAL-MIME-005", defaults_canonicalize_to_one_value),
        Entry::gap(
            "FINAL-MIME-006",
            "ornadb-ehq1",
            "`orna import` only reports a dry run, so no import resolves and writes media annotations",
            import_only_dry_runs,
        ),
        Entry::gap(
            "FINAL-MIME-007",
            "ornadb-5il1.123",
            STORAGE_METADATA,
            storage_unobservable,
        ),
        Entry::gap(
            "FINAL-MIME-008",
            "ornadb-ehq1",
            MEDIA_TYPE_EDITS,
            media_type_edit_is_not_applied,
        ),
        Entry::gap(
            "FINAL-MIME-009",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        // FINAL-VFS
        Entry::pass("FINAL-VFS-001", row_directories_project_non_key_fields).with_fuse(),
        Entry::gap(
            "FINAL-VFS-002",
            "ornadb-5il1.136",
            CWD_MOUNT,
            workspace_lacks_a_cwd_row,
        )
        .with_fuse(),
        Entry::gap(
            "FINAL-VFS-003",
            "ornadb-5il1.123",
            STORAGE_METADATA,
            storage_unobservable,
        ),
        Entry::gap(
            "FINAL-VFS-004",
            "ornadb-5il1.139",
            DESCRIPTOR_EDITS,
            traversal_descriptor_is_admitted,
        ),
        Entry::gap(
            "FINAL-VFS-005",
            "ornadb-5il1.136",
            CWD_MOUNT,
            workspace_lacks_a_cwd_row,
        )
        .with_fuse(),
        Entry::gap(
            "FINAL-VFS-006",
            "ornadb-5il1.122",
            "an `orna edit` save skips a table assertion that `orna run` enforces",
            edit_skips_table_assertions,
        ),
        Entry::gap(
            "FINAL-VFS-007",
            "ornadb-5il1.137",
            READ_ONLY_WORKSPACE,
            workspace_is_read_only,
        )
        .with_fuse(),
        Entry::gap(
            "FINAL-VFS-008",
            "ornadb-5il1.137",
            READ_ONLY_WORKSPACE,
            workspace_is_read_only,
        )
        .with_fuse(),
        Entry::gap(
            "FINAL-VFS-009",
            "ornadb-5il1.140",
            "a refused `orna edit` keeps no draft: the next edit opens the accepted text, not the rejected bytes",
            refused_edit_keeps_no_draft,
        ),
        Entry::gap(
            "FINAL-VFS-010",
            "ornadb-ame1",
            STORAGE_METADATA,
            storage_unobservable,
        ),
        Entry::gap(
            "FINAL-VFS-011",
            "ornadb-iuq4",
            READ_ONLY_WORKSPACE,
            workspace_is_read_only,
        )
        .with_fuse(),
        Entry::gap(
            "FINAL-VFS-012",
            "ornadb-5il1.139",
            READ_ONLY_WORKSPACE,
            workspace_is_read_only,
        )
        .with_fuse(),
        Entry::gap(
            "FINAL-VFS-013",
            "ornadb-5il1.139",
            DESCRIPTOR_EDITS,
            null_required_blob_is_admitted,
        ),
        Entry::gap(
            "FINAL-VFS-014",
            "ornadb-5il1.137",
            READ_ONLY_WORKSPACE,
            workspace_is_read_only,
        )
        .with_fuse(),
        Entry::gap(
            "FINAL-VFS-015",
            "ornadb-5il1.140",
            READ_ONLY_WORKSPACE,
            workspace_is_read_only,
        )
        .with_fuse(),
        Entry::gap(
            "FINAL-VFS-016",
            "ornadb-k2wq",
            REPL_EFFECT_UNADMITTED,
            row_016_blocker,
        )
        .with_fuse(),
        Entry::gap(
            "FINAL-VFS-017",
            "ornadb-ame1",
            STORAGE_METADATA,
            storage_unobservable,
        ),
        Entry::gap(
            "FINAL-VFS-018",
            "ornadb-5il1.123",
            STORAGE_METADATA,
            storage_unobservable,
        ),
        Entry::gap(
            "FINAL-VFS-019",
            "ornadb-5il1.136",
            CWD_MOUNT,
            workspace_lacks_a_cwd_row,
        )
        .with_fuse(),
        Entry::pass(
            "FINAL-VFS-020",
            workspace_source_module_writes_are_admitted_or_refused,
        )
        .with_fuse(),
        Entry::gap(
            "FINAL-VFS-021",
            "ornadb-5il1.50",
            READ_ONLY_WORKSPACE,
            workspace_is_read_only,
        )
        .with_fuse(),
        Entry::gap(
            "FINAL-VFS-022",
            "ornadb-ame1",
            READ_ONLY_WORKSPACE,
            workspace_is_read_only,
        )
        .with_fuse(),
        Entry::pass("FINAL-VFS-023", views_expose_no_repository_internals).with_fuse(),
        Entry::gap(
            "FINAL-VFS-024",
            "ornadb-mmsa.53",
            READ_ONLY_WORKSPACE,
            workspace_is_read_only,
        )
        .with_fuse(),
        // FINAL-CAP
        Entry::gap(
            "FINAL-CAP-001",
            "ornadb-5il1.196",
            RUN_CAPTURE,
            run_cannot_capture,
        ),
        Entry::gap("FINAL-CAP-002", "ornadb-gov5.21", NO_CLONE, no_clone),
        Entry::gap(
            "FINAL-CAP-003",
            "ornadb-5il1.176",
            "`orna push` to a new remote fails after `orna commit`, so no push of a database with runtime pins completes",
            push_fails_after_commit,
        ),
        Entry::gap(
            "FINAL-CAP-004",
            "ornadb-5il1.196",
            RUN_CAPTURE,
            run_cannot_capture,
        ),
        Entry::gap(
            "FINAL-CAP-005",
            "ornadb-5il1.196",
            RUN_CAPTURE,
            run_cannot_capture,
        ),
        Entry::gap(
            "FINAL-CAP-006",
            "ornadb-20ac",
            "there is no `orna prune`, so Git maintenance has no Orna-coordinated boundary",
            |db| unknown_command(db, &["prune"]),
        ),
        // FINAL-UPGRADE
        Entry::gap(
            "FINAL-UPGRADE-001",
            "ornadb-5il1.184",
            NO_UPGRADE,
            no_upgrade,
        ),
        Entry::gap(
            "FINAL-UPGRADE-002",
            "ornadb-5il1.184",
            NO_UPGRADE,
            no_upgrade,
        ),
        Entry::gap(
            "FINAL-UPGRADE-003",
            "ornadb-5il1.184",
            NO_UPGRADE,
            no_upgrade,
        ),
        Entry::gap(
            "FINAL-UPGRADE-004",
            "ornadb-5il1.184",
            NO_UPGRADE,
            no_upgrade,
        ),
        Entry::gap(
            "FINAL-UPGRADE-005",
            "ornadb-5il1.184",
            NO_UPGRADE,
            no_upgrade,
        ),
        Entry::gap(
            "FINAL-UPGRADE-006",
            "ornadb-5il1.184",
            NO_UPGRADE,
            no_upgrade,
        ),
        Entry::gap(
            "FINAL-UPGRADE-007",
            "ornadb-5il1.184",
            NO_UPGRADE,
            no_upgrade,
        ),
        Entry::gap(
            "FINAL-UPGRADE-008",
            "ornadb-mmsa.29.5",
            NO_UPGRADE,
            no_upgrade,
        ),
        Entry::gap(
            "FINAL-UPGRADE-009",
            "ornadb-5il1.120",
            BLOB_OPERATIONS,
            |db| blob_operation_unsupported(db, "sys.blob.from_bytes([1])"),
        ),
        Entry::gap(
            "FINAL-UPGRADE-010",
            "ornadb-5il1.120",
            BLOB_OPERATIONS,
            |db| blob_operation_unsupported(db, "sys.blob.from_bytes([2])"),
        ),
        // FINAL-STAGE
        Entry::gap(
            "FINAL-STAGE-001",
            "ornadb-5il1.141",
            STAGING,
            add_row_finds_no_published_row,
        ),
        Entry::gap(
            "FINAL-STAGE-002",
            "ornadb-5il1.141",
            STAGING,
            add_row_finds_no_published_row,
        ),
        Entry::gap(
            "FINAL-STAGE-003",
            "ornadb-5il1.141",
            STAGING,
            add_row_finds_no_published_row,
        ),
        Entry::pass("FINAL-STAGE-004", external_store_edits_fail_staging),
        // FINAL-CHECKOUT
        Entry::gap(
            "FINAL-CHECKOUT-001",
            "ornadb-5il1.141",
            CHECKOUT,
            different_commit_checkout_refused,
        ),
        Entry::gap(
            "FINAL-CHECKOUT-002",
            "ornadb-5il1.141",
            CHECKOUT,
            different_commit_checkout_refused,
        ),
        Entry::pass("FINAL-CHECKOUT-003", snapshot_mounts_are_pinned).with_fuse(),
        Entry::gap(
            "FINAL-CHECKOUT-004",
            "ornadb-5il1.136",
            CWD_MOUNT,
            workspace_lacks_a_cwd_row,
        )
        .with_fuse(),
        // FINAL-APP
        Entry::gap(
            "FINAL-APP-001",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-002",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-003",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-004",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-005",
            "ornadb-5il1.200",
            "a project cannot `use` the pinned std package: `orna check` fails to resolve the import",
            pinned_std_import_fails,
        ),
        Entry::gap("FINAL-APP-006", "ornadb-8zvo", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.resource(\"style.css\")")
        }),
        Entry::gap("FINAL-APP-007", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.resource(\"assets/app.js\")")
        }),
        Entry::gap(
            "FINAL-APP-008",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-009",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-010",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-011",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-012",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-013",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-014",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-015",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-016",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-017",
            "ornadb-5il1.144",
            LIVE_WATCH,
            watches_receive_no_update,
        ),
        Entry::gap(
            "FINAL-APP-018",
            "ornadb-5il1.144",
            LIVE_WATCH,
            watches_receive_no_update,
        ),
        Entry::gap(
            "FINAL-APP-019",
            "ornadb-5il1.144",
            LIVE_WATCH,
            watches_receive_no_update,
        ),
        Entry::gap(
            "FINAL-APP-020",
            "ornadb-5il1.144",
            LIVE_WATCH,
            watches_receive_no_update,
        ),
        Entry::gap(
            "FINAL-APP-021",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "FINAL-APP-022",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        // V110
        Entry::gap(
            "V110-BLOB-01",
            "ornadb-5il1.109",
            NESTED_BLOBS,
            nested_blobs_are_refused,
        ),
        Entry::gap("V110-BLOB-02", "ornadb-5il1.130", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.digest(sys.blob.from_bytes([7]))")
        }),
        Entry::gap("V110-BLOB-03", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(
                db,
                "sys.blob.same_content(sys.blob.from_bytes([3]), sys.blob.from_bytes([3]))",
            )
        }),
        Entry::gap("V110-BLOB-05", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.read(sys.blob.from_bytes([4, 5]), 0, 1)")
        }),
        Entry::gap("V110-BLOB-07", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.read(sys.blob.from_bytes([]), 0, 0)")
        }),
        Entry::gap("V110-BLOB-08", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.begin(8, 8)")
        }),
        Entry::gap("V110-BLOB-09", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.read(sys.blob.from_bytes([9]), 0, 1)")
        }),
        Entry::gap("V110-BLOB-10", "ornadb-gov5.21", NO_CLONE, no_clone),
        Entry::gap("V110-OGB-01", "ornadb-gov5.21", NO_CLONE, no_clone),
        Entry::gap("V110-OGB-02", "ornadb-5il1.120", BLOB_OPERATIONS, |db| {
            blob_operation_unsupported(db, "sys.blob.verify(sys.blob.from_bytes([1, 2]))")
        }),
        Entry::gap(
            "V110-OGB-03",
            "ornadb-9hjd",
            SIZE_LIMIT_CHUNK_CAPTURE,
            ogb_03_blocker,
        ),
        Entry::gap(
            "V110-INGEST-01",
            "ornadb-5il1.196",
            RUN_CAPTURE,
            run_cannot_capture,
        ),
        Entry::gap(
            "V110-INGEST-02",
            "ornadb-5il1.196",
            RUN_CAPTURE,
            run_cannot_capture,
        ),
        Entry::gap(
            "V110-MEDIA-01",
            "ornadb-vp8y",
            DECODED_MEDIA,
            media_is_not_decoded,
        ),
        Entry::gap(
            "V110-MEDIA-02",
            "ornadb-vp8y",
            DECODED_MEDIA,
            media_is_not_decoded,
        ),
        Entry::gap(
            "V110-KNOW-01",
            "ornadb-twfj",
            DECODED_MEDIA,
            media_is_not_decoded,
        ),
        Entry::gap(
            "V110-APP-01",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "V110-APP-02",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "V110-APP-04",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "V110-APP-05",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "V110-APP-06",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "V110-APP-07",
            "ornadb-5il1.142",
            SERVING_ENTRY,
            serve_ignores_the_serving_entry,
        ),
        Entry::gap(
            "V110-APP-08",
            "ornadb-5il1.144",
            LIVE_WATCH,
            watches_receive_no_update,
        ),
        Entry::gap(
            "V110-APP-09",
            "ornadb-5il1.144",
            LIVE_WATCH,
            watches_receive_no_update,
        ),
        Entry::gap("V110-UPGRADE-01", "ornadb-5il1.184", NO_UPGRADE, no_upgrade),
        Entry::gap("V110-UPGRADE-02", "ornadb-5il1.184", NO_UPGRADE, no_upgrade),
        Entry::gap("V110-UPGRADE-03", "ornadb-5il1.184", NO_UPGRADE, no_upgrade),
    ]
}

// Shared fixtures.

const MEDIA_SOURCE: &str = r#"pub table media(id: Str) {
    title: Str,
    content: Blob,
}

pub fn add(id: Str, title: Str, content: Blob) =
    media.insert({ id: id, title: title, content: content });
"#;

/// Bytes that occur nowhere else in a store.
fn distinctive_payload(length: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn media_database(db: &mut Db) -> Result<String, Failure> {
    let tables = db.declare(MEDIA_SOURCE)?;
    Db::relation(&tables, "media")
}

/// Writes `bytes` to `file` under the database root and inserts them as
/// media row `key` through the REPL, annotated when `annotation` is given.
fn capture_row(
    db: &mut Db,
    key: &str,
    file: &str,
    bytes: &[u8],
    annotation: Option<(&str, Option<&str>)>,
) -> Check {
    db.write(file, bytes)?;
    let root = db.root_text()?;
    let capture = format!(
        "let raw = sys.blob.capture_file({root:?}, {file:?}, {});",
        bytes.len() + 16
    );
    let annotate = match annotation {
        Some((media_type, Some(hint))) => {
            format!("let value = sys.blob.annotate(raw, {media_type:?}, {hint:?});")
        }
        Some((media_type, None)) => format!("let value = sys.blob.annotate(raw, {media_type:?});"),
        None => "let value = raw;".to_owned(),
    };
    let insert = format!("main.add({key:?}, {key:?}, value).id");
    let run = db.repl(&[&capture, &annotate, &insert])?;
    ensure!(
        run.succeeded() && !run.output().contains("error["),
        "the REPL did not insert the captured {file} as row {key}: {}",
        run.output()
    );
    Ok(())
}

fn listings(db: &mut Db, relation: &str) -> Result<Vec<serde_json::Value>, Failure> {
    let report = db.query_json(relation, &[])?;
    ensure!(
        report["media_payload_bytes_read"] == 0,
        "a metadata query read payload bytes: {report}"
    );
    report["listings"]
        .as_array()
        .cloned()
        .ok_or_else(|| Failure::Harness(format!("the query has no listings: {report}")))
}

fn listing<'a>(
    listings: &'a [serde_json::Value],
    key: &str,
) -> Result<&'a serde_json::Value, Failure> {
    listings
        .iter()
        .find(|listing| listing["key"] == key)
        .ok_or_else(|| Failure::Harness(format!("no listing for row {key}: {listings:?}")))
}

/// One entry of `git ls-tree -r -t -l` below `.orna/store`.
struct StoreEntry {
    kind: String,
    oid: String,
    size: Option<u64>,
    path: String,
}

impl StoreEntry {
    fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    fn parent(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(parent, _)| parent)
    }
}

fn store_entries(db: &mut Db, commit: &str) -> Result<Vec<StoreEntry>, Failure> {
    let listed = db
        .git(&[
            "ls-tree",
            "-r",
            "-t",
            "-l",
            "-z",
            "--full-tree",
            commit,
            "--",
            ".orna/store",
        ])?
        .ok()?;
    listed
        .raw_stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let record = String::from_utf8_lossy(record);
            let (header, path) = record
                .split_once('\t')
                .ok_or_else(|| Failure::Harness(format!("unparsable tree entry {record}")))?;
            let fields = header.split_whitespace().collect::<Vec<_>>();
            let [_mode, kind, oid, size] = fields.as_slice() else {
                return Err(Failure::Harness(format!("unparsable tree entry {record}")));
            };
            Ok(StoreEntry {
                kind: (*kind).to_owned(),
                oid: (*oid).to_owned(),
                size: size.parse().ok(),
                path: path.to_owned(),
            })
        })
        .collect()
}

fn object_bytes(db: &mut Db, oid: &str) -> Result<Vec<u8>, Failure> {
    Ok(db.git(&["cat-file", "blob", oid])?.ok()?.raw_stdout)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// The OVB-2 encoding of an annotated Blob: tag 60110 over `[bytes,
/// media_type, suffix-or-null]`.
fn ovb2(bytes: &[u8], media_type: &str, hint: Option<&str>) -> Vec<u8> {
    fn head(output: &mut Vec<u8>, major: u8, length: usize) {
        let major = major << 5;
        match length {
            0..=23 => output.push(major | length as u8),
            24..=0xff => output.extend_from_slice(&[major | 24, length as u8]),
            0x100..=0xffff => {
                output.push(major | 25);
                output.extend_from_slice(&(length as u16).to_be_bytes());
            }
            _ => {
                output.push(major | 26);
                output.extend_from_slice(&(length as u32).to_be_bytes());
            }
        }
    }
    let mut output = vec![0xd9, 0xea, 0xce, 0x83];
    head(&mut output, 2, bytes.len());
    output.extend_from_slice(bytes);
    head(&mut output, 3, media_type.len());
    output.extend_from_slice(media_type.as_bytes());
    match hint {
        Some(hint) => {
            head(&mut output, 3, hint.len());
            output.extend_from_slice(hint.as_bytes());
        }
        None => output.push(0xf6),
    }
    output
}

/// Checks the encoder above against the release golden OVB-2 values.
fn golden_ovb2_values_match() -> Check {
    let values = FINAL_VECTORS["values"]
        .as_array()
        .ok_or_else(|| Failure::Harness("the release vectors list no values".to_owned()))?;
    for (id, bytes, media_type) in [
        ("ovb2-empty", &b""[..], "application/octet-stream"),
        ("ovb2-text", &b"abc"[..], "text/plain;charset=utf-8"),
    ] {
        let golden = values
            .iter()
            .find(|value| value["id"] == id)
            .and_then(|value| value["canonical_hex"].as_str())
            .ok_or_else(|| Failure::Harness(format!("the release vectors have no {id}")))?;
        harness_ensure!(
            hex(&ovb2(bytes, media_type, None)) == golden,
            "the OVB-2 reference encoding disagrees with release vector {id}"
        );
    }
    Ok(())
}

/// `orna annotate SOURCE --type TYPE [--suffix HINT] --output OUTPUT`.
fn annotate(
    db: &mut Db,
    source: &str,
    media_type: &str,
    hint: Option<&str>,
    output: &str,
) -> Result<crate::harness::Run, Failure> {
    let mut arguments = vec!["annotate", source, "--type", media_type];
    if let Some(hint) = hint {
        arguments.extend_from_slice(&["--suffix", hint]);
    }
    arguments.extend_from_slice(&["--output", output]);
    db.orna(&arguments)
}

/// Annotates `payload` and requires the exact OVB-2 bytes for the expected
/// canonical annotation.
fn annotates_to(
    db: &mut Db,
    payload: &[u8],
    (media_type, hint): (&str, Option<&str>),
    (canonical, canonical_hint): (&str, Option<&str>),
) -> Check {
    db.write("payload.bin", payload)?;
    let _ = fs::remove_file(db.root().join("annotated.ovb2"));
    annotate(db, "payload.bin", media_type, hint, "annotated.ovb2")?.ok()?;
    ensure!(
        db.read("annotated.ovb2")? == ovb2(payload, canonical, canonical_hint),
        "annotating as {media_type:?} with hint {hint:?} did not produce the canonical {canonical:?} with hint {canonical_hint:?}"
    );
    Ok(())
}

/// Requires `orna annotate` to refuse the annotation and write nothing.
fn annotation_refused(db: &mut Db, media_type: &str, hint: Option<&str>) -> Check {
    db.write("payload.bin", b"payload")?;
    let _ = fs::remove_file(db.root().join("refused.ovb2"));
    let refused = annotate(db, "payload.bin", media_type, hint, "refused.ovb2")?;
    ensure!(
        !refused.succeeded(),
        "annotating as {media_type:?} with hint {hint:?} was accepted"
    );
    ensure!(
        !db.root().join("refused.ovb2").exists(),
        "a refused annotation left an output file"
    );
    Ok(())
}

fn media_vectors() -> Result<Vec<serde_json::Value>, Failure> {
    FINAL_VECTORS["media"]
        .as_array()
        .cloned()
        .ok_or_else(|| Failure::Harness("the release vectors list no media cases".to_owned()))
}

fn text<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str, Failure> {
    value[field]
        .as_str()
        .ok_or_else(|| Failure::Harness(format!("{field} is not text in {value}")))
}

// FINAL-MIME and FINAL-BLOB-001 executors over `orna annotate`.

/// FINAL-MIME-002: names, essences and charsets are lowercased, parameters
/// sorted and other values kept; duplicates, controls, non-ASCII and broken
/// quoting are refused.
fn media_types_normalize(db: &mut Db) -> Check {
    golden_ovb2_values_match()?;
    for vector in media_vectors()? {
        annotates_to(
            db,
            b"abc",
            (text(&vector, "input")?, vector["hint"].as_str()),
            (
                text(&vector, "canonical_type")?,
                vector["canonical_hint"].as_str(),
            ),
        )?;
    }
    for (input, canonical) in [
        (
            "Text/Plain; Format=Flowed; Charset=UTF-8",
            "text/plain;charset=utf-8;format=Flowed",
        ),
        ("text/plain;charset=\"utf-8\"", "text/plain;charset=utf-8"),
        ("text/plain;title=\"a b\"", "text/plain;title=\"a b\""),
    ] {
        annotates_to(db, b"abc", (input, None), (canonical, None))?;
    }
    for input in [
        "text/plain;charset=utf-8;charset=utf-8",
        "text/plain;x=\"unterminated",
        "text/plain;x=\u{1}",
        "text/pl\u{e9}in",
    ] {
        annotation_refused(db, input, None)?;
    }
    Ok(())
}

/// FINAL-MIME-004: hints are lowercase safe suffix segments compatible
/// with a known type; unsafe, oversized or incompatible hints are refused.
fn suffix_hints_are_validated(db: &mut Db) -> Check {
    for vector in media_vectors()? {
        if vector["hint"].is_string() {
            annotates_to(
                db,
                b"abc",
                (text(&vector, "input")?, vector["hint"].as_str()),
                (
                    text(&vector, "canonical_type")?,
                    vector["canonical_hint"].as_str(),
                ),
            )?;
        }
    }
    let long = "x".repeat(33);
    for (media_type, hint) in [
        ("image/png", "jpg"),
        ("image/png", "../x"),
        ("image/png", ".png"),
        ("application/x-example", "a..b"),
        ("application/x-example", "a/b"),
        ("application/x-example", "a\\b"),
        ("application/x-example", long.as_str()),
    ] {
        annotation_refused(db, media_type, Some(hint))?;
    }
    Ok(())
}

/// FINAL-MIME-005: the preferred suffix as a hint, and spelling, case and
/// quoting variants, all produce one annotated value.
fn defaults_canonicalize_to_one_value(db: &mut Db) -> Check {
    for hint in ["png", "PNG"] {
        annotates_to(db, b"abc", ("image/png", Some(hint)), ("image/png", None))?;
    }
    let mut outputs = BTreeSet::new();
    for (index, input) in ["image/png", "IMAGE/PNG", "Image/Png"].iter().enumerate() {
        let output = format!("variant-{index}.ovb2");
        db.write("payload.bin", b"abc")?;
        annotate(db, "payload.bin", input, None, &output)?.ok()?;
        outputs.insert(db.read(&output)?);
    }
    ensure!(
        outputs.len() == 1,
        "case variants produced different values"
    );
    let mut outputs = BTreeSet::new();
    for (index, input) in [
        "text/plain;charset=utf-8",
        "text/plain;charset=\"UTF-8\"",
        "TEXT/PLAIN; CHARSET=utf-8",
    ]
    .iter()
    .enumerate()
    {
        let output = format!("quoted-{index}.ovb2");
        annotate(db, "payload.bin", input, None, &output)?.ok()?;
        outputs.insert(db.read(&output)?);
    }
    ensure!(
        outputs.len() == 1,
        "quoting variants produced different values"
    );
    Ok(())
}

/// FINAL-BLOB-001: an annotated Blob is the exact octets plus a canonical
/// media type and optional hint; annotating the same bytes differently
/// never changes them.
fn annotation_never_alters_raw_bytes(db: &mut Db) -> Check {
    golden_ovb2_values_match()?;
    annotates_to(
        db,
        b"",
        ("application/octet-stream", None),
        ("application/octet-stream", None),
    )?;
    annotates_to(
        db,
        b"abc",
        ("text/plain;charset=utf-8", None),
        ("text/plain;charset=utf-8", None),
    )?;
    let payload = distinctive_payload(4096);
    for (media_type, hint) in [
        ("application/octet-stream", None),
        ("image/png", None),
        ("application/x-example", Some("xyz")),
    ] {
        annotates_to(db, &payload, (media_type, hint), (media_type, hint))?;
    }
    annotation_refused(db, "*/*", None)?;
    annotation_refused(db, "", None)
}

/// FINAL-MIME-001: a Blob captured without knowledge of its type is
/// application/octet-stream with no hint; wildcard and empty types are
/// refused.
fn unknown_content_is_octet_stream(db: &mut Db) -> Check {
    let relation = media_database(db)?;
    capture_row(
        db,
        "unknown",
        "unknown.bin",
        b"\x89PNG\r\n\x1a\nmaybe",
        None,
    )?;
    db.publish()?.ok()?;
    let listings = listings(db, &relation)?;
    let row = listing(&listings, "unknown")?;
    ensure!(
        row["media_type"] == "application/octet-stream" && row["suffix"].is_null(),
        "captured content without a declared type is {row}"
    );
    for media_type in ["*/*", "image/*", ""] {
        annotation_refused(db, media_type, None)?;
    }
    Ok(())
}

// Storage executors on product-made rows.

/// FINAL-ROW-004: a Blob field stores a small reference; its bytes live in
/// their own content graph, never in row pages, and a metadata query reads
/// none of them.
fn blob_rows_hold_references(db: &mut Db) -> Check {
    let relation = media_database(db)?;
    let payload = distinctive_payload(8192);
    capture_row(db, "song", "song.bin", &payload, Some(("audio/mpeg", None)))?;
    db.publish()?.ok()?;
    let row = listings(db, &relation)?;
    let row = listing(&row, "song")?;
    ensure!(
        row["length"] == payload.len() && row["sha256"] == hex(&Sha256::digest(&payload)),
        "the listing does not describe the captured bytes: {row}"
    );
    let window = &payload[1000..1064];
    let mut chunks = 0;
    for entry in store_entries(db, "HEAD")? {
        if entry.kind != "blob" {
            continue;
        }
        let bytes = object_bytes(db, &entry.oid)?;
        if entry.name() == "data" {
            ensure!(
                !contains(&bytes, window),
                "node {} embeds payload bytes",
                entry.path
            );
        } else if contains(&bytes, window) {
            chunks += 1;
        }
    }
    ensure!(chunks > 0, "no terminal chunk holds the payload");
    Ok(())
}

/// FINAL-OGB-007: a small Blob is still a reference, with one terminal chunk.
fn small_blobs_use_one_terminal_chunk(db: &mut Db) -> Check {
    media_database(db)?;
    capture_row(db, "tiny", "tiny.bin", b"abc", None)?;
    db.publish()?.ok()?;
    let mut terminal = Vec::new();
    for entry in store_entries(db, "HEAD")? {
        if entry.kind == "blob" && entry.name() != "data" {
            terminal.push(object_bytes(db, &entry.oid)?);
        }
    }
    ensure!(
        terminal
            .iter()
            .filter(|bytes| bytes.as_slice() == b"abc")
            .count()
            == 1,
        "the three-byte Blob is not exactly one terminal chunk"
    );
    Ok(())
}

/// FINAL-BLOB-004: identity is the length and raw-byte SHA-256, the same for
/// every annotation of the same bytes.
fn content_identity_ignores_annotations(db: &mut Db) -> Check {
    shared_content_rows(db).map(|_| ())
}

/// Two rows of the same bytes, one annotated as PNG and one left as octets,
/// published and checked; returns the relation and their shared descriptor.
fn shared_content_rows(db: &mut Db) -> Result<(String, String), Failure> {
    let relation = media_database(db)?;
    let payload = distinctive_payload(2048);
    capture_row(db, "as-png", "a.bin", &payload, Some(("image/png", None)))?;
    capture_row(db, "as-octets", "b.bin", &payload, None)?;
    db.publish()?.ok()?;
    let listings = listings(db, &relation)?;
    let (png, octets) = (
        listing(&listings, "as-png")?,
        listing(&listings, "as-octets")?,
    );
    let digest = hex(&Sha256::digest(&payload));
    for row in [png, octets] {
        ensure!(
            row["length"] == payload.len() && row["sha256"] == digest.as_str(),
            "content identity is not (length, SHA-256): {row}"
        );
    }
    ensure!(
        png["descriptor"] == octets["descriptor"],
        "one content has two descriptors: {png} {octets}"
    );
    ensure!(
        png["media_type"] == "image/png" && octets["media_type"] == "application/octet-stream",
        "annotations did not stay with their rows: {png} {octets}"
    );
    let descriptor = png["descriptor"]
        .as_str()
        .ok_or_else(|| Failure::Harness(format!("no descriptor in {png}")))?
        .to_owned();
    Ok((relation, descriptor))
}

/// FINAL-OGB-001: the descriptor of `abc` is the release's golden native
/// graph, and each referencing row keeps its own media type.
fn descriptors_match_the_release_graph(db: &mut Db) -> Check {
    let graph = &FINAL_VECTORS["native_graph"];
    ensure!(
        graph["algorithm"] == "sha1",
        "the golden graph is not SHA-1"
    );
    let relation = media_database(db)?;
    capture_row(db, "text", "a.bin", b"abc", Some(("text/plain", None)))?;
    capture_row(db, "bytes", "b.bin", b"abc", None)?;
    db.publish()?.ok()?;
    let listings = listings(db, &relation)?;
    for key in ["text", "bytes"] {
        let row = listing(&listings, key)?;
        ensure!(
            row["descriptor"] == graph["descriptor_oid"]
                && row["length"] == graph["blob_length"]
                && row["sha256"] == graph["blob_sha256"],
            "row {key} does not bind the release descriptor: {row}"
        );
    }
    ensure!(
        listing(&listings, "text")?["media_type"] == "text/plain",
        "the text row lost its media type"
    );
    Ok(())
}

/// FINAL-OGB-003: empty content has one canonical descriptor; a corrupted
/// descriptor is refused.
fn empty_content_has_one_descriptor(db: &mut Db) -> Check {
    let relation = media_database(db)?;
    capture_row(db, "first", "first.bin", b"", None)?;
    capture_row(db, "second", "second.bin", b"", Some(("text/plain", None)))?;
    db.publish()?.ok()?;
    let listings = listings(db, &relation)?;
    let (first, second) = (listing(&listings, "first")?, listing(&listings, "second")?);
    ensure!(
        first["length"] == 0
            && first["sha256"] == hex(&Sha256::digest(b""))
            && first["descriptor"] == second["descriptor"],
        "empty content has no single canonical descriptor: {first} {second}"
    );
    let descriptor = first["descriptor"]
        .as_str()
        .ok_or_else(|| Failure::Harness(format!("no descriptor in {first}")))?
        .to_owned();
    replace_node_data(db, &descriptor, b"\xa0")?;
    refused_reading(db, &relation)
}

fn relation_of(tables: &BTreeMap<String, String>, table: &str) -> Result<String, Failure> {
    Db::relation(tables, table)
}

/// FINAL-ROW-010: repacking the object store changes no logical row.
fn repacking_changes_no_logical_rows(db: &mut Db) -> Check {
    let tables = db.declare("pub table Note(id: Str) { text: Str, }\n")?;
    let relation = relation_of(&tables, "Note")?;
    db.repl_effect(r#"main.Note.insert({ id: "kept", text: "unchanged by repacking" })"#)?;
    db.publish()?.ok()?;
    let before = db.query_json(&relation, &[])?;
    let head = db.git_text(&["rev-parse", "HEAD"])?;
    db.git(&["repack", "-a", "-d", "-f", "-q"])?.ok()?;
    db.git(&["prune-packed"])?.ok()?;
    ensure!(
        db.query_json(&relation, &[])? == before,
        "the relation reads differently after repacking"
    );
    ensure!(
        db.runtime_mutations()? == 0,
        "repacking produced runtime mutations"
    );
    ensure!(
        db.git_text(&["status", "--porcelain"])?.is_empty(),
        "repacking left worktree or index changes"
    );
    ensure!(
        db.git_text(&["rev-parse", "HEAD"])? == head,
        "repacking moved the branch"
    );
    let diff = db
        .orna(&["history", &relation, "--diff", "HEAD", "HEAD"])?
        .ok()?;
    ensure!(
        diff.stdout.contains("0 added; 0 changed; 0 removed"),
        "the semantic diff of the repacked snapshot is not empty: {}",
        diff.output()
    );
    Ok(())
}

/// FINAL-GRAPH-001: every store node is a Git tree of one bounded `data`
/// blob and an optional `refs` tree whose entries are named by their own
/// object IDs; a node with non-canonical data is refused.
fn nodes_are_data_and_refs_trees(db: &mut Db) -> Check {
    let relation = media_database(db)?;
    capture_row(
        db,
        "song",
        "song.bin",
        &distinctive_payload(4096),
        Some(("audio/mpeg", None)),
    )?;
    db.publish()?.ok()?;
    let entries = store_entries(db, "HEAD")?;
    check_native_graph(&entries)?;
    let root = db
        .git_text(&["rev-parse", "HEAD:.orna/store"])?
        .trim()
        .to_owned();
    replace_node_data(db, &root, b"\x83\x01\x00\x00")?;
    refused_reading(db, &relation)
}

fn check_native_graph(entries: &[StoreEntry]) -> Check {
    let mut children = BTreeMap::<&str, Vec<&StoreEntry>>::new();
    for entry in entries {
        children.entry(entry.parent()).or_default().push(entry);
    }
    let nodes = std::iter::once(".orna/store").chain(
        entries
            .iter()
            .filter(|entry| entry.kind == "tree" && entry.parent().ends_with("/refs"))
            .map(|entry| entry.path.as_str()),
    );
    let mut count = 0;
    for node in nodes {
        count += 1;
        let kids = children.get(node).map(Vec::as_slice).unwrap_or_default();
        let names = kids.iter().map(|kid| kid.name()).collect::<Vec<_>>();
        ensure!(
            names == ["data"] || names == ["data", "refs"],
            "node {node} has entries {names:?}"
        );
        for kid in kids {
            match kid.name() {
                "data" => ensure!(
                    kid.kind == "blob" && kid.size.is_some_and(|size| size <= 64 * 1024),
                    "node {node} data is not a bounded blob"
                ),
                _ => {
                    ensure!(kid.kind == "tree", "node {node} refs is not a tree");
                    for edge in children
                        .get(kid.path.as_str())
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                    {
                        ensure!(
                            edge.name() == edge.oid
                                && matches!(edge.kind.as_str(), "tree" | "blob"),
                            "edge {} is not named by its {} object ID",
                            edge.path,
                            edge.kind
                        );
                    }
                }
            }
        }
    }
    ensure!(count > 1, "the store has no child nodes");
    Ok(())
}

/// FINAL-GRAPH-003: terminal chunks are raw Git blobs in the repository's
/// own object store.
fn terminal_chunks_are_raw_blobs(db: &mut Db) -> Check {
    media_database(db)?;
    let payload = distinctive_payload(3000);
    capture_row(
        db,
        "image",
        "image.bin",
        &payload,
        Some(("image/png", None)),
    )?;
    db.publish()?.ok()?;
    let mut found = false;
    for entry in store_entries(db, "HEAD")? {
        if entry.kind == "blob" && entry.name() != "data" {
            let kind = db.git_text(&["cat-file", "-t", &entry.oid])?;
            ensure!(kind.trim() == "blob", "chunk {} is a {kind}", entry.oid);
            found |= object_bytes(db, &entry.oid)? == payload;
        }
    }
    ensure!(found, "no raw blob holds the captured bytes");
    Ok(())
}

/// FINAL-GRAPH-006: a SHA-256 repository keeps 64-digit native object IDs
/// throughout the store, and content identity is still raw-byte SHA-256.
fn sha256_repositories_keep_native_widths(db: &mut Db) -> Check {
    let mut sha = Db::init_with_object_format("sha256")?;
    let checked = (|| {
        let relation = media_database(&mut sha)?;
        let payload = distinctive_payload(1024);
        capture_row(&mut sha, "song", "song.bin", &payload, None)?;
        sha.publish()?.ok()?;
        let entries = store_entries(&mut sha, "HEAD")?;
        ensure!(
            !entries.is_empty() && entries.iter().all(|entry| entry.oid.len() == 64),
            "a store object ID is not 64 digits"
        );
        check_native_graph(&entries)?;
        let listings = listings(&mut sha, &relation)?;
        let row = listing(&listings, "song")?;
        ensure!(
            row["descriptor"]
                .as_str()
                .is_some_and(|oid| oid.len() == 64)
                && row["sha256"] == hex(&Sha256::digest(&payload)),
            "the SHA-256 repository listing is {row}"
        );
        Ok(())
    })();
    checked.map_err(|failure| {
        failure.map_reason(|reason| format!("{reason}\n{}", sha.transcript()))
    })?;
    // The default database is SHA-1 with 40-digit IDs.
    ensure!(
        db.git_text(&["rev-parse", "--show-object-format"])?.trim() == "sha1",
        "the default database is not SHA-1"
    );
    Ok(())
}

/// FINAL-GRAPH-007: a later store root replaces the nodes its snapshot no
/// longer needs; the earlier snapshot stays readable through history.
fn store_roots_hold_only_the_snapshot(db: &mut Db) -> Check {
    let tables = db.declare("pub table Note(id: Str) { text: Str, }\n")?;
    let relation = relation_of(&tables, "Note")?;
    db.repl_effect(r#"main.Note.insert({ id: "first", text: "one" })"#)?;
    db.publish()?.ok()?;
    let first = store_entries(db, "HEAD")?
        .into_iter()
        .filter(|entry| entry.kind == "tree")
        .map(|entry| entry.oid)
        .collect::<BTreeSet<_>>();
    db.repl_effect(r#"main.Note.insert({ id: "second", text: "two" })"#)?;
    db.publish()?.ok()?;
    let second = store_entries(db, "HEAD")?
        .into_iter()
        .map(|entry| entry.oid)
        .collect::<BTreeSet<_>>();
    ensure!(
        first.iter().any(|oid| !second.contains(oid)),
        "the new store root still carries every node of the previous snapshot"
    );
    ensure!(
        db.query_json(&relation, &["--at", "HEAD~1"])?["rows"] == 1
            && db.query_json(&relation, &[])?["rows"] == 2,
        "history and the current snapshot disagree on the row counts"
    );
    Ok(())
}

/// FINAL-GRAPH-005: one content graph shared by two rows keeps each row's
/// own annotation, and a corrupted shared node is refused for every reader.
fn shared_children_keep_per_parent_annotations(db: &mut Db) -> Check {
    let (relation, descriptor) = shared_content_rows(db)?;
    replace_node_data(db, &descriptor, b"\xa0")?;
    refused_reading(db, &relation)
}

/// Replaces the `data` blob of the store node whose tree is `node` with
/// `bytes`, committing the edited store with plain Git.
fn replace_node_data(db: &mut Db, node: &str, bytes: &[u8]) -> Check {
    let entries = store_entries(db, "HEAD")?;
    let path = if db.git_text(&["rev-parse", "HEAD:.orna/store"])?.trim() == node {
        ".orna/store/data".to_owned()
    } else {
        let node_path = entries
            .iter()
            .find(|entry| entry.kind == "tree" && entry.oid == node)
            .map(|entry| entry.path.clone())
            .ok_or_else(|| Failure::Harness(format!("node {node} is not in the committed store")))?;
        format!("{node_path}/data")
    };
    db.write("tampered.bin", bytes)?;
    let blob = db
        .git_text(&["hash-object", "-w", "tampered.bin"])?
        .trim()
        .to_owned();
    db.git(&[
        "update-index",
        "--cacheinfo",
        &format!("100644,{blob},{path}"),
    ])?
    .ok()?;
    db.git(&["commit", "-q", "-m", "Edit a store node with plain Git"])?
        .ok()?;
    Ok(())
}

/// The blocker reproduces when the scenario's executor fails only because
/// `orna export` copies a store that the product readers refuse.
fn export_copies_refused_store(check: Check) -> Probe {
    match check {
        Ok(()) => Err(Failure::Unmet(
            "orna export now refuses the tampered store".to_owned(),
        )),
        Err(failure)
            if failure.reason().contains("copied a corrupted store")
                || failure.reason().contains("copied a content graph") =>
        {
            Ok(failure.reason().lines().next().unwrap_or_default().to_owned())
        }
        Err(failure) => Err(failure),
    }
}

macro_rules! export_blocker {
    ($($probe:ident => $executor:ident),+ $(,)?) => {
        $(
            fn $probe(db: &mut Db) -> Probe {
                export_copies_refused_store($executor(db))
            }
        )+
    };
}

export_blocker! {
    row_012_blocker => external_store_edits_are_not_used,
    row_015_blocker => tampered_graph_edges_are_refused,
    graph_001_blocker => nodes_are_data_and_refs_trees,
    graph_002_blocker => tampered_graph_edges_are_refused,
    graph_005_blocker => shared_children_keep_per_parent_annotations,
    ogb_002_blocker => corrupted_chunks_fail_verification,
    ogb_003_blocker => empty_content_has_one_descriptor,
}

const EXPORT_COPIES_TAMPERED_STORE: &str =
    "orna export copies a tampered store that every other product reader refuses";

const REPL_EFFECT_UNADMITTED: &str = "the REPL preview refuses the effectful row insert, and scenarios have no admitted runtime entry point to perform it";
const SIZE_LIMIT_CHUNK_CAPTURE: &str =
    "a 3 MiB REPL capture fails with sys.blob.limit before the row is inserted";

/// The blocker reproduces when the scenario's executor stops at `needle`,
/// the product refusal that blocks the scenario, before its own assertions.
fn executor_stops_at(check: Check, needle: &str) -> Probe {
    match check {
        Ok(()) => Err(Failure::Unmet(format!(
            "the scenario now passes; `{needle}` no longer blocks it"
        ))),
        Err(failure) if failure.reason().contains(needle) => {
            Ok(failure.reason().lines().next().unwrap_or_default().to_owned())
        }
        Err(failure) => Err(failure),
    }
}

fn row_016_blocker(db: &mut Db) -> Probe {
    executor_stops_at(row_names_escape_reversibly(db), "ORNA-REPL-EFFECT")
}

fn ogb_03_blocker(db: &mut Db) -> Probe {
    executor_stops_at(chunking_is_stable_and_reuses_chunks(db), "sys.blob.limit")
}

/// Every product reader refuses the committed store.
fn refused_reading(db: &mut Db, relation: &str) -> Check {
    let query = db.orna(&["query", relation])?;
    ensure!(
        !query.succeeded(),
        "orna query read a corrupted store: {}",
        query.output()
    );
    let export = db.orna(&["export", "../refused-export", "--at", "HEAD"])?;
    ensure!(
        !export.succeeded(),
        "orna export copied a corrupted store: {}",
        export.output()
    );
    Ok(())
}

/// FINAL-ROW-015, FINAL-GRAPH-002: dependency edges must match the decoded
/// inventory; an unused, renamed or missing edge is refused.
fn tampered_graph_edges_are_refused(db: &mut Db) -> Check {
    let relation = media_database(db)?;
    capture_row(
        db,
        "song",
        "song.bin",
        &distinctive_payload(2048),
        Some(("audio/mpeg", None)),
    )?;
    db.publish()?.ok()?;
    let good = db.git_text(&["rev-parse", "HEAD"])?.trim().to_owned();
    let entries = store_entries(db, "HEAD")?;
    let root_edge = entries
        .iter()
        .find(|entry| entry.kind == "tree" && entry.parent() == ".orna/store/refs")
        .ok_or_else(|| Failure::Harness("the store root has no child node".to_owned()))?;
    let chunk = entries
        .iter()
        .find(|entry| entry.kind == "blob" && entry.name() != "data")
        .ok_or_else(|| Failure::Harness("the store has no terminal chunk".to_owned()))?;
    let unused = format!("100644,{},.orna/store/refs/{}", chunk.oid, chunk.oid);
    let variants: [(&str, Vec<Vec<String>>); 2] = [
        (
            "an unused edge",
            vec![vec![
                "update-index".into(),
                "--add".into(),
                "--cacheinfo".into(),
                unused,
            ]],
        ),
        (
            "a missing edge",
            vec![vec![
                "rm".into(),
                "--cached".into(),
                "-r".into(),
                "-q".into(),
                root_edge.path.clone(),
            ]],
        ),
    ];
    for (label, commands) in variants {
        for command in &commands {
            let arguments = command.iter().map(String::as_str).collect::<Vec<_>>();
            db.git(&arguments)?.ok()?;
        }
        db.git(&["commit", "-q", "-m", label])?.ok()?;
        refused_reading(db, &relation)
            .map_err(|failure| failure.map_reason(|reason| format!("{label}: {reason}")))?;
        db.git(&["reset", "-q", "--hard", &good])?.ok()?;
    }
    ensure!(
        db.query_json(&relation, &[])?["rows"] == 1,
        "the restored store no longer reads"
    );
    Ok(())
}

/// FINAL-ROW-012: the views never expose raw store nodes, and a store edited
/// outside Orna is refused rather than used.
fn external_store_edits_are_not_used(db: &mut Db) -> Check {
    let relation = media_database(db)?;
    capture_row(db, "song", "song.bin", b"song bytes", None)?;
    db.publish()?.ok()?;
    let root = db
        .git_text(&["rev-parse", "HEAD:.orna/store"])?
        .trim()
        .to_owned();
    replace_node_data(db, &root, b"garbage")?;
    refused_reading(db, &relation)
}

/// FINAL-OGB-002: full verification compares every chunk: a chunk whose
/// bytes no longer match the content graph is refused by `orna export`,
/// while a metadata-only query, which reads no chunk, still answers.
fn corrupted_chunks_fail_verification(db: &mut Db) -> Check {
    let relation = media_database(db)?;
    capture_row(db, "song", "song.bin", &distinctive_payload(2048), None)?;
    db.publish()?.ok()?;
    db.orna(&["export", "../good-copy", "--at", "HEAD"])?.ok()?;
    db.orna(&["export", "../good-copy", "--check"])?.ok()?;
    let chunk = store_entries(db, "HEAD")?
        .into_iter()
        .find(|entry| entry.kind == "blob" && entry.name() != "data")
        .ok_or_else(|| Failure::Harness("the store has no terminal chunk".to_owned()))?;
    let path = chunk.path.clone();
    let mut forged = distinctive_payload(2048);
    forged[0] ^= 0xff;
    db.write("forged.bin", &forged)?;
    let blob = db
        .git_text(&["hash-object", "-w", "forged.bin"])?
        .trim()
        .to_owned();
    let parent = chunk.parent().to_owned();
    db.git(&["rm", "--cached", "--sparse", "-q", &path])?.ok()?;
    db.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{blob},{parent}/{blob}"),
    ])?
    .ok()?;
    db.git(&["commit", "-q", "-m", "Replace a chunk with plain Git"])?
        .ok()?;
    let export = db.orna(&["export", "../forged-copy", "--at", "HEAD"])?;
    ensure!(
        !export.succeeded(),
        "orna export copied a content graph with a forged chunk: {}",
        export.output()
    );
    metadata_query_reads_no_payload(db, &relation)
}

/// A metadata-only query of a damaged content graph may refuse, but if it
/// answers it reads no payload and lists no row as empty content.
fn metadata_query_reads_no_payload(db: &mut Db, relation: &str) -> Check {
    let query = db.orna(&["query", relation, "--format", "json"])?;
    if !query.succeeded() {
        return Ok(());
    }
    let report: serde_json::Value = serde_json::from_str(&query.stdout)
        .map_err(|error| Failure::Harness(format!("orna query printed no JSON ({error})")))?;
    ensure!(
        report["media_payload_bytes_read"] == 0,
        "the metadata query read payload bytes: {report}"
    );
    ensure!(
        report["listings"]
            .as_array()
            .is_none_or(|rows| rows.iter().all(|row| row["length"] != 0)),
        "a damaged content graph was listed as empty content: {report}"
    );
    Ok(())
}

/// FINAL-OGB-006: a chunk missing from the object store is reported as
/// unavailable or corrupt, never read as empty content.
fn missing_chunks_are_not_empty_content(db: &mut Db) -> Check {
    let relation = media_database(db)?;
    capture_row(db, "song", "song.bin", &distinctive_payload(4096), None)?;
    db.publish()?.ok()?;
    let chunk = store_entries(db, "HEAD")?
        .into_iter()
        .find(|entry| entry.kind == "blob" && entry.name() != "data")
        .ok_or_else(|| Failure::Harness("the store has no terminal chunk".to_owned()))?;
    let objects = db.git_text(&["rev-parse", "--git-path", "objects"])?;
    let object = db
        .root()
        .join(objects.trim())
        .join(&chunk.oid[..2])
        .join(&chunk.oid[2..]);
    fs::remove_file(&object)
        .map_err(|error| {
            Failure::Harness(format!("remove chunk object {}: {error}", object.display()))
        })?;
    let export = db.orna(&["export", "../missing-copy", "--at", "HEAD"])?;
    ensure!(
        !export.succeeded(),
        "orna export copied a store with a missing chunk: {}",
        export.output()
    );
    metadata_query_reads_no_payload(db, &relation)
}

/// V110-OGB-03: capturing the same bytes twice gives one content graph, and
/// an insertion reuses most of the unchanged chunks.
fn chunking_is_stable_and_reuses_chunks(db: &mut Db) -> Check {
    let relation = media_database(db)?;
    let original = distinctive_payload(3 << 20);
    let mut edited = original.clone();
    edited.splice(1 << 20..1 << 20, *b"inserted bytes");
    capture_row(db, "first", "first.bin", &original, None)?;
    capture_row(db, "again", "again.bin", &original, None)?;
    capture_row(db, "edited", "edited.bin", &edited, None)?;
    db.publish()?.ok()?;
    let listings = listings(db, &relation)?;
    ensure!(
        listing(&listings, "first")?["descriptor"] == listing(&listings, "again")?["descriptor"],
        "the same bytes captured twice produced two content graphs"
    );
    let chunks = |db: &mut Db, descriptor: &str| -> Result<BTreeSet<String>, Failure> {
        let tree = db.git_text(&["ls-tree", "-r", "--name-only", descriptor])?;
        Ok(tree
            .lines()
            .filter_map(|path| path.rsplit('/').next())
            .filter(|name| *name != "data")
            .map(str::to_owned)
            .collect())
    };
    let first = chunks(
        db,
        listing(&listings, "first")?["descriptor"]
            .as_str()
            .unwrap_or_default(),
    )?;
    let edited = chunks(
        db,
        listing(&listings, "edited")?["descriptor"]
            .as_str()
            .unwrap_or_default(),
    )?;
    ensure!(
        first.len() > 4,
        "3 MiB of content is not chunked: {} chunks",
        first.len()
    );
    let shared = first.intersection(&edited).count();
    ensure!(
        shared * 2 > first.len(),
        "an insertion reused only {shared} of {} chunks",
        first.len()
    );
    Ok(())
}

// VFS executors (FUSE).

/// FINAL-MIME-003: a projected content file takes the preferred suffix of
/// its media type, an explicit compatible hint, or `bin` for unknown types.
fn projected_suffixes_follow_the_registry(db: &mut Db) -> Check {
    media_database(db)?;
    let vectors = media_vectors()?;
    for (index, vector) in vectors.iter().enumerate() {
        capture_row(
            db,
            &format!("v{index}"),
            &format!("v{index}.bin"),
            b"abc",
            Some((text(vector, "input")?, vector["hint"].as_str())),
        )?;
    }
    capture_row(
        db,
        "unknown",
        "unknown.bin",
        b"abc",
        Some(("application/x-unregistered-scenario", None)),
    )?;
    db.publish()?.ok()?;
    let mount = db.mount("snapshot", &["--at", "HEAD"])?;
    for (index, vector) in vectors.iter().enumerate() {
        let file = mount.point.join(format!(
            "media/v{index}/content.{}",
            text(vector, "suffix")?
        ));
        ensure!(
            file.is_file(),
            "row v{index} does not project {}",
            file.display()
        );
    }
    let unknown = mount.point.join("media/unknown/content.bin");
    ensure!(
        unknown.is_file(),
        "an unknown type does not project content.bin"
    );
    Ok(())
}

/// FINAL-VFS-001: a row directory is addressed by table and typed key, and
/// `data.orna` holds the stored non-key fields only.
fn row_directories_project_non_key_fields(db: &mut Db) -> Check {
    let tables = db.declare(&format!(
        "{MEDIA_SOURCE}\npub table Counter(id: Int) {{ label: Str, }}\n"
    ))?;
    relation_of(&tables, "Counter")?;
    capture_row(
        db,
        "cover",
        "cover.bin",
        b"cover bytes",
        Some(("image/png", None)),
    )?;
    db.repl_effect(r#"main.Counter.insert({ id: 7, label: "seven" })"#)?;
    db.publish()?.ok()?;
    let mount = db.mount("snapshot", &["--at", "HEAD"])?;
    let read = |relative: &str| {
        fs::read_to_string(mount.point.join(relative))
            .map_err(|error| Failure::Harness(format!("read {relative}: {error}")))
    };
    let cover = read("media/cover/data.orna")?;
    ensure!(
        cover.contains("title: \"cover\"")
            && cover.contains("media_type: \"image/png\"")
            && !cover.contains("id:"),
        "media/cover/data.orna is not the stored non-key fields: {cover}"
    );
    let counter = read("Counter/7/data.orna")?;
    ensure!(
        counter.contains("label: \"seven\"") && !counter.contains("id:"),
        "Counter/7/data.orna is not the stored non-key fields: {counter}"
    );
    ensure!(
        !mount.point.join("media/missing").exists(),
        "a missing key has a row directory"
    );
    Ok(())
}

/// FINAL-VFS-016: row directory names escape reserved and colliding keys as
/// the release path vectors say.
fn row_names_escape_reversibly(db: &mut Db) -> Check {
    let tables = db.declare("pub table Item(id: Str) { n: Int, }\n")?;
    relation_of(&tables, "Item")?;
    let vectors = FINAL_VECTORS["paths"]
        .as_array()
        .cloned()
        .ok_or_else(|| Failure::Harness("the release vectors list no paths".to_owned()))?;
    let mut expected = BTreeSet::new();
    for vector in &vectors {
        let key = text(vector, "input")?;
        let literal = key.replace('\\', "\\\\").replace('"', "\\\"");
        db.repl_effect(&format!("main.Item.insert({{ id: \"{literal}\", n: 1 }})"))?;
        expected.insert(text(vector, "component")?.to_owned());
    }
    db.publish()?.ok()?;
    let mount = db.mount("snapshot", &["--at", "HEAD"])?;
    let names = fs::read_dir(mount.point.join("Item"))
        .map_err(|error| Failure::Harness(format!("list Item: {error}")))?
        .map(|entry| {
            entry
                .map_err(|error| Failure::Harness(format!("list Item: {error}")))
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    ensure!(
        names == expected,
        "row directory names {names:?} are not the release components {expected:?}"
    );
    Ok(())
}

/// FINAL-VFS-023: a view exposes tables only: no `.git`, no `.orna` and no
/// writable raw store.
fn views_expose_no_repository_internals(db: &mut Db) -> Check {
    media_database(db)?;
    capture_row(db, "cover", "cover.bin", b"cover bytes", None)?;
    db.publish()?.ok()?;
    let mount = db.mount("snapshot", &["--at", "HEAD"])?;
    let names = fs::read_dir(&mount.point)
        .map_err(|error| Failure::Harness(format!("list the view: {error}")))?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    // ORNA-VFS-020 (ornadb-9npn.1): the root lists the table directory and the reachable
    // ordinary source module main.orna (profiles/vfs-1.md:31); .git and .orna are reserved
    // and never projected (profiles/vfs-1.md:33-34).
    let mut listed = names;
    listed.sort();
    ensure!(listed == ["main.orna", "media"], "the view root lists {listed:?}");
    ensure!(
        fs::write(mount.point.join(".git"), b"x").is_err()
            && fs::write(mount.point.join("data"), b"x").is_err(),
        "the view accepted a new top-level file"
    );
    Ok(())
}

/// FINAL-CHECKOUT-003: a commit mount and a branch-name mount each stay on
/// the snapshot they resolved, even after the branch moves.
fn snapshot_mounts_are_pinned(db: &mut Db) -> Check {
    let tables = db.declare("pub table Note(id: Str) { text: Str, }\n")?;
    relation_of(&tables, "Note")?;
    db.repl_effect(r#"main.Note.insert({ id: "first", text: "one" })"#)?;
    db.publish()?.ok()?;
    let branch = db
        .git_text(&["symbolic-ref", "--short", "HEAD"])?
        .trim()
        .to_owned();
    let commit = db.git_text(&["rev-parse", "HEAD"])?.trim().to_owned();
    let by_commit = db.mount("by-commit", &["--at", &commit])?;
    let by_branch = db.mount("by-branch", &["--at", &branch])?;
    db.repl_effect(r#"main.Note.insert({ id: "second", text: "two" })"#)?;
    db.publish()?.ok()?;
    for mount in [&by_commit, &by_branch] {
        ensure!(
            mount.point.join("Note/first/data.orna").is_file()
                && !mount.point.join("Note/second").exists(),
            "a snapshot view at {} moved with the branch",
            mount.point.display()
        );
        ensure!(
            fs::write(mount.point.join("Note/first/data.orna"), b"{}").is_err(),
            "a snapshot view accepted a write"
        );
    }
    Ok(())
}

// Gap probes.

fn typed_key_lookup_fails(db: &mut Db) -> Probe {
    let tables = db.declare("pub table Counter(id: Int) { label: Str, }\n")?;
    let relation = relation_of(&tables, "Counter")?;
    db.repl_effect(r#"main.Counter.insert({ id: 2, label: "two" })"#)?;
    db.publish()?.ok()?;
    let found = db.orna(&["query", &relation, "--key", "2", "--format", "json"])?;
    ensure!(
        !found.succeeded(),
        "orna query now finds the Int-keyed row: {}",
        found.output()
    );
    Ok(format!(
        "orna query --key 2 on an Int key: {}",
        found.diagnostic()
    ))
}

const SEED_SOURCE: &str = r#"pub table Note(id: Str) { text: Str, }

pub fn seed() {
    Note.insert({ id: "a", text: "first" });
    Note.insert({ id: "b", text: "second" });
}
"#;

fn run_rows_are_not_published(db: &mut Db) -> Probe {
    db.declare(SEED_SOURCE)?;
    db.orna(&["run", "main.seed"])?.ok()?;
    let published = db.publish()?;
    ensure!(
        !published.succeeded(),
        "rows written by orna run now publish: {}",
        published.output()
    );
    Ok(format!(
        "orna publish after orna run: {}",
        published.diagnostic()
    ))
}

fn large_row_is_not_published(db: &mut Db) -> Probe {
    let tables = db.declare("pub table Doc(id: Str) { body: Str, }\n")?;
    relation_of(&tables, "Doc")?;
    db.repl_effect(r#"main.Doc.insert({ id: "big", body: "small" })"#)?;
    db.publish()?.ok()?;
    let editor = db.scratch("editor")?.join("grow.sh");
    fs::write(
        &editor,
        format!(
            "#!/bin/sh\nprintf '{{\\n    body: \"%s\",\\n}}\\n' \"{}\" > \"$1\"\n",
            "x".repeat(100_000)
        ),
    )
    .map_err(|error| Failure::Harness(format!("write the editor: {error}")))?;
    make_executable(&editor)?;
    let edited = db.orna_with_editor(&editor, &["edit", "--row", "Doc/big/data.orna"])?;
    ensure!(
        edited.succeeded(),
        "the large edit was refused: {}",
        edited.output()
    );
    let published = db.publish()?;
    ensure!(
        !published.succeeded(),
        "a 100 KB row now publishes: {}",
        published.output()
    );
    Ok(format!(
        "orna publish of a 100 KB row: {}",
        published.diagnostic()
    ))
}

fn make_executable(path: &std::path::Path) -> Check {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|error| Failure::Harness(format!("chmod {}: {error}", path.display())))
}

fn no_clone(db: &mut Db) -> Probe {
    unknown_command(db, &["clone", ".", "copy"])
}

fn no_upgrade(db: &mut Db) -> Probe {
    unknown_command(db, &["upgrade", "--to-format", "3"])
}

/// The different-commit checkout is refused as the blocker: pending drafts are not
/// carried forward (CHECKOUT-3 step 4), so `orna checkout` answers that it is
/// refused while changes are pending. The probe first proves
/// that a same-commit checkout keeps a draft and a worktree file (ORNA-BRANCH-002),
/// then that the refused different-commit checkout leaves HEAD, the branch, the
/// index, the worktree and the draft unchanged (ORNA-CHECKOUT-001). A checkout
/// that succeeds while a draft is pending moved state past the refusal, which is a
/// harness failure rather than a closed gap. A refusal for any other reason means the
/// blocker no longer reproduces, which makes the gap stale.
fn different_commit_checkout_refused(db: &mut Db) -> Probe {
    db.declare("pub table Note(id: Str) { text: Str, }\n")?;
    db.repl_effect(r#"main.Note.insert({ id: "a", text: "one" })"#)?;
    db.publish()?.ok()?;
    db.orna(&["branch", "earlier"])?.ok()?;
    db.repl_effect(r#"main.Note.insert({ id: "b", text: "two" })"#)?;
    db.publish()?.ok()?;
    db.orna(&["branch", "mirror"])?.ok()?;
    db.repl_effect(r#"main.Note.insert({ id: "c", text: "draft" })"#)?;
    db.write("scratch.txt", "draft\n")?;
    let drafts = db.runtime_mutations()?;
    harness_ensure!(
        drafts > 0,
        "the unpublished draft is not counted as a runtime mutation"
    );

    db.orna(&["checkout", "mirror"])?.ok()?;
    harness_ensure!(
        checked_out_branch(db)? == "mirror",
        "the same-commit checkout did not select mirror"
    );
    harness_ensure!(
        db.runtime_mutations()? == drafts && db.read("scratch.txt")? == b"draft\n",
        "the same-commit checkout did not keep the draft and scratch.txt"
    );

    let head = db.git_text(&["rev-parse", "HEAD"])?;
    let status = db.git_text(&["status", "--porcelain=v1"])?;
    let checkout = db.orna(&["checkout", "earlier"])?;
    if checkout.succeeded() {
        return Err(Failure::Harness(format!(
            "orna checkout earlier to a different commit succeeded while a draft was pending: {}",
            checkout.diagnostic()
        )));
    }
    ensure!(
        checkout.output().contains("checkout to a different commit is refused while"),
        "orna checkout earlier to a different commit was refused for another reason: {}",
        checkout.diagnostic()
    );
    harness_ensure!(
        checked_out_branch(db)? == "mirror" && db.git_text(&["rev-parse", "HEAD"])? == head,
        "the refused checkout moved HEAD or the branch"
    );
    harness_ensure!(
        db.git_text(&["status", "--porcelain=v1"])? == status,
        "the refused checkout changed the index or the worktree"
    );
    harness_ensure!(
        db.runtime_mutations()? == drafts && db.read("scratch.txt")? == b"draft\n",
        "the refused checkout changed the draft or scratch.txt"
    );
    Ok(format!(
        "orna checkout earlier to a different commit: {}; the same-commit checkout \
         kept {drafts} draft(s)",
        checkout.diagnostic()
    ))
}

fn checked_out_branch(db: &mut Db) -> Result<String, Failure> {
    Ok(db
        .git_text(&["symbolic-ref", "--short", "HEAD"])?
        .trim()
        .to_owned())
}

fn storage_unobservable(db: &mut Db) -> Probe {
    repl_refuses(db, &["sys.Storage"], "ORNA-EVAL-NAME")
}

fn query_reports_no_estimate(db: &mut Db) -> Probe {
    let tables = db.declare("pub table Note(id: Str) { text: Str, }\n")?;
    let relation = relation_of(&tables, "Note")?;
    db.repl_effect(r#"main.Note.insert({ id: "a", text: "one" })"#)?;
    db.publish()?.ok()?;
    let report = db.query_json(&relation, &[])?;
    let text = report.to_string();
    ensure!(
        report.get("native_objects_read").is_some() && !text.contains("estimate"),
        "orna query now reports an estimate: {report}"
    );
    Ok(format!(
        "orna query reports native_objects_read {} and no estimate",
        report["native_objects_read"]
    ))
}

const SCORE_SOURCE: &str = r#"pub table Score(id: Str) {
    points: Int,
    assert every(score => score.points >= 0);
}

pub fn negative() {
    Score.insert({ id: "run", points: -1 });
}
"#;

fn repl_enforces_table_assertions(db: &mut Db) -> Check {
    db.declare(SCORE_SOURCE)?;
    let run = db.orna(&["run", "main.negative"])?;
    ensure!(!run.succeeded(), "orna run admits the violating row");
    let repl = db.repl(&[r#"main.Score.insert({ id: "repl", points: -1 })"#])?;
    ensure!(
        repl.output().contains("ORNA-A091-009") && db.runtime_mutations()? == 0,
        "the REPL admits `points: -1` against the table assertion: {}",
        repl.output()
    );
    Ok(())
}

fn edit_skips_table_assertions(db: &mut Db) -> Probe {
    db.declare(SCORE_SOURCE)?;
    db.repl_effect(r#"main.Score.insert({ id: "kept", points: 1 })"#)?;
    db.publish()?.ok()?;
    let editor = write_editor(db, "negative.sh", "sed -i 's/points: 1/points: -1/' \"$1\"")?;
    let edited = db.orna_with_editor(&editor, &["edit", "--row", "Score/kept/data.orna"])?;
    ensure!(
        edited.succeeded() && db.runtime_mutations()? == 1,
        "orna edit now enforces the table assertion: {}",
        edited.output()
    );
    Ok(
        "orna edit admits `points: -1` against `assert every(score => score.points >= 0)`"
            .to_owned(),
    )
}

fn write_editor(db: &mut Db, name: &str, body: &str) -> Result<std::path::PathBuf, Failure> {
    let editor = db.scratch("editors")?.join(name);
    fs::write(&editor, format!("#!/bin/sh\n{body}\n"))
        .map_err(|error| Failure::Harness(format!("write the editor: {error}")))?;
    make_executable(&editor)?;
    Ok(editor)
}

fn forbidden_blob_synonyms_are_accepted(db: &mut Db) -> Probe {
    db.write("main.orna", "pub table Note(id: Str) { body: Clob, }\n")?;
    let checked = db.orna(&["check"])?;
    ensure!(
        checked.succeeded(),
        "orna check now refuses the Clob synonym: {}",
        checked.output()
    );
    Ok("orna check: project valid with `body: Clob`".to_owned())
}

fn nested_blobs_are_refused(db: &mut Db) -> Probe {
    db.declare(
        r#"pub table album(id: Str) {
    cover: Blob?,
    tracks: [Blob],
}

pub fn add(id: Str, track: Blob) = album.insert({ id: id, cover: null, tracks: [track] });
"#,
    )?;
    db.write("track.bin", b"track bytes")?;
    let root = db.root_text()?;
    let run = db.repl(&[
        &format!("let raw = sys.blob.capture_file({root:?}, \"track.bin\", 64);"),
        "main.add(\"first\", raw).id",
    ])?;
    ensure!(
        run.output().contains("error["),
        "a row with nested Blobs is now accepted: {}",
        run.output()
    );
    Ok(format!(
        "REPL insert of `cover: null, tracks: [Blob]`: {}",
        run.diagnostic()
    ))
}

fn blob_operation_unsupported(db: &mut Db, expression: &str) -> Probe {
    repl_refuses(db, &[expression], "ORNA-EVAL-UNSUPPORTED")
}

fn blob_value_cannot_be_inspected(db: &mut Db) -> Probe {
    db.write("image.bin", b"image bytes")?;
    let root = db.root_text()?;
    let run = db.repl(&[
        &format!("let raw = sys.blob.capture_file({root:?}, \"image.bin\", 64);"),
        "let image = sys.blob.annotate(raw, \"image/png\");",
        "image",
    ])?;
    ensure!(
        run.output().matches("error[").count() == 1,
        "the REPL now shows a captured Blob: {}",
        run.output()
    );
    Ok(format!("REPL `image`: {}", run.diagnostic()))
}

fn capture_is_admitted_in_an_assertion(db: &mut Db) -> Probe {
    db.write(
        "main.orna",
        r#"pub table Note(id: Str) {
    text: Str,
    assert every(note => sys.blob.length(sys.blob.capture_file("/", "etc/hostname", 64)) >= 0);
}
"#,
    )?;
    let checked = db.orna(&["check"])?;
    ensure!(
        checked.succeeded(),
        "orna check now refuses capture inside an assertion: {}",
        checked.output()
    );
    Ok("orna check: project valid with `sys.blob.capture_file` in a table assertion".to_owned())
}

fn media_type_edit_is_not_applied(db: &mut Db) -> Probe {
    let relation = media_database(db)?;
    capture_row(
        db,
        "cover",
        "cover.bin",
        b"cover bytes",
        Some(("image/png", None)),
    )?;
    db.publish()?.ok()?;
    let editor = write_editor(
        db,
        "media-type.sh",
        "sed -i 's#media_type: \"image/png\"#media_type: \"image/jpeg\"#' \"$1\"",
    )?;
    let edited = db.orna_with_editor(&editor, &["edit", "--row", "media/cover/data.orna"])?;
    let _ = db.publish()?;
    let listings = listings(db, &relation)?;
    let row = listing(&listings, "cover")?;
    ensure!(
        row["media_type"] == "image/png",
        "the media type edit is now applied: {row}"
    );
    Ok(format!(
        "orna edit of media_type to image/jpeg ({}); the stored row is still image/png",
        edited.diagnostic()
    ))
}

fn import_only_dry_runs(db: &mut Db) -> Probe {
    let bundle = db.scratch("bundle")?;
    let bundle = bundle
        .to_str()
        .ok_or_else(|| Failure::Harness("non-UTF-8 bundle path".to_owned()))?
        .to_owned();
    let imported = db.orna(&["import", &bundle])?;
    ensure!(
        !imported.succeeded() && imported.output().contains("without --dry-run"),
        "orna import now writes without --dry-run: {}",
        imported.output()
    );
    Ok(format!("orna import: {}", imported.diagnostic()))
}

const SERVING_MARKER: &str = "scenario-serving-entry-7f3a";

fn serve_ignores_the_serving_entry(db: &mut Db) -> Probe {
    db.declare(&format!(
        "pub table Note(id: Str) {{ text: Str, }}\n\npub fn serve(): Str = \"{SERVING_MARKER}\";\n"
    ))?;
    let server = db.serve()?;
    let page = server.request("GET", "/", &[], b"")?;
    ensure!(
        !page.body_text().contains(SERVING_MARKER),
        "GET / now renders the project's serve() entry"
    );
    Ok(format!(
        "GET / -> {} ({} bytes of the host's own page, without the serve() result)",
        page.status,
        page.body.len()
    ))
}

fn workspace_lacks_a_cwd_row(db: &mut Db) -> Probe {
    let tables = db.declare("pub table Note(id: Str) { text: Str, }\n")?;
    relation_of(&tables, "Note")?;
    db.repl_effect(r#"main.Note.insert({ id: "published", text: "in HEAD" })"#)?;
    db.publish()?.ok()?;
    db.repl_effect(r#"main.Note.insert({ id: "local", text: "only in the CWD" })"#)?;
    let mount = db.mount("workspace", &["--cwd"])?;
    ensure!(
        mount.point.join("Note/published").exists(),
        "the workspace mount does not show the published row either"
    );
    ensure!(
        !mount.point.join("Note/local").exists(),
        "the workspace mount now shows the CWD row"
    );
    Ok("workspace mount lists Note/published but not the CWD row Note/local".to_owned())
}

fn workspace_is_read_only(db: &mut Db) -> Probe {
    let tables = db.declare("pub table Note(id: Str) { text: Str, }\n")?;
    relation_of(&tables, "Note")?;
    db.repl_effect(r#"main.Note.insert({ id: "row", text: "editable?" })"#)?;
    db.publish()?.ok()?;
    let mount = db.mount("workspace", &["--cwd"])?;
    let data = mount.point.join("Note/row/data.orna");
    let error = fs::write(&data, b"{\n    text: \"edited\",\n}\n")
        .err()
        .ok_or_else(|| Failure::Unmet("the workspace mount now accepts a row save".to_owned()))?;
    ensure!(
        error.raw_os_error() == Some(30),
        "the workspace save failed for another reason: {error}"
    );
    Ok(format!(
        "save of Note/row/data.orna in the workspace mount: {error}"
    ))
}

fn workspace_source_module_writes_are_admitted_or_refused(db: &mut Db) -> Check {
    db.declare("pub table Note(id: Str) { text: Str, }\n")?;
    let mount = db.mount("workspace", &["--cwd"])?;
    let module = mount.point.join("main.orna");
    if !module.exists() {
        return Err(Failure::Harness(
            "the workspace mount no longer projects main.orna".to_owned(),
        ));
    }
    const EDITED: &str = "// edited\n";
    fs::write(&module, EDITED.as_bytes()).map_err(|error| {
        Failure::Unmet(format!("the workspace mount refused a source module write: {error}"))
    })?;
    expect_source_bytes(&module, EDITED)?;

    let mut malformed = open_source_for_write(&module)?;
    std::io::Write::write_all(&mut malformed, b"pub fn bad( {\n")
        .map_err(|error| Failure::Harness(format!("write the malformed candidate: {error}")))?;
    expect_source_errno(malformed.sync_all(), 22, "a malformed source candidate")?;
    expect_source_bytes(&module, EDITED)?;

    let mut stale = open_source_for_write(&module)?;
    std::io::Write::write_all(&mut stale, b"// stale candidate\n")
        .map_err(|error| Failure::Harness(format!("write the stale candidate: {error}")))?;
    const OTHER: &str = "// another writer\n";
    db.write("main.orna", OTHER)?;
    expect_source_errno(stale.sync_all(), 11, "a candidate whose disk baseline changed")?;
    expect_source_bytes(&module, OTHER)
}

fn open_source_for_write(module: &std::path::Path) -> Result<fs::File, Failure> {
    fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(module)
        .map_err(|error| Failure::Unmet(format!("write-intent open was refused: {error}")))
}

fn expect_source_errno(result: std::io::Result<()>, code: i32, what: &str) -> Check {
    match result {
        Err(error) if error.raw_os_error() == Some(code) => Ok(()),
        Err(error) => Err(Failure::Unmet(format!("{what}: expected errno {code}, got {error}"))),
        Ok(()) => Err(Failure::Unmet(format!("{what} was admitted"))),
    }
}

fn expect_source_bytes(module: &std::path::Path, expected: &str) -> Check {
    let read = fs::read_to_string(module)
        .map_err(|error| Failure::Harness(format!("read main.orna: {error}")))?;
    if read != expected {
        return Err(Failure::Unmet(format!(
            "a new open reads {read:?}, not the expected committed bytes {expected:?}"
        )));
    }
    Ok(())
}

fn traversal_descriptor_is_admitted(db: &mut Db) -> Probe {
    media_database(db)?;
    capture_row(
        db,
        "cover",
        "cover.bin",
        b"cover bytes",
        Some(("image/png", None)),
    )?;
    db.publish()?.ok()?;
    let editor = write_editor(
        db,
        "traversal.sh",
        "sed -i 's#path: \"[^\"]*\"#path: \"../../etc/hostname\"#' \"$1\"",
    )?;
    let edited = db.orna_with_editor(&editor, &["edit", "--row", "media/cover/data.orna"])?;
    ensure!(
        edited.succeeded() && db.runtime_mutations()? == 1,
        "orna edit now refuses a traversal path: {}",
        edited.output()
    );
    Ok("orna edit admitted `path: \"../../etc/hostname\"` as a runtime mutation".to_owned())
}

fn null_required_blob_is_admitted(db: &mut Db) -> Probe {
    media_database(db)?;
    capture_row(
        db,
        "cover",
        "cover.bin",
        b"cover bytes",
        Some(("image/png", None)),
    )?;
    db.publish()?.ok()?;
    let editor = write_editor(
        db,
        "null.sh",
        "sed -i 's/content: {[^}]*}/content: null/' \"$1\"",
    )?;
    let edited = db.orna_with_editor(&editor, &["edit", "--row", "media/cover/data.orna"])?;
    ensure!(
        edited.succeeded() && db.runtime_mutations()? == 1,
        "orna edit now refuses a null required Blob: {}",
        edited.output()
    );
    Ok("orna edit admitted `content: null` for the required Blob field".to_owned())
}

fn refused_edit_keeps_no_draft(db: &mut Db) -> Probe {
    media_database(db)?;
    capture_row(
        db,
        "cover",
        "cover.bin",
        b"cover bytes",
        Some(("image/png", None)),
    )?;
    db.publish()?.ok()?;
    let invalid = write_editor(
        db,
        "invalid.sh",
        "sed -i 's/title: \"cover\"/title: 5/' \"$1\"",
    )?;
    let refused = db.orna_with_editor(&invalid, &["edit", "--row", "media/cover/data.orna"])?;
    ensure!(!refused.succeeded(), "the invalid edit was accepted");
    let seen = db.scratch("seen")?.join("opened.orna");
    let capture = write_editor(
        db,
        "capture.sh",
        &format!("cp \"$1\" {:?}", seen.to_string_lossy()),
    )?;
    db.orna_with_editor(&capture, &["edit", "--row", "media/cover/data.orna"])?;
    let opened = fs::read_to_string(&seen)
        .map_err(|error| Failure::Harness(format!("the second edit opened nothing: {error}")))?;
    ensure!(
        opened.contains("title: \"cover\"") && !opened.contains("title: 5"),
        "the second edit now opens the rejected draft: {opened}"
    );
    Ok(format!(
        "after `{}`, the next edit opened the accepted text",
        refused.diagnostic()
    ))
}

fn run_cannot_capture(db: &mut Db) -> Probe {
    db.write("song.bin", b"song bytes")?;
    let root = db.root_text()?;
    db.declare(&format!(
        r#"pub table song(id: Str) {{ content: Blob, }}

pub fn ingest() {{
    let raw = sys.blob.capture_file({root:?}, "song.bin", 64);
    song.insert({{ id: "song", content: raw }});
}}
"#
    ))?;
    let run = db.orna(&["run", "main.ingest"])?;
    ensure!(
        !run.succeeded(),
        "orna run now captures a file: {}",
        run.output()
    );
    Ok(format!("orna run main.ingest: {}", run.diagnostic()))
}

fn push_fails_after_commit(db: &mut Db) -> Probe {
    db.declare("pub table Note(id: Str) { text: Str, }\n")?;
    let remote = db.scratch("remote.git")?;
    let remote = remote
        .to_str()
        .ok_or_else(|| Failure::Harness("non-UTF-8 remote path".to_owned()))?
        .to_owned();
    db.git(&["init", "-q", "--bare", &remote])?.ok()?;
    db.git(&["remote", "add", "origin", &remote])?.ok()?;
    let branch = db
        .git_text(&["symbolic-ref", "--short", "HEAD"])?
        .trim()
        .to_owned();
    let pushed = db.orna(&["push", "origin", &branch])?;
    ensure!(
        !pushed.succeeded(),
        "orna push now succeeds: {}",
        pushed.output()
    );
    Ok(format!(
        "orna push origin {branch}: {}",
        pushed.diagnostic()
    ))
}

fn add_row_finds_no_published_row(db: &mut Db) -> Probe {
    db.declare("pub table Music(id: Str) { title: Str, }\n")?;
    db.repl_effect(r#"main.Music.insert({ id: "london", title: "Live in London" })"#)?;
    db.publish()?.ok()?;
    let staged = db.orna(&["add", "--row", "Music", "london"])?;
    ensure!(
        !staged.succeeded() && staged.output().contains("logical row was not found"),
        "orna add --row now stages the published row: {}",
        staged.output()
    );
    Ok(format!(
        "orna add --row Music london: {}",
        staged.diagnostic()
    ))
}

/// FINAL-STAGE-004: a store node changed with plain Git is an external
/// candidate change: `orna commit` refuses to stage over it and commits
/// nothing.
fn external_store_edits_fail_staging(db: &mut Db) -> Check {
    let tables = db.declare("pub table Note(id: Str) { text: Str, }\n")?;
    relation_of(&tables, "Note")?;
    let head = db.git_text(&["rev-parse", "HEAD"])?.trim().to_owned();
    db.write("external.bin", b"edited with another tool")?;
    let blob = db
        .git_text(&["hash-object", "-w", "external.bin"])?
        .trim()
        .to_owned();
    db.git(&[
        "update-index",
        "--cacheinfo",
        &format!("100644,{blob},.orna/store/data"),
    ])?
    .ok()?;
    db.write(
        "main.orna",
        "pub table Note(id: Str) { text: Str, }\n// staged edit\n",
    )?;
    db.git(&["add", "main.orna"])?.ok()?;
    let staged = db.git_text(&["ls-files", "--stage", "--", ".orna/store/data"])?;
    let committed = db.orna(&["commit", "-m", "Commit over an external store edit"])?;
    ensure!(
        !committed.succeeded(),
        "orna commit accepted a store node edited with plain Git: {}",
        committed.output()
    );
    ensure!(
        db.git_text(&["rev-parse", "HEAD"])?.trim() == head,
        "the refused commit moved the branch"
    );
    ensure!(
        db.git_text(&["ls-files", "--stage", "--", ".orna/store/data"])? == staged,
        "the refused commit rewrote the externally staged store node"
    );
    Ok(())
}

fn pinned_std_import_fails(db: &mut Db) -> Probe {
    check_refuses(
        db,
        "use std.format.strings;\n\npub fn twice(): Str = strings.repeat(\"x\", 2);\n",
        "ORNA-S0",
    )
}

fn media_is_not_decoded(db: &mut Db) -> Probe {
    let relation = media_database(db)?;
    let png = fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/media/pixel.png"
    ))
    .map_err(|error| Failure::Harness(format!("read the PNG fixture: {error}")))?;
    capture_row(db, "pixel", "pixel.png", &png, Some(("image/png", None)))?;
    db.publish()?.ok()?;
    let listings = listings(db, &relation)?;
    let row = listing(&listings, "pixel")?;
    ensure!(
        row.get("dimensions").is_none() && row.get("width").is_none(),
        "orna query now reports decoded image properties: {row}"
    );
    Ok(format!(
        "orna query lists the PNG with no decoded properties: {row}"
    ))
}
