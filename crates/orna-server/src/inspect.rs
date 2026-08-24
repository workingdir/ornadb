//! Installed `orna inspect` access to the durable Inspector core.
//!
//! This module runs one closed `orna inspect` command against the fixed
//! private instance with the same host inspection and kernel access as
//! `orna state` (ADR 0061 step 5). The server derives the principal from the
//! authenticated session — the local peer UID authenticated through
//! [`PostgresKernel::authenticate_local_peer`] — and a request never carries
//! a principal identity.
//!
//! One command resolves the inspection epoch for the invocation (the latest
//! captured epoch, or the exact `--epoch` override), runs the requested
//! projection and/or the sequence-addressable trace stream (ADR 0064), and
//! renders every row as one JSON record per line, with typed values in their
//! canonical ORV5 hex form. A bare command (no projection and no trace)
//! renders the epoch summary record.
//!
//! Redaction follows the kernel: `state_cells` values are `null` unless the
//! request arms the `Values` classifier, and the epoch itself already
//! redacted every state-cell value captured without it.

use std::{fmt, io, io::Write, time::SystemTime};

use orna_core::{
    CatalogueRevisionId, FunctionId, InspectEpochId, InvocationId, PrincipalId, SourceRevisionId,
    inspect::{
        CallRow, InspectInvocationNodeKind, InspectInvocationPhase, InspectOutcomeKind,
        InspectPrivilege, InspectResourceKind, InspectResourceStatus, InspectResultSummary,
        InspectSecurityDecisionKind, InspectSecurityDecisionOutcome, InspectSnapshotSummary,
        InspectTraceEvent, InspectTraceEventKind, InspectTracePayload, InvocationNodeRow,
        PresentationCandidateRow, ResourceRow, RuntimeBindingRow, SecurityDecisionRow,
        StateCellRow, UiNodeRow, stable_inspect_error_code,
    },
    invocation::InvokeValue,
    types::{TypeDescriptor, TypeDescriptorKind},
};
use orna_postgres::{AuthenticatedInspectSnapshot, PostgresKernel, PostgresKernelError};
use orna_protocol::{ValueCodecError, encode_constructed_value};
use orna_standard::registered_opaque_codecs;

use crate::{EmbeddedHostError, inspect_ready_embedded_host};

/// One complete installed `orna inspect` command request (ADR 0064 wave 3).
///
/// The command parser parses the invocation identity, the optional exact
/// epoch override, the optional projection selector, the trace switch and
/// resume sequence, and the four classifier flags into this closed request;
/// the host derives the session principal and dispatches. The installed
/// command has no trusted CLIENT observer invocation, so trace suppression
/// uses the target invocation by default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct InstalledInspectRequest {
    /// The invocation whose inspection record is requested.
    pub invocation: InvocationId,
    /// The exact epoch override; `None` resolves the latest captured epoch
    /// for the invocation.
    pub epoch: Option<InspectEpochId>,
    /// The requested projection over the resolved epoch.
    pub projection: Option<InstalledInspectProjection>,
    /// Whether to stream the invocation trace after `after_sequence`.
    pub trace: bool,
    /// The resume sequence: only trace events with `sequence > after_sequence`
    /// are streamed. The default 0 streams the whole model-expressible trace.
    pub after_sequence: u64,
    /// Requests the `Values` classifier: typed values are rendered, never
    /// redacted to `null`.
    pub include_values: bool,
    /// Requests the `Source` classifier. No v1 projection captures source
    /// text yet, so the flag only arms the classification dimension.
    pub include_source: bool,
    /// Requests the `SecurityDetails` classifier for the
    /// `security_decisions` projection.
    pub include_security: bool,
    /// Requests the `RuntimeInternals` classifier for the
    /// `runtime_bindings` projection.
    pub include_runtime: bool,
}

impl InstalledInspectRequest {
    /// Creates one complete installed inspect command request.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        invocation: InvocationId,
        epoch: Option<InspectEpochId>,
        projection: Option<InstalledInspectProjection>,
        trace: bool,
        after_sequence: u64,
        include_values: bool,
        include_source: bool,
        include_security: bool,
        include_runtime: bool,
    ) -> Self {
        Self {
            invocation,
            epoch,
            projection,
            trace,
            after_sequence,
            include_values,
            include_source,
            include_security,
            include_runtime,
        }
    }
}

/// One closed `orna inspect` projection selector (ADR 0064).
///
/// The eight names match the sealed `sys.inspect.*` projection functions
/// exactly. The CLI parser admits only these names; any other name is a
/// usage error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledInspectProjection {
    /// The `invocation_nodes` projection.
    InvocationNodes,
    /// The `calls` projection.
    Calls,
    /// The `resources` projection.
    Resources,
    /// The `state_cells` projection.
    StateCells,
    /// The `ui_nodes` projection.
    UiNodes,
    /// The `presentation_candidates` projection.
    PresentationCandidates,
    /// The `runtime_bindings` projection.
    RuntimeBindings,
    /// The `security_decisions` projection.
    SecurityDecisions,
}

impl InstalledInspectProjection {
    /// Parses one closed projection name; any other name is `None`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "invocation_nodes" => Some(Self::InvocationNodes),
            "calls" => Some(Self::Calls),
            "resources" => Some(Self::Resources),
            "state_cells" => Some(Self::StateCells),
            "ui_nodes" => Some(Self::UiNodes),
            "presentation_candidates" => Some(Self::PresentationCandidates),
            "runtime_bindings" => Some(Self::RuntimeBindings),
            "security_decisions" => Some(Self::SecurityDecisions),
            _ => None,
        }
    }
}

/// The terminal public result of one installed inspect command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledInspectOutcome {
    /// The inspection command completed and its records were rendered.
    Completed,
}

/// The closed failure class of one installed inspect command.
///
/// The CLI maps each kind to a closed exit code: `Usage` 2, `Kernel` 1,
/// `Rendering` 5, `Internal` 7.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledInspectErrorKind {
    /// The command failed closed as a usage error.
    Usage,
    /// The protected inspection operation failed closed with an inspect
    /// error: a missing epoch, an INSPECT denial, or a payload codec
    /// failure.
    Kernel,
    /// A rendered record could not reach standard output.
    Rendering,
    /// Host inspection, recovery, authentication, or another kernel failure.
    Internal,
}

/// A failure that prevents or ends one installed inspect command.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct InstalledInspectError {
    kind: InstalledInspectErrorKind,
    message: String,
    code: Option<&'static str>,
}

impl InstalledInspectError {
    /// Creates one closed inspect failure with its message.
    pub fn new(kind: InstalledInspectErrorKind, message: String) -> Self {
        Self {
            kind,
            message,
            code: None,
        }
    }

    /// Creates one closed inspect failure carrying a stable audit reason.
    pub fn with_code(kind: InstalledInspectErrorKind, message: String, code: &'static str) -> Self {
        Self {
            kind,
            message,
            code: Some(code),
        }
    }

    /// Returns the closed failure class.
    pub const fn kind(&self) -> InstalledInspectErrorKind {
        self.kind
    }

    /// Returns the stable closed audit reason for spec-flavoured failures.
    pub const fn code(&self) -> Option<&'static str> {
        self.code
    }

    /// Returns the closed failure message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for InstalledInspectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "orna inspect: {}", self.message)
    }
}

impl std::error::Error for InstalledInspectError {}

/// Runs one installed `orna inspect` command in-process.
///
/// The host inspection retains the package and instance guards for the
/// complete authentication, epoch resolution, projection, trace, and
/// rendering operation. All result records are written to `stdout` as JSON
/// lines; failures are returned to the CLI, which writes them to `stderr`.
///
/// # Errors
///
/// Returns [`InstalledInspectError`] for host inspection, recovery,
/// authentication, epoch resolution, privilege, value codec, kernel, or
/// rendering failures.
pub fn run_installed_inspect(
    request: InstalledInspectRequest,
    stdout: &mut impl Write,
) -> Result<InstalledInspectOutcome, InstalledInspectError> {
    let host = inspect_ready_embedded_host().map_err(map_host_error)?;
    let kernel = PostgresKernel::new(host.config().clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| runtime_unavailable_error())?;

    runtime.block_on(run_inspect_with_kernel(kernel, request, stdout))
}

/// Runs one installed `orna inspect` command against a caller-supplied
/// kernel (ADR 0064 wave 3 live-proof seam).
///
/// The public entry [`run_installed_inspect`] inspects the fixed private
/// instance and delegates here; the live proof drives the exact
/// authenticate-resolve-project/trace-render path against the Compose
/// PostgreSQL test kernel with the invoking process's local peer
/// credentials. Public consumers keep [`run_installed_inspect`]; this seam
/// is hidden from the documented API surface.
#[doc(hidden)]
pub async fn run_inspect_with_kernel(
    kernel: PostgresKernel,
    request: InstalledInspectRequest,
    stdout: &mut impl Write,
) -> Result<InstalledInspectOutcome, InstalledInspectError> {
    execute_inspect(kernel, &request, stdout).await
}

async fn execute_inspect(
    kernel: PostgresKernel,
    request: &InstalledInspectRequest,
    stdout: &mut impl Write,
) -> Result<InstalledInspectOutcome, InstalledInspectError> {
    let active = kernel
        .recover()
        .await
        .map_err(|_| runtime_unavailable_error())?;
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(runtime_unavailable_error)?;
    let registry = registered_opaque_codecs(standard).map_err(|_| runtime_unavailable_error())?;

    let uid = nix::unistd::geteuid().as_raw();
    let session = kernel
        .authenticate_local_peer(uid)
        .await
        .map_err(map_kernel_error)?;

    // The invocation resolves to one immutable epoch: the exact override, or
    // the latest epoch captured for the invocation. Both paths apply the
    // authenticated ownership gate before loading or rendering the snapshot.
    // A completed protected invocation normally resolves; a missing epoch
    // fails closed.
    let epoch_id = match request.epoch {
        Some(epoch) => {
            let Some(epoch) = kernel
                .find_inspect_epoch(&session, epoch)
                .await
                .map_err(map_kernel_error)?
            else {
                return Err(missing_epoch_error());
            };
            epoch
        }
        None => {
            let Some(epoch) = kernel
                .find_latest_inspect_epoch(&session, request.invocation)
                .await
                .map_err(map_kernel_error)?
            else {
                return Err(missing_epoch_error());
            };
            epoch
        }
    };
    let Some(snapshot) = kernel
        .load_inspect_snapshot(&session, epoch_id)
        .await
        .map_err(map_kernel_error)?
    else {
        return Err(missing_epoch_error());
    };
    if snapshot.invocation_id() != request.invocation {
        return Err(InstalledInspectError::with_code(
            InstalledInspectErrorKind::Kernel,
            "inspection epoch target does not match requested invocation".to_owned(),
            "inspect.epoch_mismatch",
        ));
    }
    validate_epoch_revisions(
        snapshot.source_revision_id(),
        snapshot.catalogue_revision_id(),
        active.pair(),
    )?;

    // Typed values render as canonical ORV5 hex through the pinned standard
    // registry, mirroring the `orna state` render path.
    let hex = |value: &InvokeValue| -> Result<String, InstalledInspectError> {
        let encoded = encode_constructed_value(&active, &registry, value.value())
            .map_err(map_value_codec_error)?;
        Ok(encode_hex(&encoded))
    };

    let values_granted =
        request.include_values && snapshot.granted().contains(&InspectPrivilege::Values);
    let source_granted =
        request.include_source && snapshot.granted().contains(&InspectPrivilege::Source);
    let security_details_granted = request.include_security
        && snapshot
            .granted()
            .contains(&InspectPrivilege::SecurityDetails);
    let runtime_internals_granted = request.include_runtime
        && snapshot
            .granted()
            .contains(&InspectPrivilege::RuntimeInternals);

    if let Some(projection) = &request.projection {
        let requested = requested_privilege(projection, request);
        match projection {
            InstalledInspectProjection::InvocationNodes => {
                let rows = kernel
                    .inspect_invocation_nodes(&snapshot, requested)
                    .await
                    .map_err(map_kernel_error)?;
                for row in &rows {
                    write_json_line(stdout, &invocation_node_record(row))?;
                }
            }
            InstalledInspectProjection::Calls => {
                let rows = kernel
                    .inspect_calls(&snapshot, requested)
                    .await
                    .map_err(map_kernel_error)?;
                for row in &rows {
                    write_json_line(stdout, &call_record(row, &hex, values_granted)?)?;
                }
            }
            InstalledInspectProjection::Resources => {
                let rows = kernel
                    .inspect_resources(&snapshot, requested)
                    .await
                    .map_err(map_kernel_error)?;
                for row in &rows {
                    write_json_line(stdout, &resource_record(row))?;
                }
            }
            InstalledInspectProjection::StateCells => {
                let rows = kernel
                    .inspect_state_cells(&snapshot, requested)
                    .await
                    .map_err(map_kernel_error)?;
                for row in &rows {
                    write_json_line(stdout, &state_cell_record(row, &hex, values_granted)?)?;
                }
            }
            InstalledInspectProjection::UiNodes => {
                let rows = kernel
                    .inspect_ui_nodes(&snapshot, requested)
                    .await
                    .map_err(map_kernel_error)?;
                for row in &rows {
                    write_json_line(
                        stdout,
                        &ui_node_record(row, source_granted, runtime_internals_granted),
                    )?;
                }
            }
            InstalledInspectProjection::PresentationCandidates => {
                let rows = kernel
                    .inspect_presentation_candidates(&snapshot, requested)
                    .await
                    .map_err(map_kernel_error)?;
                for row in &rows {
                    write_json_line(
                        stdout,
                        &presentation_candidate_record(row, runtime_internals_granted),
                    )?;
                }
            }
            InstalledInspectProjection::RuntimeBindings => {
                let rows = kernel
                    .inspect_runtime_bindings(&snapshot, requested)
                    .await
                    .map_err(map_kernel_error)?;
                for row in &rows {
                    write_json_line(
                        stdout,
                        &runtime_binding_record(row, runtime_internals_granted),
                    )?;
                }
            }
            InstalledInspectProjection::SecurityDecisions => {
                let rows = kernel
                    .inspect_security_decisions(&snapshot, requested)
                    .await
                    .map_err(map_kernel_error)?;
                for row in &rows {
                    write_json_line(
                        stdout,
                        &security_decision_record(row, security_details_granted),
                    )?;
                }
            }
        }
    }

    if request.trace {
        // The installed host has no trusted CLIENT observer invocation.
        // Passing `None` makes the trace provider use the target invocation,
        // which suppresses self-observation rows without accepting caller
        // supplied identity as authority.
        let requested = trace_privilege(request);
        let events = kernel
            .stream_inspect_trace(
                &snapshot,
                requested,
                request.invocation,
                request.after_sequence,
                None,
                false,
            )
            .await
            .map_err(map_kernel_error)?;
        for event in &events {
            write_json_line(stdout, &trace_record(event, &hex, values_granted)?)?;
        }
    }

    // A bare command renders the resolved epoch's summary record.
    if request.projection.is_none() && !request.trace {
        write_json_line(stdout, &summary_record(&snapshot))?;
    }

    Ok(InstalledInspectOutcome::Completed)
}

/// Selects the requested privilege for one projection.
///
/// The `state_cells` kernel projection renders typed values only when the
/// `Values` classifier is requested; the `security_decisions` and
/// `runtime_bindings` projections arm their matching classifiers when the
/// flags request them. Every other projection requests the structural
/// `OwnInvocation` rung.
fn requested_privilege(
    projection: &InstalledInspectProjection,
    request: &InstalledInspectRequest,
) -> InspectPrivilege {
    match projection {
        InstalledInspectProjection::Calls | InstalledInspectProjection::StateCells
            if request.include_values =>
        {
            InspectPrivilege::Values
        }
        InstalledInspectProjection::SecurityDecisions if request.include_security => {
            InspectPrivilege::SecurityDetails
        }
        InstalledInspectProjection::RuntimeBindings if request.include_runtime => {
            InspectPrivilege::RuntimeInternals
        }
        _ => InspectPrivilege::OwnInvocation,
    }
}

/// Selects the requested privilege for the trace stream.
///
/// Trace is structural by default; `--include-values` arms the orthogonal
/// Values classifier and permits decoded ValueBatch payloads.
fn trace_privilege(request: &InstalledInspectRequest) -> InspectPrivilege {
    if request.include_values {
        InspectPrivilege::Values
    } else {
        InspectPrivilege::OwnInvocation
    }
}

/// Renders one resolved epoch as its closed summary record.
///
/// The record carries the epoch identity, the pinned invocation and owner,
/// the root target and outcome, the captured summary, the recording time as
/// milliseconds since the Unix epoch, and the pinned revision pair.
fn summary_record(snapshot: &AuthenticatedInspectSnapshot) -> serde_json::Value {
    summary_record_parts(
        snapshot.id(),
        snapshot.invocation_id(),
        snapshot.owner(),
        snapshot.root_target(),
        snapshot.outcome(),
        snapshot.summary(),
        snapshot.recorded_at(),
        snapshot.source_revision_id(),
        snapshot.catalogue_revision_id(),
    )
}

#[cfg(test)]
fn epoch_summary_record(epoch: &orna_core::inspect::InspectSnapshotEpoch) -> serde_json::Value {
    summary_record_parts(
        epoch.id(),
        epoch.invocation_id(),
        epoch.owner(),
        epoch.root_target(),
        epoch.outcome(),
        epoch.summary(),
        epoch.recorded_at(),
        epoch.source_revision_id(),
        epoch.catalogue_revision_id(),
    )
}

fn summary_record_parts(
    epoch: InspectEpochId,
    invocation: InvocationId,
    owner: PrincipalId,
    root_target: FunctionId,
    outcome: InspectOutcomeKind,
    summary: InspectSnapshotSummary,
    recorded_at: SystemTime,
    source_revision: SourceRevisionId,
    catalogue_revision: CatalogueRevisionId,
) -> serde_json::Value {
    let mut record = serde_json::json!({
        "epoch": epoch.canonical(),
        "invocation": invocation.canonical(),
        "owner": owner.canonical(),
        "root_target": root_target.canonical(),
        "outcome": match outcome {
            InspectOutcomeKind::Allowed => "allowed",
            InspectOutcomeKind::Denied => "denied",
            InspectOutcomeKind::Failed => "failed",
            InspectOutcomeKind::Cancelled => "cancelled",
        },
        "event_count": summary.event_count(),
        "duration_nanoseconds": summary.duration_nanoseconds(),
        "recorded_at": system_time_millis(recorded_at),
        "source_revision": source_revision.canonical(),
        "catalogue_revision": catalogue_revision.canonical(),
    });
    match summary.result() {
        InspectResultSummary::NoValues => {
            record["result"] = serde_json::json!("no_values");
        }
        InspectResultSummary::ValueBatch { value_count } => {
            record["result"] = serde_json::json!("value_batch");
            record["value_count"] = serde_json::json!(value_count);
        }
    }
    record
}

/// Renders one `invocation_nodes` projection row as its closed JSON record.
fn invocation_node_record(row: &InvocationNodeRow) -> serde_json::Value {
    serde_json::json!({
        "projection": "invocation_nodes",
        "invocation": row.id().canonical(),
        "parent": row.parent_id().map(|id| id.canonical()),
        "kind": match row.kind() {
            InspectInvocationNodeKind::Root => "root",
            InspectInvocationNodeKind::Nested => "nested",
        },
        "phase": match row.phase() {
            InspectInvocationPhase::Started => "started",
            InspectInvocationPhase::Executing => "executing",
            InspectInvocationPhase::Completed => "completed",
            InspectInvocationPhase::Failed => "failed",
            InspectInvocationPhase::Cancelled => "cancelled",
        },
        "target": row.target().canonical(),
        "sequence": row.sequence(),
    })
}

/// Renders one `calls` projection row as its closed JSON record.
///
/// The captured batch schema, when one was produced, renders as its
/// canonical ORV5 hex through the supplied encoder.
fn call_record(
    row: &CallRow,
    hex: &dyn Fn(&InvokeValue) -> Result<String, InstalledInspectError>,
    values_granted: bool,
) -> Result<serde_json::Value, InstalledInspectError> {
    let schema_hex = if values_granted {
        match row.schema() {
            Some(schema) => Some(hex(schema)?),
            None => None,
        }
    } else {
        None
    };
    Ok(serde_json::json!({
        "projection": "calls",
        "invocation": row.invocation_id().canonical(),
        "value_count": row.value_count(),
        "duration_nanoseconds": row.duration_nanoseconds(),
        "schema_hex": schema_hex,
    }))
}

/// Renders one `resources` projection row as its closed JSON record.
fn resource_record(row: &ResourceRow) -> serde_json::Value {
    serde_json::json!({
        "projection": "resources",
        "kind": match row.kind() {
            InspectResourceKind::State => "state",
            InspectResourceKind::Catalog => "catalog",
            InspectResourceKind::Standard => "standard",
            InspectResourceKind::Runtime => "runtime",
        },
        "status": match row.status() {
            InspectResourceStatus::Active => "active",
            InspectResourceStatus::Invalidated => "invalidated",
            InspectResourceStatus::Released => "released",
        },
    })
}

/// Renders one `state_cells` projection row as its closed JSON record.
///
/// The typed value renders as its canonical ORV5 hex when the row carries
/// one, and as `null` when the epoch or the privilege gate redacted it.
fn state_cell_record(
    row: &StateCellRow,
    hex: &dyn Fn(&InvokeValue) -> Result<String, InstalledInspectError>,
    values_granted: bool,
) -> Result<serde_json::Value, InstalledInspectError> {
    let value_hex = if values_granted {
        match row.value() {
            Some(value) => Some(hex(value)?),
            None => None,
        }
    } else {
        None
    };
    Ok(serde_json::json!({
        "projection": "state_cells",
        "root_function": row.key().root_function().canonical(),
        "state_profile": row.key().state_profile(),
        "function": row.key().function().canonical(),
        "instance_key": row.key().instance_key(),
        "state_slot": row.key().state_slot().canonical(),
        "revision": row.revision(),
        "value_type": row.value_type().canonical(),
        "value_hex": value_hex,
    }))
}

/// Renders one `ui_nodes` projection row as its closed JSON record.
fn ui_node_record(
    row: &UiNodeRow,
    source_granted: bool,
    runtime_internals_granted: bool,
) -> serde_json::Value {
    serde_json::json!({
        "projection": "ui_nodes",
        "function": row.function().canonical(),
        "call_site": source_granted.then(|| row.call_site()),
        "runtime_contract": runtime_internals_granted.then(|| row.runtime_contract()),
    })
}

/// Renders one `presentation_candidates` projection row as its closed JSON
/// record.
fn presentation_candidate_record(
    row: &PresentationCandidateRow,
    runtime_internals_granted: bool,
) -> serde_json::Value {
    serde_json::json!({
        "projection": "presentation_candidates",
        "presenter": runtime_internals_granted.then(|| row.presenter()),
        "accepted": row.accepted(),
        "reason": runtime_internals_granted.then(|| row.reason()),
        "selected_sink": runtime_internals_granted
            .then(|| row.selected_sink().map(descriptor_label))
            .flatten(),
        "runtime": runtime_internals_granted.then(|| row.runtime()).flatten(),
    })
}

/// Renders one `runtime_bindings` projection row as its closed JSON record.
fn runtime_binding_record(
    row: &RuntimeBindingRow,
    runtime_internals_granted: bool,
) -> serde_json::Value {
    if !runtime_internals_granted {
        return serde_json::json!({
            "projection": "runtime_bindings",
            "runtime_name": row.runtime_name(),
            "version": row.version(),
            "redacted": true,
        });
    }
    serde_json::json!({
        "projection": "runtime_bindings",
        "runtime_name": row.runtime_name(),
        "version": row.version(),
        "consumed_descriptors": row
            .consumed_descriptors()
            .iter()
            .map(descriptor_label)
            .collect::<Vec<_>>(),
        "contracts": row
            .contracts()
            .iter()
            .map(|(name, version, features)| {
                serde_json::json!({ "name": name, "version": version, "features": features })
            })
            .collect::<Vec<_>>(),
        "trusted": row.trusted(),
        "preference_rank": row.preference_rank(),
    })
}

/// Renders one `security_decisions` projection row as its closed JSON
/// record.
fn security_decision_record(
    row: &SecurityDecisionRow,
    security_details_granted: bool,
) -> serde_json::Value {
    let mut record = serde_json::json!({
        "projection": "security_decisions",
        "kind": match row.kind() {
            InspectSecurityDecisionKind::Execute => "execute",
            InspectSecurityDecisionKind::Capability => "capability",
            InspectSecurityDecisionKind::UserState => "user_state",
            InspectSecurityDecisionKind::Inspect => "inspect",
        },
        "outcome": match row.outcome() {
            InspectSecurityDecisionOutcome::Allowed => "allowed",
            InspectSecurityDecisionOutcome::Denied => "denied",
        },
        "principals": security_details_granted.then(|| row
            .principals()
            .iter()
            .map(|id| id.canonical())
            .collect::<Vec<_>>()),
        "target": row.target().map(|id| id.canonical()),
        "denial_reason": security_details_granted.then(|| row.denial_reason()).flatten(),
        "audit_refs": security_details_granted.then(|| row
            .audit_refs()
            .iter()
            .map(|id| id.canonical())
            .collect::<Vec<_>>()),
    });
    if !security_details_granted {
        record["redacted"] = serde_json::json!(true);
    }
    record
}

/// Renders one trace event as its closed JSON record.
///
/// The event kind is derived from the payload; typed values render as
/// canonical ORV5 hex only when the Values classifier was granted, otherwise
/// ValueBatch payloads render their structural count with `redacted: true`.
fn trace_record(
    event: &InspectTraceEvent,
    hex: &dyn Fn(&InvokeValue) -> Result<String, InstalledInspectError>,
    values_granted: bool,
) -> Result<serde_json::Value, InstalledInspectError> {
    let mut payload = serde_json::Map::new();
    match event.payload() {
        InspectTracePayload::Started => {}
        InspectTracePayload::ValueBatch { schema, values } if values_granted => {
            let schema_hex = match schema {
                Some(schema) => Some(hex(schema)?),
                None => None,
            };
            payload.insert("schema_hex".to_owned(), serde_json::json!(schema_hex));
            payload.insert("value_count".to_owned(), serde_json::json!(values.len()));
            let mut values_hex = Vec::with_capacity(values.len());
            for value in values {
                values_hex.push(hex(value)?);
            }
            payload.insert("values_hex".to_owned(), serde_json::json!(values_hex));
        }
        InspectTracePayload::ValueBatch { values, .. } => {
            payload.insert("value_count".to_owned(), serde_json::json!(values.len()));
            payload.insert("redacted".to_owned(), serde_json::json!(true));
        }
        InspectTracePayload::ValueBatchRedacted { value_count } => {
            payload.insert("value_count".to_owned(), serde_json::json!(value_count));
            payload.insert("redacted".to_owned(), serde_json::json!(true));
        }
        InspectTracePayload::Completed {
            duration_nanoseconds,
        } => {
            payload.insert(
                "duration_nanoseconds".to_owned(),
                serde_json::json!(duration_nanoseconds),
            );
        }
        InspectTracePayload::Failed { code } => {
            payload.insert(
                "code".to_owned(),
                serde_json::json!(stable_inspect_error_code(code)),
            );
        }
        InspectTracePayload::Cancelled { reason } => {
            payload.insert(
                "reason".to_owned(),
                serde_json::json!(reason.as_deref().map(stable_inspect_error_code)),
            );
        }
    }
    Ok(serde_json::json!({
        "trace": true,
        "invocation": event.invocation_id().canonical(),
        "sequence": event.sequence(),
        "kind": match event.kind() {
            InspectTraceEventKind::InvocationStarted => "started",
            InspectTraceEventKind::ValueBatch => "value_batch",
            InspectTraceEventKind::InvocationCompleted => "completed",
            InspectTraceEventKind::InvocationFailed => "failed",
            InspectTraceEventKind::InvocationCancelled => "cancelled",
        },
        "recorded_at": system_time_millis(event.recorded_at()),
        "observer_invocation": event.observer_invocation().map(|id| id.canonical()),
        "purpose": event.purpose(),
        "payload": serde_json::Value::Object(payload),
    }))
}

/// Renders one type descriptor as its closed nested label.
fn descriptor_label(descriptor: &TypeDescriptor) -> String {
    match descriptor.kind() {
        TypeDescriptorKind::Named(type_id) | TypeDescriptorKind::Reference(type_id) => {
            type_id.canonical()
        }
        TypeDescriptorKind::List(inner) => format!("list<{}>", descriptor_label(inner)),
        TypeDescriptorKind::Set(inner) => format!("set<{}>", descriptor_label(inner)),
        TypeDescriptorKind::Map { key, value } => {
            format!("map<{},{}>", descriptor_label(key), descriptor_label(value))
        }
        TypeDescriptorKind::Option(inner) => format!("option<{}>", descriptor_label(inner)),
        TypeDescriptorKind::Stream(inner) => format!("stream<{}>", descriptor_label(inner)),
    }
}

/// Renders one recording time as milliseconds since the Unix epoch.
fn system_time_millis(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Writes exactly one JSON record followed by the record newline.
fn write_json_line(
    output: &mut impl Write,
    record: &serde_json::Value,
) -> Result<(), InstalledInspectError> {
    let mut line = serde_json::to_string(record).map_err(|_| rendering_failed_error())?;
    line.push('\n');
    output
        .write_all(line.as_bytes())
        .map_err(presentation_error)
}

/// Renders one canonical bytes payload as lowercase hex.
fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(&mut text, "{byte:02x}").expect("writing to a String cannot fail");
    }
    text
}

const INSPECT_DENIED_CODE: &str = "inspect.denied";
const INSPECT_PROJECTION_FAILED_CODE: &str = "inspect.projection_failed";
const INSPECT_RUNTIME_UNAVAILABLE_CODE: &str = "inspect.runtime_unavailable";
const INSPECT_RECURSION_CODE: &str = "inspect.recursion";

fn runtime_unavailable_error() -> InstalledInspectError {
    InstalledInspectError::with_code(
        InstalledInspectErrorKind::Internal,
        format!("INSPECT runtime unavailable: {INSPECT_RUNTIME_UNAVAILABLE_CODE}"),
        INSPECT_RUNTIME_UNAVAILABLE_CODE,
    )
}

fn denied_error() -> InstalledInspectError {
    InstalledInspectError::with_code(
        InstalledInspectErrorKind::Kernel,
        format!("INSPECT access was denied: {INSPECT_DENIED_CODE}"),
        INSPECT_DENIED_CODE,
    )
}

fn recursion_error() -> InstalledInspectError {
    InstalledInspectError::with_code(
        InstalledInspectErrorKind::Kernel,
        format!("INSPECT recursion was denied: {INSPECT_RECURSION_CODE}"),
        INSPECT_RECURSION_CODE,
    )
}

fn missing_epoch_error() -> InstalledInspectError {
    denied_error()
}

fn validate_epoch_revisions(
    source_revision: SourceRevisionId,
    catalogue_revision: CatalogueRevisionId,
    active_pair: orna_core::revision::RevisionPair,
) -> Result<(), InstalledInspectError> {
    if source_revision != active_pair.source() || catalogue_revision != active_pair.catalogue() {
        return Err(InstalledInspectError::with_code(
            InstalledInspectErrorKind::Kernel,
            "inspection epoch revisions do not match the active revision pair".to_owned(),
            "inspect.epoch_mismatch",
        ));
    }
    Ok(())
}

fn inspect_projection_failed_error() -> InstalledInspectError {
    InstalledInspectError::with_code(
        InstalledInspectErrorKind::Kernel,
        format!("INSPECT projection failed: {INSPECT_PROJECTION_FAILED_CODE}"),
        INSPECT_PROJECTION_FAILED_CODE,
    )
}

fn map_host_error(_error: EmbeddedHostError) -> InstalledInspectError {
    runtime_unavailable_error()
}

fn map_kernel_error(error: PostgresKernelError) -> InstalledInspectError {
    match error {
        PostgresKernelError::Inspect(_) => inspect_projection_failed_error(),
        PostgresKernelError::InspectDenied { reason } => match reason {
            orna_core::security::InspectDenial::MissingEpoch
            | orna_core::security::InspectDenial::MissingPrivilege => denied_error(),
            orna_core::security::InspectDenial::ObserverSuppressed => recursion_error(),
        },
        PostgresKernelError::InspectValueCodec(_) => inspect_projection_failed_error(),
        PostgresKernelError::LocalPeerAuthentication(_) => runtime_unavailable_error(),
        _other => InstalledInspectError::with_code(
            InstalledInspectErrorKind::Internal,
            format!("INSPECT projection failed: {INSPECT_PROJECTION_FAILED_CODE}"),
            INSPECT_PROJECTION_FAILED_CODE,
        ),
    }
}

fn map_value_codec_error(_error: ValueCodecError) -> InstalledInspectError {
    inspect_projection_failed_error()
}

fn rendering_failed_error() -> InstalledInspectError {
    InstalledInspectError::with_code(
        InstalledInspectErrorKind::Rendering,
        format!("INSPECT rendering failed: {INSPECT_PROJECTION_FAILED_CODE}"),
        INSPECT_PROJECTION_FAILED_CODE,
    )
}

fn presentation_error(_error: io::Error) -> InstalledInspectError {
    rendering_failed_error()
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use orna_core::{
        CatalogueRevisionId, FunctionId, InspectEpochId, InvocationId, PrincipalId,
        SourceRevisionId, StateSlotId, TypeId,
        inspect::{
            CallRow, InspectInvocationNodeKind, InspectInvocationPhase, InspectOutcomeKind,
            InspectResourceKind, InspectResourceStatus, InspectResultSummary, InspectSnapshotEpoch,
            InspectSnapshotOptions, InspectSnapshotSummary, InspectTraceEvent, InspectTracePayload,
            InvocationNodeRow, PresentationCandidateRow, ResourceRow, RuntimeBindingRow,
            SecurityDecisionRow, StateCellRow, UiNodeRow,
        },
        invocation::InvokeValue,
        security::LocalPeerAuthenticationError,
        state::UserStateKeyWithoutPrincipal,
        value::RuntimeValue,
    };

    use super::*;

    fn invocation(value: u8) -> InvocationId {
        InvocationId::from_bytes([value; 16])
    }

    fn function(value: u8) -> FunctionId {
        FunctionId::from_bytes([value; 16])
    }

    fn slot(value: u8) -> StateSlotId {
        StateSlotId::from_bytes([value; 16])
    }

    fn value_type(value: u8) -> TypeId {
        TypeId::from_bytes([value; 16])
    }

    fn principal(value: u8) -> PrincipalId {
        PrincipalId::from_bytes([value; 16])
    }

    /// The hex encoder stub: record builders receive a typed-value encoder
    /// and this stub proves they route every value through it.
    fn stub_hex() -> impl Fn(&InvokeValue) -> Result<String, InstalledInspectError> {
        |_value: &InvokeValue| Ok("0a0b".to_owned())
    }

    fn key_without_principal() -> UserStateKeyWithoutPrincipal {
        UserStateKeyWithoutPrincipal::new(
            function(0x10),
            "profile".to_owned(),
            function(0x11),
            "instance".to_owned(),
            slot(0x12),
        )
        .expect("fixture key must validate")
    }

    /// A small checked epoch carrying the root node, one call, and one
    /// state-cell row, built through the core row constructors.
    fn small_epoch() -> InspectSnapshotEpoch {
        let node = InvocationNodeRow::new(
            invocation(0x21),
            None,
            InspectInvocationNodeKind::Root,
            InspectInvocationPhase::Completed,
            function(0x10),
            0,
        )
        .expect("fixture node must validate");
        let call = CallRow::new(invocation(0x21), None, 0, 0).expect("fixture call must validate");
        let cell = StateCellRow::new(
            key_without_principal(),
            value_type(0x41),
            3,
            SystemTime::UNIX_EPOCH,
            Some(
                InvokeValue::new(RuntimeValue::Boolean(true)).expect("fixture value must validate"),
            ),
        );
        InspectSnapshotEpoch::new(
            InspectEpochId::from_bytes([0x01; 16]),
            invocation(0x21),
            SourceRevisionId::from_bytes([0x02; 16]),
            CatalogueRevisionId::from_bytes([0x03; 16]),
            principal(0x04),
            SystemTime::UNIX_EPOCH,
            function(0x10),
            InspectOutcomeKind::Allowed,
            InspectSnapshotSummary::new(
                3,
                InspectResultSummary::ValueBatch { value_count: 1 },
                Some(7),
            )
            .expect("fixture summary must validate"),
            &InspectSnapshotOptions::structural(),
            vec![node],
            vec![call],
            Vec::new(),
            vec![cell],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("fixture epoch must validate")
    }

    /// Every closed projection name parses, and nothing else does.
    #[test]
    fn projection_names_parse_closed() {
        for (name, projection) in [
            (
                "invocation_nodes",
                InstalledInspectProjection::InvocationNodes,
            ),
            ("calls", InstalledInspectProjection::Calls),
            ("resources", InstalledInspectProjection::Resources),
            ("state_cells", InstalledInspectProjection::StateCells),
            ("ui_nodes", InstalledInspectProjection::UiNodes),
            (
                "presentation_candidates",
                InstalledInspectProjection::PresentationCandidates,
            ),
            (
                "runtime_bindings",
                InstalledInspectProjection::RuntimeBindings,
            ),
            (
                "security_decisions",
                InstalledInspectProjection::SecurityDecisions,
            ),
        ] {
            assert_eq!(InstalledInspectProjection::parse(name), Some(projection));
        }
        assert_eq!(InstalledInspectProjection::parse("state_cell"), None);
        assert_eq!(InstalledInspectProjection::parse("State_Cells"), None);
        assert_eq!(InstalledInspectProjection::parse(""), None);
    }

    /// The request constructor retains every parsed field exactly.
    #[test]
    fn request_construction_keeps_every_field() {
        let epoch = InspectEpochId::from_bytes([0x02; 16]);
        let request = InstalledInspectRequest::new(
            invocation(0x01),
            Some(epoch),
            Some(InstalledInspectProjection::StateCells),
            true,
            4,
            true,
            true,
            false,
            true,
        );
        assert_eq!(request.invocation, invocation(0x01));
        assert_eq!(request.epoch, Some(epoch));
        assert_eq!(
            request.projection,
            Some(InstalledInspectProjection::StateCells)
        );
        assert!(request.trace);
        assert_eq!(request.after_sequence, 4);
        assert!(request.include_values);
        assert!(request.include_source);
        assert!(!request.include_security);
        assert!(request.include_runtime);
    }

    #[test]
    fn epoch_revision_pair_mismatch_is_rejected() {
        let source_revision = SourceRevisionId::from_bytes([0x02; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x03; 16]);
        let active_pair =
            orna_core::revision::RevisionPair::new(source_revision, catalogue_revision);

        assert!(validate_epoch_revisions(source_revision, catalogue_revision, active_pair).is_ok());

        for (stale_source, stale_catalogue) in [
            (SourceRevisionId::from_bytes([0x12; 16]), catalogue_revision),
            (source_revision, CatalogueRevisionId::from_bytes([0x13; 16])),
        ] {
            let error = validate_epoch_revisions(stale_source, stale_catalogue, active_pair)
                .expect_err("stale epoch revisions must fail closed");
            assert_eq!(error.kind(), InstalledInspectErrorKind::Kernel);
            assert_eq!(error.code(), Some("inspect.epoch_mismatch"));
            assert_eq!(
                error.message(),
                "inspection epoch revisions do not match the active revision pair"
            );
        }
    }

    /// Classifier flags select requested detail but never manufacture a grant.
    #[test]
    fn classifier_flags_select_requested_detail() {
        let values = InstalledInspectRequest::new(
            invocation(0x01),
            None,
            Some(InstalledInspectProjection::Calls),
            false,
            0,
            true,
            false,
            false,
            false,
        );
        assert_eq!(
            requested_privilege(&InstalledInspectProjection::Calls, &values),
            InspectPrivilege::Values
        );
        assert_eq!(trace_privilege(&values), InspectPrivilege::Values);
    }

    /// The requested privilege arms the matching classifier per projection
    /// and stays structural for every other projection.
    #[test]
    fn requested_privilege_arms_the_matching_classifier() {
        let values = InstalledInspectRequest::new(
            invocation(0x01),
            None,
            None,
            false,
            0,
            true,
            false,
            false,
            false,
        );
        assert_eq!(
            requested_privilege(&InstalledInspectProjection::StateCells, &values),
            InspectPrivilege::Values
        );
        assert_eq!(trace_privilege(&values), InspectPrivilege::Values);

        let security = InstalledInspectRequest::new(
            invocation(0x01),
            None,
            None,
            false,
            0,
            false,
            false,
            true,
            false,
        );
        assert_eq!(
            requested_privilege(&InstalledInspectProjection::SecurityDecisions, &security),
            InspectPrivilege::SecurityDetails
        );

        let runtime = InstalledInspectRequest::new(
            invocation(0x01),
            None,
            None,
            false,
            0,
            false,
            false,
            false,
            true,
        );
        assert_eq!(
            requested_privilege(&InstalledInspectProjection::RuntimeBindings, &runtime),
            InspectPrivilege::RuntimeInternals
        );

        let bare = InstalledInspectRequest::new(
            invocation(0x01),
            None,
            None,
            false,
            0,
            false,
            false,
            false,
            false,
        );
        assert_eq!(trace_privilege(&bare), InspectPrivilege::OwnInvocation);
        for projection in [
            InstalledInspectProjection::InvocationNodes,
            InstalledInspectProjection::Calls,
            InstalledInspectProjection::Resources,
            InstalledInspectProjection::StateCells,
            InstalledInspectProjection::UiNodes,
            InstalledInspectProjection::PresentationCandidates,
            InstalledInspectProjection::RuntimeBindings,
            InstalledInspectProjection::SecurityDecisions,
        ] {
            assert_eq!(
                requested_privilege(&projection, &bare),
                InspectPrivilege::OwnInvocation,
                "{projection:?}"
            );
        }
    }

    /// The resolved epoch renders one closed summary record with the exact
    /// captured summary and pinned revision pair.
    #[test]
    fn epoch_summary_renders_closed_records() {
        let epoch = small_epoch();
        assert_eq!(
            epoch_summary_record(&epoch),
            serde_json::json!({
                "epoch": InspectEpochId::from_bytes([0x01; 16]).canonical(),
                "invocation": invocation(0x21).canonical(),
                "owner": principal(0x04).canonical(),
                "root_target": function(0x10).canonical(),
                "outcome": "allowed",
                "event_count": 3,
                "result": "value_batch",
                "value_count": 1,
                "duration_nanoseconds": 7,
                "recorded_at": 0,
                "source_revision": SourceRevisionId::from_bytes([0x02; 16]).canonical(),
                "catalogue_revision": CatalogueRevisionId::from_bytes([0x03; 16]).canonical(),
            })
        );
    }

    /// The epoch constructor redacts state-cell values for a structural-only
    /// capture, so the record renders `value_hex: null` unless a value was
    /// retained and encoded.
    #[test]
    fn state_cells_render_redacted_and_encoded_values() {
        let redacted = StateCellRow::new(
            key_without_principal(),
            value_type(0x41),
            3,
            SystemTime::UNIX_EPOCH,
            None,
        );
        assert_eq!(
            state_cell_record(&redacted, &stub_hex(), false).expect("record must render"),
            serde_json::json!({
                "projection": "state_cells",
                "root_function": function(0x10).canonical(),
                "state_profile": "profile",
                "function": function(0x11).canonical(),
                "instance_key": "instance",
                "state_slot": slot(0x12).canonical(),
                "revision": 3,
                "value_type": value_type(0x41).canonical(),
                "value_hex": null,
            })
        );

        let retained = StateCellRow::new(
            key_without_principal(),
            value_type(0x41),
            3,
            SystemTime::UNIX_EPOCH,
            Some(
                InvokeValue::new(RuntimeValue::Boolean(true)).expect("fixture value must validate"),
            ),
        );
        assert_eq!(
            state_cell_record(&retained, &stub_hex(), true).expect("record must render"),
            serde_json::json!({
                "projection": "state_cells",
                "root_function": function(0x10).canonical(),
                "state_profile": "profile",
                "function": function(0x11).canonical(),
                "instance_key": "instance",
                "state_slot": slot(0x12).canonical(),
                "revision": 3,
                "value_type": value_type(0x41).canonical(),
                "value_hex": "0a0b",
            })
        );
    }

    /// Every projection row renders one closed JSON record with its exact
    /// facts, routing typed values through the supplied encoder.
    #[test]
    fn projection_rows_render_closed_records() {
        let node = InvocationNodeRow::new(
            invocation(0x21),
            Some(invocation(0x22)),
            InspectInvocationNodeKind::Nested,
            InspectInvocationPhase::Failed,
            function(0x13),
            4,
        )
        .expect("fixture node must validate");
        assert_eq!(
            invocation_node_record(&node),
            serde_json::json!({
                "projection": "invocation_nodes",
                "invocation": invocation(0x21).canonical(),
                "parent": invocation(0x22).canonical(),
                "kind": "nested",
                "phase": "failed",
                "target": function(0x13).canonical(),
                "sequence": 4,
            })
        );

        let call = CallRow::new(
            invocation(0x21),
            Some(InvokeValue::new(RuntimeValue::Boolean(true)).expect("fixture value")),
            1,
            9,
        )
        .expect("fixture call must validate");
        assert_eq!(
            call_record(&call, &stub_hex(), true).expect("record must render"),
            serde_json::json!({
                "projection": "calls",
                "invocation": invocation(0x21).canonical(),
                "value_count": 1,
                "duration_nanoseconds": 9,
                "schema_hex": "0a0b",
            })
        );

        let resource = ResourceRow::new(InspectResourceKind::State, InspectResourceStatus::Active);
        assert_eq!(
            resource_record(&resource),
            serde_json::json!({
                "projection": "resources",
                "kind": "state",
                "status": "active",
            })
        );

        let ui_node = UiNodeRow::new(function(0x13), "call-site".to_owned(), "tty@1".to_owned())
            .expect("fixture UI node must validate");
        assert_eq!(
            ui_node_record(&ui_node, true, true),
            serde_json::json!({
                "projection": "ui_nodes",
                "function": function(0x13).canonical(),
                "call_site": "call-site",
                "runtime_contract": "tty@1",
            })
        );

        let candidate = PresentationCandidateRow::new(
            "terminal-table".to_owned(),
            true,
            "accepted by output resolution".to_owned(),
            None,
            Some("tty".to_owned()),
        )
        .expect("fixture candidate must validate");
        assert_eq!(
            presentation_candidate_record(&candidate, true),
            serde_json::json!({
                "projection": "presentation_candidates",
                "presenter": "terminal-table",
                "accepted": true,
                "reason": "accepted by output resolution",
                "selected_sink": null,
                "runtime": "tty",
            })
        );

        let binding = RuntimeBindingRow::new(
            "tty".to_owned(),
            "1".to_owned(),
            vec![TypeDescriptor::named(value_type(0x41))],
            vec![("tty".to_owned(), "1".to_owned(), vec!["ansi".to_owned()])],
            true,
            0,
        )
        .expect("fixture binding must validate");
        assert_eq!(
            runtime_binding_record(&binding, true),
            serde_json::json!({
                "projection": "runtime_bindings",
                "runtime_name": "tty",
                "version": "1",
                "consumed_descriptors": [value_type(0x41).canonical()],
                "contracts": [{ "name": "tty", "version": "1", "features": ["ansi"] }],
                "trusted": true,
                "preference_rank": 0,
            })
        );

        let decision = SecurityDecisionRow::new(
            InspectSecurityDecisionKind::Execute,
            InspectSecurityDecisionOutcome::Allowed,
            vec![principal(0x04), principal(0x05)],
            Some(function(0x10)),
            None,
            vec![orna_core::SecurityAuditEventId::from_bytes([0x06; 16])],
        )
        .expect("fixture decision must validate");
        assert_eq!(
            security_decision_record(&decision, true),
            serde_json::json!({
                "projection": "security_decisions",
                "kind": "execute",
                "outcome": "allowed",
                "principals": [principal(0x04).canonical(), principal(0x05).canonical()],
                "target": function(0x10).canonical(),
                "denial_reason": null,
                "audit_refs": [orna_core::SecurityAuditEventId::from_bytes([0x06; 16]).canonical()],
            })
        );
    }

    /// Installed JSON records retain structural facts while hiding classified
    /// fields when the corresponding durable classifier is absent.
    #[test]
    fn classified_fields_redact_without_durable_grants() {
        let call = CallRow::new(
            invocation(0x21),
            Some(InvokeValue::new(RuntimeValue::Boolean(true)).expect("fixture value")),
            1,
            9,
        )
        .expect("fixture call must validate");
        let call_record = call_record(&call, &stub_hex(), false).expect("call record");
        assert_eq!(call_record["invocation"], invocation(0x21).canonical());
        assert!(call_record["schema_hex"].is_null());

        let ui_node = UiNodeRow::new(function(0x13), "source".to_owned(), "runtime".to_owned())
            .expect("fixture UI node must validate");
        let ui_record = ui_node_record(&ui_node, false, false);
        assert_eq!(ui_record["function"], function(0x13).canonical());
        assert!(ui_record["call_site"].is_null());
        assert!(ui_record["runtime_contract"].is_null());

        let candidate = PresentationCandidateRow::new(
            "presenter".to_owned(),
            true,
            "reason".to_owned(),
            Some(TypeDescriptor::named(value_type(0x41))),
            Some("runtime".to_owned()),
        )
        .expect("fixture candidate must validate");
        let candidate_record = presentation_candidate_record(&candidate, false);
        assert_eq!(candidate_record["accepted"], true);
        assert!(candidate_record["presenter"].is_null());
        assert!(candidate_record["reason"].is_null());
        assert!(candidate_record["selected_sink"].is_null());
        assert!(candidate_record["runtime"].is_null());

        let binding = RuntimeBindingRow::new(
            "runtime".to_owned(),
            "1".to_owned(),
            Vec::new(),
            Vec::new(),
            true,
            0,
        )
        .expect("fixture binding must validate");
        let binding_record = runtime_binding_record(&binding, false);
        assert_eq!(binding_record["projection"], "runtime_bindings");
        assert_eq!(binding_record["runtime_name"], "runtime");
        assert_eq!(binding_record["version"], "1");
        assert_eq!(binding_record["redacted"], true);

        let decision = SecurityDecisionRow::new(
            InspectSecurityDecisionKind::Execute,
            InspectSecurityDecisionOutcome::Allowed,
            vec![principal(0x04)],
            Some(function(0x10)),
            None,
            vec![orna_core::SecurityAuditEventId::from_bytes([0x06; 16])],
        )
        .expect("fixture decision must validate");
        let decision_record = security_decision_record(&decision, false);
        assert_eq!(decision_record["kind"], "execute");
        assert_eq!(decision_record["target"], function(0x10).canonical());
        assert!(decision_record["principals"].is_null());
        assert!(decision_record["denial_reason"].is_null());
        assert!(decision_record["audit_refs"].is_null());
        assert_eq!(decision_record["redacted"], true);

        let event = InspectTraceEvent::new(
            invocation(0x21),
            1,
            InspectTracePayload::ValueBatch {
                schema: None,
                values: vec![InvokeValue::new(RuntimeValue::Integer(7)).expect("fixture value")],
            },
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect("fixture event must validate");
        let trace = trace_record(&event, &stub_hex(), false).expect("trace record");
        assert_eq!(trace["payload"]["value_count"], 1);
        assert_eq!(trace["payload"]["redacted"], true);
        assert!(trace["payload"].get("values_hex").is_none());
    }

    /// Every trace payload renders its closed record with the derived kind
    /// and typed values as hex.
    #[test]
    fn trace_events_render_closed_records() {
        let started = InspectTraceEvent::new(
            invocation(0x21),
            0,
            InspectTracePayload::Started,
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect("fixture event must validate");
        assert_eq!(
            trace_record(&started, &stub_hex(), false).expect("record must render"),
            serde_json::json!({
                "trace": true,
                "invocation": invocation(0x21).canonical(),
                "sequence": 0,
                "kind": "started",
                "recorded_at": 0,
                "observer_invocation": null,
                "purpose": null,
                "payload": {},
            })
        );

        let batch = InspectTraceEvent::new(
            invocation(0x21),
            1,
            InspectTracePayload::ValueBatch {
                schema: None,
                values: vec![
                    InvokeValue::new(RuntimeValue::Integer(7))
                        .expect("fixture value must validate"),
                ],
            },
            SystemTime::UNIX_EPOCH,
            Some(invocation(0x31)),
            Some("inspect".to_owned()),
        )
        .expect("fixture event must validate");
        assert_eq!(
            trace_record(&batch, &stub_hex(), true).expect("record must render"),
            serde_json::json!({
                "trace": true,
                "invocation": invocation(0x21).canonical(),
                "sequence": 1,
                "kind": "value_batch",
                "recorded_at": 0,
                "observer_invocation": invocation(0x31).canonical(),
                "purpose": "inspect",
                "payload": {
                    "schema_hex": null,
                    "value_count": 1,
                    "values_hex": ["0a0b"],
                },
            })
        );

        let redacted = InspectTraceEvent::new(
            invocation(0x21),
            1,
            InspectTracePayload::ValueBatchRedacted { value_count: 1 },
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect("redacted fixture event must validate");
        assert_eq!(
            trace_record(&redacted, &stub_hex(), false).expect("record must render"),
            serde_json::json!({
                "trace": true,
                "invocation": invocation(0x21).canonical(),
                "sequence": 1,
                "kind": "value_batch",
                "recorded_at": 0,
                "observer_invocation": null,
                "purpose": null,
                "payload": {
                    "value_count": 1,
                    "redacted": true,
                },
            })
        );

        let completed = InspectTraceEvent::new(
            invocation(0x21),
            2,
            InspectTracePayload::Completed {
                duration_nanoseconds: 12,
            },
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect("fixture event must validate");
        let record = trace_record(&completed, &stub_hex(), false).expect("record must render");
        assert_eq!(record["kind"], "completed");
        assert_eq!(record["payload"]["duration_nanoseconds"], 12);

        let failed = InspectTraceEvent::new(
            invocation(0x21),
            2,
            InspectTracePayload::Failed {
                code: "internal".to_owned(),
            },
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect("fixture event must validate");
        let record = trace_record(&failed, &stub_hex(), false).expect("record must render");
        assert_eq!(record["kind"], "failed");
        assert_eq!(record["payload"]["code"], "inspect.projection_failed");

        let cancelled = InspectTraceEvent::new(
            invocation(0x21),
            2,
            InspectTracePayload::Cancelled { reason: None },
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect("fixture event must validate");
        let record = trace_record(&cancelled, &stub_hex(), false).expect("record must render");
        assert_eq!(record["kind"], "cancelled");
        assert_eq!(record["payload"]["reason"], serde_json::Value::Null);

        let cancelled_with_detail = InspectTraceEvent::new(
            invocation(0x21),
            3,
            InspectTracePayload::Cancelled {
                reason: Some("provider secret".to_owned()),
            },
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect("cancelled detail fixture must validate");
        let record =
            trace_record(&cancelled_with_detail, &stub_hex(), false).expect("record must render");
        assert_eq!(record["kind"], "cancelled");
        assert_eq!(record["payload"]["reason"], "inspect.projection_failed");
    }

    /// Type descriptors render stable closed nested labels.
    #[test]
    fn descriptor_labels_render_closed() {
        assert_eq!(
            descriptor_label(&TypeDescriptor::named(value_type(0x41))),
            value_type(0x41).canonical()
        );
        let named = TypeDescriptor::named(value_type(0x41));
        let list = TypeDescriptor::list(named).expect("fixture list must validate");
        assert_eq!(
            descriptor_label(&list),
            format!("list<{}>", value_type(0x41).canonical())
        );
        let map = TypeDescriptor::map(
            TypeDescriptor::named(value_type(0x41)),
            TypeDescriptor::named(value_type(0x41)),
        )
        .expect("fixture map must validate");
        assert_eq!(
            descriptor_label(&map),
            format!(
                "map<{},{}>",
                value_type(0x41).canonical(),
                value_type(0x41).canonical()
            )
        );
    }

    /// Kernel inspect failures retain the closed kernel class and stable public
    /// code; every other kernel failure is internal.
    #[test]
    fn kernel_errors_classify_inspect_outcomes() {
        let model = orna_core::inspect::InspectError::EmptyEpoch {
            id: InspectEpochId::from_bytes([0xab; 16]),
        };
        let mapped = map_kernel_error(PostgresKernelError::Inspect(model));
        assert_eq!(mapped.kind(), InstalledInspectErrorKind::Kernel);
        assert_eq!(mapped.code(), Some("inspect.projection_failed"));
        assert_eq!(
            mapped.message(),
            "INSPECT projection failed: inspect.projection_failed"
        );

        let denied = map_kernel_error(PostgresKernelError::InspectDenied {
            reason: orna_core::security::InspectDenial::MissingPrivilege,
        });
        assert_eq!(denied.kind(), InstalledInspectErrorKind::Kernel);
        assert_eq!(denied.code(), Some("inspect.denied"));
        assert_eq!(
            denied.message(),
            "INSPECT access was denied: inspect.denied"
        );

        let missing_epoch = map_kernel_error(PostgresKernelError::InspectDenied {
            reason: orna_core::security::InspectDenial::MissingEpoch,
        });
        assert_eq!(missing_epoch.kind(), InstalledInspectErrorKind::Kernel);
        assert_eq!(missing_epoch.code(), Some("inspect.denied"));
        assert_eq!(
            missing_epoch.message(),
            "INSPECT access was denied: inspect.denied"
        );

        let recursion = map_kernel_error(PostgresKernelError::InspectDenied {
            reason: orna_core::security::InspectDenial::ObserverSuppressed,
        });
        assert_eq!(recursion.kind(), InstalledInspectErrorKind::Kernel);
        assert_eq!(recursion.code(), Some("inspect.recursion"));
        assert_eq!(
            recursion.message(),
            "INSPECT recursion was denied: inspect.recursion"
        );

        let codec = map_kernel_error(PostgresKernelError::InspectValueCodec(
            orna_protocol::ValueCodecError::UnsupportedValue,
        ));
        assert_eq!(codec.kind(), InstalledInspectErrorKind::Kernel);
        assert_eq!(codec.code(), Some("inspect.projection_failed"));
        assert_eq!(
            codec.message(),
            "INSPECT projection failed: inspect.projection_failed"
        );

        let value_codec = map_value_codec_error(orna_protocol::ValueCodecError::UnsupportedValue);
        assert_eq!(value_codec.kind(), InstalledInspectErrorKind::Kernel);
        assert_eq!(value_codec.code(), Some("inspect.projection_failed"));
        assert_eq!(
            value_codec.message(),
            "INSPECT projection failed: inspect.projection_failed"
        );

        let authentication = map_kernel_error(PostgresKernelError::LocalPeerAuthentication(
            LocalPeerAuthenticationError::UnknownUid,
        ));
        assert_eq!(authentication.kind(), InstalledInspectErrorKind::Internal);
        assert_eq!(authentication.code(), Some("inspect.runtime_unavailable"));
        assert_eq!(
            authentication.message(),
            "INSPECT runtime unavailable: inspect.runtime_unavailable"
        );

        let other = map_kernel_error(PostgresKernelError::RawCallTargetUnavailable {
            function: function(0x10),
            rule: "closed raw-call rule",
        });
        assert_eq!(other.kind(), InstalledInspectErrorKind::Internal);
        assert_eq!(other.code(), Some("inspect.projection_failed"));
        assert_eq!(
            other.message(),
            "INSPECT projection failed: inspect.projection_failed"
        );
    }

    #[test]
    fn host_errors_use_a_stable_runtime_code() {
        let host = map_host_error(EmbeddedHostError::InvalidPackageState);
        assert_eq!(host.kind(), InstalledInspectErrorKind::Internal);
        assert_eq!(host.code(), Some("inspect.runtime_unavailable"));
        assert_eq!(
            host.message(),
            "INSPECT runtime unavailable: inspect.runtime_unavailable"
        );
    }

    /// Missing epochs use the stable denial surface to avoid disclosing
    /// whether another principal has a matching invocation.
    #[test]
    fn missing_epoch_error_is_denied() {
        let error = missing_epoch_error();
        assert_eq!(error.kind(), InstalledInspectErrorKind::Kernel);
        assert_eq!(error.code(), Some("inspect.denied"));
        assert_eq!(error.message(), "INSPECT access was denied: inspect.denied");
    }

    #[test]
    fn rendering_errors_use_a_stable_code() {
        let error = presentation_error(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "secret rendering detail",
        ));
        assert_eq!(error.kind(), InstalledInspectErrorKind::Rendering);
        assert_eq!(error.code(), Some("inspect.projection_failed"));
        assert_eq!(
            error.message(),
            "INSPECT rendering failed: inspect.projection_failed"
        );
    }

    /// The closed failure display names the installed command.
    #[test]
    fn error_display_is_stable() {
        let error = InstalledInspectError::new(
            InstalledInspectErrorKind::Kernel,
            "no inspect epoch".to_owned(),
        );
        assert_eq!(error.to_string(), "orna inspect: no inspect epoch");
    }

    /// Canonical payloads render as stable lowercase hex.
    #[test]
    fn hex_encoding_is_lowercase_and_stable() {
        assert_eq!(encode_hex(&[]), "");
        assert_eq!(encode_hex(&[0x00, 0x0a, 0x0f, 0x10, 0xff]), "000a0f10ff");
    }
}
