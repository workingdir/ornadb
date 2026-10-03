use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn strings(values: &[&str]) -> CanonicalValue {
    CanonicalValue::new(Raw::Array(
        values
            .iter()
            .map(|value| Raw::Text((*value).to_owned()))
            .collect(),
    ))
    .unwrap()
}

fn pinned_format_sources() -> Vec<(String, String)> {
    let all_sources = orna_standard::reference_standard_sources_v1();
    let pinned_profile = orna_standard::reference_standard_profile_v1();
    all_sources
        .into_iter()
        .filter(|(path, _)| matches!(path.as_str(), "std/format.orna" | "std/text.orna"))
        .map(|(path, source)| {
            pinned_profile
                .verify_source(&path, &source)
                .unwrap_or_else(|error| panic!("{path} is not from the pinned std DB: {error:?}"));
            (path, source)
        })
        .collect()
}

fn pinned_format_session() -> AdmittedReplSession {
    let sources = pinned_format_sources();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/format-contracts-wi489",
        sources.clone(),
    )
    .expect("the pinned format and text module bytes form a dependency profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("the pinned std.format and std.text modules resolve against core");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .unwrap_or_else(|error| panic!("could not admit pinned formatter: {}", error.code()))
}

#[test]
fn pinned_format_module_emits_minimal_integer_and_boolean_spellings() {
    let mut session = pinned_format_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-format-contract-use-wi489.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-format-contract-integers-wi489.orna")),
        Ok(Some(strings(&[
            "0",
            "1",
            "-1",
            "9",
            "10",
            "-10",
            "99",
            "100",
            "-100",
            "90071992547409931234567890",
            "-90071992547409931234567890",
        ])))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-format-contract-booleans-wi489.orna")),
        Ok(Some(strings(&["true", "false"])))
    );
}
