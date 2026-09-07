//! Compile-only migration evidence: existing isolated foundation crates can
//! carry the shared types without this crate defining a second harness,
//! parser, or Git representation.

use orna_conformance_v1::StageOutcome;
use orna_foundation_v1::{
    CanonicalSnapshot, CwdCapture, Diagnostic, FileRef, OvbRaw, RowRef,
    SYS_INVOCATION_ARGUMENT_TABLE_ID, SYS_INVOCATION_TABLE_ID, SYS_RUN_TABLE_ID,
    SYS_STREAM_TABLE_ID, SourceSpan, SystemReferenceError, validate_invocation_argument_reference,
    validate_invocation_reference, validate_run_reference, validate_stream_reference,
};
use orna_repository_v1::Repository;
use orna_syntax_v1::parse_expression_with_file;

fn snapshot() -> CanonicalSnapshot {
    CanonicalSnapshot::Commit {
        database: [1; 16],
        algorithm: orna_foundation_v1::GitHash::Sha256,
        oid: vec![2; 32],
    }
}

fn file() -> FileRef {
    FileRef::from_row_ref(
        RowRef::new([1; 16], [2; 16], OvbRaw::Text("file".into()), snapshot()).unwrap(),
    )
}

fn harness_can_carry_shared_diagnostic(_: StageOutcome<Diagnostic>) {}
fn repository_can_be_adapted_later(_: Option<&Repository>) {}

fn cwd() -> CwdCapture {
    CwdCapture::new(
        CanonicalSnapshot::cwd([1; 16], [9; 16], 7.into()).unwrap(),
        [8; 32],
    )
    .unwrap()
}

fn run(capture: &CwdCapture) -> RowRef {
    RowRef::new(
        capture.database_id(),
        SYS_RUN_TABLE_ID,
        OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(vec![4; 16]))),
        capture.snapshot().clone(),
    )
    .unwrap()
}

fn invocation(capture: &CwdCapture) -> RowRef {
    RowRef::new(
        capture.database_id(),
        SYS_INVOCATION_TABLE_ID,
        OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(vec![5; 16]))),
        capture.snapshot().clone(),
    )
    .unwrap()
}

fn encoded_run(run: &RowRef) -> OvbRaw {
    OvbRaw::Tag(
        60_010,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(run.database_id.to_vec()))),
            OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(run.table_id.to_vec()))),
            run.key.clone(),
            run.snapshot.raw(),
        ])),
    )
}

fn stream(capture: &CwdCapture) -> RowRef {
    let run = run(capture);
    RowRef::new(
        capture.database_id(),
        SYS_STREAM_TABLE_ID,
        OvbRaw::Array(vec![
            encoded_run(&run),
            OvbRaw::Text("source".into()),
            OvbRaw::Null,
        ]),
        capture.snapshot().clone(),
    )
    .unwrap()
}

fn invocation_argument(capture: &CwdCapture) -> RowRef {
    let invocation = invocation(capture);
    RowRef::new(
        capture.database_id(),
        SYS_INVOCATION_ARGUMENT_TABLE_ID,
        OvbRaw::Array(vec![encoded_run(&invocation), OvbRaw::Int(0.into())]),
        capture.snapshot().clone(),
    )
    .unwrap()
}

#[test]
fn harness_syntax_and_repository_compile_against_one_foundation_abi() {
    let parsed = parse_expression_with_file("alpha", "src/main.orna");
    let syntax_span = parsed.value.span();
    let shared =
        SourceSpan::from_utf8_offsets(file(), "alpha", syntax_span.start, syntax_span.end).unwrap();
    assert_eq!(shared.start_byte, 0.into());
    harness_can_carry_shared_diagnostic(StageOutcome::Skipped {
        reason: "fixture".into(),
    });
    repository_can_be_adapted_later(None);
}

#[test]
fn runtime_reference_validation_requires_exact_coordinates_and_key_shapes() {
    let capture = cwd();
    let valid_run = run(&capture);
    let valid_stream = stream(&capture);
    assert_eq!(
        validate_run_reference(valid_run.clone(), &capture)
            .unwrap()
            .as_row_ref(),
        &valid_run
    );
    assert_eq!(
        validate_stream_reference(valid_stream.clone(), &capture)
            .unwrap()
            .as_row_ref(),
        &valid_stream
    );

    let wrong_relation = RowRef::new(
        capture.database_id(),
        SYS_STREAM_TABLE_ID,
        valid_run.key.clone(),
        capture.snapshot().clone(),
    )
    .unwrap();
    assert_eq!(
        validate_run_reference(wrong_relation, &capture),
        Err(SystemReferenceError::RelationMismatch)
    );

    let wrong_database = RowRef::new(
        [2; 16],
        SYS_RUN_TABLE_ID,
        valid_run.key.clone(),
        capture.snapshot().clone(),
    )
    .unwrap();
    assert_eq!(
        validate_run_reference(wrong_database, &capture),
        Err(SystemReferenceError::DatabaseMismatch)
    );

    let wrong_snapshot = RowRef::new(
        capture.database_id(),
        SYS_RUN_TABLE_ID,
        valid_run.key.clone(),
        CanonicalSnapshot::cwd([1; 16], [9; 16], 8.into()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        validate_run_reference(wrong_snapshot, &capture),
        Err(SystemReferenceError::SnapshotMismatch)
    );

    let invalid_run_key = RowRef::new(
        capture.database_id(),
        SYS_RUN_TABLE_ID,
        OvbRaw::Array(vec![]),
        capture.snapshot().clone(),
    )
    .unwrap();
    assert_eq!(
        validate_run_reference(invalid_run_key, &capture),
        Err(SystemReferenceError::InvalidRunKey)
    );

    let invalid_stream_key = RowRef::new(
        capture.database_id(),
        SYS_STREAM_TABLE_ID,
        OvbRaw::Array(vec![OvbRaw::Text("not-a-run-reference".into())]),
        capture.snapshot().clone(),
    )
    .unwrap();
    assert_eq!(
        validate_stream_reference(invalid_stream_key, &capture),
        Err(SystemReferenceError::InvalidStreamKey)
    );

    for invalid_id in [
        OvbRaw::Text("run-id-is-not-a-uuid".into()),
        OvbRaw::Tag(
            60_000,
            Box::new(OvbRaw::Array(vec![
                OvbRaw::Int(1.into()),
                OvbRaw::Int(0.into()),
            ])),
        ),
    ] {
        let invalid_run = RowRef::new(
            capture.database_id(),
            SYS_RUN_TABLE_ID,
            invalid_id,
            capture.snapshot().clone(),
        )
        .unwrap();
        assert_eq!(
            validate_run_reference(invalid_run, &capture),
            Err(SystemReferenceError::InvalidRunKey)
        );
    }

    let wrong_embedded_relation = RowRef::new(
        capture.database_id(),
        SYS_STREAM_TABLE_ID,
        OvbRaw::Array(vec![
            encoded_run(
                &RowRef::new(
                    capture.database_id(),
                    SYS_STREAM_TABLE_ID,
                    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(vec![4; 16]))),
                    capture.snapshot().clone(),
                )
                .unwrap(),
            ),
            OvbRaw::Text("source".into()),
            OvbRaw::Null,
        ]),
        capture.snapshot().clone(),
    )
    .unwrap();
    assert_eq!(
        validate_stream_reference(wrong_embedded_relation, &capture),
        Err(SystemReferenceError::InvalidStreamKey)
    );

    let malformed_embedded = OvbRaw::Tag(60_010, Box::new(OvbRaw::Array(vec![])));
    assert!(
        RowRef::new(
            capture.database_id(),
            SYS_STREAM_TABLE_ID,
            OvbRaw::Array(vec![
                malformed_embedded,
                OvbRaw::Text("source".into()),
                OvbRaw::Null,
            ]),
            capture.snapshot().clone(),
        )
        .is_err()
    );
}

#[test]
fn invocation_reference_validation_requires_declared_relation_and_natural_keys() {
    let capture = cwd();
    let valid_invocation = invocation(&capture);
    let valid_argument = invocation_argument(&capture);
    assert_eq!(
        validate_invocation_reference(valid_invocation.clone(), &capture)
            .unwrap()
            .as_row_ref(),
        &valid_invocation
    );
    assert_eq!(
        validate_invocation_argument_reference(valid_argument.clone(), &capture)
            .unwrap()
            .as_row_ref(),
        &valid_argument
    );

    let wrong_relation = RowRef::new(
        capture.database_id(),
        SYS_INVOCATION_ARGUMENT_TABLE_ID,
        valid_invocation.key.clone(),
        capture.snapshot().clone(),
    )
    .unwrap();
    assert_eq!(
        validate_invocation_reference(wrong_relation, &capture),
        Err(SystemReferenceError::RelationMismatch)
    );

    let wrong_database = RowRef::new(
        [2; 16],
        SYS_INVOCATION_TABLE_ID,
        valid_invocation.key.clone(),
        capture.snapshot().clone(),
    )
    .unwrap();
    assert_eq!(
        validate_invocation_reference(wrong_database, &capture),
        Err(SystemReferenceError::DatabaseMismatch)
    );

    let wrong_snapshot = RowRef::new(
        capture.database_id(),
        SYS_INVOCATION_TABLE_ID,
        valid_invocation.key.clone(),
        CanonicalSnapshot::cwd([1; 16], [9; 16], 8.into()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        validate_invocation_reference(wrong_snapshot, &capture),
        Err(SystemReferenceError::SnapshotMismatch)
    );

    for invalid_key in [
        OvbRaw::Text("not-an-invocation-id".into()),
        OvbRaw::Array(vec![]),
    ] {
        let reference = RowRef::new(
            capture.database_id(),
            SYS_INVOCATION_TABLE_ID,
            invalid_key,
            capture.snapshot().clone(),
        )
        .unwrap();
        assert_eq!(
            validate_invocation_reference(reference, &capture),
            Err(SystemReferenceError::InvalidInvocationKey)
        );
    }

    let wrong_embedded_relation = RowRef::new(
        capture.database_id(),
        SYS_INVOCATION_ARGUMENT_TABLE_ID,
        OvbRaw::Array(vec![
            encoded_run(
                &RowRef::new(
                    capture.database_id(),
                    SYS_RUN_TABLE_ID,
                    valid_invocation.key.clone(),
                    capture.snapshot().clone(),
                )
                .unwrap(),
            ),
            OvbRaw::Int(0.into()),
        ]),
        capture.snapshot().clone(),
    )
    .unwrap();
    assert_eq!(
        validate_invocation_argument_reference(wrong_embedded_relation, &capture),
        Err(SystemReferenceError::InvalidInvocationArgumentKey)
    );

    for invalid_key in [
        OvbRaw::Array(vec![encoded_run(&valid_invocation)]),
        OvbRaw::Array(vec![
            encoded_run(&valid_invocation),
            OvbRaw::Int((-1).into()),
        ]),
        OvbRaw::Array(vec![
            encoded_run(&valid_invocation),
            OvbRaw::Int(num_bigint::BigInt::from(u64::MAX) + 1),
        ]),
    ] {
        let reference = RowRef::new(
            capture.database_id(),
            SYS_INVOCATION_ARGUMENT_TABLE_ID,
            invalid_key,
            capture.snapshot().clone(),
        )
        .unwrap();
        assert_eq!(
            validate_invocation_argument_reference(reference, &capture),
            Err(SystemReferenceError::InvalidInvocationArgumentKey)
        );
    }
}
