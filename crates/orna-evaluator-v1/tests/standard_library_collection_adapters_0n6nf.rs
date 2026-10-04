use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const ADAPTER_MODULES: [&str; 2] = ["std/collection.orna", "std/collection/adapters.orna"];

fn session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| ADAPTER_MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), ADAPTER_MODULES.len());
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/0n6nf-collection-adapters",
        sources.clone(),
    )
    .expect("collection adapters and their pinned dependency form a source profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("collection adapters resolve against the pinned collection module");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
            .unwrap_or_else(|error| {
                panic!(
                    "collection adapter modules failed to load: {}",
                    error.code()
                )
            });
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-collection-adapters-use-0n6nf.orna"
        )),
        Ok(None),
    );
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-collection-adapters-empty-int-0n6nf.orna"
        )),
        Ok(None),
    );
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-collection-adapters-empty-pairs-0n6nf.orna"
        )),
        Ok(None),
    );
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-collection-adapters-empty-indexed-0n6nf.orna"
        )),
        Ok(None),
    );
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-collection-adapters-empty-str-0n6nf.orna"
        )),
        Ok(None),
    );
    session
}

fn assert_true_fixture(session: &mut AdmittedReplSession, fixture: &str) {
    for expression in fixture.split("&&").map(str::trim) {
        let parsed = orna_syntax_v1::parse_repl(expression);
        assert!(parsed.is_ok(), "{expression}: {:#?}", parsed.diagnostics);
        let result = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "collection adapter proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(
            result,
            Some(CanonicalValue::new(Raw::Bool(true)).expect("true is canonical")),
            "collection adapter fixture failed: {expression}",
        );
    }
}

#[test]
fn indexed_adapters_keep_zero_based_positions_and_order() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-collection-adapters-indexed-0n6nf.orna"),
    );
}

#[test]
fn zip_with_truncates_to_the_shorter_input_in_both_directions() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-collection-adapters-zip-0n6nf.orna"),
    );
}

#[test]
fn unzip_splits_pairs_stably_and_handles_empty_input() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-collection-adapters-unzip-0n6nf.orna"),
    );
}
