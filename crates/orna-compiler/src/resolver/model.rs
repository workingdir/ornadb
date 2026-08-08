//! Checked-result values produced by semantic resolution.

use std::{collections::HashMap, error::Error, fmt, hash::Hash};

use orna_core::{
    ExpressionId, FieldId, FunctionId, SchemaId, TypeId,
    catalogue::{CatalogueSnapshot, FunctionDefinition, OnDeleteAction, QualifiedSemanticName},
    types::{ResolvedType, StandardScalar},
};

use crate::{CompilerDiagnostic, ParseReport, SourceLocation, relational::RelationalQueryIr};

/// A resolved semantic type whose identities belong to the checking context.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SemanticType<T> {
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
    pub(crate) const fn scalar(scalar: StandardScalar) -> Self {
        Self::Scalar(scalar)
    }

    /// Creates a typed object reference.
    pub(crate) const fn reference(target: T) -> Self {
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
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the resolver constructs this checked catalogue in the next slice"
    )
)]
pub(crate) struct QueryObjectType<T, F> {
    id: T,
    name: QualifiedSemanticName,
    fields: Vec<(String, QueryField<T, F>)>,
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the resolver constructs this checked catalogue in the next slice"
    )
)]
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
    pub(crate) const fn id(&self) -> T
    where
        T: Copy,
    {
        self.id
    }

    /// Returns the resolved qualified type name.
    pub(crate) fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns fields in deterministic declaration order.
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
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the resolver constructs this checked catalogue in the next slice"
    )
)]
pub(crate) struct ResolutionCatalogue<T: Eq + Hash, F: Eq + Hash> {
    object_types: Vec<QueryObjectType<T, F>>,
    object_type_indices_by_name: HashMap<QualifiedSemanticName, usize>,
    object_type_indices_by_id: HashMap<T, usize>,
    field_indices_by_name: HashMap<(T, String), (usize, usize)>,
    field_indices_by_id: HashMap<(T, F), (usize, usize)>,
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the resolver constructs this checked catalogue in the next slice"
    )
)]
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

/// A validation error for a resolver-local query catalogue.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the resolver handles this validation in the next slice"
    )
)]
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
    pub(super) id: ExpressionId,
    pub(super) value: ConstantValue,
    pub(super) location: SourceLocation,
}

impl CheckedDefault {
    /// Returns the stable identity of this checked expression.
    pub const fn id(&self) -> ExpressionId {
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
    pub(super) id: FieldId,
    pub(super) name: String,
    pub(super) ordinal: u32,
    pub(super) resolved_type: ResolvedType,
    pub(super) nullable: bool,
    pub(super) unique: bool,
    pub(super) default: Option<CheckedDefault>,
    pub(super) on_delete: Option<OnDeleteAction>,
    pub(super) location: SourceLocation,
}

impl CheckedField {
    /// Returns the stable identity of the field.
    pub const fn id(&self) -> FieldId {
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
    /// Returns the resolved type.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
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
    pub(super) id: TypeId,
    pub(super) name: QualifiedSemanticName,
    pub(super) fields: Vec<CheckedField>,
    pub(super) location: SourceLocation,
}

impl CheckedObjectType {
    /// Returns the stable identity of the object type.
    pub const fn id(&self) -> TypeId {
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
    pub(super) schemas: Vec<CheckedSchema>,
    pub(super) object_types: Vec<CheckedObjectType>,
    pub(super) server_functions: Vec<CheckedServerFunction>,
}

impl CheckedBundle {
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
}

/// A checked SERVER function with an Orna-owned relational execution plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedServerFunction {
    pub(super) definition: FunctionDefinition,
    pub(super) location: SourceLocation,
    pub(super) plan: RelationalQueryIr,
}

impl CheckedServerFunction {
    /// Returns the stable function identity.
    pub const fn id(&self) -> FunctionId {
        self.definition.id()
    }

    /// Returns the resolved function name.
    pub fn name(&self) -> &QualifiedSemanticName {
        self.definition.name()
    }

    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn plan(&self) -> &RelationalQueryIr {
        &self.plan
    }
}

/// A checked logical schema declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedSchema {
    pub(super) id: SchemaId,
    pub(super) name: QualifiedSemanticName,
    pub(super) location: SourceLocation,
}

impl CheckedSchema {
    /// Returns the stable identity of the schema.
    pub const fn id(&self) -> SchemaId {
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
    pub(super) candidate: Option<CatalogueSnapshot>,
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
    /// Returns the immutable candidate catalogue when checking succeeds.
    pub fn candidate(&self) -> Option<&CatalogueSnapshot> {
        self.candidate.as_ref()
    }
}
