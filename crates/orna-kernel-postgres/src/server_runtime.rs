//! Shared PostgreSQL SERVER runtime contracts.

use orna_core::{
    TypeId,
    catalogue::{FunctionDefinition, FunctionReturn},
    revision::{
        ActiveDatabaseRevision, DefinitionReference, DefinitionReferenceKind,
        DefinitionReferenceTarget,
    },
    types::{ResolvedType, StandardScalar},
};
use tokio_postgres::{Transaction, types::Type};

use crate::{
    PostgresKernelError, physical::establish_trusted_search_path, recovery::recover_active_revision,
};

const STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '30s'";
const LOCK_TIMEOUT: &str = "SET LOCAL lock_timeout = '5s'";

/// The closed current runtime classification of one resolved type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedRuntimeType {
    LegacyScalar(StandardScalar),
    Reference(TypeId),
    Unsupported,
}

impl ResolvedRuntimeType {
    /// Classifies one resolved type without assigning scalar or value identity.
    pub(crate) fn from_resolved_type(resolved_type: ResolvedType) -> Self {
        if let Some(scalar) = resolved_type.legacy_scalar() {
            return Self::LegacyScalar(scalar);
        }
        if let Some(target) = resolved_type.reference_target() {
            return Self::Reference(target);
        }
        if resolved_type.named_type().is_some() {
            return Self::Unsupported;
        }
        if resolved_type.value_type().is_some() {
            return Self::Unsupported;
        }
        Self::Unsupported
    }
}

pub(crate) async fn configure_and_recover(
    transaction: &Transaction<'_>,
) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
    establish_trusted_search_path(transaction).await?;
    transaction
        .batch_execute(STATEMENT_TIMEOUT)
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .batch_execute(LOCK_TIMEOUT)
        .await
        .map_err(PostgresKernelError::Database)?;
    recover_active_revision(transaction).await
}

pub(crate) fn postgres_type(resolved_type: ResolvedType) -> Option<Type> {
    match ResolvedRuntimeType::from_resolved_type(resolved_type) {
        ResolvedRuntimeType::LegacyScalar(StandardScalar::Boolean) => Some(Type::BOOL),
        ResolvedRuntimeType::LegacyScalar(StandardScalar::Integer) => Some(Type::INT4),
        ResolvedRuntimeType::LegacyScalar(StandardScalar::BigInt) => Some(Type::INT8),
        ResolvedRuntimeType::LegacyScalar(StandardScalar::Float) => Some(Type::FLOAT8),
        ResolvedRuntimeType::LegacyScalar(StandardScalar::CharacterLargeObject) => Some(Type::TEXT),
        ResolvedRuntimeType::LegacyScalar(StandardScalar::BinaryLargeObject)
        | ResolvedRuntimeType::Reference(_) => Some(Type::BYTEA),
        ResolvedRuntimeType::LegacyScalar(_) | ResolvedRuntimeType::Unsupported => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExpectedDefinitionReference {
    kind: DefinitionReferenceKind,
    target: DefinitionReferenceTarget,
}

impl ExpectedDefinitionReference {
    pub(crate) const fn new(
        kind: DefinitionReferenceKind,
        target: DefinitionReferenceTarget,
    ) -> Self {
        Self { kind, target }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReferenceReplayMismatch {
    Count,
    Sequence,
}

pub(crate) fn validate_function_reference_replay(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    body: &[ExpectedDefinitionReference],
) -> Result<(), ReferenceReplayMismatch> {
    let expected = expected_function_references(function, body);
    let actual = active
        .references()
        .iter()
        .filter(|reference| {
            reference.source_function() == function.id()
                && reference.source_revision() == function.current_revision()
        })
        .collect::<Vec<_>>();
    validate_reference_sequence(&actual, &expected)
}

fn expected_function_references(
    function: &FunctionDefinition,
    body: &[ExpectedDefinitionReference],
) -> Vec<ExpectedDefinitionReference> {
    let mut expected = Vec::with_capacity(body.len());
    for parameter in function.parameters() {
        add_signature_reference(&mut expected, parameter.resolved_type());
    }
    if let FunctionReturn::Rows(columns) = function.return_type() {
        for column in columns {
            add_signature_reference(&mut expected, column.resolved_type());
        }
    }
    expected.extend_from_slice(body);
    expected
}

fn add_signature_reference(
    expected: &mut Vec<ExpectedDefinitionReference>,
    resolved_type: ResolvedType,
) {
    if let ResolvedRuntimeType::Reference(target) =
        ResolvedRuntimeType::from_resolved_type(resolved_type)
    {
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(target),
        ));
    }
}

fn validate_reference_sequence(
    actual: &[&DefinitionReference],
    expected: &[ExpectedDefinitionReference],
) -> Result<(), ReferenceReplayMismatch> {
    if actual.len() != expected.len() {
        return Err(ReferenceReplayMismatch::Count);
    }
    for (ordinal, (reference, expected)) in actual.iter().zip(expected).enumerate() {
        if reference.ordinal() != ordinal as u32
            || reference.kind() != expected.kind
            || reference.target() != expected.target
        {
            return Err(ReferenceReplayMismatch::Sequence);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use orna_core::{
        FieldId, FunctionId, FunctionRevisionId, ParameterId, SourceUnitId, TypeId,
        catalogue::{
            FunctionDomain, FunctionReturnColumnDefinition, FunctionSecurity, FunctionVolatility,
            ParameterDefinition, QualifiedSemanticName,
        },
        revision::{DefinitionReference, SourceOrigin},
    };

    use super::*;

    #[test]
    fn resolved_runtime_type_classifies_every_current_shape_fail_closed() {
        let scalar_cases = [
            (StandardScalar::Boolean, Some(Type::BOOL)),
            (StandardScalar::Integer, Some(Type::INT4)),
            (StandardScalar::BigInt, Some(Type::INT8)),
            (StandardScalar::Float, Some(Type::FLOAT8)),
            (StandardScalar::Decimal, None),
            (StandardScalar::CharacterLargeObject, Some(Type::TEXT)),
            (StandardScalar::BinaryLargeObject, Some(Type::BYTEA)),
            (StandardScalar::Uuid, None),
            (StandardScalar::Date, None),
            (StandardScalar::Time, None),
            (StandardScalar::Timestamp, None),
            (StandardScalar::Duration, None),
            (StandardScalar::Void, None),
        ];
        assert_eq!(scalar_cases.len(), StandardScalar::ALL.len());
        for (scalar, postgres) in scalar_cases {
            let resolved = ResolvedType::scalar(scalar);
            assert_eq!(
                ResolvedRuntimeType::from_resolved_type(resolved),
                ResolvedRuntimeType::LegacyScalar(scalar)
            );
            assert_eq!(postgres_type(resolved), postgres);
        }

        let named = ResolvedType::named(TypeId::from_bytes([0x51; 16]));
        assert_eq!(
            ResolvedRuntimeType::from_resolved_type(named),
            ResolvedRuntimeType::Unsupported
        );
        assert_eq!(postgres_type(named), None);

        let target = TypeId::from_bytes([0x52; 16]);
        let reference = ResolvedType::reference(target);
        assert_eq!(
            ResolvedRuntimeType::from_resolved_type(reference),
            ResolvedRuntimeType::Reference(target)
        );
        assert_eq!(postgres_type(reference), Some(Type::BYTEA));
    }

    #[test]
    fn postgres_types_cover_the_exact_runtime_subset() {
        let supported = [
            (ResolvedType::scalar(StandardScalar::Boolean), Type::BOOL),
            (ResolvedType::scalar(StandardScalar::Integer), Type::INT4),
            (ResolvedType::scalar(StandardScalar::BigInt), Type::INT8),
            (ResolvedType::scalar(StandardScalar::Float), Type::FLOAT8),
            (
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                Type::TEXT,
            ),
            (
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                Type::BYTEA,
            ),
            (
                ResolvedType::reference(TypeId::from_bytes([0x55; 16])),
                Type::BYTEA,
            ),
        ];
        for (resolved_type, expected) in supported {
            assert_eq!(postgres_type(resolved_type), Some(expected));
        }
        for scalar in [
            StandardScalar::Decimal,
            StandardScalar::Uuid,
            StandardScalar::Date,
            StandardScalar::Time,
            StandardScalar::Timestamp,
            StandardScalar::Duration,
            StandardScalar::Void,
        ] {
            assert_eq!(postgres_type(ResolvedType::scalar(scalar)), None);
        }
        assert_eq!(
            postgres_type(ResolvedType::named(TypeId::from_bytes([0x56; 16]))),
            None
        );
    }

    #[test]
    fn reference_replay_puts_signature_references_before_body_evidence() {
        let parameter_target = TypeId::from_bytes([0x61; 16]);
        let result_target = TypeId::from_bytes([0x62; 16]);
        let body_target = TypeId::from_bytes([0x63; 16]);
        let function = FunctionDefinition::new(
            FunctionId::from_bytes([0x64; 16]),
            QualifiedSemanticName::new(["test", "function"]).unwrap(),
            FunctionDomain::Server,
            vec![
                ParameterDefinition::new(
                    ParameterId::from_bytes([0x65; 16]),
                    "ignored_scalar",
                    0,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    None,
                ),
                ParameterDefinition::new(
                    ParameterId::from_bytes([0x66; 16]),
                    "reference",
                    1,
                    ResolvedType::reference(parameter_target),
                    None,
                ),
            ],
            FunctionReturn::Rows(vec![
                FunctionReturnColumnDefinition::new(
                    "reference",
                    0,
                    ResolvedType::reference(result_target),
                ),
                FunctionReturnColumnDefinition::new(
                    "ignored_scalar",
                    1,
                    ResolvedType::scalar(StandardScalar::Integer),
                ),
            ]),
            FunctionRevisionId::from_bytes([0x67; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Stable,
        );
        let body = [ExpectedDefinitionReference::new(
            DefinitionReferenceKind::QueryObject,
            DefinitionReferenceTarget::ObjectType(body_target),
        )];

        assert_eq!(
            expected_function_references(&function, &body),
            vec![
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(parameter_target),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(result_target),
                ),
                body[0],
            ]
        );
    }

    #[test]
    fn reference_replay_rejects_missing_extra_and_reordered_records() {
        let function = FunctionId::from_bytes([0x71; 16]);
        let revision = FunctionRevisionId::from_bytes([0x72; 16]);
        let first = DefinitionReferenceTarget::ObjectType(TypeId::from_bytes([0x73; 16]));
        let second = DefinitionReferenceTarget::Field {
            owner: TypeId::from_bytes([0x74; 16]),
            field: FieldId::from_bytes([0x75; 16]),
        };
        let expected = [
            ExpectedDefinitionReference::new(DefinitionReferenceKind::QueryObject, first),
            ExpectedDefinitionReference::new(DefinitionReferenceKind::QueryField, second),
        ];
        let source = SourceOrigin::new(SourceUnitId::from_bytes([0x76; 16]), 0, 0).unwrap();
        let records = [
            DefinitionReference::new(
                function,
                revision,
                0,
                first,
                DefinitionReferenceKind::QueryObject,
                source,
            ),
            DefinitionReference::new(
                function,
                revision,
                1,
                second,
                DefinitionReferenceKind::QueryField,
                source,
            ),
        ];
        assert!(validate_reference_sequence(&[&records[0], &records[1]], &expected).is_ok());
        assert_eq!(
            validate_reference_sequence(&[&records[0]], &expected),
            Err(ReferenceReplayMismatch::Count)
        );
        assert_eq!(
            validate_reference_sequence(&[&records[0], &records[1], &records[1]], &expected),
            Err(ReferenceReplayMismatch::Count)
        );
        assert_eq!(
            validate_reference_sequence(&[&records[1], &records[0]], &expected),
            Err(ReferenceReplayMismatch::Sequence)
        );
    }
}
