use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("proof result is canonical")
}

fn reference_map_session() -> AdmittedReplSession {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference standard failed to load: {}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-collection-alias-yfifu.orna")),
        Ok(None)
    );
    assert_eq!(session.submit("use std.map as map;"), Ok(None));
    assert_eq!(session.submit("let empty_pairs: [(Str, Int)] = [];"), Ok(None));
    session
}

#[test]
fn map_merge_with_an_empty_side_is_identity_on_the_other_side() {
    let mut session = reference_map_session();
    let fixture = include_str!("fixtures/stdlib-map-empty-side-ln4y7.orna");
    for (index, expression) in fixture.split("\n&& ").enumerate() {
        let actual = session.submit(expression).unwrap_or_else(|error| {
            panic!("map empty-side behavior {index} failed: {} ({expression})", error.code())
        });
        assert_eq!(actual, Some(boolean(true)), "map empty-side behavior {index}: {expression}");
    }
}
