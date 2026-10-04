use orna_evaluator_v1::{
    AdmittedReplSession, Limits, reference_standard_profile, reference_standard_sources,
};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

#[path = "support/pinned_time_text_std.rs"]
mod pinned_time_text_std;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("boolean is canonical")
}

#[test]
fn pinned_text_exports_obey_the_unicode_16_contract() {
    let mut session = pinned_time_text_std::text_math_session();
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
    let mut with_std = pinned_time_text_std::text_math_session();
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

#[test]
fn text_module_is_captured_in_the_reference_profile() {
    assert_eq!(unicode_case_mapping::UNICODE_VERSION, (16, 0, 0));
    assert_eq!(unicode_normalization::UNICODE_VERSION, (16, 0, 0));

    let sources = reference_standard_sources();
    let text_sources = sources
        .iter()
        .filter(|(path, _)| path == "std/text.orna")
        .collect::<Vec<_>>();
    assert_eq!(text_sources.len(), 1, "std/text.orna is pinned exactly once");

    let profile = reference_standard_profile();
    let (path, source) = text_sources[0];
    profile
        .verify_source(path, source)
        .expect("the bundled text source matches its captured profile");

    let mut changed_source = source.clone();
    changed_source.push_str("\n// changed after profile capture\n");
    assert!(profile.verify_source(path, &changed_source).is_err());
}
