//! Orna-owned source parsing at the compiler boundary.
//!
//! This crate accepts an ordered source bundle and returns lossless parsed
//! units with compiler diagnostics. Semantic analysis and revision construction
//! are separate stages.

use orna_core::{catalogue::QualifiedSemanticName, source::SourceBundle};
use orna_syntax::{
    Diagnostic as SyntaxDiagnostic, NamePart, Parse as SyntaxParse, QualifiedName, SourceSpan,
};

mod mutation;
mod prepare;
pub(crate) mod relational;
mod resolver;

pub use prepare::{PrepareError, prepare};

pub use resolver::{
    CheckReport, CheckedBundle, CheckedClientFunction, CheckedDefault, CheckedDefinitionReference,
    CheckedDefinitionReferenceTarget, CheckedExpressionId, CheckedField, CheckedFieldId,
    CheckedFunctionId, CheckedObjectType, CheckedParameterId, CheckedSchema, CheckedSchemaId,
    CheckedServerFunction, CheckedServerFunctionParameter, CheckedServerFunctionReturnColumn,
    CheckedStandardLibrary, CheckedStandardSchema, CheckedStandardTypeBinding,
    CheckedStandardValueType, CheckedTypeId, ConstantValue, ProvisionalExpressionId,
    ProvisionalFieldId, ProvisionalFunctionId, ProvisionalParameterId, ProvisionalSchemaId,
    ProvisionalTypeId, SemanticType, StandardLibraryCheckError, check,
    check_standard_library_source,
};

/// Resolves an identifier component with Orna quoted-name rules.
///
/// Unquoted identifiers are case-insensitive. Quoted identifiers retain their
/// case and unescape doubled double quotes.
pub(crate) fn normalise_name_part(part: &NamePart) -> String {
    if part.text.starts_with('"') {
        part.text[1..part.text.len() - 1].replace("\"\"", "\"")
    } else {
        part.text.to_lowercase()
    }
}

/// Resolves a qualified source name with Orna identifier rules.
pub(crate) fn normalise_qualified_name(name: &QualifiedName) -> QualifiedSemanticName {
    QualifiedSemanticName::new(name.parts.iter().map(normalise_name_part))
        .expect("parser produced a non-empty qualified name")
}

pub(crate) fn semantic_diagnostic(
    code: DiagnosticCode,
    message: impl Into<String>,
    logical_path: &str,
    span: &SourceSpan,
) -> CompilerDiagnostic {
    DiagnosticCode::semantic(
        code,
        message,
        SourceLocation::from_syntax(logical_path, span),
    )
}

/// Parses every source unit in a bundle without changing compiler state.
pub fn parse_bundle(bundle: &SourceBundle) -> ParseReport {
    let mut units = Vec::with_capacity(bundle.len());
    let mut diagnostics = Vec::new();

    for source_unit in bundle.units() {
        let parsed = orna_syntax::parse(source_unit.content());
        let logical_path = source_unit.logical_path().to_owned();

        diagnostics.extend(
            parsed
                .diagnostics()
                .iter()
                .map(|diagnostic| CompilerDiagnostic::from_syntax(&logical_path, diagnostic)),
        );
        units.push(ParsedSourceUnit {
            logical_path,
            source_text: source_unit.content().to_owned(),
            parsed,
        });
    }

    ParseReport { units, diagnostics }
}

/// The closed set of syntax diagnostic codes produced by this compiler stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    /// The source contains a token that is invalid at its current position.
    UnexpectedToken,
    /// The source ends before a comment, quoted identifier, or string closes.
    UnterminatedSourceConstruct,
    /// A referenced qualified name does not exist in the checking context.
    UnknownQualifiedName,
    /// More than one declaration has the same resolved semantic name.
    DuplicateDefinition,
    /// A declaration or constant does not satisfy its resolved type.
    TypeMismatch,
    /// A `REF` declaration targets a scalar rather than an object type.
    InvalidReferenceTarget,
    /// A declaration uses a valid construct outside this compiler domain.
    DomainIncompatible,
}

impl DiagnosticCode {
    /// Returns the stable Orna diagnostic code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnexpectedToken => "ORNA0001",
            Self::UnterminatedSourceConstruct => "ORNA0002",
            Self::UnknownQualifiedName => "ORNA0101",
            Self::DuplicateDefinition => "ORNA0103",
            Self::TypeMismatch => "ORNA0201",
            Self::InvalidReferenceTarget => "ORNA0203",
            Self::DomainIncompatible => "ORNA0303",
        }
    }

    fn from_syntax_code(code: &str) -> Self {
        match code {
            "ORNA0001" => Self::UnexpectedToken,
            "ORNA0002" => Self::UnterminatedSourceConstruct,
            _ => panic!("orna-syntax emitted a diagnostic outside the compiler syntax contract"),
        }
    }

    fn semantic(
        code: Self,
        message: impl Into<String>,
        location: SourceLocation,
    ) -> CompilerDiagnostic {
        CompilerDiagnostic {
            code,
            message: message.into(),
            location,
        }
    }
}

/// An owned byte span within one logical source unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ByteSpan {
    start: usize,
    end: usize,
}

impl ByteSpan {
    fn from_syntax_span(span: &SourceSpan) -> Self {
        Self {
            start: span.start,
            end: span.end,
        }
    }

    /// Returns the first byte in this span.
    pub const fn start(&self) -> usize {
        self.start
    }

    /// Returns the first byte after this span.
    pub const fn end(&self) -> usize {
        self.end
    }
}

/// An owned location for a compiler diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    logical_path: String,
    span: ByteSpan,
}

impl SourceLocation {
    pub(crate) fn from_syntax(logical_path: &str, span: &SourceSpan) -> Self {
        Self {
            logical_path: logical_path.to_owned(),
            span: ByteSpan::from_syntax_span(span),
        }
    }

    /// Returns the logical path submitted with the source unit.
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    /// Returns the byte span within the source unit.
    pub fn span(&self) -> &ByteSpan {
        &self.span
    }
}

/// A compiler diagnostic with an Orna-owned code and source location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerDiagnostic {
    code: DiagnosticCode,
    message: String,
    location: SourceLocation,
}

impl CompilerDiagnostic {
    fn from_syntax(logical_path: &str, diagnostic: &SyntaxDiagnostic) -> Self {
        Self {
            code: DiagnosticCode::from_syntax_code(diagnostic.code),
            message: diagnostic.message.clone(),
            location: SourceLocation::from_syntax(logical_path, &diagnostic.span),
        }
    }

    /// Returns the stable diagnostic code.
    pub const fn code(&self) -> DiagnosticCode {
        self.code
    }

    /// Returns a description of the source error.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the source unit and byte span that produced this diagnostic.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// One lossless source unit parsed by the compiler.
#[derive(Clone, Debug)]
pub struct ParsedSourceUnit {
    logical_path: String,
    source_text: String,
    parsed: SyntaxParse,
}

impl ParsedSourceUnit {
    /// Returns the submitted logical path for this source unit.
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    /// Returns the exact submitted UTF-8 source text.
    pub fn source_text(&self) -> &str {
        &self.source_text
    }

    /// Returns the exact source text represented by the private lossless CST.
    pub fn syntax_text(&self) -> String {
        self.parsed.syntax().text()
    }

    pub(crate) fn parsed(&self) -> &SyntaxParse {
        &self.parsed
    }

    #[cfg(test)]
    pub(crate) fn replace_source_text_for_test(&mut self, source_text: impl Into<String>) {
        self.source_text = source_text.into();
    }
}

/// The result of parsing one ordered source bundle.
#[derive(Clone, Debug)]
pub struct ParseReport {
    units: Vec<ParsedSourceUnit>,
    diagnostics: Vec<CompilerDiagnostic>,
}

impl ParseReport {
    /// Returns parsed source units in submitted bundle order.
    pub fn units(&self) -> &[ParsedSourceUnit] {
        &self.units
    }

    /// Returns compiler diagnostics in source-unit and source order.
    pub fn diagnostics(&self) -> &[CompilerDiagnostic] {
        &self.diagnostics
    }
}

#[cfg(test)]
mod tests {
    use orna_core::{
        CatalogueRevisionId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
        StandardLibraryRevisionId, TypeId,
        canonical_hash::{
            source_bundle_digest, source_revision_record_digest, source_unit_content_digest,
            verify_standard_library_snapshot,
        },
        catalogue::{
            CatalogueSnapshot, PreludeTypeName, QualifiedSemanticName, SchemaDefinition,
            TypeBinding, ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
        },
        revision::{
            DefinitionIdentity, DefinitionOrigin, Sha256Digest, SourceOrigin,
            StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
            StoredSourceUnit,
        },
        source::{SourceBundle, SourceUnit},
    };

    use super::{
        DiagnosticCode, StandardLibraryCheckError, check_standard_library_source, parse_bundle,
    };

    const SUCCESS_STANDARD_DIGEST: [u8; 32] = [
        0x72, 0x4b, 0x41, 0xcf, 0x68, 0x5c, 0x93, 0xa8, 0xc9, 0x8d, 0xf9, 0x3d, 0x96, 0x77, 0x98,
        0x98, 0x12, 0x34, 0xc0, 0x98, 0xf6, 0xc1, 0x00, 0xfa, 0x57, 0xe9, 0xac, 0x00, 0xdd, 0x03,
        0xfb, 0x6d,
    ];

    const CANONICAL_STANDARD_SOURCE: &str = include_str!("../../../stdlib/std/types.orna");
    const CANONICAL_RESERVED_ID: [u8; 16] = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x01,
    ];
    const CANONICAL_SCHEMA_IDS: [[u8; 16]; 2] = [
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x02,
        ],
    ];
    const CANONICAL_TYPE_IDS: [[u8; 16]; 13] = [
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x02,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x03,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x04,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x05,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x06,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x07,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x08,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x09,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x0a,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x0b,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x0c,
        ],
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x0d,
        ],
    ];
    const CANONICAL_STANDARD_DIGEST: [u8; 32] = [
        0xe5, 0x3c, 0x41, 0xa3, 0x5e, 0x1a, 0x09, 0x23, 0x80, 0x18, 0x8f, 0xd2, 0x0d, 0x24, 0xb6,
        0x32, 0x2a, 0xe8, 0x2c, 0x2d, 0x50, 0xdf, 0xb5, 0xdd, 0x05, 0x31, 0x00, 0xb5, 0x1c, 0x3b,
        0x7e, 0x9c,
    ];

    #[derive(Clone, Copy)]
    struct CanonicalValueTypeFact {
        name: &'static str,
        representation_contract: &'static str,
        persistence: ValueTypePersistence,
    }

    const CANONICAL_VALUE_TYPE_FACTS: [CanonicalValueTypeFact; 13] = [
        CanonicalValueTypeFact {
            name: "std.types.boolean",
            representation_contract: "orna.kernel.value.boolean@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.integer",
            representation_contract: "orna.kernel.value.integer@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.bigint",
            representation_contract: "orna.kernel.value.bigint@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.float",
            representation_contract: "orna.kernel.value.float@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.decimal",
            representation_contract: "orna.kernel.value.decimal@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.character_large_object",
            representation_contract: "orna.kernel.value.character-large-object@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.binary_large_object",
            representation_contract: "orna.kernel.value.binary-large-object@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.uuid",
            representation_contract: "orna.kernel.value.uuid@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.date",
            representation_contract: "orna.kernel.value.date@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.time",
            representation_contract: "orna.kernel.value.time@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.timestamp",
            representation_contract: "orna.kernel.value.timestamp@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.duration",
            representation_contract: "orna.kernel.value.duration@1",
            persistence: ValueTypePersistence::Persistable,
        },
        CanonicalValueTypeFact {
            name: "std.types.void",
            representation_contract: "orna.kernel.value.void@1",
            persistence: ValueTypePersistence::Transient,
        },
    ];

    #[derive(Clone, Copy)]
    enum CanonicalBindingKind {
        Qualified,
        Prelude,
    }

    impl CanonicalBindingKind {
        const fn catalogue_kind(self) -> orna_core::catalogue::TypeBindingKind {
            match self {
                Self::Qualified => orna_core::catalogue::TypeBindingKind::Qualified,
                Self::Prelude => orna_core::catalogue::TypeBindingKind::Prelude,
            }
        }
    }

    #[derive(Clone, Copy)]
    struct CanonicalBindingFact {
        kind: CanonicalBindingKind,
        name: &'static str,
        target_type_index: usize,
    }

    const CANONICAL_BINDING_FACTS: [CanonicalBindingFact; 30] = [
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.boolean",
            target_type_index: 0,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "boolean",
            target_type_index: 0,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "bool",
            target_type_index: 0,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.integer",
            target_type_index: 1,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "integer",
            target_type_index: 1,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "int",
            target_type_index: 1,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.bigint",
            target_type_index: 2,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "bigint",
            target_type_index: 2,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.float",
            target_type_index: 3,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "float",
            target_type_index: 3,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.decimal",
            target_type_index: 4,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "decimal",
            target_type_index: 4,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.character_large_object",
            target_type_index: 5,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "character large object",
            target_type_index: 5,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "text",
            target_type_index: 5,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.binary_large_object",
            target_type_index: 6,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "binary large object",
            target_type_index: 6,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "bytes",
            target_type_index: 6,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.uuid",
            target_type_index: 7,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "uuid",
            target_type_index: 7,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.date",
            target_type_index: 8,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "date",
            target_type_index: 8,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.time",
            target_type_index: 9,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "time",
            target_type_index: 9,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.timestamp",
            target_type_index: 10,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "timestamp",
            target_type_index: 10,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.duration",
            target_type_index: 11,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "duration",
            target_type_index: 11,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Qualified,
            name: "std.void",
            target_type_index: 12,
        },
        CanonicalBindingFact {
            kind: CanonicalBindingKind::Prelude,
            name: "void",
            target_type_index: 12,
        },
    ];

    const CANONICAL_BINDING_IDS: [[u8; 16]; 30] = [
        [
            0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd,
            0x4d, 0x31,
        ],
        [
            0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e, 0xf8,
            0x15, 0xaa,
        ],
        [
            0x7b, 0x20, 0xca, 0xb3, 0x61, 0x23, 0x35, 0x61, 0x03, 0xad, 0xab, 0x48, 0x61, 0x11,
            0x0c, 0xad,
        ],
        [
            0xf9, 0x2a, 0x68, 0x3c, 0xa4, 0x2b, 0x48, 0x2f, 0x77, 0x7a, 0x79, 0x86, 0xb2, 0xdf,
            0x25, 0x93,
        ],
        [
            0x19, 0x40, 0x9c, 0x7b, 0x37, 0x81, 0x68, 0xf8, 0x30, 0x0b, 0x44, 0x0c, 0xaf, 0x18,
            0x57, 0x78,
        ],
        [
            0x97, 0x0a, 0xa4, 0x1b, 0xb9, 0xb1, 0x99, 0xa3, 0xcb, 0xa3, 0x46, 0x8c, 0x9e, 0x7c,
            0x58, 0x89,
        ],
        [
            0x08, 0x52, 0xa1, 0xcb, 0xbe, 0x1c, 0x5b, 0x78, 0xb4, 0xfa, 0xd2, 0x9e, 0xed, 0x5b,
            0x0d, 0x1e,
        ],
        [
            0xa0, 0x50, 0x06, 0x28, 0xc9, 0x77, 0x06, 0xb2, 0xbd, 0x8f, 0x29, 0xf7, 0x8b, 0xaa,
            0x5e, 0x88,
        ],
        [
            0x30, 0x1f, 0x53, 0xba, 0x6e, 0xe1, 0xea, 0xd1, 0xe3, 0x18, 0x6b, 0x6b, 0x71, 0x9e,
            0xfc, 0xb5,
        ],
        [
            0x31, 0x03, 0xa7, 0xca, 0xfc, 0xc6, 0x3e, 0xd7, 0x2a, 0x10, 0x58, 0x00, 0x87, 0x97,
            0xb5, 0xe6,
        ],
        [
            0x28, 0x5c, 0x9a, 0x60, 0x1c, 0x08, 0x5b, 0xfa, 0xe9, 0x48, 0x5c, 0x9c, 0xb8, 0x6b,
            0x45, 0xf9,
        ],
        [
            0xdf, 0x8e, 0x7b, 0x74, 0x41, 0xca, 0xe1, 0xf8, 0xfd, 0x56, 0xd8, 0x83, 0xa3, 0x10,
            0x6e, 0xd5,
        ],
        [
            0x28, 0x67, 0x4f, 0xd2, 0x8e, 0x8a, 0x68, 0x08, 0x1e, 0x26, 0x3f, 0xb3, 0x1b, 0xc2,
            0xd8, 0x70,
        ],
        [
            0xf6, 0xd0, 0xd3, 0xb6, 0x31, 0x1b, 0x6b, 0xdc, 0xe6, 0x01, 0xd3, 0xcf, 0xc3, 0xa6,
            0x89, 0x1a,
        ],
        [
            0x72, 0x0f, 0xf6, 0x30, 0x3e, 0xf0, 0x01, 0x8c, 0x81, 0xd2, 0xa6, 0x73, 0x99, 0xf0,
            0xdb, 0xc2,
        ],
        [
            0xa9, 0x31, 0x64, 0x64, 0xe3, 0x52, 0xb5, 0x6a, 0x56, 0xa1, 0x4b, 0x38, 0x4c, 0x7d,
            0x81, 0x34,
        ],
        [
            0x15, 0x24, 0xb4, 0xca, 0x63, 0xbc, 0xe7, 0xf8, 0x9b, 0x24, 0xba, 0xf1, 0x8d, 0x33,
            0xaf, 0xbf,
        ],
        [
            0x84, 0xe0, 0x46, 0xbd, 0x87, 0xde, 0xc7, 0x0a, 0x1b, 0x73, 0x13, 0xae, 0x51, 0xb6,
            0x9d, 0xb7,
        ],
        [
            0x89, 0xea, 0x05, 0xd7, 0x14, 0xdc, 0x5d, 0x2f, 0x0a, 0x8e, 0x09, 0xf7, 0x5f, 0x31,
            0x66, 0x00,
        ],
        [
            0x73, 0xda, 0x8e, 0x2f, 0xac, 0xe9, 0x8a, 0x17, 0xa6, 0x63, 0xec, 0x97, 0xe6, 0x7c,
            0x79, 0x7f,
        ],
        [
            0xf9, 0x7c, 0x60, 0xa7, 0x50, 0x6b, 0x9e, 0x79, 0xa8, 0xa8, 0xd7, 0x84, 0xa1, 0x71,
            0xf7, 0xac,
        ],
        [
            0xf3, 0x2c, 0xab, 0x58, 0xdb, 0xdf, 0x3d, 0xc6, 0xfe, 0x7c, 0xb1, 0x74, 0x8e, 0x1f,
            0x93, 0x56,
        ],
        [
            0x15, 0x11, 0xd9, 0x2f, 0x12, 0xc3, 0x4c, 0x1b, 0x0c, 0x4c, 0x53, 0x26, 0xa8, 0xa0,
            0x34, 0x8d,
        ],
        [
            0x8b, 0xd8, 0x9d, 0x33, 0x32, 0x97, 0x8f, 0x32, 0xa7, 0xd0, 0xe1, 0xd6, 0x72, 0xd2,
            0x33, 0xd4,
        ],
        [
            0x47, 0xb0, 0x08, 0xa2, 0xdc, 0x0b, 0x20, 0xd1, 0x2b, 0x3e, 0x68, 0x9a, 0x30, 0xfc,
            0xff, 0x04,
        ],
        [
            0x84, 0x1f, 0xc4, 0xfb, 0x35, 0x7f, 0xf8, 0xc3, 0x10, 0x74, 0x4b, 0xfc, 0x97, 0x9c,
            0x8a, 0xa1,
        ],
        [
            0x36, 0x29, 0x37, 0xf6, 0x5e, 0x81, 0xf4, 0xa9, 0x45, 0x85, 0x47, 0xb4, 0xeb, 0x62,
            0x14, 0x9a,
        ],
        [
            0x6b, 0xdd, 0xb3, 0xa5, 0xf1, 0x4a, 0xc6, 0xf8, 0x42, 0x57, 0x35, 0xb8, 0x80, 0x2d,
            0xdc, 0x37,
        ],
        [
            0x82, 0xae, 0x45, 0x04, 0x07, 0xcf, 0xfa, 0xa6, 0x87, 0xe8, 0x1f, 0xa7, 0xdc, 0xbf,
            0x94, 0x0f,
        ],
        [
            0x56, 0xc5, 0x04, 0xe2, 0xf8, 0x07, 0xce, 0x24, 0xd3, 0x61, 0x11, 0xe6, 0x4a, 0x01,
            0x73, 0xfb,
        ],
    ];

    #[derive(Clone, Copy)]
    enum CanonicalDeclarationIdentity {
        Schema(usize),
        ValueType(usize),
        TypeBinding(usize),
    }

    #[derive(Clone, Copy)]
    struct CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity,
        byte_start: u32,
        byte_end: u32,
    }

    const CANONICAL_DECLARATION_FACTS: [CanonicalDeclarationFact; 45] = [
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::Schema(0),
            byte_start: 0,
            byte_end: 18,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::Schema(1),
            byte_start: 19,
            byte_end: 43,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(0),
            byte_start: 45,
            byte_end: 174,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(0),
            byte_start: 176,
            byte_end: 221,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(1),
            byte_start: 223,
            byte_end: 269,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(2),
            byte_start: 270,
            byte_end: 313,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(1),
            byte_start: 315,
            byte_end: 444,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(3),
            byte_start: 446,
            byte_end: 491,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(4),
            byte_start: 493,
            byte_end: 539,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(5),
            byte_start: 540,
            byte_end: 582,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(2),
            byte_start: 584,
            byte_end: 711,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(6),
            byte_start: 713,
            byte_end: 756,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(7),
            byte_start: 758,
            byte_end: 802,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(3),
            byte_start: 804,
            byte_end: 929,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(8),
            byte_start: 931,
            byte_end: 972,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(9),
            byte_start: 974,
            byte_end: 1016,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(4),
            byte_start: 1018,
            byte_end: 1147,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(10),
            byte_start: 1149,
            byte_end: 1194,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(11),
            byte_start: 1196,
            byte_end: 1242,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(5),
            byte_start: 1244,
            byte_end: 1403,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(12),
            byte_start: 1405,
            byte_end: 1480,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(13),
            byte_start: 1482,
            byte_end: 1558,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(14),
            byte_start: 1559,
            byte_end: 1617,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(6),
            byte_start: 1619,
            byte_end: 1772,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(15),
            byte_start: 1774,
            byte_end: 1843,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(16),
            byte_start: 1845,
            byte_end: 1915,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(17),
            byte_start: 1916,
            byte_end: 1972,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(7),
            byte_start: 1974,
            byte_end: 2097,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(18),
            byte_start: 2099,
            byte_end: 2138,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(19),
            byte_start: 2140,
            byte_end: 2180,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(8),
            byte_start: 2182,
            byte_end: 2305,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(20),
            byte_start: 2307,
            byte_end: 2346,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(21),
            byte_start: 2348,
            byte_end: 2388,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(9),
            byte_start: 2390,
            byte_end: 2513,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(22),
            byte_start: 2515,
            byte_end: 2554,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(23),
            byte_start: 2556,
            byte_end: 2596,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(10),
            byte_start: 2598,
            byte_end: 2731,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(24),
            byte_start: 2733,
            byte_end: 2782,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(25),
            byte_start: 2784,
            byte_end: 2834,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(11),
            byte_start: 2836,
            byte_end: 2967,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(26),
            byte_start: 2969,
            byte_end: 3016,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(27),
            byte_start: 3018,
            byte_end: 3066,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::ValueType(12),
            byte_start: 3068,
            byte_end: 3189,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(28),
            byte_start: 3191,
            byte_end: 3230,
        },
        CanonicalDeclarationFact {
            identity: CanonicalDeclarationIdentity::TypeBinding(29),
            byte_start: 3232,
            byte_end: 3272,
        },
    ];
    const CANONICAL_VALUE_TYPE_ORIGIN_INDICES: [usize; 13] =
        [2, 6, 10, 13, 16, 19, 23, 27, 30, 33, 36, 39, 42];
    const CANONICAL_BINDING_ORIGIN_INDICES: [usize; 30] = [
        3, 4, 5, 7, 8, 9, 11, 12, 14, 15, 17, 18, 20, 21, 22, 24, 25, 26, 28, 29, 31, 32, 34, 35,
        37, 38, 40, 41, 43, 44,
    ];

    #[test]
    fn checks_a_core_verified_non_golden_standard_source() {
        let snapshot = verified_standard_source_fixture();

        let checked = check_standard_library_source(&snapshot).unwrap();

        assert_eq!(
            checked.verified_snapshot().revision(),
            StandardLibraryRevisionId::from_bytes([7; 16])
        );
        assert_eq!(
            checked.verified_snapshot().digest(),
            Sha256Digest::from_bytes(SUCCESS_STANDARD_DIGEST)
        );
        assert_eq!(
            checked.verified_snapshot().source().id(),
            SourceRevisionId::from_bytes([6; 16])
        );
        assert_eq!(
            checked.verified_snapshot().catalogue().revision(),
            CatalogueRevisionId::from_bytes([8; 16])
        );
        assert_eq!(
            checked.verified_snapshot().source().units()[0].content(),
            "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;"
        );

        assert_eq!(checked.schemas().len(), 2);
        assert_eq!(checked.schemas()[0].id(), SchemaId::from_bytes([1; 16]));
        assert_eq!(checked.schemas()[0].name(), &semantic_name(["std"]));
        assert_eq!(checked.schemas()[0].origin(), source_origin(0, 18));
        assert_eq!(checked.schemas()[1].id(), SchemaId::from_bytes([2; 16]));
        assert_eq!(
            checked.schemas()[1].name(),
            &semantic_name(["std", "types"])
        );
        assert_eq!(checked.schemas()[1].origin(), source_origin(18, 42));

        assert_eq!(checked.value_types().len(), 1);
        let value_type = &checked.value_types()[0];
        assert_eq!(value_type.id(), TypeId::from_bytes([3; 16]));
        assert_eq!(
            value_type.name(),
            &semantic_name(["std", "types", "boolean"])
        );
        assert_eq!(
            value_type.kind(),
            orna_core::catalogue::ValueTypeKind::Primitive
        );
        assert_eq!(value_type.mutability(), ValueTypeMutability::Immutable);
        assert_eq!(value_type.persistence(), ValueTypePersistence::Persistable);
        assert_eq!(
            value_type.representation_contract(),
            "orna.kernel.value.boolean@1"
        );
        assert_eq!(value_type.origin(), source_origin(42, 159));

        assert_eq!(checked.type_bindings().len(), 2);
        let qualified = &checked.type_bindings()[0];
        assert_eq!(
            qualified.id().to_bytes(),
            [
                0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd,
                0x4d, 0x31,
            ]
        );
        assert_eq!(
            qualified.kind(),
            orna_core::catalogue::TypeBindingKind::Qualified
        );
        assert_eq!(
            qualified.name(),
            &orna_core::catalogue::TypeLookupName::qualified(semantic_name(["std", "boolean"]))
        );
        assert_eq!(qualified.target(), TypeId::from_bytes([3; 16]));
        assert_eq!(qualified.origin(), source_origin(159, 204));

        let prelude = &checked.type_bindings()[1];
        assert_eq!(
            prelude.id().to_bytes(),
            [
                0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e, 0xf8,
                0x15, 0xaa,
            ]
        );
        assert_eq!(
            prelude.kind(),
            orna_core::catalogue::TypeBindingKind::Prelude
        );
        assert_eq!(
            prelude.name(),
            &orna_core::catalogue::TypeLookupName::prelude(
                PreludeTypeName::new(["boolean"]).unwrap()
            )
        );
        assert_eq!(prelude.target(), TypeId::from_bytes([3; 16]));
        assert_eq!(prelude.origin(), source_origin(204, 250));

        assert_eq!(checked.schemas()[0].clone(), checked.schemas()[0]);
        assert_eq!(value_type.clone(), *value_type);
        assert_eq!(qualified.clone(), *qualified);
    }

    #[test]
    fn checks_the_accepted_canonical_standard_source() {
        let snapshot = verified_canonical_standard_source_fixture();

        let checked = check_standard_library_source(&snapshot).unwrap();

        assert_eq!(
            checked.verified_snapshot().revision().to_bytes(),
            CANONICAL_RESERVED_ID
        );
        assert_eq!(
            checked
                .verified_snapshot()
                .catalogue()
                .revision()
                .to_bytes(),
            CANONICAL_RESERVED_ID
        );
        assert_eq!(
            checked.verified_snapshot().source().id().to_bytes(),
            CANONICAL_RESERVED_ID
        );
        assert_eq!(
            checked.verified_snapshot().source().units()[0]
                .id()
                .to_bytes(),
            CANONICAL_RESERVED_ID
        );
        assert_eq!(
            checked.verified_snapshot().source().units()[0].logical_path(),
            "std/types.orna"
        );
        assert_eq!(
            checked.verified_snapshot().source().units()[0].content(),
            CANONICAL_STANDARD_SOURCE
        );
        assert_eq!(
            checked.verified_snapshot().digest().to_bytes(),
            CANONICAL_STANDARD_DIGEST
        );
        assert_eq!(CANONICAL_STANDARD_SOURCE.len(), 3273);

        assert_eq!(checked.schemas().len(), 2);
        assert_eq!(checked.value_types().len(), 13);
        assert_eq!(
            checked
                .type_bindings()
                .iter()
                .filter(|binding| binding.kind() == orna_core::catalogue::TypeBindingKind::Qualified)
                .count(),
            13
        );
        assert_eq!(
            checked
                .type_bindings()
                .iter()
                .filter(|binding| binding.kind() == orna_core::catalogue::TypeBindingKind::Prelude)
                .count(),
            17
        );

        for (index, schema) in checked.schemas().iter().enumerate() {
            let expected_name = ["std", "std.types"][index];
            assert_eq!(schema.id().to_bytes(), CANONICAL_SCHEMA_IDS[index]);
            assert_eq!(schema.name().to_string(), expected_name);
            assert_eq!(
                schema.origin(),
                canonical_source_origin(CANONICAL_DECLARATION_FACTS[index])
            );
        }
        for (index, value_type) in checked.value_types().iter().enumerate() {
            let expected = CANONICAL_VALUE_TYPE_FACTS[index];
            assert_eq!(value_type.id().to_bytes(), CANONICAL_TYPE_IDS[index]);
            assert_eq!(value_type.name().to_string(), expected.name);
            assert_eq!(
                value_type.kind(),
                orna_core::catalogue::ValueTypeKind::Primitive
            );
            assert_eq!(value_type.mutability(), ValueTypeMutability::Immutable);
            assert_eq!(value_type.persistence(), expected.persistence);
            assert_eq!(
                value_type.representation_contract(),
                expected.representation_contract
            );
            assert_eq!(
                value_type.origin(),
                canonical_source_origin(
                    CANONICAL_DECLARATION_FACTS[CANONICAL_VALUE_TYPE_ORIGIN_INDICES[index]]
                )
            );
        }
        for (index, binding) in checked.type_bindings().iter().enumerate() {
            let expected = CANONICAL_BINDING_FACTS[index];
            assert_eq!(binding.id().to_bytes(), CANONICAL_BINDING_IDS[index]);
            assert_eq!(binding.kind(), expected.kind.catalogue_kind());
            assert_eq!(binding.name().to_string(), expected.name);
            assert_eq!(
                binding.target().to_bytes(),
                CANONICAL_TYPE_IDS[expected.target_type_index]
            );
            assert_eq!(
                binding.origin(),
                canonical_source_origin(
                    CANONICAL_DECLARATION_FACTS[CANONICAL_BINDING_ORIGIN_INDICES[index]]
                )
            );
        }
    }

    #[test]
    fn counts_source_units_before_parsing_or_reconciling() {
        let empty = verified_empty_catalogue_fixture(
            &[],
            [
                0x82, 0xba, 0xf7, 0xee, 0xe1, 0x53, 0x2a, 0x90, 0xa8, 0x10, 0x14, 0x71, 0x0d, 0xa6,
                0xb7, 0x07, 0xb0, 0x37, 0x24, 0xe6, 0x62, 0xce, 0x4b, 0x3a, 0xe7, 0x07, 0xa4, 0xc7,
                0x78, 0xd3, 0xe9, 0xb1,
            ],
        );
        let two = verified_empty_catalogue_fixture(
            &[
                ("std/one.orna", "CREATE SCHEMA std.;"),
                ("std/two.orna", ""),
            ],
            [
                0x71, 0xf0, 0x0f, 0x75, 0x50, 0x39, 0xd7, 0xa5, 0x54, 0xc4, 0xec, 0xb1, 0x80, 0x92,
                0xdf, 0x21, 0xe3, 0x30, 0xc0, 0x28, 0x69, 0xd7, 0xa1, 0xbd, 0xa9, 0xbc, 0x8e, 0xb2,
                0xbe, 0x32, 0x3e, 0xba,
            ],
        );

        assert_eq!(
            check_standard_library_source(&empty).unwrap_err(),
            StandardLibraryCheckError::SourceUnitCount { actual: 0 }
        );
        assert_eq!(
            check_standard_library_source(&two).unwrap_err(),
            StandardLibraryCheckError::SourceUnitCount { actual: 2 }
        );
    }

    #[test]
    fn returns_parser_diagnostics_before_reconciliation() {
        const SOURCE: &str = "CREATE SCHEMA std.;CREATE SCHEMA ;CREATE SCHEMA std;";
        let parsed = parse_bundle(
            &SourceBundle::new([SourceUnit::new("std/malformed.orna", SOURCE)]).unwrap(),
        );
        assert_eq!(parsed.units()[0].parsed().schemas().len(), 1);
        assert_eq!(parsed.diagnostics().len(), 2);

        let snapshot = verified_empty_catalogue_fixture(
            &[("std/malformed.orna", SOURCE)],
            [
                0x6d, 0x3f, 0xaa, 0x32, 0x82, 0x0e, 0xeb, 0x73, 0x77, 0xc5, 0xbd, 0xfa, 0x3e, 0x8d,
                0x6c, 0xaf, 0xdc, 0x95, 0xa6, 0x7c, 0xbd, 0xef, 0x5b, 0x02, 0x63, 0x1f, 0x29, 0x1d,
                0x14, 0xcc, 0x68, 0xae,
            ],
        );

        let error = check_standard_library_source(&snapshot).unwrap_err();

        assert!(matches!(
            error,
            StandardLibraryCheckError::Diagnostics { .. }
        ));
        let StandardLibraryCheckError::Diagnostics { diagnostics } = error else {
            return;
        };
        assert_eq!(diagnostics, parsed.diagnostics());
    }

    #[test]
    fn standard_source_check_errors_have_the_documented_messages_and_no_causes() {
        let errors = [
            StandardLibraryCheckError::SourceUnitCount { actual: 9 },
            StandardLibraryCheckError::Diagnostics {
                diagnostics: Vec::new(),
            },
            StandardLibraryCheckError::SourceMismatch,
        ];
        let messages = [
            "the verified standard library has 9 source units, expected exactly one",
            "the verified standard library source has compiler diagnostics",
            "the verified standard library source does not match its catalogue and origins",
        ];

        for (error, message) in errors.iter().zip(messages) {
            assert_eq!(error.to_string(), message);
            assert!(std::error::Error::source(error).is_none());
        }
    }

    fn verified_standard_source_fixture() -> orna_core::revision::VerifiedStandardLibrarySnapshot {
        const SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;";

        let source_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([4; 16]),
            0,
            "std/types.orna",
            SOURCE,
            source_unit_content_digest(SOURCE).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([5; 16]),
            SourceRevisionId::from_bytes([6; 16]),
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(SourceBundleId::from_bytes([5; 16]), None, bundle_hash)
                .unwrap(),
        )
        .unwrap();

        let boolean = ValueTypeDefinition::primitive(
            TypeId::from_bytes([3; 16]),
            semantic_name(["std", "types", "boolean"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );
        let qualified =
            TypeBinding::qualified(semantic_name(["std", "boolean"]), boolean.id()).unwrap();
        let prelude =
            TypeBinding::prelude(PreludeTypeName::new(["boolean"]).unwrap(), boolean.id()).unwrap();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([8; 16]),
            vec![
                SchemaDefinition::new(SchemaId::from_bytes([1; 16]), semantic_name(["std"])),
                SchemaDefinition::new(
                    SchemaId::from_bytes([2; 16]),
                    semantic_name(["std", "types"]),
                ),
            ],
            vec![],
            vec![boolean],
            vec![qualified.clone(), prelude.clone()],
        )
        .unwrap();
        let origins = vec![
            origin(
                DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
                0,
                18,
            ),
            origin(
                DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
                18,
                42,
            ),
            origin(
                DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
                42,
                159,
            ),
            origin(DefinitionIdentity::TypeBinding(qualified.id()), 159, 204),
            origin(DefinitionIdentity::TypeBinding(prelude.id()), 204, 250),
        ];
        let snapshot = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([7; 16]),
            StandardLibraryDigestVersion::Version1,
            source,
            "orna.language/1",
            catalogue,
            origins,
            Sha256Digest::from_bytes(SUCCESS_STANDARD_DIGEST),
        )
        .unwrap();

        verify_standard_library_snapshot(snapshot).unwrap()
    }

    fn verified_canonical_standard_source_fixture()
    -> orna_core::revision::VerifiedStandardLibrarySnapshot {
        let source_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes(CANONICAL_RESERVED_ID),
            0,
            "std/types.orna",
            CANONICAL_STANDARD_SOURCE,
            source_unit_content_digest(CANONICAL_STANDARD_SOURCE).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes(CANONICAL_RESERVED_ID),
            SourceRevisionId::from_bytes(CANONICAL_RESERVED_ID),
            None,
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes(CANONICAL_RESERVED_ID),
                None,
                bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let value_types = CANONICAL_VALUE_TYPE_FACTS
            .iter()
            .enumerate()
            .map(|(index, fact)| {
                ValueTypeDefinition::primitive(
                    TypeId::from_bytes(CANONICAL_TYPE_IDS[index]),
                    semantic_name_from_dotted(fact.name),
                    ValueTypeMutability::Immutable,
                    fact.persistence,
                    fact.representation_contract,
                )
            })
            .collect();
        let type_bindings = CANONICAL_BINDING_FACTS
            .iter()
            .map(|fact| {
                let target = TypeId::from_bytes(CANONICAL_TYPE_IDS[fact.target_type_index]);
                match fact.kind {
                    CanonicalBindingKind::Qualified => {
                        TypeBinding::qualified(semantic_name_from_dotted(fact.name), target)
                    }
                    CanonicalBindingKind::Prelude => TypeBinding::prelude(
                        PreludeTypeName::new(fact.name.split(' ')).unwrap(),
                        target,
                    ),
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes(CANONICAL_RESERVED_ID),
            vec![
                SchemaDefinition::new(
                    SchemaId::from_bytes(CANONICAL_SCHEMA_IDS[0]),
                    semantic_name_from_dotted("std"),
                ),
                SchemaDefinition::new(
                    SchemaId::from_bytes(CANONICAL_SCHEMA_IDS[1]),
                    semantic_name_from_dotted("std.types"),
                ),
            ],
            vec![],
            value_types,
            type_bindings,
        )
        .unwrap();
        let snapshot = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes(CANONICAL_RESERVED_ID),
            StandardLibraryDigestVersion::Version1,
            source,
            "orna.language/1",
            catalogue,
            canonical_origins(),
            Sha256Digest::from_bytes(CANONICAL_STANDARD_DIGEST),
        )
        .unwrap();

        verify_standard_library_snapshot(snapshot).unwrap()
    }

    fn canonical_origins() -> Vec<DefinitionOrigin> {
        CANONICAL_DECLARATION_FACTS
            .iter()
            .map(|fact| {
                DefinitionOrigin::new(
                    canonical_definition_identity(fact.identity),
                    canonical_source_origin(*fact),
                )
            })
            .collect()
    }

    fn canonical_definition_identity(identity: CanonicalDeclarationIdentity) -> DefinitionIdentity {
        match identity {
            CanonicalDeclarationIdentity::Schema(index) => {
                DefinitionIdentity::Schema(SchemaId::from_bytes(CANONICAL_SCHEMA_IDS[index]))
            }
            CanonicalDeclarationIdentity::ValueType(index) => {
                DefinitionIdentity::ValueType(TypeId::from_bytes(CANONICAL_TYPE_IDS[index]))
            }
            CanonicalDeclarationIdentity::TypeBinding(index) => DefinitionIdentity::TypeBinding(
                orna_core::TypeBindingId::from_bytes(CANONICAL_BINDING_IDS[index]),
            ),
        }
    }

    fn canonical_source_origin(fact: CanonicalDeclarationFact) -> SourceOrigin {
        SourceOrigin::new(
            SourceUnitId::from_bytes(CANONICAL_RESERVED_ID),
            fact.byte_start,
            fact.byte_end,
        )
        .unwrap()
    }

    fn semantic_name_from_dotted(name: &str) -> QualifiedSemanticName {
        QualifiedSemanticName::new(name.split('.')).unwrap()
    }

    fn semantic_name<const COUNT: usize>(parts: [&str; COUNT]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts).unwrap()
    }

    fn origin(identity: DefinitionIdentity, byte_start: u32, byte_end: u32) -> DefinitionOrigin {
        DefinitionOrigin::new(identity, source_origin(byte_start, byte_end))
    }

    fn source_origin(byte_start: u32, byte_end: u32) -> SourceOrigin {
        SourceOrigin::new(SourceUnitId::from_bytes([4; 16]), byte_start, byte_end).unwrap()
    }

    fn verified_empty_catalogue_fixture(
        units: &[(&str, &str)],
        standard_digest: [u8; 32],
    ) -> orna_core::revision::VerifiedStandardLibrarySnapshot {
        let stored_units = units
            .iter()
            .enumerate()
            .map(|(ordinal, (logical_path, content))| {
                StoredSourceUnit::new(
                    SourceUnitId::from_bytes([u8::try_from(ordinal + 4).unwrap(); 16]),
                    u32::try_from(ordinal).unwrap(),
                    *logical_path,
                    *content,
                    source_unit_content_digest(content).unwrap(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let bundle_hash = source_bundle_digest(&stored_units).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([5; 16]),
            SourceRevisionId::from_bytes([6; 16]),
            None,
            stored_units,
            bundle_hash,
            source_revision_record_digest(SourceBundleId::from_bytes([5; 16]), None, bundle_hash)
                .unwrap(),
        )
        .unwrap();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([8; 16]),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap();
        let snapshot = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([7; 16]),
            StandardLibraryDigestVersion::Version1,
            source,
            "orna.language/1",
            catalogue,
            vec![],
            Sha256Digest::from_bytes(standard_digest),
        )
        .unwrap();

        verify_standard_library_snapshot(snapshot).unwrap()
    }

    #[test]
    fn retains_exact_source_and_syntax_text_in_bundle_order() {
        let first_source = "-- customer source\r\nCREATE SCHEMA crm;  \r\n";
        let second_source = "/* task source */\nCREATE SCHEMA tasks;\n";
        let bundle = SourceBundle::new([
            SourceUnit::new("crm/schema.orna", first_source),
            SourceUnit::new("tasks/schema.orna", second_source),
        ])
        .unwrap();

        let report = parse_bundle(&bundle);

        assert!(report.diagnostics().is_empty());
        assert_eq!(report.units().len(), 2);
        assert_eq!(report.units()[0].logical_path(), "crm/schema.orna");
        assert_eq!(report.units()[0].source_text(), first_source);
        assert_eq!(report.units()[0].syntax_text(), first_source);
        assert_eq!(report.units()[1].logical_path(), "tasks/schema.orna");
        assert_eq!(report.units()[1].source_text(), second_source);
        assert_eq!(report.units()[1].syntax_text(), second_source);
    }

    #[test]
    fn parses_later_units_after_an_earlier_syntax_error() {
        let bundle = SourceBundle::new([
            SourceUnit::new("broken.orna", "CREATE SCHEMA crm.;"),
            SourceUnit::new("valid.orna", "-- retained\nCREATE SCHEMA tasks;"),
        ])
        .unwrap();

        let report = parse_bundle(&bundle);

        assert_eq!(report.units().len(), 2);
        assert_eq!(
            report.units()[1].syntax_text(),
            "-- retained\nCREATE SCHEMA tasks;"
        );
        assert_eq!(report.diagnostics().len(), 1);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::UnexpectedToken
        );
    }

    #[test]
    fn maps_syntax_codes_and_retains_owned_paths_and_byte_spans() {
        let bundle = SourceBundle::new([
            SourceUnit::new("syntax.orna", "CREATE SCHEMA crm.;"),
            SourceUnit::new("comment.orna", "/* unfinished"),
        ])
        .unwrap();

        let report = parse_bundle(&bundle);

        assert_eq!(report.diagnostics().len(), 2);
        assert_eq!(
            report.diagnostics()[0].code(),
            DiagnosticCode::UnexpectedToken
        );
        assert_eq!(report.diagnostics()[0].code().as_str(), "ORNA0001");
        assert_eq!(
            report.diagnostics()[0].location().logical_path(),
            "syntax.orna"
        );
        assert_eq!(report.diagnostics()[0].location().span().start(), 18);
        assert_eq!(report.diagnostics()[0].location().span().end(), 19);
        assert_eq!(
            report.diagnostics()[1].code(),
            DiagnosticCode::UnterminatedSourceConstruct
        );
        assert_eq!(report.diagnostics()[1].code().as_str(), "ORNA0002");
        assert_eq!(
            report.diagnostics()[1].location().logical_path(),
            "comment.orna"
        );
        assert_eq!(report.diagnostics()[1].location().span().start(), 0);
        assert_eq!(report.diagnostics()[1].location().span().end(), 13);
    }
}
