use orna_evaluator_v1::{AdmittedReplSession, Limits, TIMEZONE_DATASET_VERSION};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

fn captured_sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            path == "std/text.orna"
                || path == "std/time.orna"
                || path == "std/time/calendar.orna"
                || path.starts_with("std/time/duration/")
        })
        .collect()
}

fn pinned_session() -> (AdmittedReplSession, StandardDependencyProfile) {
    let sources = captured_sources();
    assert_eq!(sources.len(), 7, "the pinned time/text surface is complete");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/1b5ob-time-text-edge-contracts",
        sources.clone(),
    )
    .expect("captured time and text sources form a dependency snapshot");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("captured time and text modules resolve against core");
    let session = AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .unwrap_or_else(|error| {
            panic!(
                "captured time/text modules failed to load: {}",
                error.code()
            )
        });
    (session, profile)
}

#[test]
fn captured_time_and_text_sources_execute_exact_edge_values() {
    let (mut session, profile) = pinned_session();
    assert_eq!(TIMEZONE_DATASET_VERSION, "orna-iana-2024a");
    for (path, source) in captured_sources() {
        profile
            .verify_source(&path, &source)
            .expect("the loaded source bytes match the captured profile");
        let mut changed = source.clone();
        changed.push_str("\n// changed after snapshot capture\n");
        assert!(profile.verify_source(&path, &changed).is_err());
    }

    for source in [
        include_str!("fixtures/stdlib-text-use-1b5ob.orna"),
        include_str!("fixtures/stdlib-time-use-1b5ob.orna"),
    ] {
        let result = session
            .submit(source)
            .unwrap_or_else(|error| panic!("module import failed: {}", error.code()));
        assert_eq!(result, None);
    }
    for assertion in include_str!("fixtures/stdlib-time-text-edge-values-1b5ob.orna")
        .split("&&")
        .map(str::trim)
    {
        let actual = session.submit(assertion).unwrap_or_else(|error| {
            panic!("edge expression failed: {}\n{assertion}", error.code())
        });
        assert_eq!(actual, Some(canonical(Raw::Bool(true))), "{assertion}");
    }
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-text-split-empty-empty-1b5ob.orna"
            ))
            .unwrap_or_else(|error| panic!("empty scalar split failed: {}", error.code())),
        Some(canonical(Raw::Array(Vec::new())))
    );
}

#[test]
fn time_rejects_invalid_local_civil_spellings_and_zones() {
    let (mut session, _) = pinned_session();
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-text-use-1b5ob.orna"))
            .unwrap_or_else(|error| panic!("text import failed: {}", error.code())),
        None
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-use-1b5ob.orna"))
            .unwrap_or_else(|error| panic!("time import failed: {}", error.code())),
        None
    );
    for source in include_str!("fixtures/stdlib-time-invalid-1b5ob.orna")
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let result = session.submit(source);
        assert_eq!(
            result.unwrap_err().code(),
            "ORNA-EVAL-VALUE",
            "invalid local-time case must fail closed: {source}"
        );
    }
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-text-invalid-normalisation-1b5ob.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn core_values_work_without_std_and_time_text_are_not_host_filled() {
    let mut core = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        core.submit(include_str!(
            "fixtures/stdlib-core-without-time-text-1b5ob.orna"
        )),
        Ok(Some(canonical(Raw::Bool(true))))
    );
    assert_eq!(
        core.submit(include_str!(
            "fixtures/stdlib-time-without-snapshot-1b5ob.orna"
        ))
        .unwrap_err()
        .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        core.submit(include_str!(
            "fixtures/stdlib-text-without-snapshot-1b5ob.orna"
        ))
        .unwrap_err()
        .code(),
        "ORNA-S012-UNRESOLVED"
    );
}
