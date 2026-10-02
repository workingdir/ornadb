use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn process_and_environment_host_effects_fail_closed_without_bindings() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-io-t7auz.orna"))
        .unwrap_or_else(|error| panic!("std.io import failed: {}", error.code()));

    for source in [
        include_str!("fixtures/stdlib-io-process-run-host-call-xbf3n.orna"),
        include_str!("fixtures/stdlib-io-environment-get-host-call-xbf3n.orna"),
        include_str!("fixtures/stdlib-io-environment-require-host-call-xbf3n.orna"),
    ] {
        assert_eq!(
            session.submit(source).unwrap_err().code(),
            "ORNA-EVAL-ERROR"
        );
    }
}

#[test]
fn core_remains_available_when_process_and_environment_modules_are_absent() {
    let mut session = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-without-process-environment-xbf3n.orna");
    assert_eq!(
        session
            .submit(core)
            .unwrap_or_else(|error| panic!("core source failed without std: {}", error.code())),
        Some(bool_value(true))
    );
    for source in [
        include_str!("fixtures/stdlib-io-process-without-snapshot-xbf3n.orna"),
        include_str!("fixtures/stdlib-io-environment-without-snapshot-xbf3n.orna"),
    ] {
        assert_eq!(
            session.submit(source).unwrap_err().code(),
            "ORNA-S010-IMPORT"
        );
    }
    assert_eq!(
        session.submit(core).unwrap_or_else(|error| panic!(
            "core source failed after missing import: {}",
            error.code()
        )),
        Some(bool_value(true))
    );
}
