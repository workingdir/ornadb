//! Canonical runtime values and the bounded authenticated raw-call protocol.

mod carrier;
mod frame;
mod rows;
mod session;
use carrier::{decode_invocation_carrier, encode_invocation_carrier};

pub use frame::{
    CallArgument, CallFailure, Channel, ClientAction, ClientFrame, ConnectionError, Event,
    EventRecord, FrameCodecError, InvocationClient, InvocationClientError,
    InvocationClientResponse, InvocationEventBatch, InvocationEventRecord, MAX_CHANNEL_WINDOW,
    MAX_FRAME_PAYLOAD_LENGTH, MAX_RESOURCE_ARGUMENTS, MAX_RESOURCE_BATCH_ITEMS,
    MAX_RESOURCE_TOTAL_ITEMS, MAX_RESOURCE_WINDOW, ProtocolConnection, RawCall, RawCallClient,
    RawCallClientError, RawCallClientResponse, ResourceAccepted, ResourceArgument, ResourceCancel,
    ResourceCancelFrame, ResourceCancelReason, ResourceCancellationCode, ResourceCancelled,
    ResourceCancelledFrame, ResourceClientFrame, ResourceCompleted, ResourceCompletedFrame,
    ResourceConnectionError, ResourceCredit, ResourceFailed, ResourceFailedFrame,
    ResourceFrameDisposition, ResourceKind, ResourceProtocolConnection, ResourceRequest,
    ResourceRequestFrame, ResourceServerFrame, ResourceValues, ResourceValuesFrame,
    ResourceWindowUpdate, ResourceWindowUpdateFrame, RetainedInvokeRequest, ServerAction,
    ServerFrame, decode_active_client_frame, decode_active_server_frame,
    decode_catalogue_client_frame, decode_catalogue_server_frame, decode_client_frame,
    decode_constructed_client_frame, decode_constructed_invocation_event_frame,
    decode_constructed_server_frame, decode_invocation_event_batch, decode_invoke_request,
    decode_registered_client_frame, decode_registered_server_frame, decode_resource_accepted,
    decode_resource_cancel, decode_resource_cancelled, decode_resource_client_frame,
    decode_resource_completed, decode_resource_failed, decode_resource_request,
    decode_resource_server_frame, decode_resource_values, decode_resource_window_update,
    decode_retained_invoke_request, decode_server_frame, encode_active_client_frame,
    encode_active_server_frame, encode_catalogue_client_frame, encode_catalogue_server_frame,
    encode_client_frame, encode_constructed_client_frame, encode_constructed_server_frame,
    encode_invocation_event_batch, encode_invoke_request, encode_registered_client_frame,
    encode_registered_server_frame, encode_resource_accepted, encode_resource_cancel,
    encode_resource_cancelled, encode_resource_client_frame, encode_resource_completed,
    encode_resource_failed, encode_resource_request, encode_resource_server_frame,
    encode_resource_values, encode_resource_window_update, encode_server_frame,
};
pub use rows::{RowsCodecError, decode_rows, encode_rows, encode_rows_value};

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
            let checked = construct_opaque_value(
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

fn construct_opaque_value(
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    opaque_type: TypeId,
    payload: &[u8],
) -> Result<OpaqueValue, OpaqueValueError> {
    if opaque_type == orna_core::system::SYS_SOURCE_FUNCTION_TYPE_ID {
        OpaqueValue::new_source_metadata_carrier(active, opaque_type, payload)
    } else {
        OpaqueValue::new(active, registry, opaque_type, payload)
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
        OPAQUE_TAG => construct_opaque_value(active, registry, type_id, payload)
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
