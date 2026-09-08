use super::{PostgresKernel, PostgresKernelError};

use orna_core::{CatalogueRevisionId, FunctionId, InvocationId, SourceRevisionId};
use orna_foundation_v1::{
    CwdCapture, InvocationArgumentRef, InvocationRef, invocation_argument_reference,
    invocation_reference, validate_invocation_argument_reference, validate_invocation_reference,
};
use tokio_postgres::{IsolationLevel, Row, types::FromSqlOwned};

use crate::kernel::bootstrap::require_current_migrations;

/// Read-only durable observation of one resolved sealed invocation.
///
/// This is deliberately reconstructed from the lifecycle and argument
/// metadata relations, rather than from private security audit evidence or a
/// live operation.  A row is observable only when it retained the complete
/// pinned target tuple; unresolved target denials therefore cannot acquire a
/// synthetic public target identity through this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SealedInvocationObservation {
    /// Checked `sys.InvocationRef` pinned to the caller's CWD capture.
    pub reference: InvocationRef,
    /// Exact durable invocation identity.
    pub invocation: InvocationId,
    /// Pinned source coordinate selected at protected admission.
    pub source_revision: SourceRevisionId,
    /// Pinned catalogue coordinate selected at protected admission.
    pub catalogue_revision: CatalogueRevisionId,
    /// Pinned resolved target identity selected at protected admission.
    pub function: FunctionId,
    /// Closed lifecycle status, with no diagnostic detail.
    pub status: SealedInvocationObservationStatus,
    /// Declaration-ordered, redaction-safe argument metadata.
    pub arguments: Vec<SealedInvocationArgumentObservation>,
}

/// One declaration-ordered redaction-safe argument observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SealedInvocationArgumentObservation {
    /// Checked `sys.InvocationArgumentRef` pinned to the caller's CWD
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
    /// SHA-256 of the canonical typed value encoding; no value bytes are
    /// retained by this observation.
    pub value_digest: [u8; 32],
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

impl SealedInvocationObservationStatus {
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
    /// Loads one durable observation without starting, resuming, or otherwise
    /// mutating its invocation. The returned source, catalogue, and target
    /// coordinates are pinned at admission, not the database's current active
    /// revision. `capture` supplies the canonical snapshot pinned into each
    /// returned row reference; this lookup never reads or advances CWD state.
    pub async fn load_sealed_invocation_observation(
        &self,
        capture: &CwdCapture,
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
            let result = load_observation_by_id(&transaction, capture, invocation).await?;
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
    /// Lifecycle rows do not yet retain the admission evidence needed for a
    /// current-runtime projection.
    ///
    /// The supplied capture is the one coherent snapshot used to construct
    /// every public reference. Rows with incomplete pinned target coordinates
    /// are intentionally excluded: they are private unresolved-denial
    /// evidence, not public observations.
    pub async fn load_sealed_invocation_observations(
        &self,
        capture: &CwdCapture,
    ) -> Result<Vec<SealedInvocationObservation>, PostgresKernelError> {
        self.load_retained_sealed_invocation_observation_collection(capture)
            .await
    }

    async fn load_retained_sealed_invocation_observation_collection(
        &self,
        capture: &CwdCapture,
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
            let rows = transaction
                .query(
                    "SELECT invocation_id FROM _orna_kernel.sealed_invocation_lifecycle \
                     WHERE source_revision_id IS NOT NULL \
                       AND catalogue_revision_id IS NOT NULL \
                       AND function_id IS NOT NULL \
                     ORDER BY started_at ASC, invocation_id ASC",
                    &[],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            let mut observations = Vec::with_capacity(rows.len());
            for row in rows {
                let invocation = InvocationId::from_bytes(observation_id(
                    &row,
                    "sealed invocation observation collection",
                    "invocation_id",
                )?);
                let observation = load_observation_by_id(&transaction, capture, invocation)
                    .await?
                    .ok_or_else(|| {
                        observation_invariant(
                            &invocation.canonical(),
                            "collection row disappeared during repeatable read",
                        )
                    })?;
                observations.push(observation);
            }
            validate_observation_collection_capture(&observations, capture)?;
            transaction
                .rollback()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(observations)
        }
        .await;
        finish_observation_session(operation, session.shutdown().await)
    }
}

fn validate_observation_collection_capture(
    observations: &[SealedInvocationObservation],
    capture: &CwdCapture,
) -> Result<(), PostgresKernelError> {
    for observation in observations {
        let record = observation.invocation.canonical();
        validate_invocation_reference(observation.reference.clone().into_row_ref(), capture)
            .map_err(|_| observation_invariant(&record, "collection contains a mixed capture"))?;
        for argument in &observation.arguments {
            validate_invocation_argument_reference(
                argument.reference.clone().into_row_ref(),
                capture,
            )
            .map_err(|_| {
                observation_invariant(&record, "collection contains a mixed argument capture")
            })?;
        }
    }
    Ok(())
}

async fn load_observation_by_id(
    transaction: &tokio_postgres::Transaction<'_>,
    capture: &CwdCapture,
    invocation: InvocationId,
) -> Result<Option<SealedInvocationObservation>, PostgresKernelError> {
    let invocation_bytes = invocation.to_bytes().to_vec();
    let row = transaction
        .query_opt(
            "SELECT source_revision_id, catalogue_revision_id, function_id, status \
             FROM _orna_kernel.sealed_invocation_lifecycle \
             WHERE invocation_id = $1 \
               AND source_revision_id IS NOT NULL \
               AND catalogue_revision_id IS NOT NULL \
               AND function_id IS NOT NULL",
            &[&invocation_bytes],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let record = invocation.canonical();
    let reference = invocation_observation_reference(capture, invocation, &record)?;
    let source_revision =
        SourceRevisionId::from_bytes(observation_id(&row, &record, "source_revision_id")?);
    let catalogue_revision =
        CatalogueRevisionId::from_bytes(observation_id(&row, &record, "catalogue_revision_id")?);
    let function = FunctionId::from_bytes(observation_id(&row, &record, "function_id")?);
    let status = SealedInvocationObservationStatus::decode(
        observation_column(&row, &record, "status")?,
        &record,
    )?;
    let argument_rows = transaction
        .query(
            "SELECT position, parameter_id, name, type_kind, scalar_type, \
                    target_type_id, value_digest, redacted \
             FROM _orna_kernel.sealed_invocation_argument_metadata \
             WHERE invocation_id = $1 ORDER BY position ASC",
            &[&invocation_bytes],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let arguments = argument_rows
        .iter()
        .map(|argument| decode_argument_observation(argument, capture, invocation, &record))
        .collect::<Result<Vec<_>, _>>()?;
    validate_argument_order(&arguments, &record)?;
    Ok(Some(SealedInvocationObservation {
        reference,
        invocation,
        source_revision,
        catalogue_revision,
        function,
        status,
        arguments,
    }))
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
    let digest: Vec<u8> = observation_column(row, record, "value_digest")?;
    let value_digest: [u8; 32] = digest
        .try_into()
        .map_err(|_| observation_invariant(record, "argument digest must be 32 bytes"))?;
    Ok(SealedInvocationArgumentObservation {
        reference: invocation_argument_observation_reference(
            capture, invocation, position, record,
        )?,
        position,
        parameter,
        name,
        type_kind,
        value_digest,
    })
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
        SYS_INVOCATION_TABLE_ID, SystemReferenceError, validate_invocation_argument_reference,
        validate_invocation_reference,
    };

    fn capture(generation: u64) -> CwdCapture {
        CwdCapture::new(
            CanonicalSnapshot::cwd([7; 16], [8; 16], generation.into()).unwrap(),
            [9; 32],
        )
        .unwrap()
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
            value_digest: [0; 32],
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

    fn observation(
        capture: &CwdCapture,
        invocation_byte: u8,
        status: SealedInvocationObservationStatus,
    ) -> SealedInvocationObservation {
        let invocation = InvocationId::from_bytes([invocation_byte; 16]);
        SealedInvocationObservation {
            reference: invocation_observation_reference(capture, invocation, "test").unwrap(),
            invocation,
            source_revision: SourceRevisionId::from_bytes([3; 16]),
            catalogue_revision: CatalogueRevisionId::from_bytes([4; 16]),
            function: FunctionId::from_bytes([5; 16]),
            status,
            arguments: vec![SealedInvocationArgumentObservation {
                reference: invocation_argument_observation_reference(
                    capture, invocation, 0, "test",
                )
                .unwrap(),
                position: 0,
                parameter: orna_core::ParameterId::from_bytes([6; 16]),
                name: "value".to_owned(),
                type_kind: SealedInvocationArgumentTypeKind::Scalar("integer".to_owned()),
                value_digest: [7; 32],
            }],
        }
    }

    #[test]
    fn retained_collection_requires_one_coherent_capture_for_rows_and_arguments() {
        let current = capture(1);
        let retained = vec![
            observation(&current, 1, SealedInvocationObservationStatus::Running),
            observation(&current, 2, SealedInvocationObservationStatus::Succeeded),
        ];
        assert!(validate_observation_collection_capture(&retained, &current).is_ok());

        let mixed = vec![
            observation(&current, 1, SealedInvocationObservationStatus::Running),
            observation(&capture(2), 2, SealedInvocationObservationStatus::Succeeded),
        ];
        assert!(validate_observation_collection_capture(&mixed, &current).is_err());
    }
}
