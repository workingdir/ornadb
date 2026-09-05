//! Explicit bridge from parser-local locations to pinned `sys` diagnostics.

use orna_foundation_v1::{CanonicalSnapshot, DiagnosticSpan, FileRef, SafeText, SourceSpan};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceDocumentId {
    Pinned(FileRef),
    Ephemeral(String),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseContext {
    pub document: SourceDocumentId,
    pub snapshot: CanonicalSnapshot,
    pub source: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyntaxAdmissionError {
    NotPinned,
    InvalidSpan,
    UnsafeText,
}

/// Converts a parser-local span only when its context supplies a real FileRef.
pub fn admit_span(
    context: &ParseContext,
    span: &super::parser::SourceSpan,
) -> Result<SourceSpan, SyntaxAdmissionError> {
    let SourceDocumentId::Pinned(file) = &context.document else {
        return Err(SyntaxAdmissionError::NotPinned);
    };
    SourceSpan::from_utf8_offsets(file.clone(), &context.source, span.start, span.end)
        .map_err(|_| SyntaxAdmissionError::InvalidSpan)
}
/// Produces the protocol source observation; ephemeral buffers never cross it.
pub fn admit_diagnostic(
    context: &ParseContext,
    span: &super::parser::SourceSpan,
    message: &str,
) -> Result<(DiagnosticSpan, SafeText), SyntaxAdmissionError> {
    let SourceDocumentId::Pinned(_) = &context.document else {
        return Err(SyntaxAdmissionError::NotPinned);
    };
    let _ = admit_span(context, span)?;
    let file_path = span
        .file
        .as_deref()
        .ok_or(SyntaxAdmissionError::NotPinned)?;
    let wire = DiagnosticSpan::new(
        context.snapshot.clone(),
        file_path,
        span.start.into(),
        span.end.into(),
    )
    .map_err(|_| SyntaxAdmissionError::InvalidSpan)?;
    Ok((
        wire,
        SafeText::new(message).map_err(|_| SyntaxAdmissionError::UnsafeText)?,
    ))
}
