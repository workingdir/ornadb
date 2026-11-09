//! Local, redaction-safe diagnostics and admission error codes.

use std::{collections::BTreeMap, fmt};

use serde::{
    Serialize, Serializer,
    ser::{SerializeSeq, SerializeStruct},
};

use crate::ids::TypeId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArgumentTypeDetail {
    Duplicate,
    Mismatch,
    ProtectedValueNeedsIdentity,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    ArgumentMissing {
        name: String,
    },
    ArgumentUnknown {
        name: String,
    },
    ArgumentType {
        name: String,
        detail: ArgumentTypeDetail,
    },
    SnapshotMismatch,
    NotVisible,
    NotCallable,
    ReturnType,
    SourceMismatch,
    TransactionMode,
    IdempotencyMismatch,
    StartMode,
    ForeignRuntime,
    ExpiredHandle,
    TerminalInvariant,
    RuntimeUnavailable,
}
impl AdmissionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ArgumentMissing { .. } => "sys.invoke.argument_missing",
            Self::ArgumentUnknown { .. } => "sys.invoke.argument_unknown",
            Self::ArgumentType { .. } => "sys.invoke.argument_type",
            Self::SnapshotMismatch => "sys.invoke.snapshot_mismatch",
            Self::NotVisible | Self::NotCallable => "sys.invoke.not_callable",
            Self::ReturnType => "sys.invoke.return_type",
            Self::SourceMismatch => "sys.invoke.source_mismatch",
            Self::TransactionMode => "sys.invoke.transaction_mode",
            Self::IdempotencyMismatch => "sys.invoke.idempotency_mismatch",
            Self::StartMode => "sys.invoke.start_mode",
            Self::ForeignRuntime => "sys.handle.foreign_runtime",
            Self::ExpiredHandle => "sys.handle.expired",
            Self::TerminalInvariant => "sys.runtime.unavailable",
            Self::RuntimeUnavailable => "sys.runtime.unavailable",
        }
    }
    pub fn diagnostic(&self) -> Diagnostic {
        Diagnostic::new(self.code(), "reflective invocation admission rejected")
    }
}
impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for AdmissionError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AwaitError {
    Timeout,
    Admission(AdmissionError),
}
impl AwaitError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Timeout => "sys.invoke.await_timeout",
            Self::Admission(error) => error.code(),
        }
    }
}
impl fmt::Display for AwaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for AwaitError {}

/// Local, untrusted diagnostic input accepted by the sys supervisor.
///
/// This is intentionally not `sys.Diagnostic`: it has no durable identity,
/// severity, source references, or system-row authority.  A Foundation
/// adapter must construct the canonical row before this boundary can be
/// exposed as the portable relation.  Every text-bearing projection emitted
/// by this crate is redacted at serialization and explanation time.
#[derive(Clone, Eq, PartialEq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: &'static str,
    pub fields: BTreeMap<String, DiagnosticField>,
    pub causes: Vec<Diagnostic>,
}

impl Diagnostic {
    /// A diagnostic that carries only its fixed code and message, with no
    /// retained fields or causes.
    pub fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message,
            fields: BTreeMap::new(),
            causes: Vec::new(),
        }
    }
}

impl fmt::Debug for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Diagnostic")
            .field("code", &self.code)
            .field("message", &"<redacted>")
            .field("fields", &self.fields.values())
            .field("causes", &self.causes)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum DiagnosticField {
    Text(String),
    Redacted { static_type: TypeId },
}

impl fmt::Debug for DiagnosticField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let static_type = match self {
            Self::Text(_) => TypeId::new("Str"),
            Self::Redacted { static_type } => static_type.clone(),
        };
        formatter
            .debug_struct("DiagnosticField")
            .field("kind", &"redacted")
            .field("static_type", &static_type)
            .finish()
    }
}

impl Serialize for DiagnosticField {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("DiagnosticField", 2)?;
        state.serialize_field("kind", "redacted")?;
        let static_type = match self {
            Self::Text(_) => TypeId::new("Str"),
            Self::Redacted { static_type } => static_type.clone(),
        };
        state.serialize_field("static_type", &static_type)?;
        state.end()
    }
}

struct SafeDiagnosticFields<'a>(&'a BTreeMap<String, DiagnosticField>);

impl Serialize for SafeDiagnosticFields<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for field in self.0.values() {
            sequence.serialize_element(&("<redacted>", field))?;
        }
        sequence.end()
    }
}

impl Serialize for Diagnostic {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Diagnostic", 4)?;
        state.serialize_field("code", self.code)?;
        state.serialize_field("message", "<redacted>")?;
        state.serialize_field("fields", &SafeDiagnosticFields(&self.fields))?;
        state.serialize_field("causes", &self.causes)?;
        state.end()
    }
}
