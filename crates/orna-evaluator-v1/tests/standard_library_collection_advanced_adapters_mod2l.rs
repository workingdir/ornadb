use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const MODULES: [&str; 2] = ["std/collection.orna", "std/collection/advanced.orna"];

fn session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), MODULES.len());
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/mod2l-collection-advanced-adapters",
        sources.clone(),
    )
    .expect("collection modules form a pinned advanced adapter profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("advanced adapters resolve against the pinned collection module");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), Limits::default())
            .unwrap_or_else(|error| {
                panic!(
                    "advanced collection modules failed to load: {}",
                    error.code()
                )
            });
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed advanced collection source matches its captured profile");
    }
    for (index, fixture) in [
        include_str!("fixtures/collection-advanced-use-mod2l.orna"),
        include_str!("fixtures/collection-advanced-filter-callback-mod2l.orna"),
        include_str!("fixtures/collection-advanced-map-while-callback-mod2l.orna"),
        include_str!("fixtures/collection-advanced-empty-ints-mod2l.orna"),
        include_str!("fixtures/collection-advanced-empty-strings-mod2l.orna"),
        include_str!("fixtures/collection-advanced-empty-pairs-mod2l.orna"),
    ]
    .into_iter()
    .enumerate()
    {
        session.submit(fixture).unwrap_or_else(|error| {
            panic!(
                "advanced collection fixture setup {index} failed: {} ({fixture})",
                error.code()
            )
        });
    }
    session
}

fn assert_true_fixture(session: &mut AdmittedReplSession, fixture: &str) {
    for expression in fixture.split("&&").map(str::trim) {
        let parsed = orna_syntax_v1::parse_repl(expression);
        assert!(parsed.is_ok(), "{expression}: {:#?}", parsed.diagnostics);
        let result = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "advanced collection proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(
            result,
            Some(CanonicalValue::new(Raw::Bool(true)).expect("true is canonical")),
            "advanced collection fixture failed: {expression}",
        );
    }
}

#[test]
fn optional_mapping_adapters_keep_some_values_and_stop_on_null() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/collection-advanced-filter-map-mod2l.orna"),
    );
}

#[test]
fn prefix_adapters_keep_the_boundary_and_empty_cases() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/collection-advanced-prefix-mod2l.orna"),
    );
}

#[test]
fn intersperse_and_scan_preserve_order_and_accumulated_states() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/collection-advanced-interleave-scan-mod2l.orna"),
    );
}

#[test]
fn zip_longest_marks_only_missing_items_as_null() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/collection-advanced-zip-longest-mod2l.orna"),
    );
}
