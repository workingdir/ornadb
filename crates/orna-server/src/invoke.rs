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
    collections::BTreeMap,
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
    system::SYS_INVOKE_FUNCTION_ID,
    types::{ResolvedType, StandardScalar, TypeDescriptor},
    value::{OpaqueCodecRegistry, RuntimeValue},
};
use orna_postgres::{
    AuthenticatedServerResourceEvent, AuthenticatedServerResourceStart, PostgresKernel,
    ResourceCancellation, ResourceCredit, SealedInvocationResult,
};
use orna_protocol::{
    CallFailure, Channel, ClientFrame, Event, InvocationEventRecord, MAX_CHANNEL_WINDOW,
    MAX_FRAME_PAYLOAD_LENGTH, MAX_RESOURCE_WINDOW, ProtocolConnection, ResourceArgument,
    ResourceCancel, ResourceCancellationCode, ResourceClientFrame,
    ResourceKind as ProtocolResourceKind, ResourceProtocolConnection, ResourceRequest,
    ResourceServerFrame, ResourceWindowUpdate, ServerFrame,
    decode_constructed_invocation_event_frame, decode_constructed_server_frame,
    decode_resource_server_frame, encode_constructed_client_frame, encode_constructed_value,
    encode_invoke_request, encode_resource_client_frame,
};
use orna_standard::{
    STD_IO_BYTE_STREAM_TYPE_ID, STD_TERMINAL_DOCUMENT_TYPE_ID, registered_opaque_codecs,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender};

use crate::{
    EmbeddedHostError, LocalRawSocketResources, inspect_ready_embedded_host,
    raw_socket::serve_local_raw_stream_with_broker,
};

const CONSTRUCTED_CLIENT_HELLO: [u8; 12] = *b"ORNA\x01\x00\x00\x05\x00\x00\x00\x00";
const CONSTRUCTED_SERVER_ACK: [u8; 12] = *b"ORNA\x81\x00\x00\x05\x00\x00\x00\x00";
const RESOURCE_MARKER: &[u8; 15] = b"ORNA-RESOURCE/1";
const RESOURCE_HEADER_LENGTH: usize = 21;
const RESOURCE_FRAME_TIMEOUT: Duration = Duration::from_secs(30);
const BROKER_RESOURCE_COMPLETION_CAPACITY: usize = 2;
/// Retain only a bounded recent history of terminal resource stream identities.
///
/// Resource stream IDs are monotonically allocated, so evicting the oldest
/// tombstone cannot make a newer stream ID ambiguous with a reused one.
const BROKER_RESOURCE_TOMBSTONE_CAPACITY: usize = 64;

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

struct PersistentResourceTransport {
    stream: Option<StandardUnixStream>,
    handshake_complete: bool,
    protocol: ResourceProtocolConnection,
    server_task: Option<thread::JoinHandle<()>>,
}
struct AuthenticatedResourceTransport {
    kernel: PostgresKernel,
    session: AuthenticatedSession,
    transport: PersistentResourceTransport,
}


enum ResourceTransportSource {
    Authenticated(AuthenticatedResourceTransport),
    Injected(InjectedResourceTransport),
}

#[derive(Clone)]
pub(crate) struct SharedInvokeBroker {
    commands: UnboundedSender<BrokerCommand>,
    task: std::sync::Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    resource_expectations: BrokerResourceExpectations,
}

/// Test-only authorisation state for manually driven installed resource
/// sockets.
///
/// The installed host normally creates this state internally while it
/// drives a sealed invocation. Direct protocol tests must register the
/// exact resource requests that the client evaluator would have produced.
#[doc(hidden)]
#[derive(Clone)]
pub struct RawResourceRequestAuthorizer {
    broker: SharedInvokeBroker,
}

impl RawResourceRequestAuthorizer {
    /// Creates an empty resource request authoriser.
    #[doc(hidden)]
    pub fn new() -> Self {
        let (broker, _receiver) = SharedInvokeBroker::pending();
        Self { broker }
    }

    /// Registers one exact resource request for the raw socket boundary.
    #[doc(hidden)]
    pub fn expect(&self, request: &ResourceRequest) -> bool {
        self.broker.register_expected_resource_request(request)
    }

    pub(crate) fn broker(&self) -> SharedInvokeBroker {
        self.broker.clone()
    }
}

impl Default for RawResourceRequestAuthorizer {
    fn default() -> Self {
        Self::new()
    }
}


enum BrokerCommand {
    StartRoot {
        request: orna_protocol::RetainedInvokeRequest,
        response:
            tokio::sync::oneshot::Sender<Result<SealedInvocationResult, ResourceTransportFailure>>,
    },
    StartResource {
        request: ResourceRequest,
        expected_type: ResolvedType,
        resource_kind: ProtocolResourceKind,
        completion: Sender<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
    },
    CancelResource {
        stream_id: u64,
        request_id: orna_core::InvocationId,
        reason: ResourceCancellationCode,
    },
    Shutdown,
}

struct BrokerRootState {
    invocation: Option<orna_core::InvocationId>,
    records: Vec<InvocationEventRecord>,
    response:
        tokio::sync::oneshot::Sender<Result<SealedInvocationResult, ResourceTransportFailure>>,
}

struct BrokerResourceState {
    request: ResourceRequest,
    expected_type: ResolvedType,
    resource_kind: ProtocolResourceKind,
    protocol: ResourceProtocolConnection,
    completion: Sender<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
    accepted: bool,
    scalar_value: Option<RuntimeValue>,
    cancellation_requested: bool,
    stream_values_seen: bool,
}

type BrokerResourceTombstones = BTreeMap<u64, orna_core::InvocationId>;
type BrokerResourceExpectations =
    std::sync::Arc<std::sync::Mutex<BTreeMap<u64, ResourceRequest>>>;

const BROKER_RESOURCE_EXPECTATION_LOCK: &str = "broker resource expectation lock";

impl SharedInvokeBroker {
    pub(crate) fn register_expected_resource_request(&self, request: &ResourceRequest) -> bool {
        let mut expectations = self
            .resource_expectations
            .lock()
            .expect(BROKER_RESOURCE_EXPECTATION_LOCK);
        if expectations.contains_key(&request.stream_id) {
            return false;
        }
        expectations.insert(request.stream_id, request.clone());
        true
    }

    pub(crate) fn take_expected_resource_request(
        &self,
        request: &ResourceRequest,
    ) -> bool {
        let mut expectations = self
            .resource_expectations
            .lock()
            .expect(BROKER_RESOURCE_EXPECTATION_LOCK);
        if !expectations
            .get(&request.stream_id)
            .is_some_and(|expected| expected == request)
        {
            return false;
        }
        expectations.remove(&request.stream_id);
        true
    }

    fn discard_expected_resource_request(&self, stream_id: u64) {
        self.resource_expectations
            .lock()
            .expect(BROKER_RESOURCE_EXPECTATION_LOCK)
            .remove(&stream_id);
    }

    fn clear_resource_expectations(&self) {
        self.resource_expectations
            .lock()
            .expect(BROKER_RESOURCE_EXPECTATION_LOCK)
            .clear();
    }
}


enum InjectedResourceTransport {
    /// A compatibility stream used by focused transport tests.
    Stream(PersistentResourceTransport),
}

struct PendingResourceTransport {
    request: ClientResourceRequest,
    receiver: Receiver<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
    control: UnboundedSender<ResourceTransportControl>,
    transport_return: std::sync::Arc<std::sync::Mutex<Option<ResourceTransportSource>>>,
    worker: thread::JoinHandle<()>,
    cancel_requested: bool,
}

struct PendingBrokerResource {
    request: ClientResourceRequest,
    receiver: Receiver<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
    control: SharedInvokeBroker,
    stream_id: u64,
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

impl PersistentResourceTransport {
    fn empty() -> Self {
        Self {
            stream: None,
            handshake_complete: false,
            protocol: ResourceProtocolConnection::new(),
            server_task: None,
        }
    }
}

impl ResourceTransportSource {
    fn persistent(&mut self) -> &mut PersistentResourceTransport {
        match self {
            Self::Injected(InjectedResourceTransport::Stream(transport)) => transport,
            Self::Authenticated(transport) => &mut transport.transport,
        }
    }

    fn take_connection(
        &mut self,
    ) -> Result<(StandardUnixStream, bool, ResourceProtocolConnection), ()> {
        if self.persistent().stream.is_none() {
            match self {
                Self::Authenticated(_) => return Err(()),
                Self::Injected(InjectedResourceTransport::Stream(transport)) => {
                    if transport.stream.is_none() {
                        return Err(());
                    }
                }
            }
        }
        let transport = self.persistent();
        let stream = transport.stream.take().ok_or(())?;
        let handshake_complete = transport.handshake_complete;
        let protocol = std::mem::take(&mut transport.protocol);
        Ok((stream, handshake_complete, protocol))
    }

    fn restore_connection(
        &mut self,
        stream: StandardUnixStream,
        handshake_complete: bool,
        protocol: ResourceProtocolConnection,
    ) {
        let transport = self.persistent();
        transport.stream = Some(stream);
        transport.handshake_complete = handshake_complete;
        transport.protocol = protocol;
    }

    fn reset(&mut self) {
        let transport = self.persistent();
        transport.stream.take();
        transport.handshake_complete = false;
        transport.protocol = ResourceProtocolConnection::new();
        if let Some(server_task) = transport.server_task.take() {
            let _ = server_task.join();
        }
    }
}
impl Drop for PersistentResourceTransport {
    fn drop(&mut self) {
        self.stream.take();
        if let Some(server_task) = self.server_task.take() {
            let _ = server_task.join();
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
    broker: Option<SharedInvokeBroker>,
    transport: Option<ResourceTransportSource>,
    pending: Option<PendingResourceTransport>,
    broker_pending: Option<PendingBrokerResource>,
    detached: Vec<DetachedResourceTransport>,
    cancellation: ResourceCancellation,
}

impl InstalledClientResourceExecutor {
    /// Creates an executor for one authenticated installed database revision.
    pub fn new(
        kernel: PostgresKernel,
        session: AuthenticatedSession,
        active: ActiveDatabaseRevision,
    ) -> Self {
        Self {
            active,
            next_stream_id: 1,
            broker: None,
            broker_pending: None,
            transport: Some(ResourceTransportSource::Authenticated(
                AuthenticatedResourceTransport {
                    kernel,
                    session,
                    transport: PersistentResourceTransport::empty(),
                },
            )),
            pending: None,
            detached: Vec::new(),
            cancellation: ResourceCancellation::new(),
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
            broker: None,
            broker_pending: None,
            transport: Some(ResourceTransportSource::Injected(
                InjectedResourceTransport::Stream(PersistentResourceTransport {
                    stream: Some(stream),
                    handshake_complete: false,
                    protocol: ResourceProtocolConnection::new(),
                    server_task: None,
                }),
            )),
            pending: None,
            detached: Vec::new(),
            cancellation: ResourceCancellation::new(),
        }
    }

    #[doc(hidden)]
    pub(crate) fn new_with_broker(
        active: ActiveDatabaseRevision,
        broker: SharedInvokeBroker,
        cancellation: ResourceCancellation,
    ) -> Self {
        Self {
            active,
            next_stream_id: 1,
            broker: Some(broker),
            transport: None,
            pending: None,
            broker_pending: None,
            detached: Vec::new(),
            cancellation,
        }
    }
    fn poll_broker(&mut self) -> Option<ClientResourceCompletion> {
        if self.cancellation.is_requested() {
            if let Some(pending) = self.broker_pending.as_mut()
                && !pending.cancel_requested
            {
                let _ = pending.control.commands.send(BrokerCommand::CancelResource {
                    stream_id: pending.stream_id,
                    request_id: pending.request.request_id(),
                    reason: ResourceCancellationCode::ParentInvocationCancelled,
                });
                pending.cancel_requested = true;
            }
        }
        let pending = self.broker_pending.as_mut()?;
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(ResourceTransportFailure::Transport),
        };
        let stream_values = matches!(&result, Ok(ResourceTransportOutcome::StreamValues(_)));
        if stream_values && pending.cancel_requested {
            return self.poll_broker();
        }
        if stream_values {
            return Some(map_resource_transport_completion(
                pending.request.clone(),
                result,
            ));
        }
        let pending = self
            .broker_pending
            .take()
            .expect("broker pending checked above");
        Some(map_resource_transport_completion(pending.request, result))
    }
}

impl ClientResourceExecutor for InstalledClientResourceExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        if self.pending.is_some() || self.broker_pending.is_some() {
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
        let (resource_kind, return_type) =
            match target_definition.map(FunctionDefinition::return_type) {
                Some(FunctionReturn::Single(return_type)) => {
                    (ProtocolResourceKind::Single, *return_type)
                }
                Some(FunctionReturn::Stream(return_type)) => {
                    (ProtocolResourceKind::Stream, *return_type)
                }
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
            state_profile: invocation_context.state_profile().to_owned(),
            function_instance_key: invocation_context.function_instance_key().to_owned(),
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
        if let Some(broker) = self.broker.clone() {
            if !broker.register_expected_resource_request(&protocol_request) {
                return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
            }
            let (completion, receiver) = mpsc::channel(BROKER_RESOURCE_COMPLETION_CAPACITY);
            if broker
                .commands
                .send(BrokerCommand::StartResource {
                    request: protocol_request,
                    expected_type: request.expected_type(),
                    resource_kind,
                    completion,
                })
                .is_err()
            {
                broker.discard_expected_resource_request(stream_id);
                return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
            }
            let pending = ClientResourceCompletion::Pending {
                request_id: request.request_id(),
                key: request.key(),
                generation: request.generation(),
            };
            self.broker_pending = Some(PendingBrokerResource {
                stream_id,
                request,
                receiver,
                control: broker,
                cancel_requested: false,
            });
            return pending;
        }
        let Some(worker_transport) = self.transport.take() else {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        };
        let (sender, receiver) = mpsc::channel(1);
        let (control, control_receiver) = mpsc::unbounded_channel();
        let (worker_transport_sender, worker_transport_receiver) =
            std::sync::mpsc::sync_channel::<ResourceTransportSource>(1);
        let transport_return = std::sync::Arc::new(std::sync::Mutex::new(None));
        let worker_transport_return = std::sync::Arc::clone(&transport_return);
        let active = self.active.clone();
        let expected_type = request.expected_type();
        let worker = thread::Builder::new()
            .name("orna-resource-transport".to_owned())
            .spawn(move || {
                let Ok(mut worker_transport) = worker_transport_receiver.recv() else {
                    let _ = sender.blocking_send(Err(ResourceTransportFailure::Transport));
                    return;
                };
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
                    match &worker_transport {
                        ResourceTransportSource::Authenticated(transport) => runtime
                            .block_on(run_authenticated_resource_transport(
                                transport.kernel.clone(),
                                transport.session.clone(),
                                active,
                                registry,
                                protocol_request,
                                expected_type,
                                resource_kind,
                                control_receiver,
                                &sender,
                            )),
                        _ => {
                            let (stream, handshake_complete, protocol) = worker_transport
                                .take_connection()
                                .map_err(|_| ResourceTransportFailure::Transport)?;
                            let run = runtime.block_on(run_resource_transport(
                                stream,
                                handshake_complete,
                                protocol,
                                active,
                                registry,
                                protocol_request,
                                expected_type,
                                resource_kind,
                                control_receiver,
                                &sender,
                            ))?;
                            let stream = run
                                .stream
                                .into_std()
                                .map_err(|_| ResourceTransportFailure::Transport)?;
                            worker_transport.restore_connection(stream, true, run.protocol);
                            Ok(run.outcome)
                        }
                    }
                })();
                if outcome.is_err() {
                    worker_transport.reset();
                }
                let _ = worker_transport_return
                    .lock()
                    .expect("resource transport return lock")
                    .replace(worker_transport);
                let _ = sender.blocking_send(outcome);
            });
        let Ok(worker) = worker else {
            self.transport = Some(worker_transport);
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        };
        if let Err(error) = worker_transport_sender.send(worker_transport) {
            let _ = worker.join();
            self.transport = Some(error.0);
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        }
        let pending = ClientResourceCompletion::Pending {
            request_id: request.request_id(),
            key: request.key(),
            generation: request.generation(),
        };
        self.pending = Some(PendingResourceTransport {
            request,
            receiver,
            control,
            transport_return,
            worker,
            cancel_requested: false,
        });
        pending
    }

    fn poll(&mut self) -> Option<ClientResourceCompletion> {
        if self.broker_pending.is_some() {
            return self.poll_broker();
        }
        if self.cancellation.is_requested() {
            if let Some(pending) = self.pending.as_mut()
                && !pending.cancel_requested
            {
                let _ = pending.control.send(ResourceTransportControl::Cancel(
                    ResourceCancellationCode::ParentInvocationCancelled,
                ));
                pending.cancel_requested = true;
            }
        }
        loop {
            let (result, cancel_requested, request) = {
                let pending = self.pending.as_mut()?;
                let result = match pending.receiver.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => return None,
                    Err(TryRecvError::Disconnected) => Err(ResourceTransportFailure::Transport),
                };
                (result, pending.cancel_requested, pending.request.clone())
            };
            let stream_values = matches!(&result, Ok(ResourceTransportOutcome::StreamValues(_)));
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
            self.transport = pending
                .transport_return
                .lock()
                .expect("resource transport return lock")
                .take();
            return Some(map_resource_transport_completion(pending.request, result));
        }
    }

    fn cancel_pending(&mut self) -> Option<ClientResourceCompletion> {
        if self.broker_pending.is_some() {
            return self.poll_broker();
        }
        if self.pending.is_some() {
            // The direct transport has its own cancellation state because it
            // is not connected to the invocation's broker cancellation token.
            // Request it here, then let `poll` send the protocol cancel once.
            let _ = self.cancellation.request_cancel();
            return self.poll();
        }
        None
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        if let Some(mut pending) = self.broker_pending.take() {
            if pending.request.request_id() != request.request_id()
                || pending.request.key() != request.key()
                || pending.request.generation() != request.generation()
            {
                self.broker_pending = Some(pending);
                return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
            }
            if pending.cancel_requested {
                self.broker_pending = Some(pending);
                return request.pending();
            }
            match pending.receiver.try_recv() {
                Ok(Ok(ResourceTransportOutcome::StreamValues(_))) => {}
                Ok(result @ Ok(_)) | Ok(result @ Err(_)) => {
                    return map_resource_transport_completion(pending.request, result);
                }
                Err(TryRecvError::Disconnected) => {
                    return map_resource_transport_completion(
                        pending.request,
                        Err(ResourceTransportFailure::Transport),
                    );
                }
                Err(TryRecvError::Empty) => {}
            }
            let _ = pending
                .control
                .commands
                .send(BrokerCommand::CancelResource {
                    stream_id: pending.stream_id,
                    request_id: pending.request.request_id(),
                    reason: ResourceCancellationCode::ClientRequested,
                });
            pending.cancel_requested = true;
            self.broker_pending = Some(pending);
            return request.pending();
        }
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
                    self.transport = pending
                        .transport_return
                        .lock()
                        .expect("resource transport return lock")
                        .take();
                    return map_resource_transport_completion(pending.request, result);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = pending.worker.join();
                    self.transport = pending
                        .transport_return
                        .lock()
                        .expect("resource transport return lock")
                        .take();
                    return map_resource_transport_completion(
                        pending.request,
                        Err(ResourceTransportFailure::Transport),
                    );
                }
            }
        }
        pending.cancel_requested = true;
        let _ = pending.control.send(ResourceTransportControl::Cancel(
            ResourceCancellationCode::ClientRequested,
        ));
        self.pending = Some(pending);
        request.pending()
    }
}
impl Drop for InstalledClientResourceExecutor {
    fn drop(&mut self) {
        if let Some(pending) = self.broker_pending.take() {
            let _ = pending
                .control
                .commands
                .send(BrokerCommand::CancelResource {
                    stream_id: pending.stream_id,
                    request_id: pending.request.request_id(),
                    reason: ResourceCancellationCode::RuntimeShutdown,
                });
        }
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
        Err(ResourceTransportFailure::Cancelled) => request.cancelled(),
        Err(ResourceTransportFailure::RootSealedDispatchInternal) => {
            request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned())
        }
        Err(ResourceTransportFailure::Transport) => {
            request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned())
        }
    }
}

#[derive(Debug)]
enum ResourceTransportOutcome {
    Ready(RuntimeValue),
    StreamValues(Vec<RuntimeValue>),
    StreamCompleted,
    Failed { failure: CallFailure },
    Cancelled,
}

#[derive(Debug)]
enum ResourceTransportFailure {
    Transport,
    Shape,
    Cancelled,
    RootSealedDispatchInternal,
}

struct ResourceTransportRun {
    stream: tokio::net::UnixStream,
    protocol: ResourceProtocolConnection,
    outcome: ResourceTransportOutcome,
}

enum ResourceFrameResult {
    Continue,
    Completed,
    Failed(CallFailure),
    Cancelled,
}

impl SharedInvokeBroker {
    fn pending() -> (Self, UnboundedReceiver<BrokerCommand>) {
        let (commands, receiver) = mpsc::unbounded_channel();
        (
            Self {
                commands,
                task: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
                resource_expectations: std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            },
            receiver,
        )
    }


    async fn activate(
        &self,
        stream: StandardUnixStream,
        active: ActiveDatabaseRevision,
        registry: OpaqueCodecRegistry,
        receiver: UnboundedReceiver<BrokerCommand>,
    ) -> Result<(), ResourceTransportFailure> {
        stream
            .set_nonblocking(true)
            .map_err(|_| ResourceTransportFailure::Transport)?;
        let mut stream = tokio::net::UnixStream::from_std(stream)
            .map_err(|_| ResourceTransportFailure::Transport)?;
        tokio::time::timeout(
            RESOURCE_FRAME_TIMEOUT,
            stream.write_all(&CONSTRUCTED_CLIENT_HELLO),
        )
        .await
        .map_err(|_| ResourceTransportFailure::Transport)?
        .map_err(|_| ResourceTransportFailure::Transport)?;
        let mut acknowledgement = [0_u8; CONSTRUCTED_SERVER_ACK.len()];
        tokio::time::timeout(
            RESOURCE_FRAME_TIMEOUT,
            stream.read_exact(&mut acknowledgement),
        )
        .await
        .map_err(|_| ResourceTransportFailure::Transport)?
        .map_err(|_| ResourceTransportFailure::Transport)?;
        if acknowledgement != CONSTRUCTED_SERVER_ACK {
            return Err(ResourceTransportFailure::Transport);
        }
        let task = tokio::spawn(run_shared_invoke_broker(stream, active, registry, receiver));
        *self.task.lock().await = Some(task);
        Ok(())
    }

    async fn shutdown(&self) {
        let _ = self.commands.send(BrokerCommand::Shutdown);
        let task = self.task.lock().await.take();
        if let Some(task) = task {
            let _ = task.await;
        }
        self.clear_resource_expectations();
    }

    async fn invoke(
        &self,
        request: orna_protocol::RetainedInvokeRequest,
    ) -> Result<SealedInvocationResult, ResourceTransportFailure> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.commands
            .send(BrokerCommand::StartRoot {
                request,
                response: sender,
            })
            .map_err(|_| ResourceTransportFailure::Transport)?;
        receiver
            .await
            .map_err(|_| ResourceTransportFailure::Transport)?
    }
}

struct BrokerWireFrame {
    resource: bool,
    bytes: Vec<u8>,
}

async fn read_shared_broker_frame<R>(
    stream: &mut R,
) -> Result<BrokerWireFrame, ResourceTransportFailure>
where
    R: AsyncRead + Unpin,
{
    let mut prefix = [0_u8; RESOURCE_MARKER.len()];
    tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, stream.read_exact(&mut prefix))
        .await
        .map_err(|_| ResourceTransportFailure::Transport)?
        .map_err(|_| ResourceTransportFailure::Transport)?;
    let resource = &prefix == RESOURCE_MARKER;
    let header_length = if resource { RESOURCE_HEADER_LENGTH } else { 18 };
    let mut header = prefix.to_vec();
    header.resize(header_length, 0);
    tokio::time::timeout(
        RESOURCE_FRAME_TIMEOUT,
        stream.read_exact(&mut header[RESOURCE_MARKER.len()..]),
    )
    .await
    .map_err(|_| ResourceTransportFailure::Transport)?
    .map_err(|_| ResourceTransportFailure::Transport)?;
    let declared_offset = if resource { 17..21 } else { 14..18 };
    let payload_length = u32::from_be_bytes(
        header[declared_offset]
            .try_into()
            .expect("shared broker frame header has a fixed length"),
    ) as usize;
    if payload_length > MAX_FRAME_PAYLOAD_LENGTH {
        return Err(ResourceTransportFailure::Shape);
    }
    let mut bytes = header;
    bytes.resize(header_length + payload_length, 0);
    tokio::time::timeout(
        RESOURCE_FRAME_TIMEOUT,
        stream.read_exact(&mut bytes[header_length..]),
    )
    .await
    .map_err(|_| ResourceTransportFailure::Transport)?
    .map_err(|_| ResourceTransportFailure::Transport)?;
    Ok(BrokerWireFrame { resource, bytes })
}

async fn read_shared_broker_frames<R>(
    mut stream: R,
    sender: Sender<Result<BrokerWireFrame, ResourceTransportFailure>>,
) where
    R: AsyncRead + Unpin,
{
    loop {
        let result = read_shared_broker_frame(&mut stream).await;
        let failed = result.is_err();
        if sender.send(result).await.is_err() || failed {
            return;
        }
    }
}

async fn write_shared_broker_frame<W>(
    stream: &mut W,
    bytes: &[u8],
) -> Result<(), ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, stream.write_all(bytes))
        .await
        .map_err(|_| ResourceTransportFailure::Transport)?
        .map_err(|_| ResourceTransportFailure::Transport)
}

async fn run_shared_invoke_broker(
    stream: tokio::net::UnixStream,
    active: ActiveDatabaseRevision,
    registry: OpaqueCodecRegistry,
    mut commands: UnboundedReceiver<BrokerCommand>,
) {
    let (reader, mut stream) = stream.into_split();
    let (frame_sender, mut frames) = mpsc::channel(1);
    let reader_task = tokio::spawn(read_shared_broker_frames(reader, frame_sender));
    let mut connection = ProtocolConnection::new();
    let mut root: Option<BrokerRootState> = None;
    let mut resources: BTreeMap<u64, BrokerResourceState> = BTreeMap::new();
    let mut resource_tombstones = BrokerResourceTombstones::new();
    let mut resource_high_water_mark = None;
    loop {
        enum BrokerNext {
            Command(Option<BrokerCommand>),
            Frame(Option<Result<BrokerWireFrame, ResourceTransportFailure>>),
        }
        let next = tokio::select! {
            command = commands.recv() => BrokerNext::Command(command),
            frame = frames.recv() => BrokerNext::Frame(frame),
        };
        match next {
            BrokerNext::Command(Some(command)) => {
                if handle_shared_broker_command(
                    command,
                    &mut stream,
                    &active,
                    &registry,
                    &mut connection,
                    &mut root,
                    &mut resources,
                    &mut resource_high_water_mark,
                )
                .await
                .is_err()
                {
                    break;
                }
            }
            BrokerNext::Command(None) => break,
            BrokerNext::Frame(Some(Ok(frame))) => {
                if handle_shared_broker_frame(
                    frame,
                    &mut stream,
                    &active,
                    &registry,
                    &mut root,
                    &mut resources,
                    &mut resource_tombstones,
                )
                .await
                .is_err()
                {
                    break;
                }
            }
            BrokerNext::Frame(Some(Err(_))) | BrokerNext::Frame(None) => break,
        }
    }
    reader_task.abort();
    let _ = reader_task.await;
    if let Some(root) = root.take() {
        let _ = root.response.send(Err(ResourceTransportFailure::Transport));
    }
    for (_, resource) in resources {
        let _ = resource
            .completion
            .send(Err(ResourceTransportFailure::Transport))
            .await;
    }
}

async fn handle_shared_broker_command<W>(
    command: BrokerCommand,
    stream: &mut W,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    connection: &mut ProtocolConnection,
    root: &mut Option<BrokerRootState>,
    resources: &mut BTreeMap<u64, BrokerResourceState>,
    resource_high_water_mark: &mut Option<u64>,
) -> Result<(), ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    match command {
        BrokerCommand::StartRoot { request, response } => {
            if root.is_some() {
                let _ = response.send(Err(ResourceTransportFailure::Transport));
                return Ok(());
            }
            let frames = [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function: SYS_INVOKE_FUNCTION_ID,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: MAX_CHANNEL_WINDOW,
                },
                ClientFrame::CallInvokeRequest { stream: 1, request },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ];
            for frame in frames {
                connection
                    .receive_constructed(active, registry, frame.clone())
                    .map_err(|_| ResourceTransportFailure::Shape)?;
                let encoded = encode_constructed_client_frame(active, registry, &frame)
                    .map_err(|_| ResourceTransportFailure::Shape)?;
                write_shared_broker_frame(stream, &encoded).await?;
            }
            *root = Some(BrokerRootState {
                invocation: None,
                records: Vec::new(),
                response,
            });
        }
        BrokerCommand::StartResource {
            request,
            expected_type,
            resource_kind,
            completion,
        } => {
            let stream_id = request.stream_id;
            if resource_high_water_mark.is_some_and(|previous| stream_id <= previous) {
                return Err(ResourceTransportFailure::Shape);
            }
            let mut protocol = ResourceProtocolConnection::new();
            protocol
                .open(request.clone())
                .map_err(|_| ResourceTransportFailure::Shape)?;
            let encoded = encode_resource_client_frame(
                active,
                registry,
                &ResourceClientFrame::Request(request.clone()),
            )
            .map_err(|_| ResourceTransportFailure::Shape)?;
            resources.insert(
                stream_id,
                BrokerResourceState {
                    request,
                    expected_type,
                    resource_kind,
                    protocol,
                    completion,
                    accepted: false,
                    scalar_value: None,
                    cancellation_requested: false,
                    stream_values_seen: false,
                },
            );
            *resource_high_water_mark = Some(stream_id);
            write_shared_broker_frame(stream, &encoded).await?;
        }
        BrokerCommand::CancelResource {
            stream_id,
            request_id,
            reason,
        } => {
            let Some(state) = resources.get_mut(&stream_id) else {
                return Ok(());
            };
            let cancel = ResourceClientFrame::Cancel(ResourceCancel {
                stream_id,
                request_id,
                reason,
            });
            state
                .protocol
                .receive(cancel.clone())
                .map_err(|_| ResourceTransportFailure::Shape)?;
            state.cancellation_requested = true;
            let encoded = encode_resource_client_frame(active, registry, &cancel)
                .map_err(|_| ResourceTransportFailure::Shape)?;
            write_shared_broker_frame(stream, &encoded).await?;
        }
        BrokerCommand::Shutdown => {
            if root.is_some() {
                let cancel = ClientFrame::CallCancel { stream: 1 };
                let _ = connection.receive_constructed(active, registry, cancel.clone());
                if let Ok(encoded) = encode_constructed_client_frame(active, registry, &cancel) {
                    let _ = write_shared_broker_frame(stream, &encoded).await;
                }
            }
            for state in resources.values_mut() {
                if state.cancellation_requested {
                    continue;
                }
                let cancel = ResourceClientFrame::Cancel(ResourceCancel {
                    stream_id: state.request.stream_id,
                    request_id: state.request.request_id,
                    reason: ResourceCancellationCode::RuntimeShutdown,
                });
                let _ = state.protocol.receive(cancel.clone());
                if let Ok(encoded) = encode_resource_client_frame(active, registry, &cancel) {
                    let _ = write_shared_broker_frame(stream, &encoded).await;
                }
            }
            return Err(ResourceTransportFailure::Transport);
        }
    }
    Ok(())
}

fn resource_server_frame_identity(frame: &ResourceServerFrame) -> (u64, orna_core::InvocationId) {
    match frame {
        ResourceServerFrame::Accepted(value) => (value.stream_id, value.request_id),
        ResourceServerFrame::Values(value) => (value.stream_id, value.request_id),
        ResourceServerFrame::Completed(value) => (value.stream_id, value.request_id),
        ResourceServerFrame::Failed(value) => (value.stream_id, value.request_id),
        ResourceServerFrame::Cancelled(value) => (value.stream_id, value.request_id),
    }
}

fn remember_broker_resource_terminal(
    tombstones: &mut BrokerResourceTombstones,
    stream_id: u64,
    request_id: orna_core::InvocationId,
) {
    tombstones.insert(stream_id, request_id);
    while tombstones.len() > BROKER_RESOURCE_TOMBSTONE_CAPACITY {
        let Some(stream_id) = tombstones.keys().next().copied() else {
            break;
        };
        tombstones.remove(&stream_id);
    }
}

async fn handle_shared_broker_frame<W>(
    frame: BrokerWireFrame,
    stream: &mut W,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    root: &mut Option<BrokerRootState>,
    resources: &mut BTreeMap<u64, BrokerResourceState>,
    resource_tombstones: &mut BrokerResourceTombstones,
) -> Result<(), ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    if frame.resource {
        let decoded = decode_resource_server_frame(active, registry, &frame.bytes)
            .map_err(|_| ResourceTransportFailure::Shape)?;
        let (stream_id, request_id) = resource_server_frame_identity(&decoded);
        if let Some(expected_request_id) = resource_tombstones.get(&stream_id) {
            if *expected_request_id != request_id {
                return Err(ResourceTransportFailure::Shape);
            }
            // The broker has already published this stream terminal outcome.
            // Keep the connection alive for the root call and every other resource.
            return Ok(());
        }
        let Some(mut state) = resources.remove(&stream_id) else {
            return Err(ResourceTransportFailure::Shape);
        };
        let keep =
            handle_shared_resource_frame(&mut state, decoded, stream, active, registry).await?;
        if keep {
            resources.insert(stream_id, state);
        } else {
            remember_broker_resource_terminal(
                resource_tombstones,
                stream_id,
                state.request.request_id,
            );
        }
        return Ok(());
    }
    let decoded = decode_constructed_invocation_event_frame(active, registry, &frame.bytes)
        .or_else(|_| decode_constructed_server_frame(active, registry, &frame.bytes))
        .map_err(|_| ResourceTransportFailure::Shape)?;
    let Some(state) = root.as_mut() else {
        return Err(ResourceTransportFailure::Shape);
    };
    match decoded {
        ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        } => state.invocation = Some(invocation),
        ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events,
        } => {
            for record in events {
                let Event::Value(RuntimeValue::InvokeEvent(event)) = record.event else {
                    return Err(ResourceTransportFailure::Shape);
                };
                if state
                    .invocation
                    .is_some_and(|invocation| event.invocation_id() != invocation)
                {
                    return Err(ResourceTransportFailure::Shape);
                }
                if state.records.is_empty() && (record.sequence != 1 || event.sequence() != 0) {
                    return Err(ResourceTransportFailure::Shape);
                }
                if state.records.last().is_some_and(|last| {
                    last.outer_sequence().checked_add(1) != Some(record.sequence)
                        || last.event().sequence().checked_add(1) != Some(event.sequence())
                }) {
                    return Err(ResourceTransportFailure::Shape);
                }
                state
                    .records
                    .push(InvocationEventRecord::new(record.sequence, event));
            }
        }
        ServerFrame::CallCompleted { stream: 1 } => {
            let state = root.take().expect("root state checked above");
            let invocation = state
                .invocation
                .unwrap_or_else(orna_core::InvocationId::new);
            if state.records.is_empty() {
                let _ = state
                    .response
                    .send(Ok(SealedInvocationResult::Denied { invocation }));
            } else {
                match reconstruct_shared_root_result(invocation, state.records) {
                    Ok(result) => {
                        let _ = state.response.send(Ok(result));
                    }
                    Err(ResourceTransportFailure::Cancelled) => {
                        let _ = state
                            .response
                            .send(Err(ResourceTransportFailure::Cancelled));
                    }
                    Err(error) => {
                        let _ = state.response.send(Err(error));
                        return Err(ResourceTransportFailure::Shape);
                    }
                }
            }
        }
        ServerFrame::CallFailed { stream: 1, failure } => {
            let state = root.take().expect("root state checked above");
            let invocation = state
                .invocation
                .unwrap_or_else(orna_core::InvocationId::new);
            let result = if failure == CallFailure::ExecuteDenied {
                Ok(SealedInvocationResult::Denied { invocation })
            } else {
                Err(ResourceTransportFailure::Transport)
            };
            let _ = state.response.send(result);
        }
        ServerFrame::CallCancelled { stream: 1 } => {
            let state = root.take().expect("root state checked above");
            let _ = state
                .response
                .send(Err(ResourceTransportFailure::Cancelled));
        }
        _ => return Err(ResourceTransportFailure::Shape),
    }
    Ok(())
}

fn reconstruct_shared_root_result(
    invocation: orna_core::InvocationId,
    records: Vec<InvocationEventRecord>,
) -> Result<SealedInvocationResult, ResourceTransportFailure> {
    let events = orna_protocol::InvocationEventBatch::new(records)
        .map_err(|_| ResourceTransportFailure::Shape)?;
    let Some(last) = events.records().last() else {
        return Err(ResourceTransportFailure::Shape);
    };
    match last.event().body() {
        InvocationEventBody::Failed(failure) if failure.code() == "INVOKE_DENIED" => {
            Ok(SealedInvocationResult::Denied { invocation })
        }
        InvocationEventBody::Failed(failure) if failure.code() == "INVOKE_PRESENTATION_FAILURE" => {
            Ok(SealedInvocationResult::PresentationFailed { invocation })
        }
        InvocationEventBody::Failed(failure) if failure.code() == "INVOKE_INTERNAL_FAILURE" => {
            Err(ResourceTransportFailure::RootSealedDispatchInternal)
        }
        InvocationEventBody::Failed(_) => Ok(SealedInvocationResult::Failed { invocation, events }),
        InvocationEventBody::Completed { .. } => {
            Ok(SealedInvocationResult::Completed { invocation, events })
        }
        InvocationEventBody::Cancelled { .. } => Err(ResourceTransportFailure::Cancelled),
        _ => Err(ResourceTransportFailure::Shape),
    }
}

/// Publishes a terminal resource result without turning an allowed
/// cancellation race into a shared broker failure.
///
/// A cancelled resource may outlive the evaluator that owns its completion
/// receiver. In that case a committed terminal frame is still a local stream
/// completion, not a transport failure for the root invocation. A live
/// resource with a closed receiver remains a genuine broker transport error.
async fn send_shared_resource_terminal(
    state: &BrokerResourceState,
    outcome: Result<ResourceTransportOutcome, ResourceTransportFailure>,
) -> Result<(), ResourceTransportFailure> {
    match state.completion.send(outcome).await {
        Ok(()) => Ok(()),
        Err(_) if state.cancellation_requested => Ok(()),
        Err(_) => Err(ResourceTransportFailure::Transport),
    }
}

async fn handle_shared_resource_frame<W>(
    state: &mut BrokerResourceState,
    frame: ResourceServerFrame,
    stream: &mut W,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<bool, ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    let disposition = if state.cancellation_requested {
        if let ResourceServerFrame::Cancelled(cancelled) = &frame {
            state
                .protocol
                .apply_cancelled_after_client_cancel(*cancelled)
                .map_err(|_| ResourceTransportFailure::Shape)?
        } else {
            state
                .protocol
                .apply_constructed(active, registry, frame.clone())
                .map_err(|_| ResourceTransportFailure::Shape)?
        }
    } else {
        state
            .protocol
            .apply_constructed(active, registry, frame.clone())
            .map_err(|_| ResourceTransportFailure::Shape)?
    };
    // A terminal frame marked DroppedLate can still be the committed server
    // result: cancellation closed the client-side protocol before that result
    // reached this broker. Drain late non-terminals, but publish this terminal
    // when its receiver is still live before removing the broker state.
    let late_terminal = state.cancellation_requested
        && matches!(disposition, orna_protocol::ResourceFrameDisposition::DroppedLate)
        && matches!(
            &frame,
            ResourceServerFrame::Completed(_)
                | ResourceServerFrame::Failed(_)
                | ResourceServerFrame::Cancelled(_)
        );
    match frame {
        ResourceServerFrame::Accepted(value) => {
            if value.request_id != state.request.request_id
                || value.target_revision != state.request.target_revision
                || value.resource_kind != state.resource_kind
            {
                return Err(ResourceTransportFailure::Shape);
            }
            state.accepted = true;
        }
        ResourceServerFrame::Values(value) => {
            if value.request_id != state.request.request_id
                || value.values.is_empty()
                || value.item_count == 0
                || value.item_count as usize != value.values.len()
                || value.byte_count == 0
                || (matches!(state.resource_kind, ProtocolResourceKind::Single)
                    && (value.values.len() != 1 || state.scalar_value.is_some()))
                || value
                    .values
                    .iter()
                    .any(|item| !runtime_value_matches_type(active, item, state.expected_type))
            {
                return Err(ResourceTransportFailure::Shape);
            }
            if state.cancellation_requested {
                if matches!(
                    disposition,
                    orna_protocol::ResourceFrameDisposition::DroppedLate
                ) && matches!(state.resource_kind, ProtocolResourceKind::Single)
                {
                    state.scalar_value = value.values.into_iter().next();
                }
                return Ok(true);
            }
            match state.resource_kind {
                ProtocolResourceKind::Single => {
                    state.scalar_value = value.values.into_iter().next()
                }
                ProtocolResourceKind::Stream => {
                    state.stream_values_seen = true;
                    state
                        .completion
                        .send(Ok(ResourceTransportOutcome::StreamValues(value.values)))
                        .await
                        .map_err(|_| ResourceTransportFailure::Transport)?;
                    let update = ResourceWindowUpdate {
                        stream_id: value.stream_id,
                        request_id: value.request_id,
                        add_items: u64::from(value.item_count),
                        add_bytes: u64::from(value.byte_count),
                    };
                    state
                        .protocol
                        .receive(orna_protocol::ResourceClientFrame::WindowUpdate(update))
                        .map_err(|_| ResourceTransportFailure::Shape)?;
                    let encoded = encode_resource_client_frame(
                        active,
                        registry,
                        &orna_protocol::ResourceClientFrame::WindowUpdate(update),
                    )
                    .map_err(|_| ResourceTransportFailure::Shape)?;
                    write_shared_broker_frame(stream, &encoded).await?;
                }
            }
        }
        ResourceServerFrame::Completed(value) => {
            if value.request_id != state.request.request_id {
                return Err(ResourceTransportFailure::Shape);
            }
            if !state.accepted {
                if late_terminal {
                    send_shared_resource_terminal(
                        state,
                        Err(ResourceTransportFailure::Shape),
                    )
                    .await?;
                    return Ok(false);
                }
                return Err(ResourceTransportFailure::Shape);
            }
            let outcome = match state.resource_kind {
                ProtocolResourceKind::Single => {
                    let Some(value) = state.scalar_value.take() else {
                        if late_terminal {
                            send_shared_resource_terminal(
                                state,
                                Err(ResourceTransportFailure::Shape),
                            )
                            .await?;
                            return Ok(false);
                        }
                        return Err(ResourceTransportFailure::Shape);
                    };
                    ResourceTransportOutcome::Ready(value)
                }
                ProtocolResourceKind::Stream => ResourceTransportOutcome::StreamCompleted,
            };
            send_shared_resource_terminal(state, Ok(outcome)).await?;
            return Ok(false);
        }
        ResourceServerFrame::Failed(value) => {
            if value.request_id != state.request.request_id {
                return Err(ResourceTransportFailure::Shape);
            }
            if state.scalar_value.is_some() {
                if late_terminal {
                    send_shared_resource_terminal(
                        state,
                        Err(ResourceTransportFailure::Shape),
                    )
                    .await?;
                    return Ok(false);
                }
                return Err(ResourceTransportFailure::Shape);
            }
            send_shared_resource_terminal(
                state,
                Ok(ResourceTransportOutcome::Failed {
                    failure: value.failure,
                }),
            )
            .await?;
            return Ok(false);
        }
        ResourceServerFrame::Cancelled(value) => {
            if value.request_id != state.request.request_id {
                return Err(ResourceTransportFailure::Shape);
            }
            send_shared_resource_terminal(
                state,
                Ok(ResourceTransportOutcome::Cancelled),
            )
            .await?;
            return Ok(false);
        }
    }
    Ok(true)
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

fn pre_request_control_failure(control: ResourceTransportControl) -> ResourceTransportFailure {
    match control {
        ResourceTransportControl::Cancel(_) => ResourceTransportFailure::Cancelled,
        ResourceTransportControl::Shutdown => ResourceTransportFailure::Transport,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResourceTransportCancellationAction {
    ReturnCancelled,
    ContinueCommitted,
}

fn resource_transport_cancellation_action(
    cancellation_won: bool,
) -> ResourceTransportCancellationAction {
    if cancellation_won {
        ResourceTransportCancellationAction::ReturnCancelled
    } else {
        ResourceTransportCancellationAction::ContinueCommitted
    }
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

async fn run_authenticated_resource_transport(
    kernel: PostgresKernel,
    session: AuthenticatedSession,
    active: ActiveDatabaseRevision,
    _registry: orna_core::value::OpaqueCodecRegistry,
    request: ResourceRequest,
    expected_type: ResolvedType,
    resource_kind: ProtocolResourceKind,
    mut controls: UnboundedReceiver<ResourceTransportControl>,
    completion_sender: &Sender<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
) -> Result<ResourceTransportOutcome, ResourceTransportFailure> {
    let cancellation = ResourceCancellation::new();
    let start = kernel.start_authenticated_server_resource_producer(&session, &request, &cancellation);
    tokio::pin!(start);
    let started = tokio::select! {
        biased;
        control = controls.recv() => match control {
            Some(_control) => {
                match resource_transport_cancellation_action(cancellation.request_cancel()) {
                    ResourceTransportCancellationAction::ReturnCancelled => {
                        return Ok(ResourceTransportOutcome::Cancelled);
                    }
                    ResourceTransportCancellationAction::ContinueCommitted => {
                        (&mut start)
                            .await
                            .map_err(|_| ResourceTransportFailure::Transport)?
                    }
                }
            }
            None => return Err(ResourceTransportFailure::Transport),
        },
        started = &mut start => started
            .map_err(|_| ResourceTransportFailure::Transport)?,
    };
    let producer = match started {
        AuthenticatedServerResourceStart::Accepted(producer) => producer,
        AuthenticatedServerResourceStart::Failed { failure, .. } => {
            return Ok(ResourceTransportOutcome::Failed { failure });
        }
    };
    let accepted = producer.accepted();
    let accepted_kind = match resource_kind {
        ProtocolResourceKind::Single => {
            orna_postgres::AuthenticatedServerResourceKind::Single
        }
        ProtocolResourceKind::Stream => orna_postgres::AuthenticatedServerResourceKind::Stream,
    };
    if accepted.stream_id != request.stream_id
        || accepted.request_id != request.request_id
        || accepted.target_revision != request.target_revision
        || accepted.resource_kind != accepted_kind
    {
        producer.cancel();
        return Err(ResourceTransportFailure::Shape);
    }

    if cancellation.is_requested() {
        // Cancellation can arrive after the acceptance commit check and before
        // the producer publishes its acceptance. Do not issue a pull to a
        // producer that is already terminating without a response.
        drop(producer);
        return Ok(ResourceTransportOutcome::Cancelled);
    }
    let mut scalar_value = None;
    let mut next_batch_sequence = 0_u64;
    let mut total_items = 0_u64;
    let mut total_bytes = 0_u64;
    let mut byte_credit = MAX_RESOURCE_WINDOW;
    loop {
        // A scalar value is followed by a zero-credit terminal probe. The
        // producer accepts that probe only after the row has been delivered;
        // ResourceCredit::new intentionally rejects zero credit for requests.
        let credit = if matches!(resource_kind, ProtocolResourceKind::Single)
            && scalar_value.is_some()
        {
            ResourceCredit {
                item_count: 0,
                byte_count: 0,
            }
        } else {
            ResourceCredit::new(1, byte_credit).ok_or(ResourceTransportFailure::Shape)?
        };
        let pull = producer.pull(credit);
        tokio::pin!(pull);
        let event = tokio::select! {
            biased;
            control = controls.recv() => match control {
                Some(_control) => {
                    match resource_transport_cancellation_action(producer.cancel()) {
                        ResourceTransportCancellationAction::ReturnCancelled => {
                            return Ok(ResourceTransportOutcome::Cancelled);
                        }
                        ResourceTransportCancellationAction::ContinueCommitted => {
                            (&mut pull)
                                .await
                                .map_err(|_| ResourceTransportFailure::Transport)?
                        }
                    }
                }
                None => return Err(ResourceTransportFailure::Transport),
            },
            event = &mut pull => event.map_err(|_| ResourceTransportFailure::Transport)?,
        };
        match event {
            AuthenticatedServerResourceEvent::Values {
                batch_sequence,
                item_count,
                byte_count,
                values,
            } => {
                if values.is_empty()
                    || item_count == 0
                    || item_count as usize != values.len()
                    || batch_sequence != next_batch_sequence
                    || byte_count == 0
                {
                    producer.cancel();
                    return Err(ResourceTransportFailure::Shape);
                }
                for value in &values {
                    if !runtime_value_matches_type(&active, value, expected_type) {
                        producer.cancel();
                        return Err(ResourceTransportFailure::Shape);
                    }
                }
                next_batch_sequence = next_batch_sequence
                    .checked_add(1)
                    .ok_or(ResourceTransportFailure::Shape)?;
                total_items = total_items
                    .checked_add(u64::from(item_count))
                    .ok_or(ResourceTransportFailure::Shape)?;
                total_bytes = total_bytes
                    .checked_add(byte_count)
                    .ok_or(ResourceTransportFailure::Shape)?;
                match resource_kind {
                    ProtocolResourceKind::Single => {
                        if values.len() != 1 || scalar_value.is_some() {
                            producer.cancel();
                            return Err(ResourceTransportFailure::Shape);
                        }
                        scalar_value = values.into_iter().next();
                    }
                    ProtocolResourceKind::Stream => {
                        let send = completion_sender
                            .send(Ok(ResourceTransportOutcome::StreamValues(values)));
                        tokio::pin!(send);
                        let sent = tokio::select! {
                            biased;
                            result = &mut send => result
                                .map(|_| ())
                                .map_err(|_| ResourceTransportFailure::Transport),
                            control = controls.recv() => match control {
                                Some(_control) => {
                                    match resource_transport_cancellation_action(producer.cancel()) {
                                        ResourceTransportCancellationAction::ReturnCancelled => {
                                            return Ok(ResourceTransportOutcome::Cancelled);
                                        }
                                        ResourceTransportCancellationAction::ContinueCommitted => {
                                            (&mut send)
                                                .await
                                                .map(|_| ())
                                                .map_err(|_| ResourceTransportFailure::Transport)
                                        }
                                    }
                                }
                                None => Err(ResourceTransportFailure::Transport),
                            },
                        };
                        match sent {
                            Ok(()) => {}
                            Err(error) => {
                                producer.cancel();
                                return Err(error);
                            }
                        }
                    }
                }
            }
            AuthenticatedServerResourceEvent::Completed {
                final_batch_sequence,
                total_items: completed_items,
                total_bytes: completed_bytes,
            } => {
                let expected_final_batch = next_batch_sequence.saturating_sub(1);
                if final_batch_sequence != expected_final_batch
                    || completed_items != total_items
                    || completed_bytes != total_bytes
                {
                    producer.cancel();
                    return Err(ResourceTransportFailure::Shape);
                }
                return match resource_kind {
                    ProtocolResourceKind::Single => scalar_value
                        .map(ResourceTransportOutcome::Ready)
                        .ok_or(ResourceTransportFailure::Shape),
                    ProtocolResourceKind::Stream => Ok(ResourceTransportOutcome::StreamCompleted)
                };
            }
            AuthenticatedServerResourceEvent::Failed { failure } => {
                if scalar_value.is_some() {
                    producer.cancel();
                    return Err(ResourceTransportFailure::Shape);
                }
                return Ok(ResourceTransportOutcome::Failed { failure });
            }
            AuthenticatedServerResourceEvent::Cancelled => {
                return Ok(ResourceTransportOutcome::Cancelled);
            }
            AuthenticatedServerResourceEvent::Waiting { required_bytes } => {
                if required_bytes == 0 || required_bytes > MAX_RESOURCE_WINDOW {
                    producer.cancel();
                    return Err(ResourceTransportFailure::Shape);
                }
                byte_credit = required_bytes;
            }
        }
    }
}

async fn run_resource_transport(
    stream: StandardUnixStream,
    handshake_complete: bool,
    protocol: ResourceProtocolConnection,
    active: ActiveDatabaseRevision,
    registry: orna_core::value::OpaqueCodecRegistry,
    request: ResourceRequest,
    expected_type: ResolvedType,
    resource_kind: ProtocolResourceKind,
    mut controls: UnboundedReceiver<ResourceTransportControl>,
    completion_sender: &Sender<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
) -> Result<ResourceTransportRun, ResourceTransportFailure> {
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
    let mut connection = protocol;
    connection
        .open(request)
        .map_err(|_| ResourceTransportFailure::Shape)?;

    if !handshake_complete {
        if let Some(control) =
            write_resource_transport_stage(&mut stream, &CONSTRUCTED_CLIENT_HELLO, &mut controls)
                .await?
        {
            return Err(pre_request_control_failure(control));
        }
        let mut acknowledgement = [0_u8; CONSTRUCTED_SERVER_ACK.len()];
        if let Some(control) =
            read_resource_ack_stage(&mut stream, &mut acknowledgement, &mut controls).await?
        {
            return Err(pre_request_control_failure(control));
        }
        if acknowledgement != CONSTRUCTED_SERVER_ACK {
            return Err(ResourceTransportFailure::Transport);
        }
    }
    if let Some(control) =
        write_resource_transport_stage(&mut stream, &encoded_request, &mut controls).await?
    {
        return Err(pre_request_control_failure(control));
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
                        return Ok(ResourceTransportRun {
                            stream,
                            protocol: connection,
                            outcome: ResourceTransportOutcome::Ready(value),
                        });
                    }
                    ProtocolResourceKind::Stream => {
                        return Ok(ResourceTransportRun {
                            stream,
                            protocol: connection,
                            outcome: ResourceTransportOutcome::StreamCompleted,
                        });
                    }
                }
            }
            ResourceFrameResult::Failed(failure) => {
                if scalar_value.is_some() {
                    return Err(ResourceTransportFailure::Shape);
                }
                return Ok(ResourceTransportRun {
                    stream,
                    protocol: connection,
                    outcome: ResourceTransportOutcome::Failed { failure },
                });
            }
            ResourceFrameResult::Cancelled => {
                return Ok(ResourceTransportRun {
                    stream,
                    protocol: connection,
                    outcome: ResourceTransportOutcome::Cancelled,
                });
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
    tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, stream.read_exact(&mut header))
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
        stream.read_exact(&mut encoded[RESOURCE_HEADER_LENGTH..]),
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
        ResolvedType::Named(type_id) => {
            active_type_definition(active, type_id).is_some_and(|definition| {
                definition.as_record_value().is_some() || definition.as_enum().is_some()
            })
        }
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

    runtime.block_on(host_invoke(kernel, request, stdout, stderr))
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

    let (server_end, client_end) = StandardUnixStream::pair().map_err(|_| {
        InstalledInvokeError::new(
            InstalledInvokeErrorKind::Authentication,
            "the local invoke connection could not be created".to_owned(),
        )
    })?;
    let (broker, receiver) = SharedInvokeBroker::pending();
    let mut server_task = tokio::spawn(serve_local_raw_stream_with_broker(
        kernel.clone(),
        server_end,
        LocalRawSocketResources::new(),
        Some(broker.clone()),
    ));
    if broker
        .activate(client_end, active.clone(), registry.clone(), receiver)
        .await
        .is_err()
    {
        broker.shutdown().await;
        if tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, &mut server_task)
            .await
            .is_err()
        {
            server_task.abort();
            let _ = server_task.await;
        }
        return Err(InstalledInvokeError::new(
            InstalledInvokeErrorKind::Authentication,
            "the local invoke connection could not authenticate".to_owned(),
        ));
    }
    let result = broker.invoke(retained).await.map_err(|error| match error {
        ResourceTransportFailure::Cancelled => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Cancelled,
            "invocation cancelled".to_owned(),
        ),
        ResourceTransportFailure::Shape => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the local invoke connection returned an invalid frame".to_owned(),
        ),
        ResourceTransportFailure::RootSealedDispatchInternal => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "sealed dispatch failed".to_owned(),
        ),
        ResourceTransportFailure::Transport => InstalledInvokeError::new(
            InstalledInvokeErrorKind::Internal,
            "the local invoke connection failed".to_owned(),
        ),
    });
    broker.shutdown().await;
    if tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, &mut server_task)
        .await
        .is_err()
    {
        server_task.abort();
        let _ = server_task.await;
    }
    let result = result?;

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
        FunctionReturn::Stream(resolved) => format!("STREAM<{}>", render_resolved_type(*resolved)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::{
        CatalogueRevisionId, FunctionId, InvocationId, ParameterId, SourceBundleId,
        SourceRevisionId,
        canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
        catalogue::{
            CatalogueSnapshot, FunctionDomain, FunctionReturn, FunctionSecurity,
            FunctionVolatility, ParameterDefinition,
        },
        invocation::{
            InvocationFailure, InvocationFailurePhase, InvocationRetryability, InvokeEvent,
            InvokeValue,
        },
        revision::{RevisionPair, StoredSourceRevision},
        types::StandardScalar,
        value::RuntimeValue,
    };
    use orna_protocol::{
        InvocationEventBatch, InvocationEventRecord, ResourceAccepted, ResourceCompleted, ResourceFailed,
        ResourceValues, decode_resource_client_frame, encode_resource_server_frame,
    };
    use orna_standard::{
        STD_UI_TYPE_ID, retained_standard_library_snapshot, verify_standard_library_snapshot,
    };
    use std::io::{Read, Write};

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

    #[tokio::test]
    async fn shared_broker_reader_retains_partial_frames_across_polling() {
        let (mut client, server) = tokio::io::duplex(64);
        let (sender, mut receiver) = mpsc::channel(1);
        let reader = tokio::spawn(read_shared_broker_frames(server, sender));
        let frame = [0_u8; 18];
        client.write_all(&frame[..5]).await.expect("partial frame write");
        assert!(tokio::time::timeout(Duration::from_millis(10), receiver.recv())
            .await
            .is_err());
        client.write_all(&frame[5..]).await.expect("frame remainder write");
        let decoded = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("frame arrives")
            .expect("reader remains connected")
            .expect("valid frame");
        assert_eq!(decoded.bytes, frame);
        reader.abort();
        let _ = reader.await;
    }

    #[tokio::test]
    async fn broker_stream_completion_queue_is_finite() {
        let (sender, mut receiver) = mpsc::channel::<Result<ResourceTransportOutcome, ResourceTransportFailure>>(
            BROKER_RESOURCE_COMPLETION_CAPACITY,
        );
        for _ in 0..BROKER_RESOURCE_COMPLETION_CAPACITY {
            sender
                .send(Ok(ResourceTransportOutcome::StreamCompleted))
                .await
                .expect("queue accepts its configured capacity");
        }
        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            sender.send(Ok(ResourceTransportOutcome::StreamCompleted)),
        )
        .await
        .is_err());
        let _ = receiver.recv().await;
    }

    #[tokio::test]
    async fn shared_broker_drops_known_terminal_frames_and_rejects_unknown_streams() {
        let (active, registry) = transport_test_context();
        let request = transport_test_request(active.pair(), 1);
        let accepted = ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x40; 16]),
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        };
        let value = RuntimeValue::Integer(7);
        let byte_count = encode_constructed_value(&active, &registry, &value)
            .expect("encoded resource value")
            .len() as u32;
        let values = ResourceValues {
            stream_id: request.stream_id,
            request_id: request.request_id,
            batch_sequence: 0,
            item_count: 1,
            byte_count,
            values: vec![value],
        };
        let completed = ResourceCompleted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            final_batch_sequence: 0,
            total_items: 1,
        };
        let protocol = {
            let mut protocol = ResourceProtocolConnection::new();
            protocol.open(request.clone()).expect("resource request opens");
            protocol
        };
        let (completion, mut completions) = mpsc::channel(2);
        let mut resources = BTreeMap::from([(
            request.stream_id,
            BrokerResourceState {
                request: request.clone(),
                expected_type: ResolvedType::Scalar(StandardScalar::Integer),
                resource_kind: ProtocolResourceKind::Single,
                protocol,
                completion,
                accepted: false,
                scalar_value: None,
                cancellation_requested: false,
                stream_values_seen: false,
            },
        )]);
        let mut tombstones = BrokerResourceTombstones::new();
        let mut root = None;
        let (_reader, mut writer) = tokio::io::duplex(128);

        for frame in [
            ResourceServerFrame::Accepted(accepted),
            ResourceServerFrame::Values(values),
            ResourceServerFrame::Completed(completed),
        ] {
            let bytes = encode_resource_server_frame(&active, &registry, &frame)
                .expect("encoded resource response");
            handle_shared_broker_frame(
                BrokerWireFrame { resource: true, bytes },
                &mut writer,
                &active,
                &registry,
                &mut root,
                &mut resources,
                &mut tombstones,
            )
            .await
            .expect("valid resource response");
        }
        assert!(resources.is_empty());
        assert_eq!(tombstones.len(), 1);
        assert!(matches!(
            completions.recv().await,
            Some(Ok(ResourceTransportOutcome::Ready(RuntimeValue::Integer(7))))
        ));

        let late_bytes = encode_resource_server_frame(
            &active,
            &registry,
            &ResourceServerFrame::Completed(completed),
        )
        .expect("encoded late resource response");
        handle_shared_broker_frame(
            BrokerWireFrame {
                resource: true,
                bytes: late_bytes,
            },
            &mut writer,
            &active,
            &registry,
            &mut root,
            &mut resources,
            &mut tombstones,
        )
        .await
        .expect("late terminal response is dropped");
        assert!(completions.try_recv().is_err());
        assert!(resources.is_empty());
        assert_eq!(tombstones.len(), 1);

        let mismatched_bytes = encode_resource_server_frame(
            &active,
            &registry,
            &ResourceServerFrame::Completed(ResourceCompleted {
                request_id: InvocationId::from_bytes([0xaa; 16]),
                ..completed
            }),
        )
        .expect("encoded mismatched late resource response");
        assert!(matches!(
            handle_shared_broker_frame(
                BrokerWireFrame {
                    resource: true,
                    bytes: mismatched_bytes,
                },
                &mut writer,
                &active,
                &registry,
                &mut root,
                &mut resources,
                &mut tombstones,
            )
            .await,
            Err(ResourceTransportFailure::Shape)
        ));

        let unknown_request = transport_test_request(active.pair(), 2);
        let unknown_bytes = encode_resource_server_frame(
            &active,
            &registry,
            &ResourceServerFrame::Failed(ResourceFailed {
                stream_id: unknown_request.stream_id,
                request_id: unknown_request.request_id,
                failure: CallFailure::ExecuteDenied,
            }),
        )
        .expect("encoded unknown resource response");
        assert!(matches!(
            handle_shared_broker_frame(
                BrokerWireFrame {
                    resource: true,
                    bytes: unknown_bytes,
                },
                &mut writer,
                &active,
                &registry,
                &mut root,
                &mut resources,
                &mut tombstones,
            )
            .await,
            Err(ResourceTransportFailure::Shape)
        ));
    }

    #[test]
    fn shared_broker_resource_expectations_require_exact_request_identity() {
        let (active, _) = transport_test_context();
        let (broker, _receiver) = SharedInvokeBroker::pending();
        let request = transport_test_request(active.pair(), 1);
        assert!(broker.register_expected_resource_request(&request));
        assert!(!broker.register_expected_resource_request(&request));

        let mut mismatched = request.clone();
        mismatched.generation += 1;
        assert!(!broker.take_expected_resource_request(&mismatched));
        assert!(broker.take_expected_resource_request(&request));
        assert!(!broker.take_expected_resource_request(&request));
    }

    #[test]
    fn shared_broker_terminal_tombstones_are_bounded() {
        let mut tombstones = BrokerResourceTombstones::new();
        for stream_id in 1..=(BROKER_RESOURCE_TOMBSTONE_CAPACITY as u64 + 1) {
            remember_broker_resource_terminal(
                &mut tombstones,
                stream_id,
                InvocationId::from_bytes([stream_id as u8; 16]),
            );
        }
        assert_eq!(tombstones.len(), BROKER_RESOURCE_TOMBSTONE_CAPACITY);
        assert!(!tombstones.contains_key(&1));
        assert!(tombstones.contains_key(&(BROKER_RESOURCE_TOMBSTONE_CAPACITY as u64 + 1)));
    }

    #[test]
    fn shared_broker_reconstructs_completed_root_events() {
        let events = echo_events();
        let invocation = events.records()[0].event().invocation_id();
        let result = reconstruct_shared_root_result(invocation, events.records().to_vec())
            .expect("completed root result");
        assert!(matches!(result, SealedInvocationResult::Completed { .. }));
    }

    #[test]
    fn shared_broker_reconstructs_failed_root_events() {
        let invocation = InvocationId::new();
        let started = InvokeEvent::new(
            invocation,
            0,
            InvocationEventBody::Started {
                visible_principal: None,
            },
        )
        .expect("started event");
        let failure = orna_core::invocation::InvocationFailure::new(
            orna_core::invocation::InvocationFailurePhase::Target,
            "TARGET_FAILED",
            "invocation failed",
            None,
            orna_core::invocation::InvocationRetryability::No,
        )
        .expect("failure event");
        let failed = InvokeEvent::new(invocation, 1, InvocationEventBody::Failed(failure))
            .expect("failed event");
        let result = reconstruct_shared_root_result(
            invocation,
            vec![
                InvocationEventRecord::new(1, started),
                InvocationEventRecord::new(2, failed),
            ],
        )
        .expect("failed root result");
        assert!(matches!(result, SealedInvocationResult::Failed { .. }));
    }

    #[test]
    fn shared_broker_maps_redacted_root_failure_classes() {
        let redacted_result = |code: &str| {
            let invocation = InvocationId::new();
            let started = InvokeEvent::new(
                invocation,
                0,
                InvocationEventBody::Started {
                    visible_principal: None,
                },
            )
            .expect("started event");
            let failure = orna_core::invocation::InvocationFailure::new(
                orna_core::invocation::InvocationFailurePhase::Internal,
                code,
                "redacted failure",
                None,
                orna_core::invocation::InvocationRetryability::No,
            )
            .expect("failure event");
            let failed = InvokeEvent::new(invocation, 1, InvocationEventBody::Failed(failure))
                .expect("failed event");
            reconstruct_shared_root_result(
                invocation,
                vec![
                    InvocationEventRecord::new(1, started),
                    InvocationEventRecord::new(2, failed),
                ],
            )
        };
        assert!(matches!(
            redacted_result("INVOKE_DENIED"),
            Ok(SealedInvocationResult::Denied { .. })
        ));
        assert!(matches!(
            redacted_result("INVOKE_PRESENTATION_FAILURE"),
            Ok(SealedInvocationResult::PresentationFailed { .. })
        ));
        assert!(matches!(
            redacted_result("INVOKE_INTERNAL_FAILURE"),
            Err(ResourceTransportFailure::RootSealedDispatchInternal)
        ));
    }

    #[test]
    fn shared_broker_maps_cancelled_root_terminal_to_cancelled_transport() {
        let invocation = InvocationId::new();
        let started = InvokeEvent::new(
            invocation,
            0,
            InvocationEventBody::Started {
                visible_principal: None,
            },
        )
        .expect("started event");
        let cancelled = InvokeEvent::new(
            invocation,
            1,
            InvocationEventBody::Cancelled { reason: None },
        )
        .expect("cancelled event");
        let result = reconstruct_shared_root_result(
            invocation,
            vec![
                InvocationEventRecord::new(1, started),
                InvocationEventRecord::new(2, cancelled),
            ],
        );
        assert!(matches!(result, Err(ResourceTransportFailure::Cancelled)));
    }

    #[tokio::test]
    async fn broker_publishes_late_committed_failure_after_cancel() {
        let (active, registry) = transport_test_context();
        let request = transport_test_request(active.pair(), 1);
        let nested_invocation_id = InvocationId::from_bytes([0x40; 16]);
        let accepted = ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id,
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        };
        let mut protocol = ResourceProtocolConnection::new();
        protocol.open(request.clone()).expect("resource request opens");
        protocol
            .apply_constructed(
                &active,
                &registry,
                ResourceServerFrame::Accepted(accepted),
            )
            .expect("resource acceptance applies");
        protocol
            .receive(ResourceClientFrame::Cancel(ResourceCancel {
                stream_id: request.stream_id,
                request_id: request.request_id,
                reason: ResourceCancellationCode::ParentInvocationCancelled,
            }))
            .expect("resource cancellation applies");
        let (completion, mut completions) = mpsc::channel(2);
        let mut state = BrokerResourceState {
            request: request.clone(),
            expected_type: ResolvedType::Scalar(StandardScalar::Integer),
            resource_kind: ProtocolResourceKind::Single,
            protocol,
            completion,
            accepted: true,
            scalar_value: None,
            cancellation_requested: true,
            stream_values_seen: false,
        };
        let (_reader, mut writer) = tokio::io::duplex(128);
        let keep = handle_shared_resource_frame(
            &mut state,
            ResourceServerFrame::Failed(ResourceFailed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                failure: CallFailure::ExecuteDenied,
            }),
            &mut writer,
            &active,
            &registry,
        )
        .await
        .expect("late committed failure is valid");
        assert!(!keep);
        assert!(matches!(
            completions.recv().await,
            Some(Ok(ResourceTransportOutcome::Failed {
                failure: CallFailure::ExecuteDenied
            }))
        ));
    }

    #[tokio::test]
    async fn broker_publishes_late_committed_completed_after_cancel() {
        let (active, registry) = transport_test_context();
        let request = transport_test_request(active.pair(), 1);
        let accepted = ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x40; 16]),
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        };
        let value = RuntimeValue::Integer(7);
        let byte_count = encode_constructed_value(&active, &registry, &value)
            .expect("encoded resource value")
            .len() as u32;
        let values = ResourceValues {
            stream_id: request.stream_id,
            request_id: request.request_id,
            batch_sequence: 0,
            item_count: 1,
            byte_count,
            values: vec![value],
        };
        let mut protocol = ResourceProtocolConnection::new();
        protocol.open(request.clone()).expect("resource request opens");
        protocol
            .apply_constructed(&active, &registry, ResourceServerFrame::Accepted(accepted))
            .expect("resource acceptance applies");
        protocol
            .receive(ResourceClientFrame::Cancel(ResourceCancel {
                stream_id: request.stream_id,
                request_id: request.request_id,
                reason: ResourceCancellationCode::ParentInvocationCancelled,
            }))
            .expect("resource cancellation applies");
        let (completion, mut completions) = mpsc::channel(2);
        let mut state = BrokerResourceState {
            request: request.clone(),
            expected_type: ResolvedType::Scalar(StandardScalar::Integer),
            resource_kind: ProtocolResourceKind::Single,
            protocol,
            completion,
            accepted: true,
            scalar_value: None,
            cancellation_requested: true,
            stream_values_seen: false,
        };
        let (_reader, mut writer) = tokio::io::duplex(128);
        assert!(
            handle_shared_resource_frame(
                &mut state,
                ResourceServerFrame::Values(values),
                &mut writer,
                &active,
                &registry,
            )
            .await
            .expect("late committed values are drained")
        );
        let keep = handle_shared_resource_frame(
            &mut state,
            ResourceServerFrame::Completed(ResourceCompleted {
                stream_id: request.stream_id,
                request_id: request.request_id,
                final_batch_sequence: 0,
                total_items: 1,
            }),
            &mut writer,
            &active,
            &registry,
        )
        .await
        .expect("late committed completion is published");
        assert!(!keep);
        assert!(matches!(
            completions.recv().await,
            Some(Ok(ResourceTransportOutcome::Ready(RuntimeValue::Integer(7))))
        ));
    }

    #[tokio::test]
    async fn broker_publishes_failure_for_dropped_late_completed_without_value() {
        let (active, registry) = transport_test_context();
        let request = transport_test_request(active.pair(), 1);
        let accepted = ResourceAccepted {
            stream_id: request.stream_id,
            request_id: request.request_id,
            nested_invocation_id: InvocationId::from_bytes([0x40; 16]),
            target_revision: request.target_revision,
            resource_kind: request.resource_kind,
        };
        let mut protocol = ResourceProtocolConnection::new();
        protocol.open(request.clone()).expect("resource request opens");
        protocol
            .apply_constructed(&active, &registry, ResourceServerFrame::Accepted(accepted))
            .expect("resource acceptance applies");
        protocol
            .receive(ResourceClientFrame::Cancel(ResourceCancel {
                stream_id: request.stream_id,
                request_id: request.request_id,
                reason: ResourceCancellationCode::ParentInvocationCancelled,
            }))
            .expect("resource cancellation applies");
        let (completion, mut completions) = mpsc::channel(2);
        let mut state = BrokerResourceState {
            request: request.clone(),
            expected_type: ResolvedType::Scalar(StandardScalar::Integer),
            resource_kind: ProtocolResourceKind::Single,
            protocol,
            completion,
            accepted: true,
            scalar_value: None,
            cancellation_requested: true,
            stream_values_seen: false,
        };
        let (_reader, mut writer) = tokio::io::duplex(128);
        let keep = handle_shared_resource_frame(
            &mut state,
            ResourceServerFrame::Completed(ResourceCompleted {
                stream_id: request.stream_id,
                request_id: request.request_id,
                final_batch_sequence: 0,
                total_items: 1,
            }),
            &mut writer,
            &active,
            &registry,
        )
        .await
        .expect("late committed completion closes the resource");
        assert!(!keep);
        assert!(matches!(
            completions.recv().await,
            Some(Err(ResourceTransportFailure::Shape))
        ));
    }

    #[tokio::test]
    async fn broker_publishes_failure_before_acceptance_after_cancel() {
        let (active, registry) = transport_test_context();
        let request = transport_test_request(active.pair(), 1);
        let mut protocol = ResourceProtocolConnection::new();
        protocol.open(request.clone()).expect("resource request opens");
        protocol
            .receive(ResourceClientFrame::Cancel(ResourceCancel {
                stream_id: request.stream_id,
                request_id: request.request_id,
                reason: ResourceCancellationCode::ParentInvocationCancelled,
            }))
            .expect("resource cancellation applies");
        let (completion, mut completions) = mpsc::channel(2);
        let mut state = BrokerResourceState {
            request: request.clone(),
            expected_type: ResolvedType::Scalar(StandardScalar::Integer),
            resource_kind: ProtocolResourceKind::Single,
            protocol,
            completion,
            accepted: false,
            scalar_value: None,
            cancellation_requested: true,
            stream_values_seen: false,
        };
        let (_reader, mut writer) = tokio::io::duplex(128);
        let keep = handle_shared_resource_frame(
            &mut state,
            ResourceServerFrame::Failed(ResourceFailed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                failure: CallFailure::ExecuteDenied,
            }),
            &mut writer,
            &active,
            &registry,
        )
        .await
        .expect("late failure closes the resource");
        assert!(!keep);
        assert!(matches!(
            completions.recv().await,
            Some(Ok(ResourceTransportOutcome::Failed {
                failure: CallFailure::ExecuteDenied
            }))
        ));
    }

    #[tokio::test]
    async fn cancelled_resource_terminal_ignores_closed_completion_receiver() {
        let (active, _) = transport_test_context();
        let request = transport_test_request(active.pair(), 1);
        let (completion, receiver) = mpsc::channel(1);
        drop(receiver);
        let state = BrokerResourceState {
            request,
            expected_type: ResolvedType::Scalar(StandardScalar::Integer),
            resource_kind: ProtocolResourceKind::Single,
            protocol: ResourceProtocolConnection::new(),
            completion,
            accepted: true,
            scalar_value: None,
            cancellation_requested: true,
            stream_values_seen: false,
        };
        send_shared_resource_terminal(&state, Ok(ResourceTransportOutcome::Cancelled))
            .await
            .expect("cancelled receiver closure is local completion");
    }

    #[tokio::test]
    async fn live_resource_terminal_rejects_closed_completion_receiver() {
        let (active, _) = transport_test_context();
        let request = transport_test_request(active.pair(), 1);
        let (completion, receiver) = mpsc::channel(1);
        drop(receiver);
        let state = BrokerResourceState {
            request,
            expected_type: ResolvedType::Scalar(StandardScalar::Integer),
            resource_kind: ProtocolResourceKind::Single,
            protocol: ResourceProtocolConnection::new(),
            completion,
            accepted: true,
            scalar_value: None,
            cancellation_requested: false,
            stream_values_seen: false,
        };
        assert!(matches!(
            send_shared_resource_terminal(&state, Ok(ResourceTransportOutcome::Cancelled)).await,
            Err(ResourceTransportFailure::Transport)
        ));
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
    fn cancellation_decision_only_returns_cancelled_when_request_wins() {
        assert_eq!(
            resource_transport_cancellation_action(true),
            ResourceTransportCancellationAction::ReturnCancelled,
        );
        assert_eq!(
            resource_transport_cancellation_action(false),
            ResourceTransportCancellationAction::ContinueCommitted,
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
    fn transport_test_context() -> (
        ActiveDatabaseRevision,
        orna_core::value::OpaqueCodecRegistry,
    ) {
        let source_bundle = SourceBundleId::from_bytes([0x81; 16]);
        let source_revision = SourceRevisionId::from_bytes([0x82; 16]);
        let bundle_hash = source_bundle_digest(&[]).expect("source bundle digest");
        let source = StoredSourceRevision::new(
            source_bundle,
            source_revision,
            None,
            Vec::new(),
            bundle_hash,
            source_revision_record_digest(source_bundle, None, bundle_hash)
                .expect("source revision digest"),
        )
        .expect("stored source revision");
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0x83; 16]),
            Vec::new(),
            Vec::new(),
        )
        .expect("empty catalogue");
        let active = ActiveDatabaseRevision::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue.clone(),
            catalogue_digest(&catalogue, &[], &[], &[], &[]).expect("catalogue digest"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("active revision");
        let standard = verify_standard_library_snapshot(
            retained_standard_library_snapshot().expect("retained standard snapshot"),
        )
        .expect("verified standard snapshot");
        let registry = registered_opaque_codecs(&standard).expect("standard codecs");
        (active, registry)
    }

    fn transport_test_request(revision: RevisionPair, stream_id: u64) -> ResourceRequest {
        ResourceRequest {
            stream_id,
            request_id: InvocationId::from_bytes([stream_id as u8; 16]),
            parent_invocation_id: InvocationId::from_bytes([0x21; 16]),
            call_site_id: orna_core::CallSiteId::from_bytes([0x22; 16]),
            state_profile: String::new(),
            function_instance_key: String::new(),
            target_function_id: FunctionId::from_bytes([0x23; 16]),
            target_revision: revision,
            generation: stream_id,
            resource_kind: ProtocolResourceKind::Single,
            arguments: Vec::new(),
            item_window: 1,
            byte_window: MAX_RESOURCE_WINDOW,
        }
    }

    fn read_resource_test_frame(stream: &mut StandardUnixStream) -> Vec<u8> {
        let mut encoded = vec![0_u8; RESOURCE_HEADER_LENGTH];
        stream
            .read_exact(&mut encoded)
            .expect("resource frame header");
        let payload_length =
            u32::from_be_bytes(encoded[17..21].try_into().expect("resource length")) as usize;
        encoded.resize(RESOURCE_HEADER_LENGTH + payload_length, 0);
        stream
            .read_exact(&mut encoded[RESOURCE_HEADER_LENGTH..])
            .expect("resource frame payload");
        encoded
    }

    fn serve_two_scalar_test_requests(
        mut stream: StandardUnixStream,
        active: ActiveDatabaseRevision,
        registry: orna_core::value::OpaqueCodecRegistry,
    ) -> Vec<ResourceRequest> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("peer read timeout");
        let mut hello = [0_u8; CONSTRUCTED_CLIENT_HELLO.len()];
        stream.read_exact(&mut hello).expect("constructed hello");
        assert_eq!(hello, CONSTRUCTED_CLIENT_HELLO);
        stream
            .write_all(&CONSTRUCTED_SERVER_ACK)
            .expect("constructed acknowledgement");

        let mut requests = Vec::new();
        for expected_stream_id in 1..=2 {
            let encoded = read_resource_test_frame(&mut stream);
            let ResourceClientFrame::Request(request) =
                decode_resource_client_frame(&active, &registry, &encoded)
                    .expect("client resource request")
            else {
                panic!("the client sent a non-request resource frame");
            };
            assert_eq!(request.stream_id, expected_stream_id);
            let value = RuntimeValue::Integer(expected_stream_id as i32);
            let byte_count = encode_constructed_value(&active, &registry, &value)
                .expect("encoded resource value")
                .len() as u32;
            let frames = [
                ResourceServerFrame::Accepted(ResourceAccepted {
                    stream_id: request.stream_id,
                    request_id: request.request_id,
                    nested_invocation_id: InvocationId::from_bytes(
                        [0x30 + expected_stream_id as u8; 16],
                    ),
                    target_revision: request.target_revision,
                    resource_kind: ProtocolResourceKind::Single,
                }),
                ResourceServerFrame::Values(ResourceValues {
                    stream_id: request.stream_id,
                    request_id: request.request_id,
                    batch_sequence: 0,
                    item_count: 1,
                    byte_count,
                    values: vec![value],
                }),
                ResourceServerFrame::Completed(ResourceCompleted {
                    stream_id: request.stream_id,
                    request_id: request.request_id,
                    final_batch_sequence: 0,
                    total_items: 1,
                }),
            ];
            for frame in frames {
                let encoded = encode_resource_server_frame(&active, &registry, &frame)
                    .expect("encoded resource response");
                stream.write_all(&encoded).expect("resource response");
            }
            requests.push(request);
        }
        requests
    }

    fn run_scalar_test_request(
        runtime: &tokio::runtime::Runtime,
        transport: &mut ResourceTransportSource,
        active: &ActiveDatabaseRevision,
        registry: &orna_core::value::OpaqueCodecRegistry,
        request: ResourceRequest,
    ) -> RuntimeValue {
        let (stream, handshake_complete, protocol) = transport
            .take_connection()
            .expect("persistent transport connection");
        let (completion_sender, _completion_receiver) = mpsc::channel(1);
        let (_control_sender, controls) = mpsc::unbounded_channel();
        let run = runtime
            .block_on(run_resource_transport(
                stream,
                handshake_complete,
                protocol,
                active.clone(),
                registry.clone(),
                request,
                ResolvedType::Scalar(StandardScalar::Integer),
                ProtocolResourceKind::Single,
                controls,
                &completion_sender,
            ))
            .unwrap_or_else(|_| panic!("resource transport run"));
        let stream = run.stream.into_std().expect("restored resource stream");
        transport.restore_connection(stream, true, run.protocol);
        match run.outcome {
            ResourceTransportOutcome::Ready(value) => value,
            _ => panic!("unexpected non-ready scalar outcome"),
        }
    }

    #[test]
    fn persistent_transport_reuses_handshake_and_monotonic_stream_ids() {
        let (active, registry) = transport_test_context();
        let (peer, client) = StandardUnixStream::pair().expect("resource socket pair");
        let peer_active = active.clone();
        let peer_registry = registry.clone();
        let peer_thread =
            thread::spawn(move || serve_two_scalar_test_requests(peer, peer_active, peer_registry));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let mut transport = ResourceTransportSource::Injected(InjectedResourceTransport::Stream(
            PersistentResourceTransport {
                stream: Some(client),
                handshake_complete: false,
                protocol: ResourceProtocolConnection::new(),
                server_task: None,
            },
        ));

        assert_eq!(
            run_scalar_test_request(
                &runtime,
                &mut transport,
                &active,
                &registry,
                transport_test_request(active.pair(), 1),
            ),
            RuntimeValue::Integer(1),
        );
        assert_eq!(
            run_scalar_test_request(
                &runtime,
                &mut transport,
                &active,
                &registry,
                transport_test_request(active.pair(), 2),
            ),
            RuntimeValue::Integer(2),
        );
        let persistent = transport.persistent();
        assert_eq!(persistent.protocol.high_water_mark(), Some(2));
        assert!(persistent.stream.is_some());
        let requests = peer_thread.join().expect("resource peer");
        assert_eq!(
            requests
                .iter()
                .map(|request| request.stream_id)
                .collect::<Vec<_>>(),
            vec![1, 2],
        );
    }

    #[test]
    fn persistent_transport_reset_clears_state_after_transport_error() {
        let (active, _registry) = transport_test_context();
        let (_peer, client) = StandardUnixStream::pair().expect("resource socket pair");
        let mut source = ResourceTransportSource::Injected(InjectedResourceTransport::Stream(
            PersistentResourceTransport {
                stream: Some(client),
                handshake_complete: true,
                protocol: ResourceProtocolConnection::new(),
                server_task: None,
            },
        ));
        source
            .persistent()
            .protocol
            .open(transport_test_request(active.pair(), 1))
            .expect("resource protocol state");

        source.reset();
        let transport = source.persistent();
        assert!(transport.stream.is_none());
        assert!(!transport.handshake_complete);
        assert_eq!(transport.protocol.high_water_mark(), None);
        assert_eq!(transport.protocol.live_resources(), 0);
    }
}
