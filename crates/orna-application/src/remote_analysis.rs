//! The production boundary for external-model analysis (ORNA-KNOW-003).
//!
//! ORNA-KNOW-003 requires explicit host/operator policy for external model use,
//! content disclosure and nontrivial cost. Recorded provenance never promises
//! that a changing external service reproduces a result, and a reproducibility
//! claim must distinguish replaying a retained output from re-executing with
//! retained model artifacts.
//!
//! [`RemoteAnalysisGate`] is the production entry point for an integration that
//! would perform a remote call. It captures the host/operator policy once and
//! returns the [`RecordedRemoteAnalysis`] in the same call that authorises it, so
//! an integration cannot obtain an allowed decision without recording it. The
//! provenance identity is derived from a committed [`LoadedProject`] and the
//! exact [`AdmittedApplication`] source that program admitted; it is never
//! caller-supplied text, and a worktree project whose source can change during
//! the analysis has no recordable provenance.
//!
//! This crate contains no remote provider. An integration performs the network
//! call itself, only after authorisation, and sends only
//! [`RecordedRemoteAnalysis::disclosed_data`].

use crate::AdmittedApplication;
use orna_project::LoadedProject;
use orna_security::{
    CostAcknowledgement, RecordedRemoteAnalysis, RemoteAnalysisPolicy, RemoteAnalysisRecordError,
    RemoteAnalysisRequest, Reproducibility,
};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// Why an admitted program has no exact remote-analysis provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteAnalysisProvenanceError {
    /// The project was loaded from a worktree, so its source can change while
    /// the analysis runs. There is no committed revision to record.
    UnpinnedSource,
    /// The admitted application source is not the module at its logical path in
    /// the pinned project snapshot, so the snapshot does not identify it.
    SourceNotInSnapshot,
}

/// Exact admitted program and dependency identity of one remote disclosure.
///
/// It binds the committed repository snapshot, the admitted module's logical
/// path and source digest, and the pinned standard dependency profile. It says
/// nothing about reproducibility: a changing external service may still not
/// reproduce the same result (ORNA-KNOW-003).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteAnalysisProvenance {
    identity: String,
}

impl RemoteAnalysisProvenance {
    /// Derives the provenance of `application` from the exact committed project
    /// snapshot it was admitted from.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteAnalysisProvenanceError::UnpinnedSource`] when `project`
    /// has no committed source snapshot, and
    /// [`RemoteAnalysisProvenanceError::SourceNotInSnapshot`] when the admitted
    /// source digest does not match its module in that snapshot.
    pub fn from_admitted_project(
        project: &LoadedProject,
        application: &AdmittedApplication,
    ) -> Result<Self, RemoteAnalysisProvenanceError> {
        let snapshot = project
            .source_snapshot_id()
            .ok_or(RemoteAnalysisProvenanceError::UnpinnedSource)?;
        let module = project
            .modules()
            .iter()
            .find(|module| module.logical_path == application.logical_path())
            .ok_or(RemoteAnalysisProvenanceError::SourceNotInSnapshot)?;
        let digest: [u8; 32] = Sha256::digest(module.source.as_bytes()).into();
        if digest != application.source_digest() {
            return Err(RemoteAnalysisProvenanceError::SourceNotInSnapshot);
        }

        let mut identity = format!(
            "git:{};module:{}:{}",
            hex(&snapshot),
            application.logical_path(),
            hex(&digest)
        );
        match project.standard_profile() {
            Some(profile) => {
                identity.push_str(";std:");
                identity.push_str(profile.snapshot());
                for (logical_path, digest) in profile.module_digests() {
                    let _ = write!(identity, ";{logical_path}:{}", hex(digest));
                }
            }
            None => identity.push_str(";std:none"),
        }
        Ok(Self { identity })
    }

    /// The exact provenance identity recorded on an authorised analysis.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }
}

/// The host/operator authority for remote analysis.
///
/// There is no transport-derived consent: a gate constructed with
/// [`RemoteAnalysisPolicy::LocalOnly`] (the policy default) refuses every
/// request, and no analysis acknowledgement can raise the policy ceiling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteAnalysisGate {
    policy: RemoteAnalysisPolicy,
}

impl RemoteAnalysisGate {
    #[must_use]
    pub const fn new(policy: RemoteAnalysisPolicy) -> Self {
        Self { policy }
    }

    #[must_use]
    pub const fn policy(&self) -> RemoteAnalysisPolicy {
        self.policy
    }

    /// Authorises one remote analysis and records the disclosed boundary,
    /// declared cost and reproducibility claim together.
    ///
    /// A denied request discloses nothing and returns the decision as
    /// [`RemoteAnalysisRecordError::NotAllowed`], so no record exists for a
    /// caller to act on. This performs no I/O.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteAnalysisRecordError::NotAllowed`] with the decision when
    /// the host/operator policy did not allow the request.
    pub fn authorise(
        &self,
        provenance: &RemoteAnalysisProvenance,
        disclosed_data: impl IntoIterator<Item = String>,
        declared_cost_units: u64,
        acknowledgement: CostAcknowledgement,
        reproducibility: Reproducibility,
    ) -> Result<RecordedRemoteAnalysis, RemoteAnalysisRecordError> {
        let request = RemoteAnalysisRequest::new(
            self.policy,
            provenance.identity.clone(),
            disclosed_data,
            declared_cost_units,
            acknowledgement,
        );
        RecordedRemoteAnalysis::record(&request, reproducibility)
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}
