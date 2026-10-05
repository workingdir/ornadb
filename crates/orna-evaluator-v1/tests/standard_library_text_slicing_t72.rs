use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const SLICING_MODULES: [&str; 3] = [
    "std/collection.orna",
    "std/text.orna",
    "std/text/slicing.orna",
];

fn session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| SLICING_MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), SLICING_MODULES.len());
    let profile =
        StandardDependencyProfile::from_sources("orna.std/t72-text-slicing", sources.clone())
            .expect("text slicing and its pinned dependencies form a source profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("text slicing resolves against pinned collection and text modules");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
            .unwrap_or_else(|error| {
                panic!(
                    "selected text slicing modules fail to load: {}",
                    error.code()
                )
            });
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-text-slicing-use-t72.orna")),
        Ok(None),
    );
    session
}

fn assert_true_fixture(session: &mut AdmittedReplSession, fixture: &str) {
    for expression in fixture.split("&&").map(str::trim) {
        let result = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "text slicing behavior proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(
            result,
            Some(CanonicalValue::new(Raw::Bool(true)).expect("true is canonical")),
            "text slicing fixture failed: {expression}",
        );
    }
}

#[test]
fn text_take_clamps_to_scalar_count_and_handles_empty_text() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-text-slicing-take-t72.orna"),
    );
}

#[test]
fn text_drop_clamps_to_scalar_count_and_handles_unicode() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-text-slicing-drop-t72.orna"),
    );
}

#[test]
fn text_slice_covers_range_boundaries_and_unicode_scalars() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-text-slicing-slice-t72.orna"),
    );
}
