//! PostgreSQL projections for persisted identifiers, types, and references.

use super::*;

pub(super) fn require_one(value: u64, rule: &'static str) -> Result<(), PostgresKernelError> {
    if value == 1 {
        Ok(())
    } else {
        Err(invariant(rule))
    }
}
pub(super) fn invariant(rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.apply",
        record: "candidate".into(),
        rule,
    }
}
pub(super) fn bytes<I>(id: I) -> Vec<u8>
where
    I: IntoBytes,
{
    id.into_bytes().to_vec()
}
pub(super) trait IntoBytes {
    fn into_bytes(self) -> [u8; 16];
}
macro_rules! id_bytes {
    ($($ty:ty),* $(,)?) => {
        $(
            impl IntoBytes for $ty {
                fn into_bytes(self) -> [u8; 16] {
                    self.to_bytes()
                }
            }
        )*
    };
}
id_bytes!(
    CatalogueRevisionId,
    ExpressionId,
    FieldId,
    FunctionId,
    FunctionRevisionId,
    ParameterId,
    SchemaId,
    SourceRevisionId,
    StandardLibraryRevisionId,
    TypeBindingId,
    TypeId,
    orna_core::SourceBundleId,
    orna_core::SourceUnitId
);
pub(super) fn digest(value: Sha256Digest) -> Vec<u8> {
    value.to_bytes().to_vec()
}
pub(super) fn origin(
    origins: &[DefinitionOrigin],
    identity: DefinitionIdentity,
) -> Result<SourceOrigin, PostgresKernelError> {
    origins
        .iter()
        .find(|origin| origin.identity() == identity)
        .map(DefinitionOrigin::source)
        .ok_or_else(|| {
            invariant("every persisted semantic definition must have one candidate source origin")
        })
}
pub(super) fn schema_for_name(
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
    name: &QualifiedSemanticName,
) -> Result<SchemaId, PostgresKernelError> {
    let namespace = name
        .parts()
        .get(..name.parts().len().saturating_sub(1))
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| invariant("qualified definition name must contain its schema namespace"))?;
    catalogue
        .schemas()
        .iter()
        .find(|schema| schema.name().parts() == namespace)
        .map(|schema| schema.id())
        .ok_or_else(|| invariant("definition schema namespace must resolve exactly"))
}
pub(super) fn scalar(
    scalar: StandardScalar,
    allow_void: bool,
) -> Result<&'static str, PostgresKernelError> {
    match scalar {
        StandardScalar::Boolean => Ok("boolean"),
        StandardScalar::Integer => Ok("integer"),
        StandardScalar::BigInt => Ok("bigint"),
        StandardScalar::Float => Ok("float"),
        StandardScalar::Decimal => Ok("decimal"),
        StandardScalar::CharacterLargeObject => Ok("character_large_object"),
        StandardScalar::BinaryLargeObject => Ok("binary_large_object"),
        StandardScalar::Uuid => Ok("uuid"),
        StandardScalar::Date => Ok("date"),
        StandardScalar::Time => Ok("time"),
        StandardScalar::Timestamp => Ok("timestamp"),
        StandardScalar::Duration => Ok("duration"),
        StandardScalar::Void if allow_void => Ok("void"),
        StandardScalar::Void => Err(invariant("VOID is valid only as a SINGLE function return")),
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TypeColumns {
    pub(super) kind: &'static str,
    pub(super) scalar: Option<&'static str>,
    pub(super) target: Option<TypeId>,
    pub(super) value_type: Option<TypeId>,
    pub(super) standard_library_revision: Option<StandardLibraryRevisionId>,
    pub(super) enum_type: Option<TypeId>,
    pub(super) record_type: Option<TypeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RecordValueFieldColumns {
    pub(super) kind: &'static str,
    pub(super) value_type: Option<TypeId>,
    pub(super) value_standard_library_revision: Option<StandardLibraryRevisionId>,
    pub(super) application_enum_type: Option<TypeId>,
    pub(super) enum_standard_library_revision: Option<StandardLibraryRevisionId>,
    pub(super) standard_enum_type: Option<TypeId>,
    pub(super) application_record_type: Option<TypeId>,
}

/// The one context-aware PostgreSQL projection for candidate type and reference
/// storage. It preserves the version-one tuple exactly and uses the selected
/// version-two standard pin only for durable value identities.
pub(super) struct CandidateEncoder<'a> {
    context: &'a CatalogueHashContext,
    catalogue: &'a CatalogueSnapshot,
}

impl<'a> CandidateEncoder<'a> {
    pub(super) const fn new(
        context: &'a CatalogueHashContext,
        catalogue: &'a CatalogueSnapshot,
    ) -> Self {
        Self { context, catalogue }
    }

    pub(super) fn catalogue_hash_version(&self) -> Result<i16, PostgresKernelError> {
        i16::try_from(self.context.version().to_u32())
            .map_err(|_| invariant("catalogue hash version must fit PostgreSQL smallint"))
    }

    pub(super) fn standard_library_revision(&self) -> Option<StandardLibraryRevisionId> {
        self.context
            .standard()
            .map(VerifiedStandardLibrarySnapshot::revision)
    }

    pub(super) fn record_value_field_columns(
        &self,
        candidate: &DeployableRevision,
        descriptor: &TypeDescriptor,
    ) -> Result<RecordValueFieldColumns, PostgresKernelError> {
        let class = candidate
            .record_value_field_descriptor_class(descriptor)
            .map_err(|_| {
                invariant(
                    "record value fields must use one supported standard value, enum, or record type",
                )
            })?;
        match class {
            RecordValueFieldDescriptorClass::ApplicationEnum(type_id) => {
                Ok(RecordValueFieldColumns {
                    kind: "enum",
                    value_type: None,
                    value_standard_library_revision: None,
                    application_enum_type: Some(type_id),
                    enum_standard_library_revision: None,
                    standard_enum_type: None,
                    application_record_type: None,
                })
            }
            RecordValueFieldDescriptorClass::ApplicationRecord(type_id) => {
                Ok(RecordValueFieldColumns {
                    kind: "record",
                    value_type: None,
                    value_standard_library_revision: None,
                    application_enum_type: None,
                    enum_standard_library_revision: None,
                    standard_enum_type: None,
                    application_record_type: Some(type_id),
                })
            }
            RecordValueFieldDescriptorClass::StandardEnum(type_id) => {
                let standard_library_revision =
                    self.standard_library_revision().ok_or_else(|| {
                        invariant("record value field standard enum must retain its standard pin")
                    })?;
                Ok(RecordValueFieldColumns {
                    kind: "enum",
                    value_type: None,
                    value_standard_library_revision: None,
                    application_enum_type: None,
                    enum_standard_library_revision: Some(standard_library_revision),
                    standard_enum_type: Some(type_id),
                    application_record_type: None,
                })
            }
            RecordValueFieldDescriptorClass::StandardPrimitive(type_id) => {
                let standard_library_revision =
                    self.standard_library_revision().ok_or_else(|| {
                        invariant(
                            "record value field standard primitive must retain its standard pin",
                        )
                    })?;
                Ok(RecordValueFieldColumns {
                    kind: "value",
                    value_type: Some(type_id),
                    value_standard_library_revision: Some(standard_library_revision),
                    application_enum_type: None,
                    enum_standard_library_revision: None,
                    standard_enum_type: None,
                    application_record_type: None,
                })
            }
            _ => Err(invariant(
                "record value field descriptor class must be supported by this kernel",
            )),
        }
    }

    pub(super) fn type_columns(
        &self,
        value: ResolvedType,
        allow_void: bool,
    ) -> Result<TypeColumns, PostgresKernelError> {
        if let Some(value) = value.legacy_scalar() {
            return Ok(TypeColumns {
                kind: "scalar",
                scalar: Some(scalar(value, allow_void)?),
                target: None,
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: None,
            });
        }
        if let Some(value) = value.named_type() {
            if self.catalogue.enum_type_by_id(value).is_some() {
                return Ok(TypeColumns {
                    kind: "enum",
                    scalar: None,
                    target: None,
                    value_type: None,
                    standard_library_revision: None,
                    enum_type: Some(value),
                    record_type: None,
                });
            }
            if self.catalogue.record_value_type_by_id(value).is_some() {
                return Ok(TypeColumns {
                    kind: "record",
                    scalar: None,
                    target: None,
                    value_type: None,
                    standard_library_revision: None,
                    enum_type: None,
                    record_type: Some(value),
                });
            }
            return Ok(TypeColumns {
                kind: "named",
                scalar: None,
                target: Some(value),
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: None,
            });
        }
        if let Some(target) = value.reference_target() {
            return Ok(TypeColumns {
                kind: "reference",
                scalar: None,
                target: Some(target),
                value_type: None,
                standard_library_revision: None,
                enum_type: None,
                record_type: None,
            });
        }
        if let Some(value_type) = value.value_type() {
            let standard_library_revision = if is_sealed_inspect_type_id(value_type) {
                None
            } else {
                Some(self.standard_library_revision().ok_or_else(|| {
                    invariant("resolved value types require version-two PostgreSQL encoding")
                })?)
            };
            return Ok(TypeColumns {
                kind: "value",
                scalar: None,
                target: None,
                value_type: Some(value_type),
                standard_library_revision,
                enum_type: None,
                record_type: None,
            });
        }
        Err(invariant(
            "resolved type must expose one supported PostgreSQL type shape",
        ))
    }

    pub(super) fn client_type_columns(
        &self,
        value: ResolvedType,
        allow_void: bool,
    ) -> Result<TypeColumns, PostgresKernelError> {
        let mut columns = self.type_columns(value, allow_void)?;
        if columns.value_type.is_some_and(is_sealed_inspect_type_id) {
            columns.standard_library_revision = None;
        }
        Ok(columns)
    }

    pub(super) fn function_type_columns(
        &self,
        domain: FunctionDomain,
        value: ResolvedType,
        allow_void: bool,
    ) -> Result<TypeColumns, PostgresKernelError> {
        if domain == FunctionDomain::Client {
            self.client_type_columns(value, allow_void)
        } else {
            self.type_columns(value, allow_void)
        }
    }

    pub(super) fn reference_target(
        &self,
        value: DefinitionReferenceTarget,
    ) -> Result<ReferenceTargetColumns, PostgresKernelError> {
        if let DefinitionReferenceTarget::ObjectType(id) = value {
            return Ok((
                "object_type",
                bytes(id),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
        if let DefinitionReferenceTarget::ValueType(id) = value {
            if self.catalogue.enum_type_by_id(id).is_some() {
                return Ok((
                    "enum_type",
                    bytes(id),
                    None,
                    None,
                    None,
                    Some(bytes(self.catalogue.revision())),
                    None,
                    None,
                    None,
                ));
            }
            if self.catalogue.record_value_type_by_id(id).is_some() {
                return Ok((
                    "record_type",
                    bytes(id),
                    None,
                    None,
                    None,
                    None,
                    Some(bytes(self.catalogue.revision())),
                    None,
                    None,
                ));
            }
            if is_sealed_inspect_type_id(id) {
                return Ok((
                    "value_type",
                    bytes(id),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                ));
            }
            let standard_library_revision = self.standard_library_revision().ok_or_else(|| {
                invariant("value type references require version-two PostgreSQL encoding")
            })?;
            return Ok((
                "value_type",
                bytes(id),
                None,
                None,
                Some(bytes(standard_library_revision)),
                None,
                None,
                None,
                None,
            ));
        }
        if let DefinitionReferenceTarget::Field { owner, field } = value {
            let record_field = self
                .catalogue
                .record_value_type_by_id(owner)
                .is_some_and(|record| record.field_by_id(field).is_some());
            if record_field {
                return Ok((
                    "record_field",
                    bytes(field),
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(bytes(self.catalogue.revision())),
                    Some(bytes(owner)),
                ));
            }
            if self
                .catalogue
                .object_type_by_id(owner)
                .is_none_or(|object| object.field_by_id(field).is_none())
            {
                return Err(invariant(
                    "definition reference field target is absent from the candidate catalogue",
                ));
            }
            return Ok((
                "field",
                bytes(field),
                Some(bytes(owner)),
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
        if let DefinitionReferenceTarget::Function(id) = value {
            return Ok((
                "function",
                bytes(id),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
        if let DefinitionReferenceTarget::Parameter { owner, parameter } = value {
            return Ok((
                "parameter",
                bytes(parameter),
                None,
                Some(bytes(owner)),
                None,
                None,
                None,
                None,
                None,
            ));
        }
        if let DefinitionReferenceTarget::Expression(id) = value {
            return Ok((
                "expression",
                bytes(id),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
        Err(invariant(
            "definition reference target is not supported by PostgreSQL persistence",
        ))
    }

    pub(super) fn reference_columns(
        &self,
        reference: &DefinitionReference,
    ) -> Result<ReferenceInsertColumns, PostgresKernelError> {
        let (
            kind,
            target,
            owner_type,
            owner_function,
            standard_library_revision,
            enum_catalogue_revision,
            record_catalogue_revision,
            record_field_catalogue_revision,
            record_field_owner_type,
        ) = self.reference_target(reference.target())?;
        Ok((
            target,
            kind,
            owner_type,
            owner_function,
            standard_library_revision,
            enum_catalogue_revision,
            record_catalogue_revision,
            record_field_catalogue_revision,
            record_field_owner_type,
        ))
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LegacyTypeColumns {
    Scalar(&'static str),
    Named(TypeId),
    Reference(TypeId),
}

#[cfg(test)]
impl LegacyTypeColumns {
    const fn tuple(self) -> (&'static str, Option<&'static str>, Option<TypeId>) {
        match self {
            Self::Scalar(value) => ("scalar", Some(value), None),
            Self::Named(value) => ("named", None, Some(value)),
            Self::Reference(target) => ("reference", None, Some(target)),
        }
    }
}

#[cfg(test)]
pub(super) fn legacy_type_projection(
    value: ResolvedType,
    allow_void: bool,
) -> Result<LegacyTypeColumns, PostgresKernelError> {
    if let Some(value) = value.legacy_scalar() {
        return Ok(LegacyTypeColumns::Scalar(scalar(value, allow_void)?));
    }
    if let Some(value) = value.named_type() {
        return Ok(LegacyTypeColumns::Named(value));
    }
    if let Some(target) = value.reference_target() {
        return Ok(LegacyTypeColumns::Reference(target));
    }
    if value.value_type().is_some() {
        return Err(invariant(
            "resolved value types are not supported by legacy PostgreSQL type encoding",
        ));
    }
    Err(invariant(
        "resolved type must expose one supported PostgreSQL type shape",
    ))
}

#[cfg(test)]
pub(super) fn type_columns(
    value: ResolvedType,
    allow_void: bool,
) -> Result<(&'static str, Option<&'static str>, Option<TypeId>), PostgresKernelError> {
    Ok(legacy_type_projection(value, allow_void)?.tuple())
}
pub(super) fn on_delete(value: Option<OnDeleteAction>) -> Option<&'static str> {
    value.map(|value| match value {
        OnDeleteAction::Restrict => "restrict",
        OnDeleteAction::SetNull => "set_null",
        OnDeleteAction::Cascade => "cascade",
    })
}
pub(super) fn function_domain(value: FunctionDomain) -> &'static str {
    match value {
        FunctionDomain::Server => "server",
        FunctionDomain::Client => "client",
    }
}
pub(super) fn function_security(value: FunctionSecurity) -> &'static str {
    match value {
        FunctionSecurity::Invoker => "invoker",
        FunctionSecurity::Definer => "definer",
    }
}
pub(super) fn function_transaction(
    value: Option<FunctionTransaction>,
) -> Result<Option<&'static str>, PostgresKernelError> {
    match value {
        None => Ok(None),
        Some(FunctionTransaction::Atomic) => Ok(Some("atomic")),
        Some(FunctionTransaction::ReadOnly) => Ok(Some("read_only")),
        Some(FunctionTransaction::Manual) => Err(invariant(
            "manual function transactions are not supported by PostgreSQL",
        )),
    }
}
pub(super) fn function_volatility(value: FunctionVolatility) -> &'static str {
    match value {
        FunctionVolatility::Immutable => "immutable",
        FunctionVolatility::Stable => "stable",
        FunctionVolatility::Volatile => "volatile",
    }
}
pub(super) fn artifact_kind(value: orna_core::revision::ExecutableArtifactKind) -> &'static str {
    match value {
        orna_core::revision::ExecutableArtifactKind::Server => "server_plan",
        orna_core::revision::ExecutableArtifactKind::Client => "client_bytecode",
    }
}
pub(super) fn reference_kind(
    value: DefinitionReferenceKind,
) -> Result<&'static str, PostgresKernelError> {
    POSTGRES_REFERENCE_KINDS
        .iter()
        .find(|(kind, _)| *kind == value)
        .map(|(_, name)| *name)
        .ok_or_else(|| {
            invariant("definition reference kind is not supported by PostgreSQL persistence")
        })
}
pub(super) const POSTGRES_REFERENCE_KINDS: &[(DefinitionReferenceKind, &str)] = &[
    (DefinitionReferenceKind::FunctionCall, "function_call"),
    (DefinitionReferenceKind::NamedType, "named_type"),
    (DefinitionReferenceKind::ObjectReference, "object_reference"),
    (DefinitionReferenceKind::ParameterRead, "parameter_read"),
    (DefinitionReferenceKind::QueryObject, "query_object"),
    (DefinitionReferenceKind::QueryField, "query_field"),
    (DefinitionReferenceKind::Expression, "expression"),
    (DefinitionReferenceKind::WriteObject, "write_object"),
    (DefinitionReferenceKind::WriteField, "write_field"),
];
pub(super) type ReferenceTargetColumns = (
    &'static str,
    Vec<u8>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

#[cfg(test)]
pub(super) type LegacyReferenceTargetColumns =
    (&'static str, Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>);

#[cfg(test)]
pub(super) fn reference_target(
    value: DefinitionReferenceTarget,
) -> Result<LegacyReferenceTargetColumns, PostgresKernelError> {
    Ok(match value {
        DefinitionReferenceTarget::ObjectType(id) => ("object_type", bytes(id), None, None),
        DefinitionReferenceTarget::Field { owner, field } => {
            ("field", bytes(field), Some(bytes(owner)), None)
        }
        DefinitionReferenceTarget::Function(id) => ("function", bytes(id), None, None),
        DefinitionReferenceTarget::Parameter { owner, parameter } => {
            ("parameter", bytes(parameter), None, Some(bytes(owner)))
        }
        other => {
            let DefinitionReferenceTarget::Expression(id) = other else {
                return Err(invariant(
                    "definition reference target is not supported by PostgreSQL persistence",
                ));
            };
            ("expression", bytes(id), None, None)
        }
    })
}
pub(super) type ReferenceInsertColumns = (
    Vec<u8>,
    &'static str,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);
