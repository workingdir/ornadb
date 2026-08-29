//! Compiler-backed analysis for one open Orna document.
#![allow(deprecated)] // lsp-types 0.97 keeps the mandatory `deprecated` field.
//!
//! The analysis stages reuse the offline Orna compiler, so they need no
//! running database and never write to disk. The standard library is
//! verified once and cached for the lifetime of the server.

use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionTriggerKind, Diagnostic,
    DiagnosticRelatedInformation, DiagnosticSeverity, DocumentSymbol, Hover, Location,
    NumberOrString, Position, SymbolKind,
};
use orna_compiler::{CompilerDiagnostic, check_new_application, check_standard_library_source};
use orna_core::catalogue::ValueTypePersistence;
use orna_core::source::{SourceBundle, SourceUnit};
use orna_standard::{retained_standard_library_snapshot, verify_standard_library_snapshot};
use orna_syntax::FunctionReturnType;
use orna_syntax::{
    ClientExpression, ClientFunctionDeclaration, EnumTypeDeclaration, HighlightKind,
    ObjectTypeDeclaration, OpaqueValueTypeDeclaration, Parse, PrimitiveValueTypeDeclaration,
    QualifiedName, RecordValueTypeDeclaration, SchemaDeclaration, ServerFunctionDeclaration,
    SourceSlice, SourceSpan, StandardLargeObjectKind, TypeSpecification,
};

use crate::documents::{Document, PositionMapper};

/// The verified, checked standard library shared by all documents.
pub struct StandardLibrary {
    checked: orna_compiler::CheckedStandardLibrary,
}

impl StandardLibrary {
    /// Loads and verifies the retained V10 standard library snapshot.
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
            related_information: Some(vec![DiagnosticRelatedInformation {
                location: Location {
                    uri: document.uri.clone(),
                    range: mapper.range(&diagnostic.span),
                },
                message: syntax_help(diagnostic.message.as_str()),
            }]),
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
    let logical_path = document.logical_path();
    let bundle =
        match SourceBundle::new([SourceUnit::new(logical_path.clone(), document.text.clone())]) {
            Ok(bundle) => bundle,
            Err(_) => return syntax_diagnostics(document, mapper),
        };
    let report = match check_new_application(&bundle, &standard.checked) {
        Ok(report) => report,
        Err(_) => return syntax_diagnostics(document, mapper),
    };
    report
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.location().logical_path() == logical_path)
        .map(|diagnostic| compiler_diagnostic(diagnostic, mapper, &document.uri))
        .collect()
}
fn compiler_diagnostic(
    diagnostic: &CompilerDiagnostic,
    mapper: &PositionMapper<'_>,
    uri: &lsp_types::Uri,
) -> Diagnostic {
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
        related_information: Some(vec![DiagnosticRelatedInformation {
            location: Location {
                uri: uri.clone(),
                range: mapper.range(&span),
            },
            message: diagnostic_help(diagnostic),
        }]),
        tags: None,
        data: None,
    }
}

fn diagnostic_help(diagnostic: &CompilerDiagnostic) -> String {
    format!(
        "{} {}",
        diagnostic.code().as_str(),
        diagnostic.code().summary()
    )
}

fn syntax_help(message: &str) -> String {
    if message.contains("expected a name") {
        "Add the missing name at this location.".to_owned()
    } else if message.contains("expected") {
        "Add the expected token or close the current construct.".to_owned()
    } else {
        "Review the syntax at this location.".to_owned()
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

fn qualified_name_text(name: &QualifiedName) -> String {
    name.parts
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

/// One declaration found by a name lookup.
#[derive(Clone, Copy)]
pub enum DeclarationRef<'a> {
    /// A parsed `CREATE SCHEMA` declaration.
    Schema(&'a SchemaDeclaration),
    /// A parsed object type declaration.
    ObjectType(&'a ObjectTypeDeclaration),
    /// A parsed enum type declaration.
    EnumType(&'a EnumTypeDeclaration),
    /// A parsed record value type declaration.
    RecordValueType(&'a RecordValueTypeDeclaration),
    /// A parsed primitive value type declaration.
    PrimitiveValueType(&'a PrimitiveValueTypeDeclaration),
    /// A parsed opaque value type declaration.
    OpaqueValueType(&'a OpaqueValueTypeDeclaration),
    /// A parsed SERVER function declaration.
    ServerFunction(&'a ServerFunctionDeclaration),
    /// A parsed CLIENT function declaration.
    ClientFunction(&'a ClientFunctionDeclaration),
}

impl DeclarationRef<'_> {
    /// Returns the declared qualified name.
    pub fn name(&self) -> &QualifiedName {
        match self {
            Self::Schema(declaration) => &declaration.name,
            Self::ObjectType(declaration) => &declaration.name,
            Self::EnumType(declaration) => &declaration.name,
            Self::RecordValueType(declaration) => &declaration.name,
            Self::PrimitiveValueType(declaration) => &declaration.name,
            Self::OpaqueValueType(declaration) => &declaration.name,
            Self::ServerFunction(declaration) => &declaration.name,
            Self::ClientFunction(declaration) => &declaration.name,
        }
    }

    /// Returns the span of the declared name.
    pub fn name_span(&self) -> &SourceSpan {
        &self.name().span
    }
}

/// Returns the token at one byte offset, including keywords.
fn token_at(
    text: &str,
    highlighted: &[orna_syntax::HighlightToken],
    byte: usize,
) -> Option<(String, HighlightKind, SourceSpan)> {
    highlighted
        .iter()
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
                    | HighlightKind::Keyword
            )
        })
        .map(|token| {
            (
                text[token.range.clone()].to_owned(),
                token.kind,
                SourceSpan {
                    start: token.range.start,
                    end: token.range.end,
                },
            )
        })
}
#[derive(Clone, PartialEq, Eq)]
enum IdentifierKey {
    Quoted(String),
    Unquoted(String),
}

/// Canonicalizes one source identifier using Orna's quoted-name rules.
///
/// Unquoted identifiers are case-insensitive. Quoted identifiers preserve
/// exact spelling and do not match an unquoted identifier.
fn identifier_key(spelling: &str) -> IdentifierKey {
    if spelling.starts_with('"') && spelling.ends_with('"') {
        IdentifierKey::Quoted(spelling.to_owned())
    } else {
        IdentifierKey::Unquoted(spelling.chars().flat_map(char::to_lowercase).collect())
    }
}

fn identifier_spelling_matches(candidate: &str, query: &str) -> bool {
    identifier_key(candidate) == identifier_key(query)
}

fn source_name_parts(name: &str) -> Vec<&str> {
    let bytes = name.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                if quoted && bytes.get(index + 1) == Some(&b'"') {
                    index += 1;
                } else {
                    quoted = !quoted;
                }
            }
            b'.' if !quoted => {
                parts.push(&name[start..index]);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    parts.push(&name[start..]);
    parts
}

fn qualified_name_matches_keys(name: &QualifiedName, keys: &[IdentifierKey]) -> bool {
    name.parts.len() == keys.len()
        && name
            .parts
            .iter()
            .zip(keys)
            .all(|(part, key)| &identifier_key(&part.text) == key)
}

fn dotted_name_separator(text: &str, start: usize, end: usize) -> bool {
    let bytes = text.as_bytes();
    let mut index = start;
    let mut dot = false;
    while index < end {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
        } else if bytes.get(index..index + 2) == Some(b"/*") {
            index += 2;
            while index + 1 < end && bytes.get(index..index + 2) != Some(b"*/") {
                index += 1;
            }
            if index + 1 >= end {
                return false;
            }
            index += 2;
        } else if bytes.get(index..index + 2) == Some(b"//") {
            index += 2;
            while index < end && bytes[index] != b'\n' {
                index += 1;
            }
        } else if bytes[index] == b'.' && !dot {
            dot = true;
            index += 1;
        } else {
            return false;
        }
    }
    dot
}

fn qualified_name_keys_at(
    text: &str,
    highlighted: &[orna_syntax::HighlightToken],
    selected_span: &SourceSpan,
) -> Option<Vec<IdentifierKey>> {
    let selected_index = highlighted.iter().position(|token| {
        token.range.start == selected_span.start && token.range.end == selected_span.end
    })?;
    let mut first_index = selected_index;
    let mut right_start = selected_span.start;
    while first_index > 0 {
        let previous_index = highlighted[..first_index]
            .iter()
            .rposition(|token| token.kind == HighlightKind::NamespaceName)?;
        let previous = &highlighted[previous_index];
        if !dotted_name_separator(text, previous.range.end, right_start) {
            break;
        }
        first_index = previous_index;
        right_start = previous.range.start;
    }
    Some(
        highlighted[first_index..=selected_index]
            .iter()
            .filter(|token| {
                token.kind == HighlightKind::NamespaceName
                    || (token.range.start == selected_span.start
                        && token.range.end == selected_span.end)
            })
            .map(|token| identifier_key(&text[token.range.clone()]))
            .collect(),
    )
}

/// Returns a case-aware declaration lookup for one simple name.
pub fn declaration_at<'a>(parse: &'a Parse, name: &str) -> Option<DeclarationRef<'a>> {
    let query_parts = source_name_parts(name);
    let query_keys: Vec<_> = query_parts
        .iter()
        .map(|part| identifier_key(part))
        .collect();
    let matches = |candidate: &QualifiedName| {
        if query_keys.len() == 1 {
            candidate
                .parts
                .last()
                .is_some_and(|part| identifier_spelling_matches(&part.text, name))
        } else {
            qualified_name_matches_keys(candidate, &query_keys)
        }
    };

    if let Some(declaration) = parse
        .schemas()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(DeclarationRef::Schema(declaration));
    }
    if let Some(declaration) = parse
        .object_types()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(DeclarationRef::ObjectType(declaration));
    }
    if let Some(declaration) = parse
        .enum_types()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(DeclarationRef::EnumType(declaration));
    }
    if let Some(declaration) = parse
        .record_value_types()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(DeclarationRef::RecordValueType(declaration));
    }
    if let Some(declaration) = parse
        .primitive_value_types()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(DeclarationRef::PrimitiveValueType(declaration));
    }
    if let Some(declaration) = parse
        .opaque_value_types()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(DeclarationRef::OpaqueValueType(declaration));
    }
    if let Some(declaration) = parse
        .server_functions()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(DeclarationRef::ServerFunction(declaration));
    }
    if let Some(declaration) = parse
        .client_functions()
        .iter()
        .find(|declaration| matches(&declaration.name))
    {
        return Some(DeclarationRef::ClientFunction(declaration));
    }
    None
}

fn declaration_for_keys<'a>(
    parse: &'a Parse,
    keys: &[IdentifierKey],
    kind: HighlightKind,
) -> Option<DeclarationRef<'a>> {
    let matches = |name: &QualifiedName| qualified_name_matches_keys(name, keys);
    match kind {
        HighlightKind::NamespaceName => parse
            .schemas()
            .iter()
            .find(|declaration| matches(&declaration.name))
            .map(DeclarationRef::Schema),
        HighlightKind::TypeName => parse
            .object_types()
            .iter()
            .find(|declaration| matches(&declaration.name))
            .map(DeclarationRef::ObjectType)
            .or_else(|| {
                parse
                    .enum_types()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::EnumType)
            })
            .or_else(|| {
                parse
                    .record_value_types()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::RecordValueType)
            })
            .or_else(|| {
                parse
                    .primitive_value_types()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::PrimitiveValueType)
            })
            .or_else(|| {
                parse
                    .opaque_value_types()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::OpaqueValueType)
            }),
        HighlightKind::FunctionName => parse
            .server_functions()
            .iter()
            .find(|declaration| matches(&declaration.name))
            .map(DeclarationRef::ServerFunction)
            .or_else(|| {
                parse
                    .client_functions()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::ClientFunction)
            }),
        _ => parse
            .schemas()
            .iter()
            .find(|declaration| matches(&declaration.name))
            .map(DeclarationRef::Schema)
            .or_else(|| {
                parse
                    .object_types()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::ObjectType)
            })
            .or_else(|| {
                parse
                    .enum_types()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::EnumType)
            })
            .or_else(|| {
                parse
                    .record_value_types()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::RecordValueType)
            })
            .or_else(|| {
                parse
                    .primitive_value_types()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::PrimitiveValueType)
            })
            .or_else(|| {
                parse
                    .opaque_value_types()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::OpaqueValueType)
            })
            .or_else(|| {
                parse
                    .server_functions()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::ServerFunction)
            })
            .or_else(|| {
                parse
                    .client_functions()
                    .iter()
                    .find(|declaration| matches(&declaration.name))
                    .map(DeclarationRef::ClientFunction)
            }),
    }
}

fn declaration_at_span<'a>(
    parse: &'a Parse,
    text: &str,
    highlighted: &[orna_syntax::HighlightToken],
    name: &str,
    kind: HighlightKind,
    selected_span: &SourceSpan,
) -> Option<DeclarationRef<'a>> {
    if let Some(keys) = qualified_name_keys_at(text, highlighted, selected_span) {
        if let Some(declaration) = declaration_for_keys(parse, &keys, kind) {
            return Some(declaration);
        }
        if keys.len() > 1 {
            return None;
        }
    }
    declaration_at(parse, name)
}

fn source_span_matches(left: &SourceSpan, right: &SourceSpan) -> bool {
    left.start == right.start && left.end == right.end
}

fn name_part_matches_span(part: &orna_syntax::NamePart, span: &SourceSpan) -> bool {
    source_span_matches(&part.span, span)
}

fn qualified_name_matches_span(name: &QualifiedName, span: &SourceSpan) -> bool {
    name.parts
        .last()
        .is_some_and(|part| name_part_matches_span(part, span))
}

fn return_type_contains_declaration(return_type: &FunctionReturnType, span: &SourceSpan) -> bool {
    match return_type {
        FunctionReturnType::Rows { columns, .. } => columns
            .iter()
            .any(|column| name_part_matches_span(&column.name, span)),
        FunctionReturnType::Single(_) | FunctionReturnType::Stream { .. } => false,
    }
}

fn client_body_contains_declaration(
    declaration: &ClientFunctionDeclaration,
    span: &SourceSpan,
) -> bool {
    declaration.body.as_state_block().is_some_and(|block| {
        block
            .states
            .iter()
            .any(|state| name_part_matches_span(&state.name, span))
            || all_client_local_declarations(block)
                .iter()
                .any(|local| name_part_matches_span(local.name(), span))
    })
}

fn is_declaration_span(parse: &Parse, span: &SourceSpan) -> bool {
    parse
        .schemas()
        .iter()
        .any(|declaration| qualified_name_matches_span(&declaration.name, span))
        || parse.object_types().iter().any(|declaration| {
            qualified_name_matches_span(&declaration.name, span)
                || declaration
                    .fields
                    .iter()
                    .any(|field| name_part_matches_span(&field.name, span))
        })
        || parse
            .enum_types()
            .iter()
            .any(|declaration| qualified_name_matches_span(&declaration.name, span))
        || parse.record_value_types().iter().any(|declaration| {
            qualified_name_matches_span(&declaration.name, span)
                || declaration
                    .fields
                    .iter()
                    .any(|field| name_part_matches_span(&field.name, span))
        })
        || parse
            .primitive_value_types()
            .iter()
            .any(|declaration| qualified_name_matches_span(&declaration.name, span))
        || parse
            .opaque_value_types()
            .iter()
            .any(|declaration| qualified_name_matches_span(&declaration.name, span))
        || parse.server_functions().iter().any(|declaration| {
            qualified_name_matches_span(&declaration.name, span)
                || declaration
                    .parameters
                    .iter()
                    .any(|parameter| name_part_matches_span(&parameter.name, span))
                || return_type_contains_declaration(&declaration.return_type, span)
        })
        || parse.client_functions().iter().any(|declaration| {
            qualified_name_matches_span(&declaration.name, span)
                || declaration
                    .parameters
                    .iter()
                    .any(|parameter| name_part_matches_span(&parameter.name, span))
                || client_body_contains_declaration(declaration, span)
                || return_type_contains_declaration(&declaration.return_type, span)
        })
}

fn name_part_matches_text(part: &orna_syntax::NamePart, name: &str) -> bool {
    identifier_spelling_matches(&part.text, name)
}

fn span_contains_span(container: &SourceSpan, contained: &SourceSpan) -> bool {
    contained.start >= container.start && contained.end <= container.end
}

fn containing_server_function<'a>(
    parse: &'a Parse,
    selected_span: &SourceSpan,
) -> Option<&'a ServerFunctionDeclaration> {
    parse
        .server_functions()
        .iter()
        .find(|declaration| span_contains_span(&declaration.span, selected_span))
}

fn containing_client_function<'a>(
    parse: &'a Parse,
    selected_span: &SourceSpan,
) -> Option<&'a ClientFunctionDeclaration> {
    parse
        .client_functions()
        .iter()
        .find(|declaration| span_contains_span(&declaration.span, selected_span))
}

fn containing_function_span(parse: &Parse, selected_span: &SourceSpan) -> Option<SourceSpan> {
    containing_server_function(parse, selected_span)
        .map(|declaration| declaration.span.clone())
        .or_else(|| {
            containing_client_function(parse, selected_span)
                .map(|declaration| declaration.span.clone())
        })
}

fn rows_column_declaration_span(
    return_type: &FunctionReturnType,
    selected_span: &SourceSpan,
) -> Option<SourceSpan> {
    match return_type {
        FunctionReturnType::Rows { columns, .. } => columns
            .iter()
            .find(|column| name_part_matches_span(&column.name, selected_span))
            .map(|column| column.name.span.clone()),
        FunctionReturnType::Single(_) | FunctionReturnType::Stream { .. } => None,
    }
}

fn return_column_scope(
    parse: &Parse,
    selected_span: &SourceSpan,
) -> Option<(SourceSpan, SourceSpan)> {
    containing_server_function(parse, selected_span)
        .and_then(|declaration| {
            rows_column_declaration_span(&declaration.return_type, selected_span)
                .map(|column_span| (declaration.span.clone(), column_span))
        })
        .or_else(|| {
            containing_client_function(parse, selected_span).and_then(|declaration| {
                rows_column_declaration_span(&declaration.return_type, selected_span)
                    .map(|column_span| (declaration.span.clone(), column_span))
            })
        })
}

fn field_declaration_span(parse: &Parse, selected_span: &SourceSpan) -> Option<SourceSpan> {
    parse
        .object_types()
        .iter()
        .flat_map(|declaration| &declaration.fields)
        .find(|field| name_part_matches_span(&field.name, selected_span))
        .map(|field| field.name.span.clone())
        .or_else(|| {
            parse
                .record_value_types()
                .iter()
                .flat_map(|declaration| &declaration.fields)
                .find(|field| name_part_matches_span(&field.name, selected_span))
                .map(|field| field.name.span.clone())
        })
}

/// Resolves only the final name token in an accepted object-field rename.
///
/// The ALTER statement is transition evidence, not a declaration. Its new
/// name still denotes the final object field, while the old name remains
/// intentionally unresolved.
fn renamed_object_field_at<'a>(
    parse: &'a Parse,
    selected_span: &SourceSpan,
) -> Option<FieldInfo<'a>> {
    parse.field_renames().iter().find_map(|rename| {
        if !name_part_matches_span(&rename.new_field_name, selected_span) {
            return None;
        }
        let declaration = parse
            .object_types()
            .iter()
            .find(|declaration| qualified_names_match(&declaration.name, &rename.type_name))?;
        let field = declaration.fields.iter().find(|field| {
            identifier_spelling_matches(&field.name.text, &rename.new_field_name.text)
        })?;
        Some(object_field_info(field))
    })
}

fn renamed_object_field_declaration_span(
    parse: &Parse,
    selected_span: &SourceSpan,
) -> Option<SourceSpan> {
    renamed_object_field_at(parse, selected_span).map(|field| field.name.span.clone())
}

fn renamed_object_field_at_byte<'a>(parse: &'a Parse, byte: usize) -> Option<FieldInfo<'a>> {
    parse.field_renames().iter().find_map(|rename| {
        (byte >= rename.new_field_name.span.start && byte < rename.new_field_name.span.end)
            .then(|| renamed_object_field_at(parse, &rename.new_field_name.span))
            .flatten()
    })
}

#[derive(Clone, Copy)]
enum ClientExpressionPart<'a> {
    ParameterRoot(&'a orna_syntax::NamePart),
    LocalRoot(&'a orna_syntax::NamePart),
    FieldRoot(&'a orna_syntax::NamePart),
    FieldMember {
        root: &'a orna_syntax::NamePart,
        members: &'a [orna_syntax::NamePart],
        index: usize,
    },
    TargetFunction {
        root: &'a orna_syntax::NamePart,
        members: &'a [orna_syntax::NamePart],
        constructor: ClientTargetConstructor,
    },
    CallArgumentLabel,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ClientTargetConstructor {
    Resource,
    StreamResource,
    Action,
}

#[derive(Clone, Copy)]
struct ClientTargetFunctionPath<'a> {
    root: &'a orna_syntax::NamePart,
    members: &'a [orna_syntax::NamePart],
    constructor: ClientTargetConstructor,
}
#[derive(Clone, Copy)]
enum ClientLocalDeclaration<'a> {
    PreBegin(&'a orna_syntax::ClientLocalBinding),
    Procedural(&'a orna_syntax::ClientLetStatement),
}

impl<'a> ClientLocalDeclaration<'a> {
    fn name(self) -> &'a orna_syntax::NamePart {
        match self {
            Self::PreBegin(local) => &local.name,
            Self::Procedural(local) => &local.name,
        }
    }

    fn type_source(self) -> Option<&'a SourceSlice> {
        match self {
            Self::PreBegin(local) => Some(&local.type_source),
            Self::Procedural(local) => local.type_source.as_ref(),
        }
    }
}

fn client_statement_span(statement: &orna_syntax::ClientProceduralStatement) -> &SourceSpan {
    match statement {
        orna_syntax::ClientProceduralStatement::Let(statement) => &statement.span,
        orna_syntax::ClientProceduralStatement::Assignment(statement) => &statement.span,
        orna_syntax::ClientProceduralStatement::Return(statement) => &statement.span,
        orna_syntax::ClientProceduralStatement::If(statement) => &statement.span,
        orna_syntax::ClientProceduralStatement::While(statement) => &statement.span,
    }
}

fn statement_contains_span(
    statements: &[orna_syntax::ClientProceduralStatement],
    selected_span: &SourceSpan,
) -> bool {
    statements
        .iter()
        .any(|statement| span_contains_span(client_statement_span(statement), selected_span))
}

fn client_locals_visible_in_statements<'a>(
    statements: &'a [orna_syntax::ClientProceduralStatement],
    selected_span: &SourceSpan,
    mut visible: Vec<ClientLocalDeclaration<'a>>,
) -> Vec<ClientLocalDeclaration<'a>> {
    for statement in statements {
        let statement_span = client_statement_span(statement);
        if statement_span.end <= selected_span.start {
            if let orna_syntax::ClientProceduralStatement::Let(local) = statement {
                visible.push(ClientLocalDeclaration::Procedural(local));
            }
            continue;
        }
        if statement_span.start > selected_span.start {
            break;
        }

        match statement {
            orna_syntax::ClientProceduralStatement::If(statement) => {
                if span_contains_span(statement.condition.span(), selected_span) {
                    return visible;
                }
                if statement_contains_span(&statement.then_statements, selected_span) {
                    return client_locals_visible_in_statements(
                        &statement.then_statements,
                        selected_span,
                        visible,
                    );
                }
                for branch in &statement.elsif_branches {
                    if span_contains_span(branch.condition.span(), selected_span) {
                        return visible;
                    }
                    if statement_contains_span(&branch.statements, selected_span) {
                        return client_locals_visible_in_statements(
                            &branch.statements,
                            selected_span,
                            visible,
                        );
                    }
                }
                if let Some(else_statements) = &statement.else_statements
                    && statement_contains_span(else_statements, selected_span)
                {
                    return client_locals_visible_in_statements(
                        else_statements,
                        selected_span,
                        visible,
                    );
                }
            }
            orna_syntax::ClientProceduralStatement::While(statement) => {
                if span_contains_span(statement.condition.span(), selected_span) {
                    return visible;
                }
                if statement_contains_span(&statement.body, selected_span) {
                    return client_locals_visible_in_statements(
                        &statement.body,
                        selected_span,
                        visible,
                    );
                }
            }
            orna_syntax::ClientProceduralStatement::Let(_)
            | orna_syntax::ClientProceduralStatement::Assignment(_)
            | orna_syntax::ClientProceduralStatement::Return(_) => {}
        }
        return visible;
    }
    visible
}

fn client_local_declarations_visible<'a>(
    block: &'a orna_syntax::ClientStateBlockBody,
    selected_span: &SourceSpan,
) -> Vec<ClientLocalDeclaration<'a>> {
    let visible = block
        .locals
        .iter()
        .take_while(|local| local.span.end <= selected_span.start)
        .map(ClientLocalDeclaration::PreBegin)
        .collect();
    client_locals_visible_in_statements(&block.statements, selected_span, visible)
}

fn collect_client_local_declarations<'a>(
    statements: &'a [orna_syntax::ClientProceduralStatement],
    locals: &mut Vec<ClientLocalDeclaration<'a>>,
) {
    for statement in statements {
        match statement {
            orna_syntax::ClientProceduralStatement::Let(local) => {
                locals.push(ClientLocalDeclaration::Procedural(local));
            }
            orna_syntax::ClientProceduralStatement::If(statement) => {
                collect_client_local_declarations(&statement.then_statements, locals);
                for branch in &statement.elsif_branches {
                    collect_client_local_declarations(&branch.statements, locals);
                }
                if let Some(else_statements) = &statement.else_statements {
                    collect_client_local_declarations(else_statements, locals);
                }
            }
            orna_syntax::ClientProceduralStatement::While(statement) => {
                collect_client_local_declarations(&statement.body, locals);
            }
            orna_syntax::ClientProceduralStatement::Assignment(_)
            | orna_syntax::ClientProceduralStatement::Return(_) => {}
        }
    }
}
fn all_client_local_declarations<'a>(
    block: &'a orna_syntax::ClientStateBlockBody,
) -> Vec<ClientLocalDeclaration<'a>> {
    let mut locals = block
        .locals
        .iter()
        .map(ClientLocalDeclaration::PreBegin)
        .collect::<Vec<_>>();
    collect_client_local_declarations(&block.statements, &mut locals);
    locals
}
fn client_statement_part_at<'a>(
    statement: &'a orna_syntax::ClientProceduralStatement,
    selected_span: &SourceSpan,
) -> Option<ClientExpressionPart<'a>> {
    match statement {
        orna_syntax::ClientProceduralStatement::Let(statement) => {
            client_expression_part_at(&statement.expression, selected_span)
        }
        orna_syntax::ClientProceduralStatement::Assignment(statement) => {
            client_expression_part_at(&statement.expression, selected_span)
        }
        orna_syntax::ClientProceduralStatement::Return(statement) => statement
            .expression
            .as_ref()
            .and_then(|expression| client_expression_part_at(expression, selected_span)),
        orna_syntax::ClientProceduralStatement::If(statement) => {
            client_expression_part_at(&statement.condition, selected_span)
                .or_else(|| {
                    statement
                        .then_statements
                        .iter()
                        .find_map(|statement| client_statement_part_at(statement, selected_span))
                })
                .or_else(|| {
                    statement.elsif_branches.iter().find_map(|branch| {
                        client_expression_part_at(&branch.condition, selected_span).or_else(|| {
                            branch.statements.iter().find_map(|statement| {
                                client_statement_part_at(statement, selected_span)
                            })
                        })
                    })
                })
                .or_else(|| {
                    statement.else_statements.as_ref().and_then(|statements| {
                        statements.iter().find_map(|statement| {
                            client_statement_part_at(statement, selected_span)
                        })
                    })
                })
        }
        orna_syntax::ClientProceduralStatement::While(statement) => {
            client_expression_part_at(&statement.condition, selected_span).or_else(|| {
                statement
                    .body
                    .iter()
                    .find_map(|statement| client_statement_part_at(statement, selected_span))
            })
        }
    }
}

fn qualified_name_matches_parts(name: &QualifiedName, parts: &[&str]) -> bool {
    name.parts.len() == parts.len()
        && name
            .parts
            .iter()
            .zip(parts)
            .all(|(part, expected)| identifier_spelling_matches(&part.text, expected))
}

fn accepted_target_constructor(callee: &QualifiedName) -> Option<ClientTargetConstructor> {
    if qualified_name_matches_parts(callee, &["std", "data", "resource"]) {
        Some(ClientTargetConstructor::Resource)
    } else if qualified_name_matches_parts(callee, &["std", "data", "stream_resource"]) {
        Some(ClientTargetConstructor::StreamResource)
    } else if qualified_name_matches_parts(callee, &["std", "action", "call"]) {
        Some(ClientTargetConstructor::Action)
    } else {
        None
    }
}

fn is_target_argument(name: &orna_syntax::NamePart) -> bool {
    identifier_spelling_matches(&name.text, "target")
}

fn target_path_matches_name(path: ClientTargetFunctionPath<'_>, name: &QualifiedName) -> bool {
    name.parts.len() == path.members.len() + 1
        && name
            .parts
            .iter()
            .zip(std::iter::once(path.root).chain(path.members.iter()))
            .all(|(candidate, source)| identifier_spelling_matches(&candidate.text, &source.text))
}

fn target_path_key(path: ClientTargetFunctionPath<'_>) -> Vec<IdentifierKey> {
    std::iter::once(path.root)
        .chain(path.members.iter())
        .map(|part| identifier_key(&part.text))
        .collect()
}

fn target_name_key(name: &QualifiedName) -> Vec<IdentifierKey> {
    name.parts
        .iter()
        .map(|part| identifier_key(&part.text))
        .collect()
}

fn client_target_function_path_at<'a>(
    parse: &'a Parse,
    selected_span: &SourceSpan,
) -> Option<ClientTargetFunctionPath<'a>> {
    let (declaration, part) = client_expression_part_in_parse(parse, selected_span)?;
    let ClientExpressionPart::TargetFunction {
        root,
        members,
        constructor,
    } = part
    else {
        return None;
    };
    let path = ClientTargetFunctionPath {
        root,
        members,
        constructor,
    };
    let target_is_declared = parse
        .server_functions()
        .iter()
        .any(|candidate| target_path_matches_name(path, &candidate.name))
        || parse
            .client_functions()
            .iter()
            .any(|candidate| target_path_matches_name(path, &candidate.name));
    // The retained standard library uses `std` as its target root. Application
    // roots, including external `sys` functions, are present in this parse.
    if client_root_binding(declaration, root, ClientExpressionPart::FieldRoot(root)).is_some()
        && !target_is_declared
        && !identifier_spelling_matches(&root.text, "std")
    {
        return None;
    }
    Some(path)
}

fn client_target_declaration<'a>(
    parse: &'a Parse,
    selected_span: &SourceSpan,
) -> Option<DeclarationRef<'a>> {
    let path = client_target_function_path_at(parse, selected_span)?;
    let mut declaration = None;
    for candidate in parse.server_functions() {
        if target_path_matches_name(path, &candidate.name) {
            if declaration.is_some() {
                return None;
            }
            declaration = Some(DeclarationRef::ServerFunction(candidate));
        }
    }
    for candidate in parse.client_functions() {
        if target_path_matches_name(path, &candidate.name) {
            if declaration.is_some() {
                return None;
            }
            declaration = Some(DeclarationRef::ClientFunction(candidate));
        }
    }
    declaration
}

fn client_target_declaration_span(parse: &Parse, selected_span: &SourceSpan) -> Option<SourceSpan> {
    client_target_declaration(parse, selected_span)
        .map(|declaration| declaration.name_span().clone())
}

/// Returns true only for the final function component of an accepted target.
pub(crate) fn is_client_target_function_span(parse: &Parse, selected_span: &SourceSpan) -> bool {
    client_target_function_path_at(parse, selected_span).is_some()
}

fn client_expression_part_at<'a>(
    expression: &'a ClientExpression,
    selected_span: &SourceSpan,
) -> Option<ClientExpressionPart<'a>> {
    match expression {
        ClientExpression::Call {
            callee, arguments, ..
        } => {
            if let Some(constructor) = accepted_target_constructor(callee)
                && let Some(argument) = arguments
                    .iter()
                    .find(|argument| argument.name.as_ref().is_some_and(is_target_argument))
                && let ClientExpression::FieldPath { root, members, .. } = &argument.value
                && members
                    .last()
                    .is_some_and(|member| name_part_matches_span(member, selected_span))
            {
                return Some(ClientExpressionPart::TargetFunction {
                    root,
                    members,
                    constructor,
                });
            }
            arguments.iter().find_map(|argument| {
                if argument
                    .name
                    .as_ref()
                    .is_some_and(|name| name_part_matches_span(name, selected_span))
                {
                    return Some(ClientExpressionPart::CallArgumentLabel);
                }
                client_expression_part_at(&argument.value, selected_span)
            })
        }
        ClientExpression::ParameterRead { parameter } => {
            name_part_matches_span(parameter, selected_span)
                .then_some(ClientExpressionPart::ParameterRoot(parameter))
        }
        ClientExpression::LocalRead { local } => name_part_matches_span(local, selected_span)
            .then_some(ClientExpressionPart::LocalRoot(local)),
        ClientExpression::FieldPath { root, members, .. } => {
            if name_part_matches_span(root, selected_span) {
                return Some(ClientExpressionPart::FieldRoot(root));
            }
            members.iter().enumerate().find_map(|(index, member)| {
                name_part_matches_span(member, selected_span).then_some(
                    ClientExpressionPart::FieldMember {
                        root,
                        members,
                        index,
                    },
                )
            })
        }
        ClientExpression::Await { expression, .. } => {
            client_expression_part_at(expression, selected_span)
        }
        ClientExpression::Concat { left, right, .. } => {
            client_expression_part_at(left, selected_span)
                .or_else(|| client_expression_part_at(right, selected_span))
        }
        ClientExpression::Unary(unary) => {
            client_expression_part_at(&unary.expression, selected_span)
        }
        ClientExpression::Binary(binary) => client_expression_part_at(&binary.left, selected_span)
            .or_else(|| client_expression_part_at(&binary.right, selected_span)),
        ClientExpression::Parenthesized { expression, .. } => {
            client_expression_part_at(expression, selected_span)
        }
        ClientExpression::StringLiteral { .. }
        | ClientExpression::IntegerLiteral { .. }
        | ClientExpression::BooleanLiteral { .. } => None,
    }
}

fn client_body_part_at<'a>(
    declaration: &'a ClientFunctionDeclaration,
    selected_span: &SourceSpan,
) -> Option<ClientExpressionPart<'a>> {
    let block_part = |block: &'a orna_syntax::ClientStateBlockBody| {
        block
            .states
            .iter()
            .find_map(|state| match &state.default {
                orna_syntax::StateDefault::Expression(expression) => {
                    client_expression_part_at(expression, selected_span)
                }
                orna_syntax::StateDefault::Unset | orna_syntax::StateDefault::Null => None,
            })
            .or_else(|| {
                block
                    .locals
                    .iter()
                    .find_map(|local| client_expression_part_at(&local.expression, selected_span))
            })
            .or_else(|| {
                block
                    .statements
                    .iter()
                    .find_map(|statement| client_statement_part_at(statement, selected_span))
            })
            .or_else(|| {
                block
                    .return_expression
                    .as_ref()
                    .and_then(|expression| client_expression_part_at(expression, selected_span))
            })
    };
    match &declaration.body {
        orna_syntax::ClientFunctionBody::Expression { expression }
        | orna_syntax::ClientFunctionBody::ReturnExpression { expression } => {
            client_expression_part_at(expression, selected_span)
        }
        orna_syntax::ClientFunctionBody::StateBlock(block) => block_part(block),
        orna_syntax::ClientFunctionBody::BooleanLiteral { .. }
        | orna_syntax::ClientFunctionBody::ExternalContract { .. } => None,
        _ => None,
    }
}

fn client_expression_part_in_parse<'a>(
    parse: &'a Parse,
    selected_span: &SourceSpan,
) -> Option<(&'a ClientFunctionDeclaration, ClientExpressionPart<'a>)> {
    parse.client_functions().iter().find_map(|declaration| {
        if !span_contains_span(&declaration.span, selected_span) {
            return None;
        }
        client_body_part_at(declaration, selected_span).map(|part| (declaration, part))
    })
}

fn qualified_names_match(left: &QualifiedName, right: &QualifiedName) -> bool {
    left.parts.len() == right.parts.len()
        && left
            .parts
            .iter()
            .zip(&right.parts)
            .all(|(left, right)| identifier_spelling_matches(&left.text, &right.text))
}

fn type_owner_name(specification: &TypeSpecification) -> Option<QualifiedName> {
    match specification {
        TypeSpecification::Named(name) => Some(name.clone()),
        TypeSpecification::Reference { target, .. }
        | TypeSpecification::List {
            element: target, ..
        }
        | TypeSpecification::Set {
            element: target, ..
        }
        | TypeSpecification::Stream {
            element: target, ..
        }
        | TypeSpecification::Option { value: target, .. } => type_owner_name(target),
        TypeSpecification::Map { key, value, .. } => {
            type_owner_name(key).or_else(|| type_owner_name(value))
        }
        TypeSpecification::StandardLargeObject { .. } => None,
    }
}

fn source_contains_unquoted_comment(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut quoted = false;
    while index < bytes.len() {
        if quoted {
            if bytes[index] == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    index += 2;
                    continue;
                }
                quoted = false;
            }
            index += 1;
            continue;
        }
        if bytes[index] == b'"' {
            quoted = true;
            index += 1;
            continue;
        }
        if index + 2 <= bytes.len()
            && (&bytes[index..index + 2] == b"/*" || &bytes[index..index + 2] == b"--")
        {
            return true;
        }
        index += 1;
    }
    false
}

const TYPE_SOURCE_PREFIX: &str = "CREATE SERVER FUNCTION __orna_lsp_type_owner(p BOOLEAN) RETURNS ";

fn type_specification_from_source(source: &str) -> Option<TypeSpecification> {
    if source_contains_unquoted_comment(source) {
        return None;
    }
    let wrapped = format!("{TYPE_SOURCE_PREFIX}{source} AS SELECT p;");
    let parsed = orna_syntax::parse(&wrapped);
    parsed
        .server_functions()
        .first()
        .and_then(|declaration| match &declaration.return_type {
            FunctionReturnType::Single(specification) => Some(specification.clone()),
            FunctionReturnType::Rows { .. } | FunctionReturnType::Stream { .. } => None,
        })
}

fn rebase_span(span: &mut SourceSpan, source_start: usize) {
    let width = span.end - span.start;
    span.start = span.start - TYPE_SOURCE_PREFIX.len() + source_start;
    span.end = span.start + width;
}

fn rebase_qualified_name(name: &mut QualifiedName, source_start: usize) {
    rebase_span(&mut name.span, source_start);
    for part in &mut name.parts {
        rebase_span(&mut part.span, source_start);
    }
}

fn rebase_type_specification(specification: &mut TypeSpecification, source_start: usize) {
    match specification {
        TypeSpecification::Named(name) => rebase_qualified_name(name, source_start),
        TypeSpecification::StandardLargeObject { source, .. } => {
            rebase_span(&mut source.span, source_start)
        }
        TypeSpecification::Reference { target, span, .. }
        | TypeSpecification::List {
            element: target,
            span,
        }
        | TypeSpecification::Set {
            element: target,
            span,
        }
        | TypeSpecification::Option {
            value: target,
            span,
            ..
        }
        | TypeSpecification::Stream {
            element: target,
            span,
        } => {
            rebase_span(span, source_start);
            rebase_type_specification(target, source_start);
        }
        TypeSpecification::Map {
            key, value, span, ..
        } => {
            rebase_span(span, source_start);
            rebase_type_specification(key, source_start);
            rebase_type_specification(value, source_start);
        }
    }
}

fn type_specification_from_slice(source: &SourceSlice) -> Option<TypeSpecification> {
    let mut specification = type_specification_from_source(&source.text)?;
    rebase_type_specification(&mut specification, source.span.start);
    Some(specification)
}

fn type_owner_name_from_source(source: &str) -> Option<QualifiedName> {
    // The compiler's CLIENT local type resolver only strips whitespace.
    if source_contains_unquoted_comment(source) {
        return None;
    }
    type_specification_from_source(source).and_then(|specification| type_owner_name(&specification))
}

struct ClientRootBinding {
    declaration_span: SourceSpan,
    owner: Option<QualifiedName>,
}

fn client_root_binding(
    declaration: &ClientFunctionDeclaration,
    root: &orna_syntax::NamePart,
    kind: ClientExpressionPart<'_>,
) -> Option<ClientRootBinding> {
    let find_local = || {
        let block = declaration.body.as_state_block()?;
        // A binding becomes visible only after its declaration. Invalid or
        // ambiguous source must fail closed rather than guessing a target.
        let matches: Vec<_> = client_local_declarations_visible(block, &root.span)
            .into_iter()
            .filter(|local| name_part_matches_text(local.name(), &root.text))
            .collect();
        if matches.len() != 1 {
            return None;
        }
        let local = matches[0];
        Some(ClientRootBinding {
            declaration_span: local.name().span.clone(),
            owner: local
                .type_source()
                .and_then(|source| type_owner_name_from_source(&source.text)),
        })
    };
    let find_state = || {
        let block = declaration.body.as_state_block()?;
        let mut matches = block.states.iter().filter(|state| {
            name_part_matches_text(&state.name, &root.text) && state.span.end <= root.span.start
        });
        let state = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(ClientRootBinding {
            declaration_span: state.name.span.clone(),
            owner: type_owner_name(&state.type_specification),
        })
    };
    match kind {
        ClientExpressionPart::LocalRoot(_) => find_local(),
        ClientExpressionPart::ParameterRoot(_) => find_local()
            .or_else(|| {
                declaration
                    .parameters
                    .iter()
                    .find(|parameter| name_part_matches_text(&parameter.name, &root.text))
                    .map(|parameter| ClientRootBinding {
                        declaration_span: parameter.name.span.clone(),
                        owner: type_owner_name(&parameter.type_specification),
                    })
            })
            .or_else(find_state),
        // The compiler's FieldPath grammar starts from a parameter. Keep
        // that precedence when a local happens to share its name.
        ClientExpressionPart::FieldRoot(_) => declaration
            .parameters
            .iter()
            .find(|parameter| name_part_matches_text(&parameter.name, &root.text))
            .map(|parameter| ClientRootBinding {
                declaration_span: parameter.name.span.clone(),
                owner: type_owner_name(&parameter.type_specification),
            })
            .or_else(find_local),
        ClientExpressionPart::FieldMember { .. }
        | ClientExpressionPart::TargetFunction { .. }
        | ClientExpressionPart::CallArgumentLabel => None,
    }
}

fn field_on_object_or_record<'a>(
    parse: &'a Parse,
    owner: &QualifiedName,
    field_name: &str,
) -> Option<FieldInfo<'a>> {
    if let Some(declaration) = parse
        .object_types()
        .iter()
        .find(|declaration| qualified_names_match(&declaration.name, owner))
    {
        let field = declaration
            .fields
            .iter()
            .find(|field| identifier_spelling_matches(&field.name.text, field_name))?;
        return Some(object_field_info(field));
    }
    let declaration = parse
        .record_value_types()
        .iter()
        .find(|declaration| qualified_names_match(&declaration.name, owner))?;
    let field = declaration
        .fields
        .iter()
        .find(|field| identifier_spelling_matches(&field.name.text, field_name))?;
    Some(record_field_info(field))
}

fn client_parameter_info<'a>(
    declaration: &'a ClientFunctionDeclaration,
    root: &orna_syntax::NamePart,
    kind: ClientExpressionPart<'_>,
) -> Option<ParameterInfo<'a>> {
    let find_parameter = || {
        declaration
            .parameters
            .iter()
            .find(|parameter| name_part_matches_text(&parameter.name, &root.text))
            .map(|parameter| ParameterInfo {
                name: &parameter.name,
                type_specification: &parameter.type_specification,
                default_text: parameter
                    .default_expression
                    .as_ref()
                    .map(|default| default.text.as_str()),
                documentation: parameter.documentation.as_ref().map(strip_quotes),
            })
    };
    let find_state = || {
        declaration
            .body
            .as_state_block()
            .and_then(|block| {
                block.states.iter().find(|state| {
                    name_part_matches_text(&state.name, &root.text)
                        && state.span.end <= root.span.start
                })
            })
            .map(|state| ParameterInfo {
                name: &state.name,
                type_specification: &state.type_specification,
                default_text: None,
                documentation: None,
            })
    };
    let visible_local = || {
        declaration.body.as_state_block().is_some_and(|block| {
            client_local_declarations_visible(block, &root.span)
                .iter()
                .any(|local| name_part_matches_text(local.name(), &root.text))
        })
    };
    match kind {
        ClientExpressionPart::ParameterRoot(_) => {
            if visible_local() {
                None
            } else {
                find_parameter().or_else(find_state)
            }
        }
        ClientExpressionPart::LocalRoot(_) => None,
        ClientExpressionPart::FieldRoot(_) => find_parameter().or_else(find_state),
        ClientExpressionPart::FieldMember { .. }
        | ClientExpressionPart::TargetFunction { .. }
        | ClientExpressionPart::CallArgumentLabel => None,
    }
}

fn client_local_hover(
    declaration: &ClientFunctionDeclaration,
    root: &orna_syntax::NamePart,
    text: &str,
    doc_link: Option<&str>,
) -> Option<Hover> {
    let block = declaration.body.as_state_block()?;
    let matches: Vec<_> = client_local_declarations_visible(block, &root.span)
        .into_iter()
        .filter(|local| name_part_matches_text(local.name(), &root.text))
        .collect();
    if matches.len() != 1 {
        return None;
    }
    let local = matches[0];
    let name = local.name();
    let type_source = local.type_source()?;
    let specification = type_specification_from_slice(type_source)?;
    let parameter = ParameterInfo {
        name,
        type_specification: &specification,
        default_text: None,
        documentation: None,
    };
    Some(crate::hover::parameter_hover(&parameter, text, doc_link))
}

fn client_field_info_at<'a>(parse: &'a Parse, selected_span: &SourceSpan) -> Option<FieldInfo<'a>> {
    let (declaration, part) = client_expression_part_in_parse(parse, selected_span)?;
    let ClientExpressionPart::FieldMember {
        root,
        members,
        index,
    } = part
    else {
        return None;
    };
    let mut owner =
        client_root_binding(declaration, root, ClientExpressionPart::FieldRoot(root))?.owner?;
    for (member_index, member) in members.iter().take(index + 1).enumerate() {
        let field = field_on_object_or_record(parse, &owner, &member.text)?;
        if member_index == index {
            return Some(field);
        }
        owner = type_owner_name(field.type_specification)?;
    }
    None
}

fn client_field_declaration_span(parse: &Parse, selected_span: &SourceSpan) -> Option<SourceSpan> {
    let (declaration, part) = client_expression_part_in_parse(parse, selected_span)?;
    let ClientExpressionPart::FieldMember {
        root,
        members,
        index,
    } = part
    else {
        return None;
    };
    let mut owner =
        client_root_binding(declaration, root, ClientExpressionPart::FieldRoot(root))?.owner?;
    let mut field_span = None;
    for (member_index, member) in members.iter().take(index + 1).enumerate() {
        let field = field_on_object_or_record(parse, &owner, &member.text)?;
        field_span = Some(field.name.span.clone());
        if member_index < index {
            owner = type_owner_name(field.type_specification)?;
        }
    }
    field_span
}

fn query_expression_field_path_at<'a>(
    expression: &'a orna_syntax::QueryExpression,
    selected_span: &SourceSpan,
) -> Option<(&'a orna_syntax::NamePart, &'a [orna_syntax::NamePart])> {
    match expression {
        orna_syntax::QueryExpression::FieldPath { root, members, .. }
            if members
                .iter()
                .any(|member| name_part_matches_span(member, selected_span)) =>
        {
            Some((root, members))
        }
        orna_syntax::QueryExpression::Equality { left, right, .. } => {
            query_expression_field_path_at(left, selected_span)
                .or_else(|| query_expression_field_path_at(right, selected_span))
        }
        orna_syntax::QueryExpression::ObjectReference { .. }
        | orna_syntax::QueryExpression::FieldPath { .. }
        | orna_syntax::QueryExpression::BooleanLiteral { .. }
        | orna_syntax::QueryExpression::ParameterRead { .. } => None,
    }
}

fn query_field_path_at<'a>(
    query: &'a orna_syntax::SelectQuery,
    selected_span: &SourceSpan,
) -> Option<(&'a orna_syntax::NamePart, &'a [orna_syntax::NamePart])> {
    let root_matches = |root: &orna_syntax::NamePart| {
        identifier_spelling_matches(&query.source_object.alias.text, &root.text)
    };
    query
        .projections
        .iter()
        .find_map(|expression| {
            query_expression_field_path_at(expression, selected_span)
                .filter(|(root, _)| root_matches(root))
        })
        .or_else(|| {
            query
                .predicate
                .as_ref()
                .and_then(|expression| query_expression_field_path_at(expression, selected_span))
                .filter(|(root, _)| root_matches(root))
        })
        .or_else(|| {
            query.ordering.iter().find_map(|ordering| {
                query_expression_field_path_at(&ordering.expression, selected_span)
                    .filter(|(root, _)| root_matches(root))
            })
        })
}

/// Returns the object or record field whose name covers one byte offset inside SQL.
fn sql_column_at<'a>(
    parse: &'a Parse,
    byte: usize,
    text: &str,
    highlighted: &[orna_syntax::HighlightToken],
) -> Option<FieldInfo<'a>> {
    let (_, kind, selected_span) = token_at(text, highlighted, byte)?;
    if !matches!(
        kind,
        HighlightKind::PropertyName | HighlightKind::QuotedIdentifier
    ) {
        return None;
    }
    for declaration in parse.server_functions() {
        let resolved = match &declaration.body {
            orna_syntax::ServerFunctionBody::SqlQuery(body) => {
                query_field_path_at(&body.query, &selected_span).and_then(|(_, members)| {
                    let mut owner = body.query.source_object.object_type.clone();
                    for (index, member) in members.iter().enumerate() {
                        let field = field_on_object_or_record(parse, &owner, &member.text)?;
                        if name_part_matches_span(member, &selected_span) {
                            return Some(field);
                        }
                        if index + 1 < members.len() {
                            owner = type_owner_name(field.type_specification)?;
                        }
                    }
                    None
                })
            }
            orna_syntax::ServerFunctionBody::SqlInsert(body) => body
                .insert
                .target_fields
                .iter()
                .find(|field| name_part_matches_span(field, &selected_span))
                .and_then(|field| {
                    field_on_object_or_record(parse, &body.insert.target_object, &field.text)
                }),
            orna_syntax::ServerFunctionBody::SqlUpdate(body) => body
                .update
                .assignments
                .iter()
                .find(|assignment| name_part_matches_span(&assignment.target_field, &selected_span))
                .and_then(|assignment| {
                    field_on_object_or_record(
                        parse,
                        &body.update.target_object,
                        &assignment.target_field.text,
                    )
                }),
            _ => None,
        };
        if resolved.is_some() {
            return resolved;
        }
    }
    None
}

fn query_field_path_any_at<'a>(
    query: &'a orna_syntax::SelectQuery,
    selected_span: &SourceSpan,
) -> Option<(&'a orna_syntax::NamePart, &'a [orna_syntax::NamePart])> {
    query
        .projections
        .iter()
        .find_map(|expression| query_expression_field_path_at(expression, selected_span))
        .or_else(|| {
            query
                .predicate
                .as_ref()
                .and_then(|expression| query_expression_field_path_at(expression, selected_span))
        })
        .or_else(|| {
            query.ordering.iter().find_map(|ordering| {
                query_expression_field_path_at(&ordering.expression, selected_span)
            })
        })
}

fn sql_query_contains_span(parse: &Parse, selected_span: &SourceSpan) -> bool {
    parse
        .server_functions()
        .iter()
        .any(|declaration| match &declaration.body {
            orna_syntax::ServerFunctionBody::SqlQuery(body) => {
                span_contains_span(&body.source.span, selected_span)
            }
            _ => false,
        })
}

fn sql_unresolved_field_or_alias(parse: &Parse, selected_span: &SourceSpan) -> bool {
    parse
        .server_functions()
        .iter()
        .any(|declaration| match &declaration.body {
            orna_syntax::ServerFunctionBody::SqlQuery(body) => {
                if name_part_matches_span(&body.query.source_object.alias, selected_span) {
                    return true;
                }
                let Some((root, members)) = query_field_path_any_at(&body.query, selected_span)
                else {
                    return false;
                };
                let root_matches =
                    identifier_spelling_matches(&body.query.source_object.alias.text, &root.text);
                if !root_matches || name_part_matches_span(root, selected_span) {
                    return true;
                }
                let mut owner = body.query.source_object.object_type.clone();
                for (index, member) in members.iter().enumerate() {
                    let Some(field) = field_on_object_or_record(parse, &owner, &member.text) else {
                        return true;
                    };
                    if name_part_matches_span(member, selected_span) {
                        return false;
                    }
                    if index + 1 < members.len() {
                        let Some(next_owner) = type_owner_name(field.type_specification) else {
                            return true;
                        };
                        owner = next_owner;
                    }
                }
                false
            }
            orna_syntax::ServerFunctionBody::SqlInsert(body) => {
                body.insert.target_fields.iter().any(|field| {
                    name_part_matches_span(field, selected_span)
                        && field_on_object_or_record(parse, &body.insert.target_object, &field.text)
                            .is_none()
                })
            }
            orna_syntax::ServerFunctionBody::SqlUpdate(body) => {
                body.update.assignments.iter().any(|assignment| {
                    name_part_matches_span(&assignment.target_field, selected_span)
                        && field_on_object_or_record(
                            parse,
                            &body.update.target_object,
                            &assignment.target_field.text,
                        )
                        .is_none()
                })
            }
            _ => false,
        })
}

fn field_reference_declaration_span(
    parse: &Parse,
    text: &str,
    highlighted: &[orna_syntax::HighlightToken],
    selected_span: &SourceSpan,
) -> Option<SourceSpan> {
    client_field_declaration_span(parse, selected_span)
        .or_else(|| renamed_object_field_declaration_span(parse, selected_span))
        .or_else(|| {
            sql_column_at(parse, selected_span.start, text, highlighted)
                .map(|field| field.name.span.clone())
        })
}

fn property_declaration_span(
    parse: &Parse,
    text: &str,
    highlighted: &[orna_syntax::HighlightToken],
    selected_span: &SourceSpan,
) -> Option<SourceSpan> {
    if let Some((_, column_span)) = return_column_scope(parse, selected_span) {
        return Some(column_span);
    }
    if let Some(span) = client_field_declaration_span(parse, selected_span) {
        return Some(span);
    }
    if is_declaration_span(parse, selected_span)
        && let Some(span) = field_declaration_span(parse, selected_span)
    {
        return Some(span);
    }
    field_reference_declaration_span(parse, text, highlighted, selected_span)
}

fn variable_declaration_span(
    parse: &Parse,
    name: &str,
    selected_span: &SourceSpan,
) -> Option<SourceSpan> {
    if let Some(declaration) = containing_server_function(parse, selected_span) {
        let matches: Vec<_> = declaration
            .parameters
            .iter()
            .filter(|parameter| name_part_matches_text(&parameter.name, name))
            .collect();
        return (matches.len() == 1).then(|| matches[0].name.span.clone());
    }

    let declaration = containing_client_function(parse, selected_span)?;
    let mut exact = Vec::new();
    let mut visible = Vec::new();
    if let Some(block) = declaration.body.as_state_block() {
        for state in &block.states {
            if !name_part_matches_text(&state.name, name) {
                continue;
            }
            if name_part_matches_span(&state.name, selected_span) {
                exact.push(state.name.span.clone());
            } else if state.span.end <= selected_span.start {
                visible.push(state.name.span.clone());
            }
        }
        for local in all_client_local_declarations(block) {
            if name_part_matches_text(local.name(), name)
                && name_part_matches_span(local.name(), selected_span)
            {
                exact.push(local.name().span.clone());
            }
        }
        for local in client_local_declarations_visible(block, selected_span) {
            if name_part_matches_text(local.name(), name) {
                visible.push(local.name().span.clone());
            }
        }
    }
    if exact.len() > 1 || visible.len() > 1 {
        return None;
    }
    if let Some(span) = exact.into_iter().next() {
        return Some(span);
    }
    if let Some(span) = visible.into_iter().next() {
        return Some(span);
    }
    let parameters: Vec<_> = declaration
        .parameters
        .iter()
        .filter(|parameter| name_part_matches_text(&parameter.name, name))
        .collect();
    (parameters.len() == 1).then(|| parameters[0].name.span.clone())
}

fn is_client_call_argument_label(parse: &Parse, selected_span: &SourceSpan) -> bool {
    client_expression_part_in_parse(parse, selected_span)
        .is_some_and(|(_, part)| matches!(part, ClientExpressionPart::CallArgumentLabel))
}

fn declaration_span_for_kind(
    parse: &Parse,
    text: &str,
    highlighted: &[orna_syntax::HighlightToken],
    name: &str,
    kind: HighlightKind,
    selected_span: &SourceSpan,
) -> Option<SourceSpan> {
    if is_client_call_argument_label(parse, selected_span) {
        return None;
    }
    if let Some(span) = client_target_declaration_span(parse, selected_span) {
        return Some(span);
    }
    if kind == HighlightKind::QuotedIdentifier
        && sql_unresolved_field_or_alias(parse, selected_span)
    {
        return None;
    }
    if let Some((declaration, part)) = client_expression_part_in_parse(parse, selected_span) {
        match part {
            ClientExpressionPart::ParameterRoot(root)
            | ClientExpressionPart::LocalRoot(root)
            | ClientExpressionPart::FieldRoot(root) => {
                return client_root_binding(declaration, root, part)
                    .map(|binding| binding.declaration_span);
            }
            ClientExpressionPart::FieldMember { .. } => {
                return client_field_declaration_span(parse, selected_span);
            }
            ClientExpressionPart::TargetFunction { .. }
            | ClientExpressionPart::CallArgumentLabel => return None,
        }
    }
    match kind {
        HighlightKind::PropertyName => {
            property_declaration_span(parse, text, highlighted, selected_span)
        }
        HighlightKind::VariableName => variable_declaration_span(parse, name, selected_span),
        HighlightKind::QuotedIdentifier => {
            property_declaration_span(parse, text, highlighted, selected_span)
                .or_else(|| variable_declaration_span(parse, name, selected_span))
                .or_else(|| {
                    declaration_at_span(parse, text, highlighted, name, kind, selected_span)
                        .map(|declaration| declaration.name_span().clone())
                })
        }
        _ => declaration_at_span(parse, text, highlighted, name, kind, selected_span)
            .map(|declaration| declaration.name_span().clone()),
    }
}

#[derive(Clone)]
enum ReferenceScope {
    /// A resolved top-level name and its full source-qualified path.
    TopLevel {
        kind: HighlightKind,
        path: Vec<IdentifierKey>,
    },
    /// A parameter, state, or local inside one function.
    Variable {
        function_span: SourceSpan,
        declaration_span: SourceSpan,
    },
    /// A ROWS return column inside one function.
    ReturnColumn {
        function_span: SourceSpan,
        column_span: SourceSpan,
    },
    /// An object or record field declaration and its resolved uses.
    Field(SourceSpan),
    /// A target function path in an accepted resource or action constructor.
    TargetFunction(Vec<IdentifierKey>),
    /// The selected token has no resolved declaration and must not leak.
    None,
}

/// Resolves references requested from a function declaration that is used as
/// an accepted CLIENT target. Target paths are highlighted as properties
/// because they are not calls, so a declaration-side `TopLevel(FunctionName)`
/// scope cannot see them. Keep this path separate from ordinary function
/// lookup: only the three accepted target constructors may contribute uses.
fn target_declaration_scope(
    parse: &Parse,
    highlighted: &[orna_syntax::HighlightToken],
    selected_span: &SourceSpan,
) -> Option<ReferenceScope> {
    let target = parse
        .server_functions()
        .iter()
        .find(|declaration| qualified_name_matches_span(&declaration.name, selected_span))
        .map(|declaration| target_name_key(&declaration.name))
        .or_else(|| {
            parse
                .client_functions()
                .iter()
                .find(|declaration| qualified_name_matches_span(&declaration.name, selected_span))
                .map(|declaration| target_name_key(&declaration.name))
        })?;

    let mut found_target = false;
    for token in highlighted {
        let token_span = SourceSpan {
            start: token.range.start,
            end: token.range.end,
        };
        let Some(path) = client_target_function_path_at(parse, &token_span) else {
            continue;
        };
        if target_path_key(path) != target {
            continue;
        }
        found_target = true;
        if client_target_declaration(parse, &token_span).is_none() {
            // An unresolved or duplicate SERVER/CLIENT target is ambiguous;
            // do not let the declaration side make it appear resolved.
            return Some(ReferenceScope::None);
        }
    }

    found_target.then_some(ReferenceScope::TargetFunction(target))
}

fn reference_scope(
    parse: &Parse,
    text: &str,
    highlighted: &[orna_syntax::HighlightToken],
    name: &str,
    kind: HighlightKind,
    selected_span: &SourceSpan,
) -> ReferenceScope {
    if is_client_call_argument_label(parse, selected_span) {
        return ReferenceScope::None;
    }
    if let Some(scope) = target_declaration_scope(parse, highlighted, selected_span) {
        return scope;
    }
    if let Some(path) = client_target_function_path_at(parse, selected_span) {
        if client_target_declaration(parse, selected_span).is_none() {
            // An unresolved or duplicate SERVER/CLIENT target is ambiguous.
            return ReferenceScope::None;
        }
        return ReferenceScope::TargetFunction(target_path_key(path));
    }
    if kind == HighlightKind::PropertyName {
        if let Some((function_span, column_span)) = return_column_scope(parse, selected_span) {
            return ReferenceScope::ReturnColumn {
                function_span,
                column_span,
            };
        }
        if is_declaration_span(parse, selected_span)
            && field_declaration_span(parse, selected_span).is_some()
        {
            return ReferenceScope::Field(
                field_declaration_span(parse, selected_span)
                    .expect("field declaration checked above"),
            );
        }
        if let Some(field_span) =
            field_reference_declaration_span(parse, text, highlighted, selected_span)
        {
            return ReferenceScope::Field(field_span);
        }
        return ReferenceScope::None;
    }
    if kind == HighlightKind::QuotedIdentifier {
        if let Some((function_span, column_span)) = return_column_scope(parse, selected_span) {
            return ReferenceScope::ReturnColumn {
                function_span,
                column_span,
            };
        }
        if is_declaration_span(parse, selected_span)
            && field_declaration_span(parse, selected_span).is_some()
        {
            return ReferenceScope::Field(
                field_declaration_span(parse, selected_span)
                    .expect("field declaration checked above"),
            );
        }
        if let Some(field_span) =
            field_reference_declaration_span(parse, text, highlighted, selected_span)
        {
            return ReferenceScope::Field(field_span);
        }
        if sql_unresolved_field_or_alias(parse, selected_span) {
            return ReferenceScope::None;
        }
    }
    if let Some((declaration, part)) = client_expression_part_in_parse(parse, selected_span) {
        match part {
            ClientExpressionPart::ParameterRoot(root)
            | ClientExpressionPart::LocalRoot(root)
            | ClientExpressionPart::FieldRoot(root) => {
                if let Some(binding) = client_root_binding(declaration, root, part) {
                    return ReferenceScope::Variable {
                        function_span: declaration.span.clone(),
                        declaration_span: binding.declaration_span,
                    };
                }
                return ReferenceScope::None;
            }
            ClientExpressionPart::FieldMember { .. } => {
                return client_field_declaration_span(parse, selected_span)
                    .map_or(ReferenceScope::None, ReferenceScope::Field);
            }
            ClientExpressionPart::TargetFunction { .. }
            | ClientExpressionPart::CallArgumentLabel => return ReferenceScope::None,
        }
    }
    if matches!(
        kind,
        HighlightKind::VariableName | HighlightKind::QuotedIdentifier
    ) && containing_function_span(parse, selected_span).is_some()
    {
        if let Some(declaration_span) = variable_declaration_span(parse, name, selected_span) {
            return ReferenceScope::Variable {
                function_span: containing_function_span(parse, selected_span)
                    .expect("containing function checked above"),
                declaration_span,
            };
        }
        if kind == HighlightKind::VariableName {
            return ReferenceScope::None;
        }
    }
    if declaration_span_for_kind(parse, text, highlighted, name, kind, selected_span).is_some() {
        let path = qualified_name_keys_at(text, highlighted, selected_span)
            .unwrap_or_else(|| vec![identifier_key(name)]);
        ReferenceScope::TopLevel { kind, path }
    } else {
        ReferenceScope::None
    }
}

fn variable_reference_declaration_span(
    parse: &Parse,
    text: &str,
    token: &orna_syntax::HighlightToken,
) -> Option<SourceSpan> {
    let token_span = SourceSpan {
        start: token.range.start,
        end: token.range.end,
    };
    if is_client_call_argument_label(parse, &token_span) {
        return None;
    }
    if let Some((declaration, part)) = client_expression_part_in_parse(parse, &token_span) {
        return match part {
            ClientExpressionPart::ParameterRoot(root)
            | ClientExpressionPart::LocalRoot(root)
            | ClientExpressionPart::FieldRoot(root) => {
                client_root_binding(declaration, root, part).map(|binding| binding.declaration_span)
            }
            ClientExpressionPart::FieldMember { .. }
            | ClientExpressionPart::TargetFunction { .. }
            | ClientExpressionPart::CallArgumentLabel => None,
        };
    }
    if matches!(
        token.kind,
        HighlightKind::VariableName | HighlightKind::QuotedIdentifier
    ) {
        let name = text[token.range.clone()].to_owned();
        return variable_declaration_span(parse, &name, &token_span);
    }
    None
}

fn reference_token_in_scope(
    parse: &Parse,
    text: &str,
    highlighted: &[orna_syntax::HighlightToken],
    token: &orna_syntax::HighlightToken,
    scope: &ReferenceScope,
) -> bool {
    let token_span = SourceSpan {
        start: token.range.start,
        end: token.range.end,
    };
    if is_client_call_argument_label(parse, &token_span) {
        return false;
    }
    match scope {
        ReferenceScope::TopLevel { kind, path } => {
            token.kind == *kind
                && qualified_name_keys_at(text, highlighted, &token_span)
                    .unwrap_or_else(|| vec![identifier_key(&text[token.range.clone()])])
                    .as_slice()
                    == path.as_slice()
        }
        ReferenceScope::None => false,
        ReferenceScope::Variable {
            function_span,
            declaration_span,
        } => {
            span_contains_span(function_span, &token_span)
                && variable_reference_declaration_span(parse, text, token)
                    .is_some_and(|candidate| source_span_matches(&candidate, declaration_span))
        }
        ReferenceScope::ReturnColumn {
            function_span,
            column_span,
        } => {
            if !span_contains_span(function_span, &token_span)
                || !matches!(
                    token.kind,
                    HighlightKind::PropertyName | HighlightKind::QuotedIdentifier
                )
            {
                return false;
            }
            source_span_matches(column_span, &token_span)
                || (sql_query_contains_span(parse, &token_span)
                    && !sql_unresolved_field_or_alias(parse, &token_span)
                    && sql_column_at(parse, token.range.start, text, highlighted).is_none()
                    && identifier_spelling_matches(
                        &text[token.range.clone()],
                        &text[column_span.start..column_span.end],
                    ))
        }
        ReferenceScope::Field(field_span) => {
            source_span_matches(field_span, &token_span)
                || field_reference_declaration_span(parse, text, highlighted, &token_span)
                    .is_some_and(|candidate| source_span_matches(&candidate, field_span))
        }
        ReferenceScope::TargetFunction(target) => {
            if let Some(path) = client_target_function_path_at(parse, &token_span) {
                return target_path_key(path).as_slice() == target.as_slice();
            }
            parse.server_functions().iter().any(|declaration| {
                target_name_key(&declaration.name).as_slice() == target.as_slice()
                    && declaration
                        .name
                        .parts
                        .last()
                        .is_some_and(|part| source_span_matches(&part.span, &token_span))
            }) || parse.client_functions().iter().any(|declaration| {
                target_name_key(&declaration.name).as_slice() == target.as_slice()
                    && declaration
                        .name
                        .parts
                        .last()
                        .is_some_and(|part| source_span_matches(&part.span, &token_span))
            })
        }
    }
}

/// The data behind a field hover.
pub struct FieldInfo<'a> {
    /// The field name as written in source.
    pub name: &'a orna_syntax::NamePart,
    /// The declared field type.
    pub type_specification: &'a orna_syntax::TypeSpecification,
    /// Whether the field is nullable; absent for record value fields.
    pub nullable: Option<bool>,
    /// Whether the field has a uniqueness constraint.
    pub unique: bool,
    /// The rendered on-delete policy, when declared.
    pub on_delete: Option<&'static str>,
    /// The documentation text, with quotes stripped.
    pub documentation: Option<&'a str>,
    /// The default expression source, when declared.
    pub default_text: Option<&'a str>,
}

/// The data behind a parameter hover.
pub struct ParameterInfo<'a> {
    /// The parameter name as written in source.
    pub name: &'a orna_syntax::NamePart,
    /// The declared parameter type.
    pub type_specification: &'a orna_syntax::TypeSpecification,
    /// The default expression source, when declared.
    pub default_text: Option<&'a str>,
    /// The documentation text, with quotes stripped.
    pub documentation: Option<&'a str>,
}

/// Returns the object or record field whose name covers one byte offset.
pub fn field_at(parse: &Parse, byte: usize) -> Option<FieldInfo<'_>> {
    if let Some(field) = renamed_object_field_at_byte(parse, byte) {
        return Some(field);
    }
    for declaration in parse.object_types() {
        for field in &declaration.fields {
            if byte >= field.name.span.start && byte < field.name.span.end {
                return Some(object_field_info(field));
            }
        }
    }
    for declaration in parse.record_value_types() {
        for field in &declaration.fields {
            if byte >= field.name.span.start && byte < field.name.span.end {
                return Some(record_field_info(field));
            }
        }
    }
    None
}

/// Returns the function parameter whose name covers one byte offset.
pub fn parameter_at<'a>(parse: &'a Parse, byte: usize) -> Option<ParameterInfo<'a>> {
    let find =
        |parameters: &'a [orna_syntax::ServerFunctionParameter]| -> Option<ParameterInfo<'a>> {
            parameters
                .iter()
                .find(|parameter| {
                    byte >= parameter.name.span.start && byte < parameter.name.span.end
                })
                .map(|parameter| ParameterInfo {
                    name: &parameter.name,
                    type_specification: &parameter.type_specification,
                    documentation: parameter.documentation.as_ref().map(strip_quotes),
                    default_text: parameter
                        .default_expression
                        .as_ref()
                        .map(|default| default.text.as_str()),
                })
        };
    parse
        .server_functions()
        .iter()
        .find_map(|declaration| find(&declaration.parameters))
        .or_else(|| {
            parse
                .client_functions()
                .iter()
                .find_map(|declaration| find(&declaration.parameters))
        })
}

/// Builds the hover data for one object field.
fn object_field_info(field: &orna_syntax::ObjectFieldDeclaration) -> FieldInfo<'_> {
    FieldInfo {
        name: &field.name,
        type_specification: &field.type_specification,
        nullable: Some(field.nullable),
        unique: field.unique,
        on_delete: field.on_delete.map(on_delete_text),
        documentation: field.documentation.as_ref().map(strip_quotes),
        default_text: field
            .default_expression
            .as_ref()
            .map(|default| default.text.as_str()),
    }
}

fn record_field_info(field: &orna_syntax::ValueFieldDeclaration) -> FieldInfo<'_> {
    FieldInfo {
        name: &field.name,
        type_specification: &field.type_specification,
        nullable: None,
        unique: false,
        on_delete: None,
        documentation: field.documentation.as_ref().map(strip_quotes),
        default_text: None,
    }
}

/// Strips the surrounding apostrophes from a captured string literal.
fn strip_quotes(slice: &orna_syntax::SourceSlice) -> &str {
    slice
        .text
        .strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
        .unwrap_or(&slice.text)
}

/// Renders an on-delete policy as source text.
fn on_delete_text(policy: orna_syntax::OnDeletePolicy) -> &'static str {
    match policy {
        orna_syntax::OnDeletePolicy::Restrict => "RESTRICT",
        orna_syntax::OnDeletePolicy::SetNull => "SET NULL",
        orna_syntax::OnDeletePolicy::Cascade => "CASCADE",
    }
}

/// Finds a canonical multi-word scalar type containing one source byte.
///
/// `Parse::highlight` deliberately emits one type-name token per word in a
/// `CHARACTER LARGE OBJECT` or `BINARY LARGE OBJECT` phrase.  Hovering one of
/// those tokens must nevertheless resolve the AST's complete type span, while
/// ordinary words with the same spelling remain ordinary words outside a type
/// specification.
fn standard_large_object_at(
    parse: &Parse,
    byte: usize,
) -> Option<(&SourceSpan, StandardLargeObjectKind)> {
    fn in_source(
        source: &orna_syntax::SourceSlice,
        byte: usize,
    ) -> Option<(&SourceSpan, StandardLargeObjectKind)> {
        if source.span.start > byte || byte >= source.span.end {
            return None;
        }
        let mut words = Vec::new();
        let mut offset = 0;
        while offset < source.text.len() {
            let rest = &source.text[offset..];
            if rest.starts_with("--") || rest.starts_with("/*") {
                return None;
            }
            let character = rest.chars().next()?;
            if character.is_ascii_whitespace() {
                offset += character.len_utf8();
                continue;
            }
            if character.is_ascii_alphabetic() {
                let start = offset;
                offset += character.len_utf8();
                while offset < source.text.len() {
                    let next = source.text[offset..].chars().next()?;
                    if next.is_ascii_alphanumeric() {
                        offset += next.len_utf8();
                    } else {
                        break;
                    }
                }
                words.push(source.text[start..offset].to_ascii_uppercase());
                continue;
            }
            return None;
        }
        let kind = match words.as_slice() {
            [character, large, object]
                if character == "CHARACTER" && large == "LARGE" && object == "OBJECT" =>
            {
                StandardLargeObjectKind::Character
            }
            [binary, large, object]
                if binary == "BINARY" && large == "LARGE" && object == "OBJECT" =>
            {
                StandardLargeObjectKind::Binary
            }
            _ => return None,
        };
        Some((&source.span, kind))
    }

    fn in_spec(
        specification: &TypeSpecification,
        byte: usize,
    ) -> Option<(&SourceSpan, StandardLargeObjectKind)> {
        match specification {
            TypeSpecification::StandardLargeObject { kind, source }
                if source.span.start <= byte && byte < source.span.end =>
            {
                Some((&source.span, *kind))
            }
            TypeSpecification::Reference { target, .. }
            | TypeSpecification::List {
                element: target, ..
            }
            | TypeSpecification::Set {
                element: target, ..
            }
            | TypeSpecification::Option { value: target, .. }
            | TypeSpecification::Stream {
                element: target, ..
            } => in_spec(target, byte),
            TypeSpecification::Map { key, value, .. } => {
                in_spec(key, byte).or_else(|| in_spec(value, byte))
            }
            TypeSpecification::Named(_) | TypeSpecification::StandardLargeObject { .. } => None,
        }
    }

    fn in_return_type(
        return_type: &FunctionReturnType,
        byte: usize,
    ) -> Option<(&SourceSpan, StandardLargeObjectKind)> {
        match return_type {
            FunctionReturnType::Single(specification) => in_spec(specification, byte),
            FunctionReturnType::Stream { element, .. } => in_spec(element, byte),
            FunctionReturnType::Rows { columns, .. } => columns
                .iter()
                .find_map(|column| in_spec(&column.type_specification, byte)),
        }
    }
    fn in_statement(
        statement: &orna_syntax::ClientProceduralStatement,
        byte: usize,
    ) -> Option<(&SourceSpan, StandardLargeObjectKind)> {
        match statement {
            orna_syntax::ClientProceduralStatement::Let(statement) => statement
                .type_source
                .as_ref()
                .and_then(|source| in_source(source, byte)),
            orna_syntax::ClientProceduralStatement::If(statement) => {
                in_statements(&statement.then_statements, byte)
                    .or_else(|| {
                        statement
                            .elsif_branches
                            .iter()
                            .find_map(|branch| in_statements(&branch.statements, byte))
                    })
                    .or_else(|| {
                        statement
                            .else_statements
                            .as_deref()
                            .and_then(|statements| in_statements(statements, byte))
                    })
            }
            orna_syntax::ClientProceduralStatement::While(statement) => {
                in_statements(&statement.body, byte)
            }
            orna_syntax::ClientProceduralStatement::Assignment(_)
            | orna_syntax::ClientProceduralStatement::Return(_) => None,
        }
    }

    fn in_statements(
        statements: &[orna_syntax::ClientProceduralStatement],
        byte: usize,
    ) -> Option<(&SourceSpan, StandardLargeObjectKind)> {
        statements
            .iter()
            .find_map(|statement| in_statement(statement, byte))
    }

    for object_type in parse.object_types() {
        if let Some(found) = object_type
            .fields
            .iter()
            .find_map(|field| in_spec(&field.type_specification, byte))
        {
            return Some(found);
        }
    }
    for value_type in parse.record_value_types() {
        if let Some(found) = value_type
            .fields
            .iter()
            .find_map(|field| in_spec(&field.type_specification, byte))
        {
            return Some(found);
        }
    }
    for function in parse.server_functions() {
        if let Some(found) = function
            .parameters
            .iter()
            .find_map(|parameter| in_spec(&parameter.type_specification, byte))
            .or_else(|| in_return_type(&function.return_type, byte))
        {
            return Some(found);
        }
    }
    for function in parse.client_functions() {
        if let Some(found) = function
            .parameters
            .iter()
            .find_map(|parameter| in_spec(&parameter.type_specification, byte))
            .or_else(|| in_return_type(&function.return_type, byte))
        {
            return Some(found);
        }
        if let orna_syntax::ClientFunctionBody::StateBlock(body) = &function.body {
            if let Some(found) = body
                .states
                .iter()
                .find_map(|state| in_spec(&state.type_specification, byte))
            {
                return Some(found);
            }
            if let Some(found) = body
                .locals
                .iter()
                .find_map(|local| in_source(&local.type_source, byte))
            {
                return Some(found);
            }
            if let Some(found) = in_statements(&body.statements, byte) {
                return Some(found);
            }
        }
    }
    None
}

fn standard_large_object_reference(
    kind: StandardLargeObjectKind,
) -> Option<&'static crate::reference::ScalarReference> {
    let canonical_name = match kind {
        StandardLargeObjectKind::Character => "CHARACTER_LARGE_OBJECT",
        StandardLargeObjectKind::Binary => "BINARY_LARGE_OBJECT",
    };
    crate::reference::scalar_reference(canonical_name)
}

/// Returns the hover content for the token at one position.
pub fn hover(
    document: &Document,
    parse: &Parse,
    standard: Option<&StandardLibrary>,
    position: Position,
    mapper: &PositionMapper<'_>,
) -> Option<Hover> {
    let byte = mapper.byte_offset(position);
    let highlighted = parse.highlight();
    let (name, kind, span) = token_at(&document.text, &highlighted, byte)?;
    let doc_link = crate::hover::spec_doc_link(&document.uri);
    if let Some(declaration) = client_target_declaration(parse, &span) {
        let mut hover =
            crate::hover::declaration_hover(declaration, &document.text, doc_link.as_deref());
        hover.range = Some(mapper.range(&span));
        return Some(hover);
    }
    // A standard or external target has no same-document declaration. Do not
    // fall through to a same-spelled function from another schema.
    if client_target_function_path_at(parse, &span).is_some() {
        return None;
    }
    if let Some((type_span, large_object_kind)) = standard_large_object_at(parse, byte) {
        let reference = standard_large_object_reference(large_object_kind)?;
        let mut hover = crate::hover::scalar_hover(reference, doc_link.as_deref());
        hover.range = Some(mapper.range(type_span));
        return Some(hover);
    }
    let mut hover = match kind {
        HighlightKind::Keyword => crate::reference::keyword_reference(&name)
            .map(|reference| crate::hover::keyword_hover(reference, doc_link.as_deref())),
        _ => {
            if let Some(field) = field_at(parse, byte) {
                // A field name shadows scalar and declaration names at the
                // same spelling, for example a field named `text`.
                Some(crate::hover::field_hover(
                    &field,
                    &document.text,
                    doc_link.as_deref(),
                ))
            } else if let Some(parameter) = parameter_at(parse, byte) {
                Some(crate::hover::parameter_hover(
                    &parameter,
                    &document.text,
                    doc_link.as_deref(),
                ))
            } else if let Some((declaration, part)) = client_expression_part_in_parse(parse, &span)
            {
                match part {
                    ClientExpressionPart::FieldMember { .. } => client_field_info_at(parse, &span)
                        .map(|field| {
                            crate::hover::field_hover(&field, &document.text, doc_link.as_deref())
                        }),
                    ClientExpressionPart::ParameterRoot(root) => {
                        client_parameter_info(declaration, root, part).map(|parameter| {
                            crate::hover::parameter_hover(
                                &parameter,
                                &document.text,
                                doc_link.as_deref(),
                            )
                        })
                    }
                    ClientExpressionPart::LocalRoot(root) => {
                        client_parameter_info(declaration, root, part)
                            .map(|parameter| {
                                crate::hover::parameter_hover(
                                    &parameter,
                                    &document.text,
                                    doc_link.as_deref(),
                                )
                            })
                            .or_else(|| {
                                client_local_hover(
                                    declaration,
                                    root,
                                    &document.text,
                                    doc_link.as_deref(),
                                )
                            })
                    }
                    ClientExpressionPart::FieldRoot(root) => {
                        client_parameter_info(declaration, root, part)
                            .map(|parameter| {
                                crate::hover::parameter_hover(
                                    &parameter,
                                    &document.text,
                                    doc_link.as_deref(),
                                )
                            })
                            .or_else(|| {
                                client_local_hover(
                                    declaration,
                                    root,
                                    &document.text,
                                    doc_link.as_deref(),
                                )
                            })
                    }
                    ClientExpressionPart::TargetFunction { .. }
                    | ClientExpressionPart::CallArgumentLabel => None,
                }
            } else if let Some(field) = sql_column_at(parse, byte, &document.text, &highlighted) {
                // A column reference inside a SQL body resolves to the field
                // of the body's target object type.
                Some(crate::hover::field_hover(
                    &field,
                    &document.text,
                    doc_link.as_deref(),
                ))
            } else if let Some(declaration) =
                declaration_at_span(parse, &document.text, &highlighted, &name, kind, &span)
            {
                Some(crate::hover::declaration_hover(
                    declaration,
                    &document.text,
                    doc_link.as_deref(),
                ))
            } else {
                crate::reference::scalar_reference(&name)
                    .map(|reference| crate::hover::scalar_hover(reference, doc_link.as_deref()))
                    .or_else(|| {
                        standard.and_then(|standard| {
                            standard_value_hover(standard, &name, doc_link.as_deref())
                        })
                    })
            }
        }
    };
    if let Some(hover) = hover.as_mut() {
        hover.range = Some(mapper.range(&span));
    }
    hover
}

/// Builds the hover for one standard-library type or schema name.
fn standard_value_hover(
    standard: &StandardLibrary,
    name: &str,
    doc_link: Option<&str>,
) -> Option<lsp_types::Hover> {
    for value_type in standard.checked.value_types() {
        let parts = value_type.name().parts();
        if parts
            .last()
            .is_some_and(|part| identifier_spelling_matches(part, name))
        {
            let kind = match value_type.kind() {
                orna_core::catalogue::ValueTypeKind::Primitive => "primitive",
                orna_core::catalogue::ValueTypeKind::Opaque => "opaque",
                _ => "value",
            };
            return Some(crate::hover::standard_type_hover(
                parts.last().expect("nonempty name"),
                kind,
                value_type.representation_contract(),
                doc_link,
            ));
        }
    }
    for schema in standard.checked.schemas() {
        let parts = schema.name().parts();
        if parts
            .last()
            .is_some_and(|part| identifier_spelling_matches(part, name))
        {
            return Some(crate::hover::standard_schema_hover(
                &parts.join("."),
                doc_link,
            ));
        }
    }
    None
}

/// Returns the declaration location for the identifier at one position.
pub fn definition(
    document: &Document,
    parse: &Parse,
    position: Position,
    mapper: &PositionMapper<'_>,
) -> Option<Location> {
    let byte = mapper.byte_offset(position);
    let highlighted = parse.highlight();
    let (name, kind, selected_span) = token_at(&document.text, &highlighted, byte)?;
    if kind == HighlightKind::Keyword {
        return None;
    }
    if let Some(span) = client_target_declaration_span(parse, &selected_span) {
        return Some(Location {
            uri: document.uri.clone(),
            range: mapper.range(&span),
        });
    }
    declaration_span_for_kind(
        parse,
        &document.text,
        &highlighted,
        &name,
        kind,
        &selected_span,
    )
    .map(|span| Location {
        uri: document.uri.clone(),
        range: mapper.range(&span),
    })
}

/// Returns every occurrence of the identifier at one position.
///
/// When `include_declaration` is false, the matching declaration is omitted.
pub fn references(
    document: &Document,
    parse: &Parse,
    position: Position,
    mapper: &PositionMapper<'_>,
    include_declaration: bool,
) -> Vec<Location> {
    let byte = mapper.byte_offset(position);
    let highlighted = parse.highlight();
    let Some((name, kind, selected_span)) = token_at(&document.text, &highlighted, byte) else {
        return Vec::new();
    };
    if kind == HighlightKind::Keyword {
        return Vec::new();
    }
    let scope = reference_scope(
        parse,
        &document.text,
        &highlighted,
        &name,
        kind,
        &selected_span,
    );
    let declaration_span = if include_declaration {
        None
    } else if is_declaration_span(parse, &selected_span) {
        Some(selected_span)
    } else {
        declaration_span_for_kind(
            parse,
            &document.text,
            &highlighted,
            &name,
            kind,
            &selected_span,
        )
    };
    highlighted
        .iter()
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
        .filter(|token| {
            reference_token_in_scope(parse, &document.text, &highlighted, token, &scope)
        })
        .filter(|token| identifier_spelling_matches(&document.text[token.range.clone()], &name))
        .filter(|token| {
            declaration_span
                .as_ref()
                .is_none_or(|span| span.start != token.range.start || span.end != token.range.end)
        })
        .map(|token| Location {
            uri: document.uri.clone(),
            range: mapper.range(&SourceSpan {
                start: token.range.start,
                end: token.range.end,
            }),
        })
        .collect()
}

/// Returns signature help for the callable declaration at a source position.
pub fn signature_help(
    document: &Document,
    parse: &Parse,
    position: Position,
    mapper: &PositionMapper<'_>,
) -> Option<lsp_types::SignatureHelp> {
    let byte = mapper.byte_offset(position);
    let highlighted = parse.highlight();
    let mut open = None;
    let mut depth = 0usize;
    for (index, character) in document.text[..byte].char_indices().rev() {
        match character {
            ')' => depth += 1,
            '(' if depth == 0 => {
                open = Some(index);
                break;
            }
            '(' => depth -= 1,
            _ => {}
        }
    }
    let open = open?;
    let function_byte = document.text[..open]
        .char_indices()
        .rev()
        .find(|(_, character)| !character.is_whitespace())
        .map(|(index, _)| index)?;
    let token = highlighted
        .iter()
        .find(|token| token.range.start <= function_byte && function_byte < token.range.end)?;
    if token.kind != HighlightKind::FunctionName {
        return None;
    }
    let name = &document.text[token.range.clone()];
    let declaration = declaration_at(parse, name)?;
    let (parameters, return_type, label) = match declaration {
        DeclarationRef::ServerFunction(function) => (
            function.parameters.as_slice(),
            return_text(&function.return_type, &document.text),
            format!("SERVER FUNCTION {}", qualified_name_text(&function.name)),
        ),
        DeclarationRef::ClientFunction(function) => (
            function.parameters.as_slice(),
            return_text(&function.return_type, &document.text),
            format!("CLIENT FUNCTION {}", qualified_name_text(&function.name)),
        ),
        _ => return None,
    };
    let active_parameter = document.text[open + 1..byte]
        .chars()
        .fold(
            (0usize, 0usize),
            |(depth, commas), character| match character {
                '(' => (depth + 1, commas),
                ')' if depth > 0 => (depth - 1, commas),
                ',' if depth == 0 => (depth, commas + 1),
                _ => (depth, commas),
            },
        )
        .1;
    let parameter_labels = parameters
        .iter()
        .map(|parameter| parameter.name.text.clone())
        .collect::<Vec<_>>();
    Some(lsp_types::SignatureHelp {
        signatures: vec![lsp_types::SignatureInformation {
            label: format!(
                "{label}({}) RETURNS {return_type}",
                parameter_labels.join(", ")
            ),
            documentation: None,
            parameters: Some(
                parameters
                    .iter()
                    .map(|parameter| lsp_types::ParameterInformation {
                        label: lsp_types::ParameterLabel::Simple(parameter.name.text.clone()),
                        documentation: parameter.documentation.as_ref().map(|documentation| {
                            lsp_types::Documentation::String(
                                documentation_text(Some(documentation))
                                    .unwrap_or_default()
                                    .to_owned(),
                            )
                        }),
                    })
                    .collect(),
            ),
            active_parameter: Some(active_parameter as u32),
        }],
        active_signature: Some(0),
        active_parameter: Some(active_parameter as u32),
    })
}
fn return_text(return_type: &FunctionReturnType, text: &str) -> String {
    match return_type {
        FunctionReturnType::Value(type_specification) => text
            .get(type_specification.span().range())
            .unwrap_or("value")
            .to_owned(),
        FunctionReturnType::Rows { columns, .. } => format!(
            "ROWS ({})",
            columns
                .iter()
                .map(|column| column.name.text.clone())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn documentation_text(slice: Option<&SourceSlice>) -> Option<&str> {
    slice.map(SourceSlice::as_str)
}
#[allow(dead_code)]
pub fn completion(parse: &Parse, standard: Option<&StandardLibrary>) -> Vec<CompletionItem> {
    completion_at(parse, standard, None, None)
}

/// Returns global completion items plus fields for an accepted CLIENT path at
/// the requested source byte.
///
/// The parser retains complete dotted paths, so completion after a dot can
/// inspect the member already present in the accepted source without adding
/// proposal-only syntax to the language grammar.
pub fn completion_at(
    parse: &Parse,
    standard: Option<&StandardLibrary>,
    byte: Option<usize>,
    context: Option<&CompletionContext>,
) -> Vec<CompletionItem> {
    let target_completion = byte
        .filter(|_| member_completion_context_allows(context))
        .and_then(|byte| client_target_completion_at_byte(parse, byte));
    let mut items = Vec::new();
    if let Some(standard) = standard {
        for value_type in standard.checked.value_types() {
            let name = value_type.name();
            if let Some(last) = name.parts().last() {
                items.push(CompletionItem {
                    label: last.clone(),
                    kind: Some(CompletionItemKind::STRUCT),
                    detail: Some(format!("standard type {name}")),
                    documentation: Some(lsp_types::Documentation::String(format!(
                        "Standard-library value type `{name}`."
                    ))),
                    sort_text: Some(format!("0-{last}")),
                    ..CompletionItem::default()
                });
            }
        }
    }
    for keyword in orna_syntax::KEYWORDS {
        items.push(CompletionItem {
            label: (*keyword).to_owned(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("language keyword".to_owned()),
            documentation: crate::reference::keyword_reference(keyword)
                .map(|reference| lsp_types::Documentation::String(reference.summary.to_owned())),
            sort_text: Some(format!("2-{keyword}")),
            ..CompletionItem::default()
        });
    }
    for scalar in orna_syntax::SCALAR_TYPES {
        items.push(CompletionItem {
            label: (*scalar).to_owned(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("standard scalar type".to_owned()),
            documentation: crate::reference::scalar_reference(scalar)
                .map(|reference| lsp_types::Documentation::String(reference.summary.to_owned())),
            sort_text: Some(format!("1-{scalar}")),
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
        let detail = if target_completion.is_some_and(|constructor| {
            server_target_is_eligible(declaration, constructor, standard)
        }) {
            "server function target"
        } else {
            "server function"
        }
        .to_owned();
        add_named(
            last_name(&declaration.name),
            CompletionItemKind::FUNCTION,
            detail,
        );
    }
    for declaration in parse.client_functions() {
        let detail = if target_completion.is_some_and(|constructor| {
            client_target_is_eligible(declaration, constructor, standard)
        }) {
            "client function target"
        } else {
            "client function"
        }
        .to_owned();
        add_named(
            last_name(&declaration.name),
            CompletionItemKind::FUNCTION,
            detail,
        );
    }
    if let Some(byte) = byte
        && member_completion_context_allows(context)
    {
        add_client_member_completions(parse, byte, &mut items);
    }
    items
}

fn client_target_completion_at_byte(parse: &Parse, byte: usize) -> Option<ClientTargetConstructor> {
    parse.client_functions().iter().find_map(|declaration| {
        client_field_path_at_byte(declaration, byte).and_then(|(_, members, index)| {
            client_target_function_path_at(parse, &members[index].span).map(|path| path.constructor)
        })
    })
}

fn server_target_is_eligible(
    declaration: &ServerFunctionDeclaration,
    constructor: ClientTargetConstructor,
    standard: Option<&StandardLibrary>,
) -> bool {
    match constructor {
        ClientTargetConstructor::Resource => {
            matches!(&declaration.return_type, FunctionReturnType::Single(_))
        }
        ClientTargetConstructor::Action => {
            action_target_return_type_is_durable(&declaration.return_type, standard)
        }
        ClientTargetConstructor::StreamResource => {
            matches!(&declaration.return_type, FunctionReturnType::Stream { .. })
        }
    }
}

fn client_target_is_eligible(
    declaration: &ClientFunctionDeclaration,
    constructor: ClientTargetConstructor,
    standard: Option<&StandardLibrary>,
) -> bool {
    matches!(constructor, ClientTargetConstructor::Action)
        && action_target_return_type_is_durable(&declaration.return_type, standard)
}

fn action_target_return_type_is_durable(
    return_type: &FunctionReturnType,
    standard: Option<&StandardLibrary>,
) -> bool {
    let FunctionReturnType::Single(type_specification) = return_type else {
        return false;
    };
    action_target_type_is_durable(type_specification, standard)
}

fn action_target_type_is_durable(
    type_specification: &TypeSpecification,
    standard: Option<&StandardLibrary>,
) -> bool {
    match type_specification {
        TypeSpecification::Reference { .. } => true,
        TypeSpecification::StandardLargeObject { kind, .. } => matches!(
            kind,
            StandardLargeObjectKind::Character | StandardLargeObjectKind::Binary
        ),
        TypeSpecification::Named(name) => {
            let prelude_scalar = name.parts.len() == 1
                && ["BOOL", "BOOLEAN", "INT", "INTEGER", "BIGINT", "FLOAT"]
                    .iter()
                    .any(|scalar| identifier_spelling_matches(&name.parts[0].text, scalar));
            let standard_scalar_alias = name.parts.len() == 2
                && identifier_spelling_matches(&name.parts[0].text, "std")
                && [
                    "BOOLEAN",
                    "INTEGER",
                    "BIGINT",
                    "FLOAT",
                    "CHARACTER_LARGE_OBJECT",
                    "BINARY_LARGE_OBJECT",
                ]
                .iter()
                .any(|scalar| identifier_spelling_matches(&name.parts[1].text, scalar));
            let standard_action_alias = (name.parts.len() == 2
                && identifier_spelling_matches(&name.parts[0].text, "std")
                && identifier_spelling_matches(&name.parts[1].text, "Action"))
                || (name.parts.len() == 3
                    && identifier_spelling_matches(&name.parts[0].text, "std")
                    && identifier_spelling_matches(&name.parts[1].text, "action")
                    && identifier_spelling_matches(&name.parts[2].text, "Action"));
            prelude_scalar
                || standard_scalar_alias
                || standard_action_alias
                || standard.is_some_and(|standard| {
                    standard.checked.value_types().iter().any(|value_type| {
                        let standard_action_type = value_type.name().parts().len() == 3
                            && identifier_spelling_matches(&value_type.name().parts()[0], "std")
                            && identifier_spelling_matches(&value_type.name().parts()[1], "action")
                            && identifier_spelling_matches(&value_type.name().parts()[2], "Action");
                        (value_type.persistence() == ValueTypePersistence::Persistable
                            || standard_action_type)
                            && value_type.name().parts().len() == name.parts.len()
                            && value_type.name().parts().iter().zip(&name.parts).all(
                                |(candidate, source)| {
                                    identifier_spelling_matches(candidate, &source.text)
                                },
                            )
                    })
                })
        }
        TypeSpecification::List { .. }
        | TypeSpecification::Set { .. }
        | TypeSpecification::Map { .. }
        | TypeSpecification::Option { .. }
        | TypeSpecification::Stream { .. } => false,
    }
}

fn member_completion_context_allows(context: Option<&CompletionContext>) -> bool {
    let Some(context) = context else {
        return true;
    };
    if context.trigger_kind == CompletionTriggerKind::INVOKED
        || context.trigger_kind == CompletionTriggerKind::TRIGGER_FOR_INCOMPLETE_COMPLETIONS
    {
        return true;
    }
    context.trigger_kind == CompletionTriggerKind::TRIGGER_CHARACTER
        && context.trigger_character.as_deref() == Some(".")
}

fn add_client_member_completions(parse: &Parse, byte: usize, items: &mut Vec<CompletionItem>) {
    let Some((declaration, root, members, index)) =
        parse.client_functions().iter().find_map(|declaration| {
            client_field_path_at_byte(declaration, byte)
                .map(|(root, members, index)| (declaration, root, members, index))
        })
    else {
        return;
    };
    let Some(mut owner) =
        client_root_binding(declaration, root, ClientExpressionPart::FieldRoot(root))
            .and_then(|binding| binding.owner)
    else {
        return;
    };
    for member in members.iter().take(index) {
        let Some(field) = field_on_object_or_record(parse, &owner, &member.text) else {
            return;
        };
        let Some(next_owner) = type_owner_name(field.type_specification) else {
            return;
        };
        owner = next_owner;
    }
    let fields = parse
        .object_types()
        .iter()
        .find(|declaration| qualified_names_match(&declaration.name, &owner))
        .map(|declaration| {
            declaration
                .fields
                .iter()
                .map(|field| (&field.name, "object"))
        })
        .into_iter()
        .flatten()
        .chain(
            parse
                .record_value_types()
                .iter()
                .find(|declaration| qualified_names_match(&declaration.name, &owner))
                .map(|declaration| {
                    declaration
                        .fields
                        .iter()
                        .map(|field| (&field.name, "record"))
                })
                .into_iter()
                .flatten(),
        );
    for (field, kind) in fields {
        items.push(CompletionItem {
            label: field.text.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(format!("{kind} field of {}", qualified_name_text(&owner))),
            ..CompletionItem::default()
        });
    }
}

fn client_field_path_at_byte(
    declaration: &ClientFunctionDeclaration,
    byte: usize,
) -> Option<(&orna_syntax::NamePart, &[orna_syntax::NamePart], usize)> {
    fn expression_at_byte(
        expression: &ClientExpression,
        byte: usize,
    ) -> Option<(&orna_syntax::NamePart, &[orna_syntax::NamePart], usize)> {
        match expression {
            ClientExpression::FieldPath { root, members, .. } => members
                .iter()
                .enumerate()
                .find(|(index, member)| {
                    let previous_end = if *index == 0 {
                        root.span.end
                    } else {
                        members[index - 1].span.end
                    };
                    byte >= previous_end && byte <= member.span.end
                })
                .map(|(index, _)| (root, members.as_slice(), index)),
            ClientExpression::Call { arguments, .. } => arguments
                .iter()
                .find_map(|argument| expression_at_byte(&argument.value, byte)),
            ClientExpression::Await { expression, .. } => expression_at_byte(expression, byte),
            ClientExpression::Concat { left, right, .. } => {
                expression_at_byte(left, byte).or_else(|| expression_at_byte(right, byte))
            }
            ClientExpression::Unary(unary) => expression_at_byte(&unary.expression, byte),
            ClientExpression::Binary(binary) => expression_at_byte(&binary.left, byte)
                .or_else(|| expression_at_byte(&binary.right, byte)),
            ClientExpression::Parenthesized { expression, .. } => {
                expression_at_byte(expression, byte)
            }
            ClientExpression::ParameterRead { .. }
            | ClientExpression::LocalRead { .. }
            | ClientExpression::StringLiteral { .. }
            | ClientExpression::IntegerLiteral { .. }
            | ClientExpression::BooleanLiteral { .. } => None,
        }
    }
    fn statements_at_byte(
        statements: &[orna_syntax::ClientProceduralStatement],
        byte: usize,
    ) -> Option<(&orna_syntax::NamePart, &[orna_syntax::NamePart], usize)> {
        statements
            .iter()
            .find_map(|statement| statement_at_byte(statement, byte))
    }

    fn statement_at_byte(
        statement: &orna_syntax::ClientProceduralStatement,
        byte: usize,
    ) -> Option<(&orna_syntax::NamePart, &[orna_syntax::NamePart], usize)> {
        match statement {
            orna_syntax::ClientProceduralStatement::Let(statement) => {
                expression_at_byte(&statement.expression, byte)
            }
            orna_syntax::ClientProceduralStatement::Assignment(statement) => {
                expression_at_byte(&statement.expression, byte)
            }
            orna_syntax::ClientProceduralStatement::Return(statement) => statement
                .expression
                .as_ref()
                .and_then(|expression| expression_at_byte(expression, byte)),
            orna_syntax::ClientProceduralStatement::If(statement) => {
                expression_at_byte(&statement.condition, byte)
                    .or_else(|| statements_at_byte(&statement.then_statements, byte))
                    .or_else(|| {
                        statement.elsif_branches.iter().find_map(|branch| {
                            expression_at_byte(&branch.condition, byte)
                                .or_else(|| statements_at_byte(&branch.statements, byte))
                        })
                    })
                    .or_else(|| {
                        statement
                            .else_statements
                            .as_deref()
                            .and_then(|statements| statements_at_byte(statements, byte))
                    })
            }
            orna_syntax::ClientProceduralStatement::While(statement) => {
                expression_at_byte(&statement.condition, byte)
                    .or_else(|| statements_at_byte(&statement.body, byte))
            }
        }
    }

    match &declaration.body {
        orna_syntax::ClientFunctionBody::Expression { expression }
        | orna_syntax::ClientFunctionBody::ReturnExpression { expression } => {
            expression_at_byte(expression, byte)
        }
        orna_syntax::ClientFunctionBody::StateBlock(block) => block
            .states
            .iter()
            .find_map(|state| match &state.default {
                orna_syntax::StateDefault::Expression(expression) => {
                    expression_at_byte(expression, byte)
                }
                orna_syntax::StateDefault::Unset | orna_syntax::StateDefault::Null => None,
            })
            .or_else(|| {
                block
                    .locals
                    .iter()
                    .find_map(|local| expression_at_byte(&local.expression, byte))
            })
            .or_else(|| statements_at_byte(&block.statements, byte))
            .or_else(|| {
                block
                    .return_expression
                    .as_ref()
                    .and_then(|expression| expression_at_byte(expression, byte))
            }),
        orna_syntax::ClientFunctionBody::BooleanLiteral { .. }
        | orna_syntax::ClientFunctionBody::ExternalContract { .. } => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        StandardLibrary, completion, declaration_at, hover, references, type_owner_name_from_source,
    };
    use crate::documents::{Document, PositionMapper};
    use lsp_types::{Hover, HoverContents, Position, Range};

    fn hover_at(text: &str, byte: usize) -> Option<Hover> {
        let document = Document::new("file:///hover.orna".parse().unwrap(), text.to_owned(), 1);
        let parse = orna_syntax::parse(text);
        let mapper = PositionMapper::new(text);
        hover(&document, &parse, None, mapper.position(byte), &mapper)
    }

    fn hover_markdown(hover: &Hover) -> &str {
        match &hover.contents {
            HoverContents::Markup(markup) => &markup.value,
            other => panic!("expected markdown hover, got {other:?}"),
        }
    }

    #[test]
    fn standard_library_loads_verified_v10_snapshot() {
        let standard = StandardLibrary::load().expect("retained V10 standard must load");
        let snapshot = standard.checked.verified_snapshot();

        assert_eq!(
            snapshot.revision(),
            orna_standard::STANDARD_LIBRARY_V9_REVISION_ID
        );
        assert_eq!(
            snapshot.source().id(),
            orna_standard::STANDARD_SOURCE_V10_REVISION_ID
        );
        assert!(snapshot.source().units().iter().any(|unit| {
            unit.logical_path() == orna_standard::STD_UI_CONSTRUCTORS_SOURCE_LOGICAL_PATH
        }));
    }

    #[test]
    fn completion_includes_canonical_scalar_type_spellings() {
        let parse = orna_syntax::parse("");
        let labels: Vec<_> = completion(&parse, None)
            .into_iter()
            .map(|item| item.label)
            .collect();

        for expected in [
            "BOOLEAN",
            "INTEGER",
            "CHARACTER LARGE OBJECT",
            "BINARY LARGE OBJECT",
        ] {
            assert!(
                labels.iter().any(|label| label == expected),
                "missing {expected}"
            );
        }
    }

    #[test]
    fn hover_multiword_scalars_cover_the_complete_type_span() {
        let text = "CREATE TYPE files.document AS OBJECT (body CHARACTER LARGE OBJECT, data BINARY LARGE OBJECT);";
        let mapper = PositionMapper::new(text);
        for (spelling, canonical) in [
            ("CHARACTER LARGE OBJECT", "CHARACTER_LARGE_OBJECT"),
            ("BINARY LARGE OBJECT", "BINARY_LARGE_OBJECT"),
        ] {
            let start = text.find(spelling).expect("scalar spelling");
            let end = start + spelling.len();
            for byte in [start, start + "LARGE".len(), end - 1] {
                let result = hover_at(text, byte).expect("scalar hover");
                assert_eq!(
                    result.range,
                    Some(mapper.range(&orna_syntax::SourceSpan { start, end })),
                    "hover range for {spelling} at byte {byte}",
                );
                assert!(
                    hover_markdown(&result).contains(canonical)
                        && hover_markdown(&result).contains("standard type"),
                    "hover content for {spelling}: {}",
                    hover_markdown(&result),
                );
            }
        }
    }

    #[test]
    fn hover_multiword_scalars_respect_utf16_positions_and_context() {
        let text = "CREATE TYPE files.document AS OBJECT (\"😀body\" CHARACTER LARGE OBJECT, data BINARY LARGE OBJECT);";
        let mapper = PositionMapper::new(text);
        let character_start = text
            .find("CHARACTER LARGE OBJECT")
            .expect("character scalar");
        let character_end = character_start + "CHARACTER LARGE OBJECT".len();
        let start_position = mapper.position(character_start);
        assert_eq!(
            start_position.character as usize,
            text[..character_start].encode_utf16().count(),
            "scalar start uses UTF-16 code units",
        );
        let scalar_hover = hover_at(text, character_start).expect("scalar hover");
        assert_eq!(
            scalar_hover.range,
            Some(Range {
                start: Position {
                    line: 0,
                    character: 47,
                },
                end: Position {
                    line: 0,
                    character: 69,
                },
            }),
            "hover range uses UTF-16 units after the quoted emoji name",
        );
        assert!(hover_at(text, character_end - 1).is_some());
        assert!(hover_at(text, character_end).is_none());

        let generic_text = "CREATE TYPE files.document AS OBJECT (LARGE BOOLEAN, value OBJECT);";
        for word in ["LARGE", "OBJECT"] {
            let byte = generic_text.rfind(word).expect("generic word");
            if let Some(result) = hover_at(generic_text, byte) {
                assert!(
                    !hover_markdown(&result).contains("standard type"),
                    "generic word {word} incorrectly resolved as scalar: {}",
                    hover_markdown(&result),
                );
            }
        }
    }

    #[test]
    fn hover_client_local_type_sources_cover_complete_multiword_ranges() {
        let text = concat!(
            "CREATE CLIENT FUNCTION files.document() RETURNS TEXT IS\n",
            "    LET body CHARACTER LARGE OBJECT := \x27body\x27;\n",
            "BEGIN\n",
            "    LET data BINARY LARGE OBJECT := body;\n",
            "    RETURN body;\n",
            "END;",
        );
        for (spelling, canonical) in [
            ("CHARACTER LARGE OBJECT", "CHARACTER_LARGE_OBJECT"),
            ("BINARY LARGE OBJECT", "BINARY_LARGE_OBJECT"),
        ] {
            let mapper = PositionMapper::new(text);
            let start = text.find(spelling).expect("local scalar spelling");
            let end = start + spelling.len();
            for byte in [start, start + "LARGE".len(), end - 1] {
                let result = hover_at(text, byte).expect("local scalar hover");
                assert_eq!(
                    result.range,
                    Some(mapper.range(&orna_syntax::SourceSpan { start, end })),
                    "hover range for {spelling} at byte {byte}",
                );
                assert!(
                    hover_markdown(&result).contains(canonical)
                        && hover_markdown(&result).contains("standard type"),
                    "hover content for {spelling}: {}",
                    hover_markdown(&result),
                );
            }
        }
    }

    #[test]
    fn hover_client_procedural_local_use_resolves_type() {
        let text = concat!(
            "CREATE CLIENT FUNCTION files.document() RETURNS BOOLEAN IS\n",
            "    LET body BOOLEAN := TRUE;\n",
            "BEGIN\n",
            "    RETURN body;\n",
            "END;",
        );
        let byte = text.rfind("body").expect("local use");
        let result = hover_at(text, byte).expect("procedural local hover");
        let markdown = hover_markdown(&result);
        assert!(
            markdown.starts_with("**parameter**"),
            "local hover kind: {markdown}"
        );
        assert!(markdown.contains("BOOLEAN"), "local hover type: {markdown}");
    }

    #[test]
    fn hover_client_local_type_sources_reject_comment_separators() {
        let text = concat!(
            "CREATE CLIENT FUNCTION files.document() RETURNS TEXT IS\n",
            "    LET body CHARACTER /* kept */ LARGE OBJECT := \x27body\x27;\n",
            "    LET data BINARY /* kept */ LARGE OBJECT := body;\n",
            "    LET invalid CHARACTERLARGEOBJECT := body;\n",
            "BEGIN\n",
            "    RETURN body;\n",
            "END;",
        );

        for (spelling, canonical) in [
            (
                "CHARACTER /* kept */ LARGE OBJECT",
                "CHARACTER_LARGE_OBJECT",
            ),
            ("BINARY /* kept */ LARGE OBJECT", "BINARY_LARGE_OBJECT"),
        ] {
            let start = text.find(spelling).expect("commented scalar spelling");
            let end = start + spelling.len();
            let words = [
                spelling
                    .split_ascii_whitespace()
                    .next()
                    .expect("first scalar word"),
                "LARGE",
                "OBJECT",
            ];
            for word in words {
                let byte = text[start..end]
                    .find(word)
                    .map(|offset| start + offset)
                    .expect("commented scalar word");
                let result = hover_at(text, byte);
                let has_standard_hover = result.as_ref().is_some_and(|hover| {
                    hover_markdown(hover).contains(canonical)
                        && hover_markdown(hover).contains("standard type")
                });
                let description = result
                    .as_ref()
                    .map(|hover| hover_markdown(hover).to_owned());
                assert!(
                    !has_standard_hover,
                    "commented local must not acquire standard scalar hover for {spelling}: {description:?}",
                );
            }
        }

        let invalid = text
            .find("CHARACTERLARGEOBJECT")
            .expect("invalid scalar spelling");
        assert!(
            !hover_at(text, invalid)
                .is_some_and(|hover| { hover_markdown(&hover).contains("CHARACTER_LARGE_OBJECT") })
        );
    }

    #[test]
    fn quoted_local_type_owner_allows_comment_markers_inside_identifier() {
        let owner = type_owner_name_from_source("REF owners.\"foo--bar\"")
            .expect("quoted owner type source");
        assert_eq!(
            owner.parts.last().map(|part| part.text.as_str()),
            Some("\"foo--bar\"")
        );
    }

    #[test]
    fn hover_client_local_initializers_and_assignments_do_not_resolve_as_scalars() {
        let text = concat!(
            "CREATE CLIENT FUNCTION files.document() RETURNS TEXT IS\n",
            "    LET body CHARACTER LARGE OBJECT := std.large.object();\n",
            "BEGIN\n",
            "    LET data BINARY LARGE OBJECT := body;\n",
            "    data := std.binary.large.object();\n",
            "    RETURN body;\n",
            "END;",
        );

        for occurrence in ["std.large.object", "std.binary.large.object"] {
            let start = text.find(occurrence).expect("non-type occurrence");
            for word in occurrence.split(".") {
                let byte = text[start..]
                    .find(word)
                    .map(|offset| start + offset)
                    .expect("occurrence word");
                let result = hover_at(text, byte);
                assert!(!result.is_some_and(|hover| {
                    hover_markdown(&hover).contains("CHARACTER_LARGE_OBJECT")
                        || hover_markdown(&hover).contains("BINARY_LARGE_OBJECT")
                }));
            }
        }
    }

    #[test]
    fn declaration_lookup_folds_unquoted_identifier_case_but_preserves_quotes() {
        let parse = orna_syntax::parse("CREATE SCHEMA foo;");

        assert!(declaration_at(&parse, "foo").is_some());
        assert!(declaration_at(&parse, "Foo").is_some());

        let quoted = orna_syntax::parse("CREATE SCHEMA \"Foo\";");
        assert!(declaration_at(&quoted, "\"Foo\"").is_some());
        assert!(declaration_at(&quoted, "\"foo\"").is_none());
        assert!(declaration_at(&quoted, "foo").is_none());
    }

    #[test]
    fn references_fold_unquoted_case_and_exclude_qualified_declaration_component() {
        let text = concat!(
            "CREATE SCHEMA foo;\n",
            "CREATE TYPE foo.bar AS OBJECT (value BOOLEAN);\n",
            "CREATE SERVER FUNCTION baz() RETURNS BOOLEAN AS SELECT Foo;\n",
        );
        let document = Document::new("file:///test.orna".parse().unwrap(), text.to_owned(), 1);
        let parse = orna_syntax::parse(text);
        let mapper = PositionMapper::new(text);

        let foo_references = references(&document, &parse, Position::new(0, 14), &mapper, true);
        assert_eq!(foo_references.len(), 2);
        assert_eq!(foo_references[0].range.start, Position::new(0, 14));
        assert_eq!(foo_references[1].range.start, Position::new(1, 12));
        let without_declaration =
            references(&document, &parse, Position::new(0, 14), &mapper, false);
        assert_eq!(without_declaration.len(), 1);
        assert_eq!(without_declaration[0].range.start, Position::new(1, 12));
        let unqualified = references(&document, &parse, Position::new(2, 55), &mapper, true);
        assert!(
            unqualified.is_empty(),
            "unqualified variable must not resolve as a schema: {unqualified:?}"
        );

        let qualified_text = "CREATE SCHEMA product_test;\nCREATE TYPE product_test.probe AS OBJECT (value BOOLEAN);\n";
        let qualified_document = Document::new(
            "file:///qualified.orna".parse().unwrap(),
            qualified_text.to_owned(),
            1,
        );
        let qualified_parse = orna_syntax::parse(qualified_text);
        let qualified_mapper = PositionMapper::new(qualified_text);
        let probe_without_declaration = references(
            &qualified_document,
            &qualified_parse,
            Position::new(1, 25),
            &qualified_mapper,
            false,
        );
        assert!(probe_without_declaration.is_empty());
        let namespace_without_declaration = references(
            &qualified_document,
            &qualified_parse,
            Position::new(1, 12),
            &qualified_mapper,
            false,
        );
        assert_eq!(namespace_without_declaration.len(), 1);
        assert_eq!(
            namespace_without_declaration[0].range.start,
            Position::new(1, 12)
        );
    }

    #[test]
    fn references_exclude_field_and_parameter_declarations() {
        let field_text = "CREATE SCHEMA people;\n\
              CREATE TYPE people.person AS OBJECT (stored BOOLEAN);\n\
              CREATE SERVER FUNCTION read_value() RETURNS BOOLEAN AS \
              SELECT probe.stored FROM people.person probe;\n";
        let field_document = Document::new(
            "file:///field.orna".parse().unwrap(),
            field_text.to_owned(),
            1,
        );
        let field_parse = orna_syntax::parse(field_text);
        let field_mapper = PositionMapper::new(field_text);
        let field_declaration = field_text
            .find("stored BOOLEAN")
            .expect("field declaration");
        let field_use = field_text.find("probe.stored").expect("field use") + "probe.".len();
        let field_references = references(
            &field_document,
            &field_parse,
            field_mapper.position(field_declaration),
            &field_mapper,
            false,
        );
        assert_eq!(field_references.len(), 1);
        assert_eq!(
            field_references[0].range.start,
            field_mapper.position(field_use)
        );
        let field_use_references = references(
            &field_document,
            &field_parse,
            field_mapper.position(field_use),
            &field_mapper,
            false,
        );
        assert_eq!(field_use_references.len(), 1);
        assert_eq!(
            field_use_references[0].range.start,
            field_mapper.position(field_use)
        );

        let parameter_text =
            "CREATE SERVER FUNCTION read_value(stored BOOLEAN) RETURNS BOOLEAN AS SELECT stored;\n";
        let parameter_document = Document::new(
            "file:///parameter.orna".parse().unwrap(),
            parameter_text.to_owned(),
            1,
        );
        let parameter_parse = orna_syntax::parse(parameter_text);
        let parameter_mapper = PositionMapper::new(parameter_text);
        let parameter_declaration = parameter_text
            .find("stored BOOLEAN")
            .expect("parameter declaration");
        let parameter_use =
            parameter_text.find("SELECT stored").expect("parameter use") + "SELECT ".len();
        let parameter_references = references(
            &parameter_document,
            &parameter_parse,
            parameter_mapper.position(parameter_declaration),
            &parameter_mapper,
            false,
        );
        assert_eq!(parameter_references.len(), 1);
        assert_eq!(
            parameter_references[0].range.start,
            parameter_mapper.position(parameter_use)
        );
        let parameter_use_references = references(
            &parameter_document,
            &parameter_parse,
            parameter_mapper.position(parameter_use),
            &parameter_mapper,
            false,
        );
        assert_eq!(parameter_use_references.len(), 1);
        assert_eq!(
            parameter_use_references[0].range.start,
            parameter_mapper.position(parameter_use)
        );
    }

    #[test]
    fn definitions_scope_rows_columns_before_unrelated_fields() {
        let text = concat!(
            "CREATE SCHEMA people;\n",
            "CREATE SCHEMA other;\n",
            "CREATE TYPE people.person AS OBJECT (stored BOOLEAN);\n",
            "CREATE TYPE other.person AS OBJECT (stored BOOLEAN);\n",
            "CREATE SERVER FUNCTION read_stored() RETURNS ROWS (stored BOOLEAN) AS\n",
            "SELECT probe.stored FROM other.person probe;\n",
        );
        let document = Document::new("file:///rows.orna".parse().unwrap(), text.to_owned(), 1);
        let parse = orna_syntax::parse(text);
        let mapper = PositionMapper::new(text);
        let field_declaration =
            text.rfind("OBJECT (stored").expect("object field") + "OBJECT (".len();
        let return_declaration = text.find("ROWS (stored").expect("return column") + "ROWS (".len();
        let field_use = text.find("probe.stored").expect("field use") + "probe.".len();

        let return_definition = super::definition(
            &document,
            &parse,
            mapper.position(return_declaration),
            &mapper,
        )
        .expect("return column definition");
        assert_eq!(
            return_definition.range.start,
            mapper.position(return_declaration)
        );

        let field_definition =
            super::definition(&document, &parse, mapper.position(field_use), &mapper)
                .expect("object field definition");
        assert_eq!(
            field_definition.range.start,
            mapper.position(field_declaration)
        );

        let return_references = references(
            &document,
            &parse,
            mapper.position(return_declaration),
            &mapper,
            false,
        );
        assert!(
            return_references.is_empty(),
            "object field references leaked into ROWS column: {return_references:?}"
        );

        let field_references = references(
            &document,
            &parse,
            mapper.position(field_declaration),
            &mapper,
            false,
        );
        assert_eq!(field_references.len(), 1);
        assert_eq!(field_references[0].range.start, mapper.position(field_use));
    }

    #[test]
    fn variable_definitions_and_references_stay_within_the_containing_function() {
        let text = "CREATE SERVER FUNCTION first(stored BOOLEAN) RETURNS BOOLEAN AS SELECT stored;
\
              CREATE SERVER FUNCTION second(stored BOOLEAN) RETURNS BOOLEAN AS SELECT stored;
";
        let document = Document::new(
            "file:///variables.orna".parse().unwrap(),
            text.to_owned(),
            1,
        );
        let parse = orna_syntax::parse(text);
        let mapper = PositionMapper::new(text);
        let first_parameter = text.find("first(stored").expect("first parameter") + "first(".len();
        let second_parameter =
            text.find("second(stored").expect("second parameter") + "second(".len();
        let first_use = text.find("SELECT stored").expect("first use") + "SELECT ".len();
        let second_use = text.rfind("SELECT stored").expect("second use") + "SELECT ".len();

        let second_definition =
            super::definition(&document, &parse, mapper.position(second_use), &mapper)
                .expect("second parameter definition");
        assert_eq!(
            second_definition.range.start,
            mapper.position(second_parameter)
        );

        let second_references = references(
            &document,
            &parse,
            mapper.position(second_use),
            &mapper,
            false,
        );
        assert_eq!(second_references.len(), 1);
        assert_eq!(
            second_references[0].range.start,
            mapper.position(second_use)
        );
        assert_ne!(second_references[0].range.start, mapper.position(first_use));

        let first_definition =
            super::definition(&document, &parse, mapper.position(first_parameter), &mapper)
                .expect("first parameter definition");
        assert_eq!(
            first_definition.range.start,
            mapper.position(first_parameter)
        );
    }

    #[test]
    fn client_state_definitions_stay_within_their_function() {
        let text = "CREATE CLIENT FUNCTION first() RETURNS BOOLEAN IS
\
              STATE stored BOOLEAN;
\
              BEGIN RETURN stored; END;
\
              CREATE CLIENT FUNCTION second() RETURNS BOOLEAN IS
\
              STATE stored BOOLEAN;
\
              BEGIN RETURN stored; END;
";
        let document = Document::new(
            "file:///client-variables.orna".parse().unwrap(),
            text.to_owned(),
            1,
        );
        let parse = orna_syntax::parse(text);
        let mapper = PositionMapper::new(text);
        let second_state = text.rfind("STATE stored").expect("second state") + "STATE ".len();
        let second_use = text.rfind("RETURN stored").expect("second state use") + "RETURN ".len();

        let definition = super::definition(&document, &parse, mapper.position(second_use), &mapper)
            .expect("second state definition");
        assert_eq!(definition.range.start, mapper.position(second_state));

        let references = references(
            &document,
            &parse,
            mapper.position(second_use),
            &mapper,
            false,
        );
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].range.start, mapper.position(second_use));
    }

    #[test]
    fn client_pre_begin_local_shadows_parameter_in_navigation() {
        let text = "CREATE CLIENT FUNCTION shadowed(p BOOLEAN) RETURNS BOOLEAN IS
\
              LET p BOOLEAN := TRUE;
\
              LET q BOOLEAN := p;
\
              BEGIN RETURN q; END;
";
        let document = Document::new(
            "file:///client-shadowing.orna".parse().unwrap(),
            text.to_owned(),
            1,
        );
        let parse = orna_syntax::parse(text);
        let mapper = PositionMapper::new(text);
        let local_definition = text.find("LET p").expect("local declaration") + "LET ".len();
        let local_use = text.rfind(":= p").expect("local initializer") + ":= ".len();

        let definition = super::definition(&document, &parse, mapper.position(local_use), &mapper)
            .expect("local shadow definition");
        assert_eq!(definition.range.start, mapper.position(local_definition));

        let references = references(
            &document,
            &parse,
            mapper.position(local_use),
            &mapper,
            false,
        );
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].range.start, mapper.position(local_use));
    }

    #[test]
    fn client_local_definitions_stay_within_their_function() {
        let text = "CREATE CLIENT FUNCTION first() RETURNS BOOLEAN IS
\
              LET marker BOOLEAN := TRUE;
\
              BEGIN RETURN marker; END;
\
              CREATE CLIENT FUNCTION second() RETURNS BOOLEAN IS
\
              LET marker BOOLEAN := TRUE;
\
              BEGIN RETURN marker; END;
";
        let document = Document::new(
            "file:///client-locals.orna".parse().unwrap(),
            text.to_owned(),
            1,
        );
        let parse = orna_syntax::parse(text);
        let mapper = PositionMapper::new(text);
        let second_local = text.rfind("LET marker").expect("second local") + "LET ".len();
        let second_use = text.rfind("RETURN marker").expect("second local use") + "RETURN ".len();

        let definition = super::definition(&document, &parse, mapper.position(second_use), &mapper)
            .expect("second local definition");
        assert_eq!(definition.range.start, mapper.position(second_local));

        let references = references(
            &document,
            &parse,
            mapper.position(second_use),
            &mapper,
            false,
        );
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].range.start, mapper.position(second_use));
    }

    #[test]
    fn references_fold_unicode_unquoted_identifier_case() {
        let text = "CREATE SCHEMA café;\nCREATE TYPE CAFÉ.probe AS OBJECT (value BOOLEAN);\n";
        let document = Document::new("file:///unicode.orna".parse().unwrap(), text.to_owned(), 1);
        let parse = orna_syntax::parse(text);
        let mapper = PositionMapper::new(text);
        let declaration = text.find("café").expect("unicode declaration");
        let use_position = text.find("CAFÉ").expect("unicode use");

        let with_declaration = references(
            &document,
            &parse,
            mapper.position(declaration),
            &mapper,
            true,
        );
        assert_eq!(with_declaration.len(), 2);
        assert_eq!(
            with_declaration[0].range.start,
            mapper.position(declaration)
        );
        assert_eq!(
            with_declaration[1].range.start,
            mapper.position(use_position)
        );

        let without_declaration = references(
            &document,
            &parse,
            mapper.position(declaration),
            &mapper,
            false,
        );
        assert_eq!(without_declaration.len(), 1);
        assert_eq!(
            without_declaration[0].range.start,
            mapper.position(use_position)
        );
    }
}
