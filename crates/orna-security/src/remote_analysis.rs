//! Auditable records of remote-analysis consent, disclosure, and reproducibility.

/// Explicit host/operator policy controlling whether source may be disclosed for remote analysis.
/// The default is local-only; transport configuration never implies consent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RemoteAnalysisPolicy {
    /// No content, dependency, resource, or query data leaves the host.
    #[default]
    LocalOnly,
    /// Remote disclosure is permitted only for analyses whose declared cost does not exceed
    /// `max_cost_units`. The ceiling is host/operator authority: no per-analysis acknowledgement
    /// can raise it.
    AllowRemoteDisclosure { max_cost_units: u64 },
}

/// Caller-supplied consent to nontrivial cost for one analysis. It narrows what the policy
/// permits and never raises the policy ceiling.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CostAcknowledgement {
    #[default]
    Withheld,
    Given,
}

/// Exact provenance and disclosure boundary for one remote analysis attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteAnalysisRequest {
    decision: RemoteAnalysisDecision,
    source_snapshot: String,
    disclosed_data: Vec<String>,
    declared_cost_units: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteAnalysisDecision {
    DeniedLocalOnly,
    DeniedCostExceedsPolicy,
    DeniedCostUnacknowledged,
    AllowedByUserPolicy,
}

impl RemoteAnalysisRequest {
    /// Makes an auditable request record. It does not perform network I/O.
    /// The snapshot identity must identify the exact admitted source/dependency/resource inputs.
    /// Any nonzero `declared_cost_units` is nontrivial: it needs the policy ceiling to cover it
    /// and `CostAcknowledgement::Given`. A denied request discloses nothing, so its disclosed
    /// boundary is empty.
    #[must_use]
    pub fn new(
        policy: RemoteAnalysisPolicy,
        source_snapshot: impl Into<String>,
        disclosed_data: impl IntoIterator<Item = String>,
        declared_cost_units: u64,
        acknowledgement: CostAcknowledgement,
    ) -> Self {
        let decision = match policy {
            RemoteAnalysisPolicy::LocalOnly => RemoteAnalysisDecision::DeniedLocalOnly,
            RemoteAnalysisPolicy::AllowRemoteDisclosure { max_cost_units }
                if declared_cost_units > max_cost_units =>
            {
                RemoteAnalysisDecision::DeniedCostExceedsPolicy
            }
            RemoteAnalysisPolicy::AllowRemoteDisclosure { .. }
                if declared_cost_units > 0 && acknowledgement == CostAcknowledgement::Withheld =>
            {
                RemoteAnalysisDecision::DeniedCostUnacknowledged
            }
            RemoteAnalysisPolicy::AllowRemoteDisclosure { .. } => {
                RemoteAnalysisDecision::AllowedByUserPolicy
            }
        };
        let disclosed_data = if decision == RemoteAnalysisDecision::AllowedByUserPolicy {
            disclosed_data.into_iter().collect()
        } else {
            Vec::new()
        };
        Self {
            decision,
            source_snapshot: source_snapshot.into(),
            disclosed_data,
            declared_cost_units,
        }
    }

    #[must_use]
    pub fn decision(&self) -> RemoteAnalysisDecision {
        self.decision
    }

    #[must_use]
    pub fn source_snapshot(&self) -> &str {
        &self.source_snapshot
    }

    #[must_use]
    pub fn disclosed_data(&self) -> &[String] {
        &self.disclosed_data
    }

    #[must_use]
    pub fn declared_cost_units(&self) -> u64 {
        self.declared_cost_units
    }

    #[must_use]
    pub fn is_allowed(&self) -> bool {
        self.decision == RemoteAnalysisDecision::AllowedByUserPolicy
    }
}

/// Identity of a retained artifact a reproduction depends on: a retained remote output, or the
/// retained model artifacts (one identity, e.g. a manifest digest, covering the set). It is
/// constructed only through `new`, which refuses an empty or control-character identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedArtifact(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedArtifactError {
    Empty,
    ControlCharacter,
}

impl RetainedArtifact {
    /// # Errors
    ///
    /// Returns `Empty` for an empty identity and `ControlCharacter` when it contains a control
    /// character.
    pub fn new(identity: impl Into<String>) -> Result<Self, RetainedArtifactError> {
        let identity = identity.into();
        if identity.is_empty() {
            return Err(RetainedArtifactError::Empty);
        }
        if identity.chars().any(char::is_control) {
            return Err(RetainedArtifactError::ControlCharacter);
        }
        Ok(Self(identity))
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.0
    }
}

/// How a recorded remote result can be reproduced. Source snapshot provenance is not a
/// reproducibility claim: a changing external service may not reproduce the same result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reproducibility {
    /// The retained output is replayed; the external service is not called again.
    RetainedOutputReplay(RetainedArtifact),
    /// The analysis is re-run, and only with the retained model artifacts identified here.
    /// Limitation: the payload is the same `RetainedArtifact` type as `RetainedOutputReplay`, so
    /// the type does not prove the identity names model artifacts rather than a retained output.
    /// Callers must supply the model-artifact identity; a distinct model-artifact type is open.
    ReExecutionWithRetainedArtifacts(RetainedArtifact),
    NotReproducible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteAnalysisRecordError {
    /// The policy did not allow the request, so nothing may be recorded as disclosed.
    NotAllowed(RemoteAnalysisDecision),
}

/// Audit record of one allowed remote analysis: provenance, disclosed boundary, declared cost,
/// and a reproducibility claim that must be stated explicitly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedRemoteAnalysis {
    source_snapshot: String,
    disclosed_data: Vec<String>,
    declared_cost_units: u64,
    reproducibility: Reproducibility,
}

impl RecordedRemoteAnalysis {
    /// # Errors
    ///
    /// Returns `NotAllowed` with the request's decision when `request` is not allowed.
    pub fn record(
        request: &RemoteAnalysisRequest,
        reproducibility: Reproducibility,
    ) -> Result<Self, RemoteAnalysisRecordError> {
        if !request.is_allowed() {
            return Err(RemoteAnalysisRecordError::NotAllowed(request.decision()));
        }
        Ok(Self {
            source_snapshot: request.source_snapshot.clone(),
            disclosed_data: request.disclosed_data.clone(),
            declared_cost_units: request.declared_cost_units,
            reproducibility,
        })
    }

    #[must_use]
    pub fn source_snapshot(&self) -> &str {
        &self.source_snapshot
    }

    #[must_use]
    pub fn disclosed_data(&self) -> &[String] {
        &self.disclosed_data
    }

    #[must_use]
    pub fn declared_cost_units(&self) -> u64 {
        self.declared_cost_units
    }

    #[must_use]
    pub fn reproducibility(&self) -> &Reproducibility {
        &self.reproducibility
    }
}

#[cfg(test)]
mod tests {
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
}
