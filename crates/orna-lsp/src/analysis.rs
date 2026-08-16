//! Compiler-backed analysis for one open Orna document.
#![allow(deprecated)] // lsp-types 0.97 keeps the mandatory `deprecated` field.
//!
//! The analysis stages reuse the offline Orna compiler, so they need no
//! running database and never write to disk. The standard library is
//! verified once and cached for the lifetime of the server.

use lsp_types::{
    CompletionItem, CompletionItemKind, Diagnostic, DiagnosticSeverity, DocumentSymbol, Hover,
    HoverContents, Location, MarkupContent, MarkupKind, NumberOrString, Position, SymbolKind,
};
use orna_compiler::{CompilerDiagnostic, check_new_application, check_standard_library_source};
use orna_core::source::{SourceBundle, SourceUnit};
use orna_standard::{retained_standard_library_snapshot, verify_standard_library_snapshot};
use orna_syntax::{
    ClientFunctionDeclaration, HighlightKind, Parse, QualifiedName, ServerFunctionDeclaration,
    SourceSpan,
};

use crate::documents::{Document, PositionMapper};

/// The verified, checked standard library shared by all documents.
pub struct StandardLibrary {
    checked: orna_compiler::CheckedStandardLibrary,
}

impl StandardLibrary {
    /// Loads and verifies the retained standard library snapshot.
    ///
    /// This runs once per server process. The checked library is immutable
    /// and safe to reuse for every document.
    pub fn load() -> Result<Self, String> {
        let snapshot = retained_standard_library_snapshot().map_err(|error| error.to_string())?;
        let verified =
            verify_standard_library_snapshot(snapshot).map_err(|error| error.to_string())?;
        let checked =
            check_standard_library_source(&verified).map_err(|error| error.to_string())?;
        Ok(Self { checked })
    }
}

/// Returns the syntax diagnostics of one document.
///
/// This path needs no standard library and is used when the verified
/// standard snapshot cannot be loaded.
pub fn syntax_diagnostics(document: &Document, mapper: &PositionMapper<'_>) -> Vec<Diagnostic> {
    let parse = orna_syntax::parse(&document.text);
    parse
        .diagnostics()
        .iter()
        .map(|diagnostic| Diagnostic {
            range: mapper.range(&diagnostic.span),
            severity: Some(DiagnosticSeverity::ERROR),
            code: Some(NumberOrString::String(diagnostic.code.to_owned())),
            code_description: None,
            source: Some("orna".to_owned()),
            message: diagnostic.message.clone(),
            related_information: None,
            tags: None,
            data: None,
        })
        .collect()
}

/// Returns the full compiler diagnostics of one document.
pub fn check_document(
    document: &Document,
    standard: Option<&StandardLibrary>,
    mapper: &PositionMapper<'_>,
) -> Vec<Diagnostic> {
    let Some(standard) = standard else {
        return syntax_diagnostics(document, mapper);
    };
    let bundle = match SourceBundle::new([SourceUnit::new(
        document.logical_path(),
        document.text.clone(),
    )]) {
        Ok(bundle) => bundle,
        Err(_) => return syntax_diagnostics(document, mapper),
    };
    let report = match check_new_application(&bundle, &standard.checked) {
        Ok(report) => report,
        Err(_) => return syntax_diagnostics(document, mapper),
    };
    let logical_path = document.logical_path();
    report
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.location().logical_path() == logical_path)
        .map(|diagnostic| compiler_diagnostic(diagnostic, mapper))
        .collect()
}

fn compiler_diagnostic(diagnostic: &CompilerDiagnostic, mapper: &PositionMapper<'_>) -> Diagnostic {
    let span = SourceSpan {
        start: diagnostic.location().span().start(),
        end: diagnostic.location().span().end(),
    };
    Diagnostic {
        range: mapper.range(&span),
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String(
            diagnostic.code().as_str().to_owned(),
        )),
        code_description: None,
        source: Some("orna".to_owned()),
        message: diagnostic.message().to_owned(),
        related_information: None,
        tags: None,
        data: None,
    }
}

/// Returns the outline symbols of one parsed document.
pub fn document_symbols(parse: &Parse, mapper: &PositionMapper<'_>) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();
    for schema in parse.schemas() {
        symbols.push(DocumentSymbol {
            name: last_name(&schema.name),
            detail: Some("schema".to_owned()),
            kind: SymbolKind::NAMESPACE,
            tags: None,
            deprecated: None,
            range: mapper.range(&schema.span),
            selection_range: mapper.range(&schema.name.span),
            children: None,
        });
    }
    for declaration in parse.object_types() {
        let children = declaration
            .fields
            .iter()
            .map(|field| DocumentSymbol {
                name: field.name.text.clone(),
                detail: Some("field".to_owned()),
                kind: SymbolKind::FIELD,
                tags: None,
                deprecated: None,
                range: mapper.range(&field.span),
                selection_range: mapper.range(&field.name.span),
                children: None,
            })
            .collect();
        symbols.push(DocumentSymbol {
            name: last_name(&declaration.name),
            detail: Some("object type".to_owned()),
            kind: SymbolKind::INTERFACE,
            tags: None,
            deprecated: None,
            range: mapper.range(&declaration.span),
            selection_range: mapper.range(&declaration.name.span),
            children: Some(children),
        });
    }
    for declaration in parse.enum_types() {
        symbols.push(DocumentSymbol {
            name: last_name(&declaration.name),
            detail: Some("enum type".to_owned()),
            kind: SymbolKind::ENUM,
            tags: None,
            deprecated: None,
            range: mapper.range(&declaration.span),
            selection_range: mapper.range(&declaration.name.span),
            children: None,
        });
    }
    for declaration in parse.record_value_types() {
        let children = declaration
            .fields
            .iter()
            .map(|field| DocumentSymbol {
                name: field.name.text.clone(),
                detail: Some("field".to_owned()),
                kind: SymbolKind::FIELD,
                tags: None,
                deprecated: None,
                range: mapper.range(&field.span),
                selection_range: mapper.range(&field.name.span),
                children: None,
            })
            .collect();
        symbols.push(DocumentSymbol {
            name: last_name(&declaration.name),
            detail: Some("record value type".to_owned()),
            kind: SymbolKind::STRUCT,
            tags: None,
            deprecated: None,
            range: mapper.range(&declaration.span),
            selection_range: mapper.range(&declaration.name.span),
            children: Some(children),
        });
    }
    for declaration in parse.primitive_value_types() {
        symbols.push(DocumentSymbol {
            name: last_name(&declaration.name),
            detail: Some("primitive value type".to_owned()),
            kind: SymbolKind::STRUCT,
            tags: None,
            deprecated: None,
            range: mapper.range(&declaration.span),
            selection_range: mapper.range(&declaration.name.span),
            children: None,
        });
    }
    for declaration in parse.opaque_value_types() {
        symbols.push(DocumentSymbol {
            name: last_name(&declaration.name),
            detail: Some("opaque value type".to_owned()),
            kind: SymbolKind::STRUCT,
            tags: None,
            deprecated: None,
            range: mapper.range(&declaration.span),
            selection_range: mapper.range(&declaration.name.span),
            children: None,
        });
    }
    for declaration in parse.server_functions() {
        symbols.push(function_symbol(declaration, "server function", mapper));
    }
    for declaration in parse.client_functions() {
        symbols.push(function_symbol(declaration, "client function", mapper));
    }
    symbols
}

fn function_symbol<F>(
    declaration: &F,
    detail: &'static str,
    mapper: &PositionMapper<'_>,
) -> DocumentSymbol
where
    F: FunctionLike,
{
    let children = declaration
        .parameter_names()
        .into_iter()
        .map(|(name, span)| DocumentSymbol {
            name,
            detail: Some("parameter".to_owned()),
            kind: SymbolKind::VARIABLE,
            tags: None,
            deprecated: None,
            range: mapper.range(&span),
            selection_range: mapper.range(&span),
            children: None,
        })
        .collect();
    DocumentSymbol {
        name: last_name(declaration.name()),
        detail: Some(detail.to_owned()),
        kind: SymbolKind::FUNCTION,
        tags: None,
        deprecated: None,
        range: mapper.range(declaration.span()),
        selection_range: mapper.range(&declaration.name().span),
        children: Some(children),
    }
}

/// A common view over SERVER and CLIENT function declarations.
pub trait FunctionLike {
    fn name(&self) -> &QualifiedName;
    fn span(&self) -> &SourceSpan;
    fn parameter_names(&self) -> Vec<(String, SourceSpan)>;
}

impl FunctionLike for ServerFunctionDeclaration {
    fn name(&self) -> &QualifiedName {
        &self.name
    }

    fn span(&self) -> &SourceSpan {
        &self.span
    }

    fn parameter_names(&self) -> Vec<(String, SourceSpan)> {
        self.parameters
            .iter()
            .map(|parameter| (parameter.name.text.clone(), parameter.name.span.clone()))
            .collect()
    }
}

impl FunctionLike for ClientFunctionDeclaration {
    fn name(&self) -> &QualifiedName {
        &self.name
    }

    fn span(&self) -> &SourceSpan {
        &self.span
    }

    fn parameter_names(&self) -> Vec<(String, SourceSpan)> {
        self.parameters
            .iter()
            .map(|parameter| (parameter.name.text.clone(), parameter.name.span.clone()))
            .collect()
    }
}

/// Returns the source text of one qualified name's final component.
fn last_name(name: &QualifiedName) -> String {
    name.parts
        .last()
        .map(|part| part.text.clone())
        .unwrap_or_default()
}

/// Returns the identifier-like token at one byte offset.
fn identifier_at(text: &str, parse: &Parse, byte: usize) -> Option<(String, SourceSpan)> {
    parse
        .highlight()
        .into_iter()
        .find(|token| token.range.contains(&byte))
        .filter(|token| {
            matches!(
                token.kind,
                HighlightKind::VariableName
                    | HighlightKind::FunctionName
                    | HighlightKind::TypeName
                    | HighlightKind::NamespaceName
                    | HighlightKind::PropertyName
                    | HighlightKind::QuotedIdentifier
            )
        })
        .map(|token| {
            (
                text[token.range.clone()].to_owned(),
                SourceSpan {
                    start: token.range.start,
                    end: token.range.end,
                },
            )
        })
}

/// Returns a case-insensitive declaration lookup for one simple name.
fn declaration_span(parse: &Parse, name: &str) -> Option<(&'static str, SourceSpan)> {
    let matches = |candidate: &QualifiedName| {
        candidate
            .parts
            .last()
            .is_some_and(|part| part.text.eq_ignore_ascii_case(name))
    };
    if let Some(declaration) = parse
        .schemas()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(("schema", declaration.name.span.clone()));
    }
    if let Some(declaration) = parse
        .object_types()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(("object type", declaration.name.span.clone()));
    }
    if let Some(declaration) = parse
        .enum_types()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(("enum type", declaration.name.span.clone()));
    }
    if let Some(declaration) = parse
        .record_value_types()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(("record value type", declaration.name.span.clone()));
    }
    if let Some(declaration) = parse
        .primitive_value_types()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(("primitive value type", declaration.name.span.clone()));
    }
    if let Some(declaration) = parse
        .opaque_value_types()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(("opaque value type", declaration.name.span.clone()));
    }
    if let Some(declaration) = parse
        .server_functions()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(("server function", declaration.name.span.clone()));
    }
    if let Some(declaration) = parse
        .client_functions()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(("client function", declaration.name.span.clone()));
    }
    None
}

/// Returns the hover content for the identifier at one position.
pub fn hover(
    document: &Document,
    parse: &Parse,
    position: Position,
    mapper: &PositionMapper<'_>,
) -> Option<Hover> {
    let byte = mapper.byte_offset(position);
    let (name, span) = identifier_at(&document.text, parse, byte)?;
    let upper = name.to_ascii_uppercase();
    let value = if let Some((kind, _)) = declaration_span(parse, &name) {
        format!("**{kind}**\n\n```orna\n{name}\n```")
    } else if orna_syntax::SCALAR_TYPES
        .binary_search_by(|candidate| (*candidate).cmp(upper.as_str()))
        .is_ok()
    {
        format!("**standard scalar type**\n\n```orna\n{upper}\n```")
    } else {
        return None;
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(mapper.range(&span)),
    })
}

/// Returns the declaration location for the identifier at one position.
pub fn definition(
    document: &Document,
    parse: &Parse,
    position: Position,
    mapper: &PositionMapper<'_>,
) -> Option<Location> {
    let byte = mapper.byte_offset(position);
    let (name, _) = identifier_at(&document.text, parse, byte)?;
    declaration_span(parse, &name).map(|(_, span)| Location {
        uri: document.uri.clone(),
        range: mapper.range(&span),
    })
}

/// Returns every occurrence of the identifier at one position.
pub fn references(
    document: &Document,
    parse: &Parse,
    position: Position,
    mapper: &PositionMapper<'_>,
) -> Vec<Location> {
    let byte = mapper.byte_offset(position);
    let Some((name, _)) = identifier_at(&document.text, parse, byte) else {
        return Vec::new();
    };
    parse
        .highlight()
        .into_iter()
        .filter(|token| {
            matches!(
                token.kind,
                HighlightKind::VariableName
                    | HighlightKind::FunctionName
                    | HighlightKind::TypeName
                    | HighlightKind::NamespaceName
                    | HighlightKind::PropertyName
                    | HighlightKind::QuotedIdentifier
            )
        })
        .filter(|token| document.text[token.range.clone()].eq_ignore_ascii_case(&name))
        .map(|token| Location {
            uri: document.uri.clone(),
            range: mapper.range(&SourceSpan {
                start: token.range.start,
                end: token.range.end,
            }),
        })
        .collect()
}

/// Returns the completion items for one document.
pub fn completion(parse: &Parse, standard: Option<&StandardLibrary>) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    if let Some(standard) = standard {
        for value_type in standard.checked.value_types() {
            let name = value_type.name();
            if let Some(last) = name.parts().last() {
                items.push(CompletionItem {
                    label: last.clone(),
                    kind: Some(CompletionItemKind::STRUCT),
                    detail: Some(format!("standard type {name}")),
                    ..CompletionItem::default()
                });
            }
        }
    }
    for keyword in orna_syntax::KEYWORDS {
        items.push(CompletionItem {
            label: (*keyword).to_owned(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("keyword".to_owned()),
            ..CompletionItem::default()
        });
    }
    for scalar in orna_syntax::SCALAR_TYPES {
        items.push(CompletionItem {
            label: (*scalar).to_owned(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("standard scalar type".to_owned()),
            ..CompletionItem::default()
        });
    }
    let mut add_named = |label: String, kind: CompletionItemKind, detail: String| {
        items.push(CompletionItem {
            label,
            kind: Some(kind),
            detail: Some(detail),
            ..CompletionItem::default()
        });
    };
    for schema in parse.schemas() {
        add_named(
            last_name(&schema.name),
            CompletionItemKind::MODULE,
            "schema".to_owned(),
        );
    }
    for declaration in parse.object_types() {
        add_named(
            last_name(&declaration.name),
            CompletionItemKind::INTERFACE,
            "object type".to_owned(),
        );
    }
    for declaration in parse.enum_types() {
        add_named(
            last_name(&declaration.name),
            CompletionItemKind::ENUM,
            "enum type".to_owned(),
        );
    }
    for declaration in parse.record_value_types() {
        add_named(
            last_name(&declaration.name),
            CompletionItemKind::STRUCT,
            "record value type".to_owned(),
        );
    }
    for declaration in parse.primitive_value_types() {
        add_named(
            last_name(&declaration.name),
            CompletionItemKind::STRUCT,
            "primitive value type".to_owned(),
        );
    }
    for declaration in parse.opaque_value_types() {
        add_named(
            last_name(&declaration.name),
            CompletionItemKind::STRUCT,
            "opaque value type".to_owned(),
        );
    }
    for declaration in parse.server_functions() {
        add_named(
            last_name(&declaration.name),
            CompletionItemKind::FUNCTION,
            "server function".to_owned(),
        );
    }
    for declaration in parse.client_functions() {
        add_named(
            last_name(&declaration.name),
            CompletionItemKind::FUNCTION,
            "client function".to_owned(),
        );
    }
    items
}
