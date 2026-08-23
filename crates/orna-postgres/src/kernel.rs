// Concrete PostgreSQL storage and transaction kernel for OrnaDB.
//
// PostgreSQL is private implementation machinery. This module does not expose
// PostgreSQL as an Orna language, protocol, or catalogue contract.

use std::{error::Error, fmt, str::FromStr};

use orna_client::ClientExecutionError;
use orna_client::capability::LocalCapabilityGrantSet;
use orna_core::{
    FunctionId,
    canonical_hash::CanonicalHashError,
    catalogue::CatalogueSnapshotError,
    inspect::InspectError,
    invocation::InvocationCarrierConstructionError,
    physical::PhysicalPlanError,
    revision::{CatalogueHashVersion, RevisionInvariantError, RevisionPair},
    security::{
        ExecuteDenial, InspectDenial, LocalPeerAuthenticationError, PrivilegeDenial,
        SecuritySnapshotError,
    },
    state::UserStateError,
};
use orna_protocol::{FrameCodecError, ValueCodecError};
use orna_standard::StandardUpgradeIdentity;

use tokio::task::{JoinError, JoinHandle};
use tokio_postgres::{Client, Config, NoTls};

/// Returns whether `type_id` is one of the exact transient Inspector carrier
/// identities. The `f7` security-principal identity is deliberately excluded.
pub(crate) fn is_sealed_inspect_type_id(type_id: orna_core::TypeId) -> bool {
    let bytes = type_id.to_bytes();
    bytes[..15].iter().all(|byte| *byte == 0)
        && matches!(bytes[15], 0xf3..=0xf6 | 0xf8..=0xff)
}

#[path = "kernel/apply.rs"]
pub(crate) mod apply;
#[path = "kernel/bootstrap.rs"]
pub(crate) mod bootstrap;
#[path = "kernel/decode.rs"]
pub(crate) mod decode;
#[path = "kernel/inspect.rs"]
pub(crate) mod inspect;
#[path = "kernel/physical.rs"]
pub(crate) mod physical;
#[path = "kernel/recovery.rs"]
pub(crate) mod recovery;
#[path = "kernel/security.rs"]
pub(crate) mod security;
#[path = "kernel/security_admin.rs"]
pub(crate) mod security_admin;
#[path = "kernel/server_execution.rs"]
pub(crate) mod server_execution;
#[path = "kernel/server_mutation_execution.rs"]
pub(crate) mod server_mutation_execution;
#[path = "kernel/server_runtime.rs"]
pub(crate) mod server_runtime;
#[path = "kernel/state.rs"]
pub(crate) mod state;
#[path = "kernel/storage.rs"]
pub(crate) mod storage;

pub use apply::StandardContextIdentity;
pub use bootstrap::ActiveRevision;
pub use inspect::AuthenticatedInspectSnapshot;
pub use recovery::RevisionPairHistoryEntry;
pub use orna_core::inspect::InspectSnapshotEpoch;
pub use security::{
    AuthenticatedRawCallResult, AuthenticatedServerResourceAccepted,
    AuthenticatedServerResourceEvent, AuthenticatedServerResourceKind,
    AuthenticatedServerResourceProducer, AuthenticatedServerResourceResult,
    AuthenticatedServerResourceStart, RecordArgumentPreflight, ResourceCancellation,
    ResourceCredit, SealedInvocationContinuation, SealedInvocationExecution,
    SealedInvocationOperation, SealedInvocationPreflight, SealedInvocationResult,
};
pub use server_execution::{ServerSelectContext, ServerSelectError, ServerSelectResult};
pub use server_mutation_execution::{
    ServerDeleteCommitState, ServerDeleteContext, ServerDeleteError, ServerDeleteResult,
    ServerInsertCommitState, ServerInsertContext, ServerInsertError, ServerInsertResult,
    ServerMutationCommitState, ServerMutationContext, ServerMutationError, ServerUpdateCommitState,
    ServerUpdateContext, ServerUpdateError, ServerUpdateResult,
};
pub use state::UserStateInstanceRequest;

/// The typed source for an unavailable authenticated raw SERVER target.
///
/// Raw dispatch supports separate immutable artefact families for `SELECT`
/// and the narrow parameter-free `INSERT` form. The source retains that
/// family without changing the closed public failure category.
#[non_exhaustive]
#[derive(Debug)]
pub enum RawServerTargetError {
    /// The pinned target failed the raw SERVER `SELECT` boundary.
    Select(ServerSelectError),
    /// The pinned target failed the raw SERVER `INSERT` boundary.
    Insert(ServerInsertError),
}

impl fmt::Display for RawServerTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Select(error) => error.fmt(formatter),
            Self::Insert(error) => error.fmt(formatter),
        }
    }
}

impl Error for RawServerTargetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Select(error) => Some(error),
            Self::Insert(error) => Some(error),
        }
    }
}

/// A concrete connection point for the private PostgreSQL kernel.
#[derive(Clone)]
pub struct PostgresKernel {
    config: Config,
    capability_grants: LocalCapabilityGrantSet,
}

impl PostgresKernel {
    /// Creates a kernel from an already parsed PostgreSQL connection config.
    ///
    /// The default grant set is empty. Callers that run the local CLIENT
    /// sandbox can install grants from local configuration with
    /// [`PostgresKernel::with_capability_grants`].
    pub const fn new(config: Config) -> Self {
        Self {
            config,
            capability_grants: LocalCapabilityGrantSet::new(),
        }
    }

    /// Returns a kernel that uses the supplied local CLIENT capability grants.
    ///
    /// The grant set is caller-owned configuration. The database cannot change
    /// it.
    pub fn with_capability_grants(mut self, capability_grants: LocalCapabilityGrantSet) -> Self {
        self.capability_grants = capability_grants;
        self
    }

    /// Verifies that the configured private kernel accepts a query.
    pub async fn health_check(&self) -> Result<(), PostgresKernelError> {
        let session = self.open().await?;
        let query_result = session.client.query_one("SELECT 1", &[]).await;
        let shutdown_result = session.shutdown().await;

        query_result.map_err(PostgresKernelError::Database)?;
        shutdown_result
    }

    async fn open(&self) -> Result<PostgresSession, PostgresKernelError> {
        let (client, connection) = self
            .config
            .connect(NoTls)
            .await
            .map_err(PostgresKernelError::Database)?;
        let driver = tokio::spawn(connection);

        Ok(PostgresSession { client, driver })
    }
}

impl FromStr for PostgresKernel {
    type Err = PostgresKernelError;

    fn from_str(connection_string: &str) -> Result<Self, Self::Err> {
        connection_string
            .parse::<Config>()
            .map(Self::new)
            .map_err(PostgresKernelError::Configuration)
    }
}

struct PostgresSession {
    client: Client,
    driver: JoinHandle<Result<(), tokio_postgres::Error>>,
}

impl PostgresSession {
    #[cfg(feature = "test-hooks")]
    fn abort_driver(&self) {
        self.driver.abort();
    }

    async fn shutdown(self) -> Result<(), PostgresKernelError> {
        let Self { client, driver } = self;
        drop(client);
        driver
            .await
            .map_err(PostgresKernelError::DriverTask)?
            .map_err(PostgresKernelError::Database)
    }
}

/// A failure to configure or communicate with the private PostgreSQL kernel.
#[derive(Debug)]
pub enum PostgresKernelError {
    /// The configured PostgreSQL connection parameters are invalid.
    Configuration(tokio_postgres::Error),
    /// PostgreSQL rejected or failed a connection or query.
    Database(tokio_postgres::Error),
    /// The asynchronous PostgreSQL connection driver terminated abnormally.
    DriverTask(JoinError),
    /// A recorded schema migration does not match this binary.
    MigrationMismatch {
        /// The incompatible migration version.
        version: i64,
    },
    /// Protected catalogue rows violate a durable kernel invariant.
    CatalogueInvariant(&'static str),
    /// Canonical version-1 hash construction failed.
    CanonicalHash(CanonicalHashError),
    /// Reconstructed revision values violate a core revision invariant.
    RevisionInvariant(RevisionInvariantError),
    /// Candidate revision values violate a core persistence invariant.
    CandidateRevisionInvariant(RevisionInvariantError),
    /// Reconstructed semantic definitions do not form a valid catalogue.
    CatalogueSnapshot(CatalogueSnapshotError),
    /// Recovered security records do not form a valid decision snapshot.
    SecuritySnapshot(SecuritySnapshotError),
    /// A security replacement targets a revision other than the active pair.
    SecurityRevisionMismatch {
        /// The revision carried by the replacement snapshot.
        expected: RevisionPair,
        /// The revision locked by the replacement transaction.
        active: RevisionPair,
    },
    /// A security replacement does not bind the complete active function set.
    SecurityFunctionSetMismatch,
    /// The active security snapshot denied a CLIENT function invocation.
    ClientExecuteDenied {
        /// The active revision pair used for the decision.
        pair: RevisionPair,
        /// The requested function identity.
        function: FunctionId,
        /// The fail-closed reason for denying execution.
        reason: ExecuteDenial,
    },
    /// The active security snapshot denied a SERVER SELECT invocation.
    ServerExecuteDenied {
        /// The active revision pair used for the decision.
        pair: RevisionPair,
        /// The requested function identity.
        function: FunctionId,
        /// The fail-closed reason for denying execution.
        reason: ExecuteDenial,
    },
    /// The active security snapshot denied a raw invocation before domain selection.
    RawExecuteDenied {
        /// The active revision pair used for the decision.
        pair: RevisionPair,
        /// The requested function identity.
        function: FunctionId,
        /// The fail-closed reason for denying execution.
        reason: ExecuteDenial,
    },
    /// An authorised function is unavailable at the generic raw-call boundary.
    RawCallTargetUnavailable {
        /// The requested function identity.
        function: FunctionId,
        /// The closed raw-call rule that rejected the target.
        rule: &'static str,
    },
    /// A sealed `sys.invoke` carrier or event failed checked construction.
    InvocationCarrier(InvocationCarrierConstructionError),
    /// A sealed `sys.invoke` carrier or event batch failed the frame codec.
    SealedInvocation(FrameCodecError),
    /// An allowed SERVER function is unavailable at the closed raw-call boundary.
    RawServerTargetUnavailable {
        /// The exact typed SERVER target validation failure.
        source: RawServerTargetError,
    },
    /// An authorised CLIENT function could not be evaluated.
    ClientExecution(ClientExecutionError),
    /// A USER state model validation failed with a closed ORNA09 error.
    UserState(UserStateError),
    /// A USER state value failed the canonical ORV5 codec.
    UserStateValueCodec(ValueCodecError),
    /// An inspection model invariant failed with a closed error.
    Inspect(InspectError),
    /// The session principal was denied INSPECT access to an inspection epoch.
    InspectDenied {
        /// The fail-closed denial reason.
        reason: InspectDenial,
    },
    /// The calling session was denied a security-admin mutation.
    SecurityAdminDenied {
        /// The fail-closed privilege denial reason.
        reason: PrivilegeDenial,
    },
    /// An inspection payload failed the canonical ORV5 codec.
    InspectValueCodec(ValueCodecError),
    /// A kernel-supplied local peer UID could not establish an Orna session.
    LocalPeerAuthentication(LocalPeerAuthenticationError),
    /// The candidate was prepared against a revision pair that is no longer active.
    ExpectedBaseMismatch {
        /// The base pair carried by the candidate.
        expected: RevisionPair,
        /// The pair locked by the apply transaction.
        active: RevisionPair,
    },
    /// The candidate uses a different catalogue hash context version.
    StandardContextTransitionRequired {
        /// The locked active catalogue hash version.
        active: CatalogueHashVersion,
        /// The candidate catalogue hash version.
        candidate: CatalogueHashVersion,
    },
    /// The candidate is pinned to a different verified standard context.
    StandardContextMismatch {
        /// The context reconstructed from the locked active revision.
        active: Box<StandardContextIdentity>,
        /// The context carried by the candidate.
        candidate: Box<StandardContextIdentity>,
    },
    /// A durable identity is reserved for the standard library upgrade.
    ReservedStandardIdentity {
        /// The conflicting durable identity.
        identity: StandardUpgradeIdentity,
    },
    /// Backend-neutral physical planning rejected the candidate.
    PhysicalPlan(PhysicalPlanError),
    /// A SERVER SELECT function cannot execute against the active revision.
    ServerSelect(ServerSelectError),
    /// A SERVER INSERT function cannot execute or complete its commit lifecycle.
    ServerInsert(ServerInsertError),
    /// A SERVER UPDATE function cannot execute or complete its commit lifecycle.
    ServerUpdate(ServerUpdateError),
    /// A SERVER DELETE function cannot execute or complete its commit lifecycle.
    ServerDelete(ServerDeleteError),
    /// A durable row value could not be decoded as its selected PostgreSQL type.
    RowDecode {
        /// The relation that supplied the row.
        relation: &'static str,
        /// The stable record description available at the decode boundary.
        record: String,
        /// The selected column that failed to decode.
        column: &'static str,
        /// The recovery rule that required this column value.
        rule: &'static str,
        /// The PostgreSQL decode failure.
        source: tokio_postgres::Error,
    },
    /// Durable state violates a recovery rule.
    DurableInvariant {
        /// The relation that owns the invalid state.
        relation: &'static str,
        /// The stable record identity or description.
        record: String,
        /// The exact recovery rule that failed.
        rule: &'static str,
    },
}

impl fmt::Display for PostgresKernelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => {
                write!(formatter, "invalid PostgreSQL configuration: {error}")
            }
            Self::Database(error) => {
                write!(formatter, "private PostgreSQL kernel failure: {error}")
            }
            Self::DriverTask(error) => {
                write!(
                    formatter,
                    "private PostgreSQL connection task failed: {error}"
                )
            }
            Self::MigrationMismatch { version } => {
                write!(
                    formatter,
                    "PostgreSQL kernel migration {version} does not match this binary"
                )
            }
            Self::CatalogueInvariant(message) => {
                write!(
                    formatter,
                    "private PostgreSQL catalogue invariant failed: {message}"
                )
            }
            Self::CanonicalHash(error) => {
                write!(formatter, "canonical durable hash failed: {error}")
            }
            Self::RevisionInvariant(error) => {
                write!(formatter, "recovered revision invariant failed: {error}")
            }
            Self::CandidateRevisionInvariant(error) => {
                write!(formatter, "candidate revision invariant failed: {error}")
            }
            Self::CatalogueSnapshot(error) => {
                write!(formatter, "recovered catalogue snapshot failed: {error}")
            }
            Self::SecuritySnapshot(error) => {
                write!(formatter, "recovered security snapshot failed: {error}")
            }
            Self::SecurityRevisionMismatch { .. } => {
                formatter.write_str("security snapshot revision pair is not active")
            }
            Self::SecurityFunctionSetMismatch => {
                formatter.write_str("security snapshot does not contain the active function set")
            }
            Self::ClientExecuteDenied { .. } => {
                formatter.write_str("CLIENT function execution was denied")
            }
            Self::ServerExecuteDenied { .. } => {
                formatter.write_str("SERVER SELECT execution was denied")
            }
            Self::RawExecuteDenied { .. } => {
                formatter.write_str("raw function execution was denied")
            }
            Self::RawCallTargetUnavailable { function, rule } => {
                write!(
                    formatter,
                    "raw call target {} is unavailable: {rule}",
                    function.canonical()
                )
            }
            Self::InvocationCarrier(error) => {
                write!(formatter, "sealed invocation carrier failed: {error}")
            }
            Self::SealedInvocation(error) => {
                write!(formatter, "sealed sys.invoke carrier failed: {error}")
            }
            Self::RawServerTargetUnavailable { source } => {
                write!(formatter, "raw SERVER target is unavailable: {source}")
            }
            Self::ClientExecution(error) => {
                write!(formatter, "CLIENT function execution failed: {error}")
            }
            Self::UserState(error) => {
                write!(formatter, "USER state operation failed: {error}")
            }
            Self::UserStateValueCodec(error) => {
                write!(formatter, "USER state value codec failed: {error}")
            }
            Self::Inspect(error) => {
                write!(formatter, "inspection model invariant failed: {error}")
            }
            Self::InspectDenied { reason } => {
                write!(
                    formatter,
                    "INSPECT access was denied: {}",
                    reason.audit_reason()
                )
            }
            Self::SecurityAdminDenied { reason } => {
                write!(
                    formatter,
                    "security-admin mutation was denied: {}",
                    reason.audit_reason()
                )
            }
            Self::InspectValueCodec(error) => {
                write!(formatter, "inspection payload codec failed: {error}")
            }
            Self::LocalPeerAuthentication(error) => {
                write!(formatter, "local peer authentication failed: {error}")
            }
            Self::ExpectedBaseMismatch { .. } => {
                formatter.write_str("expected revision pair is not active")
            }
            Self::StandardContextTransitionRequired { .. } => formatter.write_str(
                "the active and candidate catalogue hash versions require a standard context transition",
            ),
            Self::StandardContextMismatch { .. } => {
                formatter.write_str("the active and candidate standard contexts do not match")
            }
            Self::ReservedStandardIdentity { .. } => formatter.write_str(
                "the database contains an identity reserved for the standard library",
            ),
            Self::PhysicalPlan(error) => write!(formatter, "physical plan failed: {error}"),
            Self::ServerSelect(error) => write!(formatter, "server SELECT failed: {error}"),
            Self::ServerInsert(error) => write!(formatter, "row creation failed: {error}"),
            Self::ServerUpdate(error) => write!(formatter, "object update failed: {error}"),
            Self::ServerDelete(error) => write!(formatter, "object deletion failed: {error}"),
            Self::RowDecode {
                relation,
                record,
                column,
                rule,
                source,
            } => {
                write!(
                    formatter,
                    "cannot decode {relation} record {record} column {column} for rule {rule}: {source}"
                )
            }
            Self::DurableInvariant {
                relation,
                record,
                rule,
            } => {
                write!(
                    formatter,
                    "durable invariant failed for {relation} record {record}: {rule}"
                )
            }
        }
    }
}

impl Error for PostgresKernelError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Configuration(error) | Self::Database(error) => Some(error),
            Self::DriverTask(error) => Some(error),
            Self::CanonicalHash(error) => Some(error),
            Self::RevisionInvariant(error) => Some(error),
            Self::CandidateRevisionInvariant(error) => Some(error),
            Self::CatalogueSnapshot(error) => Some(error),
            Self::SecuritySnapshot(error) => Some(error),
            Self::ClientExecution(error) => Some(error),
            Self::UserState(error) => Some(error),
            Self::UserStateValueCodec(error) => Some(error),
            Self::Inspect(error) => Some(error),
            Self::InspectValueCodec(error) => Some(error),
            Self::LocalPeerAuthentication(error) => Some(error),
            Self::InvocationCarrier(error) => Some(error),
            Self::SealedInvocation(error) => Some(error),
            Self::PhysicalPlan(error) => Some(error),
            Self::ServerSelect(error) => Some(error),
            Self::RawServerTargetUnavailable { source } => Some(source),
            Self::ServerInsert(error) => Some(error),
            Self::ServerUpdate(error) => Some(error),
            Self::ServerDelete(error) => Some(error),
            Self::RowDecode { source, .. } => Some(source),
            Self::MigrationMismatch { .. }
            | Self::CatalogueInvariant(_)
            | Self::ExpectedBaseMismatch { .. }
            | Self::StandardContextTransitionRequired { .. }
            | Self::StandardContextMismatch { .. }
            | Self::ReservedStandardIdentity { .. }
            | Self::SecurityRevisionMismatch { .. }
            | Self::SecurityFunctionSetMismatch
            | Self::ClientExecuteDenied { .. }
            | Self::ServerExecuteDenied { .. }
            | Self::RawExecuteDenied { .. }
            | Self::RawCallTargetUnavailable { .. }
            | Self::InspectDenied { .. }
            | Self::SecurityAdminDenied { .. }
            | Self::DurableInvariant { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, str::FromStr};

    use orna_core::{
        CatalogueRevisionId, FunctionId, SourceRevisionId,
        canonical_hash::CanonicalHashError,
        catalogue::CatalogueSnapshotError,
        physical::PhysicalPlanError,
        revision::{CatalogueHashVersion, RevisionInvariantError, RevisionPair},
        security::{ExecuteDenial, LocalPeerAuthenticationError},
    };

    use super::{PostgresKernel, PostgresKernelError, RawServerTargetError, ServerSelectError};

    #[test]
    fn parses_connection_parameters_without_connecting() {
        let kernel =
            PostgresKernel::from_str("host=127.0.0.1 port=55432 dbname=ornadb_dev user=ornadb_dev");

        assert!(kernel.is_ok());
    }

    #[test]
    fn rejects_invalid_connection_parameters() {
        let error = PostgresKernel::from_str("host=127.0.0.1 port=not-a-number")
            .err()
            .expect("invalid port must fail");

        assert!(matches!(error, PostgresKernelError::Configuration(_)));
    }

    #[test]
    fn preserves_typed_core_errors_as_sources() {
        let canonical = PostgresKernelError::CanonicalHash(CanonicalHashError::LengthExceedsU32 {
            value: "test",
            length: usize::MAX,
        });
        let revision = PostgresKernelError::RevisionInvariant(
            RevisionInvariantError::SourceRevisionPairMismatch {
                pair: SourceRevisionId::from_bytes([1; 16]),
                source: SourceRevisionId::from_bytes([2; 16]),
            },
        );
        let candidate = PostgresKernelError::CandidateRevisionInvariant(
            RevisionInvariantError::SourceRevisionPairMismatch {
                pair: SourceRevisionId::from_bytes([6; 16]),
                source: SourceRevisionId::from_bytes([7; 16]),
            },
        );
        let catalogue =
            PostgresKernelError::CatalogueSnapshot(CatalogueSnapshotError::DuplicateSchemaId {
                id: orna_core::SchemaId::from_bytes([3; 16]),
            });
        let physical =
            PostgresKernelError::PhysicalPlan(PhysicalPlanError::UnsupportedObjectDrop {
                object_type: orna_core::TypeId::from_bytes([5; 16]),
            });

        assert!(canonical.source().is_some());
        assert!(revision.source().is_some());
        assert!(candidate.source().is_some());
        assert_eq!(
            candidate.to_string(),
            "candidate revision invariant failed: revision pair source does not match stored source"
        );
        let candidate_source = candidate
            .source()
            .expect("candidate invariant has a source");
        assert_eq!(
            candidate_source.downcast_ref::<RevisionInvariantError>(),
            Some(&RevisionInvariantError::SourceRevisionPairMismatch {
                pair: SourceRevisionId::from_bytes([6; 16]),
                source: SourceRevisionId::from_bytes([7; 16]),
            })
        );
        assert!(candidate_source.source().is_none());
        assert!(catalogue.source().is_some());
        assert!(physical.source().is_some());
        assert!(
            PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.test",
                record: CatalogueRevisionId::from_bytes([4; 16]).canonical(),
                rule: "test rule",
            }
            .source()
            .is_none()
        );

        assert!(matches!(
            candidate,
            PostgresKernelError::CandidateRevisionInvariant(
                RevisionInvariantError::SourceRevisionPairMismatch { .. }
            )
        ));
    }

    #[test]
    fn standard_context_transition_error_is_exact_and_source_free() {
        let error = PostgresKernelError::StandardContextTransitionRequired {
            active: CatalogueHashVersion::Version1,
            candidate: CatalogueHashVersion::Version2,
        };

        assert_eq!(
            error.to_string(),
            "the active and candidate catalogue hash versions require a standard context transition"
        );
        assert!(error.source().is_none());
    }

    #[test]
    fn client_execute_denial_preserves_the_pinned_target() {
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([9; 16]),
            CatalogueRevisionId::from_bytes([10; 16]),
        );
        let function = FunctionId::from_bytes([11; 16]);
        let error = PostgresKernelError::ClientExecuteDenied {
            pair,
            function,
            reason: ExecuteDenial::MissingExecuteGrant,
        };

        assert_eq!(error.to_string(), "CLIENT function execution was denied");
        assert!(error.source().is_none());
        assert!(matches!(
            error,
            PostgresKernelError::ClientExecuteDenied {
                pair: actual_pair,
                function: actual_function,
                reason: ExecuteDenial::MissingExecuteGrant,
            } if actual_pair == pair && actual_function == function
        ));
    }

    #[test]
    fn raw_execute_denial_does_not_claim_a_function_domain() {
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x21; 16]),
            CatalogueRevisionId::from_bytes([0x22; 16]),
        );
        let function = FunctionId::from_bytes([0x23; 16]);
        let error = PostgresKernelError::RawExecuteDenied {
            pair,
            function,
            reason: ExecuteDenial::UnknownFunction,
        };

        assert_eq!(error.to_string(), "raw function execution was denied");
        assert!(error.source().is_none());
        assert!(matches!(
            error,
            PostgresKernelError::RawExecuteDenied {
                pair: actual_pair,
                function: actual_function,
                reason: ExecuteDenial::UnknownFunction,
            } if actual_pair == pair && actual_function == function
        ));
    }

    #[test]
    fn raw_call_target_unavailable_reports_the_exact_function_and_rule() {
        let function = FunctionId::from_bytes([0x25; 16]);
        let error = PostgresKernelError::RawCallTargetUnavailable {
            function,
            rule: "test boundary",
        };

        assert_eq!(
            error.to_string(),
            format!(
                "raw call target {} is unavailable: test boundary",
                function.canonical()
            )
        );
        assert!(error.source().is_none());
        assert!(matches!(
            error,
            PostgresKernelError::RawCallTargetUnavailable {
                function: actual_function,
                rule: "test boundary",
            } if actual_function == function
        ));
    }

    #[test]
    fn unavailable_raw_server_target_retains_its_typed_source() {
        let function = FunctionId::from_bytes([0x24; 16]);
        let error = PostgresKernelError::RawServerTargetUnavailable {
            source: RawServerTargetError::Select(ServerSelectError::RawTarget {
                function,
                rule: "test boundary",
            }),
        };

        assert_eq!(
            error.to_string(),
            format!(
                "raw SERVER target is unavailable: function {} is not an available raw SERVER target: test boundary",
                function.canonical()
            )
        );
        assert!(error.source().is_some());
    }

    #[test]
    fn local_peer_authentication_error_remains_a_typed_source() {
        let error =
            PostgresKernelError::LocalPeerAuthentication(LocalPeerAuthenticationError::UnknownUid);

        assert_eq!(
            error.to_string(),
            "local peer authentication failed: local peer credential is unknown"
        );
        let source = error.source().expect("authentication failure has a source");
        assert_eq!(
            source.downcast_ref::<LocalPeerAuthenticationError>(),
            Some(&LocalPeerAuthenticationError::UnknownUid)
        );
        assert!(source.source().is_none());
    }

    #[test]
    fn reserved_standard_identity_error_is_exact_and_source_free() {
        let identity = orna_standard::StandardUpgradeIdentity::StandardLibraryRevision(
            orna_core::StandardLibraryRevisionId::from_bytes([8; 16]),
        );
        let error = PostgresKernelError::ReservedStandardIdentity { identity };

        assert_eq!(
            error.to_string(),
            "the database contains an identity reserved for the standard library"
        );
        assert!(error.source().is_none());
        assert!(matches!(
            error,
            PostgresKernelError::ReservedStandardIdentity {
                identity: orna_standard::StandardUpgradeIdentity::StandardLibraryRevision(_)
            }
        ));
    }

    #[tokio::test]
    #[ignore = "requires the private PostgreSQL development harness"]
    async fn probes_a_running_private_kernel() {
        let connection_string = std::env::var("ORNA_TEST_POSTGRES_URL")
            .expect("ORNA_TEST_POSTGRES_URL must identify the test kernel");
        let kernel = PostgresKernel::from_str(&connection_string).expect("config must parse");

        kernel.health_check().await.expect("kernel must answer");
    }
}
