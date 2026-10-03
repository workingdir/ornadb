use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn captured_test_source() -> (String, String) {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .find(|(path, _)| path == "std/test.orna")
        .expect("the pinned std source bundle includes std.test")
}

fn pinned_test_session() -> AdmittedReplSession {
    let sources = vec![captured_test_source()];
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/frf2c-testing-assertions",
        sources.clone(),
    )
    .expect("std.test source bytes form a captured dependency profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("std.test exports resolve against core without importing a host runner");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .unwrap_or_else(|error| panic!("could not load captured std.test: {}", error.code()))
}

#[test]
fn std_test_expect_and_should_contracts_return_their_documented_boolean_values() {
    let mut session = pinned_test_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-test-assert-frf2c.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-test-typed-absence-frf2c.orna")),
        Ok(None)
    );

    for contract in [
        include_str!("fixtures/stdlib-test-expect-contracts-frf2c.orna"),
        include_str!("fixtures/stdlib-test-should-contracts-frf2c.orna"),
    ] {
        for clause in contract.split("&&") {
            let clause = clause.trim();
            let result = session.submit(clause).unwrap_or_else(|error| {
                panic!("std.test contract {clause:?} failed: {}", error.code())
            });
            assert_eq!(result, Some(bool_value(true)), "contract: {clause}");
        }
    }
}

#[test]
fn std_test_source_is_bound_to_the_captured_snapshot() {
    let source = captured_test_source();
    let (path, contents) = &source;
    let parsed = orna_syntax_v1::parse_module_with_file(contents, path);
    assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);

    let profile = StandardDependencyProfile::from_sources(
        "orna.std/frf2c-testing-assertions",
        vec![source.clone()],
    )
    .expect("the test module source is captured exactly");
    profile
        .verify_source(path, contents)
        .expect("the pinned test module bytes match the captured snapshot");
    let mut changed = contents.clone();
    changed.push_str("\n// changed after snapshot capture\n");
    assert!(profile.verify_source(path, &changed).is_err());
    Catalogue::authoritative_core()
        .with_standard_sources(&profile, vec![source])
        .expect("the captured std.test module resolves against the core catalogue");
}

#[test]
fn core_assertions_remain_available_without_the_optional_test_module() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-use-test-without-snapshot-frf2c.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-core-assertion-without-test-frf2c.orna")),
        Ok(None)
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-core-assertion-call-without-test-frf2c.orna")),
        Ok(Some(bool_value(true)))
    );
}
