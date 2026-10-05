use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const LINE_MODULES: [&str; 3] = [
    "std/collection.orna",
    "std/text.orna",
    "std/text/lines.orna",
];

fn session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| LINE_MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), LINE_MODULES.len());
    let profile =
        StandardDependencyProfile::from_sources("orna.std/tnv71-text-lines", sources.clone())
            .expect("text line utilities and their pinned dependencies form a source profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("text line utilities resolve against pinned collection and text modules");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
            .unwrap_or_else(|error| {
                panic!("selected text line modules fail to load: {}", error.code())
            });
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-text-lines-use-t71.orna")),
        Ok(None),
    );
    session
}

fn assert_true_fixture(session: &mut AdmittedReplSession, fixture: &str) {
    for expression in fixture.split("&&").map(str::trim) {
        let result = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "text line behavior proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(
            result,
            Some(CanonicalValue::new(Raw::Bool(true)).expect("true is canonical")),
            "text line fixture failed: {expression}",
        );
    }
}

#[test]
fn line_split_preserves_blank_and_terminal_lines_across_all_delimiters() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-text-lines-boundaries-t71.orna"),
    );
}

#[test]
fn line_join_normalises_delimiters_without_changing_line_boundaries() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-text-lines-join-t71.orna"),
    );
}

#[test]
fn line_count_uses_logical_lines_and_unicode_scalars_remain_in_each_line() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-text-lines-count-t71.orna"),
    );
}
