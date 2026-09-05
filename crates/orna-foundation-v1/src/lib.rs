//! Shared, lossless Orna 1.0 foundation contracts.
//!
//! Canonical payload ownership remains in `orna-value-v1`. This crate owns the
//! portable `sys` references, spans, diagnostics, and repository adapter seam.

use std::{collections::BTreeMap, fmt, marker::PhantomData, sync::Mutex};

use num_bigint::{BigInt, Sign};
use serde::{Serialize, Serializer, ser::SerializeStruct};

pub use orna_value_v1::{
    Error as ValueError, GitHash, OVB_VERSION, Raw as OvbRaw, SchemaDescriptor, Snapshot, Value,
};

/// Canonical values, closed descriptors and snapshot encodings are owned by
/// OVB-1. Neither `CanonicalSnapshot` nor `CanonicalValue` is a sys row ref.
pub type CanonicalValue = Value;
pub type TypeDescriptor = SchemaDescriptor;
pub type CanonicalSnapshot = Snapshot;

/// `sys.RowRef<T>` as OVB tag 60010. Key and snapshot context are identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowRef {
    pub database_id: [u8; 16],
    pub table_id: [u8; 16],
    pub key: OvbRaw,
    pub snapshot: CanonicalSnapshot,
}
impl RowRef {
    pub fn new(
        database_id: [u8; 16],
        table_id: [u8; 16],
        key: OvbRaw,
        snapshot: CanonicalSnapshot,
    ) -> Result<Self, FoundationError> {
        let reference = Self {
            database_id,
            table_id,
            key,
            snapshot,
        };
        Value::new(reference.raw()?).map_err(FoundationError::Value)?;
        Ok(reference)
    }
    pub fn encode(&self) -> Result<Vec<u8>, FoundationError> {
        Value::new(self.raw()?)
            .map_err(FoundationError::Value)?
            .encode()
            .map_err(FoundationError::Value)
    }
    fn raw(&self) -> Result<OvbRaw, FoundationError> {
        Ok(OvbRaw::Tag(
            60010,
            Box::new(OvbRaw::Array(vec![
                uuid(self.database_id),
                uuid(self.table_id),
                self.key.clone(),
                self.snapshot.raw(),
            ])),
        ))
    }
}
/// A noninterchangeable typed `sys.RowRef<T>`. The only conversion into a
/// typed reference is explicit at the owning catalogue/repository boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypedRowRef<Kind> {
    raw: RowRef,
    kind: PhantomData<Kind>,
}
impl<Kind> TypedRowRef<Kind> {
    pub fn from_row_ref(raw: RowRef) -> Self {
        Self {
            raw,
            kind: PhantomData,
        }
    }
    pub fn as_row_ref(&self) -> &RowRef {
        &self.raw
    }
    pub fn into_row_ref(self) -> RowRef {
        self.raw
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObjectKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DefinitionKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceKind {}
/// Typed portable row references. `SnapshotRef` is a snapshot metadata row
/// reference, deliberately distinct from `CanonicalSnapshot` pin bytes.
pub type FileRef = TypedRowRef<FileKind>;
pub type SnapshotRef = TypedRowRef<SnapshotKind>;
pub type DiagnosticRef = TypedRowRef<DiagnosticKind>;
pub type ObjectRef = TypedRowRef<ObjectKind>;
pub type DefinitionRef = TypedRowRef<DefinitionKind>;
pub type TraceRef = TypedRowRef<TraceKind>;

/// Exact `sys.SourceSpan`. Orna `Int` coordinates can exceed machine integers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    pub file: FileRef,
    pub start_byte: BigInt,
    pub end_byte: BigInt,
    pub start_line: BigInt,
    pub start_column: BigInt,
    pub end_line: BigInt,
    pub end_column: BigInt,
}
impl SourceSpan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        file: FileRef,
        start_byte: BigInt,
        end_byte: BigInt,
        start_line: BigInt,
        start_column: BigInt,
        end_line: BigInt,
        end_column: BigInt,
    ) -> Result<Self, FoundationError> {
        if start_byte.sign() == Sign::Minus
            || end_byte < start_byte
            || [
                start_line.clone(),
                start_column.clone(),
                end_line.clone(),
                end_column.clone(),
            ]
            .iter()
            .any(|n| n <= &BigInt::from(0))
        {
            return Err(FoundationError::InvalidSourceSpan);
        }
        Ok(Self {
            file,
            start_byte,
            end_byte,
            start_line,
            start_column,
            end_line,
            end_column,
        })
    }

    /// Migration adapter for syntax front ends that only expose UTF-8 byte
    /// offsets. The source text supplies the required one-based coordinates;
    /// callers retain their existing parser-local span until migration.
    pub fn from_utf8_offsets(
        file: FileRef,
        source: &str,
        start_byte: usize,
        end_byte: usize,
    ) -> Result<Self, FoundationError> {
        if start_byte > end_byte
            || end_byte > source.len()
            || !source.is_char_boundary(start_byte)
            || !source.is_char_boundary(end_byte)
        {
            return Err(FoundationError::InvalidSourceSpan);
        }
        let coordinate = |offset: usize| {
            let prior = &source[..offset];
            (
                BigInt::from(prior.bytes().filter(|byte| *byte == b'\n').count() + 1),
                BigInt::from(
                    prior
                        .rsplit('\n')
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .count()
                        + 1,
                ),
            )
        };
        let (start_line, start_column) = coordinate(start_byte);
        let (end_line, end_column) = coordinate(end_byte);
        Self::new(
            file,
            start_byte.into(),
            end_byte.into(),
            start_line,
            start_column,
            end_line,
            end_column,
        )
    }
}
/// Exact `sys.DiagnosticLabel`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticLabel {
    pub span: SourceSpan,
    pub message: String,
    pub primary: bool,
}

/// Exact `sys.Value`: a validated tag-60026 `[closed_type_descriptor, value]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysValue(CanonicalValue);
impl SysValue {
    pub fn decode(bytes: &[u8]) -> Result<Self, FoundationError> {
        Self::from_value(Value::decode(bytes).map_err(FoundationError::Value)?)
    }
    pub fn from_value(value: CanonicalValue) -> Result<Self, FoundationError> {
        if matches!(value.raw(), OvbRaw::Tag(60026, _)) {
            Ok(Self(value))
        } else {
            Err(FoundationError::ExpectedSysValue)
        }
    }
    pub fn as_value(&self) -> &CanonicalValue {
        &self.0
    }
    pub fn encode(&self) -> Result<Vec<u8>, FoundationError> {
        self.0.encode().map_err(FoundationError::Value)
    }
}
/// Exact note/help/warning/error/fatal severity required by the portable
/// diagnostic boundary and the tag-60011 live protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Note,
    Help,
    Warning,
    Error,
    Fatal,
}
/// Text admitted to a diagnostic boundary. It rejects NUL/control injection
/// and requires producers to deliberately use [`SafeText::redacted`] when the
/// source text is not safe to disclose. This is the only constructor accepted
/// by the live diagnostic builder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafeText(String);
impl SafeText {
    pub fn new(value: impl Into<String>) -> Result<Self, FoundationError> {
        let value = value.into();
        if value.chars().any(|character| {
            character == '\0' || character.is_control() && character != '\n' && character != '\t'
        }) {
            return Err(FoundationError::UnsafeDiagnosticText);
        }
        Ok(Self(value))
    }
    pub fn redacted() -> Self {
        Self("<redacted>".into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl DiagnosticSeverity {
    fn wire(self) -> u8 {
        match self {
            Self::Note => 0,
            Self::Help => 1,
            Self::Warning => 2,
            Self::Error => 3,
            Self::Fatal => 4,
        }
    }
    fn parse(value: u64) -> Result<Self, FoundationError> {
        match value {
            0 => Ok(Self::Note),
            1 => Ok(Self::Help),
            2 => Ok(Self::Warning),
            3 => Ok(Self::Error),
            4 => Ok(Self::Fatal),
            _ => Err(FoundationError::InvalidDiagnosticEncoding),
        }
    }
}
/// Exact `api/sys.json` `sys.Diagnostic` relation shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemDiagnostic {
    pub reference: DiagnosticRef,
    pub id: String,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub object: Option<ObjectRef>,
    pub definition: Option<DefinitionRef>,
    pub primary_span: Option<SourceSpan>,
    pub labels: Vec<DiagnosticLabel>,
    pub causes: Vec<SystemDiagnostic>,
    pub help: Vec<String>,
    pub data: Option<SysValue>,
    pub redacted: bool,
    pub trace: Option<TraceRef>,
}
/// Protocol span `[snapshot, file_path, start_byte, end_byte]`. The source
/// path is repository-relative or the explicit `<redacted>` marker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticSpan {
    pub snapshot: CanonicalSnapshot,
    pub file_path: String,
    pub start_byte: BigInt,
    pub end_byte: BigInt,
}
impl DiagnosticSpan {
    pub fn new(
        snapshot: CanonicalSnapshot,
        file_path: impl Into<String>,
        start_byte: BigInt,
        end_byte: BigInt,
    ) -> Result<Self, FoundationError> {
        let file_path = file_path.into();
        if file_path.is_empty()
            || (file_path != "<redacted>"
                && (!file_path.is_ascii()
                    || file_path.starts_with('/')
                    || file_path
                        .split('/')
                        .any(|x| x.is_empty() || matches!(x, "." | ".."))))
            || start_byte.sign() == Sign::Minus
            || end_byte < start_byte
        {
            return Err(FoundationError::InvalidDiagnosticSpan);
        }
        Ok(Self {
            snapshot,
            file_path,
            start_byte,
            end_byte,
        })
    }
    fn raw(&self) -> OvbRaw {
        OvbRaw::Array(vec![
            self.snapshot.raw(),
            OvbRaw::Text(self.file_path.clone()),
            OvbRaw::Int(self.start_byte.clone()),
            OvbRaw::Int(self.end_byte.clone()),
        ])
    }
    fn from_raw(raw: &OvbRaw) -> Result<Self, FoundationError> {
        let values = array(raw)?;
        if values.len() != 4 {
            return Err(FoundationError::InvalidDiagnosticEncoding);
        }
        Self::new(
            Snapshot::decode(&values[0]).map_err(FoundationError::Value)?,
            text(&values[1])?,
            integer(&values[2])?,
            integer(&values[3])?,
        )
    }
}
/// Live-protocol `Diagnostic`: tag 60011 around exact integer-key map
/// `{0: code, 1: severity, 2: message, 3: spans, 4: notes, 5: causes,
/// 6: redacted, 7?: stable diagnostic UUID}`. Causes are recursively tagged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    code: SafeText,
    severity: DiagnosticSeverity,
    message: SafeText,
    spans: Vec<DiagnosticSpan>,
    notes: Vec<SafeText>,
    causes: Vec<Diagnostic>,
    redacted: bool,
    reference: Option<[u8; 16]>,
}
impl Diagnostic {
    pub fn new(
        code: SafeText,
        severity: DiagnosticSeverity,
        message: SafeText,
    ) -> Result<Self, FoundationError> {
        if code.as_str().is_empty() {
            return Err(FoundationError::InvalidDiagnostic);
        }
        Ok(Self {
            code,
            severity,
            message,
            spans: vec![],
            notes: vec![],
            causes: vec![],
            redacted: false,
            reference: None,
        })
    }
    pub fn with_span(mut self, span: DiagnosticSpan) -> Self {
        self.spans.push(span);
        self
    }
    pub fn with_note(mut self, note: SafeText) -> Self {
        self.notes.push(note);
        self
    }
    pub fn with_cause(mut self, cause: Diagnostic) -> Self {
        self.causes.push(cause);
        self
    }
    pub fn redacted(mut self) -> Self {
        self.redacted = true;
        self
    }
    pub fn with_reference(mut self, reference: [u8; 16]) -> Self {
        self.reference = Some(reference);
        self
    }
    pub fn code(&self) -> &str {
        self.code.as_str()
    }
    pub fn message(&self) -> &str {
        self.message.as_str()
    }
    pub fn encode_ovb(&self) -> Result<Vec<u8>, FoundationError> {
        Value::new(self.raw()?)
            .map_err(FoundationError::Value)?
            .encode()
            .map_err(FoundationError::Value)
    }
    pub fn decode_ovb(bytes: &[u8]) -> Result<Self, FoundationError> {
        Self::from_raw(Value::decode(bytes).map_err(FoundationError::Value)?.raw())
    }
    fn raw(&self) -> Result<OvbRaw, FoundationError> {
        let mut fields = vec![
            (0, OvbRaw::Text(self.code.as_str().into())),
            (1, OvbRaw::Int(self.severity.wire().into())),
            (2, OvbRaw::Text(self.message.as_str().into())),
            (
                3,
                OvbRaw::Array(self.spans.iter().map(DiagnosticSpan::raw).collect()),
            ),
            (
                4,
                OvbRaw::Array(
                    self.notes
                        .iter()
                        .map(|note| OvbRaw::Text(note.as_str().into()))
                        .collect(),
                ),
            ),
            (
                5,
                OvbRaw::Array(
                    self.causes
                        .iter()
                        .map(Diagnostic::raw)
                        .collect::<Result<_, _>>()?,
                ),
            ),
            (6, OvbRaw::Bool(self.redacted)),
        ];
        if let Some(reference) = self.reference {
            fields.push((7, uuid(reference)));
        }
        Ok(OvbRaw::Tag(
            60011,
            Box::new(OvbRaw::Map(
                fields
                    .into_iter()
                    .map(|(key, value)| (OvbRaw::Int(key.into()), value))
                    .collect(),
            )),
        ))
    }
    fn from_raw(raw: &OvbRaw) -> Result<Self, FoundationError> {
        let OvbRaw::Tag(60011, body) = raw else {
            return Err(FoundationError::InvalidDiagnosticEncoding);
        };
        let fields = integer_fields(body)?;
        if fields.keys().any(|key| *key > 7) {
            return Err(FoundationError::InvalidDiagnosticEncoding);
        }
        Ok(Self {
            code: SafeText::new(text(required(&fields, 0)?)?)?,
            severity: DiagnosticSeverity::parse(natural(required(&fields, 1)?)?)?,
            message: SafeText::new(text(required(&fields, 2)?)?)?,
            spans: array_field(&fields, 3)?
                .iter()
                .map(DiagnosticSpan::from_raw)
                .collect::<Result<_, _>>()?,
            notes: array_field(&fields, 4)?
                .iter()
                .map(text)
                .map(|note| note.and_then(SafeText::new))
                .collect::<Result<_, _>>()?,
            causes: array_field(&fields, 5)?
                .iter()
                .map(Self::from_raw)
                .collect::<Result<_, _>>()?,
            redacted: boolean(required(&fields, 6)?)?,
            reference: fields.get(&7).map(|value| uuid_bytes(value)).transpose()?,
        })
    }
}

/// JSON evidence representation for the live protocol diagnostic. This is
/// intentionally separate from tag-60011: JSON is an audit/report transport,
/// whereas tag-60011 remains the normative binary codec.
///
/// Values that JSON cannot represent safely are deliberately strings: UUIDs
/// are canonical lower-case UUID text, arbitrary precision integers are base
/// ten text, and canonical OVB values are lower-case hexadecimal bytes.
impl Serialize for Diagnostic {
    fn serialize<T: Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        let mut state = serializer.serialize_struct("Diagnostic", 8)?;
        state.serialize_field("code", self.code.as_str())?;
        state.serialize_field("severity", diagnostic_severity_name(self.severity))?;
        state.serialize_field("message", self.message.as_str())?;
        state.serialize_field("spans", &DiagnosticSpans(&self.spans))?;
        state.serialize_field("notes", &SafeTexts(&self.notes))?;
        state.serialize_field("causes", &Diagnostics(&self.causes))?;
        state.serialize_field("redacted", &self.redacted)?;
        if let Some(reference) = self.reference {
            state.serialize_field("reference", &uuid_text(reference))?;
        }
        state.end()
    }
}

struct SafeTexts<'a>(&'a [SafeText]);
impl Serialize for SafeTexts<'_> {
    fn serialize<T: Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        serializer.collect_seq(self.0.iter().map(SafeText::as_str))
    }
}
struct Diagnostics<'a>(&'a [Diagnostic]);
impl Serialize for Diagnostics<'_> {
    fn serialize<T: Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        serializer.collect_seq(self.0)
    }
}
struct DiagnosticSpans<'a>(&'a [DiagnosticSpan]);
impl Serialize for DiagnosticSpans<'_> {
    fn serialize<T: Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        serializer.collect_seq(self.0.iter().map(SerializableDiagnosticSpan))
    }
}
struct SerializableDiagnosticSpan<'a>(&'a DiagnosticSpan);
impl Serialize for SerializableDiagnosticSpan<'_> {
    fn serialize<T: Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        let span = self.0;
        let snapshot = Value::new(span.snapshot.raw())
            .and_then(|value| value.encode())
            .map_err(serde::ser::Error::custom)?;
        let mut state = serializer.serialize_struct("DiagnosticSpan", 4)?;
        state.serialize_field("snapshot", &hex(&snapshot))?;
        state.serialize_field("file-path", &span.file_path)?;
        state.serialize_field("start-byte", &span.start_byte.to_string())?;
        state.serialize_field("end-byte", &span.end_byte.to_string())?;
        state.end()
    }
}

fn diagnostic_severity_name(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Note => "note",
        DiagnosticSeverity::Help => "help",
        DiagnosticSeverity::Warning => "warning",
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Fatal => "fatal",
    }
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[usize::from(byte >> 4)] as char);
        text.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    text
}
fn uuid_text(value: [u8; 16]) -> String {
    let hex = hex(&value);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Identity supplied before CWD admission, preventing cross-repository CAS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepositoryIdentity {
    pub database_id: [u8; 16],
    pub repository_id: [u8; 16],
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepositoryProfile {
    pub bare: bool,
}
/// Exact logical CWD pin; committed snapshots cannot be placed here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwdCapture {
    snapshot: CanonicalSnapshot,
    /// The durable state digest observed with this logical generation. It is
    /// part of the capture and therefore part of the compare-and-set key.
    generation_digest: [u8; 32],
}
impl CwdCapture {
    pub fn new(
        snapshot: CanonicalSnapshot,
        generation_digest: [u8; 32],
    ) -> Result<Self, FoundationError> {
        if matches!(snapshot, Snapshot::Cwd { .. }) {
            Ok(Self {
                snapshot,
                generation_digest,
            })
        } else {
            Err(FoundationError::ExpectedCwdSnapshot)
        }
    }
    pub fn database_id(&self) -> [u8; 16] {
        match &self.snapshot {
            Snapshot::Cwd { database, .. } => *database,
            Snapshot::Commit { .. } => unreachable!("CwdCapture validates CWD snapshots"),
        }
    }
    pub fn runtime_id(&self) -> [u8; 16] {
        match &self.snapshot {
            Snapshot::Cwd { runtime, .. } => *runtime,
            Snapshot::Commit { .. } => unreachable!("CwdCapture validates CWD snapshots"),
        }
    }
    pub fn generation(&self) -> &BigInt {
        match &self.snapshot {
            Snapshot::Cwd { generation, .. } => generation,
            Snapshot::Commit { .. } => unreachable!("CwdCapture validates CWD snapshots"),
        }
    }
    pub fn snapshot(&self) -> &CanonicalSnapshot {
        &self.snapshot
    }
    pub fn generation_digest(&self) -> [u8; 32] {
        self.generation_digest
    }
}
/// CAS never collapses stale state into a generic error; callers receive the
/// currently authoritative CWD pin and must retry deliberately.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CwdCas {
    Updated { current: CwdCapture },
    Stale { current: CwdCapture },
}
/// Durable runtime state owned below the repository boundary. Git never
/// manufactures a runtime UUID or logical CWD generation: the store supplies
/// both, and advances generation monotonically when it publishes a new CWD.
pub trait RuntimeIdentityStore {
    type Error: std::error::Error + Send + Sync + 'static;
    fn database_id(&self) -> Result<[u8; 16], Self::Error>;
    fn repository_id(&self) -> Result<[u8; 16], Self::Error>;
    fn runtime_id(&self) -> Result<[u8; 16], Self::Error>;
    /// Returns the persisted complete CWD capture, including the digest which
    /// binds the logical generation to the durable runtime state.
    fn capture_cwd(&self) -> Result<CwdCapture, Self::Error>;
    /// Atomically publishes `next` only when the durable current pin equals
    /// `expected`; stale publication returns the current authoritative pin.
    fn compare_and_set_cwd(
        &self,
        expected: &CwdCapture,
        next: &CwdCapture,
    ) -> Result<CwdCas, Self::Error>;
}
/// Repository/Git and runtime adapters meet at this direction-only contract.
pub trait RepositoryGenerationAdapter {
    type Error: std::error::Error + Send + Sync + 'static;
    fn require_cwd(&self) -> Result<RepositoryIdentity, Self::Error>;
    fn profile(&self) -> Result<RepositoryProfile, Self::Error>;
    fn committed_snapshot(&self) -> Result<Option<CanonicalSnapshot>, Self::Error>;
    fn capture_cwd(&self, identity: RepositoryIdentity) -> Result<CwdCapture, Self::Error>;
    fn compare_and_set_cwd(
        &self,
        identity: RepositoryIdentity,
        expected: &CwdCapture,
        next: &CwdCapture,
    ) -> Result<CwdCas, Self::Error>;
}
pub fn require_cwd_repository(profile: RepositoryProfile) -> Result<(), FoundationError> {
    if profile.bare {
        Err(FoundationError::BareRepositoryHasNoCwd)
    } else {
        Ok(())
    }
}

/// Atomic embedded reference implementation of the shared CWD CAS contract.
pub struct InMemoryRepositoryAdapter {
    identity: RepositoryIdentity,
    profile: RepositoryProfile,
    cwd: Mutex<CwdCapture>,
}
impl InMemoryRepositoryAdapter {
    pub fn new(identity: RepositoryIdentity, profile: RepositoryProfile, cwd: CwdCapture) -> Self {
        Self {
            identity,
            profile,
            cwd: Mutex::new(cwd),
        }
    }
}
#[derive(Debug)]
pub struct InMemoryRepositoryError;
impl fmt::Display for InMemoryRepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("repository adapter state unavailable")
    }
}
impl std::error::Error for InMemoryRepositoryError {}
impl RepositoryGenerationAdapter for InMemoryRepositoryAdapter {
    type Error = InMemoryRepositoryError;
    fn require_cwd(&self) -> Result<RepositoryIdentity, Self::Error> {
        require_cwd_repository(self.profile).map_err(|_| InMemoryRepositoryError)?;
        Ok(self.identity)
    }
    fn profile(&self) -> Result<RepositoryProfile, Self::Error> {
        Ok(self.profile)
    }
    fn committed_snapshot(&self) -> Result<Option<CanonicalSnapshot>, Self::Error> {
        Ok(None)
    }
    fn capture_cwd(&self, identity: RepositoryIdentity) -> Result<CwdCapture, Self::Error> {
        if identity != self.identity {
            return Err(InMemoryRepositoryError);
        }
        Ok(self
            .cwd
            .lock()
            .map_err(|_| InMemoryRepositoryError)?
            .clone())
    }
    fn compare_and_set_cwd(
        &self,
        identity: RepositoryIdentity,
        expected: &CwdCapture,
        next: &CwdCapture,
    ) -> Result<CwdCas, Self::Error> {
        if identity != self.identity {
            return Err(InMemoryRepositoryError);
        }
        let mut current = self.cwd.lock().map_err(|_| InMemoryRepositoryError)?;
        if *current != *expected {
            return Ok(CwdCas::Stale {
                current: current.clone(),
            });
        }
        *current = next.clone();
        Ok(CwdCas::Updated {
            current: next.clone(),
        })
    }
}

#[derive(Debug)]
pub enum FoundationError {
    Value(ValueError),
    InvalidSourceSpan,
    InvalidDiagnosticSpan,
    InvalidDiagnostic,
    InvalidDiagnosticEncoding,
    ExpectedSysValue,
    ExpectedCwdSnapshot,
    BareRepositoryHasNoCwd,
    UnsafeDiagnosticText,
}
impl fmt::Display for FoundationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(error) => write!(f, "canonical value error: {error}"),
            Self::InvalidSourceSpan => f.write_str("invalid source span"),
            Self::InvalidDiagnosticSpan => f.write_str("invalid diagnostic span"),
            Self::InvalidDiagnostic => f.write_str("invalid diagnostic"),
            Self::InvalidDiagnosticEncoding => f.write_str("invalid diagnostic encoding"),
            Self::ExpectedSysValue => f.write_str("expected tag 60026 sys.Value"),
            Self::ExpectedCwdSnapshot => f.write_str("expected CWD snapshot"),
            Self::BareRepositoryHasNoCwd => f.write_str("a bare repository has no CWD"),
            Self::UnsafeDiagnosticText => f.write_str("unsafe diagnostic text"),
        }
    }
}
impl std::error::Error for FoundationError {}

fn uuid(value: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(value.to_vec())))
}
fn array(raw: &OvbRaw) -> Result<&Vec<OvbRaw>, FoundationError> {
    if let OvbRaw::Array(values) = raw {
        Ok(values)
    } else {
        Err(FoundationError::InvalidDiagnosticEncoding)
    }
}
fn text(raw: &OvbRaw) -> Result<String, FoundationError> {
    if let OvbRaw::Text(value) = raw {
        Ok(value.clone())
    } else {
        Err(FoundationError::InvalidDiagnosticEncoding)
    }
}
fn integer(raw: &OvbRaw) -> Result<BigInt, FoundationError> {
    if let OvbRaw::Int(value) = raw {
        Ok(value.clone())
    } else {
        Err(FoundationError::InvalidDiagnosticEncoding)
    }
}
fn natural(raw: &OvbRaw) -> Result<u64, FoundationError> {
    integer(raw)?
        .try_into()
        .map_err(|_| FoundationError::InvalidDiagnosticEncoding)
}
fn boolean(raw: &OvbRaw) -> Result<bool, FoundationError> {
    if let OvbRaw::Bool(value) = raw {
        Ok(*value)
    } else {
        Err(FoundationError::InvalidDiagnosticEncoding)
    }
}
fn uuid_bytes(raw: &OvbRaw) -> Result<[u8; 16], FoundationError> {
    let OvbRaw::Tag(37, value) = raw else {
        return Err(FoundationError::InvalidDiagnosticEncoding);
    };
    let OvbRaw::Bytes(bytes) = value.as_ref() else {
        return Err(FoundationError::InvalidDiagnosticEncoding);
    };
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| FoundationError::InvalidDiagnosticEncoding)
}
fn integer_fields(raw: &OvbRaw) -> Result<BTreeMap<u64, &OvbRaw>, FoundationError> {
    let OvbRaw::Map(entries) = raw else {
        return Err(FoundationError::InvalidDiagnosticEncoding);
    };
    let mut fields = BTreeMap::new();
    for (key, value) in entries {
        if fields.insert(natural(key)?, value).is_some() {
            return Err(FoundationError::InvalidDiagnosticEncoding);
        }
    }
    Ok(fields)
}
fn required<'a>(
    fields: &'a BTreeMap<u64, &'a OvbRaw>,
    key: u64,
) -> Result<&'a OvbRaw, FoundationError> {
    fields
        .get(&key)
        .copied()
        .ok_or(FoundationError::InvalidDiagnosticEncoding)
}
fn array_field<'a>(
    fields: &'a BTreeMap<u64, &'a OvbRaw>,
    key: u64,
) -> Result<&'a Vec<OvbRaw>, FoundationError> {
    array(required(fields, key)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn commit() -> CanonicalSnapshot {
        Snapshot::Commit {
            database: [7; 16],
            algorithm: GitHash::Sha256,
            oid: vec![9; 32],
        }
    }
    #[test]
    fn diagnostic_ovb_round_trip_uses_tag_60011_integer_keys_and_recursive_tags() {
        let cause = Diagnostic::new(
            SafeText::new("CAUSE").unwrap(),
            DiagnosticSeverity::Help,
            SafeText::new("nested").unwrap(),
        )
        .unwrap();
        let diagnostic = Diagnostic::new(
            SafeText::new("ORNA091-E-VAR").unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new("use let").unwrap(),
        )
        .unwrap()
        .with_span(DiagnosticSpan::new(commit(), "src/main.orna", 0.into(), 3.into()).unwrap())
        .with_note(SafeText::new("safe note").unwrap())
        .with_cause(cause)
        .with_reference([3; 16]);
        let bytes = diagnostic.encode_ovb().unwrap();
        assert!(matches!(
            Value::decode(&bytes).unwrap().raw(),
            OvbRaw::Tag(60011, _)
        ));
        assert_eq!(Diagnostic::decode_ovb(&bytes).unwrap(), diagnostic);
    }
    #[test]
    fn committed_and_cwd_snapshot_pins_round_trip_without_rebinding() {
        for snapshot in [
            commit(),
            Snapshot::cwd([1; 16], [2; 16], 42.into()).unwrap(),
        ] {
            let bytes = Value::new(snapshot.raw()).unwrap().encode().unwrap();
            assert_eq!(
                Snapshot::decode(Value::decode(&bytes).unwrap().raw()).unwrap(),
                snapshot
            );
        }
    }
    #[test]
    fn bare_repository_is_rejected_before_cwd_capture() {
        assert!(matches!(
            require_cwd_repository(RepositoryProfile { bare: true }),
            Err(FoundationError::BareRepositoryHasNoCwd)
        ));
    }
    #[test]
    fn in_memory_adapter_rejects_bare_repository_before_observing_capture() {
        let capture =
            CwdCapture::new(Snapshot::cwd([1; 16], [2; 16], 0.into()).unwrap(), [3; 32]).unwrap();
        let adapter = InMemoryRepositoryAdapter::new(
            RepositoryIdentity {
                database_id: [1; 16],
                repository_id: [4; 16],
            },
            RepositoryProfile { bare: true },
            capture,
        );
        assert!(adapter.require_cwd().is_err());
    }
    #[test]
    fn source_span_uses_pinned_file_ref_and_unbounded_int_offsets() {
        let file = FileRef::from_row_ref(
            RowRef::new([1; 16], [2; 16], OvbRaw::Text("file".into()), commit()).unwrap(),
        );
        let span = SourceSpan::new(
            file,
            0.into(),
            BigInt::from(u64::MAX) + 1,
            1.into(),
            1.into(),
            1.into(),
            2.into(),
        )
        .unwrap();
        assert!(span.end_byte > BigInt::from(u64::MAX));
    }
    #[test]
    fn diagnostic_json_evidence_is_safe_lossless_and_not_the_ovb_codec() {
        let diagnostic = Diagnostic::new(
            SafeText::new("ORNA091-E-VAR").unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new("safe message").unwrap(),
        )
        .unwrap()
        .with_span(
            DiagnosticSpan::new(
                commit(),
                "src/main.orna",
                BigInt::from(u64::MAX) + 1,
                BigInt::from(u64::MAX) + 2,
            )
            .unwrap(),
        )
        .with_note(SafeText::redacted())
        .with_cause(
            Diagnostic::new(
                SafeText::new("CAUSE").unwrap(),
                DiagnosticSeverity::Help,
                SafeText::new("nested").unwrap(),
            )
            .unwrap()
            .redacted(),
        )
        .redacted()
        .with_reference([0xab; 16]);
        let json = serde_json::to_value(&diagnostic).unwrap();
        assert_eq!(json["severity"], "error");
        assert_eq!(json["reference"], "abababab-abab-abab-abab-abababababab");
        assert_eq!(json["spans"][0]["start-byte"], "18446744073709551616");
        assert!(
            json["spans"][0]["snapshot"]
                .as_str()
                .unwrap()
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        assert_eq!(json["notes"][0], "<redacted>");
        assert_eq!(json["causes"][0]["redacted"], true);
        // The audit representation admits arbitrary precision coordinates as
        // decimal text; tag-60011 remains independently verified above.
        assert!(serde_json::to_vec(&diagnostic).unwrap().starts_with(b"{"));
    }
}
