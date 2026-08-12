//! Shared PostgreSQL SERVER runtime contracts.

use orna_core::{
    TypeId,
    catalogue::{CatalogueSnapshot, FunctionDefinition, FunctionReturn},
    revision::{
        ActiveDatabaseRevision, CatalogueHashContext, DefinitionReference, DefinitionReferenceKind,
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
    VerifiedValue {
        value_type: TypeId,
        compatibility: StandardScalar,
    },
    CatalogueEnum(TypeId),
    Record(TypeId),
    Reference(TypeId),
    Unsupported,
}

impl ResolvedRuntimeType {
    /// Returns the resolved representation without assigning identity.
    pub(crate) const fn compatibility_scalar(self) -> Option<StandardScalar> {
        match self {
            Self::LegacyScalar(scalar)
            | Self::VerifiedValue {
                compatibility: scalar,
                ..
            } => Some(scalar),
            Self::CatalogueEnum(_) | Self::Record(_) | Self::Reference(_) | Self::Unsupported => {
                None
            }
        }
    }
}

/// Projects a resolved type through the selected catalogue hash context.
///
/// Legacy scalar tags remain executable-plan compatibility data. Durable value
/// identities resolve only through the selected pinned standard snapshot. The
/// downstream operation allow-lists decide which recognised representations
/// this initial runtime can execute.
pub(crate) fn resolve_runtime_type(
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> ResolvedRuntimeType {
    if let Some(scalar) = resolved_type.legacy_scalar() {
        return runtime_type_from_legacy_scalar(scalar);
    }
    if let Some(target) = resolved_type.reference_target() {
        return ResolvedRuntimeType::Reference(target);
    }
    if resolved_type.named_type().is_some() {
        return ResolvedRuntimeType::Unsupported;
    }
    if let Some(value_type) = resolved_type.value_type() {
        return context
            .standard()
            .and_then(|standard| standard.catalogue().value_type_by_id(value_type))
            .and_then(|definition| {
                runtime_compatibility_from_contract(definition.representation_contract())
            })
            .map_or(ResolvedRuntimeType::Unsupported, |compatibility| {
                ResolvedRuntimeType::VerifiedValue {
                    value_type,
                    compatibility,
                }
            });
    }
    ResolvedRuntimeType::Unsupported
}

/// Resolves application enum identities through the active catalogue.
pub(crate) fn resolve_catalogue_runtime_type(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    resolved_type: ResolvedType,
) -> ResolvedRuntimeType {
    if let Some(enum_type) = resolved_type.named_type()
        && catalogue.enum_type_by_id(enum_type).is_some()
    {
        return ResolvedRuntimeType::CatalogueEnum(enum_type);
    }
    if let Some(record_type) = resolved_type.named_type()
        && catalogue.record_value_type_by_id(record_type).is_some()
    {
        return ResolvedRuntimeType::Record(record_type);
    }
    resolve_runtime_type(context, resolved_type)
}

fn runtime_type_from_legacy_scalar(scalar: StandardScalar) -> ResolvedRuntimeType {
    ResolvedRuntimeType::LegacyScalar(scalar)
}

fn runtime_compatibility_from_contract(contract: &str) -> Option<StandardScalar> {
    match contract {
        "orna.kernel.value.boolean@1" => Some(StandardScalar::Boolean),
        "orna.kernel.value.integer@1" => Some(StandardScalar::Integer),
        "orna.kernel.value.bigint@1" => Some(StandardScalar::BigInt),
        "orna.kernel.value.float@1" => Some(StandardScalar::Float),
        "orna.kernel.value.character-large-object@1" => Some(StandardScalar::CharacterLargeObject),
        "orna.kernel.value.binary-large-object@1" => Some(StandardScalar::BinaryLargeObject),
        "orna.kernel.value.decimal@1" => Some(StandardScalar::Decimal),
        "orna.kernel.value.uuid@1" => Some(StandardScalar::Uuid),
        "orna.kernel.value.date@1" => Some(StandardScalar::Date),
        "orna.kernel.value.time@1" => Some(StandardScalar::Time),
        "orna.kernel.value.timestamp@1" => Some(StandardScalar::Timestamp),
        "orna.kernel.value.duration@1" => Some(StandardScalar::Duration),
        "orna.kernel.value.void@1" => Some(StandardScalar::Void),
        _ => None,
    }
}

/// Checks runtime compatibility while retaining a verified value identity.
pub(crate) fn runtime_types_match(
    context: &CatalogueHashContext,
    left: ResolvedType,
    right: ResolvedType,
) -> bool {
    if left == right {
        return true;
    }
    match (
        resolve_runtime_type(context, left),
        resolve_runtime_type(context, right),
    ) {
        (
            ResolvedRuntimeType::LegacyScalar(left),
            ResolvedRuntimeType::VerifiedValue {
                compatibility: right,
                ..
            },
        )
        | (
            ResolvedRuntimeType::VerifiedValue {
                compatibility: left,
                ..
            },
            ResolvedRuntimeType::LegacyScalar(right),
        ) => left == right,
        _ => false,
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

pub(crate) fn postgres_type(runtime_type: ResolvedRuntimeType) -> Option<Type> {
    match runtime_type {
        ResolvedRuntimeType::LegacyScalar(StandardScalar::Boolean)
        | ResolvedRuntimeType::VerifiedValue {
            compatibility: StandardScalar::Boolean,
            ..
        } => Some(Type::BOOL),
        ResolvedRuntimeType::LegacyScalar(StandardScalar::Integer)
        | ResolvedRuntimeType::VerifiedValue {
            compatibility: StandardScalar::Integer,
            ..
        } => Some(Type::INT4),
        ResolvedRuntimeType::LegacyScalar(StandardScalar::BigInt)
        | ResolvedRuntimeType::VerifiedValue {
            compatibility: StandardScalar::BigInt,
            ..
        } => Some(Type::INT8),
        ResolvedRuntimeType::LegacyScalar(StandardScalar::Float)
        | ResolvedRuntimeType::VerifiedValue {
            compatibility: StandardScalar::Float,
            ..
        } => Some(Type::FLOAT8),
        ResolvedRuntimeType::LegacyScalar(StandardScalar::CharacterLargeObject)
        | ResolvedRuntimeType::VerifiedValue {
            compatibility: StandardScalar::CharacterLargeObject,
            ..
        } => Some(Type::TEXT),
        ResolvedRuntimeType::LegacyScalar(StandardScalar::BinaryLargeObject)
        | ResolvedRuntimeType::VerifiedValue {
            compatibility: StandardScalar::BinaryLargeObject,
            ..
        }
        | ResolvedRuntimeType::Record(_)
        | ResolvedRuntimeType::Reference(_) => Some(Type::BYTEA),
        ResolvedRuntimeType::CatalogueEnum(_) => Some(Type::TEXT),
        ResolvedRuntimeType::LegacyScalar(_)
        | ResolvedRuntimeType::VerifiedValue { .. }
        | ResolvedRuntimeType::Unsupported => None,
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
    if let Some(value_type) = resolved_type.value_type() {
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::NamedType,
            DefinitionReferenceTarget::ValueType(value_type),
        ));
    } else if let Some(enum_type) = resolved_type.named_type() {
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::NamedType,
            DefinitionReferenceTarget::ValueType(enum_type),
        ));
    } else if let Some(target) = resolved_type.reference_target() {
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
        CatalogueRevisionId, FieldId, FunctionId, FunctionRevisionId, ParameterId, SchemaId,
        SourceUnitId, TypeId,
        catalogue::{
            EnumTypeDefinition, FunctionDomain, FunctionReturnColumnDefinition, FunctionSecurity,
            FunctionVolatility, ParameterDefinition, QualifiedSemanticName,
            RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition,
        },
        revision::{CatalogueHashContext, DefinitionReference, SourceOrigin},
    };

    use super::*;

    #[test]
    fn resolved_runtime_type_classifies_legacy_shapes_and_postgres_types() {
        let context = CatalogueHashContext::version_one();
        let scalar_cases = [
            (
                StandardScalar::Boolean,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::Boolean),
                Some(Type::BOOL),
            ),
            (
                StandardScalar::Integer,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::Integer),
                Some(Type::INT4),
            ),
            (
                StandardScalar::BigInt,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::BigInt),
                Some(Type::INT8),
            ),
            (
                StandardScalar::Float,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::Float),
                Some(Type::FLOAT8),
            ),
            (
                StandardScalar::Decimal,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::Decimal),
                None,
            ),
            (
                StandardScalar::CharacterLargeObject,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::CharacterLargeObject),
                Some(Type::TEXT),
            ),
            (
                StandardScalar::BinaryLargeObject,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::BinaryLargeObject),
                Some(Type::BYTEA),
            ),
            (
                StandardScalar::Uuid,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::Uuid),
                None,
            ),
            (
                StandardScalar::Date,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::Date),
                None,
            ),
            (
                StandardScalar::Time,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::Time),
                None,
            ),
            (
                StandardScalar::Timestamp,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::Timestamp),
                None,
            ),
            (
                StandardScalar::Duration,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::Duration),
                None,
            ),
            (
                StandardScalar::Void,
                ResolvedRuntimeType::LegacyScalar(StandardScalar::Void),
                None,
            ),
        ];
        assert_eq!(scalar_cases.len(), StandardScalar::ALL.len());
        for (scalar, runtime, postgres) in scalar_cases {
            let resolved = ResolvedType::scalar(scalar);
            assert_eq!(resolve_runtime_type(&context, resolved), runtime);
            assert_eq!(postgres_type(runtime), postgres);
        }

        let named = ResolvedType::named(TypeId::from_bytes([0x51; 16]));
        assert_eq!(
            resolve_runtime_type(&context, named),
            ResolvedRuntimeType::Unsupported
        );
        assert_eq!(postgres_type(resolve_runtime_type(&context, named)), None);

        let target = TypeId::from_bytes([0x52; 16]);
        let reference = ResolvedType::reference(target);
        assert_eq!(
            resolve_runtime_type(&context, reference),
            ResolvedRuntimeType::Reference(target)
        );
        assert_eq!(
            postgres_type(resolve_runtime_type(&context, reference)),
            Some(Type::BYTEA)
        );
    }

    #[test]
    fn active_catalogue_classifies_declared_named_runtime_types() {
        let enum_type = TypeId::from_bytes([0x53; 16]);
        let record_type = TypeId::from_bytes([0x57; 16]);
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes([0x54; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x55; 16]),
                QualifiedSemanticName::new(["app"]).unwrap(),
            )],
            Vec::new(),
            Vec::new(),
            vec![EnumTypeDefinition::new(
                enum_type,
                QualifiedSemanticName::new(["app", "stage"]).unwrap(),
                ["lead"],
            )],
            vec![RecordValueTypeDefinition::new(
                record_type,
                QualifiedSemanticName::new(["app", "flag"]).unwrap(),
                vec![RecordValueFieldDefinition::new(
                    FieldId::from_bytes([0x58; 16]),
                    "stage",
                    0,
                    ResolvedType::named(enum_type),
                )],
            )],
            Vec::new(),
        )
        .unwrap();
        let context = CatalogueHashContext::version_one();

        assert_eq!(
            resolve_catalogue_runtime_type(&catalogue, &context, ResolvedType::named(enum_type)),
            ResolvedRuntimeType::CatalogueEnum(enum_type)
        );
        assert_eq!(
            postgres_type(ResolvedRuntimeType::CatalogueEnum(enum_type)),
            Some(Type::TEXT)
        );
        assert_eq!(
            resolve_catalogue_runtime_type(&catalogue, &context, ResolvedType::named(record_type),),
            ResolvedRuntimeType::Record(record_type)
        );
        assert_eq!(
            postgres_type(ResolvedRuntimeType::Record(record_type)),
            Some(Type::BYTEA)
        );
        assert_eq!(
            resolve_catalogue_runtime_type(
                &catalogue,
                &context,
                ResolvedType::named(TypeId::from_bytes([0x56; 16])),
            ),
            ResolvedRuntimeType::Unsupported
        );
    }

    #[test]
    fn postgres_types_cover_the_exact_runtime_subset() {
        let context = CatalogueHashContext::version_one();
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
            assert_eq!(
                postgres_type(resolve_runtime_type(&context, resolved_type)),
                Some(expected)
            );
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
            assert_eq!(
                postgres_type(resolve_runtime_type(&context, ResolvedType::scalar(scalar))),
                None
            );
        }
        assert_eq!(
            postgres_type(resolve_runtime_type(
                &context,
                ResolvedType::named(TypeId::from_bytes([0x56; 16]))
            )),
            None
        );
    }

    #[test]
    fn retained_version_two_value_contracts_match_legacy_runtime_capabilities() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard-library snapshot"),
        )
        .expect("verified standard-library snapshot");
        let context = CatalogueHashContext::version_two(standard);
        let cases = [
            (
                "orna.kernel.value.boolean@1",
                StandardScalar::Boolean,
                Some(Type::BOOL),
            ),
            (
                "orna.kernel.value.integer@1",
                StandardScalar::Integer,
                Some(Type::INT4),
            ),
            (
                "orna.kernel.value.bigint@1",
                StandardScalar::BigInt,
                Some(Type::INT8),
            ),
            (
                "orna.kernel.value.float@1",
                StandardScalar::Float,
                Some(Type::FLOAT8),
            ),
            (
                "orna.kernel.value.character-large-object@1",
                StandardScalar::CharacterLargeObject,
                Some(Type::TEXT),
            ),
            (
                "orna.kernel.value.binary-large-object@1",
                StandardScalar::BinaryLargeObject,
                Some(Type::BYTEA),
            ),
            ("orna.kernel.value.decimal@1", StandardScalar::Decimal, None),
            ("orna.kernel.value.uuid@1", StandardScalar::Uuid, None),
            ("orna.kernel.value.date@1", StandardScalar::Date, None),
            ("orna.kernel.value.time@1", StandardScalar::Time, None),
            (
                "orna.kernel.value.timestamp@1",
                StandardScalar::Timestamp,
                None,
            ),
            (
                "orna.kernel.value.duration@1",
                StandardScalar::Duration,
                None,
            ),
            ("orna.kernel.value.void@1", StandardScalar::Void, None),
        ];
        assert_eq!(cases.len(), StandardScalar::ALL.len());
        for (contract, expected_compatibility, expected_postgres) in cases {
            let value_type = context
                .standard()
                .expect("version-two standard")
                .catalogue()
                .value_types()
                .iter()
                .find(|definition| definition.representation_contract() == contract)
                .expect("retained value type")
                .id();
            let runtime = resolve_runtime_type(&context, ResolvedType::value(value_type));
            assert_eq!(
                runtime,
                ResolvedRuntimeType::VerifiedValue {
                    value_type,
                    compatibility: expected_compatibility,
                },
                "{contract}"
            );
            assert!(runtime_types_match(
                &context,
                ResolvedType::scalar(expected_compatibility),
                ResolvedType::value(value_type),
            ));
            assert_eq!(postgres_type(runtime), expected_postgres, "{contract}");
        }
    }

    #[test]
    fn values_require_the_selected_pinned_standard_identity() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard-library snapshot"),
        )
        .expect("verified standard-library snapshot");
        let integer = standard
            .catalogue()
            .value_types()
            .iter()
            .find(|definition| {
                definition.representation_contract() == "orna.kernel.value.integer@1"
            })
            .expect("retained integer value type")
            .id();
        let missing = TypeId::from_bytes([0x5a; 16]);

        assert_eq!(
            resolve_runtime_type(
                &CatalogueHashContext::version_one(),
                ResolvedType::value(integer)
            ),
            ResolvedRuntimeType::Unsupported
        );
        assert_eq!(
            resolve_runtime_type(
                &CatalogueHashContext::version_two(standard),
                ResolvedType::value(missing)
            ),
            ResolvedRuntimeType::Unsupported
        );
        assert!(!runtime_types_match(
            &CatalogueHashContext::version_two(
                orna_standard::verify_standard_library_snapshot(
                    orna_standard::retained_standard_library_snapshot()
                        .expect("retained standard-library snapshot"),
                )
                .expect("verified standard-library snapshot"),
            ),
            ResolvedType::scalar(StandardScalar::Integer),
            ResolvedType::value(missing),
        ));
    }

    #[test]
    fn reference_replay_puts_signature_references_before_body_evidence() {
        let parameter_value = TypeId::from_bytes([0x60; 16]);
        let parameter_target = TypeId::from_bytes([0x61; 16]);
        let result_target = TypeId::from_bytes([0x62; 16]);
        let body_target = TypeId::from_bytes([0x63; 16]);
        let result_value = TypeId::from_bytes([0x68; 16]);
        let enum_type = TypeId::from_bytes([0x6a; 16]);
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
                    ParameterId::from_bytes([0x69; 16]),
                    "value",
                    1,
                    ResolvedType::value(parameter_value),
                    None,
                ),
                ParameterDefinition::new(
                    ParameterId::from_bytes([0x66; 16]),
                    "reference",
                    2,
                    ResolvedType::reference(parameter_target),
                    None,
                ),
            ],
            FunctionReturn::Rows(vec![
                FunctionReturnColumnDefinition::new("value", 0, ResolvedType::value(result_value)),
                FunctionReturnColumnDefinition::new(
                    "reference",
                    1,
                    ResolvedType::reference(result_target),
                ),
                FunctionReturnColumnDefinition::new(
                    "ignored_scalar",
                    2,
                    ResolvedType::scalar(StandardScalar::Integer),
                ),
                FunctionReturnColumnDefinition::new(
                    "enum_value",
                    3,
                    ResolvedType::named(enum_type),
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
                    DefinitionReferenceKind::NamedType,
                    DefinitionReferenceTarget::ValueType(parameter_value),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(parameter_target),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::NamedType,
                    DefinitionReferenceTarget::ValueType(result_value),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(result_target),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::NamedType,
                    DefinitionReferenceTarget::ValueType(enum_type),
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
