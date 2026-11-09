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
