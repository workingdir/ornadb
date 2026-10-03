#[path = "support/pinned_error_collection_std.rs"]
mod pinned_error_collection_std;

use orna_evaluator_v1::AdmittedReplSession;
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn ints(values: &[i64]) -> Raw {
    Raw::Array(
        values
            .iter()
            .map(|value| Raw::Int((*value).into()))
            .collect(),
    )
}

fn text(value: &str) -> Raw {
    Raw::Text(value.to_owned())
}

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected result is a canonical value")
}

fn submit(session: &mut AdmittedReplSession, source: &str, contract: &str) -> CanonicalValue {
    let parsed = orna_syntax_v1::parse_repl(source);
    assert!(
        parsed.is_ok(),
        "proof fixture syntax: {:?}",
        parsed.diagnostics
    );
    session
        .submit(source)
        .unwrap_or_else(|error| panic!("{contract} proof failed: {}", error.code()))
        .expect("proof expression returns a value")
}

#[test]
fn pinned_error_and_collection_edge_contracts_return_documented_values() {
    let mut session = pinned_error_collection_std::session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-error-typdl.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-use-collection-alias-yfifu.orna"
        )),
        Ok(None)
    );
    let cases = [
        (
            "Error.caused_by preserves the complete nested cause chain",
            include_str!("fixtures/stdlib-error-caused-by-equality-p0b5d.orna"),
            Raw::Bool(true),
        ),
        (
            "chunk of empty input",
            include_str!("fixtures/stdlib-collection-empty-chunk-p0b5d.orna"),
            Raw::Array(vec![]),
        ),
        (
            "zip truncates to the shorter input",
            include_str!("fixtures/stdlib-collection-zip-short-p0b5d.orna"),
            Raw::Array(vec![Raw::Tag(
                60015,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), text("a")])),
            )]),
        ),
        (
            "zip_exact accepts two empty inputs",
            include_str!("fixtures/stdlib-collection-zip-empty-p0b5d.orna"),
            Raw::Array(vec![]),
        ),
        (
            "zip_exact returns its specified length error",
            include_str!("fixtures/stdlib-collection-zip-mismatch-p0b5d.orna"),
            Raw::Array(vec![Raw::Tag(
                60015,
                Box::new(Raw::Array(vec![Raw::Int(42.into()), Raw::Int(43.into())])),
            )]),
        ),
        (
            "window omits incomplete tails",
            include_str!("fixtures/stdlib-collection-window-short-p0b5d.orna"),
            Raw::Array(vec![]),
        ),
        (
            "split_when omits groups at leading and adjacent boundaries",
            include_str!("fixtures/stdlib-collection-split-boundaries-p0b5d.orna"),
            Raw::Array(vec![ints(&[0]), ints(&[0, 1]), ints(&[0])]),
        ),
        (
            "one returns its specified empty-input cardinality error",
            include_str!("fixtures/stdlib-collection-one-empty-p0b5d.orna"),
            Raw::Int(44.into()),
        ),
    ];

    for (contract, source, expected) in cases {
        assert_eq!(
            submit(&mut session, source, contract),
            canonical(expected),
            "{contract}"
        );
    }
}

#[test]
fn error_and_collection_sources_are_bound_to_the_captured_dependency_snapshot() {
    let (profile, sources) = pinned_error_collection_std::captured_profile();
    for path in ["std/error.orna", "std/collection.orna"] {
        let (_, source) = sources
            .iter()
            .find(|(candidate, _)| candidate == path)
            .unwrap_or_else(|| panic!("missing captured source {path}"));
        profile
            .verify_source(path, source)
            .unwrap_or_else(|_| panic!("captured bytes for {path} must verify"));

        let mut changed = source.clone();
        changed.push_str("\n// after captured dependency snapshot\n");
        assert!(
            profile.verify_source(path, &changed).is_err(),
            "{path} edits must not silently replace the captured std source"
        );
    }
}
