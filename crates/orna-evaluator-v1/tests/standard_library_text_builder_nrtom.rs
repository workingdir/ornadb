use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const BUILDER_MODULES: [&str; 3] = [
    "std/collection.orna",
    "std/text.orna",
    "std/text/builder.orna",
];

fn session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| BUILDER_MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), BUILDER_MODULES.len());
    let profile =
        StandardDependencyProfile::from_sources("orna.std/nrtom-text-builder", sources.clone())
            .expect("text builder and its pinned dependencies form a source profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("text builder resolves against the captured collection and text modules");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
            .unwrap_or_else(|error| {
                panic!(
                    "the selected text builder modules fail to load: {}",
                    error.code()
                )
            });
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-text-builder-use-nrtom.orna")),
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
                "string builder behavior proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(
            result,
            Some(CanonicalValue::new(Raw::Bool(true)).expect("true is canonical")),
            "string builder fixture failed: {expression}",
        );
    }
}

#[test]
fn text_builder_accumulates_chunks_in_order_until_build() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-text-builder-values-nrtom.orna"),
    );
}

#[test]
fn text_builder_line_and_empty_contracts_include_unicode_text() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-text-builder-lines-nrtom.orna"),
    );
}

#[test]
fn text_builder_append_preserves_previous_builder_values() {
    let mut session = session();
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-text-builder-persistence-nrtom.orna"
        )),
        Ok(None),
    );
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-text-builder-persistence-call-nrtom.orna"),
    );
}
