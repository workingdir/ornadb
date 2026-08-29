//! Rich hover markdown builders.
//!
//! Hovers mirror rust-analyzer: a kind badge and qualified name, a source
//! signature, per-parameter and per-field detail, documentation from
//! `DOCUMENTATION` clauses, a usage example, and a link to the grammar
//! specification when the spec bundle is reachable from the document.

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

use crate::analysis::{DeclarationRef, FieldInfo, FunctionDeclarationView, ParameterInfo};
use crate::reference::{KeywordReference, ScalarReference};

/// Returns the grammar specification link for one document, if reachable.
///
/// The link points at the EBNF inside the nearest ancestor directory that
/// contains `spec/spec/orna.ebnf`. Non-file URIs and trees without the
/// spec bundle yield no link.
pub fn spec_doc_link(uri: &lsp_types::Uri) -> Option<String> {
    let path = uri.as_str().strip_prefix("file://")?;
    let mut directory = std::path::PathBuf::from(path);
    if directory.is_file() {
        directory.pop();
    }
    loop {
        if directory
            .join("spec")
            .join("spec")
            .join("orna.ebnf")
            .exists()
        {
            return Some(format!(
                "file://{}/spec/spec/orna.ebnf",
                directory.display()
            ));
        }
        if !directory.pop() {
            return None;
        }
    }
}

/// Builds the hover for one keyword.
pub fn keyword_hover(reference: &KeywordReference, doc_link: Option<&str>) -> Hover {
    let mut value = format!(
        "**`{}`** keyword\n\n{}\n\n**Context**\n{}\n\n**Example**\n```orna\n{}\n```",
        reference.keyword, reference.summary, reference.context, reference.example
    );
    append_spec_link(&mut value, doc_link);
    hover(value)
}

/// Builds the hover for one scalar or standard type.
pub fn scalar_hover(reference: &ScalarReference, doc_link: Option<&str>) -> Hover {
    let mut value = format!(
        "**`{}`** standard type\n\n{}\n\n**Type information**\n- Storage: `{}`\n- Usage: `{}`\n\n**Example**\n```orna\n{}\n```",
        reference.name,
        reference.summary,
        reference.name,
        scalar_usage(reference.name),
        reference.example,
    );
    append_spec_link(&mut value, doc_link);
    hover(value)
}

/// Builds the hover for one standard-library value type.
pub fn standard_type_hover(
    name: &str,
    kind: &str,
    contract: &str,
    doc_link: Option<&str>,
) -> Hover {
    let mut value =
        format!("**`{name}`** standard {kind} value type\n\nKernel contract: `{contract}`\n");
    append_spec_link(&mut value, doc_link);
    hover(value)
}

/// Builds the hover for one standard-library schema.
pub fn standard_schema_hover(name: &str, doc_link: Option<&str>) -> Hover {
    let mut value = format!("**`{name}`** standard schema\n");
    append_spec_link(&mut value, doc_link);
    hover(value)
}

/// Builds the hover for one declaration.
pub fn declaration_hover(
    declaration: DeclarationRef<'_>,
    text: &str,
    doc_link: Option<&str>,
) -> Hover {
    match declaration {
        DeclarationRef::Schema(declaration) => {
            let mut value = format!(
                "**schema**\n\n```orna\nCREATE SCHEMA {};\n```",
                qualified(&declaration.name)
            );
            append_example(
                &mut value,
                &format!(
                    "CREATE TYPE {}.example AS OBJECT (id INT);",
                    qualified(&declaration.name)
                ),
            );
            append_spec_link(&mut value, doc_link);
            hover(value)
        }
        DeclarationRef::ObjectType(declaration) => {
            let fields = declaration
                .fields
                .iter()
                .map(|field| {
                    let mut entry = format!(
                        "- `{}`: {}",
                        field.name.text,
                        type_text(&field.type_specification, text)
                    );
                    if !field.nullable {
                        entry.push_str(" NOT NULL");
                    }
                    if field.unique {
                        entry.push_str(" UNIQUE");
                    }
                    if let Some(policy) = field.on_delete {
                        entry.push_str(&format!(" ON DELETE {policy:?}"));
                    }
                    append_inline_documentation(
                        &mut entry,
                        documentation_text(field.documentation.as_ref()),
                    );
                    entry
                })
                .collect::<Vec<_>>()
                .join("\n");
            let mut value = format!(
                "**object type**{}\n\n```orna\nCREATE TYPE {} AS OBJECT (\n{}\n);\n```",
                if declaration.final_type {
                    " — final"
                } else {
                    ""
                },
                qualified(&declaration.name),
                fields
            );
            append_documentation(
                &mut value,
                documentation_text(declaration.documentation.as_ref()),
            );
            append_example(&mut value, &format!("REF {}", qualified(&declaration.name)));
            append_spec_link(&mut value, doc_link);
            hover(value)
        }
        DeclarationRef::EnumType(declaration) => {
            let labels = declaration
                .labels
                .iter()
                .map(|label| label.literal.text.clone())
                .collect::<Vec<_>>()
                .join(", ");
            let mut value = format!(
                "**enum type**\n\n```orna\nCREATE TYPE {} AS ENUM ({labels});\n```",
                qualified(&declaration.name)
            );
            append_example(&mut value, &qualified(&declaration.name));
            append_spec_link(&mut value, doc_link);
            hover(value)
        }
        DeclarationRef::RecordValueType(declaration) => {
            let fields = declaration
                .fields
                .iter()
                .map(|field| {
                    let mut entry = format!(
                        "- `{}`: {}",
                        field.name.text,
                        type_text(&field.type_specification, text)
                    );
                    append_inline_documentation(
                        &mut entry,
                        documentation_text(field.documentation.as_ref()),
                    );
                    entry
                })
                .collect::<Vec<_>>()
                .join("\n");
            let mut value = format!(
                "**record value type**\n\n```orna\nCREATE TYPE {} AS VALUE (\n{}\n) IMMUTABLE PERSISTABLE;\n```",
                qualified(&declaration.name),
                fields
            );
            append_documentation(
                &mut value,
                documentation_text(declaration.documentation.as_ref()),
            );
            append_example(&mut value, &qualified(&declaration.name));
            append_spec_link(&mut value, doc_link);
            hover(value)
        }
        DeclarationRef::PrimitiveValueType(declaration) => {
            let mut value = format!(
                "**primitive value type**\n\n```orna\nCREATE TYPE {} AS VALUE PRIMITIVE\n    KERNEL CONTRACT {}\n    IMMUTABLE\n    PERSISTABLE;\n```",
                qualified(&declaration.name),
                declaration.kernel_contract.text
            );
            append_documentation(
                &mut value,
                documentation_text(declaration.documentation.as_ref()),
            );
            append_example(&mut value, &qualified(&declaration.name));
            append_spec_link(&mut value, doc_link);
            hover(value)
        }
        DeclarationRef::OpaqueValueType(declaration) => {
            let mut value = format!(
                "**opaque value type**\n\n```orna\nCREATE TYPE {} AS VALUE OPAQUE\n    KERNEL CONTRACT {}\n    IMMUTABLE\n    TRANSIENT;\n```",
                qualified(&declaration.name),
                declaration.kernel_contract.text
            );
            append_documentation(
                &mut value,
                documentation_text(declaration.documentation.as_ref()),
            );
            append_example(&mut value, &qualified(&declaration.name));
            append_spec_link(&mut value, doc_link);
            hover(value)
        }
        DeclarationRef::ServerFunction(declaration) => {
            let mut value = format!(
                "**server function**\n\n```orna\nCREATE SERVER FUNCTION {}({})\nRETURNS {}\n```",
                qualified(&declaration.name),
                parameters(declaration, text),
                return_text(&declaration.return_type, text)
            );
            append_parameters(&mut value, declaration, text);
            append_example(
                &mut value,
                &format!("orna invoke {}", qualified(&declaration.name)),
            );
            append_spec_link(&mut value, doc_link);
            hover(value)
        }
        DeclarationRef::ClientFunction(declaration) => {
            let mut value = if declaration.external {
                format!(
                    "**client function**\n\n```orna\nCREATE EXTERNAL CLIENT FUNCTION {}({})\nRETURNS {}{}{}\n```",
                    qualified(&declaration.name),
                    parameters(declaration, text),
                    return_text(&declaration.return_type, text),
                    declaration
                        .runtime_contract
                        .as_ref()
                        .map(|contract| format!("\nRUNTIME CONTRACT {}", contract.text))
                        .unwrap_or_default(),
                    capability_clause_text(&declaration.capabilities),
                )
            } else {
                format!(
                    "**client function**\n\n```orna\nCREATE CLIENT FUNCTION {}({})\nRETURNS {}\n```",
                    qualified(&declaration.name),
                    parameters(declaration, text),
                    return_text(&declaration.return_type, text)
                )
            };
            append_parameters(&mut value, declaration, text);
            append_example(
                &mut value,
                &format!("orna invoke {}", qualified(&declaration.name)),
            );
            append_spec_link(&mut value, doc_link);
            hover(value)
        }
    }
}

/// Builds the hover for one object or record field.
pub fn field_hover(field: &FieldInfo<'_>, text: &str, doc_link: Option<&str>) -> Hover {
    let mut value = format!(
        "**field** `{}`: {}\n",
        field.name.text,
        type_text(field.type_specification, text)
    );
    let mut modifiers: Vec<String> = Vec::new();
    if field.nullable == Some(false) {
        modifiers.push("NOT NULL".to_owned());
    }
    if field.unique {
        modifiers.push("UNIQUE".to_owned());
    }
    if let Some(policy) = field.on_delete {
        modifiers.push(format!("ON DELETE {policy}"));
    }
    if let Some(default) = field.default_text {
        modifiers.push(format!("DEFAULT {default}"));
    }
    if !modifiers.is_empty() {
        value.push_str(&format!("\n`{}`\n", modifiers.join(" ")));
    }
    append_documentation(&mut value, field.documentation);
    append_spec_link(&mut value, doc_link);
    hover(value)
}

/// Builds the hover for one function parameter.
pub fn parameter_hover(parameter: &ParameterInfo<'_>, text: &str, doc_link: Option<&str>) -> Hover {
    let mut value = format!(
        "**parameter** `{}`: {}\n",
        parameter.name.text,
        type_text(parameter.type_specification, text)
    );
    if let Some(default) = parameter.default_text {
        value.push_str(&format!("\nDefault: `{default}`\n"));
    }
    append_documentation(&mut value, parameter.documentation);
    append_spec_link(&mut value, doc_link);
    hover(value)
}

/// Returns a complete hover with markdown contents.
fn hover(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

fn scalar_usage(name: &str) -> &'static str {
    match name {
        "BOOLEAN" | "BOOL" => "Use for true/false values.",
        "INTEGER" | "INT" | "BIGINT" => "Use for whole-number values.",
        "FLOAT" | "DECIMAL" => "Use for numeric values that can contain a fraction.",
        "TEXT" | "CHARACTER LARGE OBJECT" => "Use for text values.",
        "BYTES" | "BINARY LARGE OBJECT" => "Use for binary values.",
        "UUID" => "Use for stable unique identifiers.",
        "DATE" | "TIME" | "TIMESTAMP" | "DURATION" => "Use for temporal values.",
        "VOID" => "Use when a function returns no value.",
        _ => "Use this standard type in a declaration or expression.",
    }
}

/// Appends a Documentation section from a captured source slice.
fn append_documentation(value: &mut String, documentation: Option<&str>) {
    if let Some(documentation) = documentation {
        value.push_str(&format!("\n**Documentation**\n{documentation}\n"));
    }
}

/// Appends documentation to one list entry.
fn append_inline_documentation(entry: &mut String, documentation: Option<&str>) {
    if let Some(documentation) = documentation {
        entry.push_str(&format!(" — {documentation}"));
    }
}

/// Appends an Example section.
fn append_example(value: &mut String, example: &str) {
    value.push_str(&format!("\n**Example**\n```orna\n{example}\n```\n"));
}

/// Appends the specification link.
fn append_spec_link(value: &mut String, doc_link: Option<&str>) {
    if let Some(link) = doc_link {
        value.push_str(&format!("\n**Spec**\n[Orna grammar]({link})\n"));
    }
}

/// Appends a Parameters section from a function declaration.
fn append_parameters<F>(value: &mut String, declaration: &F, text: &str)
where
    F: FunctionDeclarationView,
{
    let parameters = declaration
        .parameters()
        .iter()
        .map(|parameter| {
            let mut entry = format!(
                "- `{}`: {}",
                parameter.name.text,
                type_text(&parameter.type_specification, text)
            );
            if let Some(default) = &parameter.default_expression {
                entry.push_str(&format!(" = {}", default.text));
            }
            append_inline_documentation(
                &mut entry,
                documentation_text(parameter.documentation.as_ref()),
            );
            entry
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !parameters.is_empty() {
        value.push_str(&format!("\n**Parameters**\n{parameters}\n"));
    }
}

/// Renders the accepted capability requirements of an external CLIENT function.
///
/// Capability names and arguments are retained from their parsed source slices so
/// hover text does not reinterpret or manufacture runtime metadata.
fn capability_clause_text(capabilities: &[orna_syntax::CapabilitySpecification]) -> String {
    if capabilities.is_empty() {
        return String::new();
    }

    let requirements = capabilities
        .iter()
        .map(|capability| {
            let mut value = qualified(&capability.name);
            if let Some(arguments) = &capability.arguments {
                value.push('(');
                value.push_str(&arguments.text);
                value.push(')');
            }
            value
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("\nREQUIRES CAPABILITY {requirements}")
}

/// Renders the parameter list of a function from source slices.
fn parameters<F>(declaration: &F, text: &str) -> String
where
    F: FunctionDeclarationView,
{
    declaration
        .parameters()
        .iter()
        .map(|parameter| {
            let mut rendered = format!(
                "{} {}",
                parameter.name.text,
                type_text(&parameter.type_specification, text)
            );
            if let Some(default) = &parameter.default_expression {
                rendered.push_str(&format!(" = {}", default.text));
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Renders a return type from its source span.
fn return_text(return_type: &orna_syntax::FunctionReturnType, text: &str) -> String {
    match return_type {
        orna_syntax::FunctionReturnType::Single(specification) => type_text(specification, text),
        orna_syntax::FunctionReturnType::Rows { span, .. }
        | orna_syntax::FunctionReturnType::Stream { span, .. } => source_text_range(span, text),
    }
}

/// Renders one type specification from its source span.
pub fn type_text(specification: &orna_syntax::TypeSpecification, text: &str) -> String {
    source_text_range(specification.span(), text)
}

/// Returns the source slice text for one span.
fn source_text_range(span: &orna_syntax::SourceSpan, text: &str) -> String {
    text.get(span.start..span.end)
        .map(str::to_owned)
        .unwrap_or_else(|| "?".to_owned())
}

/// Returns the documentation text of a captured slice with quotes stripped.
fn documentation_text(slice: Option<&orna_syntax::SourceSlice>) -> Option<&str> {
    slice.map(|slice| {
        slice
            .text
            .strip_prefix('\'')
            .and_then(|inner| inner.strip_suffix('\''))
            .unwrap_or(&slice.text)
    })
}

/// Renders one qualified name.
fn qualified(name: &orna_syntax::QualifiedName) -> String {
    name.parts
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join(".")
}
