use super::*;

/// One query-visible field in a catalogue used during semantic checking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct QueryField<T, F> {
    id: F,
    resolved_type: SemanticType<T>,
    standard_value_type: Option<TypeId>,
    nullable: bool,
    unique: bool,
}

impl<T, F> QueryField<T, F> {
    /// Creates one query-visible field.
    pub(crate) fn new(id: F, resolved_type: SemanticType<T>, nullable: bool) -> Self {
        Self {
            id,
            resolved_type,
            standard_value_type: None,
            nullable,
            unique: false,
        }
    }

    /// Attaches resolved standard value-type provenance for relational checking.
    pub(crate) const fn with_standard_value_type(mut self, type_id: TypeId) -> Self {
        self.standard_value_type = Some(type_id);
        self
    }

    /// Attaches the durable uniqueness fact for one query-visible field.
    pub(crate) const fn with_unique(mut self) -> Self {
        self.unique = true;
        self
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

    /// Returns the supplied standard value-type identity when this field uses one.
    pub(crate) const fn standard_value_type(&self) -> Option<TypeId> {
        self.standard_value_type
    }

    /// Reports whether the field can contain null.
    pub(crate) const fn nullable(&self) -> bool {
        self.nullable
    }

    /// Reports whether this field has the durable uniqueness fact.
    pub(crate) const fn unique(&self) -> bool {
        self.unique
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
        QueryCatalogue::field_by_name(self, owner, name).map(mutation_field)
    }

    fn visit_fields(&self, owner: T, visitor: &mut dyn FnMut(&str, MutationField<T, F>)) {
        let Some(index) = self.object_type_indices_by_id.get(&owner) else {
            return;
        };
        for (name, field) in &self.object_types[*index].fields {
            visitor(name, mutation_field(*field));
        }
    }
}

fn mutation_field<T: Copy, F: Copy>(query_field: QueryField<T, F>) -> MutationField<T, F> {
    let mutation_field = MutationField::new(
        query_field.id(),
        query_field.semantic_type(),
        query_field.nullable(),
    );
    if let Some(type_id) = query_field.standard_value_type() {
        mutation_field.with_standard_value_type(type_id)
    } else {
        mutation_field
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
            .and_then(query_field_from_core)
    }

    fn field_by_id(&self, owner: TypeId, id: FieldId) -> Option<QueryField<TypeId, FieldId>> {
        self.object_type_by_id(owner)
            .and_then(|object_type| object_type.field_by_id(id))
            .and_then(query_field_from_core)
    }
}

fn query_field_from_core(
    field: &orna_core::catalogue::FieldDefinition,
) -> Option<QueryField<TypeId, FieldId>> {
    SemanticType::from_core(field.resolved_type()).map(|resolved_type| {
        let query_field = QueryField::new(field.id(), resolved_type, field.nullable());
        if field.unique() {
            query_field.with_unique()
        } else {
            query_field
        }
    })
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
    fn semantic_type_from_core_projects_only_current_legacy_shapes() {
        let type_id = TypeId::from_bytes([0x71; 16]);

        assert_eq!(
            SemanticType::from_core(ResolvedType::scalar(StandardScalar::Boolean)),
            Some(SemanticType::scalar(StandardScalar::Boolean))
        );
        assert_eq!(
            SemanticType::from_core(ResolvedType::named(type_id)),
            Some(SemanticType::Named(type_id))
        );
        assert_eq!(
            SemanticType::from_core(ResolvedType::reference(type_id)),
            Some(SemanticType::reference(type_id))
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

    #[test]
    fn checked_client_capability_records_redacted_name_and_argument_source() {
        let literal = super::CheckedClientCapability::new(
            "std.fs.read",
            super::CheckedClientCapabilityArgument::Text("/home/bob".to_owned()),
        );
        assert_eq!(literal.name(), "std.fs.read");
        assert_eq!(
            literal.argument(),
            &super::CheckedClientCapabilityArgument::Text("/home/bob".to_owned())
        );

        let parameter = super::CheckedClientCapability::new(
            "std.secret.use",
            super::CheckedClientCapabilityArgument::Parameter("p_key".to_owned()),
        );
        assert_eq!(parameter.name(), "std.secret.use");
        assert_eq!(
            parameter.argument(),
            &super::CheckedClientCapabilityArgument::Parameter("p_key".to_owned())
        );

        assert_ne!(literal, parameter);
        assert_eq!(literal.clone(), literal);
    }

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }
}
