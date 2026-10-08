use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const MODULES: [&str; 3] = ["std/collection.orna", "std/list.orna", "std/sorting.orna"];

fn session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), MODULES.len());
    let profile = StandardDependencyProfile::from_sources("orna.std/list-search", sources.clone())
        .expect("list, sorting and collection modules form a pinned profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("list search modules resolve against the pinned collection module");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), Limits::default())
            .unwrap_or_else(|error| panic!("list search modules failed to load: {}", error.code()));
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed list search source matches its captured profile");
    }
    for (index, fixture) in [
        include_str!("fixtures/std-list-search-use.orna"),
        include_str!("fixtures/std-list-search-sorting-use.orna"),
        include_str!("fixtures/std-list-search-empty.orna"),
    ]
    .into_iter()
    .enumerate()
    {
        session.submit(fixture).unwrap_or_else(|error| {
            panic!("list search fixture setup {index} failed: {}", error.code())
        });
    }
    session
}

/// Every `&&`-separated expression in the fixture must evaluate to `true`.
fn assert_true_fixture(session: &mut AdmittedReplSession, fixture: &str) {
    for expression in fixture.split("&&").map(str::trim) {
        let parsed = orna_syntax_v1::parse_repl(expression);
        assert!(parsed.is_ok(), "{expression}: {:#?}", parsed.diagnostics);
        let result = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "list search proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(
            result,
            Some(CanonicalValue::new(Raw::Bool(true)).expect("true is canonical")),
            "list search fixture failed: {expression}",
        );
    }
}

#[test]
fn list_results_compose_with_sorting_and_predicate_search() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/std-list-search-behavior.orna"),
    );
}

#[test]
fn one_fails_on_zero_and_on_multiple_matches() {
    let mut session = session();
    for fixture in [
        include_str!("fixtures/std-list-search-one-none.orna"),
        include_str!("fixtures/std-list-search-one-many.orna"),
    ] {
        assert!(
            session.submit(fixture).is_err(),
            "one must reject a cardinality other than exactly one: {fixture}"
        );
    }
}
