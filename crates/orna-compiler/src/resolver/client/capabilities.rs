//! CLIENT capability vocabulary, parsing, and validation.

use super::*;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClientCapabilityArgumentKind {
    PathScope,
    HostScope,
    SecretId,
}

impl ClientCapabilityArgumentKind {
    const fn label(self) -> &'static str {
        match self {
            Self::PathScope => "path-scope",
            Self::HostScope => "host-scope",
            Self::SecretId => "secret-id",
        }
    }
}

struct ClientCapabilityVocabularyEntry {
    parts: &'static [&'static str],
    argument_count: usize,
    argument_kind: ClientCapabilityArgumentKind,
}

const CLIENT_CAPABILITY_VOCABULARY: &[ClientCapabilityVocabularyEntry] = &[
    ClientCapabilityVocabularyEntry {
        parts: &["std", "fs", "read"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::PathScope,
    },
    ClientCapabilityVocabularyEntry {
        parts: &["std", "fs", "write"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::PathScope,
    },
    ClientCapabilityVocabularyEntry {
        parts: &["std", "net", "connect"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::HostScope,
    },
    ClientCapabilityVocabularyEntry {
        parts: &["std", "secret", "use"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::SecretId,
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
enum ClientCapabilityArgument {
    TextLiteral,
    Parameter(String),
}

fn client_capability_entry(
    name: &QualifiedSemanticName,
) -> Option<&'static ClientCapabilityVocabularyEntry> {
    CLIENT_CAPABILITY_VOCABULARY.iter().find(|entry| {
        name.parts()
            .iter()
            .map(String::as_str)
            .eq(entry.parts.iter().copied())
    })
}

fn client_capability_argument_count(arguments: Option<&SourceSlice>) -> usize {
    let Some(arguments) = arguments else {
        return 0;
    };
    let text = arguments.text.trim();
    if text.is_empty() {
        return 0;
    }

    let mut count = 1;
    let mut parentheses = 0usize;
    let mut quote = None;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if let Some(quote_character) = quote {
            if character == quote_character {
                if characters.peek() == Some(&quote_character) {
                    characters.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' => parentheses += 1,
            ')' => parentheses = parentheses.saturating_sub(1),
            ',' if parentheses == 0 => count += 1,
            _ => {}
        }
    }
    count
}

fn parse_client_capability_argument(text: &str) -> Option<ClientCapabilityArgument> {
    let text = text.trim();
    if is_client_text_literal(text) {
        return Some(ClientCapabilityArgument::TextLiteral);
    }
    normalise_client_parameter_name(text).map(ClientCapabilityArgument::Parameter)
}

/// Records one validated capability requirement in the checked CLIENT model.
///
/// The checked name is the closed qualified vocabulary name and the argument
/// source is the declaration's literal scope value or parameter reference.
/// Validation has already run, so a non-vocabulary name, wrong argument
/// shape, or undeclared parameter cannot reach this conversion; unknown
/// forms map to `None` and are skipped.
pub(super) fn checked_client_capability(
    capability: &CapabilitySpecification,
) -> Option<CheckedClientCapability> {
    let name = semantic_name(&capability.name);
    client_capability_entry(&name)?;
    let arguments = capability.arguments.as_ref()?;
    let argument = parse_client_capability_argument(&arguments.text)?;
    let argument = match argument {
        ClientCapabilityArgument::TextLiteral => {
            CheckedClientCapabilityArgument::Text(unquote_client_text_literal(&arguments.text)?)
        }
        ClientCapabilityArgument::Parameter(parameter) => {
            CheckedClientCapabilityArgument::Parameter(parameter)
        }
    };
    Some(CheckedClientCapability::new(name.to_string(), argument))
}

/// Unquotes one validated single-quoted CLIENT text literal.
///
/// A doubled quote inside the literal is a single literal quote, mirroring
/// `normalise_client_parameter_name`'s handling of quoted parameter names.
fn unquote_client_text_literal(text: &str) -> Option<String> {
    let text = text.trim();
    if !is_client_text_literal(text) {
        return None;
    }
    let inner = &text[1..text.len() - 1];
    let mut value = String::with_capacity(inner.len());
    let mut characters = inner.chars().peekable();
    while let Some(character) = characters.next() {
        value.push(character);
        if character == '\'' && characters.peek() == Some(&'\'') {
            characters.next();
        }
    }
    Some(value)
}

fn is_client_text_literal(text: &str) -> bool {
    let mut characters = text.chars();
    if characters.next() != Some('\'') || !text.ends_with('\'') {
        return false;
    }

    let mut characters = text[1..].chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\'' {
            continue;
        }
        if characters.peek() == Some(&'\'') {
            characters.next();
        } else {
            return characters.peek().is_none();
        }
    }
    false
}

pub(super) fn normalise_client_parameter_name(text: &str) -> Option<String> {
    if text.starts_with('"') {
        if !text.ends_with('"') || text.len() < 2 {
            return None;
        }
        let inner = &text[1..text.len() - 1];
        let mut characters = inner.chars().peekable();
        while let Some(character) = characters.next() {
            if character == '"' && characters.peek() == Some(&'"') {
                characters.next();
            } else if character == '"' {
                return None;
            }
        }
        if inner.is_empty() {
            return None;
        }
        return Some(inner.replace("\"\"", "\""));
    }

    let mut characters = text.chars();
    let first = characters.next()?;
    if first != '_' && !first.is_alphabetic() {
        return None;
    }
    if characters.any(|character| character != '_' && !character.is_alphanumeric()) {
        return None;
    }
    Some(text.to_lowercase())
}

pub(in crate::resolver) fn validate_client_capability<'a>(
    capability: &CapabilitySpecification,
    declared_parameters: impl IntoIterator<Item = &'a str>,
    logical_path: &str,
    declaration_span: &SourceSpan,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) {
    let name = semantic_name(&capability.name);
    let Some(entry) = client_capability_entry(&name) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!("unknown CLIENT capability {name}"),
            logical_path,
            declaration_span,
        ));
        return;
    };

    let argument_count = client_capability_argument_count(capability.arguments.as_ref());
    if argument_count != entry.argument_count {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} requires exactly {} {} argument",
                entry.argument_count,
                entry.argument_kind.label()
            ),
            logical_path,
            declaration_span,
        ));
        return;
    }

    let Some(arguments) = capability.arguments.as_ref() else {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} requires one {} argument",
                entry.argument_kind.label()
            ),
            logical_path,
            declaration_span,
        ));
        return;
    };
    let Some(argument) = parse_client_capability_argument(&arguments.text) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} argument must be a text literal or declared parameter"
            ),
            logical_path,
            declaration_span,
        ));
        return;
    };
    if let ClientCapabilityArgument::Parameter(parameter) = argument
        && !declared_parameters
            .into_iter()
            .any(|declared| declared == parameter)
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} argument references undeclared parameter {parameter}"
            ),
            logical_path,
            declaration_span,
        ));
    }
}
