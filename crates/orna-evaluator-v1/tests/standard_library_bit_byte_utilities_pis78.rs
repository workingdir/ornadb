use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| matches!(path.as_str(), "std/bits.orna" | "std/collection.orna"))
        .collect::<Vec<_>>();
    assert_eq!(
        sources.len(),
        2,
        "the proof loads only bits and list helpers"
    );
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/pis78-bit-byte-utilities",
        sources.clone(),
    )
    .expect("bit and byte helper sources form a captured dependency profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("bit and byte helpers resolve against the captured collection module");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
            .expect("the selected bit and byte modules load");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-bits-pis78.orna")),
        Ok(None),
    );
    session
}

fn assert_true_fixture(session: &mut AdmittedReplSession, fixture: &str) {
    let result = session
        .submit(fixture)
        .unwrap_or_else(|error| panic!("bit/byte behavior proof failed: {}", error.code()));
    assert_eq!(
        result,
        Some(CanonicalValue::new(Raw::Bool(true)).expect("true is canonical")),
        "bit/byte behavior fixture failed: {fixture}",
    );
}

#[test]
fn bit_helpers_cover_signed_values_and_reject_negative_indices() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-bits-bit-contracts-pis78.orna"),
    );
}

#[test]
fn unsigned_byte_lists_roundtrip_in_both_byte_orders() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-bits-byte-roundtrip-pis78.orna"),
    );
}

#[test]
fn byte_conversions_reject_invalid_width_order_and_byte_values() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/stdlib-bits-byte-invalid-pis78.orna"),
    );
}
