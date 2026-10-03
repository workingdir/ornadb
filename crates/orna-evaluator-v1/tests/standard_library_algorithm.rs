use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_collection_algorithms_execute_from_the_captured_orna_module() {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/collection.orna" || path == "std/algorithm.orna")
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 2, "algorithm dependencies are captured");
    let algorithm = sources
        .iter()
        .find(|(path, _)| path == "std/algorithm.orna")
        .expect("the algorithm module is captured in the pinned source bundle");
    let parsed = orna_syntax_v1::parse_module_with_file(&algorithm.1, &algorithm.0);
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/oo3k6-collection-algorithms",
        sources.clone(),
    )
    .expect("algorithm dependencies are captured in an immutable std snapshot");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("selected source bytes match their captured dependency snapshot");
        assert!(
            profile
                .verify_source(
                    path,
                    &format!("{source}\n// changed after snapshot capture")
                )
                .is_err(),
            "edited source bytes must not replace the captured {path}"
        );
    }
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .unwrap_or_else(|error| {
            panic!("the captured algorithm module failed semantic admission: {error:?}")
        });
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
            .unwrap_or_else(|error| {
                panic!("captured algorithm source failed to load: {}", error.code())
            });
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-algorithm-oo3k6.orna")),
        Ok(None)
    );
    for behavior in [
        include_str!("fixtures/stdlib-algorithm-sort-oo3k6.orna"),
        include_str!("fixtures/stdlib-algorithm-stable-ties-cli9f.orna"),
    ] {
        assert_eq!(
            session.submit(behavior).unwrap_or_else(|error| panic!(
                "algorithm fixture {behavior:?} failed: {}",
                error.code()
            )),
            Some(bool_value(true)),
            "algorithm behavior: {behavior}"
        );
    }
    let expected = Some(bool_value(true));
    let behavior = include_str!("fixtures/stdlib-algorithm-behavior-oo3k6.orna");
    for (index, check) in behavior.split("\n&& ").enumerate() {
        assert_eq!(
            session.submit(check).unwrap_or_else(|error| panic!(
                "algorithm behavior {index} failed: {} ({check})",
                error.code()
            )),
            expected,
            "algorithm behavior {index}: {check}"
        );
    }
}
