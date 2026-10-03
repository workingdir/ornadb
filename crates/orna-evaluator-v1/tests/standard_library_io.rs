#[path = "support/pinned_io_std.rs"]
mod pinned_io_std;

use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_filesystem_module_requires_a_host_effect_handler() {
    let mut session = pinned_io_std::session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-io-t7auz.orna")),
        Ok(None)
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-call-io-read-text-t7auz.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-UNSUPPORTED"
    );
}

#[test]
fn core_numeric_operations_remain_available_without_optional_io_std() {
    let mut session = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-without-io-t7auz.orna");
    assert_eq!(session.submit(core), Ok(Some(bool_value(true))));
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-io-without-snapshot-t7auz.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(session.submit(core), Ok(Some(bool_value(true))));
}
