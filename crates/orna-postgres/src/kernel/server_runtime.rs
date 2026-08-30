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
    match function.return_type() {
        FunctionReturn::Rows(columns) => {
            for column in columns {
                add_signature_reference(&mut expected, column.resolved_type());
            }
        }
        FunctionReturn::Stream(element) => add_signature_reference(&mut expected, *element),
        FunctionReturn::Single(_) => {}
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
#[path = "server_runtime/tests.rs"]
mod tests;
