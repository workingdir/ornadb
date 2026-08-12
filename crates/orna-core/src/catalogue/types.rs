//! Catalogue definitions for object and value types.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::{FieldId, TypeBindingId, TypeId, types::ResolvedType};

use super::{ObjectTypeDefinition, QualifiedSemanticName};

const TYPE_BINDING_ID_DOMAIN: &[u8] = b"ornadb.id/type-binding/v1\0";

/// The category of a catalogue type definition.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeDefinitionKind {
    /// A type with durable object identities and fields.
    Object,
    /// A by-value type without durable object identity.
    Value,
    /// An ordered set of declared labels.
    Enum,
}

/// The representation category of a value type.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueTypeKind {
    /// A value represented by one kernel primitive contract.
    Primitive,
}

/// Whether a value type can be stored durably.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueTypePersistence {
    /// Values can be persisted in accepted storage positions.
    Persistable,
    /// Values are valid only in transient accepted positions.
    Transient,
}

/// The mutability contract of a value type.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueTypeMutability {
    /// Values have immutable value semantics.
    Immutable,
}

/// One immutable, persistable enum value type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnumTypeDefinition {
    id: TypeId,
    name: QualifiedSemanticName,
    labels: Vec<String>,
}

impl EnumTypeDefinition {
    /// Creates an enum type with labels in their semantic declaration order.
    pub fn new(
        id: TypeId,
        name: QualifiedSemanticName,
        labels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            id,
            name,
            labels: labels.into_iter().map(Into::into).collect(),
        }
    }

    /// Returns this type's stable identity.
    pub const fn id(&self) -> TypeId {
        self.id
    }

    /// Returns this type's canonical qualified name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns labels in their semantic declaration order.
    pub fn labels(&self) -> &[String] {
        &self.labels
    }
}

/// One resolved field of a named immutable record value type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordValueFieldDefinition {
    id: FieldId,
    name: String,
    ordinal: u32,
    resolved_type: ResolvedType,
}

impl RecordValueFieldDefinition {
    /// Creates a record value field from resolved semantic data.
    pub fn new(
        id: FieldId,
        name: impl Into<String>,
        ordinal: u32,
        resolved_type: ResolvedType,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            ordinal,
            resolved_type,
        }
    }

    /// Returns this field's stable identity.
    pub const fn id(&self) -> FieldId {
        self.id
    }

    /// Returns this field's resolved semantic name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns this field's zero-based declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns this field's resolved type descriptor.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }
}

/// One named immutable, persistable record value type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordValueTypeDefinition {
    id: TypeId,
    name: QualifiedSemanticName,
    fields: Vec<RecordValueFieldDefinition>,
}

impl RecordValueTypeDefinition {
    /// Creates a record value type with fields in declaration order.
    pub fn new(
        id: TypeId,
        name: QualifiedSemanticName,
        fields: Vec<RecordValueFieldDefinition>,
    ) -> Self {
        Self { id, name, fields }
    }

    /// Returns this type's stable identity.
    pub const fn id(&self) -> TypeId {
        self.id
    }

    /// Returns this type's canonical qualified name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns this type's fixed immutable value contract.
    pub const fn mutability(&self) -> ValueTypeMutability {
        ValueTypeMutability::Immutable
    }

    /// Returns this type's fixed durable storage contract.
    pub const fn persistence(&self) -> ValueTypePersistence {
        ValueTypePersistence::Persistable
    }

    /// Returns fields in declaration ordinal order.
    pub fn fields(&self) -> &[RecordValueFieldDefinition] {
        &self.fields
    }

    /// Finds a field by its exact resolved semantic name.
    pub fn field_by_name(&self, name: &str) -> Option<&RecordValueFieldDefinition> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Finds a field by its stable identity.
    pub fn field_by_id(&self, id: FieldId) -> Option<&RecordValueFieldDefinition> {
        self.fields.iter().find(|field| field.id == id)
    }
}

/// One resolved catalogue value type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValueTypeDefinition {
    id: TypeId,
    name: QualifiedSemanticName,
    kind: ValueTypeKind,
    mutability: ValueTypeMutability,
    persistence: ValueTypePersistence,
    representation_contract: String,
}

impl ValueTypeDefinition {
    /// Creates a primitive value type with its kernel representation contract.
    pub fn primitive(
        id: TypeId,
        name: QualifiedSemanticName,
        mutability: ValueTypeMutability,
        persistence: ValueTypePersistence,
        representation_contract: impl Into<String>,
    ) -> Self {
        Self {
            id,
            name,
            kind: ValueTypeKind::Primitive,
            mutability,
            persistence,
            representation_contract: representation_contract.into(),
        }
    }

    /// Returns this type's stable identity.
    pub const fn id(&self) -> TypeId {
        self.id
    }

    /// Returns this type's canonical qualified name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns this type's representation category.
    pub const fn kind(&self) -> ValueTypeKind {
        self.kind
    }

    /// Returns this type's mutability contract.
    pub const fn mutability(&self) -> ValueTypeMutability {
        self.mutability
    }

    /// Returns whether this type can be stored durably.
    pub const fn persistence(&self) -> ValueTypePersistence {
        self.persistence
    }

    /// Returns the versioned kernel representation contract.
    pub fn representation_contract(&self) -> &str {
        &self.representation_contract
    }
}

/// A definition in the public catalogue type family.
///
/// A catalogue snapshot owns definitions. This view preserves that ownership
/// and provides one common interface for object and value categories.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeDefinition<'a> {
    /// A durable object type.
    Object(&'a ObjectTypeDefinition),
    /// A primitive value type.
    Value(&'a ValueTypeDefinition),
    /// A named immutable record value type.
    RecordValue(&'a RecordValueTypeDefinition),
    /// An enum value type.
    Enum(&'a EnumTypeDefinition),
}

impl<'a> TypeDefinition<'a> {
    /// Returns this definition's stable type identity.
    pub const fn id(self) -> TypeId {
        match self {
            Self::Object(definition) => definition.id(),
            Self::Value(definition) => definition.id(),
            Self::RecordValue(definition) => definition.id(),
            Self::Enum(definition) => definition.id(),
        }
    }

    /// Returns this definition's canonical qualified name.
    pub fn name(self) -> &'a QualifiedSemanticName {
        match self {
            Self::Object(definition) => definition.name(),
            Self::Value(definition) => definition.name(),
            Self::RecordValue(definition) => definition.name(),
            Self::Enum(definition) => definition.name(),
        }
    }

    /// Returns this definition's category.
    pub const fn kind(self) -> TypeDefinitionKind {
        match self {
            Self::Object(_) => TypeDefinitionKind::Object,
            Self::Value(_) => TypeDefinitionKind::Value,
            Self::RecordValue(_) => TypeDefinitionKind::Value,
            Self::Enum(_) => TypeDefinitionKind::Enum,
        }
    }

    /// Returns this definition as an object type, when it is one.
    pub const fn as_object(self) -> Option<&'a ObjectTypeDefinition> {
        match self {
            Self::Object(definition) => Some(definition),
            Self::Value(_) | Self::RecordValue(_) | Self::Enum(_) => None,
        }
    }

    /// Returns this definition as a primitive value type, when it is one.
    pub const fn as_value(self) -> Option<&'a ValueTypeDefinition> {
        match self {
            Self::Object(_) | Self::RecordValue(_) | Self::Enum(_) => None,
            Self::Value(definition) => Some(definition),
        }
    }

    /// Returns this definition as a primitive value type, when it is one.
    pub const fn as_primitive_value(self) -> Option<&'a ValueTypeDefinition> {
        self.as_value()
    }

    /// Returns this definition as a record value type, when it is one.
    pub const fn as_record_value(self) -> Option<&'a RecordValueTypeDefinition> {
        match self {
            Self::Object(_) | Self::Value(_) | Self::Enum(_) => None,
            Self::RecordValue(definition) => Some(definition),
        }
    }

    /// Returns this definition as an enum type, when it is one.
    pub const fn as_enum(self) -> Option<&'a EnumTypeDefinition> {
        match self {
            Self::Object(_) | Self::Value(_) | Self::RecordValue(_) => None,
            Self::Enum(definition) => Some(definition),
        }
    }
}

/// One normalised standard-prelude type spelling.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PreludeTypeName {
    words: Vec<String>,
}

impl PreludeTypeName {
    /// Creates a prelude spelling from its SQL keyword words.
    pub fn new(
        words: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, PreludeTypeNameError> {
        let words = words
            .into_iter()
            .map(Into::into)
            .map(|word: String| word.to_ascii_lowercase())
            .collect::<Vec<_>>();

        if words.is_empty() {
            return Err(PreludeTypeNameError::EmptyName);
        }
        for (index, word) in words.iter().enumerate() {
            if word.is_empty() {
                return Err(PreludeTypeNameError::EmptyWord { index });
            }
            if !is_unquoted_word(word) {
                return Err(PreludeTypeNameError::InvalidWord { index });
            }
        }

        Ok(Self { words })
    }

    /// Returns normalised keyword words in source order.
    pub fn words(&self) -> &[String] {
        &self.words
    }
}

fn is_unquoted_word(word: &str) -> bool {
    let mut characters = word.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

impl fmt::Display for PreludeTypeName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.words.join(" "))
    }
}

/// A closed type-name namespace used for type lookup.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TypeLookupName {
    /// A canonical primary name or schema-qualified binding.
    Qualified(QualifiedSemanticName),
    /// A standard-prelude keyword spelling.
    Prelude(PreludeTypeName),
}

impl TypeLookupName {
    /// Creates a lookup key for one qualified semantic name.
    pub const fn qualified(name: QualifiedSemanticName) -> Self {
        Self::Qualified(name)
    }

    /// Creates a lookup key for one standard-prelude spelling.
    pub const fn prelude(name: PreludeTypeName) -> Self {
        Self::Prelude(name)
    }

    /// Returns the namespace that makes this name available.
    pub const fn kind(&self) -> TypeBindingKind {
        match self {
            Self::Qualified(_) => TypeBindingKind::Qualified,
            Self::Prelude(_) => TypeBindingKind::Prelude,
        }
    }
}

impl fmt::Display for TypeLookupName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Qualified(name) => name.fmt(formatter),
            Self::Prelude(name) => name.fmt(formatter),
        }
    }
}

/// The source namespace that introduces a type binding.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeBindingKind {
    /// A binding whose name is qualified by a schema namespace.
    Qualified,
    /// A binding whose name is available in the standard prelude.
    Prelude,
}

impl TypeBindingKind {
    const fn discriminator(self) -> u8 {
        match self {
            Self::Qualified => 1,
            Self::Prelude => 2,
        }
    }
}

/// Another source name for one existing type identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeBinding {
    id: TypeBindingId,
    name: TypeLookupName,
    target: TypeId,
}

impl TypeBinding {
    /// Creates a direct qualified binding to one existing type identity.
    pub fn qualified(
        name: QualifiedSemanticName,
        target: TypeId,
    ) -> Result<Self, TypeBindingError> {
        if name.parts().len() < 2 {
            return Err(TypeBindingError::QualifiedNameIsNotQualified { name });
        }
        Self::new(TypeLookupName::qualified(name), target)
    }

    /// Creates a direct standard-prelude binding to one existing type identity.
    pub fn prelude(name: PreludeTypeName, target: TypeId) -> Result<Self, TypeBindingError> {
        Self::new(TypeLookupName::prelude(name), target)
    }

    fn new(name: TypeLookupName, target: TypeId) -> Result<Self, TypeBindingError> {
        let id = binding_id(&name)?;
        Ok(Self { id, name, target })
    }

    /// Returns this binding's stable derived identity.
    pub const fn id(&self) -> TypeBindingId {
        self.id
    }

    /// Returns the namespace that makes this binding available.
    pub const fn kind(&self) -> TypeBindingKind {
        self.name.kind()
    }

    /// Returns this binding's closed lookup name.
    pub fn name(&self) -> &TypeLookupName {
        &self.name
    }

    /// Returns the direct target type identity.
    pub const fn target(&self) -> TypeId {
        self.target
    }
}

fn binding_id(name: &TypeLookupName) -> Result<TypeBindingId, TypeBindingError> {
    let words = match name {
        TypeLookupName::Qualified(name) => name.parts(),
        TypeLookupName::Prelude(name) => name.words(),
    };
    let word_count = u32::try_from(words.len())
        .map_err(|_| TypeBindingError::WordCountExceedsU32 { count: words.len() })?;

    let mut hasher = Sha256::new();
    hasher.update(TYPE_BINDING_ID_DOMAIN);
    hasher.update([name.kind().discriminator()]);
    hasher.update(word_count.to_be_bytes());
    for (index, word) in words.iter().enumerate() {
        let length =
            u32::try_from(word.len()).map_err(|_| TypeBindingError::WordLengthExceedsU32 {
                index,
                length: word.len(),
            })?;
        hasher.update(length.to_be_bytes());
        hasher.update(word.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    Ok(TypeBindingId::from_bytes(bytes))
}

/// An error returned when a type binding cannot use its name as an identity.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeBindingError {
    /// A qualified binding requires a schema-qualified name.
    QualifiedNameIsNotQualified {
        /// The invalid name.
        name: QualifiedSemanticName,
    },
    /// The number of name words exceeds the canonical identity framing limit.
    WordCountExceedsU32 {
        /// The unrepresentable word count.
        count: usize,
    },
    /// One name word exceeds the canonical identity framing limit.
    WordLengthExceedsU32 {
        /// The zero-based word position.
        index: usize,
        /// The unrepresentable word length.
        length: usize,
    },
}

impl std::fmt::Display for TypeBindingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QualifiedNameIsNotQualified { name } => {
                write!(
                    formatter,
                    "qualified type binding {name} has no schema namespace"
                )
            }
            Self::WordCountExceedsU32 { .. } => {
                formatter.write_str("type binding name has too many words")
            }
            Self::WordLengthExceedsU32 { index, .. } => {
                write!(formatter, "type binding name word {index} is too long")
            }
        }
    }
}

impl std::error::Error for TypeBindingError {}

/// An error returned when standard-prelude keyword words cannot form one name.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreludeTypeNameError {
    /// A prelude name requires at least one keyword word.
    EmptyName,
    /// A prelude name cannot contain an empty keyword word.
    EmptyWord {
        /// The zero-based position of the invalid word.
        index: usize,
    },
    /// One word is not one unquoted SQL keyword token.
    InvalidWord {
        /// The zero-based position of the invalid word.
        index: usize,
    },
}

impl std::fmt::Display for PreludeTypeNameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => formatter.write_str("prelude type name has no words"),
            Self::EmptyWord { index } => {
                write!(formatter, "prelude type name word {index} is empty")
            }
            Self::InvalidWord { index } => write!(
                formatter,
                "prelude type name word {index} is not an unquoted SQL word"
            ),
        }
    }
}

impl std::error::Error for PreludeTypeNameError {}

#[cfg(test)]
mod tests {
    use super::{PreludeTypeName, PreludeTypeNameError, TypeBinding, TypeBindingKind};
    use crate::{TypeId, catalogue::QualifiedSemanticName};

    #[test]
    fn qualified_binding_identity_uses_the_versioned_name_contract() {
        let name = QualifiedSemanticName::new(["std", "boolean"]).unwrap();
        let binding = TypeBinding::qualified(name, TypeId::from_bytes([1; 16])).unwrap();

        assert_eq!(binding.kind(), TypeBindingKind::Qualified);
        assert_eq!(
            binding.id().to_bytes(),
            [
                0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd,
                0x4d, 0x31,
            ]
        );
    }

    #[test]
    fn prelude_binding_identity_uses_its_distinct_kind_byte() {
        let name = PreludeTypeName::new(["CHARACTER", "LARGE", "OBJECT"]).unwrap();
        let binding = TypeBinding::prelude(name, TypeId::from_bytes([1; 16])).unwrap();

        assert_eq!(binding.kind(), TypeBindingKind::Prelude);
        assert_eq!(
            binding.id().to_bytes(),
            [
                0xf6, 0xd0, 0xd3, 0xb6, 0x31, 0x1b, 0x6b, 0xdc, 0xe6, 0x01, 0xd3, 0xcf, 0xc3, 0xa6,
                0x89, 0x1a,
            ]
        );
    }

    #[test]
    fn prelude_words_reject_ambiguous_or_non_token_segmentation() {
        for (words, index) in [
            (vec!["CHARACTER LARGE", "OBJECT"], 0),
            (vec!["CHARACTER", "LARGE-OBJECT"], 1),
            (vec!["CHARACTER", ".", "OBJECT"], 1),
            (vec!["1CHARACTER", "LARGE", "OBJECT"], 0),
        ] {
            assert_eq!(
                PreludeTypeName::new(words),
                Err(PreludeTypeNameError::InvalidWord { index })
            );
        }
    }
}
