//! Component-local state for the Studio Orna SQL/source editor.
//!
//! This module is a renderer-neutral buffer and language-service view. A
//! Studio host forwards [`SourceChange`] values to `orna-lsp` and installs
//! the corresponding JSON-RPC results with [`StudioSourceEditor::install_lsp`].
//! Keeping this component independent of the host runtime avoids adding a
//! second parser or changing the shared runtime event ABI.

use serde_json::Value;

/// The LSP language identifier used for Orna source documents.
pub const ORNA_LANGUAGE_ID: &str = "orna";

/// A full-document edit for the host to forward to the language server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceChange {
    /// The editor document URI.
    pub uri: String,
    /// Monotonically increasing LSP document version.
    pub version: i32,
    /// Complete current document text for full synchronization.
    pub text: String,
}

/// LSP results associated with one exact document version.
///
/// Values retain their Language Server Protocol JSON representation so the
/// component does not reinterpret diagnostics, symbols, hover, or locations.
#[derive(Clone, Debug, PartialEq)]
pub struct LspSnapshot {
    /// The document URI used to produce these results.
    pub uri: String,
    /// The document version used to produce these results.
    pub version: i32,
    /// `textDocument/publishDiagnostics` diagnostics for the document.
    pub diagnostics: Vec<Value>,
    /// `textDocument/documentSymbol` results for the document.
    pub symbols: Vec<Value>,
    /// The latest `textDocument/hover` result, if one is selected.
    pub hover: Option<Value>,
    /// `textDocument/definition` locations for the selected position.
    pub definitions: Vec<Value>,
    /// `textDocument/references` locations for the selected position.
    pub references: Vec<Value>,
}

/// The isolated Studio editor's document and current language-service state.
#[derive(Clone, Debug, PartialEq)]
pub struct StudioSourceEditor {
    uri: String,
    text: String,
    version: i32,
    lsp: Option<LspSnapshot>,
}

impl StudioSourceEditor {
    /// Opens one `.orna` source document at LSP version 1.
    pub fn open(uri: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            text: text.into(),
            version: 1,
            lsp: None,
        }
    }

    /// Returns the document URI.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Returns the current source text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the current LSP document version.
    pub fn version(&self) -> i32 {
        self.version
    }

    /// Returns language-service results only while they match the current
    /// text buffer.
    pub fn lsp(&self) -> Option<&LspSnapshot> {
        self.lsp
            .as_ref()
            .filter(|snapshot| snapshot.version == self.version)
    }

    /// Replaces the source buffer and returns a full-text LSP change.
    ///
    /// Any previous language-service snapshot is cleared so stale diagnostics
    /// or navigation results cannot be shown for the new text.
    pub fn replace_text(
        &mut self,
        text: impl Into<String>,
    ) -> Result<SourceChange, SourceVersionExhausted> {
        let version = self.version.checked_add(1).ok_or(SourceVersionExhausted)?;
        self.version = version;
        self.text = text.into();
        self.lsp = None;
        Ok(SourceChange {
            uri: self.uri.clone(),
            version,
            text: self.text.clone(),
        })
    }

    /// Installs results from `orna-lsp` only when they match the current text.
    /// Returns false for a different document or stale/future version.
    pub fn install_lsp(&mut self, snapshot: LspSnapshot) -> bool {
        if snapshot.uri != self.uri || snapshot.version != self.version {
            return false;
        }
        self.lsp = Some(snapshot);
        true
    }
}

/// The LSP version counter cannot be advanced further.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceVersionExhausted;

impl std::fmt::Display for SourceVersionExhausted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Studio source document version is exhausted")
    }
}

impl std::error::Error for SourceVersionExhausted {}

#[cfg(test)]
mod tests {
    use super::{LspSnapshot, ORNA_LANGUAGE_ID, StudioSourceEditor};
    use serde_json::json;

    const SOURCE: &str = include_str!("tests/fixtures/synthetic-standard.orna");

    fn snapshot(uri: &str, version: i32) -> LspSnapshot {
        LspSnapshot {
            uri: uri.to_owned(),
            version,
            diagnostics: vec![json!({"message": "fixture diagnostic"})],
            symbols: vec![json!({"name": "synthetic_standard"})],
            hover: Some(json!({"contents": "fixture hover"})),
            definitions: vec![json!({"uri": "file:///definition.orna"})],
            references: vec![json!({"uri": "file:///reference.orna"})],
        }
    }

    #[test]
    fn editor_uses_the_held_qualified_source_fixture() {
        let editor = StudioSourceEditor::open("file:///fixture.orna", SOURCE);

        assert_eq!(ORNA_LANGUAGE_ID, "orna");
        assert_eq!(editor.text(), SOURCE);
        assert_eq!(editor.version(), 1);
        assert!(editor.lsp().is_none());
    }

    #[test]
    fn edits_emit_versioned_full_text_and_drop_stale_language_results() {
        let mut editor = StudioSourceEditor::open("file:///fixture.orna", SOURCE);
        assert!(editor.install_lsp(snapshot("file:///fixture.orna", 1)));
        assert_eq!(editor.lsp().unwrap().symbols.len(), 1);

        let changed_source = format!("{SOURCE}\n");
        let change = editor
            .replace_text(changed_source.clone())
            .expect("document version advances");

        assert_eq!(change.uri, "file:///fixture.orna");
        assert_eq!(change.version, 2);
        assert_eq!(change.text, changed_source);
        assert_eq!(editor.version(), 2);
        assert!(editor.lsp().is_none());
        assert!(!editor.install_lsp(snapshot("file:///fixture.orna", 1)));
        assert!(!editor.install_lsp(snapshot("file:///other.orna", 2)));
        assert!(editor.install_lsp(snapshot("file:///fixture.orna", 2)));
        assert_eq!(editor.lsp().unwrap().references.len(), 1);
    }
}
