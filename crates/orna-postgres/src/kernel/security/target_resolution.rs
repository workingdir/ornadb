use super::*;
/// One privately resolved sealed invocation target for the PostgreSQL kernel.
///
/// This mirrors the closed resolution inside `orna-core` so the durable audit
/// and execution steps can re-derive the exact pinned target without exposing
/// any resolution phase. An application target carries no executable pin; a
/// verified-standard target carries the exact executable and standard
/// revisions of the pinned snapshot. A system target carries only its sealed
/// registry definition.
#[derive(Clone, Copy)]
pub(super) enum SealedResolvedTarget<'a> {
    Application(&'a FunctionDefinition),
    System(SystemFunctionDefinition),
    VerifiedStandard {
        definition: &'a FunctionDefinition,
        executable: &'a StandardExecutable,
    },
}
pub(crate) fn is_admitted_security_identity(definition: SystemFunctionDefinition) -> bool {
    let id = definition.id();
    matches!(
        id,
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID
            | SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID
            | SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID
    ) && matches!(definition.kind(), SystemFunctionKind::SecurityIdentity)
        && definition.security_signature().is_some_and(|signature| {
            signature.parameter_count() == 0
                && signature.returns_ref_principal()
                && !signature.returns_boolean()
                && signature.stream_item_type().is_none()
                && match id {
                    SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID => signature.returns_set(),
                    SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID
                    | SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID => !signature.returns_set(),
                    _ => false,
                }
        })
}

/// One canonical resource target resolved against the active application and
/// verified-standard catalogues. Standard targets retain both immutable pins.
pub(super) enum ResolvedResourceTarget<'a> {
    Application {
        definition: &'a FunctionDefinition,
        target: InvocationTarget,
    },
    VerifiedStandard {
        definition: &'a FunctionDefinition,
        executable: &'a StandardExecutable,
        target: InvocationTarget,
    },
}

impl<'a> ResolvedResourceTarget<'a> {
    pub(super) fn target(&self) -> InvocationTarget {
        match self {
            Self::Application { target, .. } | Self::VerifiedStandard { target, .. } => *target,
        }
    }

    pub(super) fn definition(&self) -> &'a FunctionDefinition {
        match self {
            Self::Application { definition, .. } | Self::VerifiedStandard { definition, .. } => {
                definition
            }
        }
    }

    pub(super) fn executable(&self) -> Option<&'a StandardExecutable> {
        match self {
            Self::Application { .. } => None,
            Self::VerifiedStandard { executable, .. } => Some(executable),
        }
    }
}

/// Resolves one SERVER resource function to its closed security target.
///
/// A function present in both the active application and exact standard
/// catalogues is ambiguous. A standard function must have exactly one
/// executable whose immutable revision matches the catalogue definition;
/// missing, duplicate, stale, or otherwise unpinned evidence resolves to no
/// target and therefore cannot reach authorise_execute.
pub(super) fn resolve_resource_target<'a>(
    active: &'a ActiveDatabaseRevision,
    function: FunctionId,
) -> Option<ResolvedResourceTarget<'a>> {
    resolve_resource_target_in_catalogues(
        active.pair(),
        active.catalogue(),
        active.catalogue_hash_context().standard(),
        function,
    )
}

pub(super) fn resolve_resource_target_in_catalogues<'a>(
    pair: RevisionPair,
    application_catalogue: &'a orna_core::catalogue::CatalogueSnapshot,
    standard: Option<&'a orna_core::revision::VerifiedStandardLibrarySnapshot>,
    function: FunctionId,
) -> Option<ResolvedResourceTarget<'a>> {
    let application = application_catalogue.function_by_id(function);
    let standard_definition =
        standard.and_then(|snapshot| snapshot.catalogue().function_by_id(function));

    match (application, standard_definition) {
        (Some(_), Some(_)) | (None, None) => None,
        (Some(definition), None) => Some(ResolvedResourceTarget::Application {
            definition,
            target: InvocationTarget::new(function, pair),
        }),
        (None, Some(definition)) => {
            let snapshot = standard?;
            let mut executables = snapshot
                .executables()
                .iter()
                .filter(|executable| executable.function() == function);
            let executable = executables.next()?;
            if executables.next().is_some()
                || executable.revision().function() != function
                || executable.revision().id() != definition.current_revision()
            {
                return None;
            }
            Some(ResolvedResourceTarget::VerifiedStandard {
                definition,
                executable,
                target: InvocationTarget::verified_standard(
                    function,
                    pair,
                    snapshot.revision(),
                    executable.revision().id(),
                ),
            })
        }
    }
}

/// Resolves one sealed request target in the pinned application and
/// verified-standard catalogues, mirroring the private core resolution.
///
/// A function present in both catalogues is ambiguous and resolves to
/// neither, exactly as at the protected boundary. A verified-standard target
/// resolves only when its executable pin matches the snapshot's current
/// function revision.
pub(super) fn resolve_sealed_target<'a>(
    active: &'a ActiveDatabaseRevision,
    selector: &InvocationRequestTarget,
) -> Option<SealedResolvedTarget<'a>> {
    let system_target = match selector {
        InvocationRequestTarget::FunctionId(id) => system_function_by_id(*id),
        InvocationRequestTarget::QualifiedName(name) => system_function_by_name(name),
        _ => None,
    };
    if let Some(definition) = system_target {
        return is_admitted_security_identity(definition)
            .then_some(SealedResolvedTarget::System(definition));
    }
    let application = active.catalogue();
    let standard = active.catalogue_hash_context().standard();
    let application_target = match selector {
        InvocationRequestTarget::FunctionId(id) => application.function_by_id(*id),
        InvocationRequestTarget::QualifiedName(name) => application.function_by_name(name),
        _ => None,
    };
    let standard_target = standard.and_then(|snapshot| match selector {
        InvocationRequestTarget::FunctionId(id) => snapshot.catalogue().function_by_id(*id),
        InvocationRequestTarget::QualifiedName(name) => snapshot.catalogue().function_by_name(name),
        _ => None,
    });
    match (application_target, standard_target) {
        (Some(_), Some(_)) | (None, None) => None,
        (Some(definition), None) => Some(SealedResolvedTarget::Application(definition)),
        (None, Some(definition)) => {
            let snapshot = standard.expect("a standard target requires the pinned snapshot");
            let executable = snapshot
                .executables()
                .iter()
                .find(|executable| executable.function() == definition.id())?;
            if executable.revision().id() != definition.current_revision() {
                return None;
            }
            Some(SealedResolvedTarget::VerifiedStandard {
                definition,
                executable,
            })
        }
    }
}

/// Returns the closed two-class security target for one privately resolved
/// sealed target.
pub(super) fn sealed_security_target(
    active: &ActiveDatabaseRevision,
    target: SealedResolvedTarget<'_>,
) -> InvocationTarget {
    match target {
        SealedResolvedTarget::Application(definition) => {
            InvocationTarget::new(definition.id(), active.pair())
        }
        SealedResolvedTarget::System(definition) => {
            InvocationTarget::new(definition.id(), active.pair())
        }
        SealedResolvedTarget::VerifiedStandard {
            definition,
            executable,
        } => {
            let standard = active
                .catalogue_hash_context()
                .standard()
                .expect("a verified-standard target requires the pinned snapshot");
            InvocationTarget::verified_standard(
                definition.id(),
                active.pair(),
                standard.revision(),
                executable.revision().id(),
            )
        }
    }
}
pub(super) fn sealed_target_security_is_supported(target: SealedResolvedTarget<'_>) -> bool {
    match target {
        SealedResolvedTarget::Application(definition)
        | SealedResolvedTarget::VerifiedStandard { definition, .. } => {
            definition.security() == FunctionSecurity::Invoker
        }
        SealedResolvedTarget::System(_) => true,
    }
}

pub(super) fn authorise_sealed_target(
    security: &SecuritySnapshot,
    authenticated_session: &AuthenticatedSession,
    target: InvocationTarget,
) -> ExecuteDecision {
    if system_function_by_id(target.function()).is_some_and(is_admitted_security_identity) {
        security.authorise_system_function(authenticated_session, target)
    } else {
        security.authorise_execute(authenticated_session, target)
    }
}

pub(super) fn sealed_target_invariant(
    active: &ActiveDatabaseRevision,
    rule: &'static str,
) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "active catalogue",
        record: active.pair().catalogue().canonical(),
        rule,
    }
}

/// Binds one sealed request's checked arguments to the pinned function.
///
/// The protected decision already ran private prebind, so every selector
/// resolves and every value matches the declared type. This step re-checks
/// the boundary and constructs the typed arguments the closed engine accepts.
pub(super) fn bind_sealed_invoke_arguments(
    definition: &FunctionDefinition,
    arguments: &[InvocationArgument],
) -> Result<Vec<FunctionArgument>, PostgresKernelError> {
    let mut bound = Vec::with_capacity(arguments.len());
    for argument in arguments {
        let parameter = match argument.selector() {
            InvocationParameterSelector::ParameterId(id) => definition.parameter_by_id(*id),
            InvocationParameterSelector::Name(name) => definition.parameter_by_name(name),
            _ => None,
        };
        let Some(parameter) = parameter else {
            return Err(PostgresKernelError::ServerSelect(
                ServerSelectError::Argument {
                    parameter: None,
                    rule: "sealed invocation argument selector must resolve to a pinned parameter",
                },
            ));
        };
        let value = argument.value().clone().into_value();
        let argument = FunctionArgument::new(parameter.id(), value).map_err(|_| {
            PostgresKernelError::ServerSelect(ServerSelectError::Argument {
                parameter: Some(parameter.id()),
                rule: "sealed invocation argument must be one non-null typed value",
            })
        })?;
        bound.push(argument);
    }
    Ok(bound)
}
