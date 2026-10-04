use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/option.orna")
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 1, "the proof loads only std.option");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/dp2q7-option-combinators",
        sources.clone(),
    )
    .expect("option combinator source forms a captured dependency profile");
    let parsed = orna_syntax_v1::parse_module_with_file(&sources[0].1, "std/option.orna");
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .unwrap_or_else(|error| panic!("option source catalogue error: {error:?}"));
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
            .unwrap_or_else(|error| panic!("option module load failed: {}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-option-dp2q7.orna")),
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
                "option behavior proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(
            result,
            Some(CanonicalValue::new(Raw::Bool(true)).expect("true is canonical")),
            "option behavior fixture failed: {expression}",
        );
    }
}

#[test]
fn option_value_combinators_cover_presence_mapping_filtering_and_flattening() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-option-values-dp2q7.orna"),
    );
}

#[test]
fn option_branch_combinators_cover_and_or_xor_and_zip() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-option-branches-dp2q7.orna"),
    );
}

#[test]
fn option_fallbacks_run_only_on_the_none_branch() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-option-fallbacks-dp2q7.orna"),
    );
}

#[test]
fn option_combinators_preserve_callback_failures() {
    let mut session = session();
    let error = session
        .submit(include_str!(
            "fixtures/stdlib-option-callback-failure-dp2q7.orna"
        ))
        .expect_err("a failure raised by a selected callback must propagate");
    assert_eq!(error.code(), "ORNA-EVAL-ERROR");
}
