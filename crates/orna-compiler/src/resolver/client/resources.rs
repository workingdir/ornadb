//! CLIENT local-resource types and external contract validation.

use super::*;
pub(super) fn client_local_resource_family(source: &SourceSlice) -> Option<ResourceKind> {
    let mut parser = ClientResourceTypeParser::new(&source.text, source.span.start);
    let outer = parser.parse_qualified_name_parts()?;
    if outer.len() != 3
        || !outer[0].text.eq_ignore_ascii_case("std")
        || !outer[1].text.eq_ignore_ascii_case("data")
    {
        return None;
    }
    match outer[2].text.to_ascii_lowercase().as_str() {
        "resource" => Some(ResourceKind::Scalar),
        "streamresource" => Some(ResourceKind::Stream),
        _ => None,
    }
}

/// Parses a CLIENT resource declaration and returns its family plus inner descriptor.
///
/// The descriptor is resolved later against submitted and standard types; the SERVER
/// target remains authoritative for the resulting expression type.
pub(in crate::resolver) fn client_local_resource_type(
    source: &SourceSlice,
) -> Option<(ResourceKind, Option<TypeSpecification>)> {
    let mut parser = ClientResourceTypeParser::new(&source.text, source.span.start);
    let outer = parser.parse_qualified_name_parts()?;
    if outer.len() != 3
        || !outer[0].text.eq_ignore_ascii_case("std")
        || !outer[1].text.eq_ignore_ascii_case("data")
    {
        return None;
    }
    let kind = match outer[2].text.to_ascii_lowercase().as_str() {
        "resource" => ResourceKind::Scalar,
        "streamresource" => ResourceKind::Stream,
        _ => return None,
    };
    if !parser.consume(b'<') {
        return None;
    }
    let descriptor = if parser.consume_keyword("TABLE") || parser.consume_keyword("RECORD") {
        parser.parse_inline_record_shape(0)?;
        None
    } else {
        Some(parser.parse_type_specification(0)?)
    };
    if !parser.consume(b'>') || !parser.is_end() {
        return None;
    }
    Some((kind, descriptor))
}

pub(super) fn reject_deferred_client_resource_descriptor(
    descriptor: Option<&TypeSpecification>,
    local_name: &str,
    input: &ResolvedClientFunctionInput<'_>,
    source: &SourceSlice,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> bool {
    // A successful parse with no typed descriptor is the deferred inline row shape.
    if descriptor.is_some() {
        return false;
    }
    diagnostics.push(diagnostic(
        DiagnosticCode::TypeMismatch,
        format!(
            "CLIENT local {local_name} uses an inline TABLE/RECORD resource descriptor; row-resource transport is deferred"
        ),
        input.logical_path,
        &source.span,
    ));
    true
}

pub(in crate::resolver) struct ClientResourceTypeParser<'a> {
    text: &'a str,
    base: usize,
    offset: usize,
    invalid_trivia: bool,
}

impl<'a> ClientResourceTypeParser<'a> {
    pub(in crate::resolver) const MAX_TYPE_DEPTH: usize = 32;

    fn new(text: &'a str, base: usize) -> Self {
        Self {
            text,
            base,
            offset: 0,
            invalid_trivia: false,
        }
    }

    fn span(&self, start: usize, end: usize) -> SourceSpan {
        SourceSpan {
            start: self.base + start,
            end: self.base + end,
        }
    }

    fn source_slice(&self, start: usize, end: usize) -> SourceSlice {
        SourceSlice {
            text: self.text[start..end].to_owned(),
            span: self.span(start, end),
        }
    }

    fn is_end(&mut self) -> bool {
        self.skip_trivia();
        !self.invalid_trivia && self.offset == self.text.len()
    }

    fn skip_trivia(&mut self) {
        loop {
            while self
                .text
                .get(self.offset..)
                .and_then(|text| text.chars().next())
                .is_some_and(char::is_whitespace)
            {
                self.offset += self.text[self.offset..]
                    .chars()
                    .next()
                    .expect("character exists")
                    .len_utf8();
            }
            let Some(remaining) = self.text.get(self.offset..) else {
                return;
            };
            if remaining.starts_with("--") {
                self.offset += 2;
                while let Some(character) = self.text[self.offset..].chars().next() {
                    self.offset += character.len_utf8();
                    if character == '\n' {
                        break;
                    }
                }
                continue;
            }
            if let Some(comment) = remaining.strip_prefix("/*") {
                let Some(end) = comment.find("*/") else {
                    self.invalid_trivia = true;
                    self.offset = self.text.len();
                    return;
                };
                self.offset += end + 4;
                continue;
            }
            return;
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        self.skip_trivia();
        if self.text.as_bytes().get(self.offset) == Some(&expected) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    fn parse_identifier_part(&mut self) -> Option<NamePart> {
        self.skip_trivia();
        let start = self.offset;
        if self.text.as_bytes().get(self.offset) == Some(&b'"') {
            self.offset += 1;
            while let Some(character) = self.text[self.offset..].chars().next() {
                self.offset += character.len_utf8();
                if character == '"' {
                    if self.text.as_bytes().get(self.offset) == Some(&b'"') {
                        self.offset += 1;
                    } else {
                        return Some(NamePart {
                            text: self.text[start..self.offset].to_owned(),
                            span: self.span(start, self.offset),
                        });
                    }
                }
            }
            return None;
        }
        let first = self.text[self.offset..].chars().next()?;
        if first != '_' && !first.is_alphabetic() {
            return None;
        }
        self.offset += first.len_utf8();
        while let Some(character) = self.text[self.offset..].chars().next() {
            if character != '_' && !character.is_alphabetic() && !character.is_numeric() {
                break;
            }
            self.offset += character.len_utf8();
        }
        Some(NamePart {
            text: self.text[start..self.offset].to_owned(),
            span: self.span(start, self.offset),
        })
    }

    fn parse_identifier(&mut self) -> Option<String> {
        self.parse_identifier_part().map(|part| part.text)
    }

    fn parse_qualified_name_parts(&mut self) -> Option<Vec<NamePart>> {
        let mut parts = vec![self.parse_identifier_part()?];
        while self.consume(b'.') {
            parts.push(self.parse_identifier_part()?);
        }
        Some(parts)
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        self.skip_trivia();
        let saved = self.offset;
        if self.text.as_bytes().get(saved) == Some(&b'"') {
            return false;
        }
        let Some(identifier) = self.parse_identifier() else {
            return false;
        };
        if identifier.eq_ignore_ascii_case(keyword) {
            true
        } else {
            self.offset = saved;
            false
        }
    }

    fn parse_type_specification(&mut self, depth: usize) -> Option<TypeSpecification> {
        if depth > Self::MAX_TYPE_DEPTH {
            return None;
        }
        self.skip_trivia();
        let saved = self.offset;
        if self.consume_keyword("REF") {
            let target = self.parse_type_specification(depth + 1)?;
            let spec = TypeSpecification::Reference {
                span: self.span(saved, target.span().end - self.base),
                target: Box::new(target),
            };
            return self.parse_postfix_options(spec, depth);
        }
        for keyword in ["LIST", "SET", "MAP", "OPTION", "STREAM"] {
            self.offset = saved;
            if !self.consume_keyword(keyword) {
                continue;
            }
            if !self.consume(b'<') {
                return None;
            }
            let first = self.parse_type_specification(depth + 1)?;
            let second = if keyword == "MAP" {
                if !self.consume(b',') {
                    return None;
                }
                Some(self.parse_type_specification(depth + 1)?)
            } else {
                None
            };
            if !self.consume(b'>') {
                return None;
            }
            let spec = match keyword {
                "LIST" => TypeSpecification::List {
                    span: self.span(saved, self.offset),
                    element: Box::new(first),
                },
                "SET" => TypeSpecification::Set {
                    span: self.span(saved, self.offset),
                    element: Box::new(first),
                },
                "MAP" => TypeSpecification::Map {
                    span: self.span(saved, self.offset),
                    key: Box::new(first),
                    value: Box::new(second.expect("MAP value exists")),
                },
                "OPTION" => TypeSpecification::Option {
                    span: self.span(saved, self.offset),
                    value: Box::new(first),
                    spelling: OptionTypeSpelling::Prefix,
                },
                "STREAM" => TypeSpecification::Stream {
                    span: self.span(saved, self.offset),
                    element: Box::new(first),
                },
                _ => unreachable!(),
            };
            return self.parse_postfix_options(spec, depth);
        }
        self.offset = saved;
        if let Some(spec) = self.parse_standard_large_object_specification() {
            return self.parse_postfix_options(spec, depth);
        }
        self.offset = saved;
        let parts = self.parse_qualified_name_parts()?;
        let start = parts.first().expect("nonempty").span.start - self.base;
        let end = parts.last().expect("nonempty").span.end - self.base;
        self.parse_postfix_options(
            TypeSpecification::Named(QualifiedName {
                parts,
                span: self.span(start, end),
            }),
            depth,
        )
    }

    fn parse_inline_record_shape(&mut self, depth: usize) -> Option<()> {
        if depth > Self::MAX_TYPE_DEPTH || !self.consume(b'(') {
            return None;
        }
        if self.consume(b')') {
            return Some(());
        }
        loop {
            self.parse_identifier_part()?;
            self.parse_type_specification(depth + 1)?;
            if self.consume(b')') {
                return Some(());
            }
            if !self.consume(b',') {
                return None;
            }
        }
    }

    fn parse_standard_large_object_specification(&mut self) -> Option<TypeSpecification> {
        self.skip_trivia();
        let start = self.offset;
        let kind = if self.consume_keyword("CHARACTER") {
            StandardLargeObjectKind::Character
        } else {
            self.offset = start;
            if self.consume_keyword("BINARY") {
                StandardLargeObjectKind::Binary
            } else {
                self.offset = start;
                return None;
            }
        };
        if !self.consume_keyword("LARGE") || !self.consume_keyword("OBJECT") {
            self.offset = start;
            return None;
        }
        Some(TypeSpecification::StandardLargeObject {
            kind,
            source: self.source_slice(start, self.offset),
        })
    }

    fn parse_postfix_options(
        &mut self,
        mut spec: TypeSpecification,
        depth: usize,
    ) -> Option<TypeSpecification> {
        let mut option_depth = depth;
        loop {
            self.skip_trivia();
            if self.text.as_bytes().get(self.offset) != Some(&b'?') {
                return Some(spec);
            }
            if option_depth >= Self::MAX_TYPE_DEPTH {
                return None;
            }
            self.offset += 1;
            option_depth += 1;
            let start = spec.span().start - self.base;
            spec = TypeSpecification::Option {
                value: Box::new(spec),
                spelling: OptionTypeSpelling::Postfix,
                span: self.span(start, self.offset),
            };
        }
    }
}

pub(super) fn client_type_specification_from_source(
    source: &SourceSlice,
) -> Option<TypeSpecification> {
    let text = source.text.trim();
    let normalized: String = text
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    let large_object = match normalized.to_ascii_uppercase().as_str() {
        "CHARACTERLARGEOBJECT" => Some(StandardLargeObjectKind::Character),
        "BINARYLARGEOBJECT" => Some(StandardLargeObjectKind::Binary),
        _ => None,
    };
    if let Some(kind) = large_object {
        return Some(TypeSpecification::StandardLargeObject {
            kind,
            source: source.clone(),
        });
    }
    if text.is_empty()
        || text.split('.').any(|part| {
            part.is_empty()
                || part.chars().any(|character| {
                    !(character.is_ascii_alphanumeric() || character == '_' || character == '"')
                })
        })
    {
        return None;
    }
    let parts = text
        .split('.')
        .map(|part| orna_syntax::NamePart {
            text: part.to_owned(),
            span: source.span.clone(),
        })
        .collect::<Vec<_>>();
    Some(TypeSpecification::Named(QualifiedName {
        parts,
        span: source.span.clone(),
    }))
}
pub(in crate::resolver) fn client_contract_identity(source: &SourceSlice) -> Option<String> {
    let identity = decode_string_literal(source)?;
    let (name, version) = identity.rsplit_once('@')?;
    if version.is_empty()
        || version
            .parse::<u64>()
            .ok()
            .is_none_or(|version| version == 0)
        || name.contains('@')
    {
        return None;
    }
    let parts = name.split('.').collect::<Vec<_>>();
    if parts
        .iter()
        .any(|part| normalise_client_parameter_name(part).is_none())
        || QualifiedSemanticName::new(parts).is_err()
    {
        return None;
    }
    Some(identity)
}
fn is_inspect_render_identity(identity: &str) -> bool {
    identity == "devtools.inspector_shell@1"
        || identity == INSPECT_RENDER_CONTRACT
        || identity.starts_with("std.inspect.render@")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_registered_client_external_contract(
    _name: &QualifiedSemanticName,
    identity: &str,
    parameters: &[ResolvedServerFunctionParameter],
    return_type: ResolvedApplicationType,
    result_shape: ClientExpressionResultShape,
    logical_path: &str,
    declaration_span: &SourceSpan,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> bool {
    if !is_inspect_render_identity(identity) {
        return true;
    }
    if identity != INSPECT_RENDER_CONTRACT {
        diagnostics.push(diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!("unregistered CLIENT external contract {identity}"),
            logical_path,
            declaration_span,
        ));
        return false;
    }

    if parameters.len() != INSPECT_RENDER_CARRIER_SIGNATURE.len() {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("{INSPECT_RENDER_CONTRACT} requires exactly nine ordered carrier parameters"),
            logical_path,
            declaration_span,
        ));
        return false;
    }
    for (parameter, (expected_name, expected_id, _)) in
        parameters.iter().zip(INSPECT_RENDER_CARRIER_SIGNATURE)
    {
        if parameter.name != expected_name
            || parameter.semantic_type != SemanticType::Named(CheckedTypeId::Existing(expected_id))
        {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "{INSPECT_RENDER_CONTRACT} parameter {expected_name} must be {}",
                    expected_name.trim_start_matches("p_")
                ),
                logical_path,
                &parameter.name_span,
            ));
            return false;
        }
    }
    if result_shape != ClientExpressionResultShape::Value
        || return_type.semantic_type != SemanticType::Named(CheckedTypeId::Existing(STD_UI_TYPE_ID))
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("{INSPECT_RENDER_CONTRACT} must return std.ui.UI"),
            logical_path,
            declaration_span,
        ));
        return false;
    }
    true
}
