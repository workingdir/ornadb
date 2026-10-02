use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_random_hash_modules_are_optional_and_do_not_replace_core() {
    let mut with_std = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-random-dmkss.orna")),
        Ok(None)
    );
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-hash-dmkss.orna")),
        Ok(None)
    );
    assert_eq!(
        with_std
            .submit(include_str!("fixtures/stdlib-call-random-dmkss.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
    assert_eq!(
        with_std
            .submit(include_str!("fixtures/stdlib-call-hash-dmkss.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-without-collections-i7bat.orna");
    assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));
    for module_import in [
        include_str!("fixtures/stdlib-use-random-dmkss.orna"),
        include_str!("fixtures/stdlib-use-hash-dmkss.orna"),
    ] {
        assert_eq!(without_std.submit(module_import).unwrap_err().code(), "ORNA-S010-IMPORT");
        assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));
    }
}
