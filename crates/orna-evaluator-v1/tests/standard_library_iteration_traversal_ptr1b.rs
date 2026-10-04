use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const MODULES: [&str; 2] = ["std/collection.orna", "std/iteration.orna"];

fn session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), MODULES.len());
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/ptr1b-iteration-traversal",
        sources.clone(),
    )
    .expect("collection and iteration sources form a pinned profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("iteration utilities resolve against the pinned collection module");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), Limits::default())
            .unwrap_or_else(|error| panic!("iteration sources failed to load: {}", error.code()));
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed iteration source matches its pinned profile");
    }
    for (index, setup) in [
        include_str!("fixtures/iteration-use-ptr1b.orna"),
        include_str!("fixtures/iteration-neighbors-ptr1b.orna"),
        include_str!("fixtures/iteration-target-edges-ptr1b.orna"),
    ]
    .into_iter()
    .enumerate()
    {
        let parsed = orna_syntax_v1::parse_module_with_file(setup, "iteration-setup-ptr1b.orna");
        assert!(parsed.is_ok(), "setup {index}: {:#?}", parsed.diagnostics);
        let parsed_repl = orna_syntax_v1::parse_repl(setup);
        assert!(
            parsed_repl.is_ok(),
            "setup {index} REPL parse: {:#?}",
            parsed_repl.diagnostics
        );
        session.submit(setup).unwrap_or_else(|error| {
            panic!("iteration helper setup {index} failed: {}", error.code())
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
                "iteration behavior proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(
            result,
            Some(CanonicalValue::new(Raw::Bool(true)).expect("true is canonical")),
            "iteration behavior fixture failed: {expression}",
        );
    }
}

#[test]
fn finite_folds_preserve_order_and_stop_after_the_boundary() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/iteration-fold-ptr1b.orna"),
    );
}

#[test]
fn graph_walks_keep_order_visit_cycles_once_and_short_circuit_reachability() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/iteration-traversal-ptr1b.orna"),
    );
}
