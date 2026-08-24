//! Compiler-backed analysis for one open Orna document.
#![allow(deprecated)] // lsp-types 0.97 keeps the mandatory `deprecated` field.
//!
//! The analysis stages reuse the offline Orna compiler, so they need no
//! running database and never write to disk. The standard library is
//! verified once and cached for the lifetime of the server.

use lsp_types::{
    CompletionItem, CompletionItemKind, Diagnostic, DiagnosticSeverity, DocumentSymbol, Hover,
    Location, NumberOrString, Position, SymbolKind,
};
use orna_compiler::{CompilerDiagnostic, check_new_application, check_standard_library_source};
use orna_core::source::{SourceBundle, SourceUnit};
use orna_standard::{retained_standard_library_snapshot, verify_standard_library_snapshot};
use orna_syntax::FunctionReturnType;
use orna_syntax::{
    ClientExpression, ClientFunctionDeclaration, EnumTypeDeclaration, HighlightKind,
    ObjectTypeDeclaration, OpaqueValueTypeDeclaration, Parse, PrimitiveValueTypeDeclaration,
    QualifiedName, RecordValueTypeDeclaration, SchemaDeclaration, ServerFunctionDeclaration,
    SourceSpan, TypeSpecification,
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
/// Compares two source identifier spellings using Orna's quoted-name rules.
///
/// Unquoted identifiers are case-insensitive. Quoted identifiers preserve
/// exact spelling and do not match an unquoted identifier.
fn identifier_spelling_matches(candidate: &str, query: &str) -> bool {
    let candidate_quoted = candidate.starts_with('"') && candidate.ends_with('"');
    let query_quoted = query.starts_with('"') && query.ends_with('"');
    match (candidate_quoted, query_quoted) {
        (true, true) => candidate == query,
        (false, false) => candidate
            .chars()
            .flat_map(char::to_lowercase)
            .eq(query.chars().flat_map(char::to_lowercase)),
        _ => false,
    }
}

/// Returns a case-aware declaration lookup for one simple name.
pub fn declaration_at<'a>(parse: &'a Parse, name: &str) -> Option<DeclarationRef<'a>> {
    let matches = |candidate: &QualifiedName| {
        candidate
            .parts
            .last()
            .is_some_and(|part| identifier_spelling_matches(&part.text, name))
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
            || block
                .locals
                .iter()
                .any(|local| name_part_matches_span(&local.name, span))
            || block.statements.iter().any(|statement| {
                matches!(
                    statement,
                    orna_syntax::ClientProceduralStatement::Let(local)
                        if name_part_matches_span(&local.name, span)
                )
            })
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
    CallArgumentLabel,
}

fn client_expression_part_at<'a>(
    expression: &'a ClientExpression,
    selected_span: &SourceSpan,
) -> Option<ClientExpressionPart<'a>> {
    match expression {
        ClientExpression::Call { arguments, .. } => arguments.iter().find_map(|argument| {
            if argument
                .name
                .as_ref()
                .is_some_and(|name| name_part_matches_span(name, selected_span))
            {
                return Some(ClientExpressionPart::CallArgumentLabel);
            }
            client_expression_part_at(&argument.value, selected_span)
        }),
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
                    .find_map(|statement| match statement {
                        orna_syntax::ClientProceduralStatement::Let(local) => {
                            client_expression_part_at(&local.expression, selected_span)
                        }
                        orna_syntax::ClientProceduralStatement::Assignment(assignment) => {
                            client_expression_part_at(&assignment.expression, selected_span)
                        }
                    })
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

fn type_owner_name_from_source(source: &str) -> Option<QualifiedName> {
    let wrapped =
        format!("CREATE SERVER FUNCTION __orna_lsp_type_owner() RETURNS {source} AS SELECT TRUE;");
    let parsed = orna_syntax::parse(&wrapped);
    parsed
        .server_functions()
        .first()
        .and_then(|declaration| match &declaration.return_type {
            FunctionReturnType::Single(specification) => type_owner_name(specification),
            FunctionReturnType::Rows { .. } | FunctionReturnType::Stream { .. } => None,
        })
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
        let mut matches = Vec::new();
        for local in &block.locals {
            if name_part_matches_text(&local.name, &root.text) && local.span.end <= root.span.start
            {
                matches.push(ClientRootBinding {
                    declaration_span: local.name.span.clone(),
                    owner: type_owner_name_from_source(&local.type_source.text),
                });
            }
        }
        for statement in &block.statements {
            let orna_syntax::ClientProceduralStatement::Let(local) = statement else {
                continue;
            };
            if name_part_matches_text(&local.name, &root.text) && local.span.end <= root.span.start
            {
                matches.push(ClientRootBinding {
                    declaration_span: local.name.span.clone(),
                    owner: local
                        .type_source
                        .as_ref()
                        .and_then(|source| type_owner_name_from_source(&source.text)),
                });
            }
        }
        // Duplicate visible bindings are rejected by the compiler. Treat an
        // invalid/ambiguous parse the same way rather than guessing a target.
        (matches.len() == 1).then(|| matches.remove(0))
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
        ClientExpressionPart::ParameterRoot(_) => declaration
            .parameters
            .iter()
            .find(|parameter| name_part_matches_text(&parameter.name, &root.text))
            .map(|parameter| ClientRootBinding {
                declaration_span: parameter.name.span.clone(),
                owner: type_owner_name(&parameter.type_specification),
            })
            .or_else(find_state)
            .or_else(find_local),
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
        ClientExpressionPart::FieldMember { .. } | ClientExpressionPart::CallArgumentLabel => None,
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
    client_field_declaration_span(parse, selected_span).or_else(|| {
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
    if is_declaration_span(parse, selected_span) {
        if let Some(span) = field_declaration_span(parse, selected_span) {
            return Some(span);
        }
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
    let block = declaration.body.as_state_block();
    let mut exact = Vec::new();
    let mut visible = Vec::new();
    let mut consider = |name_part: &orna_syntax::NamePart, declaration_span: &SourceSpan| {
        if !name_part_matches_text(name_part, name) {
            return;
        }
        if name_part_matches_span(name_part, selected_span) {
            exact.push(name_part.span.clone());
        } else if declaration_span.end <= selected_span.start {
            visible.push(name_part.span.clone());
        }
    };
    if let Some(block) = block {
        for state in &block.states {
            consider(&state.name, &state.span);
        }
        for local in &block.locals {
            consider(&local.name, &local.span);
        }
        for statement in &block.statements {
            if let orna_syntax::ClientProceduralStatement::Let(local) = statement {
                consider(&local.name, &local.span);
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
            ClientExpressionPart::CallArgumentLabel => return None,
        }
    }
    let matches = |candidate: &QualifiedName| {
        candidate
            .parts
            .last()
            .is_some_and(|part| identifier_spelling_matches(&part.text, name))
    };
    let last_span =
        |candidate: &QualifiedName| candidate.parts.last().map(|part| part.span.clone());

    match kind {
        HighlightKind::NamespaceName => parse
            .schemas()
            .iter()
            .find(|declaration| matches(&declaration.name))
            .and_then(|declaration| last_span(&declaration.name)),
        HighlightKind::TypeName => {
            if let Some(declaration) = parse
                .object_types()
                .iter()
                .find(|declaration| matches(&declaration.name))
            {
                return last_span(&declaration.name);
            }
            if let Some(declaration) = parse
                .enum_types()
                .iter()
                .find(|declaration| matches(&declaration.name))
            {
                return last_span(&declaration.name);
            }
            if let Some(declaration) = parse
                .record_value_types()
                .iter()
                .find(|declaration| matches(&declaration.name))
            {
                return last_span(&declaration.name);
            }
            if let Some(declaration) = parse
                .primitive_value_types()
                .iter()
                .find(|declaration| matches(&declaration.name))
            {
                return last_span(&declaration.name);
            }
            parse
                .opaque_value_types()
                .iter()
                .find(|declaration| matches(&declaration.name))
                .and_then(|declaration| last_span(&declaration.name))
        }
        HighlightKind::FunctionName => {
            if let Some(declaration) = parse
                .server_functions()
                .iter()
                .find(|declaration| matches(&declaration.name))
            {
                return last_span(&declaration.name);
            }
            parse
                .client_functions()
                .iter()
                .find(|declaration| matches(&declaration.name))
                .and_then(|declaration| last_span(&declaration.name))
        }
        HighlightKind::PropertyName => {
            property_declaration_span(parse, text, highlighted, selected_span)
        }
        HighlightKind::VariableName => variable_declaration_span(parse, name, selected_span),
        HighlightKind::QuotedIdentifier => {
            property_declaration_span(parse, text, highlighted, selected_span)
                .or_else(|| variable_declaration_span(parse, name, selected_span))
                .or_else(|| {
                    declaration_at(parse, name).map(|declaration| declaration.name_span().clone())
                })
        }
        _ => declaration_at(parse, name).map(|declaration| declaration.name_span().clone()),
    }
}

#[derive(Clone)]
enum ReferenceScope {
    /// A resolved top-level name retains the existing document-wide lookup.
    TopLevel(HighlightKind),
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
    /// The selected token has no resolved declaration and must not leak.
    None,
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
            ClientExpressionPart::CallArgumentLabel => return ReferenceScope::None,
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
        ReferenceScope::TopLevel(kind)
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
            ClientExpressionPart::FieldMember { .. } | ClientExpressionPart::CallArgumentLabel => {
                None
            }
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
        ReferenceScope::TopLevel(kind) => token.kind == *kind,
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
            } else if let Some(declaration) = declaration_at(parse, &name) {
                Some(crate::hover::declaration_hover(
                    declaration,
                    &document.text,
                    doc_link.as_deref(),
                ))
            } else if let Some(field) = sql_column_at(parse, byte, &document.text, &highlighted) {
                // A column reference inside a SQL body resolves to the field
                // of the body's target object type.
                Some(crate::hover::field_hover(
                    &field,
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
            declaration_span.as_ref().map_or(true, |span| {
                span.start != token.range.start || span.end != token.range.end
            })
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

#[cfg(test)]
mod tests {
    use super::{completion, declaration_at, references};
    use crate::documents::{Document, PositionMapper};
    use lsp_types::Position;

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
