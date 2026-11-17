//! A run stores the catalogue identity of its function and a stream those of its producer and consumer, in the
//! snapshot each pins. The `sys.Run` and `sys.Stream` projections build their references from those identities and
//! fail closed on a missing or unconfirmed one, and `sys.rt.runs` serves the live runs (ornadb-jdq5h, hir-host.md A5
//! slice HA8).

use super::*;
use crate::catalogue::CatalogueTypeForm;
use crate::observation::{RuntimeObservationFence, StreamObservation};
use crate::stream_carry::consumer_identity_digest;
use crate::{
    SYS_RT_RELATIONS, SysCurrentRuntimeProjection, capture_sys_rt, read_stream_carry,
    runtime_host_rows,
};
use libsql::Value as LibsqlValue;
use orna_exec::{Host, MemoryStore, Value as ExecValue};
use orna_value::{Raw, RowRef, object_reference};

/// A catalogued run and its stream, registered in the snapshot their catalogue was admitted in.
struct Observed {
    owner: WriterLease,
    run: RunObservation,
    stream: StreamObservation,
}

async fn observed(state: &RuntimeState) -> Observed {
    let owner = state.acquire_lease(id(10)).await.unwrap();
    admit_observed_names(state, owner, &["pkg.consume"], &["source-object"]).await;
    let key = stream_delivery("one", "two").checkpoint_key();
    let request = request(11, 12);
    state.reserve_request(request, digest(13)).await.unwrap();
    let run = state
        .register_run_observation(RunObservationRegistration {
            request,
            consumer_identity: key.consumer.clone(),
            function: "pkg.consume".into(),
            source_identity: None,
            invocation_id: id(14),
        })
        .await
        .unwrap();
    let stream = state
        .register_stream_observation(StreamObservationRegistration {
            run: run.id,
            producer: "source-object".into(),
            consumer: Some("pkg.consume".into()),
            checkpoint: key,
        })
        .await
        .unwrap();
    Observed { owner, run, stream }
}

/// Overwrites one column of one observation row, as a legacy or damaged database would hold it.
async fn overwrite(
    state: &RuntimeState,
    table: &str,
    column: &str,
    key: &[u8; 16],
    value: LibsqlValue,
) {
    let key_column = if table == "sys_run_observation" {
        "run_id"
    } else {
        "stream_id"
    };
    state
        .connection
        .execute(
            &format!("UPDATE {table} SET {column} = ?1 WHERE {key_column} = ?2"),
            params![value, key.to_vec()],
        )
        .await
        .unwrap();
}

async fn assert_fails_closed(state: &RuntimeState, fence: &RuntimeObservationFence) {
    assert_eq!(
        state.retained_sys_observation_projections().await,
        Err(RuntimeError::RecoveryInvalid)
    );
    assert_eq!(
        state.current_runtime_sys_projections(fence).await,
        Err(RuntimeError::RecoveryInvalid)
    );
}

async fn assert_projects(state: &RuntimeState, fence: &RuntimeObservationFence) {
    state.retained_sys_observation_projections().await.unwrap();
    state.current_runtime_sys_projections(fence).await.unwrap();
}

#[tokio::test]
async fn projected_function_producer_and_consumer_are_the_admitted_object_ids() {
    let (_temp, repo) = repository();
    let state = open_state(&repo).await;
    let seen = observed(&state).await;
    let function = state
        .catalogue_function("pkg.consume")
        .await
        .unwrap()
        .unwrap();
    let producer = state
        .catalogue_type("source-object", CatalogueTypeForm::Named)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(seen.run.function_object_id, Some(function.object.object_id));
    assert_eq!(seen.stream.producer_object_id, Some(producer.object_id()));
    assert_eq!(
        seen.stream.consumer_object_id,
        Some(function.object.object_id)
    );
    let projections = state.retained_sys_observation_projections().await.unwrap();
    assert_eq!(
        Some(&projections.runs[0].function),
        function.object.function_reference.as_ref()
    );
    let expected_producer = object_reference(
        seen.run.snapshot.database_id(),
        producer.object_id(),
        seen.run.snapshot.snapshot(),
    )
    .unwrap();
    assert_eq!(projections.streams[0].producer, expected_producer);
    assert_eq!(
        projections.streams[0].consumer.as_ref(),
        function.object.function_reference.as_ref()
    );
    let fence = state.runtime_observation_fence(seen.owner).await.unwrap();
    let current = state.current_runtime_sys_projections(&fence).await.unwrap();
    assert_eq!(current.runs, projections.runs);
    assert_eq!(current.streams, projections.streams);
}

#[tokio::test]
async fn a_function_the_snapshot_does_not_catalogue_stores_null_and_fails_closed() {
    // An absent name, and a name the catalogue binds to a type rather than a function.
    for function in ["pkg.unknown", "source-object"] {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let owner = state.acquire_lease(id(10)).await.unwrap();
        admit_observed_names(&state, owner, &["pkg.known"], &["source-object"]).await;
        let fence = state.runtime_observation_fence(owner).await.unwrap();
        let request = request(20, 21);
        state.reserve_request(request, digest(22)).await.unwrap();
        let run = state
            .register_run_observation(RunObservationRegistration {
                request,
                consumer_identity: stream_delivery("one", "two").consumer,
                function: function.into(),
                source_identity: None,
                invocation_id: id(23),
            })
            .await
            .unwrap();
        assert_eq!(run.function_object_id, None);
        assert_eq!(run.function_reference(), Err(RuntimeError::RecoveryInvalid));
        assert_eq!(
            SysRunProjection::try_from(&run),
            Err(RuntimeError::RecoveryInvalid)
        );
        assert_fails_closed(&state, &fence).await;
        // The observation itself stays retained and readable.
        assert_eq!(state.run_observation(run.id).await.unwrap(), Some(run));
    }
}

#[tokio::test]
async fn a_producer_or_consumer_the_snapshot_does_not_catalogue_stores_null_and_fails_closed() {
    let (_temp, repo) = repository();
    let state = open_state(&repo).await;
    let seen = observed(&state).await;
    let mut absent_producer = stream_delivery("one", "two").checkpoint_key();
    absent_producer.source = Component::new("absent-producer").unwrap();
    let mut consumer_is_a_type = stream_delivery("one", "two").checkpoint_key();
    consumer_is_a_type.source = Component::new("consumer-is-a-type").unwrap();
    for (key, producer, consumer) in [
        (absent_producer, "absent-object", None),
        (
            consumer_is_a_type,
            "source-object",
            Some("source-object".to_owned()),
        ),
    ] {
        let stream = state
            .register_stream_observation(StreamObservationRegistration {
                run: seen.run.id,
                producer: producer.into(),
                consumer,
                checkpoint: key,
            })
            .await
            .unwrap();
        assert!(stream.producer_object_id.is_none() || stream.consumer_object_id.is_none());
        assert_eq!(
            SysStreamProjection::try_from_observation(&stream, &seen.run),
            Err(RuntimeError::RecoveryInvalid)
        );
    }
}

#[tokio::test]
async fn a_stored_identity_the_snapshot_does_not_confirm_fails_closed() {
    let (_temp, repo) = repository();
    let state = open_state(&repo).await;
    let seen = observed(&state).await;
    let function = state
        .catalogue_function("pkg.consume")
        .await
        .unwrap()
        .unwrap()
        .object
        .object_id;
    let producer = state
        .catalogue_type("source-object", CatalogueTypeForm::Named)
        .await
        .unwrap()
        .unwrap()
        .object_id();
    let fence = state.runtime_observation_fence(seen.owner).await.unwrap();
    let (run, stream) = (seen.run.id.0, seen.stream.id.0);
    assert_projects(&state, &fence).await;

    let blob = |id: [u8; 16]| LibsqlValue::Blob(id.to_vec());
    let text = |value: &str| LibsqlValue::Text(value.to_owned());
    let (run_table, stream_table) = ("sys_run_observation", "sys_stream_observation");
    // Each case damages one column, as another object's id or as a name its snapshot does not catalogue, then
    // restores it: another object's id under the producer's name; the producer's own id under another name; a type's id
    // under the function's name; a consumer id that is not the consumer's; a consumer name that names no function.
    let cases = [
        (
            stream_table,
            "producer_object_id",
            stream,
            blob(function),
            blob(producer),
        ),
        (
            stream_table,
            "producer",
            stream,
            text("other-object"),
            text("source-object"),
        ),
        (
            run_table,
            "function_object_id",
            run,
            blob(producer),
            blob(function),
        ),
        (
            stream_table,
            "consumer_object_id",
            stream,
            blob(producer),
            blob(function),
        ),
        (
            stream_table,
            "consumer_name",
            stream,
            text("source-object"),
            text("pkg.consume"),
        ),
    ];
    for (table, column, key, damaged, intact) in cases {
        overwrite(&state, table, column, &key, damaged).await;
        assert_fails_closed(&state, &fence).await;
        // The raw observations still load: only the projections refuse them.
        assert!(state.run_observation(seen.run.id).await.unwrap().is_some());
        assert!(
            state
                .stream_observation(seen.stream.id)
                .await
                .unwrap()
                .is_some()
        );
        overwrite(&state, table, column, &key, intact).await;
        assert_projects(&state, &fence).await;
    }
}

#[tokio::test]
async fn null_identity_columns_fail_closed_like_a_row_retained_before_the_migration() {
    let (_temp, repo) = repository();
    let state = open_state(&repo).await;
    let seen = observed(&state).await;
    let fence = state.runtime_observation_fence(seen.owner).await.unwrap();
    let (run, stream) = (seen.run.id.0, seen.stream.id.0);
    assert_projects(&state, &fence).await;

    for (table, column, key, original) in [
        (
            "sys_run_observation",
            "function_object_id",
            run,
            seen.run.function_object_id.unwrap(),
        ),
        (
            "sys_stream_observation",
            "producer_object_id",
            stream,
            seen.stream.producer_object_id.unwrap(),
        ),
        (
            "sys_stream_observation",
            "consumer_object_id",
            stream,
            seen.stream.consumer_object_id.unwrap(),
        ),
    ] {
        overwrite(&state, table, column, &key, LibsqlValue::Null).await;
        assert_fails_closed(&state, &fence).await;
        // The observation stays retained, with no identity in that column.
        let run = state.run_observation(seen.run.id).await.unwrap().unwrap();
        let stream = state
            .stream_observation(seen.stream.id)
            .await
            .unwrap()
            .unwrap();
        let stored = match column {
            "function_object_id" => run.function_object_id,
            "producer_object_id" => stream.producer_object_id,
            _ => stream.consumer_object_id,
        };
        assert_eq!(stored, None);
        overwrite(
            &state,
            table,
            column,
            &key,
            LibsqlValue::Blob(original.to_vec()),
        )
        .await;
        assert_projects(&state, &fence).await;
    }
}

#[tokio::test]
async fn a_database_from_before_the_migration_gains_null_identities_and_fails_closed() {
    let (_temp, repo) = repository();
    let (run, stream) = {
        let state = open_state(&repo).await;
        let seen = observed(&state).await;
        for statement in [
            "ALTER TABLE sys_run_observation DROP COLUMN function_object_id",
            "ALTER TABLE sys_stream_observation DROP COLUMN producer_object_id",
            "ALTER TABLE sys_stream_observation DROP COLUMN consumer_object_id",
        ] {
            state.connection.execute(statement, ()).await.unwrap();
        }
        (seen.run.id, seen.stream.id)
    };

    let reopened = open_state(&repo).await;
    let run = reopened.run_observation(run).await.unwrap().unwrap();
    let stream = reopened.stream_observation(stream).await.unwrap().unwrap();
    assert_eq!(run.function_object_id, None);
    assert_eq!(stream.producer_object_id, None);
    assert_eq!(stream.consumer_object_id, None);
    assert_eq!(
        SysRunProjection::try_from(&run),
        Err(RuntimeError::RecoveryInvalid)
    );
    assert_eq!(
        SysStreamProjection::try_from_observation(&stream, &run),
        Err(RuntimeError::RecoveryInvalid)
    );
    assert_eq!(
        reopened.retained_sys_observation_projections().await,
        Err(RuntimeError::RecoveryInvalid)
    );
    // Reopening again leaves the columns, and a migrated database keeps its rows.
    drop(reopened);
    let again = open_state(&repo).await;
    assert!(again.run_observation(run.id).await.unwrap().is_some());
}

#[tokio::test]
async fn a_carry_reads_null_for_a_source_that_predates_the_identity_columns() {
    let database = Builder::new_local(":memory:").build().await.unwrap();
    let connection = database.connect().unwrap();
    connection
        .execute("CREATE TABLE observation (function_object_id BLOB)", ())
        .await
        .unwrap();
    assert_eq!(
        crate::run_identity::carried_column(&connection, "observation", "function_object_id")
            .await
            .unwrap(),
        "function_object_id"
    );
    assert_eq!(
        crate::run_identity::carried_column(&connection, "observation", "producer_object_id")
            .await
            .unwrap(),
        "NULL"
    );
}

fn field<'a>(row: &'a Raw, name: &str) -> &'a Raw {
    let Raw::Map(entries) = row else {
        panic!("a runtime row is a record");
    };
    entries
        .iter()
        .find(|(key, _)| *key == Raw::Text(name.to_owned()))
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("the row has a {name} field"))
}

fn exec_raw(value: ExecValue) -> Raw {
    value.to_raw().unwrap()
}

/// The canonical form of a checked reference, which the executor reads back as the typed reference.
fn reference_raw(reference: &RowRef) -> Raw {
    reference.to_canonical().unwrap().raw().clone()
}

/// The `sys.rt` projection that captures two live runs of catalogued functions.
async fn two_live_runs() -> SysCurrentRuntimeProjection {
    let (_temp, repo) = repository();
    let state = open_state(&repo).await;
    let owner = state.acquire_lease(id(10)).await.unwrap();
    admit_observed_names(&state, owner, &["pkg.main", "pkg.other"], &[]).await;
    let consumer = stream_delivery("one", "two").consumer;
    for (function, session) in [("pkg.main", 11), ("pkg.other", 21)] {
        let request = request(session, session + 1);
        state
            .reserve_request(request, digest(session + 2))
            .await
            .unwrap();
        state
            .register_run_observation(RunObservationRegistration {
                request,
                consumer_identity: consumer.clone(),
                function: function.into(),
                source_identity: Some("table books".into()),
                invocation_id: id(session + 3),
            })
            .await
            .unwrap();
    }
    capture_sys_rt(&state, owner).await.unwrap()
}

/// Every row `sys.rt.runs` answers over the host built from `projection`, or the code it refuses with.
fn runs_relation(projection: &SysCurrentRuntimeProjection) -> Result<Vec<Raw>, &'static str> {
    let mut host = runtime_host_rows(Box::new(MemoryStore::new()), projection);
    let slot = host
        .manifest()
        .slot("sys.rt.runs")
        .expect("sys.rt.runs links");
    let mut rows = host
        .sys_relation(slot, None)
        .map_err(|failure| failure.code)?;
    let mut served = Vec::new();
    while let Some(row) = rows.next().map_err(|failure| failure.code)? {
        served.push(row);
    }
    Ok(served)
}

/// `row` is the `sys.Run` record of `run`: the contract's fields in order, each in its canonical form.
fn assert_sys_run_row(row: &Raw, run: &SysRunProjection) {
    let Raw::Map(entries) = row else {
        panic!("a runtime row is a record");
    };
    let names = entries
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let contract = [
        "reference",
        "id",
        "consumer_identity",
        "function",
        "source_identity",
        "snapshot",
        "started",
        "ended",
        "status_at_snapshot",
        "observed_at",
        "runtime_id",
        "live",
        "invocation",
        "checkpoint_count",
        "failure",
    ];
    assert_eq!(
        names,
        contract
            .iter()
            .map(|name| Raw::Text((*name).to_owned()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        field(row, "function"),
        &exec_raw(ExecValue::function_ref(run.function.clone()).unwrap())
    );
    assert_eq!(
        field(row, "reference"),
        &reference_raw(run.reference.as_row_ref())
    );
    assert_eq!(
        field(row, "snapshot"),
        &reference_raw(run.snapshot.as_row_ref())
    );
    assert_eq!(
        field(row, "invocation"),
        &reference_raw(run.invocation.as_row_ref())
    );
    let consumer = &run.consumer_identity;
    let digest = consumer_identity_digest(
        consumer.principal.as_str(),
        consumer.root.as_str(),
        consumer.function.as_str(),
        consumer.binding.as_str(),
    )
    .unwrap();
    assert_eq!(
        field(row, "consumer_identity"),
        &Raw::Tag(
            orna_value::OPAQUE_SYSTEM_ID_TAG,
            Box::new(Raw::Array(vec![
                Raw::Text("sys.ConsumerIdentity".into()),
                Raw::Bytes(digest.to_vec())
            ]))
        )
    );
    assert_eq!(field(row, "live"), &Raw::Bool(true));
    assert_eq!(field(row, "checkpoint_count"), &Raw::Int(BigInt::from(0)));
}

#[tokio::test]
async fn sys_rt_runs_serves_two_live_runs_as_canonical_sys_run_rows() {
    let projection = two_live_runs().await;
    assert_eq!(projection.runs.len(), 2);
    let served = runs_relation(&projection).unwrap();
    // The acceptance of HA8: `sys.rt.runs | count` is 2.
    assert_eq!(served.len(), 2);
    for (row, run) in served.iter().zip(&projection.runs) {
        assert_sys_run_row(row, run);
    }
}

#[tokio::test]
async fn sys_rt_runs_stays_unsupported_while_a_run_records_a_failure() {
    let mut projection = two_live_runs().await;
    // No row can stand in for a `sys.Diagnostic`, and a relation with a missing row would answer a count wrongly.
    projection.runs[1].failure = Some(SafeDiagnostic {
        code: DiagnosticCode::ExecutionRejected,
        class: DiagnosticClass::Permanent,
    });
    assert_eq!(runs_relation(&projection), Err("ORNA-EVAL-UNSUPPORTED"));
}

#[tokio::test]
async fn sys_rt_runs_answers_an_empty_relation_and_every_other_member_stays_unsupported() {
    let mut projection = two_live_runs().await;
    projection.runs.clear();
    assert_eq!(runs_relation(&projection), Ok(Vec::new()));
    let mut host = runtime_host_rows(Box::new(MemoryStore::new()), &projection);
    for relation in SYS_RT_RELATIONS
        .iter()
        .filter(|relation| **relation != "runs")
    {
        let slot = host
            .manifest()
            .slot(&format!("sys.rt.{relation}"))
            .expect("every member links");
        let failure = host.sys_relation(slot, None).err().expect("unsupported");
        assert_eq!(failure.code, "ORNA-EVAL-UNSUPPORTED");
    }
}

#[tokio::test]
async fn capture_sys_rt_refuses_a_run_the_catalogue_does_not_name() {
    let (_temp, repo) = repository();
    let state = open_state(&repo).await;
    let owner = state.acquire_lease(id(10)).await.unwrap();
    admit_observed_names(&state, owner, &["pkg.known"], &[]).await;
    let request = request(11, 12);
    state.reserve_request(request, digest(13)).await.unwrap();
    state
        .register_run_observation(RunObservationRegistration {
            request,
            consumer_identity: stream_delivery("one", "two").consumer,
            function: "pkg.unknown".into(),
            source_identity: None,
            invocation_id: id(14),
        })
        .await
        .unwrap();
    assert_eq!(
        capture_sys_rt(&state, owner).await,
        Err(RuntimeError::RecoveryInvalid)
    );
}

#[tokio::test]
async fn a_carry_of_a_source_that_predates_the_identity_columns_reads_null_identities() {
    let (_temp, repo) = repository();
    let state = open_state(&repo).await;
    let seen = observed(&state).await;
    state
        .complete_request_with_owner(request(11, 12), digest(13), seen.owner, outcome(15))
        .await
        .unwrap();
    let columns = [
        ("sys_run_observation", "function_object_id"),
        ("sys_stream_observation", "producer_object_id"),
        ("sys_stream_observation", "consumer_object_id"),
    ];
    for (table, column) in columns {
        let null = format!("UPDATE {table} SET {column} = NULL");
        state.connection.execute(&null, ()).await.unwrap();
    }
    let with_null_identities = read_stream_carry(&repo).await.unwrap();
    for (table, column) in columns {
        let drop = format!("ALTER TABLE {table} DROP COLUMN {column}");
        state.connection.execute(&drop, ()).await.unwrap();
    }
    // A migration reads its source without opening it as a runtime, so the source may never have run the migration.
    assert_eq!(
        read_stream_carry(&repo).await.unwrap(),
        with_null_identities
    );
}
