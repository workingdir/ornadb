//! Publishes `Str` rows past the inline row limit through the production writer
//! and reads them back from the published store, by key and by range
//! (store-format-3 section 5, ORNA-ROW-003).
//!
//! The rows are admitted through runtime request commits and published by
//! `RuntimePublicationCoordinator` (`prepare_allow_noop` -> `publish` ->
//! `complete`), the sequence `orna publish` runs. No conformance evaluator
//! writes any part of the store, and the read side uses the same native graph
//! reads the query verb uses.
//!
//! Three shapes are covered. A field over 8 KiB is lowered to its own overflow
//! node, and the row keeps an inline tuple that names it. Two fields of 6 KiB
//! each keep no overflowing child, so the row tuple itself exceeds 8 KiB and
//! the row is stored as an overflow node. A small row stays inline with no
//! overflow dependency.

use orna_repository::{
    AdmittedRow, KeyRange, NativeGraphContext, RepositoryReadScope, RowValue, TypedKey,
};
use orna_runtime::{
    NoFault, RequestIdentity, RequestState, RuntimeState, TableMutation, TableObjectId,
    TerminalOutcome,
};
use orna_storage::RuntimePublicationCoordinator;
use orna_value::{ContextValue, Raw, ValueFormat};

#[path = "../../orna-cli-testkit/src/format3.rs"]
mod format3;

const NOTE_SOURCE: &str = "pub table Note(id: Str) { title: Str, body: Str, }\n";
const TUPLE_OVERFLOW_KEY: &str = "note-tuple-overflow";
const CHILD_OVERFLOW_KEY: &str = "note-child-overflow";
const INLINE_KEY: &str = "note-inline";
const SHORT_TITLE: &str = "short title";
const INLINE_BODY: &str = "short body";

/// Six KiB of text: under the 8 KiB field limit, so it stays inline as a
/// field, but two of them push the row tuple past the 8 KiB row limit.
fn six_kib_text(seed: char) -> String {
    std::iter::repeat_n(seed, 6 * 1024).collect()
}

/// Sixteen KiB of text: over the field limit, so the field is an overflow node.
fn sixteen_kib_text() -> String {
    "0123456789abcdef".repeat(1024)
}

/// The two fields of a `Note` row, in declaration order.
fn stored_fields(
    graph: &NativeGraphContext,
    scope: &RepositoryReadScope,
    row: &AdmittedRow,
    ctx: &orna_value::Format3Context,
) -> (String, String) {
    let fields = graph
        .stored_row_fields(row, scope)
        .expect("read the stored row fields");
    let decoded = ContextValue::decode_in_context(&fields, ValueFormat::Rov3, ctx)
        .expect("decode the stored row tuple");
    let Raw::Array(values) = decoded.raw() else {
        panic!("the stored row tuple is an array");
    };
    let [Raw::Text(title), Raw::Text(body)] = values.as_slice() else {
        panic!("the Note tuple stores exactly two Str fields");
    };
    (title.clone(), body.clone())
}

#[tokio::test]
async fn overflow_text_rows_publish_and_read_back_through_production_writer() {
    let tuple_title = six_kib_text('a');
    let tuple_body = six_kib_text('b');
    let child_body = sixteen_kib_text();
    let rows = [
        (
            TUPLE_OVERFLOW_KEY,
            tuple_title.as_str(),
            tuple_body.as_str(),
        ),
        (CHILD_OVERFLOW_KEY, SHORT_TITLE, child_body.as_str()),
        (INLINE_KEY, SHORT_TITLE, INLINE_BODY),
    ];

    let (_repository_dir, repository, relation_id, ctx) =
        format3::format3_repository_with_context(NOTE_SOURCE, "Note");
    let (identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("open runtime state");
    let writer = state
        .acquire_lease(identity.repository_id)
        .await
        .expect("acquire runtime writer");

    for (index, (key, title, body)) in rows.iter().enumerate() {
        let ordinal = u8::try_from(index + 1).expect("three fixture rows fit a byte");
        let request_identity = RequestIdentity {
            session_id: [ordinal; 16],
            request_id: [ordinal + 1; 16],
        };
        let fingerprint = [ordinal + 2; 32];
        let (_, admission) = state
            .reserve_request_with_admission(request_identity, fingerprint)
            .await
            .expect("reserve the request");
        state
            .start_request_with_owner_and_admission(
                request_identity,
                fingerprint,
                writer,
                admission.expect("new request returns its admission capability"),
            )
            .await
            .expect("start the request");
        let activation = state.begin_activation().await.expect("begin activation");

        let value = ContextValue::new_in_context(
            &ctx,
            ValueFormat::Rov3,
            Raw::Array(vec![
                Raw::Text((*title).to_owned()),
                Raw::Text((*body).to_owned()),
            ]),
        )
        .and_then(|value| value.encode())
        .expect("encode the Note fields as a ROV-3 tuple");
        let mutation =
            TableMutation::insert([ordinal + 3; 16], "Note", key.as_bytes().to_vec(), value)
                .expect("the Note row is a new row")
                .with_table_object_id(TableObjectId::new(relation_id));
        let committed = state
            .commit_table_request_activation(
                writer,
                request_identity,
                fingerprint,
                &activation,
                &[mutation],
                [ordinal + 4; 32],
                TerminalOutcome::new(vec![ordinal + 5]).expect("terminal outcome"),
                &NoFault,
                orna_evaluator::EffectGate::admit_unbound_for_test(),
            )
            .await
            .expect("commit the Note row through the runtime");
        assert_eq!(committed.request.state, RequestState::Completed);
    }

    let checkpoint = state
        .latest_checkpoint()
        .await
        .expect("read latest checkpoint")
        .expect("admitted rows leave a checkpoint");
    let freeze = state
        .publication_writer(&repository)
        .expect("publication lease")
        .freeze([0x9a; 16], &checkpoint)
        .await
        .expect("freeze admitted rows");
    let head = repository
        .head()
        .expect("read repository head")
        .expect("format-3 fixture has a head");
    let index = repository
        .index_generation()
        .expect("read repository index generation");
    let mut coordinator = RuntimePublicationCoordinator::prepare_allow_noop(
        &repository,
        &state,
        &head,
        index,
        &freeze,
        "orna: publish overflow text rows",
    )
    .await
    .expect("storage writer prepares production publication")
    .expect("admitted rows are a visible change");
    coordinator
        .publish(&repository)
        .expect("storage writer publishes the rows");
    coordinator
        .complete(&repository, &state, &freeze)
        .await
        .expect("complete production publication");

    let format = repository
        .open_format_context()
        .expect("open the format context");
    let row_map = format
        .load_row_map(relation_id)
        .expect("load the relation row map");
    assert_eq!(row_map.version().row_count(), Some(3));
    let graph = format
        .open_native_graph(&row_map)
        .expect("open the native graph");
    let scope = graph.open_read_scope().expect("open a read scope");

    let expected = |key: &str| -> (String, String) {
        let (_, title, body) = rows
            .iter()
            .find(|(row_key, _, _)| *row_key == key)
            .expect("fixture row exists");
        ((*title).to_owned(), (*body).to_owned())
    };

    let by_key = |key: &str| -> AdmittedRow {
        graph
            .lookup_row(&TypedKey::Text(key.to_owned()), &scope)
            .expect("bounded point lookup")
            .expect("the row is published")
    };

    let tuple_row = by_key(TUPLE_OVERFLOW_KEY);
    assert!(
        matches!(tuple_row.value(), RowValue::Overflow(_)),
        "a row tuple past 8 KiB is stored as an overflow node"
    );
    assert_eq!(
        stored_fields(&graph, &scope, &tuple_row, &ctx),
        expected(TUPLE_OVERFLOW_KEY)
    );

    let child_row = by_key(CHILD_OVERFLOW_KEY);
    match child_row.value() {
        RowValue::Inline { dependencies, .. } => assert!(
            !dependencies.is_empty(),
            "a field past 8 KiB is an overflow node named by the inline row"
        ),
        RowValue::Overflow(_) => panic!("a 16 KiB field alone keeps the row tuple inline"),
    }
    assert_eq!(
        stored_fields(&graph, &scope, &child_row, &ctx),
        expected(CHILD_OVERFLOW_KEY)
    );

    let inline_row = by_key(INLINE_KEY);
    match inline_row.value() {
        RowValue::Inline { dependencies, .. } => assert!(
            dependencies.is_empty(),
            "a small row has no overflow dependency"
        ),
        RowValue::Overflow(_) => panic!("a small row is stored inline"),
    }
    assert_eq!(
        stored_fields(&graph, &scope, &inline_row, &ctx),
        expected(INLINE_KEY)
    );

    let range = KeyRange::new(None, None, 16).expect("key range");
    let ranged = graph
        .range_rows(&range, &scope)
        .expect("bounded range read");
    assert_eq!(ranged.len(), 3, "the range returns every published row");
    let overflow_rows = ranged
        .iter()
        .filter(|row| matches!(row.value(), RowValue::Overflow(_)))
        .count();
    assert_eq!(
        overflow_rows, 1,
        "exactly the tuple past 8 KiB is an overflow row"
    );
    let mut ranged_fields: Vec<(String, String)> = ranged
        .iter()
        .map(|row| stored_fields(&graph, &scope, row, &ctx))
        .collect();
    ranged_fields.sort();
    let mut expected_fields: Vec<(String, String)> =
        rows.iter().map(|(key, _, _)| expected(key)).collect();
    expected_fields.sort();
    assert_eq!(
        ranged_fields, expected_fields,
        "the range serves every body intact"
    );
}
