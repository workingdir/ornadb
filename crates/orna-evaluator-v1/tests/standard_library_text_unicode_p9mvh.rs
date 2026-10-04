use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("boolean is canonical")
}

#[test]
fn pinned_text_exports_obey_the_unicode_16_contract() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("the pinned reference standard loads");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-text-use-p9mvh.orna")),
        Ok(None)
    );

    for assertion in include_str!("fixtures/stdlib-text-unicode-contract-p9mvh.orna")
        .split("&&")
        .map(str::trim)
    {
        assert_eq!(
            session.submit(assertion),
            Ok(Some(bool_value(true))),
            "text contract failed: {assertion}"
        );
    }
}

#[test]
fn invalid_normalisation_form_is_rejected_and_text_requires_the_snapshot() {
    let mut with_std = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("the pinned reference standard loads");
    with_std
        .submit(include_str!("fixtures/stdlib-text-use-p9mvh.orna"))
        .expect("text import succeeds");
    assert_eq!(
        with_std
            .submit(include_str!(
                "fixtures/stdlib-text-invalid-normalisation-p9mvh.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );

    let mut core = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        core.submit(include_str!(
            "fixtures/stdlib-text-without-snapshot-p9mvh.orna"
        ))
        .unwrap_err()
        .code(),
        "ORNA-S012-UNRESOLVED"
    );
}
