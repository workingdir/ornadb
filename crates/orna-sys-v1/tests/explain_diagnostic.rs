use std::collections::BTreeMap;

use orna_sys_v1::{
    Diagnostic, DiagnosticExplanationError, DiagnosticField, SystemEffect,
    SYS_EXPLAIN_DIAGNOSTIC_DESCRIPTOR, TypeId, explain_diagnostic, system_function_descriptor,
};

#[test]
fn sys_explain_diagnostic_descriptor_is_authoritative_and_read_only() {
    let descriptor = system_function_descriptor("sys.explain(Diagnostic)")
        .expect("portable diagnostic explanation descriptor");
    assert_eq!(descriptor, &SYS_EXPLAIN_DIAGNOSTIC_DESCRIPTOR);
    assert_eq!(descriptor.name, "sys.explain(Diagnostic)");
    assert_eq!(descriptor.effect, SystemEffect::Read);
    assert_eq!(
        descriptor.signature,
        "fn sys.explain(diagnostic: sys.Diagnostic): sys.Explanation"
    );
    assert_eq!(descriptor.purpose, "Return structured causal explanation.");
    assert!(system_function_descriptor("sys.explain(Unknown)").is_none());
}

#[test]
fn sys_explain_diagnostic_returns_known_result_without_leaking_redacted_fields() {
    let diagnostic = Diagnostic {
        code: "sys.invoke.argument_unknown",
        message: "root-secret-value",
        fields: BTreeMap::from([
            (
                "argument".to_owned(),
                DiagnosticField::Redacted {
                    static_type: TypeId::new("sys.Secret"),
                },
            ),
            (
                "detail".to_owned(),
                DiagnosticField::Text("nested-secret-value".to_owned()),
            ),
        ]),
        causes: vec![Diagnostic {
            code: "vendor.cause",
            message: "cause-secret-value",
            fields: BTreeMap::from([(
                "cause-detail".to_owned(),
                DiagnosticField::Text("cause-field-secret".to_owned()),
            )]),
            causes: Vec::new(),
        }],
    };
    let input_encoded = serde_json::to_string(&diagnostic).expect("safe input encoding");
    for secret in [
        "root-secret-value",
        "nested-secret-value",
        "cause-secret-value",
        "cause-field-secret",
        "cause-detail",
    ] {
        assert!(
            !input_encoded.contains(secret),
            "serialized input leaked {secret}"
        );
    }

    let explanation = explain_diagnostic(diagnostic).expect("known diagnostic explanation");
    assert_eq!(explanation.summary, "<redacted>");
    assert_eq!(explanation.causes.len(), 1);
    assert_eq!(explanation.causes[0].message, "<redacted>");
    assert_eq!(
        explanation.causes[0]
            .fields
            .values()
            .next()
            .expect("causal field preserved"),
        &DiagnosticField::Redacted {
            static_type: TypeId::new("Str"),
        }
    );
    assert!(!format!("{:?}", explanation).contains("cause-detail"));
    assert_eq!(
        explanation.suggestions,
        vec!["bind only parameters declared by the target function".to_owned()]
    );
    assert!(explanation.related_objects.is_empty());
    assert!(explanation.plan.is_none());
    let encoded = serde_json::to_string(&explanation).expect("safe explanation encoding");
    assert!(encoded.contains("redacted"));
    for secret in [
        "root-secret-value",
        "nested-secret-value",
        "cause-secret-value",
        "cause-field-secret",
        "cause-detail",
    ] {
        assert!(!encoded.contains(secret), "serialized explanation leaked {secret}");
    }
}

#[test]
fn sys_explain_diagnostic_preserves_unknown_codes_but_rejects_invalid_input() {
    let unknown = Diagnostic {
        code: "vendor.future.diagnostic",
        message: "vendor supplied safe message",
        fields: BTreeMap::new(),
        causes: Vec::new(),
    };
    let explanation = explain_diagnostic(unknown).expect("unknown diagnostic explanation");
    assert_eq!(explanation.diagnostic.code, "vendor.future.diagnostic");
    assert_eq!(explanation.summary, "<redacted>");
    assert!(explanation.suggestions.is_empty());

    let invalid = Diagnostic {
        code: "",
        message: "not admitted",
        fields: BTreeMap::new(),
        causes: Vec::new(),
    };
    assert_eq!(
        explain_diagnostic(invalid),
        Err(DiagnosticExplanationError::EmptyCode)
    );

    let invalid_message = Diagnostic {
        code: "vendor.invalid",
        message: "contains\ncontrol",
        fields: BTreeMap::new(),
        causes: Vec::new(),
    };
    assert_eq!(
        explain_diagnostic(invalid_message),
        Err(DiagnosticExplanationError::InvalidMessage)
    );
}
