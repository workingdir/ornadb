use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

#[path = "support/pinned_time_text_std.rs"]
mod pinned_time_text_std;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

#[test]
fn pinned_math_and_text_exports_bind_to_their_runtime_intrinsics() {
    let mut session = pinned_time_text_std::text_math_session();
    assert_eq!(session.submit(include_str!("fixtures/stdlib-use-math-2189.orna")), Ok(None));
    assert_eq!(session.submit(include_str!("fixtures/stdlib-use-text-2189.orna")), Ok(None));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-text-alias-2189.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-math-intrinsics-2189.orna")),
        Ok(Some(canonical(Raw::Bool(true))))
    );
    assert_eq!(session.submit(include_str!("fixtures/stdlib-text-trim-2189.orna")), Ok(Some(canonical(Raw::Text("café".into())))));
    assert_eq!(session.submit(include_str!("fixtures/stdlib-text-split-2189.orna")), Ok(Some(canonical(Raw::Array(vec![Raw::Text("alpha".into()), Raw::Text("".into()), Raw::Text("β".into()), Raw::Text("".into())])))));
    assert_eq!(session.submit(include_str!("fixtures/stdlib-text-split-scalars-2189.orna")), Ok(Some(canonical(Raw::Array(vec![Raw::Text("a".into()), Raw::Text("β".into())])))));
    assert_eq!(session.submit(include_str!("fixtures/stdlib-text-join-2189.orna")), Ok(Some(canonical(Raw::Text("a:β:".into())))));
    assert_eq!(session.submit(include_str!("fixtures/stdlib-text-starts-with-2189.orna")), Ok(Some(canonical(Raw::Bool(true)))));
    assert_eq!(session.submit(include_str!("fixtures/stdlib-text-ends-with-2189.orna")), Ok(Some(canonical(Raw::Bool(true)))));
    assert_eq!(session.submit(include_str!("fixtures/stdlib-text-contains-2189.orna")), Ok(Some(canonical(Raw::Bool(true)))));
    assert_eq!(session.submit(include_str!("fixtures/stdlib-text-replace-2189.orna")), Ok(Some(canonical(Raw::Text("bb".into())))));
    assert_eq!(session.submit(include_str!("fixtures/stdlib-text-normalise-2189.orna")), Ok(Some(canonical(Raw::Text("Café".into())))));
    assert_eq!(session.submit(include_str!("fixtures/stdlib-text-lower-2189.orna")), Ok(Some(canonical(Raw::Text("i\u{307}ς".into())))));
    assert_eq!(session.submit(include_str!("fixtures/stdlib-text-upper-2189.orna")), Ok(Some(canonical(Raw::Text("SS".into())))));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-text-lower-2189.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-text-imported-lower-2189.orna")),
        Ok(Some(canonical(Raw::Text("hello".into()))))
    );
}

#[test]
fn text_intrinsics_are_not_ambient_without_the_optional_std_snapshot() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-text-without-snapshot-2189.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S012-UNRESOLVED"
    );
}
