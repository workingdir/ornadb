//! Evidence-grade Streams chapter audit against the frozen Orna 1.0.0 text.
//!
//! Authority: `reference/Orna-1.0.0/source/11-streams.md`.
//! Verified normative line anchors: ORNA-CONSUMER-001..009 at lines 32-58,
//! ORNA-CONNECTOR-001 at line 48, and ORNA-STREAM-001..005 at lines 63-71.
//! Evidence grades: A = a checked-in fixture reaches the public adapter/runtime;
//! B = source plus existing focused tests; C = bounded source/interface review.
//!
//! Top-five evidence gaps (exact criteria, not inferred extensions):
//! 1. A / reproduced: ORNA-CONSUMER-001 (line 32) requires function ObjectId;
//!    the adapter persists the entry spelling (`orna-conformance-v1/src/
//!    semantic_adapter.rs` lines 4683-4689). The fixture observes `main`, and
//!    the identity model is a free-form `Component`, not an ObjectId
//!    (`orna-stream-v1/src/lib.rs` lines 48-67).
//! 2. A / reproduced: ORNA-CONSUMER-003/004 (lines 36/38) require stable
//!    identity across edits/renames and a new identity after delete/recreate.
//!    The fixture pair proves renaming changes the key from `main` to `ingest`;
//!    delete/recreate lifecycle identity is not represented.
//! 3. B / partial: ORNA-CONSUMER-002 (line 34) and ORNA-CONNECTOR-001 (line 48).
//!    `CheckpointKey` has source, partition and position-format fields; the
//!    built-in list bridge hashes canonical list items plus its label. A fixture
//!    proves this is insensitive to source layout. Generic connector config
//!    canonicalization and secret-ref/plaintext exclusion are not enforced by
//!    this bounded adapter.
//! 4. A / previously absent fixture proof: ORNA-CONSUMER-005 (line 40). A
//!    checked-in multi-root source reaches semantic analysis and checks the
//!    separate-consumer diagnostic. Existing local-owner evidence for
//!    ORNA-CONSUMER-006 (line 52) is in `stream_admission_payload.rs`;
//!    ORNA-CONSUMER-008 (line 56) has equal/divergent opaque-token tests in
//!    `orna-stream-v1/src/lib.rs` lines 1327-1357. That proves the merge
//!    primitive returns Conflict, not its database mapping to
//!    `sys.CheckpointConflict`.
//! 5. A / reproduced bounded-adapter gap: ORNA-STREAM-005 (line 71). The list
//!    adapter requires one inline `for_each` insert body
//!    (`semantic_adapter.rs` lines 4001-4004, 4644-4647); a named function-value
//!    callback fixture is not admitted. This is not a claim about every runtime
//!    connector or the full language engine.
//!
//! Reference boundary: ORNA-CONSUMER-007/009 (lines 54/58) expressly exclude
//! base cross-clone ownership detection; no such diagnostic is a gap.
//! ORNA-STREAM-001/002/004 (lines 63/65/69) are exercised below. ORNA-STREAM-
//! 003 (line 67) has separate cancellation coverage in the existing
//! `semantic_runtime_adapter.rs` tests; generic provider behavior remains
//! limited to the `StreamSource` contract.
//!
//! Prose-only coverage: lines 5, 20-24 and 28 describe lazy loading, ordinary
//! invocation binding, file-name non-execution and one checkpointed root;
//! line 42 describes replay/persistence choices; line 46 lists connector
//! capabilities. Those lines carry no ORNA-* IDs. The fixture checks loading,
//! explicit dispatch and root diagnostics; there is no generic connector
//! protocol to test at the stated level of detail, so no extra behavior is
//! inferred.

#[path = "../src/test_support.rs"]
mod test_support;

use std::{fs, path::Path, process::Command};

use orna_conformance_v1::{DurableTransactionalEvaluator, SourceUnit, StageOutcome};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;
use orna_project_v1::ProjectLoader;
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};
use orna_stream_v1::{CheckpointKey, Component, ConsumerIdentity};
use orna_value_v1::Raw;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const FINITE_STREAM: &str = include_str!("fixtures/streams-pa0p-finite.orna");
const LAYOUT_VARIANT: &str = include_str!("fixtures/streams-pa0p-layout-variant.orna");
const CONFIG_VARIANT: &str = include_str!("fixtures/streams-pa0p-config-variant.orna");
const RENAMED_CONSUMER: &str = include_str!("fixtures/streams-pa0p-renamed-consumer.orna");
const MULTIPLE_ROOTS: &str = include_str!("fixtures/streams-pa0p-multiple-roots.orna");
const FUNCTION_CALLBACK: &str = include_str!("fixtures/streams-pa0p-function-callback.orna");
const PREDICATE_NESTED_PAIRS: &str =
    include_str!("fixtures/streams-jyy6a-predicate-nested-pairs.orna");
const PREDICATE_NESTED_PAIRS_DUPLICATE: &str =
    include_str!("fixtures/streams-jyy6a-predicate-nested-pairs-duplicate.orna");
const PREDICATE_NESTED_PAIRS_EVERY_MISMATCH: &str =
    include_str!("fixtures/streams-jyy6a-predicate-nested-pairs-every-mismatch.orna");

fn nested_pair_key(
    first_id: i64,
    first_label: &str,
    second_id: i64,
    second_label: &str,
    third_id: i64,
    third_label: &str,
) -> Raw {
    Raw::Array(vec![
        Raw::Array(vec![
            Raw::Array(vec![Raw::Int(first_id.into()), Raw::Text(first_label.into())]),
            Raw::Array(vec![Raw::Int(second_id.into()), Raw::Text(second_label.into())]),
        ]),
        Raw::Array(vec![Raw::Int(third_id.into()), Raw::Text(third_label.into())]),
    ])
}

fn row_field<'a>(row: &'a Value, field: &str) -> &'a Raw {
    let Raw::Map(fields) = row.raw() else {
        panic!("expected a table row record, got {:?}", row.raw());
    };
    fields
        .iter()
        .find_map(|(key, value)| (key == &Raw::Text(field.into())).then_some(value))
        .unwrap_or_else(|| panic!("table row omitted field {field}"))
}

fn nested_pair_row(id: i64, key: Raw, expected: Raw) -> Value {
    let mut fields = vec![
        (String::from("id"), Raw::Int(id.into())),
        (String::from("key"), key),
        (String::from("expected"), expected),
    ];
    fields.sort_by_cached_key(|(name, _)| {
        Value::new(Raw::Text(name.clone()))
            .expect("field name is a canonical value")
            .encode()
            .expect("field name encodes canonically")
    });
    Value::new(Raw::Map(
        fields
            .into_iter()
            .map(|(name, value)| (Raw::Text(name), value))
            .collect(),
    ))
    .expect("nested pair row is canonical")
}

fn list_checkpoint_key_for_values(
    function: &str,
    source_label: &str,
    values: &[Value],
) -> CheckpointKey {
    let payloads = values
        .iter()
        .map(|value| value.encode().expect("encode canonical source item"))
        .collect::<Vec<_>>();
    let mut digest = Sha256::new();
    digest.update(b"ORNA-LIST-STREAM-IDENTITY\0");
    for payload in &payloads {
        digest.update(u64::try_from(payload.len()).unwrap().to_be_bytes());
        digest.update(payload);
    }
    let suffix = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let component = |value: &str| Component::new(value).expect("valid identity component");
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: component("conformance"),
            root: component("main.orna"),
            function: component(function),
            binding: component("from_list"),
        },
        source_format: component("orna-stream-v1"),
        source: component(&format!("{source_label}:{suffix}")),
        partition_format: component("literal-list"),
        partition: None,
        position_format: component("orna.list.v1"),
    }
}

fn repository(source: &str) -> (TempDir, Repository) {
    let directory = tempfile::tempdir_in("/var/tmp").expect("temporary repository");
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main", "--template="])
        .current_dir(directory.path())
        .output()
        .expect("start git init");
    assert!(initialized.status.success(), "initialize temporary repository");
    test_support::configure_fixture_git_identity(directory.path());
    fs::write(directory.path().join("main.orna"), source).expect("write checked-in source fixture");
    git(directory.path(), &["add", "main.orna"]);
    git(directory.path(), &["commit", "--quiet", "-m", "fixture"]);
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

fn git(path: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("start fixture git command");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn identity() -> RuntimeIdentity {
    RuntimeIdentity {
        database_id: [0x51; 16],
        repository_id: [0x52; 16],
    }
}

fn list_checkpoint_key(function: &str, source_label: &str, values: &[i64]) -> CheckpointKey {
    let payloads = values
        .iter()
        .map(|value| Value::int((*value).into()).encode().expect("encode list item"))
        .collect::<Vec<_>>();
    let mut digest = Sha256::new();
    digest.update(b"ORNA-LIST-STREAM-IDENTITY\0");
    for payload in &payloads {
        digest.update(u64::try_from(payload.len()).unwrap().to_be_bytes());
        digest.update(payload);
    }
    let suffix = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let component = |value: &str| Component::new(value).expect("valid identity component");
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: component("conformance"),
            root: component("main.orna"),
            function: component(function),
            binding: component("from_list"),
        },
        source_format: component("orna-stream-v1"),
        source: component(&format!("{source_label}:{suffix}")),
        partition_format: component("literal-list"),
        partition: None,
        position_format: component("orna.list.v1"),
    }
}

fn source_unit() -> SourceUnit {
    SourceUnit {
        fixture_id: "streams-pa0p-finite".into(),
        source_id: "main.orna".into(),
        parse_as: "module_unit".into(),
        source: FINITE_STREAM.into(),
    }
}

fn source_unit_with(source: &str, fixture_id: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: "main.orna".into(),
        parse_as: "module_unit".into(),
        source: source.into(),
    }
}

fn source_unit_with_id(source: &str, fixture_id: &str, source_id: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: source_id.into(),
        parse_as: "module_unit".into(),
        source: source.into(),
    }
}

#[tokio::test]
async fn finite_fixture_is_inert_until_dispatch_and_commits_each_item() {
    let (_directory, repository) = repository(FINITE_STREAM);
    let identity = identity();
    let state = RuntimeState::open(&repository, identity, [0x53; 32])
        .await
        .expect("open runtime state");
    let before_load = state.capture().await.expect("capture before module load");

    let loaded = ProjectLoader::default()
        .load(&repository)
        .expect("load finite stream module");
    assert_eq!(
        loaded
            .identities()
            .iter()
            .map(|module| module.logical_path())
            .collect::<Vec<_>>(),
        ["main.orna"]
    );
    assert_eq!(state.capture().await, Ok(before_load));
    assert_eq!(state.latest_checkpoint().await, Ok(None));
    assert!(state.stream_observations().await.unwrap().is_empty());
    assert!(state.committed_table_rows("Reading").await.unwrap().is_empty());

    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    assert_eq!(evaluator.admit_list_stream_source(&source_unit()), StageOutcome::Passed);
    assert_eq!(
        evaluator
            .execute_list_stream_source(
                &repository,
                identity,
                [0x54; 16],
                [0x53; 32],
                &source_unit(),
            )
            .await
            .expect("explicit finite dispatch"),
        StageOutcome::Passed
    );

    let key = list_checkpoint_key("main", "fixture:streams-pa0p-finite", &[1, 2]);
    let checkpoint = state
        .stream_checkpoint(&key)
        .await
        .expect("one checkpoint per finite item");
    assert_eq!(checkpoint.version, 2);
    assert_eq!(
        checkpoint.committed.unwrap().token.as_str(),
        "2",
        "the finite source returns at exhaustion after both item callbacks"
    );
    for value in [1, 2] {
        let key = Value::int(value.into()).encode().expect("encoded table key");
        assert!(
            state
                .committed_table_row("Reading", &key)
                .await
                .expect("read delivered row")
                .is_some()
        );
    }

    // Evidence-grade gap: the bounded source adapter keys the consumer by the
    // entry spelling. ORNA-CONSUMER-001 instead requires the function ObjectId.
    assert_eq!(key.consumer.function.as_str(), "main");
}

#[tokio::test]
async fn semantic_consumer_rename_changes_the_spelling_key() {
    let (_directory, repository) = repository(FINITE_STREAM);
    let identity = identity();
    let state = RuntimeState::open(&repository, identity, [0x53; 32])
        .await
        .expect("open runtime state");
    let first = DurableTransactionalEvaluator::new("main", Limits::default())
        .execute_list_stream_source(
            &repository,
            identity,
            [0x54; 16],
            [0x53; 32],
            &source_unit(),
        )
        .await
        .expect("execute original consumer");
    assert_eq!(first, StageOutcome::Passed);
    let original_key = list_checkpoint_key("main", "fixture:streams-pa0p-finite", &[1, 2]);
    let original = state
        .stream_checkpoint(&original_key)
        .await
        .expect("original spelling-keyed checkpoint")
        .key
        .consumer;

    let renamed = DurableTransactionalEvaluator::new("ingest", Limits::default())
        .execute_list_stream_source(
            &repository,
            identity,
            [0x54; 16],
            [0x53; 32],
            &source_unit_with(RENAMED_CONSUMER, "streams-pa0p-renamed"),
        )
        .await
        .expect("execute semantically renamed consumer");
    assert_eq!(renamed, StageOutcome::Passed);
    let renamed_key = list_checkpoint_key("ingest", "fixture:streams-pa0p-finite", &[1, 2]);
    assert_eq!(
        state
            .stream_checkpoint(&renamed_key)
            .await
            .expect("renamed spelling-keyed checkpoint")
            .version,
        2
    );
    assert_eq!(original.function.as_str(), "main");
    assert_eq!(renamed_key.consumer.function.as_str(), "ingest");
    assert_ne!(original, renamed_key.consumer);
}

#[tokio::test]
async fn finite_list_identity_is_stable_across_source_layout_changes() {
    let (_directory, repository) = repository(FINITE_STREAM);
    let identity = identity();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    for (source, fixture_id) in [
        (FINITE_STREAM, "streams-pa0p-finite"),
        (LAYOUT_VARIANT, "streams-pa0p-layout-variant"),
    ] {
        assert_eq!(
            evaluator
                .execute_list_stream_source(
                    &repository,
                    identity,
                    [0x54; 16],
                    [0x53; 32],
                    &source_unit_with_id(source, fixture_id, "main.orna"),
                )
                .await
                .expect("execute source with fixed semantic config"),
            StageOutcome::Passed
        );
    }
    let state = RuntimeState::open(&repository, identity, [0x53; 32])
        .await
        .expect("reopen runtime state");
    let key = list_checkpoint_key("main", "fixture:streams-pa0p-finite", &[1, 2]);
    let checkpoint = state
        .stream_checkpoint(&key)
        .await
        .expect("stable source checkpoint");
    assert_eq!(
        checkpoint.key.source,
        key.source,
        "ORNA-CONNECTOR-001: the built-in list source identity depends on stable source configuration and canonical items, not source layout"
    );
    assert_eq!(checkpoint.version, 2, "the second layout resumes the same key");
}

#[tokio::test]
async fn finite_list_identity_changes_with_semantic_source_configuration() {
    let (_directory, repository) = repository(FINITE_STREAM);
    let identity = identity();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    for (source, fixture_id) in [
        (FINITE_STREAM, "streams-pa0p-finite"),
        (CONFIG_VARIANT, "streams-pa0p-config-variant"),
    ] {
        assert_eq!(
            evaluator
                .execute_list_stream_source(
                    &repository,
                    identity,
                    [0x54; 16],
                    [0x53; 32],
                    &source_unit_with_id(source, fixture_id, "main.orna"),
                )
                .await
                .expect("execute source with changed semantic list config"),
            StageOutcome::Passed
        );
    }
    let state = RuntimeState::open(&repository, identity, [0x53; 32])
        .await
        .expect("reopen runtime state");
    let first = state
        .stream_checkpoint(&list_checkpoint_key(
            "main",
            "fixture:streams-pa0p-finite",
            &[1, 2],
        ))
        .await
        .expect("first semantic source checkpoint");
    let second = state
        .stream_checkpoint(&list_checkpoint_key(
            "main",
            "fixture:streams-pa0p-finite",
            &[3, 4],
        ))
        .await
        .expect("second semantic source checkpoint");
    assert_ne!(
        first.key.source, second.key.source,
        "a changed canonical list is a changed source identity despite the same label"
    );
}

#[tokio::test]
async fn section9_predicates_fold_complete_nested_pairs_at_each_checkpoint() {
    // Section 9 compares the complete selected values. This proof interprets
    // the stream checkpoint boundary as an activation: each item must preserve
    // the full nested pair before the next checkpoint is committed.
    let (_directory, repository) = repository(PREDICATE_NESTED_PAIRS);
    let identity = identity();
    let state = RuntimeState::open(&repository, identity, [0x53; 32])
        .await
        .expect("open runtime state");
    let fixture = source_unit_with(
        PREDICATE_NESTED_PAIRS,
        "streams-jyy6a-predicate-nested-pairs",
    );
    let outcome = DurableTransactionalEvaluator::new("main", Limits::default())
        .execute_list_stream_source(
            &repository,
            identity,
            [0x54; 16],
            [0x53; 32],
            &fixture,
        )
        .await
        .expect("execute nested-pair source");
    assert_eq!(outcome, StageOutcome::Passed);

    let keys = [
        nested_pair_key(1, "east", 2, "oak", 3, "red"),
        nested_pair_key(1, "east", 2, "oak", 3, "blue"),
        nested_pair_key(1, "east", 2, "pine", 3, "red"),
        nested_pair_key(1, "west", 2, "oak", 3, "red"),
    ];
    let source_values = keys
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, key)| nested_pair_row(index as i64 + 1, key.clone(), key))
        .collect::<Vec<_>>();
    let key = list_checkpoint_key_for_values(
        "main",
        "fixture:streams-jyy6a-predicate-nested-pairs",
        &source_values,
    );
    let checkpoint = state
        .stream_checkpoint(&key)
        .await
        .expect("checkpoint after every nested pair");
    assert_eq!(checkpoint.version, 4);
    assert_eq!(checkpoint.committed.unwrap().token.as_str(), "4");

    let rows = state
        .committed_table_rows("Reading")
        .await
        .expect("read committed nested-pair rows");
    assert_eq!(rows.len(), 4);
    for (index, ((_, encoded), expected_key)) in rows.iter().zip(keys).enumerate() {
        let row = Value::decode(encoded).expect("decode committed row");
        assert_eq!(row_field(&row, "id"), &Raw::Int((index as i64 + 1).into()));
        assert_eq!(row_field(&row, "key"), &expected_key);
        assert_eq!(row_field(&row, "expected"), &expected_key);
    }
}

#[tokio::test]
async fn all_unique_rejects_a_nested_pair_duplicate_without_advancing_checkpoint() {
    let (_directory, repository) = repository(PREDICATE_NESTED_PAIRS_DUPLICATE);
    let identity = identity();
    let state = RuntimeState::open(&repository, identity, [0x53; 32])
        .await
        .expect("open runtime state");
    let fixture = source_unit_with(
        PREDICATE_NESTED_PAIRS_DUPLICATE,
        "streams-jyy6a-predicate-nested-pairs-duplicate",
    );
    assert!(matches!(
        DurableTransactionalEvaluator::new("main", Limits::default())
            .execute_list_stream_source(
                &repository,
                identity,
                [0x54; 16],
                [0x53; 32],
                &fixture,
            )
            .await
            .expect("execute duplicate nested-pair source"),
        StageOutcome::Failed(_)
    ));

    let red = nested_pair_key(1, "east", 2, "oak", 3, "red");
    let blue = nested_pair_key(1, "east", 2, "oak", 3, "blue");
    let source_values = vec![
        nested_pair_row(1, red.clone(), red.clone()),
        nested_pair_row(2, blue.clone(), blue.clone()),
        nested_pair_row(3, red.clone(), red.clone()),
    ];
    let key = list_checkpoint_key_for_values(
        "main",
        "fixture:streams-jyy6a-predicate-nested-pairs-duplicate",
        &source_values,
    );
    let checkpoint = state
        .stream_checkpoint(&key)
        .await
        .expect("checkpoint stops before duplicate nested pair");
    assert_eq!(checkpoint.version, 2);
    assert_eq!(checkpoint.committed.unwrap().token.as_str(), "2");
    let rows = state
        .committed_table_rows("Reading")
        .await
        .expect("read rows before duplicate");
    assert_eq!(rows.len(), 2);
    for (index, ((_, encoded), expected_key)) in rows.iter().zip([red, blue]).enumerate() {
        let row = Value::decode(encoded).expect("decode committed row");
        assert_eq!(row_field(&row, "id"), &Raw::Int((index as i64 + 1).into()));
        assert_eq!(row_field(&row, "key"), &expected_key);
    }
}

#[tokio::test]
async fn every_rejects_a_nested_pair_mismatch_at_its_checkpoint() {
    let (_directory, repository) = repository(PREDICATE_NESTED_PAIRS_EVERY_MISMATCH);
    let identity = identity();
    let state = RuntimeState::open(&repository, identity, [0x53; 32])
        .await
        .expect("open runtime state");
    let fixture = source_unit_with(
        PREDICATE_NESTED_PAIRS_EVERY_MISMATCH,
        "streams-jyy6a-predicate-nested-pairs-every-mismatch",
    );
    assert!(matches!(
        DurableTransactionalEvaluator::new("main", Limits::default())
            .execute_list_stream_source(
                &repository,
                identity,
                [0x54; 16],
                [0x53; 32],
                &fixture,
            )
            .await
            .expect("execute nested-pair mismatch source"),
        StageOutcome::Failed(_)
    ));

    let matching = nested_pair_key(1, "east", 2, "oak", 3, "red");
    let mismatching = nested_pair_key(1, "east", 2, "oak", 3, "blue");
    let source_values = vec![
        nested_pair_row(1, matching.clone(), matching.clone()),
        nested_pair_row(2, mismatching, matching.clone()),
    ];
    let key = list_checkpoint_key_for_values(
        "main",
        "fixture:streams-jyy6a-predicate-nested-pairs-every-mismatch",
        &source_values,
    );
    let checkpoint = state
        .stream_checkpoint(&key)
        .await
        .expect("checkpoint stops before every mismatch");
    assert_eq!(checkpoint.version, 1);
    assert_eq!(checkpoint.committed.unwrap().token.as_str(), "1");
    let rows = state
        .committed_table_rows("Reading")
        .await
        .expect("read row before mismatch");
    assert_eq!(rows.len(), 1);
    let row = Value::decode(&rows[0].1).expect("decode committed matching row");
    assert_eq!(row_field(&row, "id"), &Raw::Int(1.into()));
    assert_eq!(row_field(&row, "key"), &matching);
    assert_eq!(row_field(&row, "expected"), &matching);
}

#[test]
fn multiple_checkpointed_roots_have_the_required_extraction_diagnostic() {
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new("multiple-roots.orna", MULTIPLE_ROOTS)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.message().starts_with(
                "a durable consumer function may own only one checkpointed source root"
            )
        }),
        "ORNA-CONSUMER-005 requires the separate named-consumer refactoring guidance: {:?}",
        analysis.diagnostics
    );
}

#[test]
fn named_function_value_callback_is_outside_the_current_finite_bridge() {
    assert!(
        orna_syntax_v1::parse_module(FUNCTION_CALLBACK).is_ok(),
        "the named callback source fixture itself must parse"
    );
    let source = SourceUnit {
        fixture_id: "streams-pa0p-function-callback".into(),
        source_id: "callbacks.orna".into(),
        parse_as: "module_unit".into(),
        source: FUNCTION_CALLBACK.into(),
    };
    let outcome = DurableTransactionalEvaluator::new("main", Limits::default())
        .admit_list_stream_source(&source);
    assert!(
        !matches!(outcome, StageOutcome::Passed),
        "ORNA-STREAM-005 function-value callback must be supported alongside the anonymous callback: {outcome:?}"
    );
}
