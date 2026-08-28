use std::{error::Error, process};

use orna_artifact::client_plan::{ClientPlan, FORMAT_IDENTITY, FORMAT_VERSION};
use orna_client::{ClientArtifactIntegrityError, validate_client_artifact_integrity};
use orna_core::{
    canonical_hash::artifact_payload_digest,
    revision::{ExecutableArtifact, ExecutableArtifactKind, Sha256Digest},
};

fn main() {
    if let Err(error) = run() {
        eprintln!("client_artifact_demo: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let payload = ClientPlan::return_boolean(true).encode();
    let digest = artifact_payload_digest(&payload)?;
    let valid = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        FORMAT_IDENTITY,
        FORMAT_VERSION,
        payload.clone(),
        digest,
    )?;
    validate_client_artifact_integrity(&valid)?;
    let plan = ClientPlan::decode(valid.payload())?;
    if !plan.returned_boolean() {
        return Err("verified client plan returned the wrong value".into());
    }
    println!("client artifact integrity: valid CLIENT plan accepted and decoded");

    let wrong_kind = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        orna_artifact::client_plan::FORMAT_IDENTITY,
        orna_artifact::client_plan::FORMAT_VERSION,
        payload.clone(),
        digest,
    )?;
    assert_eq!(
        validate_client_artifact_integrity(&wrong_kind),
        Err(ClientArtifactIntegrityError::WrongExecutionDomain),
    );
    println!("client artifact integrity: SERVER payload rejected");

    let wrong_digest = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        orna_artifact::client_plan::FORMAT_IDENTITY,
        orna_artifact::client_plan::FORMAT_VERSION,
        payload,
        Sha256Digest::from_bytes([0; 32]),
    )?;
    assert_eq!(
        validate_client_artifact_integrity(&wrong_digest),
        Err(ClientArtifactIntegrityError::PayloadDigest),
    );
    println!("client artifact integrity: digest mismatch rejected");
    Ok(())
}
