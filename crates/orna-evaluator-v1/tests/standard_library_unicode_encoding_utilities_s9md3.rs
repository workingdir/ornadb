use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const MODULES: [&str; 4] = [
    "std/collection.orna",
    "std/text.orna",
    "std/encoding/base64.orna",
    "std/encoding/utilities.orna",
];

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("proof result is canonical")
}

fn pinned_unicode_encoding_session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), MODULES.len());
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/s9md3-unicode-encoding-utilities",
        sources.clone(),
    )
    .expect("captured text, collection, and Base64 sources form a dependency profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("Unicode and encoding utility dependencies resolve in the pinned profile");
    let session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), Limits::default())
            .unwrap_or_else(|error| {
                panic!(
                    "captured Unicode/encoding profile failed to load: {}",
                    error.code()
                )
            });
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed Unicode and encoding sources match the captured profile");
    }
    session
}

#[test]
fn unicode_and_encoding_utilities_preserve_scalars_and_canonical_base64url() {
    let mut session = pinned_unicode_encoding_session();
    let fixture = include_str!("fixtures/stdlib-unicode-encoding-utilities-s9md3.orna");
    for (index, expression) in fixture.split("\n&& ").enumerate() {
        assert_eq!(
            session.submit(expression).unwrap_or_else(|error| panic!(
                "Unicode/encoding behavior {index} failed: {} ({expression})",
                error.code()
            )),
            Some(boolean(true)),
            "Unicode/encoding behavior {index}: {expression}"
        );
    }

    for (invalid, expected_code) in [
        ("a", "ORNA-EVAL-VALUE"),
        ("Zh", "ORNA-EVAL-VALUE"),
        ("Zg==", "ORNA-EVAL-ERROR"),
        ("++//", "ORNA-EVAL-ERROR"),
    ] {
        assert_eq!(
            session
                .submit(&format!(
                    "std.encoding.utilities.base64url_decode(\"{invalid}\")"
                ))
                .unwrap_err()
                .code(),
            expected_code,
            "noncanonical Base64url input must be rejected: {invalid}"
        );
    }
}
