use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

#[test]
fn pinned_query_exports_execute_their_source_bodies() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-query-2213.orna")),
        Ok(None)
    );
    let expected = Some(CanonicalValue::new(Raw::Bool(true)).unwrap());
    let fixture = include_str!("fixtures/stdlib-query-complete-behavior-yn4vz.orna");
    for (index, behavior) in fixture.split("\n&& ").enumerate() {
        assert_eq!(
            session
                .submit(behavior)
                .unwrap_or_else(|error| panic!("query behavior {index} failed: {} ({behavior})", error.code())),
            expected,
            "query behavior {index}: {behavior}"
        );
    }
}
