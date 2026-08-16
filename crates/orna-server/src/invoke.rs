//! In-process sealed `sys.invoke` command host (ADR 0056 step 3).
//!
//! This module runs one `orna invoke` command against the fixed private
//! instance with the same host inspection and kernel access as `orna source
//! apply` and `orna security grant-execute`. It resolves the target in the
//! active application catalogue first and the pinned verified standard
//! catalogue second (closed ambiguity), reflects the signature and binds the
//! CLI strings through the step-2 binding model, builds one sealed
//! `sys.invoke.Request`, authenticates the local peer UID, dispatches through
//! [`PostgresKernel::dispatch_sealed_sys_invoke`], and renders the returned
//! event stream with clean channels:
//!
//! - `InvocationStarted` and `InvocationCompleted` are diagnostics to stderr
//!   unless `--no-progress`;
//! - every `ValueBatch` value goes to stdout in its canonical ORV5 typed
//!   encoding, one value per record, without progress or warning interleave;
//! - a `Denied` result prints one redacted denial line to stderr and exits 4;
//! - a bind failure prints one redacted bind line to stderr and exits 1.
//!
//! `--explain` renders the resolution and sealed request facts and exits
//! success without dispatching, authorising, or auditing.

use std::{
    error::Error,
    fmt,
    io::{self, IsTerminal, Write},
};

use orna_core::{
    FunctionRevisionId,
    catalogue::{FunctionDefinition, FunctionReturn, QualifiedSemanticName},
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationEventBody, InvocationOutputRequirement, InvocationOutputTypeSelector,
        InvocationStreamingRequirement, InvocationTarget, InvocationTracePolicy, InvokeRequest,
        InvokeRequestInput,
    },
    invocation_binding::{CliArgumentInput, bind_cli_arguments},
    revision::{ActiveDatabaseRevision, VerifiedStandardLibrarySnapshot},
    types::{ResolvedType, StandardScalar},
    value::RuntimeValue,
};
use orna_postgres::{PostgresKernel, PostgresKernelError, SealedInvocationResult};
use orna_protocol::{encode_constructed_value, encode_invoke_request};
use orna_standard::registered_opaque_codecs;

use crate::{EmbeddedHostError, inspect_ready_embedded_host};

/// The sealed connection protocol major offered by every host run.
const CONNECTION_PROTOCOL_MAJOR: u16 = 5;
/// The minimum accepted client frame size (protocol version 1 offer limit).
const MAXIMUM_FRAME_SIZE: u32 = 1_024;
/// The first client run offers no artifact budget.
const MAXIMUM_ARTIFACT_SIZE: u64 = 0;

/// One complete installed `orna invoke` command request (ADR 0056).
///
/// The command parser (step 4) strips option prefixes and splits
/// `--arg <parameter>=<value>` pairs into [`CliArgumentInput`] values before
/// constructing this request; the host reflects the resolved signature and
/// binds them.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct InstalledInvokeRequest {
    /// The target selector exactly as supplied.
    pub target: InvocationTarget,
    /// Raw CLI arguments to bind against the resolved signature.
    pub arguments: Vec<CliArgumentInput>,
    /// The raw `--output <alias|media-type|type-name>` value, when present.
    pub output: Option<String>,
    /// The `--trace` policy, when present; absent means off.
    pub trace: Option<InvocationTracePolicy>,
    /// Suppress progress diagnostics (`--no-progress`).
    pub no_progress: bool,
    /// Print the plan instead of dispatching (`--explain`).
    pub explain: bool,
}

impl InstalledInvokeRequest {
    /// Creates one complete installed invoke command request.
    ///
    /// The command parser (step 4) passes its parsed target, argument
    /// inputs, and option values through this constructor; the host reflects
    /// the resolved signature and binds them.
    pub fn new(
        target: InvocationTarget,
        arguments: Vec<CliArgumentInput>,
        output: Option<String>,
        trace: Option<InvocationTracePolicy>,
        no_progress: bool,
        explain: bool,
    ) -> Self {
        Self {
            target,
            arguments,
            output,
            trace,
            no_progress,
            explain,
        }
    }
}

/// The terminal public result of one installed sealed invocation run.
///
/// The CLI maps each variant to the ADR 0056 exit table: `Completed` 0,
/// `TargetFailure` 1, `Denied` 4, `Cancelled` 6.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledInvokeOutcome {
    /// The invocation completed and its event stream was rendered.
    Completed,
    /// The sealed boundary disclosed a redacted bind failure.
    TargetFailure,
    /// The sealed boundary denied the invocation without executing.
    Denied,
    /// The invocation stream disclosed a redacted cancellation.
    Cancelled,
}

/// The closed failure class of one installed sealed invocation.
///
/// The CLI maps each kind to the ADR 0056 exit table: `Usage` 2,
/// `Authentication` 3, `Authorisation` 4, `Presentation` 5, `Cancelled` 6,
/// `Internal` 7.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledInvokeErrorKind {
    /// Usage / argument conversion.
    Usage,
    /// Connection / authentication.
    Authentication,
    /// Authorization / capability.
    Authorisation,
    /// Presentation / runtime output.
    Presentation,
    /// Cancelled / deadline.
    Cancelled,
    /// Protocol / internal.
    Internal,
}

/// A failure that prevents or ends one installed sealed invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct InstalledInvokeError {
    kind: InstalledInvokeErrorKind,
    message: String,
}

impl InstalledInvokeError {
    /// Creates one closed host failure with its redacted message.
    pub const fn new(kind: InstalledInvokeErrorKind, message: String) -> Self {
        Self { kind, message }
    }

    /// Returns the closed failure class.
    pub const fn kind(&self) -> InstalledInvokeErrorKind {
        self.kind
    }

    /// Returns the redacted failure message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for InstalledInvokeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "orna: invoke: {}", self.message)
    }
}

impl Error for InstalledInvokeError {}

/// The private host resolution of one invocation target.
struct ResolvedTarget<'a> {
    /// The resolved function signature in the owning catalogue.
    function: &'a FunctionDefinition,
    /// The exact immutable executable revision for the resolved class.
    executable_revision: FunctionRevisionId,
    /// The durable revision pin description for the resolved class.
    revision_pin: String,
}

/// Runs one installed sealed `orna invoke` command in-process.
///
/// The host inspection retains the package and instance guards for the
/// complete recovery, authentication, dispatch, and rendering operation. All
/// result values are written to `stdout`; every diagnostic, denial, and bind
/// failure is written to `stderr`.
///
/// # Errors
///
/// Returns [`InstalledInvokeError`] for host inspection, recovery, target
/// resolution, binding, sealed request construction, encoding,
/// authentication, dispatch, or rendering failures.
pub fn run_installed_invoke(
    request: InstalledInvokeRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    let host = inspect_ready_embedded_host().map_err(map_host_error)?;
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

    runtime.block_on(run_invoke_with_kernel(kernel, request, stdout, stderr))
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
    host_invoke(kernel, request, stdout, stderr).await
}

async fn host_invoke(
    kernel: PostgresKernel,
    request: InstalledInvokeRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    let active = kernel.recover().await.map_err(|_| {
        InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the active revision could not be recovered".to_owned(),
        )
    })?;
    let standard = active.catalogue_hash_context().standard();
    let resolved = resolve_target(&active, standard, &request.target)?;

    let arguments = bind_cli_arguments(resolved.function, &request.arguments)
        .map_err(|error| usage_error(error.to_string()))?;
    let sealed = build_sealed_request(&request, arguments)?;

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

    let uid = nix::unistd::geteuid().as_raw();
    let session = kernel
        .authenticate_local_peer(uid)
        .await
        .map_err(map_authentication_error)?;
    let result = kernel
        .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
        .await
        .map_err(map_dispatch_error)?;

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
fn resolve_target<'a>(
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

/// Builds one checked sealed `sys.invoke.Request` from the CLI request and
/// the bound typed arguments.
///
/// The caller context is `CliTty` when stdout is a terminal and `CliPipe`
/// otherwise, with locale and timezone from the environment. The client
/// offer is protocol major 5 with empty sink and runtime offer lists and the
/// default limits.
fn build_sealed_request(
    request: &InstalledInvokeRequest,
    arguments: Vec<InvocationArgument>,
) -> Result<InvokeRequest, InstalledInvokeError> {
    let caller_context = build_caller_context()?;
    let client_offer = InvocationClientOffer::new(
        CONNECTION_PROTOCOL_MAJOR,
        caller_context.locale(),
        caller_context.timezone(),
        Vec::new(),
        Vec::new(),
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
fn build_output_requirement(
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

/// Renders one sealed invocation result into the supplied writers.
///
/// Progress diagnostics, denials, and bind failures go to `stderr`; every
/// `ValueBatch` value goes to `stdout` through the supplied encoder, one
/// canonical record per value, with no progress or warning interleave.
fn render_result(
    result: &SealedInvocationResult,
    no_progress: bool,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    encode: &mut impl FnMut(&RuntimeValue) -> Result<Vec<u8>, InstalledInvokeError>,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    match result {
        SealedInvocationResult::Completed { events, .. } => {
            render_event_stream(events, no_progress, stdout, stderr, encode)
        }
        SealedInvocationResult::BindFailure { .. } => {
            writeln!(stderr, "orna: invoke: invocation binding failed")
                .map_err(presentation_error)?;
            Ok(InstalledInvokeOutcome::TargetFailure)
        }
        SealedInvocationResult::Denied { .. } => {
            writeln!(stderr, "orna: invoke: invocation denied").map_err(presentation_error)?;
            Ok(InstalledInvokeOutcome::Denied)
        }
    }
}

/// Renders one sealed Event batch in record order.
fn render_event_stream(
    events: &orna_protocol::InvocationEventBatch,
    no_progress: bool,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    encode: &mut impl FnMut(&RuntimeValue) -> Result<Vec<u8>, InstalledInvokeError>,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    let mut outcome = InstalledInvokeOutcome::Completed;
    for record in events.records() {
        match record.event().body() {
            InvocationEventBody::Started { .. } if !no_progress => {
                writeln!(stderr, "orna: invoke: invocation started").map_err(presentation_error)?;
            }
            InvocationEventBody::ValueBatch { values, .. } => {
                for value in values {
                    let encoded = encode(value.value())?;
                    stdout.write_all(&encoded).map_err(presentation_error)?;
                    stdout.write_all(b"\n").map_err(presentation_error)?;
                }
            }
            InvocationEventBody::Completed {
                duration_nanoseconds,
            } if !no_progress => {
                writeln!(
                    stderr,
                    "orna: invoke: invocation completed in {duration_nanoseconds}ns"
                )
                .map_err(presentation_error)?;
            }
            InvocationEventBody::Diagnostic(diagnostic) => {
                writeln!(
                    stderr,
                    "orna: invoke: {}: {}",
                    diagnostic.code(),
                    diagnostic.message()
                )
                .map_err(presentation_error)?;
            }
            InvocationEventBody::Failed(_) => {
                writeln!(stderr, "orna: invoke: invocation failed").map_err(presentation_error)?;
                outcome = InstalledInvokeOutcome::TargetFailure;
            }
            InvocationEventBody::Cancelled { .. } => {
                writeln!(stderr, "orna: invoke: invocation cancelled")
                    .map_err(presentation_error)?;
                outcome = InstalledInvokeOutcome::Cancelled;
            }
            _ => {}
        }
    }
    Ok(outcome)
}

/// Renders the `--explain` plan: resolved target identity and revision,
/// domain, parameters, return type, and the sealed request facts.
fn render_explain(
    output: &mut impl Write,
    function: &FunctionDefinition,
    request: &InvokeRequest,
    executable_revision: &str,
    revision_pin: &str,
) -> Result<(), InstalledInvokeError> {
    let mut plan = String::new();
    plan.push_str(&format!(
        "target: {} ({})\n",
        function.name(),
        function.id().canonical()
    ));
    plan.push_str(&format!(
        "revision: {executable_revision} (pinned to {revision_pin})\n"
    ));
    plan.push_str(&format!("domain: {:?}\n", function.domain()));
    plan.push_str("parameters:\n");
    for parameter in function.parameters() {
        plan.push_str(&format!(
            "  {} ({}): {}\n",
            parameter.name(),
            parameter.id().canonical(),
            render_resolved_type(parameter.resolved_type())
        ));
    }
    plan.push_str(&format!(
        "return: {}\n",
        render_return_type(function.return_type())
    ));
    plan.push_str("request:\n");
    plan.push_str(&format!("  target: {}\n", render_target(request.target())));
    plan.push_str(&format!(
        "  caller: {}\n",
        render_caller_kind(request.caller_context().kind())
    ));
    plan.push_str(&format!(
        "  offer: protocol {}, locale {}, timezone {}, sinks {}, runtimes {}, maximum frame {}, maximum artifact {}\n",
        request.client_offer().protocol_major(),
        request.client_offer().locale(),
        request.client_offer().timezone(),
        request.client_offer().sink_offers().len(),
        request.client_offer().runtime_offers().len(),
        request.client_offer().maximum_frame_size(),
        request.client_offer().maximum_artifact_size(),
    ));
    plan.push_str(&format!("  trace: {:?}\n", request.trace_policy()));
    plan.push_str(&format!(
        "  output: {}\n",
        render_output_requirement(request.output_requirement())
    ));

    output
        .write_all(plan.as_bytes())
        .map_err(presentation_error)?;
    Ok(())
}

/// Renders one resolved type in the ADR 0056 conversion-table spelling.
fn render_resolved_type(resolved: ResolvedType) -> String {
    match resolved {
        ResolvedType::Scalar(scalar) => render_scalar(scalar).to_owned(),
        ResolvedType::Named(id) => format!("named {}", id.canonical()),
        ResolvedType::Reference { target } => format!("REF {}", target.canonical()),
        ResolvedType::Value(id) => format!("value {}", id.canonical()),
    }
}

fn render_scalar(scalar: StandardScalar) -> &'static str {
    match scalar {
        StandardScalar::Boolean => "BOOLEAN",
        StandardScalar::Integer => "INTEGER",
        StandardScalar::BigInt => "BIGINT",
        StandardScalar::Float => "FLOAT",
        StandardScalar::Decimal => "DECIMAL",
        StandardScalar::CharacterLargeObject => "TEXT",
        StandardScalar::BinaryLargeObject => "BYTES",
        StandardScalar::Uuid => "UUID",
        StandardScalar::Date => "DATE",
        StandardScalar::Time => "TIME",
        StandardScalar::Timestamp => "TIMESTAMP",
        StandardScalar::Duration => "DURATION",
        StandardScalar::Void => "VOID",
    }
}

fn render_return_type(return_type: &FunctionReturn) -> String {
    match return_type {
        FunctionReturn::Single(resolved) => render_resolved_type(*resolved),
        FunctionReturn::Rows(columns) => {
            let columns = columns
                .iter()
                .map(|column| {
                    format!(
                        "{} {}",
                        column.name(),
                        render_resolved_type(column.resolved_type())
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("ROWS ({columns})")
        }
    }
}

fn render_target(target: &InvocationTarget) -> String {
    match target {
        InvocationTarget::FunctionId(id) => format!("function {}", id.canonical()),
        InvocationTarget::QualifiedName(name) => name.to_string(),
        _ => "unknown".to_owned(),
    }
}

fn render_caller_kind(kind: InvocationCallerKind) -> &'static str {
    match kind {
        InvocationCallerKind::CliTty => "CliTty",
        InvocationCallerKind::CliPipe => "CliPipe",
        _ => "other",
    }
}

fn render_output_requirement(requirement: Option<&InvocationOutputRequirement>) -> String {
    let Some(requirement) = requirement else {
        return "none".to_owned();
    };
    let selector = match requirement.type_selector() {
        Some(InvocationOutputTypeSelector::TypeId(id)) => format!("type {}", id.canonical()),
        Some(InvocationOutputTypeSelector::QualifiedName(name)) => format!("type {name}"),
        _ => String::new(),
    };
    let fields = [
        requirement
            .alias()
            .map(|alias| format!("alias {alias}"))
            .unwrap_or_default(),
        requirement
            .media_type()
            .map(|media_type| format!("media type {media_type}"))
            .unwrap_or_default(),
        selector,
    ]
    .into_iter()
    .filter(|field| !field.is_empty())
    .collect::<Vec<_>>();
    let mut rendered = if fields.is_empty() {
        "unspecified".to_owned()
    } else {
        fields.join(", ")
    };
    if requirement.streaming() != InvocationStreamingRequirement::Unspecified {
        rendered.push_str(&format!(", streaming {:?}", requirement.streaming()));
    }
    rendered
}

fn usage_error(message: String) -> InstalledInvokeError {
    InstalledInvokeError::new(InstalledInvokeErrorKind::Usage, message)
}

fn presentation_error(error: io::Error) -> InstalledInvokeError {
    InstalledInvokeError::new(
        InstalledInvokeErrorKind::Presentation,
        format!("cannot write command output: {error}"),
    )
}

fn map_host_error(error: EmbeddedHostError) -> InstalledInvokeError {
    InstalledInvokeError::new(
        InstalledInvokeErrorKind::Internal,
        format!("the installed host is unavailable: {error}"),
    )
}

fn map_authentication_error(error: PostgresKernelError) -> InstalledInvokeError {
    match error {
        PostgresKernelError::LocalPeerAuthentication(_) => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Authentication,
            "the local peer could not be authenticated".to_owned(),
        ),
        other => map_dispatch_error(other),
    }
}

fn map_dispatch_error(error: PostgresKernelError) -> InstalledInvokeError {
    InstalledInvokeError::new(
        InstalledInvokeErrorKind::Internal,
        format!("sealed dispatch failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::{
        FunctionId, InvocationId, ParameterId,
        catalogue::{
            FunctionDomain, FunctionReturn, FunctionSecurity, FunctionVolatility,
            ParameterDefinition,
        },
        invocation::{InvokeEvent, InvokeValue},
        types::StandardScalar,
        value::RuntimeValue,
    };
    use orna_protocol::{InvocationEventBatch, InvocationEventRecord};

    const ENCODED_VALUE: &[u8] = b"ORV5-encoded-value";

    fn encoded_record() -> Vec<u8> {
        [ENCODED_VALUE, b"\n"].concat()
    }

    fn encoder(value: &RuntimeValue) -> Result<Vec<u8>, InstalledInvokeError> {
        let _ = value;
        Ok(ENCODED_VALUE.to_vec())
    }

    fn echo_events() -> InvocationEventBatch {
        let invocation = InvocationId::new();
        let started = InvokeEvent::new(
            invocation,
            0,
            InvocationEventBody::Started {
                visible_principal: None,
            },
        )
        .expect("started event");
        let values = InvokeEvent::new(
            invocation,
            1,
            InvocationEventBody::value_batch(
                None,
                [InvokeValue::new(RuntimeValue::Integer(41)).expect("integer value")],
            )
            .expect("value batch body"),
        )
        .expect("values event");
        let completed = InvokeEvent::new(
            invocation,
            2,
            InvocationEventBody::Completed {
                duration_nanoseconds: 7,
            },
        )
        .expect("completed event");
        InvocationEventBatch::new(vec![
            InvocationEventRecord::new(1, started),
            InvocationEventRecord::new(2, values),
            InvocationEventRecord::new(3, completed),
        ])
        .expect("event batch")
    }

    #[test]
    fn values_go_to_stdout_and_progress_to_stderr_without_interleave() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let outcome = render_event_stream(
            &echo_events(),
            false,
            &mut stdout,
            &mut stderr,
            &mut encoder,
        )
        .expect("rendering succeeds");
        assert_eq!(outcome, InstalledInvokeOutcome::Completed);
        assert_eq!(stdout, encoded_record());
        let stderr = String::from_utf8(stderr).expect("stderr is text");
        assert!(stderr.contains("invocation started"));
        assert!(stderr.contains("invocation completed in 7ns"));
    }

    #[test]
    fn no_progress_suppresses_diagnostics_but_keeps_values() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let outcome =
            render_event_stream(&echo_events(), true, &mut stdout, &mut stderr, &mut encoder)
                .expect("rendering succeeds");
        assert_eq!(outcome, InstalledInvokeOutcome::Completed);
        assert_eq!(stdout, encoded_record());
        assert!(stderr.is_empty());
    }

    #[test]
    fn each_value_writes_one_canonical_record() {
        let invocation = InvocationId::new();
        let values = InvokeEvent::new(
            invocation,
            0,
            InvocationEventBody::value_batch(
                None,
                [
                    InvokeValue::new(RuntimeValue::Integer(1)).expect("first value"),
                    InvokeValue::new(RuntimeValue::Integer(2)).expect("second value"),
                ],
            )
            .expect("value batch body"),
        )
        .expect("values event");
        let batch = InvocationEventBatch::new(vec![InvocationEventRecord::new(1, values)])
            .expect("event batch");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let outcome = render_event_stream(&batch, false, &mut stdout, &mut stderr, &mut encoder)
            .expect("rendering succeeds");
        assert_eq!(outcome, InstalledInvokeOutcome::Completed);
        assert_eq!(stdout, [encoded_record(), encoded_record()].concat());
    }

    #[test]
    fn denied_prints_one_redacted_line_and_exits_denied() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = SealedInvocationResult::Denied {
            invocation: InvocationId::new(),
        };
        let outcome = render_result(&result, false, &mut stdout, &mut stderr, &mut encoder)
            .expect("rendering succeeds");
        assert_eq!(outcome, InstalledInvokeOutcome::Denied);
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8(stderr).expect("stderr is text"),
            "orna: invoke: invocation denied\n"
        );
    }

    #[test]
    fn bind_failure_prints_one_redacted_line_and_exits_target_failure() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = SealedInvocationResult::BindFailure {
            invocation: InvocationId::new(),
        };
        let outcome = render_result(&result, false, &mut stdout, &mut stderr, &mut encoder)
            .expect("rendering succeeds");
        assert_eq!(outcome, InstalledInvokeOutcome::TargetFailure);
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8(stderr).expect("stderr is text"),
            "orna: invoke: invocation binding failed\n"
        );
    }

    fn echo_definition() -> FunctionDefinition {
        FunctionDefinition::new(
            FunctionId::from_bytes([0x10; 16]),
            QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("qualified name"),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                ParameterId::from_bytes([0x10; 16]),
                "p_value",
                0,
                ResolvedType::Scalar(StandardScalar::Integer),
                None,
            )],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
            FunctionRevisionId::from_bytes([0x20; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )
    }

    fn pipe_request() -> InvokeRequest {
        let caller = InvocationCallerContext::new(
            InvocationCallerKind::CliPipe,
            false,
            false,
            None,
            None,
            "en-GB",
            "UTC",
            None,
        )
        .expect("pipe caller context");
        let offer = InvocationClientOffer::new(
            5,
            "en-GB",
            "UTC",
            Vec::new(),
            Vec::new(),
            MAXIMUM_FRAME_SIZE,
            MAXIMUM_ARTIFACT_SIZE,
            None,
            None,
        )
        .expect("client offer");
        InvokeRequest::new(InvokeRequestInput {
            target: InvocationTarget::qualified_name(
                QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("qualified name"),
            )
            .expect("target"),
            arguments: Vec::new(),
            caller_context: caller,
            client_offer: offer,
            output_requirement: None,
            state_profile: None,
            trace_policy: InvocationTracePolicy::Off,
            idempotency_key: None,
            parent_invocation_id: None,
            observer_context: None,
        })
        .expect("checked request")
    }

    #[test]
    fn explain_renders_resolution_and_sealed_request_facts() {
        let mut output = Vec::new();
        render_explain(
            &mut output,
            &echo_definition(),
            &pipe_request(),
            "function-rev:test",
            "verified standard std:test",
        )
        .expect("explain renders");
        let plan = String::from_utf8(output).expect("plan is text");
        assert!(plan.contains("target: std.invoke.echo (function:"));
        assert!(
            plan.contains("revision: function-rev:test (pinned to verified standard std:test)")
        );
        assert!(plan.contains("domain: Server"));
        assert!(plan.contains("p_value (parameter:"));
        assert!(plan.contains(": INTEGER"));
        assert!(plan.contains("return: INTEGER"));
        assert!(plan.contains("request:"));
        assert!(plan.contains("target: std.invoke.echo"));
        assert!(plan.contains("caller: CliPipe"));
        assert!(
            plan.contains("offer: protocol 5, locale en-GB, timezone UTC, sinks 0, runtimes 0")
        );
        assert!(plan.contains("trace: Off"));
        assert!(plan.contains("output: none"));
    }

    #[test]
    fn output_requirement_classifies_alias_media_type_and_type_name() {
        let alias = build_output_requirement("json").expect("alias requirement");
        assert_eq!(alias.alias(), Some("json"));
        assert_eq!(alias.media_type(), None);
        assert!(alias.type_selector().is_none());

        let media = build_output_requirement("application/json").expect("media requirement");
        assert_eq!(media.alias(), None);
        assert_eq!(media.media_type(), Some("application/json"));
        assert!(media.type_selector().is_none());

        let typed = build_output_requirement("std.ui.UI").expect("type requirement");
        assert_eq!(typed.alias(), None);
        assert_eq!(typed.media_type(), None);
        assert!(matches!(
            typed.type_selector(),
            Some(InvocationOutputTypeSelector::QualifiedName(name))
                if name.to_string() == "std.ui.UI"
        ));

        let error = build_output_requirement("").expect_err("empty output is a usage error");
        assert_eq!(error.kind(), InstalledInvokeErrorKind::Usage);
    }

    #[test]
    fn error_kinds_map_to_the_spec_exit_table() {
        let cases = [
            (InstalledInvokeErrorKind::Usage, 2),
            (InstalledInvokeErrorKind::Authentication, 3),
            (InstalledInvokeErrorKind::Authorisation, 4),
            (InstalledInvokeErrorKind::Presentation, 5),
            (InstalledInvokeErrorKind::Cancelled, 6),
            (InstalledInvokeErrorKind::Internal, 7),
        ];
        for (kind, exit) in cases {
            assert_eq!(exit_code_for_test(kind), exit, "{kind:?}");
        }
    }

    fn exit_code_for_test(kind: InstalledInvokeErrorKind) -> u8 {
        match kind {
            InstalledInvokeErrorKind::Usage => 2,
            InstalledInvokeErrorKind::Authentication => 3,
            InstalledInvokeErrorKind::Authorisation => 4,
            InstalledInvokeErrorKind::Presentation => 5,
            InstalledInvokeErrorKind::Cancelled => 6,
            InstalledInvokeErrorKind::Internal => 7,
        }
    }

    #[test]
    fn binding_rejects_positional_and_unknown_inputs_as_usage() {
        let definition = echo_definition();
        let positional = bind_cli_arguments(
            &definition,
            &[CliArgumentInput::Positional("41".to_owned())],
        )
        .expect_err("a positional argument is rejected");
        assert_eq!(
            positional.to_string(),
            "unexpected positional argument `41`"
        );

        let unknown = bind_cli_arguments(
            &definition,
            &[CliArgumentInput::Friendly {
                name: "p_other".to_owned(),
                value: "1".to_owned(),
            }],
        )
        .expect_err("an unknown parameter is rejected");
        assert_eq!(unknown.to_string(), "unknown parameter `p_other`");
    }
}
