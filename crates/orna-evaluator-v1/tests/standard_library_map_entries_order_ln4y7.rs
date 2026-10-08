use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("proof result is canonical")
}

#[test]
fn map_entries_preserve_first_insertion_order() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(session.submit("use std.map as map;"), Ok(None));
    let fixture = include_str!("fixtures/stdlib-map-entries-order-ln4y7.orna");
    for (index, expression) in fixture.split("\n&& ").enumerate() {
        let expression = expression.trim_start_matches("&& ").trim();
        let actual = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "map entries order {index} failed: {} ({expression})",
                error.code()
            )
        });
        assert_eq!(
            actual,
            Some(boolean(true)),
            "map entries order {index}: {expression}"
        );
    }
}
