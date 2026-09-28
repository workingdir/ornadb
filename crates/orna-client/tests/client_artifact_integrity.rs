use orna_artifact::client_plan::{ClientPlan, FORMAT_IDENTITY, FORMAT_VERSION};
use orna_client::{ClientArtifactIntegrityError, validate_client_artifact_integrity};
use orna_core::{
    canonical_hash::artifact_payload_digest,
    revision::{ExecutableArtifact, ExecutableArtifactKind, Sha256Digest},
};

#[test]
fn public_validator_accepts_a_client_artifact_with_a_matching_digest() {
    let payload = ClientPlan::return_boolean(true).encode();
    let digest = artifact_payload_digest(&payload).expect("client plan payload digest");
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        FORMAT_IDENTITY,
        FORMAT_VERSION,
        payload,
        digest,
    )
    .expect("valid client artifact");

    assert_eq!(validate_client_artifact_integrity(&artifact), Ok(()));
}

#[test]
fn public_validator_rejects_a_non_client_execution_domain() {
    let payload = ClientPlan::return_boolean(true).encode();
    let digest = artifact_payload_digest(&payload).expect("client plan payload digest");
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        FORMAT_IDENTITY,
        FORMAT_VERSION,
        payload,
        digest,
    )
    .expect("valid server artifact");

    assert_eq!(
        validate_client_artifact_integrity(&artifact),
        Err(ClientArtifactIntegrityError::WrongExecutionDomain),
    );
}

#[test]
fn public_validator_rejects_a_client_artifact_with_a_mismatched_digest() {
    let payload = ClientPlan::return_boolean(true).encode();
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        FORMAT_IDENTITY,
        FORMAT_VERSION,
        payload,
        Sha256Digest::from_bytes([0; 32]),
    )
    .expect("constructible client artifact with mismatched digest");

    assert_eq!(
        validate_client_artifact_integrity(&artifact),
        Err(ClientArtifactIntegrityError::PayloadDigest),
    );
}
