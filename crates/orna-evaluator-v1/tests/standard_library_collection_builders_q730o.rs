use orna_evaluator_v1::{
    AdmittedReplSession, Limits, reference_standard_profile, reference_standard_sources,
};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const BUILDER_MODULES: [&str; 4] = [
    "std/collection.orna",
    "std/list.orna",
    "std/map.orna",
    "std/set.orna",
];

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("boolean is canonical")
}

fn empty_array() -> CanonicalValue {
    CanonicalValue::new(Raw::Array(Vec::new())).expect("empty array is canonical")
}

fn import_builders(session: &mut AdmittedReplSession) {
    for source in [
        include_str!("fixtures/stdlib-collection-builders-use-list-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-use-map-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-use-set-q730o.orna"),
    ] {
        session
            .submit(source)
            .unwrap_or_else(|error| panic!("builder module import failed: {}", error.code()));
    }
}

fn builders_session() -> AdmittedReplSession {
    let sources = reference_standard_sources()
        .into_iter()
        .filter(|(path, _)| {
            BUILDER_MODULES.contains(&path.as_str()) || path.starts_with("std/encoding/")
        })
        .collect::<Vec<_>>();
    for module in BUILDER_MODULES {
        assert!(
            sources.iter().any(|(path, _)| path == module),
            "the reference profile includes {module}"
        );
    }

    let profile = StandardDependencyProfile::from_sources(
        "orna.std/q730o-collection-builders",
        sources.clone(),
    )
    .expect("the builder source subset forms a profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("the builder modules resolve against core and their captured dependencies");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .unwrap_or_else(|error| panic!("collection builders failed to load: {}", error.code()))
}

#[test]
fn pinned_list_map_and_set_builders_preserve_their_collection_contracts() {
    let mut session = builders_session();
    import_builders(&mut session);

    for source in [
        include_str!("fixtures/stdlib-collection-builders-list-order-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-list-select-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-list-slice-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-list-unique-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-list-reverse-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-map-deduplicate-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-map-insert-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-map-merge-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-map-lookup-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-set-deduplicate-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-set-insert-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-set-ops-q730o.orna"),
    ] {
        assert_eq!(
            session.submit(source),
            Ok(Some(bool_value(true))),
            "builder contract failed: {source}"
        );
    }

    for (declaration, call) in [
        (
            include_str!("fixtures/stdlib-collection-builders-list-empty-q730o.orna"),
            include_str!("fixtures/stdlib-collection-builders-list-empty-call-q730o.orna"),
        ),
        (
            include_str!("fixtures/stdlib-collection-builders-map-empty-q730o.orna"),
            include_str!("fixtures/stdlib-collection-builders-map-empty-call-q730o.orna"),
        ),
        (
            include_str!("fixtures/stdlib-collection-builders-set-empty-q730o.orna"),
            include_str!("fixtures/stdlib-collection-builders-set-empty-call-q730o.orna"),
        ),
    ] {
        assert_eq!(
            session.submit(declaration),
            Ok(None),
            "empty builder declaration failed: {declaration}"
        );
        assert_eq!(
            session.submit(call),
            Ok(Some(empty_array())),
            "empty builder result failed: {call}"
        );
    }
}

#[test]
fn pinned_builder_sources_match_the_profile_and_reject_invalid_counts() {
    let sources = reference_standard_sources();
    let profile = reference_standard_profile();
    for module in BUILDER_MODULES {
        let matches = sources
            .iter()
            .filter(|(path, _)| path == module)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{module} occurs once in the reference DB");
        let (path, source) = matches[0];
        profile
            .verify_source(path, source)
            .unwrap_or_else(|_| panic!("{module} differs from its pin"));

        let mut changed = source.clone();
        changed.push_str("\n// changed after profile capture\n");
        assert!(profile.verify_source(path, &changed).is_err());
    }

    let mut session = builders_session();
    import_builders(&mut session);
    for source in [
        include_str!("fixtures/stdlib-collection-builders-list-negative-take-q730o.orna"),
        include_str!("fixtures/stdlib-collection-builders-list-negative-drop-q730o.orna"),
    ] {
        assert_eq!(
            session.submit(source).unwrap_err().code(),
            "ORNA-EVAL-ERROR",
            "negative builder count must fail: {source}"
        );
    }

    let mut core = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        core.submit(include_str!(
            "fixtures/stdlib-collection-builders-use-list-q730o.orna"
        ))
        .unwrap_err()
        .code(),
        "ORNA-S010-IMPORT"
    );
}
