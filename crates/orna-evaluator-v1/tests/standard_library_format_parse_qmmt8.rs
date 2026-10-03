use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn captured_sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            matches!(
                path.as_str(),
                "std/collection.orna" | "std/text.orna" | "std/format.orna" | "std/parse.orna"
            )
        })
        .collect()
}

fn pinned_session() -> AdmittedReplSession {
    let sources = captured_sources();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/qmmt8-format-parse",
        sources.clone(),
    )
    .expect("format, parse, text, and collection source bytes form a pinned dependency profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("captured format and parse dependencies resolve against core");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .unwrap_or_else(|error| panic!("could not load captured format/parse snapshot: {}", error.code()))
}

#[test]
fn primitive_format_and_parse_helpers_compute_canonical_text_values() {
    let mut session = pinned_session();
    for import in [
        include_str!("fixtures/stdlib-use-format-qmmt8.orna"),
        include_str!("fixtures/stdlib-use-parse-qmmt8.orna"),
    ] {
        assert_eq!(session.submit(import), Ok(None));
    }

    for contract in [
        include_str!("fixtures/stdlib-format-integer-values-qmmt8.orna"),
        include_str!("fixtures/stdlib-parse-integer-values-qmmt8.orna"),
        include_str!("fixtures/stdlib-format-parse-boolean-values-qmmt8.orna"),
    ] {
        for clause in contract.split("&&") {
            let clause = clause.trim();
            let result = session.submit(clause).unwrap_or_else(|error| {
                panic!("format/parse contract {clause:?} failed: {}", error.code())
            });
            assert_eq!(result, Some(bool_value(true)), "contract: {clause}");
        }
    }
}

#[test]
fn format_and_parse_modules_are_captured_by_the_selected_std_snapshot() {
    let sources = captured_sources();
    assert_eq!(orna_evaluator_v1::reference_standard_sources().len(), 55);
    for (path, source) in &sources {
        let parsed = orna_syntax_v1::parse_module_with_file(source, path);
        assert!(parsed.is_ok(), "{path}: {:#?}", parsed.diagnostics);
    }
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/qmmt8-format-parse",
        sources.clone(),
    )
    .expect("format, parse, text, and collection source bytes form a pinned dependency profile");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("captured module bytes match the selected std snapshot");
    }
    for path in ["std/format.orna", "std/parse.orna"] {
        let mut changed = sources
            .iter()
            .find(|(source_path, _)| source_path == path)
            .expect("the module source is published in the pinned source bundle")
            .1
            .clone();
        changed.push_str("\n// changed after snapshot capture\n");
        assert!(profile.verify_source(path, &changed).is_err());
    }
    Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources)
        .expect("format and parse imports resolve from their captured source dependencies");
}

#[test]
fn optional_format_parse_imports_do_not_replace_core_without_std() {
    for import in [
        include_str!("fixtures/stdlib-use-format-without-snapshot-qmmt8.orna"),
        include_str!("fixtures/stdlib-use-parse-without-snapshot-qmmt8.orna"),
    ] {
        let mut session = AdmittedReplSession::new(Limits::default());
        assert_eq!(session.submit(import).unwrap_err().code(), "ORNA-S010-IMPORT");
        assert_eq!(
            session.submit(include_str!("fixtures/stdlib-core-without-format-parse-qmmt8.orna")),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!("fixtures/stdlib-core-call-without-format-parse-qmmt8.orna")),
            Ok(Some(bool_value(true)))
        );
    }
}
