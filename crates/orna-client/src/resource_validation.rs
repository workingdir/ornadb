use super::*;

pub(super) fn stream_item_descriptor(expected: ResolvedType) -> Option<TypeDescriptor> {
    match expected {
        ResolvedType::Scalar(scalar) => {
            let type_id = match scalar {
                StandardScalar::Boolean => orna_standard::BOOLEAN_TYPE_ID,
                StandardScalar::Integer => orna_standard::INTEGER_TYPE_ID,
                StandardScalar::BigInt => orna_standard::BIGINT_TYPE_ID,
                StandardScalar::Float => orna_standard::FLOAT_TYPE_ID,
                StandardScalar::CharacterLargeObject => {
                    orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID
                }
                StandardScalar::BinaryLargeObject => orna_standard::BINARY_LARGE_OBJECT_TYPE_ID,
                StandardScalar::Decimal
                | StandardScalar::Uuid
                | StandardScalar::Date
                | StandardScalar::Time
                | StandardScalar::Timestamp
                | StandardScalar::Duration
                | StandardScalar::Void => return None,
            };
            Some(TypeDescriptor::named(type_id))
        }
        ResolvedType::Named(type_id) | ResolvedType::Value(type_id) => {
            Some(TypeDescriptor::named(type_id))
        }
        ResolvedType::Reference { target } => Some(TypeDescriptor::reference(target)),
    }
}
pub(super) fn source_reference_target_name(
    active: &ActiveDatabaseRevision,
    target: DefinitionReferenceTarget,
) -> Option<String> {
    let standard_catalogue = active
        .catalogue_hash_context()
        .standard()
        .map(|snapshot| snapshot.catalogue());
    match target {
        DefinitionReferenceTarget::ObjectType(id) => active
            .catalogue()
            .object_type_by_id(id)
            .or_else(|| standard_catalogue.and_then(|catalogue| catalogue.object_type_by_id(id)))
            .map(|definition| definition.name().to_string()),
        DefinitionReferenceTarget::ValueType(id) => active
            .catalogue()
            .value_type_by_id(id)
            .or_else(|| standard_catalogue.and_then(|catalogue| catalogue.value_type_by_id(id)))
            .map(|definition| definition.name().to_string())
            .or_else(|| {
                (id == orna_core::system::SYS_SOURCE_FUNCTION_TYPE_ID)
                    .then_some(orna_core::system::SYS_SOURCE_FUNCTION_TYPE_NAME.to_string())
            }),
        DefinitionReferenceTarget::Function(id) => active
            .catalogue()
            .function_by_id(id)
            .or_else(|| standard_catalogue.and_then(|catalogue| catalogue.function_by_id(id)))
            .map(|definition| definition.name().to_string())
            .or_else(|| {
                orna_core::system::system_function_by_id(id)
                    .map(|definition| definition.name_parts().join("."))
            }),
        DefinitionReferenceTarget::Parameter { owner, parameter } => active
            .catalogue()
            .function_by_id(owner)
            .and_then(|function| {
                function
                    .parameter_by_id(parameter)
                    .map(|parameter| format!("{}.{}", function.name(), parameter.name()))
            })
            .or_else(|| {
                standard_catalogue
                    .and_then(|catalogue| catalogue.function_by_id(owner))
                    .and_then(|function| {
                        function
                            .parameter_by_id(parameter)
                            .map(|parameter| format!("{}.{}", function.name(), parameter.name()))
                    })
            }),
        DefinitionReferenceTarget::Field { owner, field } => active
            .catalogue()
            .object_type_by_id(owner)
            .and_then(|definition| {
                definition.field_by_id(field).map(|field_definition| {
                    format!("{}.{}", definition.name(), field_definition.name())
                })
            })
            .or_else(|| {
                standard_catalogue
                    .and_then(|catalogue| catalogue.object_type_by_id(owner))
                    .and_then(|definition| {
                        definition.field_by_id(field).map(|field_definition| {
                            format!("{}.{}", definition.name(), field_definition.name())
                        })
                    })
            }),
        DefinitionReferenceTarget::Expression(id) => Some(format!("expression:{id:?}")),
        _ => None,
    }
}
pub(super) fn source_metadata_type_id(
    active: &ActiveDatabaseRevision,
    resolved_type: ResolvedType,
) -> Option<TypeId> {
    match resolved_type {
        ResolvedType::Value(type_id)
        | ResolvedType::Named(type_id)
        | ResolvedType::Reference { target: type_id } => Some(type_id),
        ResolvedType::Scalar(scalar) => {
            let contract = match scalar {
                StandardScalar::Boolean => "orna.kernel.value.boolean@1",
                StandardScalar::Integer => "orna.kernel.value.integer@1",
                StandardScalar::BigInt => "orna.kernel.value.bigint@1",
                StandardScalar::Float => "orna.kernel.value.float@1",
                StandardScalar::Decimal => "orna.kernel.value.decimal@1",
                StandardScalar::CharacterLargeObject => {
                    "orna.kernel.value.character-large-object@1"
                }
                StandardScalar::BinaryLargeObject => "orna.kernel.value.binary-large-object@1",
                StandardScalar::Uuid => "orna.kernel.value.uuid@1",
                StandardScalar::Date => "orna.kernel.value.date@1",
                StandardScalar::Time => "orna.kernel.value.time@1",
                StandardScalar::Timestamp => "orna.kernel.value.timestamp@1",
                StandardScalar::Duration => "orna.kernel.value.duration@1",
                StandardScalar::Void => return None,
            };
            active
                .catalogue_hash_context()
                .standard()
                .map(|snapshot| snapshot.catalogue())
                .into_iter()
                .flat_map(|catalogue| catalogue.value_types())
                .chain(active.catalogue().value_types())
                .find(|definition| definition.representation_contract() == contract)
                .map(|definition| definition.id())
        }
    }
}

pub(super) fn source_metadata_body_kind(
    artifact: &ExecutableArtifact,
) -> orna_core::source_metadata::SourceBodyKind {
    match artifact.version() {
        EXPRESSION_FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::Expression,
        PROCEDURAL_FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::Procedural,
        orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION => {
            orna_core::source_metadata::SourceBodyKind::ControlFlow
        }
        STATE_FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::State,
        OPAQUE_FORMAT_VERSION => orna_core::source_metadata::SourceBodyKind::ExternalContract,
        _ => orna_core::source_metadata::SourceBodyKind::Unknown,
    }
}

pub(super) fn source_metadata_return_metadata(
    active: &ActiveDatabaseRevision,
    return_type: &FunctionReturn,
) -> Option<orna_core::source_metadata::SourceReturnMetadata> {
    match return_type {
        FunctionReturn::Single(resolved_type) => source_metadata_type_id(active, *resolved_type)
            .map(orna_core::source_metadata::SourceReturnMetadata::Single),
        FunctionReturn::Stream(resolved_type) => source_metadata_type_id(active, *resolved_type)
            .map(orna_core::source_metadata::SourceReturnMetadata::Stream),
        FunctionReturn::Rows(_) => None,
    }
}

pub(super) fn supported_stream_item_descriptor(
    active: &ActiveDatabaseRevision,
    expected: ResolvedType,
) -> Option<TypeDescriptor> {
    let descriptor = stream_item_descriptor(expected)?;
    match expected {
        ResolvedType::Scalar(_) => Some(descriptor),
        ResolvedType::Named(type_id) => (execution::active_has_enum_type(active, type_id)
            || execution::active_has_record_type(active, type_id))
        .then_some(descriptor),
        ResolvedType::Value(type_id) => {
            let definition = active
                .catalogue_hash_context()
                .standard()
                .and_then(|standard| standard.catalogue().value_type_by_id(type_id))?;
            if definition.kind() == ValueTypeKind::Opaque {
                return None;
            }
            matches!(
                definition.representation_contract(),
                "orna.kernel.value.boolean@1"
                    | "orna.kernel.value.integer@1"
                    | "orna.kernel.value.bigint@1"
                    | "orna.kernel.value.float@1"
                    | "orna.kernel.value.character-large-object@1"
                    | "orna.kernel.value.binary-large-object@1"
            )
            .then_some(descriptor)
        }
        ResolvedType::Reference { target } => {
            execution::active_has_object_type(active, target).then_some(descriptor)
        }
    }
}

pub(super) fn canonical_resource_arguments(
    arguments: &[FunctionArgument],
) -> Result<Vec<FunctionArgument>, ClientResourceError> {
    if arguments.len() > MAX_RESOURCE_ARGUMENTS {
        return Err(ClientResourceError::ResourceArgumentLimitExceeded {
            limit: MAX_RESOURCE_ARGUMENTS,
        });
    }
    let mut arguments = arguments.to_vec();
    arguments.sort_by_key(FunctionArgument::parameter);
    for pair in arguments.windows(2) {
        if pair[0].parameter() == pair[1].parameter() {
            return Err(ClientResourceError::DuplicateArgument {
                parameter: pair[0].parameter(),
            });
        }
    }
    Ok(arguments)
}

pub(super) struct ResolvedResourceTarget<'a> {
    pub(super) target: InvocationTarget,
    pub(super) definition: &'a FunctionDefinition,
}

pub(super) struct ResolvedClientFunction<'a> {
    pub(super) definition: &'a FunctionDefinition,
    pub(super) revision: &'a FunctionRevisionRecord,
    pub(super) references: &'a [orna_core::revision::DefinitionReference],
    pub(super) standard: Option<&'a VerifiedStandardLibrarySnapshot>,
}

fn verified_standard_executable(
    standard: &VerifiedStandardLibrarySnapshot,
    function: FunctionId,
) -> Option<&StandardExecutable> {
    let mut executables = standard
        .executables()
        .iter()
        .filter(|executable| executable.function() == function);
    let executable = executables.next()?;
    executables.next().is_none().then_some(executable)
}

/// Resolves one CLIENT function from the active application first, then from
/// the exact verified standard snapshot pinned by that application revision.
///
/// Application definitions retain precedence even when a malformed or
/// incomplete application revision would otherwise allow a standard fallback.
/// A standard definition is executable only when its snapshot carries exactly
/// one executable whose immutable revision is the definition's current
/// revision.
pub(super) fn resolve_client_function<'a>(
    active: &'a ActiveDatabaseRevision,
    function: FunctionId,
) -> Option<ResolvedClientFunction<'a>> {
    if let Some(definition) = active.catalogue().function_by_id(function) {
        let revision = active.function_revisions().iter().find(|candidate| {
            candidate.function() == function && candidate.id() == definition.current_revision()
        })?;
        return Some(ResolvedClientFunction {
            definition,
            revision,
            references: active.references(),
            standard: None,
        });
    }

    let standard = active.catalogue_hash_context().standard()?;
    let definition = standard.catalogue().function_by_id(function)?;
    let executable = verified_standard_executable(standard, function)?;
    let revision = executable.revision();
    if revision.function() != function || revision.id() != definition.current_revision() {
        return None;
    }
    Some(ResolvedClientFunction {
        definition,
        revision,
        references: executable.references(),
        standard: Some(standard),
    })
}

pub(super) fn client_invocation_target_is_resolved(
    active: &ActiveDatabaseRevision,
    target: InvocationTarget,
) -> bool {
    let Some(resolved) = resolve_client_function(active, target.function()) else {
        return false;
    };
    match resolved.standard {
        Some(standard) => {
            target.class() == Some(TargetClass::VerifiedStandard)
                && target.standard_revision() == Some(standard.revision())
                && target.executable_revision() == Some(resolved.revision.id())
        }
        None => {
            matches!(target.class(), None | Some(TargetClass::Application))
                && target.standard_revision().is_none()
                && target.executable_revision().is_none()
        }
    }
}

fn verified_standard_executable_revision(
    standard: &VerifiedStandardLibrarySnapshot,
    function: FunctionId,
) -> Option<FunctionRevisionId> {
    verified_standard_executable(standard, function).map(|executable| executable.revision().id())
}

/// Resolves a resource target against the active application catalogue and its
/// exact verified standard snapshot. A standard target must carry both the
/// snapshot and executable revision pins; a raw class-less target is never
/// upgraded implicitly by this path.
pub(super) fn resolve_resource_target<'a>(
    active: &'a ActiveDatabaseRevision,
    target: InvocationTarget,
) -> Option<ResolvedResourceTarget<'a>> {
    if target.revision() != active.pair() {
        return None;
    }
    let application = active.catalogue().function_by_id(target.function());
    let standard = active.catalogue_hash_context().standard();
    let standard_definition =
        standard.and_then(|snapshot| snapshot.catalogue().function_by_id(target.function()));
    match target.class() {
        None | Some(TargetClass::Application) => {
            if target.standard_revision().is_some() || target.executable_revision().is_some() {
                return None;
            }
            match (application, standard_definition) {
                (Some(definition), None) => Some(ResolvedResourceTarget { target, definition }),
                _ => None,
            }
        }
        Some(TargetClass::VerifiedStandard) => {
            let standard_revision = target.standard_revision()?;
            let executable_revision = target.executable_revision()?;
            let standard = standard?;
            if application.is_some() || standard.revision() != standard_revision {
                return None;
            }
            let definition = standard_definition?;
            if verified_standard_executable_revision(standard, target.function())
                != Some(executable_revision)
            {
                return None;
            }
            Some(ResolvedResourceTarget { target, definition })
        }
    }
}

/// Resolves artifact resource metadata to the canonical application or pinned
/// verified-standard invocation target. The artifact stores the active pair;
/// standard identity is admitted only when the function exists in the exact
/// pinned snapshot and has one executable record.
pub(super) fn resolve_resource_operation_target<'a>(
    active: &'a ActiveDatabaseRevision,
    operation: &ResourceOperationNode,
) -> Option<ResolvedResourceTarget<'a>> {
    resolve_unclassified_target(
        active,
        InvocationTarget::new(operation.target_function(), operation.target_revision()),
    )
}

/// Resolves an action's unclassified target to the application function or to
/// the exact verified-standard executable selected by the active catalogue
/// hash context. A raw target is intentionally never upgraded unless the
/// active catalogue proves that the identity belongs only to the standard
/// snapshot.
pub(super) fn resolve_unclassified_target<'a>(
    active: &'a ActiveDatabaseRevision,
    raw_target: InvocationTarget,
) -> Option<ResolvedResourceTarget<'a>> {
    if raw_target.revision() != active.pair() {
        return None;
    }
    let application = active.catalogue().function_by_id(raw_target.function());
    let standard = active.catalogue_hash_context().standard();
    let standard_definition =
        standard.and_then(|snapshot| snapshot.catalogue().function_by_id(raw_target.function()));
    match (application, standard_definition) {
        (Some(_), None) => resolve_resource_target(active, raw_target),
        (None, Some(_)) => {
            let standard = standard?;
            let executable_revision =
                verified_standard_executable_revision(standard, raw_target.function())?;
            let target = InvocationTarget::verified_standard(
                raw_target.function(),
                raw_target.revision(),
                standard.revision(),
                executable_revision,
            );
            resolve_resource_target(active, target)
        }
        _ => None,
    }
}

// ClientActionError preserves its public diagnostic layout at this resolver boundary.
#[allow(clippy::result_large_err)]
pub(super) fn resolve_action_target<'a>(
    active: &'a ActiveDatabaseRevision,
    descriptor: &ClientActionDescriptor,
) -> Result<ResolvedResourceTarget<'a>, ClientActionError> {
    if descriptor.target_revision != active.pair() {
        return Err(ClientActionError::RevisionMismatch);
    }
    let Some(resolved) = resolve_unclassified_target(
        active,
        InvocationTarget::new(descriptor.target, descriptor.target_revision),
    ) else {
        return Err(ClientActionError::TargetMismatch);
    };
    let expected_domain = match descriptor.domain {
        ActionTargetDomain::Client => FunctionDomain::Client,
        ActionTargetDomain::Server => FunctionDomain::Server,
    };
    if resolved.definition.domain() != expected_domain {
        return Err(ClientActionError::TargetMismatch);
    }
    Ok(resolved)
}

pub(super) fn validate_resource_arguments(
    active: &ActiveDatabaseRevision,
    target: InvocationTarget,
    arguments: &[FunctionArgument],
) -> Result<Vec<FunctionArgument>, ClientResourceError> {
    let Some(resolved) = resolve_resource_target(active, target) else {
        return Err(ClientResourceError::TargetMismatch { expected: target });
    };
    let definition = resolved.definition;
    let arguments = canonical_resource_arguments(arguments)?;
    for argument in &arguments {
        let Some(parameter) = definition
            .parameters()
            .iter()
            .find(|candidate| candidate.id() == argument.parameter())
        else {
            return Err(ClientResourceError::UnknownArgument {
                parameter: argument.parameter(),
            });
        };
        if !execution::runtime_value_matches(active, argument.value(), parameter.resolved_type()) {
            return Err(ClientResourceError::TypeMismatch);
        }
    }
    for parameter in definition.parameters() {
        if !arguments
            .iter()
            .any(|argument| argument.parameter() == parameter.id())
        {
            return Err(ClientResourceError::MissingArgument {
                parameter: parameter.id(),
            });
        }
    }
    Ok(arguments)
}

pub(super) fn canonical_resource_argument_digest(
    active: &ActiveDatabaseRevision,
    arguments: &[FunctionArgument],
) -> Result<Sha256Digest, ClientResourceError> {
    let mut hasher = Sha256::new();
    hasher.update(b"ornadb.client-resource-arguments/v1\0");
    let argument_count =
        u32::try_from(arguments.len()).map_err(|_| ClientResourceError::ArgumentEncoding)?;
    hasher.update(argument_count.to_be_bytes());
    for argument in arguments {
        let frame = ClientFrame::CallArgument {
            stream: 1,
            parameter: argument.parameter(),
            value: argument.value().clone(),
        };
        let encoded = encode_active_client_frame(active, &frame)
            .map_err(|_| ClientResourceError::ArgumentEncoding)?;
        let encoded_length =
            u32::try_from(encoded.len()).map_err(|_| ClientResourceError::ArgumentEncoding)?;
        hasher.update(encoded_length.to_be_bytes());
        hasher.update(encoded);
    }
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}
