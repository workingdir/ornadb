use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn io_path_edge_cases_for_empty_dot_and_dotfile_names_have_documented_results() {
    let contract = include_str!("fixtures/stdlib-io-path-edge-contract-ogp1.orna");
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-io-t7auz.orna"))
        .unwrap_or_else(|error| panic!("std.io import failed: {}", error.code()));
    for (index, expression) in contract.split(" &&\n").enumerate() {
        assert_eq!(
            session.submit(expression).unwrap_or_else(|error| {
                panic!(
                    "path edge clause {index} failed: {}: {}",
                    error.code(),
                    error.diagnostic().message()
                )
            }),
            Some(bool_value(true)),
            "path edge clause {index} returned false"
        );
    }
}
