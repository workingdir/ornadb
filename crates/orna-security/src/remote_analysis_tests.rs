//! Remote-analysis policy tests. They live at the crate root as
//! `remote_analysis_tests` because the gate evidence manifest names them by that path.

use super::*;

const SNAPSHOT: &str = "git:source=abc;deps=def;resources=ghi;toolchain=jkl;abi=mno";

fn disclosed() -> Vec<String> {
    ["source", "dependency-snapshot", "query"]
        .map(str::to_owned)
        .to_vec()
}

fn allowed_within(max_cost_units: u64) -> RemoteAnalysisPolicy {
    RemoteAnalysisPolicy::AllowRemoteDisclosure { max_cost_units }
}

#[test]
fn local_only_policy_refuses_disclosure_even_when_cost_is_acknowledged() {
    let request = RemoteAnalysisRequest::new(
        RemoteAnalysisPolicy::default(),
        SNAPSHOT,
        disclosed(),
        0,
        CostAcknowledgement::Given,
    );
    assert_eq!(request.decision(), RemoteAnalysisDecision::DeniedLocalOnly);
    assert!(!request.is_allowed());
    assert!(request.disclosed_data().is_empty());
    assert_eq!(request.source_snapshot(), SNAPSHOT);
    assert_eq!(
        RecordedRemoteAnalysis::record(&request, Reproducibility::NotReproducible),
        Err(RemoteAnalysisRecordError::NotAllowed(
            RemoteAnalysisDecision::DeniedLocalOnly
        ))
    );
}

#[test]
fn zero_cost_disclosure_needs_no_cost_acknowledgement_and_records_the_boundary() {
    let request = RemoteAnalysisRequest::new(
        allowed_within(10),
        SNAPSHOT,
        disclosed(),
        0,
        CostAcknowledgement::Withheld,
    );
    assert_eq!(
        request.decision(),
        RemoteAnalysisDecision::AllowedByUserPolicy
    );
    assert_eq!(request.disclosed_data(), disclosed().as_slice());
}

#[test]
fn nontrivial_cost_within_ceiling_requires_per_analysis_acknowledgement() {
    let withheld = RemoteAnalysisRequest::new(
        allowed_within(10),
        SNAPSHOT,
        disclosed(),
        5,
        CostAcknowledgement::Withheld,
    );
    assert_eq!(
        withheld.decision(),
        RemoteAnalysisDecision::DeniedCostUnacknowledged
    );
    assert!(withheld.disclosed_data().is_empty());

    let given = RemoteAnalysisRequest::new(
        allowed_within(10),
        SNAPSHOT,
        disclosed(),
        5,
        CostAcknowledgement::Given,
    );
    assert!(given.is_allowed());
    assert_eq!(given.declared_cost_units(), 5);
}

#[test]
fn cost_above_host_ceiling_is_refused_even_when_acknowledged() {
    let over = RemoteAnalysisRequest::new(
        allowed_within(10),
        SNAPSHOT,
        disclosed(),
        11,
        CostAcknowledgement::Given,
    );
    assert_eq!(
        over.decision(),
        RemoteAnalysisDecision::DeniedCostExceedsPolicy
    );
    assert!(over.disclosed_data().is_empty());

    let at_ceiling = RemoteAnalysisRequest::new(
        allowed_within(10),
        SNAPSHOT,
        disclosed(),
        10,
        CostAcknowledgement::Given,
    );
    assert!(at_ceiling.is_allowed());
}

#[test]
fn recording_refuses_every_request_the_policy_did_not_allow() {
    let denied = [
        RemoteAnalysisRequest::new(
            RemoteAnalysisPolicy::LocalOnly,
            SNAPSHOT,
            disclosed(),
            0,
            CostAcknowledgement::Given,
        ),
        RemoteAnalysisRequest::new(
            allowed_within(10),
            SNAPSHOT,
            disclosed(),
            5,
            CostAcknowledgement::Withheld,
        ),
        RemoteAnalysisRequest::new(
            allowed_within(10),
            SNAPSHOT,
            disclosed(),
            11,
            CostAcknowledgement::Given,
        ),
    ];
    for request in &denied {
        assert_eq!(
            RecordedRemoteAnalysis::record(request, Reproducibility::NotReproducible),
            Err(RemoteAnalysisRecordError::NotAllowed(request.decision()))
        );
    }
}

#[test]
fn provenance_snapshot_is_not_a_reproducibility_claim() {
    let request = RemoteAnalysisRequest::new(
        allowed_within(0),
        SNAPSHOT,
        disclosed(),
        0,
        CostAcknowledgement::Withheld,
    );
    let recorded =
        RecordedRemoteAnalysis::record(&request, Reproducibility::NotReproducible).unwrap();
    assert_eq!(recorded.source_snapshot(), SNAPSHOT);
    assert_eq!(recorded.disclosed_data(), disclosed().as_slice());
    assert_eq!(recorded.declared_cost_units(), 0);
    assert_eq!(
        recorded.reproducibility(),
        &Reproducibility::NotReproducible
    );
}

#[test]
fn replay_and_reexecution_are_distinct_claims_over_a_retained_artifact() {
    let artifact = RetainedArtifact::new("model-artifacts:sha256:abc").unwrap();
    let request = RemoteAnalysisRequest::new(
        allowed_within(0),
        SNAPSHOT,
        disclosed(),
        0,
        CostAcknowledgement::Withheld,
    );
    let replay = RecordedRemoteAnalysis::record(
        &request,
        Reproducibility::RetainedOutputReplay(artifact.clone()),
    )
    .unwrap();
    let reexecution = RecordedRemoteAnalysis::record(
        &request,
        Reproducibility::ReExecutionWithRetainedArtifacts(artifact.clone()),
    )
    .unwrap();
    assert_eq!(
        replay.reproducibility(),
        &Reproducibility::RetainedOutputReplay(artifact.clone())
    );
    assert_eq!(
        reexecution.reproducibility(),
        &Reproducibility::ReExecutionWithRetainedArtifacts(artifact)
    );
    assert_ne!(replay.reproducibility(), reexecution.reproducibility());
}

#[test]
fn retained_artifact_refuses_empty_and_control_character_identities() {
    assert_eq!(RetainedArtifact::new(""), Err(RetainedArtifactError::Empty));
    assert_eq!(
        RetainedArtifact::new("model\nartifact"),
        Err(RetainedArtifactError::ControlCharacter)
    );
    assert_eq!(
        RetainedArtifact::new("model:sha256:abc")
            .unwrap()
            .identity(),
        "model:sha256:abc"
    );
}

#[test]
fn cost_bounds_hold_at_u64_maximum() {
    let unbounded = RemoteAnalysisRequest::new(
        allowed_within(u64::MAX),
        SNAPSHOT,
        disclosed(),
        u64::MAX,
        CostAcknowledgement::Given,
    );
    assert!(unbounded.is_allowed());
    assert_eq!(unbounded.declared_cost_units(), u64::MAX);

    let capped = RemoteAnalysisRequest::new(
        allowed_within(u64::MAX - 1),
        SNAPSHOT,
        disclosed(),
        u64::MAX,
        CostAcknowledgement::Given,
    );
    assert_eq!(
        capped.decision(),
        RemoteAnalysisDecision::DeniedCostExceedsPolicy
    );
    assert!(capped.disclosed_data().is_empty());
}

#[test]
fn zero_ceiling_refuses_any_cost_even_when_acknowledged() {
    let request = RemoteAnalysisRequest::new(
        allowed_within(0),
        SNAPSHOT,
        disclosed(),
        1,
        CostAcknowledgement::Given,
    );
    assert_eq!(
        request.decision(),
        RemoteAnalysisDecision::DeniedCostExceedsPolicy
    );
    assert!(request.disclosed_data().is_empty());
    assert_eq!(
        RecordedRemoteAnalysis::record(&request, Reproducibility::NotReproducible),
        Err(RemoteAnalysisRecordError::NotAllowed(
            RemoteAnalysisDecision::DeniedCostExceedsPolicy
        ))
    );
}

#[test]
fn ceiling_check_takes_precedence_over_acknowledgement() {
    let request = RemoteAnalysisRequest::new(
        allowed_within(10),
        SNAPSHOT,
        disclosed(),
        11,
        CostAcknowledgement::Withheld,
    );
    assert_eq!(
        request.decision(),
        RemoteAnalysisDecision::DeniedCostExceedsPolicy
    );
}
