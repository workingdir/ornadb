use crate::{
    ClientFunctionDeclaration, EnumTypeDeclaration, ObjectTypeDeclaration,
    OpaqueValueTypeDeclaration, Parse, PrimitiveValueTypeDeclaration, QualifiedName,
    RecordValueTypeDeclaration, SchemaDeclaration, ServerFunctionDeclaration, SourceSpan,
};

/// The canonical identity of one written Orna identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IdentifierKey {
    /// A quoted identifier, whose exact spelling is significant.
    Quoted(String),
    /// An unquoted identifier, compared with Unicode lowercase folding.
    Unquoted(String),
}

/// Canonicalizes one identifier using Orna's quoted-name rules.
pub fn identifier_key(spelling: &str) -> IdentifierKey {
    if spelling.starts_with('"') && spelling.ends_with('"') {
        IdentifierKey::Quoted(spelling.to_owned())
    } else {
        IdentifierKey::Unquoted(spelling.chars().flat_map(char::to_lowercase).collect())
    }
}

/// Returns whether two source identifiers refer to the same name.
pub fn identifier_spelling_matches(candidate: &str, query: &str) -> bool {
    identifier_key(candidate) == identifier_key(query)
}

/// Splits a written qualified name without splitting inside a quoted component.
pub fn source_name_parts(name: &str) -> Vec<&str> {
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

/// Returns whether two parsed qualified names identify the same declaration.
pub fn qualified_names_match(left: &QualifiedName, right: &QualifiedName) -> bool {
    left.parts.len() == right.parts.len()
        && left
            .parts
            .iter()
            .zip(&right.parts)
            .all(|(left, right)| identifier_spelling_matches(&left.text, &right.text))
}

/// Returns whether a parsed qualified name matches canonical component keys.
pub fn qualified_name_matches_keys(name: &QualifiedName, keys: &[IdentifierKey]) -> bool {
    name.parts.len() == keys.len()
        && name
            .parts
            .iter()
            .zip(keys)
            .all(|(part, key)| identifier_key(&part.text) == *key)
}

/// The source kind of a declaration exposed by [`LanguageModel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageDeclarationKind {
    /// A namespace declaration.
    Schema,
    /// A durable object type.
    ObjectType,
    /// An enumeration type.
    EnumType,
    /// A record value type.
    RecordValueType,
    /// A kernel-backed primitive value type.
    PrimitiveValueType,
    /// An opaque value type.
    OpaqueValueType,
    /// A trusted server function.
    ServerFunction,
    /// A local client function.
    ClientFunction,
}

/// One declaration borrowed from a parsed Orna source file.
#[derive(Debug, Clone, Copy)]
pub enum LanguageDeclaration<'a> {
    /// A schema declaration.
    Schema(&'a SchemaDeclaration),
    /// An object type declaration.
    ObjectType(&'a ObjectTypeDeclaration),
    /// An enum type declaration.
    EnumType(&'a EnumTypeDeclaration),
    /// A record value type declaration.
    RecordValueType(&'a RecordValueTypeDeclaration),
    /// A primitive value type declaration.
    PrimitiveValueType(&'a PrimitiveValueTypeDeclaration),
    /// An opaque value type declaration.
    OpaqueValueType(&'a OpaqueValueTypeDeclaration),
    /// A server function declaration.
    ServerFunction(&'a ServerFunctionDeclaration),
    /// A client function declaration.
    ClientFunction(&'a ClientFunctionDeclaration),
}

impl LanguageDeclaration<'_> {
    /// The declaration's qualified name.
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

    /// The full declaration span.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Schema(declaration) => &declaration.span,
            Self::ObjectType(declaration) => &declaration.span,
            Self::EnumType(declaration) => &declaration.span,
            Self::RecordValueType(declaration) => &declaration.span,
            Self::PrimitiveValueType(declaration) => &declaration.span,
            Self::OpaqueValueType(declaration) => &declaration.span,
            Self::ServerFunction(declaration) => &declaration.span,
            Self::ClientFunction(declaration) => &declaration.span,
        }
    }

    /// The declaration's language kind.
    pub fn kind(&self) -> LanguageDeclarationKind {
        match self {
            Self::Schema(_) => LanguageDeclarationKind::Schema,
            Self::ObjectType(_) => LanguageDeclarationKind::ObjectType,
            Self::EnumType(_) => LanguageDeclarationKind::EnumType,
            Self::RecordValueType(_) => LanguageDeclarationKind::RecordValueType,
            Self::PrimitiveValueType(_) => LanguageDeclarationKind::PrimitiveValueType,
            Self::OpaqueValueType(_) => LanguageDeclarationKind::OpaqueValueType,
            Self::ServerFunction(_) => LanguageDeclarationKind::ServerFunction,
            Self::ClientFunction(_) => LanguageDeclarationKind::ClientFunction,
        }
    }

    /// Whether this declaration has a renameable persistent name.
    pub fn is_renameable(&self) -> bool {
        !matches!(self, Self::Schema(_))
    }
}

/// A shared, lossless-language view over one parse result.
#[derive(Debug, Clone, Copy)]
pub struct LanguageModel<'a> {
    parse: &'a Parse,
}

impl Parse {
    /// Returns the shared language model for this source parse.
    pub fn language_model(&self) -> LanguageModel<'_> {
        LanguageModel { parse: self }
    }
}

impl<'a> LanguageModel<'a> {
    /// Returns parsed declarations in source order.
    pub fn declarations(&self) -> Vec<LanguageDeclaration<'a>> {
        let mut declarations = Vec::new();
        declarations.extend(self.parse.schemas().iter().map(LanguageDeclaration::Schema));
        declarations.extend(
            self.parse
                .object_types()
                .iter()
                .map(LanguageDeclaration::ObjectType),
        );
        declarations.extend(
            self.parse
                .enum_types()
                .iter()
                .map(LanguageDeclaration::EnumType),
        );
        declarations.extend(
            self.parse
                .record_value_types()
                .iter()
                .map(LanguageDeclaration::RecordValueType),
        );
        declarations.extend(
            self.parse
                .primitive_value_types()
                .iter()
                .map(LanguageDeclaration::PrimitiveValueType),
        );
        declarations.extend(
            self.parse
                .opaque_value_types()
                .iter()
                .map(LanguageDeclaration::OpaqueValueType),
        );
        declarations.extend(
            self.parse
                .server_functions()
                .iter()
                .map(LanguageDeclaration::ServerFunction),
        );
        declarations.extend(
            self.parse
                .client_functions()
                .iter()
                .map(LanguageDeclaration::ClientFunction),
        );
        declarations.sort_by_key(|declaration| declaration.span().start);
        declarations
    }
}

#[cfg(test)]
mod tests {
    use super::{LanguageDeclarationKind, identifier_key, qualified_names_match};
    use crate::parse;

    #[test]
    fn identifier_identity_matches_language_quoting_rules() {
        assert_eq!(identifier_key("café"), identifier_key("CAFÉ"));
        assert_ne!(identifier_key("name"), identifier_key("\"name\""));
        assert_ne!(identifier_key("\"Name\""), identifier_key("\"name\""));

        let parsed = parse("CREATE TYPE \"app\".item AS ENUM ('x');");
        let declared = &parsed.enum_types()[0].name;
        let equivalent = parse("CREATE TYPE \"app\".ITEM AS ENUM ('x');");
        assert!(qualified_names_match(
            declared,
            &equivalent.enum_types()[0].name
        ));
    }

    #[test]
    fn language_model_returns_every_declaration_in_source_order() {
        let parsed = parse(include_str!(
            "../tests/fixtures/language-model-declarations.orna"
        ));
        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        let declarations = parsed.language_model().declarations();
        assert_eq!(
            declarations
                .iter()
                .map(|declaration| declaration.kind())
                .collect::<Vec<_>>(),
            [
                LanguageDeclarationKind::Schema,
                LanguageDeclarationKind::ObjectType,
                LanguageDeclarationKind::EnumType,
                LanguageDeclarationKind::RecordValueType,
                LanguageDeclarationKind::PrimitiveValueType,
                LanguageDeclarationKind::OpaqueValueType,
                LanguageDeclarationKind::ServerFunction,
                LanguageDeclarationKind::ClientFunction,
            ]
        );
        assert!(
            declarations
                .windows(2)
                .all(|pair| { pair[0].span().start <= pair[1].span().start })
        );
        assert!(!declarations[0].is_renameable());
        assert!(
            declarations[1..]
                .iter()
                .all(|declaration| declaration.is_renameable())
        );
    }
}
