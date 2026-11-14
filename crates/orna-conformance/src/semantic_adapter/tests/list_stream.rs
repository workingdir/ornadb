use super::{
    Catalogue, DurableTransactionalEvaluator, PROJECT_STREAM_ROOT_ADMISSION_REASON, SourceUnit,
    StageOutcome, admit_project_list_stream, admit_transaction_project, evaluate_list_item_row,
    stream_handler_diagnostic, table_key,
};
use crate::{ProjectEnvironment, ProjectExpectations, ProjectUnit};
use orna_evaluator::Limits;
use orna_repository::Repository;
use orna_runtime::{
    ListStreamSource, RuntimeIdentity, RuntimeState, StreamHandler, StreamHandlerResult, StreamItem,
};
use orna_stream::{
    AsyncCheckpointBackend, CheckpointKey, Component, ConsumerIdentity, DeliveryIdentity,
    FailureIdentity, Position,
};
use orna_value::{OvbRaw, Value};
use std::{path::Path, process::Command};
use tempfile::TempDir;

fn git(path: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git command")
            .success()
    );
}

fn repository() -> (TempDir, Repository) {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init", "-q"]);
    crate::test_support::configure_fixture_git_identity(temp.path());
    let repository = Repository::discover(temp.path()).expect("repository");
    (temp, repository)
}

fn identity() -> RuntimeIdentity {
    RuntimeIdentity {
        database_id: [21; 16],
        repository_id: [22; 16],
    }
}

fn source_with_values(values: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "list-stream".into(),
        source_id: "sensors.orna".into(),
        parse_as: "module_unit".into(),
        source: format!(
            "pub table Reading(id: Int) {{ value: Int, }} fn main() {{ Stream.from_list([{values}], source_identity: \"fixture:readings\") | for_each(value => {{ Reading.insert({{ id: value, value: value }}); }}); }}"
        ),
    }
}

fn source() -> SourceUnit {
    source_with_values("1, 2")
}

fn asserted_project() -> ProjectUnit {
    super::assertion_checkpoint_091_project()
}

#[tokio::test]
async fn project_stream_assertion_rolls_back_invalid_delivery_and_checkpoint() {
    let (_temp, repository) = repository();
    let project = asserted_project();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

    let outcome = evaluator
        .execute_project_stream(
            &repository,
            identity(),
            [23; 16],
            [24; 32],
            &project,
            "main.ingest",
        )
        .await;
    assert!(
        matches!(outcome, Ok(StageOutcome::Failed(ref diagnostic)) if diagnostic.code() == "ORNA-LIST-STREAM-DELIVERY"),
        "invalid delivery must be retained as a stream failure: {outcome:?}"
    );

    let bridge = admit_project_list_stream(
        &project,
        &admit_transaction_project(
            &project,
            Limits::default(),
            "main.ingest",
            &Catalogue::authoritative_core(),
        )
        .expect("project admission"),
        "main.ingest",
        identity(),
    )
    .expect("stream bridge admission");
    let state = RuntimeState::open(&repository, identity(), [24; 32])
        .await
        .expect("runtime state");
    let one = Value::int(1.into()).encode().expect("canonical first key");
    let two = Value::int(2.into()).encode().expect("canonical second key");
    assert!(
        state
            .committed_table_row("Reading", &one)
            .await
            .expect("first row")
            .is_some(),
        "the accepted first delivery must remain committed"
    );
    assert!(
        state
            .committed_table_row("Reading", &two)
            .await
            .expect("second row")
            .is_none(),
        "the rejected second delivery must not publish a row"
    );
    let writer = state.acquire_lease([23; 16]).await.expect("writer lease");
    let checkpoint_key = bridge.checkpoint_key().expect("checkpoint key");
    let checkpoint = state
        .stream_backend(writer)
        .checkpoint_async(&checkpoint_key)
        .await
        .expect("checkpoint");
    assert_eq!(
        checkpoint
            .committed
            .as_ref()
            .map(|position| position.token.as_str()),
        Some("1"),
        "the failed second delivery must not advance the checkpoint"
    );
    let failure = FailureIdentity(DeliveryIdentity {
        consumer: checkpoint_key.consumer.clone(),
        source_format: checkpoint_key.source_format.clone(),
        source: checkpoint_key.source.clone(),
        partition_format: checkpoint_key.partition_format.clone(),
        partition: checkpoint_key.partition.clone(),
        position_format: checkpoint_key.position_format.clone(),
        position: Position {
            token: Component::new("1").expect("canonical second position"),
        },
        successor: Position {
            token: Component::new("2").expect("canonical successor position"),
        },
    });
    let stream = state.stream_backend(writer);
    let failure = stream
        .failure_async(&failure)
        .await
        .expect("failure lookup")
        .expect("failed delivery record");
    assert_eq!(failure.attempts, 1);
    let payload = stream
        .failure_payload_metadata_async(&failure.identity)
        .await
        .expect("failure payload metadata")
        .expect("replay payload metadata");
    assert!(payload.redacted);
    assert!(payload.plaintext_bytes.is_some());
}

fn authoritative_sensor_project() -> ProjectUnit {
    ProjectUnit {
        fixture_id: "reference".into(),
        project_id: "reference".into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: "reference".into(),
            source_id: "sensors.orna".into(),
            parse_as: "module_unit".into(),
            source: r#"
                pub type Sample { pub sensor: Str, pub sequence: Int, pub value: Decimal, }
                pub table Reading(sensor: Str, sequence: Int) { value: Decimal, }
                pub fn input() = Stream.from_list([
                    Sample { sensor: "greenhouse", sequence: 0, value: 18.25 },
                    Sample { sensor: "greenhouse", sequence: 1, value: 18.50 },
                    Sample { sensor: "greenhouse", sequence: 2, value: 18.75 },
                ], source_identity: "example:sensors:v1");
                pub fn ingest() { input() | for_each(sample => {
                    Reading.insert({ sensor: sample.sensor, sequence: sample.sequence, value: sample.value });
                }); }
            "#.into(),
        }],
        loose_rows: Vec::new(),
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.1.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    }
}

#[tokio::test]
async fn project_stream_admits_authoritative_nominal_decimal_composite_readings() {
    let (_temp, repository) = repository();
    let project = authoritative_sensor_project();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

    let outcome = evaluator
        .execute_project_stream(
            &repository,
            identity(),
            [23; 16],
            [24; 32],
            &project,
            "sensors.ingest",
        )
        .await;
    assert!(matches!(outcome, Ok(StageOutcome::Passed)), "{outcome:?}");

    let bridge = admit_project_list_stream(
        &project,
        &admit_transaction_project(
            &project,
            Limits::default(),
            "sensors.ingest",
            &Catalogue::authoritative_core(),
        )
        .expect("authoritative project admission"),
        "sensors.ingest",
        identity(),
    )
    .expect("authoritative stream admission");
    assert!(bridge.source_identity.starts_with("example:sensors:v1:"));
    assert_eq!(bridge.key_fields, ["sensor", "sequence"]);
    let sample = Value::decode(&bridge.payloads[0]).expect("nominal sample payload");
    assert!(matches!(
        sample.raw(),
        OvbRaw::Map(fields) if fields.iter().any(|(name, value)| {
            matches!(name, OvbRaw::Text(name) if name == "value")
                && matches!(value, OvbRaw::Tag(60000, _))
        })
    ));
    let row = evaluate_list_item_row(&bridge, &sample, Limits::default())
        .expect("Reading row projection");
    assert_eq!(
        table_key(&row, &bridge.key_fields).expect("composite Reading key"),
        Value::new(OvbRaw::Array(vec![
            OvbRaw::Text("greenhouse".into()),
            OvbRaw::Int(0.into()),
        ]))
        .expect("canonical key")
        .encode()
        .expect("encoded key")
    );

    let state = RuntimeState::open(&repository, identity(), [24; 32])
        .await
        .expect("reopened runtime");
    assert_eq!(
        state
            .committed_table_rows("sensors.Reading")
            .await
            .unwrap()
            .len(),
        3
    );
    let writer = state.acquire_lease([23; 16]).await.expect("writer lease");
    let checkpoint = state
        .stream_backend(writer)
        .checkpoint_async(&bridge.checkpoint_key().expect("checkpoint key"))
        .await
        .expect("checkpoint");
    assert_eq!(checkpoint.committed.unwrap().token.as_str(), "3");
}

#[tokio::test]
async fn project_list_stream_changed_contents_select_a_fresh_checkpoint() {
    let (_temp, repository) = repository();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let first = authoritative_sensor_project();

    assert!(matches!(
        evaluator
            .execute_project_stream(
                &repository,
                identity(),
                [23; 16],
                [24; 32],
                &first,
                "sensors.ingest",
            )
            .await,
        Ok(StageOutcome::Passed)
    ));

    let first_bridge = admit_project_list_stream(
        &first,
        &admit_transaction_project(
            &first,
            Limits::default(),
            "sensors.ingest",
            &Catalogue::authoritative_core(),
        )
        .expect("first project admission"),
        "sensors.ingest",
        identity(),
    )
    .expect("first stream admission");

    let mut changed = authoritative_sensor_project();
    changed.modules[0].source = changed.modules[0]
        .source
        .replace("sequence: 0", "sequence: 3")
        .replace("sequence: 1", "sequence: 4")
        .replace("sequence: 2", "sequence: 5");
    assert!(matches!(
        evaluator
            .execute_project_stream(
                &repository,
                identity(),
                [23; 16],
                [24; 32],
                &changed,
                "sensors.ingest",
            )
            .await,
        Ok(StageOutcome::Passed)
    ));

    let changed_bridge = admit_project_list_stream(
        &changed,
        &admit_transaction_project(
            &changed,
            Limits::default(),
            "sensors.ingest",
            &Catalogue::authoritative_core(),
        )
        .expect("changed project admission"),
        "sensors.ingest",
        identity(),
    )
    .expect("changed stream admission");
    assert_ne!(first_bridge.source_identity, changed_bridge.source_identity);

    let state = RuntimeState::open(&repository, identity(), [24; 32])
        .await
        .expect("reopened runtime");
    assert_eq!(
        state
            .committed_table_rows("sensors.Reading")
            .await
            .unwrap()
            .len(),
        6,
        "changed literal contents must not resume the predecessor checkpoint"
    );
}

#[tokio::test]
async fn project_transaction_route_skips_admitted_stream_root_before_evaluation() {
    let (_temp, repository) = repository();
    let project = authoritative_sensor_project();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

    assert!(evaluator.project_stream_root_admitted(&project, "sensors.ingest"));
    let outcome = evaluator
        .execute_project(
            &repository,
            identity(),
            [23; 16],
            [24; 32],
            &project,
            "sensors.ingest",
        )
        .await
        .expect("transaction route should return a semantic skip");
    assert!(matches!(
        outcome,
        StageOutcome::Skipped { ref reason }
            if reason == PROJECT_STREAM_ROOT_ADMISSION_REASON
    ));
}

#[tokio::test]
async fn project_stream_preserves_inline_literal_source_support() {
    let (_temp, repository) = repository();
    let mut project = authoritative_sensor_project();
    let source = &mut project.modules[0].source;
    let declaration_start = source
        .find("pub fn input() = ")
        .expect("inline source declaration");
    let declaration_end = source[declaration_start..]
        .find(";\n")
        .map(|offset| declaration_start + offset)
        .expect("inline source declaration terminator");
    let producer =
        source[declaration_start + "pub fn input() = ".len()..declaration_end].to_owned();
    source.replace_range(declaration_start..=declaration_end, "");
    *source = source.replace("input() | for_each", &format!("{producer} | for_each"));

    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let outcome = evaluator
        .execute_project_stream(
            &repository,
            identity(),
            [23; 16],
            [24; 32],
            &project,
            "sensors.ingest",
        )
        .await
        .expect("inline project stream");
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[tokio::test]
async fn project_stream_skips_arbitrary_pipeline_without_publishing_rows_or_checkpoint() {
    let (_temp, repository) = repository();
    let mut project = authoritative_sensor_project();
    project.modules[0].source = project.modules[0].source.replace(
        "                    }); }\n",
        "                    });\n                    input() | for_each(sample => {\n                        Reading.insert({ sensor: sample.sensor, sequence: sample.sequence, value: sample.value });\n                    }); }\n",
    );
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

    let outcome = evaluator
        .execute_project_stream(
            &repository,
            identity(),
            [23; 16],
            [24; 32],
            &project,
            "sensors.ingest",
        )
        .await
        .expect("unsupported project stream must not fail the runtime");
    assert!(
        matches!(outcome, StageOutcome::Skipped { ref reason } if reason.contains("one producer pipeline")),
        "unsupported arbitrary pipeline must be explicitly skipped: {outcome:?}"
    );

    let expected = authoritative_sensor_project();
    let bridge = admit_project_list_stream(
        &expected,
        &admit_transaction_project(
            &expected,
            Limits::default(),
            "sensors.ingest",
            &Catalogue::authoritative_core(),
        )
        .expect("authoritative project admission"),
        "sensors.ingest",
        identity(),
    )
    .expect("authoritative stream admission");
    let state = RuntimeState::open(&repository, identity(), [24; 32])
        .await
        .expect("runtime remains empty after skipped stream");
    assert!(
        state
            .committed_table_rows("Reading")
            .await
            .unwrap()
            .is_empty(),
        "a skipped project stream must not publish rows"
    );
    let writer = state.acquire_lease([23; 16]).await.expect("writer lease");
    let checkpoint = state
        .stream_backend(writer)
        .checkpoint_async(&bridge.checkpoint_key().expect("checkpoint key"))
        .await
        .expect("checkpoint read");
    assert!(
        checkpoint.committed.is_none(),
        "a skipped project stream must not publish a checkpoint"
    );
}

#[tokio::test]
async fn literal_list_stream_reopens_without_duplicate_delivery() {
    let (_temp, repository) = repository();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

    assert!(matches!(
        evaluator
            .execute_source(&repository, identity(), [23; 16], [24; 32], &source())
            .await,
        Ok(StageOutcome::Passed)
    ));
    let state = RuntimeState::open(&repository, identity(), [24; 32])
        .await
        .expect("reopened runtime");
    let first_generation = state
        .capture()
        .await
        .expect("first capture")
        .generation()
        .clone();
    for value in [1, 2] {
        let key = Value::int(value.into()).encode().expect("encoded key");
        assert!(
            state
                .committed_table_row("Reading", &key)
                .await
                .expect("durable row read")
                .is_some()
        );
    }
    drop(state);

    assert!(matches!(
        evaluator
            .execute_source(&repository, identity(), [23; 16], [24; 32], &source())
            .await,
        Ok(StageOutcome::Passed)
    ));
    let state = RuntimeState::open(&repository, identity(), [24; 32])
        .await
        .expect("second reopened runtime");
    assert_eq!(
        state
            .capture()
            .await
            .expect("second capture")
            .generation()
            .clone(),
        first_generation,
        "an exhausted reopened source must not publish duplicate table commits"
    );
}

#[tokio::test]
async fn literal_list_stream_changed_contents_do_not_resume_a_named_predecessor() {
    let (_temp, repository) = repository();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

    assert!(matches!(
        evaluator
            .execute_list_stream_source(&repository, identity(), [23; 16], [24; 32], &source())
            .await,
        Ok(StageOutcome::Passed)
    ));
    let before = RuntimeState::open(&repository, identity(), [24; 32])
        .await
        .expect("reopened first runtime")
        .capture()
        .await
        .expect("first capture")
        .generation()
        .clone();

    assert!(matches!(
        evaluator
            .execute_list_stream_source(
                &repository,
                identity(),
                [23; 16],
                [24; 32],
                &source_with_values("3, 4"),
            )
            .await,
        Ok(StageOutcome::Passed)
    ));
    let state = RuntimeState::open(&repository, identity(), [24; 32])
        .await
        .expect("reopened changed-list runtime");
    assert!(
        state
            .capture()
            .await
            .expect("changed-list capture")
            .generation()
            > &before
    );
    for value in [3, 4] {
        let key = Value::int(value.into()).encode().expect("encoded key");
        assert!(
            state
                .committed_table_row("Reading", &key)
                .await
                .expect("durable row read")
                .is_some(),
            "changed list item {value} must not resume the prior list's checkpoint"
        );
    }
}

#[tokio::test]
async fn literal_list_stream_identity_preserves_payload_boundaries() {
    let (_temp, repository) = repository();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());

    assert!(matches!(
        evaluator
            .execute_list_stream_source(
                &repository,
                identity(),
                [23; 16],
                [24; 32],
                &source_with_values("1, 23"),
            )
            .await,
        Ok(StageOutcome::Passed)
    ));
    let before = RuntimeState::open(&repository, identity(), [24; 32])
        .await
        .expect("reopened first runtime")
        .capture()
        .await
        .expect("first capture")
        .generation()
        .clone();

    assert!(matches!(
        evaluator
            .execute_list_stream_source(
                &repository,
                identity(),
                [23; 16],
                [24; 32],
                &source_with_values("12, 3"),
            )
            .await,
        Ok(StageOutcome::Passed)
    ));
    let state = RuntimeState::open(&repository, identity(), [24; 32])
        .await
        .expect("reopened second runtime");
    assert!(
        state.capture().await.expect("second capture").generation() > &before,
        "distinct canonical payload boundaries must select a fresh checkpoint"
    );
    for value in [12, 3] {
        let key = Value::int(value.into()).encode().expect("encoded key");
        assert!(
            state
                .committed_table_row("Reading", &key)
                .await
                .expect("durable row read")
                .is_some(),
            "boundary-distinct list item {value} must be delivered"
        );
    }
}

struct FailingHandler;

impl StreamHandler for FailingHandler {
    fn handle(&mut self, _: &StreamItem) -> StreamHandlerResult {
        StreamHandlerResult::Fail(stream_handler_diagnostic())
    }
}

fn key() -> CheckpointKey {
    let component = |value| Component::new(value).expect("static component");
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: component("conformance"),
            root: component("handler-failure"),
            function: component("main"),
            binding: component("from_list"),
        },
        source_format: component("orna-stream"),
        source: component("fixture:failure"),
        partition_format: component("literal-list"),
        partition: None,
        position_format: component("orna.list.v1"),
    }
}

#[tokio::test]
async fn handler_failure_does_not_advance_literal_list_checkpoint() {
    let (_temp, repository) = repository();
    let state = RuntimeState::open(&repository, identity(), [24; 32])
        .await
        .expect("runtime");
    let writer = state.acquire_lease([23; 16]).await.expect("writer lease");
    let key = key();
    let payload = Value::int(1.into()).encode().expect("canonical payload");
    let mut source = ListStreamSource::new(key.clone(), vec![payload]);
    let mut handler = FailingHandler;

    let outcome = state
        .run_stream(
            writer,
            &key,
            &mut source,
            &mut handler,
            &orna_runtime::NeverCancelled,
        )
        .await
        .expect("handler failure is a stream outcome");
    assert!(matches!(
        outcome,
        orna_runtime::StreamRunOutcome::Failed { delivered: 0, .. }
    ));
    assert!(
        state
            .stream_backend(writer)
            .checkpoint_async(&key)
            .await
            .expect("checkpoint")
            .committed
            .is_none(),
        "a failed handler must leave the first literal list item uncommitted"
    );
}

#[test]
fn source_ui_action_descriptor_preserves_explicit_provenance_and_input_type() {
    let descriptor = super::BoundedEvaluator::source_action_descriptor(
        r#"std.ui.action("save", as: Text, debug_kind: "button")"#,
    )
    .expect("source action descriptor");
    assert_eq!(descriptor.0, "save");
    assert_eq!(descriptor.1, "std.text");
    assert_ne!(descriptor.2, [0; 32]);
}
