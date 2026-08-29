//! Canonical runtime values and the bounded authenticated raw-call protocol.

mod frame;
mod session;

pub use frame::{
    CallArgument, CallFailure, Channel, ClientAction, ClientFrame, ConnectionError, Event,
    EventRecord, FrameCodecError, InvocationClient, InvocationClientError,
    InvocationClientResponse, InvocationEventBatch, InvocationEventRecord, MAX_CHANNEL_WINDOW,
    MAX_FRAME_PAYLOAD_LENGTH, MAX_RESOURCE_ARGUMENTS, MAX_RESOURCE_BATCH_ITEMS,
    MAX_RESOURCE_TOTAL_ITEMS, MAX_RESOURCE_WINDOW, ProtocolConnection, RawCall, RawCallClient,
    RawCallClientError, RawCallClientResponse, ResourceAccepted, ResourceAcceptedFrame,
    ResourceArgument, ResourceCancel, ResourceCancelFrame, ResourceCancelReason,
    ResourceCancellationCode, ResourceCancelled, ResourceCancelledFrame, ResourceClientFrame,
    ResourceCompleted, ResourceCompletedFrame, ResourceConnectionError, ResourceCredit,
    ResourceFailed, ResourceFailedFrame, ResourceFrameDisposition, ResourceKind,
    ResourceProtocolConnection, ResourceRequest, ResourceRequestFrame, ResourceServerFrame,
    ResourceValues, ResourceValuesFrame, ResourceWindowUpdate, ResourceWindowUpdateFrame,
    RetainedInvokeRequest, ServerAction, ServerFrame, decode_active_client_frame,
    decode_active_server_frame, decode_catalogue_client_frame, decode_catalogue_server_frame,
    decode_client_frame, decode_constructed_client_frame,
    decode_constructed_invocation_event_frame, decode_constructed_server_frame,
    decode_invocation_event_batch, decode_invoke_request, decode_registered_client_frame,
    decode_registered_server_frame, decode_resource_accepted, decode_resource_cancel,
    decode_resource_cancelled, decode_resource_client_frame, decode_resource_completed,
    decode_resource_failed, decode_resource_request, decode_resource_server_frame,
    decode_resource_values, decode_resource_window_update, decode_retained_invoke_request,
    decode_server_frame, encode_active_client_frame, encode_active_server_frame,
    encode_catalogue_client_frame, encode_catalogue_server_frame, encode_client_frame,
    encode_constructed_client_frame, encode_constructed_server_frame,
    encode_invocation_event_batch, encode_invoke_request, encode_registered_client_frame,
    encode_registered_server_frame, encode_resource_accepted, encode_resource_cancel,
    encode_resource_cancelled, encode_resource_client_frame, encode_resource_completed,
    encode_resource_failed, encode_resource_request, encode_resource_server_frame,
    encode_resource_values, encode_resource_window_update, encode_server_frame,
};

pub use session::{
    InputRequested, MAX_SESSION_ERROR_LENGTH, MAX_SESSION_FRAME_LENGTH, MAX_SESSION_LINE_LENGTH,
    SESSION_HEADER_LENGTH, SESSION_MARKER, SessionClientFrame, SessionCodecError,
    SessionInputState, SessionServerFrame, SessionStateError, decode_session_client_frame,
    decode_session_server_frame, encode_session_client_frame, encode_session_server_frame,
};

use std::{cmp::Ordering, collections::BTreeMap, error::Error, fmt};

use orna_core::{
    FieldId, FunctionId, InvocationId, ObjectId, ParameterId, PrincipalId, TypeId,
    catalogue::{CatalogueSnapshot, QualifiedSemanticName, ValueTypeKind},
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind,
        InvocationCarrierConstructionError, InvocationClientOffer, InvocationDiagnostic,
        InvocationDiagnosticSeverity, InvocationEventBody, InvocationFailure,
        InvocationFailurePhase, InvocationOutputRequirement, InvocationOutputTypeSelector,
        InvocationParameterSelector, InvocationRetryability, InvocationRuntimeContract,
        InvocationRuntimeOffer, InvocationSinkOffer, InvocationStreamingRequirement,
        InvocationTarget, InvocationTracePolicy, InvokeEvent, InvokeRequest, InvokeRequestInput,
        InvokeValue, MAX_INVOCATION_CARRIER_NODES, invocation_carrier_type_id,
    },
    revision::ActiveDatabaseRevision,
    system::{
        SYS_INVOKE_EVENT_TYPE_ID, SYS_INVOKE_REQUEST_TYPE_ID, SYS_INVOKE_VALUE_TYPE_ID,
        invocation_carrier_by_id,
    },
    types::{
        MAX_TYPE_DESCRIPTOR_DEPTH, ResolvedType, StandardScalar, TypeDescriptor,
        TypeDescriptorError, TypeDescriptorKind,
    },
    value::{
        CollectionValueError, CollectionValuePathSegment, ConstructedValueKind, EnumValue,
        EnumValueError, MAX_ROWS_CELLS, MAX_ROWS_COLUMNS, MAX_ROWS_PAYLOAD_LENGTH, MAX_ROWS_ROWS,
        MAX_RUNTIME_VALUE_NODES, OpaqueCodecRegistry, OpaqueValue, OpaqueValueError, RecordValue,
        ResultColumn, ResultRow, ResultRows, ResultRowsError, RuntimeFloat, RuntimeType,
        RuntimeValue,
    },
};
use orna_standard::{
    BIGINT_TYPE_ID, BINARY_LARGE_OBJECT_TYPE_ID, BOOLEAN_TYPE_ID, CHARACTER_LARGE_OBJECT_TYPE_ID,
    FLOAT_TYPE_ID, INTEGER_TYPE_ID, STANDARD_TYPE_IDS,
};

const MARKER: &[u8; 4] = b"ORV1";
const CATALOGUE_MARKER: &[u8; 4] = b"ORV2";
const ACTIVE_MARKER: &[u8; 4] = b"ORV3";
const REGISTERED_MARKER: &[u8; 4] = b"ORV4";
const CONSTRUCTED_MARKER: &[u8; 4] = b"ORV5";
const SET_MARKER: &[u8; 4] = b"ORV6";
const HEADER_LENGTH: usize = 25;
const RECORD_FIELD_HEADER_LENGTH: usize = 20;
const NULL_SCALAR_TAG: u8 = 0x00;
const NULL_REFERENCE_TAG: u8 = 0x01;
const BOOLEAN_TAG: u8 = 0x02;
const INTEGER_TAG: u8 = 0x03;
const BIGINT_TAG: u8 = 0x04;
const FLOAT_TAG: u8 = 0x05;
const TEXT_TAG: u8 = 0x06;
const BYTES_TAG: u8 = 0x07;
const REFERENCE_TAG: u8 = 0x08;
const NULL_ENUM_TAG: u8 = 0x09;
const ENUM_TAG: u8 = 0x0a;
const RECORD_TAG: u8 = 0x0b;
const OPAQUE_TAG: u8 = 0x0c;
/// The fixed Work ADR 0087 `std.data.Rows` opaque type identity (`...12`).
const STD_DATA_ROWS_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);
const CONSTRUCTED_TAG: u8 = 0x0d;
const PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const ROWS_COLUMN_MIN_BYTES: usize = 23;
const SUPPORTED_SCALAR_TYPES: [(TypeId, StandardScalar, u8); 6] = [
    (BOOLEAN_TYPE_ID, StandardScalar::Boolean, BOOLEAN_TAG),
    (INTEGER_TYPE_ID, StandardScalar::Integer, INTEGER_TAG),
    (BIGINT_TYPE_ID, StandardScalar::BigInt, BIGINT_TAG),
    (FLOAT_TYPE_ID, StandardScalar::Float, FLOAT_TAG),
    (
        CHARACTER_LARGE_OBJECT_TYPE_ID,
        StandardScalar::CharacterLargeObject,
        TEXT_TAG,
    ),
    (
        BINARY_LARGE_OBJECT_TYPE_ID,
        StandardScalar::BinaryLargeObject,
        BYTES_TAG,
    ),
];

/// An error from canonical runtime value encoding or decoding.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValueCodecError {
    /// The runtime value category is not defined by the selected codec version.
    UnsupportedValue,
    /// A constructed value does not use the required all-zero identity sentinel.
    ConstructedTypeIdentityNotZero {
        /// The non-zero identity from the constructed value header.
        identity: TypeId,
    },
    /// A constructed payload does not contain its two-byte descriptor length.
    TruncatedConstructedHeader {
        /// The available constructed payload bytes.
        actual: usize,
    },
    /// A constructed payload has an empty descriptor region.
    EmptyConstructedDescriptor,
    /// The declared descriptor region exceeds the constructed payload.
    TruncatedConstructedDescriptor {
        /// The declared descriptor length.
        declared: usize,
        /// The available descriptor bytes.
        available: usize,
    },
    /// A descriptor node ends before its required bytes occur.
    TruncatedConstructedDescriptorNode {
        /// The zero-based descriptor offset of the incomplete node.
        offset: usize,
        /// The minimum bytes required at this node.
        required: usize,
        /// The available bytes from this node.
        available: usize,
    },
    /// Bytes remain after one complete descriptor tree.
    TrailingConstructedDescriptor {
        /// The unconsumed descriptor bytes.
        remaining: usize,
    },
    /// A descriptor byte is not defined by ORV5.
    UnknownConstructedDescriptorTag {
        /// The unrecognised descriptor byte.
        tag: u8,
    },
    /// A descriptor exceeds the core structural bounds.
    InvalidConstructedDescriptor {
        /// The core descriptor failure.
        source: TypeDescriptorError,
    },
    /// A descriptor is structurally valid but is not admitted for collections.
    UnsupportedConstructedDescriptor {
        /// The rejected complete descriptor.
        descriptor: TypeDescriptor,
    },
    /// A constructed OPTION presence byte is not zero or one.
    InvalidOptionPresence {
        /// The invalid presence byte.
        value: u8,
    },
    /// A collection entry ends before its length or value is complete.
    TruncatedCollectionEntry {
        /// The first incomplete collection path.
        path: Vec<CollectionValuePathSegment>,
    },
    /// One isolated complete child value is invalid.
    ConstructedChild {
        /// The child path from the constructed root.
        path: Vec<CollectionValuePathSegment>,
        /// The child codec failure.
        source: Box<ValueCodecError>,
    },
    /// MAP entries are not already in canonical key order.
    NonCanonicalMapOrder {
        /// The first non-canonical wire entry index.
        index: usize,
    },
    /// SET elements are not already in canonical element order.
    NonCanonicalSetOrder {
        /// The first non-canonical wire entry index.
        index: usize,
    },
    /// The core checked collection constructor rejected a value.
    CollectionValue {
        /// The core collection failure.
        source: CollectionValueError,
    },
    /// The encoded value does not contain the complete fixed header.
    TruncatedHeader {
        /// The total number of available bytes.
        actual: usize,
    },
    /// The encoded value does not start with the selected codec marker.
    InvalidMarker,
    /// The value tag is not defined by the selected codec version.
    UnknownTag {
        /// The unrecognised wire tag.
        tag: u8,
    },
    /// The value tag and stable type identity do not agree.
    WrongType {
        /// The recognised wire tag.
        tag: u8,
        /// The incompatible type identity.
        actual: TypeId,
    },
    /// The encoded payload is shorter than its declared length.
    TruncatedPayload {
        /// The length declared by the header.
        declared: usize,
        /// The number of available payload bytes.
        actual: usize,
    },
    /// Bytes occur after the declared payload.
    TrailingBytes {
        /// The length declared by the header.
        declared: usize,
        /// The number of available payload bytes.
        actual: usize,
    },
    /// A fixed-width payload has the wrong length.
    WrongPayloadLength {
        /// The recognised wire tag.
        tag: u8,
        /// The only valid payload length for this tag.
        expected: usize,
        /// The declared and available payload length.
        actual: usize,
    },
    /// A Boolean payload is not the canonical zero or one byte.
    InvalidBoolean {
        /// The invalid payload byte.
        value: u8,
    },
    /// A float payload is non-finite or is the non-canonical negative zero.
    NonCanonicalFloat,
    /// A supplied or declared payload exceeds the shared codec limit.
    PayloadTooLarge {
        /// The supplied or declared payload length.
        actual: usize,
        /// The shared canonical value payload limit.
        maximum: usize,
    },
    /// A text payload is not valid UTF-8.
    InvalidUtf8,
    /// A stable standard scalar identity was used as a reference target.
    StandardTypeAsReference {
        /// The stable scalar identity used as a reference target.
        target: TypeId,
    },
    /// The active catalogue does not contain the supplied enum type.
    InactiveEnumType {
        /// The inactive enum type identity.
        enum_type: TypeId,
    },
    /// The active enum type does not declare the encoded exact label.
    UndeclaredEnumLabel {
        /// The active enum type identity.
        enum_type: TypeId,
        /// The undeclared label.
        label: String,
    },
    /// The active revision does not contain the supplied record type.
    InactiveRecordType {
        /// The inactive record type identity.
        record_type: TypeId,
    },
    /// A checked record value is not valid against the supplied active revision.
    RecordValueNotActive {
        /// The incompatible record type identity.
        record_type: TypeId,
    },
    /// The encoded field count differs from the active record definition.
    WrongRecordFieldCount {
        /// The field count required by the active definition.
        expected: usize,
        /// The field count declared by the encoded payload.
        actual: usize,
    },
    /// An encoded field identity differs from the active declaration ordinal.
    WrongRecordFieldIdentity {
        /// The zero-based declaration ordinal.
        ordinal: usize,
        /// The stable field identity required at this ordinal.
        expected: FieldId,
        /// The stable field identity found in the encoded payload.
        actual: FieldId,
    },
    /// The record payload ends before one complete field-entry header.
    TruncatedRecordFieldHeader {
        /// The zero-based declaration ordinal.
        ordinal: usize,
        /// The bytes available for the field-entry header.
        actual: usize,
    },
    /// An encoded complete field-value length cannot fit in the record payload.
    InvalidRecordFieldLength {
        /// The zero-based declaration ordinal.
        ordinal: usize,
        /// The complete field-value length declared by the entry.
        declared: usize,
        /// The bytes available after the entry header.
        remaining: usize,
    },
    /// A record field value does not use its declared wire type.
    WrongRecordFieldType {
        /// The zero-based declaration ordinal.
        ordinal: usize,
        /// The field descriptor required by the active definition.
        expected: TypeDescriptor,
        /// The encoded value tag.
        tag: u8,
        /// The encoded stable type identity.
        actual: TypeId,
    },
    /// A registered opaque value is invalid for the supplied revision or registry.
    OpaqueValue {
        /// The opaque value validation failure.
        source: OpaqueValueError,
    },
    /// A sealed invocation carrier is invalid.
    InvocationCarrier {
        /// The exact sealed carrier identity.
        carrier: TypeId,
        /// The carrier payload validation failure.
        source: InvocationCarrierCodecError,
    },
}

/// One immutable logical path in a sealed invocation carrier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationCarrierPath {
    segments: Vec<InvocationCarrierPathSegment>,
}

impl InvocationCarrierPath {
    /// Returns the logical path segments in wire order.
    pub fn segments(&self) -> &[InvocationCarrierPathSegment] {
        &self.segments
    }

    fn one(segment: InvocationCarrierPathSegment) -> Self {
        Self {
            segments: vec![segment],
        }
    }

    fn with(&self, segment: InvocationCarrierPathSegment) -> Self {
        let mut segments = self.segments.clone();
        segments.push(segment);
        Self { segments }
    }
}

/// One closed logical segment in an invocation-carrier path.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationCarrierPathSegment {
    /// The inner runtime value of `sys.invoke.Value`.
    ValueInner,
    /// The request target selector.
    RequestTarget,
    /// The request argument sequence.
    RequestArguments,
    /// The request caller context.
    RequestCaller,
    /// The request client offer.
    RequestClientOffer,
    /// The optional request output requirement.
    RequestOutputRequirement,
    /// The optional request state profile.
    RequestStateProfile,
    /// The request trace policy.
    RequestTracePolicy,
    /// The closed version-1 request deadline.
    RequestDeadline,
    /// The optional request idempotency key.
    RequestIdempotencyKey,
    /// The optional parent invocation identity.
    RequestParentInvocation,
    /// The optional typed observer context.
    RequestObserverContext,
    /// One zero-based request argument.
    Argument(usize),
    /// One argument selector.
    Selector,
    /// One embedded typed value.
    Value,
    /// The caller kind.
    CallerKind,
    /// The caller fact flags.
    CallerFlags,
    /// The optional terminal column count.
    TerminalColumns,
    /// The optional terminal row count.
    TerminalRows,
    /// A caller locale.
    Locale,
    /// A caller timezone.
    Timezone,
    /// The optional typed preference policy.
    PreferencePolicy,
    /// The already negotiated client protocol major.
    ClientProtocol,
    /// The client locale.
    ClientLocale,
    /// The client timezone.
    ClientTimezone,
    /// The client sink-offer sequence.
    ClientSinks,
    /// One zero-based sink offer.
    Sink(usize),
    /// One type descriptor.
    Descriptor,
    /// A media-type sequence.
    MediaTypes,
    /// One zero-based media type.
    MediaType(usize),
    /// A streaming fact or requirement.
    Streaming,
    /// A signed preference rank.
    PreferenceRank,
    /// Optional typed limits.
    Limits,
    /// The client runtime-offer sequence.
    ClientRuntimes,
    /// One zero-based runtime offer.
    Runtime(usize),
    /// A runtime name.
    RuntimeName,
    /// A runtime version.
    RuntimeVersion,
    /// A consumed-type sequence.
    ConsumedTypes,
    /// One zero-based consumed type.
    ConsumedType(usize),
    /// A runtime-contract sequence.
    Contracts,
    /// One zero-based runtime contract.
    Contract(usize),
    /// A runtime-contract name.
    ContractName,
    /// A runtime-contract version.
    ContractVersion,
    /// A runtime-contract feature sequence.
    Features,
    /// One zero-based runtime-contract feature.
    Feature(usize),
    /// A runtime trust fact.
    Trusted,
    /// The maximum accepted client frame size.
    ClientMaximumFrameSize,
    /// The maximum accepted client artifact size.
    ClientMaximumArtifactSize,
    /// Optional typed client limits.
    ClientLimits,
    /// Optional typed client preferences.
    ClientPreferences,
    /// An optional output alias.
    OutputAlias,
    /// An optional output media type.
    OutputMediaType,
    /// An optional output type selector.
    OutputType,
    /// An output streaming requirement.
    OutputStreaming,
    /// The event kind.
    EventKind,
    /// The event invocation identity.
    EventInvocation,
    /// The event sequence.
    EventSequence,
    /// The event body.
    EventBody,
    /// An optional visible principal identity.
    VisiblePrincipal,
    /// A result channel.
    Channel,
    /// An optional typed result schema.
    Schema,
    /// A result-value batch.
    BatchValues,
    /// One zero-based result value.
    BatchValue(usize),
    /// A diagnostic severity.
    Severity,
    /// A stable diagnostic or failure code.
    Code,
    /// A diagnostic or failure message.
    Message,
    /// A completed invocation duration.
    Duration,
    /// A failure phase.
    Phase,
    /// Optional typed failure details.
    Details,
    /// A failure retryability value.
    Retryability,
    /// An optional cancellation reason.
    Reason,
}

/// One failure from a sealed invocation-carrier payload codec.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationCarrierCodecError {
    /// The carrier payload version is not supported.
    UnsupportedVersion {
        /// The unsupported version byte.
        actual: u8,
    },
    /// Required carrier bytes are not available.
    Truncated {
        /// The zero-based payload offset of the incomplete field.
        offset: usize,
        /// The required byte count at the offset.
        required: usize,
        /// The available byte count at the offset.
        available: usize,
    },
    /// Bytes remain after one complete carrier payload.
    Trailing {
        /// The unconsumed byte count.
        remaining: usize,
    },
    /// A discriminant is not in its closed version-1 set.
    UnknownDiscriminant {
        /// The field that contains the discriminant.
        path: InvocationCarrierPath,
        /// The unknown discriminant byte.
        actual: u8,
    },
    /// A Boolean or presence byte is not zero or one.
    InvalidBoolean {
        /// The field that contains the invalid Boolean.
        path: InvocationCarrierPath,
        /// The invalid Boolean byte.
        actual: u8,
    },
    /// A text field is not valid UTF-8.
    InvalidText {
        /// The invalid text field.
        path: InvocationCarrierPath,
    },
    /// A resolved semantic name is not canonical and qualified.
    InvalidSemanticName {
        /// The invalid semantic-name field.
        path: InvocationCarrierPath,
    },
    /// A field violates its version-1 semantic constraint.
    InvalidField {
        /// The invalid field.
        path: InvocationCarrierPath,
    },
    /// A wire sequence is not strictly increasing by its canonical key.
    NonCanonicalOrder {
        /// The non-canonical sequence.
        path: InvocationCarrierPath,
        /// The first non-canonical wire index.
        index: usize,
    },
    /// A canonical-key sequence contains an exact duplicate.
    DuplicateItem {
        /// The sequence that contains the duplicate.
        path: InvocationCarrierPath,
        /// The first original source or wire index.
        first: usize,
        /// The duplicate original source or wire index.
        duplicate: usize,
    },
    /// A sealed carrier occurs where ordinary runtime data is required.
    NestedCarrier {
        /// The typed-value or descriptor field that contains the carrier.
        path: InvocationCarrierPath,
        /// The exact rejected carrier identity.
        carrier: TypeId,
    },
    /// An embedded ordinary ORV5 value is invalid.
    InnerValue {
        /// The typed-value field that contains the invalid value.
        path: InvocationCarrierPath,
        /// The ordinary ORV5 codec failure.
        source: Box<ValueCodecError>,
    },
    /// The complete aggregate carrier tree exceeds its node bound.
    TooManyNodes {
        /// The accepted aggregate node maximum.
        maximum: usize,
    },
    /// A carrier payload exceeds the shared ORV5 payload bound.
    PayloadTooLarge {
        /// The supplied or computed payload size.
        actual: usize,
        /// The shared maximum payload size.
        maximum: usize,
    },
}

impl fmt::Display for InvocationCarrierCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { .. } => {
                formatter.write_str("invocation carrier version is not supported")
            }
            Self::Truncated { .. } => formatter.write_str("invocation carrier is truncated"),
            Self::Trailing { .. } => formatter.write_str("invocation carrier has trailing bytes"),
            Self::UnknownDiscriminant { .. } => {
                formatter.write_str("invocation carrier discriminant is unknown")
            }
            Self::InvalidBoolean { .. } => {
                formatter.write_str("invocation carrier Boolean is invalid")
            }
            Self::InvalidText { .. } => formatter.write_str("invocation carrier text is invalid"),
            Self::InvalidSemanticName { .. } => {
                formatter.write_str("invocation carrier semantic name is invalid")
            }
            Self::InvalidField { .. } => formatter.write_str("invocation carrier field is invalid"),
            Self::NonCanonicalOrder { .. } => {
                formatter.write_str("invocation carrier items are not in canonical order")
            }
            Self::DuplicateItem { .. } => {
                formatter.write_str("invocation carrier contains a duplicate item")
            }
            Self::NestedCarrier { .. } => {
                formatter.write_str("invocation carrier cannot contain another carrier here")
            }
            Self::InnerValue { .. } => {
                formatter.write_str("invocation carrier typed value is invalid")
            }
            Self::TooManyNodes { .. } => {
                formatter.write_str("invocation carrier tree has too many nodes")
            }
            Self::PayloadTooLarge { .. } => {
                formatter.write_str("invocation carrier payload is too large")
            }
        }
    }
}

impl Error for InvocationCarrierCodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InnerValue { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl fmt::Display for ValueCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedValue => {
                formatter.write_str("runtime value is not supported by the selected codec version")
            }
            Self::ConstructedTypeIdentityNotZero { .. } => {
                formatter.write_str("constructed runtime value identity must be zero")
            }
            Self::TruncatedConstructedHeader { .. } => {
                formatter.write_str("constructed runtime value header is truncated")
            }
            Self::EmptyConstructedDescriptor => {
                formatter.write_str("constructed runtime value descriptor is empty")
            }
            Self::TruncatedConstructedDescriptor { .. } => {
                formatter.write_str("constructed runtime value descriptor is truncated")
            }
            Self::TruncatedConstructedDescriptorNode { .. } => {
                formatter.write_str("constructed runtime value descriptor node is truncated")
            }
            Self::TrailingConstructedDescriptor { .. } => {
                formatter.write_str("constructed runtime value descriptor has trailing bytes")
            }
            Self::UnknownConstructedDescriptorTag { .. } => {
                formatter.write_str("constructed runtime value descriptor tag is unknown")
            }
            Self::InvalidConstructedDescriptor { .. } => {
                formatter.write_str("constructed runtime value descriptor is invalid")
            }
            Self::UnsupportedConstructedDescriptor { .. } => {
                formatter.write_str("constructed runtime value descriptor is not accepted")
            }
            Self::InvalidOptionPresence { .. } => {
                formatter.write_str("constructed OPTION presence is invalid")
            }
            Self::TruncatedCollectionEntry { .. } => {
                formatter.write_str("constructed runtime value entry is truncated")
            }
            Self::ConstructedChild { .. } => {
                formatter.write_str("constructed runtime value child is invalid")
            }
            Self::NonCanonicalMapOrder { .. } => {
                formatter.write_str("constructed MAP entries are not in canonical key order")
            }
            Self::NonCanonicalSetOrder { .. } => {
                formatter.write_str("constructed SET elements are not in canonical order")
            }
            Self::CollectionValue { .. } => {
                formatter.write_str("constructed runtime value is invalid")
            }
            Self::TruncatedHeader { .. } => {
                formatter.write_str("runtime value header is truncated")
            }
            Self::InvalidMarker => formatter.write_str("runtime value marker is invalid"),
            Self::UnknownTag { .. } => formatter.write_str("runtime value tag is unknown"),
            Self::WrongType { .. } => {
                formatter.write_str("runtime value tag and type identity do not agree")
            }
            Self::TruncatedPayload { .. } => {
                formatter.write_str("runtime value payload is truncated")
            }
            Self::TrailingBytes { .. } => formatter.write_str("runtime value has trailing bytes"),
            Self::WrongPayloadLength { .. } => {
                formatter.write_str("runtime value payload has the wrong length")
            }
            Self::InvalidBoolean { .. } => {
                formatter.write_str("BOOLEAN payload must be zero or one")
            }
            Self::NonCanonicalFloat => {
                formatter.write_str("FLOAT payload is not canonical and finite")
            }
            Self::PayloadTooLarge { .. } => {
                formatter.write_str("runtime value payload exceeds the codec limit")
            }
            Self::InvalidUtf8 => formatter.write_str("text payload is not valid UTF-8"),
            Self::StandardTypeAsReference { .. } => {
                formatter.write_str("stable standard scalar cannot be a reference target")
            }
            Self::InactiveEnumType { .. } => {
                formatter.write_str("enum type is not active for the canonical value")
            }
            Self::UndeclaredEnumLabel { .. } => {
                formatter.write_str("enum label is not declared by the active type")
            }
            Self::InactiveRecordType { .. } => {
                formatter.write_str("record type is not active for the canonical value")
            }
            Self::RecordValueNotActive { .. } => {
                formatter.write_str("record value is not valid for the active revision")
            }
            Self::WrongRecordFieldCount { .. } => {
                formatter.write_str("record field count does not match the active definition")
            }
            Self::WrongRecordFieldIdentity { .. } => {
                formatter.write_str("record field identity does not match its declaration ordinal")
            }
            Self::TruncatedRecordFieldHeader { .. } => {
                formatter.write_str("record field-entry header is truncated")
            }
            Self::InvalidRecordFieldLength { .. } => {
                formatter.write_str("record field length is invalid")
            }
            Self::WrongRecordFieldType { .. } => {
                formatter.write_str("record field value does not match its declared type")
            }
            Self::OpaqueValue { .. } => {
                formatter.write_str("opaque value is not valid for the active registry")
            }
            Self::InvocationCarrier { .. } => {
                formatter.write_str("sealed invocation carrier is invalid")
            }
        }
    }
}

impl Error for ValueCodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidConstructedDescriptor { source } => Some(source),
            Self::ConstructedChild { source, .. } => Some(source),
            Self::CollectionValue { source } => Some(source),
            Self::OpaqueValue { source } => Some(source),
            Self::InvocationCarrier { source, .. } => Some(source),
            _ => None,
        }
    }
}
/// An error from canonical `ORNA-ROWS/1` encoding or decoding.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowsCodecError {
    /// The complete Rows frame exceeds the shared opaque payload bound.
    PayloadTooLarge { actual: usize, maximum: usize },
    /// The frame does not start with the exact Rows magic.
    InvalidMagic,
    /// The frame version is not supported.
    UnsupportedVersion(u16),
    /// The frame ended before a complete field was available.
    Truncated,
    /// Bytes remained after the complete Rows frame.
    TrailingBytes,
    /// The column count is outside the accepted bound.
    ColumnCountExceeded { actual: usize, maximum: usize },
    /// The row count is outside the accepted bound.
    RowCountExceeded { actual: usize, maximum: usize },
    /// The cell count is outside the accepted bound.
    CellCountExceeded { actual: usize, maximum: usize },
    /// The checked row/column product exceeds the accepted bound.
    CellProductExceeded {
        rows: usize,
        columns: usize,
        maximum: usize,
    },
    /// One column name is not valid UTF-8.
    InvalidColumnNameUtf8 { column: usize },
    /// One column name is empty.
    EmptyColumnName { column: usize },
    /// One column name repeats an earlier exact byte name.
    DuplicateColumnName { first: usize, duplicate: usize },
    /// A column type form is not part of ORNA-ROWS/1.
    UnknownTypeForm { column: usize, type_form: u8 },
    /// A column nullable byte is not zero or one.
    InvalidNullable { column: usize, value: u8 },
    /// A declared column type is not active in the supplied revision.
    InactiveType {
        column: usize,
        type_form: u8,
        type_id: TypeId,
    },
    /// A declared value type is opaque and therefore cannot be a Rows cell.
    OpaqueColumnType { column: usize, type_id: TypeId },
    /// A row's cell count differs from the declared column count.
    RowWidthMismatch {
        row: usize,
        expected: usize,
        actual: usize,
    },
    /// A cell's ORV5 marker is not the exact ORV5 marker.
    InvalidCellMarker { row: usize, column: usize },
    /// A decoded cell does not re-encode to its exact supplied ORV5 bytes.
    NonCanonicalCell { row: usize, column: usize },
    /// An ORV5 cell could not be decoded against the active revision.
    CellValue {
        row: usize,
        column: usize,
        source: ValueCodecError,
    },
    /// A decoded cell violates the declared ResultRows shape.
    ResultRows { source: ResultRowsError },
    /// The Rows value cannot be registered under the active standard snapshot.
    OpaqueValue { source: OpaqueValueError },
}

impl fmt::Display for RowsCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadTooLarge { .. } => formatter.write_str("Rows payload exceeds its bound"),
            Self::InvalidMagic => formatter.write_str("invalid ORNA-ROWS/1 magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported ORNA-ROWS version {version}")
            }
            Self::Truncated => formatter.write_str("truncated ORNA-ROWS frame"),
            Self::TrailingBytes => formatter.write_str("trailing bytes after ORNA-ROWS frame"),
            Self::ColumnCountExceeded { .. } => {
                formatter.write_str("Rows column count exceeds its bound")
            }
            Self::RowCountExceeded { .. } => {
                formatter.write_str("Rows row count exceeds its bound")
            }
            Self::CellCountExceeded { .. } => {
                formatter.write_str("Rows cell count exceeds its bound")
            }
            Self::CellProductExceeded { .. } => {
                formatter.write_str("Rows cell product exceeds its bound")
            }
            Self::InvalidColumnNameUtf8 { .. } => {
                formatter.write_str("Rows column name is not valid UTF-8")
            }
            Self::EmptyColumnName { .. } => formatter.write_str("Rows column name is empty"),
            Self::DuplicateColumnName { .. } => {
                formatter.write_str("Rows column name is duplicated")
            }
            Self::UnknownTypeForm { .. } => formatter.write_str("Rows column type form is unknown"),
            Self::InvalidNullable { .. } => formatter.write_str("Rows nullable flag is invalid"),
            Self::InactiveType { .. } => formatter.write_str("Rows column type is not active"),
            Self::OpaqueColumnType { .. } => {
                formatter.write_str("opaque value types are not valid Rows columns")
            }
            Self::RowWidthMismatch { .. } => formatter.write_str("Rows row width does not match"),
            Self::InvalidCellMarker { .. } => formatter.write_str("Rows cell is not ORV5"),
            Self::NonCanonicalCell { .. } => formatter.write_str("Rows cell is not canonical ORV5"),
            Self::CellValue { source, .. } => write!(formatter, "Rows cell is invalid: {source}"),
            Self::ResultRows { source } => source.fmt(formatter),
            Self::OpaqueValue { source } => source.fmt(formatter),
        }
    }
}

impl Error for RowsCodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CellValue { source, .. } => Some(source),
            Self::ResultRows { source } => Some(source),
            Self::OpaqueValue { source } => Some(source),
            _ => None,
        }
    }
}

/// Encodes one runtime value as canonical version-1 bytes.
///
/// # Errors
///
/// Returns [`ValueCodecError::PayloadTooLarge`] for text or byte payloads over
/// 16 MiB. Returns [`ValueCodecError::StandardTypeAsReference`] when a
/// reference uses a stable standard scalar identity. Returns
/// [`ValueCodecError::UnsupportedValue`] when the non-exhaustive core value
/// model contains a category that version 1 does not define.
pub fn encode_value(value: &RuntimeValue) -> Result<Vec<u8>, ValueCodecError> {
    match value {
        RuntimeValue::Boolean(value) => {
            let payload = [u8::from(*value)];
            Ok(encode(BOOLEAN_TAG, BOOLEAN_TYPE_ID, &payload))
        }
        RuntimeValue::Integer(value) => {
            Ok(encode(INTEGER_TAG, INTEGER_TYPE_ID, &value.to_be_bytes()))
        }
        RuntimeValue::BigInt(value) => Ok(encode(BIGINT_TAG, BIGINT_TYPE_ID, &value.to_be_bytes())),
        RuntimeValue::Float(value) => {
            let value = if value.value() == 0.0 {
                0.0
            } else {
                value.value()
            };
            Ok(encode(
                FLOAT_TAG,
                FLOAT_TYPE_ID,
                &value.to_bits().to_be_bytes(),
            ))
        }
        RuntimeValue::Text(value) => {
            encode_variable(TEXT_TAG, CHARACTER_LARGE_OBJECT_TYPE_ID, value.as_bytes())
        }
        RuntimeValue::Bytes(value) => {
            encode_variable(BYTES_TAG, BINARY_LARGE_OBJECT_TYPE_ID, value)
        }
        RuntimeValue::Null(value) => {
            let resolved_type = value.resolved_type();
            if let Some(target) = resolved_type.reference_target() {
                require_reference_target(target)?;
                Ok(encode(NULL_REFERENCE_TAG, target, &[]))
            } else if let Some(scalar) = resolved_type.legacy_scalar() {
                let type_id =
                    supported_scalar_type_id(scalar).ok_or(ValueCodecError::UnsupportedValue)?;
                Ok(encode(NULL_SCALAR_TAG, type_id, &[]))
            } else {
                Err(ValueCodecError::UnsupportedValue)
            }
        }
        RuntimeValue::Reference { target, object } => {
            require_reference_target(*target)?;
            Ok(encode(REFERENCE_TAG, *target, &object.to_bytes()))
        }
        _ => Err(ValueCodecError::UnsupportedValue),
    }
}

/// Encodes one runtime value against the active catalogue as canonical
/// version-2 bytes.
///
/// Version 2 retains every version-1 tag and payload unchanged under the
/// `ORV2` marker. It adds catalogue enum values and typed enum nulls.
///
/// # Errors
///
/// Returns [`ValueCodecError`] when the value violates the version-1 rules or
/// when an enum type or label is absent from the active catalogue.
pub fn encode_catalogue_value(
    catalogue: &CatalogueSnapshot,
    value: &RuntimeValue,
) -> Result<Vec<u8>, ValueCodecError> {
    match value {
        RuntimeValue::Enum(value) => {
            validate_enum_value(catalogue, value.enum_type(), value.label())?;
            encode_variable(ENUM_TAG, value.enum_type(), value.label().as_bytes())
                .map(with_catalogue_marker)
        }
        RuntimeValue::Null(value) if value.resolved_type().named_type().is_some() => {
            let enum_type = value
                .resolved_type()
                .named_type()
                .expect("named type checked");
            require_active_enum_type(catalogue, enum_type)?;
            Ok(encode_with_marker(
                CATALOGUE_MARKER,
                NULL_ENUM_TAG,
                enum_type,
                &[],
            ))
        }
        _ => encode_value(value).map(with_catalogue_marker),
    }
}

/// Decodes one complete canonical version-1 runtime value.
///
/// # Errors
///
/// Returns a [`ValueCodecError`] for a truncated header or payload, invalid
/// marker, unknown tag, wrong stable type identity, trailing bytes, wrong
/// fixed payload length, invalid Boolean, non-canonical float, oversized
/// declared payload, invalid UTF-8, or stable scalar identity used as a
/// reference target. It never returns a partial value.
pub fn decode_value(encoded: &[u8]) -> Result<RuntimeValue, ValueCodecError> {
    let (tag, type_id, payload) = decode_envelope(encoded, MARKER)?;
    decode_non_enum_value(tag, type_id, payload)
}

/// Decodes one complete canonical version-2 value against the active
/// catalogue.
///
/// # Errors
///
/// Returns [`ValueCodecError`] for every invalid version-1 byte shape and when
/// an enum type or exact label is absent from the active catalogue.
pub fn decode_catalogue_value(
    catalogue: &CatalogueSnapshot,
    encoded: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let (tag, type_id, payload) = decode_envelope(encoded, CATALOGUE_MARKER)?;
    decode_catalogue_value_parts(catalogue, tag, type_id, payload)
}

/// Encodes one runtime value against an active revision as canonical
/// version-3 bytes.
///
/// Version 3 retains every version-2 scalar and enum shape under the `ORV3`
/// marker and adds named immutable record values.
///
/// # Errors
///
/// Returns [`ValueCodecError`] when the value violates an earlier codec rule
/// or is not valid against the supplied active revision.
pub fn encode_active_value(
    active: &ActiveDatabaseRevision,
    value: &RuntimeValue,
) -> Result<Vec<u8>, ValueCodecError> {
    match value {
        RuntimeValue::Record(value) => encode_record_value(active, value),
        RuntimeValue::Enum(value) => {
            validate_active_enum_value(active, value.enum_type(), value.label())?;
            encode_variable(ENUM_TAG, value.enum_type(), value.label().as_bytes())
                .map(with_active_marker)
        }
        RuntimeValue::Null(value) if value.resolved_type().named_type().is_some() => {
            let enum_type = value
                .resolved_type()
                .named_type()
                .expect("named type checked");
            require_active_enum_type_for_revision(active, enum_type)?;
            Ok(encode_with_marker(
                ACTIVE_MARKER,
                NULL_ENUM_TAG,
                enum_type,
                &[],
            ))
        }
        _ => encode_catalogue_value(active.catalogue(), value).map(with_active_marker),
    }
}

/// Decodes one complete canonical version-3 value against an active revision.
///
/// # Errors
///
/// Returns [`ValueCodecError`] for every invalid version-2 byte shape and for
/// a record that does not match the active nominal definition. It never
/// returns a partial value.
pub fn decode_active_value(
    active: &ActiveDatabaseRevision,
    encoded: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let (tag, type_id, payload) = decode_envelope(encoded, ACTIVE_MARKER)?;
    if tag == RECORD_TAG {
        decode_record_value(active, type_id, payload)
    } else {
        decode_active_non_record_value(active, tag, type_id, payload)
    }
}

/// Encodes one runtime value against an active revision and immutable opaque
/// codec registry as canonical version-4 bytes.
///
/// Version 4 retains every version-3 value shape under the `ORV4` marker and
/// adds non-null registered opaque values.
///
/// # Errors
///
/// Returns [`ValueCodecError`] when the value violates an earlier codec rule,
/// is not valid against the supplied active revision, or is not accepted by
/// the supplied opaque codec registry.
pub fn encode_registered_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &RuntimeValue,
) -> Result<Vec<u8>, ValueCodecError> {
    encode_registered_value_with_marker(active, registry, value, REGISTERED_MARKER)
}

fn encode_registered_value_with_marker(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &RuntimeValue,
    marker: &[u8; 4],
) -> Result<Vec<u8>, ValueCodecError> {
    match value {
        RuntimeValue::Opaque(value) => {
            let checked = OpaqueValue::new(
                active,
                registry,
                value.opaque_type(),
                value.canonical_payload(),
            )
            .map_err(|source| ValueCodecError::OpaqueValue { source })?;
            Ok(encode_with_marker(
                marker,
                OPAQUE_TAG,
                checked.opaque_type(),
                checked.canonical_payload(),
            ))
        }
        RuntimeValue::Record(value) => encode_record_value_with_marker(active, value, marker),
        _ => encode_catalogue_value(active.catalogue(), value).map(|mut encoded| {
            encoded[..marker.len()].copy_from_slice(marker);
            encoded
        }),
    }
}

/// Decodes one complete canonical version-4 value against an active revision
/// and immutable opaque codec registry.
///
/// # Errors
///
/// Returns [`ValueCodecError`] for every invalid version-3 byte shape and for
/// an opaque value rejected by the supplied active revision or registry. It
/// never returns a partial value.
pub fn decode_registered_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    decode_registered_value_with_marker(active, registry, encoded, REGISTERED_MARKER)
}

fn decode_registered_value_with_marker(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
    marker: &[u8; 4],
) -> Result<RuntimeValue, ValueCodecError> {
    let (tag, type_id, payload) = decode_envelope(encoded, marker)?;
    match tag {
        RECORD_TAG => decode_record_value_with_marker(active, type_id, payload, marker),
        OPAQUE_TAG => OpaqueValue::new(active, registry, type_id, payload)
            .map(RuntimeValue::Opaque)
            .map_err(|source| ValueCodecError::OpaqueValue { source }),
        _ => decode_catalogue_value_parts(active.catalogue(), tag, type_id, payload),
    }
}
/// Encodes one complete ORV5/ORV6 runtime value.
///
/// ORV5 retains every ORV4 value and adds checked OPTION, LIST, and MAP values.
/// ORV6 is selected for checked SET values.
pub fn encode_constructed_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &RuntimeValue,
) -> Result<Vec<u8>, ValueCodecError> {
    encode_orv5_value(active, registry, value)
}

/// Decodes one complete ORV5 or ORV6 runtime value.
///
/// The decoder validates the whole structural tree before materialising any
/// value. This preserves the authoritative global node-limit precedence.
pub fn decode_constructed_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let (is_set_version, tag, type_id, payload) = decode_constructed_envelope(encoded)?;
    if is_set_version && tag != CONSTRUCTED_TAG {
        return Err(ValueCodecError::InvalidMarker);
    }
    if tag == OPAQUE_TAG && invocation_carrier_by_id(type_id).is_some() {
        return decode_invocation_carrier(active, registry, type_id, payload);
    }
    if tag == CONSTRUCTED_TAG && type_id.to_bytes() != [0; 16] {
        return Err(ValueCodecError::ConstructedTypeIdentityNotZero { identity: type_id });
    }
    if tag == CONSTRUCTED_TAG {
        let (descriptor, body) = decode_constructed_descriptor_with_set(payload, is_set_version)?;
        preflight_constructed_descriptor(active, &descriptor)?;
        let mut budget = NodeBudget::runtime();
        preflight_orv5_tree(
            payload,
            tag,
            &mut budget,
            &mut Vec::new(),
            InvocationCarrierPreflightPolicy::Allow,
            is_set_version,
        )?;
        return decode_constructed_parts(active, registry, descriptor, body);
    }
    let mut budget = NodeBudget::runtime();
    preflight_orv5_tree(
        payload,
        tag,
        &mut budget,
        &mut Vec::new(),
        InvocationCarrierPreflightPolicy::Allow,
        false,
    )?;
    decode_orv5_parts(active, registry, tag, type_id, payload)
}
/// Encodes one complete `ResultRows` value as the canonical `ORNA-ROWS/1`
/// payload and verifies it against the registered V8 Rows codec.
pub fn encode_rows(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    rows: &ResultRows,
) -> Result<Vec<u8>, RowsCodecError> {
    let columns = rows.columns();
    let row_values = rows.rows();
    validate_rows_shape_bounds(columns.len(), row_values.len())?;

    let mut writer = RowsWriter::new();
    writer.bytes(orna_core::value::ROWS_MAGIC)?;
    writer.u16(orna_core::value::ROWS_FRAME_VERSION)?;
    writer.u32(u32::try_from(columns.len()).map_err(|_| {
        RowsCodecError::ColumnCountExceeded {
            actual: columns.len(),
            maximum: MAX_ROWS_COLUMNS,
        }
    })?)?;
    for (column_index, column) in columns.iter().enumerate() {
        let name = column.name().as_bytes();
        writer.u32(
            u32::try_from(name.len()).map_err(|_| RowsCodecError::PayloadTooLarge {
                actual: name.len(),
                maximum: MAX_ROWS_PAYLOAD_LENGTH,
            })?,
        )?;
        writer.bytes(name)?;
        let (type_form, type_id) = rows_type_wire(active, column.resolved_type(), column_index)?;
        writer.byte(type_form)?;
        writer.bytes(&type_id.to_bytes())?;
        writer.byte(u8::from(column.nullable()))?;
    }
    writer.u32(u32::try_from(row_values.len()).map_err(|_| {
        RowsCodecError::RowCountExceeded {
            actual: row_values.len(),
            maximum: MAX_ROWS_ROWS,
        }
    })?)?;

    for (row_index, row) in row_values.iter().enumerate() {
        if row.values().len() != columns.len() {
            return Err(RowsCodecError::RowWidthMismatch {
                row: row_index,
                expected: columns.len(),
                actual: row.values().len(),
            });
        }
        writer.u32(u32::try_from(row.values().len()).map_err(|_| {
            RowsCodecError::CellCountExceeded {
                actual: row.values().len(),
                maximum: MAX_ROWS_COLUMNS,
            }
        })?)?;
        for (column_index, (column, value)) in columns.iter().zip(row.values()).enumerate() {
            validate_rows_value(active, column, value, row_index, column_index)?;
            let encoded = encode_constructed_value(active, registry, value).map_err(|source| {
                RowsCodecError::CellValue {
                    row: row_index,
                    column: column_index,
                    source,
                }
            })?;
            if encoded.len() > MAX_ROWS_PAYLOAD_LENGTH {
                return Err(RowsCodecError::PayloadTooLarge {
                    actual: encoded.len(),
                    maximum: MAX_ROWS_PAYLOAD_LENGTH,
                });
            }
            writer.u32(u32::try_from(encoded.len()).map_err(|_| {
                RowsCodecError::PayloadTooLarge {
                    actual: encoded.len(),
                    maximum: MAX_ROWS_PAYLOAD_LENGTH,
                }
            })?)?;
            writer.bytes(&encoded)?;
        }
    }

    let payload = writer.finish();
    OpaqueValue::new(active, registry, STD_DATA_ROWS_TYPE_ID, &payload)
        .map_err(|source| RowsCodecError::OpaqueValue { source })?;
    Ok(payload)
}

/// Wraps a canonical Rows payload as one registered opaque runtime value.
pub fn encode_rows_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    rows: &ResultRows,
) -> Result<RuntimeValue, RowsCodecError> {
    let payload = encode_rows(active, registry, rows)?;
    OpaqueValue::new(active, registry, STD_DATA_ROWS_TYPE_ID, payload)
        .map(RuntimeValue::Opaque)
        .map_err(|source| RowsCodecError::OpaqueValue { source })
}

/// Decodes and validates one complete canonical `ORNA-ROWS/1` payload into
/// the existing immutable [`ResultRows`] model.
pub fn decode_rows(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
) -> Result<ResultRows, RowsCodecError> {
    if encoded.len() > MAX_ROWS_PAYLOAD_LENGTH {
        return Err(RowsCodecError::PayloadTooLarge {
            actual: encoded.len(),
            maximum: MAX_ROWS_PAYLOAD_LENGTH,
        });
    }
    let mut reader = RowsReader::new(encoded);
    let magic = reader.take(orna_core::value::ROWS_MAGIC.len())?;
    if magic != orna_core::value::ROWS_MAGIC {
        return Err(RowsCodecError::InvalidMagic);
    }
    let version = reader.u16()?;
    if version != orna_core::value::ROWS_FRAME_VERSION {
        return Err(RowsCodecError::UnsupportedVersion(version));
    }

    let column_count = reader.usize_u32()?;
    if !(1..=MAX_ROWS_COLUMNS).contains(&column_count) {
        return Err(RowsCodecError::ColumnCountExceeded {
            actual: column_count,
            maximum: MAX_ROWS_COLUMNS,
        });
    }
    let minimum_columns = column_count
        .checked_mul(ROWS_COLUMN_MIN_BYTES)
        .and_then(|bytes| bytes.checked_add(4));
    if minimum_columns.is_none_or(|minimum| reader.remaining() < minimum) {
        return Err(RowsCodecError::Truncated);
    }
    let mut names = BTreeMap::new();
    let mut columns = Vec::with_capacity(column_count);
    for column_index in 0..column_count {
        let name_length = reader.usize_u32()?;
        let name_bytes = reader.take(name_length)?;
        let name =
            std::str::from_utf8(name_bytes).map_err(|_| RowsCodecError::InvalidColumnNameUtf8 {
                column: column_index,
            })?;
        if name.is_empty() {
            return Err(RowsCodecError::EmptyColumnName {
                column: column_index,
            });
        }
        if let Some(first) = names.insert(name.to_owned(), column_index) {
            return Err(RowsCodecError::DuplicateColumnName {
                first,
                duplicate: column_index,
            });
        }
        let type_form = reader.byte()?;
        let type_id = TypeId::from_bytes(reader.array::<16>()?);
        let nullable = reader.byte()?;
        if nullable > 1 {
            return Err(RowsCodecError::InvalidNullable {
                column: column_index,
                value: nullable,
            });
        }
        let resolved_type = rows_type_resolved(active, type_form, type_id, column_index)?;
        let column = ResultColumn::new(name, resolved_type, nullable == 1)
            .map_err(|source| RowsCodecError::ResultRows { source })?;
        columns.push(column);
    }

    let row_count = reader.usize_u32()?;
    if row_count > MAX_ROWS_ROWS {
        return Err(RowsCodecError::RowCountExceeded {
            actual: row_count,
            maximum: MAX_ROWS_ROWS,
        });
    }
    if row_count
        .checked_mul(column_count)
        .is_none_or(|cells| cells > MAX_ROWS_CELLS)
    {
        return Err(RowsCodecError::CellProductExceeded {
            rows: row_count,
            columns: column_count,
            maximum: MAX_ROWS_CELLS,
        });
    }

    let mut rows = Vec::with_capacity(row_count);
    for row_index in 0..row_count {
        let cell_count = reader.usize_u32()?;
        if cell_count != column_count {
            return Err(RowsCodecError::RowWidthMismatch {
                row: row_index,
                expected: column_count,
                actual: cell_count,
            });
        }
        let mut values = Vec::with_capacity(cell_count);
        for column_index in 0..cell_count {
            let length = reader.usize_u32()?;
            if length > MAX_ROWS_PAYLOAD_LENGTH {
                return Err(RowsCodecError::PayloadTooLarge {
                    actual: length,
                    maximum: MAX_ROWS_PAYLOAD_LENGTH,
                });
            }
            let cell = reader.take(length)?;
            if cell.get(..4) != Some(b"ORV5".as_slice()) {
                return Err(RowsCodecError::InvalidCellMarker {
                    row: row_index,
                    column: column_index,
                });
            }
            let value = decode_constructed_value(active, registry, cell).map_err(|source| {
                RowsCodecError::CellValue {
                    row: row_index,
                    column: column_index,
                    source,
                }
            })?;
            let canonical =
                encode_constructed_value(active, registry, &value).map_err(|source| {
                    RowsCodecError::CellValue {
                        row: row_index,
                        column: column_index,
                        source,
                    }
                })?;
            if canonical != cell {
                return Err(RowsCodecError::NonCanonicalCell {
                    row: row_index,
                    column: column_index,
                });
            }
            values.push(value);
        }
        rows.push(ResultRow::new(values));
    }
    reader.require_finished()?;

    let result =
        ResultRows::new(columns, rows).map_err(|source| RowsCodecError::ResultRows { source })?;
    OpaqueValue::new(active, registry, STD_DATA_ROWS_TYPE_ID, encoded)
        .map_err(|source| RowsCodecError::OpaqueValue { source })?;
    Ok(result)
}

fn validate_rows_shape_bounds(columns: usize, rows: usize) -> Result<(), RowsCodecError> {
    if !(1..=MAX_ROWS_COLUMNS).contains(&columns) {
        return Err(RowsCodecError::ColumnCountExceeded {
            actual: columns,
            maximum: MAX_ROWS_COLUMNS,
        });
    }
    if rows > MAX_ROWS_ROWS {
        return Err(RowsCodecError::RowCountExceeded {
            actual: rows,
            maximum: MAX_ROWS_ROWS,
        });
    }
    if rows
        .checked_mul(columns)
        .is_none_or(|cells| cells > MAX_ROWS_CELLS)
    {
        return Err(RowsCodecError::CellProductExceeded {
            rows,
            columns,
            maximum: MAX_ROWS_CELLS,
        });
    }
    Ok(())
}

fn rows_type_wire(
    active: &ActiveDatabaseRevision,
    resolved_type: ResolvedType,
    column: usize,
) -> Result<(u8, TypeId), RowsCodecError> {
    let (type_form, type_id) = match resolved_type {
        ResolvedType::Scalar(scalar) => (
            0x01,
            supported_scalar_type_id(scalar).ok_or(RowsCodecError::InactiveType {
                column,
                type_form: 0x01,
                type_id: TypeId::from_bytes([0; 16]),
            })?,
        ),
        ResolvedType::Named(type_id) => (0x02, type_id),
        ResolvedType::Reference { target } => (0x03, target),
        ResolvedType::Value(type_id) => (0x04, type_id),
    };
    validate_rows_declared_type(active, type_form, type_id, column)?;
    Ok((type_form, type_id))
}

fn rows_type_resolved(
    active: &ActiveDatabaseRevision,
    type_form: u8,
    type_id: TypeId,
    column: usize,
) -> Result<ResolvedType, RowsCodecError> {
    validate_rows_declared_type(active, type_form, type_id, column)?;
    match type_form {
        0x01 => supported_scalar_from_type_id(type_id)
            .map(ResolvedType::scalar)
            .ok_or(RowsCodecError::InactiveType {
                column,
                type_form,
                type_id,
            }),
        0x02 => Ok(ResolvedType::named(type_id)),
        0x03 => Ok(ResolvedType::reference(type_id)),
        0x04 => Ok(ResolvedType::value(type_id)),
        _ => Err(RowsCodecError::UnknownTypeForm { column, type_form }),
    }
}

fn validate_rows_declared_type(
    active: &ActiveDatabaseRevision,
    type_form: u8,
    type_id: TypeId,
    column: usize,
) -> Result<(), RowsCodecError> {
    match type_form {
        0x01 => {
            if supported_scalar_from_type_id(type_id).is_none() {
                return Err(RowsCodecError::InactiveType {
                    column,
                    type_form,
                    type_id,
                });
            }
        }
        0x02 => {
            let active_named = active.catalogue().enum_type_by_id(type_id).is_some()
                || active
                    .catalogue()
                    .record_value_type_by_id(type_id)
                    .is_some()
                || active
                    .catalogue_hash_context()
                    .standard()
                    .is_some_and(|standard| {
                        standard.catalogue().enum_type_by_id(type_id).is_some()
                            || standard
                                .catalogue()
                                .record_value_type_by_id(type_id)
                                .is_some()
                    });
            if !active_named {
                return Err(RowsCodecError::InactiveType {
                    column,
                    type_form,
                    type_id,
                });
            }
        }
        0x03 => {
            let active_reference = active.catalogue().object_type_by_id(type_id).is_some()
                || active
                    .catalogue_hash_context()
                    .standard()
                    .is_some_and(|standard| {
                        standard.catalogue().object_type_by_id(type_id).is_some()
                    });
            if !active_reference {
                return Err(RowsCodecError::InactiveType {
                    column,
                    type_form,
                    type_id,
                });
            }
        }
        0x04 => {
            let definition = active.catalogue().value_type_by_id(type_id).or_else(|| {
                active
                    .catalogue_hash_context()
                    .standard()
                    .and_then(|standard| standard.catalogue().value_type_by_id(type_id))
            });
            let Some(definition) = definition else {
                return Err(RowsCodecError::InactiveType {
                    column,
                    type_form,
                    type_id,
                });
            };
            if definition.kind() == ValueTypeKind::Opaque {
                return Err(RowsCodecError::OpaqueColumnType { column, type_id });
            }
        }
        _ => return Err(RowsCodecError::UnknownTypeForm { column, type_form }),
    }
    Ok(())
}

fn validate_rows_value(
    active: &ActiveDatabaseRevision,
    column: &ResultColumn,
    value: &RuntimeValue,
    row: usize,
    column_index: usize,
) -> Result<(), RowsCodecError> {
    if let RuntimeValue::Opaque(opaque) = value {
        return Err(RowsCodecError::ResultRows {
            source: ResultRowsError::OpaqueValueNotAccepted {
                row,
                column: column_index,
                opaque_type: opaque.opaque_type(),
            },
        });
    }
    if let Some(carrier) = orna_core::invocation::invocation_carrier_kind(value) {
        return Err(RowsCodecError::ResultRows {
            source: ResultRowsError::InvocationCarrierNotAccepted {
                row,
                column: column_index,
                carrier,
            },
        });
    }
    if let RuntimeValue::Constructed(constructed) = value {
        return Err(RowsCodecError::ResultRows {
            source: ResultRowsError::ConstructedValueNotAccepted {
                row,
                column: column_index,
                descriptor: constructed.descriptor().clone(),
            },
        });
    }
    if value.is_null() && !column.nullable() {
        return Err(RowsCodecError::ResultRows {
            source: ResultRowsError::NullInNonNullableColumn {
                row,
                column: column_index,
            },
        });
    }
    let RuntimeType::Flat(actual) = value.runtime_type() else {
        unreachable!("constructed values are rejected above");
    };
    if actual != column.resolved_type() {
        return Err(RowsCodecError::ResultRows {
            source: ResultRowsError::ValueTypeMismatch {
                row,
                column: column_index,
                expected: column.resolved_type(),
                actual,
            },
        });
    }
    let (type_form, type_id) = rows_type_wire(active, column.resolved_type(), column_index)?;
    match type_form {
        0x02 if active.catalogue().enum_type_by_id(type_id).is_none()
            && active
                .catalogue()
                .record_value_type_by_id(type_id)
                .is_none()
            && active
                .catalogue_hash_context()
                .standard()
                .is_none_or(|standard| {
                    standard.catalogue().enum_type_by_id(type_id).is_none()
                        && standard
                            .catalogue()
                            .record_value_type_by_id(type_id)
                            .is_none()
                }) =>
        {
            return Err(RowsCodecError::InactiveType {
                column: column_index,
                type_form,
                type_id,
            });
        }
        _ => {}
    }
    Ok(())
}

struct RowsWriter {
    bytes: Vec<u8>,
}

impl RowsWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), RowsCodecError> {
        let next =
            self.bytes
                .len()
                .checked_add(value.len())
                .ok_or(RowsCodecError::PayloadTooLarge {
                    actual: usize::MAX,
                    maximum: MAX_ROWS_PAYLOAD_LENGTH,
                })?;
        if next > MAX_ROWS_PAYLOAD_LENGTH {
            return Err(RowsCodecError::PayloadTooLarge {
                actual: next,
                maximum: MAX_ROWS_PAYLOAD_LENGTH,
            });
        }
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn byte(&mut self, value: u8) -> Result<(), RowsCodecError> {
        self.bytes(&[value])
    }

    fn u16(&mut self, value: u16) -> Result<(), RowsCodecError> {
        self.bytes(&value.to_be_bytes())
    }

    fn u32(&mut self, value: u32) -> Result<(), RowsCodecError> {
        self.bytes(&value.to_be_bytes())
    }
}

struct RowsReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> RowsReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], RowsCodecError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(RowsCodecError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(RowsCodecError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    fn array<const LENGTH: usize>(&mut self) -> Result<[u8; LENGTH], RowsCodecError> {
        self.take(LENGTH)?
            .try_into()
            .map_err(|_| RowsCodecError::Truncated)
    }

    fn byte(&mut self) -> Result<u8, RowsCodecError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, RowsCodecError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn usize_u32(&mut self) -> Result<usize, RowsCodecError> {
        usize::try_from(u32::from_be_bytes(self.array()?)).map_err(|_| RowsCodecError::Truncated)
    }

    fn require_finished(&self) -> Result<(), RowsCodecError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(RowsCodecError::TrailingBytes)
        }
    }
}

fn encode_orv5_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &RuntimeValue,
) -> Result<Vec<u8>, ValueCodecError> {
    if let Some(carrier) = invocation_carrier_type_id(value) {
        return encode_invocation_carrier(active, registry, value)
            .map_err(|source| ValueCodecError::InvocationCarrier { carrier, source });
    }
    let RuntimeValue::Constructed(constructed) = value else {
        return encode_registered_value_with_marker(active, registry, value, CONSTRUCTED_MARKER);
    };
    let descriptor = constructed.descriptor().clone();
    let mut descriptor_bytes = Vec::new();
    encode_constructed_descriptor(&descriptor, &mut descriptor_bytes)?;
    let descriptor_length = u16::try_from(descriptor_bytes.len()).map_err(|_| {
        ValueCodecError::InvalidConstructedDescriptor {
            source: TypeDescriptorError::TooLarge {
                maximum: u16::MAX as usize,
                actual: descriptor_bytes.len(),
            },
        }
    })?;
    let mut payload = Vec::new();
    payload.extend_from_slice(&descriptor_length.to_be_bytes());
    payload.extend_from_slice(&descriptor_bytes);

    let marker = match constructed.kind() {
        ConstructedValueKind::Option(value) => {
            RuntimeValue::option(active, descriptor.clone(), value.cloned())
                .map_err(|source| ValueCodecError::CollectionValue { source })?;
            match value {
                None => payload.push(0),
                Some(value) => {
                    payload.push(1);
                    append_orv5_child(active, registry, &mut payload, value)?;
                }
            }
            CONSTRUCTED_MARKER
        }
        ConstructedValueKind::List(values) => {
            RuntimeValue::list(active, descriptor.clone(), values.to_vec())
                .map_err(|source| ValueCodecError::CollectionValue { source })?;
            append_count(&mut payload, values.len())?;
            for child in values {
                append_orv5_child(active, registry, &mut payload, child)?;
            }
            CONSTRUCTED_MARKER
        }
        ConstructedValueKind::Set(values) => {
            RuntimeValue::set(active, descriptor.clone(), values.to_vec())
                .map_err(|source| ValueCodecError::CollectionValue { source })?;
            append_count(&mut payload, values.len())?;
            for child in values {
                append_orv5_child(active, registry, &mut payload, child)?;
            }
            SET_MARKER
        }
        ConstructedValueKind::Map(entries) => {
            RuntimeValue::map(active, descriptor.clone(), entries.to_vec())
                .map_err(|source| ValueCodecError::CollectionValue { source })?;
            append_count(&mut payload, entries.len())?;
            for (key, mapped) in entries {
                append_orv5_child(active, registry, &mut payload, key)?;
                append_orv5_child(active, registry, &mut payload, mapped)?;
            }
            CONSTRUCTED_MARKER
        }
        _ => return Err(ValueCodecError::UnsupportedValue),
    };
    require_payload_limit(payload.len())?;
    Ok(encode_with_marker(
        marker,
        CONSTRUCTED_TAG,
        TypeId::from_bytes([0; 16]),
        &payload,
    ))
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TextKey(Vec<u8>);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DescriptorKey(Vec<u8>);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SinkKey {
    descriptor: DescriptorKey,
    media_types: Vec<u8>,
    streaming: u8,
    preference_rank: [u8; 4],
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ContractKey {
    name: TextKey,
    version: TextKey,
    features: Vec<u8>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RuntimeKey {
    name: TextKey,
    version: TextKey,
    remaining: Vec<u8>,
}

struct CarrierWriter {
    bytes: Vec<u8>,
}

impl CarrierWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), InvocationCarrierCodecError> {
        let actual = self.bytes.len().checked_add(bytes.len()).ok_or(
            InvocationCarrierCodecError::PayloadTooLarge {
                actual: usize::MAX,
                maximum: PAYLOAD_LIMIT,
            },
        )?;
        if actual > PAYLOAD_LIMIT {
            return Err(InvocationCarrierCodecError::PayloadTooLarge {
                actual,
                maximum: PAYLOAD_LIMIT,
            });
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn u8(&mut self, value: u8) -> Result<(), InvocationCarrierCodecError> {
        self.append(&[value])
    }

    fn u16(&mut self, value: u16) -> Result<(), InvocationCarrierCodecError> {
        self.append(&value.to_be_bytes())
    }

    fn u32(&mut self, value: u32) -> Result<(), InvocationCarrierCodecError> {
        self.append(&value.to_be_bytes())
    }

    fn u64(&mut self, value: u64) -> Result<(), InvocationCarrierCodecError> {
        self.append(&value.to_be_bytes())
    }

    fn i32(&mut self, value: i32) -> Result<(), InvocationCarrierCodecError> {
        self.append(&value.to_be_bytes())
    }

    fn count(&mut self, count: usize) -> Result<(), InvocationCarrierCodecError> {
        let count =
            u32::try_from(count).map_err(|_| InvocationCarrierCodecError::PayloadTooLarge {
                actual: usize::MAX,
                maximum: PAYLOAD_LIMIT,
            })?;
        self.u32(count)
    }

    fn text(&mut self, value: &str) -> Result<(), InvocationCarrierCodecError> {
        self.length_prefixed(value.as_bytes())
    }

    fn length_prefixed(&mut self, value: &[u8]) -> Result<(), InvocationCarrierCodecError> {
        let length = u32::try_from(value.len()).map_err(|_| {
            InvocationCarrierCodecError::PayloadTooLarge {
                actual: value.len(),
                maximum: PAYLOAD_LIMIT,
            }
        })?;
        self.u32(length)?;
        self.append(value)
    }
}

fn encode_invocation_carrier(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &RuntimeValue,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let (carrier, payload) = match value {
        RuntimeValue::InvokeValue(value) => (
            SYS_INVOKE_VALUE_TYPE_ID,
            encode_invoke_value_payload(
                active,
                registry,
                value,
                InvocationCarrierPath::one(InvocationCarrierPathSegment::ValueInner),
            )?,
        ),
        RuntimeValue::InvokeRequest(request) => (
            SYS_INVOKE_REQUEST_TYPE_ID,
            encode_invoke_request_payload(active, registry, request)?,
        ),
        RuntimeValue::InvokeEvent(event) => (
            SYS_INVOKE_EVENT_TYPE_ID,
            encode_invoke_event_payload(active, registry, event)?,
        ),
        _ => unreachable!("carrier classification and runtime variant must agree"),
    };
    let validated = parse_invocation_carrier(carrier, &payload)?;
    validated.preflight(carrier)?;
    Ok(encode_with_marker(
        CONSTRUCTED_MARKER,
        OPAQUE_TAG,
        carrier,
        &payload,
    ))
}

fn encode_invoke_value_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &InvokeValue,
    path: InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    if let Some(carrier) = invocation_carrier_type_id(value.value()) {
        return Err(InvocationCarrierCodecError::NestedCarrier { path, carrier });
    }
    let encoded = encode_orv5_value(active, registry, value.value()).map_err(|source| {
        InvocationCarrierCodecError::InnerValue {
            path,
            source: Box::new(source),
        }
    })?;
    let mut writer = CarrierWriter::new();
    writer.u8(1)?;
    writer.length_prefixed(&encoded)?;
    Ok(writer.finish())
}

fn encode_embedded_invoke_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &InvokeValue,
    path: InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let payload = encode_invoke_value_payload(active, registry, value, path)?;
    Ok(encode_with_marker(
        CONSTRUCTED_MARKER,
        OPAQUE_TAG,
        SYS_INVOKE_VALUE_TYPE_ID,
        &payload,
    ))
}

fn append_embedded_invoke_value(
    writer: &mut CarrierWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: &InvokeValue,
    path: InvocationCarrierPath,
) -> Result<(), InvocationCarrierCodecError> {
    let encoded = encode_embedded_invoke_value(active, registry, value, path)?;
    writer.length_prefixed(&encoded)
}

fn append_optional_invoke_value(
    writer: &mut CarrierWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: Option<&InvokeValue>,
    path: InvocationCarrierPath,
) -> Result<(), InvocationCarrierCodecError> {
    match value {
        Some(value) => {
            writer.u8(1)?;
            append_embedded_invoke_value(writer, active, registry, value, path)
        }
        None => writer.u8(0),
    }
}

fn append_optional_text(
    writer: &mut CarrierWriter,
    value: Option<&str>,
) -> Result<(), InvocationCarrierCodecError> {
    match value {
        Some(value) => {
            writer.u8(1)?;
            writer.text(value)
        }
        None => writer.u8(0),
    }
}

fn append_optional_id<T>(
    writer: &mut CarrierWriter,
    value: Option<T>,
    bytes: impl FnOnce(T) -> [u8; 16],
) -> Result<(), InvocationCarrierCodecError> {
    match value {
        Some(value) => {
            writer.u8(1)?;
            writer.append(&bytes(value))
        }
        None => writer.u8(0),
    }
}

fn append_semantic_name(
    writer: &mut CarrierWriter,
    name: &QualifiedSemanticName,
) -> Result<(), InvocationCarrierCodecError> {
    writer.count(name.parts().len())?;
    for part in name.parts() {
        writer.text(part)?;
    }
    Ok(())
}

fn encoded_descriptor(
    descriptor: &TypeDescriptor,
    path: InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    reject_carrier_descriptor(descriptor, &path)?;
    let mut encoded = Vec::new();
    encode_constructed_descriptor(descriptor, &mut encoded)
        .map_err(|_| InvocationCarrierCodecError::InvalidField { path })?;
    Ok(encoded)
}

fn append_descriptor_bytes(
    writer: &mut CarrierWriter,
    encoded: &[u8],
) -> Result<(), InvocationCarrierCodecError> {
    let length =
        u16::try_from(encoded.len()).map_err(|_| InvocationCarrierCodecError::PayloadTooLarge {
            actual: encoded.len(),
            maximum: PAYLOAD_LIMIT,
        })?;
    writer.u16(length)?;
    writer.append(encoded)
}

fn reject_carrier_descriptor(
    descriptor: &TypeDescriptor,
    path: &InvocationCarrierPath,
) -> Result<(), InvocationCarrierCodecError> {
    match descriptor.kind() {
        TypeDescriptorKind::Named(type_id) | TypeDescriptorKind::Reference(type_id) => {
            if invocation_carrier_by_id(type_id).is_some() {
                Err(InvocationCarrierCodecError::NestedCarrier {
                    path: path.clone(),
                    carrier: type_id,
                })
            } else {
                Ok(())
            }
        }
        TypeDescriptorKind::List(child) | TypeDescriptorKind::Option(child) => {
            reject_carrier_descriptor(child, path)
        }
        TypeDescriptorKind::Map { key, value } => {
            reject_carrier_descriptor(key, path)?;
            reject_carrier_descriptor(value, path)
        }
        TypeDescriptorKind::Set(child) => {
            if !matches!(
                child.kind(),
                TypeDescriptorKind::Named(_) | TypeDescriptorKind::Reference(_)
            ) {
                return Err(InvocationCarrierCodecError::InvalidField { path: path.clone() });
            }
            reject_carrier_descriptor(child, path)
        }
        TypeDescriptorKind::Stream(_) => {
            Err(InvocationCarrierCodecError::InvalidField { path: path.clone() })
        }
    }
}

fn insert_canonical<K: Ord>(
    items: &mut BTreeMap<K, (usize, Vec<u8>)>,
    key: K,
    index: usize,
    encoded: Vec<u8>,
    path: &InvocationCarrierPath,
) -> Result<(), InvocationCarrierCodecError> {
    if let Some((first, _)) = items.get(&key) {
        return Err(InvocationCarrierCodecError::DuplicateItem {
            path: path.clone(),
            first: *first,
            duplicate: index,
        });
    }
    items.insert(key, (index, encoded));
    Ok(())
}

fn add_prepared_size(
    total: &mut usize,
    additional: usize,
) -> Result<(), InvocationCarrierCodecError> {
    let actual =
        total
            .checked_add(additional)
            .ok_or(InvocationCarrierCodecError::PayloadTooLarge {
                actual: usize::MAX,
                maximum: PAYLOAD_LIMIT,
            })?;
    if actual > PAYLOAD_LIMIT {
        return Err(InvocationCarrierCodecError::PayloadTooLarge {
            actual,
            maximum: PAYLOAD_LIMIT,
        });
    }
    *total = actual;
    Ok(())
}

fn canonical_text_list(
    values: &[String],
    path: &InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let mut canonical = BTreeMap::new();
    let mut prepared_size = 4;
    for (index, value) in values.iter().enumerate() {
        if value.is_empty() {
            return Err(InvocationCarrierCodecError::InvalidField { path: path.clone() });
        }
        let mut encoded = CarrierWriter::new();
        encoded.text(value)?;
        let encoded = encoded.finish();
        add_prepared_size(&mut prepared_size, encoded.len())?;
        insert_canonical(
            &mut canonical,
            TextKey(value.as_bytes().to_vec()),
            index,
            encoded,
            path,
        )?;
    }
    let mut writer = CarrierWriter::new();
    writer.count(canonical.len())?;
    for (_, encoded) in canonical.into_values() {
        writer.append(&encoded)?;
    }
    Ok(writer.finish())
}

fn canonical_descriptor_list(
    values: &[TypeDescriptor],
    path: &InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let mut canonical = BTreeMap::new();
    let mut prepared_size = 4;
    for (index, value) in values.iter().enumerate() {
        let item_path = path.with(InvocationCarrierPathSegment::ConsumedType(index));
        let descriptor = encoded_descriptor(value, item_path)?;
        let mut encoded = CarrierWriter::new();
        append_descriptor_bytes(&mut encoded, &descriptor)?;
        let encoded = encoded.finish();
        add_prepared_size(&mut prepared_size, encoded.len())?;
        insert_canonical(
            &mut canonical,
            DescriptorKey(descriptor),
            index,
            encoded,
            path,
        )?;
    }
    let mut writer = CarrierWriter::new();
    writer.count(canonical.len())?;
    for (_, encoded) in canonical.into_values() {
        writer.append(&encoded)?;
    }
    Ok(writer.finish())
}

fn canonical_contract_list(
    values: &[InvocationRuntimeContract],
    path: &InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let mut canonical = BTreeMap::new();
    let mut prepared_size = 4;
    for (index, value) in values.iter().enumerate() {
        let item_path = path.with(InvocationCarrierPathSegment::Contract(index));
        if value.name().is_empty() || value.version().is_empty() {
            return Err(InvocationCarrierCodecError::InvalidField { path: item_path });
        }
        let features_path = item_path.with(InvocationCarrierPathSegment::Features);
        let features = canonical_text_list(value.features(), &features_path)?;
        let mut encoded = CarrierWriter::new();
        encoded.text(value.name())?;
        encoded.text(value.version())?;
        encoded.append(&features)?;
        let encoded = encoded.finish();
        add_prepared_size(&mut prepared_size, encoded.len())?;
        insert_canonical(
            &mut canonical,
            ContractKey {
                name: TextKey(value.name().as_bytes().to_vec()),
                version: TextKey(value.version().as_bytes().to_vec()),
                features,
            },
            index,
            encoded,
            path,
        )?;
    }
    let mut writer = CarrierWriter::new();
    writer.count(canonical.len())?;
    for (_, encoded) in canonical.into_values() {
        writer.append(&encoded)?;
    }
    Ok(writer.finish())
}

fn canonical_sink_list(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    values: &[InvocationSinkOffer],
    path: &InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let mut canonical = BTreeMap::new();
    let mut prepared_size = 4;
    for (index, value) in values.iter().enumerate() {
        let item_path = path.with(InvocationCarrierPathSegment::Sink(index));
        let descriptor_path = item_path.with(InvocationCarrierPathSegment::Descriptor);
        let descriptor = encoded_descriptor(value.descriptor(), descriptor_path)?;
        let media_types_path = item_path.with(InvocationCarrierPathSegment::MediaTypes);
        let media_types = canonical_text_list(value.media_types(), &media_types_path)?;
        let mut encoded = CarrierWriter::new();
        append_descriptor_bytes(&mut encoded, &descriptor)?;
        encoded.append(&media_types)?;
        encoded.u8(u8::from(value.streaming()))?;
        encoded.i32(value.preference_rank())?;
        append_optional_invoke_value(
            &mut encoded,
            active,
            registry,
            value.limits(),
            item_path.with(InvocationCarrierPathSegment::Limits),
        )?;
        let encoded = encoded.finish();
        add_prepared_size(&mut prepared_size, encoded.len())?;
        insert_canonical(
            &mut canonical,
            SinkKey {
                descriptor: DescriptorKey(descriptor),
                media_types,
                streaming: u8::from(value.streaming()),
                preference_rank: value.preference_rank().to_be_bytes(),
            },
            index,
            encoded,
            path,
        )?;
    }
    let mut writer = CarrierWriter::new();
    writer.count(canonical.len())?;
    for (_, encoded) in canonical.into_values() {
        writer.append(&encoded)?;
    }
    Ok(writer.finish())
}

fn canonical_runtime_list(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    values: &[InvocationRuntimeOffer],
    path: &InvocationCarrierPath,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    let mut canonical = BTreeMap::new();
    let mut prepared_size = 4;
    for (index, value) in values.iter().enumerate() {
        let item_path = path.with(InvocationCarrierPathSegment::Runtime(index));
        if value.name().is_empty() || value.version().is_empty() {
            return Err(InvocationCarrierCodecError::InvalidField { path: item_path });
        }
        let consumed_path = item_path.with(InvocationCarrierPathSegment::ConsumedTypes);
        let consumed = canonical_descriptor_list(value.consumed_descriptors(), &consumed_path)?;
        let contracts_path = item_path.with(InvocationCarrierPathSegment::Contracts);
        let contracts = canonical_contract_list(value.contracts(), &contracts_path)?;
        let mut remaining = CarrierWriter::new();
        remaining.append(&consumed)?;
        remaining.append(&contracts)?;
        remaining.i32(value.preference_rank())?;
        remaining.u8(u8::from(value.trusted()))?;
        append_optional_invoke_value(
            &mut remaining,
            active,
            registry,
            value.limits(),
            item_path.with(InvocationCarrierPathSegment::Limits),
        )?;
        let remaining = remaining.finish();
        let mut encoded = CarrierWriter::new();
        encoded.text(value.name())?;
        encoded.text(value.version())?;
        encoded.append(&remaining)?;
        let encoded = encoded.finish();
        add_prepared_size(&mut prepared_size, encoded.len())?;
        insert_canonical(
            &mut canonical,
            RuntimeKey {
                name: TextKey(value.name().as_bytes().to_vec()),
                version: TextKey(value.version().as_bytes().to_vec()),
                remaining,
            },
            index,
            encoded,
            path,
        )?;
    }
    let mut writer = CarrierWriter::new();
    writer.count(canonical.len())?;
    for (_, encoded) in canonical.into_values() {
        writer.append(&encoded)?;
    }
    Ok(writer.finish())
}

fn encode_invoke_request_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    request: &InvokeRequest,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    if request.node_count() > MAX_INVOCATION_CARRIER_NODES {
        return Err(InvocationCarrierCodecError::TooManyNodes {
            maximum: MAX_INVOCATION_CARRIER_NODES,
        });
    }
    let mut writer = CarrierWriter::new();
    writer.u8(1)?;
    match request.target() {
        InvocationTarget::FunctionId(function) => {
            writer.u8(0)?;
            writer.append(&function.to_bytes())?;
        }
        InvocationTarget::QualifiedName(name) => {
            writer.u8(1)?;
            append_semantic_name(&mut writer, name)?;
        }
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTarget),
            });
        }
    }
    writer.count(request.arguments().len())?;
    for (index, argument) in request.arguments().iter().enumerate() {
        let argument_path =
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments)
                .with(InvocationCarrierPathSegment::Argument(index));
        match argument.selector() {
            InvocationParameterSelector::ParameterId(parameter) => {
                writer.u8(0)?;
                writer.append(&parameter.to_bytes())?;
            }
            InvocationParameterSelector::Name(name) => {
                if name.is_empty() {
                    return Err(InvocationCarrierCodecError::InvalidField {
                        path: argument_path.with(InvocationCarrierPathSegment::Selector),
                    });
                }
                writer.u8(1)?;
                writer.text(name)?;
            }
            _ => {
                return Err(InvocationCarrierCodecError::InvalidField {
                    path: argument_path.with(InvocationCarrierPathSegment::Selector),
                });
            }
        }
        append_embedded_invoke_value(
            &mut writer,
            active,
            registry,
            argument.value(),
            argument_path.with(InvocationCarrierPathSegment::Value),
        )?;
    }
    append_caller_context(&mut writer, active, registry, request.caller_context())?;
    append_client_offer(&mut writer, active, registry, request.client_offer())?;
    append_output_requirement(&mut writer, request.output_requirement())?;
    append_optional_text(&mut writer, request.state_profile())?;
    writer.u8(trace_policy_discriminant(request.trace_policy())?)?;
    writer.u8(0)?;
    match request.idempotency_key() {
        Some(key) => {
            if key.is_empty() {
                return Err(InvocationCarrierCodecError::InvalidField {
                    path: InvocationCarrierPath::one(
                        InvocationCarrierPathSegment::RequestIdempotencyKey,
                    ),
                });
            }
            writer.u8(1)?;
            writer.length_prefixed(key)?;
        }
        None => writer.u8(0)?,
    }
    append_optional_id(
        &mut writer,
        request.parent_invocation_id(),
        InvocationId::to_bytes,
    )?;
    append_optional_invoke_value(
        &mut writer,
        active,
        registry,
        request.observer_context(),
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestObserverContext),
    )?;
    Ok(writer.finish())
}

fn append_caller_context(
    writer: &mut CarrierWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    caller: &InvocationCallerContext,
) -> Result<(), InvocationCarrierCodecError> {
    writer.u8(caller_kind_discriminant(caller.kind())?)?;
    writer.u8(u8::from(caller.interactive()) | (u8::from(caller.stdout_is_tty()) << 1))?;
    append_optional_u32(writer, caller.terminal_columns())?;
    append_optional_u32(writer, caller.terminal_rows())?;
    writer.text(caller.locale())?;
    writer.text(caller.timezone())?;
    append_optional_invoke_value(
        writer,
        active,
        registry,
        caller.preference_policy(),
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller)
            .with(InvocationCarrierPathSegment::PreferencePolicy),
    )
}

fn append_optional_u32(
    writer: &mut CarrierWriter,
    value: Option<u32>,
) -> Result<(), InvocationCarrierCodecError> {
    match value {
        Some(value) => {
            writer.u8(1)?;
            writer.u32(value)
        }
        None => writer.u8(0),
    }
}

fn append_client_offer(
    writer: &mut CarrierWriter,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    offer: &InvocationClientOffer,
) -> Result<(), InvocationCarrierCodecError> {
    writer.u16(offer.protocol_major())?;
    writer.text(offer.locale())?;
    writer.text(offer.timezone())?;
    let sinks_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
        .with(InvocationCarrierPathSegment::ClientSinks);
    writer.append(&canonical_sink_list(
        active,
        registry,
        offer.sink_offers(),
        &sinks_path,
    )?)?;
    let runtimes_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
            .with(InvocationCarrierPathSegment::ClientRuntimes);
    writer.append(&canonical_runtime_list(
        active,
        registry,
        offer.runtime_offers(),
        &runtimes_path,
    )?)?;
    writer.u32(offer.maximum_frame_size())?;
    writer.u64(offer.maximum_artifact_size())?;
    append_optional_invoke_value(
        writer,
        active,
        registry,
        offer.limits(),
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
            .with(InvocationCarrierPathSegment::ClientLimits),
    )?;
    append_optional_invoke_value(
        writer,
        active,
        registry,
        offer.preferences(),
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer)
            .with(InvocationCarrierPathSegment::ClientPreferences),
    )
}

fn append_output_requirement(
    writer: &mut CarrierWriter,
    output: Option<&InvocationOutputRequirement>,
) -> Result<(), InvocationCarrierCodecError> {
    let Some(output) = output else {
        return writer.u8(0);
    };
    writer.u8(1)?;
    append_optional_text(writer, output.alias())?;
    append_optional_text(writer, output.media_type())?;
    match output.type_selector() {
        Some(InvocationOutputTypeSelector::TypeId(type_id)) => {
            writer.u8(1)?;
            writer.u8(0)?;
            writer.append(&type_id.to_bytes())?;
        }
        Some(InvocationOutputTypeSelector::QualifiedName(name)) => {
            writer.u8(1)?;
            writer.u8(1)?;
            append_semantic_name(writer, name)?;
        }
        Some(_) => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(
                    InvocationCarrierPathSegment::RequestOutputRequirement,
                )
                .with(InvocationCarrierPathSegment::OutputType),
            });
        }
        None => writer.u8(0)?,
    }
    writer.u8(streaming_requirement_discriminant(output.streaming())?)
}

fn caller_kind_discriminant(kind: InvocationCallerKind) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match kind {
        InvocationCallerKind::CliTty => 0,
        InvocationCallerKind::CliPipe => 1,
        InvocationCallerKind::DesktopLauncher => 2,
        InvocationCallerKind::Browser => 3,
        InvocationCallerKind::ClientFunction => 4,
        InvocationCallerKind::JsonRpcGateway => 5,
        InvocationCallerKind::McpGateway => 6,
        InvocationCallerKind::Scheduler => 7,
        InvocationCallerKind::TestRunner => 8,
        InvocationCallerKind::Recovery => 9,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller)
                    .with(InvocationCarrierPathSegment::CallerKind),
            });
        }
    })
}

fn streaming_requirement_discriminant(
    streaming: InvocationStreamingRequirement,
) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match streaming {
        InvocationStreamingRequirement::Unspecified => 0,
        InvocationStreamingRequirement::Required => 1,
        InvocationStreamingRequirement::Preferred => 2,
        InvocationStreamingRequirement::Forbidden => 3,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(
                    InvocationCarrierPathSegment::RequestOutputRequirement,
                )
                .with(InvocationCarrierPathSegment::OutputStreaming),
            });
        }
    })
}

fn trace_policy_discriminant(
    trace: InvocationTracePolicy,
) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match trace {
        InvocationTracePolicy::Off => 0,
        InvocationTracePolicy::Basic => 1,
        InvocationTracePolicy::Normal => 2,
        InvocationTracePolicy::Verbose => 3,
        InvocationTracePolicy::Profile => 4,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTracePolicy),
            });
        }
    })
}

fn event_kind_discriminant(body: &InvocationEventBody) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match body {
        InvocationEventBody::Started { .. } => 0,
        InvocationEventBody::ValueBatch { .. } => 1,
        InvocationEventBody::Diagnostic(_) => 2,
        InvocationEventBody::Completed { .. } => 3,
        InvocationEventBody::Failed(_) => 4,
        InvocationEventBody::Cancelled { .. } => 5,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody),
            });
        }
    })
}

fn encode_invoke_event_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    event: &InvokeEvent,
) -> Result<Vec<u8>, InvocationCarrierCodecError> {
    if event.node_count() > MAX_INVOCATION_CARRIER_NODES {
        return Err(InvocationCarrierCodecError::TooManyNodes {
            maximum: MAX_INVOCATION_CARRIER_NODES,
        });
    }
    let mut writer = CarrierWriter::new();
    writer.u8(1)?;
    writer.u8(event_kind_discriminant(event.body())?)?;
    writer.append(&event.invocation_id().to_bytes())?;
    writer.u64(event.sequence())?;
    match event.body() {
        InvocationEventBody::Started { visible_principal } => {
            append_optional_id(&mut writer, *visible_principal, PrincipalId::to_bytes)?;
        }
        InvocationEventBody::ValueBatch { schema, values } => {
            writer.u8(0)?;
            append_optional_invoke_value(
                &mut writer,
                active,
                registry,
                schema.as_ref(),
                InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                    .with(InvocationCarrierPathSegment::Schema),
            )?;
            if values.is_empty() {
                return Err(InvocationCarrierCodecError::InvalidField {
                    path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                        .with(InvocationCarrierPathSegment::BatchValues),
                });
            }
            writer.count(values.len())?;
            for (index, value) in values.iter().enumerate() {
                append_embedded_invoke_value(
                    &mut writer,
                    active,
                    registry,
                    value,
                    InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                        .with(InvocationCarrierPathSegment::BatchValues)
                        .with(InvocationCarrierPathSegment::BatchValue(index)),
                )?;
            }
        }
        InvocationEventBody::Diagnostic(diagnostic) => {
            writer.u8(match diagnostic.severity() {
                InvocationDiagnosticSeverity::Info => 0,
                InvocationDiagnosticSeverity::Warning => 1,
                InvocationDiagnosticSeverity::Error => 2,
                _ => {
                    return Err(InvocationCarrierCodecError::InvalidField {
                        path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                            .with(InvocationCarrierPathSegment::Severity),
                    });
                }
            })?;
            writer.text(diagnostic.code())?;
            writer.text(diagnostic.message())?;
        }
        InvocationEventBody::Completed {
            duration_nanoseconds,
        } => writer.u64(*duration_nanoseconds)?,
        InvocationEventBody::Failed(failure) => {
            writer.u8(failure_phase_discriminant(failure.phase())?)?;
            writer.text(failure.code())?;
            writer.text(failure.message())?;
            append_optional_invoke_value(
                &mut writer,
                active,
                registry,
                failure.details(),
                InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                    .with(InvocationCarrierPathSegment::Details),
            )?;
            writer.u8(retryability_discriminant(failure.retryability())?)?;
        }
        InvocationEventBody::Cancelled { reason } => {
            append_optional_text(&mut writer, reason.as_deref())?;
        }
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody),
            });
        }
    }
    Ok(writer.finish())
}

fn failure_phase_discriminant(
    phase: InvocationFailurePhase,
) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match phase {
        InvocationFailurePhase::Resolve => 0,
        InvocationFailurePhase::Bind => 1,
        InvocationFailurePhase::Authorise => 2,
        InvocationFailurePhase::Target => 3,
        InvocationFailurePhase::Present => 4,
        InvocationFailurePhase::Runtime => 5,
        InvocationFailurePhase::Transport => 6,
        InvocationFailurePhase::Internal => 7,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                    .with(InvocationCarrierPathSegment::Phase),
            });
        }
    })
}

fn retryability_discriminant(
    retryability: InvocationRetryability,
) -> Result<u8, InvocationCarrierCodecError> {
    Ok(match retryability {
        InvocationRetryability::Unknown => 0,
        InvocationRetryability::No => 1,
        InvocationRetryability::Yes => 2,
        _ => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody)
                    .with(InvocationCarrierPathSegment::Retryability),
            });
        }
    })
}

struct CarrierReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
    base: usize,
}

impl<'a> CarrierReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            cursor: 0,
            base: 0,
        }
    }

    fn with_base(bytes: &'a [u8], base: usize) -> Self {
        Self {
            bytes,
            cursor: 0,
            base,
        }
    }

    fn position(&self) -> usize {
        self.cursor
    }

    fn absolute_position(&self) -> usize {
        self.base.saturating_add(self.cursor)
    }

    fn slice(&self, start: usize, end: usize) -> &'a [u8] {
        &self.bytes[start..end]
    }

    fn take(&mut self, required: usize) -> Result<&'a [u8], InvocationCarrierCodecError> {
        let available = self.bytes.len().saturating_sub(self.cursor);
        if available < required {
            return Err(InvocationCarrierCodecError::Truncated {
                offset: self.absolute_position(),
                required,
                available,
            });
        }
        let end = self.cursor.checked_add(required).ok_or(
            InvocationCarrierCodecError::PayloadTooLarge {
                actual: usize::MAX,
                maximum: PAYLOAD_LIMIT,
            },
        )?;
        let value = &self.bytes[self.cursor..end];
        self.cursor = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, InvocationCarrierCodecError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, InvocationCarrierCodecError> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("reader returned two bytes"),
        ))
    }

    fn u32(&mut self) -> Result<u32, InvocationCarrierCodecError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .expect("reader returned four bytes"),
        ))
    }

    fn i32(&mut self) -> Result<i32, InvocationCarrierCodecError> {
        Ok(i32::from_be_bytes(
            self.take(4)?
                .try_into()
                .expect("reader returned four bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64, InvocationCarrierCodecError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .expect("reader returned eight bytes"),
        ))
    }

    fn id(&mut self) -> Result<[u8; 16], InvocationCarrierCodecError> {
        Ok(self
            .take(16)?
            .try_into()
            .expect("reader returned sixteen bytes"))
    }

    fn length_prefixed(&mut self) -> Result<CarrierSpan<'a>, InvocationCarrierCodecError> {
        let length = self.u32()? as usize;
        let offset = self.absolute_position();
        let bytes = self.take(length)?;
        Ok(CarrierSpan { bytes, offset })
    }

    fn text(
        &mut self,
        path: InvocationCarrierPath,
    ) -> Result<&'a str, InvocationCarrierCodecError> {
        let span = self.length_prefixed()?;
        std::str::from_utf8(span.bytes)
            .map_err(|_| InvocationCarrierCodecError::InvalidText { path })
    }

    fn required_text(
        &mut self,
        path: InvocationCarrierPath,
    ) -> Result<&'a str, InvocationCarrierCodecError> {
        let value = self.text(path.clone())?;
        if value.is_empty() {
            Err(InvocationCarrierCodecError::InvalidField { path })
        } else {
            Ok(value)
        }
    }

    fn presence(
        &mut self,
        path: InvocationCarrierPath,
    ) -> Result<bool, InvocationCarrierCodecError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            actual => Err(InvocationCarrierCodecError::InvalidBoolean { path, actual }),
        }
    }

    fn boolean(
        &mut self,
        path: InvocationCarrierPath,
    ) -> Result<bool, InvocationCarrierCodecError> {
        self.presence(path)
    }

    fn semantic_name(
        &mut self,
        path: InvocationCarrierPath,
    ) -> Result<Vec<&'a str>, InvocationCarrierCodecError> {
        let count = self.u32()? as usize;
        if count < 2 {
            return Err(InvocationCarrierCodecError::InvalidSemanticName { path });
        }
        let mut parts = Vec::new();
        for _ in 0..count {
            let part = self.text(path.clone())?;
            if part.is_empty() {
                return Err(InvocationCarrierCodecError::InvalidSemanticName { path });
            }
            parts.push(part);
        }
        Ok(parts)
    }

    fn finish(self) -> Result<(), InvocationCarrierCodecError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(InvocationCarrierCodecError::Trailing {
                remaining: self.bytes.len() - self.cursor,
            })
        }
    }
}

#[derive(Clone, Copy)]
struct CarrierSpan<'a> {
    bytes: &'a [u8],
    offset: usize,
}

struct ValidatedInvokeValueWire<'a> {
    inner: CarrierSpan<'a>,
    path: InvocationCarrierPath,
}

enum ValidatedInvocationTargetWire<'a> {
    FunctionId(FunctionId),
    QualifiedName(Vec<&'a str>),
}

enum ValidatedParameterSelectorWire<'a> {
    ParameterId(ParameterId),
    Name(&'a str),
}

struct ValidatedArgumentWire<'a> {
    selector: ValidatedParameterSelectorWire<'a>,
    value: ValidatedInvokeValueWire<'a>,
}

struct ValidatedCallerWire<'a> {
    kind: InvocationCallerKind,
    interactive: bool,
    stdout_is_tty: bool,
    terminal_columns: Option<u32>,
    terminal_rows: Option<u32>,
    locale: &'a str,
    timezone: &'a str,
    preference_policy: Option<ValidatedInvokeValueWire<'a>>,
}

struct ValidatedSinkWire<'a> {
    descriptor: TypeDescriptor,
    media_types: Vec<&'a str>,
    streaming: bool,
    preference_rank: i32,
    limits: Option<ValidatedInvokeValueWire<'a>>,
}

struct ValidatedContractWire<'a> {
    name: &'a str,
    version: &'a str,
    features: Vec<&'a str>,
}

struct ValidatedRuntimeWire<'a> {
    name: &'a str,
    version: &'a str,
    consumed_descriptors: Vec<TypeDescriptor>,
    contracts: Vec<ValidatedContractWire<'a>>,
    preference_rank: i32,
    trusted: bool,
    limits: Option<ValidatedInvokeValueWire<'a>>,
}

struct ValidatedClientOfferWire<'a> {
    protocol_major: u16,
    locale: &'a str,
    timezone: &'a str,
    sinks: Vec<ValidatedSinkWire<'a>>,
    runtimes: Vec<ValidatedRuntimeWire<'a>>,
    maximum_frame_size: u32,
    maximum_artifact_size: u64,
    limits: Option<ValidatedInvokeValueWire<'a>>,
    preferences: Option<ValidatedInvokeValueWire<'a>>,
}

enum ValidatedOutputTypeWire<'a> {
    TypeId(TypeId),
    QualifiedName(Vec<&'a str>),
}

struct ValidatedOutputWire<'a> {
    alias: Option<&'a str>,
    media_type: Option<&'a str>,
    type_selector: Option<ValidatedOutputTypeWire<'a>>,
    streaming: InvocationStreamingRequirement,
}

struct ValidatedRequestWire<'a> {
    target: ValidatedInvocationTargetWire<'a>,
    arguments: Vec<ValidatedArgumentWire<'a>>,
    caller: ValidatedCallerWire<'a>,
    client_offer: ValidatedClientOfferWire<'a>,
    output: Option<ValidatedOutputWire<'a>>,
    state_profile: Option<&'a str>,
    trace_policy: InvocationTracePolicy,
    idempotency_key: Option<&'a [u8]>,
    parent_invocation: Option<InvocationId>,
    observer_context: Option<ValidatedInvokeValueWire<'a>>,
}

enum ValidatedEventBodyWire<'a> {
    Started {
        visible_principal: Option<PrincipalId>,
    },
    ValueBatch {
        schema: Option<ValidatedInvokeValueWire<'a>>,
        values: Vec<ValidatedInvokeValueWire<'a>>,
    },
    Diagnostic {
        severity: InvocationDiagnosticSeverity,
        code: &'a str,
        message: &'a str,
    },
    Completed {
        duration_nanoseconds: u64,
    },
    Failed {
        phase: InvocationFailurePhase,
        code: &'a str,
        message: &'a str,
        details: Option<ValidatedInvokeValueWire<'a>>,
        retryability: InvocationRetryability,
    },
    Cancelled {
        reason: Option<&'a str>,
    },
}

struct ValidatedEventWire<'a> {
    invocation_id: InvocationId,
    sequence: u64,
    body: ValidatedEventBodyWire<'a>,
}

enum ValidatedCarrierWire<'a> {
    Value(ValidatedInvokeValueWire<'a>),
    Request(Box<ValidatedRequestWire<'a>>),
    Event(ValidatedEventWire<'a>),
}

fn parse_invocation_carrier<'a>(
    carrier: TypeId,
    payload: &'a [u8],
) -> Result<ValidatedCarrierWire<'a>, InvocationCarrierCodecError> {
    let mut reader = CarrierReader::new(payload);
    let version = reader.u8()?;
    if version != 1 {
        return Err(InvocationCarrierCodecError::UnsupportedVersion { actual: version });
    }
    let validated = if carrier == SYS_INVOKE_VALUE_TYPE_ID {
        ValidatedCarrierWire::Value(parse_invoke_value_wire(
            &mut reader,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::ValueInner),
        )?)
    } else if carrier == SYS_INVOKE_REQUEST_TYPE_ID {
        ValidatedCarrierWire::Request(Box::new(parse_request_wire(&mut reader)?))
    } else if carrier == SYS_INVOKE_EVENT_TYPE_ID {
        ValidatedCarrierWire::Event(parse_event_wire(&mut reader)?)
    } else {
        unreachable!("only registry carrier identities reach the carrier parser")
    };
    reader.finish()?;
    Ok(validated)
}

fn parse_invoke_value_wire<'a>(
    reader: &mut CarrierReader<'a>,
    path: InvocationCarrierPath,
) -> Result<ValidatedInvokeValueWire<'a>, InvocationCarrierCodecError> {
    Ok(ValidatedInvokeValueWire {
        inner: reader.length_prefixed()?,
        path,
    })
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SelectorKey(Vec<u8>);

fn require_increasing<K: Ord>(
    previous: &mut Option<(K, usize)>,
    key: K,
    index: usize,
    path: &InvocationCarrierPath,
) -> Result<(), InvocationCarrierCodecError> {
    if let Some((previous_key, previous_index)) = previous {
        match (*previous_key).cmp(&key) {
            Ordering::Less => {}
            Ordering::Equal => {
                return Err(InvocationCarrierCodecError::DuplicateItem {
                    path: path.clone(),
                    first: *previous_index,
                    duplicate: index,
                });
            }
            Ordering::Greater => {
                return Err(InvocationCarrierCodecError::NonCanonicalOrder {
                    path: path.clone(),
                    index,
                });
            }
        }
    }
    *previous = Some((key, index));
    Ok(())
}

fn parse_request_wire<'a>(
    reader: &mut CarrierReader<'a>,
) -> Result<ValidatedRequestWire<'a>, InvocationCarrierCodecError> {
    let target_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTarget);
    let target = match reader.u8()? {
        0 => ValidatedInvocationTargetWire::FunctionId(FunctionId::from_bytes(reader.id()?)),
        1 => {
            ValidatedInvocationTargetWire::QualifiedName(reader.semantic_name(target_path.clone())?)
        }
        actual => {
            return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                path: target_path,
                actual,
            });
        }
    };

    let arguments_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments);
    let argument_count = reader.u32()? as usize;
    let mut arguments = Vec::new();
    let mut previous_selector = None;
    for index in 0..argument_count {
        let argument_path = arguments_path.with(InvocationCarrierPathSegment::Argument(index));
        let selector_path = argument_path.with(InvocationCarrierPathSegment::Selector);
        let (selector, key) = match reader.u8()? {
            0 => {
                let bytes = reader.id()?;
                let mut key = vec![0];
                key.extend_from_slice(&bytes);
                (
                    ValidatedParameterSelectorWire::ParameterId(ParameterId::from_bytes(bytes)),
                    SelectorKey(key),
                )
            }
            1 => {
                let name = reader.required_text(selector_path.clone())?;
                let mut key = vec![1];
                key.extend_from_slice(name.as_bytes());
                (ValidatedParameterSelectorWire::Name(name), SelectorKey(key))
            }
            actual => {
                return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                    path: selector_path,
                    actual,
                });
            }
        };
        require_increasing(&mut previous_selector, key, index, &arguments_path)?;
        let value = parse_embedded_invoke_value(
            reader,
            argument_path.with(InvocationCarrierPathSegment::Value),
        )?;
        arguments.push(ValidatedArgumentWire { selector, value });
    }

    let caller = parse_caller_wire(reader)?;
    let client_offer = parse_client_offer_wire(reader)?;
    let output = parse_output_wire(reader)?;
    let state_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestStateProfile);
    let state_profile = if reader.presence(state_path.clone())? {
        Some(reader.required_text(state_path)?)
    } else {
        None
    };
    let trace_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTracePolicy);
    let trace_policy = match reader.u8()? {
        0 => InvocationTracePolicy::Off,
        1 => InvocationTracePolicy::Basic,
        2 => InvocationTracePolicy::Normal,
        3 => InvocationTracePolicy::Verbose,
        4 => InvocationTracePolicy::Profile,
        actual => {
            return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                path: trace_path,
                actual,
            });
        }
    };
    let deadline_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestDeadline);
    match reader.u8()? {
        0 => {}
        1 => {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: deadline_path,
            });
        }
        actual => {
            return Err(InvocationCarrierCodecError::InvalidBoolean {
                path: deadline_path,
                actual,
            });
        }
    }
    let idempotency_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestIdempotencyKey);
    let idempotency_key = if reader.presence(idempotency_path.clone())? {
        let span = reader.length_prefixed()?;
        if span.bytes.is_empty() {
            return Err(InvocationCarrierCodecError::InvalidField {
                path: idempotency_path,
            });
        }
        Some(span.bytes)
    } else {
        None
    };
    let parent_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestParentInvocation);
    let parent_invocation = if reader.presence(parent_path)? {
        Some(InvocationId::from_bytes(reader.id()?))
    } else {
        None
    };
    let observer_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestObserverContext);
    let observer_context = if reader.presence(observer_path.clone())? {
        Some(parse_embedded_invoke_value(reader, observer_path)?)
    } else {
        None
    };
    Ok(ValidatedRequestWire {
        target,
        arguments,
        caller,
        client_offer,
        output,
        state_profile,
        trace_policy,
        idempotency_key,
        parent_invocation,
        observer_context,
    })
}

fn parse_optional_u32(
    reader: &mut CarrierReader<'_>,
    path: InvocationCarrierPath,
) -> Result<Option<u32>, InvocationCarrierCodecError> {
    if reader.presence(path.clone())? {
        let value = reader.u32()?;
        if value == 0 {
            return Err(InvocationCarrierCodecError::InvalidField { path });
        }
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

fn parse_caller_wire<'a>(
    reader: &mut CarrierReader<'a>,
) -> Result<ValidatedCallerWire<'a>, InvocationCarrierCodecError> {
    let caller_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller);
    let kind_path = caller_path.with(InvocationCarrierPathSegment::CallerKind);
    let kind = match reader.u8()? {
        0 => InvocationCallerKind::CliTty,
        1 => InvocationCallerKind::CliPipe,
        2 => InvocationCallerKind::DesktopLauncher,
        3 => InvocationCallerKind::Browser,
        4 => InvocationCallerKind::ClientFunction,
        5 => InvocationCallerKind::JsonRpcGateway,
        6 => InvocationCallerKind::McpGateway,
        7 => InvocationCallerKind::Scheduler,
        8 => InvocationCallerKind::TestRunner,
        9 => InvocationCallerKind::Recovery,
        actual => {
            return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                path: kind_path,
                actual,
            });
        }
    };
    let flags_path = caller_path.with(InvocationCarrierPathSegment::CallerFlags);
    let flags = reader.u8()?;
    if flags & !0b11 != 0 {
        return Err(InvocationCarrierCodecError::InvalidField { path: flags_path });
    }
    let interactive = flags & 1 != 0;
    let stdout_is_tty = flags & 2 != 0;
    let terminal_columns = parse_optional_u32(
        reader,
        caller_path.with(InvocationCarrierPathSegment::TerminalColumns),
    )?;
    let terminal_rows = parse_optional_u32(
        reader,
        caller_path.with(InvocationCarrierPathSegment::TerminalRows),
    )?;
    if (kind == InvocationCallerKind::CliTty
        && (!interactive
            || !stdout_is_tty
            || terminal_columns.is_none()
            || terminal_rows.is_none()))
        || (kind == InvocationCallerKind::CliPipe && (interactive || stdout_is_tty))
    {
        return Err(InvocationCarrierCodecError::InvalidField { path: caller_path });
    }
    let locale = reader.required_text(caller_path.with(InvocationCarrierPathSegment::Locale))?;
    let timezone =
        reader.required_text(caller_path.with(InvocationCarrierPathSegment::Timezone))?;
    let policy_path = caller_path.with(InvocationCarrierPathSegment::PreferencePolicy);
    let preference_policy = if reader.presence(policy_path.clone())? {
        Some(parse_embedded_invoke_value(reader, policy_path)?)
    } else {
        None
    };
    Ok(ValidatedCallerWire {
        kind,
        interactive,
        stdout_is_tty,
        terminal_columns,
        terminal_rows,
        locale,
        timezone,
        preference_policy,
    })
}

fn parse_embedded_invoke_value<'a>(
    reader: &mut CarrierReader<'a>,
    path: InvocationCarrierPath,
) -> Result<ValidatedInvokeValueWire<'a>, InvocationCarrierCodecError> {
    let envelope = reader.length_prefixed()?;
    let (tag, type_id, payload) = decode_envelope(envelope.bytes, CONSTRUCTED_MARKER)
        .map_err(|source| map_embedded_envelope_error(source, envelope, path.clone()))?;
    if type_id != SYS_INVOKE_VALUE_TYPE_ID || tag != OPAQUE_TAG {
        if invocation_carrier_by_id(type_id).is_some() {
            return Err(InvocationCarrierCodecError::NestedCarrier {
                path,
                carrier: type_id,
            });
        }
        return Err(InvocationCarrierCodecError::InvalidField { path });
    }
    let payload_offset = envelope.offset.saturating_add(HEADER_LENGTH);
    let mut nested = CarrierReader::with_base(payload, payload_offset);
    let version = nested.u8()?;
    if version != 1 {
        return Err(InvocationCarrierCodecError::UnsupportedVersion { actual: version });
    }
    let value = parse_invoke_value_wire(&mut nested, path)?;
    nested.finish()?;
    Ok(value)
}

fn map_embedded_envelope_error(
    source: ValueCodecError,
    span: CarrierSpan<'_>,
    path: InvocationCarrierPath,
) -> InvocationCarrierCodecError {
    match source {
        ValueCodecError::TruncatedHeader { actual } => InvocationCarrierCodecError::Truncated {
            offset: span.offset,
            required: HEADER_LENGTH,
            available: actual,
        },
        ValueCodecError::TruncatedPayload { declared, actual } => {
            InvocationCarrierCodecError::Truncated {
                offset: span.offset.saturating_add(HEADER_LENGTH),
                required: declared,
                available: actual,
            }
        }
        ValueCodecError::TrailingBytes { declared, actual } => {
            InvocationCarrierCodecError::Trailing {
                remaining: actual - declared,
            }
        }
        ValueCodecError::PayloadTooLarge { actual, maximum } => {
            InvocationCarrierCodecError::PayloadTooLarge { actual, maximum }
        }
        _ => InvocationCarrierCodecError::InvalidField { path },
    }
}

fn parse_descriptor(
    reader: &mut CarrierReader<'_>,
    path: InvocationCarrierPath,
) -> Result<(TypeDescriptor, DescriptorKey), InvocationCarrierCodecError> {
    let length = reader.u16()? as usize;
    if length == 0 {
        return Err(InvocationCarrierCodecError::InvalidField { path });
    }
    let offset = reader.absolute_position();
    let bytes = reader.take(length)?;
    let (descriptor, consumed) =
        parse_constructed_descriptor(bytes, 0, true).map_err(|source| match source {
            ValueCodecError::TruncatedConstructedDescriptorNode {
                offset: inner,
                required,
                available,
            } => InvocationCarrierCodecError::Truncated {
                offset: offset.saturating_add(inner),
                required,
                available,
            },
            ValueCodecError::UnknownConstructedDescriptorTag { tag } => {
                InvocationCarrierCodecError::UnknownDiscriminant {
                    path: path.clone(),
                    actual: tag,
                }
            }
            _ => InvocationCarrierCodecError::InvalidField { path: path.clone() },
        })?;
    if consumed != bytes.len() {
        return Err(InvocationCarrierCodecError::InvalidField { path });
    }
    reject_carrier_descriptor(&descriptor, &path)?;
    Ok((descriptor, DescriptorKey(bytes.to_vec())))
}

enum CanonicalTextItem {
    MediaType,
    Feature,
}

fn parse_canonical_text_list<'a>(
    reader: &mut CarrierReader<'a>,
    path: &InvocationCarrierPath,
    item: CanonicalTextItem,
) -> Result<(Vec<&'a str>, Vec<u8>), InvocationCarrierCodecError> {
    let start = reader.position();
    let count = reader.u32()? as usize;
    let mut values = Vec::new();
    let mut previous = None;
    for index in 0..count {
        let item_path = path.with(match item {
            CanonicalTextItem::MediaType => InvocationCarrierPathSegment::MediaType(index),
            CanonicalTextItem::Feature => InvocationCarrierPathSegment::Feature(index),
        });
        let value = reader.required_text(item_path)?;
        require_increasing(
            &mut previous,
            TextKey(value.as_bytes().to_vec()),
            index,
            path,
        )?;
        values.push(value);
    }
    let encoded = reader.slice(start, reader.position()).to_vec();
    Ok((values, encoded))
}

fn parse_descriptor_list(
    reader: &mut CarrierReader<'_>,
    path: &InvocationCarrierPath,
) -> Result<Vec<TypeDescriptor>, InvocationCarrierCodecError> {
    let count = reader.u32()? as usize;
    let mut values = Vec::new();
    let mut previous = None;
    for index in 0..count {
        let item_path = path.with(InvocationCarrierPathSegment::ConsumedType(index));
        let (descriptor, key) = parse_descriptor(reader, item_path)?;
        require_increasing(&mut previous, key, index, path)?;
        values.push(descriptor);
    }
    Ok(values)
}

fn parse_contract_list<'a>(
    reader: &mut CarrierReader<'a>,
    path: &InvocationCarrierPath,
) -> Result<Vec<ValidatedContractWire<'a>>, InvocationCarrierCodecError> {
    let count = reader.u32()? as usize;
    let mut values = Vec::new();
    let mut previous = None;
    for index in 0..count {
        let item_path = path.with(InvocationCarrierPathSegment::Contract(index));
        let name =
            reader.required_text(item_path.with(InvocationCarrierPathSegment::ContractName))?;
        let version =
            reader.required_text(item_path.with(InvocationCarrierPathSegment::ContractVersion))?;
        let features_path = item_path.with(InvocationCarrierPathSegment::Features);
        let (features, feature_bytes) =
            parse_canonical_text_list(reader, &features_path, CanonicalTextItem::Feature)?;
        let key = ContractKey {
            name: TextKey(name.as_bytes().to_vec()),
            version: TextKey(version.as_bytes().to_vec()),
            features: feature_bytes,
        };
        require_increasing(&mut previous, key, index, path)?;
        values.push(ValidatedContractWire {
            name,
            version,
            features,
        });
    }
    Ok(values)
}

fn parse_sink_list<'a>(
    reader: &mut CarrierReader<'a>,
    path: &InvocationCarrierPath,
) -> Result<Vec<ValidatedSinkWire<'a>>, InvocationCarrierCodecError> {
    let count = reader.u32()? as usize;
    let mut values = Vec::new();
    let mut previous = None;
    for index in 0..count {
        let item_path = path.with(InvocationCarrierPathSegment::Sink(index));
        let (descriptor, descriptor_key) = parse_descriptor(
            reader,
            item_path.with(InvocationCarrierPathSegment::Descriptor),
        )?;
        let media_path = item_path.with(InvocationCarrierPathSegment::MediaTypes);
        let (media_types, media_bytes) =
            parse_canonical_text_list(reader, &media_path, CanonicalTextItem::MediaType)?;
        let streaming = reader.boolean(item_path.with(InvocationCarrierPathSegment::Streaming))?;
        let preference_rank = reader.i32()?;
        let limits_path = item_path.with(InvocationCarrierPathSegment::Limits);
        let limits = if reader.presence(limits_path.clone())? {
            Some(parse_embedded_invoke_value(reader, limits_path)?)
        } else {
            None
        };
        let key = SinkKey {
            descriptor: descriptor_key,
            media_types: media_bytes,
            streaming: u8::from(streaming),
            preference_rank: preference_rank.to_be_bytes(),
        };
        require_increasing(&mut previous, key, index, path)?;
        values.push(ValidatedSinkWire {
            descriptor,
            media_types,
            streaming,
            preference_rank,
            limits,
        });
    }
    Ok(values)
}

fn parse_runtime_list<'a>(
    reader: &mut CarrierReader<'a>,
    path: &InvocationCarrierPath,
) -> Result<Vec<ValidatedRuntimeWire<'a>>, InvocationCarrierCodecError> {
    let count = reader.u32()? as usize;
    let mut values = Vec::new();
    let mut previous = None;
    for index in 0..count {
        let item_path = path.with(InvocationCarrierPathSegment::Runtime(index));
        let name =
            reader.required_text(item_path.with(InvocationCarrierPathSegment::RuntimeName))?;
        let version =
            reader.required_text(item_path.with(InvocationCarrierPathSegment::RuntimeVersion))?;
        let remaining_start = reader.position();
        let consumed_path = item_path.with(InvocationCarrierPathSegment::ConsumedTypes);
        let consumed_descriptors = parse_descriptor_list(reader, &consumed_path)?;
        let contracts_path = item_path.with(InvocationCarrierPathSegment::Contracts);
        let contracts = parse_contract_list(reader, &contracts_path)?;
        let preference_rank = reader.i32()?;
        let trusted = reader.boolean(item_path.with(InvocationCarrierPathSegment::Trusted))?;
        let limits_path = item_path.with(InvocationCarrierPathSegment::Limits);
        let limits = if reader.presence(limits_path.clone())? {
            Some(parse_embedded_invoke_value(reader, limits_path)?)
        } else {
            None
        };
        let remaining = reader.slice(remaining_start, reader.position()).to_vec();
        let key = RuntimeKey {
            name: TextKey(name.as_bytes().to_vec()),
            version: TextKey(version.as_bytes().to_vec()),
            remaining,
        };
        require_increasing(&mut previous, key, index, path)?;
        values.push(ValidatedRuntimeWire {
            name,
            version,
            consumed_descriptors,
            contracts,
            preference_rank,
            trusted,
            limits,
        });
    }
    Ok(values)
}

fn parse_client_offer_wire<'a>(
    reader: &mut CarrierReader<'a>,
) -> Result<ValidatedClientOfferWire<'a>, InvocationCarrierCodecError> {
    let offer_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer);
    let protocol_major = reader.u16()?;
    if protocol_major != 5 {
        return Err(InvocationCarrierCodecError::InvalidField {
            path: offer_path.with(InvocationCarrierPathSegment::ClientProtocol),
        });
    }
    let locale =
        reader.required_text(offer_path.with(InvocationCarrierPathSegment::ClientLocale))?;
    let timezone =
        reader.required_text(offer_path.with(InvocationCarrierPathSegment::ClientTimezone))?;
    let sinks_path = offer_path.with(InvocationCarrierPathSegment::ClientSinks);
    let sinks = parse_sink_list(reader, &sinks_path)?;
    let runtimes_path = offer_path.with(InvocationCarrierPathSegment::ClientRuntimes);
    let runtimes = parse_runtime_list(reader, &runtimes_path)?;
    let maximum_frame_size = reader.u32()?;
    if maximum_frame_size < 1_024 {
        return Err(InvocationCarrierCodecError::InvalidField {
            path: offer_path.with(InvocationCarrierPathSegment::ClientMaximumFrameSize),
        });
    }
    let maximum_artifact_size = reader.u64()?;
    let limits_path = offer_path.with(InvocationCarrierPathSegment::ClientLimits);
    let limits = if reader.presence(limits_path.clone())? {
        Some(parse_embedded_invoke_value(reader, limits_path)?)
    } else {
        None
    };
    let preferences_path = offer_path.with(InvocationCarrierPathSegment::ClientPreferences);
    let preferences = if reader.presence(preferences_path.clone())? {
        Some(parse_embedded_invoke_value(reader, preferences_path)?)
    } else {
        None
    };
    Ok(ValidatedClientOfferWire {
        protocol_major,
        locale,
        timezone,
        sinks,
        runtimes,
        maximum_frame_size,
        maximum_artifact_size,
        limits,
        preferences,
    })
}

fn parse_optional_text<'a>(
    reader: &mut CarrierReader<'a>,
    path: InvocationCarrierPath,
) -> Result<Option<&'a str>, InvocationCarrierCodecError> {
    if reader.presence(path.clone())? {
        Ok(Some(reader.required_text(path)?))
    } else {
        Ok(None)
    }
}

fn parse_output_wire<'a>(
    reader: &mut CarrierReader<'a>,
) -> Result<Option<ValidatedOutputWire<'a>>, InvocationCarrierCodecError> {
    let output_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestOutputRequirement);
    if !reader.presence(output_path.clone())? {
        return Ok(None);
    }
    let alias = parse_optional_text(
        reader,
        output_path.with(InvocationCarrierPathSegment::OutputAlias),
    )?;
    let media_type = parse_optional_text(
        reader,
        output_path.with(InvocationCarrierPathSegment::OutputMediaType),
    )?;
    let type_path = output_path.with(InvocationCarrierPathSegment::OutputType);
    let type_selector = if reader.presence(type_path.clone())? {
        Some(match reader.u8()? {
            0 => ValidatedOutputTypeWire::TypeId(TypeId::from_bytes(reader.id()?)),
            1 => ValidatedOutputTypeWire::QualifiedName(reader.semantic_name(type_path.clone())?),
            actual => {
                return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                    path: type_path,
                    actual,
                });
            }
        })
    } else {
        None
    };
    if alias.is_none() && media_type.is_none() && type_selector.is_none() {
        return Err(InvocationCarrierCodecError::InvalidField { path: output_path });
    }
    let streaming_path = output_path.with(InvocationCarrierPathSegment::OutputStreaming);
    let streaming = match reader.u8()? {
        0 => InvocationStreamingRequirement::Unspecified,
        1 => InvocationStreamingRequirement::Required,
        2 => InvocationStreamingRequirement::Preferred,
        3 => InvocationStreamingRequirement::Forbidden,
        actual => {
            return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                path: streaming_path,
                actual,
            });
        }
    };
    Ok(Some(ValidatedOutputWire {
        alias,
        media_type,
        type_selector,
        streaming,
    }))
}

fn parse_event_wire<'a>(
    reader: &mut CarrierReader<'a>,
) -> Result<ValidatedEventWire<'a>, InvocationCarrierCodecError> {
    let kind_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::EventKind);
    let kind = reader.u8()?;
    if kind > 5 {
        return Err(InvocationCarrierCodecError::UnknownDiscriminant {
            path: kind_path,
            actual: kind,
        });
    }
    let invocation_id = InvocationId::from_bytes(reader.id()?);
    let sequence = reader.u64()?;
    let body_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody);
    let body = match kind {
        0 => {
            let principal_path = body_path.with(InvocationCarrierPathSegment::VisiblePrincipal);
            let visible_principal = if reader.presence(principal_path)? {
                Some(PrincipalId::from_bytes(reader.id()?))
            } else {
                None
            };
            ValidatedEventBodyWire::Started { visible_principal }
        }
        1 => {
            let channel_path = body_path.with(InvocationCarrierPathSegment::Channel);
            let channel = reader.u8()?;
            if channel != 0 {
                return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                    path: channel_path,
                    actual: channel,
                });
            }
            let schema_path = body_path.with(InvocationCarrierPathSegment::Schema);
            let schema = if reader.presence(schema_path.clone())? {
                Some(parse_embedded_invoke_value(reader, schema_path)?)
            } else {
                None
            };
            let values_path = body_path.with(InvocationCarrierPathSegment::BatchValues);
            let count = reader.u32()? as usize;
            if count == 0 {
                return Err(InvocationCarrierCodecError::InvalidField { path: values_path });
            }
            let mut values = Vec::new();
            for index in 0..count {
                values.push(parse_embedded_invoke_value(
                    reader,
                    values_path.with(InvocationCarrierPathSegment::BatchValue(index)),
                )?);
            }
            ValidatedEventBodyWire::ValueBatch { schema, values }
        }
        2 => {
            let severity_path = body_path.with(InvocationCarrierPathSegment::Severity);
            let severity = match reader.u8()? {
                0 => InvocationDiagnosticSeverity::Info,
                1 => InvocationDiagnosticSeverity::Warning,
                2 => InvocationDiagnosticSeverity::Error,
                actual => {
                    return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                        path: severity_path,
                        actual,
                    });
                }
            };
            let code_path = body_path.with(InvocationCarrierPathSegment::Code);
            let code = reader.required_text(code_path.clone())?;
            if !is_printable_ascii(code) {
                return Err(InvocationCarrierCodecError::InvalidField { path: code_path });
            }
            let message = reader.text(body_path.with(InvocationCarrierPathSegment::Message))?;
            ValidatedEventBodyWire::Diagnostic {
                severity,
                code,
                message,
            }
        }
        3 => ValidatedEventBodyWire::Completed {
            duration_nanoseconds: reader.u64()?,
        },
        4 => {
            let phase_path = body_path.with(InvocationCarrierPathSegment::Phase);
            let phase = match reader.u8()? {
                0 => InvocationFailurePhase::Resolve,
                1 => InvocationFailurePhase::Bind,
                2 => InvocationFailurePhase::Authorise,
                3 => InvocationFailurePhase::Target,
                4 => InvocationFailurePhase::Present,
                5 => InvocationFailurePhase::Runtime,
                6 => InvocationFailurePhase::Transport,
                7 => InvocationFailurePhase::Internal,
                actual => {
                    return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                        path: phase_path,
                        actual,
                    });
                }
            };
            let code_path = body_path.with(InvocationCarrierPathSegment::Code);
            let code = reader.required_text(code_path.clone())?;
            if !is_printable_ascii(code) {
                return Err(InvocationCarrierCodecError::InvalidField { path: code_path });
            }
            let message = reader.text(body_path.with(InvocationCarrierPathSegment::Message))?;
            let details_path = body_path.with(InvocationCarrierPathSegment::Details);
            let details = if reader.presence(details_path.clone())? {
                Some(parse_embedded_invoke_value(reader, details_path)?)
            } else {
                None
            };
            let retryability_path = body_path.with(InvocationCarrierPathSegment::Retryability);
            let retryability = match reader.u8()? {
                0 => InvocationRetryability::Unknown,
                1 => InvocationRetryability::No,
                2 => InvocationRetryability::Yes,
                actual => {
                    return Err(InvocationCarrierCodecError::UnknownDiscriminant {
                        path: retryability_path,
                        actual,
                    });
                }
            };
            ValidatedEventBodyWire::Failed {
                phase,
                code,
                message,
                details,
                retryability,
            }
        }
        5 => {
            let reason_path = body_path.with(InvocationCarrierPathSegment::Reason);
            ValidatedEventBodyWire::Cancelled {
                reason: parse_optional_text(reader, reason_path)?,
            }
        }
        _ => unreachable!("event kind range checked"),
    };
    Ok(ValidatedEventWire {
        invocation_id,
        sequence,
        body,
    })
}

fn is_printable_ascii(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

fn decode_invocation_carrier(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    carrier: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let validated = parse_invocation_carrier(carrier, payload)
        .map_err(|source| ValueCodecError::InvocationCarrier { carrier, source })?;
    validated
        .preflight(carrier)
        .map_err(|source| ValueCodecError::InvocationCarrier { carrier, source })?;
    validated
        .materialise(active, registry)
        .map_err(|source| ValueCodecError::InvocationCarrier { carrier, source })
}

impl ValidatedCarrierWire<'_> {
    fn preflight(&self, carrier: TypeId) -> Result<(), InvocationCarrierCodecError> {
        let mut budget = NodeBudget::invocation(carrier);
        match self {
            Self::Value(value) => preflight_validated_value(value, &mut budget, carrier, false),
            Self::Request(request) => {
                for argument in &request.arguments {
                    preflight_validated_value(&argument.value, &mut budget, carrier, true)?;
                }
                if let Some(value) = &request.caller.preference_policy {
                    preflight_validated_value(value, &mut budget, carrier, true)?;
                }
                for sink in &request.client_offer.sinks {
                    if let Some(value) = &sink.limits {
                        preflight_validated_value(value, &mut budget, carrier, true)?;
                    }
                }
                for runtime in &request.client_offer.runtimes {
                    if let Some(value) = &runtime.limits {
                        preflight_validated_value(value, &mut budget, carrier, true)?;
                    }
                }
                if let Some(value) = &request.client_offer.limits {
                    preflight_validated_value(value, &mut budget, carrier, true)?;
                }
                if let Some(value) = &request.client_offer.preferences {
                    preflight_validated_value(value, &mut budget, carrier, true)?;
                }
                if let Some(value) = &request.observer_context {
                    preflight_validated_value(value, &mut budget, carrier, true)?;
                }
                Ok(())
            }
            Self::Event(event) => {
                match &event.body {
                    ValidatedEventBodyWire::ValueBatch { schema, values } => {
                        if let Some(value) = schema {
                            preflight_validated_value(value, &mut budget, carrier, true)?;
                        }
                        for value in values {
                            preflight_validated_value(value, &mut budget, carrier, true)?;
                        }
                    }
                    ValidatedEventBodyWire::Failed {
                        details: Some(value),
                        ..
                    } => preflight_validated_value(value, &mut budget, carrier, true)?,
                    _ => {}
                }
                Ok(())
            }
        }
    }

    fn materialise(
        self,
        active: &ActiveDatabaseRevision,
        registry: &OpaqueCodecRegistry,
    ) -> Result<RuntimeValue, InvocationCarrierCodecError> {
        match self {
            Self::Value(value) => {
                materialise_invoke_value(active, registry, value).map(RuntimeValue::InvokeValue)
            }
            Self::Request(request) => {
                materialise_request(active, registry, *request).map(RuntimeValue::InvokeRequest)
            }
            Self::Event(event) => {
                materialise_event(active, registry, event).map(RuntimeValue::InvokeEvent)
            }
        }
    }
}

fn preflight_validated_value(
    value: &ValidatedInvokeValueWire<'_>,
    budget: &mut NodeBudget,
    outer: TypeId,
    add_wrapper: bool,
) -> Result<(), InvocationCarrierCodecError> {
    if add_wrapper {
        budget
            .increment()
            .map_err(extract_carrier_preflight_error)?;
    }
    preflight_orv5_envelope(
        value.inner.bytes,
        budget,
        &mut Vec::new(),
        InvocationCarrierPreflightPolicy::Reject {
            outer,
            path: &value.path,
        },
    )
    .map_err(|source| match source {
        ValueCodecError::InvocationCarrier {
            carrier: rejected_outer,
            source,
        } if rejected_outer == outer => source,
        source => InvocationCarrierCodecError::InnerValue {
            path: value.path.clone(),
            source: Box::new(source),
        },
    })
}

fn extract_carrier_preflight_error(source: ValueCodecError) -> InvocationCarrierCodecError {
    match source {
        ValueCodecError::InvocationCarrier { source, .. } => source,
        _ => InvocationCarrierCodecError::TooManyNodes {
            maximum: MAX_INVOCATION_CARRIER_NODES,
        },
    }
}

fn materialise_invoke_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: ValidatedInvokeValueWire<'_>,
) -> Result<InvokeValue, InvocationCarrierCodecError> {
    let decoded =
        decode_constructed_value(active, registry, value.inner.bytes).map_err(|source| {
            InvocationCarrierCodecError::InnerValue {
                path: value.path.clone(),
                source: Box::new(source),
            }
        })?;
    InvokeValue::new(decoded).map_err(|source| map_construction_error(source, value.path))
}

fn map_construction_error(
    source: InvocationCarrierConstructionError,
    path: InvocationCarrierPath,
) -> InvocationCarrierCodecError {
    match source {
        InvocationCarrierConstructionError::TooManyNodes { maximum } => {
            InvocationCarrierCodecError::TooManyNodes { maximum }
        }
        InvocationCarrierConstructionError::NestedCarrier { carrier } => {
            InvocationCarrierCodecError::NestedCarrier { path, carrier }
        }
        _ => InvocationCarrierCodecError::InvalidField { path },
    }
}

fn qualified_name(
    parts: Vec<&str>,
    path: InvocationCarrierPath,
) -> Result<QualifiedSemanticName, InvocationCarrierCodecError> {
    QualifiedSemanticName::new(parts)
        .map_err(|_| InvocationCarrierCodecError::InvalidSemanticName { path })
}

fn materialise_optional_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    value: Option<ValidatedInvokeValueWire<'_>>,
) -> Result<Option<InvokeValue>, InvocationCarrierCodecError> {
    value
        .map(|value| materialise_invoke_value(active, registry, value))
        .transpose()
}

fn materialise_request(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    request: ValidatedRequestWire<'_>,
) -> Result<InvokeRequest, InvocationCarrierCodecError> {
    let target_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestTarget);
    let target = match request.target {
        ValidatedInvocationTargetWire::FunctionId(function) => {
            InvocationTarget::function_id(function)
        }
        ValidatedInvocationTargetWire::QualifiedName(parts) => {
            InvocationTarget::qualified_name(qualified_name(parts, target_path.clone())?)
                .map_err(|source| map_construction_error(source, target_path.clone()))?
        }
    };
    let mut arguments = Vec::new();
    for (index, argument) in request.arguments.into_iter().enumerate() {
        let argument_path =
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments)
                .with(InvocationCarrierPathSegment::Argument(index));
        let selector = match argument.selector {
            ValidatedParameterSelectorWire::ParameterId(parameter) => {
                InvocationParameterSelector::parameter_id(parameter)
            }
            ValidatedParameterSelectorWire::Name(name) => InvocationParameterSelector::name(name)
                .map_err(|source| {
                map_construction_error(
                    source,
                    argument_path.with(InvocationCarrierPathSegment::Selector),
                )
            })?,
        };
        let value = materialise_invoke_value(active, registry, argument.value)?;
        arguments.push(InvocationArgument::new(selector, value));
    }
    let caller = InvocationCallerContext::new(
        request.caller.kind,
        request.caller.interactive,
        request.caller.stdout_is_tty,
        request.caller.terminal_columns,
        request.caller.terminal_rows,
        request.caller.locale,
        request.caller.timezone,
        materialise_optional_value(active, registry, request.caller.preference_policy)?,
    )
    .map_err(|source| {
        map_construction_error(
            source,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestCaller),
        )
    })?;
    let offer_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestClientOffer);
    let mut sinks = Vec::new();
    for (index, sink) in request.client_offer.sinks.into_iter().enumerate() {
        let item_path = offer_path
            .with(InvocationCarrierPathSegment::ClientSinks)
            .with(InvocationCarrierPathSegment::Sink(index));
        sinks.push(
            InvocationSinkOffer::new(
                sink.descriptor,
                sink.media_types,
                sink.streaming,
                sink.preference_rank,
                materialise_optional_value(active, registry, sink.limits)?,
            )
            .map_err(|source| map_construction_error(source, item_path))?,
        );
    }
    let mut runtimes = Vec::new();
    for (index, runtime) in request.client_offer.runtimes.into_iter().enumerate() {
        let item_path = offer_path
            .with(InvocationCarrierPathSegment::ClientRuntimes)
            .with(InvocationCarrierPathSegment::Runtime(index));
        let mut contracts = Vec::new();
        for (contract_index, contract) in runtime.contracts.into_iter().enumerate() {
            contracts.push(
                InvocationRuntimeContract::new(contract.name, contract.version, contract.features)
                    .map_err(|source| {
                        map_construction_error(
                            source,
                            item_path
                                .with(InvocationCarrierPathSegment::Contracts)
                                .with(InvocationCarrierPathSegment::Contract(contract_index)),
                        )
                    })?,
            );
        }
        runtimes.push(
            InvocationRuntimeOffer::new(
                runtime.name,
                runtime.version,
                runtime.consumed_descriptors,
                contracts,
                runtime.preference_rank,
                runtime.trusted,
                materialise_optional_value(active, registry, runtime.limits)?,
            )
            .map_err(|source| map_construction_error(source, item_path))?,
        );
    }
    let client_offer = InvocationClientOffer::new(
        request.client_offer.protocol_major,
        request.client_offer.locale,
        request.client_offer.timezone,
        sinks,
        runtimes,
        request.client_offer.maximum_frame_size,
        request.client_offer.maximum_artifact_size,
        materialise_optional_value(active, registry, request.client_offer.limits)?,
        materialise_optional_value(active, registry, request.client_offer.preferences)?,
    )
    .map_err(|source| map_construction_error(source, offer_path))?;
    let output_requirement = request
        .output
        .map(|output| materialise_output(output))
        .transpose()?;
    let input = InvokeRequestInput {
        target,
        arguments,
        caller_context: caller,
        client_offer,
        output_requirement,
        state_profile: request.state_profile.map(str::to_owned),
        trace_policy: request.trace_policy,
        idempotency_key: request.idempotency_key.map(<[u8]>::to_vec),
        parent_invocation_id: request.parent_invocation,
        observer_context: materialise_optional_value(active, registry, request.observer_context)?,
    };
    InvokeRequest::new(input).map_err(|source| {
        map_construction_error(
            source,
            InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestArguments),
        )
    })
}

fn materialise_output(
    output: ValidatedOutputWire<'_>,
) -> Result<InvocationOutputRequirement, InvocationCarrierCodecError> {
    let output_path =
        InvocationCarrierPath::one(InvocationCarrierPathSegment::RequestOutputRequirement);
    let type_selector = output
        .type_selector
        .map(|selector| match selector {
            ValidatedOutputTypeWire::TypeId(type_id) => {
                Ok(InvocationOutputTypeSelector::type_id(type_id))
            }
            ValidatedOutputTypeWire::QualifiedName(parts) => {
                let path = output_path.with(InvocationCarrierPathSegment::OutputType);
                InvocationOutputTypeSelector::qualified_name(qualified_name(parts, path.clone())?)
                    .map_err(|source| map_construction_error(source, path))
            }
        })
        .transpose()?;
    InvocationOutputRequirement::new(
        output.alias.map(str::to_owned),
        output.media_type.map(str::to_owned),
        type_selector,
        output.streaming,
    )
    .map_err(|source| map_construction_error(source, output_path))
}

fn materialise_event(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    event: ValidatedEventWire<'_>,
) -> Result<InvokeEvent, InvocationCarrierCodecError> {
    let body_path = InvocationCarrierPath::one(InvocationCarrierPathSegment::EventBody);
    let body = match event.body {
        ValidatedEventBodyWire::Started { visible_principal } => {
            InvocationEventBody::Started { visible_principal }
        }
        ValidatedEventBodyWire::ValueBatch { schema, values } => {
            let schema = materialise_optional_value(active, registry, schema)?;
            let mut decoded = Vec::new();
            for value in values {
                decoded.push(materialise_invoke_value(active, registry, value)?);
            }
            InvocationEventBody::value_batch(schema, decoded).map_err(|source| {
                map_construction_error(
                    source,
                    body_path.with(InvocationCarrierPathSegment::BatchValues),
                )
            })?
        }
        ValidatedEventBodyWire::Diagnostic {
            severity,
            code,
            message,
        } => InvocationEventBody::Diagnostic(
            InvocationDiagnostic::new(severity, code, message).map_err(|source| {
                map_construction_error(source, body_path.with(InvocationCarrierPathSegment::Code))
            })?,
        ),
        ValidatedEventBodyWire::Completed {
            duration_nanoseconds,
        } => InvocationEventBody::Completed {
            duration_nanoseconds,
        },
        ValidatedEventBodyWire::Failed {
            phase,
            code,
            message,
            details,
            retryability,
        } => InvocationEventBody::Failed(
            InvocationFailure::new(
                phase,
                code,
                message,
                materialise_optional_value(active, registry, details)?,
                retryability,
            )
            .map_err(|source| {
                map_construction_error(source, body_path.with(InvocationCarrierPathSegment::Code))
            })?,
        ),
        ValidatedEventBodyWire::Cancelled { reason } => {
            InvocationEventBody::cancelled(reason.map(str::to_owned)).map_err(|source| {
                map_construction_error(source, body_path.with(InvocationCarrierPathSegment::Reason))
            })?
        }
    };
    InvokeEvent::new(event.invocation_id, event.sequence, body)
        .map_err(|source| map_construction_error(source, body_path))
}

fn append_count(payload: &mut Vec<u8>, count: usize) -> Result<(), ValueCodecError> {
    let count = u32::try_from(count).map_err(|_| ValueCodecError::PayloadTooLarge {
        actual: usize::MAX,
        maximum: PAYLOAD_LIMIT,
    })?;
    payload.extend_from_slice(&count.to_be_bytes());
    Ok(())
}

fn append_orv5_child(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    payload: &mut Vec<u8>,
    value: &RuntimeValue,
) -> Result<(), ValueCodecError> {
    let encoded = encode_orv5_value(active, registry, value)?;
    let length = u32::try_from(encoded.len()).map_err(|_| ValueCodecError::PayloadTooLarge {
        actual: encoded.len(),
        maximum: PAYLOAD_LIMIT,
    })?;
    let next = payload
        .len()
        .checked_add(4)
        .and_then(|length| length.checked_add(encoded.len()))
        .ok_or(ValueCodecError::PayloadTooLarge {
            actual: usize::MAX,
            maximum: PAYLOAD_LIMIT,
        })?;
    require_payload_limit(next)?;
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(&encoded);
    Ok(())
}

fn encode_constructed_descriptor(
    descriptor: &TypeDescriptor,
    encoded: &mut Vec<u8>,
) -> Result<(), ValueCodecError> {
    match descriptor.kind() {
        TypeDescriptorKind::Named(type_id) => {
            encoded.push(0);
            encoded.extend_from_slice(&type_id.to_bytes());
        }
        TypeDescriptorKind::Reference(type_id) => {
            encoded.push(1);
            encoded.extend_from_slice(&type_id.to_bytes());
        }
        TypeDescriptorKind::List(child) => {
            encoded.push(2);
            encode_constructed_descriptor(child, encoded)?;
        }
        TypeDescriptorKind::Map { key, value } => {
            encoded.push(3);
            encode_constructed_descriptor(key, encoded)?;
            encode_constructed_descriptor(value, encoded)?;
        }
        TypeDescriptorKind::Option(child) => {
            encoded.push(4);
            encode_constructed_descriptor(child, encoded)?;
        }
        TypeDescriptorKind::Set(child) => {
            encoded.push(5);
            encode_constructed_descriptor(child, encoded)?;
        }
        TypeDescriptorKind::Stream(_) => {
            return Err(ValueCodecError::UnsupportedConstructedDescriptor {
                descriptor: descriptor.clone(),
            });
        }
    }
    Ok(())
}

fn decode_orv5_parts(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    tag: u8,
    type_id: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    if tag == OPAQUE_TAG && invocation_carrier_by_id(type_id).is_some() {
        return decode_invocation_carrier(active, registry, type_id, payload);
    }
    if tag == CONSTRUCTED_TAG {
        if type_id.to_bytes() != [0; 16] {
            return Err(ValueCodecError::ConstructedTypeIdentityNotZero { identity: type_id });
        }
        return decode_constructed_payload(active, registry, payload);
    }
    let encoded = encode_with_marker(CONSTRUCTED_MARKER, tag, type_id, payload);
    decode_registered_value_with_marker(active, registry, &encoded, CONSTRUCTED_MARKER)
}

fn decode_constructed_payload(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let (descriptor, body) = decode_constructed_descriptor(payload)?;
    preflight_constructed_descriptor(active, &descriptor)?;
    decode_constructed_parts(active, registry, descriptor, body)
}

fn decode_constructed_parts(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    descriptor: TypeDescriptor,
    body: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    match descriptor.kind() {
        TypeDescriptorKind::Option(_) => decode_option_chain(active, registry, descriptor, body),
        TypeDescriptorKind::List(_) => decode_list_body(active, registry, descriptor, body),
        TypeDescriptorKind::Set(_) => decode_set_body(active, registry, descriptor, body),
        TypeDescriptorKind::Map { .. } => decode_map_body(active, registry, descriptor, body),
        _ => Err(ValueCodecError::UnsupportedConstructedDescriptor { descriptor }),
    }
}

fn decode_constructed_descriptor(
    payload: &[u8],
) -> Result<(TypeDescriptor, &[u8]), ValueCodecError> {
    decode_constructed_descriptor_with_set(payload, false)
}

fn decode_constructed_descriptor_with_set(
    payload: &[u8],
    allow_set: bool,
) -> Result<(TypeDescriptor, &[u8]), ValueCodecError> {
    if payload.len() < 2 {
        return Err(ValueCodecError::TruncatedConstructedHeader {
            actual: payload.len(),
        });
    }
    let length = u16::from_be_bytes(payload[..2].try_into().expect("length checked")) as usize;
    if length == 0 {
        return Err(ValueCodecError::EmptyConstructedDescriptor);
    }
    let available = payload.len() - 2;
    if available < length {
        return Err(ValueCodecError::TruncatedConstructedDescriptor {
            declared: length,
            available,
        });
    }
    let bytes = &payload[2..2 + length];
    let (descriptor, consumed) = parse_constructed_descriptor(bytes, 0, allow_set)?;
    if consumed != bytes.len() {
        return Err(ValueCodecError::TrailingConstructedDescriptor {
            remaining: bytes.len() - consumed,
        });
    }
    Ok((descriptor, &payload[2 + length..]))
}

fn parse_constructed_descriptor(
    encoded: &[u8],
    offset: usize,
    allow_set: bool,
) -> Result<(TypeDescriptor, usize), ValueCodecError> {
    enum Pending {
        List,
        Set,
        Option,
        MapKey,
        MapValue(TypeDescriptor),
    }

    let mut cursor = offset;
    let mut pending = Vec::new();
    let mut complete = None;
    loop {
        if let Some(mut descriptor) = complete.take() {
            loop {
                match pending.pop() {
                    Some(Pending::List) => {
                        descriptor = TypeDescriptor::list(descriptor).map_err(|source| {
                            ValueCodecError::InvalidConstructedDescriptor { source }
                        })?;
                    }
                    Some(Pending::Set) => {
                        descriptor = TypeDescriptor::set(descriptor).map_err(|source| {
                            ValueCodecError::InvalidConstructedDescriptor { source }
                        })?;
                    }
                    Some(Pending::Option) => {
                        descriptor = TypeDescriptor::option(descriptor).map_err(|source| {
                            ValueCodecError::InvalidConstructedDescriptor { source }
                        })?;
                    }
                    Some(Pending::MapKey) => {
                        pending.push(Pending::MapValue(descriptor));
                        break;
                    }
                    Some(Pending::MapValue(key)) => {
                        descriptor = TypeDescriptor::map(key, descriptor).map_err(|source| {
                            ValueCodecError::InvalidConstructedDescriptor { source }
                        })?;
                    }
                    None => return Ok((descriptor, cursor)),
                }
            }
            continue;
        }

        let available = encoded.len().saturating_sub(cursor);
        let tag =
            *encoded
                .get(cursor)
                .ok_or(ValueCodecError::TruncatedConstructedDescriptorNode {
                    offset: cursor,
                    required: 1,
                    available,
                })?;
        match tag {
            0 | 1 => {
                if available < 17 {
                    return Err(ValueCodecError::TruncatedConstructedDescriptorNode {
                        offset: cursor,
                        required: 17,
                        available,
                    });
                }
                let type_id = TypeId::from_bytes(
                    encoded[cursor + 1..cursor + 17]
                        .try_into()
                        .expect("descriptor leaf length checked"),
                );
                cursor += 17;
                complete = Some(if tag == 0 {
                    TypeDescriptor::named(type_id)
                } else {
                    TypeDescriptor::reference(type_id)
                });
            }
            2..=4 => {
                let depth = pending.len() + 1;
                if depth > MAX_TYPE_DESCRIPTOR_DEPTH {
                    return Err(ValueCodecError::InvalidConstructedDescriptor {
                        source: TypeDescriptorError::TooDeep {
                            maximum: MAX_TYPE_DESCRIPTOR_DEPTH,
                            actual: depth,
                        },
                    });
                }
                cursor += 1;
                pending.push(match tag {
                    2 => Pending::List,
                    3 => Pending::MapKey,
                    4 => Pending::Option,
                    _ => unreachable!("constructor tag was checked"),
                });
            }
            5 if allow_set => {
                let depth = pending.len() + 1;
                if depth > MAX_TYPE_DESCRIPTOR_DEPTH {
                    return Err(ValueCodecError::InvalidConstructedDescriptor {
                        source: TypeDescriptorError::TooDeep {
                            maximum: MAX_TYPE_DESCRIPTOR_DEPTH,
                            actual: depth,
                        },
                    });
                }
                cursor += 1;
                pending.push(Pending::Set);
            }
            5 => return Err(ValueCodecError::UnknownConstructedDescriptorTag { tag }),
            tag => return Err(ValueCodecError::UnknownConstructedDescriptorTag { tag }),
        }
    }
}

fn preflight_constructed_descriptor(
    active: &ActiveDatabaseRevision,
    descriptor: &TypeDescriptor,
) -> Result<(), ValueCodecError> {
    let result = match descriptor.kind() {
        TypeDescriptorKind::Option(_) => RuntimeValue::option(active, descriptor.clone(), None),
        TypeDescriptorKind::List(_) => RuntimeValue::list(active, descriptor.clone(), Vec::new()),
        TypeDescriptorKind::Set(_) => RuntimeValue::set(active, descriptor.clone(), Vec::new()),
        TypeDescriptorKind::Map { .. } => RuntimeValue::map(active, descriptor.clone(), Vec::new()),
        _ => {
            return Err(ValueCodecError::UnsupportedConstructedDescriptor {
                descriptor: descriptor.clone(),
            });
        }
    };
    result
        .map(|_| ())
        .map_err(|source| ValueCodecError::CollectionValue { source })
}

fn decode_option_chain(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    mut descriptor: TypeDescriptor,
    mut body: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let mut parents = Vec::new();
    loop {
        let Some((&presence, remaining)) = body.split_first() else {
            return Err(ValueCodecError::TruncatedCollectionEntry { path: Vec::new() });
        };
        match presence {
            0 => {
                if !remaining.is_empty() {
                    return Err(ValueCodecError::TrailingBytes {
                        declared: 1,
                        actual: body.len(),
                    });
                }
                let tail = RuntimeValue::option(active, descriptor, None)
                    .map_err(|source| ValueCodecError::CollectionValue { source })?;
                return rebuild_option_chain(active, parents, tail);
            }
            1 => {
                let child_path = [CollectionValuePathSegment::OptionChild];
                let (encoded, consumed) = take_constructed_child(remaining, 0, &child_path)?;
                if consumed != remaining.len() {
                    return Err(ValueCodecError::TrailingBytes {
                        declared: consumed + 1,
                        actual: body.len(),
                    });
                }
                let (tag, type_id, payload) = decode_envelope(encoded, CONSTRUCTED_MARKER)
                    .map_err(|source| ValueCodecError::ConstructedChild {
                        path: vec![CollectionValuePathSegment::OptionChild],
                        source: Box::new(source),
                    })?;
                if tag == CONSTRUCTED_TAG {
                    if type_id.to_bytes() != [0; 16] {
                        return Err(ValueCodecError::ConstructedChild {
                            path: vec![CollectionValuePathSegment::OptionChild],
                            source: Box::new(ValueCodecError::ConstructedTypeIdentityNotZero {
                                identity: type_id,
                            }),
                        });
                    }
                    let (child_descriptor, child_body) = decode_constructed_descriptor(payload)
                        .map_err(|source| ValueCodecError::ConstructedChild {
                            path: vec![CollectionValuePathSegment::OptionChild],
                            source: Box::new(source),
                        })?;
                    preflight_constructed_descriptor(active, &child_descriptor).map_err(
                        |source| ValueCodecError::ConstructedChild {
                            path: vec![CollectionValuePathSegment::OptionChild],
                            source: Box::new(source),
                        },
                    )?;
                    if matches!(child_descriptor.kind(), TypeDescriptorKind::Option(_)) {
                        parents.push(descriptor);
                        descriptor = child_descriptor;
                        body = child_body;
                        continue;
                    }
                    let child = decode_constructed_payload(active, registry, payload).map_err(
                        |source| ValueCodecError::ConstructedChild {
                            path: vec![CollectionValuePathSegment::OptionChild],
                            source: Box::new(source),
                        },
                    )?;
                    parents.push(descriptor);
                    return rebuild_option_chain(active, parents, child);
                }
                let child = decode_orv5_parts(active, registry, tag, type_id, payload).map_err(
                    |source| ValueCodecError::ConstructedChild {
                        path: vec![CollectionValuePathSegment::OptionChild],
                        source: Box::new(source),
                    },
                )?;
                parents.push(descriptor);
                return rebuild_option_chain(active, parents, child);
            }
            value => return Err(ValueCodecError::InvalidOptionPresence { value }),
        }
    }
}

fn rebuild_option_chain(
    active: &ActiveDatabaseRevision,
    parents: Vec<TypeDescriptor>,
    mut value: RuntimeValue,
) -> Result<RuntimeValue, ValueCodecError> {
    for (index, descriptor) in parents.into_iter().enumerate().rev() {
        value = match RuntimeValue::option(active, descriptor, Some(value)) {
            Ok(value) => value,
            Err(source) if index == 0 => return Err(ValueCodecError::CollectionValue { source }),
            Err(source) => {
                return Err(ValueCodecError::ConstructedChild {
                    path: vec![CollectionValuePathSegment::OptionChild; index],
                    source: Box::new(ValueCodecError::CollectionValue { source }),
                });
            }
        };
    }
    Ok(value)
}

fn decode_list_body(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    descriptor: TypeDescriptor,
    body: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let count = decode_constructed_count(body)?;
    let mut cursor = 4;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let path = vec![CollectionValuePathSegment::ListElement(index)];
        let (encoded, consumed) = take_constructed_child(body, cursor, &path)?;
        values.push(decode_orv5_child(active, registry, encoded, path)?);
        cursor = consumed;
    }
    if cursor != body.len() {
        return Err(ValueCodecError::TrailingBytes {
            declared: cursor,
            actual: body.len(),
        });
    }
    RuntimeValue::list(active, descriptor, values)
        .map_err(|source| ValueCodecError::CollectionValue { source })
}

fn decode_set_body(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    descriptor: TypeDescriptor,
    body: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let count = decode_constructed_count(body)?;
    let mut cursor = 4;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let path = vec![CollectionValuePathSegment::SetElement(index)];
        let (encoded, consumed) = take_constructed_child(body, cursor, &path)?;
        values.push(decode_orv5_child(active, registry, encoded, path)?);
        cursor = consumed;
    }
    if cursor != body.len() {
        return Err(ValueCodecError::TrailingBytes {
            declared: cursor,
            actual: body.len(),
        });
    }
    let wire_values = values.clone();
    let value = RuntimeValue::set(active, descriptor, values)
        .map_err(|source| ValueCodecError::CollectionValue { source })?;
    let RuntimeValue::Constructed(constructed) = &value else {
        unreachable!("checked SET construction returns a constructed value");
    };
    let ConstructedValueKind::Set(canonical) = constructed.kind() else {
        unreachable!("checked SET construction retains SET contents");
    };
    if canonical != wire_values.as_slice() {
        let index = canonical
            .iter()
            .zip(&wire_values)
            .position(|(left, right)| left != right)
            .unwrap_or(0);
        return Err(ValueCodecError::NonCanonicalSetOrder { index });
    }
    Ok(value)
}

fn decode_map_body(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    descriptor: TypeDescriptor,
    body: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    let count = decode_constructed_count(body)?;
    let mut cursor = 4;
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let key_path = vec![CollectionValuePathSegment::MapKey(index)];
        let (encoded_key, key_end) = take_constructed_child(body, cursor, &key_path)?;
        let key = decode_orv5_child(active, registry, encoded_key, key_path)?;
        let value_path = vec![CollectionValuePathSegment::MapValue(index)];
        let (encoded_value, value_end) = take_constructed_child(body, key_end, &value_path)?;
        let value = decode_orv5_child(active, registry, encoded_value, value_path)?;
        entries.push((key, value));
        cursor = value_end;
    }
    if cursor != body.len() {
        return Err(ValueCodecError::TrailingBytes {
            declared: cursor,
            actual: body.len(),
        });
    }
    let wire_entries = entries.clone();
    let value = RuntimeValue::map(active, descriptor, entries)
        .map_err(|source| ValueCodecError::CollectionValue { source })?;
    let RuntimeValue::Constructed(constructed) = &value else {
        unreachable!("checked MAP construction returns a constructed value");
    };
    let ConstructedValueKind::Map(canonical) = constructed.kind() else {
        unreachable!("checked MAP construction retains MAP contents");
    };
    if canonical != wire_entries.as_slice() {
        let index = canonical
            .iter()
            .zip(&wire_entries)
            .position(|(left, right)| left != right)
            .unwrap_or(0);
        return Err(ValueCodecError::NonCanonicalMapOrder { index });
    }
    Ok(value)
}

fn decode_constructed_count(body: &[u8]) -> Result<usize, ValueCodecError> {
    if body.len() < 4 {
        return Err(ValueCodecError::TruncatedCollectionEntry { path: Vec::new() });
    }
    Ok(u32::from_be_bytes(body[..4].try_into().expect("count length checked")) as usize)
}

fn take_constructed_child<'a>(
    body: &'a [u8],
    cursor: usize,
    path: &[CollectionValuePathSegment],
) -> Result<(&'a [u8], usize), ValueCodecError> {
    let remaining =
        body.get(cursor..)
            .ok_or_else(|| ValueCodecError::TruncatedCollectionEntry {
                path: path.to_vec(),
            })?;
    if remaining.len() < 4 {
        return Err(ValueCodecError::TruncatedCollectionEntry {
            path: path.to_vec(),
        });
    }
    let declared = u32::from_be_bytes(remaining[..4].try_into().expect("length checked")) as usize;
    let available = remaining.len() - 4;
    if declared < HEADER_LENGTH || declared > available {
        return Err(ValueCodecError::TruncatedCollectionEntry {
            path: path.to_vec(),
        });
    }
    let end = cursor
        .checked_add(4)
        .and_then(|start| start.checked_add(declared))
        .ok_or_else(|| ValueCodecError::TruncatedCollectionEntry {
            path: path.to_vec(),
        })?;
    Ok((&body[cursor + 4..end], end))
}

fn decode_orv5_child(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    encoded: &[u8],
    path: Vec<CollectionValuePathSegment>,
) -> Result<RuntimeValue, ValueCodecError> {
    decode_constructed_value(active, registry, encoded).map_err(|source| {
        ValueCodecError::ConstructedChild {
            path,
            source: Box::new(source),
        }
    })
}

struct NodeBudget {
    nodes: usize,
    maximum: usize,
    carrier: Option<TypeId>,
}

impl NodeBudget {
    fn runtime() -> Self {
        Self {
            nodes: 0,
            maximum: MAX_RUNTIME_VALUE_NODES,
            carrier: None,
        }
    }

    fn invocation(carrier: TypeId) -> Self {
        Self {
            nodes: 1,
            maximum: MAX_INVOCATION_CARRIER_NODES,
            carrier: Some(carrier),
        }
    }

    fn increment(&mut self) -> Result<(), ValueCodecError> {
        let next = self
            .nodes
            .checked_add(1)
            .ok_or_else(|| self.limit_error())?;
        if next > self.maximum {
            return Err(self.limit_error());
        }
        self.nodes = next;
        Ok(())
    }

    fn limit_error(&self) -> ValueCodecError {
        match self.carrier {
            Some(carrier) => ValueCodecError::InvocationCarrier {
                carrier,
                source: InvocationCarrierCodecError::TooManyNodes {
                    maximum: self.maximum,
                },
            },
            None => ValueCodecError::CollectionValue {
                source: CollectionValueError::TooManyNodes {
                    maximum: self.maximum,
                },
            },
        }
    }
}

#[derive(Clone, Copy)]
enum InvocationCarrierPreflightPolicy<'a> {
    Allow,
    Reject {
        outer: TypeId,
        path: &'a InvocationCarrierPath,
    },
}

fn preflight_orv5_tree(
    payload: &[u8],
    tag: u8,
    budget: &mut NodeBudget,
    path: &mut Vec<CollectionValuePathSegment>,
    policy: InvocationCarrierPreflightPolicy<'_>,
    allow_set: bool,
) -> Result<(), ValueCodecError> {
    budget.increment()?;
    match tag {
        CONSTRUCTED_TAG => {
            let (descriptor, body) = decode_constructed_descriptor_with_set(payload, allow_set)?;
            match descriptor.kind() {
                TypeDescriptorKind::Option(_) => {
                    preflight_orv5_option_chain(body, budget, path, policy)?
                }
                TypeDescriptorKind::List(_) => {
                    let count = decode_constructed_count(body)?;
                    let mut cursor = 4;
                    for index in 0..count {
                        let child_path = [CollectionValuePathSegment::ListElement(index)];
                        let (child, end) = take_constructed_child(body, cursor, &child_path)?;
                        path.push(CollectionValuePathSegment::ListElement(index));
                        let result = preflight_orv5_child(child, budget, path, policy);
                        path.pop();
                        result?;
                        cursor = end;
                    }
                    if cursor != body.len() {
                        return Err(ValueCodecError::TrailingBytes {
                            declared: cursor,
                            actual: body.len(),
                        });
                    }
                }
                TypeDescriptorKind::Set(_) => {
                    let count = decode_constructed_count(body)?;
                    let mut cursor = 4;
                    for index in 0..count {
                        let child_path = [CollectionValuePathSegment::SetElement(index)];
                        let (child, end) = take_constructed_child(body, cursor, &child_path)?;
                        path.push(CollectionValuePathSegment::SetElement(index));
                        let result = preflight_orv5_child(child, budget, path, policy);
                        path.pop();
                        result?;
                        cursor = end;
                    }
                    if cursor != body.len() {
                        return Err(ValueCodecError::TrailingBytes {
                            declared: cursor,
                            actual: body.len(),
                        });
                    }
                }
                TypeDescriptorKind::Map { .. } => {
                    let count = decode_constructed_count(body)?;
                    let mut cursor = 4;
                    for index in 0..count {
                        let key_path = [CollectionValuePathSegment::MapKey(index)];
                        let (key, key_end) = take_constructed_child(body, cursor, &key_path)?;
                        path.push(CollectionValuePathSegment::MapKey(index));
                        let key_result = preflight_orv5_child(key, budget, path, policy);
                        path.pop();
                        key_result?;
                        let value_path = [CollectionValuePathSegment::MapValue(index)];
                        let (value, value_end) =
                            take_constructed_child(body, key_end, &value_path)?;
                        path.push(CollectionValuePathSegment::MapValue(index));
                        let value_result = preflight_orv5_child(value, budget, path, policy);
                        path.pop();
                        value_result?;
                        cursor = value_end;
                    }
                    if cursor != body.len() {
                        return Err(ValueCodecError::TrailingBytes {
                            declared: cursor,
                            actual: body.len(),
                        });
                    }
                }
                _ => return Err(ValueCodecError::UnsupportedConstructedDescriptor { descriptor }),
            }
        }
        RECORD_TAG => preflight_orv5_record_tree(payload, budget, path, policy)?,
        _ => {}
    }
    Ok(())
}

fn preflight_orv5_option_chain(
    mut body: &[u8],
    budget: &mut NodeBudget,
    path: &mut Vec<CollectionValuePathSegment>,
    policy: InvocationCarrierPreflightPolicy<'_>,
) -> Result<(), ValueCodecError> {
    let path_start = path.len();
    loop {
        let Some((&presence, remaining)) = body.split_first() else {
            return Err(option_chain_body_error(
                path,
                path_start,
                ValueCodecError::TruncatedCollectionEntry { path: path.clone() },
            ));
        };
        match presence {
            0 => {
                if !remaining.is_empty() {
                    return Err(option_chain_body_error(
                        path,
                        path_start,
                        ValueCodecError::TrailingBytes {
                            declared: 1,
                            actual: body.len(),
                        },
                    ));
                }
                path.truncate(path_start);
                return Ok(());
            }
            1 => {
                let mut child_path = path.clone();
                child_path.push(CollectionValuePathSegment::OptionChild);
                let (child, consumed) = take_constructed_child(remaining, 0, &child_path)
                    .map_err(|source| wrap_preflight_child_error(&child_path, source))?;
                if consumed != remaining.len() {
                    return Err(option_chain_body_error(
                        path,
                        path_start,
                        ValueCodecError::TrailingBytes {
                            declared: consumed + 1,
                            actual: body.len(),
                        },
                    ));
                }
                let (tag, type_id, payload) = decode_envelope(child, CONSTRUCTED_MARKER)
                    .map_err(|source| wrap_preflight_child_error(&child_path, source))?;
                if tag == CONSTRUCTED_TAG {
                    if type_id.to_bytes() != [0; 16] {
                        return Err(wrap_preflight_child_error(
                            &child_path,
                            ValueCodecError::ConstructedTypeIdentityNotZero { identity: type_id },
                        ));
                    }
                    let (descriptor, child_body) = decode_constructed_descriptor(payload)
                        .map_err(|source| wrap_preflight_child_error(&child_path, source))?;
                    if matches!(descriptor.kind(), TypeDescriptorKind::Option(_)) {
                        budget.increment()?;
                        path.push(CollectionValuePathSegment::OptionChild);
                        body = child_body;
                        continue;
                    }
                }
                path.push(CollectionValuePathSegment::OptionChild);
                let result = preflight_orv5_child(child, budget, path, policy);
                path.truncate(path_start);
                return result;
            }
            value => {
                return Err(option_chain_body_error(
                    path,
                    path_start,
                    ValueCodecError::InvalidOptionPresence { value },
                ));
            }
        }
    }
}

fn option_chain_body_error(
    path: &[CollectionValuePathSegment],
    path_start: usize,
    source: ValueCodecError,
) -> ValueCodecError {
    if path.len() == path_start {
        source
    } else {
        wrap_preflight_child_error(path, source)
    }
}

fn wrap_preflight_child_error(
    path: &[CollectionValuePathSegment],
    source: ValueCodecError,
) -> ValueCodecError {
    if is_global_node_limit(&source) || matches!(source, ValueCodecError::ConstructedChild { .. }) {
        source
    } else {
        ValueCodecError::ConstructedChild {
            path: path.to_vec(),
            source: Box::new(source),
        }
    }
}

fn preflight_orv5_child(
    encoded: &[u8],
    budget: &mut NodeBudget,
    path: &mut Vec<CollectionValuePathSegment>,
    policy: InvocationCarrierPreflightPolicy<'_>,
) -> Result<(), ValueCodecError> {
    match preflight_orv5_envelope(encoded, budget, path, policy) {
        Err(error)
            if is_global_node_limit(&error)
                || matches!(error, ValueCodecError::ConstructedChild { .. }) =>
        {
            Err(error)
        }
        Err(source) => Err(ValueCodecError::ConstructedChild {
            path: path.clone(),
            source: Box::new(source),
        }),
        Ok(()) => Ok(()),
    }
}

fn is_global_node_limit(error: &ValueCodecError) -> bool {
    matches!(
        error,
        ValueCodecError::CollectionValue {
            source: CollectionValueError::TooManyNodes { .. },
        } | ValueCodecError::InvocationCarrier {
            source: InvocationCarrierCodecError::TooManyNodes { .. },
            ..
        }
    )
}

fn preflight_orv5_envelope(
    encoded: &[u8],
    budget: &mut NodeBudget,
    path: &mut Vec<CollectionValuePathSegment>,
    policy: InvocationCarrierPreflightPolicy<'_>,
) -> Result<(), ValueCodecError> {
    let (allow_set, tag, type_id, payload) = decode_constructed_envelope(encoded)?;
    if let InvocationCarrierPreflightPolicy::Reject { outer, path } = policy
        && invocation_carrier_by_id(type_id).is_some()
    {
        return Err(ValueCodecError::InvocationCarrier {
            carrier: outer,
            source: InvocationCarrierCodecError::NestedCarrier {
                path: path.clone(),
                carrier: type_id,
            },
        });
    }
    if tag == CONSTRUCTED_TAG && type_id.to_bytes() != [0; 16] {
        return Err(ValueCodecError::ConstructedTypeIdentityNotZero { identity: type_id });
    }
    preflight_orv5_tree(payload, tag, budget, path, policy, allow_set)
}

fn preflight_orv5_record_tree(
    payload: &[u8],
    budget: &mut NodeBudget,
    path: &mut Vec<CollectionValuePathSegment>,
    policy: InvocationCarrierPreflightPolicy<'_>,
) -> Result<(), ValueCodecError> {
    if payload.len() < 4 {
        return Err(ValueCodecError::TruncatedPayload {
            declared: 4,
            actual: payload.len(),
        });
    }
    let count = u32::from_be_bytes(payload[..4].try_into().expect("count length checked")) as usize;
    let mut cursor = 4;
    for ordinal in 0..count {
        let remaining = payload.len() - cursor;
        if remaining < RECORD_FIELD_HEADER_LENGTH {
            return Err(ValueCodecError::TruncatedRecordFieldHeader {
                ordinal,
                actual: remaining,
            });
        }
        let field = FieldId::from_bytes(
            payload[cursor..cursor + 16]
                .try_into()
                .expect("field header length checked"),
        );
        let declared = u32::from_be_bytes(
            payload[cursor + 16..cursor + RECORD_FIELD_HEADER_LENGTH]
                .try_into()
                .expect("field header length checked"),
        ) as usize;
        cursor += RECORD_FIELD_HEADER_LENGTH;
        let remaining = payload.len() - cursor;
        if declared < HEADER_LENGTH || declared > remaining {
            return Err(ValueCodecError::InvalidRecordFieldLength {
                ordinal,
                declared,
                remaining,
            });
        }
        let end = cursor + declared;
        path.push(CollectionValuePathSegment::RecordField(field));
        let result = preflight_orv5_child(&payload[cursor..end], budget, path, policy);
        path.pop();
        result?;
        cursor = end;
    }
    if cursor != payload.len() {
        return Err(ValueCodecError::TrailingBytes {
            declared: cursor,
            actual: payload.len(),
        });
    }
    Ok(())
}

fn encode_record_value(
    active: &ActiveDatabaseRevision,
    value: &RecordValue,
) -> Result<Vec<u8>, ValueCodecError> {
    encode_record_value_with_marker(active, value, ACTIVE_MARKER)
}

fn encode_record_value_with_marker(
    active: &ActiveDatabaseRevision,
    value: &RecordValue,
    marker: &[u8; 4],
) -> Result<Vec<u8>, ValueCodecError> {
    let definition = active
        .catalogue()
        .record_value_type_by_id(value.record_type())
        .ok_or(ValueCodecError::InactiveRecordType {
            record_type: value.record_type(),
        })?;
    if definition.fields().len() != value.fields().len() {
        return Err(ValueCodecError::RecordValueNotActive {
            record_type: value.record_type(),
        });
    }
    let checked = RecordValue::new(
        active,
        value.record_type(),
        definition
            .fields()
            .iter()
            .zip(value.fields())
            .map(|(field, value)| (field.name().to_owned(), value.clone())),
    )
    .map_err(|_| ValueCodecError::RecordValueNotActive {
        record_type: value.record_type(),
    })?;
    if checked != *value {
        return Err(ValueCodecError::RecordValueNotActive {
            record_type: value.record_type(),
        });
    }

    let field_count =
        u32::try_from(definition.fields().len()).map_err(|_| ValueCodecError::PayloadTooLarge {
            actual: usize::MAX,
            maximum: PAYLOAD_LIMIT,
        })?;
    let mut payload = Vec::new();
    payload.extend_from_slice(&field_count.to_be_bytes());
    for (field, value) in definition.fields().iter().zip(value.fields()) {
        let encoded =
            encode_record_field_value(active, definition.id(), field.descriptor(), value, marker)?;
        let encoded_length =
            u32::try_from(encoded.len()).map_err(|_| ValueCodecError::PayloadTooLarge {
                actual: encoded.len(),
                maximum: PAYLOAD_LIMIT,
            })?;
        let next_length = payload
            .len()
            .checked_add(RECORD_FIELD_HEADER_LENGTH)
            .and_then(|length| length.checked_add(encoded.len()))
            .ok_or(ValueCodecError::PayloadTooLarge {
                actual: usize::MAX,
                maximum: PAYLOAD_LIMIT,
            })?;
        require_payload_limit(next_length)?;
        payload.reserve(RECORD_FIELD_HEADER_LENGTH + encoded.len());
        payload.extend_from_slice(&field.id().to_bytes());
        payload.extend_from_slice(&encoded_length.to_be_bytes());
        payload.extend_from_slice(&encoded);
    }
    Ok(encode_with_marker(
        marker,
        RECORD_TAG,
        value.record_type(),
        &payload,
    ))
}

fn decode_record_value(
    active: &ActiveDatabaseRevision,
    record_type: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    decode_record_value_with_marker(active, record_type, payload, ACTIVE_MARKER)
}

fn decode_record_value_with_marker(
    active: &ActiveDatabaseRevision,
    record_type: TypeId,
    payload: &[u8],
    marker: &[u8; 4],
) -> Result<RuntimeValue, ValueCodecError> {
    let definition = active
        .catalogue()
        .record_value_type_by_id(record_type)
        .ok_or(ValueCodecError::InactiveRecordType { record_type })?;
    if payload.len() < 4 {
        return Err(ValueCodecError::TruncatedPayload {
            declared: 4,
            actual: payload.len(),
        });
    }
    let field_count = u32::from_be_bytes(payload[..4].try_into().expect("length checked")) as usize;
    if field_count != definition.fields().len() {
        return Err(ValueCodecError::WrongRecordFieldCount {
            expected: definition.fields().len(),
            actual: field_count,
        });
    }
    let minimum = 4_usize
        .checked_add(
            field_count
                .checked_mul(RECORD_FIELD_HEADER_LENGTH + HEADER_LENGTH)
                .ok_or(ValueCodecError::PayloadTooLarge {
                    actual: usize::MAX,
                    maximum: PAYLOAD_LIMIT,
                })?,
        )
        .ok_or(ValueCodecError::PayloadTooLarge {
            actual: usize::MAX,
            maximum: PAYLOAD_LIMIT,
        })?;
    if payload.len() < minimum {
        return Err(ValueCodecError::TruncatedPayload {
            declared: minimum,
            actual: payload.len(),
        });
    }

    let mut cursor = 4;
    let mut fields = Vec::with_capacity(field_count);
    for (ordinal, definition_field) in definition.fields().iter().enumerate() {
        let remaining = payload.len() - cursor;
        if remaining < RECORD_FIELD_HEADER_LENGTH {
            return Err(ValueCodecError::TruncatedRecordFieldHeader {
                ordinal,
                actual: remaining,
            });
        }
        let field_end = cursor + 16;
        let field = FieldId::from_bytes(
            payload[cursor..field_end]
                .try_into()
                .expect("minimum record entry length checked"),
        );
        if field != definition_field.id() {
            return Err(ValueCodecError::WrongRecordFieldIdentity {
                ordinal,
                expected: definition_field.id(),
                actual: field,
            });
        }
        cursor = field_end;
        let length_end = cursor + 4;
        let declared = u32::from_be_bytes(
            payload[cursor..length_end]
                .try_into()
                .expect("minimum record entry length checked"),
        ) as usize;
        cursor = length_end;
        let remaining = payload.len() - cursor;
        if declared < HEADER_LENGTH || declared > remaining {
            return Err(ValueCodecError::InvalidRecordFieldLength {
                ordinal,
                declared,
                remaining,
            });
        }
        let encoded_end = cursor + declared;
        let encoded = &payload[cursor..encoded_end];
        let (tag, type_id, field_payload) = decode_envelope(encoded, marker)?;
        require_record_field_wire_type(
            active,
            definition_field.descriptor(),
            ordinal,
            tag,
            type_id,
        )?;
        let value = decode_record_field_value(
            active,
            definition_field.descriptor(),
            tag,
            type_id,
            field_payload,
            marker,
        )?;
        fields.push((definition_field.name().to_owned(), value));
        cursor = encoded_end;
    }
    if cursor != payload.len() {
        return Err(ValueCodecError::TrailingBytes {
            declared: cursor,
            actual: payload.len(),
        });
    }
    RecordValue::new(active, record_type, fields)
        .map(RuntimeValue::Record)
        .map_err(|_| ValueCodecError::RecordValueNotActive { record_type })
}

fn decode_active_non_record_value(
    active: &ActiveDatabaseRevision,
    tag: u8,
    type_id: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    match tag {
        NULL_ENUM_TAG => {
            require_empty_payload(tag, payload)?;
            require_active_enum_type_for_revision(active, type_id)?;
            RuntimeValue::null(ResolvedType::named(type_id))
                .map_err(|_| ValueCodecError::UnsupportedValue)
        }
        ENUM_TAG => {
            require_payload_limit(payload.len())?;
            let label = std::str::from_utf8(payload).map_err(|_| ValueCodecError::InvalidUtf8)?;
            validate_active_enum_value(active, type_id, label).map(RuntimeValue::Enum)
        }
        _ => decode_catalogue_value_parts(active.catalogue(), tag, type_id, payload),
    }
}

fn decode_catalogue_value_parts(
    catalogue: &CatalogueSnapshot,
    tag: u8,
    type_id: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    match tag {
        NULL_ENUM_TAG => {
            require_empty_payload(tag, payload)?;
            require_active_enum_type(catalogue, type_id)?;
            RuntimeValue::null(ResolvedType::named(type_id))
                .map_err(|_| ValueCodecError::UnsupportedValue)
        }
        ENUM_TAG => {
            require_payload_limit(payload.len())?;
            let label = std::str::from_utf8(payload).map_err(|_| ValueCodecError::InvalidUtf8)?;
            validate_enum_value(catalogue, type_id, label).map(RuntimeValue::Enum)
        }
        _ => decode_non_enum_value(tag, type_id, payload),
    }
}

fn require_record_field_wire_type(
    active: &ActiveDatabaseRevision,
    expected: &TypeDescriptor,
    ordinal: usize,
    tag: u8,
    actual: TypeId,
) -> Result<(), ValueCodecError> {
    let expected_type = match expected.kind() {
        TypeDescriptorKind::Named(type_id) => type_id,
        TypeDescriptorKind::Reference(_)
        | TypeDescriptorKind::List(_)
        | TypeDescriptorKind::Set(_)
        | TypeDescriptorKind::Map { .. }
        | TypeDescriptorKind::Option(_)
        | TypeDescriptorKind::Stream(_) => {
            return Err(ValueCodecError::WrongRecordFieldType {
                ordinal,
                expected: expected.clone(),
                tag,
                actual,
            });
        }
    };
    let matches = if application_record_field_target(active, expected).is_some() {
        actual == expected_type && tag == RECORD_TAG
    } else {
        active
            .record_value_field_descriptor_runtime_type(expected)
            .is_some_and(|runtime| {
                actual == expected_type
                    && runtime.legacy_scalar().map_or(tag == ENUM_TAG, |scalar| {
                        supported_scalar_tag_from_scalar(scalar) == Some(tag)
                    })
            })
    };
    if matches {
        Ok(())
    } else {
        Err(ValueCodecError::WrongRecordFieldType {
            ordinal,
            expected: expected.clone(),
            tag,
            actual,
        })
    }
}

fn encode_record_field_value(
    active: &ActiveDatabaseRevision,
    record_type: TypeId,
    declared: &TypeDescriptor,
    value: &RuntimeValue,
    marker: &[u8; 4],
) -> Result<Vec<u8>, ValueCodecError> {
    let TypeDescriptorKind::Named(type_id) = declared.kind() else {
        return Err(ValueCodecError::UnsupportedValue);
    };
    if let Some(expected_record_type) = application_record_field_target(active, declared) {
        let RuntimeValue::Record(value) = value else {
            return Err(ValueCodecError::RecordValueNotActive { record_type });
        };
        if value.record_type() != expected_record_type {
            return Err(ValueCodecError::RecordValueNotActive { record_type });
        }
        return encode_record_value_with_marker(active, value, marker);
    }
    let expected = active
        .record_value_field_descriptor_runtime_type(declared)
        .ok_or(ValueCodecError::UnsupportedValue)?;
    match expected {
        ResolvedType::Scalar(_) => {
            if value.runtime_type() != RuntimeType::Flat(expected) {
                return Err(ValueCodecError::RecordValueNotActive { record_type });
            }
            let mut encoded = encode_value(value)?;
            encoded[..marker.len()].copy_from_slice(marker);
            encoded[5..21].copy_from_slice(&type_id.to_bytes());
            Ok(encoded)
        }
        ResolvedType::Named(enum_type) => {
            let RuntimeValue::Enum(value) = value else {
                return Err(ValueCodecError::RecordValueNotActive { record_type });
            };
            validate_active_enum_value(active, enum_type, value.label())?;
            encode_variable(ENUM_TAG, enum_type, value.label().as_bytes()).map(|mut encoded| {
                encoded[..marker.len()].copy_from_slice(marker);
                encoded
            })
        }
        ResolvedType::Value(_) | ResolvedType::Reference { .. } => {
            Err(ValueCodecError::UnsupportedValue)
        }
    }
}

fn decode_record_field_value(
    active: &ActiveDatabaseRevision,
    declared: &TypeDescriptor,
    tag: u8,
    type_id: TypeId,
    payload: &[u8],
    marker: &[u8; 4],
) -> Result<RuntimeValue, ValueCodecError> {
    if application_record_field_target(active, declared).is_some() {
        return decode_record_value_with_marker(active, type_id, payload, marker);
    }
    let expected = active
        .record_value_field_descriptor_runtime_type(declared)
        .ok_or(ValueCodecError::UnsupportedValue)?;
    match expected {
        ResolvedType::Scalar(scalar) => {
            let canonical_type =
                supported_scalar_type_id(scalar).ok_or(ValueCodecError::UnsupportedValue)?;
            decode_non_enum_value(tag, canonical_type, payload)
        }
        ResolvedType::Named(enum_type) => {
            require_payload_limit(payload.len())?;
            let label = std::str::from_utf8(payload).map_err(|_| ValueCodecError::InvalidUtf8)?;
            validate_active_enum_value(active, enum_type, label).map(RuntimeValue::Enum)
        }
        ResolvedType::Value(_) | ResolvedType::Reference { .. } => {
            Err(ValueCodecError::UnsupportedValue)
        }
    }
}

fn application_record_field_target(
    active: &ActiveDatabaseRevision,
    descriptor: &TypeDescriptor,
) -> Option<TypeId> {
    let TypeDescriptorKind::Named(type_id) = descriptor.kind() else {
        return None;
    };
    active
        .catalogue()
        .record_value_type_by_id(type_id)
        .is_some()
        .then_some(type_id)
}

fn decode_non_enum_value(
    tag: u8,
    type_id: TypeId,
    payload: &[u8],
) -> Result<RuntimeValue, ValueCodecError> {
    match tag {
        NULL_SCALAR_TAG => {
            require_empty_payload(tag, payload)?;
            let scalar =
                supported_scalar_from_type_id(type_id).ok_or(ValueCodecError::WrongType {
                    tag,
                    actual: type_id,
                })?;
            RuntimeValue::null(ResolvedType::scalar(scalar))
                .map_err(|_| ValueCodecError::UnsupportedValue)
        }
        NULL_REFERENCE_TAG => {
            require_empty_payload(tag, payload)?;
            require_reference_target(type_id)?;
            RuntimeValue::null(ResolvedType::reference(type_id))
                .map_err(|_| ValueCodecError::UnsupportedValue)
        }
        BOOLEAN_TAG => {
            require_type(tag, type_id, BOOLEAN_TYPE_ID)?;
            let [value] = payload else {
                return Err(ValueCodecError::WrongPayloadLength {
                    tag,
                    expected: 1,
                    actual: payload.len(),
                });
            };
            match value {
                0 => Ok(RuntimeValue::Boolean(false)),
                1 => Ok(RuntimeValue::Boolean(true)),
                value => Err(ValueCodecError::InvalidBoolean { value: *value }),
            }
        }
        INTEGER_TAG => {
            require_type(tag, type_id, INTEGER_TYPE_ID)?;
            let payload = require_fixed_payload::<4>(tag, payload)?;
            Ok(RuntimeValue::Integer(i32::from_be_bytes(payload)))
        }
        BIGINT_TAG => {
            require_type(tag, type_id, BIGINT_TYPE_ID)?;
            let payload = require_fixed_payload::<8>(tag, payload)?;
            Ok(RuntimeValue::BigInt(i64::from_be_bytes(payload)))
        }
        FLOAT_TAG => {
            require_type(tag, type_id, FLOAT_TYPE_ID)?;
            let payload = require_fixed_payload::<8>(tag, payload)?;
            let bits = u64::from_be_bytes(payload);
            let value = f64::from_bits(bits);
            if bits == (-0.0_f64).to_bits() || !value.is_finite() {
                return Err(ValueCodecError::NonCanonicalFloat);
            }
            RuntimeFloat::new(value)
                .map(RuntimeValue::Float)
                .map_err(|_| ValueCodecError::NonCanonicalFloat)
        }
        TEXT_TAG => {
            require_type(tag, type_id, CHARACTER_LARGE_OBJECT_TYPE_ID)?;
            require_payload_limit(payload.len())?;
            String::from_utf8(payload.to_vec())
                .map(RuntimeValue::Text)
                .map_err(|_| ValueCodecError::InvalidUtf8)
        }
        BYTES_TAG => {
            require_type(tag, type_id, BINARY_LARGE_OBJECT_TYPE_ID)?;
            require_payload_limit(payload.len())?;
            Ok(RuntimeValue::Bytes(payload.to_vec()))
        }
        REFERENCE_TAG => {
            require_reference_target(type_id)?;
            let object = require_fixed_payload::<16>(tag, payload)?;
            Ok(RuntimeValue::Reference {
                target: type_id,
                object: ObjectId::from_bytes(object),
            })
        }
        tag => Err(ValueCodecError::UnknownTag { tag }),
    }
}

fn encode(tag: u8, type_id: TypeId, payload: &[u8]) -> Vec<u8> {
    encode_with_marker(MARKER, tag, type_id, payload)
}

fn encode_with_marker(marker: &[u8; 4], tag: u8, type_id: TypeId, payload: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(HEADER_LENGTH + payload.len());
    encoded.extend_from_slice(marker);
    encoded.push(tag);
    encoded.extend_from_slice(&type_id.to_bytes());
    encoded.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    encoded.extend_from_slice(payload);
    encoded
}

fn with_catalogue_marker(mut encoded: Vec<u8>) -> Vec<u8> {
    encoded[..CATALOGUE_MARKER.len()].copy_from_slice(CATALOGUE_MARKER);
    encoded
}

fn with_active_marker(mut encoded: Vec<u8>) -> Vec<u8> {
    encoded[..ACTIVE_MARKER.len()].copy_from_slice(ACTIVE_MARKER);
    encoded
}

fn encode_variable(tag: u8, type_id: TypeId, payload: &[u8]) -> Result<Vec<u8>, ValueCodecError> {
    require_payload_limit(payload.len())?;
    Ok(encode(tag, type_id, payload))
}

fn require_payload_limit(actual: usize) -> Result<(), ValueCodecError> {
    if actual <= PAYLOAD_LIMIT {
        Ok(())
    } else {
        Err(ValueCodecError::PayloadTooLarge {
            actual,
            maximum: PAYLOAD_LIMIT,
        })
    }
}

fn decode_envelope<'a>(
    encoded: &'a [u8],
    marker: &[u8; 4],
) -> Result<(u8, TypeId, &'a [u8]), ValueCodecError> {
    if encoded.len() < HEADER_LENGTH {
        return Err(ValueCodecError::TruncatedHeader {
            actual: encoded.len(),
        });
    }
    if &encoded[..marker.len()] != marker {
        return Err(ValueCodecError::InvalidMarker);
    }
    let tag = encoded[4];
    let type_id = TypeId::from_bytes(encoded[5..21].try_into().expect("header length checked"));
    let declared =
        u32::from_be_bytes(encoded[21..25].try_into().expect("header length checked")) as usize;
    require_payload_limit(declared)?;
    let actual = encoded.len() - HEADER_LENGTH;
    if actual < declared {
        return Err(ValueCodecError::TruncatedPayload { declared, actual });
    }
    if actual > declared {
        return Err(ValueCodecError::TrailingBytes { declared, actual });
    }
    Ok((tag, type_id, &encoded[HEADER_LENGTH..]))
}

fn decode_constructed_envelope(
    encoded: &[u8],
) -> Result<(bool, u8, TypeId, &[u8]), ValueCodecError> {
    if encoded.len() < HEADER_LENGTH {
        return Err(ValueCodecError::TruncatedHeader {
            actual: encoded.len(),
        });
    }
    let is_set_version = &encoded[..SET_MARKER.len()] == SET_MARKER;
    if !is_set_version && &encoded[..CONSTRUCTED_MARKER.len()] != CONSTRUCTED_MARKER {
        return Err(ValueCodecError::InvalidMarker);
    }
    let marker = if is_set_version {
        SET_MARKER
    } else {
        CONSTRUCTED_MARKER
    };
    let (tag, type_id, payload) = decode_envelope(encoded, marker)?;
    Ok((is_set_version, tag, type_id, payload))
}

fn require_active_enum_type(
    catalogue: &CatalogueSnapshot,
    enum_type: TypeId,
) -> Result<(), ValueCodecError> {
    catalogue
        .enum_type_by_id(enum_type)
        .map(|_| ())
        .ok_or(ValueCodecError::InactiveEnumType { enum_type })
}

fn validate_enum_value(
    catalogue: &CatalogueSnapshot,
    enum_type: TypeId,
    label: &str,
) -> Result<EnumValue, ValueCodecError> {
    EnumValue::new(catalogue, enum_type, label).map_err(|error| match error {
        EnumValueError::UnknownType { enum_type } => {
            ValueCodecError::InactiveEnumType { enum_type }
        }
        EnumValueError::UndeclaredLabel { enum_type, label } => {
            ValueCodecError::UndeclaredEnumLabel { enum_type, label }
        }
        _ => ValueCodecError::UnsupportedValue,
    })
}

fn require_active_enum_type_for_revision(
    active: &ActiveDatabaseRevision,
    enum_type: TypeId,
) -> Result<(), ValueCodecError> {
    if active.catalogue().enum_type_by_id(enum_type).is_some()
        || active
            .catalogue_hash_context()
            .standard()
            .is_some_and(|standard| standard.catalogue().enum_type_by_id(enum_type).is_some())
    {
        Ok(())
    } else {
        Err(ValueCodecError::InactiveEnumType { enum_type })
    }
}

fn validate_active_enum_value(
    active: &ActiveDatabaseRevision,
    enum_type: TypeId,
    label: &str,
) -> Result<EnumValue, ValueCodecError> {
    if active.catalogue().enum_type_by_id(enum_type).is_some() {
        return validate_enum_value(active.catalogue(), enum_type, label);
    }
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or(ValueCodecError::InactiveEnumType { enum_type })?
        .catalogue();
    validate_enum_value(standard, enum_type, label)
}

fn require_type(tag: u8, actual: TypeId, expected: TypeId) -> Result<(), ValueCodecError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ValueCodecError::WrongType { tag, actual })
    }
}

fn require_empty_payload(tag: u8, payload: &[u8]) -> Result<(), ValueCodecError> {
    if payload.is_empty() {
        Ok(())
    } else {
        Err(ValueCodecError::WrongPayloadLength {
            tag,
            expected: 0,
            actual: payload.len(),
        })
    }
}

fn require_reference_target(target: TypeId) -> Result<(), ValueCodecError> {
    if STANDARD_TYPE_IDS.contains(&target) {
        Err(ValueCodecError::StandardTypeAsReference { target })
    } else {
        Ok(())
    }
}

fn supported_scalar_type_id(scalar: StandardScalar) -> Option<TypeId> {
    SUPPORTED_SCALAR_TYPES
        .iter()
        .find_map(|(type_id, candidate, _)| (*candidate == scalar).then_some(*type_id))
}

fn supported_scalar_from_type_id(type_id: TypeId) -> Option<StandardScalar> {
    SUPPORTED_SCALAR_TYPES
        .iter()
        .find_map(|(candidate, scalar, _)| (*candidate == type_id).then_some(*scalar))
}

fn supported_scalar_tag_from_scalar(scalar: StandardScalar) -> Option<u8> {
    SUPPORTED_SCALAR_TYPES
        .iter()
        .find_map(|(_, candidate, tag)| (*candidate == scalar).then_some(*tag))
}

fn require_fixed_payload<const LENGTH: usize>(
    tag: u8,
    payload: &[u8],
) -> Result<[u8; LENGTH], ValueCodecError> {
    payload
        .try_into()
        .map_err(|_| ValueCodecError::WrongPayloadLength {
            tag,
            expected: LENGTH,
            actual: payload.len(),
        })
}

#[cfg(test)]
mod tests;
