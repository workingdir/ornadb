use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("proof result is canonical")
}

#[test]
fn list_concat_identity_and_associativity_hold() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-list-i7bat.orna")),
        Ok(None)
    );
    assert_eq!(session.submit("let empty: [Int] = [];"), Ok(None));
    let fixture = include_str!("fixtures/stdlib-list-concat-identity-ln4y7.orna");
    for (index, expression) in fixture.split("\n&& ").enumerate() {
        let expression = expression.trim_start_matches("&& ").trim();
        let actual = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "list concat identity {index} failed: {} ({expression})",
                error.code()
            )
        });
        assert_eq!(
            actual,
            Some(boolean(true)),
            "list concat identity {index}: {expression}"
        );
    }
}
