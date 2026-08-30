//! Local Turso SQLite adapter for the backend-neutral Orna revision lifecycle.
//!
//! This adapter persists compiler-produced source/catalogue lineage, semantic
//! revision snapshots, the application migration ledger, and generated object
//! tables for the supported physical artifact subset. Its execution surface
//! handles checked server-plan and parameter-echo artifacts; unsupported value,
//! enum, record, binding, and artifact shapes remain explicitly fail-closed.

use std::{
    error::Error,
    fmt,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use orna_artifact::server_parameter_echo::ServerParameterEcho;
use orna_artifact::server_plan::{
    DistinctServerPlan, Expression, ExpressionKind, IdentitySelectedServerPlan, Scan,
    SelectBindValue, ServerPlan, UniqueTextSelectedServerPlan,
};
use orna_core::{
    CatalogueRevisionId, FieldId, FunctionId, InspectEpochId, InvocationId, ObjectId, ParameterId,
    PrincipalId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, StateSlotId, TypeId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
        ObjectTypeDefinition, QualifiedSemanticName, RecordValueTypeDefinition, SchemaDefinition,
        TypeBinding, ValueTypeDefinition,
    },
    physical::{
        AddField, CreateField, CreateObject, PhysicalFieldType, PhysicalMigrationArtifact,
        PhysicalOperation,
    },
    revision::{
        ActiveDatabaseRevision, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
        DefinitionReference, DeployableRevision, ExecutableArtifactKind, ExpressionArtifact,
        FunctionRevisionRecord, RevisionPair, Sha256Digest, SourceOrigin, StoredSourceRevision,
        StoredSourceUnit,
    },
    security::{
        AuthenticatedSession, AuthorisedInvocation, CATALOGUE_HEALTH_FUNCTION_ID,
        CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, ExecuteDecision, ExecuteDenial, ExecuteGrant,
        LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus, PrivilegeClass,
        PrivilegeDecision, PrivilegeGrant, RoleMembership, SecuritySnapshot,
    },
    state::{
        UserStateCell, UserStateChange, UserStateError, UserStateKey, UserStateWriteOutcome,
        UserStateWriteResult, apply_change, is_sealed_inspect_runtime_value,
    },
    system::system_function_by_id,
    types::{ResolvedType, StandardScalar},
    value::{RuntimeFloat, RuntimeType, RuntimeValue},
};
use orna_protocol::{decode_value, encode_value};
use orna_standard::{
    BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID, BOOLEAN_TYPE_ID, CHARACTER_LARGE_OBJECT_TYPE_ID,
    DATE_TYPE_ID, DECIMAL_TYPE_ID, DURATION_TYPE_ID, FLOAT_TYPE_ID, INTEGER_TYPE_ID, TIME_TYPE_ID,
    TIMESTAMP_TYPE_ID, UUID_TYPE_ID, VOID_TYPE_ID,
};
use orna_storage::{
    ApplicationRevisionStore, BootstrapRevision, MigrationLedgerEntry, MigrationLedgerEntryError,
    StorageError,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use turso::{Builder, Connection, Value};

const SCHEMA: &str = concat!(
    include_str!("../migrations/0001_revision_store.sql"),
    include_str!("../migrations/0002_security_runtime.sql"),
);

/// A capability that the SQLite adapter does not accept.
///
/// The adapter supports the bounded local catalogue, migration, and runtime
/// subset implemented below. These checks reject unsupported catalogue
/// categories in fixed precedence, then the catalogue hash context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqliteCapability {
    /// A by-value catalogue type definition.
    ValueType,
    /// An enum catalogue type definition.
    EnumType,
    /// A record-value catalogue type definition.
    RecordValueType,
    /// A catalogue type-name binding.
    TypeBinding,
    /// A scalar field type outside SQLite's supported runtime set.
    ScalarType,
    /// A catalogue hash context other than the SQLite-supported version 1.
    CatalogueHashVersion,
}

impl fmt::Display for SqliteCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::ValueType => "value type",
            Self::EnumType => "enum type",
            Self::RecordValueType => "record value type",
            Self::TypeBinding => "type binding",
            Self::ScalarType => "scalar type",
            Self::CatalogueHashVersion => "catalogue hash version",
        };
        f.write_str(name)
    }
}

#[derive(Debug)]
pub enum SqliteError {
    Backend(turso::Error),
    InvalidPersistedData(&'static str),
    UnsupportedCapability(SqliteCapability),
    Domain(String),
}

impl fmt::Display for SqliteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(error) => write!(f, "Turso backend error: {error}"),
            Self::InvalidPersistedData(message) => {
                write!(f, "invalid persisted SQLite data: {message}")
            }
            Self::UnsupportedCapability(capability) => {
                write!(f, "SQLite adapter does not support applying {capability}")
            }
            Self::Domain(message) => write!(f, "Orna domain error: {message}"),
        }
    }
}
impl Error for SqliteError {}
impl From<turso::Error> for SqliteError {
    fn from(error: turso::Error) -> Self {
        Self::Backend(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteConfig {
    path: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
struct PersistedCatalogueSnapshot {
    revision: CatalogueRevisionId,
    schemas: Vec<SchemaDefinition>,
    object_types: Vec<ObjectTypeDefinition>,
    value_types: Vec<ValueTypeDefinition>,
    enum_types: Vec<EnumTypeDefinition>,
    record_value_types: Vec<RecordValueTypeDefinition>,
    type_bindings: Vec<TypeBinding>,
    functions: Vec<FunctionDefinition>,
}

impl PersistedCatalogueSnapshot {
    fn from_catalogue(catalogue: &CatalogueSnapshot) -> Self {
        Self {
            revision: catalogue.revision(),
            schemas: catalogue.schemas().to_vec(),
            object_types: catalogue.object_types().to_vec(),
            value_types: catalogue.value_types().to_vec(),
            enum_types: catalogue.enum_types().to_vec(),
            record_value_types: catalogue.record_value_types().to_vec(),
            type_bindings: catalogue.type_bindings().to_vec(),
            functions: catalogue.functions().to_vec(),
        }
    }

    fn into_catalogue(self) -> Result<CatalogueSnapshot, SqliteError> {
        CatalogueSnapshot::new_with_functions_and_record_value_types(
            self.revision,
            self.schemas,
            self.object_types,
            self.value_types,
            self.enum_types,
            self.record_value_types,
            self.type_bindings,
            self.functions,
        )
        .map_err(|error| SqliteError::Domain(error.to_string()))
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct PersistedActiveRevision {
    source: StoredSourceRevision,
    catalogue: PersistedCatalogueSnapshot,
    catalogue_hash: Sha256Digest,
    expressions: Vec<ExpressionArtifact>,
    function_revisions: Vec<FunctionRevisionRecord>,
    historical_function_revisions: Vec<FunctionRevisionRecord>,
    origins: Vec<DefinitionOrigin>,
    references: Vec<DefinitionReference>,
}

impl PersistedActiveRevision {
    fn from_active(active: &ActiveDatabaseRevision) -> Self {
        Self {
            source: active.source().clone(),
            catalogue: PersistedCatalogueSnapshot::from_catalogue(active.catalogue()),
            catalogue_hash: active.catalogue_hash(),
            expressions: active.expressions().to_vec(),
            function_revisions: active.function_revisions().to_vec(),
            historical_function_revisions: active.historical_function_revisions().to_vec(),
            origins: active.origins().to_vec(),
            references: active.references().to_vec(),
        }
    }

    fn into_active(self) -> Result<ActiveDatabaseRevision, SqliteError> {
        let expected_bundle_hash = source_bundle_digest(self.source.units())
            .map_err(|error| SqliteError::Domain(error.to_string()))?;
        if expected_bundle_hash != self.source.bundle_hash() {
            return Err(SqliteError::InvalidPersistedData(
                "revision snapshot source bundle hash mismatch",
            ));
        }
        let expected_source_hash = source_revision_record_digest(
            self.source.bundle(),
            self.source.parent(),
            self.source.bundle_hash(),
        )
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
        if expected_source_hash != self.source.revision_hash() {
            return Err(SqliteError::InvalidPersistedData(
                "revision snapshot source hash mismatch",
            ));
        }
        let source = StoredSourceRevision::new(
            self.source.bundle(),
            self.source.id(),
            self.source.parent(),
            self.source.units().to_vec(),
            self.source.bundle_hash(),
            self.source.revision_hash(),
        )
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
        let catalogue = self.catalogue.into_catalogue()?;
        let expected_hash = catalogue_digest(
            &catalogue,
            &self.function_revisions,
            &self.expressions,
            &self.origins,
            &self.references,
        )
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
        if expected_hash != self.catalogue_hash {
            return Err(SqliteError::InvalidPersistedData(
                "catalogue snapshot hash mismatch",
            ));
        }
        ActiveDatabaseRevision::new_with_history(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            self.catalogue_hash,
            self.expressions,
            self.function_revisions,
            self.historical_function_revisions,
            self.origins,
            self.references,
        )
        .map_err(|error| SqliteError::Domain(error.to_string()))
    }
}

/// One persisted SQLite security-admin mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqliteSecurityMutation {
    /// Creates an active principal.
    CreatePrincipal {
        /// The principal identity.
        principal: PrincipalId,
        /// The principal kind.
        kind: PrincipalKind,
    },
    /// Disables a principal.
    DisablePrincipal {
        /// The principal identity.
        principal: PrincipalId,
    },
    /// Creates an active role.
    CreateRole {
        /// The role identity.
        role: PrincipalId,
    },
    /// Adds one role membership.
    GrantRole {
        /// The role identity.
        role: PrincipalId,
        /// The member identity.
        member: PrincipalId,
    },
    /// Removes one role membership.
    RevokeRole {
        /// The role identity.
        role: PrincipalId,
        /// The member identity.
        member: PrincipalId,
    },
    /// Adds one privilege grant.
    GrantPrivilege {
        /// The grantee identity.
        grantee: PrincipalId,
        /// The privilege class.
        class: PrivilegeClass,
        /// The optional function object.
        object: Option<orna_core::FunctionId>,
    },
    /// Removes one privilege grant.
    RevokePrivilege {
        /// The grantee identity.
        grantee: PrincipalId,
        /// The privilege class.
        class: PrivilegeClass,
        /// The optional function object.
        object: Option<orna_core::FunctionId>,
    },
    /// Adds one direct function execute grant.
    GrantExecute {
        /// The grantee identity.
        grantee: PrincipalId,
        /// The function identity.
        function: orna_core::FunctionId,
    },
}

/// The result of one local-peer authorization and pinned SERVER execution.
#[derive(Debug)]
pub enum SqliteExecutionResult {
    /// The call was authorized and executed against the supplied revision.
    Allowed {
        /// The session authenticated from the local operating-system peer.
        session: AuthenticatedSession,
        /// The immutable authorization evidence for the call.
        authorisation: AuthorisedInvocation,
        /// The canonical runtime values returned by the call.
        values: Vec<RuntimeValue>,
    },
    /// The local peer authenticated, but no execute grant admitted the call.
    Denied {
        /// The session authenticated from the local operating-system peer.
        session: AuthenticatedSession,
        /// The closed authorization denial.
        reason: ExecuteDenial,
    },
    /// The call was authorized but its supported executor failed.
    Failed {
        /// The session authenticated from the local operating-system peer.
        session: AuthenticatedSession,
        /// The immutable authorization evidence for the call.
        authorisation: AuthorisedInvocation,
        /// The redacted execution failure.
        error: SqliteError,
    },
}

/// Redacted durable evidence for one local invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteInvocationAuditEvent {
    /// Stable invocation identity.
    pub invocation: InvocationId,
    /// Closed terminal outcome (`allowed`, `denied`, `completed`, or `failed`).
    pub outcome: String,
    /// Authenticated session principal.
    pub session_principal: PrincipalId,
    /// Effective principal, when one was selected.
    pub effective_principal: Option<PrincipalId>,
    /// Principal whose grant authorised execution, when one was selected.
    pub authorising_principal: Option<PrincipalId>,
    /// Invoked function, when target resolution succeeded.
    pub function: Option<orna_core::FunctionId>,
    /// Active source revision at decision time.
    pub source_revision: Option<SourceRevisionId>,
    /// Active catalogue revision at decision time.
    pub catalogue_revision: Option<CatalogueRevisionId>,
    /// Stable failure or denial code, without arguments or result payloads.
    pub error_code: Option<String>,
}

/// Redacted durable summary for one inspection epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteInspectSnapshotRecord {
    /// Stable inspection epoch identity.
    pub epoch: InspectEpochId,
    /// Invocation represented by this epoch.
    pub invocation: InvocationId,
    /// Principal that owns the epoch.
    pub owner: PrincipalId,
    /// Source revision pinned by the epoch.
    pub source_revision: SourceRevisionId,
    /// Catalogue revision pinned by the epoch.
    pub catalogue_revision: CatalogueRevisionId,
    /// Bounded canonical summary bytes; never resource or value payloads.
    pub summary: Vec<u8>,
}

/// One bounded redacted inspection trace event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteInspectTraceEvent {
    /// Invocation whose trace is streamed.
    pub invocation: InvocationId,
    /// Monotonic sequence within the invocation.
    pub sequence: u64,
    /// Closed trace kind.
    pub kind: String,
    /// Bounded canonical event payload.
    pub payload: Vec<u8>,
    /// Observer invocation, when the event was produced by an observer.
    pub observer_invocation: Option<InvocationId>,
}

/// Redacted durable evidence for one USER-state operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteUserStateAuditEvent {
    /// Stable audit identity.
    pub audit_id: InvocationId,
    /// Closed operation (`load` or `write`).
    pub operation: String,
    /// Closed outcome (`completed` or `conflict`).
    pub outcome: String,
    /// Authenticated session principal.
    pub session_principal: PrincipalId,
    /// USER-state root function.
    pub root_function: orna_core::FunctionId,
    /// USER-state profile.
    pub state_profile: String,
    /// Number of cells selected or changed.
    pub cell_count: u64,
}
impl SqliteConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Clone)]
pub struct SqliteRevisionStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteRevisionStore {
    pub async fn open(config: &SqliteConfig) -> Result<Self, SqliteError> {
        let path = config
            .path
            .to_str()
            .ok_or(SqliteError::InvalidPersistedData(
                "database path is not UTF-8",
            ))?;
        let database = Builder::new_local(path).build().await?;
        let mut connection = database.connect()?;
        ensure_schema(&mut connection).await?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }
    /// Opens an existing local database without write access or schema setup.
    pub async fn open_read_only(config: &SqliteConfig) -> Result<Self, SqliteError> {
        let path = config
            .path
            .to_str()
            .ok_or(SqliteError::InvalidPersistedData(
                "database path is not UTF-8",
            ))?;
        let database = Builder::new_local(path).read_only(true).build().await?;
        let connection = database.connect()?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }
    /// Provisions the current local UID as a durable SQLite peer identity.
    ///
    /// Provisioning is idempotent for an existing UID. A newly provisioned
    /// local owner receives the class-wide privileges needed to operate the
    /// local database; subsequent security administration can disable or
    /// narrow that identity through the persisted security tables.
    pub async fn provision_local_peer(&self, uid: u32) -> Result<PrincipalId, SqliteError> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await?;

        let result = async {
            let reserved = CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID.to_bytes().to_vec();
            let mut rows = transaction
                .query(
                    "SELECT kind, status
                     FROM orna_security_principals
                     WHERE principal_id = ?1",
                    [Value::Blob(reserved.clone())],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                if row.get::<String>(0)? != "service" || row.get::<String>(1)? != "active" {
                    return Err(SqliteError::InvalidPersistedData(
                        "reserved catalogue-health service principal is invalid",
                    ));
                }
            } else {
                let inserted = transaction
                    .execute(
                        "INSERT INTO orna_security_principals
                         (principal_id, kind, status)
                         VALUES (?1, 'service', 'active')",
                        [Value::Blob(reserved)],
                    )
                    .await?;
                if inserted != 1 {
                    return Err(SqliteError::InvalidPersistedData(
                        "reserved service principal insert affected an unexpected number of rows",
                    ));
                }
            }
            drop(rows);
            let mut rows = transaction
                .query(
                    "SELECT principal_id
                     FROM orna_security_local_peer_credentials
                     WHERE uid = ?1",
                    [Value::Integer(i64::from(uid))],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                return Ok(PrincipalId::from_bytes(id16(
                    row.get::<Vec<u8>>(0)?,
                    "local peer principal id",
                )?));
            }
            drop(rows);

            let principal = local_peer_principal_id(uid);
            let inserted = transaction
                .execute(
                    "INSERT INTO orna_security_principals
                     (principal_id, kind, status)
                     VALUES (?1, 'user', 'active')",
                    [Value::Blob(principal.to_bytes().to_vec())],
                )
                .await?;
            if inserted != 1 {
                return Err(SqliteError::InvalidPersistedData(
                    "local peer principal insert affected an unexpected number of rows",
                ));
            }
            let inserted = transaction
                .execute(
                    "INSERT INTO orna_security_local_peer_credentials
                     (uid, principal_id)
                     VALUES (?1, ?2)",
                    [
                        Value::Integer(i64::from(uid)),
                        Value::Blob(principal.to_bytes().to_vec()),
                    ],
                )
                .await?;
            if inserted != 1 {
                return Err(SqliteError::InvalidPersistedData(
                    "local peer credential insert affected an unexpected number of rows",
                ));
            }
            for privilege in [
                "execute",
                "security_admin",
                "inspect:own-invocation",
                "inspect:session-invocations",
                "inspect:any-invocation",
                "inspect:values",
                "inspect:source",
                "inspect:security-details",
                "inspect:runtime-internals",
            ] {
                let inserted = transaction
                    .execute(
                        "INSERT INTO orna_security_privilege_grants
                         (grantee_id, privilege, object_id)
                         VALUES (?1, ?2, NULL)",
                        [
                            Value::Blob(principal.to_bytes().to_vec()),
                            Value::Text(privilege.to_owned()),
                        ],
                    )
                    .await?;
                if inserted != 1 {
                    return Err(SqliteError::InvalidPersistedData(
                        "local peer privilege insert affected an unexpected number of rows",
                    ));
                }
            }
            Ok(principal)
        }
        .await;

        match result {
            Ok(principal) => {
                transaction.commit().await?;
                Ok(principal)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(SqliteError::from(rollback)),
            },
        }
    }

    /// Reconstructs the validated security snapshot for the active revision.
    pub async fn security_snapshot(
        &self,
        active: &ActiveDatabaseRevision,
    ) -> Result<SecuritySnapshot, SqliteError> {
        let connection = self.connection.lock().await;
        load_security_snapshot(&connection, active).await
    }

    /// Authenticates one kernel-supplied local UID against the durable
    /// protected peer-credential mapping.
    pub async fn authenticate_local_peer(
        &self,
        active: &ActiveDatabaseRevision,
        uid: u32,
    ) -> Result<AuthenticatedSession, SqliteError> {
        let snapshot = self.security_snapshot(active).await?;
        snapshot.authenticate_local_peer(uid).map_err(|error| {
            SqliteError::Domain(format!("local peer authentication failed: {error}"))
        })
    }

    /// Evaluates the durable local-peer `EXECUTE` decision for one function.
    pub async fn authorise_local_execute(
        &self,
        active: &ActiveDatabaseRevision,
        session: &AuthenticatedSession,
        function_id: orna_core::FunctionId,
    ) -> Result<ExecuteDecision, SqliteError> {
        let snapshot = self.security_snapshot(active).await?;
        Ok(snapshot.authorise_execute(
            session,
            orna_core::security::InvocationTarget::new(function_id, active.pair()),
        ))
    }

    /// Loads every durable USER-state cell for one authenticated principal
    /// and root scope.
    ///
    /// The caller supplies the active and security snapshots used for
    /// authentication. Both are revalidated, together with the session
    /// binding, inside the read transaction before any cell is returned.
    pub async fn load_user_state(
        &self,
        active: &ActiveDatabaseRevision,
        security: &SecuritySnapshot,
        session: &AuthenticatedSession,
        root_function: orna_core::FunctionId,
        state_profile: &str,
    ) -> Result<Vec<UserStateCell>, SqliteError> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await?;
        let result = async {
            let current_active = load_active_from(&transaction).await?;
            let current_security = load_security_snapshot(&transaction, &current_active).await?;
            validate_pinned_context(
                active,
                security,
                session,
                &current_active,
                &current_security,
            )?;
            let principal = session.principal();
            let mut rows = transaction
                .query(
                    "SELECT function_id, function_instance_key, state_slot_id,
                            value_bytes, value_type_id, revision
                     FROM orna_user_state_cells
                     WHERE principal_id = ?1
                       AND root_function_id = ?2
                       AND root_state_profile = ?3
                     ORDER BY function_id, function_instance_key, state_slot_id",
                    [
                        Value::Blob(principal.to_bytes().to_vec()),
                        Value::Blob(root_function.to_bytes().to_vec()),
                        Value::Text(state_profile.to_owned()),
                    ],
                )
                .await?;
            let mut cells = Vec::new();
            while let Some(row) = rows.next().await? {
                let function = orna_core::FunctionId::from_bytes(id16(
                    row.get::<Vec<u8>>(0)?,
                    "USER state function id",
                )?);
                let instance_key = row.get::<String>(1)?;
                let state_slot =
                    StateSlotId::from_bytes(id16(row.get::<Vec<u8>>(2)?, "USER state slot id")?);
                let value_bytes = row.get::<Vec<u8>>(3)?;
                let value = decode_value(&value_bytes)
                    .map_err(|_| SqliteError::InvalidPersistedData("USER state value encoding"))?;
                let value_type =
                    TypeId::from_bytes(id16(row.get::<Vec<u8>>(4)?, "USER state value type id")?);
                if is_sealed_inspect_type_id(value_type) || is_sealed_inspect_runtime_value(&value)
                {
                    return Err(SqliteError::InvalidPersistedData(
                        "USER state cannot expose sealed Inspector values",
                    ));
                }
                let revision = u64::try_from(row.get::<i64>(5)?)
                    .map_err(|_| SqliteError::InvalidPersistedData("USER state revision"))?;
                let key = UserStateKey::new(
                    principal,
                    root_function,
                    state_profile.to_owned(),
                    function,
                    instance_key,
                    state_slot,
                )
                .map_err(|error| SqliteError::Domain(error.to_string()))?;
                cells.push(UserStateCell::new(
                    key,
                    value,
                    value_type,
                    revision,
                    SystemTime::now(),
                ));
            }
            drop(rows);
            Self::record_user_state_audit_on(
                &transaction,
                &SqliteUserStateAuditEvent {
                    audit_id: InvocationId::new(),
                    operation: "load".to_owned(),
                    outcome: "completed".to_owned(),
                    session_principal: principal,
                    root_function,
                    state_profile: state_profile.to_owned(),
                    cell_count: cells.len() as u64,
                },
            )
            .await?;
            Ok(cells)
        }
        .await;
        match result {
            Ok(cells) => {
                transaction.commit().await?;
                Ok(cells)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(SqliteError::from(rollback)),
            },
        }
    }

    /// Applies one authenticated USER-state change atomically.
    ///
    /// The active revision, security snapshot, and authenticated session are
    /// revalidated in the same write transaction before the current cell is
    /// read or changed.
    pub async fn write_user_state(
        &self,
        active: &ActiveDatabaseRevision,
        security: &SecuritySnapshot,
        session: &AuthenticatedSession,
        change: &UserStateChange,
    ) -> Result<UserStateWriteResult, SqliteError> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await?;
        let result = async {
            let current_active = load_active_from(&transaction).await?;
            let current_security = load_security_snapshot(&transaction, &current_active).await?;
            validate_pinned_context(
                active,
                security,
                session,
                &current_active,
                &current_security,
            )?;
            let principal = session.principal();
            let mut rows = transaction
                .query(
                    "SELECT function_id, function_instance_key, state_slot_id,
                            value_bytes, value_type_id, revision
                     FROM orna_user_state_cells
                     WHERE principal_id = ?1
                       AND root_function_id = ?2
                       AND root_state_profile = ?3
                       AND function_id = ?4
                       AND function_instance_key = ?5
                       AND state_slot_id = ?6",
                    [
                        Value::Blob(principal.to_bytes().to_vec()),
                        Value::Blob(change.root_function().to_bytes().to_vec()),
                        Value::Text(change.state_profile().to_owned()),
                        Value::Blob(change.function().to_bytes().to_vec()),
                        Value::Text(change.instance_key().to_owned()),
                        Value::Blob(change.state_slot().to_bytes().to_vec()),
                    ],
                )
                .await?;
            let current = if let Some(row) = rows.next().await? {
                let function = orna_core::FunctionId::from_bytes(id16(
                    row.get::<Vec<u8>>(0)?,
                    "USER state function id",
                )?);
                let instance_key = row.get::<String>(1)?;
                let state_slot =
                    StateSlotId::from_bytes(id16(row.get::<Vec<u8>>(2)?, "USER state slot id")?);
                let value = decode_value(&row.get::<Vec<u8>>(3)?)
                    .map_err(|_| SqliteError::InvalidPersistedData("USER state value encoding"))?;
                let value_type =
                    TypeId::from_bytes(id16(row.get::<Vec<u8>>(4)?, "USER state value type id")?);
                if is_sealed_inspect_type_id(value_type) || is_sealed_inspect_runtime_value(&value)
                {
                    return Err(SqliteError::InvalidPersistedData(
                        "USER state cannot expose sealed Inspector values",
                    ));
                }
                let revision = u64::try_from(row.get::<i64>(5)?)
                    .map_err(|_| SqliteError::InvalidPersistedData("USER state revision"))?;
                let key = UserStateKey::new(
                    principal,
                    change.root_function(),
                    change.state_profile().to_owned(),
                    function,
                    instance_key,
                    state_slot,
                )
                .map_err(|error| SqliteError::Domain(error.to_string()))?;
                Some(UserStateCell::new(
                    key,
                    value,
                    value_type,
                    revision,
                    SystemTime::now(),
                ))
            } else {
                None
            };
            drop(rows);
            if let Some(current) = current.as_ref()
                && current.value_type() != change.value_type()
            {
                return Err(SqliteError::Domain(
                    "USER state value type does not match the existing cell".to_owned(),
                ));
            }
            let result = match apply_change(current.as_ref(), change, principal) {
                Ok(result) => result,
                Err(UserStateError::RevisionConflict { current, .. }) => UserStateWriteResult::new(
                    change.key_without_principal(),
                    UserStateWriteOutcome::Conflict {
                        current_revision: current,
                    },
                ),
                Err(error) => return Err(SqliteError::Domain(error.to_string())),
            };
            if let UserStateWriteOutcome::Written { revision } = result.outcome() {
                let value_bytes = encode_value(change.value()).map_err(|_| {
                    SqliteError::Domain("USER state value cannot be encoded as ORV5".to_owned())
                })?;
                let updated = transaction
                    .execute(
                        "INSERT INTO orna_user_state_cells
                         (principal_id, root_function_id, root_state_profile,
                          function_id, function_instance_key, state_slot_id,
                          value_bytes, value_type_id, revision)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                         ON CONFLICT (
                             principal_id, root_function_id, root_state_profile,
                             function_id, function_instance_key, state_slot_id
                         ) DO UPDATE SET
                             value_bytes = excluded.value_bytes,
                             value_type_id = excluded.value_type_id,
                             revision = excluded.revision,
                             updated_at = CURRENT_TIMESTAMP",
                        [
                            Value::Blob(principal.to_bytes().to_vec()),
                            Value::Blob(change.root_function().to_bytes().to_vec()),
                            Value::Text(change.state_profile().to_owned()),
                            Value::Blob(change.function().to_bytes().to_vec()),
                            Value::Text(change.instance_key().to_owned()),
                            Value::Blob(change.state_slot().to_bytes().to_vec()),
                            Value::Blob(value_bytes),
                            Value::Blob(change.value_type().to_bytes().to_vec()),
                            Value::Integer(i64::try_from(revision).map_err(|_| {
                                SqliteError::Domain("USER state revision overflow".to_owned())
                            })?),
                        ],
                    )
                    .await?;
                if updated != 1 {
                    return Err(SqliteError::InvalidPersistedData(
                        "USER state write affected an unexpected number of rows",
                    ));
                }
            }
            let outcome = match result.outcome() {
                UserStateWriteOutcome::Written { .. } => "completed",
                UserStateWriteOutcome::Conflict { .. } => "conflict",
            };
            Self::record_user_state_audit_on(
                &transaction,
                &SqliteUserStateAuditEvent {
                    audit_id: InvocationId::new(),
                    operation: "write".to_owned(),
                    outcome: outcome.to_owned(),
                    session_principal: principal,
                    root_function: change.root_function(),
                    state_profile: change.state_profile().to_owned(),
                    cell_count: 1,
                },
            )
            .await?;
            Ok(result)
        }
        .await;
        match result {
            Ok(result) => {
                transaction.commit().await?;
                Ok(result)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(SqliteError::from(rollback)),
            },
        }
    }

    /// Applies one security-admin mutation under an authenticated local
    /// session and returns the rebuilt validated snapshot.
    pub async fn apply_security_mutation(
        &self,
        active: &ActiveDatabaseRevision,
        session: &AuthenticatedSession,
        mutation: SqliteSecurityMutation,
    ) -> Result<SecuritySnapshot, SqliteError> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await?;
        let result = async {
            let current_active = load_active_from(&transaction).await?;
            if current_active.pair() != active.pair() {
                return Err(SqliteError::Domain(
                    "the active SQLite revision changed before the security mutation".to_owned(),
                ));
            }
            let active = &current_active;
            let current = load_security_snapshot(&transaction, active).await?;
            let bound_session = current
                .bind_authenticated_session(session.principal(), session.active_roles().to_vec())
                .map_err(|_| {
                    SqliteError::Domain("security administration was denied".to_owned())
                })?;
            let mut granted = current
                .privilege_grants()
                .filter(|grant| grant.grantee() == bound_session.principal())
                .map(PrivilegeGrant::class)
                .collect::<Vec<_>>();
            for role in bound_session.active_roles() {
                granted.extend(
                    current
                        .privilege_grants()
                        .filter(|grant| grant.grantee() == *role)
                        .map(PrivilegeGrant::class),
                );
            }
            if matches!(
                orna_core::security::authorise_privilege(
                    bound_session.principal(),
                    PrivilegeClass::SecurityAdmin,
                    None,
                    &granted,
                ),
                PrivilegeDecision::Denied(_)
            ) {
                return Err(SqliteError::Domain(
                    "security administration was denied".to_owned(),
                ));
            }

            match mutation {
                SqliteSecurityMutation::CreatePrincipal { principal, kind } => {
                    if principal == PrincipalId::from_bytes([0; 16]) {
                        return Err(SqliteError::Domain(
                            "the principal identity must not be empty".to_owned(),
                        ));
                    }
                    if kind == PrincipalKind::Role {
                        return Err(SqliteError::Domain(
                            "roles must be created through the role operation".to_owned(),
                        ));
                    }
                    if principal == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                        return Err(SqliteError::Domain(
                            "the reserved catalogue-health service cannot be created here"
                                .to_owned(),
                        ));
                    }
                    Principal::try_new(principal, kind, PrincipalStatus::Active)
                        .map_err(|error| SqliteError::Domain(error.to_string()))?;
                    if current.principals().any(|value| value.id() == principal) {
                        return Err(SqliteError::Domain(
                            "the security principal already exists".to_owned(),
                        ));
                    }
                    Self::mutate_security_rows_on(
                        &transaction,
                        "INSERT INTO orna_security_principals
                         (principal_id, kind, status)
                         VALUES (?1, ?2, 'active')",
                        [
                            Value::Blob(principal.to_bytes().to_vec()),
                            Value::Text(principal_kind_text(kind).to_owned()),
                        ],
                    )
                    .await?;
                }
                SqliteSecurityMutation::DisablePrincipal { principal } => {
                    if principal == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                        return Err(SqliteError::Domain(
                            "the reserved catalogue-health service cannot be disabled".to_owned(),
                        ));
                    }
                    require_security_principal(&current, principal)?;
                    Self::mutate_security_rows_on(
                        &transaction,
                        "UPDATE orna_security_principals
                         SET status = 'disabled'
                         WHERE principal_id = ?1",
                        [Value::Blob(principal.to_bytes().to_vec())],
                    )
                    .await?;
                }
                SqliteSecurityMutation::CreateRole { role } => {
                    if role == PrincipalId::from_bytes([0; 16]) {
                        return Err(SqliteError::Domain(
                            "the role identity must not be empty".to_owned(),
                        ));
                    }
                    if role == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                        return Err(SqliteError::Domain(
                            "the reserved catalogue-health service cannot be a role".to_owned(),
                        ));
                    }
                    Principal::try_new(role, PrincipalKind::Role, PrincipalStatus::Active)
                        .map_err(|error| SqliteError::Domain(error.to_string()))?;
                    if current.principals().any(|value| value.id() == role) {
                        return Err(SqliteError::Domain(
                            "the security role already exists".to_owned(),
                        ));
                    }
                    Self::mutate_security_rows_on(
                        &transaction,
                        "INSERT INTO orna_security_principals
                         (principal_id, kind, status)
                         VALUES (?1, 'role', 'active')",
                        [Value::Blob(role.to_bytes().to_vec())],
                    )
                    .await?;
                }
                SqliteSecurityMutation::GrantRole { role, member } => {
                    require_security_role(&current, role)?;
                    require_security_principal(&current, member)?;
                    if member == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                        return Err(SqliteError::Domain(
                            "the reserved catalogue-health service cannot become a role member"
                                .to_owned(),
                        ));
                    }
                    Self::mutate_security_rows_on(
                        &transaction,
                        "INSERT INTO orna_security_role_memberships (role_id, member_id)
                         VALUES (?1, ?2)",
                        [
                            Value::Blob(role.to_bytes().to_vec()),
                            Value::Blob(member.to_bytes().to_vec()),
                        ],
                    )
                    .await?;
                }
                SqliteSecurityMutation::RevokeRole { role, member } => {
                    require_security_role(&current, role)?;
                    require_security_principal(&current, member)?;
                    Self::mutate_security_rows_on(
                        &transaction,
                        "DELETE FROM orna_security_role_memberships
                         WHERE role_id = ?1 AND member_id = ?2",
                        [
                            Value::Blob(role.to_bytes().to_vec()),
                            Value::Blob(member.to_bytes().to_vec()),
                        ],
                    )
                    .await?;
                }
                SqliteSecurityMutation::GrantPrivilege {
                    grantee,
                    class,
                    object,
                } => {
                    require_security_principal(&current, grantee)?;
                    if grantee == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                        return Err(SqliteError::Domain(
                            "the reserved catalogue-health service cannot receive privilege grants"
                                .to_owned(),
                        ));
                    }
                    let grant = PrivilegeGrant::new(grantee, class, object)
                        .map_err(|error| SqliteError::Domain(error.to_string()))?;
                    if let Some(function) = object {
                        require_security_privilege_object(active, function)?;
                    }
                    Self::mutate_security_rows_on(
                        &transaction,
                        "INSERT INTO orna_security_privilege_grants
                         (grantee_id, privilege, object_id)
                         VALUES (?1, ?2, ?3)",
                        [
                            Value::Blob(grant.grantee().to_bytes().to_vec()),
                            Value::Text(grant.class().to_string()),
                            grant.object().map_or(Value::Null, |function| {
                                Value::Blob(function.to_bytes().to_vec())
                            }),
                        ],
                    )
                    .await?;
                }
                SqliteSecurityMutation::RevokePrivilege {
                    grantee,
                    class,
                    object,
                } => {
                    require_security_principal(&current, grantee)?;
                    let grant = PrivilegeGrant::new(grantee, class, object)
                        .map_err(|error| SqliteError::Domain(error.to_string()))?;
                    if let Some(function) = object {
                        require_security_privilege_object(active, function)?;
                    }
                    Self::mutate_security_rows_on(
                        &transaction,
                        "DELETE FROM orna_security_privilege_grants
                         WHERE grantee_id = ?1 AND privilege = ?2
                           AND ((object_id IS NULL AND ?3 IS NULL) OR object_id = ?3)",
                        [
                            Value::Blob(grant.grantee().to_bytes().to_vec()),
                            Value::Text(grant.class().to_string()),
                            grant.object().map_or(Value::Null, |function| {
                                Value::Blob(function.to_bytes().to_vec())
                            }),
                        ],
                    )
                    .await?;
                }
                SqliteSecurityMutation::GrantExecute { grantee, function } => {
                    if grantee != CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                        return Err(SqliteError::Domain(
                            "the fixed execute grant must target the catalogue-health service"
                                .to_owned(),
                        ));
                    }
                    if function == CATALOGUE_HEALTH_FUNCTION_ID {
                        return Err(SqliteError::Domain(
                            "the catalogue-health intrinsic cannot receive an application grant"
                                .to_owned(),
                        ));
                    }
                    require_security_principal(&current, grantee)?;
                    if active.catalogue().function_by_id(function).is_none() {
                        return Err(SqliteError::Domain(
                            "the execute target function is not installed".to_owned(),
                        ));
                    }
                    let changed = transaction
                        .execute(
                            "INSERT INTO orna_security_execute_grants (grantee_id, function_id)
                             VALUES (?1, ?2)
                             ON CONFLICT (grantee_id, function_id) DO NOTHING",
                            [
                                Value::Blob(grantee.to_bytes().to_vec()),
                                Value::Blob(function.to_bytes().to_vec()),
                            ],
                        )
                        .await?;
                    if changed > 1 {
                        return Err(SqliteError::InvalidPersistedData(
                            "execute grant write affected an unexpected number of rows",
                        ));
                    }
                }
            }
            load_security_snapshot(&transaction, active).await
        }
        .await;
        match result {
            Ok(updated) => {
                transaction.commit().await?;
                Ok(updated)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(SqliteError::from(rollback)),
            },
        }
    }

    /// Records redacted invocation evidence without arguments, results, or
    /// resource payloads.
    ///
    /// The write is serialized in an immediate transaction so a replay cannot
    /// race the immutable-content check.
    pub async fn record_invocation_audit(
        &self,
        event: &SqliteInvocationAuditEvent,
    ) -> Result<(), SqliteError> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await?;
        let result = Self::record_invocation_audit_on(&transaction, event).await;
        match result {
            Ok(()) => {
                transaction.commit().await?;
                Ok(())
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(SqliteError::from(rollback)),
            },
        }
    }

    async fn record_invocation_audit_on(
        connection: &Connection,
        event: &SqliteInvocationAuditEvent,
    ) -> Result<(), SqliteError> {
        let changed = connection
            .execute(
                "INSERT INTO orna_invocation_audit_events
                 (invocation_id, outcome, session_principal_id,
                  effective_principal_id, authorising_principal_id, function_id,
                  source_revision_id, catalogue_revision_id, error_code)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (invocation_id) DO NOTHING",
                [
                    Value::Blob(event.invocation.to_bytes().to_vec()),
                    Value::Text(event.outcome.clone()),
                    Value::Blob(event.session_principal.to_bytes().to_vec()),
                    event
                        .effective_principal
                        .map_or(Value::Null, |value| Value::Blob(value.to_bytes().to_vec())),
                    event
                        .authorising_principal
                        .map_or(Value::Null, |value| Value::Blob(value.to_bytes().to_vec())),
                    event
                        .function
                        .map_or(Value::Null, |value| Value::Blob(value.to_bytes().to_vec())),
                    event
                        .source_revision
                        .map_or(Value::Null, |value| Value::Blob(value.to_bytes().to_vec())),
                    event
                        .catalogue_revision
                        .map_or(Value::Null, |value| Value::Blob(value.to_bytes().to_vec())),
                    event
                        .error_code
                        .as_ref()
                        .map_or(Value::Null, |value| Value::Text(value.clone())),
                ],
            )
            .await?;
        if changed == 1 {
            return Ok(());
        }
        if changed != 0 {
            return Err(SqliteError::InvalidPersistedData(
                "invocation audit insert affected an unexpected number of rows",
            ));
        }

        let mut rows = connection
            .query(
                "SELECT outcome, session_principal_id, effective_principal_id,
                        authorising_principal_id, function_id, source_revision_id,
                        catalogue_revision_id, error_code
                 FROM orna_invocation_audit_events
                 WHERE invocation_id = ?1",
                [Value::Blob(event.invocation.to_bytes().to_vec())],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(SqliteError::InvalidPersistedData(
                "invocation audit row disappeared during immutable check",
            ));
        };
        let existing = SqliteInvocationAuditEvent {
            invocation: event.invocation,
            outcome: row.get(0)?,
            session_principal: PrincipalId::from_bytes(id16(
                row.get::<Vec<u8>>(1)?,
                "invocation audit session principal",
            )?),
            effective_principal: row
                .get::<Option<Vec<u8>>>(2)?
                .map(|bytes| id16(bytes, "invocation audit effective principal"))
                .transpose()?
                .map(PrincipalId::from_bytes),
            authorising_principal: row
                .get::<Option<Vec<u8>>>(3)?
                .map(|bytes| id16(bytes, "invocation audit authorising principal"))
                .transpose()?
                .map(PrincipalId::from_bytes),
            function: row
                .get::<Option<Vec<u8>>>(4)?
                .map(|bytes| id16(bytes, "invocation audit function"))
                .transpose()?
                .map(orna_core::FunctionId::from_bytes),
            source_revision: row
                .get::<Option<Vec<u8>>>(5)?
                .map(|bytes| id16(bytes, "invocation audit source revision"))
                .transpose()?
                .map(SourceRevisionId::from_bytes),
            catalogue_revision: row
                .get::<Option<Vec<u8>>>(6)?
                .map(|bytes| id16(bytes, "invocation audit catalogue revision"))
                .transpose()?
                .map(CatalogueRevisionId::from_bytes),
            error_code: row.get(7)?,
        };
        drop(rows);

        if existing == *event {
            return Ok(());
        }
        if existing.outcome == "allowed"
            && matches!(event.outcome.as_str(), "completed" | "failed")
            && same_invocation_facts(&existing, event)
        {
            let changed = connection
                .execute(
                    "UPDATE orna_invocation_audit_events
                     SET outcome = ?1, error_code = ?2
                     WHERE invocation_id = ?3 AND outcome = 'allowed'",
                    [
                        Value::Text(event.outcome.clone()),
                        event
                            .error_code
                            .as_ref()
                            .map_or(Value::Null, |value| Value::Text(value.clone())),
                        Value::Blob(event.invocation.to_bytes().to_vec()),
                    ],
                )
                .await?;
            if changed == 1 {
                return Ok(());
            }
        }
        Err(SqliteError::Domain(
            "invocation audit evidence is immutable or has an invalid transition".to_owned(),
        ))
    }

    /// Loads one redacted invocation evidence record.
    pub async fn load_invocation_audit(
        &self,
        invocation: InvocationId,
    ) -> Result<Option<SqliteInvocationAuditEvent>, SqliteError> {
        let connection = self.connection.lock().await;
        let mut rows = connection
            .query(
                "SELECT outcome, session_principal_id, effective_principal_id,
                        authorising_principal_id, function_id, source_revision_id,
                        catalogue_revision_id, error_code
                 FROM orna_invocation_audit_events
                 WHERE invocation_id = ?1",
                [Value::Blob(invocation.to_bytes().to_vec())],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let session_principal = PrincipalId::from_bytes(id16(
            row.get::<Vec<u8>>(1)?,
            "invocation audit session principal",
        )?);
        let effective_principal = row
            .get::<Option<Vec<u8>>>(2)?
            .map(|bytes| id16(bytes, "invocation audit effective principal"))
            .transpose()?
            .map(PrincipalId::from_bytes);
        let authorising_principal = row
            .get::<Option<Vec<u8>>>(3)?
            .map(|bytes| id16(bytes, "invocation audit authorising principal"))
            .transpose()?
            .map(PrincipalId::from_bytes);
        let function = row
            .get::<Option<Vec<u8>>>(4)?
            .map(|bytes| id16(bytes, "invocation audit function"))
            .transpose()?
            .map(orna_core::FunctionId::from_bytes);
        let source_revision = row
            .get::<Option<Vec<u8>>>(5)?
            .map(|bytes| id16(bytes, "invocation audit source revision"))
            .transpose()?
            .map(SourceRevisionId::from_bytes);
        let catalogue_revision = row
            .get::<Option<Vec<u8>>>(6)?
            .map(|bytes| id16(bytes, "invocation audit catalogue revision"))
            .transpose()?
            .map(CatalogueRevisionId::from_bytes);
        Ok(Some(SqliteInvocationAuditEvent {
            invocation,
            outcome: row.get(0)?,
            session_principal,
            effective_principal,
            authorising_principal,
            function,
            source_revision,
            catalogue_revision,
            error_code: row.get(7)?,
        }))
    }

    /// Loads the most recently recorded invocation for one local principal.
    pub async fn load_latest_invocation_audit(
        &self,
        principal: PrincipalId,
    ) -> Result<Option<SqliteInvocationAuditEvent>, SqliteError> {
        let connection = self.connection.lock().await;
        let mut rows = connection
            .query(
                "SELECT invocation_id
                 FROM orna_invocation_audit_events
                 WHERE session_principal_id = ?1
                 ORDER BY rowid DESC LIMIT 1",
                [Value::Blob(principal.to_bytes().to_vec())],
            )
            .await?;
        let bytes = if let Some(row) = rows.next().await? {
            Some(row.get::<Vec<u8>>(0)?)
        } else {
            None
        };
        let invocation = bytes
            .map(|bytes| id16(bytes, "latest invocation audit identity"))
            .transpose()?
            .map(InvocationId::from_bytes);
        drop(connection);
        match invocation {
            Some(invocation) => self.load_invocation_audit(invocation).await,
            None => Ok(None),
        }
    }

    /// Persists one bounded redacted inspection epoch summary.
    pub async fn record_inspect_snapshot(
        &self,
        record: &SqliteInspectSnapshotRecord,
    ) -> Result<(), SqliteError> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await?;
        let result = Self::record_inspect_snapshot_on(&transaction, record).await;
        match result {
            Ok(()) => {
                transaction.commit().await?;
                Ok(())
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(SqliteError::from(rollback)),
            },
        }
    }

    async fn record_inspect_snapshot_on(
        connection: &Connection,
        record: &SqliteInspectSnapshotRecord,
    ) -> Result<(), SqliteError> {
        ensure_evidence_size(&record.summary, "inspection summary")?;
        let changed = connection
            .execute(
                "INSERT INTO orna_inspect_snapshots
                 (epoch_id, invocation_id, owner_principal_id,
                  source_revision_id, catalogue_revision_id, summary_bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (invocation_id) DO NOTHING",
                [
                    Value::Blob(record.epoch.to_bytes().to_vec()),
                    Value::Blob(record.invocation.to_bytes().to_vec()),
                    Value::Blob(record.owner.to_bytes().to_vec()),
                    Value::Blob(record.source_revision.to_bytes().to_vec()),
                    Value::Blob(record.catalogue_revision.to_bytes().to_vec()),
                    Value::Blob(record.summary.clone()),
                ],
            )
            .await?;
        if changed == 1 {
            return Ok(());
        }
        if changed != 0 {
            return Err(SqliteError::InvalidPersistedData(
                "inspection summary insert affected an unexpected number of rows",
            ));
        }
        let mut rows = connection
            .query(
                "SELECT epoch_id, invocation_id, owner_principal_id,
                        source_revision_id, catalogue_revision_id, summary_bytes
                 FROM orna_inspect_snapshots
                 WHERE invocation_id = ?1 OR epoch_id = ?2",
                [
                    Value::Blob(record.invocation.to_bytes().to_vec()),
                    Value::Blob(record.epoch.to_bytes().to_vec()),
                ],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(SqliteError::InvalidPersistedData(
                "inspection summary row disappeared during immutable check",
            ));
        };
        let existing = SqliteInspectSnapshotRecord {
            epoch: InspectEpochId::from_bytes(id16(row.get(0)?, "inspection epoch")?),
            invocation: InvocationId::from_bytes(id16(row.get(1)?, "inspection invocation")?),
            owner: PrincipalId::from_bytes(id16(row.get(2)?, "inspection owner")?),
            source_revision: SourceRevisionId::from_bytes(id16(
                row.get(3)?,
                "inspection source revision",
            )?),
            catalogue_revision: CatalogueRevisionId::from_bytes(id16(
                row.get(4)?,
                "inspection catalogue revision",
            )?),
            summary: row.get(5)?,
        };
        if existing == *record {
            return Ok(());
        }
        Err(SqliteError::Domain(
            "inspection summary evidence is immutable".to_owned(),
        ))
    }

    /// Reads one inspection snapshot and, optionally, its trace from one
    /// pinned active/security/session transaction.
    ///
    /// The active pair and security digest are checked before any evidence is
    /// selected. Trace and snapshot rows therefore cannot be combined across a
    /// revision or authenticated-session change.
    #[expect(
        clippy::too_many_arguments,
        reason = "the read contract carries active, security, and session pins"
    )]
    pub async fn read_inspect_at(
        &self,
        active: &ActiveDatabaseRevision,
        security: &SecuritySnapshot,
        session: &AuthenticatedSession,
        invocation: InvocationId,
        epoch: Option<InspectEpochId>,
        after_sequence: u64,
        include_trace: bool,
    ) -> Result<
        (
            Option<SqliteInspectSnapshotRecord>,
            Vec<SqliteInspectTraceEvent>,
        ),
        SqliteError,
    > {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await?;
        let result = async {
            let current_active = load_active_from(&transaction).await?;
            let current_security = load_security_snapshot(&transaction, &current_active).await?;
            validate_pinned_context(
                active,
                security,
                session,
                &current_active,
                &current_security,
            )?;
            let snapshot = load_inspect_snapshot_on(&transaction, invocation, epoch).await?;
            if let Some(snapshot) = snapshot.as_ref()
                && (snapshot.source_revision != active.pair().source()
                    || snapshot.catalogue_revision != active.pair().catalogue())
            {
                return Err(SqliteError::InvalidPersistedData(
                    "inspection snapshot is not pinned to the active revision",
                ));
            }
            let events = if include_trace {
                load_inspect_trace_events_on(&transaction, invocation, after_sequence).await?
            } else {
                Vec::new()
            };
            Ok((snapshot, events))
        }
        .await;
        match result {
            Ok(value) => {
                transaction.commit().await?;
                Ok(value)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(SqliteError::from(rollback)),
            },
        }
    }

    /// Loads an exact inspection epoch or the latest epoch for an invocation
    /// under one pinned active/security/session context.
    pub async fn load_inspect_snapshot(
        &self,
        active: &ActiveDatabaseRevision,
        security: &SecuritySnapshot,
        session: &AuthenticatedSession,
        invocation: InvocationId,
        epoch: Option<InspectEpochId>,
    ) -> Result<Option<SqliteInspectSnapshotRecord>, SqliteError> {
        self.read_inspect_at(active, security, session, invocation, epoch, 0, false)
            .await
            .map(|(snapshot, _)| snapshot)
    }

    /// Persists one bounded redacted inspection trace event.
    pub async fn record_inspect_trace_event(
        &self,
        event: &SqliteInspectTraceEvent,
    ) -> Result<(), SqliteError> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await?;
        let result = Self::record_inspect_trace_event_on(&transaction, event).await;
        match result {
            Ok(()) => {
                transaction.commit().await?;
                Ok(())
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(SqliteError::from(rollback)),
            },
        }
    }

    async fn record_inspect_trace_event_on(
        connection: &Connection,
        event: &SqliteInspectTraceEvent,
    ) -> Result<(), SqliteError> {
        ensure_evidence_size(&event.payload, "inspection trace payload")?;
        let sequence = i64::try_from(event.sequence)
            .map_err(|_| SqliteError::Domain("inspection trace sequence overflow".to_owned()))?;
        let changed = connection
            .execute(
                "INSERT INTO orna_inspect_trace_events
                 (invocation_id, sequence, kind, payload_bytes, observer_invocation_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (invocation_id, sequence) DO NOTHING",
                [
                    Value::Blob(event.invocation.to_bytes().to_vec()),
                    Value::Integer(sequence),
                    Value::Text(event.kind.clone()),
                    Value::Blob(event.payload.clone()),
                    event
                        .observer_invocation
                        .map_or(Value::Null, |value| Value::Blob(value.to_bytes().to_vec())),
                ],
            )
            .await?;
        if changed == 1 {
            return Ok(());
        }
        if changed != 0 {
            return Err(SqliteError::InvalidPersistedData(
                "inspection trace insert affected an unexpected number of rows",
            ));
        }
        let mut rows = connection
            .query(
                "SELECT kind, payload_bytes, observer_invocation_id
                 FROM orna_inspect_trace_events
                 WHERE invocation_id = ?1 AND sequence = ?2",
                [
                    Value::Blob(event.invocation.to_bytes().to_vec()),
                    Value::Integer(sequence),
                ],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(SqliteError::InvalidPersistedData(
                "inspection trace row disappeared during immutable check",
            ));
        };
        let existing = SqliteInspectTraceEvent {
            invocation: event.invocation,
            sequence: event.sequence,
            kind: row.get(0)?,
            payload: row.get(1)?,
            observer_invocation: row
                .get::<Option<Vec<u8>>>(2)?
                .map(|bytes| id16(bytes, "inspection observer invocation"))
                .transpose()?
                .map(InvocationId::from_bytes),
        };
        if existing == *event {
            return Ok(());
        }
        Err(SqliteError::Domain(
            "inspection trace evidence is immutable".to_owned(),
        ))
    }

    /// Loads bounded trace events after a resume sequence under one pinned
    /// active/security/session context.
    pub async fn load_inspect_trace_events(
        &self,
        active: &ActiveDatabaseRevision,
        security: &SecuritySnapshot,
        session: &AuthenticatedSession,
        invocation: InvocationId,
        after_sequence: u64,
    ) -> Result<Vec<SqliteInspectTraceEvent>, SqliteError> {
        self.read_inspect_at(
            active,
            security,
            session,
            invocation,
            None,
            after_sequence,
            true,
        )
        .await
        .map(|(_, events)| events)
    }

    /// Persists one redacted USER-state operation record.
    pub async fn record_user_state_audit(
        &self,
        event: &SqliteUserStateAuditEvent,
    ) -> Result<(), SqliteError> {
        let connection = self.connection.lock().await;
        Self::record_user_state_audit_on(&connection, event).await
    }

    async fn record_user_state_audit_on(
        connection: &Connection,
        event: &SqliteUserStateAuditEvent,
    ) -> Result<(), SqliteError> {
        let cell_count = i64::try_from(event.cell_count)
            .map_err(|_| SqliteError::Domain("USER state audit cell count overflow".to_owned()))?;
        let changed = connection
            .execute(
                "INSERT INTO orna_user_state_audit_events
                 (audit_id, operation, outcome, session_principal_id,
                  root_function_id, root_state_profile, cell_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                [
                    Value::Blob(event.audit_id.to_bytes().to_vec()),
                    Value::Text(event.operation.clone()),
                    Value::Text(event.outcome.clone()),
                    Value::Blob(event.session_principal.to_bytes().to_vec()),
                    Value::Blob(event.root_function.to_bytes().to_vec()),
                    Value::Text(event.state_profile.clone()),
                    Value::Integer(cell_count),
                ],
            )
            .await?;
        if changed != 1 {
            return Err(SqliteError::InvalidPersistedData(
                "USER state audit write affected an unexpected number of rows",
            ));
        }
        Ok(())
    }

    /// Loads redacted USER-state operation records for one local principal.
    pub async fn load_user_state_audit(
        &self,
        principal: PrincipalId,
        root_function: orna_core::FunctionId,
        state_profile: &str,
    ) -> Result<Vec<SqliteUserStateAuditEvent>, SqliteError> {
        let connection = self.connection.lock().await;
        let mut rows = connection
            .query(
                "SELECT audit_id, operation, outcome, root_state_profile,
                        cell_count
                 FROM orna_user_state_audit_events
                 WHERE session_principal_id = ?1
                   AND root_function_id = ?2
                   AND root_state_profile = ?3
                 ORDER BY rowid",
                [
                    Value::Blob(principal.to_bytes().to_vec()),
                    Value::Blob(root_function.to_bytes().to_vec()),
                    Value::Text(state_profile.to_owned()),
                ],
            )
            .await?;
        let mut events = Vec::new();
        while let Some(row) = rows.next().await? {
            let cell_count = u64::try_from(row.get::<i64>(4)?)
                .map_err(|_| SqliteError::InvalidPersistedData("USER state audit cell count"))?;
            events.push(SqliteUserStateAuditEvent {
                audit_id: InvocationId::from_bytes(id16(row.get(0)?, "USER state audit id")?),
                operation: row.get(1)?,
                outcome: row.get(2)?,
                session_principal: principal,
                root_function,
                state_profile: row.get(3)?,
                cell_count,
            });
        }
        Ok(events)
    }

    async fn mutate_security_rows_on(
        connection: &Connection,
        statement: &str,
        parameters: impl turso::IntoParams,
    ) -> Result<(), SqliteError> {
        let changed = connection.execute(statement, parameters).await?;
        if changed != 1 {
            return Err(SqliteError::Domain(
                "security mutation affected an unexpected number of rows".to_owned(),
            ));
        }
        Ok(())
    }

    async fn seed_pair(&self) -> Result<BootstrapRevision, SqliteError> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await?;
        let result = seed_pair_in_transaction(&transaction).await;
        match result {
            Ok(pair) => {
                transaction.commit().await?;
                Ok(pair)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(SqliteError::from(rollback)),
            },
        }
    }

    /// Recovers the active durable revision after validating all ledger rows.
    pub async fn recover(&self) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Deferred,
        )
        .await
        .map_err(SqliteError::from)
        .map_err(StorageError::Backend)?;

        let result = async {
            let active = load_active_from(&transaction)
                .await
                .map_err(StorageError::Backend)?;
            let ledger = load_ledger_from(&transaction)
                .await
                .map_err(StorageError::Backend)?;
            validate_ledger_active_pair(&ledger, &active).map_err(StorageError::Backend)?;
            validate_active_catalogue_lineage(&transaction, &active, &ledger)
                .await
                .map_err(StorageError::Backend)?;
            Ok(active)
        }
        .await;

        match result {
            Ok(active) => {
                transaction
                    .commit()
                    .await
                    .map_err(SqliteError::from)
                    .map_err(StorageError::Backend)?;
                Ok(active)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(StorageError::Backend(SqliteError::from(rollback))),
            },
        }
    }

    /// Reads the durable migration ledger oldest-first.
    pub async fn read_ledger(
        &self,
    ) -> Result<Vec<MigrationLedgerEntry>, StorageError<SqliteError>> {
        let connection = self.connection.lock().await;
        let active = load_active_from(&connection)
            .await
            .map_err(StorageError::Backend)?;
        let ledger = load_ledger_from(&connection)
            .await
            .map_err(StorageError::Backend)?;
        validate_ledger_active_pair(&ledger, &active).map_err(StorageError::Backend)?;
        validate_active_catalogue_lineage(&connection, &active, &ledger)
            .await
            .map_err(StorageError::Backend)?;
        Ok(ledger)
    }

    /// Executes the supported pure server-function subset against this local
    /// database and returns the canonical result values.
    ///
    /// The initial SQLite runtime slice accepts checked parameter-echo and
    /// server-plan artifacts. Other server artifacts remain explicit backend
    /// errors rather than being interpreted as source text.
    pub async fn execute_server_function(
        &self,
        function_id: orna_core::FunctionId,
        arguments: &[(ParameterId, RuntimeValue)],
    ) -> Result<Vec<RuntimeValue>, SqliteError> {
        let active = self.recover().await.map_err(storage_error_to_sqlite)?;
        self.execute_server_function_at(&active, function_id, arguments)
            .await
    }

    /// Executes one supported SERVER function against a caller-pinned active
    /// revision.
    ///
    /// Routes that authenticate and authorize against a recovered revision
    /// should use this entry point so execution cannot silently switch to a
    /// newer catalogue or executable artifact between those checks and the
    /// call.
    pub async fn execute_server_function_at(
        &self,
        active: &ActiveDatabaseRevision,
        function_id: orna_core::FunctionId,
        arguments: &[(ParameterId, RuntimeValue)],
    ) -> Result<Vec<RuntimeValue>, SqliteError> {
        let connection = self.connection.lock().await;
        self.execute_server_function_at_with_connection(&connection, active, function_id, arguments)
            .await
    }

    /// Authenticates the local peer, authorizes one SERVER call, and executes
    /// it under one SQLite write transaction against the pinned revision.
    pub async fn execute_local_peer_server_function_at(
        &self,
        active: &ActiveDatabaseRevision,
        uid: u32,
        invocation: InvocationId,
        function_id: orna_core::FunctionId,
        arguments: &[(ParameterId, RuntimeValue)],
    ) -> Result<SqliteExecutionResult, SqliteError> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await?;
        let current_active = match load_active_from(&transaction).await {
            Ok(active) => active,
            Err(error) => {
                let _ = transaction.rollback().await;
                return Err(error);
            }
        };
        if current_active.pair() != active.pair() {
            let _ = transaction.rollback().await;
            return Err(SqliteError::Domain(
                "the active SQLite revision changed before execution".to_owned(),
            ));
        }
        let active = &current_active;
        let snapshot = match load_security_snapshot(&transaction, active).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = transaction.rollback().await;
                return Err(error);
            }
        };
        let session = snapshot.authenticate_local_peer(uid).map_err(|error| {
            SqliteError::Domain(format!("local peer authentication failed: {error}"))
        })?;
        let decision = snapshot.authorise_execute(
            &session,
            orna_core::security::InvocationTarget::new(function_id, active.pair()),
        );
        let authorisation = match decision {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(reason) => {
                Self::record_invocation_audit_on(
                    &transaction,
                    &SqliteInvocationAuditEvent {
                        invocation,
                        outcome: "denied".to_owned(),
                        session_principal: session.principal(),
                        effective_principal: None,
                        authorising_principal: None,
                        function: Some(function_id),
                        source_revision: Some(active.pair().source()),
                        catalogue_revision: Some(active.pair().catalogue()),
                        error_code: Some("execution.execute_denied".to_owned()),
                    },
                )
                .await?;
                transaction.commit().await?;
                return Ok(SqliteExecutionResult::Denied { session, reason });
            }
        };
        Self::record_invocation_audit_on(
            &transaction,
            &SqliteInvocationAuditEvent {
                invocation,
                outcome: "allowed".to_owned(),
                session_principal: session.principal(),
                effective_principal: Some(authorisation.effective_principal()),
                authorising_principal: Some(authorisation.authorising_principal()),
                function: Some(function_id),
                source_revision: Some(active.pair().source()),
                catalogue_revision: Some(active.pair().catalogue()),
                error_code: None,
            },
        )
        .await?;
        let values = match self
            .execute_server_function_at_with_connection(
                &transaction,
                active,
                function_id,
                arguments,
            )
            .await
        {
            Ok(values) => values,
            Err(error) => {
                Self::record_invocation_audit_on(
                    &transaction,
                    &SqliteInvocationAuditEvent {
                        invocation,
                        outcome: "failed".to_owned(),
                        session_principal: session.principal(),
                        effective_principal: Some(authorisation.effective_principal()),
                        authorising_principal: Some(authorisation.authorising_principal()),
                        function: Some(function_id),
                        source_revision: Some(active.pair().source()),
                        catalogue_revision: Some(active.pair().catalogue()),
                        error_code: Some("execution.target_failure".to_owned()),
                    },
                )
                .await?;
                transaction.commit().await?;
                return Ok(SqliteExecutionResult::Failed {
                    session,
                    authorisation,
                    error,
                });
            }
        };
        let summary = serde_json::to_vec(&serde_json::json!({
            "record": "inspect_summary",
            "invocation_id": invocation.canonical(),
            "owner_principal": session.principal().canonical(),
            "source_revision_id": active.pair().source().canonical(),
            "catalogue_revision_id": active.pair().catalogue().canonical(),
            "function_id": function_id.canonical(),
            "outcome": "completed",
            "result_count": values.len(),
            "calls": [{
                "function_id": function_id.canonical(),
                "outcome": "completed",
                "result_count": values.len(),
            }],
            "resources": [],
            "state_cells": [],
        }))
        .map_err(|error| {
            SqliteError::Domain(format!("could not encode inspection summary: {error}"))
        })?;
        Self::record_inspect_snapshot_on(
            &transaction,
            &SqliteInspectSnapshotRecord {
                epoch: InspectEpochId::new(),
                invocation,
                owner: session.principal(),
                source_revision: active.pair().source(),
                catalogue_revision: active.pair().catalogue(),
                summary,
            },
        )
        .await?;
        let payload = serde_json::to_vec(&serde_json::json!({
            "record": "inspect_trace",
            "invocation_id": invocation.canonical(),
            "function_id": function_id.canonical(),
            "outcome": "completed",
        }))
        .map_err(|error| {
            SqliteError::Domain(format!("could not encode inspection trace event: {error}"))
        })?;
        Self::record_inspect_trace_event_on(
            &transaction,
            &SqliteInspectTraceEvent {
                invocation,
                sequence: 1,
                kind: "invocation.completed".to_owned(),
                payload,
                observer_invocation: None,
            },
        )
        .await?;
        Self::record_invocation_audit_on(
            &transaction,
            &SqliteInvocationAuditEvent {
                invocation,
                outcome: "completed".to_owned(),
                session_principal: session.principal(),
                effective_principal: Some(authorisation.effective_principal()),
                authorising_principal: Some(authorisation.authorising_principal()),
                function: Some(function_id),
                source_revision: Some(active.pair().source()),
                catalogue_revision: Some(active.pair().catalogue()),
                error_code: None,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(SqliteExecutionResult::Allowed {
            session,
            authorisation,
            values,
        })
    }

    async fn execute_server_function_at_with_connection(
        &self,
        connection: &Connection,
        active: &ActiveDatabaseRevision,
        function_id: orna_core::FunctionId,
        arguments: &[(ParameterId, RuntimeValue)],
    ) -> Result<Vec<RuntimeValue>, SqliteError> {
        let function = active
            .catalogue()
            .function_by_id(function_id)
            .ok_or_else(|| {
                SqliteError::Domain(format!("function {function_id} is not installed"))
            })?;
        if function.domain() != FunctionDomain::Server {
            return Err(SqliteError::Domain(format!(
                "function {function_id} is not a server function"
            )));
        }
        let revision = active
            .function_revisions()
            .iter()
            .find(|revision| {
                revision.function() == function_id && revision.id() == function.current_revision()
            })
            .ok_or(SqliteError::InvalidPersistedData(
                "active function has no executable revision",
            ))?;
        if revision.artifact().kind() != ExecutableArtifactKind::Server {
            return Err(SqliteError::Domain(format!(
                "function {function_id} does not have a server executable"
            )));
        }
        validate_function_arguments(function, arguments)?;
        if revision.artifact().format() == orna_artifact::server_parameter_echo::FORMAT_IDENTITY {
            if revision.artifact().version() != orna_artifact::server_parameter_echo::FORMAT_VERSION
            {
                return Err(SqliteError::Domain(format!(
                    "SQLite execution does not support {} artifact version {}",
                    revision.artifact().format(),
                    revision.artifact().version()
                )));
            }
            let [parameter] = function.parameters() else {
                return Err(SqliteError::Domain(
                    "server parameter-echo function must have one parameter".to_owned(),
                ));
            };
            let expected_type = standard_type_id(parameter.resolved_type()).ok_or_else(|| {
                SqliteError::Domain("server parameter-echo value type is not supported".to_owned())
            })?;
            ServerParameterEcho::decode(
                revision.artifact().payload(),
                parameter.id(),
                expected_type,
            )
            .map_err(|error| SqliteError::Domain(error.to_string()))?;
            let value = arguments
                .iter()
                .find(|(parameter_id, _)| *parameter_id == parameter.id())
                .map(|(_, value)| value)
                .ok_or_else(|| {
                    SqliteError::Domain(format!(
                        "function {function_id} requires exactly parameter {}",
                        parameter.id()
                    ))
                })?;
            return Ok(vec![value.clone()]);
        }
        if revision.artifact().format() == orna_artifact::server_plan::FORMAT_IDENTITY {
            return self
                .execute_server_plan(connection, active, function, revision, arguments)
                .await;
        }
        Err(SqliteError::Domain(format!(
            "SQLite execution does not support the {} server artifact",
            revision.artifact().format()
        )))
    }

    async fn execute_server_plan(
        &self,
        connection: &Connection,
        active: &ActiveDatabaseRevision,
        function: &FunctionDefinition,
        revision: &FunctionRevisionRecord,
        arguments: &[(ParameterId, RuntimeValue)],
    ) -> Result<Vec<RuntimeValue>, SqliteError> {
        let decoded =
            decode_query_plan(revision.artifact().version(), revision.artifact().payload())?;
        let return_count = match function.return_type() {
            FunctionReturn::Single(_) | FunctionReturn::Stream(_) => 1,
            FunctionReturn::Rows(columns) => columns.len(),
        };
        if decoded.projections.len() != return_count {
            return Err(SqliteError::Domain(
                "server plan projection count does not match function result shape".to_owned(),
            ));
        }
        if !decoded.ordering.is_empty() {
            return Err(SqliteError::Domain(
                "SQLite server plan ordering is not supported".to_owned(),
            ));
        }
        let object = active
            .catalogue()
            .object_type_by_id(decoded.scan.object_type)
            .ok_or_else(|| {
                SqliteError::Domain("server plan scans an unknown object type".to_owned())
            })?;
        let mut columns = Vec::with_capacity(object.fields().len() + 1);
        columns.push(object_id_column().to_owned());
        columns.extend(object.fields().iter().map(|field| field_name(field.id())));
        let query = format!(
            "SELECT {} FROM {} ORDER BY {}",
            columns.join(", "),
            object_table_name(object.id()),
            object_id_column()
        );
        let mut rows = connection.query(&query, ()).await?;
        let mut output_rows: Vec<Vec<RuntimeValue>> = Vec::new();
        while let Some(row) = rows.next().await? {
            let object_id = ObjectId::from_bytes(id16(row.get::<Vec<u8>>(0)?, "object id")?);
            let mut fields = Vec::with_capacity(object.fields().len());
            for (index, field) in object.fields().iter().enumerate() {
                fields.push((
                    field.id(),
                    runtime_value_from_sql(
                        row.get_value(index + 1)?,
                        field.resolved_type(),
                        field.nullable(),
                    )?,
                ));
            }
            let record = SqliteObjectRow {
                object_type: decoded.scan.object_type,
                object_id,
                fields,
            };
            if !selector_matches(&decoded.selector, &record, arguments)? {
                continue;
            }
            if let Some(selection) = decoded.selection.as_ref() {
                let RuntimeValue::Boolean(accepted) = evaluate_expression(selection, &record)?
                else {
                    return Err(SqliteError::Domain(
                        "server plan selection does not evaluate to BOOLEAN".to_owned(),
                    ));
                };
                if !accepted {
                    continue;
                }
            }
            let mut projected = Vec::with_capacity(decoded.projections.len());
            for expression in &decoded.projections {
                projected.push(evaluate_expression(expression, &record)?);
            }
            if decoded.distinct && output_rows.contains(&projected) {
                continue;
            }
            output_rows.push(projected);
        }
        Ok(output_rows.into_iter().flatten().collect())
    }

    /// Compatibility entry point that plans the exact artifact before applying.
    pub async fn apply(
        &self,
        candidate: &DeployableRevision,
    ) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        let active = self.recover().await?;
        let artifact =
            PhysicalMigrationArtifact::from_revisions(&active, candidate).map_err(|error| {
                StorageError::InvalidRequest(MigrationLedgerEntryError::PhysicalArtifact(error))
            })?;
        self.apply_with_artifact(candidate, &artifact).await
    }

    async fn apply_with_artifact(
        &self,
        candidate: &DeployableRevision,
        artifact: &PhysicalMigrationArtifact,
    ) -> Result<ActiveDatabaseRevision, StorageError<SqliteError>> {
        let mut connection = self.connection.lock().await;
        let transaction = turso::transaction::Transaction::new(
            &mut connection,
            turso::transaction::TransactionBehavior::Immediate,
        )
        .await
        .map_err(SqliteError::from)
        .map_err(StorageError::Backend)?;

        let result = apply_in_transaction(&transaction, candidate, artifact).await;
        match result {
            Ok(()) => {
                let active = match load_active_from(&transaction).await {
                    Ok(active) => active,
                    Err(error) => match transaction.rollback().await {
                        Ok(()) => return Err(StorageError::Backend(error)),
                        Err(rollback) => {
                            return Err(StorageError::Backend(SqliteError::from(rollback)));
                        }
                    },
                };
                transaction
                    .commit()
                    .await
                    .map_err(SqliteError::from)
                    .map_err(StorageError::Backend)?;
                Ok(active)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(StorageError::Backend(SqliteError::from(rollback))),
            },
        }
    }
}

fn storage_error_to_sqlite(error: StorageError<SqliteError>) -> SqliteError {
    match error {
        StorageError::Backend(error) => error,
        StorageError::InvalidRequest(error) => SqliteError::Domain(error.to_string()),
    }
}

fn validate_pinned_context(
    active: &ActiveDatabaseRevision,
    security: &SecuritySnapshot,
    session: &AuthenticatedSession,
    current_active: &ActiveDatabaseRevision,
    current_security: &SecuritySnapshot,
) -> Result<(), SqliteError> {
    if security.revision() != active.pair() {
        return Err(SqliteError::Domain(
            "the supplied SQLite security snapshot is not pinned to the active revision".to_owned(),
        ));
    }
    if current_active.pair() != active.pair() {
        return Err(SqliteError::Domain(
            "the active SQLite revision changed before the operation".to_owned(),
        ));
    }
    if current_security.revision() != active.pair()
        || current_security.security_context_digest() != security.security_context_digest()
    {
        return Err(SqliteError::Domain(
            "the SQLite security snapshot changed before the operation".to_owned(),
        ));
    }
    current_security
        .bind_authenticated_session(session.principal(), session.active_roles().to_vec())
        .map_err(|error| {
            SqliteError::Domain(format!(
                "the authenticated SQLite session is no longer valid: {error}"
            ))
        })?;
    Ok(())
}

fn is_sealed_inspect_type_id(type_id: TypeId) -> bool {
    matches!(
        type_id,
        orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID
            | orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID
            | orna_core::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID
            | orna_core::system::SYS_INSPECT_TRACE_EVENT_TYPE_ID
            | orna_core::system::SYS_INSPECT_INVOCATION_NODES_TYPE_ID
            | orna_core::system::SYS_INSPECT_CALLS_TYPE_ID
            | orna_core::system::SYS_INSPECT_RESOURCES_TYPE_ID
            | orna_core::system::SYS_INSPECT_STATE_CELLS_TYPE_ID
            | orna_core::system::SYS_INSPECT_UI_NODES_TYPE_ID
            | orna_core::system::SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID
            | orna_core::system::SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID
            | orna_core::system::SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID
    )
}

fn same_invocation_facts(
    left: &SqliteInvocationAuditEvent,
    right: &SqliteInvocationAuditEvent,
) -> bool {
    left.invocation == right.invocation
        && left.session_principal == right.session_principal
        && left.effective_principal == right.effective_principal
        && left.authorising_principal == right.authorising_principal
        && left.function == right.function
        && left.source_revision == right.source_revision
        && left.catalogue_revision == right.catalogue_revision
}

async fn load_inspect_snapshot_on(
    connection: &Connection,
    invocation: InvocationId,
    epoch: Option<InspectEpochId>,
) -> Result<Option<SqliteInspectSnapshotRecord>, SqliteError> {
    let (sql, params): (&str, Vec<Value>) = match epoch {
        Some(epoch) => (
            "SELECT epoch_id, owner_principal_id, source_revision_id,
                    catalogue_revision_id, summary_bytes
             FROM orna_inspect_snapshots
             WHERE invocation_id = ?1 AND epoch_id = ?2",
            vec![
                Value::Blob(invocation.to_bytes().to_vec()),
                Value::Blob(epoch.to_bytes().to_vec()),
            ],
        ),
        None => (
            "SELECT epoch_id, owner_principal_id, source_revision_id,
                    catalogue_revision_id, summary_bytes
             FROM orna_inspect_snapshots
             WHERE invocation_id = ?1
             ORDER BY rowid DESC LIMIT 1",
            vec![Value::Blob(invocation.to_bytes().to_vec())],
        ),
    };
    let mut rows = connection.query(sql, params).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(SqliteInspectSnapshotRecord {
        epoch: InspectEpochId::from_bytes(id16(row.get(0)?, "inspection epoch")?),
        invocation,
        owner: PrincipalId::from_bytes(id16(row.get(1)?, "inspection owner")?),
        source_revision: SourceRevisionId::from_bytes(id16(
            row.get(2)?,
            "inspection source revision",
        )?),
        catalogue_revision: CatalogueRevisionId::from_bytes(id16(
            row.get(3)?,
            "inspection catalogue revision",
        )?),
        summary: row.get(4)?,
    }))
}

async fn load_inspect_trace_events_on(
    connection: &Connection,
    invocation: InvocationId,
    after_sequence: u64,
) -> Result<Vec<SqliteInspectTraceEvent>, SqliteError> {
    let after = i64::try_from(after_sequence)
        .map_err(|_| SqliteError::Domain("inspection trace sequence overflow".to_owned()))?;
    let mut rows = connection
        .query(
            "SELECT sequence, kind, payload_bytes, observer_invocation_id
             FROM orna_inspect_trace_events
             WHERE invocation_id = ?1 AND sequence > ?2
             ORDER BY sequence",
            [
                Value::Blob(invocation.to_bytes().to_vec()),
                Value::Integer(after),
            ],
        )
        .await?;
    let mut events = Vec::new();
    while let Some(row) = rows.next().await? {
        let sequence = u64::try_from(row.get::<i64>(0)?)
            .map_err(|_| SqliteError::InvalidPersistedData("inspection trace sequence"))?;
        let observer_invocation = row
            .get::<Option<Vec<u8>>>(3)?
            .map(|bytes| id16(bytes, "inspection observer invocation"))
            .transpose()?
            .map(InvocationId::from_bytes);
        events.push(SqliteInspectTraceEvent {
            invocation,
            sequence,
            kind: row.get(1)?,
            payload: row.get(2)?,
            observer_invocation,
        });
    }
    Ok(events)
}

async fn load_security_snapshot(
    connection: &Connection,
    active: &ActiveDatabaseRevision,
) -> Result<SecuritySnapshot, SqliteError> {
    let mut principals = Vec::new();
    let mut rows = connection
        .query(
            "SELECT principal_id, kind, status
             FROM orna_security_principals
             ORDER BY principal_id",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let principal_id =
            PrincipalId::from_bytes(id16(row.get::<Vec<u8>>(0)?, "security principal id")?);
        let kind = match row.get::<String>(1)?.as_str() {
            "user" => PrincipalKind::User,
            "role" => PrincipalKind::Role,
            "service" => PrincipalKind::Service,
            _ => {
                return Err(SqliteError::InvalidPersistedData(
                    "security principal kind is invalid",
                ));
            }
        };
        let status = match row.get::<String>(2)?.as_str() {
            "active" => PrincipalStatus::Active,
            "disabled" => PrincipalStatus::Disabled,
            _ => {
                return Err(SqliteError::InvalidPersistedData(
                    "security principal status is invalid",
                ));
            }
        };
        principals.push(Principal::new(principal_id, kind, status));
    }
    drop(rows);

    let mut memberships = Vec::new();
    let mut rows = connection
        .query(
            "SELECT role_id, member_id
             FROM orna_security_role_memberships
             ORDER BY member_id, role_id",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        memberships.push(RoleMembership::new(
            PrincipalId::from_bytes(id16(row.get::<Vec<u8>>(0)?, "security role id")?),
            PrincipalId::from_bytes(id16(
                row.get::<Vec<u8>>(1)?,
                "security membership member id",
            )?),
        ));
    }
    drop(rows);

    let mut execute_grants = Vec::new();
    let mut rows = connection
        .query(
            "SELECT grantee_id, function_id
             FROM orna_security_execute_grants
             ORDER BY grantee_id, function_id",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        execute_grants.push(ExecuteGrant::new(
            PrincipalId::from_bytes(id16(row.get::<Vec<u8>>(0)?, "execute grantee id")?),
            orna_core::FunctionId::from_bytes(id16(row.get::<Vec<u8>>(1)?, "execute function id")?),
        ));
    }
    drop(rows);

    let mut local_peer_credentials = Vec::new();
    let mut rows = connection
        .query(
            "SELECT uid, principal_id
             FROM orna_security_local_peer_credentials
             ORDER BY uid",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let uid = u32::try_from(row.get::<i64>(0)?)
            .map_err(|_| SqliteError::InvalidPersistedData("local peer UID is invalid"))?;
        local_peer_credentials.push(LocalPeerCredential::new(
            uid,
            PrincipalId::from_bytes(id16(row.get::<Vec<u8>>(1)?, "local peer principal id")?),
        ));
    }
    drop(rows);

    let mut privilege_grants = Vec::new();
    let mut rows = connection
        .query(
            "SELECT grantee_id, privilege, object_id
             FROM orna_security_privilege_grants
             ORDER BY grantee_id, privilege, object_id",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let grantee =
            PrincipalId::from_bytes(id16(row.get::<Vec<u8>>(0)?, "privilege grantee id")?);
        let class = parse_privilege_class(&row.get::<String>(1)?)?;
        let object = match row.get_value(2)? {
            Value::Null => None,
            Value::Blob(value) => Some(orna_core::FunctionId::from_bytes(id16(
                value,
                "privilege object id",
            )?)),
            _ => {
                return Err(SqliteError::InvalidPersistedData(
                    "privilege object id is not a BLOB or NULL",
                ));
            }
        };
        privilege_grants.push(
            PrivilegeGrant::new(grantee, class, object)
                .map_err(|_| SqliteError::InvalidPersistedData("invalid privilege grant"))?,
        );
    }
    drop(rows);

    SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
        active.pair(),
        active
            .catalogue()
            .functions()
            .iter()
            .map(|function| orna_core::security::SecurityFunctionTarget::application(function.id()))
            .collect(),
        principals,
        memberships,
        execute_grants,
        local_peer_credentials,
        privilege_grants,
    )
    .map_err(|error| SqliteError::Domain(format!("invalid SQLite security snapshot: {error}")))
}

fn parse_privilege_class(value: &str) -> Result<PrivilegeClass, SqliteError> {
    match value {
        "execute" => Ok(PrivilegeClass::Execute),
        "security_admin" => Ok(PrivilegeClass::SecurityAdmin),
        "inspect:own-invocation" => Ok(PrivilegeClass::Inspect(
            orna_core::inspect::InspectPrivilege::OwnInvocation,
        )),
        "inspect:session-invocations" => Ok(PrivilegeClass::Inspect(
            orna_core::inspect::InspectPrivilege::SessionInvocations,
        )),
        "inspect:any-invocation" => Ok(PrivilegeClass::Inspect(
            orna_core::inspect::InspectPrivilege::AnyInvocation,
        )),
        "inspect:values" => Ok(PrivilegeClass::Inspect(
            orna_core::inspect::InspectPrivilege::Values,
        )),
        "inspect:source" => Ok(PrivilegeClass::Inspect(
            orna_core::inspect::InspectPrivilege::Source,
        )),
        "inspect:security-details" => Ok(PrivilegeClass::Inspect(
            orna_core::inspect::InspectPrivilege::SecurityDetails,
        )),
        "inspect:runtime-internals" => Ok(PrivilegeClass::Inspect(
            orna_core::inspect::InspectPrivilege::RuntimeInternals,
        )),
        _ => Err(SqliteError::InvalidPersistedData(
            "security privilege class is invalid",
        )),
    }
}

fn local_peer_principal_id(uid: u32) -> PrincipalId {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(b"ORNAUID\0");
    bytes[8..12].copy_from_slice(&uid.to_be_bytes());
    bytes[12..].copy_from_slice(&(!uid).to_be_bytes());
    PrincipalId::from_bytes(bytes)
}

fn principal_kind_text(kind: PrincipalKind) -> &'static str {
    match kind {
        PrincipalKind::User => "user",
        PrincipalKind::Role => "role",
        PrincipalKind::Service => "service",
    }
}

fn require_security_principal(
    snapshot: &SecuritySnapshot,
    principal: PrincipalId,
) -> Result<(), SqliteError> {
    if snapshot.principals().any(|value| value.id() == principal) {
        Ok(())
    } else {
        Err(SqliteError::Domain(
            "the security principal is not installed".to_owned(),
        ))
    }
}

fn require_security_role(
    snapshot: &SecuritySnapshot,
    role: PrincipalId,
) -> Result<(), SqliteError> {
    let Some(principal) = snapshot.principals().find(|value| value.id() == role) else {
        return Err(SqliteError::Domain(
            "the security role is not installed".to_owned(),
        ));
    };
    if principal.kind() != PrincipalKind::Role {
        return Err(SqliteError::Domain(
            "the membership target is not a role".to_owned(),
        ));
    }
    Ok(())
}
fn require_security_privilege_object(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> Result<(), SqliteError> {
    if active.catalogue().function_by_id(function).is_some()
        || system_function_by_id(function).is_some()
    {
        Ok(())
    } else {
        Err(SqliteError::Domain(
            "the privilege object function is not installed".to_owned(),
        ))
    }
}

fn ensure_evidence_size(bytes: &[u8], label: &'static str) -> Result<(), SqliteError> {
    const MAX_EVIDENCE_BYTES: usize = 64 * 1024;
    if bytes.len() > MAX_EVIDENCE_BYTES {
        return Err(SqliteError::Domain(format!(
            "{label} exceeds the {MAX_EVIDENCE_BYTES} byte SQLite evidence limit"
        )));
    }
    Ok(())
}

fn validate_function_arguments(
    function: &FunctionDefinition,
    arguments: &[(ParameterId, RuntimeValue)],
) -> Result<(), SqliteError> {
    if arguments.len() != function.parameters().len() {
        return Err(SqliteError::Domain(format!(
            "function {} requires exactly {} arguments",
            function.id(),
            function.parameters().len()
        )));
    }
    for parameter in function.parameters() {
        let Some((_, value)) = arguments
            .iter()
            .find(|(parameter_id, _)| *parameter_id == parameter.id())
        else {
            return Err(SqliteError::Domain(format!(
                "function {} is missing parameter {}",
                function.id(),
                parameter.id()
            )));
        };
        if value.runtime_type() != RuntimeType::Flat(parameter.resolved_type()) {
            return Err(SqliteError::Domain(format!(
                "function {} received a value with the wrong type for parameter {}",
                function.id(),
                parameter.id()
            )));
        }
    }
    Ok(())
}

fn standard_type_id(resolved_type: ResolvedType) -> Option<TypeId> {
    match resolved_type {
        ResolvedType::Scalar(scalar) => Some(match scalar {
            StandardScalar::Boolean => BOOLEAN_TYPE_ID,
            StandardScalar::Integer => INTEGER_TYPE_ID,
            StandardScalar::BigInt => BIGINT_TYPE_ID,
            StandardScalar::Float => FLOAT_TYPE_ID,
            StandardScalar::Decimal => DECIMAL_TYPE_ID,
            StandardScalar::CharacterLargeObject => CHARACTER_LARGE_OBJECT_TYPE_ID,
            StandardScalar::BinaryLargeObject => BINARY_LARGE_OBJECT_TYPE_ID,
            StandardScalar::Uuid => UUID_TYPE_ID,
            StandardScalar::Date => DATE_TYPE_ID,
            StandardScalar::Time => TIME_TYPE_ID,
            StandardScalar::Timestamp => TIMESTAMP_TYPE_ID,
            StandardScalar::Duration => DURATION_TYPE_ID,
            StandardScalar::Void => VOID_TYPE_ID,
        }),
        ResolvedType::Named(type_id)
        | ResolvedType::Reference { target: type_id }
        | ResolvedType::Value(type_id) => Some(type_id),
    }
}

struct DecodedQueryPlan {
    scan: Scan,
    projections: Vec<Expression>,
    selection: Option<Expression>,
    selector: Option<QuerySelector>,
    ordering: Vec<orna_artifact::server_plan::Ordering>,
    distinct: bool,
}

enum QuerySelector {
    Identity {
        target: TypeId,
        parameter: ParameterId,
    },
    UniqueText {
        field: FieldId,
        parameter: ParameterId,
    },
}

fn decode_query_plan(version: u32, payload: &[u8]) -> Result<DecodedQueryPlan, SqliteError> {
    match version {
        orna_artifact::server_plan::FORMAT_VERSION => {
            let plan = ServerPlan::decode(payload)
                .map_err(|error| SqliteError::Domain(error.to_string()))?;
            Ok(DecodedQueryPlan {
                scan: plan.scan,
                projections: plan.projections,
                selection: plan.selection,
                selector: None,
                ordering: plan.ordering,
                distinct: false,
            })
        }
        orna_artifact::server_plan::IDENTITY_SELECTED_FORMAT_VERSION => {
            let plan = IdentitySelectedServerPlan::decode(payload)
                .map_err(|error| SqliteError::Domain(error.to_string()))?;
            let selector = plan.selector();
            Ok(DecodedQueryPlan {
                scan: plan.scan(),
                projections: plan.projections().to_vec(),
                selection: None,
                selector: Some(QuerySelector::Identity {
                    target: plan.scan().object_type,
                    parameter: selector.parameter(),
                }),
                ordering: Vec::new(),
                distinct: false,
            })
        }
        orna_artifact::server_plan::DISTINCT_FORMAT_VERSION => {
            let plan = DistinctServerPlan::decode(payload)
                .map_err(|error| SqliteError::Domain(error.to_string()))?;
            Ok(DecodedQueryPlan {
                scan: plan.scan(),
                projections: plan.projections().to_vec(),
                selection: plan.selection().cloned(),
                selector: None,
                ordering: Vec::new(),
                distinct: true,
            })
        }
        orna_artifact::server_plan::UNIQUE_TEXT_SELECTED_FORMAT_VERSION => {
            let plan = UniqueTextSelectedServerPlan::decode(payload)
                .map_err(|error| SqliteError::Domain(error.to_string()))?;
            let SelectBindValue::Text {
                field, parameter, ..
            } = *plan.selector();
            Ok(DecodedQueryPlan {
                scan: plan.scan(),
                projections: plan.projections().to_vec(),
                selection: None,
                selector: Some(QuerySelector::UniqueText { field, parameter }),
                ordering: Vec::new(),
                distinct: false,
            })
        }
        version => Err(SqliteError::Domain(format!(
            "unsupported orna.server-plan artifact version {version}"
        ))),
    }
}
struct SqliteObjectRow {
    object_type: TypeId,
    object_id: ObjectId,
    fields: Vec<(FieldId, RuntimeValue)>,
}

fn selector_matches(
    selector: &Option<QuerySelector>,
    row: &SqliteObjectRow,
    arguments: &[(ParameterId, RuntimeValue)],
) -> Result<bool, SqliteError> {
    match selector {
        None => Ok(true),
        Some(QuerySelector::Identity { target, parameter }) => {
            let value = arguments
                .iter()
                .find(|(candidate, _)| candidate == parameter)
                .map(|(_, value)| value)
                .ok_or_else(|| {
                    SqliteError::Domain("server selector parameter is missing".to_owned())
                })?;
            Ok(matches!(
                value,
                RuntimeValue::Reference {
                    target: actual_target,
                    object
                } if *actual_target == *target && *object == row.object_id
            ))
        }
        Some(QuerySelector::UniqueText { field, parameter }) => {
            let value = arguments
                .iter()
                .find(|(candidate, _)| candidate == parameter)
                .map(|(_, value)| value)
                .ok_or_else(|| {
                    SqliteError::Domain("server selector parameter is missing".to_owned())
                })?;
            let RuntimeValue::Text(value) = value else {
                return Ok(false);
            };
            let field_value = row
                .fields
                .iter()
                .find(|(candidate, _)| candidate == field)
                .map(|(_, value)| value)
                .ok_or_else(|| {
                    SqliteError::Domain("server selector field is missing".to_owned())
                })?;
            Ok(matches!(field_value, RuntimeValue::Text(candidate) if candidate == value))
        }
    }
}

fn evaluate_expression(
    expression: &Expression,
    row: &SqliteObjectRow,
) -> Result<RuntimeValue, SqliteError> {
    match &expression.kind {
        ExpressionKind::ObjectReference { .. } => Ok(RuntimeValue::Reference {
            target: row.object_type,
            object: row.object_id,
        }),
        ExpressionKind::FieldPath { steps, .. } => {
            if steps.len() != 1 {
                return Err(SqliteError::Domain(
                    "SQLite server plans support one-step field paths".to_owned(),
                ));
            }
            let field = row
                .fields
                .iter()
                .find(|(candidate, _)| *candidate == steps[0].field)
                .map(|(_, value)| value)
                .ok_or_else(|| SqliteError::Domain("server plan field is missing".to_owned()))?;
            Ok(field.clone())
        }
        ExpressionKind::BooleanLiteral { value } => Ok(RuntimeValue::Boolean(*value)),
        ExpressionKind::Equality { left, right } => {
            let left = evaluate_expression(left, row)?;
            let right = evaluate_expression(right, row)?;
            Ok(RuntimeValue::Boolean(
                !left.is_null() && !right.is_null() && left == right,
            ))
        }
    }
}

fn runtime_value_from_sql(
    value: Value,
    resolved_type: ResolvedType,
    nullable: bool,
) -> Result<RuntimeValue, SqliteError> {
    if matches!(value, Value::Null) {
        if !nullable {
            return Err(SqliteError::InvalidPersistedData(
                "non-null SQLite object field contains NULL",
            ));
        }
        return RuntimeValue::null(resolved_type)
            .map_err(|error| SqliteError::Domain(error.to_string()));
    }
    if let Some(scalar) = resolved_type.legacy_scalar() {
        return match (scalar, value) {
            (StandardScalar::Boolean, Value::Integer(value)) => match value {
                0 => Ok(RuntimeValue::Boolean(false)),
                1 => Ok(RuntimeValue::Boolean(true)),
                _ => Err(SqliteError::InvalidPersistedData(
                    "SQLite BOOLEAN field is not 0 or 1",
                )),
            },
            (StandardScalar::Integer, Value::Integer(value)) => i32::try_from(value)
                .map(RuntimeValue::Integer)
                .map_err(|_| SqliteError::InvalidPersistedData("SQLite INTEGER is out of range")),
            (StandardScalar::BigInt, Value::Integer(value)) => Ok(RuntimeValue::BigInt(value)),
            (StandardScalar::Float, Value::Real(value)) => RuntimeFloat::new(value)
                .map(RuntimeValue::Float)
                .map_err(|error| SqliteError::Domain(error.to_string())),
            (StandardScalar::CharacterLargeObject, Value::Text(value)) => {
                Ok(RuntimeValue::Text(value))
            }
            (StandardScalar::BinaryLargeObject, Value::Blob(value)) => {
                Ok(RuntimeValue::Bytes(value))
            }
            (scalar, value) => Err(SqliteError::Domain(format!(
                "SQLite value {value:?} does not match {scalar:?}"
            ))),
        };
    }
    if let Some(target) = resolved_type.reference_target() {
        let Value::Blob(value) = value else {
            return Err(SqliteError::Domain(
                "SQLite reference field is not stored as BLOB".to_owned(),
            ));
        };
        let object = ObjectId::from_bytes(id16(value, "reference object id")?);
        return Ok(RuntimeValue::Reference { target, object });
    }
    Err(SqliteError::Domain(
        "SQLite server plan value type is not in the supported runtime subset".to_owned(),
    ))
}
async fn ensure_schema(connection: &mut Connection) -> Result<(), SqliteError> {
    connection.execute("PRAGMA foreign_keys = ON", ()).await?;
    connection.execute_batch(SCHEMA).await?;

    let transaction = turso::transaction::Transaction::new(
        connection,
        turso::transaction::TransactionBehavior::Immediate,
    )
    .await?;
    let result = async {
        // Legacy databases predate source_parent_revision_id. Their active source
        // lineage is therefore recoverable only as a NULL parent; backfill validates
        // the stored source content against that identity before creating registries.
        let mut columns = transaction
            .query("PRAGMA table_info(orna_active_revision)", ())
            .await?;
        let mut has_source_parent = false;
        while let Some(column) = columns.next().await? {
            if column.get::<String>(1)? == "source_parent_revision_id" {
                has_source_parent = true;
                break;
            }
        }
        let legacy_active_schema = !has_source_parent;
        drop(columns);
        if legacy_active_schema {
            transaction
                .execute(
                    "ALTER TABLE orna_active_revision ADD COLUMN source_parent_revision_id BLOB",
                    (),
                )
                .await?;
        }
        ensure_catalogue_revision_lineage_schema(&transaction).await?;
        // Source-unit identities are immutable globally, not just within one
        // revision. Creating this index also hardens legacy databases; a
        // duplicate legacy identity fails the transaction and therefore keeps
        // the database unopened rather than silently accepting ambiguous rows.
        transaction
            .execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS
                 orna_source_units_source_unit_id_idx
                 ON orna_source_units (source_unit_id)",
                (),
            )
            .await?;
        // A source bundle is likewise immutable and globally identified.
        transaction
            .execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS
                 orna_source_revisions_source_bundle_id_idx
                 ON orna_source_revisions (source_bundle_id)",
                (),
            )
            .await?;

        backfill_active_identity_registries(&transaction, legacy_active_schema).await?;
        backfill_catalogue_revision_lineage(&transaction).await
    }
    .await;
    match result {
        Ok(()) => {
            transaction.commit().await?;
            Ok(())
        }
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(SqliteError::from(rollback)),
        },
    }
}

async fn ensure_catalogue_revision_lineage_schema(
    connection: &Connection,
) -> Result<(), SqliteError> {
    let mut columns = connection
        .query("PRAGMA table_info(orna_catalogue_revisions)", ())
        .await?;
    let mut has_source_revision = false;
    let mut has_parent_catalogue_revision = false;
    while let Some(column) = columns.next().await? {
        match column.get::<String>(1)?.as_str() {
            "source_revision_id" => has_source_revision = true,
            "parent_catalogue_revision_id" => has_parent_catalogue_revision = true,
            _ => {}
        }
    }
    drop(columns);
    if !has_source_revision {
        connection
            .execute(
                "ALTER TABLE orna_catalogue_revisions
                 ADD COLUMN source_revision_id BLOB",
                (),
            )
            .await?;
    }
    if !has_parent_catalogue_revision {
        connection
            .execute(
                "ALTER TABLE orna_catalogue_revisions
                 ADD COLUMN parent_catalogue_revision_id BLOB",
                (),
            )
            .await?;
    }
    Ok(())
}

async fn backfill_catalogue_revision_lineage(connection: &Connection) -> Result<(), SqliteError> {
    let active = load_active_identity_metadata(connection).await?;
    let mut rows = connection
        .query(
            "SELECT expected_source_revision_id, expected_catalogue_revision_id,
                    candidate_source_revision_id, candidate_catalogue_revision_id
             FROM orna_application_migrations ORDER BY ordinal ASC",
            (),
        )
        .await?;
    let mut edges = Vec::new();
    while let Some(row) = rows.next().await? {
        edges.push((
            SourceRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(0)?,
                "migration expected source revision id",
            )?),
            CatalogueRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(1)?,
                "migration expected catalogue revision id",
            )?),
            SourceRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(2)?,
                "migration candidate source revision id",
            )?),
            CatalogueRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(3)?,
                "migration candidate catalogue revision id",
            )?),
        ));
    }
    drop(rows);

    let mut desired = Vec::with_capacity(edges.len().saturating_add(1));
    if let Some((source, catalogue, _, _)) = edges.first().copied() {
        desired.push((catalogue, source, None));
    } else if let Some(active) = active {
        desired.push((active.catalogue_id, active.source_id, None));
    }
    desired.extend(edges.iter().map(
        |(_, expected_catalogue, candidate_source, candidate_catalogue)| {
            (
                *candidate_catalogue,
                *candidate_source,
                Some(*expected_catalogue),
            )
        },
    ));

    for (catalogue, source, parent) in desired {
        connection
            .execute(
                "UPDATE orna_catalogue_revisions
                 SET source_revision_id = ?1,
                     parent_catalogue_revision_id = ?2
                 WHERE catalogue_revision_id = ?3
                   AND source_revision_id IS NULL",
                [
                    Value::Blob(source.to_bytes().to_vec()),
                    parent.map_or(Value::Null, |parent| {
                        Value::Blob(parent.to_bytes().to_vec())
                    }),
                    Value::Blob(catalogue.to_bytes().to_vec()),
                ],
            )
            .await?;
        let Some(actual) = load_catalogue_revision_lineage(connection, catalogue).await? else {
            return Err(SqliteError::InvalidPersistedData(
                "catalogue revision has no registry record",
            ));
        };
        if actual.source != Some(source) || actual.parent != parent {
            return Err(SqliteError::InvalidPersistedData(
                "catalogue revision lineage does not match migration history",
            ));
        }
    }

    let mut rows = connection
        .query(
            "SELECT COUNT(*) FROM orna_catalogue_revisions
             WHERE source_revision_id IS NULL",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(SqliteError::InvalidPersistedData(
            "catalogue lineage completeness query returned no row",
        ));
    };
    if row.get::<i64>(0)? != 0 {
        return Err(SqliteError::InvalidPersistedData(
            "catalogue revision lineage is incomplete",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceRevisionIdentity {
    parent: Option<SourceRevisionId>,
    bundle: SourceBundleId,
    bundle_hash: Sha256Digest,
    revision_hash: Sha256Digest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CatalogueRevisionIdentity {
    hash: Sha256Digest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CatalogueRevisionLineage {
    source: Option<SourceRevisionId>,
    parent: Option<CatalogueRevisionId>,
}

#[derive(Clone, Copy)]
struct ActiveIdentityMetadata {
    source_id: SourceRevisionId,
    source: SourceRevisionIdentity,
    catalogue_id: CatalogueRevisionId,
    catalogue: CatalogueRevisionIdentity,
}

async fn load_active_identity_metadata(
    connection: &Connection,
) -> Result<Option<ActiveIdentityMetadata>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT source_revision_id, source_parent_revision_id,
                    catalogue_revision_id, source_bundle_id, source_bundle_hash,
                    source_revision_hash, catalogue_hash
             FROM orna_active_revision WHERE singleton = 1",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(ActiveIdentityMetadata {
        source_id: SourceRevisionId::from_bytes(id16(
            row.get::<Vec<u8>>(0)?,
            "source revision id",
        )?),
        source: SourceRevisionIdentity {
            parent: optional_source_revision_id(row.get_value(1)?, "source parent revision id")?,
            bundle: SourceBundleId::from_bytes(id16(row.get::<Vec<u8>>(3)?, "source bundle id")?),
            bundle_hash: digest32(row.get::<Vec<u8>>(4)?, "source bundle hash")?,
            revision_hash: digest32(row.get::<Vec<u8>>(5)?, "source revision hash")?,
        },
        catalogue_id: CatalogueRevisionId::from_bytes(id16(
            row.get::<Vec<u8>>(2)?,
            "catalogue revision id",
        )?),
        catalogue: CatalogueRevisionIdentity {
            hash: digest32(row.get::<Vec<u8>>(6)?, "catalogue hash")?,
        },
    }))
}

async fn load_source_revision_registry(
    connection: &Connection,
    revision: SourceRevisionId,
) -> Result<Option<SourceRevisionIdentity>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT source_parent_revision_id, source_bundle_id,
                    source_bundle_hash, source_revision_hash
             FROM orna_source_revisions
             WHERE source_revision_id = ?1",
            [Value::Blob(revision.to_bytes().to_vec())],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(SourceRevisionIdentity {
        parent: optional_source_revision_id(
            row.get_value(0)?,
            "source revision registry parent revision id",
        )?,
        bundle: SourceBundleId::from_bytes(id16(
            row.get::<Vec<u8>>(1)?,
            "source revision registry bundle id",
        )?),
        bundle_hash: digest32(
            row.get::<Vec<u8>>(2)?,
            "source revision registry bundle hash",
        )?,
        revision_hash: digest32(row.get::<Vec<u8>>(3)?, "source revision registry hash")?,
    }))
}

async fn load_catalogue_revision_registry(
    connection: &Connection,
    revision: CatalogueRevisionId,
) -> Result<Option<CatalogueRevisionIdentity>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT catalogue_hash
             FROM orna_catalogue_revisions
             WHERE catalogue_revision_id = ?1",
            [Value::Blob(revision.to_bytes().to_vec())],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(CatalogueRevisionIdentity {
        hash: digest32(row.get::<Vec<u8>>(0)?, "catalogue revision registry hash")?,
    }))
}

async fn load_catalogue_revision_lineage(
    connection: &Connection,
    revision: CatalogueRevisionId,
) -> Result<Option<CatalogueRevisionLineage>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT source_revision_id, parent_catalogue_revision_id
             FROM orna_catalogue_revisions
             WHERE catalogue_revision_id = ?1",
            [Value::Blob(revision.to_bytes().to_vec())],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(CatalogueRevisionLineage {
        source: optional_source_revision_id(row.get_value(0)?, "catalogue source revision id")?,
        parent: optional_catalogue_revision_id(row.get_value(1)?, "catalogue parent revision id")?,
    }))
}

async fn insert_source_revision_registry(
    connection: &Connection,
    revision: SourceRevisionId,
    identity: SourceRevisionIdentity,
) -> Result<(), SqliteError> {
    let inserted = connection
        .execute(
            "INSERT INTO orna_source_revisions
             (source_revision_id, source_parent_revision_id, source_bundle_id,
              source_bundle_hash, source_revision_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            [
                Value::Blob(revision.to_bytes().to_vec()),
                identity.parent.map_or(Value::Null, |parent| {
                    Value::Blob(parent.to_bytes().to_vec())
                }),
                Value::Blob(identity.bundle.to_bytes().to_vec()),
                Value::Blob(identity.bundle_hash.to_bytes().to_vec()),
                Value::Blob(identity.revision_hash.to_bytes().to_vec()),
            ],
        )
        .await?;
    if inserted != 1 {
        return Err(SqliteError::InvalidPersistedData(
            "source revision registry insert affected an unexpected number of rows",
        ));
    }
    Ok(())
}

async fn insert_catalogue_revision_registry(
    connection: &Connection,
    revision: CatalogueRevisionId,
    source: SourceRevisionId,
    parent: Option<CatalogueRevisionId>,
    identity: CatalogueRevisionIdentity,
) -> Result<(), SqliteError> {
    let inserted = connection
        .execute(
            "INSERT INTO orna_catalogue_revisions
             (catalogue_revision_id, source_revision_id,
              parent_catalogue_revision_id, catalogue_hash)
             VALUES (?1, ?2, ?3, ?4)",
            [
                Value::Blob(revision.to_bytes().to_vec()),
                Value::Blob(source.to_bytes().to_vec()),
                parent.map_or(Value::Null, |parent| {
                    Value::Blob(parent.to_bytes().to_vec())
                }),
                Value::Blob(identity.hash.to_bytes().to_vec()),
            ],
        )
        .await?;
    if inserted != 1 {
        return Err(SqliteError::InvalidPersistedData(
            "catalogue revision registry insert affected an unexpected number of rows",
        ));
    }
    Ok(())
}

async fn infer_catalogue_parent(
    connection: &Connection,
    catalogue: CatalogueRevisionId,
) -> Result<Option<CatalogueRevisionId>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT expected_catalogue_revision_id
             FROM orna_application_migrations
             WHERE candidate_catalogue_revision_id = ?1
             ORDER BY ordinal ASC LIMIT 1",
            [Value::Blob(catalogue.to_bytes().to_vec())],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(CatalogueRevisionId::from_bytes(id16(
        row.get::<Vec<u8>>(0)?,
        "migration expected catalogue revision id",
    )?)))
}

async fn backfill_active_identity_registries(
    connection: &Connection,
    legacy_active_schema: bool,
) -> Result<(), SqliteError> {
    let Some(active) = load_active_identity_metadata(connection).await? else {
        return Ok(());
    };

    if legacy_active_schema {
        validate_legacy_active_source(connection, active).await?;
    }
    match load_source_revision_registry(connection, active.source_id).await? {
        Some(existing) if existing != active.source => {
            return Err(SqliteError::InvalidPersistedData(
                "source revision registry conflicts with active metadata",
            ));
        }
        Some(_) => {}
        None => {
            insert_source_revision_registry(connection, active.source_id, active.source).await?;
        }
    }

    match load_catalogue_revision_registry(connection, active.catalogue_id).await? {
        Some(existing) if existing != active.catalogue => {
            return Err(SqliteError::InvalidPersistedData(
                "catalogue revision registry conflicts with active metadata",
            ));
        }
        Some(_) => {}
        None => {
            let parent = infer_catalogue_parent(connection, active.catalogue_id).await?;
            insert_catalogue_revision_registry(
                connection,
                active.catalogue_id,
                active.source_id,
                parent,
                active.catalogue,
            )
            .await?;
        }
    }
    Ok(())
}

async fn validate_legacy_active_source(
    connection: &Connection,
    active: ActiveIdentityMetadata,
) -> Result<(), SqliteError> {
    let mut rows = connection
        .query(
            "SELECT source_unit_id, ordinal, logical_path, content, content_hash
             FROM orna_source_units
             WHERE source_revision_id = ?1 ORDER BY ordinal ASC",
            [Value::Blob(active.source_id.to_bytes().to_vec())],
        )
        .await?;
    let mut units = Vec::new();
    while let Some(unit) = rows.next().await? {
        units.push(
            StoredSourceUnit::new(
                SourceUnitId::from_bytes(id16(unit.get::<Vec<u8>>(0)?, "source unit id")?),
                u32::try_from(unit.get::<i64>(1)?).map_err(|_| {
                    SqliteError::InvalidPersistedData("source unit ordinal must fit u32")
                })?,
                unit.get::<String>(2)?,
                unit.get::<String>(3)?,
                digest32(unit.get::<Vec<u8>>(4)?, "source unit hash")?,
            )
            .map_err(|_| {
                SqliteError::InvalidPersistedData(
                    "legacy active source content/hash is not valid for a NULL parent",
                )
            })?,
        );
    }

    let computed_bundle_hash = source_bundle_digest(&units).map_err(|_| {
        SqliteError::InvalidPersistedData(
            "legacy active source content/hash is not valid for a NULL parent",
        )
    })?;
    if computed_bundle_hash != active.source.bundle_hash {
        return Err(SqliteError::InvalidPersistedData(
            "legacy active source content/hash is not valid for a NULL parent",
        ));
    }

    let computed_source_hash =
        source_revision_record_digest(active.source.bundle, None, active.source.bundle_hash)
            .map_err(|_| {
                SqliteError::InvalidPersistedData(
                    "legacy active source content/hash is not valid for a NULL parent",
                )
            })?;
    if computed_source_hash != active.source.revision_hash {
        return Err(SqliteError::InvalidPersistedData(
            "legacy active source content/hash is not valid for a NULL parent",
        ));
    }
    Ok(())
}

async fn validate_active_identity_registries(
    connection: &Connection,
    active: ActiveIdentityMetadata,
) -> Result<(), SqliteError> {
    let source = load_source_revision_registry(connection, active.source_id)
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "active source revision has no registry record",
        ))?;
    if source != active.source {
        return Err(SqliteError::InvalidPersistedData(
            "active source revision registry does not match active metadata",
        ));
    }
    if let Some(parent) = active.source.parent
        && load_source_revision_registry(connection, parent)
            .await?
            .is_none()
    {
        return Err(SqliteError::InvalidPersistedData(
            "active source parent revision has no registry record",
        ));
    }

    let catalogue = load_catalogue_revision_registry(connection, active.catalogue_id)
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "active catalogue revision has no registry record",
        ))?;
    if catalogue != active.catalogue {
        return Err(SqliteError::InvalidPersistedData(
            "active catalogue revision registry does not match active metadata",
        ));
    }
    Ok(())
}

async fn require_source_revision_registry(
    connection: &Connection,
    revision: SourceRevisionId,
) -> Result<(), SqliteError> {
    if load_source_revision_registry(connection, revision)
        .await?
        .is_none()
    {
        return Err(SqliteError::InvalidPersistedData(
            "candidate source parent revision is not registered",
        ));
    }
    Ok(())
}

async fn require_catalogue_revision_registry(
    connection: &Connection,
    revision: CatalogueRevisionId,
) -> Result<(), SqliteError> {
    if load_catalogue_revision_registry(connection, revision)
        .await?
        .is_none()
    {
        return Err(SqliteError::InvalidPersistedData(
            "candidate catalogue parent revision is not registered",
        ));
    }
    Ok(())
}

async fn seed_pair_in_transaction(
    transaction: &turso::transaction::Transaction<'_>,
) -> Result<BootstrapRevision, SqliteError> {
    if load_active_identity_metadata(transaction).await?.is_some() {
        let active = load_active_from(transaction).await?;
        let ledger = load_ledger_from(transaction).await?;
        validate_ledger_active_pair(&ledger, &active)?;
        validate_active_catalogue_lineage(transaction, &active, &ledger).await?;
        return Ok(BootstrapRevision::new(
            active.pair().source(),
            active.pair().catalogue(),
        ));
    }

    let durable_rows = {
        let mut rows = transaction
            .query(
                "SELECT
                    (SELECT COUNT(*) FROM orna_source_revisions),
                    (SELECT COUNT(*) FROM orna_catalogue_revisions),
                    (SELECT COUNT(*) FROM orna_source_units),
                    (SELECT COUNT(*) FROM orna_catalogue_schemas),
                    (SELECT COUNT(*) FROM orna_application_migrations)",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(SqliteError::InvalidPersistedData(
                "durable row count query returned no row",
            ));
        };
        let has_rows = row.get::<i64>(0)? != 0
            || row.get::<i64>(1)? != 0
            || row.get::<i64>(2)? != 0
            || row.get::<i64>(3)? != 0
            || row.get::<i64>(4)? != 0;
        drop(rows);
        has_rows
    };
    if durable_rows {
        return Err(SqliteError::InvalidPersistedData(
            "durable revisions exist without an active revision",
        ));
    }

    let pair = BootstrapRevision::new(SourceRevisionId::new(), CatalogueRevisionId::new());
    let bundle = SourceBundleId::new();
    let source_bundle_hash =
        source_bundle_digest(&[]).map_err(|error| SqliteError::Domain(error.to_string()))?;
    let source_hash = source_revision_record_digest(bundle, None, source_bundle_hash)
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    let catalogue = CatalogueSnapshot::new(pair.catalogue(), Vec::new(), Vec::new())
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[])
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    insert_source_revision_registry(
        transaction,
        pair.source(),
        SourceRevisionIdentity {
            parent: None,
            bundle,
            bundle_hash: source_bundle_hash,
            revision_hash: source_hash,
        },
    )
    .await?;
    insert_catalogue_revision_registry(
        transaction,
        pair.catalogue(),
        pair.source(),
        None,
        CatalogueRevisionIdentity {
            hash: catalogue_hash,
        },
    )
    .await?;
    let inserted = transaction
        .execute(
            "INSERT INTO orna_active_revision
             (singleton, source_revision_id, source_parent_revision_id,
              catalogue_revision_id, source_bundle_id, source_bundle_hash,
              source_revision_hash, catalogue_hash)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            [
                Value::Blob(pair.source().to_bytes().to_vec()),
                Value::Null,
                Value::Blob(pair.catalogue().to_bytes().to_vec()),
                Value::Blob(bundle.to_bytes().to_vec()),
                Value::Blob(source_bundle_hash.to_bytes().to_vec()),
                Value::Blob(source_hash.to_bytes().to_vec()),
                Value::Blob(catalogue_hash.to_bytes().to_vec()),
            ],
        )
        .await?;
    if inserted != 1 {
        return Err(SqliteError::InvalidPersistedData(
            "bootstrap active row insert affected an unexpected number of rows",
        ));
    }
    load_pair_from(transaction)
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "bootstrap row disappeared",
        ))
}

async fn load_pair_from(connection: &Connection) -> Result<Option<BootstrapRevision>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT source_revision_id, catalogue_revision_id
             FROM orna_active_revision WHERE singleton = 1",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let source = SourceRevisionId::from_bytes(id16(row.get::<Vec<u8>>(0)?, "source revision id")?);
    let catalogue =
        CatalogueRevisionId::from_bytes(id16(row.get::<Vec<u8>>(1)?, "catalogue revision id")?);
    Ok(Some(BootstrapRevision::new(source, catalogue)))
}
async fn load_source_revision_from(
    connection: &Connection,
    revision: SourceRevisionId,
    identity: SourceRevisionIdentity,
) -> Result<StoredSourceRevision, SqliteError> {
    let mut unit_rows = connection
        .query(
            "SELECT source_unit_id, ordinal, logical_path, content, content_hash
             FROM orna_source_units
             WHERE source_revision_id = ?1 ORDER BY ordinal ASC",
            [Value::Blob(revision.to_bytes().to_vec())],
        )
        .await?;
    let mut units = Vec::new();
    while let Some(unit) = unit_rows.next().await? {
        units.push(
            StoredSourceUnit::new(
                SourceUnitId::from_bytes(id16(unit.get::<Vec<u8>>(0)?, "source unit id")?),
                u32::try_from(unit.get::<i64>(1)?).map_err(|_| {
                    SqliteError::InvalidPersistedData("source unit ordinal must fit u32")
                })?,
                unit.get::<String>(2)?,
                unit.get::<String>(3)?,
                digest32(unit.get::<Vec<u8>>(4)?, "source unit hash")?,
            )
            .map_err(|error| SqliteError::Domain(error.to_string()))?,
        );
    }
    drop(unit_rows);

    let computed_bundle_hash =
        source_bundle_digest(&units).map_err(|error| SqliteError::Domain(error.to_string()))?;
    if computed_bundle_hash != identity.bundle_hash {
        return Err(SqliteError::InvalidPersistedData(
            "source bundle hash mismatch",
        ));
    }
    let computed_source_hash =
        source_revision_record_digest(identity.bundle, identity.parent, identity.bundle_hash)
            .map_err(|error| SqliteError::Domain(error.to_string()))?;
    if computed_source_hash != identity.revision_hash {
        return Err(SqliteError::InvalidPersistedData(
            "source revision hash mismatch",
        ));
    }

    StoredSourceRevision::new(
        identity.bundle,
        revision,
        identity.parent,
        units,
        identity.bundle_hash,
        identity.revision_hash,
    )
    .map_err(|error| SqliteError::Domain(error.to_string()))
}

async fn load_persisted_active(
    connection: &Connection,
    metadata: ActiveIdentityMetadata,
) -> Result<Option<ActiveDatabaseRevision>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT payload FROM orna_revision_snapshots
             WHERE source_revision_id = ?1 AND catalogue_revision_id = ?2",
            [
                Value::Blob(metadata.source_id.to_bytes().to_vec()),
                Value::Blob(metadata.catalogue_id.to_bytes().to_vec()),
            ],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let payload = row.get::<Vec<u8>>(0)?;
    drop(rows);
    let persisted = serde_json::from_slice::<PersistedActiveRevision>(&payload)
        .map_err(|error| SqliteError::Domain(format!("invalid revision snapshot: {error}")))?;
    let active = persisted.into_active()?;
    let durable_source =
        load_source_revision_from(connection, metadata.source_id, metadata.source).await?;
    if active.source() != &durable_source {
        return Err(SqliteError::InvalidPersistedData(
            "revision snapshot source does not match durable source revision",
        ));
    }
    if active.pair() != RevisionPair::new(metadata.source_id, metadata.catalogue_id)
        || active.source().bundle() != metadata.source.bundle
        || active.source().parent() != metadata.source.parent
        || active.source().bundle_hash() != metadata.source.bundle_hash
        || active.source().revision_hash() != metadata.source.revision_hash
        || active.catalogue_hash() != metadata.catalogue.hash
    {
        return Err(SqliteError::InvalidPersistedData(
            "revision snapshot does not match active identity metadata",
        ));
    }
    Ok(Some(active))
}

async fn load_active_from(connection: &Connection) -> Result<ActiveDatabaseRevision, SqliteError> {
    let active = load_active_identity_metadata(connection).await?.ok_or(
        SqliteError::InvalidPersistedData("database has not been bootstrapped"),
    )?;

    if let Some(revision) = load_persisted_active(connection, active).await? {
        validate_active_identity_registries(connection, active).await?;
        return Ok(revision);
    }

    let source_id = active.source_id;
    let catalogue_id = active.catalogue_id;
    let catalogue_hash = active.catalogue.hash;
    let source = load_source_revision_from(connection, source_id, active.source).await?;
    let mut schema_rows = connection
        .query(
            "SELECT schema_id, name_parts, source_unit_id, source_start, source_end
             FROM orna_catalogue_schemas
             WHERE catalogue_revision_id = ?1 ORDER BY rowid ASC",
            [Value::Blob(catalogue_id.to_bytes().to_vec())],
        )
        .await?;
    let mut schemas = Vec::new();
    let mut origins = Vec::new();
    while let Some(schema) = schema_rows.next().await? {
        let schema_id = SchemaId::from_bytes(id16(schema.get::<Vec<u8>>(0)?, "schema id")?);
        let encoded_name = schema.get::<String>(1)?;
        let name = decode_qualified_semantic_name(&encoded_name)?;
        schemas.push(SchemaDefinition::new(schema_id, name));
        let source_origin = SourceOrigin::new(
            SourceUnitId::from_bytes(id16(schema.get::<Vec<u8>>(2)?, "schema source unit id")?),
            u32::try_from(schema.get::<i64>(3)?).map_err(|_| {
                SqliteError::InvalidPersistedData("schema source start must fit u32")
            })?,
            u32::try_from(schema.get::<i64>(4)?)
                .map_err(|_| SqliteError::InvalidPersistedData("schema source end must fit u32"))?,
        )
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema_id),
            source_origin,
        ));
    }
    let catalogue = CatalogueSnapshot::new(catalogue_id, schemas, Vec::new())
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    let computed_catalogue_hash = catalogue_digest(&catalogue, &[], &[], &origins, &[])
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    if computed_catalogue_hash != catalogue_hash {
        return Err(SqliteError::InvalidPersistedData("catalogue hash mismatch"));
    }
    validate_active_identity_registries(connection, active).await?;

    ActiveDatabaseRevision::new(
        RevisionPair::new(source_id, catalogue_id),
        source,
        catalogue,
        catalogue_hash,
        Vec::new(),
        Vec::new(),
        origins,
        Vec::new(),
    )
    .map_err(|error| SqliteError::Domain(error.to_string()))
}

async fn load_ledger_from(
    connection: &Connection,
) -> Result<Vec<MigrationLedgerEntry>, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT ordinal, format, version,
                    expected_source_revision_id, expected_catalogue_revision_id,
                    candidate_source_revision_id, candidate_catalogue_revision_id,
                    canonical_bytes, digest
             FROM orna_application_migrations ORDER BY ordinal ASC",
            (),
        )
        .await?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next().await? {
        let ordinal = row.get::<i64>(0)?;
        if ordinal < 0 {
            return Err(SqliteError::InvalidPersistedData(
                "migration ledger ordinal must be non-negative",
            ));
        }
        let expected_ordinal = i64::try_from(entries.len())
            .map_err(|_| SqliteError::InvalidPersistedData("migration ledger ordinal overflow"))?;
        if ordinal != expected_ordinal {
            return Err(SqliteError::Domain(format!(
                "invalid migration ledger ordinal {ordinal}: expected {expected_ordinal}",
            )));
        }
        let version = u32::try_from(row.get::<i64>(2)?).map_err(|_| {
            SqliteError::InvalidPersistedData("migration ledger version must fit u32")
        })?;
        let expected_base = RevisionPair::new(
            SourceRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(3)?,
                "migration expected source revision id",
            )?),
            CatalogueRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(4)?,
                "migration expected catalogue revision id",
            )?),
        );
        let candidate_pair = RevisionPair::new(
            SourceRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(5)?,
                "migration candidate source revision id",
            )?),
            CatalogueRevisionId::from_bytes(id16(
                row.get::<Vec<u8>>(6)?,
                "migration candidate catalogue revision id",
            )?),
        );
        let entry = MigrationLedgerEntry::from_parts(
            row.get::<String>(1)?,
            version,
            expected_base,
            candidate_pair,
            row.get::<Vec<u8>>(7)?,
            digest32(row.get::<Vec<u8>>(8)?, "migration digest")?,
        )
        .map_err(|error| {
            SqliteError::Domain(format!(
                "invalid migration ledger entry at ordinal {ordinal}: {error}"
            ))
        })?;
        entries.push(entry);
    }
    drop(rows);
    validate_loaded_ledger_chain(connection, &entries).await?;
    Ok(entries)
}

fn validate_ledger_active_pair(
    ledger: &[MigrationLedgerEntry],
    active: &ActiveDatabaseRevision,
) -> Result<(), SqliteError> {
    if let Some(last) = ledger.last()
        && last.candidate_pair() != active.pair()
    {
        return Err(SqliteError::InvalidPersistedData(
            "migration ledger does not end at active revision",
        ));
    }
    Ok(())
}

async fn validate_active_catalogue_lineage(
    connection: &Connection,
    active: &ActiveDatabaseRevision,
    ledger: &[MigrationLedgerEntry],
) -> Result<(), SqliteError> {
    let lineage = load_catalogue_revision_lineage(connection, active.pair().catalogue())
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "active catalogue revision has no lineage record",
        ))?;
    let expected_parent = ledger.last().map(|entry| entry.expected_base().catalogue());
    if lineage.source != Some(active.pair().source()) {
        return Err(SqliteError::InvalidPersistedData(
            "active catalogue revision source does not match active source",
        ));
    }
    if lineage.parent != expected_parent {
        return Err(SqliteError::InvalidPersistedData(
            "active catalogue revision parent does not match migration ledger",
        ));
    }
    Ok(())
}

async fn validate_catalogue_revision(
    connection: &Connection,
    revision: CatalogueRevisionId,
    source: &StoredSourceRevision,
    expected_parent: Option<CatalogueRevisionId>,
) -> Result<(), SqliteError> {
    let registry = load_catalogue_revision_registry(connection, revision)
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "catalogue revision has no registry record",
        ))?;
    let lineage = load_catalogue_revision_lineage(connection, revision)
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "catalogue revision has no lineage record",
        ))?;
    if lineage.source != Some(source.id()) {
        return Err(SqliteError::InvalidPersistedData(
            "catalogue revision source does not match its source revision",
        ));
    }
    if lineage.parent != expected_parent {
        return Err(SqliteError::InvalidPersistedData(
            "catalogue revision parent does not match migration history",
        ));
    }
    let mut snapshot_rows = connection
        .query(
            "SELECT payload FROM orna_revision_snapshots
             WHERE source_revision_id = ?1 AND catalogue_revision_id = ?2",
            [
                Value::Blob(source.id().to_bytes().to_vec()),
                Value::Blob(revision.to_bytes().to_vec()),
            ],
        )
        .await?;
    let snapshot_catalogue = if let Some(snapshot_row) = snapshot_rows.next().await? {
        let payload = snapshot_row.get::<Vec<u8>>(0)?;
        let persisted = serde_json::from_slice::<PersistedActiveRevision>(&payload)
            .map_err(|error| SqliteError::Domain(format!("invalid revision snapshot: {error}")))?;
        let snapshot_active = persisted.into_active()?;
        if snapshot_active.source() != source
            || snapshot_active.pair().catalogue() != revision
            || snapshot_active.catalogue_hash() != registry.hash
        {
            return Err(SqliteError::InvalidPersistedData(
                "historical catalogue hash mismatch",
            ));
        }
        Some(snapshot_active.catalogue().clone())
    } else {
        None
    };
    drop(snapshot_rows);

    let mut rows = connection
        .query(
            "SELECT schema_id, name_parts, source_unit_id, source_start, source_end
             FROM orna_catalogue_schemas
             WHERE catalogue_revision_id = ?1 ORDER BY rowid ASC",
            [Value::Blob(revision.to_bytes().to_vec())],
        )
        .await?;
    let mut schemas = Vec::new();
    let mut origins = Vec::new();
    while let Some(row) = rows.next().await? {
        let schema_id = SchemaId::from_bytes(id16(row.get::<Vec<u8>>(0)?, "schema id")?);
        let name = decode_qualified_semantic_name(&row.get::<String>(1)?)?;
        let source_origin = SourceOrigin::new(
            SourceUnitId::from_bytes(id16(row.get::<Vec<u8>>(2)?, "schema source unit id")?),
            u32::try_from(row.get::<i64>(3)?).map_err(|_| {
                SqliteError::InvalidPersistedData("schema source start must fit u32")
            })?,
            u32::try_from(row.get::<i64>(4)?)
                .map_err(|_| SqliteError::InvalidPersistedData("schema source end must fit u32"))?,
        )
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
        if !source
            .units()
            .iter()
            .any(|unit| unit.id() == source_origin.source_unit())
        {
            return Err(SqliteError::InvalidPersistedData(
                "catalogue schema source unit is not in its source revision",
            ));
        }
        schemas.push(SchemaDefinition::new(schema_id, name));
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema_id),
            source_origin,
        ));
    }
    let catalogue = CatalogueSnapshot::new(revision, schemas, Vec::new())
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    if let Some(snapshot_catalogue) = snapshot_catalogue {
        if snapshot_catalogue.schemas() != catalogue.schemas() {
            return Err(SqliteError::InvalidPersistedData(
                "historical catalogue hash mismatch",
            ));
        }
        return Ok(());
    }
    let computed_hash = catalogue_digest(&catalogue, &[], &[], &origins, &[])
        .map_err(|error| SqliteError::Domain(error.to_string()))?;
    if computed_hash != registry.hash {
        return Err(SqliteError::InvalidPersistedData(
            "historical catalogue hash mismatch",
        ));
    }
    Ok(())
}

async fn validate_loaded_ledger_chain(
    connection: &Connection,
    entries: &[MigrationLedgerEntry],
) -> Result<(), SqliteError> {
    let Some(first) = entries.first() else {
        if let Some(active) = load_active_identity_metadata(connection).await?
            && active.source.parent.is_some()
        {
            return Err(SqliteError::InvalidPersistedData(
                "migration ledger is empty but active source revision has a parent",
            ));
        }
        return Ok(());
    };

    let first_source = load_source_revision_registry(connection, first.expected_base().source())
        .await?
        .ok_or(SqliteError::InvalidPersistedData(
            "migration ledger first expected source revision has no registry record",
        ))?;
    if first_source.parent.is_some() {
        return Err(SqliteError::InvalidPersistedData(
            "migration ledger first expected source revision is not a root source",
        ));
    }
    let first_source_revision =
        load_source_revision_from(connection, first.expected_base().source(), first_source).await?;
    validate_catalogue_revision(
        connection,
        first.expected_base().catalogue(),
        &first_source_revision,
        None,
    )
    .await?;

    for (ordinal, entry) in entries.iter().enumerate() {
        if ordinal > 0 {
            let previous = &entries[ordinal - 1];
            if entry.expected_base() != previous.candidate_pair() {
                return Err(SqliteError::Domain(format!(
                    "invalid migration ledger chain at ordinal {ordinal}: \
                     expected base does not match previous candidate pair",
                )));
            }

            let expected_source =
                load_source_revision_registry(connection, entry.expected_base().source())
                    .await?
                    .ok_or(SqliteError::InvalidPersistedData(
                        "migration ledger expected source revision has no registry record",
                    ))?;
            load_source_revision_from(connection, entry.expected_base().source(), expected_source)
                .await?;
        }

        let candidate_source =
            load_source_revision_registry(connection, entry.candidate_pair().source())
                .await?
                .ok_or(SqliteError::InvalidPersistedData(
                    "migration ledger candidate source revision has no registry record",
                ))?;
        if candidate_source.parent != Some(entry.expected_base().source()) {
            return Err(SqliteError::InvalidPersistedData(
                "migration ledger candidate source parent does not match expected source revision",
            ));
        }
        let candidate_source_revision = load_source_revision_from(
            connection,
            entry.candidate_pair().source(),
            candidate_source,
        )
        .await?;
        validate_catalogue_revision(
            connection,
            entry.candidate_pair().catalogue(),
            &candidate_source_revision,
            Some(entry.expected_base().catalogue()),
        )
        .await?;
    }

    Ok(())
}

async fn next_ledger_ordinal(connection: &Connection) -> Result<i64, SqliteError> {
    let mut rows = connection
        .query(
            "SELECT COALESCE(MAX(ordinal), -1) FROM orna_application_migrations",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(SqliteError::InvalidPersistedData(
            "migration ledger ordinal query returned no row",
        ));
    };
    let max = row.get::<i64>(0)?;
    if max < -1 {
        return Err(SqliteError::InvalidPersistedData(
            "migration ledger ordinal must be non-negative",
        ));
    }
    max.checked_add(1).ok_or(SqliteError::InvalidPersistedData(
        "migration ledger ordinal overflow",
    ))
}

async fn apply_in_transaction(
    transaction: &turso::transaction::Transaction<'_>,
    candidate: &DeployableRevision,
    artifact: &PhysicalMigrationArtifact,
) -> Result<(), StorageError<SqliteError>> {
    let active = load_active_from(transaction)
        .await
        .map_err(StorageError::Backend)?;
    if candidate.expected_base() != active.pair() {
        return Err(StorageError::InvalidRequest(
            MigrationLedgerEntryError::ActiveBaseMismatch {
                expected: candidate.expected_base(),
                actual: active.pair(),
            },
        ));
    }

    let entry = MigrationLedgerEntry::from_artifact(artifact);
    entry
        .validate(&active, candidate)
        .map_err(StorageError::InvalidRequest)?;

    if let Err(error) = ensure_supported_candidate(candidate) {
        return Err(StorageError::Backend(error));
    }
    validate_candidate_records(candidate).map_err(StorageError::InvalidRequest)?;
    validate_candidate_parent_registries(transaction, candidate, active.pair())
        .await
        .map_err(StorageError::Backend)?;

    let ledger = load_ledger_from(transaction)
        .await
        .map_err(StorageError::Backend)?;
    validate_ledger_active_pair(&ledger, &active).map_err(StorageError::Backend)?;
    validate_active_catalogue_lineage(transaction, &active, &ledger)
        .await
        .map_err(StorageError::Backend)?;
    let ordinal = next_ledger_ordinal(transaction)
        .await
        .map_err(StorageError::Backend)?;
    install_physical_artifact(transaction, artifact)
        .await
        .map_err(StorageError::Backend)?;
    persist_candidate(&active, transaction, candidate, &entry, ordinal)
        .await
        .map_err(StorageError::Backend)
}
async fn install_physical_artifact(
    transaction: &turso::transaction::Transaction<'_>,
    artifact: &PhysicalMigrationArtifact,
) -> Result<(), SqliteError> {
    for operation in artifact.operations() {
        let statement = match operation {
            PhysicalOperation::CreateObject(object) => create_object_statement(object)?,
            PhysicalOperation::AddField(add_field) => add_field_statement(add_field)?,
        };
        transaction.execute(&statement, ()).await?;
        if let PhysicalOperation::AddField(add_field) = operation
            && add_field.field().unique()
        {
            let index_name = format!(
                "orna_unique_{}_{}",
                hex_bytes(add_field.object_type().to_bytes()),
                hex_bytes(add_field.field().field_id().to_bytes()),
            );
            let statement = format!(
                "CREATE UNIQUE INDEX {} ON {} ({})",
                index_name,
                object_table_name(add_field.object_type()),
                field_name(add_field.field().field_id()),
            );
            transaction.execute(&statement, ()).await?;
        }
    }
    Ok(())
}

fn create_object_statement(object: &CreateObject) -> Result<String, SqliteError> {
    let mut definitions = vec![format!("{} BLOB NOT NULL PRIMARY KEY", object_id_column())];
    for field in object.fields() {
        definitions.push(field_definition(field)?);
        if field.unique() {
            definitions.push(format!("UNIQUE ({})", field_name(field.field_id())));
        }
        if let PhysicalFieldType::Reference { target, on_delete } = field.field_type() {
            definitions.push(format!(
                "FOREIGN KEY ({}) REFERENCES {} ({}) ON DELETE {}",
                field_name(field.field_id()),
                object_table_name(target),
                object_id_column(),
                sqlite_on_delete(on_delete),
            ));
        }
    }
    Ok(format!(
        "CREATE TABLE {} ({})",
        object_table_name(object.type_id()),
        definitions.join(", "),
    ))
}

fn add_field_statement(add_field: &AddField) -> Result<String, SqliteError> {
    let mut statement = format!(
        "ALTER TABLE {} ADD COLUMN {}",
        object_table_name(add_field.object_type()),
        field_definition(add_field.field())?,
    );
    if let PhysicalFieldType::Reference { target, on_delete } = add_field.field().field_type() {
        statement.push_str(&format!(
            " REFERENCES {} ({}) ON DELETE {}",
            object_table_name(target),
            object_id_column(),
            sqlite_on_delete(on_delete),
        ));
    }
    Ok(statement)
}

fn field_definition(field: &CreateField) -> Result<String, SqliteError> {
    let storage_type = match field.field_type() {
        PhysicalFieldType::Scalar(scalar) => sqlite_scalar_type(scalar)?,
        PhysicalFieldType::Enum(_) => "TEXT",
        PhysicalFieldType::Record(_) | PhysicalFieldType::Reference { .. } => "BLOB",
    };
    let nullability = if field.nullable() { "" } else { " NOT NULL" };
    Ok(format!(
        "{} {}{}",
        field_name(field.field_id()),
        storage_type,
        nullability,
    ))
}

fn sqlite_scalar_type(scalar: StandardScalar) -> Result<&'static str, SqliteError> {
    match scalar {
        StandardScalar::Boolean | StandardScalar::Integer | StandardScalar::BigInt => Ok("INTEGER"),
        StandardScalar::Float => Ok("REAL"),
        StandardScalar::CharacterLargeObject => Ok("TEXT"),
        StandardScalar::BinaryLargeObject => Ok("BLOB"),
        StandardScalar::Void => Err(SqliteError::Domain(
            "SQLite object fields cannot use VOID".to_owned(),
        )),
        StandardScalar::Decimal
        | StandardScalar::Uuid
        | StandardScalar::Date
        | StandardScalar::Time
        | StandardScalar::Timestamp
        | StandardScalar::Duration => Err(SqliteError::UnsupportedCapability(
            SqliteCapability::ScalarType,
        )),
    }
}

fn object_id_column() -> &'static str {
    "_orna_object_id"
}

fn object_table_name(type_id: orna_core::TypeId) -> String {
    format!("orna_object_{}", hex_bytes(type_id.to_bytes()))
}

fn field_name(field_id: orna_core::FieldId) -> String {
    format!("f_{}", hex_bytes(field_id.to_bytes()))
}

fn hex_bytes<const N: usize>(bytes: [u8; N]) -> String {
    let mut encoded = String::with_capacity(N * 2);
    for byte in bytes {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn sqlite_on_delete(action: Option<orna_core::catalogue::OnDeleteAction>) -> &'static str {
    match action {
        None => "NO ACTION",
        Some(orna_core::catalogue::OnDeleteAction::Restrict) => "RESTRICT",
        Some(orna_core::catalogue::OnDeleteAction::SetNull) => "SET NULL",
        Some(orna_core::catalogue::OnDeleteAction::Cascade) => "CASCADE",
    }
}

fn ensure_supported_candidate(candidate: &DeployableRevision) -> Result<(), SqliteError> {
    let catalogue = candidate.candidate();

    if !catalogue.value_types().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::ValueType,
        ));
    }
    if !catalogue.enum_types().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::EnumType,
        ));
    }
    if !catalogue.record_value_types().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::RecordValueType,
        ));
    }
    if !catalogue.type_bindings().is_empty() {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::TypeBinding,
        ));
    }

    if catalogue.object_types().iter().any(|object| {
        object.fields().iter().any(|field| {
            matches!(
                field.resolved_type().legacy_scalar(),
                Some(scalar)
                    if !matches!(
                        scalar,
                        StandardScalar::Boolean
                            | StandardScalar::Integer
                            | StandardScalar::BigInt
                            | StandardScalar::Float
                            | StandardScalar::CharacterLargeObject
                            | StandardScalar::BinaryLargeObject
                    )
            )
        })
    }) {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::ScalarType,
        ));
    }
    if candidate.catalogue_hash_context().version() != CatalogueHashVersion::Version1 {
        return Err(SqliteError::UnsupportedCapability(
            SqliteCapability::CatalogueHashVersion,
        ));
    }
    Ok(())
}

fn validate_candidate_records(
    candidate: &DeployableRevision,
) -> Result<(), MigrationLedgerEntryError> {
    let source = candidate.source();
    let expected_bundle_hash =
        source_bundle_digest(source.units()).map_err(MigrationLedgerEntryError::CanonicalHash)?;
    if expected_bundle_hash != source.bundle_hash() {
        return Err(MigrationLedgerEntryError::DigestMismatch {
            expected: expected_bundle_hash,
            actual: source.bundle_hash(),
        });
    }
    let expected_source_hash =
        source_revision_record_digest(source.bundle(), source.parent(), source.bundle_hash())
            .map_err(MigrationLedgerEntryError::CanonicalHash)?;
    if expected_source_hash != source.revision_hash() {
        return Err(MigrationLedgerEntryError::DigestMismatch {
            expected: expected_source_hash,
            actual: source.revision_hash(),
        });
    }
    let function_revisions = candidate
        .current_function_revisions()
        .unwrap_or_else(|| candidate.new_function_revisions());
    let expected_catalogue_hash = catalogue_digest(
        candidate.candidate(),
        function_revisions,
        candidate.expressions(),
        candidate.origins(),
        candidate.references(),
    )
    .map_err(MigrationLedgerEntryError::CanonicalHash)?;
    if expected_catalogue_hash != candidate.catalogue_hash() {
        return Err(MigrationLedgerEntryError::DigestMismatch {
            expected: expected_catalogue_hash,
            actual: candidate.catalogue_hash(),
        });
    }
    Ok(())
}

async fn validate_candidate_parent_registries(
    connection: &Connection,
    candidate: &DeployableRevision,
    active: RevisionPair,
) -> Result<(), SqliteError> {
    if candidate.source().parent() != Some(active.source()) {
        return Err(SqliteError::InvalidPersistedData(
            "candidate source parent does not match active source revision",
        ));
    }
    require_source_revision_registry(connection, active.source()).await?;

    if candidate.parent_catalogue() != active.catalogue() {
        return Err(SqliteError::InvalidPersistedData(
            "candidate catalogue parent does not match active catalogue revision",
        ));
    }
    require_catalogue_revision_registry(connection, active.catalogue()).await?;
    Ok(())
}

fn build_persisted_active(
    active: &ActiveDatabaseRevision,
    candidate: &DeployableRevision,
) -> Result<PersistedActiveRevision, SqliteError> {
    let function_revisions = candidate.current_function_revisions().map_or_else(
        || candidate.new_function_revisions().to_vec(),
        |revisions| revisions.to_vec(),
    );
    let current_ids = function_revisions
        .iter()
        .map(FunctionRevisionRecord::id)
        .collect::<Vec<_>>();
    let historical_function_revisions = active
        .function_revisions()
        .iter()
        .chain(active.historical_function_revisions())
        .filter(|revision| !current_ids.contains(&revision.id()))
        .cloned()
        .collect();
    let next = ActiveDatabaseRevision::new_with_history(
        candidate.candidate_pair(),
        candidate.source().clone(),
        candidate.candidate().clone(),
        candidate.catalogue_hash(),
        candidate.expressions().to_vec(),
        function_revisions,
        historical_function_revisions,
        candidate.origins().to_vec(),
        candidate.references().to_vec(),
    )
    .map_err(|error| SqliteError::Domain(error.to_string()))?;
    Ok(PersistedActiveRevision::from_active(&next))
}

async fn persist_candidate(
    active: &ActiveDatabaseRevision,
    transaction: &turso::transaction::Transaction<'_>,
    candidate: &DeployableRevision,
    entry: &MigrationLedgerEntry,
    ordinal: i64,
) -> Result<(), SqliteError> {
    let source = candidate.source();
    let catalogue_pair = candidate.candidate_pair();
    let snapshot_payload = serde_json::to_vec(&build_persisted_active(active, candidate)?)
        .map_err(|error| {
            SqliteError::Domain(format!("revision snapshot encoding failed: {error}"))
        })?;
    insert_source_revision_registry(
        transaction,
        source.id(),
        SourceRevisionIdentity {
            parent: source.parent(),
            bundle: source.bundle(),
            bundle_hash: source.bundle_hash(),
            revision_hash: source.revision_hash(),
        },
    )
    .await?;
    insert_catalogue_revision_registry(
        transaction,
        catalogue_pair.catalogue(),
        source.id(),
        Some(candidate.parent_catalogue()),
        CatalogueRevisionIdentity {
            hash: candidate.catalogue_hash(),
        },
    )
    .await?;

    for unit in source.units() {
        let inserted = transaction
            .execute(
                "INSERT INTO orna_source_units
                 (source_revision_id, source_unit_id, ordinal, logical_path, content, content_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                [
                    Value::Blob(source.id().to_bytes().to_vec()),
                    Value::Blob(unit.id().to_bytes().to_vec()),
                    Value::Integer(i64::from(unit.ordinal())),
                    Value::Text(unit.logical_path().to_owned()),
                    Value::Text(unit.content().to_owned()),
                    Value::Blob(unit.content_hash().to_bytes().to_vec()),
                ],
            )
            .await?;
        if inserted != 1 {
            return Err(SqliteError::InvalidPersistedData(
                "source unit insert affected an unexpected number of rows",
            ));
        }
    }

    for origin in candidate.origins() {
        let DefinitionIdentity::Schema(schema_id) = origin.identity() else {
            continue;
        };
        let schema = candidate.candidate().schema_by_id(schema_id).ok_or(
            SqliteError::InvalidPersistedData("schema origin has no candidate schema"),
        )?;
        let inserted = transaction
            .execute(
                "INSERT INTO orna_catalogue_schemas
                 (catalogue_revision_id, schema_id, name_parts, source_unit_id, source_start, source_end)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                [
                    Value::Blob(catalogue_pair.catalogue().to_bytes().to_vec()),
                    Value::Blob(schema.id().to_bytes().to_vec()),
                    Value::Text(encode_qualified_semantic_name(schema.name())),
                    Value::Blob(origin.source().source_unit().to_bytes().to_vec()),
                    Value::Integer(i64::from(origin.source().byte_start())),
                    Value::Integer(i64::from(origin.source().byte_end())),
                ],
            )
            .await?;
        if inserted != 1 {
            return Err(SqliteError::InvalidPersistedData(
                "schema insert affected an unexpected number of rows",
            ));
        }
    }

    let inserted = transaction
        .execute(
            "INSERT INTO orna_revision_snapshots
             (source_revision_id, catalogue_revision_id, payload)
             VALUES (?1, ?2, ?3)",
            [
                Value::Blob(source.id().to_bytes().to_vec()),
                Value::Blob(catalogue_pair.catalogue().to_bytes().to_vec()),
                Value::Blob(snapshot_payload),
            ],
        )
        .await?;
    if inserted != 1 {
        return Err(SqliteError::InvalidPersistedData(
            "revision snapshot insert affected an unexpected number of rows",
        ));
    }

    let next_ordinal = ordinal;
    let inserted = transaction
        .execute(
            "INSERT INTO orna_application_migrations
             (ordinal, format, version,
              expected_source_revision_id, expected_catalogue_revision_id,
              candidate_source_revision_id, candidate_catalogue_revision_id,
              canonical_bytes, digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            [
                Value::Integer(next_ordinal),
                Value::Text(entry.format().to_owned()),
                Value::Integer(i64::from(entry.version())),
                Value::Blob(entry.expected_base().source().to_bytes().to_vec()),
                Value::Blob(entry.expected_base().catalogue().to_bytes().to_vec()),
                Value::Blob(entry.candidate_pair().source().to_bytes().to_vec()),
                Value::Blob(entry.candidate_pair().catalogue().to_bytes().to_vec()),
                Value::Blob(entry.canonical_bytes().to_owned()),
                Value::Blob(entry.digest().to_bytes().to_vec()),
            ],
        )
        .await?;
    if inserted != 1 {
        return Err(SqliteError::InvalidPersistedData(
            "migration ledger insert affected an unexpected number of rows",
        ));
    }

    let source_parent = source.parent().map_or(Value::Null, |parent| {
        Value::Blob(parent.to_bytes().to_vec())
    });
    let updated = transaction
        .execute(
            "UPDATE orna_active_revision
             SET source_revision_id = ?1,
                 source_parent_revision_id = ?2,
                 catalogue_revision_id = ?3,
                 source_bundle_id = ?4,
                 source_bundle_hash = ?5,
                 source_revision_hash = ?6,
                 catalogue_hash = ?7
             WHERE singleton = 1",
            [
                Value::Blob(source.id().to_bytes().to_vec()),
                source_parent,
                Value::Blob(catalogue_pair.catalogue().to_bytes().to_vec()),
                Value::Blob(source.bundle().to_bytes().to_vec()),
                Value::Blob(source.bundle_hash().to_bytes().to_vec()),
                Value::Blob(source.revision_hash().to_bytes().to_vec()),
                Value::Blob(candidate.catalogue_hash().to_bytes().to_vec()),
            ],
        )
        .await?;
    if updated != 1 {
        return Err(SqliteError::InvalidPersistedData(
            "active pointer update affected an unexpected number of rows",
        ));
    }
    Ok(())
}

fn optional_source_revision_id(
    value: Value,
    field: &'static str,
) -> Result<Option<SourceRevisionId>, SqliteError> {
    match value {
        Value::Null => Ok(None),
        Value::Blob(value) => Ok(Some(SourceRevisionId::from_bytes(id16(value, field)?))),
        _ => Err(SqliteError::InvalidPersistedData(field)),
    }
}

fn optional_catalogue_revision_id(
    value: Value,
    field: &'static str,
) -> Result<Option<CatalogueRevisionId>, SqliteError> {
    match value {
        Value::Null => Ok(None),
        Value::Blob(value) => Ok(Some(CatalogueRevisionId::from_bytes(id16(value, field)?))),
        _ => Err(SqliteError::InvalidPersistedData(field)),
    }
}

fn id16(value: Vec<u8>, field: &'static str) -> Result<[u8; 16], SqliteError> {
    value
        .try_into()
        .map_err(|_| SqliteError::InvalidPersistedData(field))
}

fn digest32(value: Vec<u8>, field: &'static str) -> Result<Sha256Digest, SqliteError> {
    Ok(Sha256Digest::from_bytes(
        value
            .try_into()
            .map_err(|_| SqliteError::InvalidPersistedData(field))?,
    ))
}

fn encode_qualified_semantic_name(name: &QualifiedSemanticName) -> String {
    let separator = '\u{1f}';
    let capacity = name
        .parts()
        .iter()
        .map(String::len)
        .sum::<usize>()
        .saturating_add(name.parts().len().saturating_sub(1));
    let mut encoded = String::with_capacity(capacity);
    for (index, part) in name.parts().iter().enumerate() {
        if index > 0 {
            encoded.push(separator);
        }
        for character in part.chars() {
            encoded.push(character);
            if character == separator {
                encoded.push(separator);
            }
        }
    }
    encoded
}

fn decode_qualified_semantic_name(encoded: &str) -> Result<QualifiedSemanticName, SqliteError> {
    let separator = '\u{1f}';
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut characters = encoded.chars().peekable();
    while let Some(character) = characters.next() {
        if character != separator {
            part.push(character);
            continue;
        }
        if characters.peek() == Some(&separator) {
            characters.next();
            part.push(separator);
            continue;
        }
        if part.is_empty() {
            return Err(SqliteError::InvalidPersistedData(
                "schema name contains an empty part",
            ));
        }
        parts.push(std::mem::take(&mut part));
    }
    if part.is_empty() {
        return Err(SqliteError::InvalidPersistedData(
            "schema name contains an empty part",
        ));
    }
    parts.push(part);
    QualifiedSemanticName::new(parts).map_err(|_| {
        SqliteError::InvalidPersistedData("schema name parts must form one exact semantic name")
    })
}

impl ApplicationRevisionStore for SqliteRevisionStore {
    type Error = SqliteError;

    #[allow(clippy::manual_async_fn)]
    fn bootstrap(
        &self,
    ) -> impl Future<Output = Result<BootstrapRevision, StorageError<Self::Error>>> + Send {
        async move { self.seed_pair().await.map_err(StorageError::Backend) }
    }

    fn recover(
        &self,
    ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send
    {
        SqliteRevisionStore::recover(self)
    }

    fn apply(
        &self,
        candidate: &DeployableRevision,
        artifact: &PhysicalMigrationArtifact,
    ) -> impl Future<Output = Result<ActiveDatabaseRevision, StorageError<Self::Error>>> + Send
    {
        self.apply_with_artifact(candidate, artifact)
    }

    fn read_ledger(
        &self,
    ) -> impl Future<Output = Result<Vec<MigrationLedgerEntry>, StorageError<Self::Error>>> + Send
    {
        SqliteRevisionStore::read_ledger(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::canonical_hash::source_unit_content_digest;
    use orna_core::{
        FieldId, StandardLibraryRevisionId, TypeId,
        canonical_hash::{calculate_standard_library_digest, verify_standard_library_v2_snapshot},
        catalogue::{
            EnumTypeDefinition, RecordValueFieldDefinition, RecordValueTypeDefinition, TypeBinding,
            ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
        },
        revision::{
            CatalogueHashContext, DefinitionReference, DeployableRevisionContent,
            DeployableRevisionInput, FunctionRevisionRecord, StandardLibraryDigestVersion,
            StandardLibrarySnapshot,
        },
        types::TypeDescriptor,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("orna-sqlite-{nonce}.db"))
    }

    fn schema_candidate(
        active: &ActiveDatabaseRevision,
        source_byte: u8,
        catalogue_byte: u8,
        schema_byte: u8,
    ) -> DeployableRevision {
        schema_candidate_with_revision_ids(
            active,
            SourceRevisionId::from_bytes([source_byte; 16]),
            CatalogueRevisionId::from_bytes([catalogue_byte; 16]),
            source_byte,
            schema_byte,
        )
    }

    fn schema_candidate_with_revision_ids(
        active: &ActiveDatabaseRevision,
        source_id: SourceRevisionId,
        catalogue_id: CatalogueRevisionId,
        source_byte: u8,
        schema_byte: u8,
    ) -> DeployableRevision {
        let content = format!("CREATE SCHEMA schema_{schema_byte};\n");
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([source_byte; 16]),
            0,
            format!("schema_{schema_byte}.orna"),
            content.clone(),
            source_unit_content_digest(&content).unwrap(),
        )
        .unwrap();
        let bundle = SourceBundleId::from_bytes([source_byte.wrapping_add(1); 16]);
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source_hash =
            source_revision_record_digest(bundle, Some(active.pair().source()), bundle_hash)
                .unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            source_id,
            Some(active.pair().source()),
            vec![unit.clone()],
            bundle_hash,
            source_hash,
        )
        .unwrap();
        let schema_id = SchemaId::from_bytes([schema_byte; 16]);
        let schema = SchemaDefinition::new(
            schema_id,
            QualifiedSemanticName::new(vec!["schema".to_owned(), schema_byte.to_string()]).unwrap(),
        );
        let catalogue = CatalogueSnapshot::new(catalogue_id, vec![schema], Vec::new()).unwrap();
        let origin = DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema_id),
            SourceOrigin::new(unit.id(), 0, content.len() as u32).unwrap(),
        );
        let catalogue_hash =
            catalogue_digest(&catalogue, &[], &[], std::slice::from_ref(&origin), &[]).unwrap();
        DeployableRevision::new(
            active.pair(),
            source,
            active.pair().catalogue(),
            catalogue,
            catalogue_hash,
            vec![origin],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    const UNSUPPORTED_SOURCE_CONTENT: &str = "CREATE SCHEMA candidate;\n";

    fn unsupported_source_origin(source_byte: u8) -> SourceOrigin {
        SourceOrigin::new(
            SourceUnitId::from_bytes([source_byte; 16]),
            0,
            UNSUPPORTED_SOURCE_CONTENT.len() as u32,
        )
        .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn unsupported_candidate(
        active: &ActiveDatabaseRevision,
        source_byte: u8,
        catalogue: CatalogueSnapshot,
        identities: impl IntoIterator<Item = DefinitionIdentity>,
        expressions: Vec<orna_core::revision::ExpressionArtifact>,
        new_function_revisions: Vec<FunctionRevisionRecord>,
        references: Vec<DefinitionReference>,
        context: CatalogueHashContext,
    ) -> DeployableRevision {
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([source_byte; 16]),
            0,
            format!("candidate-{source_byte}.orna"),
            UNSUPPORTED_SOURCE_CONTENT,
            source_unit_content_digest(UNSUPPORTED_SOURCE_CONTENT).unwrap(),
        )
        .unwrap();
        let bundle = SourceBundleId::from_bytes([source_byte.wrapping_add(1); 16]);
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source_hash =
            source_revision_record_digest(bundle, Some(active.pair().source()), bundle_hash)
                .unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            SourceRevisionId::from_bytes([source_byte; 16]),
            Some(active.pair().source()),
            vec![unit],
            bundle_hash,
            source_hash,
        )
        .unwrap();
        let origins = identities
            .into_iter()
            .map(|identity| DefinitionOrigin::new(identity, unsupported_source_origin(source_byte)))
            .collect::<Vec<_>>();
        let candidate_hash = Sha256Digest::from_bytes([0; 32]);
        let expected_base = active.pair();

        if context.version() == CatalogueHashVersion::Version1 {
            return DeployableRevision::new(
                expected_base,
                source,
                expected_base.catalogue(),
                catalogue,
                candidate_hash,
                origins,
                expressions,
                new_function_revisions,
                references,
            )
            .unwrap();
        }

        let current_function_revisions = new_function_revisions.clone();
        let content = DeployableRevisionContent::new(
            origins,
            expressions,
            new_function_revisions,
            references,
        )
        .with_current_function_revisions(current_function_revisions);
        let input = DeployableRevisionInput::new(
            expected_base,
            source,
            expected_base.catalogue(),
            catalogue,
            candidate_hash,
            content,
        );
        DeployableRevision::new_with_catalogue_hash_context_and_parent(input, context, None)
            .unwrap()
    }

    fn empty_version_two_context() -> CatalogueHashContext {
        let content = "standard source\n";
        let unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0xe0; 16]),
            0,
            "standard.orna",
            content,
            source_unit_content_digest(content).unwrap(),
        )
        .unwrap();
        let bundle = SourceBundleId::from_bytes([0xe2; 16]);
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source_parent = SourceRevisionId::from_bytes([0xe1; 16]);
        let source_hash =
            source_revision_record_digest(bundle, Some(source_parent), bundle_hash).unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            SourceRevisionId::from_bytes([0xe0; 16]),
            Some(source_parent),
            vec![unit],
            bundle_hash,
            source_hash,
        )
        .unwrap();
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0xe3; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let make_snapshot = |digest| {
            StandardLibrarySnapshot::new(
                StandardLibraryRevisionId::from_bytes([0xe4; 16]),
                StandardLibraryDigestVersion::Version2,
                source.clone(),
                "orna.language/2",
                catalogue.clone(),
                Vec::new(),
                digest,
            )
            .unwrap()
        };
        let unchecked = make_snapshot(Sha256Digest::from_bytes([0; 32]));
        let digest = calculate_standard_library_digest(&unchecked).unwrap();
        let verified = verify_standard_library_v2_snapshot(make_snapshot(digest)).unwrap();
        CatalogueHashContext::version_two(verified)
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct PersistedRowCounts {
        active: i64,
        source_revisions: i64,
        catalogue_revisions: i64,
        source_units: i64,
        schemas: i64,
        snapshots: i64,
        ledger: i64,
    }

    async fn persisted_row_counts(store: &SqliteRevisionStore) -> PersistedRowCounts {
        let connection = store.connection.lock().await;
        PersistedRowCounts {
            active: row_count(&connection, "SELECT COUNT(*) FROM orna_active_revision").await,
            source_revisions: row_count(&connection, "SELECT COUNT(*) FROM orna_source_revisions")
                .await,
            catalogue_revisions: row_count(
                &connection,
                "SELECT COUNT(*) FROM orna_catalogue_revisions",
            )
            .await,
            source_units: row_count(&connection, "SELECT COUNT(*) FROM orna_source_units").await,
            schemas: row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_schemas").await,
            snapshots: row_count(&connection, "SELECT COUNT(*) FROM orna_revision_snapshots").await,
            ledger: row_count(
                &connection,
                "SELECT COUNT(*) FROM orna_application_migrations",
            )
            .await,
        }
    }

    async fn assert_unsupported_candidate_without_mutation(
        store: &SqliteRevisionStore,
        candidate: &DeployableRevision,
        artifact: &PhysicalMigrationArtifact,
        expected: SqliteCapability,
    ) {
        let before = persisted_row_counts(store).await;
        let error = ApplicationRevisionStore::apply(store, candidate, artifact)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::UnsupportedCapability(actual))
                if actual == expected
        ));
        assert_eq!(persisted_row_counts(store).await, before);
        assert_eq!(
            store.recover().await.unwrap().pair(),
            candidate.expected_base()
        );
    }

    fn unsupported_schema(schema_byte: u8) -> (SchemaId, SchemaDefinition) {
        let id = SchemaId::from_bytes([schema_byte; 16]);
        let schema = SchemaDefinition::new(id, QualifiedSemanticName::new(["schema"]).unwrap());
        (id, schema)
    }

    async fn row_count(connection: &Connection, query: &str) -> i64 {
        let mut rows = connection.query(query, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    #[test]
    fn qualified_name_serialization_escapes_delimiters_and_round_trips() {
        let name = QualifiedSemanticName::new(vec![
            "catalog\u{1f}part".to_owned(),
            "schema".to_owned(),
            "tail\u{1f}".to_owned(),
        ])
        .unwrap();
        let encoded = encode_qualified_semantic_name(&name);
        assert_eq!(
            encoded,
            "catalog\u{1f}\u{1f}part\u{1f}schema\u{1f}tail\u{1f}\u{1f}"
        );
        assert_eq!(decode_qualified_semantic_name(&encoded).unwrap(), name);
    }

    #[test]
    fn qualified_name_decoder_preserves_legacy_rows_and_rejects_empty_parts() {
        let legacy = decode_qualified_semantic_name("catalog\u{1f}schema").unwrap();
        assert_eq!(
            legacy,
            QualifiedSemanticName::new(["catalog", "schema"]).unwrap()
        );

        for encoded in [
            "",
            "\u{1f}",
            "leading\u{1f}",
            "\u{1f}trailing",
            "a\u{1f}\u{1f}\u{1f}",
        ] {
            assert!(matches!(
                decode_qualified_semantic_name(encoded),
                Err(SqliteError::InvalidPersistedData(
                    "schema name contains an empty part"
                ))
            ));
        }
    }

    #[tokio::test]
    async fn rejects_unsupported_capabilities_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let active = store.recover().await.unwrap();

        let (schema_id, schema) = unsupported_schema(0x11);
        let value_id = TypeId::from_bytes([0x12; 16]);
        let binary_value = ValueTypeDefinition::primitive(
            value_id,
            QualifiedSemanticName::new(["schema", "binary"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.binary-large-object@1",
        );
        let binary_catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x13; 16]),
            vec![schema],
            Vec::new(),
            vec![binary_value],
            Vec::new(),
        )
        .unwrap();
        let binary_value_candidate = unsupported_candidate(
            &active,
            0x14,
            binary_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ValueType(value_id),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );

        let (schema_id, schema) = unsupported_schema(0x21);
        let array_like_id = TypeId::from_bytes([0x22; 16]);
        let array_like_value = ValueTypeDefinition::opaque(
            array_like_id,
            QualifiedSemanticName::new(["schema", "array"]).unwrap(),
            "orna.kernel.value.array@1",
        );
        let array_like_catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x23; 16]),
            vec![schema],
            Vec::new(),
            vec![array_like_value],
            Vec::new(),
        )
        .unwrap();
        let array_like_value_candidate = unsupported_candidate(
            &active,
            0x24,
            array_like_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ValueType(array_like_id),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );

        let (schema_id, schema) = unsupported_schema(0x31);
        let binding_value_id = TypeId::from_bytes([0x32; 16]);
        let binding = TypeBinding::qualified(
            QualifiedSemanticName::new(["schema", "bound"]).unwrap(),
            binding_value_id,
        )
        .unwrap();
        let binding_id = binding.id();
        let binding_catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x33; 16]),
            vec![schema],
            Vec::new(),
            vec![ValueTypeDefinition::primitive(
                binding_value_id,
                QualifiedSemanticName::new(["schema", "value"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.kernel.value.boolean@1",
            )],
            vec![binding],
        )
        .unwrap();
        let binding_candidate = unsupported_candidate(
            &active,
            0x34,
            binding_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ValueType(binding_value_id),
                DefinitionIdentity::TypeBinding(binding_id),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );

        let (schema_id, schema) = unsupported_schema(0x41);
        let enum_id = TypeId::from_bytes([0x42; 16]);
        let enum_catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x43; 16]),
            vec![schema],
            Vec::new(),
            Vec::new(),
            vec![EnumTypeDefinition::new(
                enum_id,
                QualifiedSemanticName::new(["schema", "status"]).unwrap(),
                ["active", "closed"],
            )],
            Vec::new(),
        )
        .unwrap();
        let enum_candidate = unsupported_candidate(
            &active,
            0x44,
            enum_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ValueType(enum_id),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );

        let (schema_id, schema) = unsupported_schema(0x51);
        let record_enum_id = TypeId::from_bytes([0x52; 16]);
        let record_id = TypeId::from_bytes([0x53; 16]);
        let record_field_id = FieldId::from_bytes([0x54; 16]);
        let record_field = RecordValueFieldDefinition::try_new_descriptor(
            record_field_id,
            "status",
            0,
            TypeDescriptor::named(record_enum_id),
        )
        .unwrap();
        let record_catalogue = CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes([0x55; 16]),
            vec![schema],
            Vec::new(),
            Vec::new(),
            vec![EnumTypeDefinition::new(
                record_enum_id,
                QualifiedSemanticName::new(["schema", "record_status"]).unwrap(),
                ["active", "closed"],
            )],
            vec![RecordValueTypeDefinition::new(
                record_id,
                QualifiedSemanticName::new(["schema", "status_record"]).unwrap(),
                vec![record_field],
            )],
            Vec::new(),
        )
        .unwrap();
        let record_candidate = unsupported_candidate(
            &active,
            0x56,
            record_catalogue,
            [
                DefinitionIdentity::Schema(schema_id),
                DefinitionIdentity::ValueType(record_enum_id),
                DefinitionIdentity::ValueType(record_id),
                DefinitionIdentity::Field {
                    owner: record_id,
                    field: record_field_id,
                },
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );
        let (schema_id, schema) = unsupported_schema(0xb1);
        let hash_catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0xb2; 16]),
            vec![schema],
            Vec::new(),
        )
        .unwrap();
        let hash_candidate = unsupported_candidate(
            &active,
            0xb3,
            hash_catalogue,
            [DefinitionIdentity::Schema(schema_id)],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            empty_version_two_context(),
        );

        let cases = [
            (SqliteCapability::ValueType, binary_value_candidate),
            (SqliteCapability::ValueType, array_like_value_candidate),
            (SqliteCapability::ValueType, binding_candidate),
            (SqliteCapability::EnumType, enum_candidate),
            (SqliteCapability::EnumType, record_candidate),
            (SqliteCapability::CatalogueHashVersion, hash_candidate),
        ];
        for (expected, candidate) in cases {
            let artifact = PhysicalMigrationArtifact::from_revisions(&active, &candidate).unwrap();
            assert_unsupported_candidate_without_mutation(&store, &candidate, &artifact, expected)
                .await;
        }
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn opens_local_file_bootstraps_idempotently_and_recovers_empty_revision() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        let first = store.bootstrap().await.unwrap();
        let second = store.bootstrap().await.unwrap();
        assert_eq!(first, second);
        let recovered = store.recover().await.unwrap();
        assert_eq!(recovered.pair().source(), first.source());
        assert_eq!(recovered.pair().catalogue(), first.catalogue());
        assert!(recovered.source().units().is_empty());
        assert!(recovered.catalogue().schemas().is_empty());
        assert!(store.read_ledger().await.unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_malformed_source_hash_with_null_parent_on_recovery() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_active_revision
                 SET source_revision_hash = ?1
                 WHERE singleton = 1",
                [Value::Blob(vec![0; 32])],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "source revision hash mismatch"
            ))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_tampered_active_source_registry_on_recovery() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_source_revisions
                 SET source_bundle_hash = ?1",
                [Value::Blob(vec![0; 32])],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "active source revision registry does not match active metadata"
            ))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn applies_exact_artifacts_in_order_and_recovers_schema_records() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0x11, 0x12, 0x13);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first = ApplicationRevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();

        let second_candidate = schema_candidate(&first, 0x21, 0x22, 0x23);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first, &second_candidate).unwrap();
        ApplicationRevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap();

        let ledger = ApplicationRevisionStore::read_ledger(&store).await.unwrap();
        assert_eq!(ledger.len(), 2);
        assert_eq!(
            ledger[0],
            MigrationLedgerEntry::from_artifact(&first_artifact)
        );
        assert_eq!(
            ledger[1],
            MigrationLedgerEntry::from_artifact(&second_artifact)
        );
        assert_eq!(ledger[0].candidate_pair(), first_candidate.candidate_pair());
        assert_eq!(
            ledger[1].candidate_pair(),
            second_candidate.candidate_pair()
        );

        let reopened = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        let recovered = reopened.recover().await.unwrap();
        assert_eq!(recovered.pair(), second_candidate.candidate_pair());
        assert_eq!(recovered.source().units().len(), 1);
        assert_eq!(recovered.catalogue().schemas().len(), 1);
        assert_eq!(recovered.origins().len(), 1);
        assert_eq!(reopened.read_ledger().await.unwrap(), ledger);
        let _ = std::fs::remove_file(path);
    }
    #[tokio::test]
    async fn rejects_tampered_active_snapshot_source_after_reopen() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let candidate = schema_candidate(&initial, 0x91, 0x92, 0x93);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();
        ApplicationRevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap();

        let connection = store.connection.lock().await;
        let mut rows = connection
            .query(
                "SELECT payload FROM orna_revision_snapshots
                 WHERE source_revision_id = ?1 AND catalogue_revision_id = ?2",
                [
                    Value::Blob(candidate.source().id().to_bytes().to_vec()),
                    Value::Blob(candidate.candidate_pair().catalogue().to_bytes().to_vec()),
                ],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().expect("active snapshot");
        let payload = row.get::<Vec<u8>>(0).unwrap();
        drop(rows);
        let mut snapshot =
            serde_json::from_slice::<serde_json::Value>(&payload).expect("snapshot JSON");
        let tampered_content = "tampered active snapshot\n";
        let tampered_hash = source_unit_content_digest(tampered_content).unwrap();
        snapshot["source"]["units"][0]["content"] =
            serde_json::Value::String(tampered_content.to_owned());
        snapshot["source"]["units"][0]["content_hash"] =
            serde_json::to_value(tampered_hash).unwrap();
        let updated = connection
            .execute(
                "UPDATE orna_revision_snapshots SET payload = ?1
                 WHERE source_revision_id = ?2 AND catalogue_revision_id = ?3",
                [
                    Value::Blob(serde_json::to_vec(&snapshot).unwrap()),
                    Value::Blob(candidate.source().id().to_bytes().to_vec()),
                    Value::Blob(candidate.candidate_pair().catalogue().to_bytes().to_vec()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);
        drop(store);

        let reopened = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        assert!(matches!(
            reopened.recover().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "revision snapshot source bundle hash mismatch"
            ))
        ));
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_tampered_ledger_fields_after_reopen() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let candidate = schema_candidate(&initial, 0xa1, 0xa2, 0xa3);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();
        ApplicationRevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap();
        let ledger_entry = MigrationLedgerEntry::from_artifact(&artifact);
        drop(store);

        let mut corrupted_canonical = artifact.canonical_bytes().to_owned();
        corrupted_canonical[0] ^= 0xff;
        let mut corrupted_digest = artifact.digest().to_bytes().to_vec();
        corrupted_digest[0] ^= 1;
        let cases = vec![
            (
                "format",
                Value::Text("ORNA-OTHER-FORMAT".to_owned()),
                Value::Text(ledger_entry.format().to_owned()),
            ),
            (
                "version",
                Value::Integer(i64::from(ledger_entry.version()) + 1),
                Value::Integer(i64::from(ledger_entry.version())),
            ),
            (
                "canonical_bytes",
                Value::Blob(corrupted_canonical),
                Value::Blob(artifact.canonical_bytes().to_owned()),
            ),
            (
                "digest",
                Value::Blob(corrupted_digest),
                Value::Blob(artifact.digest().to_bytes().to_vec()),
            ),
        ];
        for (column, corrupted, original) in cases {
            let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
                .await
                .unwrap();
            let connection = store.connection.lock().await;
            let statement =
                format!("UPDATE orna_application_migrations SET {column} = ?1 WHERE ordinal = 0");
            assert_eq!(
                connection.execute(&statement, [corrupted]).await.unwrap(),
                1
            );
            drop(connection);
            drop(store);

            let reopened = SqliteRevisionStore::open(&SqliteConfig::new(&path))
                .await
                .unwrap();
            assert!(matches!(
                reopened.recover().await.unwrap_err(),
                StorageError::Backend(SqliteError::Domain(message))
                    if message.contains("invalid migration ledger entry at ordinal 0")
            ));
            drop(reopened);

            let repaired = SqliteRevisionStore::open(&SqliteConfig::new(&path))
                .await
                .unwrap();
            let connection = repaired.connection.lock().await;
            assert_eq!(connection.execute(&statement, [original]).await.unwrap(), 1);
            drop(connection);
            drop(repaired);
        }
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_active_pointer_divergence_when_reading_ledger() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let initial_metadata = {
            let connection = store.connection.lock().await;
            load_active_identity_metadata(&connection)
                .await
                .unwrap()
                .unwrap()
        };

        let candidate = schema_candidate(&initial, 0x31, 0x32, 0x33);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();
        ApplicationRevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_active_revision
                 SET source_revision_id = ?1,
                     source_parent_revision_id = ?2,
                     catalogue_revision_id = ?3,
                     source_bundle_id = ?4,
                     source_bundle_hash = ?5,
                     source_revision_hash = ?6,
                     catalogue_hash = ?7
                 WHERE singleton = 1",
                [
                    Value::Blob(initial_metadata.source_id.to_bytes().to_vec()),
                    Value::Null,
                    Value::Blob(initial_metadata.catalogue_id.to_bytes().to_vec()),
                    Value::Blob(initial_metadata.source.bundle.to_bytes().to_vec()),
                    Value::Blob(initial_metadata.source.bundle_hash.to_bytes().to_vec()),
                    Value::Blob(initial_metadata.source.revision_hash.to_bytes().to_vec()),
                    Value::Blob(initial_metadata.catalogue.hash.to_bytes().to_vec()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        assert!(matches!(
            store.read_ledger().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger does not end at active revision"
            ))
        ));
        assert!(matches!(
            store.recover().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger does not end at active revision"
            ))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_tampered_active_catalogue_lineage_before_apply() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let candidate = schema_candidate(&initial, 0x81, 0x82, 0x83);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_catalogue_revisions
                 SET source_revision_id = ?1
                 WHERE catalogue_revision_id = ?2",
                [
                    Value::Blob(vec![0x99; 16]),
                    Value::Blob(initial.pair().catalogue().to_bytes().to_vec()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        assert!(matches!(
            ApplicationRevisionStore::apply(&store, &candidate, &artifact)
                .await
                .unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "active catalogue revision source does not match active source"
            ))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_tampered_historical_catalogue_when_reading_ledger() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0x41, 0x42, 0x43);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first = ApplicationRevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();
        let second_candidate = schema_candidate(&first, 0x51, 0x52, 0x53);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first, &second_candidate).unwrap();
        ApplicationRevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_catalogue_revisions
                 SET catalogue_hash = ?1
                 WHERE catalogue_revision_id = ?2",
                [
                    Value::Blob(vec![0; 32]),
                    Value::Blob(
                        first_candidate
                            .candidate_pair()
                            .catalogue()
                            .to_bytes()
                            .to_vec(),
                    ),
                ],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        assert!(matches!(
            store.read_ledger().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "historical catalogue hash mismatch"
            ))
        ));
        assert!(matches!(
            store.recover().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "historical catalogue hash mismatch"
            ))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_tampered_historical_catalogue_parent_when_reading_ledger() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let first_candidate = schema_candidate(&initial, 0x61, 0x62, 0x63);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first = ApplicationRevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();
        let second_candidate = schema_candidate(&first, 0x71, 0x72, 0x73);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first, &second_candidate).unwrap();
        ApplicationRevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_catalogue_revisions
                 SET parent_catalogue_revision_id = NULL
                 WHERE catalogue_revision_id = ?1",
                [Value::Blob(
                    first_candidate
                        .candidate_pair()
                        .catalogue()
                        .to_bytes()
                        .to_vec(),
                )],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        assert!(matches!(
            store.read_ledger().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "catalogue revision parent does not match migration history"
            ))
        ));
        assert!(matches!(
            store.recover().await.unwrap_err(),
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "catalogue revision parent does not match migration history"
            ))
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_reused_source_revision_identity_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let first_candidate = schema_candidate(&initial, 0x71, 0x72, 0x73);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let active = ApplicationRevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();

        let ledger_before = store.read_ledger().await.unwrap();
        let connection = store.connection.lock().await;
        let source_units_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_units").await;
        let schemas_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_schemas").await;
        let source_revisions_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_revisions").await;
        let catalogue_revisions_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_revisions").await;
        drop(connection);

        let candidate = schema_candidate_with_revision_ids(
            &active,
            initial.pair().source(),
            CatalogueRevisionId::from_bytes([0x82; 16]),
            0x81,
            0x83,
        );
        let artifact = PhysicalMigrationArtifact::from_revisions(&active, &candidate).unwrap();
        let error = ApplicationRevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::Backend(_)));

        assert_eq!(store.read_ledger().await.unwrap(), ledger_before);
        let recovered = store.recover().await.unwrap();
        assert_eq!(recovered.pair(), active.pair());
        assert_eq!(
            recovered.source().revision_hash(),
            active.source().revision_hash()
        );
        assert_eq!(recovered.catalogue_hash(), active.catalogue_hash());
        let connection = store.connection.lock().await;
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_units").await,
            source_units_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_schemas").await,
            schemas_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_revisions").await,
            source_revisions_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_revisions").await,
            catalogue_revisions_before
        );
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_reused_catalogue_revision_identity_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let first_candidate = schema_candidate(&initial, 0x91, 0x92, 0x93);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let active = ApplicationRevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();

        let ledger_before = store.read_ledger().await.unwrap();
        let connection = store.connection.lock().await;
        let source_units_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_units").await;
        let schemas_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_schemas").await;
        let source_revisions_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_revisions").await;
        let catalogue_revisions_before =
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_revisions").await;
        drop(connection);

        let candidate = schema_candidate_with_revision_ids(
            &active,
            SourceRevisionId::from_bytes([0xa1; 16]),
            initial.pair().catalogue(),
            0xa1,
            0xa3,
        );
        let artifact = PhysicalMigrationArtifact::from_revisions(&active, &candidate).unwrap();
        let error = ApplicationRevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::Backend(_)));

        assert_eq!(store.read_ledger().await.unwrap(), ledger_before);
        let recovered = store.recover().await.unwrap();
        assert_eq!(recovered.pair(), active.pair());
        assert_eq!(
            recovered.source().revision_hash(),
            active.source().revision_hash()
        );
        assert_eq!(recovered.catalogue_hash(), active.catalogue_hash());
        let connection = store.connection.lock().await;
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_units").await,
            source_units_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_schemas").await,
            schemas_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_source_revisions").await,
            source_revisions_before
        );
        assert_eq!(
            row_count(&connection, "SELECT COUNT(*) FROM orna_catalogue_revisions").await,
            catalogue_revisions_before
        );
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_non_contiguous_migration_ledger_ordinals() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0x51, 0x52, 0x53);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first = ApplicationRevisionStore::apply(&store, &first_candidate, &first_artifact)
            .await
            .unwrap();

        let second_candidate = schema_candidate(&first, 0x61, 0x62, 0x63);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first, &second_candidate).unwrap();
        ApplicationRevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap();

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_application_migrations
                 SET ordinal = 2
                 WHERE ordinal = 1",
                (),
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        let error = store.read_ledger().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::Domain(message))
                if message.contains("ordinal 2") && message.contains("expected 1")
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_mismatched_artifact_without_visible_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let candidate = schema_candidate(&initial, 0x31, 0x32, 0x33);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();
        let wrong_candidate = schema_candidate(&initial, 0x41, 0x42, 0x43);

        let before = store.read_ledger().await.unwrap();
        let error = ApplicationRevisionStore::apply(&store, &wrong_candidate, &artifact)
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::InvalidRequest(_)));
        assert_eq!(store.read_ledger().await.unwrap(), before);
        assert_eq!(store.recover().await.unwrap().pair(), initial.pair());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn concurrent_independent_applies_commit_exactly_once() {
        let path = temp_path();
        let store_a = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store_a.bootstrap().await.unwrap();
        let store_b = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        let initial = store_a.recover().await.unwrap();
        let candidate = schema_candidate(&initial, 0xb1, 0xb2, 0xb3);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();

        let (result_a, result_b) = tokio::join!(
            ApplicationRevisionStore::apply(&store_a, &candidate, &artifact),
            ApplicationRevisionStore::apply(&store_b, &candidate, &artifact),
        );

        let mut successes = 0;
        for result in [result_a, result_b] {
            match result {
                Ok(active) => {
                    successes += 1;
                    assert_eq!(active.pair(), candidate.candidate_pair());
                }
                Err(error) => match error {
                    StorageError::InvalidRequest(
                        MigrationLedgerEntryError::ActiveBaseMismatch { expected, actual },
                    ) => {
                        assert_eq!(expected, initial.pair());
                        assert_eq!(actual, candidate.candidate_pair());
                    }
                    StorageError::Backend(SqliteError::Backend(turso::Error::Busy(_)))
                    | StorageError::Backend(SqliteError::Backend(turso::Error::BusySnapshot(_))) => {
                    }
                    StorageError::Backend(SqliteError::Backend(turso::Error::Error(message)))
                        if message.to_ascii_lowercase().contains("busy")
                            || message.to_ascii_lowercase().contains("locked") => {}
                    other => panic!("unexpected concurrent apply result: {other:?}"),
                },
            }
        }
        assert_eq!(successes, 1);
        assert_eq!(
            persisted_row_counts(&store_a).await,
            PersistedRowCounts {
                active: 1,
                source_revisions: 2,
                catalogue_revisions: 2,
                source_units: 1,
                schemas: 1,
                snapshots: 1,
                ledger: 1,
            }
        );
        assert_eq!(
            store_a.read_ledger().await.unwrap(),
            vec![MigrationLedgerEntry::from_artifact(&artifact)]
        );
        assert_eq!(
            store_a.recover().await.unwrap().pair(),
            candidate.candidate_pair()
        );

        drop(store_b);
        drop(store_a);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_non_contiguous_ledger_before_applying_candidate() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0xc1, 0xc2, 0xc3);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let active_before =
            ApplicationRevisionStore::apply(&store, &first_candidate, &first_artifact)
                .await
                .unwrap();
        let counts_before = persisted_row_counts(&store).await;

        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_application_migrations
                 SET ordinal = 1
                 WHERE ordinal = 0",
                (),
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        let second_candidate = schema_candidate(&active_before, 0xd1, 0xd2, 0xd3);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&active_before, &second_candidate).unwrap();
        let error = ApplicationRevisionStore::apply(&store, &second_candidate, &second_artifact)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::Domain(message))
                if message == "invalid migration ledger ordinal 1: expected 0"
        ));

        assert_eq!(persisted_row_counts(&store).await, counts_before);
        let active_after = {
            let connection = store.connection.lock().await;
            load_active_from(&connection).await.unwrap()
        };
        assert_eq!(active_after.pair(), active_before.pair());
        assert_eq!(
            active_after.source().revision_hash(),
            active_before.source().revision_hash()
        );
        assert_eq!(
            active_after.catalogue_hash(),
            active_before.catalogue_hash()
        );
        let ordinal = {
            let connection = store.connection.lock().await;
            let mut rows = connection
                .query(
                    "SELECT ordinal
                     FROM orna_application_migrations",
                    (),
                )
                .await
                .unwrap();
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
        };
        assert_eq!(ordinal, 1);

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_deleted_and_renumbered_first_ledger_entry_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0xe1, 0xe2, 0xe3);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first_active =
            ApplicationRevisionStore::apply(&store, &first_candidate, &first_artifact)
                .await
                .unwrap();

        let second_candidate = schema_candidate(&first_active, 0xe4, 0xe5, 0xe6);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first_active, &second_candidate).unwrap();
        let second_active =
            ApplicationRevisionStore::apply(&store, &second_candidate, &second_artifact)
                .await
                .unwrap();

        let third_candidate = schema_candidate(&second_active, 0xe7, 0xe8, 0xe9);
        let third_artifact =
            PhysicalMigrationArtifact::from_revisions(&second_active, &third_candidate).unwrap();

        let connection = store.connection.lock().await;
        let deleted = connection
            .execute(
                "DELETE FROM orna_application_migrations
                 WHERE ordinal = 0",
                (),
            )
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        let renumbered = connection
            .execute(
                "UPDATE orna_application_migrations
                 SET ordinal = 0
                 WHERE ordinal = 1",
                (),
            )
            .await
            .unwrap();
        assert_eq!(renumbered, 1);
        drop(connection);

        let counts_after_corruption = persisted_row_counts(&store).await;
        let error = store.read_ledger().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger first expected source revision is not a root source"
            ))
        ));

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger first expected source revision is not a root source"
            ))
        ));

        let error = ApplicationRevisionStore::apply(&store, &third_candidate, &third_artifact)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger first expected source revision is not a root source"
            ))
        ));
        assert_eq!(persisted_row_counts(&store).await, counts_after_corruption);

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_empty_ledger_after_non_root_apply_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let candidate = schema_candidate(&initial, 0xea, 0xeb, 0xec);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();
        let active = ApplicationRevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap();
        let next_candidate = schema_candidate(&active, 0xed, 0xee, 0xef);
        let next_artifact =
            PhysicalMigrationArtifact::from_revisions(&active, &next_candidate).unwrap();

        let connection = store.connection.lock().await;
        let deleted = connection
            .execute("DELETE FROM orna_application_migrations", ())
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        drop(connection);

        let counts_after_corruption = persisted_row_counts(&store).await;
        let error = store.read_ledger().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger is empty but active source revision has a parent"
            ))
        ));

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger is empty but active source revision has a parent"
            ))
        ));

        let error = ApplicationRevisionStore::apply(&store, &next_candidate, &next_artifact)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "migration ledger is empty but active source revision has a parent"
            ))
        ));
        assert_eq!(persisted_row_counts(&store).await, counts_after_corruption);

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn rejects_recovery_when_active_source_parent_registry_is_missing() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();
        let candidate = schema_candidate(&initial, 0xe1, 0xe2, 0xe3);
        let artifact = PhysicalMigrationArtifact::from_revisions(&initial, &candidate).unwrap();
        let active = ApplicationRevisionStore::apply(&store, &candidate, &artifact)
            .await
            .unwrap();
        let counts_before_delete = persisted_row_counts(&store).await;
        let metadata_before = {
            let connection = store.connection.lock().await;
            load_active_identity_metadata(&connection)
                .await
                .unwrap()
                .unwrap()
        };
        let parent = active
            .source()
            .parent()
            .expect("applied candidate must retain its active source parent");

        let connection = store.connection.lock().await;
        let deleted = connection
            .execute(
                "DELETE FROM orna_source_revisions
                 WHERE source_revision_id = ?1",
                [Value::Blob(parent.to_bytes().to_vec())],
            )
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        drop(connection);

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "active source parent revision has no registry record"
            ))
        ));
        assert_eq!(
            persisted_row_counts(&store).await,
            PersistedRowCounts {
                source_revisions: counts_before_delete.source_revisions - 1,
                ..counts_before_delete
            }
        );
        let metadata_after = {
            let connection = store.connection.lock().await;
            load_active_identity_metadata(&connection)
                .await
                .unwrap()
                .unwrap()
        };
        assert_eq!(metadata_after.source_id, metadata_before.source_id);
        assert_eq!(metadata_after.source, metadata_before.source);
        assert_eq!(metadata_after.catalogue_id, metadata_before.catalogue_id);
        assert_eq!(metadata_after.catalogue, metadata_before.catalogue);

        drop(store);
        let _ = std::fs::remove_file(path);
    }
    #[tokio::test]
    async fn rejects_tampered_historical_source_units_without_mutation() {
        let path = temp_path();
        let store = SqliteRevisionStore::open(&SqliteConfig::new(&path))
            .await
            .unwrap();
        store.bootstrap().await.unwrap();
        let initial = store.recover().await.unwrap();

        let first_candidate = schema_candidate(&initial, 0x31, 0x32, 0x33);
        let first_artifact =
            PhysicalMigrationArtifact::from_revisions(&initial, &first_candidate).unwrap();
        let first_active =
            ApplicationRevisionStore::apply(&store, &first_candidate, &first_artifact)
                .await
                .unwrap();

        let second_candidate = schema_candidate(&first_active, 0x41, 0x42, 0x43);
        let second_artifact =
            PhysicalMigrationArtifact::from_revisions(&first_active, &second_candidate).unwrap();
        let second_active =
            ApplicationRevisionStore::apply(&store, &second_candidate, &second_artifact)
                .await
                .unwrap();
        assert_ne!(
            first_candidate.source().id(),
            second_active.pair().source(),
            "the tampered source must be historical rather than active"
        );

        let counts_before_tamper = persisted_row_counts(&store).await;
        let metadata_before_tamper = {
            let connection = store.connection.lock().await;
            load_active_identity_metadata(&connection)
                .await
                .unwrap()
                .unwrap()
        };
        let third_candidate = schema_candidate(&second_active, 0x51, 0x52, 0x53);
        let third_artifact =
            PhysicalMigrationArtifact::from_revisions(&second_active, &third_candidate).unwrap();

        let tampered_content = "tampered historical source\n";
        let tampered_hash = source_unit_content_digest(tampered_content).unwrap();
        let connection = store.connection.lock().await;
        let updated = connection
            .execute(
                "UPDATE orna_source_units
                 SET content = ?1, content_hash = ?2
                 WHERE source_revision_id = ?3",
                [
                    Value::Text(tampered_content.to_owned()),
                    Value::Blob(tampered_hash.to_bytes().to_vec()),
                    Value::Blob(first_candidate.source().id().to_bytes().to_vec()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(updated, 1);
        drop(connection);

        let error = store.read_ledger().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "source bundle hash mismatch"
            ))
        ));

        let error = store.recover().await.unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "source bundle hash mismatch"
            ))
        ));

        let error = ApplicationRevisionStore::apply(&store, &third_candidate, &third_artifact)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::Backend(SqliteError::InvalidPersistedData(
                "source bundle hash mismatch"
            ))
        ));

        assert_eq!(persisted_row_counts(&store).await, counts_before_tamper);
        let metadata_after_tamper = {
            let connection = store.connection.lock().await;
            load_active_identity_metadata(&connection)
                .await
                .unwrap()
                .unwrap()
        };
        assert_eq!(
            metadata_after_tamper.source_id,
            metadata_before_tamper.source_id
        );
        assert_eq!(metadata_after_tamper.source, metadata_before_tamper.source);
        assert_eq!(
            metadata_after_tamper.catalogue_id,
            metadata_before_tamper.catalogue_id
        );
        assert_eq!(
            metadata_after_tamper.catalogue,
            metadata_before_tamper.catalogue
        );

        drop(store);
        let _ = std::fs::remove_file(path);
    }
}
