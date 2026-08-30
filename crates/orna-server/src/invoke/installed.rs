use super::*;

#[derive(Debug)]
pub(super) enum InvokeTransport {
    InProcess,
    UnixSocket(PathBuf),
}

/// The private host resolution of one invocation target.
pub(super) struct ResolvedTarget<'a> {
    /// The resolved function signature in the owning catalogue.
    pub(super) function: &'a FunctionDefinition,
    /// The exact immutable executable revision for the resolved class.
    pub(super) executable_revision: FunctionRevisionId,
    /// The durable revision pin description for the resolved class.
    pub(super) revision_pin: String,
}

/// Runs one local sealed `orna invoke` command in-process.
///
/// This compatibility entry point keeps the in-process test seam.
/// User-facing endpoint routing goes through [`run_installed_invoke_at`].
pub fn run_installed_invoke(
    request: InstalledInvokeRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    run_installed_invoke_with_transport(InvokeTransport::InProcess, request, stdout, stderr)
}

/// Runs one installed sealed invocation against the selected database endpoint.
///
/// Managed local and explicit Unix endpoints use the authenticated Orna socket.
/// Other endpoint kinds fail closed until their session bootstrap is available.
pub fn run_installed_invoke_at(
    endpoint: &DatabaseEndpoint,
    request: InstalledInvokeRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    let transport = endpoint_transport(endpoint)?;
    run_installed_invoke_with_transport(transport, request, stdout, stderr)
}

fn run_installed_invoke_with_transport(
    transport: InvokeTransport,
    request: InstalledInvokeRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    let host = inspect_current_embedded_host().map_err(map_host_error)?;
    let kernel = PostgresKernel::new(host.config().clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            InstalledInvokeError::new(
                InstalledInvokeErrorKind::Internal,
                "the private runtime could not start".to_owned(),
            )
        })?;

    runtime.block_on(host_invoke(kernel, request, stdout, stderr, transport))
}

pub(super) fn endpoint_transport(
    endpoint: &DatabaseEndpoint,
) -> Result<InvokeTransport, InstalledInvokeError> {
    match endpoint {
        DatabaseEndpoint::ManagedLocal { instance } if instance == "default" => Ok(
            InvokeTransport::UnixSocket(crate::embedded::active_runtime_root().join("orna.sock")),
        ),
        DatabaseEndpoint::ManagedLocal { instance } => Err(endpoint_error(format!(
            "managed local instance `{instance}` is not available in this binary",
        ))),
        DatabaseEndpoint::UnixSocket { path } => {
            let expected = crate::embedded::active_runtime_root().join("orna.sock");
            if path != &expected {
                return Err(endpoint_error(
                    "this Unix socket is not the current managed Orna instance",
                ));
            }
            Ok(InvokeTransport::UnixSocket(path.clone()))
        }
        DatabaseEndpoint::LocalPath { .. } => Err(endpoint_error(
            "local database paths need session bootstrap and are not available yet",
        )),
        DatabaseEndpoint::RemoteTls { .. } => Err(endpoint_error(
            "remote Orna URIs need TLS session bootstrap and are not available yet",
        )),
    }
}

fn endpoint_error(message: impl Into<String>) -> InstalledInvokeError {
    InstalledInvokeError::new(InstalledInvokeErrorKind::Authentication, message.into())
}
pub(super) fn connect_local_socket(path: &PathBuf) -> io::Result<StandardUnixStream> {
    let stream = StandardUnixStream::connect(path)?;
    stream.set_read_timeout(Some(RESOURCE_FRAME_TIMEOUT))?;
    stream.set_write_timeout(Some(RESOURCE_FRAME_TIMEOUT))?;
    Ok(stream)
}

/// Runs one installed sealed `orna invoke` command against a caller-supplied
/// kernel (ADR 0056 step 5 live-proof seam).
///
/// The public entry [`run_installed_invoke`] inspects the fixed private
/// instance and delegates here; the live proof drives the exact
/// reflect-bind-encode-authenticate-dispatch-render path against the Compose
/// PostgreSQL test kernel with the invoking process's local peer credentials.
/// Public consumers keep [`run_installed_invoke`]; this seam is hidden from
/// the documented API surface.
#[doc(hidden)]
pub async fn run_invoke_with_kernel(
    kernel: PostgresKernel,
    request: InstalledInvokeRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    host_invoke(kernel, request, stdout, stderr, InvokeTransport::InProcess).await
}

pub(super) fn bind_installed_cli_arguments(
    application: &orna_core::catalogue::CatalogueSnapshot,
    standard: Option<&VerifiedStandardLibrarySnapshot>,
    function: &FunctionDefinition,
    arguments: &[CliArgumentInput],
) -> Result<Vec<InvocationArgument>, orna_core::invocation_binding::InvocationBindingError> {
    let definition = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        function.domain(),
        function
            .parameters()
            .iter()
            .map(|parameter| {
                orna_core::catalogue::ParameterDefinition::new(
                    parameter.id(),
                    parameter.name(),
                    parameter.ordinal(),
                    installed_cli_resolved_type(application, standard, parameter.resolved_type()),
                    parameter.default_expression(),
                )
            })
            .collect(),
        function.return_type().clone(),
        function.current_revision(),
        function.security(),
        function.transaction(),
        function.volatility(),
    );
    bind_cli_arguments(&definition, arguments)
}

pub(super) fn installed_cli_resolved_type(
    application: &orna_core::catalogue::CatalogueSnapshot,
    standard: Option<&VerifiedStandardLibrarySnapshot>,
    resolved_type: ResolvedType,
) -> ResolvedType {
    let ResolvedType::Value(type_id) = resolved_type else {
        return resolved_type;
    };
    if application.type_definition_by_id(type_id).is_some() {
        return resolved_type;
    }
    let Some(value_type) =
        standard.and_then(|snapshot| snapshot.catalogue().value_type_by_id(type_id))
    else {
        return resolved_type;
    };
    if value_type.kind() != ValueTypeKind::Primitive
        || value_type.mutability() != ValueTypeMutability::Immutable
        || value_type.persistence() != ValueTypePersistence::Persistable
    {
        return resolved_type;
    }
    let scalar = match value_type.representation_contract() {
        "orna.kernel.value.boolean@1" => StandardScalar::Boolean,
        "orna.kernel.value.integer@1" => StandardScalar::Integer,
        "orna.kernel.value.bigint@1" => StandardScalar::BigInt,
        "orna.kernel.value.float@1" => StandardScalar::Float,
        "orna.kernel.value.character-large-object@1" => StandardScalar::CharacterLargeObject,
        "orna.kernel.value.binary-large-object@1" => StandardScalar::BinaryLargeObject,
        _ => return resolved_type,
    };
    ResolvedType::Scalar(scalar)
}

async fn host_invoke(
    kernel: PostgresKernel,
    request: InstalledInvokeRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    transport: InvokeTransport,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    let active = kernel.recover().await.map_err(|_| {
        InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the active revision could not be recovered".to_owned(),
        )
    })?;
    let standard = active.catalogue_hash_context().standard();
    let resolved = resolve_target(&active, standard, &request.target)?;

    let arguments = bind_installed_cli_arguments(
        active.catalogue(),
        standard,
        resolved.function,
        &request.arguments,
    )
    .map_err(|error| usage_error(error.to_string()))?;
    let ui_required = client_function_returns_ui(resolved.function);
    let selected = selected_runtime(&request, ui_required)?;
    let sealed = build_sealed_request(&request, arguments, selected)?;

    if request.explain {
        render_explain(
            stdout,
            resolved.function,
            &sealed,
            &resolved.executable_revision.canonical(),
            &resolved.revision_pin,
        )?;
        return Ok(InstalledInvokeOutcome::Completed);
    }

    let standard = standard.ok_or_else(|| {
        InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "sealed sys.invoke requires the verified standard snapshot".to_owned(),
        )
    })?;
    let registry = registered_opaque_codecs(standard).map_err(|_| {
        InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the verified standard snapshot does not bind its opaque codec registry".to_owned(),
        )
    })?;
    let retained = encode_invoke_request(&active, &registry, &sealed).map_err(|_| {
        InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the sealed request could not be encoded".to_owned(),
        )
    })?;

    let (broker, receiver) = SharedInvokeBroker::pending();
    let (mut server_task, client_end) = match transport {
        InvokeTransport::InProcess => {
            let (server_end, client_end) = StandardUnixStream::pair().map_err(|_| {
                InstalledInvokeError::new(
                    InstalledInvokeErrorKind::Authentication,
                    "the local invoke connection could not be created".to_owned(),
                )
            })?;
            let server_task = tokio::spawn(serve_local_raw_stream_with_broker(
                kernel.clone(),
                server_end,
                LocalRawSocketResources::new(),
                Some(broker.clone()),
            ));
            (Some(server_task), client_end)
        }
        InvokeTransport::UnixSocket(path) => {
            let client_end = connect_local_socket(&path).map_err(|_| {
                InstalledInvokeError::new(
                    InstalledInvokeErrorKind::Authentication,
                    format!(
                        "the local Orna socket could not be opened: {}",
                        path.display()
                    ),
                )
            })?;
            (None, client_end)
        }
    };
    if broker
        .activate(client_end, active.clone(), registry.clone(), receiver)
        .await
        .is_err()
    {
        broker.shutdown().await;
        if let Some(server_task) = server_task.take() {
            let mut server_task = server_task;
            if tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, &mut server_task)
                .await
                .is_err()
            {
                server_task.abort();
                let _ = server_task.await;
            }
        }
        return Err(InstalledInvokeError::new(
            InstalledInvokeErrorKind::Authentication,
            "the local invoke connection could not authenticate".to_owned(),
        ));
    }
    let result = broker.invoke(retained).await;
    broker.shutdown().await;
    if let Some(server_task) = server_task.take() {
        let mut server_task = server_task;
        if tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, &mut server_task)
            .await
            .is_err()
        {
            server_task.abort();
            let _ = server_task.await;
        }
    }
    if matches!(
        result.as_ref(),
        Err(ResourceTransportFailure::RootPreflightDenied)
    ) {
        writeln!(stderr, "orna: invoke: invocation denied").map_err(presentation_error)?;
        return Ok(InstalledInvokeOutcome::Denied);
    }
    let result = result.map_err(|error| match error {
        ResourceTransportFailure::Cancelled => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Cancelled,
            "invocation cancelled".to_owned(),
        ),
        ResourceTransportFailure::Shape => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the local invoke connection returned an invalid frame".to_owned(),
        ),
        ResourceTransportFailure::SessionInputUnavailable => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the client session input channel is unavailable".to_owned(),
        ),
        ResourceTransportFailure::RootPreflightDenied => {
            unreachable!("preflight denial handled before sealed result mapping")
        }
        ResourceTransportFailure::RootSealedDispatchInternal => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "sealed dispatch failed".to_owned(),
        ),
        ResourceTransportFailure::Transport => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the local invoke connection failed".to_owned(),
        ),
    })?;

    render_result(&result, request.no_progress, stdout, stderr, &mut |value| {
        encode_constructed_value(&active, &registry, value).map_err(|_| {
            InstalledInvokeError::new(
                InstalledInvokeErrorKind::Presentation,
                "a result value could not be encoded in its canonical typed form".to_owned(),
            )
        })
    })
}

/// Resolves one invocation target in the active application catalogue first
/// and the pinned verified standard catalogue second.
///
/// A function present in both catalogues resolves to neither (closed
/// ambiguity, the same rule as the sealed boundary); a function absent from
/// both is a not-found usage error.
pub(super) fn resolve_target<'a>(
    active: &'a ActiveDatabaseRevision,
    standard: Option<&'a VerifiedStandardLibrarySnapshot>,
    target: &InvocationTarget,
) -> Result<ResolvedTarget<'a>, InstalledInvokeError> {
    let application = active.catalogue();
    let standard_catalogue = standard.map(|standard| standard.catalogue());
    let (application_hit, standard_hit) = match target {
        InvocationTarget::FunctionId(id) => (
            application.function_by_id(*id),
            standard_catalogue.and_then(|catalogue| catalogue.function_by_id(*id)),
        ),
        InvocationTarget::QualifiedName(name) => (
            application.function_by_name(name),
            standard_catalogue.and_then(|catalogue| catalogue.function_by_name(name)),
        ),
        _ => (None, None),
    };
    match (application_hit, standard_hit) {
        (Some(_), Some(_)) => Err(usage_error(
            "the target resolves in both the application and standard catalogues".to_owned(),
        )),
        (Some(function), None) => Ok(ResolvedTarget {
            function,
            executable_revision: function.current_revision(),
            revision_pin: format!(
                "application catalogue {}",
                active.pair().catalogue().canonical()
            ),
        }),
        (None, Some(function)) => {
            let standard = standard.expect("a standard hit requires the standard snapshot");
            let executable = standard
                .executables()
                .iter()
                .find(|executable| executable.function() == function.id())
                .ok_or_else(|| {
                    InstalledInvokeError::new(
                        InstalledInvokeErrorKind::Internal,
                        "the verified standard catalogue function has no executable".to_owned(),
                    )
                })?;
            Ok(ResolvedTarget {
                function,
                executable_revision: executable.revision().id(),
                revision_pin: format!("verified standard {}", standard.revision().canonical()),
            })
        }
        (None, None) => Err(usage_error(
            "the target does not resolve to a function".to_owned(),
        )),
    }
}

fn invocation_argument_order(left: &InvocationArgument, right: &InvocationArgument) -> Ordering {
    use orna_core::invocation::InvocationParameterSelector;

    match (left.selector(), right.selector()) {
        (
            InvocationParameterSelector::ParameterId(left),
            InvocationParameterSelector::ParameterId(right),
        ) => left.to_bytes().cmp(&right.to_bytes()),
        (InvocationParameterSelector::ParameterId(_), InvocationParameterSelector::Name(_)) => {
            Ordering::Less
        }
        (InvocationParameterSelector::Name(_), InvocationParameterSelector::ParameterId(_)) => {
            Ordering::Greater
        }
        (InvocationParameterSelector::Name(left), InvocationParameterSelector::Name(right)) => {
            left.as_bytes().cmp(right.as_bytes())
        }
        _ => Ordering::Equal,
    }
}

pub(super) fn canonicalise_invocation_arguments(
    mut arguments: Vec<InvocationArgument>,
) -> Vec<InvocationArgument> {
    arguments.sort_by(invocation_argument_order);
    arguments
}

/// Builds one checked sealed `sys.invoke.Request` from the CLI request and
/// the bound typed arguments.
///
/// The caller context is `CliTty` when stdout is a terminal and `CliPipe`
/// otherwise, with locale and timezone from the environment. The client
/// offer carries the selected family's sink and runtime capabilities without
/// exposing a native library path.
pub(super) fn build_sealed_request(
    request: &InstalledInvokeRequest,
    arguments: Vec<InvocationArgument>,
    selected: RuntimeFamily,
) -> Result<InvokeRequest, InstalledInvokeError> {
    let arguments = canonicalise_invocation_arguments(arguments);
    let caller_context = build_caller_context()?;
    let runtime_offers = match selected {
        RuntimeFamily::Tty => installed_tty_runtime_offers(),
        RuntimeFamily::Qt => vec![installed_qt_runtime_offer()?],
        RuntimeFamily::NotInstalled => Vec::new(),
    };
    let client_offer = InvocationClientOffer::new(
        CONNECTION_PROTOCOL_MAJOR,
        caller_context.locale(),
        caller_context.timezone(),
        client_sink_offers(selected)?,
        runtime_offers,
        MAXIMUM_FRAME_SIZE,
        MAXIMUM_ARTIFACT_SIZE,
        None,
        None,
    )
    .map_err(|_| {
        InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the client offer could not be built".to_owned(),
        )
    })?;
    let output_requirement = request
        .output
        .as_deref()
        .map(build_output_requirement)
        .transpose()?;

    InvokeRequest::new(InvokeRequestInput {
        target: request.target.clone(),
        arguments,
        caller_context,
        client_offer,
        output_requirement,
        state_profile: None,
        trace_policy: request.trace.unwrap_or(InvocationTracePolicy::Off),
        idempotency_key: None,
        parent_invocation_id: None,
        observer_context: None,
    })
    .map_err(|error| usage_error(format!("the sealed request is invalid: {error}")))
}

/// Builds the runtime offer for the installed TTY runtime.
pub(super) fn installed_tty_runtime_offers() -> Vec<InvocationRuntimeOffer> {
    vec![
        InvocationRuntimeOffer::new(
            TTY_RUNTIME_NAME,
            TTY_RUNTIME_VERSION,
            [
                TypeDescriptor::named(STD_TERMINAL_DOCUMENT_TYPE_ID),
                TypeDescriptor::named(STD_IO_BYTE_STREAM_TYPE_ID),
            ],
            Vec::<InvocationRuntimeContract>::new(),
            0,
            true,
            None,
        )
        .expect("the tty runtime offer is structurally valid"),
    ]
}

/// Builds one pathless offer from the validated installed Qt descriptor.
pub(super) fn map_qt_runtime_load_error(
    _error: orna_client::RuntimeLoadError,
) -> InstalledInvokeError {
    InstalledInvokeError::new(
        InstalledInvokeErrorKind::Presentation,
        "the installed Qt runtime is unavailable".to_owned(),
    )
}

fn installed_qt_runtime_offer() -> Result<InvocationRuntimeOffer, InstalledInvokeError> {
    let library = RuntimeLibrary::load_installed_qt().map_err(map_qt_runtime_load_error)?;
    let descriptor = library.descriptor();
    let consumed_descriptors = descriptor
        .sinks
        .iter()
        .map(|sink| match sink.type_name.as_str() {
            "std.ui.UI" => Ok(TypeDescriptor::named(STD_UI_TYPE_ID)),
            _ => Err(InstalledInvokeError::new(
                InstalledInvokeErrorKind::Internal,
                "the installed Qt runtime advertises an unknown sink".to_owned(),
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let contracts = descriptor
        .contracts
        .iter()
        .map(|contract| {
            InvocationRuntimeContract::new(
                contract.name.clone(),
                format!("{}.{}", contract.major, contract.minor),
                contract.features.iter().cloned(),
            )
            .map_err(|_| {
                InstalledInvokeError::new(
                    InstalledInvokeErrorKind::Internal,
                    "the installed Qt runtime advertises an invalid contract".to_owned(),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    InvocationRuntimeOffer::new(
        descriptor.runtime_name.clone(),
        descriptor.runtime_version.clone(),
        consumed_descriptors,
        contracts,
        0,
        true,
        None,
    )
    .map_err(|_| {
        InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the installed Qt runtime offer is invalid".to_owned(),
        )
    })
}

/// Selects the local runtime family before sealed request construction.
pub(super) fn selected_runtime(
    request: &InstalledInvokeRequest,
    ui_required: bool,
) -> Result<RuntimeFamily, InstalledInvokeError> {
    match (request.runtime, ui_required) {
        (None, true) => Ok(RuntimeFamily::Qt),
        (None, false) | (Some(RuntimeFamily::Tty), false) => Ok(RuntimeFamily::Tty),
        (Some(RuntimeFamily::Tty), true) => Err(usage_error(
            "the tty runtime cannot consume a std.ui.UI result".to_owned(),
        )),
        (Some(RuntimeFamily::Qt), true) => Ok(RuntimeFamily::Qt),
        (Some(RuntimeFamily::Qt), false) => Err(usage_error(
            "the Qt runtime can consume only a std.ui.UI result".to_owned(),
        )),
        (Some(RuntimeFamily::NotInstalled), _) => Err(usage_error(
            "the not-installed runtime family is not installed".to_owned(),
        )),
    }
}

/// Returns whether the target's result is consumed by the graphical runtime.
fn client_function_returns_ui(function: &FunctionDefinition) -> bool {
    matches!(
        function.return_type(),
        FunctionReturn::Single(ResolvedType::Value(type_id)) if *type_id == STD_UI_TYPE_ID
    )
}

/// Builds the sink offers consumed by the selected local runtime.
pub(super) fn client_sink_offers(
    selected: RuntimeFamily,
) -> Result<Vec<InvocationSinkOffer>, InstalledInvokeError> {
    let document = InvocationSinkOffer::new(
        TypeDescriptor::named(STD_TERMINAL_DOCUMENT_TYPE_ID),
        [DOCUMENT_SINK_MEDIA_TYPE],
        false,
        0,
        None,
    )
    .map_err(|error| sink_offer_error("std.terminal.Document", error))?;
    let byte_stream = InvocationSinkOffer::new(
        TypeDescriptor::named(STD_IO_BYTE_STREAM_TYPE_ID),
        [BYTE_STREAM_SINK_MEDIA_TYPE],
        false,
        0,
        None,
    )
    .map_err(|error| sink_offer_error("std.io.ByteStream", error))?;
    let mut offers = vec![document, byte_stream];
    if matches!(selected, RuntimeFamily::Qt) {
        offers.push(
            InvocationSinkOffer::new(
                TypeDescriptor::named(STD_UI_TYPE_ID),
                ["application/orna-ui"],
                false,
                0,
                None,
            )
            .map_err(|error| sink_offer_error("std.ui.UI", error))?,
        );
    }
    Ok(offers)
}

/// Maps one structurally invalid sink offer to a closed internal error.
fn sink_offer_error(name: &str, error: InvocationCarrierConstructionError) -> InstalledInvokeError {
    InstalledInvokeError::new(
        InstalledInvokeErrorKind::Internal,
        format!("the {name} client sink offer is invalid: {error}"),
    )
}

/// Builds the checked caller context from the live process environment.
fn build_caller_context() -> Result<InvocationCallerContext, InstalledInvokeError> {
    let (stdout_is_tty, columns, rows) = caller_terminal_facts();
    let (kind, interactive) = if stdout_is_tty {
        (InvocationCallerKind::CliTty, true)
    } else {
        (InvocationCallerKind::CliPipe, false)
    };
    InvocationCallerContext::new(
        kind,
        interactive,
        stdout_is_tty,
        columns,
        rows,
        environment_locale(),
        environment_timezone(),
        None,
    )
    .map_err(|_| {
        InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the caller context could not be built".to_owned(),
        )
    })
}

/// Returns whether stdout is a terminal and its window size, when known.
///
/// A terminal whose window size cannot be read is treated as a pipe so the
/// caller context stays honest about the facts it records.
fn caller_terminal_facts() -> (bool, Option<u32>, Option<u32>) {
    if !io::stdout().is_terminal() {
        return (false, None, None);
    }
    match terminal_size() {
        Some((columns, rows)) if columns > 0 && rows > 0 => (true, Some(columns), Some(rows)),
        _ => (false, None, None),
    }
}

/// Reads the terminal window size from the standard-output descriptor.
fn terminal_size() -> Option<(u32, u32)> {
    use std::os::fd::AsRawFd;

    let mut size = nix::libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes one `winsize` through the supplied pointer.
    // The pointer is valid for the struct's lifetime and names the
    // standard-output descriptor, which the process owns.
    let result =
        unsafe { nix::libc::ioctl(io::stdout().as_raw_fd(), nix::libc::TIOCGWINSZ, &mut size) };
    if result == 0 && size.ws_col > 0 && size.ws_row > 0 {
        Some((size.ws_col as u32, size.ws_row as u32))
    } else {
        None
    }
}

/// Returns the caller locale from `LC_ALL` then `LANG`, with a stable
/// non-empty fallback.
fn environment_locale() -> String {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_else(|_| "C".to_owned())
}

/// Returns the caller timezone from `TZ`, with a stable non-empty fallback.
fn environment_timezone() -> String {
    std::env::var("TZ").unwrap_or_else(|_| "UTC".to_owned())
}

/// Classifies one raw `--output` value into its checked output requirement.
///
/// A media type contains `/`; a type name is a qualified semantic name of
/// two or more parts (one word is an alias, matching the documented
/// `--output json` example); anything else is an alias.
pub(super) fn build_output_requirement(
    value: &str,
) -> Result<InvocationOutputRequirement, InstalledInvokeError> {
    let streaming = InvocationStreamingRequirement::Unspecified;
    let requirement = if let Ok(name) =
        QualifiedSemanticName::new(value.split('.').map(str::to_owned))
        && name.parts().len() > 1
    {
        InvocationOutputRequirement::new(
            None,
            None,
            Some(InvocationOutputTypeSelector::QualifiedName(name)),
            streaming,
        )
    } else if value.contains('/') {
        InvocationOutputRequirement::new(None, Some(value.to_owned()), None, streaming)
    } else {
        InvocationOutputRequirement::new(Some(value.to_owned()), None, None, streaming)
    };
    requirement.map_err(|_| usage_error(format!("invalid --output value `{value}`")))
}

fn usage_error(message: String) -> InstalledInvokeError {
    InstalledInvokeError::new(InstalledInvokeErrorKind::Usage, message)
}

fn map_host_error(error: EmbeddedHostError) -> InstalledInvokeError {
    InstalledInvokeError::new(
        InstalledInvokeErrorKind::Internal,
        format!("the installed host is unavailable: {error}"),
    )
}
