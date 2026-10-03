use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn concurrent_collections_session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/concurrent/main.orna" || path == "std/collection.orna")
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 2, "the pinned modules are present");

    let profile =
        StandardDependencyProfile::from_sources("orna.std/70cj2-concurrent", sources.clone())
            .expect("selected source bytes form a captured std snapshot");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("loaded source matches the captured std snapshot");
        profile
            .verify_source(path, &format!("{source}\n// changed after capture"))
            .expect_err("a changed module must not be substituted into this snapshot");
    }

    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("concurrency and collection sources resolve in the captured snapshot");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .unwrap_or_else(|error| panic!("pinned std modules failed to load: {}", error.code()))
}

fn canonical(value: Raw) -> CanonicalValue {
    CanonicalValue::new(value).expect("expected value is canonical")
}

#[test]
fn parallel_map_computes_each_child_result_and_joins_in_input_order() {
    let mut session = concurrent_collections_session();
    assert_eq!(
        session.submit("use std.concurrent;"),
        Ok(None),
        "the pinned module import is admitted"
    );
    let actual = session
        .submit(include_str!(
            "fixtures/stdlib-concurrent-parallel-map-70cj2.orna"
        ))
        .unwrap_or_else(|error| panic!("parallel_map failed with {}", error.code()));
    assert_eq!(
        actual,
        Some(canonical(Raw::Array(vec![
            Raw::Int(4.into()),
            Raw::Int(9.into()),
            Raw::Int(25.into()),
        ])))
    );
}

#[test]
fn parallel_map_of_an_empty_input_returns_an_empty_list_without_children() {
    let mut session = concurrent_collections_session();
    assert_eq!(session.submit("use std.concurrent;"), Ok(None));
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-concurrent-parallel-map-empty-setup-70cj2.orna"
        )),
        Ok(None)
    );
    let actual = session
        .submit(include_str!(
            "fixtures/stdlib-concurrent-parallel-map-empty-70cj2.orna"
        ))
        .unwrap_or_else(|error| panic!("empty parallel_map failed with {}", error.code()));
    assert_eq!(actual, Some(canonical(Raw::Array(vec![]))));
}

#[test]
fn core_assertions_work_without_the_optional_concurrent_snapshot() {
    let mut core = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        core.submit("2 + 3"),
        Ok(Some(canonical(Raw::Int(5.into()))))
    );
    assert_eq!(
        core.submit(include_str!(
            "fixtures/stdlib-concurrent-without-snapshot-zhw5h.orna"
        ))
        .unwrap_err()
        .code(),
        "ORNA-S010-IMPORT"
    );
}
