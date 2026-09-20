use super::{PostgresKernel, PostgresKernelError};

use std::{collections::BTreeSet, time::SystemTime};

use orna_core::{
    CatalogueRevisionId, FunctionId, InvocationId, PrincipalId, SourceRevisionId,
    inspect::InspectPrivilege,
    security::{
        AuthenticatedSession, InspectDecision, InspectDenial, SecurityAuditDecision,
        authorise_inspect,
    },
};
use orna_foundation_v1::{
    CwdCapture, FunctionRef, InvocationArgumentRef, InvocationRef, InvocationStatus, OvbRaw,
    RowRef, Snapshot, SnapshotRef, TypeRef, Value, invocation_argument_reference,
    invocation_reference, snapshot_reference, validate_function_reference,
    validate_invocation_argument_reference, validate_invocation_reference,
    validate_object_reference_for_id, validate_type_reference,
};
use tokio_postgres::{IsolationLevel, Row, types::FromSqlOwned};

use crate::kernel::{
    bootstrap::require_current_migrations,
    security::{append_security_audit_event, recover_security_snapshot_for_active},
    security_admin::inspect_privileges_for_session,
    server_runtime::configure_and_recover,
};

/// Read-only durable observation of one resolved sealed invocation.
///
/// This is deliberately reconstructed from the lifecycle and argument
/// metadata relations, rather than from private security audit evidence or a
/// live operation.  A row is observable only when it retained the complete
/// pinned target tuple; unresolved target denials therefore cannot acquire a
/// synthetic public target identity through this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SealedInvocationObservation {
    /// Complete CWD capture decoded from the persisted admission evidence.
    ///
    /// This stays private because it is projection provenance, not a second
    /// public snapshot field. It lets the public projection derive checked
    /// snapshot coordinates from the admission pin rather than from a caller
    /// or current runtime state.
    admission_capture: CwdCapture,
    /// Checked `sys.InvocationRef` pinned to the durable admission capture.
    pub reference: InvocationRef,
    /// Exact durable invocation identity.
    pub invocation: InvocationId,
    /// Pinned source coordinate selected at protected admission.
    pub source_revision: SourceRevisionId,
    /// Pinned catalogue coordinate selected at protected admission.
    pub catalogue_revision: CatalogueRevisionId,
    /// Pinned resolved target identity selected at protected admission.
    pub function: FunctionId,
    /// Checked catalogue function witness, when the trusted owner resolved
    /// the corresponding row in the pinned catalogue object projection.
    pub function_reference: Option<FunctionRef>,
    /// Checked result type witness, when available at admission.
    pub result_type_reference: Option<TypeRef>,
    /// Closed lifecycle status, with no diagnostic detail.
    pub status: SealedInvocationObservationStatus,
    /// Durable admission time recorded by the sealed lifecycle relation.
    pub started: SystemTime,
    /// Durable terminal-publication time. Active observations never expose an
    /// end time; every terminal observation must expose one.
    pub ended: Option<SystemTime>,
    /// Declaration-ordered, redaction-safe argument metadata.
    pub arguments: Vec<SealedInvocationArgumentObservation>,
}

/// One declaration-ordered redaction-safe argument observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SealedInvocationArgumentObservation {
    /// Checked `sys.InvocationArgumentRef` pinned to the durable admission
    /// capture.
    pub reference: InvocationArgumentRef,
    /// Pinned declaration position.
    pub position: u64,
    /// Pinned parameter identity.
    pub parameter: orna_core::ParameterId,
    /// Pinned parameter name.
    pub name: String,
    /// Closed resolved-type family.
    pub type_kind: SealedInvocationArgumentTypeKind,
    /// Checked `sys.TypeRef` witness, when trusted admission retained one.
    pub type_reference: Option<TypeRef>,
    /// Explicit retained marker proving that this metadata may cross the
    /// public system-value boundary.
    redacted: bool,
}

/// Closed, value-free argument type metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SealedInvocationArgumentTypeKind {
    Scalar(String),
    Named(orna_core::TypeId),
    Reference(orna_core::TypeId),
    Value(orna_core::TypeId),
}

/// Closed durable status vocabulary for the invocation observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SealedInvocationObservationStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Orphaned,
}

/// The currently supportable, durable subset of one public
/// `sys.Invocation` row.
///
/// This is intentionally not a claim that the whole specification relation is
/// available. Unsupported or unavailable fields remain absent instead of
/// being reconstructed from implementation identifiers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableSysInvocationObservation {
    /// `sys.Invocation.reference`.
    pub reference: InvocationRef,
    /// `sys.Invocation.id`.
    pub id: InvocationId,
    /// `sys.Invocation.snapshot`, derived from the exact CWD capture pinned
    /// at durable admission.
    pub snapshot: SnapshotRef,
    /// The retained `sys.Invocation.arguments` children.
    pub arguments: Vec<DurableSysInvocationArgumentObservation>,
    /// `sys.Invocation.started`. The sealed lifecycle always records this
    /// durable instant.
    pub started: Option<SystemTime>,
    /// `sys.Invocation.ended`; this remains absent until terminal publication.
    pub ended: Option<SystemTime>,
    /// `sys.Invocation.status`, using the exact 1.0.0 closed vocabulary.
    pub status: InvocationStatus,
    /// `sys.Invocation.function`, backed by the retained catalogue ObjectRef
    /// witness and revalidated against the pinned admission capture.
    pub function: FunctionRef,
    /// `sys.Invocation.result_type`, when its TypeRef witness was retained and
    /// revalidated.
    pub result_type: Option<TypeRef>,
}

/// The currently supportable, durable subset of one public
/// `sys.InvocationArgument` row.
///
/// The durable metadata is always redacted. It retains a `sys.TypeRef` only
/// when the persisted catalogue witness is authoritative; otherwise the
/// argument is not projected. It never exposes a recoverable `sys.Value` or
/// the private value digest: hashes of protected, low-entropy values remain
/// dictionary-testable and must not cross the public system-value boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableSysInvocationArgumentObservation {
    /// `sys.InvocationArgument.reference`.
    pub reference: InvocationArgumentRef,
    /// `sys.InvocationArgument.invocation`.
    pub invocation: InvocationRef,
    /// `sys.InvocationArgument.name`.
    pub name: String,
    /// `sys.InvocationArgument.position`.
    pub position: u64,
    /// `sys.InvocationArgument.type`, backed by a checked retained catalogue
    /// witness. An argument without this required evidence is not projected.
    pub type_reference: TypeRef,
    /// `sys.InvocationArgument.digest`; protected argument observations do not
    /// expose the private value digest.
    pub digest: Option<[u8; 32]>,
    /// `sys.InvocationArgument.redacted`.
    pub redacted: bool,
}

impl SealedInvocationObservation {
    /// Converts checked private durable evidence into the public field subset
    /// that the current schema can prove without inventing references or
    /// optional relation ownership.
    pub fn durable_sys_projection(
        &self,
    ) -> Result<DurableSysInvocationObservation, PostgresKernelError> {
        let record = self.invocation.canonical();
        validate_invocation_reference(
            self.reference.clone().into_row_ref(),
            &self.admission_capture,
        )
        .map_err(|_| {
            observation_invariant(
                &record,
                "invocation reference disagrees with persisted admission capture",
            )
        })?;
        let expected_reference =
            invocation_observation_reference(&self.admission_capture, self.invocation, &record)?;
        if self.reference != expected_reference {
            return Err(observation_invariant(
                &record,
                "invocation reference disagrees with persisted invocation identity",
            ));
        }
        // The loader validates this shape, but the public conversion must
        // preserve the invariant itself: a malformed retained value must not
        // become a contradictory `sys.Invocation` projection through another
        // internal caller.
        validate_observation_timestamp_shape(self.status, self.started, self.ended, &record)?;
        let snapshot = snapshot_reference(
            self.admission_capture.database_id(),
            self.admission_capture.snapshot().clone(),
        )
        .map_err(|_| {
            observation_invariant(
                &record,
                "persisted admission capture cannot construct a snapshot reference",
            )
        })?;
        validate_argument_order(&self.arguments, &record)?;
        validate_argument_parameter_identity(&self.arguments, &record)?;
        validate_argument_public_metadata(&self.arguments, &record)?;
        let function = self
            .function_reference
            .as_ref()
            .ok_or_else(|| {
                observation_invariant(
                    &record,
                    "public invocation observation requires an authoritative function reference",
                )
            })
            .and_then(|reference| {
                validate_catalogue_function_reference(
                    reference,
                    &self.admission_capture,
                    self.function,
                    &record,
                )
            })?;
        let result_type = self
            .result_type_reference
            .as_ref()
            .map(|reference| {
                validate_catalogue_type_reference(reference, &self.admission_capture, None, &record)
            })
            .transpose()?;
        for argument in &self.arguments {
            let expected = invocation_argument_observation_reference(
                &self.admission_capture,
                self.invocation,
                argument.position,
                &record,
            )?;
            if argument.reference != expected {
                return Err(observation_invariant(
                    &record,
                    "argument reference disagrees with persisted admission capture",
                ));
            }
            let type_reference = argument.type_reference.as_ref().ok_or_else(|| {
                observation_invariant(
                    &record,
                    "public invocation argument requires an authoritative type reference",
                )
            })?;
            let expected_type = argument_type_reference_target(&argument.type_kind, &record)?;
            validate_catalogue_type_reference(
                type_reference,
                &self.admission_capture,
                Some(expected_type),
                &record,
            )?;
        }
        Ok(DurableSysInvocationObservation {
            reference: self.reference.clone(),
            id: self.invocation,
            snapshot,
            arguments: self
                .arguments
                .iter()
                .map(|argument| {
                    Ok(DurableSysInvocationArgumentObservation {
                        reference: argument.reference.clone(),
                        invocation: self.reference.clone(),
                        name: argument.name.clone(),
                        position: argument.position,
                        type_reference: argument.type_reference.clone().ok_or_else(|| {
                            observation_invariant(
                                &record,
                                "public invocation argument requires an authoritative type reference",
                            )
                        })?,
                        digest: None,
                        redacted: argument.redacted,
                    })
                })
                .collect::<Result<Vec<_>, PostgresKernelError>>()?,
            started: Some(self.started),
            ended: self.ended,
            status: self.status.into(),
            function,
            result_type,
        })
    }
}

impl From<SealedInvocationObservationStatus> for InvocationStatus {
    fn from(status: SealedInvocationObservationStatus) -> Self {
        match status {
            SealedInvocationObservationStatus::Queued => Self::Queued,
            SealedInvocationObservationStatus::Running => Self::Running,
            SealedInvocationObservationStatus::Succeeded => Self::Succeeded,
            SealedInvocationObservationStatus::Failed => Self::Failed,
            SealedInvocationObservationStatus::Cancelled => Self::Cancelled,
            SealedInvocationObservationStatus::Orphaned => Self::Orphaned,
        }
    }
}

impl SealedInvocationObservationStatus {
    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Orphaned
        )
    }

    fn decode(value: String, record: &str) -> Result<Self, PostgresKernelError> {
        match value.as_str() {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "orphaned" => Ok(Self::Orphaned),
            _ => Err(observation_invariant(
                record,
                "lifecycle status is not closed",
            )),
        }
    }
}

impl PostgresKernel {
    /// Loads the checked durable subset of one retained `sys.Invocation` row.
    ///
    /// This is a read-only historical observation: it uses the persisted
    /// admission capture and never starts, resumes, or otherwise mutates the
    /// invocation. Rows without coherent persisted admission evidence are
    /// excluded fail-closed. This does not make a `sys.rt` membership claim.
    pub async fn load_durable_sys_invocation_observation(
        &self,
        authenticated_session: &AuthenticatedSession,
        invocation: InvocationId,
    ) -> Result<Option<DurableSysInvocationObservation>, PostgresKernelError> {
        self.load_retained_sealed_invocation_observation(authenticated_session, invocation)
            .await?
            .map(|observation| observation.durable_sys_projection())
            .transpose()
    }

    /// Loads the checked durable subset of retained `sys.Invocation` rows in
    /// stable durable order.
    ///
    /// This collection is read-only and retains the loader's fail-closed
    /// treatment of legacy or malformed admission evidence. It intentionally
    /// does not filter for current-runtime membership.
    pub async fn load_durable_sys_invocation_observations(
        &self,
        authenticated_session: &AuthenticatedSession,
    ) -> Result<Vec<DurableSysInvocationObservation>, PostgresKernelError> {
        project_durable_sys_invocation_collection(
            self.load_retained_sealed_invocation_observation_collection(authenticated_session)
                .await?,
        )
    }

    /// Loads the checked `sys.rt.invocations` subset for the supplied current
    /// runtime identity.
    ///
    /// Unlike the durable canonical relation, this filters retained rows to
    /// the current database/runtime owner. The CWD generation is deliberately
    /// not part of membership: a live runtime may advance its CWD after an
    /// invocation is admitted, but that later advance must not rewrite the
    /// invocation's admission-pinned reference or snapshot.
    pub async fn load_current_runtime_sys_invocation_observations(
        &self,
        authenticated_session: &AuthenticatedSession,
        current_capture: &CwdCapture,
    ) -> Result<Vec<DurableSysInvocationObservation>, PostgresKernelError> {
        project_current_runtime_sys_invocation_collection(
            self.load_retained_sealed_invocation_observation_collection(authenticated_session)
                .await?,
            current_capture,
        )
    }

    /// Loads the checked durable `sys.InvocationArgument` relation in stable
    /// parent-plus-position order.
    ///
    /// This is a read-only relation boundary, not a second source of argument
    /// metadata: every child is first projected through its checked retained
    /// `sys.Invocation` parent from the same repeatable-read snapshot. That
    /// keeps the public natural key (`invocation + position`) bound to a
    /// retained parent and excludes malformed/legacy admissions before any
    /// child can be exposed. Physical `sys.TypeRef` and recoverable
    /// `sys.Value` fields remain unavailable rather than being fabricated.
    pub async fn load_durable_sys_invocation_argument_observations(
        &self,
        authenticated_session: &AuthenticatedSession,
    ) -> Result<Vec<DurableSysInvocationArgumentObservation>, PostgresKernelError> {
        project_durable_sys_invocation_argument_collection(
            self.load_retained_sealed_invocation_observation_collection(authenticated_session)
                .await?,
        )
    }

    /// Loads the checked `sys.rt.invocation_arguments` subset for the
    /// supplied current runtime identity.
    ///
    /// Children are selected only through their admitted parent, so this
    /// grouped handle cannot expose an argument whose invocation is excluded
    /// from `sys.rt.invocations`.
    pub async fn load_current_runtime_sys_invocation_argument_observations(
        &self,
        authenticated_session: &AuthenticatedSession,
        current_capture: &CwdCapture,
    ) -> Result<Vec<DurableSysInvocationArgumentObservation>, PostgresKernelError> {
        project_current_runtime_sys_invocation_argument_collection(
            self.load_retained_sealed_invocation_observation_collection(authenticated_session)
                .await?,
            current_capture,
        )
    }

    /// Loads one durable observation without starting, resuming, or otherwise
    /// mutating its invocation. The returned source, catalogue, and target
    /// coordinates are pinned at admission, not the database's current active
    /// revision. The caller is authenticated against the current security
    /// snapshot, but public references are reconstructed from durable
    /// admission evidence so a later CWD change cannot rewrite retained
    /// identity.
    pub async fn load_sealed_invocation_observation(
        &self,
        authenticated_session: &AuthenticatedSession,
        invocation: InvocationId,
    ) -> Result<Option<SealedInvocationObservation>, PostgresKernelError> {
        self.load_retained_sealed_invocation_observation(authenticated_session, invocation)
            .await
    }

    async fn load_retained_sealed_invocation_observation(
        &self,
        authenticated_session: &AuthenticatedSession,
        invocation: InvocationId,
    ) -> Result<Option<SealedInvocationObservation>, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .read_only(true)
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let (bound_session, granted) = self
                .authorised_sealed_invocation_observation_session(
                    &transaction,
                    authenticated_session,
                )
                .await?;
            let result =
                load_observation_by_id(self, &transaction, &bound_session, &granted, invocation)
                    .await?;
            transaction
                .rollback()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(result)
        }
        .await;
        finish_observation_session(operation, session.shutdown().await)
    }

    /// Loads all retained sealed-invocation observations in stable durable
    /// order without starting, resuming, or mutating any invocation.
    ///
    /// This retained collection makes no current-runtime membership claim.
    /// Durable admission evidence pins retained identity, but it does not by
    /// itself establish current runtime ownership or membership.
    ///
    /// Rows with incomplete target or admission coordinates are intentionally
    /// excluded: they are private unresolved-denial or legacy evidence, not
    /// public observations. Without `AnyInvocation`, this collection is
    /// limited to the authenticated principal's durable owner rows.
    pub async fn load_sealed_invocation_observations(
        &self,
        authenticated_session: &AuthenticatedSession,
    ) -> Result<Vec<SealedInvocationObservation>, PostgresKernelError> {
        self.load_retained_sealed_invocation_observation_collection(authenticated_session)
            .await
    }

    async fn load_retained_sealed_invocation_observation_collection(
        &self,
        authenticated_session: &AuthenticatedSession,
    ) -> Result<Vec<SealedInvocationObservation>, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .read_only(true)
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let (bound_session, granted) = self
                .authorised_sealed_invocation_observation_session(
                    &transaction,
                    authenticated_session,
                )
                .await?;
            let rows = if granted.contains(&InspectPrivilege::AnyInvocation) {
                transaction
                    .query(
                        "SELECT invocation_id FROM _orna_kernel.sealed_invocation_lifecycle \
                     WHERE source_revision_id IS NOT NULL \
                       AND catalogue_revision_id IS NOT NULL \
                       AND function_id IS NOT NULL \
                       AND function_reference IS NOT NULL \
                       AND admission_snapshot IS NOT NULL \
                       AND admission_generation_digest IS NOT NULL \
                       AND admission_runtime_id IS NOT NULL \
                       AND admission_runtime_generation IS NOT NULL \
                     ORDER BY started_at ASC, invocation_id ASC",
                        &[],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?
            } else {
                let owner = bound_session.principal().to_bytes().to_vec();
                transaction
                    .query(
                        "SELECT invocation_id FROM _orna_kernel.sealed_invocation_lifecycle \
                     WHERE owner_principal_id = $1 \
                       AND source_revision_id IS NOT NULL \
                       AND catalogue_revision_id IS NOT NULL \
                       AND function_id IS NOT NULL \
                       AND function_reference IS NOT NULL \
                       AND admission_snapshot IS NOT NULL \
                       AND admission_generation_digest IS NOT NULL \
                       AND admission_runtime_id IS NOT NULL \
                       AND admission_runtime_generation IS NOT NULL \
                     ORDER BY started_at ASC, invocation_id ASC",
                        &[&owner],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?
            };
            let mut observations = Vec::with_capacity(rows.len());
            for row in rows {
                let invocation = InvocationId::from_bytes(observation_id(
                    &row,
                    "sealed invocation observation collection",
                    "invocation_id",
                )?);
                let observation = load_observation_by_id(
                    self,
                    &transaction,
                    &bound_session,
                    &granted,
                    invocation,
                )
                .await?
                .ok_or_else(|| {
                    observation_invariant(
                        &invocation.canonical(),
                        "collection row disappeared during repeatable read",
                    )
                })?;
                observations.push(observation);
            }
            validate_observation_collection_capture(&observations)?;
            transaction
                .rollback()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(observations)
        }
        .await;
        finish_observation_session(operation, session.shutdown().await)
    }

    /// Rebinds the caller against the current security snapshot and resolves
    /// only class-wide INSPECT grants. Durable lifecycle rows retain no
    /// session identity, so `SessionInvocations` cannot be claimed here.
    async fn authorised_sealed_invocation_observation_session(
        &self,
        transaction: &tokio_postgres::Transaction<'_>,
        authenticated_session: &AuthenticatedSession,
    ) -> Result<(AuthenticatedSession, Vec<InspectPrivilege>), PostgresKernelError> {
        let active = configure_and_recover(transaction).await?;
        let security = recover_security_snapshot_for_active(transaction, &active).await?;
        let bound_session = match security.bind_authenticated_session(
            authenticated_session.principal(),
            authenticated_session.active_roles().to_vec(),
        ) {
            Ok(session) => session,
            Err(_) => {
                self.append_sealed_invocation_observation_denial_audit(
                    authenticated_session,
                    None,
                    InspectDenial::MissingPrivilege,
                )
                .await?;
                return Err(PostgresKernelError::InspectDenied {
                    reason: InspectDenial::MissingPrivilege,
                });
            }
        };
        let mut granted = vec![InspectPrivilege::OwnInvocation];
        granted.extend(inspect_privileges_for_session(&security, &bound_session));
        Ok((bound_session, granted))
    }

    async fn append_sealed_invocation_observation_denial_audit(
        &self,
        authenticated_session: &AuthenticatedSession,
        owner: Option<PrincipalId>,
        reason: InspectDenial,
    ) -> Result<(), PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(false)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            append_security_audit_event(
                &transaction,
                SecurityAuditDecision::inspect_denied(authenticated_session, owner, reason),
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(())
        }
        .await;
        finish_observation_session(operation, session.shutdown().await)
    }
}

fn project_durable_sys_invocation_collection(
    observations: Vec<SealedInvocationObservation>,
) -> Result<Vec<DurableSysInvocationObservation>, PostgresKernelError> {
    observations
        .into_iter()
        .map(|observation| observation.durable_sys_projection())
        .collect()
}

/// Projects the current-runtime live subset without resolving retained rows
/// against the caller's later CWD snapshot.
fn project_current_runtime_sys_invocation_collection(
    observations: Vec<SealedInvocationObservation>,
    current_capture: &CwdCapture,
) -> Result<Vec<DurableSysInvocationObservation>, PostgresKernelError> {
    observations
        .into_iter()
        .filter(|observation| observation_belongs_to_current_runtime(observation, current_capture))
        .map(|observation| observation.durable_sys_projection())
        .collect()
}

/// Flattens checked retained parent rows into the public durable
/// `sys.InvocationArgument` relation without changing their stable parent and
/// declaration-position order.
fn project_durable_sys_invocation_argument_collection(
    observations: Vec<SealedInvocationObservation>,
) -> Result<Vec<DurableSysInvocationArgumentObservation>, PostgresKernelError> {
    project_durable_sys_invocation_collection(observations).map(|observations| {
        observations
            .into_iter()
            .flat_map(|observation| observation.arguments)
            .collect()
    })
}

/// Projects the current-runtime argument relation through the same parent
/// selection as `sys.rt.invocations`.
fn project_current_runtime_sys_invocation_argument_collection(
    observations: Vec<SealedInvocationObservation>,
    current_capture: &CwdCapture,
) -> Result<Vec<DurableSysInvocationArgumentObservation>, PostgresKernelError> {
    project_current_runtime_sys_invocation_collection(observations, current_capture).map(
        |observations| {
            observations
                .into_iter()
                .flat_map(|observation| observation.arguments)
                .collect()
        },
    )
}

fn observation_belongs_to_current_runtime(
    observation: &SealedInvocationObservation,
    current_capture: &CwdCapture,
) -> bool {
    observation.admission_capture.database_id() == current_capture.database_id()
        && observation.admission_capture.runtime_id() == current_capture.runtime_id()
}

fn validate_observation_collection_capture(
    observations: &[SealedInvocationObservation],
) -> Result<(), PostgresKernelError> {
    for observation in observations {
        let record = observation.invocation.canonical();
        validate_invocation_reference(
            observation.reference.clone().into_row_ref(),
            &observation.admission_capture,
        )
        .map_err(|_| observation_invariant(&record, "collection contains an invalid capture"))?;
        for argument in &observation.arguments {
            validate_invocation_argument_reference(
                argument.reference.clone().into_row_ref(),
                &observation.admission_capture,
            )
            .map_err(|_| {
                observation_invariant(&record, "collection contains an invalid argument capture")
            })?;
        }
    }
    Ok(())
}

async fn load_observation_by_id(
    kernel: &PostgresKernel,
    transaction: &tokio_postgres::Transaction<'_>,
    authenticated_session: &AuthenticatedSession,
    granted: &[InspectPrivilege],
    invocation: InvocationId,
) -> Result<Option<SealedInvocationObservation>, PostgresKernelError> {
    let invocation_bytes = invocation.to_bytes().to_vec();
    let row = transaction
        .query_opt(
            "SELECT owner_principal_id, source_revision_id, catalogue_revision_id, function_id, status, \
                    started_at, ended_at, admission_snapshot, \
                    admission_generation_digest, admission_runtime_id, \
                    admission_runtime_generation, admission_writer_lease_owner, \
                    admission_writer_lease_epoch, function_reference, \
                    result_type_reference \
             FROM _orna_kernel.sealed_invocation_lifecycle \
             WHERE invocation_id = $1 \
               AND source_revision_id IS NOT NULL \
               AND catalogue_revision_id IS NOT NULL \
               AND function_id IS NOT NULL \
               AND function_reference IS NOT NULL \
               AND admission_snapshot IS NOT NULL \
               AND admission_generation_digest IS NOT NULL \
               AND admission_runtime_id IS NOT NULL \
               AND admission_runtime_generation IS NOT NULL",
            &[&invocation_bytes],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let record = invocation.canonical();
    let owner = observation_owner(&row, &record)?;
    require_sealed_invocation_observation_access(kernel, authenticated_session, owner, granted)
        .await?;
    validate_writer_lease_evidence(&row, &record)?;
    let capture = decode_admission_capture(&row, &record)?;
    let reference = invocation_observation_reference(&capture, invocation, &record)?;
    let source_revision =
        SourceRevisionId::from_bytes(observation_id(&row, &record, "source_revision_id")?);
    let catalogue_revision =
        CatalogueRevisionId::from_bytes(observation_id(&row, &record, "catalogue_revision_id")?);
    let function = FunctionId::from_bytes(observation_id(&row, &record, "function_id")?);
    let function_reference = decode_catalogue_function_reference(
        observation_optional_bytes(&row, &record, "function_reference")?,
        &capture,
        function,
        &record,
    )?
    .ok_or_else(|| {
        observation_invariant(
            &record,
            "accepted invocation observation must retain an authoritative function reference",
        )
    })?;
    let result_type_reference = decode_catalogue_type_reference(
        observation_optional_bytes(&row, &record, "result_type_reference")?,
        &capture,
        None,
        &record,
    )?;
    let status = SealedInvocationObservationStatus::decode(
        observation_column(&row, &record, "status")?,
        &record,
    )?;
    let (started, ended) = decode_observation_timestamps(&row, &status, &record)?;
    let catalogue_revision_bytes = catalogue_revision.to_bytes().to_vec();
    let function_bytes = function.to_bytes().to_vec();
    let argument_rows = transaction
        .query(
            "SELECT metadata.position, metadata.parameter_id, metadata.name, \
                    metadata.type_kind, metadata.scalar_type, metadata.target_type_id, \
                    metadata.type_reference, \
                    metadata.value_digest, metadata.redacted, \
                    declared.parameter_id AS declared_parameter_id \
             FROM _orna_kernel.sealed_invocation_argument_metadata AS metadata \
             LEFT JOIN _orna_kernel.catalogue_function_parameters AS declared \
               ON declared.catalogue_revision_id = $2 \
              AND declared.function_id = $3 \
              AND declared.parameter_id = metadata.parameter_id \
              AND declared.ordinal = metadata.position \
              AND declared.name = metadata.name \
             WHERE metadata.invocation_id = $1 \
             ORDER BY metadata.position ASC",
            &[
                &invocation_bytes,
                &catalogue_revision_bytes,
                &function_bytes,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let arguments = argument_rows
        .iter()
        .map(|argument| {
            let observation = decode_argument_observation(argument, &capture, invocation, &record)?;
            validate_argument_declaration_witness(
                &observation,
                observation_optional_id(argument, &record, "declared_parameter_id")?
                    .map(orna_core::ParameterId::from_bytes),
                &record,
            )?;
            Ok(observation)
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_argument_order(&arguments, &record)?;
    Ok(Some(SealedInvocationObservation {
        reference,
        invocation,
        source_revision,
        catalogue_revision,
        function,
        function_reference: Some(function_reference),
        admission_capture: capture,
        result_type_reference,
        status,
        started,
        ended,
        arguments,
    }))
}

async fn require_sealed_invocation_observation_access(
    kernel: &PostgresKernel,
    authenticated_session: &AuthenticatedSession,
    owner: PrincipalId,
    granted: &[InspectPrivilege],
) -> Result<(), PostgresKernelError> {
    match authorise_inspect(
        authenticated_session.principal(),
        InspectPrivilege::OwnInvocation,
        Some(owner),
        granted,
    ) {
        InspectDecision::Allowed { .. } => Ok(()),
        InspectDecision::Denied(reason) => {
            kernel
                .append_sealed_invocation_observation_denial_audit(
                    authenticated_session,
                    Some(owner),
                    reason,
                )
                .await?;
            Err(PostgresKernelError::InspectDenied { reason })
        }
    }
}

fn observation_owner(row: &Row, record: &str) -> Result<PrincipalId, PostgresKernelError> {
    let owner = observation_id(row, record, "owner_principal_id")?;
    if owner.iter().all(|byte| *byte == 0) {
        return Err(observation_invariant(
            record,
            "retained lifecycle row contains an empty owner principal identity",
        ));
    }
    Ok(PrincipalId::from_bytes(owner))
}

fn validate_writer_lease_evidence(row: &Row, record: &str) -> Result<(), PostgresKernelError> {
    let owner: Option<Vec<u8>> = row
        .try_get("admission_writer_lease_owner")
        .map_err(PostgresKernelError::Database)?;
    let epoch: Option<i64> = row
        .try_get("admission_writer_lease_epoch")
        .map_err(PostgresKernelError::Database)?;
    validate_writer_lease_evidence_fields(owner, epoch, record)
}

fn validate_writer_lease_evidence_fields(
    owner: Option<Vec<u8>>,
    epoch: Option<i64>,
    record: &str,
) -> Result<(), PostgresKernelError> {
    match (owner, epoch) {
        (None, None) => Ok(()),
        (Some(owner), Some(epoch))
            if owner.len() == 16 && owner.iter().any(|byte| *byte != 0) && epoch > 0 =>
        {
            Ok(())
        }
        _ => Err(observation_invariant(
            record,
            "retained lifecycle row contains malformed writer lease evidence",
        )),
    }
}

fn decode_admission_capture(row: &Row, record: &str) -> Result<CwdCapture, PostgresKernelError> {
    let encoded_snapshot: Vec<u8> = observation_column(row, record, "admission_snapshot")?;
    let digest: Vec<u8> = observation_column(row, record, "admission_generation_digest")?;
    let runtime: Vec<u8> = observation_column(row, record, "admission_runtime_id")?;
    let generation: i64 = observation_column(row, record, "admission_runtime_generation")?;
    decode_admission_capture_fields(encoded_snapshot, digest, runtime, generation, record)
}

fn decode_admission_capture_fields(
    encoded_snapshot: Vec<u8>,
    digest: Vec<u8>,
    runtime: Vec<u8>,
    generation: i64,
    record: &str,
) -> Result<CwdCapture, PostgresKernelError> {
    if encoded_snapshot.is_empty() {
        return Err(observation_invariant(
            record,
            "admission snapshot must not be empty",
        ));
    }
    let digest: [u8; 32] = digest.try_into().map_err(|_| {
        observation_invariant(record, "admission generation digest must be 32 bytes")
    })?;
    let runtime: [u8; 16] = runtime.try_into().map_err(|_| {
        observation_invariant(record, "admission runtime identity must be 16 bytes")
    })?;
    if generation < 0 {
        return Err(observation_invariant(
            record,
            "admission runtime generation must be nonnegative",
        ));
    }

    let value = Value::decode(&encoded_snapshot)
        .map_err(|_| observation_invariant(record, "admission snapshot is not canonical OVB"))?;
    let canonical = value.encode().map_err(|_| {
        observation_invariant(record, "admission snapshot cannot be canonically encoded")
    })?;
    if canonical != encoded_snapshot {
        return Err(observation_invariant(
            record,
            "admission snapshot encoding is not canonical",
        ));
    }
    let snapshot = Snapshot::decode(value.raw())
        .map_err(|_| observation_invariant(record, "admission snapshot is not a CWD snapshot"))?;
    let capture = CwdCapture::new(snapshot, digest)
        .map_err(|_| observation_invariant(record, "admission snapshot must be a CWD capture"))?;
    if capture.runtime_id() != runtime {
        return Err(observation_invariant(
            record,
            "admission runtime identity disagrees with snapshot",
        ));
    }
    let snapshot_generation: i64 = capture.generation().to_string().parse().map_err(|_| {
        observation_invariant(
            record,
            "admission snapshot generation is not a PostgreSQL bigint",
        )
    })?;
    if snapshot_generation != generation {
        return Err(observation_invariant(
            record,
            "admission runtime generation disagrees with snapshot",
        ));
    }
    Ok(capture)
}

fn decode_observation_timestamps(
    row: &Row,
    status: &SealedInvocationObservationStatus,
    record: &str,
) -> Result<(SystemTime, Option<SystemTime>), PostgresKernelError> {
    let started: SystemTime = observation_column(row, record, "started_at")?;
    let ended: Option<SystemTime> = observation_column(row, record, "ended_at")?;
    validate_observation_timestamp_shape(*status, started, ended, record)?;
    Ok((started, ended))
}

fn validate_observation_timestamp_shape(
    status: SealedInvocationObservationStatus,
    started: SystemTime,
    ended: Option<SystemTime>,
    record: &str,
) -> Result<(), PostgresKernelError> {
    match (status.is_terminal(), ended) {
        (false, None) => Ok(()),
        (true, Some(ended)) if ended >= started => Ok(()),
        (false, Some(_)) => Err(observation_invariant(
            record,
            "active lifecycle observation must not have an end time",
        )),
        (true, None) => Err(observation_invariant(
            record,
            "terminal lifecycle observation must have an end time",
        )),
        (true, Some(_)) => Err(observation_invariant(
            record,
            "lifecycle end time must not precede its start time",
        )),
    }
}

fn decode_argument_observation(
    row: &Row,
    capture: &CwdCapture,
    invocation: InvocationId,
    record: &str,
) -> Result<SealedInvocationArgumentObservation, PostgresKernelError> {
    let position: i64 = observation_column(row, record, "position")?;
    let position = u64::try_from(position)
        .map_err(|_| observation_invariant(record, "argument position must be nonnegative"))?;
    let parameter =
        orna_core::ParameterId::from_bytes(observation_id(row, record, "parameter_id")?);
    let name: String = observation_column(row, record, "name")?;
    if name.is_empty() {
        return Err(observation_invariant(
            record,
            "argument name must be nonempty",
        ));
    }
    let redacted: bool = observation_column(row, record, "redacted")?;
    if !redacted {
        return Err(observation_invariant(
            record,
            "argument observation must be redacted",
        ));
    }
    let type_kind = decode_argument_type_kind(
        observation_column(row, record, "type_kind")?,
        observation_column(row, record, "scalar_type")?,
        observation_optional_id(row, record, "target_type_id")?,
        record,
    )?;
    let type_reference = match observation_optional_bytes(row, record, "type_reference")? {
        Some(encoded) => decode_catalogue_type_reference(
            Some(encoded),
            capture,
            Some(argument_type_reference_target(&type_kind, record)?),
            record,
        )?,
        None => None,
    };
    // This private storage digest supports neither observation identity nor a
    // public projection. It is an unsalted hash of an argument value, so even
    // a low-entropy value is dictionary-testable. Validate the retained
    // storage shape, then discard it before constructing any observation.
    let digest: Vec<u8> = observation_column(row, record, "value_digest")?;
    validate_retained_argument_digest(&digest, record)?;
    Ok(SealedInvocationArgumentObservation {
        reference: invocation_argument_observation_reference(
            capture, invocation, position, record,
        )?,
        position,
        parameter,
        name,
        type_kind,
        type_reference,
        redacted,
    })
}

/// Checks the shape of private retained storage without admitting the digest
/// into the observable invocation model.
fn validate_retained_argument_digest(
    digest: &[u8],
    record: &str,
) -> Result<(), PostgresKernelError> {
    if digest.len() == 32 {
        Ok(())
    } else {
        Err(observation_invariant(
            record,
            "argument digest must be 32 bytes",
        ))
    }
}

fn invocation_observation_reference(
    capture: &CwdCapture,
    invocation: InvocationId,
    record: &str,
) -> Result<InvocationRef, PostgresKernelError> {
    invocation_reference(
        capture.database_id(),
        capture.snapshot().clone(),
        invocation.to_bytes(),
    )
    .map_err(|_| observation_invariant(record, "invocation reference coordinates are invalid"))
}

fn invocation_argument_observation_reference(
    capture: &CwdCapture,
    invocation: InvocationId,
    position: u64,
    record: &str,
) -> Result<InvocationArgumentRef, PostgresKernelError> {
    invocation_argument_reference(
        capture.database_id(),
        capture.snapshot().clone(),
        invocation.to_bytes(),
        position.into(),
    )
    .map_err(|_| observation_invariant(record, "argument reference coordinates are invalid"))
}

fn decode_argument_type_kind(
    type_kind: String,
    scalar_type: Option<String>,
    target_type_id: Option<[u8; 16]>,
    record: &str,
) -> Result<SealedInvocationArgumentTypeKind, PostgresKernelError> {
    match (type_kind.as_str(), scalar_type, target_type_id) {
        ("scalar", Some(scalar), None) if sealed_scalar_type_is_closed(&scalar) => {
            Ok(SealedInvocationArgumentTypeKind::Scalar(scalar))
        }
        ("named", None, Some(target)) => Ok(SealedInvocationArgumentTypeKind::Named(
            orna_core::TypeId::from_bytes(target),
        )),
        ("reference", None, Some(target)) => Ok(SealedInvocationArgumentTypeKind::Reference(
            orna_core::TypeId::from_bytes(target),
        )),
        ("value", None, Some(target)) => Ok(SealedInvocationArgumentTypeKind::Value(
            orna_core::TypeId::from_bytes(target),
        )),
        _ => Err(observation_invariant(
            record,
            "argument type metadata has an invalid shape",
        )),
    }
}

/// Returns the exact catalogue identity a retained argument type witness must
/// prove. Only named arguments have an authoritative type witness at this
/// boundary; all other retained type families stay unavailable.
fn argument_type_reference_target(
    type_kind: &SealedInvocationArgumentTypeKind,
    record: &str,
) -> Result<[u8; 16], PostgresKernelError> {
    match type_kind {
        SealedInvocationArgumentTypeKind::Named(type_id) => Ok(type_id.to_bytes()),
        SealedInvocationArgumentTypeKind::Reference(_)
        | SealedInvocationArgumentTypeKind::Value(_)
        | SealedInvocationArgumentTypeKind::Scalar(_) => Err(observation_invariant(
            record,
            "argument type witness requires an authoritative named type",
        )),
    }
}

fn decode_catalogue_function_reference(
    encoded: Option<Vec<u8>>,
    capture: &CwdCapture,
    expected_function: FunctionId,
    record: &str,
) -> Result<Option<FunctionRef>, PostgresKernelError> {
    encoded
        .map(|encoded| {
            let row = decode_row_reference(&encoded, record)?;
            let reference = validate_function_reference(row.clone(), capture).map_err(|_| {
                observation_invariant(record, "function witness has invalid capture or relation")
            })?;
            let object = decode_row_reference_raw(&row.key, record)?;
            validate_object_reference_for_id(object, capture, expected_function.to_bytes())
                .map_err(|_| {
                    observation_invariant(record, "function witness has the wrong object identity")
                })?;
            Ok(reference)
        })
        .transpose()
}

fn validate_catalogue_function_reference(
    reference: &FunctionRef,
    capture: &CwdCapture,
    expected_function: FunctionId,
    record: &str,
) -> Result<FunctionRef, PostgresKernelError> {
    let row = reference.as_row_ref().clone();
    let reference = validate_function_reference(row.clone(), capture).map_err(|_| {
        observation_invariant(record, "function witness has invalid capture or relation")
    })?;
    let object = decode_row_reference_raw(&row.key, record)?;
    validate_object_reference_for_id(object, capture, expected_function.to_bytes()).map_err(
        |_| observation_invariant(record, "function witness has the wrong object identity"),
    )?;
    Ok(reference)
}

fn decode_catalogue_type_reference(
    encoded: Option<Vec<u8>>,
    capture: &CwdCapture,
    expected_type: Option<[u8; 16]>,
    record: &str,
) -> Result<Option<TypeRef>, PostgresKernelError> {
    encoded
        .map(|encoded| {
            let row = decode_row_reference(&encoded, record)?;
            let reference = validate_type_reference(row.clone(), capture).map_err(|_| {
                observation_invariant(record, "type witness has invalid capture or relation")
            })?;
            let object = decode_row_reference_raw(&row.key, record)?;
            let object_id = object_id_from_object_reference(&object, record)?;
            validate_object_reference_for_id(object, capture, expected_type.unwrap_or(object_id))
                .map_err(|_| {
                observation_invariant(record, "type witness has the wrong object identity")
            })?;
            Ok(reference)
        })
        .transpose()
}

fn validate_catalogue_type_reference(
    reference: &TypeRef,
    capture: &CwdCapture,
    expected_type: Option<[u8; 16]>,
    record: &str,
) -> Result<TypeRef, PostgresKernelError> {
    let row = reference.as_row_ref().clone();
    let reference = validate_type_reference(row.clone(), capture).map_err(|_| {
        observation_invariant(record, "type witness has invalid capture or relation")
    })?;
    let object = decode_row_reference_raw(&row.key, record)?;
    let object_id = object_id_from_object_reference(&object, record)?;
    validate_object_reference_for_id(object, capture, expected_type.unwrap_or(object_id))
        .map_err(|_| observation_invariant(record, "type witness has the wrong object identity"))?;
    Ok(reference)
}

fn decode_row_reference(encoded: &[u8], record: &str) -> Result<RowRef, PostgresKernelError> {
    let value = Value::decode(encoded)
        .map_err(|_| observation_invariant(record, "witness is not canonical OVB"))?;
    let canonical = value
        .encode()
        .map_err(|_| observation_invariant(record, "witness cannot be canonically encoded"))?;
    if canonical != encoded {
        return Err(observation_invariant(
            record,
            "witness encoding is not canonical",
        ));
    }
    decode_row_reference_raw(value.raw(), record)
}

fn decode_row_reference_raw(raw: &OvbRaw, record: &str) -> Result<RowRef, PostgresKernelError> {
    let OvbRaw::Tag(60010, body) = raw else {
        return Err(observation_invariant(record, "witness is not a sys.RowRef"));
    };
    let OvbRaw::Array(fields) = body.as_ref() else {
        return Err(observation_invariant(
            record,
            "witness RowRef body is malformed",
        ));
    };
    let [database, table, key, snapshot] = fields.as_slice() else {
        return Err(observation_invariant(
            record,
            "witness RowRef has the wrong arity",
        ));
    };
    RowRef::new(
        reference_uuid(database, record)?,
        reference_uuid(table, record)?,
        key.clone(),
        Snapshot::decode(snapshot)
            .map_err(|_| observation_invariant(record, "witness snapshot is malformed"))?,
    )
    .map_err(|_| observation_invariant(record, "witness RowRef is not canonical"))
}

fn reference_uuid(raw: &OvbRaw, record: &str) -> Result<[u8; 16], PostgresKernelError> {
    let OvbRaw::Tag(37, bytes) = raw else {
        return Err(observation_invariant(record, "witness UUID is malformed"));
    };
    let OvbRaw::Bytes(bytes) = bytes.as_ref() else {
        return Err(observation_invariant(record, "witness UUID is malformed"));
    };
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| observation_invariant(record, "witness UUID must be 16 bytes"))
}

fn object_id_from_object_reference(
    object: &RowRef,
    record: &str,
) -> Result<[u8; 16], PostgresKernelError> {
    let OvbRaw::Array(fields) = &object.key else {
        return Err(observation_invariant(
            record,
            "witness object key is malformed",
        ));
    };
    let [object_id, _snapshot] = fields.as_slice() else {
        return Err(observation_invariant(
            record,
            "witness object key has the wrong arity",
        ));
    };
    reference_uuid(object_id, record)
}

fn sealed_scalar_type_is_closed(scalar: &str) -> bool {
    matches!(
        scalar,
        "boolean"
            | "integer"
            | "bigint"
            | "float"
            | "decimal"
            | "character_large_object"
            | "binary_large_object"
            | "uuid"
            | "date"
            | "time"
            | "timestamp"
            | "duration"
            | "void"
    )
}

fn validate_argument_order(
    arguments: &[SealedInvocationArgumentObservation],
    record: &str,
) -> Result<(), PostgresKernelError> {
    if arguments
        .windows(2)
        .any(|pair| pair[0].position >= pair[1].position)
    {
        return Err(observation_invariant(
            record,
            "argument observations must have distinct ascending positions",
        ));
    }
    Ok(())
}

/// The metadata carries each declared parameter identity privately so the
/// reader can confirm that separate public natural keys do not represent the
/// same parameter twice.  The public relation cannot expose that identity
/// without a proven `sys.FunctionRef`, but it must not project contradictory
/// children while those physical coordinates remain unavailable.
fn validate_argument_parameter_identity(
    arguments: &[SealedInvocationArgumentObservation],
    record: &str,
) -> Result<(), PostgresKernelError> {
    let mut parameters = BTreeSet::new();
    if arguments
        .iter()
        .any(|argument| !parameters.insert(argument.parameter))
    {
        return Err(observation_invariant(
            record,
            "argument observations must retain distinct declared parameters",
        ));
    }
    Ok(())
}

/// Confirms that every retained child still denotes the exact declared
/// parameter at the lifecycle row's pinned catalogue revision and function.
/// The SQL join supplies this witness only when its identity, name, and
/// declaration ordinal all agree; a missing witness is therefore not an
/// absent optional field and must not become a public argument projection.
fn validate_argument_declaration_witness(
    argument: &SealedInvocationArgumentObservation,
    declared_parameter: Option<orna_core::ParameterId>,
    record: &str,
) -> Result<(), PostgresKernelError> {
    if declared_parameter == Some(argument.parameter) {
        Ok(())
    } else {
        Err(observation_invariant(
            record,
            "argument observation must match its pinned declared parameter",
        ))
    }
}

/// Revalidates metadata that becomes visible in the redacted public argument
/// projection. The database decoder enforces this too, but public conversion
/// must fail closed when another retained-observation caller supplies malformed
/// evidence directly.
fn validate_argument_public_metadata(
    arguments: &[SealedInvocationArgumentObservation],
    record: &str,
) -> Result<(), PostgresKernelError> {
    if arguments.iter().any(|argument| argument.name.is_empty()) {
        return Err(observation_invariant(
            record,
            "redacted argument observation must retain a nonempty name",
        ));
    }
    if arguments.iter().any(|argument| !argument.redacted) {
        return Err(observation_invariant(
            record,
            "argument observation must be redacted before public projection",
        ));
    }
    Ok(())
}

fn observation_id(row: &Row, record: &str, column: &str) -> Result<[u8; 16], PostgresKernelError> {
    let value: Vec<u8> = observation_column(row, record, column)?;
    value
        .try_into()
        .map_err(|_| observation_invariant(record, "observation identity must be 16 bytes"))
}

fn observation_optional_id(
    row: &Row,
    record: &str,
    column: &str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    let value: Option<Vec<u8>> = observation_column(row, record, column)?;
    value
        .map(|value| {
            value
                .try_into()
                .map_err(|_| observation_invariant(record, "observation identity must be 16 bytes"))
        })
        .transpose()
}

fn observation_optional_bytes(
    row: &Row,
    record: &str,
    column: &str,
) -> Result<Option<Vec<u8>>, PostgresKernelError> {
    row.try_get(column).map_err(|_| {
        observation_invariant(record, "observation relation has an invalid witness layout")
    })
}

fn observation_column<T: FromSqlOwned>(
    row: &Row,
    record: &str,
    column: &str,
) -> Result<T, PostgresKernelError> {
    row.try_get(column).map_err(|_| {
        observation_invariant(record, "observation relation has an invalid row layout")
    })
}

fn observation_invariant(record: &str, rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "sealed invocation observation",
        record: record.to_owned(),
        rule,
    }
}

fn finish_observation_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_foundation_v1::{
        CanonicalSnapshot, OvbRaw, RowRef, SYS_INVOCATION_ARGUMENT_TABLE_ID,
        SYS_INVOCATION_TABLE_ID, SystemReferenceError, Value, function_reference, object_reference,
        type_reference, validate_invocation_argument_reference, validate_invocation_reference,
    };
    use sha2::{Digest, Sha256};

    fn capture(generation: u64) -> CwdCapture {
        capture_for_runtime([8; 16], generation)
    }

    fn capture_for_runtime(runtime: [u8; 16], generation: u64) -> CwdCapture {
        capture_for_database_and_runtime([7; 16], runtime, generation)
    }

    fn capture_for_database_and_runtime(
        database: [u8; 16],
        runtime: [u8; 16],
        generation: u64,
    ) -> CwdCapture {
        CwdCapture::new(
            CanonicalSnapshot::cwd(database, runtime, generation.into()).unwrap(),
            [9; 32],
        )
        .unwrap()
    }

    fn persisted_capture_fields(capture: &CwdCapture) -> (Vec<u8>, Vec<u8>, Vec<u8>, i64) {
        let snapshot = Value::new(capture.snapshot().raw())
            .unwrap()
            .encode()
            .unwrap();
        (
            snapshot,
            capture.generation_digest().to_vec(),
            capture.runtime_id().to_vec(),
            capture.generation().to_string().parse().unwrap(),
        )
    }

    #[test]
    fn observation_status_is_closed() {
        assert!(matches!(
            SealedInvocationObservationStatus::decode("succeeded".to_owned(), "test"),
            Ok(SealedInvocationObservationStatus::Succeeded)
        ));
        assert!(SealedInvocationObservationStatus::decode("restarted".to_owned(), "test").is_err());
    }

    #[test]
    fn argument_order_requires_distinct_declaration_positions() {
        let argument = |position| SealedInvocationArgumentObservation {
            reference: invocation_argument_observation_reference(
                &capture(1),
                InvocationId::from_bytes([2; 16]),
                position,
                "test",
            )
            .unwrap(),
            position,
            parameter: orna_core::ParameterId::from_bytes([1; 16]),
            name: "value".to_owned(),
            type_kind: SealedInvocationArgumentTypeKind::Scalar("integer".to_owned()),
            type_reference: None,
            redacted: true,
        };
        assert!(validate_argument_order(&[argument(0), argument(1)], "test").is_ok());
        assert!(validate_argument_order(&[argument(1), argument(1)], "test").is_err());
    }

    #[test]
    fn scalar_metadata_cannot_expand_the_closed_vocabulary() {
        assert!(sealed_scalar_type_is_closed("integer"));
        assert!(!sealed_scalar_type_is_closed("unredacted-value"));
    }

    #[test]
    fn observation_references_are_checked_and_snapshot_pinned() {
        let capture = capture(1);
        let invocation = InvocationId::from_bytes([2; 16]);
        let reference = invocation_observation_reference(&capture, invocation, "test").unwrap();
        let argument =
            invocation_argument_observation_reference(&capture, invocation, 3, "test").unwrap();

        assert_eq!(reference.as_row_ref().table_id, SYS_INVOCATION_TABLE_ID);
        assert_eq!(
            argument.as_row_ref().table_id,
            SYS_INVOCATION_ARGUMENT_TABLE_ID
        );
        assert_eq!(reference.as_row_ref().snapshot, *capture.snapshot());
        assert_eq!(argument.as_row_ref().snapshot, *capture.snapshot());
        assert!(validate_invocation_reference(reference.into_row_ref(), &capture).is_ok());
        assert!(validate_invocation_argument_reference(argument.into_row_ref(), &capture).is_ok());
    }

    #[test]
    fn observation_reference_validation_rejects_wrong_snapshot_relation_and_position() {
        let pinned_capture = capture(1);
        let invocation = InvocationId::from_bytes([2; 16]);
        let reference = invocation_observation_reference(&pinned_capture, invocation, "test")
            .unwrap()
            .into_row_ref();
        assert_eq!(
            validate_invocation_reference(reference.clone(), &capture(2)),
            Err(SystemReferenceError::SnapshotMismatch)
        );

        let wrong_relation = RowRef::new(
            pinned_capture.database_id(),
            SYS_INVOCATION_ARGUMENT_TABLE_ID,
            reference.key.clone(),
            pinned_capture.snapshot().clone(),
        )
        .unwrap();
        assert_eq!(
            validate_invocation_reference(wrong_relation, &pinned_capture),
            Err(SystemReferenceError::RelationMismatch)
        );

        let invalid_position = RowRef::new(
            pinned_capture.database_id(),
            SYS_INVOCATION_ARGUMENT_TABLE_ID,
            OvbRaw::Array(vec![]),
            pinned_capture.snapshot().clone(),
        )
        .unwrap();
        assert_eq!(
            validate_invocation_argument_reference(invalid_position, &pinned_capture),
            Err(SystemReferenceError::InvalidInvocationArgumentKey)
        );
    }

    #[test]
    fn retained_writer_lease_evidence_accepts_legacy_or_exact_pairs_only() {
        assert!(validate_writer_lease_evidence_fields(None, None, "test").is_ok());
        assert!(validate_writer_lease_evidence_fields(Some(vec![1; 16]), Some(2), "test").is_ok());
        assert!(validate_writer_lease_evidence_fields(Some(vec![1; 15]), Some(2), "test").is_err());
        assert!(validate_writer_lease_evidence_fields(Some(vec![0; 16]), Some(2), "test").is_err());
        assert!(validate_writer_lease_evidence_fields(Some(vec![1; 16]), Some(0), "test").is_err());
        assert!(validate_writer_lease_evidence_fields(Some(vec![1; 16]), None, "test").is_err());
    }

    fn observation(
        capture: &CwdCapture,
        invocation_byte: u8,
        status: SealedInvocationObservationStatus,
    ) -> SealedInvocationObservation {
        let invocation = InvocationId::from_bytes([invocation_byte; 16]);
        let function = FunctionId::from_bytes([5; 16]);
        let argument_type = orna_core::TypeId::from_bytes([8; 16]);
        SealedInvocationObservation {
            admission_capture: capture.clone(),
            reference: invocation_observation_reference(capture, invocation, "test").unwrap(),
            invocation,
            source_revision: SourceRevisionId::from_bytes([3; 16]),
            catalogue_revision: CatalogueRevisionId::from_bytes([4; 16]),
            function,
            function_reference: Some(retained_function_reference(capture, function)),
            result_type_reference: None,
            status,
            started: SystemTime::UNIX_EPOCH,
            ended: status.is_terminal().then_some(SystemTime::UNIX_EPOCH),
            arguments: vec![SealedInvocationArgumentObservation {
                reference: invocation_argument_observation_reference(
                    capture, invocation, 0, "test",
                )
                .unwrap(),
                position: 0,
                parameter: orna_core::ParameterId::from_bytes([6; 16]),
                name: "value".to_owned(),
                type_kind: SealedInvocationArgumentTypeKind::Named(argument_type),
                type_reference: Some(retained_type_reference(capture, argument_type)),
                redacted: true,
            }],
        }
    }

    fn retained_function_reference(capture: &CwdCapture, function_id: FunctionId) -> FunctionRef {
        function_reference(
            object_reference(
                capture.database_id(),
                function_id.to_bytes(),
                capture.snapshot().clone(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn retained_type_reference(capture: &CwdCapture, type_id: orna_core::TypeId) -> TypeRef {
        type_reference(
            object_reference(
                capture.database_id(),
                type_id.to_bytes(),
                capture.snapshot().clone(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn retained_collection_requires_valid_admission_capture_for_rows_and_arguments() {
        let current = capture(1);
        let retained = vec![
            observation(&current, 1, SealedInvocationObservationStatus::Running),
            observation(&current, 2, SealedInvocationObservationStatus::Succeeded),
        ];
        assert!(validate_observation_collection_capture(&retained).is_ok());
    }

    #[test]
    fn durable_sys_projection_maps_only_proven_public_fields() {
        let internal = observation(&capture(1), 2, SealedInvocationObservationStatus::Succeeded);

        let projection = internal.durable_sys_projection().unwrap();
        assert_eq!(projection.reference, internal.reference);
        assert_eq!(projection.id, internal.invocation);
        assert_eq!(
            projection.snapshot.as_row_ref().snapshot,
            *internal.admission_capture.snapshot()
        );
        assert_eq!(
            projection.snapshot.as_row_ref().database_id,
            internal.admission_capture.database_id()
        );
        assert_eq!(projection.started, Some(internal.started));
        assert_eq!(projection.ended, internal.ended);
        assert_eq!(projection.status, InvocationStatus::Succeeded);
        assert_eq!(projection.arguments.len(), 1);
        assert_eq!(
            projection.arguments[0].reference,
            internal.arguments[0].reference
        );
        assert_eq!(projection.arguments[0].invocation, internal.reference);
        assert_eq!(projection.arguments[0].name, internal.arguments[0].name);
        assert_eq!(
            projection.arguments[0].position,
            internal.arguments[0].position
        );
        assert_eq!(projection.arguments[0].digest, None);
        assert!(projection.arguments[0].redacted);
    }

    #[test]
    fn durable_sys_projection_rejects_missing_required_function_witness() {
        let mut internal =
            observation(&capture(1), 2, SealedInvocationObservationStatus::Succeeded);
        internal.function_reference = None;

        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn durable_sys_projection_rejects_missing_required_argument_type_witness() {
        let mut internal =
            observation(&capture(1), 2, SealedInvocationObservationStatus::Succeeded);
        internal.arguments[0].type_reference = None;

        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn durable_sys_projection_retains_matching_named_argument_type_witness() {
        let admitted = capture(1);
        let type_id = orna_core::TypeId::from_bytes([8; 16]);
        let mut internal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        internal.arguments[0].type_kind = SealedInvocationArgumentTypeKind::Named(type_id);
        internal.arguments[0].type_reference = Some(retained_type_reference(&admitted, type_id));

        let projection = internal.durable_sys_projection().unwrap();

        assert_eq!(
            projection.arguments[0].type_reference,
            internal.arguments[0].type_reference.clone().unwrap()
        );
    }

    #[test]
    fn durable_sys_projection_rejects_type_witness_for_reference_argument() {
        let admitted = capture(1);
        let type_id = orna_core::TypeId::from_bytes([8; 16]);
        let mut internal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        internal.arguments[0].type_kind = SealedInvocationArgumentTypeKind::Reference(type_id);
        internal.arguments[0].type_reference = Some(retained_type_reference(&admitted, type_id));

        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn durable_sys_projection_rejects_type_witness_for_value_argument() {
        let admitted = capture(1);
        let type_id = orna_core::TypeId::from_bytes([8; 16]);
        let mut internal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        internal.arguments[0].type_kind = SealedInvocationArgumentTypeKind::Value(type_id);
        internal.arguments[0].type_reference = Some(retained_type_reference(&admitted, type_id));

        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn durable_sys_projection_rejects_type_witness_for_scalar_argument() {
        let admitted = capture(1);
        let type_id = orna_core::TypeId::from_bytes([8; 16]);
        let mut internal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        internal.arguments[0].type_kind =
            SealedInvocationArgumentTypeKind::Scalar("integer".to_owned());
        internal.arguments[0].type_reference = Some(retained_type_reference(&admitted, type_id));

        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn retained_observation_discards_low_entropy_candidate_digest() {
        let internal = observation(&capture(1), 2, SealedInvocationObservationStatus::Running);
        let pin_digest: [u8; 32] = Sha256::digest(b"0000").into();
        let alternate_pin_digest: [u8; 32] = Sha256::digest(b"1234").into();
        assert!(validate_retained_argument_digest(&pin_digest, "test").is_ok());

        let projection = internal.durable_sys_projection().unwrap();
        let retained_debug = format!("{internal:?}");

        assert_eq!(projection.arguments[0].digest, None);
        assert_ne!(projection.arguments[0].digest, Some(pin_digest));
        assert_ne!(projection.arguments[0].digest, Some(alternate_pin_digest));
        assert!(!retained_debug.contains("value_digest"));
        assert!(!retained_debug.contains(&format!("{pin_digest:?}")));
        assert!(!retained_debug.contains(&format!("{alternate_pin_digest:?}")));
    }

    #[test]
    fn retained_observation_rejects_malformed_private_digest_shape() {
        assert!(validate_retained_argument_digest(&[0; 31], "test").is_err());
        assert!(validate_retained_argument_digest(&[0; 33], "test").is_err());
    }

    #[test]
    fn durable_sys_projection_exhaustively_omits_unsupported_fields() {
        let projection = observation(&capture(1), 2, SealedInvocationObservationStatus::Running)
            .durable_sys_projection()
            .unwrap();
        let DurableSysInvocationObservation {
            reference: _,
            id: _,
            snapshot: _,
            arguments,
            started: _,
            ended: _,
            status: _,
            function: _,
            result_type: _,
        } = projection;
        let DurableSysInvocationArgumentObservation {
            reference: _,
            invocation: _,
            name: _,
            position: _,
            digest: _,
            redacted: _,
            type_reference: _,
        } = &arguments[0];
    }

    #[test]
    fn durable_sys_projection_rejects_private_ids_that_disagree_with_catalogue_witnesses() {
        let admitted = capture(1);
        let retained = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);

        let mut different_private_ids = retained;
        different_private_ids.function = FunctionId::from_bytes([9; 16]);
        different_private_ids.arguments[0].type_kind =
            SealedInvocationArgumentTypeKind::Named(orna_core::TypeId::from_bytes([8; 16]));

        assert!(different_private_ids.durable_sys_projection().is_err());
    }

    #[test]
    fn durable_sys_projection_collection_preserves_retained_order_without_mutation() {
        let retained = vec![
            observation(&capture(1), 3, SealedInvocationObservationStatus::Running),
            observation(&capture(1), 2, SealedInvocationObservationStatus::Succeeded),
        ];
        let original = retained.clone();

        let projected = project_durable_sys_invocation_collection(retained).unwrap();

        assert_eq!(original[0].invocation, InvocationId::from_bytes([3; 16]));
        assert_eq!(original[1].invocation, InvocationId::from_bytes([2; 16]));
        assert_eq!(projected[0].id, original[0].invocation);
        assert_eq!(projected[1].id, original[1].invocation);
        assert_eq!(projected[0].status, InvocationStatus::Running);
        assert_eq!(projected[1].status, InvocationStatus::Succeeded);
    }

    #[test]
    fn durable_sys_argument_relation_preserves_checked_parent_and_position_order() {
        let admitted = capture(1);
        let mut first = observation(&admitted, 3, SealedInvocationObservationStatus::Running);
        first.arguments.push(SealedInvocationArgumentObservation {
            reference: invocation_argument_observation_reference(
                &admitted,
                first.invocation,
                1,
                "test",
            )
            .unwrap(),
            position: 1,
            parameter: orna_core::ParameterId::from_bytes([8; 16]),
            name: "next".to_owned(),
            type_kind: SealedInvocationArgumentTypeKind::Named(orna_core::TypeId::from_bytes(
                [8; 16],
            )),
            type_reference: Some(retained_type_reference(
                &admitted,
                orna_core::TypeId::from_bytes([8; 16]),
            )),
            redacted: true,
        });
        let retained = vec![
            first,
            observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded),
        ];
        let original = retained.clone();

        let projected = project_durable_sys_invocation_argument_collection(retained).unwrap();

        assert_eq!(projected.len(), 3);
        assert_eq!(projected[0].invocation, original[0].reference);
        assert_eq!(projected[0].position, 0);
        assert_eq!(projected[1].invocation, original[0].reference);
        assert_eq!(projected[1].position, 1);
        assert_eq!(projected[2].invocation, original[1].reference);
        assert_eq!(projected[2].position, 0);
        assert!(projected.iter().all(|argument| argument.redacted));
        assert_eq!(original[0].arguments.len(), 2);
        assert_eq!(original[1].arguments.len(), 1);
    }

    #[test]
    fn current_runtime_relations_exclude_prior_runtime_without_rewriting_pinned_snapshots() {
        let current_after_cwd_advance = capture_for_runtime([8; 16], 9);
        let admitted_before_cwd_advance = capture_for_runtime([8; 16], 1);
        let prior_runtime = capture_for_runtime([10; 16], 3);
        let retained = vec![
            observation(
                &admitted_before_cwd_advance,
                2,
                SealedInvocationObservationStatus::Running,
            ),
            observation(
                &prior_runtime,
                3,
                SealedInvocationObservationStatus::Succeeded,
            ),
        ];

        let invocations = project_current_runtime_sys_invocation_collection(
            retained.clone(),
            &current_after_cwd_advance,
        )
        .unwrap();
        let arguments = project_current_runtime_sys_invocation_argument_collection(
            retained,
            &current_after_cwd_advance,
        )
        .unwrap();

        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].id, InvocationId::from_bytes([2; 16]));
        assert_eq!(
            invocations[0].snapshot.as_row_ref().snapshot,
            *admitted_before_cwd_advance.snapshot()
        );
        assert_ne!(
            invocations[0].snapshot.as_row_ref().snapshot,
            *current_after_cwd_advance.snapshot()
        );
        assert_eq!(arguments.len(), 1);
        assert_eq!(arguments[0].invocation, invocations[0].reference);
        assert_eq!(arguments[0].digest, None);
    }

    #[test]
    fn current_runtime_relations_exclude_a_matching_runtime_from_another_database() {
        let current = capture_for_database_and_runtime([7; 16], [8; 16], 9);
        let admitted_here = capture_for_database_and_runtime([7; 16], [8; 16], 1);
        let admitted_elsewhere = capture_for_database_and_runtime([10; 16], [8; 16], 1);
        let retained = vec![
            observation(
                &admitted_here,
                2,
                SealedInvocationObservationStatus::Running,
            ),
            observation(
                &admitted_elsewhere,
                3,
                SealedInvocationObservationStatus::Succeeded,
            ),
        ];

        let invocations =
            project_current_runtime_sys_invocation_collection(retained.clone(), &current).unwrap();
        let arguments =
            project_current_runtime_sys_invocation_argument_collection(retained, &current).unwrap();

        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].id, InvocationId::from_bytes([2; 16]));
        assert_eq!(arguments.len(), 1);
        assert_eq!(arguments[0].invocation, invocations[0].reference);
    }

    #[test]
    fn durable_sys_projection_snapshot_remains_pinned_after_later_capture_changes() {
        let admitted = capture(1);
        let later = capture(2);
        let projection = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded)
            .durable_sys_projection()
            .unwrap();

        assert_eq!(
            projection.snapshot,
            snapshot_reference(admitted.database_id(), admitted.snapshot().clone()).unwrap()
        );
        assert_ne!(projection.snapshot.as_row_ref().snapshot, *later.snapshot());
        assert_ne!(
            projection.snapshot,
            snapshot_reference(later.database_id(), later.snapshot().clone()).unwrap()
        );
    }

    #[test]
    fn durable_sys_projection_rejects_malformed_admission_coordinates() {
        let admitted = capture(1);
        let mut internal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        let reference = internal.reference.as_row_ref();
        internal.reference = InvocationRef::from_row_ref(
            RowRef::new(
                admitted.database_id(),
                SYS_INVOCATION_ARGUMENT_TABLE_ID,
                reference.key.clone(),
                admitted.snapshot().clone(),
            )
            .unwrap(),
        );

        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn durable_sys_projection_rejects_reference_for_another_invocation() {
        let admitted = capture(1);
        let mut internal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        internal.reference =
            invocation_observation_reference(&admitted, InvocationId::from_bytes([3; 16]), "test")
                .unwrap();

        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn durable_sys_projection_rejects_argument_reference_outside_admission_identity() {
        let admitted = capture(1);
        let mut internal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        internal.arguments[0].reference = invocation_argument_observation_reference(
            &admitted,
            InvocationId::from_bytes([3; 16]),
            internal.arguments[0].position,
            "test",
        )
        .unwrap();
        assert!(internal.durable_sys_projection().is_err());

        internal.arguments[0].reference = invocation_argument_observation_reference(
            &admitted,
            internal.invocation,
            internal.arguments[0].position + 1,
            "test",
        )
        .unwrap();
        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn durable_sys_projection_rejects_unordered_argument_natural_keys() {
        let admitted = capture(1);
        let mut internal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        let position = internal.arguments[0].position;
        internal
            .arguments
            .push(SealedInvocationArgumentObservation {
                reference: invocation_argument_observation_reference(
                    &admitted,
                    internal.invocation,
                    position + 1,
                    "test",
                )
                .unwrap(),
                position: position + 1,
                parameter: orna_core::ParameterId::from_bytes([3; 16]),
                name: "later".to_owned(),
                type_kind: SealedInvocationArgumentTypeKind::Named(orna_core::TypeId::from_bytes(
                    [8; 16],
                )),
                type_reference: Some(retained_type_reference(
                    &admitted,
                    orna_core::TypeId::from_bytes([8; 16]),
                )),
                redacted: true,
            });
        internal.arguments.swap(0, 1);

        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn durable_sys_projection_rejects_duplicate_declared_parameter_metadata() {
        let admitted = capture(1);
        let mut internal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        let first = internal.arguments[0].clone();
        internal
            .arguments
            .push(SealedInvocationArgumentObservation {
                reference: invocation_argument_observation_reference(
                    &admitted,
                    internal.invocation,
                    first.position + 1,
                    "test",
                )
                .unwrap(),
                position: first.position + 1,
                parameter: first.parameter,
                name: "same-parameter".to_owned(),
                type_kind: first.type_kind.clone(),
                type_reference: first.type_reference.clone(),
                redacted: true,
            });

        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn retained_argument_requires_a_matching_pinned_declaration_witness() {
        let internal = observation(&capture(1), 2, SealedInvocationObservationStatus::Succeeded);
        let argument = &internal.arguments[0];

        assert!(
            validate_argument_declaration_witness(argument, Some(argument.parameter), "test",)
                .is_ok()
        );
        assert!(validate_argument_declaration_witness(argument, None, "test").is_err());
        assert!(
            validate_argument_declaration_witness(
                argument,
                Some(orna_core::ParameterId::from_bytes([9; 16])),
                "test",
            )
            .is_err()
        );
    }

    #[test]
    fn durable_sys_projection_rejects_redacted_argument_without_a_name() {
        let admitted = capture(1);
        let mut internal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        internal.arguments[0].name.clear();

        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn durable_sys_projection_rejects_unredacted_argument_metadata() {
        let admitted = capture(1);
        let mut internal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        internal.arguments[0].redacted = false;

        assert!(internal.durable_sys_projection().is_err());
    }

    #[test]
    fn durable_sys_projection_rejects_contradictory_terminal_timestamps() {
        let admitted = capture(1);
        let mut terminal = observation(&admitted, 2, SealedInvocationObservationStatus::Succeeded);
        terminal.ended = None;
        assert!(terminal.durable_sys_projection().is_err());

        let mut active = observation(&admitted, 3, SealedInvocationObservationStatus::Running);
        active.ended = Some(SystemTime::UNIX_EPOCH);
        assert!(active.durable_sys_projection().is_err());
    }

    #[test]
    fn persisted_admission_capture_reconstructs_snapshot_pinned_references() {
        let admitted = capture(7);
        let fields = persisted_capture_fields(&admitted);
        let recovered =
            decode_admission_capture_fields(fields.0, fields.1, fields.2, fields.3, "test")
                .unwrap();
        let invocation = InvocationId::from_bytes([2; 16]);
        let reference = invocation_observation_reference(&recovered, invocation, "test").unwrap();
        let argument =
            invocation_argument_observation_reference(&recovered, invocation, 3, "test").unwrap();

        assert_eq!(recovered, admitted);
        assert_eq!(reference.as_row_ref().database_id, admitted.database_id());
        assert_eq!(reference.as_row_ref().snapshot, *admitted.snapshot());
        assert_eq!(argument.as_row_ref().snapshot, *admitted.snapshot());
        assert!(validate_invocation_reference(reference.into_row_ref(), &admitted).is_ok());
        assert!(validate_invocation_argument_reference(argument.into_row_ref(), &admitted).is_ok());
    }

    #[test]
    fn persisted_admission_capture_rejects_malformed_or_mismatched_evidence() {
        let admitted = capture(7);
        let (snapshot, digest, runtime, generation) = persisted_capture_fields(&admitted);
        assert!(
            decode_admission_capture_fields(
                vec![0xff],
                digest.clone(),
                runtime.clone(),
                generation,
                "test",
            )
            .is_err()
        );
        assert!(
            decode_admission_capture_fields(
                snapshot.clone(),
                digest[..31].to_vec(),
                runtime.clone(),
                generation,
                "test",
            )
            .is_err()
        );
        assert!(
            decode_admission_capture_fields(
                snapshot.clone(),
                digest.clone(),
                runtime[..15].to_vec(),
                generation,
                "test",
            )
            .is_err()
        );
        assert!(
            decode_admission_capture_fields(
                snapshot.clone(),
                digest.clone(),
                vec![0; 16],
                generation,
                "test",
            )
            .is_err()
        );
        assert!(
            decode_admission_capture_fields(snapshot, digest, runtime, generation + 1, "test",)
                .is_err()
        );
    }

    #[test]
    fn persisted_admission_capture_is_stable_when_caller_cwd_changes() {
        let admitted = capture(7);
        let (snapshot, digest, runtime, generation) = persisted_capture_fields(&admitted);
        let recovered =
            decode_admission_capture_fields(snapshot, digest, runtime, generation, "test").unwrap();
        let caller_after_change = capture(8);
        let reference =
            invocation_observation_reference(&recovered, InvocationId::from_bytes([2; 16]), "test")
                .unwrap();

        assert_eq!(reference.as_row_ref().snapshot, *admitted.snapshot());
        assert_eq!(
            validate_invocation_reference(reference.into_row_ref(), &caller_after_change),
            Err(SystemReferenceError::SnapshotMismatch)
        );
    }

    #[test]
    fn timestamp_shape_requires_active_rows_to_be_open_and_terminal_rows_to_be_closed() {
        let start = SystemTime::UNIX_EPOCH;
        let end = start + std::time::Duration::from_secs(1);

        assert!(
            validate_observation_timestamp_shape(
                SealedInvocationObservationStatus::Queued,
                start,
                None,
                "test",
            )
            .is_ok()
        );
        assert!(
            validate_observation_timestamp_shape(
                SealedInvocationObservationStatus::Running,
                start,
                None,
                "test",
            )
            .is_ok()
        );
        assert!(
            validate_observation_timestamp_shape(
                SealedInvocationObservationStatus::Succeeded,
                start,
                Some(end),
                "test",
            )
            .is_ok()
        );

        assert!(
            validate_observation_timestamp_shape(
                SealedInvocationObservationStatus::Running,
                start,
                Some(end),
                "test",
            )
            .is_err()
        );
        assert!(
            validate_observation_timestamp_shape(
                SealedInvocationObservationStatus::Failed,
                start,
                None,
                "test",
            )
            .is_err()
        );
        assert!(
            validate_observation_timestamp_shape(
                SealedInvocationObservationStatus::Cancelled,
                end,
                Some(start),
                "test",
            )
            .is_err()
        );
    }
}
