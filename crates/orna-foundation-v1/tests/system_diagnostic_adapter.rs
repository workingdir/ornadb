use orna_foundation_v1::{
    CanonicalSnapshot, DiagnosticKind, DiagnosticLabel, DiagnosticSeverity, FileKind, OvbRaw,
    RowRef, SourceSpan, SysValue, SystemDiagnostic, SystemDiagnosticAdapterError, Value,
    system_diagnostic_explanation_input,
};

fn snapshot() -> CanonicalSnapshot {
    CanonicalSnapshot::Commit {
        database: [1; 16],
        algorithm: orna_foundation_v1::GitHash::Sha256,
        oid: vec![2; 32],
    }
}

fn reference<Kind>(table_id: [u8; 16], key: &str) -> orna_foundation_v1::TypedRowRef<Kind> {
    orna_foundation_v1::TypedRowRef::from_row_ref(
        RowRef::new([3; 16], table_id, OvbRaw::Text(key.to_owned()), snapshot()).unwrap(),
    )
}

fn span() -> SourceSpan {
    SourceSpan::new(
        reference::<FileKind>([4; 16], "file"),
        2.into(),
        7.into(),
        1.into(),
        3.into(),
        1.into(),
        8.into(),
    )
    .unwrap()
}

fn uuid_raw(bytes: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(bytes.to_vec())))
}

fn diagnostic() -> SystemDiagnostic {
    let currency = [9; 16];
    let type_node = OvbRaw::Array(vec![OvbRaw::Int(8.into()), uuid_raw(currency)]);
    let amount = OvbRaw::Tag(
        60000,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Int(1.into()),
            OvbRaw::Int(0.into()),
        ])),
    );
    let money = OvbRaw::Tag(
        60007,
        Box::new(OvbRaw::Array(vec![amount, uuid_raw(currency)])),
    );
    let data = SysValue::from_value(
        Value::new(OvbRaw::Tag(
            60026,
            Box::new(OvbRaw::Array(vec![type_node, money])),
        ))
        .unwrap(),
    )
    .unwrap();
    let cause = SystemDiagnostic {
        reference: reference::<DiagnosticKind>([5; 16], "cause"),
        id: "cause-id".into(),
        severity: DiagnosticSeverity::Warning,
        code: "ORNA-CAUSE".into(),
        message: "<redacted>".into(),
        object: Some(reference([6; 16], "cause-object")),
        definition: None,
        primary_span: Some(span()),
        labels: vec![],
        causes: vec![],
        help: vec!["use the safe source context".into()],
        data: None,
        redacted: true,
        trace: Some(reference([7; 16], "cause-trace")),
    };
    SystemDiagnostic {
        reference: reference::<DiagnosticKind>([5; 16], "root"),
        id: "root-id".into(),
        severity: DiagnosticSeverity::Error,
        code: "ORNA-ROOT".into(),
        message: "<redacted>".into(),
        object: Some(reference([6; 16], "root-object")),
        definition: Some(reference([8; 16], "root-definition")),
        primary_span: Some(span()),
        labels: vec![DiagnosticLabel {
            span: span(),
            message: "<redacted>".into(),
            primary: true,
        }],
        causes: vec![cause],
        help: vec!["inspect the retained diagnostic".into()],
        data: Some(data),
        redacted: true,
        trace: Some(reference([7; 16], "root-trace")),
    }
}

#[test]
fn adapter_keeps_correlation_metadata_and_redacts_causal_payloads() {
    let canonical = diagnostic();
    let output = system_diagnostic_explanation_input(canonical.clone())
        .unwrap()
        .finish("<redacted>", vec!["inspect retained context".into()])
        .unwrap();

    assert_eq!(output.diagnostic().reference, canonical.reference);
    assert_eq!(output.diagnostic().id, canonical.id);
    assert_eq!(output.diagnostic().code, canonical.code);
    assert_eq!(output.diagnostic().trace, canonical.trace);
    assert_eq!(output.summary(), "<redacted>");
    assert_eq!(output.suggestions()[0], "inspect retained context");
    assert!(output.diagnostic().redacted);
    assert!(output.diagnostic().causes[0].redacted);
    assert_eq!(output.diagnostic().message, "<redacted>");
    assert_eq!(output.diagnostic().labels[0].message, "<redacted>");
    assert_eq!(output.diagnostic().help[0], "<redacted>");
    assert_eq!(output.diagnostic().causes[0].message, "<redacted>");
    assert_eq!(output.diagnostic().causes[0].help[0], "<redacted>");
    assert_eq!(output.diagnostic().object, canonical.object);
    assert_eq!(output.diagnostic().definition, canonical.definition);
    assert_eq!(output.diagnostic().primary_span, canonical.primary_span);
    assert_eq!(output.diagnostic().labels[0].span, canonical.labels[0].span);
    assert_eq!(output.diagnostic().data, None);
}

fn secret_sys_value(secret: &str) -> SysValue {
    let descriptor = OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Str".into())]);
    SysValue::from_value(
        Value::new(OvbRaw::Tag(
            60026,
            Box::new(OvbRaw::Array(vec![descriptor, OvbRaw::Text(secret.into())])),
        ))
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn diagnostic_redaction_does_not_depend_on_an_attached_trace() {
    let fixture = include_str!("fixtures/secret-surface.orna").trim();
    for trace in [None, diagnostic().trace] {
        let mut candidate = diagnostic();
        candidate.trace = trace.clone();
        candidate.message = fixture.to_owned();
        candidate.labels[0].message = fixture.to_owned();
        candidate.help[0] = fixture.to_owned();
        candidate.data = Some(secret_sys_value(fixture));
        candidate.redacted = false;
        candidate.causes[0].message = fixture.to_owned();
        candidate.causes[0].help[0] = fixture.to_owned();
        candidate.causes[0].redacted = false;

        let debug = format!("{candidate:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(fixture));

        let input = system_diagnostic_explanation_input(candidate).unwrap();
        let output = input
            .finish(fixture, [fixture.to_owned()])
            .unwrap();
        assert_eq!(output.diagnostic().trace, trace);
        assert_eq!(output.diagnostic().message, "<redacted>");
        assert_eq!(output.diagnostic().labels[0].message, "<redacted>");
        assert_eq!(output.diagnostic().help[0], "<redacted>");
        assert_eq!(output.diagnostic().data, None);
        assert_eq!(output.diagnostic().causes[0].message, "<redacted>");
        assert_eq!(output.diagnostic().causes[0].help[0], "<redacted>");
        let output_debug = format!("{output:?}");
        assert!(output_debug.contains("<redacted>"));
        assert!(!output_debug.contains(fixture));
    }
}

#[test]
fn adapter_rejects_unsafe_or_invalid_canonical_input_with_typed_failure() {
    let mut unsafe_text = diagnostic();
    unsafe_text.message = "bad\0message".into();
    assert_eq!(
        system_diagnostic_explanation_input(unsafe_text),
        Err(SystemDiagnosticAdapterError::UnsafeText { field: "message" })
    );

    let mut invalid_span = diagnostic();
    invalid_span.primary_span.as_mut().unwrap().start_byte = (-1).into();
    assert_eq!(
        system_diagnostic_explanation_input(invalid_span),
        Err(SystemDiagnosticAdapterError::InvalidSpan)
    );
}

#[test]
fn adapter_does_not_turn_authority_fields_into_flattened_live_diagnostic() {
    let canonical = diagnostic();
    let input = system_diagnostic_explanation_input(canonical.clone()).unwrap();
    assert_eq!(input.diagnostic().reference, canonical.reference);
    assert_eq!(input.diagnostic().code, canonical.code);
    assert_eq!(input.diagnostic().message, "<redacted>");
    assert!(input.diagnostic().data.is_none());
    assert!(input.diagnostic().trace.is_some());
    assert!(input.diagnostic().reference.as_row_ref().key != OvbRaw::Null);
}
