//! Deterministic regression coverage for the public Inspector carrier contract.
//!
//! These tests intentionally stay above private server helpers. They exercise
//! the public sealed carrier and lifecycle-binding APIs that server adapters
//! consume, so malformed bytes and stale target provenance cannot be accepted
//! by a future adapter without changing the contract.

use orna_core::{
    CatalogueRevisionId, InspectEpochId, InvocationId, PrincipalId, SourceRevisionId,
    inspect::{INSPECT_RENDER_CARRIER_SIGNATURE, InspectProjection},
    inspect_carrier::{
        InspectCarrierEnvelope, InspectCarrierError, InspectCarrierKind, InspectCarrierProvenance,
        InspectRowError,
    },
    inspect_lifecycle::{InspectEpochBinding, InspectLifecycleError, InspectProjectionVersions},
    revision::RevisionPair,
};
use orna_server::InstalledInspectProjection;

fn invocation(byte: u8) -> InvocationId {
    InvocationId::from_bytes([byte; 16])
}

fn principal(byte: u8) -> PrincipalId {
    PrincipalId::from_bytes([byte; 16])
}

fn source(byte: u8) -> SourceRevisionId {
    SourceRevisionId::from_bytes([byte; 16])
}

fn catalogue(byte: u8) -> CatalogueRevisionId {
    CatalogueRevisionId::from_bytes([byte; 16])
}

fn epoch(high: u8, low: u8) -> InspectEpochId {
    let mut bytes = [0; 16];
    bytes[..8].fill(high);
    bytes[8..].fill(low);
    InspectEpochId::from_bytes(bytes)
}

fn integer_row(value: i32) -> Vec<u8> {
    let mut row = Vec::with_capacity(25 + 4);
    row.extend_from_slice(b"ORV5");
    row.push(0x03);
    row.extend_from_slice(&[0; 16]);
    row.extend_from_slice(&4u32.to_be_bytes());
    row.extend_from_slice(&value.to_be_bytes());
    row
}

fn binding(generation: u64, target: InvocationId) -> InspectEpochBinding {
    InspectEpochBinding::new(
        invocation(0x10),
        epoch(0xa1, 0xa8),
        target,
        principal(0x20),
        RevisionPair::new(source(0x30), catalogue(0x40)),
        invocation(0x50),
        invocation(0x60),
        InspectProjectionVersions::v1(),
        generation,
    )
}

#[test]
fn carrier_identity_and_provenance_are_stable_across_all_projections() {
    let expected = [
        ("p_snapshot", InspectCarrierKind::Snapshot, None, 1),
        (
            "p_invocation_nodes",
            InspectCarrierKind::InvocationNodes,
            Some(InspectProjection::InvocationNodes),
            2,
        ),
        (
            "p_calls",
            InspectCarrierKind::Calls,
            Some(InspectProjection::Calls),
            3,
        ),
        (
            "p_resources",
            InspectCarrierKind::Resources,
            Some(InspectProjection::Resources),
            4,
        ),
        (
            "p_state_cells",
            InspectCarrierKind::StateCells,
            Some(InspectProjection::StateCells),
            5,
        ),
        (
            "p_ui_nodes",
            InspectCarrierKind::UiNodes,
            Some(InspectProjection::UiNodes),
            6,
        ),
        (
            "p_presentation_candidates",
            InspectCarrierKind::PresentationCandidates,
            Some(InspectProjection::PresentationCandidates),
            7,
        ),
        (
            "p_runtime_bindings",
            InspectCarrierKind::RuntimeBindings,
            Some(InspectProjection::RuntimeBindings),
            8,
        ),
        (
            "p_security_decisions",
            InspectCarrierKind::SecurityDecisions,
            Some(InspectProjection::SecurityDecisions),
            9,
        ),
    ];

    assert_eq!(INSPECT_RENDER_CARRIER_SIGNATURE.len(), expected.len());
    for (
        (name, type_id, kind),
        (expected_name, expected_kind, expected_projection, expected_tag),
    ) in INSPECT_RENDER_CARRIER_SIGNATURE
        .iter()
        .zip(expected.iter().copied())
    {
        assert_eq!(*name, expected_name);
        assert_eq!(*kind, expected_kind);
        assert_eq!(*type_id, expected_kind.type_id());
        assert_eq!(expected_kind.projection(), expected_projection);
        assert_eq!(expected_kind.tag(), expected_tag);
        assert_eq!(
            InspectCarrierKind::from_type_id(*type_id),
            Some(expected_kind)
        );
    }

    let target = invocation(0x70);
    let provenance = InspectCarrierProvenance::trusted_for_target(
        epoch(0xa1, 0xa8),
        target,
        source(0x80),
        catalogue(0x90),
    );

    for (_, kind, _, _) in expected {
        let carrier = InspectCarrierEnvelope::new_with_target(kind, target, provenance, vec![])
            .expect("matching target provenance must be accepted");
        assert_eq!(carrier.carrier_kind(), kind);
        assert_eq!(carrier.target_invocation_id(), Some(target));
        assert_eq!(carrier.server_epoch_id(), provenance.server_epoch_id());
        assert_eq!(
            carrier.source_revision_id(),
            provenance.source_revision_id()
        );
        assert_eq!(
            carrier.catalogue_revision_id(),
            provenance.catalogue_revision_id()
        );

        let decoded = InspectCarrierEnvelope::decode(
            &carrier
                .encode()
                .expect("canonical empty carrier must encode"),
        )
        .expect("canonical carrier must decode");
        assert_eq!(decoded.carrier_kind(), kind);
        assert_eq!(decoded.server_epoch_id(), provenance.server_epoch_id());
        assert_eq!(
            decoded.source_revision_id(),
            provenance.source_revision_id()
        );
        assert_eq!(
            decoded.catalogue_revision_id(),
            provenance.catalogue_revision_id()
        );
        // The version-one wire envelope carries server epoch and revisions;
        // target binding remains an in-memory provider fact.
        assert_eq!(decoded.target_invocation_id(), None);
    }
}

#[test]
fn malformed_carrier_bytes_and_rows_fail_closed() {
    let carrier = InspectCarrierEnvelope::new(
        InspectCarrierKind::Calls,
        epoch(0, 7),
        source(0xa1),
        catalogue(0xb2),
        vec![integer_row(1)],
    )
    .expect("canonical row must be accepted");
    let encoded = carrier.encode().expect("canonical carrier must encode");

    let mut invalid_magic = encoded.clone();
    invalid_magic[0] = b'X';
    assert_eq!(
        InspectCarrierEnvelope::decode(&invalid_magic),
        Err(InspectCarrierError::InvalidMagic)
    );

    let mut unsupported_version = encoded.clone();
    unsupported_version[15..17].copy_from_slice(&2u16.to_be_bytes());
    assert_eq!(
        InspectCarrierEnvelope::decode(&unsupported_version),
        Err(InspectCarrierError::UnsupportedVersion(2))
    );

    let mut truncated = encoded.clone();
    truncated.pop();
    assert_eq!(
        InspectCarrierEnvelope::decode(&truncated),
        Err(InspectCarrierError::Truncated)
    );

    let mut trailing = encoded.clone();
    trailing.push(0);
    assert_eq!(
        InspectCarrierEnvelope::decode(&trailing),
        Err(InspectCarrierError::TrailingBytes)
    );

    let mut unknown_tag = encoded.clone();
    unknown_tag[17] = 0xff;
    assert_eq!(
        InspectCarrierEnvelope::decode(&unknown_tag),
        Err(InspectCarrierError::UnknownProjectionTag(0xff))
    );

    let malformed_row = InspectCarrierEnvelope::new(
        InspectCarrierKind::Calls,
        epoch(0, 7),
        source(0xa1),
        catalogue(0xb2),
        vec![b"not-an-orv5-row".to_vec()],
    );
    assert!(matches!(
        malformed_row,
        Err(InspectCarrierError::InvalidRow(
            InspectRowError::TruncatedHeader { actual: 15 }
        ))
    ));
}

#[test]
fn target_provenance_and_stale_epoch_bindings_fail_closed() {
    let target = invocation(0xc1);
    let other_target = invocation(0xc2);
    let provenance =
        InspectCarrierProvenance::trusted_for_target(
            epoch(0, 11),
            target,
            source(0xd1),
            catalogue(0xe1),
        );

    assert_eq!(
        provenance.bind_target(other_target),
        Err(InspectCarrierError::TargetInvocationMismatch {
            expected: target,
            actual: other_target,
        })
    );
    assert_eq!(
        InspectCarrierEnvelope::new_with_target(
            InspectCarrierKind::Snapshot,
            other_target,
            provenance,
            vec![],
        ),
        Err(InspectCarrierError::TargetInvocationMismatch {
            expected: target,
            actual: other_target,
        })
    );

    let current = binding(4, target);
    let stale = binding(3, target);
    assert_eq!(
        stale.validate_against(&current),
        Err(InspectLifecycleError::StaleEpoch {
            expected: 4,
            actual: 3,
        })
    );
    assert_eq!(
        InspectLifecycleError::StaleEpoch {
            expected: 4,
            actual: 3,
        }
        .code(),
        "inspect.stale_epoch"
    );

    let future = binding(5, target);
    assert_eq!(
        future.validate_against(&current),
        Err(InspectLifecycleError::FutureEpoch {
            expected: 4,
            actual: 5,
        })
    );

    let target_mismatch = binding(4, other_target);
    assert_eq!(
        target_mismatch.validate_against(&current),
        Err(InspectLifecycleError::EpochMismatch)
    );
    assert_eq!(
        InspectLifecycleError::EpochMismatch.code(),
        "inspect.epoch_mismatch"
    );
}

#[test]
fn revision_mismatch_fails_closed_with_public_epoch_error() {
    let target = invocation(0xc3);
    let current = binding(4, target);
    let revision_mismatch = InspectEpochBinding::new(
        invocation(0x10),
        epoch(0x01, 0x08),
        target,
        principal(0x20),
        RevisionPair::new(source(0x31), catalogue(0x40)),
        invocation(0x50),
        invocation(0x60),
        InspectProjectionVersions::v1(),
        4,
    );

    assert_eq!(
        revision_mismatch.validate_against(&current),
        Err(InspectLifecycleError::RevisionMismatch {
            expected: current.revision(),
            actual: revision_mismatch.revision(),
        })
    );
    assert_eq!(
        InspectLifecycleError::RevisionMismatch {
            expected: current.revision(),
            actual: revision_mismatch.revision(),
        }
        .code(),
        "inspect.epoch_mismatch"
    );
}

#[test]
fn installed_projection_names_remain_aligned_with_carrier_tags() {
    let names = [
        ("invocation_nodes", InspectCarrierKind::InvocationNodes),
        ("calls", InspectCarrierKind::Calls),
        ("resources", InspectCarrierKind::Resources),
        ("state_cells", InspectCarrierKind::StateCells),
        ("ui_nodes", InspectCarrierKind::UiNodes),
        (
            "presentation_candidates",
            InspectCarrierKind::PresentationCandidates,
        ),
        ("runtime_bindings", InspectCarrierKind::RuntimeBindings),
        ("security_decisions", InspectCarrierKind::SecurityDecisions),
    ];

    for (name, kind) in names {
        let projection = InstalledInspectProjection::parse(name)
            .expect("every sealed projection name must parse");
        assert_eq!(projection_name(projection), name);
        assert_eq!(projection_tag(projection), kind.tag());
    }
    assert_eq!(InstalledInspectProjection::parse("snapshot"), None);
    assert_eq!(InstalledInspectProjection::parse("unknown"), None);
}

fn projection_name(projection: InstalledInspectProjection) -> &'static str {
    match projection {
        InstalledInspectProjection::InvocationNodes => "invocation_nodes",
        InstalledInspectProjection::Calls => "calls",
        InstalledInspectProjection::Resources => "resources",
        InstalledInspectProjection::StateCells => "state_cells",
        InstalledInspectProjection::UiNodes => "ui_nodes",
        InstalledInspectProjection::PresentationCandidates => "presentation_candidates",
        InstalledInspectProjection::RuntimeBindings => "runtime_bindings",
        InstalledInspectProjection::SecurityDecisions => "security_decisions",
        _ => unreachable!("unknown installed Inspector projection"),
    }
}

fn projection_tag(projection: InstalledInspectProjection) -> u8 {
    match projection {
        InstalledInspectProjection::InvocationNodes => InspectCarrierKind::InvocationNodes.tag(),
        InstalledInspectProjection::Calls => InspectCarrierKind::Calls.tag(),
        InstalledInspectProjection::Resources => InspectCarrierKind::Resources.tag(),
        InstalledInspectProjection::StateCells => InspectCarrierKind::StateCells.tag(),
        InstalledInspectProjection::UiNodes => InspectCarrierKind::UiNodes.tag(),
        InstalledInspectProjection::PresentationCandidates => {
            InspectCarrierKind::PresentationCandidates.tag()
        }
        InstalledInspectProjection::RuntimeBindings => InspectCarrierKind::RuntimeBindings.tag(),
        InstalledInspectProjection::SecurityDecisions => {
            InspectCarrierKind::SecurityDecisions.tag()
        }
        _ => unreachable!("unknown installed Inspector projection"),
    }
}
