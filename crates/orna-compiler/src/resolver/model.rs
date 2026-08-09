//! Checked-result values produced by semantic resolution.

use std::{collections::HashMap, error::Error, fmt, hash::Hash};

use orna_core::{
    CatalogueRevisionId, FieldId, TypeId,
    catalogue::{CatalogueSnapshot, FunctionDomain, OnDeleteAction, QualifiedSemanticName},
    revision::DefinitionReferenceKind,
    types::{ResolvedType, StandardScalar},
};

use crate::{
    CompilerDiagnostic, ParseReport, SourceLocation,
    mutation::{MutationCatalogue, MutationField},
    relational::{DistinctQueryIr, IdentitySelectedQueryIr, RelationalQueryIr},
};

use super::{
    CheckedExpressionId, CheckedFieldId, CheckedFunctionId, CheckedParameterId, CheckedSchemaId,
    CheckedTypeId,
};

/// A resolved semantic type whose identities belong to the checking context.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SemanticType<T> {
    /// A standard scalar type.
    Scalar(StandardScalar),
    /// A resolved non-scalar named type.
    Named(T),
    /// A reference to a resolved object type.
    Reference {
        /// The referenced object type.
        target: T,
    },
}

impl<T> SemanticType<T> {
    /// Creates a standard scalar type.
    pub const fn scalar(scalar: StandardScalar) -> Self {
        Self::Scalar(scalar)
    }

    /// Creates a typed object reference.
    pub const fn reference(target: T) -> Self {
        Self::Reference { target }
    }
}

impl SemanticType<TypeId> {
    /// Converts a durable core type into the compiler identity domain.
    pub(crate) const fn from_core(resolved_type: ResolvedType) -> Self {
        match resolved_type {
            ResolvedType::Scalar(scalar) => Self::Scalar(scalar),
            ResolvedType::Named(type_id) => Self::Named(type_id),
            ResolvedType::Reference { target } => Self::Reference { target },
        }
    }

    /// Converts a durable compiler type back into the core representation.
    pub(crate) const fn into_core(self) -> ResolvedType {
        match self {
            Self::Scalar(scalar) => ResolvedType::Scalar(scalar),
            Self::Named(type_id) => ResolvedType::Named(type_id),
            Self::Reference { target } => ResolvedType::Reference { target },
        }
    }
}

/// One query-visible field in a catalogue used during semantic checking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct QueryField<T, F> {
    id: F,
    resolved_type: SemanticType<T>,
    nullable: bool,
}

impl<T, F> QueryField<T, F> {
    /// Creates one query-visible field.
    pub(crate) fn new(id: F, resolved_type: SemanticType<T>, nullable: bool) -> Self {
        Self {
            id,
            resolved_type,
            nullable,
        }
    }

    /// Returns the field identity.
    pub(crate) const fn id(&self) -> F
    where
        F: Copy,
    {
        self.id
    }

    /// Returns the resolved field type.
    pub(crate) const fn semantic_type(&self) -> SemanticType<T>
    where
        T: Copy,
    {
        self.resolved_type
    }

    /// Reports whether the field can contain null.
    pub(crate) const fn nullable(&self) -> bool {
        self.nullable
    }
}

/// The name and field data for one object type in a query catalogue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QueryObjectType<T, F> {
    id: T,
    name: QualifiedSemanticName,
    fields: Vec<(String, QueryField<T, F>)>,
}

impl<T, F> QueryObjectType<T, F> {
    /// Creates one query-visible object type.
    pub(crate) fn new(
        id: T,
        name: QualifiedSemanticName,
        fields: Vec<(String, QueryField<T, F>)>,
    ) -> Self {
        Self { id, name, fields }
    }

    /// Returns the type identity.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn id(&self) -> T
    where
        T: Copy,
    {
        self.id
    }

    /// Returns the resolved qualified type name.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns fields in deterministic declaration order.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn fields(&self) -> &[(String, QueryField<T, F>)] {
        &self.fields
    }
}

/// The query lookup contract shared by durable and resolver-local catalogues.
pub(crate) trait QueryCatalogue<T, F> {
    /// Finds an object type identity by its exact resolved qualified name.
    fn object_type_id_by_name(&self, name: &QualifiedSemanticName) -> Option<T>;

    /// Finds an object type name by its identity.
    fn object_type_name_by_id(&self, id: T) -> Option<&QualifiedSemanticName>;

    /// Finds an exact field name on one object type.
    fn field_by_name(&self, owner: T, name: &str) -> Option<QueryField<T, F>>;

    /// Finds a field identity on one object type.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "checked reference evidence uses identity lookup in the next slice"
        )
    )]
    fn field_by_id(&self, owner: T, id: F) -> Option<QueryField<T, F>>;
}

/// A deterministic, validated catalogue for resolver-local identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolutionCatalogue<T: Eq + Hash, F: Eq + Hash> {
    object_types: Vec<QueryObjectType<T, F>>,
    object_type_indices_by_name: HashMap<QualifiedSemanticName, usize>,
    object_type_indices_by_id: HashMap<T, usize>,
    field_indices_by_name: HashMap<(T, String), (usize, usize)>,
    field_indices_by_id: HashMap<(T, F), (usize, usize)>,
}

impl<T, F> ResolutionCatalogue<T, F>
where
    T: Copy + Eq + Hash,
    F: Copy + Eq + Hash,
{
    /// Validates and creates a deterministic query catalogue.
    pub(crate) fn new(
        object_types: Vec<QueryObjectType<T, F>>,
    ) -> Result<Self, ResolutionCatalogueError<T, F>> {
        let mut object_type_indices_by_name = HashMap::with_capacity(object_types.len());
        let mut object_type_indices_by_id = HashMap::with_capacity(object_types.len());
        let mut field_indices_by_name = HashMap::new();
        let mut field_indices_by_id = HashMap::new();

        for (type_index, object_type) in object_types.iter().enumerate() {
            if object_type_indices_by_name
                .insert(object_type.name.clone(), type_index)
                .is_some()
            {
                return Err(ResolutionCatalogueError::DuplicateObjectTypeName {
                    name: object_type.name.clone(),
                });
            }
            if object_type_indices_by_id
                .insert(object_type.id, type_index)
                .is_some()
            {
                return Err(ResolutionCatalogueError::DuplicateObjectTypeId { id: object_type.id });
            }

            for (field_index, (field_name, field)) in object_type.fields.iter().enumerate() {
                if field_name.is_empty() {
                    return Err(ResolutionCatalogueError::EmptyFieldName {
                        owner: object_type.id,
                        field: field.id,
                    });
                }
                if field_indices_by_name
                    .insert(
                        (object_type.id, field_name.clone()),
                        (type_index, field_index),
                    )
                    .is_some()
                {
                    return Err(ResolutionCatalogueError::DuplicateFieldName {
                        owner: object_type.id,
                        name: field_name.clone(),
                    });
                }
                if field_indices_by_id
                    .insert((object_type.id, field.id), (type_index, field_index))
                    .is_some()
                {
                    return Err(ResolutionCatalogueError::DuplicateFieldId {
                        owner: object_type.id,
                        id: field.id,
                    });
                }
            }
        }

        for object_type in &object_types {
            for (_, field) in &object_type.fields {
                if let SemanticType::Reference { target } = field.resolved_type
                    && !object_type_indices_by_id.contains_key(&target)
                {
                    return Err(ResolutionCatalogueError::UnknownReferenceTarget {
                        owner: object_type.id,
                        field: field.id,
                        target,
                    });
                }
            }
        }

        Ok(Self {
            object_types,
            object_type_indices_by_name,
            object_type_indices_by_id,
            field_indices_by_name,
            field_indices_by_id,
        })
    }

    /// Returns object types in deterministic construction order.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn object_types(&self) -> &[QueryObjectType<T, F>] {
        &self.object_types
    }
}

impl<T, F> QueryCatalogue<T, F> for ResolutionCatalogue<T, F>
where
    T: Copy + Eq + Hash,
    F: Copy + Eq + Hash,
{
    fn object_type_id_by_name(&self, name: &QualifiedSemanticName) -> Option<T> {
        self.object_type_indices_by_name
            .get(name)
            .map(|index| self.object_types[*index].id)
    }

    fn object_type_name_by_id(&self, id: T) -> Option<&QualifiedSemanticName> {
        self.object_type_indices_by_id
            .get(&id)
            .map(|index| &self.object_types[*index].name)
    }

    fn field_by_name(&self, owner: T, name: &str) -> Option<QueryField<T, F>> {
        self.field_indices_by_name
            .get(&(owner, name.to_owned()))
            .map(|(type_index, field_index)| self.object_types[*type_index].fields[*field_index].1)
    }

    fn field_by_id(&self, owner: T, id: F) -> Option<QueryField<T, F>> {
        self.field_indices_by_id
            .get(&(owner, id))
            .map(|(type_index, field_index)| self.object_types[*type_index].fields[*field_index].1)
    }
}

impl<T, F> MutationCatalogue<T, F> for ResolutionCatalogue<T, F>
where
    T: Copy + Eq + Hash,
    F: Copy + Eq + Hash,
{
    fn object_type_id_by_name(&self, name: &QualifiedSemanticName) -> Option<T> {
        QueryCatalogue::object_type_id_by_name(self, name)
    }

    fn field_by_name(&self, owner: T, name: &str) -> Option<MutationField<T, F>> {
        QueryCatalogue::field_by_name(self, owner, name)
            .map(|field| MutationField::new(field.id(), field.semantic_type(), field.nullable()))
    }

    fn visit_fields(&self, owner: T, visitor: &mut dyn FnMut(&str, MutationField<T, F>)) {
        let Some(index) = self.object_type_indices_by_id.get(&owner) else {
            return;
        };
        for (name, field) in &self.object_types[*index].fields {
            visitor(
                name,
                MutationField::new(field.id(), field.semantic_type(), field.nullable()),
            );
        }
    }
}

/// A validation error for a resolver-local query catalogue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolutionCatalogueError<T, F> {
    /// More than one object type has the same resolved qualified name.
    DuplicateObjectTypeName {
        /// The repeated name.
        name: QualifiedSemanticName,
    },
    /// More than one object type has the same identity.
    DuplicateObjectTypeId {
        /// The repeated identity.
        id: T,
    },
    /// A query field has no semantic name.
    EmptyFieldName {
        /// The owning object type.
        owner: T,
        /// The invalid field identity.
        field: F,
    },
    /// More than one field on an object type has the same semantic name.
    DuplicateFieldName {
        /// The owning object type.
        owner: T,
        /// The repeated name.
        name: String,
    },
    /// More than one field on an object type has the same identity.
    DuplicateFieldId {
        /// The owning object type.
        owner: T,
        /// The repeated identity.
        id: F,
    },
    /// A REF field targets an object type outside this catalogue.
    UnknownReferenceTarget {
        /// The owning object type.
        owner: T,
        /// The field with the invalid target.
        field: F,
        /// The missing target identity.
        target: T,
    },
}

impl<T: fmt::Debug, F: fmt::Debug> fmt::Display for ResolutionCatalogueError<T, F> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateObjectTypeName { name } => {
                write!(formatter, "duplicate object type name {name}")
            }
            Self::DuplicateObjectTypeId { id } => {
                write!(formatter, "duplicate object type identity {id:?}")
            }
            Self::EmptyFieldName { owner, field } => {
                write!(formatter, "empty field name for {field:?} on {owner:?}")
            }
            Self::DuplicateFieldName { owner, name } => {
                write!(formatter, "duplicate field name {name} on {owner:?}")
            }
            Self::DuplicateFieldId { owner, id } => {
                write!(formatter, "duplicate field identity {id:?} on {owner:?}")
            }
            Self::UnknownReferenceTarget {
                owner,
                field,
                target,
            } => write!(
                formatter,
                "field {field:?} on {owner:?} references unknown object type {target:?}"
            ),
        }
    }
}

impl<T: fmt::Debug, F: fmt::Debug> Error for ResolutionCatalogueError<T, F> {}

impl QueryCatalogue<TypeId, FieldId> for CatalogueSnapshot {
    fn object_type_id_by_name(&self, name: &QualifiedSemanticName) -> Option<TypeId> {
        self.object_type_by_name(name)
            .map(|object_type| object_type.id())
    }

    fn object_type_name_by_id(&self, id: TypeId) -> Option<&QualifiedSemanticName> {
        self.object_type_by_id(id)
            .map(|object_type| object_type.name())
    }

    fn field_by_name(&self, owner: TypeId, name: &str) -> Option<QueryField<TypeId, FieldId>> {
        self.object_type_by_id(owner)
            .and_then(|object_type| object_type.field_by_name(name))
            .map(query_field_from_core)
    }

    fn field_by_id(&self, owner: TypeId, id: FieldId) -> Option<QueryField<TypeId, FieldId>> {
        self.object_type_by_id(owner)
            .and_then(|object_type| object_type.field_by_id(id))
            .map(query_field_from_core)
    }
}

fn query_field_from_core(
    field: &orna_core::catalogue::FieldDefinition,
) -> QueryField<TypeId, FieldId> {
    QueryField::new(
        field.id(),
        SemanticType::from_core(field.resolved_type()),
        field.nullable(),
    )
}

#[cfg(test)]
mod tests {
    use orna_core::{
        FieldId, TypeId,
        catalogue::QualifiedSemanticName,
        types::{ResolvedType, StandardScalar},
    };

    use super::{
        QueryCatalogue, QueryField, QueryObjectType, ResolutionCatalogue, ResolutionCatalogueError,
        SemanticType,
    };

    #[test]
    fn resolution_catalogue_indexes_exact_names_and_ids() {
        let task_type = TypeId::from_bytes([1; 16]);
        let task_field = FieldId::from_bytes([2; 16]);
        let catalogue = ResolutionCatalogue::new(vec![QueryObjectType::new(
            task_type,
            name(&["tasks", "task"]),
            vec![(
                "title".to_owned(),
                QueryField::new(
                    task_field,
                    SemanticType::scalar(StandardScalar::CharacterLargeObject),
                    true,
                ),
            )],
        )])
        .unwrap();

        assert_eq!(catalogue.object_types().len(), 1);
        assert_eq!(catalogue.object_types()[0].id(), task_type);
        assert_eq!(
            catalogue.object_types()[0].name(),
            &name(&["tasks", "task"])
        );
        assert_eq!(catalogue.object_types()[0].fields()[0].0, "title");
        assert_eq!(
            catalogue.object_type_id_by_name(&name(&["tasks", "task"])),
            Some(task_type)
        );
        assert_eq!(
            catalogue.object_type_name_by_id(task_type),
            Some(&name(&["tasks", "task"]))
        );
        let resolved_field = catalogue.field_by_name(task_type, "title").unwrap();
        assert_eq!(resolved_field.id(), task_field);
        assert_eq!(
            resolved_field.semantic_type(),
            SemanticType::scalar(StandardScalar::CharacterLargeObject)
        );
        assert!(resolved_field.nullable());
        assert_eq!(
            catalogue.field_by_id(task_type, task_field).unwrap().id(),
            task_field
        );
        assert_eq!(
            SemanticType::reference(task_type).into_core(),
            ResolvedType::Reference { target: task_type }
        );
    }

    #[test]
    fn resolution_catalogue_rejects_duplicate_checked_ids() {
        let type_id = TypeId::from_bytes([3; 16]);
        let field_id = FieldId::from_bytes([4; 16]);
        let error = ResolutionCatalogue::new(vec![
            QueryObjectType::<TypeId, FieldId>::new(type_id, name(&["tasks", "first"]), vec![]),
            QueryObjectType::<TypeId, FieldId>::new(type_id, name(&["tasks", "second"]), vec![]),
        ])
        .unwrap_err();
        assert_eq!(
            error,
            ResolutionCatalogueError::DuplicateObjectTypeId { id: type_id }
        );

        let error = ResolutionCatalogue::new(vec![QueryObjectType::new(
            type_id,
            name(&["tasks", "task"]),
            vec![
                (
                    "first".to_owned(),
                    QueryField::new(
                        field_id,
                        SemanticType::scalar(StandardScalar::Boolean),
                        false,
                    ),
                ),
                (
                    "second".to_owned(),
                    QueryField::new(
                        field_id,
                        SemanticType::scalar(StandardScalar::Boolean),
                        false,
                    ),
                ),
            ],
        )])
        .unwrap_err();
        assert_eq!(
            error,
            ResolutionCatalogueError::DuplicateFieldId {
                owner: type_id,
                id: field_id,
            }
        );
    }

    #[test]
    fn resolution_catalogue_rejects_dangling_reference_targets() {
        let owner = TypeId::from_bytes([5; 16]);
        let field = FieldId::from_bytes([6; 16]);
        let target = TypeId::from_bytes([7; 16]);

        let error = ResolutionCatalogue::new(vec![QueryObjectType::new(
            owner,
            name(&["tasks", "task"]),
            vec![(
                "assignee".to_owned(),
                QueryField::new(field, SemanticType::reference(target), true),
            )],
        )])
        .unwrap_err();

        assert_eq!(
            error,
            ResolutionCatalogueError::UnknownReferenceTarget {
                owner,
                field,
                target,
            }
        );
    }

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }
}

/// The value of a default expression accepted in this first compiler slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConstantValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Text(String),
}

/// A checked constant default expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedDefault {
    pub(super) id: CheckedExpressionId,
    pub(super) value: ConstantValue,
    pub(super) location: SourceLocation,
}

impl CheckedDefault {
    /// Returns the identity of this checked expression.
    pub const fn id(&self) -> CheckedExpressionId {
        self.id
    }

    /// Returns the checked constant value.
    pub fn value(&self) -> &ConstantValue {
        &self.value
    }

    /// Returns the location of the source expression.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked field definition without parser implementation values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedField {
    pub(super) id: CheckedFieldId,
    pub(super) name: String,
    pub(super) ordinal: u32,
    pub(super) semantic_type: SemanticType<CheckedTypeId>,
    pub(super) nullable: bool,
    pub(super) unique: bool,
    pub(super) default: Option<CheckedDefault>,
    pub(super) on_delete: Option<OnDeleteAction>,
    pub(super) location: SourceLocation,
}

impl CheckedField {
    /// Returns the identity of the field.
    pub const fn id(&self) -> CheckedFieldId {
        self.id
    }
    /// Returns the resolved field name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }
    /// Returns the checked semantic type.
    pub const fn semantic_type(&self) -> SemanticType<CheckedTypeId> {
        self.semantic_type
    }
    /// Reports whether the field permits null.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }
    /// Reports whether the field is unique.
    pub const fn unique(&self) -> bool {
        self.unique
    }
    /// Returns the checked default expression, when declared.
    pub fn default(&self) -> Option<&CheckedDefault> {
        self.default.as_ref()
    }
    /// Returns the resolved delete action, when declared.
    pub const fn on_delete(&self) -> Option<OnDeleteAction> {
        self.on_delete
    }
    /// Returns the source location of the field declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked object type declaration without parser implementation values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedObjectType {
    pub(super) id: CheckedTypeId,
    pub(super) name: QualifiedSemanticName,
    pub(super) fields: Vec<CheckedField>,
    pub(super) location: SourceLocation,
}

/// One accepted field-name transition bound to a stable checked identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckedFieldRename {
    pub(crate) owner: CheckedTypeId,
    pub(crate) field: CheckedFieldId,
    pub(crate) old_name: String,
    pub(crate) new_name: String,
}

impl CheckedObjectType {
    /// Returns the identity of the object type.
    pub const fn id(&self) -> CheckedTypeId {
        self.id
    }
    /// Returns the resolved qualified type name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }
    /// Returns checked fields in declaration order.
    pub fn fields(&self) -> &[CheckedField] {
        &self.fields
    }
    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked source bundle ready for a later semantic-diff and apply stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedBundle {
    pub(super) base_catalogue_revision: CatalogueRevisionId,
    pub(super) schemas: Vec<CheckedSchema>,
    pub(super) object_types: Vec<CheckedObjectType>,
    pub(super) server_functions: Vec<CheckedServerFunction>,
    pub(super) client_functions: Vec<CheckedClientFunction>,
    pub(super) field_renames: Vec<CheckedFieldRename>,
}

impl CheckedBundle {
    /// Returns the immutable catalogue revision used for identity continuity.
    pub const fn base_catalogue_revision(&self) -> CatalogueRevisionId {
        self.base_catalogue_revision
    }

    /// Returns submitted schema declarations in source order.
    pub fn schemas(&self) -> &[CheckedSchema] {
        &self.schemas
    }

    /// Returns submitted object declarations in source order.
    pub fn object_types(&self) -> &[CheckedObjectType] {
        &self.object_types
    }

    /// Returns submitted checked SERVER functions in source order.
    pub fn server_functions(&self) -> &[CheckedServerFunction] {
        &self.server_functions
    }

    /// Returns submitted checked CLIENT functions in source order.
    pub fn client_functions(&self) -> &[CheckedClientFunction] {
        &self.client_functions
    }

    pub(crate) fn field_renames(&self) -> &[CheckedFieldRename] {
        &self.field_renames
    }
}

/// A checked CLIENT function body.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckedClientFunctionBody {
    /// A Boolean literal returned by the function.
    BooleanLiteral {
        /// The resolved Boolean value.
        value: bool,
        /// The exact source location of the literal.
        location: SourceLocation,
    },
}

impl CheckedClientFunctionBody {
    /// Returns the closed Boolean body data.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "preparation reads the checked CLIENT body")
    )]
    pub(crate) fn as_boolean_literal(&self) -> Option<(bool, &SourceLocation)> {
        match self {
            Self::BooleanLiteral { value, location } => Some((*value, location)),
        }
    }
}

/// A checked CLIENT function with a closed Boolean constant body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedClientFunction {
    pub(super) id: CheckedFunctionId,
    pub(super) name: QualifiedSemanticName,
    pub(super) domain: FunctionDomain,
    pub(super) parameters: Vec<CheckedServerFunctionParameter>,
    pub(super) return_type: SemanticType<CheckedTypeId>,
    pub(super) security: orna_core::catalogue::FunctionSecurity,
    pub(super) transaction: Option<orna_core::catalogue::FunctionTransaction>,
    pub(super) volatility: orna_core::catalogue::FunctionVolatility,
    pub(super) location: SourceLocation,
    pub(super) body: CheckedClientFunctionBody,
    pub(super) references: Vec<CheckedDefinitionReference>,
}

impl CheckedClientFunction {
    /// Returns the checked function identity.
    pub const fn id(&self) -> CheckedFunctionId {
        self.id
    }

    /// Returns the resolved function name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns the runtime domain of the checked function.
    pub const fn domain(&self) -> FunctionDomain {
        self.domain
    }

    /// Returns checked parameters in declaration order.
    pub fn parameters(&self) -> &[CheckedServerFunctionParameter] {
        &self.parameters
    }

    /// Returns the checked scalar return type.
    pub const fn return_type(&self) -> SemanticType<CheckedTypeId> {
        self.return_type
    }

    /// Returns the function security context mode.
    pub const fn security(&self) -> orna_core::catalogue::FunctionSecurity {
        self.security
    }

    /// Returns the declared transaction mode.
    pub const fn transaction(&self) -> Option<orna_core::catalogue::FunctionTransaction> {
        self.transaction
    }

    /// Returns the declared volatility mode.
    pub const fn volatility(&self) -> orna_core::catalogue::FunctionVolatility {
        self.volatility
    }

    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }

    /// Returns the Boolean body value and its exact source location.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "preparation reads the checked CLIENT body")
    )]
    pub(crate) fn boolean_body(&self) -> Option<(bool, &SourceLocation)> {
        self.body.as_boolean_literal()
    }

    /// Returns checked definition references in source-resolution order.
    pub fn references(&self) -> &[CheckedDefinitionReference] {
        &self.references
    }
}

/// One checked SERVER function parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedServerFunctionParameter {
    pub(super) id: CheckedParameterId,
    pub(super) name: String,
    pub(super) ordinal: u32,
    pub(super) semantic_type: SemanticType<CheckedTypeId>,
    pub(super) location: SourceLocation,
}

impl CheckedServerFunctionParameter {
    /// Returns the identity of the parameter.
    pub const fn id(&self) -> CheckedParameterId {
        self.id
    }

    /// Returns the resolved parameter name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns the checked semantic type.
    pub const fn semantic_type(&self) -> SemanticType<CheckedTypeId> {
        self.semantic_type
    }

    /// Returns the full parameter declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// One checked `ROWS` return column.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedServerFunctionReturnColumn {
    pub(super) name: String,
    pub(super) ordinal: u32,
    pub(super) semantic_type: SemanticType<CheckedTypeId>,
    pub(super) location: SourceLocation,
}

impl CheckedServerFunctionReturnColumn {
    /// Returns the resolved return-column name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns the checked semantic type.
    pub const fn semantic_type(&self) -> SemanticType<CheckedTypeId> {
        self.semantic_type
    }

    /// Returns the full return-column declaration location.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked definition target referenced by one declaration or query.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CheckedDefinitionReferenceTarget {
    /// A checked object type.
    ObjectType(CheckedTypeId),
    /// A checked field on an object type.
    Field {
        /// The owning checked object type.
        owner: CheckedTypeId,
        /// The checked field identity.
        field: CheckedFieldId,
    },
    /// A checked SERVER function.
    Function(CheckedFunctionId),
    /// A checked parameter on a SERVER function.
    Parameter {
        /// The owning checked SERVER function.
        owner: CheckedFunctionId,
        /// The checked parameter identity.
        parameter: CheckedParameterId,
    },
    /// A checked expression.
    Expression(CheckedExpressionId),
}

/// One ordered checked definition reference with its exact source location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedDefinitionReference {
    pub(super) target: CheckedDefinitionReferenceTarget,
    pub(super) kind: DefinitionReferenceKind,
    pub(super) location: SourceLocation,
}

impl CheckedDefinitionReference {
    /// Returns the checked target definition.
    pub const fn target(&self) -> CheckedDefinitionReferenceTarget {
        self.target
    }

    /// Returns the resolved reference category.
    pub const fn kind(&self) -> DefinitionReferenceKind {
        self.kind
    }

    /// Returns the exact source location of this reference.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked SERVER function body with its source-free execution plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckedServerFunctionBody {
    /// A checked relational query body.
    Query(RelationalQueryIr<CheckedTypeId, CheckedFieldId>),
    /// A checked parameter-free `SELECT DISTINCT` query body.
    DistinctQuery(DistinctQueryIr<CheckedTypeId, CheckedFieldId>),
    /// A checked SERVER query with one fixed identity selector.
    IdentitySelectedQuery(
        IdentitySelectedQueryIr<
            CheckedTypeId,
            CheckedFieldId,
            CheckedFunctionId,
            CheckedParameterId,
        >,
    ),
    /// A checked single-object mutation body.
    Mutation(
        crate::mutation::MutationPlanIr<
            CheckedTypeId,
            CheckedFieldId,
            CheckedFunctionId,
            CheckedParameterId,
        >,
    ),
    /// A checked single-object DELETE body.
    Delete(crate::mutation::DeletePlanIr<CheckedTypeId, CheckedFunctionId, CheckedParameterId>),
}

/// A checked SERVER function with an Orna-owned execution plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedServerFunction {
    pub(super) id: CheckedFunctionId,
    pub(super) name: QualifiedSemanticName,
    pub(super) parameters: Vec<CheckedServerFunctionParameter>,
    pub(super) return_columns: Vec<CheckedServerFunctionReturnColumn>,
    pub(super) security: orna_core::catalogue::FunctionSecurity,
    pub(super) transaction: Option<orna_core::catalogue::FunctionTransaction>,
    pub(super) volatility: orna_core::catalogue::FunctionVolatility,
    pub(super) location: SourceLocation,
    pub(super) body: CheckedServerFunctionBody,
    pub(super) references: Vec<CheckedDefinitionReference>,
}

impl CheckedServerFunction {
    /// Returns the checked function identity.
    pub const fn id(&self) -> CheckedFunctionId {
        self.id
    }

    /// Returns the resolved function name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns checked parameters in declaration order.
    pub fn parameters(&self) -> &[CheckedServerFunctionParameter] {
        &self.parameters
    }

    /// Returns checked `ROWS` return columns in declaration order.
    pub fn return_columns(&self) -> &[CheckedServerFunctionReturnColumn] {
        &self.return_columns
    }

    /// Returns the function security context mode.
    pub const fn security(&self) -> orna_core::catalogue::FunctionSecurity {
        self.security
    }

    /// Returns the declared transaction mode.
    pub const fn transaction(&self) -> Option<orna_core::catalogue::FunctionTransaction> {
        self.transaction
    }

    /// Returns the declared volatility mode.
    pub const fn volatility(&self) -> orna_core::catalogue::FunctionVolatility {
        self.volatility
    }

    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }

    /// Returns checked definition references in source-resolution order.
    pub fn references(&self) -> &[CheckedDefinitionReference] {
        &self.references
    }

    /// Returns the checked duplicate-preserving relational query when present.
    pub(crate) fn query_plan(&self) -> Option<&RelationalQueryIr<CheckedTypeId, CheckedFieldId>> {
        match &self.body {
            CheckedServerFunctionBody::Query(plan) => Some(plan),
            CheckedServerFunctionBody::DistinctQuery(_)
            | CheckedServerFunctionBody::IdentitySelectedQuery(_)
            | CheckedServerFunctionBody::Mutation(_)
            | CheckedServerFunctionBody::Delete(_) => None,
        }
    }

    /// Returns the checked DISTINCT query plan when the function has one.
    pub(crate) fn distinct_query_plan(
        &self,
    ) -> Option<&DistinctQueryIr<CheckedTypeId, CheckedFieldId>> {
        match &self.body {
            CheckedServerFunctionBody::DistinctQuery(plan) => Some(plan),
            CheckedServerFunctionBody::Query(_)
            | CheckedServerFunctionBody::IdentitySelectedQuery(_)
            | CheckedServerFunctionBody::Mutation(_)
            | CheckedServerFunctionBody::Delete(_) => None,
        }
    }

    /// Returns the checked identity-selected query plan when the function has one.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn identity_selected_query_plan(
        &self,
    ) -> Option<
        &IdentitySelectedQueryIr<
            CheckedTypeId,
            CheckedFieldId,
            CheckedFunctionId,
            CheckedParameterId,
        >,
    > {
        match &self.body {
            CheckedServerFunctionBody::IdentitySelectedQuery(plan) => Some(plan),
            CheckedServerFunctionBody::Query(_)
            | CheckedServerFunctionBody::DistinctQuery(_)
            | CheckedServerFunctionBody::Mutation(_)
            | CheckedServerFunctionBody::Delete(_) => None,
        }
    }

    /// Returns the checked INSERT or UPDATE plan when the function has that body.
    pub(crate) fn mutation_plan(
        &self,
    ) -> Option<
        &crate::mutation::MutationPlanIr<
            CheckedTypeId,
            CheckedFieldId,
            CheckedFunctionId,
            CheckedParameterId,
        >,
    > {
        match &self.body {
            CheckedServerFunctionBody::Query(_)
            | CheckedServerFunctionBody::DistinctQuery(_)
            | CheckedServerFunctionBody::IdentitySelectedQuery(_)
            | CheckedServerFunctionBody::Delete(_) => None,
            CheckedServerFunctionBody::Mutation(plan) => Some(plan),
        }
    }

    /// Returns the checked DELETE plan when the function has a DELETE body.
    pub(crate) fn delete_plan(
        &self,
    ) -> Option<&crate::mutation::DeletePlanIr<CheckedTypeId, CheckedFunctionId, CheckedParameterId>>
    {
        match &self.body {
            CheckedServerFunctionBody::Delete(plan) => Some(plan),
            CheckedServerFunctionBody::Query(_)
            | CheckedServerFunctionBody::DistinctQuery(_)
            | CheckedServerFunctionBody::IdentitySelectedQuery(_)
            | CheckedServerFunctionBody::Mutation(_) => None,
        }
    }
}

/// A checked logical schema declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedSchema {
    pub(super) id: CheckedSchemaId,
    pub(super) name: QualifiedSemanticName,
    pub(super) location: SourceLocation,
}

impl CheckedSchema {
    /// Returns the identity of the schema.
    pub const fn id(&self) -> CheckedSchemaId {
        self.id
    }

    /// Returns the resolved logical schema name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// The result of parsing and checking a source bundle.
#[derive(Clone, Debug)]
pub struct CheckReport {
    pub(super) parse_report: ParseReport,
    pub(super) diagnostics: Vec<CompilerDiagnostic>,
    pub(super) checked_bundle: Option<CheckedBundle>,
}

impl CheckReport {
    /// Returns the retained parse report on both success and failure.
    pub fn parse_report(&self) -> &ParseReport {
        &self.parse_report
    }
    /// Returns syntax and semantic diagnostics in source order.
    pub fn diagnostics(&self) -> &[CompilerDiagnostic] {
        &self.diagnostics
    }
    /// Returns checked Orna-owned definitions when checking succeeds.
    pub fn checked_bundle(&self) -> Option<&CheckedBundle> {
        self.checked_bundle.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn replace_checked_field_facts_for_test(
        &mut self,
        owner: CheckedTypeId,
        field: CheckedFieldId,
        semantic_type: SemanticType<CheckedTypeId>,
        nullable: bool,
        unique: bool,
    ) -> bool {
        let Some(checked) = self.checked_bundle.as_mut() else {
            return false;
        };
        let Some(field) = checked
            .object_types
            .iter_mut()
            .find(|object_type| object_type.id == owner)
            .and_then(|object_type| {
                object_type
                    .fields
                    .iter_mut()
                    .find(|candidate| candidate.id == field)
            })
        else {
            return false;
        };

        field.semantic_type = semantic_type;
        field.nullable = nullable;
        field.unique = unique;
        true
    }
}
