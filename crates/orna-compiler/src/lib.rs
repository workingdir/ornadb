//! Orna-owned source parsing at the compiler boundary.
//!
//! This crate accepts an ordered source bundle and returns lossless parsed
//! units with compiler diagnostics. Semantic analysis and revision construction
//! are separate stages.

use orna_core::source::SourceBundle;
use orna_syntax::{Diagnostic as SyntaxDiagnostic, Parse as SyntaxParse, SourceSpan};

mod resolver;

pub use resolver::{
    CheckReport, CheckedBundle, CheckedDefault, CheckedField, CheckedObjectType, CheckedSchema,
    ConstantValue, check,
};

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
            location: SourceLocation {
                logical_path: logical_path.to_owned(),
                span: ByteSpan::from_syntax_span(&diagnostic.span),
            },
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
    use orna_core::source::{SourceBundle, SourceUnit};

    use super::{DiagnosticCode, parse_bundle};

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
