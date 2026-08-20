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
//! - every `ValueBatch` value goes to stdout: a `std.terminal.Document`
//!   renders as the document text and a `std.io.ByteStream` as the raw
//!   stream bytes through `orna-runtime-tty` (ADR 0057 step 9); every other
//!   value keeps its canonical ORV5 typed encoding, one value per record,
//!   without progress or warning interleave;
//! - `InvocationFailed` events print one redacted failure line to stderr and exit 1.
//!
//! `--explain` renders the resolution and sealed request facts and exits
//! success without dispatching, authorising, or auditing.

use std::{
    error::Error,
    fmt,
    io::{self, IsTerminal, Write},
    os::unix::net::UnixStream as StandardUnixStream,
    thread,
    time::Duration,
};

use orna_client::{ClientResourceCompletion, ClientResourceExecutor, ClientResourceRequest};
use orna_core::{
    FunctionRevisionId, TypeId,
    catalogue::{FunctionDefinition, FunctionReturn, QualifiedSemanticName, ValueTypeKind},
    invocation::InvocationCarrierConstructionError,
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationEventBody, InvocationOutputRequirement, InvocationOutputTypeSelector,
        InvocationRuntimeContract, InvocationRuntimeOffer, InvocationSinkOffer,
        InvocationStreamingRequirement, InvocationTarget, InvocationTracePolicy, InvokeRequest,
        InvokeRequestInput,
    },
    invocation_binding::{CliArgumentInput, bind_cli_arguments},
    revision::{ActiveDatabaseRevision, VerifiedStandardLibrarySnapshot},
    security::AuthenticatedSession,
    types::{ResolvedType, StandardScalar, TypeDescriptor},
    value::RuntimeValue,
};
use orna_postgres::{
    PostgresKernel, PostgresKernelError, SealedInvocationResult,
};
use orna_protocol::{
    CallFailure, MAX_FRAME_PAYLOAD_LENGTH, MAX_RESOURCE_WINDOW, ResourceArgument,
    ResourceCancel, ResourceCancellationCode, ResourceClientFrame,
    ResourceKind as ProtocolResourceKind, ResourceProtocolConnection, ResourceRequest,
    ResourceServerFrame, ResourceWindowUpdate, decode_resource_server_frame,
    encode_constructed_value, encode_invoke_request, encode_resource_client_frame,
};
use orna_standard::{
    STD_IO_BYTE_STREAM_TYPE_ID, STD_TERMINAL_DOCUMENT_TYPE_ID, registered_opaque_codecs,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender};
use tokio::sync::mpsc::error::TryRecvError;

use crate::{
    EmbeddedHostError, LocalRawSocketResources, inspect_ready_embedded_host, serve_local_raw_stream,
};

const INSTALLED_RESOURCE_SOCKET: &str = "/run/orna/default/orna.sock";
const CONSTRUCTED_CLIENT_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00";
const CONSTRUCTED_SERVER_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00";
const RESOURCE_MARKER: &[u8; 15] = b"ORNA-RESOURCE/1";
const RESOURCE_HEADER_LENGTH: usize = 21;
const RESOURCE_FRAME_TIMEOUT: Duration = Duration::from_secs(30);

/// The stable CLIENT failure code for a denied SERVER resource request.
const SERVER_RESOURCE_DENIED_CODE: &str = "server.resource.execute-denied";
/// The stable CLIENT failure code for an unavailable SERVER resource target.
const SERVER_RESOURCE_UNAVAILABLE_CODE: &str = "server.resource.target-unavailable";
/// The stable CLIENT failure code for an internal SERVER resource failure.
const SERVER_RESOURCE_INTERNAL_CODE: &str = "server.resource.internal-failure";
/// The stable CLIENT failure code for a result shape that the scalar evaluator
/// cannot publish.
const SERVER_RESOURCE_SHAPE_CODE: &str = "server.resource.invalid-result-shape";

/// The sealed connection protocol major offered by every host run.
const CONNECTION_PROTOCOL_MAJOR: u16 = 5;
/// The minimum accepted client frame size (protocol version 1 offer limit).
const MAXIMUM_FRAME_SIZE: u32 = 1_024;
/// The first client run offers no artifact budget.
const MAXIMUM_ARTIFACT_SIZE: u64 = 0;

/// The media type of the `std.terminal.Document` sink: the ADR 0057 document
/// layout is plain text, so the client sink consumes `text/plain`.
const DOCUMENT_SINK_MEDIA_TYPE: &str = "text/plain";
/// The media type of the `std.io.ByteStream` sink: the sink consumes the raw
/// bytes of any byte stream, so the client offers the generic binary type.
const BYTE_STREAM_SINK_MEDIA_TYPE: &str = "application/octet-stream";

/// The family name of the installed tty runtime (ADR 0063), taken from the
/// runtime crate so family identity is not duplicated here.
const TTY_RUNTIME_NAME: &str = orna_runtime_tty::RUNTIME_NAME;
/// The installed tty runtime version (ADR 0063), taken from the runtime
/// crate so the offer names the exact linked binary.
const TTY_RUNTIME_VERSION: &str = orna_runtime_tty::RUNTIME_VERSION;

/// The installed runtime family of one `orna invoke` run (ADR 0063).
///
/// Today the only installed family is [`RuntimeFamily::Tty`]; the spec's
/// other desktop families (`qt`, `gtk`, `imgui`, `swiftui`, `web`) parse to
/// `None` so an override to one fails closed at the CLI as a usage error.
///
/// `#[allow(clippy::manual_non_exhaustive)]`: the hidden variant below is
/// deliberately not the `#[non_exhaustive]` marker. A marker would make the
/// selection policy's fail-closed arm unreachable inside this crate (the
/// compiler sees a one-variant enum), and the arm must stay live: it is
/// what rejects a recognised-but-not-installed family once a second variant
/// exists, and the unit tests exercise it.
#[allow(clippy::manual_non_exhaustive)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeFamily {
    /// The terminal runtime (`orna-runtime-tty`).
    Tty,
    /// A recognised-but-not-installed family (hidden).
    ///
    /// `--runtime` parsing never produces this variant — only `tty` parses
    /// today — so it cannot reach the selection policy through the CLI. It
    /// exists so the policy's fail-closed arm is expressible and testable:
    /// when a second family lands as an installed variant, the arm keeps
    /// rejecting families with no installed runtime.
    #[doc(hidden)]
    NotInstalled,
}

impl RuntimeFamily {
    /// Parses one `--runtime <family>` override value.
    ///
    /// Only installed families parse; an unknown or not-installed family is
    /// `None`, which the command parser reports as a usage error.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            TTY_RUNTIME_NAME => Some(RuntimeFamily::Tty),
            _ => None,
        }
    }

    /// Returns the family name.
    pub fn name(self) -> &'static str {
        match self {
            RuntimeFamily::Tty => TTY_RUNTIME_NAME,
            RuntimeFamily::NotInstalled => "not-installed",
        }
    }
}

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
    /// The `--runtime <family>` override, when present; absent selects the
    /// deterministic default runtime (ADR 0063).
    pub runtime: Option<RuntimeFamily>,
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
        runtime: Option<RuntimeFamily>,
    ) -> Self {
        Self {
            target,
            arguments,
            output,
            trace,
            no_progress,
            explain,
            runtime,
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

enum ResourceTransportSource {
    Installed,
    Injected(InjectedResourceTransport),
}

enum InjectedResourceTransport {
    /// A compatibility one-shot stream used by focused transport tests.
    Stream(Option<StandardUnixStream>),
    /// A host-owned source that creates one local connection per resource
    /// request. Keeping the kernel and admission resources here avoids
    /// consuming the source after the first request.
    Factory {
        kernel: PostgresKernel,
        resources: LocalRawSocketResources,
    },
}

struct PendingResourceTransport {
    request: ClientResourceRequest,
    receiver: Receiver<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
    control: UnboundedSender<ResourceTransportControl>,
    worker: thread::JoinHandle<()>,
    cancel_requested: bool,
}

struct DetachedResourceTransport {
    control: UnboundedSender<ResourceTransportControl>,
    worker: thread::JoinHandle<()>,
}

enum ResourceTransportControl {
    Cancel(ResourceCancellationCode),
    Shutdown,
}

impl ResourceTransportSource {
    fn take_stream(
        &mut self,
    ) -> Result<(StandardUnixStream, Option<thread::JoinHandle<()>>), ()> {
        match self {
            Self::Installed => StandardUnixStream::connect(INSTALLED_RESOURCE_SOCKET)
                .map(|stream| (stream, None))
                .map_err(|_| ()),
            Self::Injected(InjectedResourceTransport::Stream(stream)) => {
                stream.take().map(|stream| (stream, None)).ok_or(())
            }
            Self::Injected(InjectedResourceTransport::Factory { kernel, resources }) => {
                let (server, client) = StandardUnixStream::pair().map_err(|_| ())?;
                let server_kernel = kernel.clone();
                let server_resources = resources.clone();
                let server_task = thread::Builder::new()
                    .name("orna-resource-server".to_owned())
                    .spawn(move || {
                        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        else {
                            return;
                        };
                        let _ = runtime.block_on(serve_local_raw_stream(
                            server_kernel,
                            server,
                            server_resources,
                        ));
                    })
                    .map_err(|_| ())?;
                Ok((client, Some(server_task)))
            }
        }
    }
}

/// Executes one CLIENT resource or SERVER action request through the
/// authenticated installed-server boundary.
///
/// The CLIENT evaluator is synchronous, while the authenticated PostgreSQL
/// resource boundary is asynchronous. The adapter therefore runs the resource
/// operation on a separate current-thread runtime and connection. This keeps
/// the resource transaction outside the sealed invocation transaction and
/// avoids re-entering the installed host runtime.
///
/// The adapter owns the transport stream allocation. It does not accept
/// caller-supplied authority; the authenticated session remains the source of
/// the server decision.
pub struct InstalledClientResourceExecutor {
    active: ActiveDatabaseRevision,
    next_stream_id: u64,
    transport: ResourceTransportSource,
    pending: Option<PendingResourceTransport>,
    detached: Vec<DetachedResourceTransport>,
}

impl InstalledClientResourceExecutor {
    /// Creates an executor for one authenticated installed database revision.
    pub fn new(
        _kernel: PostgresKernel,
        _session: AuthenticatedSession,
        active: ActiveDatabaseRevision,
    ) -> Self {
        Self {
            active,
            next_stream_id: 1,
            transport: ResourceTransportSource::Installed,
            pending: None,
            detached: Vec::new(),
        }
    }

    #[doc(hidden)]
    pub fn new_with_stream(
        _kernel: PostgresKernel,
        _session: AuthenticatedSession,
        active: ActiveDatabaseRevision,
        stream: StandardUnixStream,
    ) -> Self {
        Self {
            active,
            next_stream_id: 1,
            transport: ResourceTransportSource::Injected(InjectedResourceTransport::Stream(
                Some(stream),
            )),
            pending: None,
            detached: Vec::new(),
        }
    }

    fn new_with_factory(
        active: ActiveDatabaseRevision,
        kernel: PostgresKernel,
        resources: LocalRawSocketResources,
    ) -> Self {
        Self {
            active,
            next_stream_id: 1,
            transport: ResourceTransportSource::Injected(InjectedResourceTransport::Factory {
                kernel,
                resources,
            }),
            pending: None,
            detached: Vec::new(),
        }
    }
}

impl ClientResourceExecutor for InstalledClientResourceExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        if self.pending.is_some() {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        }
        let Some(next_stream_id) = self.next_stream_id.checked_add(1) else {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        };
        let stream_id = self.next_stream_id;
        self.next_stream_id = next_stream_id;
        let target = request.target();
        let target_definition = self
            .active
            .catalogue()
            .function_by_id(target.function())
            .or_else(|| {
                self.active
                    .catalogue_hash_context()
                    .standard()
                    .and_then(|standard| standard.catalogue().function_by_id(target.function()))
            });
        let (resource_kind, return_type) = match target_definition.map(FunctionDefinition::return_type) {
            Some(FunctionReturn::Single(return_type)) =>
                (ProtocolResourceKind::Single, *return_type),
            Some(FunctionReturn::Stream(return_type)) =>
                (ProtocolResourceKind::Stream, *return_type),
            Some(FunctionReturn::Rows(_)) | None => {
                return request.failed(SERVER_RESOURCE_SHAPE_CODE.to_owned());
            }
        };
        if return_type != request.expected_type() {
            return request.failed(SERVER_RESOURCE_SHAPE_CODE.to_owned());
        }
        let Some(invocation_context) = request.invocation_context() else {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        };
        let protocol_request = ResourceRequest {
            stream_id,
            request_id: request.request_id(),
            parent_invocation_id: invocation_context.parent_invocation_id(),
            call_site_id: invocation_context.call_site_id(),
            target_function_id: target.function(),
            target_revision: target.revision(),
            generation: request.generation().value(),
            resource_kind,
            arguments: request
                .arguments()
                .iter()
                .map(|argument| ResourceArgument {
                    parameter: argument.parameter(),
                    value: argument.value().clone(),
                })
                .collect(),
            item_window: 1,
            byte_window: MAX_RESOURCE_WINDOW,
        };
        let mut worker_transport = match &mut self.transport {
            ResourceTransportSource::Installed => ResourceTransportSource::Installed,
            ResourceTransportSource::Injected(InjectedResourceTransport::Stream(stream)) => {
                ResourceTransportSource::Injected(InjectedResourceTransport::Stream(stream.take()))
            }
            ResourceTransportSource::Injected(InjectedResourceTransport::Factory {
                kernel,
                resources,
            }) => ResourceTransportSource::Injected(InjectedResourceTransport::Factory {
                kernel: kernel.clone(),
                resources: resources.clone(),
            }),
        };
        if matches!(
            &worker_transport,
            ResourceTransportSource::Injected(InjectedResourceTransport::Stream(None))
        ) {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        }
        let (sender, receiver) = mpsc::channel(1);
        let (control, control_receiver) = mpsc::unbounded_channel();
        let active = self.active.clone();
        let expected_type = request.expected_type();
        let worker = thread::Builder::new()
            .name("orna-resource-transport".to_owned())
            .spawn(move || {
                let stream = worker_transport.take_stream();
                let (stream, server_task) = match stream {
                    Ok(stream) => stream,
                    Err(()) => {
                        let _ = sender.blocking_send(Err(ResourceTransportFailure::Transport));
                        return;
                    }
                };
                let mut stream = Some(stream);
                let outcome = (|| {
                    let registry = active
                        .catalogue_hash_context()
                        .standard()
                        .and_then(|standard| registered_opaque_codecs(standard).ok())
                        .ok_or(ResourceTransportFailure::Transport)?;
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| ResourceTransportFailure::Transport)?;
                    runtime.block_on(run_resource_transport(
                        stream
                            .take()
                            .ok_or(ResourceTransportFailure::Transport)?,
                        active,
                        registry,
                        protocol_request,
                        expected_type,
                        resource_kind,
                        control_receiver,
                        &sender,
                    ))
                })();
                drop(stream);
                if let Some(server_task) = server_task {
                    let _ = server_task.join();
                }
                let _ = sender.blocking_send(outcome);
            });
        let Ok(worker) = worker else {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        };
        let pending = ClientResourceCompletion::Pending {
            request_id: request.request_id(),
            key: request.key(),
            generation: request.generation(),
        };
        self.pending = Some(PendingResourceTransport {
            request,
            receiver,
            control,
            worker,
            cancel_requested: false,
        });
        pending
    }

    fn poll(&mut self) -> Option<ClientResourceCompletion> {
        loop {
            let (result, cancel_requested, request) = {
                let pending = self.pending.as_mut()?;
                let result = match pending.receiver.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => return None,
                    Err(TryRecvError::Disconnected) => {
                        Err(ResourceTransportFailure::Transport)
                    }
                };
                (result, pending.cancel_requested, pending.request.clone())
            };
            let stream_values = matches!(
                &result,
                Ok(ResourceTransportOutcome::StreamValues(_))
            );
            if stream_values && !cancel_requested {
                return Some(map_resource_transport_completion(request, result));
            }
            if stream_values {
                continue;
            }
            let pending = self
                .pending
                .take()
                .expect("pending resource transport was checked above");
            let _ = pending.worker.join();
            return Some(map_resource_transport_completion(pending.request, result));
        }
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        let Some(mut pending) = self.pending.take() else {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        };
        if pending.request.request_id() != request.request_id()
            || pending.request.key() != request.key()
            || pending.request.generation() != request.generation()
        {
            self.pending = Some(pending);
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        }
        if pending.cancel_requested {
            self.pending = Some(pending);
            return request.pending();
        }
        loop {
            match pending.receiver.try_recv() {
                Ok(Ok(ResourceTransportOutcome::StreamValues(_))) => {}
                Ok(result @ Ok(_)) | Ok(result @ Err(_)) => {
                    let _ = pending.worker.join();
                    return map_resource_transport_completion(pending.request, result);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = pending.worker.join();
                    return map_resource_transport_completion(
                        pending.request,
                        Err(ResourceTransportFailure::Transport),
                    );
                }
            }
        }
        pending.cancel_requested = true;
        let _ = pending
            .control
            .send(ResourceTransportControl::Cancel(ResourceCancellationCode::ClientRequested));
        self.pending = Some(pending);
        request.pending()
    }

}
impl Drop for InstalledClientResourceExecutor {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.take() {
            drop(pending.receiver);
            let _ = pending.control.send(ResourceTransportControl::Shutdown);
            let _ = pending.worker.join();
        }
        for detached in self.detached.drain(..) {
            let _ = detached.control.send(ResourceTransportControl::Shutdown);
            let _ = detached.worker.join();
        }
    }
}

fn map_resource_transport_completion(
    request: ClientResourceRequest,
    outcome: Result<ResourceTransportOutcome, ResourceTransportFailure>,
) -> ClientResourceCompletion {
    match outcome {
        Ok(ResourceTransportOutcome::Ready(value)) => request.ready(value),
        Ok(ResourceTransportOutcome::StreamValues(values)) => request.stream_values(values),
        Ok(ResourceTransportOutcome::StreamCompleted) => request.stream_completed(),
        Ok(ResourceTransportOutcome::Failed { failure }) => {
            request.failed(server_resource_failure_code(failure).to_owned())
        }
        Ok(ResourceTransportOutcome::Cancelled) => request.cancelled(),
        Err(ResourceTransportFailure::Shape) => {
            request.failed(SERVER_RESOURCE_SHAPE_CODE.to_owned())
        }
        Err(ResourceTransportFailure::Transport) => {
            request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned())
        }
    }
}

enum ResourceTransportFailure {
    Transport,
    Shape,
}

enum ResourceTransportOutcome {
    Ready(RuntimeValue),
    StreamValues(Vec<RuntimeValue>),
    StreamCompleted,
    Failed { failure: CallFailure },
    Cancelled,
}

enum ResourceFrameResult {
    Continue,
    Completed,
    Failed(CallFailure),
    Cancelled,
}

async fn send_resource_cancel(
    stream: &mut tokio::net::UnixStream,
    active: &ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
    stream_id: u64,
    request_id: orna_core::InvocationId,
    reason: ResourceCancellationCode,
) -> Result<(), ResourceTransportFailure> {
    let encoded_cancel = encode_resource_client_frame(
        active,
        registry,
        &ResourceClientFrame::Cancel(ResourceCancel {
            stream_id,
            request_id,
            reason,
        }),
    )
    .map_err(|_| ResourceTransportFailure::Shape)?;
    tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, stream.write_all(&encoded_cancel))
        .await
        .map_err(|_| ResourceTransportFailure::Transport)?
        .map_err(|_| ResourceTransportFailure::Transport)
}

async fn write_resource_transport_stage(
    stream: &mut tokio::net::UnixStream,
    bytes: &[u8],
    controls: &mut UnboundedReceiver<ResourceTransportControl>,
) -> Result<Option<ResourceTransportControl>, ResourceTransportFailure> {
    tokio::select! {
        biased;
        result = tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, stream.write_all(bytes)) => {
            result
                .map_err(|_| ResourceTransportFailure::Transport)?
                .map_err(|_| ResourceTransportFailure::Transport)?;
            Ok(None)
        }
        control = controls.recv() => match control {
            Some(control) => Ok(Some(control)),
            None => Err(ResourceTransportFailure::Transport),
        },
    }
}

async fn read_resource_ack_stage(
    stream: &mut tokio::net::UnixStream,
    acknowledgement: &mut [u8],
    controls: &mut UnboundedReceiver<ResourceTransportControl>,
) -> Result<Option<ResourceTransportControl>, ResourceTransportFailure> {
    tokio::select! {
        biased;
        result = tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, stream.read_exact(acknowledgement)) => {
            result
                .map_err(|_| ResourceTransportFailure::Transport)?
                .map_err(|_| ResourceTransportFailure::Transport)?;
            Ok(None)
        }
        control = controls.recv() => match control {
            Some(control) => Ok(Some(control)),
            None => Err(ResourceTransportFailure::Transport),
        },
    }
}

fn pre_request_control_outcome(
    control: ResourceTransportControl,
) -> Result<ResourceTransportOutcome, ResourceTransportFailure> {
    match control {
        ResourceTransportControl::Cancel(_) => Ok(ResourceTransportOutcome::Cancelled),
        ResourceTransportControl::Shutdown => Err(ResourceTransportFailure::Transport),
    }
}

async fn send_resource_outcome(
    sender: &Sender<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
    controls: &mut UnboundedReceiver<ResourceTransportControl>,
    outcome: Result<ResourceTransportOutcome, ResourceTransportFailure>,
) -> Result<Option<ResourceTransportControl>, ResourceTransportFailure> {
    tokio::select! {
        biased;
        result = sender.send(outcome) => {
            result
                .map(|_| None)
                .map_err(|_| ResourceTransportFailure::Transport)
        }
        control = controls.recv() => match control {
            Some(control) => Ok(Some(control)),
            None => Err(ResourceTransportFailure::Transport),
        },
    }
}


#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResourceFrameDispositionAction {
    Apply,
    Drain,
    Drop,
    Reject,
}

/// Decides what a server frame means after the client has requested cancel.
///
/// The local connection stays live after sending `RESOURCE_CANCEL`: an
/// already-committed server terminal frame is therefore `Applied` and wins,
/// while `Accepted`/`Values` are drained without publication. A
/// `DroppedLate` frame is already after a terminal transition and is never
/// published. The rule is independent of scalar versus stream; only the
/// caller's publication policy differs for those resource kinds.
fn resource_transport_disposition_action(
    disposition: orna_protocol::ResourceFrameDisposition,
    cancellation_requested: bool,
    terminal: bool,
) -> ResourceFrameDispositionAction {
    match (cancellation_requested, disposition, terminal) {
        (false, orna_protocol::ResourceFrameDisposition::Applied, _)
        | (true, orna_protocol::ResourceFrameDisposition::Applied, true) => {
            ResourceFrameDispositionAction::Apply
        }
        (true, orna_protocol::ResourceFrameDisposition::Applied, false) => {
            ResourceFrameDispositionAction::Drain
        }
        (true, orna_protocol::ResourceFrameDisposition::DroppedLate, _) => {
            ResourceFrameDispositionAction::Drop
        }
        (false, orna_protocol::ResourceFrameDisposition::DroppedLate, _) => {
            ResourceFrameDispositionAction::Reject
        }
    }
}

async fn run_resource_transport(
    stream: StandardUnixStream,
    active: ActiveDatabaseRevision,
    registry: orna_core::value::OpaqueCodecRegistry,
    request: ResourceRequest,
    expected_type: ResolvedType,
    resource_kind: ProtocolResourceKind,
    mut controls: UnboundedReceiver<ResourceTransportControl>,
    completion_sender: &Sender<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
) -> Result<ResourceTransportOutcome, ResourceTransportFailure> {
    stream
        .set_nonblocking(true)
        .map_err(|_| ResourceTransportFailure::Transport)?;
    let mut stream = tokio::net::UnixStream::from_std(stream)
        .map_err(|_| ResourceTransportFailure::Transport)?;
    let encoded_request = encode_resource_client_frame(
        &active,
        &registry,
        &ResourceClientFrame::Request(request.clone()),
    )
    .map_err(|_| ResourceTransportFailure::Shape)?;
    let stream_id = request.stream_id;
    let request_id = request.request_id;
    let mut connection = ResourceProtocolConnection::new();
    connection
        .open(request)
        .map_err(|_| ResourceTransportFailure::Shape)?;

    if let Some(control) = write_resource_transport_stage(
        &mut stream,
        &CONSTRUCTED_CLIENT_HELLO,
        &mut controls,
    )
    .await?
    {
        return pre_request_control_outcome(control);
    }
    let mut acknowledgement = [0_u8; CONSTRUCTED_SERVER_ACK.len()];
    if let Some(control) = read_resource_ack_stage(
        &mut stream,
        &mut acknowledgement,
        &mut controls,
    )
    .await?
    {
        return pre_request_control_outcome(control);
    }
    if acknowledgement != CONSTRUCTED_SERVER_ACK {
        return Err(ResourceTransportFailure::Transport);
    }
    if let Some(control) = write_resource_transport_stage(
        &mut stream,
        &encoded_request,
        &mut controls,
    )
    .await?
    {
        return pre_request_control_outcome(control);
    }

    let mut accepted = false;
    let mut scalar_value = None;
    let mut stream_values_seen = false;
    let mut cancellation_requested = false;
    loop {
        let frame = loop {
            let (control, frame) = tokio::select! {
                control = controls.recv() => (Some(control), None),
                frame = read_resource_server_frame(&mut stream, &active, &registry) => {
                    (None, Some(frame))
                }
            };
            if let Some(control) = control {
                match control {
                    Some(control) if !cancellation_requested => {
                        let reason = match control {
                            ResourceTransportControl::Cancel(reason) => reason,
                            ResourceTransportControl::Shutdown => {
                                ResourceCancellationCode::RuntimeShutdown
                            }
                        };
                        send_resource_cancel(
                            &mut stream,
                            &active,
                            &registry,
                            stream_id,
                            request_id,
                            reason,
                        )
                        .await?;
                        cancellation_requested = true;
                    }
                    Some(_) => {}
                    None => return Err(ResourceTransportFailure::Transport),
                }
                continue;
            }
            break frame.expect("resource frame branch produced no frame")?;
        };

        if matches!(&frame, ResourceServerFrame::Failed(_)) && scalar_value.is_some() {
            return Err(ResourceTransportFailure::Shape);
        }
        let is_accepted = match &frame {
            ResourceServerFrame::Accepted(accepted) => {
                if accepted.resource_kind != resource_kind {
                    return Err(ResourceTransportFailure::Shape);
                }
                true
            }
            _ => false,
        };
        let value_batch = match &frame {
            ResourceServerFrame::Values(values) => {
                if values.values.is_empty()
                    || values.item_count == 0
                    || values.item_count as usize != values.values.len()
                {
                    return Err(ResourceTransportFailure::Shape);
                }
                if matches!(resource_kind, ProtocolResourceKind::Single)
                    && (values.values.len() != 1 || scalar_value.is_some())
                {
                    return Err(ResourceTransportFailure::Shape);
                }
                for value in &values.values {
                    if !runtime_value_matches_type(&active, value, expected_type) {
                        return Err(ResourceTransportFailure::Shape);
                    }
                }
                Some((values.values.clone(), values.item_count, values.byte_count))
            }
            _ => None,
        };
        let response = match &frame {
            ResourceServerFrame::Accepted(_) | ResourceServerFrame::Values(_) => {
                ResourceFrameResult::Continue
            }
            ResourceServerFrame::Completed(_) => ResourceFrameResult::Completed,
            ResourceServerFrame::Failed(frame) => ResourceFrameResult::Failed(frame.failure),
            ResourceServerFrame::Cancelled(_) => ResourceFrameResult::Cancelled,
        };
        let disposition = match frame {
            ResourceServerFrame::Cancelled(cancelled) if cancellation_requested => {
                let cancel = ResourceClientFrame::Cancel(ResourceCancel {
                    stream_id,
                    request_id,
                    reason: ResourceCancellationCode::ClientRequested,
                });
                connection
                    .receive(cancel)
                    .map_err(|_| ResourceTransportFailure::Shape)?;
                connection
                    .apply_cancelled_after_client_cancel(cancelled)
                    .map_err(|_| ResourceTransportFailure::Shape)?
            }
            frame => connection
                .apply_constructed(&active, &registry, frame)
                .map_err(|_| ResourceTransportFailure::Shape)?,
        };
        match resource_transport_disposition_action(
            disposition,
            cancellation_requested,
            matches!(
                &response,
                ResourceFrameResult::Completed
                    | ResourceFrameResult::Failed(_)
                    | ResourceFrameResult::Cancelled
            ),
        ) {
            ResourceFrameDispositionAction::Reject => {
                return Err(ResourceTransportFailure::Shape);
            }
            ResourceFrameDispositionAction::Drop => continue,
            ResourceFrameDispositionAction::Apply | ResourceFrameDispositionAction::Drain => {}
        }
        if is_accepted {
            accepted = true;
        }
        if let Some((values, item_count, byte_count)) = value_batch {
            match resource_kind {
                ProtocolResourceKind::Single => {
                    scalar_value = values.into_iter().next();
                }
                ProtocolResourceKind::Stream => {
                    // Drain late values after cancellation without publishing or crediting them.
                    if !cancellation_requested {
                        if let Some(control) = send_resource_outcome(
                            completion_sender,
                            &mut controls,
                            Ok(ResourceTransportOutcome::StreamValues(values)),
                        )
                        .await?
                        {
                            if !cancellation_requested {
                                let reason = match control {
                                    ResourceTransportControl::Cancel(reason) => reason,
                                    ResourceTransportControl::Shutdown => {
                                        ResourceCancellationCode::RuntimeShutdown
                                    }
                                };
                                send_resource_cancel(
                                    &mut stream,
                                    &active,
                                    &registry,
                                    stream_id,
                                    request_id,
                                    reason,
                                )
                                .await?;
                                cancellation_requested = true;
                            }
                            continue;
                        }
                        stream_values_seen = true;
                        let update = ResourceWindowUpdate {
                            stream_id,
                            request_id,
                            add_items: u64::from(item_count),
                            add_bytes: u64::from(byte_count),
                        };
                        if !matches!(
                            connection
                                .receive(ResourceClientFrame::WindowUpdate(update))
                                .map_err(|_| ResourceTransportFailure::Shape)?,
                            orna_protocol::ResourceFrameDisposition::Applied
                        ) {
                            return Err(ResourceTransportFailure::Shape);
                        }
                        let encoded_update = encode_resource_client_frame(
                            &active,
                            &registry,
                            &ResourceClientFrame::WindowUpdate(update),
                        )
                        .map_err(|_| ResourceTransportFailure::Shape)?;
                        if let Some(control) = write_resource_transport_stage(
                            &mut stream,
                            &encoded_update,
                            &mut controls,
                        )
                        .await?
                        {
                            if !cancellation_requested {
                                let reason = match control {
                                    ResourceTransportControl::Cancel(reason) => reason,
                                    ResourceTransportControl::Shutdown => {
                                        ResourceCancellationCode::RuntimeShutdown
                                    }
                                };
                                send_resource_cancel(
                                    &mut stream,
                                    &active,
                                    &registry,
                                    stream_id,
                                    request_id,
                                    reason,
                                )
                                .await?;
                                cancellation_requested = true;
                            }
                        }
                    }
                }
            }
        }
        match response {
            ResourceFrameResult::Continue => {
                if (scalar_value.is_some() || stream_values_seen) && !accepted {
                    return Err(ResourceTransportFailure::Shape);
                }
            }
            ResourceFrameResult::Completed => {
                if !accepted {
                    return Err(ResourceTransportFailure::Shape);
                }
                match resource_kind {
                    ProtocolResourceKind::Single => {
                        let value = scalar_value.ok_or(ResourceTransportFailure::Shape)?;
                        return Ok(ResourceTransportOutcome::Ready(value));
                    }
                    ProtocolResourceKind::Stream => {
                        return Ok(ResourceTransportOutcome::StreamCompleted);
                    }
                }
            }
            ResourceFrameResult::Failed(failure) => {
                if scalar_value.is_some() {
                    return Err(ResourceTransportFailure::Shape);
                }
                return Ok(ResourceTransportOutcome::Failed { failure });
            }
            ResourceFrameResult::Cancelled => {
                return Ok(ResourceTransportOutcome::Cancelled);
            }
        }
    }
}
async fn read_resource_server_frame(
    stream: &mut tokio::net::UnixStream,
    active: &ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
) -> Result<ResourceServerFrame, ResourceTransportFailure> {
    let mut header = [0_u8; RESOURCE_HEADER_LENGTH];
    tokio::time::timeout(
        RESOURCE_FRAME_TIMEOUT,
        stream.read_exact( &mut header),
    )
    .await
    .map_err(|_| ResourceTransportFailure::Transport)?
    .map_err(|_| ResourceTransportFailure::Transport)?;
    if &header[..RESOURCE_MARKER.len()] != RESOURCE_MARKER {
        return Err(ResourceTransportFailure::Shape);
    }
    let payload_length = u32::from_be_bytes(
        header[17..21]
            .try_into()
            .expect("resource frame header has a fixed length"),
    ) as usize;
    if payload_length > MAX_FRAME_PAYLOAD_LENGTH {
        return Err(ResourceTransportFailure::Shape);
    }
    let mut encoded = header.to_vec();
    encoded.resize(RESOURCE_HEADER_LENGTH + payload_length, 0);
    tokio::time::timeout(
        RESOURCE_FRAME_TIMEOUT,
        stream.read_exact( &mut encoded[RESOURCE_HEADER_LENGTH..]),
    )
    .await
    .map_err(|_| ResourceTransportFailure::Transport)?
    .map_err(|_| ResourceTransportFailure::Transport)?;
    decode_resource_server_frame(active, registry, &encoded)
        .map_err(|_| ResourceTransportFailure::Shape)
}

fn runtime_value_matches_type(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
    expected: ResolvedType,
) -> bool {
    if let RuntimeValue::Null(null) = value {
        return null.resolved_type() == expected && active_type_is_known(active, expected);
    }
    let scalar_matches = |scalar| match (scalar, value) {
        (StandardScalar::Boolean, RuntimeValue::Boolean(_))
        | (StandardScalar::Integer, RuntimeValue::Integer(_))
        | (StandardScalar::BigInt, RuntimeValue::BigInt(_))
        | (StandardScalar::Float, RuntimeValue::Float(_))
        | (StandardScalar::CharacterLargeObject, RuntimeValue::Text(_))
        | (StandardScalar::BinaryLargeObject, RuntimeValue::Bytes(_)) => true,
        _ => false,
    };
    match expected {
        ResolvedType::Scalar(scalar) => scalar_matches(scalar),
        ResolvedType::Value(type_id) => {
            let Some(definition) = active_type_definition(active, type_id)
                .and_then(|definition| definition.as_value())
            else {
                return false;
            };
            if definition.kind() == ValueTypeKind::Opaque {
                return matches!(
                    value,
                    RuntimeValue::Opaque(opaque) if opaque.opaque_type() == type_id
                );
            }
            match definition.representation_contract() {
                "orna.kernel.value.boolean@1" => scalar_matches(StandardScalar::Boolean),
                "orna.kernel.value.integer@1" => scalar_matches(StandardScalar::Integer),
                "orna.kernel.value.bigint@1" => scalar_matches(StandardScalar::BigInt),
                "orna.kernel.value.float@1" => scalar_matches(StandardScalar::Float),
                "orna.kernel.value.character-large-object@1" => {
                    scalar_matches(StandardScalar::CharacterLargeObject)
                }
                "orna.kernel.value.binary-large-object@1" => {
                    scalar_matches(StandardScalar::BinaryLargeObject)
                }
                _ => false,
            }
        }
        ResolvedType::Named(type_id) => match value {
            RuntimeValue::Record(record) => {
                record.record_type() == type_id
                    && active_type_definition(active, type_id)
                        .is_some_and(|definition| definition.as_record_value().is_some())
            }
            RuntimeValue::Enum(enum_value) => {
                enum_value.enum_type() == type_id
                    && active_type_definition(active, type_id)
                        .and_then(|definition| definition.as_enum())
                        .is_some_and(|definition| {
                            definition
                                .labels()
                                .iter()
                                .any(|label| label == enum_value.label())
                        })
            }
            _ => false,
        },
        ResolvedType::Reference { target } => {
            matches!(value, RuntimeValue::Reference { target: actual, .. } if *actual == target)
                && active_type_definition(active, target)
                    .is_some_and(|definition| definition.as_object().is_some())
        }
    }
}

fn active_type_is_known(active: &ActiveDatabaseRevision, resolved: ResolvedType) -> bool {
    match resolved {
        ResolvedType::Scalar(_) => true,
        ResolvedType::Value(type_id) => active_type_definition(active, type_id)
            .is_some_and(|definition| definition.as_value().is_some()),
        ResolvedType::Named(type_id) => active_type_definition(active, type_id).is_some_and(
            |definition| {
                definition.as_record_value().is_some() || definition.as_enum().is_some()
            },
        ),
        ResolvedType::Reference { target } => active_type_definition(active, target)
            .is_some_and(|definition| definition.as_object().is_some()),
    }
}

fn active_type_definition(
    active: &ActiveDatabaseRevision,
    type_id: TypeId,
) -> Option<orna_core::catalogue::TypeDefinition<'_>> {
    let application = active.catalogue().type_definition_by_id(type_id);
    let standard = active
        .catalogue_hash_context()
        .standard()
        .and_then(|snapshot| snapshot.catalogue().type_definition_by_id(type_id));
    match (application, standard) {
        (Some(_), Some(_)) => None,
        (Some(definition), None) | (None, Some(definition)) => Some(definition),
        (None, None) => None,
    }
}

const fn server_resource_failure_code(failure: orna_protocol::CallFailure) -> &'static str {
    match failure {
        orna_protocol::CallFailure::ExecuteDenied => SERVER_RESOURCE_DENIED_CODE,
        orna_protocol::CallFailure::TargetUnavailable => SERVER_RESOURCE_UNAVAILABLE_CODE,
        orna_protocol::CallFailure::ClientEvaluationFailed
        | orna_protocol::CallFailure::InternalFailure => SERVER_RESOURCE_INTERNAL_CODE,
    }
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

    runtime.block_on(host_invoke(
        kernel,
        request,
        stdout,
        stderr,
        ResourceTransportSource::Installed,
    ))
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
    host_invoke(
        kernel.clone(),
        request,
        stdout,
        stderr,
        ResourceTransportSource::Injected(InjectedResourceTransport::Factory {
            kernel,
            resources: LocalRawSocketResources::new(),
        }),
    )
    .await
}

async fn host_invoke(
    kernel: PostgresKernel,
    request: InstalledInvokeRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    transport: ResourceTransportSource,
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
    let mut resource_executor = match transport {
        ResourceTransportSource::Installed => {
            InstalledClientResourceExecutor::new(kernel.clone(), session.clone(), active.clone())
        }
        ResourceTransportSource::Injected(InjectedResourceTransport::Stream(Some(stream))) => {
            InstalledClientResourceExecutor::new_with_stream(
                kernel.clone(),
                session.clone(),
                active.clone(),
                stream,
            )
        }
        ResourceTransportSource::Injected(InjectedResourceTransport::Stream(None)) => {
            return Err(InstalledInvokeError::new(
                InstalledInvokeErrorKind::Internal,
                "the injected resource socket was unavailable".to_owned(),
            ));
        }
        ResourceTransportSource::Injected(InjectedResourceTransport::Factory { kernel, resources }) => {
            InstalledClientResourceExecutor::new_with_factory(active.clone(), kernel, resources)
        }
    };
    let result = kernel
        .dispatch_sealed_sys_invoke_with_resource_executor(
            &session,
            CONNECTION_PROTOCOL_MAJOR,
            &retained,
            Some(&mut resource_executor),
        )
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
/// offer is protocol major 5 with the two ADR 0057 sink offers
/// (`std.terminal.Document` and `std.io.ByteStream`), the installed runtime
/// offer list filtered to the selected family (ADR 0063), and the default
/// limits.
fn build_sealed_request(
    request: &InstalledInvokeRequest,
    arguments: Vec<InvocationArgument>,
) -> Result<InvokeRequest, InstalledInvokeError> {
    let caller_context = build_caller_context()?;
    let runtime_offers = match selected_runtime(request)? {
        Some(RuntimeFamily::Tty) => installed_runtime_offers(),
        // A future selection path could select a family with no installed
        // runtime; the sealed request then carries no runtime offer. Today
        // every request selects the tty runtime.
        _ => Vec::new(),
    };
    let client_offer = InvocationClientOffer::new(
        CONNECTION_PROTOCOL_MAJOR,
        caller_context.locale(),
        caller_context.timezone(),
        client_sink_offers()?,
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

/// Builds the runtime offers for the installed tty runtime (ADR 0063).
///
/// The one offer names the two sink types the tty runtime renders
/// (`std.terminal.Document` and `std.io.ByteStream`), carries no UI
/// contract surface yet, and marks the linked runtime trusted with the
/// default preference rank. The construction cannot fail: the name and
/// version are non-empty and the consumed descriptors are the same
/// standard named descriptors the sink offers already carry.
fn installed_runtime_offers() -> Vec<InvocationRuntimeOffer> {
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

/// Selects the runtime family for one invoke request (ADR 0063).
///
/// The default (no `--runtime` override) is the tty runtime, the only
/// runtime installed in this workspace, and an explicit `tty` override
/// selects the same. Any other family — a future desktop family such as
/// `qt`, `gtk`, or `imgui` — fails closed as a usage error because no such
/// runtime is installed. Platform preference defaults (Linux desktop
/// gtk > qt > imgui) are a later slice that depends on local configuration;
/// this policy is deliberately family-explicit.
fn selected_runtime(
    request: &InstalledInvokeRequest,
) -> Result<Option<RuntimeFamily>, InstalledInvokeError> {
    match request.runtime {
        None => Ok(Some(RuntimeFamily::Tty)),
        Some(RuntimeFamily::Tty) => Ok(Some(RuntimeFamily::Tty)),
        Some(other) => Err(usage_error(format!(
            "the {} runtime family is not installed",
            other.name()
        ))),
    }
}

/// Builds the two sink offers the installed client consumes (ADR 0057 step 9).
///
/// The offer names `std.terminal.Document` and `std.io.ByteStream` so the
/// sealed route's presentation planning sees the sinks the client can
/// consume. Neither sink streams in this slice, both carry the default
/// preference rank, and runtime selection stays the client's own
/// deterministic decision, not a server-visible negotiation.
fn client_sink_offers() -> Result<Vec<InvocationSinkOffer>, InstalledInvokeError> {
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
    Ok(vec![document, byte_stream])
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
/// `ValueBatch` value goes to `stdout` — Document and ByteStream values
/// through `orna-runtime-tty`, every other value through the supplied
/// encoder as one canonical record — with no progress or warning interleave.
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
        SealedInvocationResult::Failed { events, .. } => {
            render_event_stream(events, no_progress, stdout, stderr, encode)
        }
        SealedInvocationResult::Denied { .. } => {
            writeln!(stderr, "orna: invoke: invocation denied").map_err(presentation_error)?;
            Ok(InstalledInvokeOutcome::Denied)
        }
        SealedInvocationResult::PresentationFailed { .. } => Err(InstalledInvokeError::new(
            InstalledInvokeErrorKind::Presentation,
            "presentation failed".to_owned(),
        )),
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
                    render_value(value.value(), stdout, encode)?;
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

/// Renders one `ValueBatch` value to stdout.
///
/// A `std.terminal.Document` value renders as the document text and a
/// `std.io.ByteStream` value as the raw stream bytes, both through
/// `orna-runtime-tty` (ADR 0057 step 9); every other value keeps the
/// milestone-5 rule: the canonical ORV5 typed encoding followed by the
/// record newline.
fn render_value(
    value: &RuntimeValue,
    stdout: &mut impl Write,
    encode: &mut impl FnMut(&RuntimeValue) -> Result<Vec<u8>, InstalledInvokeError>,
) -> Result<(), InstalledInvokeError> {
    if let RuntimeValue::Opaque(opaque) = value
        && let Some(sink) = select_runtime_sink(opaque.opaque_type())
    {
        render_opaque_payload(sink, opaque.canonical_payload(), stdout)?;
        return Ok(());
    }
    let encoded = encode(value)?;
    stdout.write_all(&encoded).map_err(presentation_error)?;
    stdout.write_all(b"\n").map_err(presentation_error)?;
    Ok(())
}

/// Returns the deterministic tty runtime sink for one opaque result type
/// (ADR 0057 step 9), or `None` when the value keeps the ORV5 envelope.
///
/// The rule is unconditional for the two sink types: `--output table`
/// produces a `std.terminal.Document` and `--output json` produces a
/// `std.io.ByteStream`, and in both cases the bytes must reach stdout
/// whether stdout is a terminal or piped to a file. The stdout-is-terminal
/// fact still feeds the caller context (`CliTty` versus `CliPipe`); it does
/// not gate sink consumption.
///
/// Seam (ADR 0063): this mapping is the tty family's sink map and stays
/// unconditional while tty is the only installed runtime. When a second
/// family lands, the renderer gains the selected-family parameter and this
/// function becomes the selected family's sink map.
fn select_runtime_sink(opaque_type: TypeId) -> Option<orna_runtime_tty::Sink> {
    match opaque_type {
        STD_TERMINAL_DOCUMENT_TYPE_ID => Some(orna_runtime_tty::Sink::Document),
        STD_IO_BYTE_STREAM_TYPE_ID => Some(orna_runtime_tty::Sink::ByteStream),
        _ => None,
    }
}

/// Renders one presented opaque payload through the selected runtime sink.
///
/// The payload is the canonical codec frame the sealed route emitted; the
/// runtime validates it again before writing anything.
fn render_opaque_payload(
    sink: orna_runtime_tty::Sink,
    payload: &[u8],
    stdout: &mut impl Write,
) -> Result<(), InstalledInvokeError> {
    sink.render(payload, stdout).map_err(map_runtime_tty_error)
}

/// Maps one runtime rendering failure to a closed installed error.
///
/// A write failure is a presentation failure like any other output error; a
/// frame rejection cannot occur for a registry-validated value and is an
/// internal inconsistency.
fn map_runtime_tty_error(error: orna_runtime_tty::RuntimeTtyError) -> InstalledInvokeError {
    match error {
        orna_runtime_tty::RuntimeTtyError::Io(error) => presentation_error(error),
        other => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            format!("the tty runtime rejected a presented value: {other}"),
        ),
    }
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
        render_runtime_offers(request.client_offer().runtime_offers()),
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

/// Renders the offered runtimes as `name@version` entries, or `none`.
fn render_runtime_offers(offers: &[InvocationRuntimeOffer]) -> String {
    let mut rendered = offers
        .iter()
        .map(|offer| format!("{}@{}", offer.name(), offer.version()))
        .collect::<Vec<_>>()
        .join(", ");
    if rendered.is_empty() {
        rendered.push_str("none");
    }
    rendered
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
        FunctionReturn::Stream(resolved) =>
            format!("STREAM<{}>", render_resolved_type(*resolved)),
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

fn map_dispatch_error(_error: PostgresKernelError) -> InstalledInvokeError {
    InstalledInvokeError::new(
        InstalledInvokeErrorKind::Internal,
        "sealed dispatch failed".to_owned(),
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
        invocation::{
            InvocationFailure, InvocationFailurePhase, InvocationRetryability, InvokeEvent,
            InvokeValue,
        },
        types::StandardScalar,
        value::RuntimeValue,
    };
    use orna_protocol::{InvocationEventBatch, InvocationEventRecord};
    use orna_standard::STD_UI_TYPE_ID;

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
    fn cancellation_disposition_preserves_committed_terminals_and_drops_late_frames() {
        use orna_protocol::ResourceFrameDisposition::{Applied, DroppedLate};

        assert_eq!(
            resource_transport_disposition_action(Applied, true, true),
            ResourceFrameDispositionAction::Apply,
        );
        assert_eq!(
            resource_transport_disposition_action(Applied, true, false),
            ResourceFrameDispositionAction::Drain,
        );
        assert_eq!(
            resource_transport_disposition_action(DroppedLate, true, true),
            ResourceFrameDispositionAction::Drop,
        );
        assert_eq!(
            resource_transport_disposition_action(DroppedLate, true, false),
            ResourceFrameDispositionAction::Drop,
        );
        assert_eq!(
            resource_transport_disposition_action(DroppedLate, false, true),
            ResourceFrameDispositionAction::Reject,
        );
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
    fn failed_event_prints_one_redacted_line_and_exits_target_failure() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let invocation = InvocationId::new();
        let started = InvokeEvent::new(
            invocation,
            0,
            InvocationEventBody::Started {
                visible_principal: None,
            },
        )
        .expect("started event");
        let failure = InvocationFailure::new(
            InvocationFailurePhase::Bind,
            "INVOKE_BIND_FAILED",
            "invocation arguments were not accepted",
            None,
            InvocationRetryability::No,
        )
        .expect("failure body");
        let failed = InvokeEvent::new(invocation, 1, InvocationEventBody::Failed(failure))
            .expect("failed event");
        let events = InvocationEventBatch::new(vec![
            InvocationEventRecord::new(1, started),
            InvocationEventRecord::new(2, failed),
        ])
        .expect("event batch");
        let result = SealedInvocationResult::Failed { invocation, events };
        let outcome = render_result(&result, false, &mut stdout, &mut stderr, &mut encoder)
            .expect("rendering succeeds");
        assert_eq!(outcome, InstalledInvokeOutcome::TargetFailure);
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8(stderr).expect("stderr is text"),
            "orna: invoke: invocation started\norna: invoke: invocation failed\n"
        );
    }

    #[test]
    fn presentation_failure_returns_the_closed_presentation_error() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = SealedInvocationResult::PresentationFailed {
            invocation: InvocationId::new(),
        };
        let error = render_result(&result, false, &mut stdout, &mut stderr, &mut encoder)
            .expect_err("a presentation failure is a closed presentation error");
        assert_eq!(error.kind(), InstalledInvokeErrorKind::Presentation);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }

    /// Builds one canonical `std.terminal.Document` payload frame.
    fn document_frame(body: &[u8]) -> Vec<u8> {
        let mut frame = b"ORNA-TERMINAL-DOCUMENT/1 ".to_vec();
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        frame
    }

    /// Builds one canonical `std.io.ByteStream` payload frame.
    fn byte_stream_frame(media_type: &[u8], body: &[u8]) -> Vec<u8> {
        let mut frame = b"ORNA-BYTE-STREAM/1 ".to_vec();
        frame.extend_from_slice(&(media_type.len() as u32).to_be_bytes());
        frame.extend_from_slice(media_type);
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        frame
    }

    #[test]
    fn selection_rule_maps_document_and_byte_stream_to_the_tty_runtime() {
        let cases = [
            (
                STD_TERMINAL_DOCUMENT_TYPE_ID,
                Some(orna_runtime_tty::Sink::Document),
            ),
            (
                STD_IO_BYTE_STREAM_TYPE_ID,
                Some(orna_runtime_tty::Sink::ByteStream),
            ),
            (TypeId::from_bytes([0x41; 16]), None),
        ];
        for (opaque_type, expected) in cases {
            assert_eq!(select_runtime_sink(opaque_type), expected);
        }
    }

    #[test]
    fn document_value_renders_as_document_text_on_stdout() {
        let body = b"name | age\nalice | 41\n";
        let frame = document_frame(body);
        let mut stdout = Vec::new();
        render_opaque_payload(orna_runtime_tty::Sink::Document, &frame, &mut stdout)
            .expect("rendering a document frame succeeds");
        assert_eq!(stdout, body);
    }

    #[test]
    fn byte_stream_value_renders_as_raw_bytes_on_stdout() {
        let body = b"{\"ok\":true}";
        let frame = byte_stream_frame(b"application/json", body);
        let mut stdout = Vec::new();
        render_opaque_payload(orna_runtime_tty::Sink::ByteStream, &frame, &mut stdout)
            .expect("rendering a byte-stream frame succeeds");
        // The stream bytes go to stdout with no envelope, progress
        // interleave, or trailing record newline.
        assert_eq!(stdout, body);
    }

    #[test]
    fn a_rejected_runtime_payload_writes_nothing_and_returns_internal() {
        let mut stdout = Vec::new();
        let error = render_opaque_payload(
            orna_runtime_tty::Sink::Document,
            b"ORNA-TERMINAL-DOCUMENT/1 \0\0\0\x05broken",
            &mut stdout,
        )
        .expect_err("an inconsistent frame is rejected");
        assert_eq!(error.kind(), InstalledInvokeErrorKind::Internal);
        assert!(stdout.is_empty());
    }

    #[test]
    fn non_sink_values_keep_the_orv5_envelope() {
        let mut stdout = Vec::new();
        render_value(&RuntimeValue::Integer(41), &mut stdout, &mut encoder)
            .expect("rendering a non-sink value succeeds");
        assert_eq!(stdout, encoded_record());
    }

    #[test]
    fn the_client_offer_names_the_tty_runtime() {
        let request = InstalledInvokeRequest::new(
            InvocationTarget::qualified_name(
                QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("qualified name"),
            )
            .expect("target"),
            Vec::new(),
            None,
            None,
            false,
            false,
            None,
        );
        let sealed = build_sealed_request(&request, Vec::new()).expect("the sealed request builds");
        let offer = sealed.client_offer();
        assert_eq!(offer.sink_offers().len(), 2);
        assert_eq!(
            offer.sink_offers()[0].descriptor(),
            &TypeDescriptor::named(STD_TERMINAL_DOCUMENT_TYPE_ID)
        );
        assert_eq!(
            offer.sink_offers()[0].media_types(),
            &[DOCUMENT_SINK_MEDIA_TYPE.to_owned()]
        );
        assert!(!offer.sink_offers()[0].streaming());
        assert_eq!(
            offer.sink_offers()[1].descriptor(),
            &TypeDescriptor::named(STD_IO_BYTE_STREAM_TYPE_ID)
        );
        assert_eq!(
            offer.sink_offers()[1].media_types(),
            &[BYTE_STREAM_SINK_MEDIA_TYPE.to_owned()]
        );
        assert!(!offer.sink_offers()[1].streaming());
        // The installed tty runtime offer survives the sealed request
        // construction (ADR 0063).
        assert_eq!(offer.runtime_offers().len(), 1);
        let runtime = &offer.runtime_offers()[0];
        assert_eq!(runtime.name(), "tty");
        assert_eq!(runtime.version(), orna_runtime_tty::RUNTIME_VERSION);
        assert!(!runtime.version().is_empty());
        assert_eq!(
            runtime.consumed_descriptors(),
            &[
                TypeDescriptor::named(STD_TERMINAL_DOCUMENT_TYPE_ID),
                TypeDescriptor::named(STD_IO_BYTE_STREAM_TYPE_ID),
            ]
        );
        assert!(runtime.contracts().is_empty());
        assert_eq!(runtime.preference_rank(), 0);
        assert!(runtime.trusted());
        assert!(runtime.limits().is_none());
    }

    /// Builds one echo request with the given runtime override.
    fn runtime_request(runtime: Option<RuntimeFamily>) -> InstalledInvokeRequest {
        InstalledInvokeRequest::new(
            InvocationTarget::qualified_name(
                QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("qualified name"),
            )
            .expect("target"),
            Vec::new(),
            None,
            None,
            false,
            false,
            runtime,
        )
    }

    #[test]
    fn selection_policy_defaults_to_tty_and_rejects_unknown_families() {
        // No override selects the installed tty runtime...
        assert_eq!(
            selected_runtime(&runtime_request(None)),
            Ok(Some(RuntimeFamily::Tty))
        );
        // ...and an explicit tty override selects the same runtime.
        assert_eq!(
            selected_runtime(&runtime_request(Some(RuntimeFamily::Tty))),
            Ok(Some(RuntimeFamily::Tty))
        );
        // A recognised-but-not-installed family fails closed as a usage
        // error naming the family. `--runtime` parsing never produces this
        // variant today — unknown families are rejected at the CLI — so
        // the hidden variant stands in for the future family that will.
        let error = selected_runtime(&runtime_request(Some(RuntimeFamily::NotInstalled)))
            .expect_err("a not-installed family is rejected");
        assert_eq!(error.kind(), InstalledInvokeErrorKind::Usage);
        assert!(error.message().contains("not-installed"));
    }

    #[test]
    fn tty_default_selection_maps_document_and_byte_stream() {
        let selected =
            selected_runtime(&runtime_request(None)).expect("the default selects the tty runtime");
        assert_eq!(selected, Some(RuntimeFamily::Tty));
        // The tty family's sink map consumes exactly the two standard
        // sink types; a UI value keeps the ORV5 envelope.
        assert_eq!(
            select_runtime_sink(STD_TERMINAL_DOCUMENT_TYPE_ID),
            Some(orna_runtime_tty::Sink::Document)
        );
        assert_eq!(
            select_runtime_sink(STD_IO_BYTE_STREAM_TYPE_ID),
            Some(orna_runtime_tty::Sink::ByteStream)
        );
        assert_eq!(select_runtime_sink(STD_UI_TYPE_ID), None);
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
            installed_runtime_offers(),
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
    fn render_return_type_preserves_scalar_and_rows_and_names_stream_items() {
        assert_eq!(
            render_return_type(&FunctionReturn::Single(ResolvedType::Scalar(
                StandardScalar::Integer,
            ))),
            "INTEGER",
        );
        assert_eq!(
            render_return_type(&FunctionReturn::Stream(ResolvedType::Scalar(
                StandardScalar::Integer,
            ))),
            "STREAM<INTEGER>",
        );
        assert_eq!(
            render_return_type(&FunctionReturn::Rows(vec![
                orna_core::catalogue::FunctionReturnColumnDefinition::new(
                    "value",
                    0,
                    ResolvedType::Scalar(StandardScalar::Integer),
                ),
            ])),
            "ROWS (value INTEGER)",
        );
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
        assert!(plan.contains(
            "offer: protocol 5, locale en-GB, timezone UTC, sinks 0, runtimes tty@0.1.0"
        ));
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
