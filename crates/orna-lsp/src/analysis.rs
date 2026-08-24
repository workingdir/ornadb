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
use orna_syntax::{
    ClientFunctionDeclaration, EnumTypeDeclaration, HighlightKind, ObjectTypeDeclaration,
    OpaqueValueTypeDeclaration, Parse, PrimitiveValueTypeDeclaration, QualifiedName,
    RecordValueTypeDeclaration, SchemaDeclaration, ServerFunctionDeclaration, SourceSpan,
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
fn token_at(text: &str, parse: &Parse, byte: usize) -> Option<(String, HighlightKind, SourceSpan)> {
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
        (false, false) => candidate.eq_ignore_ascii_case(query),
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
                return Some(FieldInfo {
                    name: &field.name,
                    type_specification: &field.type_specification,
                    nullable: None,
                    unique: false,
                    on_delete: None,
                    documentation: field.documentation.as_ref().map(strip_quotes),
                    default_text: None,
                });
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

/// Returns the object or record field whose name covers one byte offset
/// inside a SQL body, resolved through the body's target object type.
fn sql_column_at<'a>(parse: &'a Parse, byte: usize, text: &str) -> Option<FieldInfo<'a>> {
    let (name, kind, _) = token_at(text, parse, byte)?;
    if kind != HighlightKind::PropertyName {
        return None;
    }
    for declaration in parse.server_functions() {
        let resolved = match &declaration.body {
            orna_syntax::ServerFunctionBody::SqlQuery(body)
                if contains(&body.source.span, byte) =>
            {
                field_on_object(parse, &body.query.source_object.object_type, &name)
            }
            orna_syntax::ServerFunctionBody::SqlInsert(body)
                if contains(&body.source.span, byte)
                    && body
                        .insert
                        .target_fields
                        .iter()
                        .any(|field| contains(&field.span, byte)) =>
            {
                field_on_object(parse, &body.insert.target_object, &name)
            }
            orna_syntax::ServerFunctionBody::SqlUpdate(body)
                if contains(&body.source.span, byte)
                    && body
                        .update
                        .assignments
                        .iter()
                        .any(|assignment| contains(&assignment.target_field.span, byte)) =>
            {
                field_on_object(parse, &body.update.target_object, &name)
            }
            _ => None,
        };
        if resolved.is_some() {
            return resolved;
        }
    }
    None
}

/// Resolves one field name against a declared object type.
fn field_on_object<'a>(
    parse: &'a Parse,
    object_type: &QualifiedName,
    field_name: &str,
) -> Option<FieldInfo<'a>> {
    let matches = |candidate: &QualifiedName| {
        candidate.parts.len() == object_type.parts.len()
            && candidate
                .parts
                .iter()
                .zip(&object_type.parts)
                .all(|(left, right)| left.text.eq_ignore_ascii_case(&right.text))
    };
    let declaration = parse
        .object_types()
        .iter()
        .find(|declaration| matches(&declaration.name))?;
    let field = declaration
        .fields
        .iter()
        .find(|field| field.name.text.eq_ignore_ascii_case(field_name))?;
    Some(object_field_info(field))
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

/// Returns true when one byte lies inside a span.
fn contains(span: &SourceSpan, byte: usize) -> bool {
    byte >= span.start && byte < span.end
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
    let (name, kind, span) = token_at(&document.text, parse, byte)?;
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
            } else if let Some(field) = sql_column_at(parse, byte, &document.text) {
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
            .is_some_and(|part| part.eq_ignore_ascii_case(name))
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
            .is_some_and(|part| part.eq_ignore_ascii_case(name))
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
    let (name, kind, _) = token_at(&document.text, parse, byte)?;
    if kind == HighlightKind::Keyword {
        return None;
    }
    declaration_at(parse, &name).map(|declaration| Location {
        uri: document.uri.clone(),
        range: mapper.range(declaration.name_span()),
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
    let Some((name, kind, _)) = token_at(&document.text, parse, byte) else {
        return Vec::new();
    };
    if kind == HighlightKind::Keyword {
        return Vec::new();
    }
    let declaration_span = if include_declaration {
        None
    } else {
        declaration_at(parse, &name).and_then(|declaration| {
            declaration
                .name()
                .parts
                .last()
                .map(|part| part.span.clone())
        })
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
            assert!(labels.iter().any(|label| label == expected), "missing {expected}");
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
        let text = "CREATE SCHEMA foo;\nCREATE SERVER FUNCTION bar() RETURNS BOOLEAN AS SELECT Foo;\n";
        let document = Document::new("file:///test.orna".parse().unwrap(), text.to_owned(), 1);
        let parse = orna_syntax::parse(text);
        let mapper = PositionMapper::new(text);

        let foo_references = references(&document, &parse, Position::new(0, 14), &mapper, true);
        assert_eq!(foo_references.len(), 2);
        assert_eq!(foo_references[0].range.start, Position::new(0, 14));
        assert_eq!(foo_references[1].range.start, Position::new(1, 55));
        let without_declaration = references(&document, &parse, Position::new(0, 14), &mapper, false);
        assert_eq!(without_declaration.len(), 1);
        assert_eq!(without_declaration[0].range.start, Position::new(1, 55));

        let qualified_text =
            "CREATE SCHEMA product_test;\nCREATE TYPE product_test.probe AS OBJECT (value BOOLEAN);\n";
        let qualified_document =
            Document::new("file:///qualified.orna".parse().unwrap(), qualified_text.to_owned(), 1);
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
        assert_eq!(namespace_without_declaration[0].range.start, Position::new(1, 12));
    }
}
