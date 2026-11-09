//! A bounded session-security state machine.
//!
//! This crate deliberately performs no TLS, socket, clock, entropy-provider, or
//! session-store I/O. Integrations supply those capabilities through the small
//! adapter traits below; secret storage is the explicit SOPS-backed exception.
//! No request payload may select a provider or authority.

mod remote_analysis;
#[cfg(test)]
mod remote_analysis_tests;
mod secrets;
mod session;
mod sops;

pub use remote_analysis::{
    CostAcknowledgement, RecordedRemoteAnalysis, RemoteAnalysisDecision, RemoteAnalysisPolicy,
    RemoteAnalysisRecordError, RemoteAnalysisRequest, Reproducibility, RetainedArtifact,
    RetainedArtifactError,
};
pub use secrets::{SecretBoundaryError, SecretMetadata, SecretRef, SecretResolver};
pub use session::{
    AttachOutcome, AttachmentId, BoundaryError, CREDENTIAL_BYTES, CredentialIssuer, Lease,
    MAX_SESSION_LEASE, OpaqueCredential, Origin, OriginPolicy, SessionBoundary,
    SessionDeletionAdapter, SessionId, SocketAdapter, TlsPeerAdapter,
};
pub use sops::SopsProvider;
