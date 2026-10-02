use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

#[test]
fn pinned_regex_package_and_pattern_combinators_are_importable() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("the captured standard source bundle includes regex and pattern");

    assert_eq!(session.submit(include_str!("fixtures/stdlib-use-regex-g8rd3.orna")), Ok(None));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-regex-version-g8rd3.orna")),
        Ok(Some(canonical(Raw::Text("orna.regex/1".into()))))
    );
    assert_eq!(session.submit(include_str!("fixtures/stdlib-use-pattern-g8rd3.orna")), Ok(None));
    // The package is present and typechecked, but matching fails closed until
    // an evaluator explicitly supplies the pinned dialect implementation.
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-regex-compile-g8rd3.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
}

#[test]
fn core_remains_usable_without_optional_regex_or_pattern_modules() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-core-without-regex-g8rd3.orna")),
        Ok(Some(canonical(Raw::Bool(true))))
    );
    for import in [
        include_str!("fixtures/stdlib-regex-without-snapshot-g8rd3.orna"),
        include_str!("fixtures/stdlib-pattern-without-snapshot-g8rd3.orna"),
    ] {
        assert_eq!(session.submit(import).unwrap_err().code(), "ORNA-S010-IMPORT");
        assert_eq!(
            session.submit(include_str!("fixtures/stdlib-core-without-regex-g8rd3.orna")),
            Ok(Some(canonical(Raw::Bool(true))))
        );
    }
}
