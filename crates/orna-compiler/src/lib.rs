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

pub use prepare::{
    PrepareError, PrepareStandardApplicationError, PrepareStandardUpgradeError,
    PreparedStandardUpgrade, StandardSourceIdentitySeed, StandardUpgradeIdentity, prepare,
    prepare_checked_standard_upgrade, prepare_standard_application, prepare_standard_source,
};

pub use orna_core::revision::EMPTY_APPLICATION_CATALOGUE_REVISION_ID;
pub use resolver::{
    CheckReport, CheckedApplicationTypeUse, CheckedBundle, CheckedClientBodyKind,
    CheckedClientCapability, CheckedClientCapabilityArgument, CheckedClientFunction,
    CheckedDefault, CheckedDefinitionReference, CheckedDefinitionReferenceTarget,
    CheckedExpressionId, CheckedField, CheckedFieldId, CheckedFunctionId,
    CheckedObjectReferenceUse, CheckedObjectType, CheckedParameterId, CheckedSchema,
    CheckedSchemaId, CheckedServerFunction, CheckedServerFunctionParameter,
    CheckedServerFunctionReturnColumn, CheckedStandardExecutable, CheckedStandardJsonEncode,
    CheckedStandardLibrary, CheckedStandardParameterEcho, CheckedStandardSchema,
    CheckedStandardTerminalPresentTable, CheckedStandardTypeBinding, CheckedStandardTypeReference,
    CheckedStandardUiConstructor, CheckedStandardUiWindow, CheckedStandardValueType, CheckedTypeId,
    CheckedTypeUseKind, CheckedValueTypeUse, ConstantValue, NewApplicationCheckError,
    ProvisionalExpressionId, ProvisionalFieldId, STANDARD_LIBRARY_V3_REVISION_ID,
    STANDARD_LIBRARY_V4_REVISION_ID, STANDARD_LIBRARY_V5_REVISION_ID,
    STANDARD_LIBRARY_V6_REVISION_ID, STANDARD_LIBRARY_V7_REVISION_ID,
    STANDARD_LIBRARY_V8_REVISION_ID, STANDARD_LIBRARY_V9_REVISION_ID,
    STANDARD_LIBRARY_V10_REVISION_ID, STD_ACTION_SCHEMA_ID, STD_ACTION_SOURCE_UNIT_ID,
    STD_ACTION_TYPE_ID, STD_BOOLEAN_TYPE_ID, STD_CHARACTER_LARGE_OBJECT_TYPE_ID,
    STD_CLI_REPL_FUNCTION_ID, STD_CLI_REPL_FUNCTION_REVISION_ID, STD_CLI_REPL_REVISION_NUMBER,
    STD_CLI_SCHEMA_ID, STD_CLI_SOURCE_UNIT_ID, STD_CSV_ENCODE_FUNCTION_ID,
    STD_DATA_ROWS_TYPE_BINDING_ID, STD_DATA_ROWS_TYPE_ID, STD_DATA_SCHEMA_ID,
    STD_DATA_SOURCE_UNIT_ID, STD_INTEGER_TYPE_ID, STD_INVOKE_ECHO_FUNCTION_ID,
    STD_INVOKE_ECHO_FUNCTION_REVISION_ID, STD_INVOKE_ECHO_PARAMETER_ID,
    STD_INVOKE_ECHO_REVISION_NUMBER, STD_INVOKE_SCHEMA_ID, STD_INVOKE_SOURCE_UNIT_ID,
    STD_IO_BYTE_STREAM_TYPE_ID, STD_IO_SCHEMA_ID, STD_JSON_ENCODE_FUNCTION_ID,
    STD_JSON_ENCODE_FUNCTION_REVISION_ID, STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_SCHEMA_ID,
    STD_JSON_SOURCE_UNIT_ID, STD_JSON_VALUE_TYPE_ID, STD_OUTPUT_SOURCE_UNIT_ID,
    STD_TERMINAL_DOCUMENT_TYPE_ID, STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
    STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID, STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
    STD_TERMINAL_SCHEMA_ID, STD_TYPES_SOURCE_UNIT_ID, STD_UI_BUTTON_ENABLED_PARAMETER_ID,
    STD_UI_BUTTON_FUNCTION_ID, STD_UI_BUTTON_FUNCTION_REVISION_ID,
    STD_UI_BUTTON_LABEL_PARAMETER_ID, STD_UI_BUTTON_RUNTIME_CONTRACT,
    STD_UI_COLUMN_CONTENT_PARAMETER_ID, STD_UI_COLUMN_FUNCTION_ID,
    STD_UI_COLUMN_FUNCTION_REVISION_ID, STD_UI_COLUMN_RUNTIME_CONTRACT,
    STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID, STD_UI_PANEL_CONTENT_PARAMETER_ID,
    STD_UI_PANEL_FUNCTION_ID, STD_UI_PANEL_FUNCTION_REVISION_ID, STD_UI_PANEL_RUNTIME_CONTRACT,
    STD_UI_ROW_CONTENT_PARAMETER_ID, STD_UI_ROW_FUNCTION_ID, STD_UI_ROW_FUNCTION_REVISION_ID,
    STD_UI_ROW_RUNTIME_CONTRACT, STD_UI_SCHEMA_ID, STD_UI_SOURCE_UNIT_ID,
    STD_UI_TABS_CONTENT_PARAMETER_ID, STD_UI_TABS_FUNCTION_ID, STD_UI_TABS_FUNCTION_REVISION_ID,
    STD_UI_TABS_RUNTIME_CONTRACT, STD_UI_TEXT_FUNCTION_ID, STD_UI_TEXT_FUNCTION_REVISION_ID,
    STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID, STD_UI_TEXT_INPUT_FUNCTION_ID,
    STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID, STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
    STD_UI_TEXT_INPUT_RUNTIME_CONTRACT, STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
    STD_UI_TEXT_PARAMETER_ID, STD_UI_TEXT_RUNTIME_CONTRACT, STD_UI_TYPE_ID,
    STD_UI_WINDOW_CONTENT_PARAMETER_ID, STD_UI_WINDOW_FUNCTION_ID,
    STD_UI_WINDOW_FUNCTION_REVISION_ID, STD_UI_WINDOW_REVISION_NUMBER,
    STD_UI_WINDOW_RUNTIME_CONTRACT, STD_UI_WINDOW_TITLE_PARAMETER_ID, STD_WINDOW_SOURCE_UNIT_ID,
    SemanticType, StandardApplicationCheckContext, StandardApplicationCheckReport,
    StandardApplicationContextError, StandardLibraryCheckError, check, check_new_application,
    check_standard_application, check_standard_cli_repl, check_standard_json_encode,
    check_standard_library_source, check_standard_parameter_echo, check_standard_source,
    check_standard_source_v11, check_standard_terminal_present_table,
    check_standard_ui_constructor, check_standard_ui_window,
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

/// The user-facing importance of one compiler diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    /// Compilation cannot continue until the diagnostic is resolved.
    Error,
    /// Compilation can continue, but the source contains a suspicious construct.
    Warning,
}

impl DiagnosticSeverity {
    /// Returns the stable lowercase severity name used by diagnostic renderers.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// The closed set of stable compiler diagnostic categories.
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
    /// A CLIENT capability requirement is outside the closed vocabulary or body subset.
    CapabilityRequirement,
    /// A declaration uses a valid construct outside this compiler domain.
    DomainIncompatible,
    /// A statement cannot execute because an earlier statement always returns.
    UnreachableCode,
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
            Self::CapabilityRequirement => "ORNA0304",
            Self::DomainIncompatible => "ORNA0303",
            Self::UnreachableCode => "ORNA0401",
        }
    }

    /// Returns the short, user-facing title for this diagnostic category.
    pub const fn title(self) -> &'static str {
        match self {
            Self::UnexpectedToken => "unexpected syntax",
            Self::UnterminatedSourceConstruct => "unterminated source construct",
            Self::UnknownQualifiedName => "unknown name",
            Self::DuplicateDefinition => "duplicate definition",
            Self::TypeMismatch => "type mismatch",
            Self::InvalidReferenceTarget => "invalid reference target",
            Self::CapabilityRequirement => "unsupported capability",
            Self::DomainIncompatible => "unsupported construct",
            Self::UnreachableCode => "unreachable code",
        }
    }

    /// Returns a stable, user-facing next action for this diagnostic category.
    pub const fn help(self) -> Option<&'static str> {
        match self {
            Self::UnexpectedToken => Some("check the syntax at this location"),
            Self::UnterminatedSourceConstruct => {
                Some("close the comment, quoted identifier, or string literal")
            }
            Self::UnknownQualifiedName => {
                Some("check the name against the declarations in the source bundle")
            }
            Self::DuplicateDefinition => Some("give each declaration a distinct semantic name"),
            Self::TypeMismatch => Some("check the declared type and the value or expression"),
            Self::InvalidReferenceTarget => Some("use REF only with an object type"),
            Self::CapabilityRequirement => Some("use a supported client capability"),
            Self::DomainIncompatible => Some("use this construct in its supported function domain"),
            Self::UnreachableCode => {
                Some("remove the unreachable code or move it before the preceding return")
            }
        }
    }

    /// Returns a short explanation of the diagnostic category.
    pub const fn summary(self) -> &'static str {
        match self {
            Self::UnexpectedToken => "The source contains invalid syntax.",
            Self::UnterminatedSourceConstruct => "A source construct is not closed.",
            Self::UnknownQualifiedName => "A referenced name does not exist.",
            Self::DuplicateDefinition => "A name is declared more than once.",
            Self::TypeMismatch => "A value does not match its declared type.",
            Self::InvalidReferenceTarget => "A reference targets an invalid type.",
            Self::CapabilityRequirement => "A client capability is not supported.",
            Self::DomainIncompatible => "A valid construct is outside this compiler domain.",
            Self::UnreachableCode => "A statement cannot execute.",
        }
    }

    /// Returns the severity assigned to this stable diagnostic category.
    pub const fn severity(self) -> DiagnosticSeverity {
        match self {
            Self::UnreachableCode => DiagnosticSeverity::Warning,
            Self::UnexpectedToken
            | Self::UnterminatedSourceConstruct
            | Self::UnknownQualifiedName
            | Self::DuplicateDefinition
            | Self::TypeMismatch
            | Self::InvalidReferenceTarget
            | Self::CapabilityRequirement
            | Self::DomainIncompatible => DiagnosticSeverity::Error,
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
            primary_label: None,
            help: None,
            notes: Vec::new(),
            related: Vec::new(),
        }
    }

    /// Converts a syntax diagnostic code into the compiler's stable category.
    fn from_syntax_code(code: &'static str) -> Self {
        match code {
            "ORNA0001" => Self::UnexpectedToken,
            "ORNA0002" => Self::UnterminatedSourceConstruct,
            "ORNA0101" => Self::UnknownQualifiedName,
            "ORNA0103" => Self::DuplicateDefinition,
            "ORNA0201" => Self::TypeMismatch,
            "ORNA0203" => Self::InvalidReferenceTarget,
            "ORNA0304" => Self::CapabilityRequirement,
            "ORNA0303" => Self::DomainIncompatible,
            _ => Self::UnexpectedToken,
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

/// A secondary source location that explains one compiler diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticLabel {
    location: SourceLocation,
    message: String,
}

impl DiagnosticLabel {
    /// Returns the exact related source location.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }

    /// Returns the explanation attached to the related source location.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A compiler diagnostic with an Orna-owned code and source location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerDiagnostic {
    code: DiagnosticCode,
    message: String,
    location: SourceLocation,
    primary_label: Option<String>,
    help: Option<String>,
    notes: Vec<String>,
    related: Vec<DiagnosticLabel>,
}

impl CompilerDiagnostic {
    fn from_syntax(logical_path: &str, diagnostic: &SyntaxDiagnostic) -> Self {
        Self {
            code: DiagnosticCode::from_syntax_code(diagnostic.code),
            message: diagnostic.message.clone(),
            location: SourceLocation::from_syntax(logical_path, &diagnostic.span),
            primary_label: None,
            help: None,
            notes: Vec::new(),
            related: Vec::new(),
        }
    }

    /// Returns the stable diagnostic code.
    pub const fn code(&self) -> DiagnosticCode {
        self.code
    }

    /// Returns the diagnostic's detailed user-facing message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the source unit and byte span that produced this diagnostic.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }

    /// Returns this diagnostic's user-facing severity.
    pub const fn severity(&self) -> DiagnosticSeverity {
        self.code.severity()
    }

    /// Returns whether this diagnostic prevents successful compilation.
    pub const fn is_error(&self) -> bool {
        matches!(self.severity(), DiagnosticSeverity::Error)
    }

    /// Returns the concise label shown beside the primary source underline.
    pub fn primary_label(&self) -> &str {
        self.primary_label
            .as_deref()
            .unwrap_or_else(|| self.code.title())
    }

    /// Returns the actionable next step, when one is available.
    pub fn help(&self) -> Option<&str> {
        self.help.as_deref().or_else(|| self.code.help())
    }

    /// Returns supplementary explanations in display order.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Returns related source locations in display order.
    pub fn related(&self) -> &[DiagnosticLabel] {
        &self.related
    }

    pub(crate) fn with_primary_label(mut self, label: impl Into<String>) -> Self {
        self.primary_label = Some(label.into());
        self
    }

    pub(crate) fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub(crate) fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub(crate) fn with_related(
        mut self,
        location: SourceLocation,
        message: impl Into<String>,
    ) -> Self {
        self.related.push(DiagnosticLabel {
            location,
            message: message.into(),
        });
        self
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

    /// Returns whether parsing produced any error-level diagnostics.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(CompilerDiagnostic::is_error)
    }

    /// Returns the number of error-level diagnostics.
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.is_error())
            .count()
    }

    /// Returns the number of warning-level diagnostics.
    pub fn warning_count(&self) -> usize {
        self.diagnostics.len() - self.error_count()
    }
}

#[cfg(test)]
mod tests;
