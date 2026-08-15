//! Bounded raw-call protocol frames and connection state.

use std::{collections::BTreeMap, error::Error, fmt};

use orna_core::{
    FunctionId, InvocationId, ParameterId, TypeId,
    catalogue::CatalogueSnapshot,
    invocation::invocation_carrier_type_id,
    revision::ActiveDatabaseRevision,
    types::TypeDescriptor,
    value::{OpaqueCodecRegistry, RuntimeValue},
};

use crate::{
    ValueCodecError, decode_active_value, decode_catalogue_value, decode_constructed_value,
    decode_registered_value, decode_value, encode_active_value, encode_catalogue_value,
    encode_constructed_value, encode_registered_value, encode_value,
};

const MARKER: &[u8; 4] = b"ORF1";
const CATALOGUE_MARKER: &[u8; 4] = b"ORF2";
const ACTIVE_MARKER: &[u8; 4] = b"ORF3";
const REGISTERED_MARKER: &[u8; 4] = b"ORF4";
const CONSTRUCTED_MARKER: &[u8; 4] = b"ORF5";
const HEADER_LENGTH: usize = 18;
const PING_TAG: u8 = 0x06;
const PONG_TAG: u8 = 0x86;
const CALL_RAW_START_TAG: u8 = 0x01;
const CALL_ARGUMENT_TAG: u8 = 0x02;
const CALL_ARGUMENTS_COMPLETE_TAG: u8 = 0x03;
const WINDOW_UPDATE_TAG: u8 = 0x04;
const CALL_CANCEL_TAG: u8 = 0x05;
const CALL_ACCEPTED_TAG: u8 = 0x81;
const EVENT_BATCH_TAG: u8 = 0x82;
const CALL_COMPLETED_TAG: u8 = 0x83;
const CALL_FAILED_TAG: u8 = 0x84;
const CALL_CANCELLED_TAG: u8 = 0x85;
/// The largest payload accepted by one raw-call frame.
pub const MAX_FRAME_PAYLOAD_LENGTH: usize = 16 * 1024 * 1024 + 64;

/// A separately flow-controlled raw-call output channel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Channel {
    /// Canonical typed result values.
    ResultValues,
    /// Uninterpreted result bytes.
    ResultBytes,
    /// Structured diagnostic values.
    Diagnostic,
    /// Reserved progress events.
    Progress,
    /// Reserved trace events.
    Trace,
    /// Reserved client-control events.
    ClientControl,
}

impl Channel {
    const fn wire(self) -> u8 {
        match self {
            Self::ResultValues => 0x01,
            Self::ResultBytes => 0x02,
            Self::Diagnostic => 0x03,
            Self::Progress => 0x04,
            Self::Trace => 0x05,
            Self::ClientControl => 0x06,
        }
    }

    fn from_wire(value: u8) -> Result<Self, FrameCodecError> {
        match value {
            0x01 => Ok(Self::ResultValues),
            0x02 => Ok(Self::ResultBytes),
            0x03 => Ok(Self::Diagnostic),
            0x04 => Ok(Self::Progress),
            0x05 => Ok(Self::Trace),
            0x06 => Ok(Self::ClientControl),
            value => Err(FrameCodecError::UnknownChannel { value }),
        }
    }
}

/// A closed public failure that does not expose internal error detail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallFailure {
    /// Authentication or authorisation did not permit the call.
    ExecuteDenied,
    /// The selected target was not available for execution.
    TargetUnavailable,
    /// The accepted CLIENT function could not be evaluated.
    ClientEvaluationFailed,
    /// An internal failure prevented a safe public result.
    InternalFailure,
}

impl CallFailure {
    const fn wire(self) -> [u8; 4] {
        match self {
            Self::ExecuteDenied => [0x01, 0x00, 0x01, 0x00],
            Self::TargetUnavailable => [0x02, 0x00, 0x01, 0x00],
            Self::ClientEvaluationFailed => [0x03, 0x00, 0x01, 0x00],
            Self::InternalFailure => [0xff, 0x00, 0x01, 0x00],
        }
    }

    fn from_wire(bytes: [u8; 4]) -> Result<Self, FrameCodecError> {
        match bytes {
            [0x01, 0x00, 0x01, 0x00] => Ok(Self::ExecuteDenied),
            [0x02, 0x00, 0x01, 0x00] => Ok(Self::TargetUnavailable),
            [0x03, 0x00, 0x01, 0x00] => Ok(Self::ClientEvaluationFailed),
            [0xff, 0x00, 0x01, 0x00] => Ok(Self::InternalFailure),
            bytes => Err(FrameCodecError::InvalidFailure { bytes }),
        }
    }
}

/// One raw-call event value.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// One canonical typed result value.
    Value(RuntimeValue),
    /// One non-empty uninterpreted result byte chunk.
    Bytes(Vec<u8>),
    /// One redacted structured diagnostic.
    Failure(CallFailure),
}

impl Event {
    const fn channel(&self) -> Channel {
        match self {
            Self::Value(_) => Channel::ResultValues,
            Self::Bytes(_) => Channel::ResultBytes,
            Self::Failure(_) => Channel::Diagnostic,
        }
    }

    const fn kind(&self) -> u8 {
        match self {
            Self::Value(_) => 0x01,
            Self::Bytes(_) => 0x02,
            Self::Failure(_) => 0x03,
        }
    }
}

/// One sequenced event in a server event batch.
#[derive(Clone, Debug, PartialEq)]
pub struct EventRecord {
    /// The contiguous stream-wide event sequence.
    pub sequence: u64,
    /// The event content.
    pub event: Event,
}

/// A frame sent from an authenticated client to the server.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientFrame {
    /// Starts one raw call for a stable function identity.
    CallRawStart {
        /// The strictly increasing connection-local stream number.
        stream: u64,
        /// The stable function identity.
        function: FunctionId,
    },
    /// Supplies one uniquely identified typed argument.
    CallArgument {
        /// The call stream number.
        stream: u64,
        /// The stable parameter identity.
        parameter: ParameterId,
        /// The canonical runtime value.
        value: RuntimeValue,
    },
    /// Declares that the call has no more arguments.
    CallArgumentsComplete {
        /// The call stream number.
        stream: u64,
    },
    /// Adds byte credit to one output channel.
    WindowUpdate {
        /// The call stream number.
        stream: u64,
        /// The channel whose window receives credit.
        channel: Channel,
        /// The non-zero credit increase.
        credit: u64,
    },
    /// Requests cancellation of one call.
    CallCancel {
        /// The call stream number.
        stream: u64,
    },
    /// A connection-level liveness check with an opaque token.
    Ping {
        /// The token that the server must return without interpretation.
        token: [u8; 8],
    },
}

/// A frame sent from the server to an authenticated client.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerFrame {
    /// Confirms that the adapter accepted a dispatched call.
    CallAccepted {
        /// The call stream number.
        stream: u64,
        /// The server-generated invocation identity.
        invocation: InvocationId,
    },
    /// Carries one non-empty, single-channel event batch.
    EventBatch {
        /// The call stream number.
        stream: u64,
        /// The flow-control channel used by every event.
        channel: Channel,
        /// The contiguous event records.
        events: Vec<EventRecord>,
    },
    /// Reports successful call completion.
    CallCompleted {
        /// The call stream number.
        stream: u64,
    },
    /// Reports a redacted call failure.
    CallFailed {
        /// The call stream number.
        stream: u64,
        /// The closed public failure value.
        failure: CallFailure,
    },
    /// Reports final call cancellation.
    CallCancelled {
        /// The call stream number.
        stream: u64,
    },
    /// The exact response to one connection-level liveness check.
    Pong {
        /// The opaque token supplied by the client.
        token: [u8; 8],
    },
}

/// One typed argument retained for a dispatched raw call.
#[derive(Clone, Debug, PartialEq)]
pub struct CallArgument {
    /// The stable parameter identity.
    pub parameter: ParameterId,
    /// The canonical runtime value.
    pub value: RuntimeValue,
}

/// One complete raw call with arguments in canonical parameter order.
#[derive(Clone, Debug, PartialEq)]
pub struct RawCall {
    /// The stable function identity.
    pub function: FunctionId,
    /// Unique arguments ordered by ascending parameter identity bytes.
    pub arguments: Vec<CallArgument>,
}

/// An action that the authenticated transport adapter must perform.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientAction {
    /// Authorise and dispatch one complete raw call.
    Dispatch {
        /// The connection-local call stream.
        stream: u64,
        /// The complete raw call without authentication context.
        call: RawCall,
    },
    /// Request cancellation of one dispatched call.
    Cancel {
        /// The connection-local call stream.
        stream: u64,
        /// The invocation identity when the adapter already accepted the call.
        invocation: Option<InvocationId>,
    },
    /// Send one immediate connection or pre-dispatch response.
    Send(ServerFrame),
}

/// A result supplied by the authenticated server adapter.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerAction {
    /// Accepts a dispatched call with its new invocation identity.
    Accepted {
        /// The connection-local call stream.
        stream: u64,
        /// The server-generated invocation identity.
        invocation: InvocationId,
    },
    /// Emits one non-empty, single-channel event batch.
    Events {
        /// The connection-local call stream.
        stream: u64,
        /// Events in send order. The machine assigns their sequence numbers.
        events: Vec<Event>,
    },
    /// Completes a running call successfully.
    Completed {
        /// The connection-local call stream.
        stream: u64,
    },
    /// Terminates a dispatched or running call with a redacted failure.
    Failed {
        /// The connection-local call stream.
        stream: u64,
        /// The public failure value.
        failure: CallFailure,
    },
    /// Confirms final cancellation of a dispatched or running call.
    Cancelled {
        /// The connection-local call stream.
        stream: u64,
    },
}

/// An error from the bounded raw-call connection state machine.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionError {
    /// The connection already accepted the maximum possible stream number.
    StreamNumberExhausted,
    /// A new stream number is not larger than the connection high-water mark.
    StreamNotIncreasing {
        /// The rejected stream number.
        stream: u64,
        /// The current high-water mark.
        previous: u64,
    },
    /// The connection already retains the maximum number of live streams.
    TooManyLiveStreams,
    /// A frame or action names no live stream.
    UnknownStream {
        /// The missing stream number.
        stream: u64,
    },
    /// A frame or action is not valid in the stream's current phase.
    WrongState {
        /// The affected stream number.
        stream: u64,
    },
    /// A call supplied the same stable parameter more than once.
    DuplicateArgument {
        /// The affected stream number.
        stream: u64,
        /// The duplicate parameter identity.
        parameter: ParameterId,
    },
    /// A call already retains the maximum number of arguments.
    TooManyArguments {
        /// The affected stream number.
        stream: u64,
    },
    /// A call would exceed the aggregate retained argument-byte limit.
    ArgumentsTooLarge {
        /// The affected stream number.
        stream: u64,
    },
    /// A window update would exceed the per-channel limit.
    WindowOverflow {
        /// The affected stream number.
        stream: u64,
        /// The affected channel.
        channel: Channel,
    },
    /// An event batch exceeds the available selected-channel credit.
    InsufficientCredit {
        /// The affected stream number.
        stream: u64,
        /// The affected channel.
        channel: Channel,
        /// The available byte credit.
        available: u64,
        /// The complete event-frame payload length.
        required: u64,
    },
    /// The stream cannot assign another contiguous event sequence.
    EventSequenceExhausted {
        /// The affected stream number.
        stream: u64,
    },
    /// A typed frame or event cannot satisfy the frame codec contract.
    InvalidFrame {
        /// The frame codec failure.
        source: FrameCodecError,
    },
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StreamNumberExhausted => {
                formatter.write_str("raw-call stream number is exhausted")
            }
            Self::StreamNotIncreasing { .. } => {
                formatter.write_str("raw-call stream number is not increasing")
            }
            Self::TooManyLiveStreams => {
                formatter.write_str("raw-call connection has too many live streams")
            }
            Self::UnknownStream { .. } => formatter.write_str("raw-call stream is not live"),
            Self::WrongState { .. } => {
                formatter.write_str("raw-call frame is not valid in the current state")
            }
            Self::DuplicateArgument { .. } => {
                formatter.write_str("raw-call argument is duplicated")
            }
            Self::TooManyArguments { .. } => formatter.write_str("raw call has too many arguments"),
            Self::ArgumentsTooLarge { .. } => {
                formatter.write_str("raw-call retained arguments exceed the byte limit")
            }
            Self::WindowOverflow { .. } => {
                formatter.write_str("raw-call channel window exceeds its limit")
            }
            Self::InsufficientCredit { .. } => {
                formatter.write_str("raw-call channel has insufficient byte credit")
            }
            Self::EventSequenceExhausted { .. } => {
                formatter.write_str("raw-call event sequence is exhausted")
            }
            Self::InvalidFrame { .. } => {
                formatter.write_str("raw-call state action contains an invalid frame")
            }
        }
    }
}

impl Error for ConnectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidFrame { source } => Some(source),
            _ => None,
        }
    }
}

const MAX_LIVE_STREAMS: usize = 64;
const MAX_ARGUMENTS: usize = 256;
const MAX_ARGUMENT_BYTES: usize = MAX_FRAME_PAYLOAD_LENGTH;
/// The largest byte credit retained for one raw-call output channel.
pub const MAX_CHANNEL_WINDOW: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy)]
enum FrameVersion<'a> {
    One,
    Catalogue(&'a CatalogueSnapshot),
    Active(&'a ActiveDatabaseRevision),
    Registered(&'a ActiveDatabaseRevision, &'a OpaqueCodecRegistry),
    Constructed(&'a ActiveDatabaseRevision, &'a OpaqueCodecRegistry),
}

impl FrameVersion<'_> {
    const fn marker(self) -> &'static [u8; 4] {
        match self {
            Self::One => MARKER,
            Self::Catalogue(_) => CATALOGUE_MARKER,
            Self::Active(_) => ACTIVE_MARKER,
            Self::Registered(_, _) => REGISTERED_MARKER,
            Self::Constructed(_, _) => CONSTRUCTED_MARKER,
        }
    }

    fn encode_value(self, value: &RuntimeValue) -> Result<Vec<u8>, ValueCodecError> {
        match self {
            Self::One => encode_value(value),
            Self::Catalogue(catalogue) => encode_catalogue_value(catalogue, value),
            Self::Active(active) => encode_active_value(active, value),
            Self::Registered(active, registry) => encode_registered_value(active, registry, value),
            Self::Constructed(active, registry) => {
                encode_constructed_value(active, registry, value)
            }
        }
    }

    fn decode_value(self, encoded: &[u8]) -> Result<RuntimeValue, ValueCodecError> {
        match self {
            Self::One => decode_value(encoded),
            Self::Catalogue(catalogue) => decode_catalogue_value(catalogue, encoded),
            Self::Active(active) => decode_active_value(active, encoded),
            Self::Registered(active, registry) => {
                decode_registered_value(active, registry, encoded)
            }
            Self::Constructed(active, registry) => {
                decode_constructed_value(active, registry, encoded)
            }
        }
    }

    fn require_call_argument(self, value: &RuntimeValue) -> Result<(), FrameCodecError> {
        if matches!(self, Self::Registered(_, _) | Self::Constructed(_, _))
            && let RuntimeValue::Opaque(value) = value
        {
            return Err(FrameCodecError::OpaqueArgumentNotAccepted {
                opaque_type: value.opaque_type(),
            });
        }
        self.require_ordinary_value_position_closed(value)
    }

    fn require_event_value(self, value: &RuntimeValue) -> Result<(), FrameCodecError> {
        self.require_ordinary_value_position_closed(value)
    }

    fn require_ordinary_value_position_closed(
        self,
        value: &RuntimeValue,
    ) -> Result<(), FrameCodecError> {
        if matches!(self, Self::Constructed(_, _))
            && let Some(carrier) = invocation_carrier_type_id(value)
        {
            return Err(FrameCodecError::InvocationCarrierNotAccepted { carrier });
        }
        if matches!(self, Self::Constructed(_, _))
            && let RuntimeValue::Constructed(value) = value
        {
            return Err(FrameCodecError::ConstructedValueNotAccepted {
                descriptor: value.descriptor().clone(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Receiving {
        function: FunctionId,
        arguments: BTreeMap<ParameterId, RuntimeValue>,
        argument_bytes: usize,
    },
    Dispatching,
    DispatchCancelling,
    Running {
        invocation: InvocationId,
    },
    RunningCancelling,
}

#[derive(Clone, Debug, PartialEq)]
struct StreamState {
    phase: Phase,
    windows: [u64; 6],
    last_sequence: u64,
}

impl StreamState {
    fn receiving(function: FunctionId) -> Self {
        Self {
            phase: Phase::Receiving {
                function,
                arguments: BTreeMap::new(),
                argument_bytes: 0,
            },
            windows: [0; 6],
            last_sequence: 0,
        }
    }
}

/// The bounded state machine for one authenticated raw-call connection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProtocolConnection {
    high_water_mark: Option<u64>,
    streams: BTreeMap<u64, StreamState>,
}

impl ProtocolConnection {
    /// Creates an empty connection with zero initial channel credit.
    pub const fn new() -> Self {
        Self {
            high_water_mark: None,
            streams: BTreeMap::new(),
        }
    }

    /// Returns the highest stream number that this connection accepted.
    pub const fn high_water_mark(&self) -> Option<u64> {
        self.high_water_mark
    }

    /// Returns the number of currently retained call streams.
    pub fn live_streams(&self) -> usize {
        self.streams.len()
    }

    /// Receives one validated client frame and returns at most one adapter action.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the frame violates a state transition
    /// or bounded connection limit. An error leaves all prior state unchanged.
    pub fn receive(&mut self, frame: ClientFrame) -> Result<Option<ClientAction>, ConnectionError> {
        self.receive_with_version(FrameVersion::One, frame)
    }

    /// Receives one catalogue-bound version-2 client frame.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the frame violates a state transition,
    /// bounded connection limit, or active-catalogue value rule. An error leaves
    /// all prior state unchanged.
    pub fn receive_catalogue(
        &mut self,
        catalogue: &CatalogueSnapshot,
        frame: ClientFrame,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        self.receive_with_version(FrameVersion::Catalogue(catalogue), frame)
    }

    /// Receives one active-revision version-3 client frame.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the frame violates a state transition,
    /// bounded connection limit, or active-revision value rule. An error leaves
    /// all prior state unchanged.
    pub fn receive_active(
        &mut self,
        active: &ActiveDatabaseRevision,
        frame: ClientFrame,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        self.receive_with_version(FrameVersion::Active(active), frame)
    }

    /// Receives one registry-bound version-4 client frame.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the frame violates a state transition,
    /// bounded connection limit, active-revision rule, or the closed opaque
    /// argument boundary. An error leaves all prior state unchanged.
    pub fn receive_registered(
        &mut self,
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
        frame: ClientFrame,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        self.receive_with_version(FrameVersion::Registered(active, registry), frame)
    }

    /// Receives one registry-bound version-5 client frame.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the frame violates a state transition,
    /// bounded connection limit, active-revision rule, opaque argument boundary,
    /// closed constructed application-value boundary, or sealed invocation
    /// carrier in an ordinary argument position. An error leaves all prior state
    /// unchanged.
    pub fn receive_constructed(
        &mut self,
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
        frame: ClientFrame,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        self.receive_with_version(FrameVersion::Constructed(active, registry), frame)
    }

    fn receive_with_version(
        &mut self,
        version: FrameVersion<'_>,
        frame: ClientFrame,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        match frame {
            ClientFrame::CallRawStart { stream, function } => self.start(stream, function),
            ClientFrame::CallArgument {
                stream,
                parameter,
                value,
            } => self.argument(version, stream, parameter, value),
            ClientFrame::CallArgumentsComplete { stream } => self.complete_arguments(stream),
            ClientFrame::WindowUpdate {
                stream,
                channel,
                credit,
            } => self.update_window(stream, channel, credit),
            ClientFrame::CallCancel { stream } => self.cancel(stream),
            ClientFrame::Ping { token } => {
                Ok(Some(ClientAction::Send(ServerFrame::Pong { token })))
            }
        }
    }

    /// Applies one serialised server-adapter result and returns its client frame.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the action violates the current call
    /// state, sequence, frame, or flow-control contract. An error leaves all
    /// prior state and window credit unchanged.
    pub fn apply(&mut self, action: ServerAction) -> Result<ServerFrame, ConnectionError> {
        self.apply_with_version(FrameVersion::One, action)
    }

    /// Applies one catalogue-bound version-2 server-adapter result.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the action violates the current call
    /// state, sequence, frame, flow-control, or active-catalogue value contract.
    /// An error leaves all prior state and window credit unchanged.
    pub fn apply_catalogue(
        &mut self,
        catalogue: &CatalogueSnapshot,
        action: ServerAction,
    ) -> Result<ServerFrame, ConnectionError> {
        self.apply_with_version(FrameVersion::Catalogue(catalogue), action)
    }

    /// Applies one active-revision version-3 server-adapter result.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the action violates the current call
    /// state, sequence, frame, flow-control, or active-revision value contract.
    /// An error leaves all prior state and window credit unchanged.
    pub fn apply_active(
        &mut self,
        active: &ActiveDatabaseRevision,
        action: ServerAction,
    ) -> Result<ServerFrame, ConnectionError> {
        self.apply_with_version(FrameVersion::Active(active), action)
    }

    /// Applies one registry-bound version-4 server-adapter result.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the action violates the current call
    /// state, sequence, frame, flow-control, active-revision, or opaque registry
    /// contract. An error leaves all prior state and window credit unchanged.
    pub fn apply_registered(
        &mut self,
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
        action: ServerAction,
    ) -> Result<ServerFrame, ConnectionError> {
        self.apply_with_version(FrameVersion::Registered(active, registry), action)
    }

    /// Applies one registry-bound version-5 server-adapter result.
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] when the action violates the current call
    /// state, sequence, frame, flow-control, active-revision, opaque registry,
    /// closed constructed application-value contract, or sealed invocation
    /// carrier in an ordinary event position. An error leaves all prior state
    /// and window credit unchanged.
    pub fn apply_constructed(
        &mut self,
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
        action: ServerAction,
    ) -> Result<ServerFrame, ConnectionError> {
        self.apply_with_version(FrameVersion::Constructed(active, registry), action)
    }

    fn apply_with_version(
        &mut self,
        version: FrameVersion<'_>,
        action: ServerAction,
    ) -> Result<ServerFrame, ConnectionError> {
        match action {
            ServerAction::Accepted { stream, invocation } => self.accept(stream, invocation),
            ServerAction::Events { stream, events } => self.events(version, stream, events),
            ServerAction::Completed { stream } => self.complete(stream),
            ServerAction::Failed { stream, failure } => self.fail(stream, failure),
            ServerAction::Cancelled { stream } => self.cancelled(stream),
        }
    }

    fn start(
        &mut self,
        stream: u64,
        function: FunctionId,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        if self.high_water_mark == Some(u64::MAX) {
            return Err(ConnectionError::StreamNumberExhausted);
        }
        if let Some(previous) = self.high_water_mark
            && stream <= previous
        {
            return Err(ConnectionError::StreamNotIncreasing { stream, previous });
        }
        if stream == 0 {
            return Err(ConnectionError::StreamNotIncreasing {
                stream,
                previous: self.high_water_mark.unwrap_or(0),
            });
        }
        if self.streams.len() == MAX_LIVE_STREAMS {
            return Err(ConnectionError::TooManyLiveStreams);
        }
        self.streams
            .insert(stream, StreamState::receiving(function));
        self.high_water_mark = Some(stream);
        Ok(None)
    }

    fn argument(
        &mut self,
        version: FrameVersion<'_>,
        stream: u64,
        parameter: ParameterId,
        value: RuntimeValue,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        version
            .require_call_argument(&value)
            .map_err(|source| ConnectionError::InvalidFrame { source })?;
        let value_length = version
            .encode_value(&value)
            .map_err(|source| ConnectionError::InvalidFrame {
                source: FrameCodecError::Value { source },
            })?
            .len();
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        let Phase::Receiving {
            arguments,
            argument_bytes,
            ..
        } = &state.phase
        else {
            return Err(ConnectionError::WrongState { stream });
        };
        if arguments.contains_key(&parameter) {
            return Err(ConnectionError::DuplicateArgument { stream, parameter });
        }
        if arguments.len() == MAX_ARGUMENTS {
            return Err(ConnectionError::TooManyArguments { stream });
        }
        let next_bytes = argument_bytes
            .checked_add(16 + value_length)
            .filter(|value| *value <= MAX_ARGUMENT_BYTES)
            .ok_or(ConnectionError::ArgumentsTooLarge { stream })?;
        let state = self.streams.get_mut(&stream).expect("live stream checked");
        let Phase::Receiving {
            arguments,
            argument_bytes,
            ..
        } = &mut state.phase
        else {
            unreachable!("phase checked before mutation");
        };
        arguments.insert(parameter, value);
        *argument_bytes = next_bytes;
        Ok(None)
    }

    fn complete_arguments(&mut self, stream: u64) -> Result<Option<ClientAction>, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        let Phase::Receiving {
            function,
            arguments,
            ..
        } = &state.phase
        else {
            return Err(ConnectionError::WrongState { stream });
        };
        let call = RawCall {
            function: *function,
            arguments: arguments
                .iter()
                .map(|(parameter, value)| CallArgument {
                    parameter: *parameter,
                    value: value.clone(),
                })
                .collect(),
        };
        self.streams
            .get_mut(&stream)
            .expect("live stream checked")
            .phase = Phase::Dispatching;
        Ok(Some(ClientAction::Dispatch { stream, call }))
    }

    fn update_window(
        &mut self,
        stream: u64,
        channel: Channel,
        credit: u64,
    ) -> Result<Option<ClientAction>, ConnectionError> {
        if credit == 0 {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::ZeroWindowCredit,
            });
        }
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        let index = channel_index(channel);
        let next = state.windows[index]
            .checked_add(credit)
            .filter(|value| *value <= MAX_CHANNEL_WINDOW)
            .ok_or(ConnectionError::WindowOverflow { stream, channel })?;
        self.streams
            .get_mut(&stream)
            .expect("live stream checked")
            .windows[index] = next;
        Ok(None)
    }

    fn cancel(&mut self, stream: u64) -> Result<Option<ClientAction>, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        match state.phase {
            Phase::Receiving { .. } => {
                self.streams.remove(&stream);
                Ok(Some(ClientAction::Send(ServerFrame::CallCancelled {
                    stream,
                })))
            }
            Phase::Dispatching => {
                self.streams
                    .get_mut(&stream)
                    .expect("live stream checked")
                    .phase = Phase::DispatchCancelling;
                Ok(Some(ClientAction::Cancel {
                    stream,
                    invocation: None,
                }))
            }
            Phase::Running { invocation } => {
                self.streams
                    .get_mut(&stream)
                    .expect("live stream checked")
                    .phase = Phase::RunningCancelling;
                Ok(Some(ClientAction::Cancel {
                    stream,
                    invocation: Some(invocation),
                }))
            }
            Phase::DispatchCancelling | Phase::RunningCancelling => {
                Err(ConnectionError::WrongState { stream })
            }
        }
    }

    fn accept(
        &mut self,
        stream: u64,
        invocation: InvocationId,
    ) -> Result<ServerFrame, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if !matches!(state.phase, Phase::Dispatching) {
            return Err(ConnectionError::WrongState { stream });
        }
        self.streams
            .get_mut(&stream)
            .expect("live stream checked")
            .phase = Phase::Running { invocation };
        Ok(ServerFrame::CallAccepted { stream, invocation })
    }

    fn events(
        &mut self,
        version: FrameVersion<'_>,
        stream: u64,
        events: Vec<Event>,
    ) -> Result<ServerFrame, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if !matches!(
            state.phase,
            Phase::Running { .. } | Phase::RunningCancelling
        ) {
            return Err(ConnectionError::WrongState { stream });
        }
        let Some(first) = events.first() else {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::EmptyEventBatch,
            });
        };
        let channel = first.channel();
        if events.iter().any(|event| event.channel() != channel) {
            return Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::InvalidEventChannel {
                    channel,
                    kind: events
                        .iter()
                        .find(|event| event.channel() != channel)
                        .expect("mismatched event found")
                        .kind(),
                },
            });
        }
        let count = u64::try_from(events.len()).expect("usize fits u64");
        let first_sequence = state
            .last_sequence
            .checked_add(1)
            .ok_or(ConnectionError::EventSequenceExhausted { stream })?;
        state
            .last_sequence
            .checked_add(count)
            .ok_or(ConnectionError::EventSequenceExhausted { stream })?;
        let records: Vec<_> = events
            .into_iter()
            .enumerate()
            .map(|(index, event)| EventRecord {
                sequence: first_sequence + index as u64,
                event,
            })
            .collect();
        let frame = ServerFrame::EventBatch {
            stream,
            channel,
            events: records,
        };
        let payload_length = encode_server_frame_with_version(version, &frame)
            .map_err(|source| ConnectionError::InvalidFrame { source })?
            .len()
            - HEADER_LENGTH;
        let required = payload_length as u64;
        let index = channel_index(channel);
        let available = state.windows[index];
        if available < required {
            return Err(ConnectionError::InsufficientCredit {
                stream,
                channel,
                available,
                required,
            });
        }
        let state = self.streams.get_mut(&stream).expect("live stream checked");
        state.windows[index] -= required;
        state.last_sequence += count;
        Ok(frame)
    }

    fn complete(&mut self, stream: u64) -> Result<ServerFrame, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if !matches!(
            state.phase,
            Phase::Running { .. } | Phase::RunningCancelling
        ) {
            return Err(ConnectionError::WrongState { stream });
        }
        self.streams.remove(&stream);
        Ok(ServerFrame::CallCompleted { stream })
    }

    fn fail(&mut self, stream: u64, failure: CallFailure) -> Result<ServerFrame, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if !matches!(
            state.phase,
            Phase::Dispatching
                | Phase::DispatchCancelling
                | Phase::Running { .. }
                | Phase::RunningCancelling
        ) {
            return Err(ConnectionError::WrongState { stream });
        }
        self.streams.remove(&stream);
        Ok(ServerFrame::CallFailed { stream, failure })
    }

    fn cancelled(&mut self, stream: u64) -> Result<ServerFrame, ConnectionError> {
        let state = self
            .streams
            .get(&stream)
            .ok_or(ConnectionError::UnknownStream { stream })?;
        if !matches!(
            state.phase,
            Phase::DispatchCancelling | Phase::RunningCancelling
        ) {
            return Err(ConnectionError::WrongState { stream });
        }
        self.streams.remove(&stream);
        Ok(ServerFrame::CallCancelled { stream })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawCallClientPhase {
    AwaitingAcceptance,
    Running,
    Terminal,
}

/// The validated result of one server frame received by a raw-call client.
#[derive(Clone, Debug, PartialEq)]
pub enum RawCallClientResponse {
    /// The server accepted the call and assigned its invocation identity.
    Accepted {
        /// The server-assigned invocation identity.
        invocation: InvocationId,
    },
    /// One non-empty ordered batch of canonical result values.
    Values(Vec<RuntimeValue>),
    /// The call completed successfully.
    Completed,
    /// The call returned one closed public failure.
    Failed(CallFailure),
    /// The requested cancellation completed.
    Cancelled,
}

/// A protocol or state failure while receiving one raw-call result.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RawCallClientError {
    /// The encoded server frame is malformed or uses another codec marker.
    Frame {
        /// The frame codec failure.
        source: FrameCodecError,
    },
    /// A frame names another stream.
    WrongStream {
        /// The required stream number.
        expected: u64,
        /// The received stream number.
        actual: u64,
    },
    /// A frame is not valid in the current client phase.
    WrongState,
    /// An event batch uses a channel other than `RESULT_VALUES`.
    WrongChannel {
        /// The received channel.
        actual: Channel,
    },
    /// A result batch contains a non-value event.
    WrongEvent,
    /// An event does not have the next contiguous sequence number.
    WrongSequence {
        /// The required next sequence number.
        expected: u64,
        /// The received sequence number.
        actual: u64,
    },
    /// The contiguous event sequence cannot represent another value.
    SequenceExhausted,
    /// A result batch exceeds the remaining granted byte credit.
    InsufficientCredit {
        /// The remaining result-value credit.
        available: u64,
        /// The complete event-frame payload length.
        required: u64,
    },
}

impl fmt::Display for RawCallClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame { .. } => formatter.write_str("raw-call server frame is invalid"),
            Self::WrongStream { .. } => {
                formatter.write_str("raw-call server frame uses the wrong stream")
            }
            Self::WrongState => {
                formatter.write_str("raw-call server frame is not valid in the current state")
            }
            Self::WrongChannel { .. } => {
                formatter.write_str("raw-call server event uses the wrong channel")
            }
            Self::WrongEvent => formatter.write_str("raw-call server event is not a value"),
            Self::WrongSequence { .. } => {
                formatter.write_str("raw-call server event sequence is not contiguous")
            }
            Self::SequenceExhausted => {
                formatter.write_str("raw-call server event sequence is exhausted")
            }
            Self::InsufficientCredit { .. } => {
                formatter.write_str("raw-call server exceeded its result-value credit")
            }
        }
    }
}

impl Error for RawCallClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frame { source } => Some(source),
            _ => None,
        }
    }
}

/// The bounded response state for one parameter-free protocol-1 raw call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawCallClient {
    phase: RawCallClientPhase,
    cancellation_requested: bool,
    next_sequence: Option<u64>,
    remaining_result_credit: u64,
}

impl RawCallClient {
    /// Starts stream 1 and returns the exact initial client frames in wire order.
    pub fn start(function: FunctionId) -> (Self, [ClientFrame; 3]) {
        (
            Self {
                phase: RawCallClientPhase::AwaitingAcceptance,
                cancellation_requested: false,
                next_sequence: Some(1),
                remaining_result_credit: MAX_CHANNEL_WINDOW,
            },
            [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: MAX_CHANNEL_WINDOW,
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ],
        )
    }

    /// Requests cancellation once after stream 1 has been created.
    ///
    /// # Errors
    ///
    /// Returns [`RawCallClientError::WrongState`] after a prior cancellation
    /// request or terminal result.
    pub fn request_cancellation(&mut self) -> Result<ClientFrame, RawCallClientError> {
        if self.phase == RawCallClientPhase::Terminal || self.cancellation_requested {
            return Err(RawCallClientError::WrongState);
        }
        self.cancellation_requested = true;
        Ok(ClientFrame::CallCancel { stream: 1 })
    }

    /// Decodes and validates one complete protocol-1 server frame.
    ///
    /// # Errors
    ///
    /// Returns [`RawCallClientError`] when the frame bytes, stream, phase,
    /// channel, event kind, or sequence violate the closed client contract.
    pub fn receive_encoded(
        &mut self,
        encoded: &[u8],
    ) -> Result<RawCallClientResponse, RawCallClientError> {
        let frame =
            decode_server_frame(encoded).map_err(|source| RawCallClientError::Frame { source })?;
        let event_payload_length = matches!(frame, ServerFrame::EventBatch { .. }).then(|| {
            u64::try_from(encoded.len() - HEADER_LENGTH)
                .expect("frame payload length is bounded by u32")
        });
        if let Some(required) = event_payload_length
            && self.remaining_result_credit < required
        {
            return Err(RawCallClientError::InsufficientCredit {
                available: self.remaining_result_credit,
                required,
            });
        }
        let response = self.receive(frame)?;
        if let Some(required) = event_payload_length {
            self.remaining_result_credit -= required;
        }
        Ok(response)
    }

    fn receive(&mut self, frame: ServerFrame) -> Result<RawCallClientResponse, RawCallClientError> {
        let stream = server_frame_stream(&frame);
        if stream != 1 {
            return Err(RawCallClientError::WrongStream {
                expected: 1,
                actual: stream,
            });
        }
        match frame {
            ServerFrame::CallAccepted { invocation, .. }
                if self.phase == RawCallClientPhase::AwaitingAcceptance =>
            {
                self.phase = RawCallClientPhase::Running;
                Ok(RawCallClientResponse::Accepted { invocation })
            }
            ServerFrame::EventBatch {
                channel, events, ..
            } if self.phase == RawCallClientPhase::Running => {
                if channel != Channel::ResultValues {
                    return Err(RawCallClientError::WrongChannel { actual: channel });
                }
                let mut values = Vec::with_capacity(events.len());
                let mut next_sequence = self.next_sequence;
                for event in events {
                    let Some(expected) = next_sequence else {
                        return Err(RawCallClientError::SequenceExhausted);
                    };
                    if event.sequence != expected {
                        return Err(RawCallClientError::WrongSequence {
                            expected,
                            actual: event.sequence,
                        });
                    }
                    let Event::Value(value) = event.event else {
                        return Err(RawCallClientError::WrongEvent);
                    };
                    values.push(value);
                    next_sequence = expected.checked_add(1);
                }
                self.next_sequence = next_sequence;
                Ok(RawCallClientResponse::Values(values))
            }
            ServerFrame::CallCompleted { .. } if self.phase == RawCallClientPhase::Running => {
                self.phase = RawCallClientPhase::Terminal;
                Ok(RawCallClientResponse::Completed)
            }
            ServerFrame::CallFailed { failure, .. }
                if self.phase == RawCallClientPhase::Running =>
            {
                self.phase = RawCallClientPhase::Terminal;
                Ok(RawCallClientResponse::Failed(failure))
            }
            ServerFrame::CallCancelled { .. }
                if matches!(
                    self.phase,
                    RawCallClientPhase::AwaitingAcceptance | RawCallClientPhase::Running
                ) && self.cancellation_requested =>
            {
                self.phase = RawCallClientPhase::Terminal;
                Ok(RawCallClientResponse::Cancelled)
            }
            _ => Err(RawCallClientError::WrongState),
        }
    }
}

const fn server_frame_stream(frame: &ServerFrame) -> u64 {
    match frame {
        ServerFrame::CallAccepted { stream, .. }
        | ServerFrame::EventBatch { stream, .. }
        | ServerFrame::CallCompleted { stream }
        | ServerFrame::CallFailed { stream, .. }
        | ServerFrame::CallCancelled { stream } => *stream,
        ServerFrame::Pong { .. } => 0,
    }
}

const fn channel_index(channel: Channel) -> usize {
    channel.wire() as usize - 1
}

/// An error from raw-call frame encoding or decoding.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameCodecError {
    /// The encoded frame does not contain the complete fixed header.
    TruncatedHeader {
        /// The total number of available bytes.
        actual: usize,
    },
    /// The encoded frame does not start with the selected protocol marker.
    InvalidMarker,
    /// The frame tag belongs to the opposite direction.
    WrongDirection {
        /// The recognised tag from the opposite direction.
        tag: u8,
    },
    /// The frame tag is not defined by the selected protocol version.
    UnknownTag {
        /// The unrecognised frame tag.
        tag: u8,
    },
    /// The selected protocol version requires the flags byte to be zero.
    NonZeroFlags {
        /// The unsupported flags byte.
        flags: u8,
    },
    /// The frame uses an invalid stream number for its tag.
    InvalidStream {
        /// The recognised frame tag.
        tag: u8,
        /// The supplied stream number.
        stream: u64,
    },
    /// A supplied or declared payload exceeds the shared frame limit.
    PayloadTooLarge {
        /// The supplied or declared payload length.
        actual: usize,
        /// The shared raw-call frame payload limit.
        maximum: usize,
    },
    /// The encoded payload is shorter than its declared length.
    TruncatedPayload {
        /// The length declared by the frame header.
        declared: usize,
        /// The number of available payload bytes.
        actual: usize,
    },
    /// Bytes occur after the declared payload.
    TrailingBytes {
        /// The length declared by the frame header.
        declared: usize,
        /// The number of available payload bytes.
        actual: usize,
    },
    /// A frame payload has the wrong fixed length.
    WrongPayloadLength {
        /// The recognised frame tag.
        tag: u8,
        /// The only valid payload length for this tag.
        expected: usize,
        /// The declared and available payload length.
        actual: usize,
    },
    /// A call-argument payload does not contain its complete parameter prefix.
    ArgumentPrefixTooShort {
        /// The number of available prefix bytes.
        actual: usize,
    },
    /// The window-update payload names an undefined channel.
    UnknownChannel {
        /// The unrecognised channel byte.
        value: u8,
    },
    /// A window update supplies no additional credit.
    ZeroWindowCredit,
    /// A canonical runtime value in a frame is invalid.
    Value {
        /// The canonical value codec failure.
        source: ValueCodecError,
    },
    /// Protocol 4 does not admit an opaque call argument.
    OpaqueArgumentNotAccepted {
        /// The rejected opaque type identity.
        opaque_type: TypeId,
    },
    /// Protocol 5 does not admit a constructed application argument or result.
    ConstructedValueNotAccepted {
        /// The rejected constructed value descriptor.
        descriptor: TypeDescriptor,
    },
    /// Protocol 5 does not admit a sealed invocation carrier in an ordinary frame position.
    InvocationCarrierNotAccepted {
        /// The rejected sealed carrier identity.
        carrier: TypeId,
    },
    /// A failure payload is not one of the four closed values.
    InvalidFailure {
        /// The invalid four-byte failure value.
        bytes: [u8; 4],
    },
    /// An event batch contains no events.
    EmptyEventBatch,
    /// An event batch exceeds the unsigned 16-bit count field.
    TooManyEvents {
        /// The supplied number of events.
        actual: usize,
    },
    /// An event kind does not belong to the selected channel.
    InvalidEventChannel {
        /// The selected channel.
        channel: Channel,
        /// The supplied or decoded event kind.
        kind: u8,
    },
    /// An uninterpreted byte event has no content.
    EmptyByteChunk,
    /// An event batch ends before a declared entry is complete.
    TruncatedEventBatch,
    /// An event batch contains bytes after its declared events.
    TrailingEventBytes,
    /// Event sequences in one batch are zero, non-contiguous, or overflow.
    InvalidEventSequence,
    /// An event content field has the wrong fixed length.
    WrongEventContentLength {
        /// The recognised event kind.
        kind: u8,
        /// The only valid content length for this event kind.
        expected: usize,
        /// The supplied content length.
        actual: usize,
    },
}

impl fmt::Display for FrameCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedHeader { .. } => {
                formatter.write_str("raw-call frame header is truncated")
            }
            Self::InvalidMarker => formatter.write_str("raw-call frame marker is invalid"),
            Self::WrongDirection { .. } => {
                formatter.write_str("raw-call frame has the wrong direction")
            }
            Self::UnknownTag { .. } => formatter.write_str("raw-call frame tag is unknown"),
            Self::NonZeroFlags { .. } => {
                formatter.write_str("raw-call frame flags are not supported")
            }
            Self::InvalidStream { .. } => {
                formatter.write_str("raw-call frame stream number is invalid")
            }
            Self::PayloadTooLarge { .. } => {
                formatter.write_str("raw-call frame payload exceeds the protocol limit")
            }
            Self::TruncatedPayload { .. } => {
                formatter.write_str("raw-call frame payload is truncated")
            }
            Self::TrailingBytes { .. } => formatter.write_str("raw-call frame has trailing bytes"),
            Self::WrongPayloadLength { .. } => {
                formatter.write_str("raw-call frame payload has the wrong length")
            }
            Self::ArgumentPrefixTooShort { .. } => {
                formatter.write_str("raw-call argument parameter prefix is truncated")
            }
            Self::UnknownChannel { .. } => formatter.write_str("raw-call frame channel is unknown"),
            Self::ZeroWindowCredit => {
                formatter.write_str("raw-call window credit must be non-zero")
            }
            Self::Value { .. } => formatter.write_str("raw-call frame value is invalid"),
            Self::OpaqueArgumentNotAccepted { .. } => {
                formatter.write_str("raw-call opaque arguments are not accepted")
            }
            Self::ConstructedValueNotAccepted { .. } => formatter
                .write_str("constructed runtime values are not accepted by protocol 5 frames"),
            Self::InvocationCarrierNotAccepted { .. } => formatter.write_str(
                "sealed invocation carriers are not accepted by ordinary protocol 5 frames",
            ),
            Self::InvalidFailure { .. } => formatter.write_str("raw-call failure value is invalid"),
            Self::EmptyEventBatch => formatter.write_str("raw-call event batch is empty"),
            Self::TooManyEvents { .. } => {
                formatter.write_str("raw-call event batch has too many events")
            }
            Self::InvalidEventChannel { .. } => {
                formatter.write_str("raw-call event does not belong to its channel")
            }
            Self::EmptyByteChunk => formatter.write_str("raw-call byte chunk is empty"),
            Self::TruncatedEventBatch => formatter.write_str("raw-call event batch is truncated"),
            Self::TrailingEventBytes => {
                formatter.write_str("raw-call event batch has trailing bytes")
            }
            Self::InvalidEventSequence => formatter.write_str("raw-call event sequence is invalid"),
            Self::WrongEventContentLength { .. } => {
                formatter.write_str("raw-call event content has the wrong length")
            }
        }
    }
}

impl Error for FrameCodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Value { source } => Some(source),
            _ => None,
        }
    }
}

/// Encodes one complete client frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] when the frame cannot satisfy the version-1
/// envelope or payload contract.
pub fn encode_client_frame(frame: &ClientFrame) -> Result<Vec<u8>, FrameCodecError> {
    encode_client_frame_with_version(FrameVersion::One, frame)
}

/// Encodes one complete catalogue-bound version-2 client frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] when the frame cannot satisfy the version-2
/// envelope, payload, or active-catalogue value contract.
pub fn encode_catalogue_client_frame(
    catalogue: &CatalogueSnapshot,
    frame: &ClientFrame,
) -> Result<Vec<u8>, FrameCodecError> {
    encode_client_frame_with_version(FrameVersion::Catalogue(catalogue), frame)
}

/// Encodes one complete active-revision version-3 client frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] when the frame cannot satisfy the version-3
/// envelope, payload, or active-revision value contract.
pub fn encode_active_client_frame(
    active: &ActiveDatabaseRevision,
    frame: &ClientFrame,
) -> Result<Vec<u8>, FrameCodecError> {
    encode_client_frame_with_version(FrameVersion::Active(active), frame)
}

/// Encodes one complete registry-bound version-4 client frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] when the frame cannot satisfy the version-4
/// envelope, active-revision, registry, or closed call-argument contract.
pub fn encode_registered_client_frame(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    frame: &ClientFrame,
) -> Result<Vec<u8>, FrameCodecError> {
    encode_client_frame_with_version(FrameVersion::Registered(active, registry), frame)
}

/// Encodes one complete registry-bound version-5 client frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] when the frame cannot satisfy the version-5
/// envelope, active-revision, registry, closed application-value contract, or
/// sealed invocation-carrier closure.
pub fn encode_constructed_client_frame(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    frame: &ClientFrame,
) -> Result<Vec<u8>, FrameCodecError> {
    encode_client_frame_with_version(FrameVersion::Constructed(active, registry), frame)
}

fn encode_client_frame_with_version(
    version: FrameVersion<'_>,
    frame: &ClientFrame,
) -> Result<Vec<u8>, FrameCodecError> {
    match frame {
        ClientFrame::CallRawStart { stream, function } => {
            require_stream(CALL_RAW_START_TAG, *stream, false)?;
            encode(version, CALL_RAW_START_TAG, *stream, &function.to_bytes())
        }
        ClientFrame::CallArgument {
            stream,
            parameter,
            value,
        } => {
            require_stream(CALL_ARGUMENT_TAG, *stream, false)?;
            version.require_call_argument(value)?;
            let value = version
                .encode_value(value)
                .map_err(|source| FrameCodecError::Value { source })?;
            let mut payload = Vec::with_capacity(16 + value.len());
            payload.extend_from_slice(&parameter.to_bytes());
            payload.extend_from_slice(&value);
            encode(version, CALL_ARGUMENT_TAG, *stream, &payload)
        }
        ClientFrame::CallArgumentsComplete { stream } => {
            require_stream(CALL_ARGUMENTS_COMPLETE_TAG, *stream, false)?;
            encode(version, CALL_ARGUMENTS_COMPLETE_TAG, *stream, &[])
        }
        ClientFrame::WindowUpdate {
            stream,
            channel,
            credit,
        } => {
            require_stream(WINDOW_UPDATE_TAG, *stream, false)?;
            if *credit == 0 {
                return Err(FrameCodecError::ZeroWindowCredit);
            }
            let mut payload = Vec::with_capacity(9);
            payload.push(channel.wire());
            payload.extend_from_slice(&credit.to_be_bytes());
            encode(version, WINDOW_UPDATE_TAG, *stream, &payload)
        }
        ClientFrame::CallCancel { stream } => {
            require_stream(CALL_CANCEL_TAG, *stream, false)?;
            encode(version, CALL_CANCEL_TAG, *stream, &[])
        }
        ClientFrame::Ping { token } => encode(version, PING_TAG, 0, token),
    }
}

/// Decodes one complete client frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] for an invalid envelope, wrong-direction tag,
/// unknown tag, or invalid tag-specific payload.
pub fn decode_client_frame(encoded: &[u8]) -> Result<ClientFrame, FrameCodecError> {
    decode_client_frame_with_version(FrameVersion::One, encoded)
}

/// Decodes one complete catalogue-bound version-2 client frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] for an invalid version-2 envelope,
/// wrong-direction tag, unknown tag, invalid payload, or enum value rejected
/// by the active catalogue.
pub fn decode_catalogue_client_frame(
    catalogue: &CatalogueSnapshot,
    encoded: &[u8],
) -> Result<ClientFrame, FrameCodecError> {
    decode_client_frame_with_version(FrameVersion::Catalogue(catalogue), encoded)
}

/// Decodes one complete active-revision version-3 client frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] for an invalid version-3 envelope,
/// wrong-direction tag, unknown tag, invalid payload, or value rejected by the
/// active revision.
pub fn decode_active_client_frame(
    active: &ActiveDatabaseRevision,
    encoded: &[u8],
) -> Result<ClientFrame, FrameCodecError> {
    decode_client_frame_with_version(FrameVersion::Active(active), encoded)
}

/// Decodes one complete registry-bound version-4 client frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] for an invalid version-4 envelope, payload,
/// active value, or opaque call argument.
pub fn decode_registered_client_frame(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
) -> Result<ClientFrame, FrameCodecError> {
    decode_client_frame_with_version(FrameVersion::Registered(active, registry), encoded)
}

/// Decodes one complete registry-bound version-5 client frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] for an invalid version-5 envelope, payload,
/// active value, opaque call argument, constructed application value, or
/// sealed invocation carrier in the ordinary argument position.
pub fn decode_constructed_client_frame(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
) -> Result<ClientFrame, FrameCodecError> {
    decode_client_frame_with_version(FrameVersion::Constructed(active, registry), encoded)
}

fn decode_client_frame_with_version(
    version: FrameVersion<'_>,
    encoded: &[u8],
) -> Result<ClientFrame, FrameCodecError> {
    let (tag, stream, payload) = decode_envelope(version, encoded)?;
    match tag {
        CALL_RAW_START_TAG => {
            require_stream(tag, stream, false)?;
            Ok(ClientFrame::CallRawStart {
                stream,
                function: FunctionId::from_bytes(require_fixed_payload(tag, payload)?),
            })
        }
        CALL_ARGUMENT_TAG => {
            require_stream(tag, stream, false)?;
            if payload.len() < 16 {
                return Err(FrameCodecError::ArgumentPrefixTooShort {
                    actual: payload.len(),
                });
            }
            let parameter = ParameterId::from_bytes(
                payload[..16]
                    .try_into()
                    .expect("argument prefix length checked"),
            );
            let value = version
                .decode_value(&payload[16..])
                .map_err(|source| FrameCodecError::Value { source })?;
            version.require_call_argument(&value)?;
            Ok(ClientFrame::CallArgument {
                stream,
                parameter,
                value,
            })
        }
        CALL_ARGUMENTS_COMPLETE_TAG => {
            require_stream(tag, stream, false)?;
            require_empty_payload(tag, payload)?;
            Ok(ClientFrame::CallArgumentsComplete { stream })
        }
        WINDOW_UPDATE_TAG => {
            require_stream(tag, stream, false)?;
            let payload = require_fixed_payload::<9>(tag, payload)?;
            let channel = Channel::from_wire(payload[0])?;
            let credit =
                u64::from_be_bytes(payload[1..].try_into().expect("window length checked"));
            if credit == 0 {
                return Err(FrameCodecError::ZeroWindowCredit);
            }
            Ok(ClientFrame::WindowUpdate {
                stream,
                channel,
                credit,
            })
        }
        CALL_CANCEL_TAG => {
            require_stream(tag, stream, false)?;
            require_empty_payload(tag, payload)?;
            Ok(ClientFrame::CallCancel { stream })
        }
        PING_TAG => {
            require_stream(tag, stream, true)?;
            Ok(ClientFrame::Ping {
                token: require_fixed_payload(tag, payload)?,
            })
        }
        0x81..=PONG_TAG => Err(FrameCodecError::WrongDirection { tag }),
        tag => Err(FrameCodecError::UnknownTag { tag }),
    }
}

/// Encodes one complete server frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] when the frame cannot satisfy the version-1
/// envelope or payload contract.
pub fn encode_server_frame(frame: &ServerFrame) -> Result<Vec<u8>, FrameCodecError> {
    encode_server_frame_with_version(FrameVersion::One, frame)
}

/// Encodes one complete catalogue-bound version-2 server frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] when the frame cannot satisfy the version-2
/// envelope, payload, or active-catalogue value contract.
pub fn encode_catalogue_server_frame(
    catalogue: &CatalogueSnapshot,
    frame: &ServerFrame,
) -> Result<Vec<u8>, FrameCodecError> {
    encode_server_frame_with_version(FrameVersion::Catalogue(catalogue), frame)
}

/// Encodes one complete active-revision version-3 server frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] when the frame cannot satisfy the version-3
/// envelope, payload, or active-revision value contract.
pub fn encode_active_server_frame(
    active: &ActiveDatabaseRevision,
    frame: &ServerFrame,
) -> Result<Vec<u8>, FrameCodecError> {
    encode_server_frame_with_version(FrameVersion::Active(active), frame)
}

/// Encodes one complete registry-bound version-4 server frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] when the frame cannot satisfy the version-4
/// envelope, active-revision, or registry contract.
pub fn encode_registered_server_frame(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    frame: &ServerFrame,
) -> Result<Vec<u8>, FrameCodecError> {
    encode_server_frame_with_version(FrameVersion::Registered(active, registry), frame)
}

/// Encodes one complete registry-bound version-5 server frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] when the frame cannot satisfy the version-5
/// envelope, active-revision, registry, closed application-value contract, or
/// sealed invocation-carrier closure.
pub fn encode_constructed_server_frame(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    frame: &ServerFrame,
) -> Result<Vec<u8>, FrameCodecError> {
    encode_server_frame_with_version(FrameVersion::Constructed(active, registry), frame)
}

fn encode_server_frame_with_version(
    version: FrameVersion<'_>,
    frame: &ServerFrame,
) -> Result<Vec<u8>, FrameCodecError> {
    match frame {
        ServerFrame::CallAccepted { stream, invocation } => {
            require_stream(CALL_ACCEPTED_TAG, *stream, false)?;
            encode(version, CALL_ACCEPTED_TAG, *stream, &invocation.to_bytes())
        }
        ServerFrame::EventBatch {
            stream,
            channel,
            events,
        } => {
            require_stream(EVENT_BATCH_TAG, *stream, false)?;
            let payload = encode_event_batch(version, *channel, events)?;
            encode(version, EVENT_BATCH_TAG, *stream, &payload)
        }
        ServerFrame::CallCompleted { stream } => {
            require_stream(CALL_COMPLETED_TAG, *stream, false)?;
            encode(version, CALL_COMPLETED_TAG, *stream, &[])
        }
        ServerFrame::CallFailed { stream, failure } => {
            require_stream(CALL_FAILED_TAG, *stream, false)?;
            encode(version, CALL_FAILED_TAG, *stream, &failure.wire())
        }
        ServerFrame::CallCancelled { stream } => {
            require_stream(CALL_CANCELLED_TAG, *stream, false)?;
            encode(version, CALL_CANCELLED_TAG, *stream, &[])
        }
        ServerFrame::Pong { token } => encode(version, PONG_TAG, 0, token),
    }
}

/// Decodes one complete server frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] for an invalid envelope, wrong-direction tag,
/// unknown tag, or invalid tag-specific payload.
pub fn decode_server_frame(encoded: &[u8]) -> Result<ServerFrame, FrameCodecError> {
    decode_server_frame_with_version(FrameVersion::One, encoded)
}

/// Decodes one complete catalogue-bound version-2 server frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] for an invalid version-2 envelope,
/// wrong-direction tag, unknown tag, invalid payload, or enum value rejected
/// by the active catalogue.
pub fn decode_catalogue_server_frame(
    catalogue: &CatalogueSnapshot,
    encoded: &[u8],
) -> Result<ServerFrame, FrameCodecError> {
    decode_server_frame_with_version(FrameVersion::Catalogue(catalogue), encoded)
}

/// Decodes one complete active-revision version-3 server frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] for an invalid version-3 envelope,
/// wrong-direction tag, unknown tag, invalid payload, or value rejected by the
/// active revision.
pub fn decode_active_server_frame(
    active: &ActiveDatabaseRevision,
    encoded: &[u8],
) -> Result<ServerFrame, FrameCodecError> {
    decode_server_frame_with_version(FrameVersion::Active(active), encoded)
}

/// Decodes one complete registry-bound version-4 server frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] for an invalid version-4 envelope, payload,
/// active value, or registry-bound opaque value.
pub fn decode_registered_server_frame(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
) -> Result<ServerFrame, FrameCodecError> {
    decode_server_frame_with_version(FrameVersion::Registered(active, registry), encoded)
}

/// Decodes one complete registry-bound version-5 server frame.
///
/// # Errors
///
/// Returns a [`FrameCodecError`] for an invalid version-5 envelope, payload,
/// active value, registry-bound opaque value, constructed application value,
/// or sealed invocation carrier in the ordinary result position.
pub fn decode_constructed_server_frame(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
) -> Result<ServerFrame, FrameCodecError> {
    decode_server_frame_with_version(FrameVersion::Constructed(active, registry), encoded)
}

fn decode_server_frame_with_version(
    version: FrameVersion<'_>,
    encoded: &[u8],
) -> Result<ServerFrame, FrameCodecError> {
    let (tag, stream, payload) = decode_envelope(version, encoded)?;
    match tag {
        CALL_ACCEPTED_TAG => {
            require_stream(tag, stream, false)?;
            Ok(ServerFrame::CallAccepted {
                stream,
                invocation: InvocationId::from_bytes(require_fixed_payload(tag, payload)?),
            })
        }
        EVENT_BATCH_TAG => {
            require_stream(tag, stream, false)?;
            let (channel, events) = decode_event_batch(version, payload)?;
            Ok(ServerFrame::EventBatch {
                stream,
                channel,
                events,
            })
        }
        CALL_COMPLETED_TAG => {
            require_stream(tag, stream, false)?;
            require_empty_payload(tag, payload)?;
            Ok(ServerFrame::CallCompleted { stream })
        }
        CALL_FAILED_TAG => {
            require_stream(tag, stream, false)?;
            Ok(ServerFrame::CallFailed {
                stream,
                failure: CallFailure::from_wire(require_fixed_payload(tag, payload)?)?,
            })
        }
        CALL_CANCELLED_TAG => {
            require_stream(tag, stream, false)?;
            require_empty_payload(tag, payload)?;
            Ok(ServerFrame::CallCancelled { stream })
        }
        PONG_TAG => {
            require_stream(tag, stream, true)?;
            Ok(ServerFrame::Pong {
                token: require_fixed_payload(tag, payload)?,
            })
        }
        CALL_RAW_START_TAG..=PING_TAG => Err(FrameCodecError::WrongDirection { tag }),
        tag => Err(FrameCodecError::UnknownTag { tag }),
    }
}

fn encode(
    version: FrameVersion<'_>,
    tag: u8,
    stream: u64,
    payload: &[u8],
) -> Result<Vec<u8>, FrameCodecError> {
    require_payload_limit(payload.len())?;
    let mut encoded = Vec::with_capacity(HEADER_LENGTH + payload.len());
    encoded.extend_from_slice(version.marker());
    encoded.push(tag);
    encoded.push(0);
    encoded.extend_from_slice(&stream.to_be_bytes());
    encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    encoded.extend_from_slice(payload);
    Ok(encoded)
}

fn decode_envelope<'a>(
    version: FrameVersion<'_>,
    encoded: &'a [u8],
) -> Result<(u8, u64, &'a [u8]), FrameCodecError> {
    if encoded.len() < HEADER_LENGTH {
        return Err(FrameCodecError::TruncatedHeader {
            actual: encoded.len(),
        });
    }
    if &encoded[..version.marker().len()] != version.marker() {
        return Err(FrameCodecError::InvalidMarker);
    }
    let tag = encoded[4];
    let flags = encoded[5];
    if flags != 0 {
        return Err(FrameCodecError::NonZeroFlags { flags });
    }
    let stream = u64::from_be_bytes(encoded[6..14].try_into().expect("header length checked"));
    let declared =
        u32::from_be_bytes(encoded[14..18].try_into().expect("header length checked")) as usize;
    require_payload_limit(declared)?;
    let actual = encoded.len() - HEADER_LENGTH;
    if actual < declared {
        return Err(FrameCodecError::TruncatedPayload { declared, actual });
    }
    if actual > declared {
        return Err(FrameCodecError::TrailingBytes { declared, actual });
    }
    Ok((tag, stream, &encoded[HEADER_LENGTH..]))
}

fn require_payload_limit(actual: usize) -> Result<(), FrameCodecError> {
    if actual <= MAX_FRAME_PAYLOAD_LENGTH {
        Ok(())
    } else {
        Err(FrameCodecError::PayloadTooLarge {
            actual,
            maximum: MAX_FRAME_PAYLOAD_LENGTH,
        })
    }
}

fn require_stream(tag: u8, stream: u64, control: bool) -> Result<(), FrameCodecError> {
    if (control && stream == 0) || (!control && stream != 0) {
        Ok(())
    } else {
        Err(FrameCodecError::InvalidStream { tag, stream })
    }
}

fn require_fixed_payload<const LENGTH: usize>(
    tag: u8,
    payload: &[u8],
) -> Result<[u8; LENGTH], FrameCodecError> {
    payload
        .try_into()
        .map_err(|_| FrameCodecError::WrongPayloadLength {
            tag,
            expected: LENGTH,
            actual: payload.len(),
        })
}

fn require_empty_payload(tag: u8, payload: &[u8]) -> Result<(), FrameCodecError> {
    if payload.is_empty() {
        Ok(())
    } else {
        Err(FrameCodecError::WrongPayloadLength {
            tag,
            expected: 0,
            actual: payload.len(),
        })
    }
}

fn encode_event_batch(
    version: FrameVersion<'_>,
    channel: Channel,
    events: &[EventRecord],
) -> Result<Vec<u8>, FrameCodecError> {
    if events.is_empty() {
        return Err(FrameCodecError::EmptyEventBatch);
    }
    let count = u16::try_from(events.len()).map_err(|_| FrameCodecError::TooManyEvents {
        actual: events.len(),
    })?;
    let mut payload = Vec::new();
    payload.push(channel.wire());
    payload.extend_from_slice(&count.to_be_bytes());
    let mut previous: Option<u64> = None;
    for record in events {
        if record.sequence == 0
            || previous.is_some_and(|value| value.checked_add(1) != Some(record.sequence))
        {
            return Err(FrameCodecError::InvalidEventSequence);
        }
        previous = Some(record.sequence);
        if record.event.channel() != channel {
            return Err(FrameCodecError::InvalidEventChannel {
                channel,
                kind: record.event.kind(),
            });
        }
        let content = encode_event(version, &record.event)?;
        payload.extend_from_slice(&record.sequence.to_be_bytes());
        payload.push(record.event.kind());
        payload.extend_from_slice(&(content.len() as u32).to_be_bytes());
        payload.extend_from_slice(&content);
        require_payload_limit(payload.len())?;
    }
    Ok(payload)
}

fn encode_event(version: FrameVersion<'_>, event: &Event) -> Result<Vec<u8>, FrameCodecError> {
    match event {
        Event::Value(value) => {
            version.require_event_value(value)?;
            version
                .encode_value(value)
                .map_err(|source| FrameCodecError::Value { source })
        }
        Event::Bytes(bytes) if bytes.is_empty() => Err(FrameCodecError::EmptyByteChunk),
        Event::Bytes(bytes) => {
            require_payload_limit(bytes.len())?;
            Ok(bytes.clone())
        }
        Event::Failure(failure) => Ok(failure.wire().to_vec()),
    }
}

fn decode_event_batch(
    version: FrameVersion<'_>,
    payload: &[u8],
) -> Result<(Channel, Vec<EventRecord>), FrameCodecError> {
    if payload.len() < 3 {
        return Err(FrameCodecError::TruncatedEventBatch);
    }
    let channel = Channel::from_wire(payload[0])?;
    let count = u16::from_be_bytes(
        payload[1..3]
            .try_into()
            .expect("event batch prefix length checked"),
    );
    if count == 0 {
        return Err(FrameCodecError::EmptyEventBatch);
    }
    let mut remaining = &payload[3..];
    let mut events = Vec::with_capacity(count as usize);
    let mut previous: Option<u64> = None;
    for _ in 0..count {
        if remaining.len() < 13 {
            return Err(FrameCodecError::TruncatedEventBatch);
        }
        let sequence = u64::from_be_bytes(
            remaining[..8]
                .try_into()
                .expect("event entry prefix length checked"),
        );
        if sequence == 0 || previous.is_some_and(|value| value.checked_add(1) != Some(sequence)) {
            return Err(FrameCodecError::InvalidEventSequence);
        }
        previous = Some(sequence);
        let kind = remaining[8];
        let length = u32::from_be_bytes(
            remaining[9..13]
                .try_into()
                .expect("event entry prefix length checked"),
        ) as usize;
        remaining = &remaining[13..];
        if remaining.len() < length {
            return Err(FrameCodecError::TruncatedEventBatch);
        }
        let content = &remaining[..length];
        remaining = &remaining[length..];
        events.push(EventRecord {
            sequence,
            event: decode_event(version, channel, kind, content)?,
        });
    }
    if !remaining.is_empty() {
        return Err(FrameCodecError::TrailingEventBytes);
    }
    Ok((channel, events))
}

fn decode_event(
    version: FrameVersion<'_>,
    channel: Channel,
    kind: u8,
    content: &[u8],
) -> Result<Event, FrameCodecError> {
    match (channel, kind) {
        (Channel::ResultValues, 0x01) => {
            let value = version
                .decode_value(content)
                .map_err(|source| FrameCodecError::Value { source })?;
            version.require_event_value(&value)?;
            Ok(Event::Value(value))
        }
        (Channel::ResultBytes, 0x02) if content.is_empty() => Err(FrameCodecError::EmptyByteChunk),
        (Channel::ResultBytes, 0x02) => Ok(Event::Bytes(content.to_vec())),
        (Channel::Diagnostic, 0x03) => Ok(Event::Failure(CallFailure::from_wire(
            content
                .try_into()
                .map_err(|_| FrameCodecError::WrongEventContentLength {
                    kind,
                    expected: 4,
                    actual: content.len(),
                })?,
        )?)),
        (channel, kind) => Err(FrameCodecError::InvalidEventChannel { channel, kind }),
    }
}

#[cfg(test)]
mod tests {
    use orna_core::{
        CatalogueRevisionId, FunctionId, ParameterId, SchemaId, TypeId,
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, SchemaDefinition,
        },
        value::EnumValue,
    };
    use proptest::prelude::*;

    use super::*;

    const ENUM_TYPE: TypeId = TypeId::from_bytes([0x51; 16]);

    fn enum_catalogue(labels: &[&str]) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x52; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x53; 16]),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                ENUM_TYPE,
                QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
                labels.iter().copied(),
            )],
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn ping_and_pong_have_exact_golden_bytes_and_round_trip() {
        let token = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut ping = b"ORF1\x06\0".to_vec();
        ping.extend_from_slice(&0_u64.to_be_bytes());
        ping.extend_from_slice(&8_u32.to_be_bytes());
        ping.extend_from_slice(&token);
        assert_eq!(
            encode_client_frame(&ClientFrame::Ping { token }),
            Ok(ping.clone())
        );
        assert_eq!(decode_client_frame(&ping), Ok(ClientFrame::Ping { token }));

        let mut pong = ping.clone();
        pong[4] = 0x86;
        assert_eq!(
            encode_server_frame(&ServerFrame::Pong { token }),
            Ok(pong.clone())
        );
        assert_eq!(decode_server_frame(&pong), Ok(ServerFrame::Pong { token }));
        assert_eq!(
            decode_server_frame(&ping),
            Err(FrameCodecError::WrongDirection { tag: PING_TAG })
        );
        assert_eq!(
            decode_client_frame(&pong),
            Err(FrameCodecError::WrongDirection { tag: PONG_TAG })
        );
    }

    #[test]
    fn catalogue_frames_have_exact_markers_and_enum_value_bytes() {
        let catalogue = enum_catalogue(&["lead", "qualified"]);
        let parameter = ParameterId::from_bytes([0x54; 16]);
        let value = RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "qualified").unwrap());
        let frame = ClientFrame::CallArgument {
            stream: 1,
            parameter,
            value: value.clone(),
        };
        let encoded = encode_catalogue_client_frame(&catalogue, &frame).unwrap();

        assert_eq!(&encoded[..4], b"ORF2");
        assert_eq!(&encoded[34..38], b"ORV2");
        assert_eq!(
            decode_catalogue_client_frame(&catalogue, &encoded),
            Ok(frame)
        );
        assert_eq!(
            decode_client_frame(&encoded),
            Err(FrameCodecError::InvalidMarker)
        );

        let server = ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(value),
            }],
        };
        let encoded = encode_catalogue_server_frame(&catalogue, &server).unwrap();
        assert_eq!(&encoded[..4], b"ORF2");
        assert!(encoded.windows(4).any(|bytes| bytes == b"ORV2"));
        assert_eq!(
            decode_catalogue_server_frame(&catalogue, &encoded),
            Ok(server)
        );
        assert_eq!(
            decode_server_frame(&encoded),
            Err(FrameCodecError::InvalidMarker)
        );
    }

    #[test]
    fn catalogue_connection_carries_enum_arguments_and_results_fail_closed() {
        let original = enum_catalogue(&["lead", "qualified"]);
        let active = enum_catalogue(&["lead", "customer"]);
        let function = FunctionId::from_bytes([0x55; 16]);
        let parameter = ParameterId::from_bytes([0x56; 16]);
        let stale = RuntimeValue::Enum(EnumValue::new(&original, ENUM_TYPE, "qualified").unwrap());
        let mut connection = ProtocolConnection::new();
        connection
            .receive_catalogue(
                &active,
                ClientFrame::CallRawStart {
                    stream: 1,
                    function,
                },
            )
            .unwrap();
        let before = connection.clone();
        assert_eq!(
            connection.receive_catalogue(
                &active,
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter,
                    value: stale,
                }
            ),
            Err(ConnectionError::InvalidFrame {
                source: FrameCodecError::Value {
                    source: ValueCodecError::UndeclaredEnumLabel {
                        enum_type: ENUM_TYPE,
                        label: String::from("qualified"),
                    },
                },
            })
        );
        assert_eq!(connection, before);

        let value = RuntimeValue::Enum(EnumValue::new(&active, ENUM_TYPE, "customer").unwrap());
        connection
            .receive_catalogue(
                &active,
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter,
                    value: value.clone(),
                },
            )
            .unwrap();
        connection
            .receive_catalogue(
                &active,
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: 1024,
                },
            )
            .unwrap();
        assert_eq!(
            connection
                .receive_catalogue(&active, ClientFrame::CallArgumentsComplete { stream: 1 })
                .unwrap(),
            Some(ClientAction::Dispatch {
                stream: 1,
                call: RawCall {
                    function,
                    arguments: vec![CallArgument {
                        parameter,
                        value: value.clone(),
                    }],
                },
            })
        );
        let invocation = InvocationId::from_bytes([0x57; 16]);
        assert_eq!(
            connection
                .apply_catalogue(
                    &active,
                    ServerAction::Accepted {
                        stream: 1,
                        invocation,
                    },
                )
                .unwrap(),
            ServerFrame::CallAccepted {
                stream: 1,
                invocation,
            }
        );
        let event = Event::Value(value);
        assert_eq!(
            connection
                .apply_catalogue(
                    &active,
                    ServerAction::Events {
                        stream: 1,
                        events: vec![event.clone()],
                    },
                )
                .unwrap(),
            ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultValues,
                events: vec![EventRecord { sequence: 1, event }],
            }
        );
    }

    #[test]
    fn every_client_call_frame_has_exact_golden_bytes_and_round_trips() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let parameter = ParameterId::from_bytes([0x22; 16]);
        let value = RuntimeValue::Boolean(true);

        let cases = [
            (
                ClientFrame::CallRawStart {
                    stream: 1,
                    function,
                },
                [
                    b"ORF1\x01\0".as_slice(),
                    &1_u64.to_be_bytes(),
                    &16_u32.to_be_bytes(),
                    &[0x11; 16],
                ]
                .concat(),
            ),
            (
                ClientFrame::CallArgument {
                    stream: 1,
                    parameter,
                    value: value.clone(),
                },
                [
                    b"ORF1\x02\0".as_slice(),
                    &1_u64.to_be_bytes(),
                    &42_u32.to_be_bytes(),
                    &[0x22; 16],
                    b"ORV1\x02",
                    &orna_standard::BOOLEAN_TYPE_ID.to_bytes(),
                    &1_u32.to_be_bytes(),
                    &[1],
                ]
                .concat(),
            ),
            (
                ClientFrame::CallArgumentsComplete { stream: 1 },
                [
                    b"ORF1\x03\0".as_slice(),
                    &1_u64.to_be_bytes(),
                    &0_u32.to_be_bytes(),
                ]
                .concat(),
            ),
            (
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::Diagnostic,
                    credit: 9,
                },
                [
                    b"ORF1\x04\0".as_slice(),
                    &1_u64.to_be_bytes(),
                    &9_u32.to_be_bytes(),
                    &[0x03],
                    &9_u64.to_be_bytes(),
                ]
                .concat(),
            ),
            (
                ClientFrame::CallCancel { stream: 1 },
                [
                    b"ORF1\x05\0".as_slice(),
                    &1_u64.to_be_bytes(),
                    &0_u32.to_be_bytes(),
                ]
                .concat(),
            ),
        ];

        for (frame, expected) in cases {
            assert_eq!(encode_client_frame(&frame), Ok(expected.clone()));
            assert_eq!(decode_client_frame(&expected), Ok(frame));
        }
    }

    #[test]
    fn every_server_call_frame_has_exact_golden_bytes_and_round_trips() {
        let invocation = InvocationId::from_bytes([0x33; 16]);
        let accepted = ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        };
        let expected_accepted = [
            b"ORF1\x81\0".as_slice(),
            &1_u64.to_be_bytes(),
            &16_u32.to_be_bytes(),
            &[0x33; 16],
        ]
        .concat();

        let events = ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(RuntimeValue::Boolean(true)),
            }],
        };
        let expected_events = [
            b"ORF1\x82\0".as_slice(),
            &1_u64.to_be_bytes(),
            &42_u32.to_be_bytes(),
            &[0x01],
            &1_u16.to_be_bytes(),
            &1_u64.to_be_bytes(),
            &[0x01],
            &26_u32.to_be_bytes(),
            b"ORV1\x02",
            &orna_standard::BOOLEAN_TYPE_ID.to_bytes(),
            &1_u32.to_be_bytes(),
            &[1],
        ]
        .concat();

        let completed = ServerFrame::CallCompleted { stream: 1 };
        let expected_completed = [
            b"ORF1\x83\0".as_slice(),
            &1_u64.to_be_bytes(),
            &0_u32.to_be_bytes(),
        ]
        .concat();
        let failed = ServerFrame::CallFailed {
            stream: 1,
            failure: CallFailure::ExecuteDenied,
        };
        let expected_failed = [
            b"ORF1\x84\0".as_slice(),
            &1_u64.to_be_bytes(),
            &4_u32.to_be_bytes(),
            &[0x01, 0x00, 0x01, 0x00],
        ]
        .concat();
        let cancelled = ServerFrame::CallCancelled { stream: 1 };
        let expected_cancelled = [
            b"ORF1\x85\0".as_slice(),
            &1_u64.to_be_bytes(),
            &0_u32.to_be_bytes(),
        ]
        .concat();

        for (frame, expected) in [
            (accepted, expected_accepted),
            (events, expected_events),
            (completed, expected_completed),
            (failed, expected_failed),
            (cancelled, expected_cancelled),
        ] {
            assert_eq!(encode_server_frame(&frame), Ok(expected.clone()));
            assert_eq!(decode_server_frame(&expected), Ok(frame));
        }
    }

    #[test]
    fn failures_and_event_kinds_use_only_the_closed_version_one_bytes() {
        for (failure, expected) in [
            (CallFailure::ExecuteDenied, [0x01, 0x00, 0x01, 0x00]),
            (CallFailure::TargetUnavailable, [0x02, 0x00, 0x01, 0x00]),
            (
                CallFailure::ClientEvaluationFailed,
                [0x03, 0x00, 0x01, 0x00],
            ),
            (CallFailure::InternalFailure, [0xff, 0x00, 0x01, 0x00]),
        ] {
            let frame = ServerFrame::CallFailed { stream: 1, failure };
            let encoded = encode_server_frame(&frame).unwrap();
            assert_eq!(&encoded[18..], &expected);
            assert_eq!(decode_server_frame(&encoded), Ok(frame));
        }

        for (channel, event) in [
            (Channel::ResultBytes, Event::Bytes(vec![0xaa, 0xbb])),
            (
                Channel::Diagnostic,
                Event::Failure(CallFailure::InternalFailure),
            ),
        ] {
            let frame = ServerFrame::EventBatch {
                stream: 1,
                channel,
                events: vec![EventRecord { sequence: 9, event }],
            };
            let encoded = encode_server_frame(&frame).unwrap();
            assert_eq!(decode_server_frame(&encoded), Ok(frame));
        }

        assert_eq!(
            CallFailure::from_wire([0x01, 0x00, 0x01, 0x01]),
            Err(FrameCodecError::InvalidFailure {
                bytes: [0x01, 0x00, 0x01, 0x01],
            })
        );
    }

    #[test]
    fn codecs_reject_invalid_envelopes_payloads_and_event_shapes() {
        assert_eq!(
            decode_client_frame(b"ORF1"),
            Err(FrameCodecError::TruncatedHeader { actual: 4 })
        );
        let valid = encode_client_frame(&ClientFrame::Ping { token: [7; 8] }).unwrap();

        let mut invalid = valid.clone();
        invalid[0] = b'X';
        assert_eq!(
            decode_client_frame(&invalid),
            Err(FrameCodecError::InvalidMarker)
        );
        invalid = valid.clone();
        invalid[5] = 1;
        assert_eq!(
            decode_client_frame(&invalid),
            Err(FrameCodecError::NonZeroFlags { flags: 1 })
        );
        invalid = valid.clone();
        invalid[4] = CALL_RAW_START_TAG;
        assert_eq!(
            decode_client_frame(&invalid),
            Err(FrameCodecError::InvalidStream {
                tag: CALL_RAW_START_TAG,
                stream: 0,
            })
        );
        invalid = valid.clone();
        invalid[14..18].copy_from_slice(&((MAX_FRAME_PAYLOAD_LENGTH + 1) as u32).to_be_bytes());
        assert_eq!(
            decode_client_frame(&invalid),
            Err(FrameCodecError::PayloadTooLarge {
                actual: MAX_FRAME_PAYLOAD_LENGTH + 1,
                maximum: MAX_FRAME_PAYLOAD_LENGTH,
            })
        );
        invalid = valid[..valid.len() - 1].to_vec();
        assert_eq!(
            decode_client_frame(&invalid),
            Err(FrameCodecError::TruncatedPayload {
                declared: 8,
                actual: 7,
            })
        );
        invalid = valid.clone();
        invalid.push(0);
        assert_eq!(
            decode_client_frame(&invalid),
            Err(FrameCodecError::TrailingBytes {
                declared: 8,
                actual: 9,
            })
        );
        invalid = valid.clone();
        invalid[4] = 0x7f;
        assert_eq!(
            decode_client_frame(&invalid),
            Err(FrameCodecError::UnknownTag { tag: 0x7f })
        );

        let mut window = [
            b"ORF1\x04\0".as_slice(),
            &1_u64.to_be_bytes(),
            &9_u32.to_be_bytes(),
            &[0xff],
            &1_u64.to_be_bytes(),
        ]
        .concat();
        assert_eq!(
            decode_client_frame(&window),
            Err(FrameCodecError::UnknownChannel { value: 0xff })
        );
        window[19..27].copy_from_slice(&0_u64.to_be_bytes());
        window[18] = Channel::ResultValues.wire();
        assert_eq!(
            decode_client_frame(&window),
            Err(FrameCodecError::ZeroWindowCredit)
        );

        let mut argument = encode_client_frame(&ClientFrame::CallArgument {
            stream: 1,
            parameter: ParameterId::from_bytes([1; 16]),
            value: RuntimeValue::Boolean(true),
        })
        .unwrap();
        argument[34] = b'X';
        assert_eq!(
            decode_client_frame(&argument),
            Err(FrameCodecError::Value {
                source: ValueCodecError::InvalidMarker,
            })
        );

        assert_eq!(
            encode_server_frame(&ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultBytes,
                events: vec![],
            }),
            Err(FrameCodecError::EmptyEventBatch)
        );
        assert_eq!(
            encode_server_frame(&ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultBytes,
                events: vec![EventRecord {
                    sequence: 1,
                    event: Event::Bytes(vec![]),
                }],
            }),
            Err(FrameCodecError::EmptyByteChunk)
        );
        assert_eq!(
            encode_server_frame(&ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::Diagnostic,
                events: vec![EventRecord {
                    sequence: 1,
                    event: Event::Bytes(vec![1]),
                }],
            }),
            Err(FrameCodecError::InvalidEventChannel {
                channel: Channel::Diagnostic,
                kind: 0x02,
            })
        );
    }

    #[test]
    fn connection_dispatches_sorted_arguments_and_closes_the_stream() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let low = ParameterId::from_bytes([0x10; 16]);
        let high = ParameterId::from_bytes([0x20; 16]);
        let invocation = InvocationId::from_bytes([0x33; 16]);
        let mut connection = ProtocolConnection::new();

        assert_eq!(
            connection.receive(ClientFrame::CallRawStart {
                stream: 1,
                function,
            }),
            Ok(None)
        );
        assert_eq!(
            connection.receive(ClientFrame::CallArgument {
                stream: 1,
                parameter: high,
                value: RuntimeValue::Boolean(true),
            }),
            Ok(None)
        );
        assert_eq!(
            connection.receive(ClientFrame::CallArgument {
                stream: 1,
                parameter: low,
                value: RuntimeValue::Integer(7),
            }),
            Ok(None)
        );
        assert_eq!(
            connection.receive(ClientFrame::CallArgumentsComplete { stream: 1 }),
            Ok(Some(ClientAction::Dispatch {
                stream: 1,
                call: RawCall {
                    function,
                    arguments: vec![
                        CallArgument {
                            parameter: low,
                            value: RuntimeValue::Integer(7),
                        },
                        CallArgument {
                            parameter: high,
                            value: RuntimeValue::Boolean(true),
                        },
                    ],
                },
            }))
        );
        assert_eq!(
            connection.apply(ServerAction::Accepted {
                stream: 1,
                invocation,
            }),
            Ok(ServerFrame::CallAccepted {
                stream: 1,
                invocation,
            })
        );
        assert_eq!(
            connection.apply(ServerAction::Completed { stream: 1 }),
            Ok(ServerFrame::CallCompleted { stream: 1 })
        );
        assert_eq!(connection.live_streams(), 0);
        assert_eq!(connection.high_water_mark(), Some(1));
        assert_eq!(
            connection.receive(ClientFrame::CallRawStart {
                stream: 1,
                function,
            }),
            Err(ConnectionError::StreamNotIncreasing {
                stream: 1,
                previous: 1,
            })
        );
    }

    #[test]
    fn cancellation_distinguishes_receiving_dispatching_and_running_calls() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let invocation = InvocationId::from_bytes([0x33; 16]);
        let mut connection = ProtocolConnection::new();

        connection
            .receive(ClientFrame::CallRawStart {
                stream: 1,
                function,
            })
            .unwrap();
        assert_eq!(
            connection.receive(ClientFrame::CallCancel { stream: 1 }),
            Ok(Some(ClientAction::Send(ServerFrame::CallCancelled {
                stream: 1,
            })))
        );

        connection
            .receive(ClientFrame::CallRawStart {
                stream: 2,
                function,
            })
            .unwrap();
        connection
            .receive(ClientFrame::CallArgumentsComplete { stream: 2 })
            .unwrap();
        assert_eq!(
            connection.receive(ClientFrame::CallCancel { stream: 2 }),
            Ok(Some(ClientAction::Cancel {
                stream: 2,
                invocation: None,
            }))
        );
        assert_eq!(
            connection.apply(ServerAction::Accepted {
                stream: 2,
                invocation,
            }),
            Err(ConnectionError::WrongState { stream: 2 })
        );
        assert_eq!(
            connection.apply(ServerAction::Cancelled { stream: 2 }),
            Ok(ServerFrame::CallCancelled { stream: 2 })
        );

        connection
            .receive(ClientFrame::CallRawStart {
                stream: 3,
                function,
            })
            .unwrap();
        connection
            .receive(ClientFrame::CallArgumentsComplete { stream: 3 })
            .unwrap();
        connection
            .apply(ServerAction::Accepted {
                stream: 3,
                invocation,
            })
            .unwrap();
        assert_eq!(
            connection.receive(ClientFrame::CallCancel { stream: 3 }),
            Ok(Some(ClientAction::Cancel {
                stream: 3,
                invocation: Some(invocation),
            }))
        );
        assert_eq!(
            connection.receive(ClientFrame::CallCancel { stream: 3 }),
            Err(ConnectionError::WrongState { stream: 3 })
        );
        assert_eq!(
            connection.apply(ServerAction::Cancelled { stream: 3 }),
            Ok(ServerFrame::CallCancelled { stream: 3 })
        );
        assert_eq!(connection.live_streams(), 0);
    }

    #[test]
    fn event_windows_start_at_zero_and_consume_the_exact_payload() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let invocation = InvocationId::from_bytes([0x33; 16]);
        let mut connection = ProtocolConnection::new();
        connection
            .receive(ClientFrame::CallRawStart {
                stream: 1,
                function,
            })
            .unwrap();
        connection
            .receive(ClientFrame::CallArgumentsComplete { stream: 1 })
            .unwrap();
        connection
            .apply(ServerAction::Accepted {
                stream: 1,
                invocation,
            })
            .unwrap();

        let event = Event::Value(RuntimeValue::Boolean(true));
        assert_eq!(
            connection.apply(ServerAction::Events {
                stream: 1,
                events: vec![event.clone()],
            }),
            Err(ConnectionError::InsufficientCredit {
                stream: 1,
                channel: Channel::ResultValues,
                available: 0,
                required: 42,
            })
        );
        connection
            .receive(ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: 42,
            })
            .unwrap();
        assert_eq!(
            connection.apply(ServerAction::Events {
                stream: 1,
                events: vec![event.clone()],
            }),
            Ok(ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultValues,
                events: vec![EventRecord {
                    sequence: 1,
                    event: event.clone(),
                }],
            })
        );
        assert_eq!(
            connection.apply(ServerAction::Events {
                stream: 1,
                events: vec![event.clone()],
            }),
            Err(ConnectionError::InsufficientCredit {
                stream: 1,
                channel: Channel::ResultValues,
                available: 0,
                required: 42,
            })
        );
        connection
            .receive(ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultValues,
                credit: 42,
            })
            .unwrap();
        assert_eq!(
            connection.apply(ServerAction::Events {
                stream: 1,
                events: vec![event.clone()],
            }),
            Ok(ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultValues,
                events: vec![EventRecord { sequence: 2, event }],
            })
        );
    }

    #[test]
    fn raw_call_client_starts_exactly_and_preserves_ordered_values() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let invocation = InvocationId::from_bytes([0x22; 16]);
        let (mut client, frames) = RawCallClient::start(function);
        assert_eq!(
            frames,
            [
                ClientFrame::CallRawStart {
                    stream: 1,
                    function,
                },
                ClientFrame::WindowUpdate {
                    stream: 1,
                    channel: Channel::ResultValues,
                    credit: MAX_CHANNEL_WINDOW,
                },
                ClientFrame::CallArgumentsComplete { stream: 1 },
            ]
        );
        assert_eq!(
            client
                .receive_encoded(
                    &encode_server_frame(&ServerFrame::CallAccepted {
                        stream: 1,
                        invocation,
                    })
                    .unwrap(),
                )
                .unwrap(),
            RawCallClientResponse::Accepted { invocation }
        );
        assert_eq!(
            client
                .receive_encoded(
                    &encode_server_frame(&ServerFrame::EventBatch {
                        stream: 1,
                        channel: Channel::ResultValues,
                        events: vec![
                            EventRecord {
                                sequence: 1,
                                event: Event::Value(RuntimeValue::Boolean(true)),
                            },
                            EventRecord {
                                sequence: 2,
                                event: Event::Value(RuntimeValue::Integer(7)),
                            },
                        ],
                    })
                    .unwrap(),
                )
                .unwrap(),
            RawCallClientResponse::Values(vec![
                RuntimeValue::Boolean(true),
                RuntimeValue::Integer(7),
            ])
        );
        assert_eq!(
            client
                .receive_encoded(
                    &encode_server_frame(&ServerFrame::CallCompleted { stream: 1 }).unwrap(),
                )
                .unwrap(),
            RawCallClientResponse::Completed
        );
        assert_eq!(
            client.receive_encoded(
                &encode_server_frame(&ServerFrame::CallCompleted { stream: 1 }).unwrap()
            ),
            Err(RawCallClientError::WrongState)
        );
    }

    #[test]
    fn raw_call_client_closes_failures_and_cancellation() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let invocation = InvocationId::from_bytes([0x22; 16]);
        let accepted = encode_server_frame(&ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        })
        .unwrap();

        let (mut failed, _) = RawCallClient::start(function);
        failed.receive_encoded(&accepted).unwrap();
        assert_eq!(
            failed
                .receive_encoded(
                    &encode_server_frame(&ServerFrame::CallFailed {
                        stream: 1,
                        failure: CallFailure::ExecuteDenied,
                    })
                    .unwrap(),
                )
                .unwrap(),
            RawCallClientResponse::Failed(CallFailure::ExecuteDenied)
        );

        let (mut cancelled, _) = RawCallClient::start(function);
        assert_eq!(
            cancelled.request_cancellation().unwrap(),
            ClientFrame::CallCancel { stream: 1 }
        );

        let (mut cancelled_before_acceptance, _) = RawCallClient::start(function);
        cancelled_before_acceptance.request_cancellation().unwrap();
        assert_eq!(
            cancelled_before_acceptance
                .receive_encoded(
                    &encode_server_frame(&ServerFrame::CallCancelled { stream: 1 }).unwrap(),
                )
                .unwrap(),
            RawCallClientResponse::Cancelled
        );
        assert_eq!(
            cancelled.request_cancellation(),
            Err(RawCallClientError::WrongState)
        );
        cancelled.receive_encoded(&accepted).unwrap();
        assert_eq!(
            cancelled
                .receive_encoded(
                    &encode_server_frame(&ServerFrame::CallCancelled { stream: 1 }).unwrap(),
                )
                .unwrap(),
            RawCallClientResponse::Cancelled
        );
    }

    #[test]
    fn raw_call_client_rejects_every_response_boundary() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let invocation = InvocationId::from_bytes([0x22; 16]);
        let accepted = |stream| {
            encode_server_frame(&ServerFrame::CallAccepted { stream, invocation }).unwrap()
        };
        let event = |stream, channel, sequence, event| {
            encode_server_frame(&ServerFrame::EventBatch {
                stream,
                channel,
                events: vec![EventRecord { sequence, event }],
            })
            .unwrap()
        };

        let (mut client, _) = RawCallClient::start(function);
        assert_eq!(
            client.receive_encoded(&accepted(2)),
            Err(RawCallClientError::WrongStream {
                expected: 1,
                actual: 2,
            })
        );
        assert_eq!(
            client.receive_encoded(&event(
                1,
                Channel::ResultValues,
                1,
                Event::Value(RuntimeValue::Boolean(true)),
            )),
            Err(RawCallClientError::WrongState)
        );
        client.receive_encoded(&accepted(1)).unwrap();
        assert_eq!(
            client.receive_encoded(&event(1, Channel::ResultBytes, 1, Event::Bytes(vec![1]),)),
            Err(RawCallClientError::WrongChannel {
                actual: Channel::ResultBytes,
            })
        );
        assert_eq!(
            client.receive_encoded(&event(
                1,
                Channel::ResultValues,
                2,
                Event::Value(RuntimeValue::Boolean(true)),
            )),
            Err(RawCallClientError::WrongSequence {
                expected: 1,
                actual: 2,
            })
        );
        assert_eq!(
            client.receive_encoded(&event(
                1,
                Channel::Diagnostic,
                1,
                Event::Failure(CallFailure::InternalFailure),
            )),
            Err(RawCallClientError::WrongChannel {
                actual: Channel::Diagnostic,
            })
        );
        assert_eq!(
            client.receive_encoded(
                &encode_server_frame(&ServerFrame::CallCancelled { stream: 1 }).unwrap()
            ),
            Err(RawCallClientError::WrongState)
        );

        let mut wrong_marker = accepted(1);
        wrong_marker[..4].copy_from_slice(b"ORF2");
        let (mut marker_client, _) = RawCallClient::start(function);
        assert!(matches!(
            marker_client.receive_encoded(&wrong_marker),
            Err(RawCallClientError::Frame {
                source: FrameCodecError::InvalidMarker,
            })
        ));
    }

    #[test]
    fn raw_call_client_accepts_the_terminal_sequence_once() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let invocation = InvocationId::from_bytes([0x22; 16]);
        let (mut client, _) = RawCallClient::start(function);
        client
            .receive_encoded(
                &encode_server_frame(&ServerFrame::CallAccepted {
                    stream: 1,
                    invocation,
                })
                .unwrap(),
            )
            .unwrap();
        client.next_sequence = Some(u64::MAX);
        let terminal = encode_server_frame(&ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: u64::MAX,
                event: Event::Value(RuntimeValue::Boolean(true)),
            }],
        })
        .unwrap();
        assert_eq!(
            client.receive_encoded(&terminal).unwrap(),
            RawCallClientResponse::Values(vec![RuntimeValue::Boolean(true)])
        );
        assert_eq!(
            client.receive_encoded(&terminal),
            Err(RawCallClientError::SequenceExhausted)
        );
    }

    #[test]
    fn raw_call_client_charges_the_exact_event_payload_credit() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let invocation = InvocationId::from_bytes([0x22; 16]);
        let accepted = encode_server_frame(&ServerFrame::CallAccepted {
            stream: 1,
            invocation,
        })
        .unwrap();
        let event = encode_server_frame(&ServerFrame::EventBatch {
            stream: 1,
            channel: Channel::ResultValues,
            events: vec![EventRecord {
                sequence: 1,
                event: Event::Value(RuntimeValue::Boolean(true)),
            }],
        })
        .unwrap();
        let required = u64::try_from(event.len() - HEADER_LENGTH).unwrap();

        let (mut exact, _) = RawCallClient::start(function);
        exact.receive_encoded(&accepted).unwrap();
        exact.remaining_result_credit = required;
        assert_eq!(
            exact.receive_encoded(&event).unwrap(),
            RawCallClientResponse::Values(vec![RuntimeValue::Boolean(true)])
        );
        assert_eq!(exact.remaining_result_credit, 0);

        let (mut short, _) = RawCallClient::start(function);
        short.receive_encoded(&accepted).unwrap();
        short.remaining_result_credit = required - 1;
        let before = short.clone();
        assert_eq!(
            short.receive_encoded(&event),
            Err(RawCallClientError::InsufficientCredit {
                available: required - 1,
                required,
            })
        );
        assert_eq!(short, before);
    }

    #[test]
    fn sixty_four_interleaved_streams_keep_all_call_state_independent() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let mut connection = ProtocolConnection::new();
        for stream in 1_u64..=64 {
            let parameter = ParameterId::from_bytes([stream as u8; 16]);
            connection
                .receive(ClientFrame::CallRawStart { stream, function })
                .unwrap();
            connection
                .receive(ClientFrame::CallArgument {
                    stream,
                    parameter,
                    value: RuntimeValue::Integer(stream as i32),
                })
                .unwrap();
            connection
                .receive(ClientFrame::WindowUpdate {
                    stream,
                    channel: Channel::ResultBytes,
                    credit: 18 + stream,
                })
                .unwrap();
            assert_eq!(
                connection
                    .receive(ClientFrame::CallArgumentsComplete { stream })
                    .unwrap(),
                Some(ClientAction::Dispatch {
                    stream,
                    call: RawCall {
                        function,
                        arguments: vec![CallArgument {
                            parameter,
                            value: RuntimeValue::Integer(stream as i32),
                        }],
                    },
                })
            );
        }
        for stream in (1_u64..=64).rev() {
            let invocation = InvocationId::from_bytes([stream as u8; 16]);
            connection
                .apply(ServerAction::Accepted { stream, invocation })
                .unwrap();
            assert_eq!(
                connection.apply(ServerAction::Events {
                    stream,
                    events: vec![Event::Bytes(vec![0xaa, 0xbb])],
                }),
                Ok(ServerFrame::EventBatch {
                    stream,
                    channel: Channel::ResultBytes,
                    events: vec![EventRecord {
                        sequence: 1,
                        event: Event::Bytes(vec![0xaa, 0xbb]),
                    }],
                })
            );
        }
        for stream in 1_u64..=64 {
            let state = connection.streams.get(&stream).expect("stream is live");
            assert!(matches!(state.phase, Phase::Running { .. }));
            assert_eq!(state.last_sequence, 1);
            assert_eq!(state.windows[channel_index(Channel::ResultBytes)], stream);
        }
    }

    #[test]
    fn event_sequence_exhaustion_fails_without_consuming_credit() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let invocation = InvocationId::from_bytes([0x33; 16]);
        let mut connection = ProtocolConnection::new();
        connection
            .receive(ClientFrame::CallRawStart {
                stream: 1,
                function,
            })
            .unwrap();
        connection
            .receive(ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::ResultBytes,
                credit: 36,
            })
            .unwrap();
        connection
            .receive(ClientFrame::CallArgumentsComplete { stream: 1 })
            .unwrap();
        connection
            .apply(ServerAction::Accepted {
                stream: 1,
                invocation,
            })
            .unwrap();
        connection
            .streams
            .get_mut(&1)
            .expect("stream is live")
            .last_sequence = u64::MAX - 1;

        assert_eq!(
            connection.apply(ServerAction::Events {
                stream: 1,
                events: vec![Event::Bytes(vec![0xaa, 0xbb])],
            }),
            Ok(ServerFrame::EventBatch {
                stream: 1,
                channel: Channel::ResultBytes,
                events: vec![EventRecord {
                    sequence: u64::MAX,
                    event: Event::Bytes(vec![0xaa, 0xbb]),
                }],
            })
        );
        assert_eq!(
            connection.apply(ServerAction::Events {
                stream: 1,
                events: vec![Event::Bytes(vec![0xaa, 0xbb])],
            }),
            Err(ConnectionError::EventSequenceExhausted { stream: 1 })
        );
        assert_eq!(
            connection
                .streams
                .get(&1)
                .expect("failed event retains stream")
                .windows[channel_index(Channel::ResultBytes)],
            18
        );
    }

    #[test]
    fn window_and_transition_errors_leave_the_complete_state_unchanged() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let parameter = ParameterId::from_bytes([0x22; 16]);
        let mut connection = ProtocolConnection::new();
        connection
            .receive(ClientFrame::CallRawStart {
                stream: 1,
                function,
            })
            .unwrap();
        connection
            .receive(ClientFrame::CallArgument {
                stream: 1,
                parameter,
                value: RuntimeValue::Boolean(true),
            })
            .unwrap();
        let before_duplicate = connection.clone();
        assert_eq!(
            connection.receive(ClientFrame::CallArgument {
                stream: 1,
                parameter,
                value: RuntimeValue::Boolean(false),
            }),
            Err(ConnectionError::DuplicateArgument {
                stream: 1,
                parameter,
            })
        );
        assert_eq!(connection, before_duplicate);

        connection
            .receive(ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::Diagnostic,
                credit: MAX_CHANNEL_WINDOW,
            })
            .unwrap();
        let before_overflow = connection.clone();
        assert_eq!(
            connection.receive(ClientFrame::WindowUpdate {
                stream: 1,
                channel: Channel::Diagnostic,
                credit: 1,
            }),
            Err(ConnectionError::WindowOverflow {
                stream: 1,
                channel: Channel::Diagnostic,
            })
        );
        assert_eq!(connection, before_overflow);

        connection
            .receive(ClientFrame::CallArgumentsComplete { stream: 1 })
            .unwrap();
        let before_wrong_state = connection.clone();
        assert_eq!(
            connection.receive(ClientFrame::CallArgument {
                stream: 1,
                parameter: ParameterId::from_bytes([0x23; 16]),
                value: RuntimeValue::Boolean(true),
            }),
            Err(ConnectionError::WrongState { stream: 1 })
        );
        assert_eq!(connection, before_wrong_state);
        assert_eq!(
            connection.apply(ServerAction::Completed { stream: 1 }),
            Err(ConnectionError::WrongState { stream: 1 })
        );
        assert_eq!(connection, before_wrong_state);
    }

    #[test]
    fn connection_and_argument_limits_fail_without_changing_prior_state() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let mut streams = ProtocolConnection::new();
        for stream in 1..=64 {
            assert_eq!(
                streams.receive(ClientFrame::CallRawStart { stream, function }),
                Ok(None)
            );
        }
        assert_eq!(
            streams.receive(ClientFrame::CallRawStart {
                stream: 65,
                function,
            }),
            Err(ConnectionError::TooManyLiveStreams)
        );
        assert_eq!(streams.high_water_mark(), Some(64));
        streams
            .receive(ClientFrame::CallCancel { stream: 1 })
            .unwrap();
        assert_eq!(
            streams.receive(ClientFrame::CallRawStart {
                stream: 65,
                function,
            }),
            Ok(None)
        );

        let mut exhausted = ProtocolConnection::new();
        exhausted
            .receive(ClientFrame::CallRawStart {
                stream: u64::MAX,
                function,
            })
            .unwrap();
        exhausted
            .receive(ClientFrame::CallCancel { stream: u64::MAX })
            .unwrap();
        assert_eq!(
            exhausted.receive(ClientFrame::CallRawStart {
                stream: u64::MAX,
                function,
            }),
            Err(ConnectionError::StreamNumberExhausted)
        );

        let null = RuntimeValue::null(orna_core::types::ResolvedType::scalar(
            orna_core::types::StandardScalar::Boolean,
        ))
        .unwrap();
        let mut count = ProtocolConnection::new();
        count
            .receive(ClientFrame::CallRawStart {
                stream: 1,
                function,
            })
            .unwrap();
        for index in 0_u16..256 {
            let mut bytes = [0; 16];
            bytes[14..].copy_from_slice(&index.to_be_bytes());
            assert_eq!(
                count.receive(ClientFrame::CallArgument {
                    stream: 1,
                    parameter: ParameterId::from_bytes(bytes),
                    value: null.clone(),
                }),
                Ok(None)
            );
        }
        assert_eq!(
            count.receive(ClientFrame::CallArgument {
                stream: 1,
                parameter: ParameterId::from_bytes([0xff; 16]),
                value: null.clone(),
            }),
            Err(ConnectionError::TooManyArguments { stream: 1 })
        );

        let mut bytes = ProtocolConnection::new();
        bytes
            .receive(ClientFrame::CallRawStart {
                stream: 1,
                function,
            })
            .unwrap();
        bytes
            .receive(ClientFrame::CallArgument {
                stream: 1,
                parameter: ParameterId::from_bytes([1; 16]),
                value: RuntimeValue::Bytes(vec![0; MAX_FRAME_PAYLOAD_LENGTH - 82]),
            })
            .unwrap();
        bytes
            .receive(ClientFrame::CallArgument {
                stream: 1,
                parameter: ParameterId::from_bytes([2; 16]),
                value: null.clone(),
            })
            .unwrap();
        assert_eq!(
            bytes.receive(ClientFrame::CallArgument {
                stream: 1,
                parameter: ParameterId::from_bytes([3; 16]),
                value: null,
            }),
            Err(ConnectionError::ArgumentsTooLarge { stream: 1 })
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn arbitrary_frame_bytes_never_panic(encoded in prop::collection::vec(any::<u8>(), 0..8192)) {
            let _ = decode_client_frame(&encoded);
            let _ = decode_server_frame(&encoded);
        }

        #[test]
        fn marker_valid_arbitrary_frames_never_panic(
            tag in any::<u8>(),
            flags in any::<u8>(),
            stream in any::<u64>(),
            declared in 0_u32..8192,
            payload in prop::collection::vec(any::<u8>(), 0..8192),
        ) {
            let mut encoded = b"ORF1".to_vec();
            encoded.push(tag);
            encoded.push(flags);
            encoded.extend_from_slice(&stream.to_be_bytes());
            encoded.extend_from_slice(&declared.to_be_bytes());
            encoded.extend_from_slice(&payload);
            let _ = decode_client_frame(&encoded);
            let _ = decode_server_frame(&encoded);
        }

        #[test]
        fn arbitrary_typed_actions_preserve_all_connection_bounds(
            operations in prop::collection::vec((any::<u8>(), any::<u16>(), any::<u8>()), 0..512),
        ) {
            let mut connection = ProtocolConnection::new();
            let mut observed_high_water = None;
            for (operation, number, value) in operations {
                let stream = u64::from(number) + 1;
                let function = FunctionId::from_bytes([value; 16]);
                let invocation = InvocationId::from_bytes([value; 16]);
                let parameter = ParameterId::from_bytes([value; 16]);
                let before = connection.clone();
                let failed = match operation % 11 {
                    0 => connection
                        .receive(ClientFrame::CallRawStart { stream, function })
                        .is_err(),
                    1 => connection
                        .receive(ClientFrame::CallArgument {
                            stream,
                            parameter,
                            value: RuntimeValue::Boolean(value & 1 == 1),
                        })
                        .is_err(),
                    2 => connection
                        .receive(ClientFrame::CallArgumentsComplete { stream })
                        .is_err(),
                    3 => connection
                        .receive(ClientFrame::WindowUpdate {
                            stream,
                            channel: Channel::ResultValues,
                            credit: u64::from(value) + 1,
                        })
                        .is_err(),
                    4 => connection
                        .receive(ClientFrame::CallCancel { stream })
                        .is_err(),
                    5 => connection
                        .apply(ServerAction::Accepted { stream, invocation })
                        .is_err(),
                    6 => connection
                        .apply(ServerAction::Events {
                            stream,
                            events: vec![Event::Value(RuntimeValue::Boolean(value & 1 == 1))],
                        })
                        .is_err(),
                    7 => connection
                        .apply(ServerAction::Completed { stream })
                        .is_err(),
                    8 => connection
                        .apply(ServerAction::Failed {
                            stream,
                            failure: CallFailure::InternalFailure,
                        })
                        .is_err(),
                    9 => connection
                        .apply(ServerAction::Cancelled { stream })
                        .is_err(),
                    _ => connection
                        .receive(ClientFrame::Ping { token: [value; 8] })
                        .is_err(),
                };
                if failed {
                    prop_assert_eq!(&connection, &before);
                }

                prop_assert!(connection.live_streams() <= MAX_LIVE_STREAMS);
                prop_assert!(connection.high_water_mark() >= observed_high_water);
                observed_high_water = connection.high_water_mark();
                for state in connection.streams.values() {
                    prop_assert!(state
                        .windows
                        .iter()
                        .all(|window| *window <= MAX_CHANNEL_WINDOW));
                    if let Phase::Receiving {
                        arguments,
                        argument_bytes,
                        ..
                    } = &state.phase
                    {
                        prop_assert!(arguments.len() <= MAX_ARGUMENTS);
                        prop_assert!(*argument_bytes <= MAX_ARGUMENT_BYTES);
                    }
                }
            }
        }
    }
}
