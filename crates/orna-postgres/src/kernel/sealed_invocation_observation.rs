use super::{PostgresKernel, PostgresKernelError};

use orna_core::{CatalogueRevisionId, FunctionId, InvocationId, SourceRevisionId};
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
    /// mutating its invocation.  The returned coordinates are the snapshot
    /// and target pinned at admission, not the database's current active
    /// revision.
    pub async fn load_sealed_invocation_observation(
        &self,
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
            let invocation_bytes = invocation.to_bytes().to_vec();
            // Requiring every target coordinate at query time is intentional:
            // the private unresolved-denial row has no FunctionRef-equivalent
            // coordinate and is not part of this public observation boundary.
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
            let result = match row {
                None => None,
                Some(row) => {
                    let record = invocation.canonical();
                    let source_revision = SourceRevisionId::from_bytes(observation_id(
                        &row,
                        &record,
                        "source_revision_id",
                    )?);
                    let catalogue_revision = CatalogueRevisionId::from_bytes(observation_id(
                        &row,
                        &record,
                        "catalogue_revision_id",
                    )?);
                    let function =
                        FunctionId::from_bytes(observation_id(&row, &record, "function_id")?);
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
                        .map(|argument| decode_argument_observation(argument, &record))
                        .collect::<Result<Vec<_>, _>>()?;
                    validate_argument_order(&arguments, &record)?;
                    Some(SealedInvocationObservation {
                        invocation,
                        source_revision,
                        catalogue_revision,
                        function,
                        status,
                        arguments,
                    })
                }
            };
            transaction
                .rollback()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(result)
        }
        .await;
        finish_observation_session(operation, session.shutdown().await)
    }
}

fn decode_argument_observation(
    row: &Row,
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
        position,
        parameter,
        name,
        type_kind,
        value_digest,
    })
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
}
