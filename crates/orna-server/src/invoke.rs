#![allow(clippy::clone_on_copy)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::question_mark)]
#![allow(clippy::match_like_matches_macro)]
// Invocation execution preserves the accepted error and carrier layouts.
#![allow(clippy::large_enum_variant)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
// Invocation operations return the stable embedded-host error boundary.
#![allow(clippy::result_large_err)]
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
//! `--explain` renders the resolution, sealed request, and local
//! sink/runtime offer facts and exits success without dispatching, authorising,
//! or auditing. Presenter candidates remain deferred because they are computed
//! only by the sealed route after target execution.
mod presentation;

use presentation::{presentation_error, render_explain, render_result};
#[cfg(test)]
use presentation::{
    render_event_stream, render_explain_final_sink, render_opaque_payload, render_return_type,
    render_value, select_runtime_sink,
};

use std::{
    cmp::Ordering,
    collections::BTreeMap,
    error::Error,
    fmt,
    future::Future,
    io::{self, IsTerminal, Write},
    os::unix::net::UnixStream as StandardUnixStream,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::Duration,
};

use orna_client::{
    ClientExecutionContext, ClientExternalContractRequest, ClientInspectOperation,
    ClientInspectRequest, ClientResourceCompletion, ClientResourceExecutor, ClientResourceRequest,
    DatabaseEndpoint, QtRuntimeExecutor, RuntimeLibrary, RuntimeSession,
};
use orna_core::inspect::{
    CallRow, INSPECT_RENDER_CARRIER_SIGNATURE, INSPECT_RENDER_CONTRACT, InspectInvocationNodeKind,
    InspectInvocationPhase, InspectObserverContext, InspectOutcomeKind, InspectPrivilege,
    InspectResourceKind, InspectResourceStatus, InspectResultSummary, InspectSecurityDecisionKind,
    InspectSecurityDecisionOutcome, InvocationNodeRow, PresentationCandidateRow, ResourceRow,
    RuntimeBindingRow, SecurityDecisionRow, StateCellRow, UiNodeRow,
};
use orna_core::inspect_carrier::{
    InspectCarrierEnvelope, InspectCarrierError, InspectCarrierKind, InspectCarrierProvenance,
};
use orna_core::{
    CatalogueRevisionId, FunctionId, FunctionRevisionId, InspectEpochId, InvocationId,
    SourceRevisionId, TypeId,
    catalogue::{
        FunctionDefinition, FunctionDomain, FunctionReturn, QualifiedSemanticName, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    },
    invocation::InvocationCarrierConstructionError,
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationEventBody, InvocationOutputRequirement, InvocationOutputTypeSelector,
        InvocationRuntimeContract, InvocationRuntimeOffer, InvocationSinkOffer,
        InvocationStreamingRequirement, InvocationTarget, InvocationTracePolicy, InvokeRequest,
        InvokeRequestInput,
    },
    invocation_binding::{CliArgumentInput, bind_cli_arguments},
    revision::{ActiveDatabaseRevision, ExecutableArtifactKind, VerifiedStandardLibrarySnapshot},
    security::AuthenticatedSession,
    system::{
        SYS_INSPECT_INVOCATION_TYPE_ID, SYS_INSPECT_SNAPSHOT_TYPE_ID, SYS_INVOKE_FUNCTION_ID,
    },
    types::{ResolvedType, StandardScalar, TypeDescriptor, TypeDescriptorKind},
    value::{
        ConstructedValueKind, OpaqueCodecRegistry, OpaqueValue, OpaqueValueError, RuntimeValue,
    },
};
use orna_postgres::{
    AuthenticatedInspectSnapshot, AuthenticatedServerResourceEvent,
    AuthenticatedServerResourceStart, PostgresKernel, PostgresKernelError, ResourceCancellation,
    ResourceCredit, SealedInvocationResult,
};
use orna_protocol::{
    CallFailure, Channel, ClientFrame, Event, InputRequested, InvocationEventRecord,
    MAX_CHANNEL_WINDOW, MAX_FRAME_PAYLOAD_LENGTH, MAX_RESOURCE_TOTAL_ITEMS, MAX_RESOURCE_WINDOW,
    MAX_SESSION_FRAME_LENGTH, ProtocolConnection, ResourceArgument, ResourceCancel,
    ResourceCancellationCode, ResourceClientFrame, ResourceKind as ProtocolResourceKind,
    ResourceProtocolConnection, ResourceRequest, ResourceServerFrame, ResourceWindowUpdate,
    SESSION_HEADER_LENGTH, SESSION_MARKER, ServerFrame, SessionClientFrame, SessionInputState,
    SessionServerFrame, SessionStateError, decode_constructed_invocation_event_frame,
    decode_constructed_server_frame, decode_constructed_value, decode_resource_server_frame,
    decode_session_server_frame, encode_constructed_client_frame, encode_constructed_value,
    encode_invoke_request, encode_resource_client_frame,
};
use orna_standard::{
    BINARY_LARGE_OBJECT_TYPE_ID, STD_IO_BYTE_STREAM_TYPE_ID, STD_TERMINAL_DOCUMENT_TYPE_ID,
    STD_UI_TYPE_ID, STD_UI_WINDOW_RUNTIME_CONTRACT, registered_opaque_codecs,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Notify;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender};

use crate::{
    EmbeddedHostError, LocalRawSocketResources, inspect_current_embedded_host,
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
/// The stable CLIENT failure code for an unavailable or unauthorised INSPECT epoch.
const INSPECT_DENIED_CODE: &str = "inspect.denied";

/// The sealed connection protocol major offered by every host run.
const CONNECTION_PROTOCOL_MAJOR: u16 = 5;
/// The minimum accepted client frame size (protocol version 1 offer limit).
const MAXIMUM_FRAME_SIZE: u32 = 1_024;
/// The first client run offers no artifact budget.
const MAXIMUM_ARTIFACT_SIZE: u64 = 0;
#[derive(Debug)]
enum InvokeTransport {
    InProcess,
    UnixSocket(PathBuf),
}
/// Parses one bounded command entered through `std.cli.evaluate`.
///
/// The grammar deliberately reuses the installed CLI binding model:
/// `qualified.function [--parameter=value ...]`. Values remain opaque strings
/// until the authenticated invocation binder converts them against the target
/// signature. Shell expansion and positional arguments are not supported.
fn parse_session_command(command: &str) -> Result<InstalledInvokeRequest, String> {
    let mut tokens = command.split_whitespace();
    let target = tokens
        .next()
        .and_then(|value| parse_session_target(value))
        .ok_or_else(|| "client.dynamic_invocation_invalid_command".to_owned())?;
    let mut arguments = Vec::new();
    for token in tokens {
        let pair = token
            .strip_prefix("--")
            .and_then(|value| value.split_once('='))
            .filter(|(name, _)| !name.is_empty())
            .ok_or_else(|| "client.dynamic_invocation_invalid_command".to_owned())?;
        arguments.push(CliArgumentInput::Friendly {
            name: pair.0.to_owned(),
            value: pair.1.to_owned(),
        });
    }
    Ok(InstalledInvokeRequest::new(
        target,
        arguments,
        None,
        None,
        true,
        false,
        Some(RuntimeFamily::Tty),
    ))
}

fn parse_session_target(value: &str) -> Option<InvocationTarget> {
    let mut parts = value.split('.');
    let first = parts.next()?;
    let rest = parts.collect::<Vec<_>>();
    if first.is_empty() || rest.is_empty() || rest.iter().any(|part| part.is_empty()) {
        return None;
    }
    let name = QualifiedSemanticName::new(std::iter::once(first).chain(rest)).ok()?;
    InvocationTarget::qualified_name(name).ok()
}

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
const QT_RUNTIME_FAMILY_NAME: &str = "qt";

/// The installed runtime family of one `orna invoke` run.
///
/// The local client selects between the accepted TTY and Qt offers. The
/// database plan receives pathless capability facts only.
#[allow(clippy::manual_non_exhaustive)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeFamily {
    /// The terminal runtime (`orna-runtime-tty`).
    Tty,
    /// The first production graphical runtime (`orna-runtime-qt`).
    Qt,
    /// A recognised-but-not-installed family used by fail-closed tests.
    #[doc(hidden)]
    NotInstalled,
}

impl RuntimeFamily {
    /// Parses one `--runtime <family>` override value.
    ///
    /// Only installed family names parse; an unknown name is `None`, which
    /// the command parser reports as a usage error.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            TTY_RUNTIME_NAME => Some(RuntimeFamily::Tty),
            QT_RUNTIME_FAMILY_NAME => Some(RuntimeFamily::Qt),
            _ => None,
        }
    }

    /// Returns the family name.
    pub fn name(self) -> &'static str {
        match self {
            RuntimeFamily::Tty => TTY_RUNTIME_NAME,
            RuntimeFamily::Qt => QT_RUNTIME_FAMILY_NAME,
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

/// Internal provenance for a resource terminal. This marker never crosses
/// ORNA-RESOURCE/1; it records whether the authenticated producer reached its
/// terminal commit so adapters can apply cancellation precedence consistently.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceTerminalProvenance {
    Uncommitted,
    Authenticated,
}

impl ResourceTerminalProvenance {
    pub(crate) fn is_committed(self) -> bool {
        matches!(self, Self::Authenticated)
    }
}

enum ResourceTransportSource {
    Authenticated(AuthenticatedResourceTransport),
    Injected(InjectedResourceTransport),
}

#[derive(Clone)]
pub(crate) struct SharedInvokeBroker {
    commands: UnboundedSender<BrokerCommand>,
    task: std::sync::Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    session_bridge: Arc<Mutex<Option<Arc<SessionBridge>>>>,
    resource_expectations: BrokerResourceExpectations,
    next_resource_stream_id: std::sync::Arc<std::sync::Mutex<u64>>,
    resource_terminal_provenance: BrokerResourceProvenance,
}

pub(crate) struct SessionBridge {
    root_invocation_id: InvocationId,
    call_stream: u64,
    outbound: UnboundedSender<SessionServerFrame>,
    outbound_receiver: Mutex<UnboundedReceiver<SessionServerFrame>>,
    outbound_notify: Notify,
    waiting: Mutex<SessionBridgeWaiting>,
    response_ready: Condvar,
}

struct SessionBridgeWaiting {
    state: SessionInputState,
    response: Option<SessionClientFrame>,
    closed: bool,
}

impl SessionBridge {
    pub(crate) fn new(
        root_invocation_id: InvocationId,
        call_stream: u64,
    ) -> Result<Arc<Self>, SessionStateError> {
        let state = SessionInputState::new(root_invocation_id, call_stream)?;
        let (outbound, outbound_receiver) = mpsc::unbounded_channel();
        Ok(Arc::new(Self {
            root_invocation_id,
            call_stream,
            outbound,
            outbound_receiver: Mutex::new(outbound_receiver),
            outbound_notify: Notify::new(),
            waiting: Mutex::new(SessionBridgeWaiting {
                state,
                response: None,
                closed: false,
            }),
            response_ready: Condvar::new(),
        }))
    }

    pub(crate) fn request_input(&self, root_invocation_id: InvocationId) -> Result<String, String> {
        let request_invocation_id = InvocationId::new();
        let frame = SessionServerFrame::InputRequested(InputRequested {
            root_invocation_id: self.root_invocation_id,
            call_stream: self.call_stream,
            request_invocation_id,
            prompt: String::new(),
        });
        {
            let mut waiting = self.waiting.lock().expect("session bridge waiting lock");
            if waiting.closed || root_invocation_id != self.root_invocation_id {
                return Err("client.input_unavailable".to_owned());
            }
            waiting
                .state
                .request(request_invocation_id)
                .map_err(|_| "client.input_unavailable".to_owned())?;
        }
        if self.outbound.send(frame).is_err() {
            self.close();
            return Err("client.input_unavailable".to_owned());
        }
        self.outbound_notify.notify_one();

        let mut waiting = self.waiting.lock().expect("session bridge waiting lock");
        while waiting.response.is_none() && !waiting.closed {
            waiting = self
                .response_ready
                .wait(waiting)
                .expect("session bridge response wait");
        }
        let Some(response) = waiting.response.take() else {
            return Err("client.input_unavailable".to_owned());
        };
        match response {
            SessionClientFrame::InputLine { line, .. } => Ok(line),
            SessionClientFrame::InputEof { .. } => Err("client.input_eof".to_owned()),
            SessionClientFrame::InputFailed { error, .. } => Err(error),
        }
    }

    pub(crate) fn accept_response(
        &self,
        frame: SessionClientFrame,
    ) -> Result<(), SessionStateError> {
        let mut waiting = self.waiting.lock().expect("session bridge waiting lock");
        if waiting.closed {
            return Err(SessionStateError::WrongState);
        }
        waiting.state.accept(&frame)?;
        waiting.response = Some(frame);
        if waiting.state.is_closed() {
            waiting.closed = true;
        }
        drop(waiting);
        self.response_ready.notify_all();
        Ok(())
    }

    pub(crate) fn close(&self) {
        let mut waiting = self.waiting.lock().expect("session bridge waiting lock");
        waiting.closed = true;
        if waiting.response.is_none() {
            if let Some(request_invocation_id) = waiting.state.pending_request() {
                waiting.response = Some(SessionClientFrame::InputFailed {
                    root_invocation_id: self.root_invocation_id,
                    call_stream: self.call_stream,
                    request_invocation_id,
                    error: "client.session_closed".to_owned(),
                });
            }
        }
        drop(waiting);
        self.response_ready.notify_all();
    }

    pub(crate) fn try_take_outbound(&self) -> Option<SessionServerFrame> {
        self.outbound_receiver
            .lock()
            .expect("session bridge outbound lock")
            .try_recv()
            .ok()
    }

    pub(crate) async fn wait_for_outbound(&self) {
        self.outbound_notify.notified().await;
    }
}

/// Test-only authorisation state for manually driven installed resource
/// sockets.
///
/// The installed host normally creates this state internally while it
/// drives a sealed invocation. Direct protocol tests must register the
/// exact resource requests that the client evaluator would have produced.
#[doc(hidden)]
#[cfg(feature = "test-hooks")]
#[derive(Clone)]
pub struct RawResourceRequestAuthorizer {
    broker: SharedInvokeBroker,
}

#[cfg(feature = "test-hooks")]
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

#[cfg(feature = "test-hooks")]
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
    AbandonResource {
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
    accepted_nested_invocation_id: Option<orna_core::InvocationId>,
    scalar_value: Option<RuntimeValue>,
    cancellation_requested: bool,
    stream_values_seen: bool,
    terminal_provenance: ResourceTerminalProvenance,
    scalar_value_after_cancellation: bool,
}

type BrokerResourceTombstones = BTreeMap<u64, orna_core::InvocationId>;
type BrokerResourceExpectations = std::sync::Arc<std::sync::Mutex<BTreeMap<u64, ResourceRequest>>>;
type BrokerResourceProvenance =
    Arc<Mutex<BTreeMap<(u64, orna_core::InvocationId), ResourceTerminalProvenance>>>;

const BROKER_RESOURCE_EXPECTATION_LOCK: &str = "broker resource expectation lock";
const BROKER_RESOURCE_STREAM_ID_LOCK: &str = "broker resource stream id lock";

impl SharedInvokeBroker {
    fn allocate_resource_stream_id(&self) -> Option<u64> {
        let mut next_stream_id = self
            .next_resource_stream_id
            .lock()
            .expect(BROKER_RESOURCE_STREAM_ID_LOCK);
        let stream_id = *next_stream_id;
        *next_stream_id = stream_id.checked_add(1)?;
        Some(stream_id)
    }

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

    pub(crate) fn take_expected_resource_request(&self, request: &ResourceRequest) -> bool {
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

    pub(crate) fn record_resource_terminal_provenance(
        &self,
        stream_id: u64,
        request_id: orna_core::InvocationId,
        provenance: ResourceTerminalProvenance,
    ) {
        if !provenance.is_committed() || stream_id == 0 || !valid_resource_invocation_id(request_id)
        {
            return;
        }
        self.resource_terminal_provenance
            .lock()
            .expect("broker resource provenance lock")
            .insert((stream_id, request_id), provenance);
    }

    fn resource_terminal_provenance(
        &self,
        stream_id: u64,
        request_id: orna_core::InvocationId,
    ) -> ResourceTerminalProvenance {
        self.resource_terminal_provenance
            .lock()
            .expect("broker resource provenance lock")
            .get(&(stream_id, request_id))
            .copied()
            .unwrap_or(ResourceTerminalProvenance::Uncommitted)
    }

    #[cfg(test)]
    fn take_resource_terminal_provenance(
        &self,
        stream_id: u64,
        request_id: orna_core::InvocationId,
    ) {
        remove_resource_terminal_provenance(
            &self.resource_terminal_provenance,
            stream_id,
            request_id,
        );
    }

    fn clear_resource_terminal_provenance(&self) {
        self.resource_terminal_provenance
            .lock()
            .expect("broker resource provenance lock")
            .clear();
    }
}

#[cfg(test)]
fn remove_resource_terminal_provenance(
    provenance: &BrokerResourceProvenance,
    stream_id: u64,
    request_id: orna_core::InvocationId,
) {
    provenance
        .lock()
        .expect("broker resource provenance lock")
        .remove(&(stream_id, request_id));
}

fn clear_resource_terminal_provenance_for_stream(
    provenance: &BrokerResourceProvenance,
    stream_id: u64,
) {
    provenance
        .lock()
        .expect("broker resource provenance lock")
        .retain(|entry, _| entry.0 != stream_id);
}

enum InjectedResourceTransport {
    /// A compatibility stream used by focused transport tests.
    Stream(PersistentResourceTransport),
}

struct PendingResourceTransport {
    request: ClientResourceRequest,
    stream_id: u64,
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
    /// Retained while a detached worker can still receive shutdown controls.
    request: ClientResourceRequest,
    control: Option<UnboundedSender<ResourceTransportControl>>,
    worker: thread::JoinHandle<()>,
    waiter: Option<thread::JoinHandle<()>>,
    completion: Option<std::sync::mpsc::Receiver<CancellationWaitOutcome>>,
    transport_return: std::sync::Arc<std::sync::Mutex<Option<ResourceTransportSource>>>,
}

struct DetachedBrokerResource {
    control: SharedInvokeBroker,
    stream_id: u64,
    request: ClientResourceRequest,
    waiter: Option<thread::JoinHandle<()>>,
    completion: Option<std::sync::mpsc::Receiver<CancellationWaitOutcome>>,
    abandoned: bool,
}

enum CancellationWaitOutcome {
    Terminal(Result<ResourceTransportOutcome, ResourceTransportFailure>),
    TimedOut,
}

struct CancellationWaiter {
    completion: std::sync::mpsc::Receiver<CancellationWaitOutcome>,
    thread: thread::JoinHandle<()>,
}

impl CancellationWaiter {
    fn start(
        receiver: Receiver<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
    ) -> Result<Self, Receiver<Result<ResourceTransportOutcome, ResourceTransportFailure>>> {
        let (sender, completion) = std::sync::mpsc::sync_channel(2);
        let receiver_slot = std::sync::Arc::new(std::sync::Mutex::new(Some(receiver)));
        let thread_receiver_slot = std::sync::Arc::clone(&receiver_slot);
        let thread = match thread::Builder::new()
            .name("orna-resource-cancel-waiter".to_owned())
            .spawn(move || {
                let timeout_sender = sender.clone();
                let mut receiver = thread_receiver_slot
                    .lock()
                    .expect("resource cancellation receiver lock")
                    .take()
                    .expect("resource cancellation receiver");
                let outcome = match tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(async move {
                        let receive_terminal = async {
                            loop {
                                match receiver.recv().await {
                                    Some(Ok(ResourceTransportOutcome::StreamValues(_))) => {}
                                    Some(result) => {
                                        break CancellationWaitOutcome::Terminal(result);
                                    }
                                    None => {
                                        break CancellationWaitOutcome::Terminal(Err(
                                            ResourceTransportFailure::Transport,
                                        ));
                                    }
                                }
                            }
                        };
                        match tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, receive_terminal).await {
                            Ok(outcome) => outcome,
                            Err(_) => {
                                let _ = timeout_sender.send(CancellationWaitOutcome::TimedOut);
                                loop {
                                    match receiver.recv().await {
                                        Some(Ok(ResourceTransportOutcome::StreamValues(_))) => {}
                                        Some(result) => {
                                            break CancellationWaitOutcome::Terminal(result);
                                        }
                                        None => {
                                            break CancellationWaitOutcome::Terminal(Err(
                                                ResourceTransportFailure::Transport,
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }),
                    Err(_) => {
                        CancellationWaitOutcome::Terminal(Err(ResourceTransportFailure::Transport))
                    }
                };
                let _ = sender.send(outcome);
            }) {
            Ok(thread) => thread,
            Err(_) => {
                return Err(receiver_slot
                    .lock()
                    .expect("resource cancellation receiver lock")
                    .take()
                    .expect("resource cancellation receiver"));
            }
        };
        Ok(Self { completion, thread })
    }

    fn wait(
        self,
    ) -> (
        Option<CancellationWaitOutcome>,
        Option<thread::JoinHandle<()>>,
        Option<std::sync::mpsc::Receiver<CancellationWaitOutcome>>,
    ) {
        let Self { completion, thread } = self;
        match completion.recv_timeout(RESOURCE_FRAME_TIMEOUT + Duration::from_secs(1)) {
            Ok(CancellationWaitOutcome::TimedOut) => (
                Some(CancellationWaitOutcome::TimedOut),
                Some(thread),
                Some(completion),
            ),
            Ok(outcome) => {
                let _ = thread.join();
                (Some(outcome), None, None)
            }
            Err(_) => (None, Some(thread), Some(completion)),
        }
    }
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

    /// Returns whether a worker-owned source can be reused after it returns.
    ///
    /// An injected source whose worker reset the stream has no usable
    /// connection left. Do not put that source back into the executor: doing
    /// so would make the next request fail through the normal pending path
    /// while presenting a transport that cannot actually be opened.
    fn can_restore(&self) -> bool {
        match self {
            // Authenticated sources open a fresh connection for each request.
            Self::Authenticated(_) => true,
            Self::Injected(InjectedResourceTransport::Stream(transport)) => {
                transport.stream.is_some()
            }
        }
    }

    fn take_connection(
        &mut self,
    ) -> Result<(StandardUnixStream, bool, ResourceProtocolConnection), ()> {
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
    inspect_kernel: Option<PostgresKernel>,
    inspect_session: Option<AuthenticatedSession>,
    current_invocation: Option<InvocationId>,
    next_stream_id: u64,
    broker: Option<SharedInvokeBroker>,
    raw_resource_authorizer: Option<SharedInvokeBroker>,
    transport: Option<ResourceTransportSource>,
    pending: Option<PendingResourceTransport>,
    broker_pending: Option<PendingBrokerResource>,
    detached: Vec<DetachedResourceTransport>,
    detached_broker: Option<DetachedBrokerResource>,
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
            inspect_kernel: Some(kernel.clone()),
            inspect_session: Some(session.clone()),
            current_invocation: None,
            next_stream_id: 1,
            broker: None,
            raw_resource_authorizer: None,
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
            detached_broker: None,
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
            inspect_kernel: None,
            inspect_session: None,
            current_invocation: None,
            next_stream_id: 1,
            broker: None,
            raw_resource_authorizer: None,
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
            detached_broker: None,
            cancellation: ResourceCancellation::new(),
        }
    }

    /// Creates an injected transport executor with an explicit test broker.
    ///
    /// The broker registers each generated protocol request immediately
    /// before the injected stream sends it. This preserves exact request
    /// provenance while allowing tests to use fresh request identifiers.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub fn new_with_stream_and_resource_authorizer(
        kernel: PostgresKernel,
        session: AuthenticatedSession,
        active: ActiveDatabaseRevision,
        stream: StandardUnixStream,
        authorizer: RawResourceRequestAuthorizer,
    ) -> Self {
        let mut executor = Self::new_with_stream(kernel, session, active, stream);
        executor.raw_resource_authorizer = Some(authorizer.broker());
        executor
    }

    #[doc(hidden)]
    pub(crate) fn new_with_broker(
        kernel: PostgresKernel,
        session: AuthenticatedSession,
        active: ActiveDatabaseRevision,
        broker: SharedInvokeBroker,
        cancellation: ResourceCancellation,
    ) -> Self {
        Self {
            active,
            inspect_kernel: Some(kernel),
            inspect_session: Some(session),
            current_invocation: None,
            next_stream_id: 1,
            broker: Some(broker),
            raw_resource_authorizer: None,
            transport: None,
            pending: None,
            broker_pending: None,
            detached: Vec::new(),
            detached_broker: None,
            cancellation,
        }
    }
    fn allocate_stream_id(&mut self) -> Option<u64> {
        if let Some(broker) = self.broker.as_ref() {
            return broker.allocate_resource_stream_id();
        }
        if let Some(authorizer) = self.raw_resource_authorizer.as_ref() {
            return authorizer.allocate_resource_stream_id();
        }
        let next_stream_id = self.next_stream_id.checked_add(1)?;
        let stream_id = self.next_stream_id;
        self.next_stream_id = next_stream_id;
        Some(stream_id)
    }

    fn discard_raw_resource_request(&self, stream_id: u64) {
        if let Some(broker) = self.broker.as_ref() {
            broker.discard_expected_resource_request(stream_id);
        }
        if let Some(authorizer) = self.raw_resource_authorizer.as_ref() {
            authorizer.discard_expected_resource_request(stream_id);
        }
    }

    fn poll_broker(&mut self) -> Option<ClientResourceCompletion> {
        if self.cancellation.is_requested() {
            if let Some(pending) = self.broker_pending.as_mut()
                && !pending.cancel_requested
            {
                let _ = pending
                    .control
                    .commands
                    .send(BrokerCommand::CancelResource {
                        stream_id: pending.stream_id,
                        request_id: pending.request.request_id(),
                        reason: ResourceCancellationCode::ParentInvocationCancelled,
                    });
                pending.cancel_requested = true;
            }
        }
        // Cancellation may leave an arbitrary number of queued stream batches;
        // drain them iteratively so a slow broker cannot grow the call stack.
        loop {
            let (result, cancel_requested, request) = {
                let pending = self.broker_pending.as_mut()?;
                let result = match pending.receiver.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => return None,
                    Err(TryRecvError::Disconnected) => Err(ResourceTransportFailure::Transport),
                };
                (result, pending.cancel_requested, pending.request.clone())
            };
            let stream_values = matches!(&result, Ok(ResourceTransportOutcome::StreamValues(_)));
            if stream_values && cancel_requested {
                continue;
            }
            if stream_values {
                return Some(map_resource_transport_completion(request, result));
            }
            let pending = self
                .broker_pending
                .take()
                .expect("broker pending checked above");
            self.discard_raw_resource_request(pending.stream_id);
            return Some(map_resource_transport_completion(pending.request, result));
        }
    }

    fn poll_detached(&mut self) -> Option<ClientResourceCompletion> {
        for index in 0..self.detached.len() {
            let result = self.detached[index]
                .completion
                .as_ref()
                .map(|completion| completion.try_recv());
            let Some(result) = result else {
                continue;
            };
            let outcome = match result {
                Ok(CancellationWaitOutcome::Terminal(outcome)) => outcome,
                Ok(CancellationWaitOutcome::TimedOut) => continue,
                Err(std::sync::mpsc::TryRecvError::Empty) => continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err(ResourceTransportFailure::Transport)
                }
            };
            let mut detached = self.detached.remove(index);
            if let Some(waiter) = detached.waiter.take() {
                let _ = waiter.join();
            }
            let _ = detached.worker.join();
            self.restore_transport(&detached.transport_return);
            return Some(map_resource_transport_completion(detached.request, outcome));
        }

        let Some(mut detached) = self.detached_broker.take() else {
            return None;
        };
        let result = detached
            .completion
            .as_ref()
            .map(|completion| completion.try_recv());
        let Some(result) = result else {
            self.detached_broker = Some(detached);
            return None;
        };
        let outcome = match result {
            Ok(CancellationWaitOutcome::Terminal(outcome)) => outcome,
            Ok(CancellationWaitOutcome::TimedOut) | Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.detached_broker = Some(detached);
                return None;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(ResourceTransportFailure::Transport)
            }
        };
        if let Some(waiter) = detached.waiter.take() {
            let _ = waiter.join();
        }
        self.discard_raw_resource_request(detached.stream_id);
        if !detached.abandoned {
            let _ = detached
                .control
                .commands
                .send(BrokerCommand::AbandonResource {
                    stream_id: detached.stream_id,
                    request_id: detached.request.request_id(),
                    reason: ResourceCancellationCode::RuntimeShutdown,
                });
        }
        Some(map_resource_transport_completion(detached.request, outcome))
    }

    fn reap_detached(&mut self) {
        let detached = std::mem::take(&mut self.detached);
        for mut resource in detached {
            if resource.completion.is_some() {
                self.detached.push(resource);
                continue;
            }
            let waiter_finished = resource
                .waiter
                .as_ref()
                .map(|waiter| waiter.is_finished())
                .unwrap_or(true);
            if waiter_finished && resource.worker.is_finished() {
                if let Some(waiter) = resource.waiter.take() {
                    let _ = waiter.join();
                }
                let _ = resource.worker.join();
                self.restore_transport(&resource.transport_return);
            } else {
                self.detached.push(resource);
            }
        }
        if let Some(mut resource) = self.detached_broker.take() {
            if resource.completion.is_some() {
                self.detached_broker = Some(resource);
                return;
            }
            let waiter_finished = resource
                .waiter
                .as_ref()
                .map(|waiter| waiter.is_finished())
                .unwrap_or(true);
            if waiter_finished {
                if let Some(waiter) = resource.waiter.take() {
                    let _ = waiter.join();
                }
                if !resource.abandoned {
                    let result = resource
                        .control
                        .commands
                        .send(BrokerCommand::AbandonResource {
                            stream_id: resource.stream_id,
                            request_id: resource.request.request_id(),
                            reason: ResourceCancellationCode::RuntimeShutdown,
                        });
                    if result.is_err() {
                        self.detached_broker = Some(resource);
                    } else {
                        resource.abandoned = true;
                    }
                }
            } else {
                self.detached_broker = Some(resource);
            }
        }
    }

    /// Starts a cancellation waiter without dropping the transport receiver
    /// when the synchronous caller reaches its bounded wait.
    fn wait_for_cancelled_transport(
        receiver: Receiver<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
    ) -> Result<
        (
            Option<CancellationWaitOutcome>,
            Option<thread::JoinHandle<()>>,
            Option<std::sync::mpsc::Receiver<CancellationWaitOutcome>>,
        ),
        Receiver<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
    > {
        Ok(CancellationWaiter::start(receiver)?.wait())
    }

    fn restore_transport(
        &mut self,
        transport_return: &std::sync::Arc<std::sync::Mutex<Option<ResourceTransportSource>>>,
    ) {
        if self.transport.is_some() {
            return;
        }
        let Some(transport) = transport_return
            .lock()
            .expect("resource transport return lock")
            .take()
        else {
            return;
        };
        if transport.can_restore() {
            self.transport = Some(transport);
        }
    }
}

const INSPECT_SNAPSHOT_ROW_TAG: u8 = 1;
/// Typed marker used in a classified row field when its classifier is absent.
///
/// Length-delimited text fields use this value in place of their u32 byte
/// length; optional fields use it in place of its presence byte. The value
/// is outside the ordinary field domain and therefore cannot be mistaken for
/// a caller-provided classified value.
const INSPECT_REDACTED_FIELD_TAG: u8 = 2;
const INSPECT_REDACTED_TEXT_LENGTH: u32 = u32::MAX;

/// The canonical client-plan header used by installed CLIENT artifacts.
///
/// The server depends on orna-client for execution, but this callback is a
/// separate trust boundary. Keep the narrow root decoder local so the callback
/// can authenticate the installed artifact body without widening the client
/// validator's public API.
const CLIENT_PLAN_MAGIC: &[u8; 8] = b"ORNACP\0\0";
const CLIENT_PLAN_EXPRESSION_VERSION: u32 = 3;
const CLIENT_PLAN_CAPABILITY_VERSION: u32 = 5;
const CLIENT_PLAN_EXPRESSION_OPERATION: u8 = 3;
const CLIENT_PLAN_EXTERNAL_CONTRACT_NODE: u8 = 8;
const CLIENT_PLAN_CAPABILITY_OPERATION: u8 = 5;

fn inspect_render_artifact_is_external(
    revision: &orna_core::revision::FunctionRevisionRecord,
) -> bool {
    let artifact = revision.artifact();
    if artifact.kind() != ExecutableArtifactKind::Client || artifact.format() != "orna.client-plan"
    {
        return false;
    }

    match artifact.version() {
        CLIENT_PLAN_EXPRESSION_VERSION => {
            client_expression_artifact_is_external(artifact.payload())
        }
        CLIENT_PLAN_CAPABILITY_VERSION => {
            let Some((inner_version, inner_payload)) =
                client_capability_inner_artifact(artifact.payload())
            else {
                return false;
            };
            inner_version == CLIENT_PLAN_EXPRESSION_VERSION
                && client_expression_artifact_is_external(inner_payload)
        }
        _ => false,
    }
}

fn client_expression_artifact_is_external(payload: &[u8]) -> bool {
    if payload.len() < 18
        || payload.get(..8) != Some(CLIENT_PLAN_MAGIC.as_slice())
        || u32::from_be_bytes(payload[8..12].try_into().expect("checked header width"))
            != CLIENT_PLAN_EXPRESSION_VERSION
        || payload[12] != CLIENT_PLAN_EXPRESSION_OPERATION
        || payload[13] != CLIENT_PLAN_EXTERNAL_CONTRACT_NODE
    {
        return false;
    }
    let identity_length =
        u32::from_be_bytes(payload[14..18].try_into().expect("checked identity width")) as usize;
    let identity_end = 18usize.saturating_add(identity_length);
    identity_end == payload.len()
        && &payload[18..identity_end] == INSPECT_RENDER_CONTRACT.as_bytes()
}

fn client_capability_inner_artifact(payload: &[u8]) -> Option<(u32, &[u8])> {
    if payload.len() < 21
        || payload.get(..8) != Some(CLIENT_PLAN_MAGIC.as_slice())
        || u32::from_be_bytes(payload[8..12].try_into().ok()?) != CLIENT_PLAN_CAPABILITY_VERSION
        || payload[12] != CLIENT_PLAN_CAPABILITY_OPERATION
    {
        return None;
    }
    let inner_version = u32::from_be_bytes(payload[13..17].try_into().ok()?);
    let inner_length = u32::from_be_bytes(payload[17..21].try_into().ok()?) as usize;
    let inner_end = 21usize.checked_add(inner_length)?;
    (inner_end == payload.len()).then(|| (inner_version, &payload[21..inner_end]))
}

fn run_installed_qt_external_contract(
    request: &ClientExternalContractRequest,
) -> Result<RuntimeValue, String> {
    let library =
        RuntimeLibrary::load_installed_qt().map_err(|_| "runtime.unavailable".to_owned())?;
    let session = RuntimeSession::new_qt(library, "en-GB", "UTC", "light")
        .map_err(|_| "runtime.unavailable".to_owned())?;
    let mut executor = QtRuntimeExecutor::new(session);
    let result = (|| {
        let value = ClientResourceExecutor::external_contract(&mut executor, request.clone())?;
        executor
            .wait_for_surfaces()
            .map_err(|_| "runtime.unavailable".to_owned())?;
        Ok::<RuntimeValue, String>(value)
    })();
    let shutdown = executor.shutdown();
    match result {
        Err(error) => Err(error),
        Ok(value) => {
            shutdown.map_err(|_| "runtime.unavailable".to_owned())?;
            Ok(value)
        }
    }
}

/// Evaluates the installed standard Inspector render contract without selecting a
/// graphical runtime or reading mutable state. The carrier envelope does not
/// encode its owning principal, so the full epoch is authenticated through the
/// installed session before the UI value is constructed.
async fn run_installed_external_contract(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    active: &ActiveDatabaseRevision,
    request: &ClientExternalContractRequest,
    current_invocation: Option<InvocationId>,
) -> Result<RuntimeValue, String> {
    if request.identity() == STD_UI_WINDOW_RUNTIME_CONTRACT {
        return run_installed_qt_external_contract(request);
    }
    if request.identity() != INSPECT_RENDER_CONTRACT {
        return Err("inspect.runtime_unavailable".to_owned());
    }
    require_current_observer_invocation(current_invocation, request.observer_root_invocation_id())?;
    if request.context().pair() != active.pair() {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    let Some(definition) = active
        .catalogue()
        .function_by_id(request.context().function())
    else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    let Some(revision) = active.function_revisions().iter().find(|revision| {
        revision.function() == request.context().function()
            && revision.id() == request.context().function_revision()
    }) else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    if request.context().function_revision() != definition.current_revision()
        || definition.domain() != FunctionDomain::Client
        || !matches!(
            definition.return_type(),
            FunctionReturn::Single(ResolvedType::Value(type_id)) if *type_id == STD_UI_TYPE_ID
        )
        || !inspect_render_artifact_is_external(revision)
    {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let arguments = request.arguments();
    if arguments.len() != INSPECT_RENDER_CARRIER_SIGNATURE.len()
        || definition.parameters().len() != INSPECT_RENDER_CARRIER_SIGNATURE.len()
    {
        return Err("inspect.malformed_carrier".to_owned());
    }

    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| "inspect.runtime_unavailable".to_owned())?;
    let registry =
        registered_opaque_codecs(standard).map_err(|_| "inspect.runtime_unavailable".to_owned())?;
    let mut epoch_id = None;
    let mut server_epoch = None;
    let mut server_root_target = None;
    let mut target_invocation_id = None;
    let mut row_counts = Vec::with_capacity(arguments.len());
    for (index, ((parameter_id, value), (expected_name, expected_type, expected_kind))) in arguments
        .iter()
        .zip(INSPECT_RENDER_CARRIER_SIGNATURE)
        .enumerate()
    {
        let parameter = &definition.parameters()[index];
        if parameter.id() != *parameter_id
            || parameter.name() != expected_name
            || parameter.resolved_type() != ResolvedType::Value(expected_type)
        {
            return Err("inspect.malformed_carrier".to_owned());
        }
        let RuntimeValue::Opaque(value) = value else {
            return Err("inspect.malformed_carrier".to_owned());
        };
        if value.opaque_type() != expected_type {
            return Err("inspect.unknown_carrier".to_owned());
        }
        let envelope = InspectCarrierEnvelope::decode(value.canonical_payload())
            .map_err(map_inspect_carrier_error)?;
        let _validated =
            OpaqueValue::new_inspect_carrier(active, expected_type, value.canonical_payload())
                .map_err(map_inspect_opaque_value_error)?;
        if envelope.carrier_kind() != expected_kind {
            return Err("inspect.malformed_carrier".to_owned());
        }
        if envelope.source_revision_id() != active.pair().source()
            || envelope.catalogue_revision_id() != active.pair().catalogue()
        {
            return Err("inspect.epoch_mismatch".to_owned());
        }
        let mut carrier_target = None;
        if expected_kind == InspectCarrierKind::Snapshot {
            if envelope.rows().len() != 1 {
                return Err("inspect.malformed_carrier".to_owned());
            }
            let snapshot_row = envelope
                .rows()
                .first()
                .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
            let (carrier_epoch, carrier_target_id, carrier_root_target) =
                decode_snapshot_row_epoch(active, &registry, snapshot_row, envelope.epoch_id())?;
            if server_epoch.is_some_and(|known| known != carrier_epoch) {
                return Err("inspect.epoch_mismatch".to_owned());
            }
            server_epoch = Some(carrier_epoch);
            if server_root_target.is_some_and(|known| known != carrier_root_target) {
                return Err("inspect.epoch_mismatch".to_owned());
            }
            server_root_target = Some(carrier_root_target);
            carrier_target = Some(carrier_target_id);
        } else {
            for row in envelope.rows() {
                let (carrier_epoch, target, root_target) = decode_enriched_inspect_row_target(
                    active,
                    &registry,
                    row,
                    expected_kind,
                    envelope.epoch_id(),
                )?;
                if server_epoch.is_some_and(|known| known != carrier_epoch) {
                    return Err("inspect.epoch_mismatch".to_owned());
                }
                server_epoch = Some(carrier_epoch);
                if server_root_target.is_some_and(|known| known != root_target) {
                    return Err("inspect.epoch_mismatch".to_owned());
                }
                server_root_target = Some(root_target);
                if carrier_target.is_some_and(|known| known != target) {
                    return Err("inspect.epoch_mismatch".to_owned());
                }
                carrier_target = Some(target);
            }
        }
        if let Some(carrier_target) = carrier_target {
            if target_invocation_id.is_some_and(|known| known != carrier_target) {
                return Err("inspect.epoch_mismatch".to_owned());
            }
            target_invocation_id = Some(carrier_target);
        }
        match epoch_id {
            Some(expected_epoch) if expected_epoch != envelope.epoch_id() => {
                return Err("inspect.epoch_mismatch".to_owned());
            }
            None => epoch_id = Some(envelope.epoch_id()),
            _ => {}
        }
        row_counts.push(envelope.rows().len());
    }

    let epoch_id = epoch_id.ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    // The required snapshot carrier is the epoch-bearing anchor. Empty
    // projections have no row payload, so their header epoch is checked
    // against this anchor and the authenticated snapshot below.
    let server_epoch = server_epoch.ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    if u64::from_be_bytes(
        server_epoch.to_bytes()[8..]
            .try_into()
            .expect("inspect epoch identity width"),
    ) != epoch_id
    {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    let target_invocation_id =
        target_invocation_id.ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    let root_target = server_root_target.ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    let observer_lineage = [
        request.observer_root_invocation_id(),
        request.observer_parent_invocation_id(),
    ];
    let loaded_snapshot = authorize_inspect_target_before_recursion(
        || async {
            let Some(snapshot) = kernel
                .load_inspect_snapshot(session, server_epoch)
                .await
                .map_err(inspect_kernel_error_code)?
            else {
                return Err(INSPECT_DENIED_CODE.to_owned());
            };
            validate_epoch(&snapshot, target_invocation_id, active.pair())?;
            require_inspect_root_provenance(snapshot.root_target(), root_target)?;
            Ok(snapshot)
        },
        target_invocation_id,
        &observer_lineage,
        |observer, target| async move {
            kernel
                .inspect_target_is_recursive(observer, target)
                .await
                .map_err(inspect_kernel_error_code)
        },
    )
    .await?;
    require_inspect_observer_context(
        loaded_snapshot.observer_context(),
        request.observer_root_invocation_id(),
        request.context().parent_invocation_id(),
    )?;
    let client_epoch_id = request.context().client_epoch_id().invocation_id();
    let body = serde_json::to_vec(&serde_json::json!({
        "kind": "node",
        "contract": {
            "id": "std.ui.window",
            "name": "std.ui.window",
            "version": "1.0",
        },
        "call_site_id": null,
        "function_instance_id": null,
        "key": {
            "type": "std.types.text",
            "value": format!("inspector-{client_epoch_id}-{epoch_id}"),
        },
        "properties": {
            "client_epoch": {
                "type": "std.types.text",
                "value": client_epoch_id.to_string(),
            },
            "server_epoch": {
                "type": "std.types.text",
                "value": epoch_id.to_string(),
            },
            "carrier_rows": {
                "type": "std.types.text",
                "value": row_counts
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            },
        },
        "slots": {},
        "actions": {},
    }))
    .map_err(|_| "inspect.projection_failed".to_owned())?;
    let body_length = u32::try_from(body.len()).map_err(|_| "inspect.limit".to_owned())?;
    let mut payload = b"ORNA-UI/1 ".to_vec();
    payload.extend_from_slice(&body_length.to_be_bytes());
    payload.extend_from_slice(&body);
    OpaqueValue::new(active, &registry, STD_UI_TYPE_ID, payload)
        .map(RuntimeValue::Opaque)
        .map_err(|_| "inspect.projection_failed".to_owned())
}

#[cfg(test)]
async fn reject_recursive_inspect_target<F, Fut>(
    target: InvocationId,
    observer_root: InvocationId,
    observer_parent: InvocationId,
    mut is_recursive: F,
) -> Result<(), String>
where
    F: FnMut(InvocationId, InvocationId) -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    if is_recursive(observer_root, target).await? {
        return Err("inspect.recursion".to_owned());
    }
    if observer_parent != observer_root && is_recursive(observer_parent, target).await? {
        return Err("inspect.recursion".to_owned());
    }
    Ok(())
}

async fn reject_recursive_inspect_lineage_target<F, Fut>(
    target: InvocationId,
    observer_lineage: &[InvocationId],
    mut is_recursive: F,
) -> Result<(), String>
where
    F: FnMut(InvocationId, InvocationId) -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    for observer in observer_lineage {
        if is_recursive(*observer, target).await? {
            return Err("inspect.recursion".to_owned());
        }
    }
    Ok(())
}

async fn authorize_inspect_target_before_recursion<T, A, AFut, F, Fut>(
    authorize: A,
    target: InvocationId,
    observer_lineage: &[InvocationId],
    is_recursive: F,
) -> Result<T, String>
where
    A: FnOnce() -> AFut,
    AFut: Future<Output = Result<T, String>>,
    F: FnMut(InvocationId, InvocationId) -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    let authorized = authorize().await?;
    reject_recursive_inspect_lineage_target(target, observer_lineage, is_recursive).await?;
    Ok(authorized)
}

fn map_inspect_carrier_error(error: InspectCarrierError) -> String {
    match error {
        InspectCarrierError::EnvelopeTooLarge { .. }
        | InspectCarrierError::RowCountExceeded { .. }
        | InspectCarrierError::RowTooLarge { .. }
        | InspectCarrierError::InvalidRow(
            orna_core::inspect_carrier::InspectRowError::PayloadTooLarge { .. },
        ) => "inspect.limit".to_owned(),
        InspectCarrierError::InvalidTargetInvocation => "inspect.invalid_target".to_owned(),
        InspectCarrierError::TargetInvocationMismatch { .. } => "inspect.epoch_mismatch".to_owned(),
        _ => "inspect.malformed_carrier".to_owned(),
    }
}

fn map_inspect_opaque_value_error(error: OpaqueValueError) -> String {
    match error {
        OpaqueValueError::UnregisteredType { .. } => "inspect.unknown_carrier".to_owned(),
        OpaqueValueError::InspectCarrierRevisionMismatch { .. } => {
            "inspect.epoch_mismatch".to_owned()
        }
        _ => "inspect.malformed_carrier".to_owned(),
    }
}

fn require_current_observer_invocation(
    current_invocation: Option<InvocationId>,
    observer_root: InvocationId,
) -> Result<InvocationId, String> {
    let Some(current_invocation) = current_invocation else {
        return Err("inspect.epoch_mismatch".to_owned());
    };
    if observer_root != current_invocation {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(current_invocation)
}

async fn run_installed_inspect(
    kernel: PostgresKernel,
    session: AuthenticatedSession,
    active: ActiveDatabaseRevision,
    request: ClientInspectRequest,
    current_invocation: Option<InvocationId>,
) -> Result<RuntimeValue, String> {
    let current_invocation = require_current_observer_invocation(
        current_invocation,
        request.observer_root_invocation_id(),
    )?;
    let observer_root = request.observer_root_invocation_id();
    // The enclosing server invocation is stable across the nested CLIENT
    // helper calls that make up one Inspector operation.
    let observer_parent = request.context().parent_invocation_id();

    validate_inspect_request_context(&request, &active)?;
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| "inspect.runtime_unavailable".to_owned())?;
    let registry =
        registered_opaque_codecs(standard).map_err(|_| "inspect.runtime_unavailable".to_owned())?;
    match request.operation() {
        ClientInspectOperation::Snapshot { target } => {
            // The installed v1 provider has no closed decoder for the opaque
            // snapshot-options carrier. Omitted options are the structural
            // default; reject supplied options rather than silently discarding
            // classifier bits.
            if request.snapshot_options().is_some() {
                return Err("inspect.invalid_options".to_owned());
            }
            let invocation = inspect_snapshot_request_target(target)?;
            require_inspect_target_provenance(request.target_invocation_id(), invocation)?;
            let authorization_kernel = kernel.clone();
            let authorization_session = session.clone();
            let authorization_pair = active.pair();
            let recursion_kernel = kernel.clone();
            let loaded_snapshot = authorize_inspect_target_before_recursion(
                move || async move {
                    let Some(epoch_id) = authorization_kernel
                        .find_latest_inspect_epoch(&authorization_session, invocation)
                        .await
                        .map_err(inspect_kernel_error_code)?
                    else {
                        return Err(INSPECT_DENIED_CODE.to_owned());
                    };
                    let Some(loaded_snapshot) = authorization_kernel
                        .load_inspect_snapshot(&authorization_session, epoch_id)
                        .await
                        .map_err(inspect_kernel_error_code)?
                    else {
                        return Err(INSPECT_DENIED_CODE.to_owned());
                    };
                    validate_epoch(&loaded_snapshot, invocation, authorization_pair)?;
                    Ok(loaded_snapshot)
                },
                invocation,
                request.observer_lineage(),
                move |observer, target| {
                    let kernel = recursion_kernel.clone();
                    async move {
                        kernel
                            .inspect_target_is_recursive(observer, target)
                            .await
                            .map_err(inspect_kernel_error_code)
                    }
                },
            )
            .await?;
            let observer_context = InspectObserverContext::new(observer_root, observer_parent)
                .map_err(|_| "inspect.epoch_mismatch".to_owned())?;
            let Some(loaded_snapshot) = kernel
                .clone_inspect_snapshot_for_current_invocation(
                    &session,
                    loaded_snapshot.id(),
                    observer_context,
                    current_invocation,
                )
                .await
                .map_err(inspect_kernel_error_code)?
            else {
                return Err(INSPECT_DENIED_CODE.to_owned());
            };
            let payload = make_inspect_carrier(
                &active,
                &registry,
                InspectCarrierKind::Snapshot,
                &loaded_snapshot,
                invocation,
                vec![encode_snapshot_row(&loaded_snapshot)],
                0,
            )?;
            make_opaque(&active, SYS_INSPECT_SNAPSHOT_TYPE_ID, payload)
        }
        ClientInspectOperation::Projection { snapshot, .. } => {
            let tag = request
                .operation()
                .projection_carrier_tag()
                .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
            let snapshot = match snapshot {
                RuntimeValue::Opaque(value)
                    if value.opaque_type() == SYS_INSPECT_SNAPSHOT_TYPE_ID =>
                {
                    value
                }
                RuntimeValue::Opaque(_) => return Err("inspect.unknown_carrier".to_owned()),
                _ => return Err("inspect.malformed_carrier".to_owned()),
            };
            let envelope = InspectCarrierEnvelope::decode(snapshot.canonical_payload())
                .map_err(map_inspect_carrier_error)?;
            if envelope.carrier_kind() != InspectCarrierKind::Snapshot {
                return Err("inspect.malformed_carrier".to_owned());
            }
            if envelope.source_revision_id() != active.pair().source()
                || envelope.catalogue_revision_id() != active.pair().catalogue()
            {
                return Err("inspect.epoch_mismatch".to_owned());
            }
            if envelope.rows().len() != 1 {
                return Err("inspect.malformed_carrier".to_owned());
            }
            let snapshot_row = envelope
                .rows()
                .first()
                .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
            let (epoch_id, target_invocation, root_target) =
                decode_snapshot_row_epoch(&active, &registry, snapshot_row, envelope.epoch_id())?;
            validate_inspect_projection_binding(
                request.target_invocation_id(),
                &envelope,
                epoch_id,
                target_invocation,
                active.pair(),
            )?;
            let authorization_kernel = kernel.clone();
            let authorization_session = session.clone();
            let authorization_pair = active.pair();
            let recursion_kernel = kernel.clone();
            let loaded_snapshot = authorize_inspect_target_before_recursion(
                move || async move {
                    let Some(_) = authorization_kernel
                        .find_inspect_epoch(&authorization_session, epoch_id)
                        .await
                        .map_err(inspect_kernel_error_code)?
                    else {
                        return Err(INSPECT_DENIED_CODE.to_owned());
                    };
                    let Some(loaded_snapshot) = authorization_kernel
                        .load_inspect_snapshot(&authorization_session, epoch_id)
                        .await
                        .map_err(inspect_kernel_error_code)?
                    else {
                        return Err(INSPECT_DENIED_CODE.to_owned());
                    };
                    validate_epoch(&loaded_snapshot, target_invocation, authorization_pair)?;
                    require_inspect_root_provenance(loaded_snapshot.root_target(), root_target)?;
                    require_inspect_observer_context(
                        loaded_snapshot.observer_context(),
                        observer_root,
                        observer_parent,
                    )?;
                    Ok(loaded_snapshot)
                },
                target_invocation,
                request.observer_lineage(),
                move |observer, target| {
                    let kernel = recursion_kernel.clone();
                    async move {
                        kernel
                            .inspect_target_is_recursive(observer, target)
                            .await
                            .map_err(inspect_kernel_error_code)
                    }
                },
            )
            .await?;
            let privilege = InspectPrivilege::OwnInvocation;
            let granted = [InspectPrivilege::OwnInvocation];
            let values_granted = inspect_classifier_granted(&granted, InspectPrivilege::Values);
            let security_details_granted =
                inspect_classifier_granted(&granted, InspectPrivilege::SecurityDetails);
            let runtime_internals_granted =
                inspect_classifier_granted(&granted, InspectPrivilege::RuntimeInternals);
            let source_granted = inspect_classifier_granted(&granted, InspectPrivilege::Source);
            // The installed v1 request carries only the structural
            // OwnInvocation grant. The epoch rows are immutable, but the
            // carrier boundary still enforces each classifier independently:
            // a future armed path may pass the matching protected grant, while
            // this ordinary path emits typed redaction markers.
            let rows = match tag {
                2 => encode_invocation_nodes(
                    &kernel
                        .inspect_invocation_nodes(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                ),
                3 => encode_calls(
                    &kernel
                        .inspect_calls(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                    values_granted,
                ),
                4 => encode_resources(
                    &kernel
                        .inspect_resources(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                ),
                5 => encode_state_cells(
                    &kernel
                        .inspect_state_cells(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                ),
                6 => encode_ui_nodes(
                    &kernel
                        .inspect_ui_nodes(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                    source_granted,
                    runtime_internals_granted,
                ),
                7 => encode_presentation_candidates(
                    &kernel
                        .inspect_presentation_candidates(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                    runtime_internals_granted,
                ),
                8 => encode_runtime_bindings(
                    &kernel
                        .inspect_runtime_bindings(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                    runtime_internals_granted,
                ),
                9 => encode_security_decisions(
                    &kernel
                        .inspect_security_decisions(&loaded_snapshot, privilege)
                        .await
                        .map_err(inspect_kernel_error_code)?,
                    security_details_granted,
                ),
                _ => return Err("inspect.malformed_carrier".to_owned()),
            }?;
            let kind = InspectCarrierKind::from_tag(tag)
                .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
            let payload = make_inspect_carrier(
                &active,
                &registry,
                kind,
                &loaded_snapshot,
                target_invocation,
                rows,
                inspect_classification_tag(kind, privilege),
            )?;
            make_opaque(&active, kind.type_id(), payload)
        }
    }
}

fn inspect_snapshot_request_target(target: &RuntimeValue) -> Result<InvocationId, String> {
    let RuntimeValue::Reference { target, object } = target else {
        return Err("inspect.invalid_target".to_owned());
    };
    let object_bytes = object.to_bytes();
    if *target != SYS_INSPECT_INVOCATION_TYPE_ID || object_bytes == [0; 16] {
        return Err("inspect.invalid_target".to_owned());
    }
    Ok(InvocationId::from_bytes(object_bytes))
}

fn validate_inspect_request_context(
    request: &ClientInspectRequest,
    active: &ActiveDatabaseRevision,
) -> Result<(), String> {
    if request.context().pair() != active.pair() {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

fn require_inspect_target_provenance(
    request_target: Option<InvocationId>,
    decoded_target: InvocationId,
) -> Result<(), String> {
    let Some(request_target) = request_target else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    if request_target != decoded_target {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

fn require_inspect_root_provenance(
    snapshot_root: FunctionId,
    decoded_root: FunctionId,
) -> Result<(), String> {
    if snapshot_root != decoded_root {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

fn require_inspect_observer_context(
    stored: Option<InspectObserverContext>,
    observer_root: InvocationId,
    observer_parent: InvocationId,
) -> Result<(), String> {
    let expected = InspectObserverContext::new(observer_root, observer_parent)
        .map_err(|_| "inspect.epoch_mismatch".to_owned())?;
    if stored != Some(expected) {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

fn validate_inspect_projection_binding(
    request_target: Option<InvocationId>,
    envelope: &InspectCarrierEnvelope,
    decoded_epoch: InspectEpochId,
    decoded_target: InvocationId,
    pair: orna_core::revision::RevisionPair,
) -> Result<(), String> {
    require_inspect_target_provenance(request_target, decoded_target)?;
    if envelope
        .target_invocation_id()
        .is_some_and(|target| target != decoded_target)
    {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    let decoded_epoch_id = u64::from_be_bytes(
        decoded_epoch.to_bytes()[8..]
            .try_into()
            .expect("inspect epoch identity width"),
    );
    if envelope.epoch_id() != decoded_epoch_id {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    if envelope.source_revision_id() != pair.source()
        || envelope.catalogue_revision_id() != pair.catalogue()
    {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

fn inspect_kernel_error_code(error: PostgresKernelError) -> String {
    match error {
        PostgresKernelError::InspectDenied { reason } => match reason {
            orna_core::security::InspectDenial::MissingEpoch
            | orna_core::security::InspectDenial::MissingPrivilege => {
                INSPECT_DENIED_CODE.to_owned()
            }
            orna_core::security::InspectDenial::ObserverSuppressed => {
                "inspect.recursion".to_owned()
            }
        },
        _ => "inspect.projection_failed".to_owned(),
    }
}

fn validate_epoch(
    snapshot: &AuthenticatedInspectSnapshot,
    invocation: InvocationId,
    pair: orna_core::revision::RevisionPair,
) -> Result<(), String> {
    if snapshot.invocation_id() != invocation {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    if snapshot.source_revision_id() != pair.source()
        || snapshot.catalogue_revision_id() != pair.catalogue()
    {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    Ok(())
}

fn make_opaque(
    active: &ActiveDatabaseRevision,
    opaque_type: TypeId,
    payload: Vec<u8>,
) -> Result<RuntimeValue, String> {
    OpaqueValue::new_inspect_carrier(active, opaque_type, payload)
        .map(RuntimeValue::Opaque)
        .map_err(|_| "inspect.projection_failed".to_owned())
}
trait InspectCarrierSnapshot {
    fn id(&self) -> InspectEpochId;
    fn invocation_id(&self) -> InvocationId;
    fn root_target(&self) -> FunctionId;
    fn source_revision_id(&self) -> SourceRevisionId;
    fn catalogue_revision_id(&self) -> CatalogueRevisionId;
}

impl InspectCarrierSnapshot for AuthenticatedInspectSnapshot {
    fn id(&self) -> InspectEpochId {
        AuthenticatedInspectSnapshot::id(self)
    }

    fn invocation_id(&self) -> InvocationId {
        AuthenticatedInspectSnapshot::invocation_id(self)
    }

    fn root_target(&self) -> FunctionId {
        AuthenticatedInspectSnapshot::root_target(self)
    }

    fn source_revision_id(&self) -> SourceRevisionId {
        AuthenticatedInspectSnapshot::source_revision_id(self)
    }

    fn catalogue_revision_id(&self) -> CatalogueRevisionId {
        AuthenticatedInspectSnapshot::catalogue_revision_id(self)
    }
}

#[cfg(test)]
impl InspectCarrierSnapshot for orna_core::inspect::InspectSnapshotEpoch {
    fn id(&self) -> InspectEpochId {
        self.id()
    }

    fn invocation_id(&self) -> InvocationId {
        self.invocation_id()
    }

    fn root_target(&self) -> FunctionId {
        self.root_target()
    }

    fn source_revision_id(&self) -> SourceRevisionId {
        self.source_revision_id()
    }

    fn catalogue_revision_id(&self) -> CatalogueRevisionId {
        self.catalogue_revision_id()
    }
}

fn make_inspect_carrier(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    kind: InspectCarrierKind,
    snapshot: &impl InspectCarrierSnapshot,
    target_invocation: InvocationId,
    rows: Vec<Vec<u8>>,
    classification: u8,
) -> Result<Vec<u8>, String> {
    let epoch_id = u64::from_be_bytes(snapshot.id().to_bytes()[8..].try_into().expect("epoch id"));
    let mut encoded_rows = rows
        .into_iter()
        .map(|row| {
            let row = if kind == InspectCarrierKind::Snapshot {
                row
            } else {
                enrich_inspect_row(snapshot, row, classification)
            };
            encode_inspect_row(active, registry, row)
        })
        .collect::<Result<Vec<_>, _>>()?;
    encoded_rows.sort_unstable();
    InspectCarrierEnvelope::new_with_target(
        kind,
        target_invocation,
        InspectCarrierProvenance::trusted(
            epoch_id,
            snapshot.source_revision_id(),
            snapshot.catalogue_revision_id(),
        ),
        encoded_rows,
    )
    .and_then(|envelope| envelope.encode())
    .map_err(|_| "inspect.projection_failed".to_owned())
}

/// Wraps one accepted Inspector identity payload in the canonical ORV5
/// constructed-value codec. The list descriptor and byte child are fixed so
/// every projection has one deterministic row representation while the
/// existing row payload remains intact inside the ORV5 value.
fn encode_inspect_row(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    row: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let descriptor = TypeDescriptor::list(TypeDescriptor::named(BINARY_LARGE_OBJECT_TYPE_ID))
        .map_err(|_| "inspect.projection_failed".to_owned())?;
    let value = RuntimeValue::list(active, descriptor, vec![RuntimeValue::Bytes(row)])
        .map_err(|_| "inspect.projection_failed".to_owned())?;
    encode_constructed_value(active, registry, &value)
        .map_err(|_| "inspect.projection_failed".to_owned())
}

fn row(tag: u8, index: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(128);
    bytes.push(tag);
    bytes.extend_from_slice(&(index as u64).to_be_bytes());
    bytes
}

/// Adds the canonical common row evidence required by ADR 0080. The
/// projection-specific encoders retain their complete row fields after this
/// fixed header; all identities use their full sixteen-byte form.
fn enrich_inspect_row(
    snapshot: &impl InspectCarrierSnapshot,
    row: Vec<u8>,
    classification: u8,
) -> Vec<u8> {
    if row.len() < 9 {
        return row;
    }
    let mut enriched = Vec::with_capacity(row.len() + 82);
    enriched.extend_from_slice(&row[..9]);
    // Fixed common prefix: epoch identity, target invocation, root target,
    // pinned revisions, own-invocation scope, and classifier evidence. The
    // owner principal is deliberately not copied: the scope fact is enough
    // for this CLIENT carrier and the principal is security-classified.
    enriched.extend_from_slice(&snapshot.id().to_bytes());
    enriched.extend_from_slice(&snapshot.invocation_id().to_bytes());
    enriched.extend_from_slice(&snapshot.root_target().to_bytes());
    enriched.extend_from_slice(&snapshot.source_revision_id().to_bytes());
    enriched.extend_from_slice(&snapshot.catalogue_revision_id().to_bytes());
    enriched.push(1);
    enriched.push(classification);
    enriched.extend_from_slice(&row[9..]);
    enriched
}

fn inspect_classification_tag(kind: InspectCarrierKind, privilege: InspectPrivilege) -> u8 {
    match (kind, privilege) {
        (InspectCarrierKind::StateCells, InspectPrivilege::Values) => 1,
        (InspectCarrierKind::SecurityDecisions, InspectPrivilege::SecurityDetails) => 3,
        (InspectCarrierKind::RuntimeBindings, InspectPrivilege::RuntimeInternals) => 4,
        _ => 0,
    }
}

fn inspect_classifier_granted(granted: &[InspectPrivilege], classifier: InspectPrivilege) -> bool {
    debug_assert!(classifier.is_classifier());
    granted.contains(&classifier)
}

fn encode_classified_text(bytes: &mut Vec<u8>, value: &str, granted: bool) -> Result<(), String> {
    if !granted {
        bytes.extend_from_slice(&INSPECT_REDACTED_TEXT_LENGTH.to_be_bytes());
        return Ok(());
    }
    text(bytes, value)
}

fn encode_classified_optional_text(
    bytes: &mut Vec<u8>,
    value: Option<&str>,
    granted: bool,
) -> Result<(), String> {
    if !granted {
        bytes.push(INSPECT_REDACTED_FIELD_TAG);
        return Ok(());
    }
    match value {
        Some(value) => {
            bytes.push(1);
            text(bytes, value)?;
        }
        None => bytes.push(0),
    }
    Ok(())
}

/// Encodes an optional descriptor with the persisted projection convention.
///
/// A denied classified field gets the same marker used by the other optional
/// classified fields, with no presence bit or descriptor bytes. When the
/// classifier is granted, the field uses the persisted TypeDescriptor tags so
/// the selected sink remains a complete canonical descriptor.
fn encode_classified_optional_descriptor(
    bytes: &mut Vec<u8>,
    value: Option<&TypeDescriptor>,
    granted: bool,
) -> Result<(), String> {
    if !granted {
        bytes.push(INSPECT_REDACTED_FIELD_TAG);
        return Ok(());
    }
    match value {
        Some(value) => {
            bytes.push(1);
            encode_type_descriptor(bytes, value)?;
        }
        None => bytes.push(0),
    }
    Ok(())
}

/// Encodes a TypeDescriptor using the canonical persisted projection tags.
fn encode_type_descriptor(bytes: &mut Vec<u8>, descriptor: &TypeDescriptor) -> Result<(), String> {
    match descriptor.kind() {
        TypeDescriptorKind::Named(type_id) => {
            bytes.push(0);
            id(bytes, &type_id.to_bytes());
        }
        TypeDescriptorKind::Reference(type_id) => {
            bytes.push(1);
            id(bytes, &type_id.to_bytes());
        }
        TypeDescriptorKind::List(element) => {
            bytes.push(2);
            encode_type_descriptor(bytes, element)?;
        }
        TypeDescriptorKind::Set(element) => {
            bytes.push(3);
            encode_type_descriptor(bytes, element)?;
        }
        TypeDescriptorKind::Map { key, value } => {
            bytes.push(4);
            encode_type_descriptor(bytes, key)?;
            encode_type_descriptor(bytes, value)?;
        }
        TypeDescriptorKind::Option(value) => {
            bytes.push(5);
            encode_type_descriptor(bytes, value)?;
        }
        TypeDescriptorKind::Stream(element) => {
            bytes.push(6);
            encode_type_descriptor(bytes, element)?;
        }
    }
    Ok(())
}

fn id(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend_from_slice(value);
}
fn text(bytes: &mut Vec<u8>, value: &str) -> Result<(), String> {
    if value.len() > 65_536 {
        return Err("inspect.projection_failed".to_owned());
    }
    bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn encode_snapshot_row(snapshot: &AuthenticatedInspectSnapshot) -> Vec<u8> {
    let mut bytes = row(INSPECT_SNAPSHOT_ROW_TAG, 0);
    id(&mut bytes, &snapshot.id().to_bytes());
    id(&mut bytes, &snapshot.invocation_id().to_bytes());
    id(&mut bytes, &snapshot.root_target().to_bytes());
    bytes.push(match snapshot.outcome() {
        InspectOutcomeKind::Allowed => 1,
        InspectOutcomeKind::Denied => 2,
        InspectOutcomeKind::Failed => 3,
        InspectOutcomeKind::Cancelled => 4,
    });
    let summary = snapshot.summary();
    bytes.extend_from_slice(&summary.event_count().to_be_bytes());
    match summary.result() {
        InspectResultSummary::NoValues => bytes.push(0),
        InspectResultSummary::ValueBatch { value_count } => {
            bytes.push(1);
            bytes.extend_from_slice(&value_count.to_be_bytes());
        }
    }
    match summary.duration_nanoseconds() {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        None => bytes.push(0),
    }
    bytes
}
fn decode_enriched_inspect_row_target(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    row: &[u8],
    expected_kind: InspectCarrierKind,
    epoch_id: u64,
) -> Result<(InspectEpochId, InvocationId, FunctionId), String> {
    let value = decode_constructed_value(active, registry, row)
        .map_err(|_| "inspect.malformed_carrier".to_owned())?;
    let RuntimeValue::Constructed(constructed) = value else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    let TypeDescriptorKind::List(child) = constructed.descriptor().kind() else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    if child.kind() != TypeDescriptorKind::Named(BINARY_LARGE_OBJECT_TYPE_ID) {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let ConstructedValueKind::List(values) = constructed.kind() else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    let [RuntimeValue::Bytes(payload)] = values else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    if payload.len() < 91 || payload[0] != expected_kind.tag() {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let epoch = InspectEpochId::from_bytes(
        payload[9..25]
            .try_into()
            .map_err(|_| "inspect.malformed_carrier".to_owned())?,
    );
    if u64::from_be_bytes(epoch.to_bytes()[8..].try_into().expect("epoch id")) != epoch_id {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    if payload[57..73] != active.pair().source().to_bytes()
        || payload[73..89] != active.pair().catalogue().to_bytes()
    {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    if payload[89] != 1 || payload[90] > 4 {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let target = InvocationId::from_bytes(
        payload[25..41]
            .try_into()
            .map_err(|_| "inspect.malformed_carrier".to_owned())?,
    );
    if target.to_bytes() == [0; 16] {
        return Err("inspect.invalid_target".to_owned());
    }
    let root_target = FunctionId::from_bytes(
        payload[41..57]
            .try_into()
            .map_err(|_| "inspect.malformed_carrier".to_owned())?,
    );
    Ok((epoch, target, root_target))
}

fn decode_snapshot_row_epoch(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    row: &[u8],
    epoch_id: u64,
) -> Result<(InspectEpochId, InvocationId, FunctionId), String> {
    let value = decode_constructed_value(active, registry, row)
        .map_err(|_| "inspect.malformed_carrier".to_owned())?;
    let RuntimeValue::Constructed(constructed) = value else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    let TypeDescriptorKind::List(child) = constructed.descriptor().kind() else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    if child.kind() != TypeDescriptorKind::Named(BINARY_LARGE_OBJECT_TYPE_ID) {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let ConstructedValueKind::List(values) = constructed.kind() else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    let [RuntimeValue::Bytes(payload)] = values else {
        return Err("inspect.malformed_carrier".to_owned());
    };
    decode_snapshot_row_payload(payload, epoch_id)
}

fn decode_snapshot_row_payload(
    row: &[u8],
    epoch_id: u64,
) -> Result<(InspectEpochId, InvocationId, FunctionId), String> {
    if row.len() < 68 || row[0] != INSPECT_SNAPSHOT_ROW_TAG || row[1..9] != [0; 8] {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let bytes: [u8; 16] = row[9..25]
        .try_into()
        .map_err(|_| "inspect.malformed_carrier".to_owned())?;
    let id = InspectEpochId::from_bytes(bytes);
    if u64::from_be_bytes(id.to_bytes()[8..].try_into().expect("epoch id")) != epoch_id {
        return Err("inspect.epoch_mismatch".to_owned());
    }
    let invocation = InvocationId::from_bytes(
        row[25..41]
            .try_into()
            .map_err(|_| "inspect.malformed_carrier".to_owned())?,
    );
    if invocation.to_bytes() == [0; 16] {
        return Err("inspect.invalid_target".to_owned());
    }
    let root_target = FunctionId::from_bytes(
        row[41..57]
            .try_into()
            .map_err(|_| "inspect.malformed_carrier".to_owned())?,
    );
    let mut offset = 57;
    let outcome = *row
        .get(offset)
        .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    if !(1..=4).contains(&outcome) {
        return Err("inspect.malformed_carrier".to_owned());
    }
    offset += 1 + 8;
    let result = *row
        .get(offset)
        .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    offset += 1;
    if result == 1 {
        let value_count = row
            .get(offset..)
            .and_then(|bytes| bytes.get(..8))
            .and_then(|bytes| bytes.try_into().ok())
            .map(u64::from_be_bytes)
            .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
        if value_count == 0 {
            return Err("inspect.malformed_carrier".to_owned());
        }
        offset += 8;
    } else if result != 0 {
        return Err("inspect.malformed_carrier".to_owned());
    }
    let duration = *row
        .get(offset)
        .ok_or_else(|| "inspect.malformed_carrier".to_owned())?;
    offset += 1;
    if duration == 1 {
        offset += 8;
    } else if duration != 0 {
        return Err("inspect.malformed_carrier".to_owned());
    }
    if offset != row.len() {
        return Err("inspect.malformed_carrier".to_owned());
    }
    Ok((id, invocation, root_target))
}

fn encode_invocation_nodes(rows: &[InvocationNodeRow]) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(2, index);
            id(&mut bytes, &value.id().to_bytes());
            match value.parent_id() {
                Some(parent) => {
                    bytes.push(1);
                    id(&mut bytes, &parent.to_bytes());
                }
                None => bytes.push(0),
            };
            bytes.push(match value.kind() {
                InspectInvocationNodeKind::Root => 1,
                InspectInvocationNodeKind::Nested => 2,
            });
            bytes.push(match value.phase() {
                InspectInvocationPhase::Started => 1,
                InspectInvocationPhase::Executing => 2,
                InspectInvocationPhase::Completed => 3,
                InspectInvocationPhase::Failed => 4,
                InspectInvocationPhase::Cancelled => 5,
            });
            id(&mut bytes, &value.target().to_bytes());
            bytes.extend_from_slice(&value.sequence().to_be_bytes());
            Ok(bytes)
        })
        .collect()
}
fn encode_calls(rows: &[CallRow], values_granted: bool) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(3, index);
            id(&mut bytes, &value.invocation_id().to_bytes());
            bytes.push(u8::from(values_granted && value.schema().is_some()));
            bytes.extend_from_slice(&value.value_count().to_be_bytes());
            bytes.extend_from_slice(&value.duration_nanoseconds().to_be_bytes());
            Ok(bytes)
        })
        .collect()
}
fn encode_resources(rows: &[ResourceRow]) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(4, index);
            bytes.push(match value.kind() {
                InspectResourceKind::State => 1,
                InspectResourceKind::Catalog => 2,
                InspectResourceKind::Standard => 3,
                InspectResourceKind::Runtime => 4,
            });
            bytes.push(match value.status() {
                InspectResourceStatus::Active => 1,
                InspectResourceStatus::Invalidated => 2,
                InspectResourceStatus::Released => 3,
            });
            Ok(bytes)
        })
        .collect()
}
fn encode_state_cells(rows: &[StateCellRow]) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let key = value.key();
            let mut bytes = row(5, index);
            id(&mut bytes, &key.root_function().to_bytes());
            text(&mut bytes, key.state_profile())?;
            id(&mut bytes, &key.function().to_bytes());
            text(&mut bytes, key.instance_key())?;
            id(&mut bytes, &key.state_slot().to_bytes());
            id(&mut bytes, &value.value_type().to_bytes());
            bytes.extend_from_slice(&value.revision().to_be_bytes());
            bytes.push(u8::from(value.value().is_some()));
            Ok(bytes)
        })
        .collect()
}
fn encode_ui_nodes(
    rows: &[UiNodeRow],
    source_granted: bool,
    runtime_internals_granted: bool,
) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(6, index);
            id(&mut bytes, &value.function().to_bytes());
            encode_classified_text(&mut bytes, value.call_site(), source_granted)?;
            encode_classified_text(
                &mut bytes,
                value.runtime_contract(),
                runtime_internals_granted,
            )?;
            Ok(bytes)
        })
        .collect()
}
fn encode_presentation_candidates(
    rows: &[PresentationCandidateRow],
    runtime_internals_granted: bool,
) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(7, index);
            encode_classified_text(&mut bytes, value.presenter(), runtime_internals_granted)?;
            bytes.push(u8::from(value.accepted()));
            encode_classified_text(&mut bytes, value.reason(), runtime_internals_granted)?;
            encode_classified_optional_descriptor(
                &mut bytes,
                value.selected_sink(),
                runtime_internals_granted,
            )?;
            encode_classified_optional_text(
                &mut bytes,
                value.runtime(),
                runtime_internals_granted,
            )?;
            Ok(bytes)
        })
        .collect()
}
fn encode_runtime_bindings(
    rows: &[RuntimeBindingRow],
    runtime_internals_granted: bool,
) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(8, index);
            if !runtime_internals_granted {
                bytes.push(INSPECT_REDACTED_FIELD_TAG);
                return Ok(bytes);
            }
            text(&mut bytes, value.runtime_name())?;
            text(&mut bytes, value.version())?;
            bytes.push(u8::from(value.trusted()));
            bytes.extend_from_slice(&value.preference_rank().to_be_bytes());
            bytes.extend_from_slice(&(value.consumed_descriptors().len() as u32).to_be_bytes());
            bytes.extend_from_slice(&(value.contracts().len() as u32).to_be_bytes());
            Ok(bytes)
        })
        .collect()
}
fn encode_security_decisions(
    rows: &[SecurityDecisionRow],
    security_details_granted: bool,
) -> Result<Vec<Vec<u8>>, String> {
    rows.iter()
        .enumerate()
        .map(|(index, value)| {
            let mut bytes = row(9, index);
            bytes.push(match value.kind() {
                InspectSecurityDecisionKind::Execute => 1,
                InspectSecurityDecisionKind::Capability => 2,
                InspectSecurityDecisionKind::UserState => 3,
                InspectSecurityDecisionKind::Inspect => 4,
            });
            bytes.push(match value.outcome() {
                InspectSecurityDecisionOutcome::Allowed => 1,
                InspectSecurityDecisionOutcome::Denied => 2,
            });
            match value.target() {
                Some(target) => {
                    bytes.push(1);
                    text(&mut bytes, &target.canonical())?;
                }
                None => bytes.push(0),
            }
            if !security_details_granted {
                bytes.push(INSPECT_REDACTED_FIELD_TAG);
                return Ok(bytes);
            }
            match value.denial_reason() {
                Some(reason) => {
                    bytes.push(1);
                    text(&mut bytes, reason)?;
                }
                None => bytes.push(0),
            }
            bytes.extend_from_slice(&(value.principals().len() as u32).to_be_bytes());
            for principal in value.principals() {
                text(&mut bytes, &principal.canonical())?;
            }
            bytes.extend_from_slice(&(value.audit_refs().len() as u32).to_be_bytes());
            for event in value.audit_refs() {
                text(&mut bytes, &event.canonical())?;
            }
            Ok(bytes)
        })
        .collect()
}

fn same_resource_request_identity(
    expected: &ClientResourceRequest,
    actual: &ClientResourceRequest,
) -> bool {
    expected.request_id() == actual.request_id()
        && expected.key() == actual.key()
        && expected.generation() == actual.generation()
}

impl ClientResourceExecutor for InstalledClientResourceExecutor {
    fn bind_current_invocation(&mut self, invocation: InvocationId) {
        self.current_invocation = Some(invocation);
    }

    fn read_input(&mut self, context: ClientExecutionContext) -> Result<RuntimeValue, String> {
        let Some(broker) = self.broker.as_ref() else {
            return Err("client.input_unavailable".to_owned());
        };
        let Some(bridge) = broker.session_bridge() else {
            return Err("client.input_unavailable".to_owned());
        };
        let root_invocation_id = self
            .current_invocation
            .unwrap_or_else(|| context.parent_invocation_id());
        bridge
            .request_input(root_invocation_id)
            .map(RuntimeValue::Text)
    }

    fn inspect(&mut self, request: ClientInspectRequest) -> Result<RuntimeValue, String> {
        let (Some(kernel), Some(session)) =
            (self.inspect_kernel.clone(), self.inspect_session.clone())
        else {
            return Err("inspect.runtime_unavailable".to_owned());
        };
        if self.cancellation.is_requested() {
            return Err("inspect.cancelled".to_owned());
        }
        let active = self.active.clone();
        let current_invocation = self.current_invocation;
        let cancellation = self.cancellation.clone();
        let result = thread::Builder::new()
            .name("orna-inspect".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "inspect.runtime_unavailable".to_owned())?;
                let result = runtime.block_on(run_installed_inspect(
                    kernel,
                    session,
                    active,
                    request,
                    current_invocation,
                ));
                if cancellation.is_requested() {
                    Err("inspect.cancelled".to_owned())
                } else {
                    result
                }
            })
            .map_err(|_| "inspect.runtime_unavailable".to_owned())?
            .join()
            .map_err(|_| "inspect.runtime_unavailable".to_owned())?;
        if self.cancellation.is_requested() {
            return Err("inspect.cancelled".to_owned());
        }
        result
    }
    fn external_contract(
        &mut self,
        request: ClientExternalContractRequest,
    ) -> Result<RuntimeValue, String> {
        let (Some(kernel), Some(session)) =
            (self.inspect_kernel.clone(), self.inspect_session.clone())
        else {
            return Err("inspect.runtime_unavailable".to_owned());
        };
        if self.cancellation.is_requested() {
            return Err("inspect.cancelled".to_owned());
        }
        let active = self.active.clone();
        let cancellation = self.cancellation.clone();
        let current_invocation = self.current_invocation;
        let result = thread::Builder::new()
            .name("orna-inspect-render".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "inspect.runtime_unavailable".to_owned())?;
                let result = runtime.block_on(run_installed_external_contract(
                    &kernel,
                    &session,
                    &active,
                    &request,
                    current_invocation,
                ));
                if cancellation.is_requested() {
                    Err("inspect.cancelled".to_owned())
                } else {
                    result
                }
            })
            .map_err(|_| "inspect.runtime_unavailable".to_owned())?
            .join()
            .map_err(|_| "inspect.runtime_unavailable".to_owned())?;
        if self.cancellation.is_requested() {
            return Err("inspect.cancelled".to_owned());
        }
        result
    }

    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        if self.cancellation.is_requested() {
            return request.cancelled();
        }
        self.reap_detached();
        if self.pending.is_some()
            || self.broker_pending.is_some()
            || !self.detached.is_empty()
            || self.detached_broker.is_some()
        {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        }
        let Some(stream_id) = self.allocate_stream_id() else {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        };
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
            // Direct installed execution has no window-update channel. Grant
            // the protocol maximum item window so multi-batch streams can
            // reach terminal completion without a zero-credit pull.
            item_window: MAX_RESOURCE_WINDOW,
            byte_window: MAX_RESOURCE_WINDOW,
        };
        if let Some(broker) = self.broker.as_ref()
            && !broker.register_expected_resource_request(&protocol_request)
        {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        } else if let Some(authorizer) = self.raw_resource_authorizer.as_ref()
            && !authorizer.register_expected_resource_request(&protocol_request)
        {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        }
        if let Some(broker) = self.broker.clone() {
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
                self.discard_raw_resource_request(stream_id);
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
            self.discard_raw_resource_request(stream_id);
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
        let raw_resource_provenance = self.raw_resource_authorizer.clone();
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
                        ResourceTransportSource::Authenticated(transport) => {
                            runtime.block_on(run_authenticated_resource_transport(
                                transport.kernel.clone(),
                                transport.session.clone(),
                                active,
                                registry,
                                protocol_request,
                                expected_type,
                                resource_kind,
                                control_receiver,
                                &sender,
                            ))
                        }
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
                                raw_resource_provenance,
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
            self.discard_raw_resource_request(stream_id);
            self.transport = Some(worker_transport);
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        };
        if let Err(error) = worker_transport_sender.send(worker_transport) {
            let _ = worker.join();
            self.discard_raw_resource_request(stream_id);
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
            stream_id,
            receiver,
            control,
            transport_return,
            worker,
            cancel_requested: false,
        });
        pending
    }

    fn poll(&mut self) -> Option<ClientResourceCompletion> {
        if let Some(completion) = self.poll_detached() {
            return Some(completion);
        }
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
            self.discard_raw_resource_request(pending.stream_id);
            let _ = pending.worker.join();
            self.restore_transport(&pending.transport_return);
            return Some(map_resource_transport_completion(pending.request, result));
        }
    }

    fn cancel_pending(&mut self) -> Option<ClientResourceCompletion> {
        if self.broker_pending.is_some() {
            return self.poll_broker();
        }
        if self.pending.is_some() {
            return self.poll();
        }
        None
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        if let Some(mut pending) = self.broker_pending.take() {
            if !same_resource_request_identity(&pending.request, &request) {
                self.broker_pending = Some(pending);
                return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
            }
            if !pending.cancel_requested {
                match pending.receiver.try_recv() {
                    Ok(Ok(ResourceTransportOutcome::StreamValues(_))) => {}
                    Ok(result @ Ok(_)) | Ok(result @ Err(_)) => {
                        self.discard_raw_resource_request(pending.stream_id);
                        return map_resource_transport_completion(pending.request, result);
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.discard_raw_resource_request(pending.stream_id);
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
            }
            let waiter = match Self::wait_for_cancelled_transport(pending.receiver) {
                Ok(waiter) => waiter,
                Err(receiver) => {
                    pending.receiver = receiver;
                    self.broker_pending = Some(pending);
                    return request.pending();
                }
            };
            let (outcome, waiter_thread, completion) = waiter;
            self.discard_raw_resource_request(pending.stream_id);
            return match outcome {
                Some(CancellationWaitOutcome::Terminal(result)) => {
                    let _ = pending
                        .control
                        .commands
                        .send(BrokerCommand::AbandonResource {
                            stream_id: pending.stream_id,
                            request_id: pending.request.request_id(),
                            reason: ResourceCancellationCode::RuntimeShutdown,
                        });
                    map_resource_transport_completion(pending.request, result)
                }
                Some(CancellationWaitOutcome::TimedOut) => {
                    self.detached_broker = Some(DetachedBrokerResource {
                        control: pending.control,
                        stream_id: pending.stream_id,
                        request: pending.request.clone(),
                        waiter: waiter_thread,
                        completion,
                        abandoned: false,
                    });
                    pending.request.pending()
                }
                None => {
                    self.detached_broker = Some(DetachedBrokerResource {
                        control: pending.control,
                        stream_id: pending.stream_id,
                        request: pending.request.clone(),
                        waiter: waiter_thread,
                        completion,
                        abandoned: false,
                    });
                    pending.request.pending()
                }
            };
        }
        if let Some(detached) = self.detached_broker.as_ref() {
            if same_resource_request_identity(&detached.request, &request) {
                return request.pending();
            }
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        }
        if !self.detached.is_empty() {
            if self
                .detached
                .iter()
                .any(|detached| same_resource_request_identity(&detached.request, &request))
            {
                return request.pending();
            }
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        }
        let Some(mut pending) = self.pending.take() else {
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        };
        if !same_resource_request_identity(&pending.request, &request) {
            self.pending = Some(pending);
            return request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned());
        }
        if !pending.cancel_requested {
            loop {
                match pending.receiver.try_recv() {
                    Ok(Ok(ResourceTransportOutcome::StreamValues(_))) => {}
                    Ok(result @ Ok(_)) | Ok(result @ Err(_)) => {
                        self.discard_raw_resource_request(pending.stream_id);
                        let _ = pending.worker.join();
                        self.restore_transport(&pending.transport_return);
                        return map_resource_transport_completion(pending.request, result);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.discard_raw_resource_request(pending.stream_id);
                        let _ = pending.worker.join();
                        self.restore_transport(&pending.transport_return);
                        return map_resource_transport_completion(
                            pending.request,
                            Err(ResourceTransportFailure::Transport),
                        );
                    }
                }
            }
            let _ = pending.control.send(ResourceTransportControl::Cancel(
                ResourceCancellationCode::ClientRequested,
            ));
            pending.cancel_requested = true;
        }
        let waiter = match Self::wait_for_cancelled_transport(pending.receiver) {
            Ok(waiter) => waiter,
            Err(receiver) => {
                pending.receiver = receiver;
                self.pending = Some(pending);
                return request.pending();
            }
        };
        let (outcome, waiter_thread, completion) = waiter;
        self.discard_raw_resource_request(pending.stream_id);
        match outcome {
            Some(CancellationWaitOutcome::Terminal(result)) => {
                let _ = pending.worker.join();
                self.restore_transport(&pending.transport_return);
                map_resource_transport_completion(pending.request, result)
            }
            Some(CancellationWaitOutcome::TimedOut) | None => {
                let _ = pending.control.send(ResourceTransportControl::Shutdown);
                self.detached.push(DetachedResourceTransport {
                    request: pending.request.clone(),
                    control: Some(pending.control),
                    worker: pending.worker,
                    waiter: waiter_thread,
                    completion,
                    transport_return: pending.transport_return,
                });
                pending.request.pending()
            }
        }
    }

    fn abandon(&mut self, request: ClientResourceRequest) -> Result<(), String> {
        if let Some(pending) = self.broker_pending.take() {
            if !same_resource_request_identity(&pending.request, &request) {
                self.broker_pending = Some(pending);
                return Err("resource executor request mismatch".to_owned());
            }
            let cancel_result = pending
                .control
                .commands
                .send(BrokerCommand::CancelResource {
                    stream_id: pending.stream_id,
                    request_id: pending.request.request_id(),
                    reason: ResourceCancellationCode::ClientRequested,
                });
            let abandon_result = pending
                .control
                .commands
                .send(BrokerCommand::AbandonResource {
                    stream_id: pending.stream_id,
                    request_id: pending.request.request_id(),
                    reason: ResourceCancellationCode::RuntimeShutdown,
                });
            if cancel_result.is_err() || abandon_result.is_err() {
                self.broker_pending = Some(pending);
                return Err("resource executor broker unavailable".to_owned());
            }
            self.discard_raw_resource_request(pending.stream_id);
            return Ok(());
        }

        if let Some(detached) = self
            .detached
            .iter_mut()
            .find(|candidate| same_resource_request_identity(&candidate.request, &request))
        {
            detached.completion = None;
            if let Some(control) = detached.control.take() {
                let _ = control.send(ResourceTransportControl::Shutdown);
            }
            return Ok(());
        }
        if let Some(detached) = self.detached_broker.as_mut() {
            if !same_resource_request_identity(&detached.request, &request) {
                return Err("resource executor request mismatch".to_owned());
            }
            if !detached.abandoned {
                let result = detached
                    .control
                    .commands
                    .send(BrokerCommand::AbandonResource {
                        stream_id: detached.stream_id,
                        request_id: detached.request.request_id(),
                        reason: ResourceCancellationCode::RuntimeShutdown,
                    });
                if result.is_err() {
                    return Err("resource executor broker unavailable".to_owned());
                }
                detached.completion = None;
                detached.abandoned = true;
            } else {
                detached.completion = None;
            }
            return Ok(());
        }
        if !self.detached.is_empty() {
            return Err("resource executor request mismatch".to_owned());
        }

        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if !same_resource_request_identity(&pending.request, &request) {
            self.pending = Some(pending);
            return Err("resource executor request mismatch".to_owned());
        }
        self.discard_raw_resource_request(pending.stream_id);
        let _ = pending.control.send(ResourceTransportControl::Shutdown);
        drop(pending.receiver);
        self.detached.push(DetachedResourceTransport {
            request: pending.request,
            // `Shutdown` was already sent. Drop the sender so a late terminal
            // frame cannot leave the worker waiting for another control.
            control: None,
            worker: pending.worker,
            waiter: None,
            completion: None,
            transport_return: pending.transport_return,
        });
        Ok(())
    }
}
impl Drop for InstalledClientResourceExecutor {
    fn drop(&mut self) {
        if let Some(pending) = self.broker_pending.take() {
            self.discard_raw_resource_request(pending.stream_id);
            let _ = pending
                .control
                .commands
                .send(BrokerCommand::CancelResource {
                    stream_id: pending.stream_id,
                    request_id: pending.request.request_id(),
                    reason: ResourceCancellationCode::RuntimeShutdown,
                });
            let _ = pending
                .control
                .commands
                .send(BrokerCommand::AbandonResource {
                    stream_id: pending.stream_id,
                    request_id: pending.request.request_id(),
                    reason: ResourceCancellationCode::RuntimeShutdown,
                });
        }
        if let Some(pending) = self.pending.take() {
            self.discard_raw_resource_request(pending.stream_id);
            let _ = pending.control.send(ResourceTransportControl::Shutdown);
            drop(pending.receiver);
        }
        for detached in self.detached.drain(..) {
            if let Some(control) = detached.control {
                let _ = control.send(ResourceTransportControl::Shutdown);
            }
        }
        if let Some(detached) = self.detached_broker.take() {
            let _ = detached
                .control
                .commands
                .send(BrokerCommand::AbandonResource {
                    stream_id: detached.stream_id,
                    request_id: detached.request.request_id(),
                    reason: ResourceCancellationCode::RuntimeShutdown,
                });
        }
    }
}

fn map_resource_transport_completion(
    request: ClientResourceRequest,
    outcome: Result<ResourceTransportOutcome, ResourceTransportFailure>,
) -> ClientResourceCompletion {
    match outcome {
        Ok(ResourceTransportOutcome::Ready {
            value,
            nested_invocation_id,
        }) => {
            debug_assert!(valid_resource_invocation_id(nested_invocation_id));
            request.ready(value)
        }
        Ok(ResourceTransportOutcome::StreamValues(values)) => request.stream_values(values),
        Ok(ResourceTransportOutcome::StreamCompleted {
            nested_invocation_id,
        }) => {
            debug_assert!(valid_resource_invocation_id(nested_invocation_id));
            request.stream_completed()
        }
        Ok(ResourceTransportOutcome::Failed {
            failure,
            nested_invocation_id,
        }) => {
            if let Some(nested_invocation_id) = nested_invocation_id {
                debug_assert!(valid_resource_invocation_id(nested_invocation_id));
            }
            request.failed(server_resource_failure_code(failure).to_owned())
        }
        Ok(ResourceTransportOutcome::Cancelled {
            nested_invocation_id,
        }) => {
            if let Some(nested_invocation_id) = nested_invocation_id {
                debug_assert!(valid_resource_invocation_id(nested_invocation_id));
            }
            request.cancelled()
        }
        Err(ResourceTransportFailure::Shape) => {
            request.failed(SERVER_RESOURCE_SHAPE_CODE.to_owned())
        }
        Err(ResourceTransportFailure::Cancelled) => request.cancelled(),
        Err(ResourceTransportFailure::SessionInputUnavailable) => {
            request.failed("client.input_unavailable".to_owned())
        }
        Err(
            ResourceTransportFailure::RootPreflightDenied
            | ResourceTransportFailure::RootSealedDispatchInternal,
        ) => request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned()),
        Err(ResourceTransportFailure::Transport) => {
            request.failed(SERVER_RESOURCE_INTERNAL_CODE.to_owned())
        }
    }
}

#[derive(Debug)]
enum ResourceTransportOutcome {
    Ready {
        value: RuntimeValue,
        nested_invocation_id: orna_core::InvocationId,
    },
    StreamValues(Vec<RuntimeValue>),
    StreamCompleted {
        nested_invocation_id: orna_core::InvocationId,
    },
    Failed {
        failure: CallFailure,
        nested_invocation_id: Option<orna_core::InvocationId>,
    },
    Cancelled {
        nested_invocation_id: Option<orna_core::InvocationId>,
    },
}

#[derive(Debug)]
enum ResourceTransportFailure {
    Transport,
    Shape,
    Cancelled,
    SessionInputUnavailable,
    /// The sealed root was denied before acceptance, so no InvocationId exists
    /// to carry in a [`SealedInvocationResult::Denied`] value.
    RootPreflightDenied,
    RootSealedDispatchInternal,
}

struct ResourceTransportRun {
    stream: tokio::net::UnixStream,
    protocol: ResourceProtocolConnection,
    outcome: ResourceTransportOutcome,
}

fn valid_resource_invocation_id(invocation: orna_core::InvocationId) -> bool {
    invocation.to_bytes().iter().any(|byte| *byte != 0)
}

/// Signals broker shutdown to one resource without waiting for its bounded
/// completion queue. Existing terminal outcomes stay ahead of the transport
/// failure; when the queue is full, dropping this sender lets the receiver
/// drain its bounded backlog and then observe closure.
fn signal_broker_resource_cleanup(
    completion: Sender<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
) {
    let _ = completion.try_send(Err(ResourceTransportFailure::Transport));
    drop(completion);
}

enum ResourceFrameResult {
    Continue,
    Completed,
    Failed(CallFailure),
    Cancelled,
}

impl SharedInvokeBroker {
    pub(crate) fn install_session_bridge(
        &self,
        root_invocation_id: InvocationId,
        call_stream: u64,
    ) -> Result<Arc<SessionBridge>, SessionStateError> {
        let bridge = SessionBridge::new(root_invocation_id, call_stream)?;
        let mut slot = self
            .session_bridge
            .lock()
            .expect("shared session bridge lock");
        if slot.is_some() {
            return Err(SessionStateError::WrongState);
        }
        slot.replace(bridge.clone());
        Ok(bridge)
    }

    pub(crate) fn session_bridge(&self) -> Option<Arc<SessionBridge>> {
        self.session_bridge
            .lock()
            .expect("shared session bridge lock")
            .clone()
    }

    fn clear_session_bridge(&self) {
        let bridge = self
            .session_bridge
            .lock()
            .expect("shared session bridge lock")
            .take();
        if let Some(bridge) = bridge {
            bridge.close();
        }
    }

    fn pending() -> (Self, UnboundedReceiver<BrokerCommand>) {
        let (commands, receiver) = mpsc::unbounded_channel();
        (
            Self {
                commands,
                task: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
                session_bridge: Arc::new(Mutex::new(None)),
                resource_expectations: std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::new())),
                next_resource_stream_id: std::sync::Arc::new(std::sync::Mutex::new(1)),
                resource_terminal_provenance: Arc::new(Mutex::new(BTreeMap::new())),
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
        let task = tokio::spawn(run_shared_invoke_broker(
            stream,
            active,
            registry,
            receiver,
            Arc::clone(&self.resource_terminal_provenance),
        ));
        *self.task.lock().await = Some(task);
        Ok(())
    }

    async fn shutdown(&self) {
        let _ = self.commands.send(BrokerCommand::Shutdown);
        let task = self.task.lock().await.take();
        if let Some(task) = task {
            let _ = task.await;
        }
        self.clear_session_bridge();
        self.clear_resource_expectations();
        self.clear_resource_terminal_provenance();
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
    let mut header = vec![0_u8; SESSION_HEADER_LENGTH];
    tokio::time::timeout(
        RESOURCE_FRAME_TIMEOUT,
        stream.read_exact(&mut header[..SESSION_MARKER.len()]),
    )
    .await
    .map_err(|_| ResourceTransportFailure::Transport)?
    .map_err(|_| ResourceTransportFailure::Transport)?;
    let session = &header[..SESSION_MARKER.len()] == SESSION_MARKER;
    if !session {
        tokio::time::timeout(
            RESOURCE_FRAME_TIMEOUT,
            stream.read_exact(&mut header[SESSION_MARKER.len()..RESOURCE_MARKER.len()]),
        )
        .await
        .map_err(|_| ResourceTransportFailure::Transport)?
        .map_err(|_| ResourceTransportFailure::Transport)?;
    }
    let resource = !session && &header[..RESOURCE_MARKER.len()] == RESOURCE_MARKER;
    let header_length = if session {
        SESSION_HEADER_LENGTH
    } else if resource {
        RESOURCE_HEADER_LENGTH
    } else {
        18
    };
    let consumed = if session {
        SESSION_MARKER.len()
    } else {
        RESOURCE_MARKER.len()
    };
    tokio::time::timeout(
        RESOURCE_FRAME_TIMEOUT,
        stream.read_exact(&mut header[consumed..header_length]),
    )
    .await
    .map_err(|_| ResourceTransportFailure::Transport)?
    .map_err(|_| ResourceTransportFailure::Transport)?;
    let declared_offset = if session {
        SESSION_HEADER_LENGTH - std::mem::size_of::<u32>()..SESSION_HEADER_LENGTH
    } else if resource {
        17..21
    } else {
        14..18
    };
    let payload_length = u32::from_be_bytes(
        header[declared_offset]
            .try_into()
            .expect("shared broker frame header has a fixed length"),
    ) as usize;
    if (session && payload_length > MAX_SESSION_FRAME_LENGTH - SESSION_HEADER_LENGTH)
        || payload_length > MAX_FRAME_PAYLOAD_LENGTH
    {
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

fn wire_frame_is_session(frame: &BrokerWireFrame) -> bool {
    frame.bytes.len() >= SESSION_MARKER.len()
        && &frame.bytes[..SESSION_MARKER.len()] == SESSION_MARKER
}

async fn handle_shared_session_frame<W>(
    frame: BrokerWireFrame,
    stream: &mut W,
    root: &Option<BrokerRootState>,
) -> Result<(), ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    let SessionServerFrame::InputRequested(request) =
        decode_session_server_frame(&frame.bytes).map_err(|_| ResourceTransportFailure::Shape)?;
    let Some(root_invocation_id) = root.as_ref().and_then(|state| state.invocation) else {
        return Err(ResourceTransportFailure::Shape);
    };
    if request.root_invocation_id != root_invocation_id || request.call_stream == 0 {
        return Err(ResourceTransportFailure::Shape);
    }
    let _ = stream;
    Err(ResourceTransportFailure::SessionInputUnavailable)
}

async fn run_shared_invoke_broker(
    stream: tokio::net::UnixStream,
    active: ActiveDatabaseRevision,
    registry: OpaqueCodecRegistry,
    mut commands: UnboundedReceiver<BrokerCommand>,
    resource_terminal_provenance: BrokerResourceProvenance,
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
                    &mut resource_tombstones,
                    &resource_terminal_provenance,
                )
                .await
                .is_err()
                {
                    break;
                }
            }
            BrokerNext::Command(None) => break,
            BrokerNext::Frame(Some(Ok(frame))) => {
                let result = if wire_frame_is_session(&frame) {
                    handle_shared_session_frame(frame, &mut stream, &root).await
                } else {
                    handle_shared_broker_frame(
                        frame,
                        &mut stream,
                        &active,
                        &registry,
                        &mut root,
                        &mut resources,
                        resource_high_water_mark,
                        &mut resource_tombstones,
                        &resource_terminal_provenance,
                    )
                    .await
                };
                if result.is_err() {
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
        signal_broker_resource_cleanup(resource.completion);
    }
    resource_terminal_provenance
        .lock()
        .expect("broker resource provenance lock")
        .clear();
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
    resource_tombstones: &mut BrokerResourceTombstones,
    resource_terminal_provenance: &BrokerResourceProvenance,
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
                    accepted_nested_invocation_id: None,
                    scalar_value: None,
                    cancellation_requested: false,
                    stream_values_seen: false,
                    terminal_provenance: ResourceTerminalProvenance::Uncommitted,
                    scalar_value_after_cancellation: false,
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
                clear_resource_terminal_provenance_for_stream(
                    resource_terminal_provenance,
                    stream_id,
                );
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
        BrokerCommand::AbandonResource {
            stream_id,
            request_id,
            reason,
        } => {
            let Some(state) = resources.get(&stream_id) else {
                clear_resource_terminal_provenance_for_stream(
                    resource_terminal_provenance,
                    stream_id,
                );
                return Ok(());
            };
            if state.request.request_id != request_id {
                return Err(ResourceTransportFailure::Shape);
            }
            clear_resource_terminal_provenance_for_stream(resource_terminal_provenance, stream_id);
            let mut state = resources
                .remove(&stream_id)
                .expect("broker resource checked above");
            if !state.cancellation_requested {
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
            remember_broker_resource_terminal(resource_tombstones, stream_id, request_id);
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
fn resource_action_is_terminal(frame: &ResourceServerFrame) -> bool {
    matches!(
        frame,
        ResourceServerFrame::Completed(_)
            | ResourceServerFrame::Failed(_)
            | ResourceServerFrame::Cancelled(_)
    )
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
    resource_high_water_mark: Option<u64>,
    resource_tombstones: &mut BrokerResourceTombstones,
    resource_terminal_provenance: &BrokerResourceProvenance,
) -> Result<(), ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    if frame.resource {
        let decoded = decode_resource_server_frame(active, registry, &frame.bytes)
            .map_err(|_| ResourceTransportFailure::Shape)?;
        let (stream_id, request_id) = resource_server_frame_identity(&decoded);
        if let Some(expected_request_id) = resource_tombstones.get(&stream_id) {
            // A tombstone is final for this stream identity. Clear every
            // provenance entry for the stream before either dropping a valid
            // late frame or rejecting a forged request identity.
            clear_resource_terminal_provenance_for_stream(resource_terminal_provenance, stream_id);
            if *expected_request_id != request_id {
                return Err(ResourceTransportFailure::Shape);
            }
            // The broker has already published this stream terminal outcome.
            // Keep the connection alive for the root call and every other resource.
            return Ok(());
        }
        let Some(mut state) = resources.remove(&stream_id) else {
            // No live state can accept this frame. A stream at or below the
            // broker high-water mark is an evicted tombstone, so late frames
            // are drained and cannot revive the old request or its provenance.
            clear_resource_terminal_provenance_for_stream(resource_terminal_provenance, stream_id);
            if stream_id != 0 && resource_high_water_mark.is_some_and(|high| stream_id <= high) {
                return Ok(());
            }
            return Err(ResourceTransportFailure::Shape);
        };
        let frame_terminal = resource_action_is_terminal(&decoded);
        if frame_terminal {
            state.terminal_provenance = resource_terminal_provenance
                .lock()
                .expect("broker resource provenance lock")
                .get(&(stream_id, request_id))
                .copied()
                .unwrap_or(ResourceTerminalProvenance::Uncommitted);
        }
        match handle_shared_resource_frame_classified(&mut state, decoded, stream, active, registry)
            .await
        {
            Ok(true) => {
                resources.insert(stream_id, state);
            }
            Ok(false) => {
                remember_broker_resource_terminal(
                    resource_tombstones,
                    stream_id,
                    state.request.request_id,
                );
                clear_resource_terminal_provenance_for_stream(
                    resource_terminal_provenance,
                    stream_id,
                );
            }
            Err(
                SharedResourceFrameError::Protocol | SharedResourceFrameError::RequestLocalShape,
            ) => {
                clear_resource_terminal_provenance_for_stream(
                    resource_terminal_provenance,
                    stream_id,
                );
                let _ = send_shared_resource_completion(
                    &mut state,
                    Err(ResourceTransportFailure::Shape),
                    stream,
                    active,
                    registry,
                )
                .await?;
                if !state.cancellation_requested {
                    let cancel = ResourceClientFrame::Cancel(ResourceCancel {
                        stream_id: state.request.stream_id,
                        request_id: state.request.request_id,
                        reason: ResourceCancellationCode::RuntimeShutdown,
                    });
                    state
                        .protocol
                        .receive(cancel.clone())
                        .map_err(|_| ResourceTransportFailure::Shape)?;
                    let encoded = encode_resource_client_frame(active, registry, &cancel)
                        .map_err(|_| ResourceTransportFailure::Shape)?;
                    write_shared_broker_frame(stream, &encoded).await?;
                    state.cancellation_requested = true;
                }
                remember_broker_resource_terminal(
                    resource_tombstones,
                    stream_id,
                    state.request.request_id,
                );
            }
            Err(SharedResourceFrameError::Transport(error)) => {
                clear_resource_terminal_provenance_for_stream(
                    resource_terminal_provenance,
                    stream_id,
                );
                return Err(error);
            }
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
        } => {
            if state.invocation.replace(invocation).is_some() {
                return Err(ResourceTransportFailure::Shape);
            }
        }
        ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events,
        } => {
            let Some(invocation) = state.invocation else {
                return Err(ResourceTransportFailure::Shape);
            };
            if state.records.last().is_some_and(|last| {
                matches!(
                    last.event().body(),
                    InvocationEventBody::Completed { .. }
                        | InvocationEventBody::Failed(_)
                        | InvocationEventBody::Cancelled { .. }
                )
            }) {
                return Err(ResourceTransportFailure::Shape);
            }
            let event_count = events.len();
            for (index, record) in events.into_iter().enumerate() {
                let Event::Value(RuntimeValue::InvokeEvent(event)) = record.event else {
                    return Err(ResourceTransportFailure::Shape);
                };
                if event.invocation_id() != invocation {
                    return Err(ResourceTransportFailure::Shape);
                }
                if state.records.is_empty()
                    && !matches!(event.body(), InvocationEventBody::Started { .. })
                {
                    return Err(ResourceTransportFailure::Shape);
                }
                if !state.records.is_empty()
                    && matches!(event.body(), InvocationEventBody::Started { .. })
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
                if matches!(
                    event.body(),
                    InvocationEventBody::Completed { .. }
                        | InvocationEventBody::Failed(_)
                        | InvocationEventBody::Cancelled { .. }
                ) && index + 1 != event_count
                {
                    return Err(ResourceTransportFailure::Shape);
                }
                state
                    .records
                    .push(InvocationEventRecord::new(record.sequence, event));
            }
        }
        ServerFrame::CallCompleted { stream: 1 } => {
            let state = root.take().expect("root state checked above");
            let Some(invocation) = state.invocation else {
                return Err(ResourceTransportFailure::Shape);
            };
            if state.records.is_empty() {
                let _ = state.response.send(Err(ResourceTransportFailure::Shape));
                return Err(ResourceTransportFailure::Shape);
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
            let result = match (state.invocation, failure) {
                // Entry denial is the one legal pre-accept terminal result. It
                // has no InvocationId by contract, so keep it out of the
                // accepted-only SealedInvocationResult::Denied variant.
                (None, CallFailure::ExecuteDenied) if state.records.is_empty() => {
                    Err(ResourceTransportFailure::RootPreflightDenied)
                }
                // Request decode, protocol-major, and standard-snapshot failures
                // are terminal internal outcomes before acceptance.
                (None, CallFailure::InternalFailure) if state.records.is_empty() => {
                    Err(ResourceTransportFailure::RootSealedDispatchInternal)
                }
                // Every other missing identity is an invalid root frame.
                (None, _) => {
                    let _ = state.response.send(Err(ResourceTransportFailure::Shape));
                    return Err(ResourceTransportFailure::Shape);
                }
                // Accepted invocations must terminate with a terminal Event
                // followed by CALL_COMPLETED. Raw CALL_FAILED is pre-accept
                // only, so notify the waiting caller before rejecting it.
                (Some(_), _) => {
                    let _ = state.response.send(Err(ResourceTransportFailure::Shape));
                    return Err(ResourceTransportFailure::Shape);
                }
            };
            let _ = state.response.send(result);
        }
        ServerFrame::CallCancelled { stream: 1 } => {
            let state = root.take().expect("root state checked above");
            if state.invocation.is_some() {
                // Accepted invocations must publish InvocationCancelled as a
                // terminal Event and then CALL_COMPLETED. Raw CALL_CANCELLED
                // is pre-accept only, so do not expose a public cancellation.
                let _ = state.response.send(Err(ResourceTransportFailure::Shape));
                return Err(ResourceTransportFailure::Shape);
            }
            if state.invocation.is_none() && !state.records.is_empty() {
                let _ = state.response.send(Err(ResourceTransportFailure::Shape));
                return Err(ResourceTransportFailure::Shape);
            }
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
    let Some(first) = events.records().first() else {
        return Err(ResourceTransportFailure::Shape);
    };
    if !matches!(first.event().body(), InvocationEventBody::Started { .. }) {
        return Err(ResourceTransportFailure::Shape);
    }
    let mut started_seen = false;
    let mut terminal_seen = false;
    for record in events.records() {
        if matches!(record.event().body(), InvocationEventBody::Started { .. }) {
            if started_seen {
                return Err(ResourceTransportFailure::Shape);
            }
            started_seen = true;
        }
        let terminal = matches!(
            record.event().body(),
            InvocationEventBody::Completed { .. }
                | InvocationEventBody::Failed(_)
                | InvocationEventBody::Cancelled { .. }
        );
        if terminal_seen {
            return Err(ResourceTransportFailure::Shape);
        }
        terminal_seen = terminal;
    }
    let Some(last) = events.records().last() else {
        return Err(ResourceTransportFailure::Shape);
    };
    match last.event().body() {
        InvocationEventBody::Failed(failure) if failure.code() == "INVOKE_DENIED" => {
            Ok(SealedInvocationResult::Denied { invocation })
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

async fn send_shared_resource_completion<W>(
    state: &mut BrokerResourceState,
    outcome: Result<ResourceTransportOutcome, ResourceTransportFailure>,
    stream: &mut W,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<bool, ResourceTransportFailure>
where
    W: AsyncWrite + Unpin,
{
    if state.completion.send(outcome).await.is_ok() {
        return Ok(true);
    }
    if state.cancellation_requested {
        return Ok(false);
    }
    let cancel = ResourceClientFrame::Cancel(ResourceCancel {
        stream_id: state.request.stream_id,
        request_id: state.request.request_id,
        reason: ResourceCancellationCode::RuntimeShutdown,
    });
    state
        .protocol
        .receive(cancel.clone())
        .map_err(|_| ResourceTransportFailure::Shape)?;
    state.cancellation_requested = true;
    let encoded = encode_resource_client_frame(active, registry, &cancel)
        .map_err(|_| ResourceTransportFailure::Shape)?;
    write_shared_broker_frame(stream, &encoded).await?;
    Ok(true)
}

#[derive(Debug)]
enum SharedResourceFrameError {
    Protocol,
    RequestLocalShape,
    Transport(ResourceTransportFailure),
}

impl From<ResourceTransportFailure> for SharedResourceFrameError {
    fn from(error: ResourceTransportFailure) -> Self {
        Self::Transport(error)
    }
}

#[cfg(test)]
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
    match handle_shared_resource_frame_classified(state, frame, stream, active, registry).await {
        Ok(keep) => Ok(keep),
        Err(SharedResourceFrameError::Protocol | SharedResourceFrameError::RequestLocalShape) => {
            Err(ResourceTransportFailure::Shape)
        }
        Err(SharedResourceFrameError::Transport(error)) => Err(error),
    }
}

async fn handle_shared_resource_frame_classified<W>(
    state: &mut BrokerResourceState,
    frame: ResourceServerFrame,
    stream: &mut W,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
) -> Result<bool, SharedResourceFrameError>
where
    W: AsyncWrite + Unpin,
{
    // Validate against a candidate so a request-local error cannot consume the
    // live stream credit or state before the broker publishes its terminal.
    let mut protocol = state.protocol.clone();
    let disposition = if state.cancellation_requested {
        if let ResourceServerFrame::Cancelled(cancelled) = &frame {
            protocol
                .apply_cancelled_after_client_cancel(*cancelled)
                .map_err(|_| SharedResourceFrameError::Protocol)?
        } else {
            protocol
                .apply_constructed(active, registry, frame.clone())
                .map_err(|_| SharedResourceFrameError::Protocol)?
        }
    } else {
        protocol
            .apply_constructed(active, registry, frame.clone())
            .map_err(|_| SharedResourceFrameError::Protocol)?
    };
    // A terminal frame marked DroppedLate can still be the committed server
    // result: cancellation closed the client-side protocol before that result
    // reached this broker. Drain late non-terminals, but publish this terminal
    // before removing the broker state.
    let late_terminal = state.cancellation_requested
        && matches!(
            disposition,
            orna_protocol::ResourceFrameDisposition::DroppedLate
        )
        && state.terminal_provenance.is_committed()
        && matches!(
            &frame,
            ResourceServerFrame::Completed(_) | ResourceServerFrame::Failed(_)
        );
    // Once cancellation has moved the protocol into a terminal tombstone,
    // validate late non-terminals and advance only the private candidate state
    // needed to validate a later terminal. No late value is published; a scalar
    // value and accepted lineage are retained only until a committed terminal
    // proves they may be delivered.
    if state.cancellation_requested
        && matches!(
            disposition,
            orna_protocol::ResourceFrameDisposition::DroppedLate
        )
        && !late_terminal
        && !matches!(
            &frame,
            ResourceServerFrame::Completed(_)
                | ResourceServerFrame::Failed(_)
                | ResourceServerFrame::Cancelled(_)
        )
    {
        match &frame {
            ResourceServerFrame::Accepted(value) => {
                if value.request_id != state.request.request_id
                    || value.target_revision != state.request.target_revision
                    || value.resource_kind != state.resource_kind
                    || !valid_resource_invocation_id(value.nested_invocation_id)
                {
                    return Err(SharedResourceFrameError::RequestLocalShape);
                }
                // Keep the authenticated lineage identity while draining a
                // late acceptance. A committed terminal may arrive after it
                // and still needs the nested invocation identity.
                state.accepted = true;
                state.accepted_nested_invocation_id = Some(value.nested_invocation_id);
            }
            ResourceServerFrame::Values(value) => {
                if value.request_id != state.request.request_id
                    || value.target_revision != state.request.target_revision
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
                    return Err(SharedResourceFrameError::RequestLocalShape);
                }
                if matches!(state.resource_kind, ProtocolResourceKind::Single) {
                    state.scalar_value = value.values.first().cloned();
                    state.scalar_value_after_cancellation = true;
                }
            }
            ResourceServerFrame::Completed(_)
            | ResourceServerFrame::Failed(_)
            | ResourceServerFrame::Cancelled(_) => unreachable!("late terminal handled above"),
        }
        // Retain the validated candidate so repeated late frames and the
        // eventual terminal are checked against the drained batch sequence
        // and credit state, without publishing those frames.
        state.protocol = protocol;
        return Ok(true);
    }
    if state.cancellation_requested
        && !late_terminal
        && matches!(
            &frame,
            ResourceServerFrame::Completed(_) | ResourceServerFrame::Failed(_)
        )
    {
        state.scalar_value.take();
        state.scalar_value_after_cancellation = false;
        let _ = send_shared_resource_completion(
            state,
            Ok(ResourceTransportOutcome::Cancelled {
                nested_invocation_id: state.accepted_nested_invocation_id,
            }),
            stream,
            active,
            registry,
        )
        .await?;
        return Ok(false);
    }
    match frame {
        ResourceServerFrame::Accepted(value) => {
            if value.request_id != state.request.request_id
                || value.target_revision != state.request.target_revision
                || value.resource_kind != state.resource_kind
                || !valid_resource_invocation_id(value.nested_invocation_id)
            {
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            state.protocol = protocol;
            state.accepted = true;
            state.accepted_nested_invocation_id = Some(value.nested_invocation_id);
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
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            if state.cancellation_requested {
                return Ok(true);
            }
            match state.resource_kind {
                ProtocolResourceKind::Single => {
                    state.protocol = protocol;
                    state.scalar_value = value.values.into_iter().next()
                }
                ProtocolResourceKind::Stream => {
                    state.protocol = protocol;
                    state.stream_values_seen = true;
                    if !send_shared_resource_completion(
                        state,
                        Ok(ResourceTransportOutcome::StreamValues(value.values)),
                        stream,
                        active,
                        registry,
                    )
                    .await?
                    {
                        return Ok(false);
                    }
                    if state.cancellation_requested {
                        return Ok(true);
                    }
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
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            if !state.accepted {
                if late_terminal {
                    let _ = send_shared_resource_completion(
                        state,
                        Err(ResourceTransportFailure::Shape),
                        stream,
                        active,
                        registry,
                    )
                    .await?;
                    return Ok(false);
                }
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            let outcome = match state.resource_kind {
                ProtocolResourceKind::Single => {
                    let Some(value) = state.scalar_value.take() else {
                        if late_terminal {
                            let _ = send_shared_resource_completion(
                                state,
                                Err(ResourceTransportFailure::Shape),
                                stream,
                                active,
                                registry,
                            )
                            .await?;
                            return Ok(false);
                        }
                        return Err(SharedResourceFrameError::RequestLocalShape);
                    };
                    ResourceTransportOutcome::Ready {
                        value,
                        nested_invocation_id: state
                            .accepted_nested_invocation_id
                            .ok_or(ResourceTransportFailure::Shape)?,
                    }
                }
                ProtocolResourceKind::Stream => ResourceTransportOutcome::StreamCompleted {
                    nested_invocation_id: state
                        .accepted_nested_invocation_id
                        .ok_or(ResourceTransportFailure::Shape)?,
                },
            };
            state.protocol = protocol;
            let _ = send_shared_resource_completion(state, Ok(outcome), stream, active, registry)
                .await?;
            return Ok(false);
        }
        ResourceServerFrame::Failed(value) => {
            if value.request_id != state.request.request_id {
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            if state.scalar_value.is_some() && !late_terminal {
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            if late_terminal {
                state.scalar_value.take();
            }
            state.protocol = protocol;
            let _ = send_shared_resource_completion(
                state,
                Ok(ResourceTransportOutcome::Failed {
                    failure: value.failure,
                    nested_invocation_id: state.accepted_nested_invocation_id,
                }),
                stream,
                active,
                registry,
            )
            .await?;
            return Ok(false);
        }
        ResourceServerFrame::Cancelled(value) => {
            if value.request_id != state.request.request_id {
                return Err(SharedResourceFrameError::RequestLocalShape);
            }
            state.protocol = protocol;
            let _ = send_shared_resource_completion(
                state,
                Ok(ResourceTransportOutcome::Cancelled {
                    nested_invocation_id: state.accepted_nested_invocation_id,
                }),
                stream,
                active,
                registry,
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
    Cancel,
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

/// Converts the decoded request's initial receive windows into the first
/// producer pull credit. The protocol decoder already enforces the wire
/// bounds; keeping the checked conversion here makes the authenticated
/// boundary fail closed if it is ever called with an unvalidated request.
fn initial_authenticated_resource_credit(
    request: &ResourceRequest,
) -> Result<ResourceCredit, ResourceTransportFailure> {
    ResourceCredit::new(request.item_window, request.byte_window)
        .ok_or(ResourceTransportFailure::Shape)
}

/// Validates one in-memory producer values event at the same boundary as a
/// decoded resource values frame. The producer is trusted to execute under
/// the authenticated session, but its event metadata is still untrusted at
/// the adapter boundary: publication must consume only the offered credit and
/// only values whose active canonical encoding matches the declared byte
/// count.
fn authenticated_resource_producer_credit(
    resource_kind: ProtocolResourceKind,
    scalar_value_present: bool,
    item_available: u64,
    byte_available: u64,
) -> Option<ResourceCredit> {
    if scalar_value_present {
        return Some(ResourceCredit {
            item_count: 0,
            byte_count: 0,
        });
    }
    if matches!(resource_kind, ProtocolResourceKind::Stream)
        && (item_available == 0 || byte_available == 0)
    {
        return Some(ResourceCredit {
            item_count: item_available,
            byte_count: byte_available,
        });
    }
    ResourceCredit::new(item_available, byte_available)
}

fn validate_authenticated_resource_values(
    active: &ActiveDatabaseRevision,
    registry: &orna_core::value::OpaqueCodecRegistry,
    request: &ResourceRequest,
    expected_type: ResolvedType,
    resource_kind: ProtocolResourceKind,
    next_batch_sequence: u64,
    scalar_value_present: bool,
    total_items: u64,
    total_bytes: u64,
    offered_credit: ResourceCredit,
    batch_sequence: u64,
    item_count: u64,
    byte_count: u64,
    values: &[RuntimeValue],
) -> Result<u64, ResourceTransportFailure> {
    if values.is_empty()
        || item_count == 0
        || item_count != values.len() as u64
        || item_count > u64::from(u32::MAX)
        || batch_sequence != next_batch_sequence
        || byte_count == 0
        || item_count > offered_credit.item_count
        || byte_count > offered_credit.byte_count
        || (matches!(resource_kind, ProtocolResourceKind::Single)
            && (values.len() != 1 || scalar_value_present))
    {
        return Err(ResourceTransportFailure::Shape);
    }
    if values
        .iter()
        .any(|value| !runtime_value_matches_type(active, value, expected_type))
    {
        return Err(ResourceTransportFailure::Shape);
    }

    let total_items = total_items
        .checked_add(item_count)
        .filter(|total| *total <= MAX_RESOURCE_TOTAL_ITEMS)
        .ok_or(ResourceTransportFailure::Shape)?;
    total_bytes
        .checked_add(byte_count)
        .ok_or(ResourceTransportFailure::Shape)?;
    let frame = orna_protocol::ResourceValues {
        stream_id: request.stream_id,
        request_id: request.request_id,
        target_revision: request.target_revision,
        batch_sequence,
        item_count: u32::try_from(item_count).map_err(|_| ResourceTransportFailure::Shape)?,
        byte_count: u32::try_from(byte_count).map_err(|_| ResourceTransportFailure::Shape)?,
        values: values.to_vec(),
    };
    orna_protocol::encode_resource_values(active, registry, &frame)
        .map_err(|_| ResourceTransportFailure::Shape)?;
    Ok(total_items)
}

/// Decides what a server frame means after the client has requested cancel.
///
/// The local connection stays live after sending `RESOURCE_CANCEL`: an
/// already-committed server terminal frame is therefore `Applied` and wins,
/// while `Accepted`/`Values` are drained without publication. A
/// `DroppedLate` terminal is published only when its internal provenance
/// confirms the authenticated producer committed it; otherwise cancellation
/// wins. The rule is independent of scalar versus stream; only the
/// caller's publication policy differs for those resource kinds.
fn resource_transport_disposition_action(
    disposition: orna_protocol::ResourceFrameDisposition,
    cancellation_requested: bool,
    terminal: bool,
    terminal_provenance: ResourceTerminalProvenance,
    cancellation_frame: bool,
) -> ResourceFrameDispositionAction {
    if !cancellation_requested {
        return match disposition {
            orna_protocol::ResourceFrameDisposition::Applied => {
                ResourceFrameDispositionAction::Apply
            }
            orna_protocol::ResourceFrameDisposition::DroppedLate => {
                ResourceFrameDispositionAction::Reject
            }
        };
    }
    let terminal_wins = cancellation_frame || terminal_provenance.is_committed();
    match (disposition, terminal) {
        (_, false)
            if matches!(
                disposition,
                orna_protocol::ResourceFrameDisposition::Applied
            ) =>
        {
            ResourceFrameDispositionAction::Drain
        }
        (_, false) => ResourceFrameDispositionAction::Drop,
        (_, true) if terminal_wins => ResourceFrameDispositionAction::Apply,
        (_, true) => ResourceFrameDispositionAction::Cancel,
    }
}

async fn run_authenticated_resource_transport(
    kernel: PostgresKernel,
    session: AuthenticatedSession,
    active: ActiveDatabaseRevision,
    registry: orna_core::value::OpaqueCodecRegistry,
    request: ResourceRequest,
    expected_type: ResolvedType,
    resource_kind: ProtocolResourceKind,
    mut controls: UnboundedReceiver<ResourceTransportControl>,
    completion_sender: &Sender<Result<ResourceTransportOutcome, ResourceTransportFailure>>,
) -> Result<ResourceTransportOutcome, ResourceTransportFailure> {
    let cancellation = ResourceCancellation::new();
    let start =
        kernel.start_authenticated_server_resource_producer(&session, &request, &cancellation);
    tokio::pin!(start);
    let started = tokio::select! {
        biased;
        control = controls.recv() => match control {
            Some(_control) => {
                match resource_transport_cancellation_action(cancellation.request_cancel()) {
                    ResourceTransportCancellationAction::ReturnCancelled => {
                        return Ok(ResourceTransportOutcome::Cancelled {
                            nested_invocation_id: None,
                        });
                    }
                    ResourceTransportCancellationAction::ContinueCommitted => {
                        tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, &mut start)
                            .await
                            .map_err(|_| ResourceTransportFailure::Transport)?
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
            return Ok(ResourceTransportOutcome::Failed {
                failure,
                nested_invocation_id: None,
            });
        }
    };
    let accepted = producer.accepted();
    let accepted_kind = match resource_kind {
        ProtocolResourceKind::Single => orna_postgres::AuthenticatedServerResourceKind::Single,
        ProtocolResourceKind::Stream => orna_postgres::AuthenticatedServerResourceKind::Stream,
    };
    if accepted.stream_id != request.stream_id
        || accepted.request_id != request.request_id
        || accepted.target_revision != request.target_revision
        || accepted.resource_kind != accepted_kind
        || !valid_resource_invocation_id(accepted.nested_invocation_id)
    {
        producer.cancel();
        return Err(ResourceTransportFailure::Shape);
    }

    let accepted_nested_invocation_id = accepted.nested_invocation_id;
    if cancellation.is_requested() {
        // Cancellation can arrive after the acceptance commit check and before
        // the producer publishes its acceptance. Do not issue a pull to a
        // producer that is already terminating without a response.
        drop(producer);
        return Ok(ResourceTransportOutcome::Cancelled {
            nested_invocation_id: Some(accepted_nested_invocation_id),
        });
    }
    let mut scalar_value = None;
    let mut next_batch_sequence = 0_u64;
    let mut total_items = 0_u64;
    let mut total_bytes = 0_u64;
    let initial_credit = initial_authenticated_resource_credit(&request)?;
    loop {
        let item_available = initial_credit
            .item_count
            .checked_sub(total_items)
            .ok_or(ResourceTransportFailure::Shape)?;
        let byte_available = initial_credit
            .byte_count
            .checked_sub(total_bytes)
            .ok_or(ResourceTransportFailure::Shape)?;
        // A scalar value, or an exhausted stream window, is followed by a
        // zero-credit terminal probe. The producer accepts that probe only
        // after the already-granted values have been delivered. Unlike the
        // socket transport, this direct path has no window-update frames, so
        // a Waiting event cannot replenish either remaining window.
        let credit = authenticated_resource_producer_credit(
            resource_kind,
            scalar_value.is_some(),
            item_available,
            byte_available,
        )
        .ok_or(ResourceTransportFailure::Shape)?;
        let pull = producer.pull(credit);
        tokio::pin!(pull);
        let event = tokio::select! {
            biased;
            control = controls.recv() => match control {
                Some(_control) => {
                    match resource_transport_cancellation_action(producer.cancel()) {
                        ResourceTransportCancellationAction::ReturnCancelled => {
                            return Ok(ResourceTransportOutcome::Cancelled {
                                nested_invocation_id: Some(accepted_nested_invocation_id),
                            });
                        }
                        ResourceTransportCancellationAction::ContinueCommitted => {
                            tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, &mut pull)
                                .await
                                .map_err(|_| ResourceTransportFailure::Transport)?
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
                let validated_total_items = match validate_authenticated_resource_values(
                    &active,
                    &registry,
                    &request,
                    expected_type,
                    resource_kind,
                    next_batch_sequence,
                    scalar_value.is_some(),
                    total_items,
                    total_bytes,
                    credit,
                    batch_sequence,
                    item_count,
                    byte_count,
                    &values,
                ) {
                    Ok(total_items) => total_items,
                    Err(error) => {
                        producer.cancel();
                        return Err(error);
                    }
                };
                next_batch_sequence = next_batch_sequence
                    .checked_add(1)
                    .ok_or(ResourceTransportFailure::Shape)?;
                total_items = validated_total_items;
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
                                            return Ok(ResourceTransportOutcome::Cancelled {
                                                nested_invocation_id: Some(accepted_nested_invocation_id),
                                            });
                                        }
                                        ResourceTransportCancellationAction::ContinueCommitted => {
                                            tokio::time::timeout(RESOURCE_FRAME_TIMEOUT, &mut send)
                                                .await
                                                .map_err(|_| ResourceTransportFailure::Transport)?
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
                        .map(|value| ResourceTransportOutcome::Ready {
                            value,
                            nested_invocation_id: accepted_nested_invocation_id,
                        })
                        .ok_or(ResourceTransportFailure::Shape),
                    ProtocolResourceKind::Stream => Ok(ResourceTransportOutcome::StreamCompleted {
                        nested_invocation_id: accepted_nested_invocation_id,
                    }),
                };
            }
            AuthenticatedServerResourceEvent::Failed { failure } => {
                if scalar_value.is_some() {
                    producer.cancel();
                    return Err(ResourceTransportFailure::Shape);
                }
                return Ok(ResourceTransportOutcome::Failed {
                    failure,
                    nested_invocation_id: Some(accepted_nested_invocation_id),
                });
            }
            AuthenticatedServerResourceEvent::Cancelled => {
                return Ok(ResourceTransportOutcome::Cancelled {
                    nested_invocation_id: Some(accepted_nested_invocation_id),
                });
            }
            AuthenticatedServerResourceEvent::Waiting { required_bytes } => {
                // A direct authenticated producer has no RESOURCE_WINDOW_UPDATE
                // channel. A pending value therefore cannot be admitted after
                // either initial window is exhausted; fail closed without
                // publishing that value or advancing the local totals.
                if required_bytes == 0 || required_bytes > MAX_RESOURCE_WINDOW {
                    producer.cancel();
                    return Err(ResourceTransportFailure::Shape);
                }
                producer.cancel();
                return Err(ResourceTransportFailure::Shape);
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
    provenance: Option<SharedInvokeBroker>,
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
    let mut accepted_nested_invocation_id = None;
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

        let response = match &frame {
            ResourceServerFrame::Accepted(_) | ResourceServerFrame::Values(_) => {
                ResourceFrameResult::Continue
            }
            ResourceServerFrame::Completed(_) => ResourceFrameResult::Completed,
            ResourceServerFrame::Failed(frame) => ResourceFrameResult::Failed(frame.failure),
            ResourceServerFrame::Cancelled(_) => ResourceFrameResult::Cancelled,
        };
        let frame_terminal = matches!(
            &response,
            ResourceFrameResult::Completed
                | ResourceFrameResult::Failed(_)
                | ResourceFrameResult::Cancelled
        );
        let cancellation_frame = matches!(&response, ResourceFrameResult::Cancelled);
        let frame_provenance = if cancellation_requested && frame_terminal {
            provenance
                .as_ref()
                .map(|broker| broker.resource_terminal_provenance(stream_id, request_id))
                .unwrap_or(ResourceTerminalProvenance::Uncommitted)
        } else {
            ResourceTerminalProvenance::Uncommitted
        };
        let mut candidate = connection.clone();
        let disposition = match &frame {
            ResourceServerFrame::Cancelled(cancelled) if cancellation_requested => {
                let cancel = ResourceClientFrame::Cancel(ResourceCancel {
                    stream_id,
                    request_id,
                    reason: ResourceCancellationCode::ClientRequested,
                });
                candidate
                    .receive(cancel)
                    .map_err(|_| ResourceTransportFailure::Shape)?;
                candidate
                    .apply_cancelled_after_client_cancel(*cancelled)
                    .map_err(|_| ResourceTransportFailure::Shape)?
            }
            _ => candidate
                .apply_constructed(&active, &registry, frame.clone())
                .map_err(|_| ResourceTransportFailure::Shape)?,
        };
        let disposition_action = resource_transport_disposition_action(
            disposition,
            cancellation_requested,
            frame_terminal,
            frame_provenance,
            cancellation_frame,
        );
        let cancellation_first_terminal =
            matches!(disposition_action, ResourceFrameDispositionAction::Cancel);
        if frame_terminal {
            if let Some(broker) = provenance.as_ref() {
                // A terminal closes the stream identity even when its request
                // key is malformed. Clear the whole stream entry so a stale
                // request id cannot retain or later reuse provenance.
                clear_resource_terminal_provenance_for_stream(
                    &broker.resource_terminal_provenance,
                    stream_id,
                );
            }
        }
        match disposition_action {
            ResourceFrameDispositionAction::Reject => {
                return Err(ResourceTransportFailure::Shape);
            }
            ResourceFrameDispositionAction::Drop => {}
            ResourceFrameDispositionAction::Apply | ResourceFrameDispositionAction::Drain => {}
            ResourceFrameDispositionAction::Cancel => {}
        }

        if matches!(&frame, ResourceServerFrame::Failed(_))
            && scalar_value.is_some()
            && !cancellation_first_terminal
        {
            if frame_provenance.is_committed() {
                scalar_value.take();
            } else {
                return Err(ResourceTransportFailure::Shape);
            }
        }
        let accepted_frame_nested_invocation_id = match &frame {
            ResourceServerFrame::Accepted(accepted) => {
                if accepted.resource_kind != resource_kind
                    || !valid_resource_invocation_id(accepted.nested_invocation_id)
                {
                    return Err(ResourceTransportFailure::Shape);
                }
                Some(accepted.nested_invocation_id)
            }
            _ => None,
        };
        let value_batch = match &frame {
            ResourceServerFrame::Values(values) => {
                if values.values.is_empty()
                    || values.item_count == 0
                    || values.item_count as usize != values.values.len()
                    || values.byte_count == 0
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
        if matches!(
            disposition_action,
            ResourceFrameDispositionAction::Apply | ResourceFrameDispositionAction::Drain
        ) {
            // Drain still advances the local protocol state. The next late
            // terminal is validated against every drained batch before an
            // authenticated committed terminal can be published.
            connection = candidate;
            if let Some(nested_invocation_id) = accepted_frame_nested_invocation_id {
                accepted = true;
                accepted_nested_invocation_id = Some(nested_invocation_id);
            }
        }
        if let Some((values, item_count, byte_count)) = value_batch {
            match resource_kind {
                ProtocolResourceKind::Single => {
                    scalar_value = values.into_iter().next();
                }
                ProtocolResourceKind::Stream => {
                    // Drain late values after cancellation without publishing or crediting them.
                    if !cancellation_requested
                        && matches!(disposition_action, ResourceFrameDispositionAction::Apply)
                    {
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
        if matches!(disposition_action, ResourceFrameDispositionAction::Drop) {
            continue;
        }
        if cancellation_first_terminal {
            scalar_value.take();
            return Ok(ResourceTransportRun {
                stream,
                protocol: connection,
                outcome: ResourceTransportOutcome::Cancelled {
                    nested_invocation_id: accepted_nested_invocation_id,
                },
            });
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
                            outcome: ResourceTransportOutcome::Ready {
                                value,
                                nested_invocation_id: accepted_nested_invocation_id
                                    .ok_or(ResourceTransportFailure::Shape)?,
                            },
                        });
                    }
                    ProtocolResourceKind::Stream => {
                        return Ok(ResourceTransportRun {
                            stream,
                            protocol: connection,
                            outcome: ResourceTransportOutcome::StreamCompleted {
                                nested_invocation_id: accepted_nested_invocation_id
                                    .ok_or(ResourceTransportFailure::Shape)?,
                            },
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
                    outcome: ResourceTransportOutcome::Failed {
                        failure,
                        nested_invocation_id: accepted_nested_invocation_id,
                    },
                });
            }
            ResourceFrameResult::Cancelled => {
                return Ok(ResourceTransportRun {
                    stream,
                    protocol: connection,
                    outcome: ResourceTransportOutcome::Cancelled {
                        nested_invocation_id: accepted_nested_invocation_id,
                    },
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

fn endpoint_transport(
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
fn connect_local_socket(path: &PathBuf) -> io::Result<StandardUnixStream> {
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

fn bind_installed_cli_arguments(
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

fn installed_cli_resolved_type(
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

fn canonicalise_invocation_arguments(
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
fn build_sealed_request(
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
fn installed_tty_runtime_offers() -> Vec<InvocationRuntimeOffer> {
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
fn map_qt_runtime_load_error(_error: orna_client::RuntimeLoadError) -> InstalledInvokeError {
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
fn selected_runtime(
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
fn client_sink_offers(
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

fn usage_error(message: String) -> InstalledInvokeError {
    InstalledInvokeError::new(InstalledInvokeErrorKind::Usage, message)
}

fn map_host_error(error: EmbeddedHostError) -> InstalledInvokeError {
    InstalledInvokeError::new(
        InstalledInvokeErrorKind::Internal,
        format!("the installed host is unavailable: {error}"),
    )
}

#[cfg(test)]
mod tests;
