use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const MODULES: [&str; 3] = ["std/collection.orna", "std/bits.orna", "std/bytes.orna"];

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("proof result is canonical")
}

fn pinned_bytes_session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), MODULES.len());
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/n9hdj-byte-buffer-utilities",
        sources.clone(),
    )
    .expect("captured collection and bits sources form a byte-buffer dependency profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("byte-buffer utilities resolve against the pinned collection and bits modules");
    let session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), Limits::default())
            .unwrap_or_else(|error| {
                panic!(
                    "captured byte-buffer profile failed to load: {}",
                    error.code()
                )
            });
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed byte-buffer sources match the captured profile");
    }
    session
}

#[test]
fn byte_buffer_utilities_validate_ranges_and_preserve_byte_order() {
    let mut session = pinned_bytes_session();
    let fixture = include_str!("fixtures/stdlib-bytes-utilities-n9hdj.orna");
    for (index, expression) in fixture.split("&&").map(str::trim).enumerate() {
        assert_eq!(
            session.submit(expression).unwrap_or_else(|error| panic!(
                "byte-buffer behavior {index} failed: {} ({expression})",
                error.code()
            )),
            Some(boolean(true)),
            "byte-buffer behavior {index}: {expression}"
        );
    }
}
