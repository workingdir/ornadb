use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn captured_sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            path == "std/collection.orna"
                || path == "std/error.orna"
                || path == "std/error/combinators.orna"
        })
        .collect()
}

fn session() -> AdmittedReplSession {
    let sources = captured_sources();
    assert_eq!(
        sources.len(),
        3,
        "the proof loads only its pinned dependencies"
    );
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/wt345-error-combinators",
        sources.clone(),
    )
    .expect("error combinator dependencies form a captured source profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("error combinators resolve against pinned std.error and std.collection");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .expect("the selected error combinator modules load")
}

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected result is canonical")
}

#[test]
fn error_context_combinators_preserve_and_extend_ordered_cause_chains() {
    let mut session = session();
    for (fixture, expected) in [
        (
            include_str!("fixtures/stdlib-error-combinators-context-wt345.orna"),
            vec![
                "outer.failure",
                "inner.failure",
                "root.failure",
                "leaf.failure",
            ],
        ),
        (
            include_str!("fixtures/stdlib-error-combinators-contexts-wt345.orna"),
            vec![
                "outer.failure",
                "middle.failure",
                "inner.failure",
                "root.failure",
                "leaf.failure",
            ],
        ),
        (
            include_str!("fixtures/stdlib-error-combinators-empty-contexts-wt345.orna"),
            vec!["root.failure", "leaf.failure"],
        ),
    ] {
        let result = session
            .submit(fixture)
            .unwrap_or_else(|error| panic!("error context proof failed: {}", error.code()));
        assert_eq!(
            result,
            Some(canonical(Raw::Array(
                expected
                    .into_iter()
                    .map(|code| Raw::Text(code.into()))
                    .collect(),
            ))),
        );
    }
}

#[test]
fn error_contains_code_predicates_compute_membership() {
    let mut session = session();
    for (fixture, expected) in [
        (
            include_str!("fixtures/stdlib-error-combinators-contains-code-found-wt345.orna"),
            true,
        ),
        (
            include_str!("fixtures/stdlib-error-combinators-contains-code-missing-wt345.orna"),
            false,
        ),
    ] {
        assert_eq!(
            session
                .submit(fixture)
                .unwrap_or_else(|error| panic!("contains_code proof failed: {}", error.code())),
            Some(canonical(Raw::Bool(expected))),
        );
    }
}

#[test]
fn error_contains_any_codes_predicate_checks_membership() {
    let mut session = session();
    for (fixture, expected) in [
        (
            include_str!("fixtures/stdlib-error-combinators-contains-any-found-wt345.orna"),
            true,
        ),
        (
            include_str!("fixtures/stdlib-error-combinators-contains-any-missing-wt345.orna"),
            false,
        ),
    ] {
        assert_eq!(
            session
                .submit(fixture)
                .unwrap_or_else(|error| panic!("contains_any_code proof failed: {}", error.code())),
            Some(canonical(Raw::Bool(expected))),
        );
    }
}

#[test]
fn error_contains_all_codes_predicate_checks_membership_and_empty_input() {
    let mut session = session();
    for (fixture, expected) in [
        (
            include_str!("fixtures/stdlib-error-combinators-contains-all-found-wt345.orna"),
            true,
        ),
        (
            include_str!("fixtures/stdlib-error-combinators-contains-all-missing-wt345.orna"),
            false,
        ),
        (
            include_str!("fixtures/stdlib-error-combinators-contains-all-empty-wt345.orna"),
            true,
        ),
    ] {
        assert_eq!(
            session.submit(fixture).unwrap_or_else(|error| panic!(
                "contains_all_codes proof failed: {}",
                error.code()
            )),
            Some(canonical(Raw::Bool(expected))),
        );
    }
}

#[test]
fn error_matches_code_chain_requires_order_and_length() {
    let mut session = session();
    for (fixture, expected) in [
        (
            include_str!("fixtures/stdlib-error-combinators-matches-chain-exact-wt345.orna"),
            true,
        ),
        (
            include_str!("fixtures/stdlib-error-combinators-matches-chain-order-wt345.orna"),
            false,
        ),
        (
            include_str!("fixtures/stdlib-error-combinators-matches-chain-length-wt345.orna"),
            false,
        ),
    ] {
        assert_eq!(
            session.submit(fixture).unwrap_or_else(|error| panic!(
                "matches_code_chain proof failed: {}",
                error.code()
            )),
            Some(canonical(Raw::Bool(expected))),
        );
    }
}
