use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn networking_url_modules_are_optional_and_host_effects_are_separated() {
    let mut with_std = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    let imports = with_std.submit(include_str!("fixtures/stdlib-use-net-bc963.orna"));
    assert!(
        imports == Ok(None),
        "optional std imports failed: {:?}",
        imports.err().map(|error| error.code().to_owned())
    );
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-url-bc963.orna")),
        Ok(None)
    );
    assert_eq!(
        with_std
            .submit(include_str!("fixtures/stdlib-call-net-url-bc963.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-UNSUPPORTED"
    );
    for network_call in [
        include_str!("fixtures/stdlib-call-net-http-bc963.orna"),
        include_str!("fixtures/stdlib-call-net-websocket-bc963.orna"),
    ] {
        assert_eq!(
            with_std.submit(network_call).unwrap_err().code(),
            "ORNA-REPL-EFFECT"
        );
    }

    let mut without_std = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-without-net-bc963.orna");
    assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-use-net-missing-bc963.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-use-url-missing-bc963.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(without_std.submit(core), Ok(Some(bool_value(true))));
}
