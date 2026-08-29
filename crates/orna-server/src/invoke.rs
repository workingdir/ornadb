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
mod inspect;
mod installed;
mod presentation;

use inspect::{
    run_installed_external_contract, run_installed_inspect, same_resource_request_identity,
};
#[cfg(test)]
use installed::{
    InvokeTransport, bind_installed_cli_arguments, build_output_requirement, build_sealed_request,
    canonicalise_invocation_arguments, client_sink_offers, connect_local_socket,
    endpoint_transport, installed_cli_resolved_type, installed_tty_runtime_offers,
    map_qt_runtime_load_error, selected_runtime,
};
pub use installed::{run_installed_invoke, run_installed_invoke_at, run_invoke_with_kernel};
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
    dynamic_context: Arc<Mutex<Option<DynamicInvocationContext>>>,
    resource_expectations: BrokerResourceExpectations,
    next_resource_stream_id: std::sync::Arc<std::sync::Mutex<u64>>,
    resource_terminal_provenance: BrokerResourceProvenance,
}

#[derive(Clone)]
struct DynamicInvocationContext {
    active: ActiveDatabaseRevision,
    security: orna_core::security::SecuritySnapshot,
    session: AuthenticatedSession,
    root_invocation: InvocationId,
}

impl SharedInvokeBroker {
    pub(crate) fn bind_dynamic_context(
        &self,
        active: ActiveDatabaseRevision,
        security: orna_core::security::SecuritySnapshot,
        session: AuthenticatedSession,
        root_invocation: InvocationId,
    ) {
        *self
            .dynamic_context
            .lock()
            .expect("dynamic invocation context lock") = Some(DynamicInvocationContext {
            active,
            security,
            session,
            root_invocation,
        });
    }

    fn dynamic_context(&self) -> Option<DynamicInvocationContext> {
        self.dynamic_context
            .lock()
            .expect("dynamic invocation context lock")
            .clone()
    }
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
    fn evaluate_command(
        &mut self,
        context: ClientExecutionContext,
        command: &str,
    ) -> Result<RuntimeValue, String> {
        let Some(broker) = self.broker.as_ref() else {
            return Err("client.dynamic_invocation_unavailable".to_owned());
        };
        let Some(dynamic) = broker.dynamic_context() else {
            return Err("client.dynamic_invocation_unavailable".to_owned());
        };
        if self.cancellation.is_requested() {
            return Err("client.dynamic_invocation_cancelled".to_owned());
        }
        let tokens = command.split_whitespace().collect::<Vec<_>>();
        let target_name = tokens
            .first()
            .copied()
            .filter(|value| value.split('.').count() > 1)
            .ok_or_else(|| "client.dynamic_invocation_invalid_command".to_owned())?;
        let name = QualifiedSemanticName::new(target_name.split('.').map(str::to_owned))
            .map_err(|_| "client.dynamic_invocation_invalid_command".to_owned())?;
        let target = InvocationTarget::qualified_name(name)
            .map_err(|_| "client.dynamic_invocation_invalid_command".to_owned())?;
        let standard = dynamic.active.catalogue_hash_context().standard();
        let resolved = installed::resolve_target(&dynamic.active, standard, &target)
            .map_err(|_| "client.dynamic_invocation_invalid_command".to_owned())?;
        if resolved.function.domain() != FunctionDomain::Client {
            return Err("client.dynamic_invocation_target_not_client".to_owned());
        }
        let target = orna_core::security::InvocationTarget::new(
            resolved.function.id(),
            dynamic.active.pair(),
        );
        let orna_core::security::ExecuteDecision::Allowed(authorisation) =
            dynamic.security.authorise_execute(&dynamic.session, target)
        else {
            return Err("client.dynamic_invocation_denied".to_owned());
        };
        let mut state = orna_client::ClientStateStore::new();
        let grants = orna_client::capability::LocalCapabilityGrantSet::new();
        let result = orna_client::evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
            &dynamic.active,
            &authorisation,
            &[],
            &[],
            &grants,
            &mut state,
            context.parent_invocation_id(),
            self,
        )
        .map_err(|_| "client.dynamic_invocation_failed".to_owned())?;
        Ok(result.into_value())
    }
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
    fn evaluate_command(
        &mut self,
        context: ClientExecutionContext,
        command: &str,
    ) -> Result<RuntimeValue, String> {
        InstalledClientResourceExecutor::evaluate_command(self, context, command)
    }
    #[cfg(test)]
    #[test]
    fn trait_dispatches_dynamic_command_to_installed_executor() {
        let _ = <Self as ClientResourceExecutor>::evaluate_command;
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
                dynamic_context: Arc::new(Mutex::new(None)),
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

#[cfg(test)]
mod tests;
