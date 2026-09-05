//! Bounded, deterministic `orna.present.v1` envelopes.
//!
//! Rich Present and Patch semantics are structurally validated here. Applying
//! patches and renderer-specific interpretation remain responsibilities of a
//! higher presentation layer.

use std::{collections::BTreeMap, fmt};

use num_bigint::{BigInt, Sign};
use num_traits::ToPrimitive;
use orna_foundation_v1::{CanonicalSnapshot, CanonicalValue, OvbRaw};
use sha2::{Digest, Sha256};

pub const PROFILE: &str = "orna.present.v1";
pub const VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_message_bytes: usize,
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_collection_items: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: 16 * 1024 * 1024,
            max_depth: 64,
            max_nodes: 100_000,
            max_collection_items: 100_000,
        }
    }
}

impl Limits {
    pub fn validate(self) -> Result<Self> {
        if self.max_message_bytes == 0
            || self.max_depth == 0
            || self.max_nodes == 0
            || self.max_collection_items == 0
        {
            return Err(Error::Limit);
        }
        Ok(self)
    }
}

/// Deliberately payload-free diagnostics suitable for a network boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Truncated,
    TrailingBytes,
    Limit,
    Unsupported,
    NonCanonical,
    InvalidEnvelope,
    InvalidMessage,
    UnknownMandatoryExtension,
    InvalidValue,
}

impl Error {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Limit => "wire.limit",
            Self::Unsupported | Self::UnknownMandatoryExtension => "wire.unsupported",
            _ => "wire.invalid_message",
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// Computes the canonical REQUEST-1 fingerprint for an envelope.
///
/// The digest covers the session identity, message type, watch identity, and
/// complete body. For `Event` and `Eval`, their redundant transmitted
/// fingerprint field is omitted; every retained extension remains covered.
pub fn canonical_request_fingerprint(
    session_id: [u8; 16],
    envelope: &Envelope,
    limits: Limits,
) -> Result<[u8; 32]> {
    // Keep the helper's accepted public values identical to envelope encoding,
    // including bounds and validation of opaque typed fields and extensions.
    let _ = envelope.encode(limits)?;
    let mut body = envelope.message.body(&envelope.extensions)?;
    if matches!(
        envelope.message,
        Message::Event { .. } | Message::Eval { .. }
    ) {
        let Node::Map(fields) = &mut body else {
            return Err(Error::InvalidMessage);
        };
        fields.retain(|(key, _)| u64_value(key).ok() != Some(3));
    }
    let input = encode_node(&Node::Array(vec![
        Node::Bytes(session_id.to_vec()),
        uint(envelope.message.code()),
        option_bytes(envelope.watch),
        body,
    ]))?;
    let mut hasher = Sha256::new();
    hasher.update(b"orna.request.v1\0");
    hasher.update(input);
    Ok(hasher.finalize().into())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Envelope {
    pub request: Option<[u8; 16]>,
    pub watch: Option<[u8; 16]>,
    pub message: Message,
    /// Ignorable extension fields are retained so re-encoding keeps their
    /// canonical fingerprint contribution.
    pub extensions: BTreeMap<u16, ValueNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    Subscribe {
        resource: [u8; 16],
        presentation: PresentationContext,
    },
    Unsubscribe,
    Resync,
    Event {
        revision: u64,
        action: [u8; 16],
        value: CanonicalValue,
        fingerprint: [u8; 32],
    },
    Eval {
        source: String,
        database: DatabaseContext,
        presentation: PresentationContext,
        fingerprint: [u8; 32],
    },
    Watch {
        source: String,
        database: DatabaseContext,
        presentation: PresentationContext,
        refresh_floor: Option<Duration>,
    },
    Cancel {
        target_kind: TargetKind,
        target: [u8; 16],
    },
    RequestStatus {
        target: [u8; 16],
        fingerprint: [u8; 32],
    },
    Snapshot {
        revision: u64,
        present: PresentNode,
        snapshot: CanonicalSnapshot,
    },
    Delta {
        base_revision: u64,
        new_revision: u64,
        patches: PatchList,
        snapshot: CanonicalSnapshot,
    },
    Result {
        status: ResultStatus,
        value: Option<CanonicalValue>,
        fingerprint: [u8; 32],
        diagnostic: Option<Diagnostic>,
    },
    Diagnostic {
        diagnostic: Diagnostic,
        recoverable: Option<bool>,
    },
    RequestStatusResult {
        target: [u8; 16],
        state: RequestState,
        fingerprint: Option<[u8; 32]>,
        result: Option<ResultBody>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseContext {
    pub database: [u8; 16],
    pub snapshot: Option<CanonicalSnapshot>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentationContext {
    pub locale: String,
    pub timezone: Option<String>,
    pub width: Option<u64>,
    pub theme: String,
    pub supported_kinds: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Duration {
    pub floor_seconds: BigInt,
    pub nanosecond: u32,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetKind {
    Request,
    Watch,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultStatus {
    Success,
    Failure,
    Cancellation,
    RetainedWithoutValue,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestState {
    Unknown,
    Reserved,
    Running,
    Terminal,
    Orphaned,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentNode(ValueNode);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchList(ValueNode);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic(ValueNode);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultBody(ValueNode);

/// A canonical CBOR value accepted only after the configured bounds check.
/// It is intentionally opaque: callers cannot bypass protocol validation by
/// mutating a decoded structural node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValueNode(Node);
#[derive(Clone, Debug, Eq, PartialEq)]
enum Node {
    Null,
    Bool(bool),
    Int(BigInt),
    Float(u64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Node>),
    Map(Vec<(Node, Node)>),
    Tag(u64, Box<Node>),
}

impl Envelope {
    pub fn encode(&self, limits: Limits) -> Result<Vec<u8>> {
        limits.validate()?;
        self.validate()?;
        let body = self.message.body(&self.extensions)?;
        let root = Node::Map(vec![
            (uint(0), uint(VERSION)),
            (uint(1), uint(self.message.code())),
            (uint(2), option_bytes(self.request)),
            (uint(3), option_bytes(self.watch)),
            (uint(4), body),
        ]);
        let bytes = encode_node(&root)?;
        if bytes.len() > limits.max_message_bytes {
            return Err(Error::Limit);
        }
        // Validate public typed fields too, not only decoded wire values.
        let _ = Self::decode(&bytes, limits)?;
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8], limits: Limits) -> Result<Self> {
        limits.validate()?;
        if bytes.len() > limits.max_message_bytes {
            return Err(Error::Limit);
        }
        let mut reader = Reader::new(bytes, limits);
        let root = reader.node(0)?;
        if reader.at != bytes.len() {
            return Err(Error::TrailingBytes);
        }
        if encode_node(&root)? != bytes {
            return Err(Error::NonCanonical);
        }
        let envelope_map = map(&root).ok_or(Error::InvalidEnvelope)?;
        require_exact_keys(envelope_map, &[0, 1, 2, 3, 4], Error::InvalidEnvelope)?;
        let version = u64_value(field(envelope_map, 0)?)?;
        if version != VERSION {
            return Err(Error::InvalidEnvelope);
        }
        let code = u64_value(field(envelope_map, 1)?)?;
        if code > u16::MAX.into() {
            return Err(Error::InvalidEnvelope);
        }
        let request = nullable_bytes16(field(envelope_map, 2)?)?;
        let watch = nullable_bytes16(field(envelope_map, 3)?)?;
        let body = map(field(envelope_map, 4)?).ok_or(Error::InvalidMessage)?;
        let (message, extensions) = Message::decode(code as u16, request, watch, body)?;
        let envelope = Self {
            request,
            watch,
            message,
            extensions,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    fn validate(&self) -> Result<()> {
        self.message.validate_envelope(self.request, self.watch)
    }
}

impl Message {
    pub const fn code(&self) -> u64 {
        match self {
            Self::Subscribe { .. } => 0,
            Self::Unsubscribe => 1,
            Self::Resync => 2,
            Self::Event { .. } => 3,
            Self::Eval { .. } => 4,
            Self::Watch { .. } => 5,
            Self::Cancel { .. } => 6,
            Self::RequestStatus { .. } => 7,
            Self::Snapshot { .. } => 16,
            Self::Delta { .. } => 17,
            Self::Result { .. } => 18,
            Self::Diagnostic { .. } => 19,
            Self::RequestStatusResult { .. } => 20,
        }
    }
    fn validate_envelope(&self, request: Option<[u8; 16]>, watch: Option<[u8; 16]>) -> Result<()> {
        let (request_rule, watch_rule) = match self {
            Self::Subscribe { .. }
            | Self::Eval { .. }
            | Self::Watch { .. }
            | Self::Cancel { .. }
            | Self::RequestStatus { .. }
            | Self::Result { .. }
            | Self::RequestStatusResult { .. } => (1, 0),
            Self::Unsubscribe | Self::Resync | Self::Event { .. } => (1, 1),
            Self::Snapshot { .. } => (2, 1),
            Self::Delta { .. } => (0, 1),
            Self::Diagnostic { .. } => (2, 2),
        };
        if (request_rule == 0 && request.is_some())
            || (request_rule == 1 && request.is_none())
            || (watch_rule == 0 && watch.is_some())
            || (watch_rule == 1 && watch.is_none())
        {
            return Err(Error::InvalidEnvelope);
        }
        if let Self::Cancel {
            target_kind,
            target,
        } = self
            && *target_kind == TargetKind::Request
            && request == Some(*target)
        {
            return Err(Error::InvalidMessage);
        }
        if let Self::Delta {
            base_revision,
            new_revision,
            ..
        } = self
            && new_revision <= base_revision
        {
            return Err(Error::InvalidMessage);
        }
        if let Self::Result { status, value, .. } = self
            && (*status == ResultStatus::Success) != value.is_some()
        {
            return Err(Error::InvalidMessage);
        }
        Ok(())
    }
    fn body(&self, extensions: &BTreeMap<u16, ValueNode>) -> Result<Node> {
        let mut fields = match self {
            Self::Subscribe {
                resource,
                presentation,
            } => vec![
                (0, Node::Bytes(resource.to_vec())),
                (1, presentation.node()),
            ],
            Self::Unsubscribe | Self::Resync => vec![],
            Self::Event {
                revision,
                action,
                value,
                fingerprint,
            } => vec![
                (0, uint(*revision)),
                (1, Node::Bytes(action.to_vec())),
                (2, value_node(value)?),
                (3, Node::Bytes(fingerprint.to_vec())),
            ],
            Self::Eval {
                source,
                database,
                presentation,
                fingerprint,
            } => vec![
                (0, Node::Text(source.clone())),
                (1, database.node()),
                (2, presentation.node()),
                (3, Node::Bytes(fingerprint.to_vec())),
            ],
            Self::Watch {
                source,
                database,
                presentation,
                refresh_floor,
            } => {
                let mut x = vec![
                    (0, Node::Text(source.clone())),
                    (1, database.node()),
                    (2, presentation.node()),
                ];
                if let Some(duration) = refresh_floor {
                    x.push((3, duration.node()));
                }
                x
            }
            Self::Cancel {
                target_kind,
                target,
            } => vec![
                (0, uint(target_kind.code())),
                (1, Node::Bytes(target.to_vec())),
            ],
            Self::RequestStatus {
                target,
                fingerprint,
            } => vec![
                (0, Node::Bytes(target.to_vec())),
                (1, Node::Bytes(fingerprint.to_vec())),
            ],
            Self::Snapshot {
                revision,
                present,
                snapshot,
            } => vec![
                (0, uint(*revision)),
                (1, present.0.0.clone()),
                (2, snapshot_node(snapshot)?),
            ],
            Self::Delta {
                base_revision,
                new_revision,
                patches,
                snapshot,
            } => vec![
                (0, uint(*base_revision)),
                (1, uint(*new_revision)),
                (2, patches.0.0.clone()),
                (3, snapshot_node(snapshot)?),
            ],
            Self::Result {
                status,
                value,
                fingerprint,
                diagnostic,
            } => vec![
                (0, uint(status.code())),
                (
                    1,
                    value
                        .as_ref()
                        .map(value_node)
                        .transpose()?
                        .unwrap_or(Node::Null),
                ),
                (2, Node::Bytes(fingerprint.to_vec())),
                (3, diagnostic.as_ref().map_or(Node::Null, |x| x.0.0.clone())),
            ],
            Self::Diagnostic {
                diagnostic,
                recoverable,
            } => {
                let mut x = vec![(0, diagnostic.0.0.clone())];
                if let Some(v) = recoverable {
                    x.push((1, Node::Bool(*v)));
                }
                x
            }
            Self::RequestStatusResult {
                target,
                state,
                fingerprint,
                result,
            } => vec![
                (0, Node::Bytes(target.to_vec())),
                (1, uint(state.code())),
                (
                    2,
                    fingerprint.map_or(Node::Null, |x| Node::Bytes(x.to_vec())),
                ),
                (3, result.as_ref().map_or(Node::Null, |x| x.0.0.clone())),
            ],
        };
        for (key, value) in extensions {
            if fields.iter().any(|(known, _)| known == key) {
                return Err(Error::InvalidMessage);
            }
            fields.push((*key, value.0.clone()));
        }
        Ok(Node::Map(
            fields
                .into_iter()
                .map(|(key, value)| (uint(key.into()), value))
                .collect(),
        ))
    }
    fn decode(
        code: u16,
        request: Option<[u8; 16]>,
        watch: Option<[u8; 16]>,
        body: &[(Node, Node)],
    ) -> Result<(Self, BTreeMap<u16, ValueNode>)> {
        let (known, message) = match code {
            0 => (
                vec![0, 1],
                Self::Subscribe {
                    resource: bytes16(field(body, 0)?)?,
                    presentation: PresentationContext::decode(field(body, 1)?)?,
                },
            ),
            1 => (vec![], Self::Unsubscribe),
            2 => (vec![], Self::Resync),
            3 => (
                vec![0, 1, 2, 3],
                Self::Event {
                    revision: u64_value(field(body, 0)?)?,
                    action: bytes16(field(body, 1)?)?,
                    value: canonical_value(field(body, 2)?)?,
                    fingerprint: bytes32(field(body, 3)?)?,
                },
            ),
            4 => (
                vec![0, 1, 2, 3],
                Self::Eval {
                    source: text(field(body, 0)?)?.to_owned(),
                    database: DatabaseContext::decode(field(body, 1)?)?,
                    presentation: PresentationContext::decode(field(body, 2)?)?,
                    fingerprint: bytes32(field(body, 3)?)?,
                },
            ),
            5 => (
                vec![0, 1, 2, 3],
                Self::Watch {
                    source: text(field(body, 0)?)?.to_owned(),
                    database: DatabaseContext::decode(field(body, 1)?)?,
                    presentation: PresentationContext::decode(field(body, 2)?)?,
                    refresh_floor: optional_field(body, 3).map(Duration::decode).transpose()?,
                },
            ),
            6 => (
                vec![0, 1],
                Self::Cancel {
                    target_kind: TargetKind::decode(field(body, 0)?)?,
                    target: bytes16(field(body, 1)?)?,
                },
            ),
            7 => (
                vec![0, 1],
                Self::RequestStatus {
                    target: bytes16(field(body, 0)?)?,
                    fingerprint: bytes32(field(body, 1)?)?,
                },
            ),
            16 => (
                vec![0, 1, 2],
                Self::Snapshot {
                    revision: u64_value(field(body, 0)?)?,
                    present: PresentNode::decode(field(body, 1)?)?,
                    snapshot: snapshot(field(body, 2)?)?,
                },
            ),
            17 => (
                vec![0, 1, 2, 3],
                Self::Delta {
                    base_revision: u64_value(field(body, 0)?)?,
                    new_revision: u64_value(field(body, 1)?)?,
                    patches: PatchList::decode(field(body, 2)?)?,
                    snapshot: snapshot(field(body, 3)?)?,
                },
            ),
            18 => (
                vec![0, 1, 2, 3],
                Self::Result {
                    status: ResultStatus::decode(field(body, 0)?)?,
                    value: nullable_value(field(body, 1)?)?,
                    fingerprint: bytes32(field(body, 2)?)?,
                    diagnostic: nullable_diagnostic(field(body, 3)?)?,
                },
            ),
            19 => (
                vec![0, 1],
                Self::Diagnostic {
                    diagnostic: Diagnostic::decode(field(body, 0)?)?,
                    recoverable: optional_field(body, 1).map(bool_value).transpose()?,
                },
            ),
            20 => (
                vec![0, 1, 2, 3],
                Self::RequestStatusResult {
                    target: bytes16(field(body, 0)?)?,
                    state: RequestState::decode(field(body, 1)?)?,
                    fingerprint: nullable_bytes32(field(body, 2)?)?,
                    result: nullable_result_body(field(body, 3)?)?,
                },
            ),
            _ => return Err(Error::Unsupported),
        };
        for key in &known {
            let _ = field(body, *key)?;
        }
        let mut extensions = BTreeMap::new();
        for (key, value) in body {
            let number = u64_value(key)?;
            if number > u16::MAX.into() {
                return Err(Error::InvalidMessage);
            }
            let number = number as u16;
            if !known.contains(&number) {
                if number >= 32768 {
                    return Err(Error::UnknownMandatoryExtension);
                }
                extensions.insert(number, ValueNode(value.clone()));
            }
        }
        message.validate_envelope(request, watch)?;
        Ok((message, extensions))
    }
}

impl DatabaseContext {
    fn node(&self) -> Node {
        Node::Map(vec![
            (uint(0), uuid_node(self.database)),
            (
                uint(1),
                match self.snapshot.as_ref() {
                    Some(snapshot) => snapshot_node(snapshot).unwrap_or(Node::Null),
                    None => Node::Null,
                },
            ),
        ])
    }
    fn decode(node: &Node) -> Result<Self> {
        let m = map(node).ok_or(Error::InvalidValue)?;
        require_exact_keys(m, &[0, 1], Error::InvalidValue)?;
        Ok(Self {
            database: uuid(field(m, 0)?)?,
            snapshot: nullable_snapshot(field(m, 1)?)?,
        })
    }
}
impl PresentationContext {
    fn node(&self) -> Node {
        Node::Map(vec![
            (uint(0), Node::Text(self.locale.clone())),
            (
                uint(1),
                self.timezone.clone().map_or(Node::Null, Node::Text),
            ),
            (uint(2), self.width.map_or(Node::Null, uint)),
            (uint(3), Node::Text(self.theme.clone())),
            (
                uint(4),
                Node::Array(
                    self.supported_kinds
                        .iter()
                        .cloned()
                        .map(Node::Text)
                        .collect(),
                ),
            ),
        ])
    }
    fn decode(node: &Node) -> Result<Self> {
        let m = map(node).ok_or(Error::InvalidValue)?;
        require_exact_keys(m, &[0, 1, 2, 3, 4], Error::InvalidValue)?;
        let width = match field(m, 2)? {
            Node::Null => None,
            n => {
                let v = u64_value(n)?;
                if v == 0 {
                    return Err(Error::InvalidValue);
                }
                Some(v)
            }
        };
        let kinds = array(field(m, 4)?)
            .ok_or(Error::InvalidValue)?
            .iter()
            .map(|x| text(x).map(str::to_owned))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            locale: text(field(m, 0)?)?.to_owned(),
            timezone: nullable_text(field(m, 1)?)?,
            width,
            theme: text(field(m, 3)?)?.to_owned(),
            supported_kinds: kinds,
        })
    }
}
impl Duration {
    fn node(&self) -> Node {
        Node::Tag(
            60005,
            Box::new(Node::Array(vec![
                Node::Int(self.floor_seconds.clone()),
                uint(self.nanosecond.into()),
            ])),
        )
    }
    fn decode(node: &Node) -> Result<Self> {
        let Node::Tag(60005, body) = node else {
            return Err(Error::InvalidValue);
        };
        let a = array(body).ok_or(Error::InvalidValue)?;
        if a.len() != 2 {
            return Err(Error::InvalidValue);
        }
        let Node::Int(seconds) = &a[0] else {
            return Err(Error::InvalidValue);
        };
        let nanos = u64_value(&a[1])?;
        if seconds.sign() == Sign::Minus || nanos >= 1_000_000_000 {
            return Err(Error::InvalidValue);
        }
        Ok(Self {
            floor_seconds: seconds.clone(),
            nanosecond: nanos as u32,
        })
    }
}
impl TargetKind {
    const fn code(self) -> u64 {
        match self {
            Self::Request => 0,
            Self::Watch => 1,
        }
    }
    fn decode(node: &Node) -> Result<Self> {
        match u64_value(node)? {
            0 => Ok(Self::Request),
            1 => Ok(Self::Watch),
            _ => Err(Error::InvalidValue),
        }
    }
}
impl ResultStatus {
    const fn code(self) -> u64 {
        match self {
            Self::Success => 0,
            Self::Failure => 1,
            Self::Cancellation => 2,
            Self::RetainedWithoutValue => 3,
        }
    }
    fn decode(node: &Node) -> Result<Self> {
        match u64_value(node)? {
            0 => Ok(Self::Success),
            1 => Ok(Self::Failure),
            2 => Ok(Self::Cancellation),
            3 => Ok(Self::RetainedWithoutValue),
            _ => Err(Error::InvalidValue),
        }
    }
}
impl RequestState {
    const fn code(self) -> u64 {
        match self {
            Self::Unknown => 0,
            Self::Reserved => 1,
            Self::Running => 2,
            Self::Terminal => 3,
            Self::Orphaned => 4,
        }
    }
    fn decode(node: &Node) -> Result<Self> {
        match u64_value(node)? {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::Reserved),
            2 => Ok(Self::Running),
            3 => Ok(Self::Terminal),
            4 => Ok(Self::Orphaned),
            _ => Err(Error::InvalidValue),
        }
    }
}

impl PresentNode {
    fn decode(node: &Node) -> Result<Self> {
        validate_present(node)?;
        Ok(Self(ValueNode(node.clone())))
    }
}
impl PatchList {
    fn decode(node: &Node) -> Result<Self> {
        validate_patches(node)?;
        Ok(Self(ValueNode(node.clone())))
    }
}
impl Diagnostic {
    fn decode(node: &Node) -> Result<Self> {
        validate_diagnostic(node)?;
        Ok(Self(ValueNode(node.clone())))
    }
}
impl ResultBody {
    /// Extracts the validated canonical body from a terminal `Result` envelope.
    ///
    /// This preserves the wire representation used by `RequestStatusResult`
    /// without allowing callers to manufacture an unchecked body.
    pub fn from_result(response: &Envelope, limits: Limits) -> Result<Self> {
        let _ = response.encode(limits)?;
        let Message::Result { .. } = &response.message else {
            return Err(Error::InvalidMessage);
        };
        let body = response.message.body(&BTreeMap::new())?;
        Self::decode(&body)
    }

    fn decode(node: &Node) -> Result<Self> {
        let body = map(node).ok_or(Error::InvalidValue)?;
        let _ = Message::decode(18, Some([0; 16]), None, body)?;
        Ok(Self(ValueNode(node.clone())))
    }
}

fn validate_present(node: &Node) -> Result<()> {
    if !matches!(node, Node::Tag(60012, _)) {
        return Err(Error::InvalidValue);
    }
    let _ = canonical_value(node)?;
    Ok(())
}
fn validate_patches(node: &Node) -> Result<()> {
    for operation in array(node).ok_or(Error::InvalidValue)? {
        let a = array(operation).ok_or(Error::InvalidValue)?;
        let opcode = u64_value(a.first().ok_or(Error::InvalidValue)?)?;
        if !matches!((opcode, a.len()), (0, 3) | (1, 2) | (2, 3) | (3, 3)) {
            return Err(Error::InvalidValue);
        }
        validate_path(&a[1])?;
        if opcode == 3 {
            validate_path(&a[2])?;
        } else if opcode != 1 {
            let _ = canonical_value(&a[2])?;
        }
    }
    Ok(())
}
fn validate_path(node: &Node) -> Result<()> {
    for component in array(node).ok_or(Error::InvalidValue)? {
        let a = array(component).ok_or(Error::InvalidValue)?;
        let kind = u64_value(a.first().ok_or(Error::InvalidValue)?)?;
        match kind {
            0 if a.len() == 2 && matches!(&a[1], Node::Text(_) | Node::Tag(37, _)) => {}
            1 if a.len() == 3 && uuid(&a[1]).is_ok() => {
                let _ = canonical_value(&a[2])?;
            }
            2 if a.len() == 2 && u64_value(&a[1]).is_ok() => {}
            3 if a.len() == 2 => {
                let _ = canonical_value(&a[1])?;
            }
            _ => return Err(Error::InvalidValue),
        }
    }
    Ok(())
}
fn validate_diagnostic(node: &Node) -> Result<()> {
    let bytes = encode_node(node)?;
    orna_foundation_v1::Diagnostic::decode_ovb(&bytes)
        .map(|_| ())
        .map_err(|_| Error::InvalidValue)
}

fn canonical_value(node: &Node) -> Result<CanonicalValue> {
    CanonicalValue::new(to_ovb(node)?).map_err(|_| Error::InvalidValue)
}
fn nullable_value(node: &Node) -> Result<Option<CanonicalValue>> {
    if matches!(node, Node::Null) {
        Ok(None)
    } else {
        canonical_value(node).map(Some)
    }
}
fn to_ovb(node: &Node) -> Result<OvbRaw> {
    Ok(match node {
        Node::Null => OvbRaw::Null,
        Node::Bool(v) => OvbRaw::Bool(*v),
        Node::Int(v) => OvbRaw::Int(v.clone()),
        Node::Float(v) => OvbRaw::Float(*v),
        Node::Bytes(v) => OvbRaw::Bytes(v.clone()),
        Node::Text(v) => OvbRaw::Text(v.clone()),
        Node::Array(v) => OvbRaw::Array(v.iter().map(to_ovb).collect::<Result<_>>()?),
        Node::Map(v) => OvbRaw::Map(
            v.iter()
                .map(|(k, v)| Ok((to_ovb(k)?, to_ovb(v)?)))
                .collect::<Result<_>>()?,
        ),
        Node::Tag(tag, value) => OvbRaw::Tag(*tag, Box::new(to_ovb(value)?)),
    })
}
fn value_node(value: &CanonicalValue) -> Result<Node> {
    from_ovb(value.raw())
}
fn from_ovb(value: &OvbRaw) -> Result<Node> {
    Ok(match value {
        OvbRaw::Null => Node::Null,
        OvbRaw::Bool(v) => Node::Bool(*v),
        OvbRaw::Int(v) => Node::Int(v.clone()),
        OvbRaw::Float(v) => Node::Float(*v),
        OvbRaw::Bytes(v) => Node::Bytes(v.clone()),
        OvbRaw::Text(v) => Node::Text(v.clone()),
        OvbRaw::Array(v) => Node::Array(v.iter().map(from_ovb).collect::<Result<_>>()?),
        OvbRaw::Map(v) => Node::Map(
            v.iter()
                .map(|(k, v)| Ok((from_ovb(k)?, from_ovb(v)?)))
                .collect::<Result<_>>()?,
        ),
        OvbRaw::Tag(tag, value) => Node::Tag(*tag, Box::new(from_ovb(value)?)),
    })
}
fn snapshot(node: &Node) -> Result<CanonicalSnapshot> {
    CanonicalSnapshot::decode(&to_ovb(node)?).map_err(|_| Error::InvalidValue)
}
fn nullable_snapshot(node: &Node) -> Result<Option<CanonicalSnapshot>> {
    if matches!(node, Node::Null) {
        Ok(None)
    } else {
        snapshot(node).map(Some)
    }
}
fn snapshot_node(snapshot: &CanonicalSnapshot) -> Result<Node> {
    from_ovb(&snapshot.raw())
}
fn nullable_diagnostic(node: &Node) -> Result<Option<Diagnostic>> {
    if matches!(node, Node::Null) {
        Ok(None)
    } else {
        Diagnostic::decode(node).map(Some)
    }
}
fn nullable_result_body(node: &Node) -> Result<Option<ResultBody>> {
    if matches!(node, Node::Null) {
        Ok(None)
    } else {
        ResultBody::decode(node).map(Some)
    }
}

fn uint(value: u64) -> Node {
    Node::Int(BigInt::from(value))
}
fn option_bytes(value: Option<[u8; 16]>) -> Node {
    value.map_or(Node::Null, |x| Node::Bytes(x.to_vec()))
}
fn map(node: &Node) -> Option<&Vec<(Node, Node)>> {
    if let Node::Map(value) = node {
        Some(value)
    } else {
        None
    }
}
fn array(node: &Node) -> Option<&Vec<Node>> {
    if let Node::Array(value) = node {
        Some(value)
    } else {
        None
    }
}
fn text(node: &Node) -> Result<&str> {
    if let Node::Text(value) = node {
        Ok(value)
    } else {
        Err(Error::InvalidValue)
    }
}
fn bool_value(node: &Node) -> Result<bool> {
    if let Node::Bool(value) = node {
        Ok(*value)
    } else {
        Err(Error::InvalidValue)
    }
}
fn u64_value(node: &Node) -> Result<u64> {
    match node {
        Node::Int(value) if value.sign() != Sign::Minus => {
            value.to_u64().ok_or(Error::InvalidValue)
        }
        _ => Err(Error::InvalidValue),
    }
}
fn bytes16(node: &Node) -> Result<[u8; 16]> {
    fixed_bytes(node)
}
fn bytes32(node: &Node) -> Result<[u8; 32]> {
    fixed_bytes(node)
}
fn fixed_bytes<const N: usize>(node: &Node) -> Result<[u8; N]> {
    let Node::Bytes(value) = node else {
        return Err(Error::InvalidValue);
    };
    value.as_slice().try_into().map_err(|_| Error::InvalidValue)
}
fn nullable_bytes16(node: &Node) -> Result<Option<[u8; 16]>> {
    if matches!(node, Node::Null) {
        Ok(None)
    } else {
        bytes16(node).map(Some)
    }
}
fn nullable_bytes32(node: &Node) -> Result<Option<[u8; 32]>> {
    if matches!(node, Node::Null) {
        Ok(None)
    } else {
        bytes32(node).map(Some)
    }
}
fn nullable_text(node: &Node) -> Result<Option<String>> {
    if matches!(node, Node::Null) {
        Ok(None)
    } else {
        text(node).map(str::to_owned).map(Some)
    }
}
fn uuid(node: &Node) -> Result<[u8; 16]> {
    match node {
        Node::Tag(37, value) => bytes16(value),
        _ => Err(Error::InvalidValue),
    }
}
fn uuid_node(value: [u8; 16]) -> Node {
    Node::Tag(37, Box::new(Node::Bytes(value.to_vec())))
}
fn field(map: &[(Node, Node)], key: u16) -> Result<&Node> {
    map.iter()
        .find_map(|(k, v)| (u64_value(k).ok() == Some(key.into())).then_some(v))
        .ok_or(Error::InvalidValue)
}
fn optional_field(map: &[(Node, Node)], key: u16) -> Option<&Node> {
    map.iter()
        .find_map(|(k, v)| (u64_value(k).ok() == Some(key.into())).then_some(v))
}
fn require_exact_keys(map: &[(Node, Node)], expected: &[u16], error: Error) -> Result<()> {
    if map.len() != expected.len() {
        return Err(error);
    }
    for key in expected {
        let _ = field(map, *key).map_err(|_| error)?;
    }
    Ok(())
}

fn encode_node(node: &Node) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_node(node, &mut out)?;
    Ok(out)
}
fn head(out: &mut Vec<u8>, major: u8, value: u64) {
    if value < 24 {
        out.push((major << 5) | value as u8);
    } else if u8::try_from(value).is_ok() {
        out.extend([major << 5 | 24, value as u8]);
    } else if u16::try_from(value).is_ok() {
        out.push(major << 5 | 25);
        out.extend((value as u16).to_be_bytes());
    } else if u32::try_from(value).is_ok() {
        out.push(major << 5 | 26);
        out.extend((value as u32).to_be_bytes());
    } else {
        out.push(major << 5 | 27);
        out.extend(value.to_be_bytes());
    }
}
fn write_node(node: &Node, out: &mut Vec<u8>) -> Result<()> {
    match node {
        Node::Null => out.push(0xf6),
        Node::Bool(false) => out.push(0xf4),
        Node::Bool(true) => out.push(0xf5),
        Node::Int(value) => {
            let negative = value.sign() == Sign::Minus;
            let magnitude = if negative { -value - 1 } else { value.clone() };
            if let Some(v) = magnitude.to_u64() {
                head(out, if negative { 1 } else { 0 }, v);
            } else {
                let (_, bytes) = magnitude.to_bytes_be();
                head(out, 6, if negative { 3 } else { 2 });
                head(out, 2, bytes.len() as u64);
                out.extend(bytes);
            }
        }
        Node::Float(bits) => {
            out.push(0xfb);
            out.extend(bits.to_be_bytes());
        }
        Node::Bytes(value) => {
            head(out, 2, value.len() as u64);
            out.extend(value);
        }
        Node::Text(value) => {
            head(out, 3, value.len() as u64);
            out.extend(value.as_bytes());
        }
        Node::Array(values) => {
            head(out, 4, values.len() as u64);
            for value in values {
                write_node(value, out)?;
            }
        }
        Node::Map(values) => {
            let mut sorted = values
                .iter()
                .map(|(key, value)| Ok((encode_node(key)?, value)))
                .collect::<Result<Vec<_>>>()?;
            sorted.sort_by(|left, right| left.0.cmp(&right.0));
            if sorted.windows(2).any(|window| window[0].0 == window[1].0) {
                return Err(Error::NonCanonical);
            }
            head(out, 5, sorted.len() as u64);
            for (key, value) in sorted {
                out.extend(key);
                write_node(value, out)?;
            }
        }
        Node::Tag(tag, value) => {
            head(out, 6, *tag);
            write_node(value, out)?;
        }
    };
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    nodes: usize,
    limits: Limits,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], limits: Limits) -> Self {
        Self {
            bytes,
            at: 0,
            nodes: 0,
            limits,
        }
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(count).ok_or(Error::Limit)?;
        let value = self.bytes.get(self.at..end).ok_or(Error::Truncated)?;
        self.at = end;
        Ok(value)
    }
    fn arg(&mut self, ai: u8) -> Result<u64> {
        let (minimum, width) = match ai {
            0..=23 => return Ok(ai.into()),
            24 => (24, 1),
            25 => (256, 2),
            26 => (65536, 4),
            27 => (4_294_967_296, 8),
            _ => return Err(Error::Unsupported),
        };
        let mut value = 0u64;
        for byte in self.take(width)? {
            value = value << 8 | u64::from(*byte);
        }
        if value < minimum {
            return Err(Error::NonCanonical);
        }
        Ok(value)
    }
    fn node(&mut self, depth: usize) -> Result<Node> {
        if depth > self.limits.max_depth || self.nodes >= self.limits.max_nodes {
            return Err(Error::Limit);
        }
        self.nodes += 1;
        let first = self.take(1)?[0];
        let major = first >> 5;
        let ai = first & 31;
        if major == 7 {
            return match ai {
                20 => Ok(Node::Bool(false)),
                21 => Ok(Node::Bool(true)),
                22 => Ok(Node::Null),
                27 => {
                    let mut bytes = [0; 8];
                    bytes.copy_from_slice(self.take(8)?);
                    let bits = u64::from_be_bytes(bytes);
                    if is_noncanonical_nan(bits) {
                        Err(Error::NonCanonical)
                    } else {
                        Ok(Node::Float(bits))
                    }
                }
                _ => Err(Error::Unsupported),
            };
        }
        let arg = self.arg(ai)?;
        match major {
            0 => Ok(uint(arg)),
            1 => Ok(Node::Int(-BigInt::from(arg) - 1)),
            2 => Ok(Node::Bytes(
                self.take(usize::try_from(arg).map_err(|_| Error::Limit)?)?
                    .to_vec(),
            )),
            3 => Ok(Node::Text(
                String::from_utf8(
                    self.take(usize::try_from(arg).map_err(|_| Error::Limit)?)?
                        .to_vec(),
                )
                .map_err(|_| Error::InvalidValue)?,
            )),
            4 => {
                let count = usize::try_from(arg).map_err(|_| Error::Limit)?;
                if count > self.limits.max_collection_items {
                    return Err(Error::Limit);
                }
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(self.node(depth + 1)?);
                }
                Ok(Node::Array(values))
            }
            5 => {
                let count = usize::try_from(arg).map_err(|_| Error::Limit)?;
                if count > self.limits.max_collection_items {
                    return Err(Error::Limit);
                }
                let mut values = Vec::with_capacity(count);
                let mut previous = None;
                for _ in 0..count {
                    let start = self.at;
                    let key = self.node(depth + 1)?;
                    let key_bytes = &self.bytes[start..self.at];
                    if previous
                        .as_ref()
                        .is_some_and(|x: &Vec<u8>| key_bytes <= x.as_slice())
                    {
                        return Err(Error::NonCanonical);
                    }
                    previous = Some(key_bytes.to_vec());
                    let value = self.node(depth + 1)?;
                    values.push((key, value));
                }
                Ok(Node::Map(values))
            }
            6 => {
                let value = self.node(depth + 1)?;
                if arg == 2 || arg == 3 {
                    let Node::Bytes(bytes) = value else {
                        return Err(Error::InvalidValue);
                    };
                    if bytes.is_empty() || bytes[0] == 0 || bytes.len() < 9 {
                        return Err(Error::NonCanonical);
                    }
                    let number = BigInt::from_bytes_be(Sign::Plus, &bytes);
                    Ok(Node::Int(if arg == 2 { number } else { -number - 1 }))
                } else {
                    Ok(Node::Tag(arg, Box::new(value)))
                }
            }
            _ => Err(Error::Unsupported),
        }
    }
}
fn is_noncanonical_nan(bits: u64) -> bool {
    let exponent = bits & 0x7ff0_0000_0000_0000;
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    exponent == 0x7ff0_0000_0000_0000 && fraction != 0 && bits != 0x7ff8_0000_0000_0000
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(byte: u8) -> [u8; 16] {
        [byte; 16]
    }
    fn digest(byte: u8) -> [u8; 32] {
        [byte; 32]
    }
    fn context() -> PresentationContext {
        PresentationContext {
            locale: "en-GB".into(),
            timezone: None,
            width: Some(80),
            theme: "terminal/default".into(),
            supported_kinds: vec!["text".into()],
        }
    }
    fn database() -> DatabaseContext {
        DatabaseContext {
            database: id(1),
            snapshot: None,
        }
    }
    fn value() -> CanonicalValue {
        CanonicalValue::new(OvbRaw::Text("ok".into())).unwrap()
    }
    fn snapshot() -> CanonicalSnapshot {
        CanonicalSnapshot::cwd(id(1), id(2), 0.into()).unwrap()
    }
    fn present() -> PresentNode {
        PresentNode::decode(&present_node(Node::Null, vec![])).unwrap()
    }
    fn present_node(key: Node, children: Vec<Node>) -> Node {
        Node::Tag(
            60012,
            Box::new(Node::Array(vec![
                Node::Text("text".into()),
                key,
                Node::Map(vec![]),
                Node::Array(children),
            ])),
        )
    }
    fn diagnostic() -> Diagnostic {
        Diagnostic::decode(&diagnostic_node(vec![])).unwrap()
    }
    fn diagnostic_node(spans: Vec<Node>) -> Node {
        Node::Tag(
            60011,
            Box::new(Node::Map(vec![
                (uint(0), Node::Text("wire.invalid_message".into())),
                (uint(1), uint(3)),
                (uint(2), Node::Text("invalid message".into())),
                (uint(3), Node::Array(spans)),
                (uint(4), Node::Array(vec![])),
                (uint(5), Node::Array(vec![])),
                (uint(6), Node::Bool(true)),
            ])),
        )
    }
    fn result_body(result: Node) -> Vec<u8> {
        wire(
            20,
            Some(id(1)),
            None,
            Node::Map(vec![
                (uint(0), Node::Bytes(id(3).to_vec())),
                (uint(1), uint(RequestState::Terminal.code())),
                (uint(2), Node::Bytes(digest(1).to_vec())),
                (uint(3), result),
            ]),
        )
    }
    fn messages() -> Vec<Envelope> {
        vec![
            Envelope {
                request: Some(id(1)),
                watch: None,
                message: Message::Subscribe {
                    resource: id(2),
                    presentation: context(),
                },
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: Some(id(1)),
                watch: Some(id(2)),
                message: Message::Unsubscribe,
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: Some(id(1)),
                watch: Some(id(2)),
                message: Message::Resync,
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: Some(id(1)),
                watch: Some(id(2)),
                message: Message::Event {
                    revision: 0,
                    action: id(3),
                    value: value(),
                    fingerprint: digest(1),
                },
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: Some(id(1)),
                watch: None,
                message: Message::Eval {
                    source: "1".into(),
                    database: database(),
                    presentation: context(),
                    fingerprint: digest(1),
                },
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: Some(id(1)),
                watch: None,
                message: Message::Watch {
                    source: "1".into(),
                    database: database(),
                    presentation: context(),
                    refresh_floor: Some(Duration {
                        floor_seconds: 0.into(),
                        nanosecond: 0,
                    }),
                },
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: Some(id(1)),
                watch: None,
                message: Message::Cancel {
                    target_kind: TargetKind::Watch,
                    target: id(3),
                },
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: Some(id(1)),
                watch: None,
                message: Message::RequestStatus {
                    target: id(3),
                    fingerprint: digest(1),
                },
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: Some(id(1)),
                watch: Some(id(2)),
                message: Message::Snapshot {
                    revision: 0,
                    present: present(),
                    snapshot: snapshot(),
                },
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: None,
                watch: Some(id(2)),
                message: Message::Delta {
                    base_revision: 0,
                    new_revision: 1,
                    patches: PatchList::decode(&Node::Array(vec![])).unwrap(),
                    snapshot: snapshot(),
                },
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: Some(id(1)),
                watch: None,
                message: Message::Result {
                    status: ResultStatus::Success,
                    value: Some(value()),
                    fingerprint: digest(1),
                    diagnostic: None,
                },
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: None,
                watch: None,
                message: Message::Diagnostic {
                    diagnostic: diagnostic(),
                    recoverable: Some(true),
                },
                extensions: BTreeMap::new(),
            },
            Envelope {
                request: Some(id(1)),
                watch: None,
                message: Message::RequestStatusResult {
                    target: id(3),
                    state: RequestState::Terminal,
                    fingerprint: Some(digest(1)),
                    result: None,
                },
                extensions: BTreeMap::new(),
            },
        ]
    }
    fn wire(code: u64, request: Option<[u8; 16]>, watch: Option<[u8; 16]>, body: Node) -> Vec<u8> {
        encode_node(&Node::Map(vec![
            (uint(0), uint(VERSION)),
            (uint(1), uint(code)),
            (uint(2), option_bytes(request)),
            (uint(3), option_bytes(watch)),
            (uint(4), body),
        ]))
        .unwrap()
    }
    #[test]
    fn every_registry_message_round_trips_stably() {
        for message in messages() {
            let bytes = message.encode(Limits::default()).unwrap();
            assert_eq!(
                Envelope::decode(&bytes, Limits::default()).unwrap(),
                message
            );
            assert_eq!(message.encode(Limits::default()).unwrap(), bytes);
            assert!(canonical_request_fingerprint(id(9), &message, Limits::default()).is_ok());
        }
    }

    #[test]
    fn result_body_extracts_only_a_bounded_canonical_result() {
        let result = messages().into_iter().nth(10).unwrap();
        let body = ResultBody::from_result(&result, Limits::default()).unwrap();
        let status = Envelope {
            request: Some(id(4)),
            watch: None,
            message: Message::RequestStatusResult {
                target: id(1),
                state: RequestState::Terminal,
                fingerprint: Some(digest(1)),
                result: Some(body),
            },
            extensions: BTreeMap::new(),
        };
        assert_eq!(
            Envelope::decode(
                &status.encode(Limits::default()).unwrap(),
                Limits::default()
            )
            .unwrap(),
            status
        );
        assert_eq!(
            ResultBody::from_result(&messages()[0], Limits::default()),
            Err(Error::InvalidMessage)
        );
    }
    #[test]
    fn request_fingerprint_omits_event_and_eval_redundant_fields() {
        let session_id = id(9);
        for index in [3, 4] {
            let envelope = messages().remove(index);
            let mut changed = envelope.clone();
            match &mut changed.message {
                Message::Event { fingerprint, .. } | Message::Eval { fingerprint, .. } => {
                    *fingerprint = digest(2);
                }
                _ => unreachable!(),
            }
            assert_eq!(
                canonical_request_fingerprint(session_id, &envelope, Limits::default()),
                canonical_request_fingerprint(session_id, &changed, Limits::default()),
            );
        }
    }
    #[test]
    fn request_fingerprint_covers_extensions() {
        let session_id = id(9);
        let envelope = messages().remove(3);
        let mut extended = envelope.clone();
        extended
            .extensions
            .insert(11, ValueNode(Node::Text("extension".into())));
        assert_ne!(
            canonical_request_fingerprint(session_id, &envelope, Limits::default()),
            canonical_request_fingerprint(session_id, &extended, Limits::default()),
        );
    }
    #[test]
    fn rejects_malformed_envelopes_and_extensions() {
        let bytes = wire(
            1,
            Some(id(1)),
            Some(id(2)),
            Node::Map(vec![(uint(32768), Node::Null)]),
        );
        assert_eq!(
            Envelope::decode(&bytes, Limits::default()),
            Err(Error::UnknownMandatoryExtension)
        );
        let duplicate = vec![0xa2, 0x00, 0x01, 0x00, 0x01];
        assert_eq!(
            Envelope::decode(&duplicate, Limits::default()),
            Err(Error::NonCanonical)
        );
        let malformed_body = wire(
            1,
            Some(id(1)),
            Some(id(2)),
            Node::Map(vec![(Node::Text("key".into()), Node::Null)]),
        );
        assert_eq!(
            Envelope::decode(&malformed_body, Limits::default()),
            Err(Error::InvalidValue)
        );
        let mut optional = messages().remove(0);
        optional.extensions.insert(11, ValueNode(Node::Null));
        let bytes = optional.encode(Limits::default()).unwrap();
        assert_eq!(
            Envelope::decode(&bytes, Limits::default()).unwrap(),
            optional
        );
    }
    #[test]
    fn rejects_invariants_lengths_enums_and_integers() {
        let mut message = messages().remove(0);
        message.watch = Some(id(9));
        assert!(message.encode(Limits::default()).is_err());
        let mut cancel = messages().remove(6);
        if let Message::Cancel {
            target_kind,
            target,
        } = &mut cancel.message
        {
            *target_kind = TargetKind::Request;
            *target = id(1);
        }
        assert!(cancel.encode(Limits::default()).is_err());
        let bytes = vec![
            0xa5, 0x00, 0x01, 0x01, 0x00, 0x02, 0x50, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0x03, 0xf6, 0x04, 0xa1, 0x00, 0x41, 0,
        ];
        assert!(Envelope::decode(&bytes, Limits::default()).is_err());
        let enum_value = wire(
            6,
            Some(id(1)),
            None,
            Node::Map(vec![
                (uint(0), uint(2)),
                (uint(1), Node::Bytes(id(2).to_vec())),
            ]),
        );
        assert_eq!(
            Envelope::decode(&enum_value, Limits::default()),
            Err(Error::InvalidValue)
        );
        let negative_duration = wire(
            5,
            Some(id(1)),
            None,
            Node::Map(vec![
                (uint(0), Node::Text("1".into())),
                (uint(1), database().node()),
                (uint(2), context().node()),
                (
                    uint(3),
                    Node::Tag(
                        60005,
                        Box::new(Node::Array(vec![Node::Int((-1).into()), uint(0)])),
                    ),
                ),
            ]),
        );
        assert_eq!(
            Envelope::decode(&negative_duration, Limits::default()),
            Err(Error::InvalidValue)
        );
    }
    #[test]
    fn rejects_noncanonical_depth_and_size() {
        let message = messages().remove(0);
        let bytes = message.encode(Limits::default()).unwrap();
        let small = Limits {
            max_message_bytes: bytes.len() - 1,
            ..Limits::default()
        };
        assert_eq!(Envelope::decode(&bytes, small), Err(Error::Limit));
        let noncanonical = vec![
            0xa5, 0x18, 0x00, 0x01, 0x01, 0x00, 0x02, 0xf6, 0x03, 0xf6, 0x04, 0xa0,
        ];
        assert_eq!(
            Envelope::decode(&noncanonical, Limits::default()),
            Err(Error::NonCanonical)
        );
        let nested = vec![0x81, 0x81, 0x81, 0x81, 0xf6];
        let low = Limits {
            max_depth: 2,
            ..Limits::default()
        };
        assert_eq!(Envelope::decode(&nested, low), Err(Error::Limit));
    }
    #[test]
    fn request_status_result_rejects_malformed_result_body_maps() {
        let missing_required_fields = [
            Node::Map(vec![]),
            Node::Map(vec![
                (uint(0), uint(ResultStatus::Failure.code())),
                (uint(1), Node::Null),
                (uint(2), Node::Bytes(digest(1).to_vec())),
            ]),
        ];
        for body in missing_required_fields {
            assert_eq!(
                Envelope::decode(&result_body(body), Limits::default()),
                Err(Error::InvalidValue)
            );
        }

        let malformed_required_field = Node::Map(vec![
            (uint(0), uint(ResultStatus::Failure.code())),
            (uint(1), Node::Null),
            (uint(2), Node::Bytes(vec![1; 31])),
            (uint(3), Node::Null),
        ]);
        assert_eq!(
            Envelope::decode(&result_body(malformed_required_field), Limits::default()),
            Err(Error::InvalidValue)
        );
    }
    #[test]
    fn request_status_result_accepts_and_preserves_a_valid_result_body() {
        let retained = Node::Map(vec![
            (uint(0), uint(ResultStatus::Failure.code())),
            (uint(1), Node::Null),
            (uint(2), Node::Bytes(digest(1).to_vec())),
            (uint(3), diagnostic().0.0),
            (uint(11), Node::Text("retained extension".into())),
        ]);
        let bytes = result_body(retained);
        let decoded = Envelope::decode(&bytes, Limits::default()).unwrap();
        assert_eq!(decoded.encode(Limits::default()).unwrap(), bytes);
    }
    #[test]
    fn diagnostics_validate_each_span_shape_and_value() {
        let valid_span = Node::Array(vec![
            snapshot_node(&snapshot()).unwrap(),
            Node::Text("src/main.orna".into()),
            uint(1),
            uint(3),
        ]);
        assert!(Diagnostic::decode(&diagnostic_node(vec![valid_span])).is_ok());

        let malformed_spans = [
            Node::Null,
            Node::Array(vec![
                Node::Null,
                Node::Text("src/main.orna".into()),
                uint(1),
            ]),
            Node::Array(vec![
                Node::Null,
                Node::Text("src/main.orna".into()),
                uint(1),
                uint(3),
            ]),
            Node::Array(vec![
                snapshot_node(&snapshot()).unwrap(),
                Node::Text("../main.orna".into()),
                uint(1),
                uint(3),
            ]),
            Node::Array(vec![
                snapshot_node(&snapshot()).unwrap(),
                Node::Text("src/main.orna".into()),
                Node::Int((-1).into()),
                uint(3),
            ]),
            Node::Array(vec![
                snapshot_node(&snapshot()).unwrap(),
                Node::Text("src/main.orna".into()),
                uint(4),
                uint(3),
            ]),
        ];
        for span in malformed_spans {
            assert_eq!(
                Diagnostic::decode(&diagnostic_node(vec![span])),
                Err(Error::InvalidValue)
            );
        }
    }
    #[test]
    fn present_rejects_duplicate_sibling_stable_keys() {
        let stable_key = Node::Array(vec![uint(0), Node::Text("same".into())]);
        let root = present_node(
            Node::Null,
            vec![
                present_node(stable_key.clone(), vec![]),
                present_node(stable_key, vec![]),
            ],
        );
        let bytes = wire(
            16,
            Some(id(1)),
            Some(id(2)),
            Node::Map(vec![
                (uint(0), uint(0)),
                (uint(1), root),
                (uint(2), snapshot_node(&snapshot()).unwrap()),
            ]),
        );
        assert_eq!(
            Envelope::decode(&bytes, Limits::default()),
            Err(Error::InvalidValue)
        );
    }
    #[test]
    fn diagnostics_do_not_disclose_payloads() {
        let error = Envelope::decode(&[0xff], Limits::default()).unwrap_err();
        assert_eq!(error.to_string(), "wire.unsupported");
        assert!(!error.to_string().contains("ff"));
    }
}
