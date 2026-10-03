use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;
use orna_standard::{
    REFERENCE_STANDARD_COLLECTION_PATH_V1, REFERENCE_STANDARD_OPTION_PATH_V1,
    REFERENCE_STANDARD_PRELUDE_EXPORTS_V1, REFERENCE_STANDARD_PRELUDE_PATH_V1,
    REFERENCE_STANDARD_TEXT_PATH_V1, reference_standard_sources_v1,
};

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn prelude_sources() -> Vec<(String, String)> {
    let wanted = [
        REFERENCE_STANDARD_COLLECTION_PATH_V1,
        REFERENCE_STANDARD_OPTION_PATH_V1,
        REFERENCE_STANDARD_PRELUDE_PATH_V1,
        REFERENCE_STANDARD_TEXT_PATH_V1,
    ];
    let sources = reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| wanted.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), wanted.len());
    sources
}

fn pinned_prelude_session() -> AdmittedReplSession {
    let sources = prelude_sources();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/9npab-prelude-proof",
        sources.clone(),
    )
    .expect("the selected facade sources form a captured dependency snapshot")
    .with_module_prelude_exports(
        REFERENCE_STANDARD_PRELUDE_PATH_V1,
        REFERENCE_STANDARD_PRELUDE_EXPORTS_V1.iter().copied(),
    );
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("the selected prelude and dependencies resolve from their captured sources");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .unwrap_or_else(|error| panic!("could not load pinned std.prelude: {}", error.code()))
}

#[test]
fn curated_prelude_helpers_compute_versioned_collection_text_and_option_values() {
    let mut session = pinned_prelude_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-pinned-prelude-9npab.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-prelude-typed-absence-9npab.orna")),
        Ok(None)
    );

    let contracts = include_str!("fixtures/stdlib-pinned-prelude-values-9npab.orna");
    for contract in contracts.split("&&") {
        let contract = contract.trim();
        let result = session
            .submit(contract)
            .unwrap_or_else(|error| panic!("prelude contract {contract:?}: {}", error.code()));
        assert_eq!(result, Some(bool_value(true)), "contract: {contract}");
    }
}

#[test]
fn prelude_names_and_bytes_are_bound_to_the_selected_snapshot() {
    let sources = prelude_sources();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/9npab-prelude-proof",
        sources.clone(),
    )
    .expect("the selected facade sources form a captured dependency snapshot")
    .with_module_prelude_exports(
        REFERENCE_STANDARD_PRELUDE_PATH_V1,
        REFERENCE_STANDARD_PRELUDE_EXPORTS_V1.iter().copied(),
    );
    let (path, contents) = sources
        .iter()
        .find(|(path, _)| path == REFERENCE_STANDARD_PRELUDE_PATH_V1)
        .expect("the captured bundle contains std.prelude");
    profile
        .verify_source(path, contents)
        .expect("the facade source matches this snapshot");
    let mut changed = contents.clone();
    changed.push_str("\n// changed after capture\n");
    assert!(profile.verify_source(path, &changed).is_err());
    assert_eq!(
        profile.module_prelude_exports()[REFERENCE_STANDARD_PRELUDE_PATH_V1]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        REFERENCE_STANDARD_PRELUDE_EXPORTS_V1
    );
}

#[test]
fn core_assert_remains_available_when_optional_prelude_is_absent() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-use-prelude-without-snapshot-9npab.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    session
        .submit(include_str!("fixtures/stdlib-core-assertion-without-prelude-9npab.orna"))
        .unwrap_or_else(|error| panic!("core assertion declaration failed without std: {}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-core-assertion-call-without-prelude-9npab.orna")),
        Ok(Some(bool_value(true)))
    );
}
