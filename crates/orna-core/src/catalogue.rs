//! Immutable semantic catalogue snapshots.
//!
//! A snapshot contains resolved definitions for one active catalogue revision.
//! It does not contain source syntax, physical storage state, or backend types.

use std::{collections::HashMap, error::Error, fmt, hash::Hash};

use crate::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, TypeBindingId, TypeId, types::ResolvedType,
};

mod types;

pub use types::{
    PreludeTypeName, PreludeTypeNameError, TypeBinding, TypeBindingError, TypeBindingKind,
    TypeDefinition, TypeDefinitionKind, TypeLookupName, ValueTypeDefinition, ValueTypeKind,
    ValueTypeMutability, ValueTypePersistence,
};

/// A resolved, qualified semantic name.
///
/// Name resolution establishes identifier case and quoted-name semantics before
/// it creates this value. This type compares its parts exactly.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct QualifiedSemanticName {
    parts: Vec<String>,
}

impl QualifiedSemanticName {
    /// Creates a semantic name from resolved name parts.
    pub fn new(
        parts: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, SemanticNameError> {
        let parts = parts.into_iter().map(Into::into).collect::<Vec<_>>();

        if parts.is_empty() {
            return Err(SemanticNameError::EmptyName);
        }

        for (index, part) in parts.iter().enumerate() {
            if part.is_empty() {
                return Err(SemanticNameError::EmptyPart { index });
            }
        }

        Ok(Self { parts })
    }

    /// Returns the resolved name parts in qualification order.
    pub fn parts(&self) -> &[String] {
        &self.parts
    }
}

impl fmt::Display for QualifiedSemanticName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.parts.join("."))
    }
}

/// An error returned when a semantic name cannot represent qualified parts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticNameError {
    /// A qualified name must contain at least one part.
    EmptyName,
    /// A qualified name cannot contain an empty part.
    EmptyPart {
        /// The zero-based position of the invalid part.
        index: usize,
    },
}

impl fmt::Display for SemanticNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => {
                formatter.write_str("a semantic name must contain at least one part")
            }
            Self::EmptyPart { index } => write!(formatter, "semantic name part {index} is empty"),
        }
    }
}

impl Error for SemanticNameError {}

/// One declared logical schema.
///
/// A schema is a durable semantic definition. It can exist without object
/// types or functions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaDefinition {
    id: SchemaId,
    name: QualifiedSemanticName,
}

impl SchemaDefinition {
    /// Creates a schema definition from resolved semantic data.
    pub fn new(id: SchemaId, name: QualifiedSemanticName) -> Self {
        Self { id, name }
    }

    /// Returns this schema's stable identity.
    pub const fn id(&self) -> SchemaId {
        self.id
    }

    /// Returns this schema's resolved qualified name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }
}

/// The action to take when a referenced object is deleted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OnDeleteAction {
    /// Reject deletion while a referencing value exists.
    Restrict,
    /// Set the reference field to null when its target is deleted.
    SetNull,
    /// Delete the object that contains the reference.
    Cascade,
}

/// One resolved field of an object type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldDefinition {
    id: FieldId,
    name: String,
    ordinal: u32,
    resolved_type: ResolvedType,
    nullable: bool,
    unique: bool,
    default_expression: Option<ExpressionId>,
    on_delete: Option<OnDeleteAction>,
}

impl FieldDefinition {
    /// Creates a field definition from resolved semantic data.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: FieldId,
        name: impl Into<String>,
        ordinal: u32,
        resolved_type: ResolvedType,
        nullable: bool,
        unique: bool,
        default_expression: Option<ExpressionId>,
        on_delete: Option<OnDeleteAction>,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            ordinal,
            resolved_type,
            nullable,
            unique,
            default_expression,
            on_delete,
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

    /// Reports whether this field can contain null.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }

    /// Reports whether this field has a uniqueness constraint.
    pub const fn unique(&self) -> bool {
        self.unique
    }

    /// Reports whether this is the required typed-reference uniqueness shape.
    pub const fn is_required_unique_reference(&self) -> bool {
        self.unique
            && !self.nullable
            && matches!(self.resolved_type, ResolvedType::Reference { .. })
    }

    /// Returns the identity of the resolved default expression, when present.
    pub const fn default_expression(&self) -> Option<ExpressionId> {
        self.default_expression
    }

    /// Returns the delete policy for a typed reference, when present.
    pub const fn on_delete(&self) -> Option<OnDeleteAction> {
        self.on_delete
    }
}

/// One resolved durable object type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectTypeDefinition {
    id: TypeId,
    name: QualifiedSemanticName,
    fields: Vec<FieldDefinition>,
}

impl ObjectTypeDefinition {
    /// Creates an object type definition from resolved semantic data.
    ///
    /// [`CatalogueSnapshot::new`] validates the type's field invariants before
    /// it accepts this definition into an immutable snapshot.
    pub fn new(id: TypeId, name: QualifiedSemanticName, fields: Vec<FieldDefinition>) -> Self {
        Self { id, name, fields }
    }

    /// Returns this type's stable identity.
    pub const fn id(&self) -> TypeId {
        self.id
    }

    /// Returns this type's resolved qualified name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns fields in declaration ordinal order.
    pub fn fields(&self) -> &[FieldDefinition] {
        &self.fields
    }

    /// Finds a field by its exact resolved semantic name.
    pub fn field_by_name(&self, name: &str) -> Option<&FieldDefinition> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Finds a field by its stable identity.
    pub fn field_by_id(&self, id: FieldId) -> Option<&FieldDefinition> {
        self.fields.iter().find(|field| field.id == id)
    }
}

/// The execution location of a function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FunctionDomain {
    /// The function executes in the database server runtime.
    Server,
    /// The function executes in a client runtime.
    Client,
}

/// The principal context used to execute a function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FunctionSecurity {
    /// Execute with the invoking principal's security context.
    Invoker,
    /// Execute with the function owner's security context.
    Definer,
}

/// The transaction behaviour of a server function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FunctionTransaction {
    /// Execute within one atomic transaction.
    Atomic,
    /// Execute without writes.
    ReadOnly,
    /// Let the function manage transaction boundaries.
    Manual,
}

/// The state-dependence contract of a function result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FunctionVolatility {
    /// The result is independent of database state.
    Immutable,
    /// The result is stable within one statement.
    Stable,
    /// The result can change for each call.
    Volatile,
}

/// One resolved parameter of a function signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParameterDefinition {
    id: ParameterId,
    name: String,
    ordinal: u32,
    resolved_type: ResolvedType,
    default_expression: Option<ExpressionId>,
}

impl ParameterDefinition {
    /// Creates a parameter definition from resolved semantic data.
    pub fn new(
        id: ParameterId,
        name: impl Into<String>,
        ordinal: u32,
        resolved_type: ResolvedType,
        default_expression: Option<ExpressionId>,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            ordinal,
            resolved_type,
            default_expression,
        }
    }

    /// Returns this parameter's stable identity.
    pub const fn id(&self) -> ParameterId {
        self.id
    }

    /// Returns this parameter's resolved semantic name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns this parameter's zero-based declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns this parameter's resolved type descriptor.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }

    /// Returns the identity of the resolved default expression, when present.
    pub const fn default_expression(&self) -> Option<ExpressionId> {
        self.default_expression
    }
}

/// One named column in a `ROWS (...)` function result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionReturnColumnDefinition {
    name: String,
    ordinal: u32,
    resolved_type: ResolvedType,
}

impl FunctionReturnColumnDefinition {
    /// Creates a `ROWS (...)` result column from resolved semantic data.
    pub fn new(name: impl Into<String>, ordinal: u32, resolved_type: ResolvedType) -> Self {
        Self {
            name: name.into(),
            ordinal,
            resolved_type,
        }
    }

    /// Returns this result column's resolved semantic name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns this result column's zero-based declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns this result column's resolved type descriptor.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }
}

/// The resolved result shape of a function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FunctionReturn {
    /// A function returns one resolved semantic value.
    Single(ResolvedType),
    /// A function returns zero or more records with this named ordered shape.
    Rows(Vec<FunctionReturnColumnDefinition>),
}

/// One resolved executable function signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionDefinition {
    id: FunctionId,
    name: QualifiedSemanticName,
    domain: FunctionDomain,
    parameters: Vec<ParameterDefinition>,
    return_type: FunctionReturn,
    current_revision: FunctionRevisionId,
    security: FunctionSecurity,
    transaction: Option<FunctionTransaction>,
    volatility: FunctionVolatility,
}

impl FunctionDefinition {
    /// Creates a function definition from resolved semantic signature data.
    ///
    /// [`CatalogueSnapshot::new_with_functions`] validates the function's
    /// signature invariants before it accepts this definition into a snapshot.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: FunctionId,
        name: QualifiedSemanticName,
        domain: FunctionDomain,
        parameters: Vec<ParameterDefinition>,
        return_type: FunctionReturn,
        current_revision: FunctionRevisionId,
        security: FunctionSecurity,
        transaction: Option<FunctionTransaction>,
        volatility: FunctionVolatility,
    ) -> Self {
        Self {
            id,
            name,
            domain,
            parameters,
            return_type,
            current_revision,
            security,
            transaction,
            volatility,
        }
    }

    /// Returns this function's stable identity.
    pub const fn id(&self) -> FunctionId {
        self.id
    }

    /// Returns this function's resolved qualified name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns the runtime domain that executes this function.
    pub const fn domain(&self) -> FunctionDomain {
        self.domain
    }

    /// Returns parameters in declaration ordinal order.
    pub fn parameters(&self) -> &[ParameterDefinition] {
        &self.parameters
    }

    /// Finds a parameter by its exact resolved semantic name.
    pub fn parameter_by_name(&self, name: &str) -> Option<&ParameterDefinition> {
        self.parameters
            .iter()
            .find(|parameter| parameter.name == name)
    }

    /// Finds a parameter by its stable identity.
    pub fn parameter_by_id(&self, id: ParameterId) -> Option<&ParameterDefinition> {
        self.parameters.iter().find(|parameter| parameter.id == id)
    }

    /// Returns this function's resolved result shape.
    pub fn return_type(&self) -> &FunctionReturn {
        &self.return_type
    }

    /// Returns the stable identity of the active function revision.
    pub const fn current_revision(&self) -> FunctionRevisionId {
        self.current_revision
    }

    /// Returns the function's security context mode.
    pub const fn security(&self) -> FunctionSecurity {
        self.security
    }

    /// Returns server transaction behaviour, when declared.
    pub const fn transaction(&self) -> Option<FunctionTransaction> {
        self.transaction
    }

    /// Returns the function's volatility contract.
    pub const fn volatility(&self) -> FunctionVolatility {
        self.volatility
    }
}

/// An immutable set of resolved definitions for one catalogue revision.
#[derive(Clone, Debug)]
pub struct CatalogueSnapshot {
    revision: CatalogueRevisionId,
    schemas: Vec<SchemaDefinition>,
    schema_indices_by_name: HashMap<QualifiedSemanticName, usize>,
    schema_indices_by_id: HashMap<SchemaId, usize>,
    object_types: Vec<ObjectTypeDefinition>,
    object_type_indices_by_name: HashMap<QualifiedSemanticName, usize>,
    object_type_indices_by_id: HashMap<TypeId, usize>,
    value_types: Vec<ValueTypeDefinition>,
    value_type_indices_by_name: HashMap<QualifiedSemanticName, usize>,
    value_type_indices_by_id: HashMap<TypeId, usize>,
    type_bindings: Vec<TypeBinding>,
    type_binding_indices_by_name: HashMap<TypeLookupName, usize>,
    type_binding_indices_by_id: HashMap<TypeBindingId, usize>,
    type_ids_by_qualified_name: HashMap<QualifiedSemanticName, TypeId>,
    type_ids_by_prelude_name: HashMap<PreludeTypeName, TypeId>,
    functions: Vec<FunctionDefinition>,
    function_indices_by_name: HashMap<QualifiedSemanticName, usize>,
    function_indices_by_id: HashMap<FunctionId, usize>,
}

impl CatalogueSnapshot {
    /// Validates and creates an immutable catalogue snapshot with schemas and object types.
    pub fn new(
        revision: CatalogueRevisionId,
        schemas: Vec<SchemaDefinition>,
        object_types: Vec<ObjectTypeDefinition>,
    ) -> Result<Self, CatalogueSnapshotError> {
        Self::new_with_functions_and_types(
            revision,
            schemas,
            object_types,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    /// Validates and creates a snapshot with schemas, object types, and functions.
    pub fn new_with_functions(
        revision: CatalogueRevisionId,
        schemas: Vec<SchemaDefinition>,
        object_types: Vec<ObjectTypeDefinition>,
        functions: Vec<FunctionDefinition>,
    ) -> Result<Self, CatalogueSnapshotError> {
        Self::new_with_functions_and_types(
            revision,
            schemas,
            object_types,
            Vec::new(),
            Vec::new(),
            functions,
        )
    }

    /// Validates and creates a snapshot with object types, value types, and bindings.
    pub fn new_with_types(
        revision: CatalogueRevisionId,
        schemas: Vec<SchemaDefinition>,
        object_types: Vec<ObjectTypeDefinition>,
        value_types: Vec<ValueTypeDefinition>,
        type_bindings: Vec<TypeBinding>,
    ) -> Result<Self, CatalogueSnapshotError> {
        Self::new_with_functions_and_types(
            revision,
            schemas,
            object_types,
            value_types,
            type_bindings,
            Vec::new(),
        )
    }

    /// Validates and creates a snapshot with functions and all catalogue type categories.
    pub fn new_with_functions_and_types(
        revision: CatalogueRevisionId,
        schemas: Vec<SchemaDefinition>,
        object_types: Vec<ObjectTypeDefinition>,
        value_types: Vec<ValueTypeDefinition>,
        type_bindings: Vec<TypeBinding>,
        functions: Vec<FunctionDefinition>,
    ) -> Result<Self, CatalogueSnapshotError> {
        let mut schema_indices_by_name = HashMap::with_capacity(schemas.len());
        let mut schema_indices_by_id = HashMap::with_capacity(schemas.len());
        let mut object_type_indices_by_name = HashMap::with_capacity(object_types.len());
        let mut object_type_indices_by_id = HashMap::with_capacity(object_types.len());
        let mut value_type_indices_by_name = HashMap::with_capacity(value_types.len());
        let mut value_type_indices_by_id = HashMap::with_capacity(value_types.len());
        let mut type_binding_indices_by_name = HashMap::with_capacity(type_bindings.len());
        let mut type_binding_indices_by_id = HashMap::with_capacity(type_bindings.len());
        let mut type_ids_by_qualified_name =
            HashMap::with_capacity(object_types.len() + value_types.len() + type_bindings.len());
        let mut type_ids_by_prelude_name = HashMap::with_capacity(type_bindings.len());
        let mut primary_type_ids = HashMap::with_capacity(object_types.len() + value_types.len());
        let mut function_indices_by_name = HashMap::with_capacity(functions.len());
        let mut function_indices_by_id = HashMap::with_capacity(functions.len());

        for (schema_index, schema) in schemas.iter().enumerate() {
            if schema_indices_by_name
                .insert(schema.name.clone(), schema_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateSchemaName {
                    name: schema.name.clone(),
                });
            }

            if schema_indices_by_id
                .insert(schema.id, schema_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateSchemaId { id: schema.id });
            }
        }

        for (type_index, object_type) in object_types.iter().enumerate() {
            if object_type_indices_by_name
                .insert(object_type.name.clone(), type_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateObjectTypeName {
                    name: object_type.name.clone(),
                });
            }

            if object_type_indices_by_id
                .insert(object_type.id, type_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateObjectTypeId { id: object_type.id });
            }

            type_ids_by_qualified_name.insert(object_type.name.clone(), object_type.id);
            primary_type_ids.insert(object_type.id, object_type.name.clone());

            let namespace = namespace_of(&object_type.name).ok_or(
                CatalogueSnapshotError::ObjectTypeHasNoSchema {
                    object_type: object_type.id,
                },
            )?;
            if !schema_indices_by_name.contains_key(&namespace) {
                return Err(CatalogueSnapshotError::ObjectTypeSchemaNotDeclared {
                    object_type: object_type.id,
                    schema: namespace,
                });
            }

            Self::validate_fields(object_type)?;
        }

        for (type_index, value_type) in value_types.iter().enumerate() {
            if value_type.representation_contract().is_empty() {
                return Err(
                    CatalogueSnapshotError::EmptyValueTypeRepresentationContract {
                        value_type: value_type.id(),
                    },
                );
            }

            if value_type_indices_by_name
                .insert(value_type.name().clone(), type_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateValueTypeName {
                    name: value_type.name().clone(),
                });
            }

            if value_type_indices_by_id
                .insert(value_type.id(), type_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateValueTypeId {
                    id: value_type.id(),
                });
            }

            if type_ids_by_qualified_name
                .insert(value_type.name().clone(), value_type.id())
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateTypeName {
                    name: TypeLookupName::qualified(value_type.name().clone()),
                });
            }

            if primary_type_ids
                .insert(value_type.id(), value_type.name().clone())
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateTypeId {
                    id: value_type.id(),
                });
            }

            let namespace = namespace_of(value_type.name()).ok_or(
                CatalogueSnapshotError::ValueTypeHasNoSchema {
                    value_type: value_type.id(),
                },
            )?;
            if !schema_indices_by_name.contains_key(&namespace) {
                return Err(CatalogueSnapshotError::ValueTypeSchemaNotDeclared {
                    value_type: value_type.id(),
                    schema: namespace,
                });
            }
        }

        for (binding_index, binding) in type_bindings.iter().enumerate() {
            if type_binding_indices_by_name
                .insert(binding.name().clone(), binding_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateTypeName {
                    name: binding.name().clone(),
                });
            }

            if type_binding_indices_by_id
                .insert(binding.id(), binding_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateTypeBindingId { id: binding.id() });
            }

            if !primary_type_ids.contains_key(&binding.target()) {
                return Err(CatalogueSnapshotError::TypeBindingTargetNotFound {
                    binding: binding.id(),
                    target: binding.target(),
                });
            }

            match binding.name() {
                TypeLookupName::Qualified(name) => {
                    if type_ids_by_qualified_name
                        .insert(name.clone(), binding.target())
                        .is_some()
                    {
                        return Err(CatalogueSnapshotError::DuplicateTypeName {
                            name: binding.name().clone(),
                        });
                    }

                    let namespace = namespace_of(name).ok_or(
                        CatalogueSnapshotError::QualifiedTypeBindingHasNoSchema {
                            binding: binding.id(),
                        },
                    )?;
                    if !schema_indices_by_name.contains_key(&namespace) {
                        return Err(
                            CatalogueSnapshotError::QualifiedTypeBindingSchemaNotDeclared {
                                binding: binding.id(),
                                schema: namespace,
                            },
                        );
                    }
                }
                TypeLookupName::Prelude(name) => {
                    if type_ids_by_prelude_name
                        .insert(name.clone(), binding.target())
                        .is_some()
                    {
                        return Err(CatalogueSnapshotError::DuplicateTypeName {
                            name: binding.name().clone(),
                        });
                    }
                }
            }
        }

        for (function_index, function) in functions.iter().enumerate() {
            if function_indices_by_name
                .insert(function.name.clone(), function_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateFunctionName {
                    name: function.name.clone(),
                });
            }

            if function_indices_by_id
                .insert(function.id, function_index)
                .is_some()
            {
                return Err(CatalogueSnapshotError::DuplicateFunctionId { id: function.id });
            }

            let namespace = namespace_of(&function.name).ok_or(
                CatalogueSnapshotError::FunctionHasNoSchema {
                    function: function.id,
                },
            )?;
            if !schema_indices_by_name.contains_key(&namespace) {
                return Err(CatalogueSnapshotError::FunctionSchemaNotDeclared {
                    function: function.id,
                    schema: namespace,
                });
            }

            Self::validate_function(function)?;
        }

        Ok(Self {
            revision,
            schemas,
            schema_indices_by_name,
            schema_indices_by_id,
            object_types,
            object_type_indices_by_name,
            object_type_indices_by_id,
            value_types,
            value_type_indices_by_name,
            value_type_indices_by_id,
            type_bindings,
            type_binding_indices_by_name,
            type_binding_indices_by_id,
            type_ids_by_qualified_name,
            type_ids_by_prelude_name,
            functions,
            function_indices_by_name,
            function_indices_by_id,
        })
    }

    /// Returns the stable identity of this catalogue revision.
    pub const fn revision(&self) -> CatalogueRevisionId {
        self.revision
    }

    /// Returns declared schemas in their snapshot order.
    pub fn schemas(&self) -> &[SchemaDefinition] {
        &self.schemas
    }

    /// Finds a schema by its exact resolved qualified name.
    pub fn schema_by_name(&self, name: &QualifiedSemanticName) -> Option<&SchemaDefinition> {
        self.schema_indices_by_name
            .get(name)
            .map(|index| &self.schemas[*index])
    }

    /// Finds a schema by its stable identity.
    pub fn schema_by_id(&self, id: SchemaId) -> Option<&SchemaDefinition> {
        self.schema_indices_by_id
            .get(&id)
            .map(|index| &self.schemas[*index])
    }

    /// Returns the object type definitions in their snapshot order.
    pub fn object_types(&self) -> &[ObjectTypeDefinition] {
        &self.object_types
    }

    /// Finds an object type by its exact resolved qualified name.
    pub fn object_type_by_name(
        &self,
        name: &QualifiedSemanticName,
    ) -> Option<&ObjectTypeDefinition> {
        self.object_type_indices_by_name
            .get(name)
            .map(|index| &self.object_types[*index])
    }

    /// Finds an object type by its stable identity.
    pub fn object_type_by_id(&self, id: TypeId) -> Option<&ObjectTypeDefinition> {
        self.object_type_indices_by_id
            .get(&id)
            .map(|index| &self.object_types[*index])
    }

    /// Returns the value type definitions in their snapshot order.
    pub fn value_types(&self) -> &[ValueTypeDefinition] {
        &self.value_types
    }

    /// Finds a value type by its exact canonical qualified name.
    pub fn value_type_by_name(&self, name: &QualifiedSemanticName) -> Option<&ValueTypeDefinition> {
        self.value_type_indices_by_name
            .get(name)
            .map(|index| &self.value_types[*index])
    }

    /// Finds a value type by its stable identity.
    pub fn value_type_by_id(&self, id: TypeId) -> Option<&ValueTypeDefinition> {
        self.value_type_indices_by_id
            .get(&id)
            .map(|index| &self.value_types[*index])
    }

    /// Returns direct type bindings in their snapshot order.
    pub fn type_bindings(&self) -> &[TypeBinding] {
        &self.type_bindings
    }

    /// Finds a direct type binding by its closed lookup name.
    pub fn type_binding_by_name(&self, name: &TypeLookupName) -> Option<&TypeBinding> {
        self.type_binding_indices_by_name
            .get(name)
            .map(|index| &self.type_bindings[*index])
    }

    /// Finds a direct type binding by its stable derived identity.
    pub fn type_binding_by_id(&self, id: TypeBindingId) -> Option<&TypeBinding> {
        self.type_binding_indices_by_id
            .get(&id)
            .map(|index| &self.type_bindings[*index])
    }

    /// Resolves one qualified primary name or direct binding to its stable type identity.
    pub fn type_id_by_name(&self, name: &TypeLookupName) -> Option<TypeId> {
        match name {
            TypeLookupName::Qualified(name) => self.type_ids_by_qualified_name.get(name).copied(),
            TypeLookupName::Prelude(name) => self.type_ids_by_prelude_name.get(name).copied(),
        }
    }

    /// Finds one primary type definition by its stable identity.
    pub fn type_definition_by_id(&self, id: TypeId) -> Option<TypeDefinition<'_>> {
        self.object_type_by_id(id)
            .map(TypeDefinition::Object)
            .or_else(|| self.value_type_by_id(id).map(TypeDefinition::Value))
    }

    /// Finds a primary type definition through its exact name or direct binding.
    pub fn type_definition_by_name(&self, name: &TypeLookupName) -> Option<TypeDefinition<'_>> {
        self.type_id_by_name(name)
            .and_then(|id| self.type_definition_by_id(id))
    }

    /// Returns the function definitions in their snapshot order.
    pub fn functions(&self) -> &[FunctionDefinition] {
        &self.functions
    }

    /// Finds a function by its exact resolved qualified name.
    pub fn function_by_name(&self, name: &QualifiedSemanticName) -> Option<&FunctionDefinition> {
        self.function_indices_by_name
            .get(name)
            .map(|index| &self.functions[*index])
    }

    /// Finds a function by its stable identity.
    pub fn function_by_id(&self, id: FunctionId) -> Option<&FunctionDefinition> {
        self.function_indices_by_id
            .get(&id)
            .map(|index| &self.functions[*index])
    }

    fn validate_fields(object_type: &ObjectTypeDefinition) -> Result<(), CatalogueSnapshotError> {
        Self::validate_ordered_named_members(
            object_type
                .fields
                .iter()
                .map(|field| (field.id, field.name.as_str(), field.ordinal)),
            |field| CatalogueSnapshotError::EmptyFieldName {
                owner: object_type.id,
                field,
            },
            |name| CatalogueSnapshotError::DuplicateFieldName {
                owner: object_type.id,
                name: name.to_owned(),
            },
            |id| CatalogueSnapshotError::DuplicateFieldId {
                owner: object_type.id,
                id,
            },
            |ordinal| CatalogueSnapshotError::DuplicateFieldOrdinal {
                owner: object_type.id,
                ordinal,
            },
            |field| CatalogueSnapshotError::FieldOrdinalOutOfRange {
                owner: object_type.id,
                field,
            },
            |field, expected, actual| CatalogueSnapshotError::FieldOrdinalOutOfSequence {
                owner: object_type.id,
                field,
                expected,
                actual,
            },
        )
    }

    fn validate_function(function: &FunctionDefinition) -> Result<(), CatalogueSnapshotError> {
        if function.domain == FunctionDomain::Client && function.transaction.is_some() {
            return Err(CatalogueSnapshotError::ClientFunctionTransaction {
                function: function.id,
            });
        }

        Self::validate_parameters(function)?;
        if let FunctionReturn::Rows(columns) = &function.return_type {
            Self::validate_return_columns(function, columns)?;
        }

        Ok(())
    }

    fn validate_parameters(function: &FunctionDefinition) -> Result<(), CatalogueSnapshotError> {
        Self::validate_ordered_named_members(
            function
                .parameters
                .iter()
                .map(|parameter| (parameter.id, parameter.name.as_str(), parameter.ordinal)),
            |parameter| CatalogueSnapshotError::EmptyParameterName {
                owner: function.id,
                parameter,
            },
            |name| CatalogueSnapshotError::DuplicateParameterName {
                owner: function.id,
                name: name.to_owned(),
            },
            |id| CatalogueSnapshotError::DuplicateParameterId {
                owner: function.id,
                id,
            },
            |ordinal| CatalogueSnapshotError::DuplicateParameterOrdinal {
                owner: function.id,
                ordinal,
            },
            |parameter| CatalogueSnapshotError::ParameterOrdinalOutOfRange {
                owner: function.id,
                parameter,
            },
            |parameter, expected, actual| CatalogueSnapshotError::ParameterOrdinalOutOfSequence {
                owner: function.id,
                parameter,
                expected,
                actual,
            },
        )
    }

    fn validate_return_columns(
        function: &FunctionDefinition,
        columns: &[FunctionReturnColumnDefinition],
    ) -> Result<(), CatalogueSnapshotError> {
        if columns.is_empty() {
            return Err(CatalogueSnapshotError::EmptyRowsReturn {
                function: function.id,
            });
        }

        let mut column_names = HashMap::with_capacity(columns.len());
        let mut ordinals = HashMap::with_capacity(columns.len());

        for (index, column) in columns.iter().enumerate() {
            if column.name.is_empty() {
                return Err(CatalogueSnapshotError::EmptyReturnColumnName {
                    owner: function.id,
                    ordinal: column.ordinal,
                });
            }

            if column_names.insert(column.name.as_str(), index).is_some() {
                return Err(CatalogueSnapshotError::DuplicateReturnColumnName {
                    owner: function.id,
                    name: column.name.clone(),
                });
            }

            if ordinals.insert(column.ordinal, index).is_some() {
                return Err(CatalogueSnapshotError::DuplicateReturnColumnOrdinal {
                    owner: function.id,
                    ordinal: column.ordinal,
                });
            }

            let expected = u32::try_from(index).map_err(|_| {
                CatalogueSnapshotError::ReturnColumnOrdinalOutOfRange { owner: function.id }
            })?;
            if column.ordinal != expected {
                return Err(CatalogueSnapshotError::ReturnColumnOrdinalOutOfSequence {
                    owner: function.id,
                    expected,
                    actual: column.ordinal,
                });
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_ordered_named_members<'a, Id>(
        members: impl IntoIterator<Item = (Id, &'a str, u32)>,
        empty_name: impl Fn(Id) -> CatalogueSnapshotError,
        duplicate_name: impl Fn(&str) -> CatalogueSnapshotError,
        duplicate_id: impl Fn(Id) -> CatalogueSnapshotError,
        duplicate_ordinal: impl Fn(u32) -> CatalogueSnapshotError,
        ordinal_out_of_range: impl Fn(Id) -> CatalogueSnapshotError,
        ordinal_out_of_sequence: impl Fn(Id, u32, u32) -> CatalogueSnapshotError,
    ) -> Result<(), CatalogueSnapshotError>
    where
        Id: Copy + Eq + Hash,
    {
        let iterator = members.into_iter();
        let (minimum, _) = iterator.size_hint();
        let mut ids = HashMap::with_capacity(minimum);
        let mut names = HashMap::with_capacity(minimum);
        let mut ordinals = HashMap::with_capacity(minimum);

        for (index, (id, name, ordinal)) in iterator.enumerate() {
            if name.is_empty() {
                return Err(empty_name(id));
            }
            if names.insert(name, index).is_some() {
                return Err(duplicate_name(name));
            }
            if ids.insert(id, index).is_some() {
                return Err(duplicate_id(id));
            }
            if ordinals.insert(ordinal, index).is_some() {
                return Err(duplicate_ordinal(ordinal));
            }

            let expected = u32::try_from(index).map_err(|_| ordinal_out_of_range(id))?;
            if ordinal != expected {
                return Err(ordinal_out_of_sequence(id, expected, ordinal));
            }
        }

        Ok(())
    }
}

/// Returns the exact schema that owns a qualified definition name.
///
/// A schema can be qualified itself. This does not infer ancestor schemas or
/// add a schema hierarchy. The final definition part is removed exactly.
fn namespace_of(name: &QualifiedSemanticName) -> Option<QualifiedSemanticName> {
    let namespace_parts = name.parts().get(..name.parts().len().checked_sub(1)?)?;
    if namespace_parts.is_empty() {
        return None;
    }
    QualifiedSemanticName::new(namespace_parts.iter().cloned()).ok()
}

/// An error returned when definitions cannot form a coherent snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogueSnapshotError {
    /// More than one primary type or binding has the same resolved name.
    DuplicateTypeName {
        /// The repeated name.
        name: TypeLookupName,
    },
    /// More than one primary type has the same stable identity.
    DuplicateTypeId {
        /// The repeated identity.
        id: TypeId,
    },
    /// More than one object type has the same resolved qualified name.
    DuplicateObjectTypeName {
        /// The repeated name.
        name: QualifiedSemanticName,
    },
    /// More than one object type has the same stable identity.
    DuplicateObjectTypeId {
        /// The repeated identity.
        id: TypeId,
    },
    /// More than one value type has the same resolved qualified name.
    DuplicateValueTypeName {
        /// The repeated name.
        name: QualifiedSemanticName,
    },
    /// More than one value type has the same stable identity.
    DuplicateValueTypeId {
        /// The repeated identity.
        id: TypeId,
    },
    /// A value type has no versioned representation contract.
    EmptyValueTypeRepresentationContract {
        /// The invalid value type identity.
        value_type: TypeId,
    },
    /// A value type has no namespace that can refer to a declared schema.
    ValueTypeHasNoSchema {
        /// The invalid value type identity.
        value_type: TypeId,
    },
    /// A value type refers to an undeclared exact namespace.
    ValueTypeSchemaNotDeclared {
        /// The value type identity.
        value_type: TypeId,
        /// The missing exact schema name.
        schema: QualifiedSemanticName,
    },
    /// More than one direct type binding has the same derived identity.
    DuplicateTypeBindingId {
        /// The repeated identity.
        id: TypeBindingId,
    },
    /// A direct type binding has no declared primary type target.
    TypeBindingTargetNotFound {
        /// The binding with the missing target.
        binding: TypeBindingId,
        /// The missing target type identity.
        target: TypeId,
    },
    /// A qualified type binding has no namespace that can refer to a declared schema.
    QualifiedTypeBindingHasNoSchema {
        /// The invalid binding identity.
        binding: TypeBindingId,
    },
    /// A qualified type binding refers to an undeclared exact namespace.
    QualifiedTypeBindingSchemaNotDeclared {
        /// The binding identity.
        binding: TypeBindingId,
        /// The missing exact schema name.
        schema: QualifiedSemanticName,
    },
    /// More than one schema has the same resolved qualified name.
    DuplicateSchemaName {
        /// The repeated name.
        name: QualifiedSemanticName,
    },
    /// More than one schema has the same stable identity.
    DuplicateSchemaId {
        /// The repeated identity.
        id: SchemaId,
    },
    /// An object type has no namespace that can refer to a declared schema.
    ObjectTypeHasNoSchema {
        /// The invalid object type identity.
        object_type: TypeId,
    },
    /// An object type refers to an undeclared exact namespace.
    ObjectTypeSchemaNotDeclared {
        /// The object type identity.
        object_type: TypeId,
        /// The missing exact schema name.
        schema: QualifiedSemanticName,
    },
    /// More than one function has the same resolved qualified name.
    DuplicateFunctionName {
        /// The repeated name.
        name: QualifiedSemanticName,
    },
    /// More than one function has the same stable identity.
    DuplicateFunctionId {
        /// The repeated identity.
        id: FunctionId,
    },
    /// A function has no namespace that can refer to a declared schema.
    FunctionHasNoSchema {
        /// The invalid function identity.
        function: FunctionId,
    },
    /// A function refers to an undeclared exact namespace.
    FunctionSchemaNotDeclared {
        /// The function identity.
        function: FunctionId,
        /// The missing exact schema name.
        schema: QualifiedSemanticName,
    },
    /// A client function declares server transaction behaviour.
    ClientFunctionTransaction {
        /// The invalid client function identity.
        function: FunctionId,
    },
    /// A function parameter has no semantic name.
    EmptyParameterName {
        /// The function that owns the invalid parameter.
        owner: FunctionId,
        /// The invalid parameter identity.
        parameter: ParameterId,
    },
    /// More than one parameter in a function has the same semantic name.
    DuplicateParameterName {
        /// The owning function.
        owner: FunctionId,
        /// The repeated name.
        name: String,
    },
    /// More than one parameter in a function has the same stable identity.
    DuplicateParameterId {
        /// The owning function.
        owner: FunctionId,
        /// The repeated identity.
        id: ParameterId,
    },
    /// More than one parameter in a function has the same ordinal.
    DuplicateParameterOrdinal {
        /// The owning function.
        owner: FunctionId,
        /// The repeated ordinal.
        ordinal: u32,
    },
    /// A function has more parameters than the ordinal representation allows.
    ParameterOrdinalOutOfRange {
        /// The owning function.
        owner: FunctionId,
        /// The parameter without a representable ordinal.
        parameter: ParameterId,
    },
    /// Parameters must be contiguous and stored in declaration ordinal order.
    ParameterOrdinalOutOfSequence {
        /// The owning function.
        owner: FunctionId,
        /// The parameter that has the invalid ordinal.
        parameter: ParameterId,
        /// The expected zero-based ordinal.
        expected: u32,
        /// The actual ordinal.
        actual: u32,
    },
    /// A `ROWS (...)` return shape must declare at least one named column.
    EmptyRowsReturn {
        /// The function with the empty row shape.
        function: FunctionId,
    },
    /// A `ROWS (...)` return column has no semantic name.
    EmptyReturnColumnName {
        /// The function that owns the invalid result column.
        owner: FunctionId,
        /// The invalid column ordinal.
        ordinal: u32,
    },
    /// More than one `ROWS (...)` return column has the same semantic name.
    DuplicateReturnColumnName {
        /// The owning function.
        owner: FunctionId,
        /// The repeated name.
        name: String,
    },
    /// More than one `ROWS (...)` return column has the same ordinal.
    DuplicateReturnColumnOrdinal {
        /// The owning function.
        owner: FunctionId,
        /// The repeated ordinal.
        ordinal: u32,
    },
    /// A function has more return columns than the ordinal representation allows.
    ReturnColumnOrdinalOutOfRange {
        /// The owning function.
        owner: FunctionId,
    },
    /// Return columns must be contiguous and stored in declaration ordinal order.
    ReturnColumnOrdinalOutOfSequence {
        /// The owning function.
        owner: FunctionId,
        /// The expected zero-based ordinal.
        expected: u32,
        /// The actual ordinal.
        actual: u32,
    },
    /// An object field has no semantic name.
    EmptyFieldName {
        /// The type that owns the invalid field.
        owner: TypeId,
        /// The invalid field identity.
        field: FieldId,
    },
    /// More than one field in an object type has the same semantic name.
    DuplicateFieldName {
        /// The owning object type.
        owner: TypeId,
        /// The repeated name.
        name: String,
    },
    /// More than one field in an object type has the same stable identity.
    DuplicateFieldId {
        /// The owning object type.
        owner: TypeId,
        /// The repeated identity.
        id: FieldId,
    },
    /// More than one field in an object type has the same ordinal.
    DuplicateFieldOrdinal {
        /// The owning object type.
        owner: TypeId,
        /// The repeated ordinal.
        ordinal: u32,
    },
    /// An object type has more fields than the ordinal representation allows.
    FieldOrdinalOutOfRange {
        /// The owning object type.
        owner: TypeId,
        /// The field without a representable ordinal.
        field: FieldId,
    },
    /// Fields must be contiguous and stored in declaration ordinal order.
    FieldOrdinalOutOfSequence {
        /// The owning object type.
        owner: TypeId,
        /// The field that has the invalid ordinal.
        field: FieldId,
        /// The expected zero-based ordinal.
        expected: u32,
        /// The actual ordinal.
        actual: u32,
    },
}

impl fmt::Display for CatalogueSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateTypeName { name } => {
                write!(formatter, "duplicate type name {name}")
            }
            Self::DuplicateTypeId { id } => {
                write!(formatter, "duplicate type identity {id}")
            }
            Self::DuplicateObjectTypeName { name } => {
                write!(formatter, "duplicate object type name {name}")
            }
            Self::DuplicateObjectTypeId { id } => {
                write!(formatter, "duplicate object type identity {id}")
            }
            Self::DuplicateValueTypeName { name } => {
                write!(formatter, "duplicate value type name {name}")
            }
            Self::DuplicateValueTypeId { id } => {
                write!(formatter, "duplicate value type identity {id}")
            }
            Self::EmptyValueTypeRepresentationContract { value_type } => write!(
                formatter,
                "value type {value_type} has an empty representation contract"
            ),
            Self::ValueTypeHasNoSchema { value_type } => {
                write!(formatter, "value type {value_type} has no declared schema")
            }
            Self::ValueTypeSchemaNotDeclared { value_type, schema } => write!(
                formatter,
                "value type {value_type} refers to undeclared schema {schema}"
            ),
            Self::DuplicateTypeBindingId { id } => {
                write!(formatter, "duplicate type binding identity {id}")
            }
            Self::TypeBindingTargetNotFound { binding, target } => write!(
                formatter,
                "type binding {binding} refers to undeclared type {target}"
            ),
            Self::QualifiedTypeBindingHasNoSchema { binding } => {
                write!(
                    formatter,
                    "qualified type binding {binding} has no declared schema"
                )
            }
            Self::QualifiedTypeBindingSchemaNotDeclared { binding, schema } => write!(
                formatter,
                "qualified type binding {binding} refers to undeclared schema {schema}"
            ),
            Self::DuplicateSchemaName { name } => {
                write!(formatter, "duplicate schema name {name}")
            }
            Self::DuplicateSchemaId { id } => {
                write!(formatter, "duplicate schema identity {id}")
            }
            Self::ObjectTypeHasNoSchema { object_type } => {
                write!(
                    formatter,
                    "object type {object_type} has no declared schema"
                )
            }
            Self::ObjectTypeSchemaNotDeclared {
                object_type,
                schema,
            } => write!(
                formatter,
                "object type {object_type} refers to undeclared schema {schema}"
            ),
            Self::DuplicateFunctionName { name } => {
                write!(formatter, "duplicate function name {name}")
            }
            Self::DuplicateFunctionId { id } => {
                write!(formatter, "duplicate function identity {id}")
            }
            Self::FunctionHasNoSchema { function } => {
                write!(formatter, "function {function} has no declared schema")
            }
            Self::FunctionSchemaNotDeclared { function, schema } => write!(
                formatter,
                "function {function} refers to undeclared schema {schema}"
            ),
            Self::ClientFunctionTransaction { function } => {
                write!(
                    formatter,
                    "client function {function} cannot declare server transaction behaviour"
                )
            }
            Self::EmptyParameterName { owner, parameter } => {
                write!(
                    formatter,
                    "parameter {parameter} in function {owner} has an empty name"
                )
            }
            Self::DuplicateParameterName { owner, name } => {
                write!(
                    formatter,
                    "duplicate parameter name {name} in function {owner}"
                )
            }
            Self::DuplicateParameterId { owner, id } => {
                write!(
                    formatter,
                    "duplicate parameter identity {id} in function {owner}"
                )
            }
            Self::DuplicateParameterOrdinal { owner, ordinal } => {
                write!(
                    formatter,
                    "duplicate parameter ordinal {ordinal} in function {owner}"
                )
            }
            Self::ParameterOrdinalOutOfRange { owner, parameter } => {
                write!(
                    formatter,
                    "parameter {parameter} in function {owner} has no representable ordinal"
                )
            }
            Self::ParameterOrdinalOutOfSequence {
                owner,
                parameter,
                expected,
                actual,
            } => write!(
                formatter,
                "parameter {parameter} in function {owner} has ordinal {actual}, expected {expected}"
            ),
            Self::EmptyRowsReturn { function } => {
                write!(
                    formatter,
                    "function {function} has an empty ROWS return shape"
                )
            }
            Self::EmptyReturnColumnName { owner, ordinal } => {
                write!(
                    formatter,
                    "return column {ordinal} in function {owner} has an empty name"
                )
            }
            Self::DuplicateReturnColumnName { owner, name } => {
                write!(
                    formatter,
                    "duplicate return column name {name} in function {owner}"
                )
            }
            Self::DuplicateReturnColumnOrdinal { owner, ordinal } => {
                write!(
                    formatter,
                    "duplicate return column ordinal {ordinal} in function {owner}"
                )
            }
            Self::ReturnColumnOrdinalOutOfRange { owner } => {
                write!(
                    formatter,
                    "a return column in function {owner} has no representable ordinal"
                )
            }
            Self::ReturnColumnOrdinalOutOfSequence {
                owner,
                expected,
                actual,
            } => write!(
                formatter,
                "return column in function {owner} has ordinal {actual}, expected {expected}"
            ),
            Self::EmptyFieldName { owner, field } => {
                write!(
                    formatter,
                    "field {field} in object type {owner} has an empty name"
                )
            }
            Self::DuplicateFieldName { owner, name } => {
                write!(
                    formatter,
                    "duplicate field name {name} in object type {owner}"
                )
            }
            Self::DuplicateFieldId { owner, id } => {
                write!(
                    formatter,
                    "duplicate field identity {id} in object type {owner}"
                )
            }
            Self::DuplicateFieldOrdinal { owner, ordinal } => {
                write!(
                    formatter,
                    "duplicate field ordinal {ordinal} in object type {owner}"
                )
            }
            Self::FieldOrdinalOutOfRange { owner, field } => {
                write!(
                    formatter,
                    "field {field} in object type {owner} has no representable ordinal"
                )
            }
            Self::FieldOrdinalOutOfSequence {
                owner,
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "field {field} in object type {owner} has ordinal {actual}, expected {expected}"
            ),
        }
    }
}

impl Error for CatalogueSnapshotError {}

#[cfg(test)]
mod tests {
    use crate::catalogue::{
        CatalogueSnapshot, CatalogueSnapshotError, FieldDefinition, FunctionDefinition,
        FunctionDomain, FunctionReturn, FunctionReturnColumnDefinition, FunctionSecurity,
        FunctionTransaction, FunctionVolatility, ObjectTypeDefinition, OnDeleteAction,
        ParameterDefinition, PreludeTypeName, QualifiedSemanticName, SchemaDefinition,
        SemanticNameError, TypeBinding, TypeBindingKind, TypeDefinitionKind, TypeLookupName,
        ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
    };
    use crate::{
        CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
        SchemaId, TypeId,
        types::{ResolvedType, StandardScalar},
    };

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    fn field(id: u8, name: &str, ordinal: u32) -> FieldDefinition {
        FieldDefinition::new(
            FieldId::from_bytes([id; 16]),
            name,
            ordinal,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            true,
            false,
            None,
            None,
        )
    }

    fn object(id: u8, name_parts: &[&str], fields: Vec<FieldDefinition>) -> ObjectTypeDefinition {
        ObjectTypeDefinition::new(TypeId::from_bytes([id; 16]), name(name_parts), fields)
    }

    fn schema(id: u8, name_parts: &[&str]) -> SchemaDefinition {
        SchemaDefinition::new(SchemaId::from_bytes([id; 16]), name(name_parts))
    }

    fn snapshot(
        schemas: Vec<SchemaDefinition>,
        types: Vec<ObjectTypeDefinition>,
    ) -> CatalogueSnapshot {
        CatalogueSnapshot::new(CatalogueRevisionId::from_bytes([7; 16]), schemas, types).unwrap()
    }

    fn parameter(id: u8, name: &str, ordinal: u32) -> ParameterDefinition {
        ParameterDefinition::new(
            ParameterId::from_bytes([id; 16]),
            name,
            ordinal,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )
    }

    fn return_column(name: &str, ordinal: u32) -> FunctionReturnColumnDefinition {
        FunctionReturnColumnDefinition::new(
            name,
            ordinal,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        )
    }

    #[test]
    fn required_unique_reference_shape_is_exact() {
        let target = TypeId::from_bytes([99; 16]);
        let definition = |resolved_type, nullable, unique| {
            FieldDefinition::new(
                FieldId::from_bytes([98; 16]),
                "owner",
                0,
                resolved_type,
                nullable,
                unique,
                None,
                None,
            )
        };

        assert!(
            definition(ResolvedType::reference(target), false, true).is_required_unique_reference()
        );
        assert!(
            !definition(ResolvedType::reference(target), true, true).is_required_unique_reference()
        );
        assert!(
            !definition(ResolvedType::reference(target), false, false)
                .is_required_unique_reference()
        );
        assert!(
            !definition(ResolvedType::Named(target), false, true).is_required_unique_reference()
        );
        for scalar in StandardScalar::ALL {
            assert!(
                !definition(ResolvedType::scalar(scalar), false, true)
                    .is_required_unique_reference()
            );
        }
    }

    fn function(
        id: u8,
        name_parts: &[&str],
        domain: FunctionDomain,
        parameters: Vec<ParameterDefinition>,
        return_type: FunctionReturn,
        transaction: Option<FunctionTransaction>,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            FunctionId::from_bytes([id; 16]),
            name(name_parts),
            domain,
            parameters,
            return_type,
            FunctionRevisionId::from_bytes([id.saturating_add(20); 16]),
            FunctionSecurity::Invoker,
            transaction,
            FunctionVolatility::Volatile,
        )
    }

    #[test]
    fn snapshot_resolves_object_types_by_exact_name_and_stable_id() {
        let contact_id = TypeId::from_bytes([1; 16]);
        let person_id = TypeId::from_bytes([2; 16]);
        let contact = ObjectTypeDefinition::new(
            contact_id,
            name(&["crm", "contact"]),
            vec![FieldDefinition::new(
                FieldId::from_bytes([3; 16]),
                "person",
                0,
                ResolvedType::reference(person_id),
                false,
                true,
                Some(ExpressionId::from_bytes([4; 16])),
                Some(OnDeleteAction::Restrict),
            )],
        );
        let catalogue = snapshot(vec![schema(1, &["crm"])], vec![contact]);

        let contact = catalogue
            .object_type_by_name(&name(&["crm", "contact"]))
            .unwrap();
        assert_eq!(
            catalogue.revision(),
            CatalogueRevisionId::from_bytes([7; 16])
        );
        assert_eq!(catalogue.object_type_by_id(contact_id), Some(contact));
        assert!(
            catalogue
                .object_type_by_name(&name(&["CRM", "contact"]))
                .is_none()
        );

        let person = contact.field_by_name("person").unwrap();
        assert_eq!(person.resolved_type(), ResolvedType::reference(person_id));
        assert!(!person.nullable());
        assert!(person.unique());
        assert_eq!(
            person.default_expression(),
            Some(ExpressionId::from_bytes([4; 16]))
        );
        assert_eq!(person.on_delete(), Some(OnDeleteAction::Restrict));
    }

    #[test]
    fn snapshot_resolves_primary_value_types_and_direct_bindings_to_one_type_id() {
        let boolean = TypeId::from_bytes([1; 16]);
        let value_type = ValueTypeDefinition::primitive(
            boolean,
            name(&["std", "types", "boolean"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );
        let qualified = TypeBinding::qualified(name(&["std", "boolean"]), boolean).unwrap();
        let prelude_name = PreludeTypeName::new(["BOOLEAN"]).unwrap();
        let prelude = TypeBinding::prelude(prelude_name.clone(), boolean).unwrap();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([7; 16]),
            vec![schema(1, &["std"]), schema(2, &["std", "types"])],
            vec![],
            vec![value_type],
            vec![qualified, prelude],
        )
        .unwrap();

        for type_name in [
            TypeLookupName::qualified(name(&["std", "types", "boolean"])),
            TypeLookupName::qualified(name(&["std", "boolean"])),
            TypeLookupName::prelude(prelude_name),
        ] {
            assert_eq!(catalogue.type_id_by_name(&type_name), Some(boolean));
            let definition = catalogue.type_definition_by_name(&type_name).unwrap();
            assert_eq!(definition.id(), boolean);
            assert_eq!(definition.kind(), TypeDefinitionKind::Value);
            assert!(definition.as_value().is_some());
            assert!(definition.as_object().is_none());
        }

        assert_eq!(catalogue.value_types().len(), 1);
        assert_eq!(catalogue.type_bindings().len(), 2);
        assert_eq!(
            catalogue.type_bindings()[0].kind(),
            TypeBindingKind::Qualified
        );
        assert_eq!(catalogue.object_types(), &[]);
    }

    #[test]
    fn snapshot_rejects_a_binding_that_collides_with_a_primary_type_name() {
        let object = object(1, &["std", "boolean"], vec![]);
        let value_type = ValueTypeDefinition::primitive(
            TypeId::from_bytes([2; 16]),
            name(&["std", "types", "boolean"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );
        let binding = TypeBinding::qualified(name(&["std", "boolean"]), value_type.id()).unwrap();

        let error = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([7; 16]),
            vec![schema(1, &["std"]), schema(2, &["std", "types"])],
            vec![object],
            vec![value_type],
            vec![binding],
        )
        .unwrap_err();

        assert_eq!(
            error,
            CatalogueSnapshotError::DuplicateTypeName {
                name: TypeLookupName::qualified(name(&["std", "boolean"])),
            }
        );
    }

    #[test]
    fn snapshot_rejects_cross_category_type_identity_collisions() {
        let shared_id = TypeId::from_bytes([1; 16]);
        let object = ObjectTypeDefinition::new(shared_id, name(&["std", "object"]), vec![]);
        let value = ValueTypeDefinition::primitive(
            shared_id,
            name(&["std", "types", "boolean"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );

        let error = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([7; 16]),
            vec![schema(1, &["std"]), schema(2, &["std", "types"])],
            vec![object],
            vec![value],
            vec![],
        )
        .unwrap_err();

        assert_eq!(
            error,
            CatalogueSnapshotError::DuplicateTypeId { id: shared_id }
        );
    }

    #[test]
    fn snapshot_rejects_bindings_without_a_primary_type_target() {
        let target = TypeId::from_bytes([9; 16]);
        let binding =
            TypeBinding::prelude(PreludeTypeName::new(["BOOLEAN"]).unwrap(), target).unwrap();

        let error = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([7; 16]),
            vec![],
            vec![],
            vec![],
            vec![binding.clone()],
        )
        .unwrap_err();

        assert_eq!(
            error,
            CatalogueSnapshotError::TypeBindingTargetNotFound {
                binding: binding.id(),
                target,
            }
        );
    }

    #[test]
    fn snapshot_keeps_prelude_keyword_words_distinct_from_qualified_name_parts() {
        let object_id = TypeId::from_bytes([1; 16]);
        let value_id = TypeId::from_bytes([2; 16]);
        let object = object(
            object_id.to_bytes()[0],
            &["character", "large", "object"],
            vec![],
        );
        let value = ValueTypeDefinition::primitive(
            value_id,
            name(&["std", "types", "text"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.character-large-object@1",
        );
        let prelude = PreludeTypeName::new(["CHARACTER", "LARGE", "OBJECT"]).unwrap();
        let binding = TypeBinding::prelude(prelude.clone(), value_id).unwrap();
        let catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([7; 16]),
            vec![
                schema(1, &["character", "large"]),
                schema(2, &["std"]),
                schema(3, &["std", "types"]),
            ],
            vec![object],
            vec![value],
            vec![binding],
        )
        .unwrap();

        assert_eq!(
            catalogue.type_id_by_name(&TypeLookupName::qualified(name(&[
                "character",
                "large",
                "object",
            ]))),
            Some(object_id)
        );
        assert_eq!(
            catalogue.type_id_by_name(&TypeLookupName::prelude(prelude)),
            Some(value_id)
        );
    }

    #[test]
    fn snapshot_requires_an_exact_declared_schema_for_qualified_type_bindings() {
        let value_id = TypeId::from_bytes([1; 16]);
        let value = ValueTypeDefinition::primitive(
            value_id,
            name(&["std", "types", "boolean"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        );
        let binding = TypeBinding::qualified(name(&["other", "boolean"]), value_id).unwrap();

        let error = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([7; 16]),
            vec![schema(1, &["std"]), schema(2, &["std", "types"])],
            vec![],
            vec![value],
            vec![binding.clone()],
        )
        .unwrap_err();

        assert_eq!(
            error,
            CatalogueSnapshotError::QualifiedTypeBindingSchemaNotDeclared {
                binding: binding.id(),
                schema: name(&["other"]),
            }
        );
    }

    #[test]
    fn snapshot_rejects_value_types_without_a_representation_contract() {
        let value_id = TypeId::from_bytes([1; 16]);
        let value = ValueTypeDefinition::primitive(
            value_id,
            name(&["std", "types", "boolean"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "",
        );

        let error = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([7; 16]),
            vec![schema(1, &["std"]), schema(2, &["std", "types"])],
            vec![],
            vec![value],
            vec![],
        )
        .unwrap_err();

        assert_eq!(
            error,
            CatalogueSnapshotError::EmptyValueTypeRepresentationContract {
                value_type: value_id,
            }
        );
    }

    #[test]
    fn semantic_names_require_nonempty_parts() {
        assert_eq!(
            QualifiedSemanticName::new(Vec::<String>::new()),
            Err(SemanticNameError::EmptyName)
        );
        assert_eq!(
            QualifiedSemanticName::new(["crm", ""]),
            Err(SemanticNameError::EmptyPart { index: 1 })
        );
        assert_eq!(
            QualifiedSemanticName::new(["crm.contact"]),
            Ok(name(&["crm.contact"]))
        );
    }

    #[test]
    fn snapshot_retains_empty_schemas_and_resolves_them_by_name_and_id() {
        let sales = schema(3, &["crm", "sales"]);
        let catalogue = snapshot(vec![sales.clone()], vec![]);

        assert_eq!(catalogue.schemas(), std::slice::from_ref(&sales));
        assert_eq!(catalogue.schema_by_name(sales.name()), Some(&sales));
        assert_eq!(catalogue.schema_by_id(sales.id()), Some(&sales));
    }

    #[test]
    fn snapshot_rejects_duplicate_schema_names_and_ids() {
        let first = schema(1, &["crm"]);
        let same_name = schema(2, &["crm"]);
        let same_id = schema(1, &["tasks"]);

        assert!(matches!(
            CatalogueSnapshot::new(CatalogueRevisionId::new(), vec![first.clone(), same_name], vec![]),
            Err(CatalogueSnapshotError::DuplicateSchemaName { name: duplicate_name })
                if duplicate_name == name(&["crm"])
        ));
        assert!(matches!(
            CatalogueSnapshot::new(CatalogueRevisionId::new(), vec![first, same_id], vec![]),
            Err(CatalogueSnapshotError::DuplicateSchemaId { id }) if id == SchemaId::from_bytes([1; 16])
        ));
    }

    #[test]
    fn snapshot_requires_an_exact_declared_schema_for_definitions() {
        let object_type = object(1, &["crm", "contact"], vec![]);
        let function = function(
            2,
            &["crm", "find"],
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            None,
        );

        assert!(matches!(
            CatalogueSnapshot::new(CatalogueRevisionId::new(), vec![], vec![object_type]),
            Err(CatalogueSnapshotError::ObjectTypeSchemaNotDeclared { schema, .. })
                if schema == name(&["crm"])
        ));
        assert!(matches!(
            CatalogueSnapshot::new_with_functions(
                CatalogueRevisionId::new(),
                vec![],
                vec![],
                vec![function]
            ),
            Err(CatalogueSnapshotError::FunctionSchemaNotDeclared { schema, .. })
                if schema == name(&["crm"])
        ));
    }

    #[test]
    fn snapshot_rejects_duplicate_type_names_and_ids() {
        let first = object(1, &["crm", "contact"], vec![]);
        let same_name = object(2, &["crm", "contact"], vec![]);
        let same_id = object(1, &["crm", "person"], vec![]);

        assert!(matches!(
            CatalogueSnapshot::new(
                CatalogueRevisionId::new(),
                vec![schema(1, &["crm"])],
                vec![first.clone(), same_name]
            ),
            Err(CatalogueSnapshotError::DuplicateObjectTypeName { name: duplicate_name })
                if duplicate_name == name(&["crm", "contact"])
        ));
        assert!(matches!(
            CatalogueSnapshot::new(
                CatalogueRevisionId::new(),
                vec![schema(1, &["crm"])],
                vec![first, same_id]
            ),
            Err(CatalogueSnapshotError::DuplicateObjectTypeId { id }) if id == TypeId::from_bytes([1; 16])
        ));
    }

    #[test]
    fn snapshot_rejects_duplicate_fields_per_object_type() {
        let duplicate_name = object(
            1,
            &["crm", "contact"],
            vec![field(1, "name", 0), field(2, "name", 1)],
        );
        let duplicate_id = object(
            1,
            &["crm", "contact"],
            vec![field(1, "name", 0), field(1, "email", 1)],
        );
        let duplicate_ordinal = object(
            1,
            &["crm", "contact"],
            vec![field(1, "name", 0), field(2, "email", 0)],
        );

        assert!(matches!(
            CatalogueSnapshot::new(
                CatalogueRevisionId::new(),
                vec![schema(1, &["crm"])],
                vec![duplicate_name]
            ),
            Err(CatalogueSnapshotError::DuplicateFieldName { .. })
        ));
        assert!(matches!(
            CatalogueSnapshot::new(
                CatalogueRevisionId::new(),
                vec![schema(1, &["crm"])],
                vec![duplicate_id]
            ),
            Err(CatalogueSnapshotError::DuplicateFieldId { .. })
        ));
        assert!(matches!(
            CatalogueSnapshot::new(
                CatalogueRevisionId::new(),
                vec![schema(1, &["crm"])],
                vec![duplicate_ordinal]
            ),
            Err(CatalogueSnapshotError::DuplicateFieldOrdinal { .. })
        ));
    }

    #[test]
    fn snapshot_requires_contiguous_fields_in_ordinal_order() {
        let missing_first_ordinal = object(1, &["crm", "contact"], vec![field(1, "name", 1)]);
        let out_of_order = object(
            1,
            &["crm", "contact"],
            vec![field(1, "name", 1), field(2, "email", 0)],
        );

        assert!(matches!(
            CatalogueSnapshot::new(
                CatalogueRevisionId::new(),
                vec![schema(1, &["crm"])],
                vec![missing_first_ordinal]
            ),
            Err(CatalogueSnapshotError::FieldOrdinalOutOfSequence {
                expected: 0,
                actual: 1,
                ..
            })
        ));
        assert!(matches!(
            CatalogueSnapshot::new(
                CatalogueRevisionId::new(),
                vec![schema(1, &["crm"])],
                vec![out_of_order]
            ),
            Err(CatalogueSnapshotError::FieldOrdinalOutOfSequence {
                expected: 0,
                actual: 1,
                ..
            })
        ));
    }

    #[test]
    fn snapshot_resolves_function_signatures_by_exact_name_and_stable_id() {
        let function_id = FunctionId::from_bytes([9; 16]);
        let parameter_id = ParameterId::from_bytes([10; 16]);
        let function = FunctionDefinition::new(
            function_id,
            name(&["tasks", "overdue"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "p_before",
                0,
                ResolvedType::scalar(StandardScalar::Timestamp),
                Some(ExpressionId::from_bytes([11; 16])),
            )],
            FunctionReturn::Rows(vec![
                FunctionReturnColumnDefinition::new(
                    "title",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                ),
                FunctionReturnColumnDefinition::new(
                    "due_at",
                    1,
                    ResolvedType::scalar(StandardScalar::Timestamp),
                ),
            ]),
            FunctionRevisionId::from_bytes([12; 16]),
            FunctionSecurity::Definer,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([7; 16]),
            vec![schema(1, &["tasks"])],
            vec![],
            vec![function],
        )
        .unwrap();

        let function = catalogue
            .function_by_name(&name(&["tasks", "overdue"]))
            .unwrap();
        assert_eq!(catalogue.function_by_id(function_id), Some(function));
        assert!(
            catalogue
                .function_by_name(&name(&["TASKS", "overdue"]))
                .is_none()
        );
        assert_eq!(function.domain(), FunctionDomain::Server);
        assert_eq!(
            function.current_revision(),
            FunctionRevisionId::from_bytes([12; 16])
        );
        assert_eq!(function.security(), FunctionSecurity::Definer);
        assert_eq!(function.transaction(), Some(FunctionTransaction::ReadOnly));
        assert_eq!(function.volatility(), FunctionVolatility::Stable);

        let parameter = function.parameter_by_name("p_before").unwrap();
        assert_eq!(function.parameter_by_id(parameter_id), Some(parameter));
        assert_eq!(parameter.ordinal(), 0);
        assert_eq!(
            parameter.resolved_type(),
            ResolvedType::scalar(StandardScalar::Timestamp)
        );
        assert_eq!(
            parameter.default_expression(),
            Some(ExpressionId::from_bytes([11; 16]))
        );

        let FunctionReturn::Rows(columns) = function.return_type() else {
            panic!("tasks.overdue must return rows");
        };
        assert_eq!(columns.len(), 2);
        assert_eq!(columns[0].name(), "title");
        assert_eq!(columns[0].ordinal(), 0);
        assert_eq!(
            columns[1].resolved_type(),
            ResolvedType::scalar(StandardScalar::Timestamp)
        );
    }

    #[test]
    fn snapshot_rejects_duplicate_function_and_parameter_identities() {
        let first = function(
            1,
            &["tasks", "overdue"],
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            None,
        );
        let same_name = function(
            2,
            &["tasks", "overdue"],
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            None,
        );
        let same_id = function(
            1,
            &["tasks", "archive"],
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            None,
        );
        let duplicate_parameter_name = function(
            3,
            &["tasks", "assign"],
            FunctionDomain::Server,
            vec![parameter(1, "p_task", 0), parameter(2, "p_task", 1)],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            None,
        );
        let duplicate_parameter_id = function(
            4,
            &["tasks", "complete"],
            FunctionDomain::Server,
            vec![parameter(1, "p_task", 0), parameter(1, "p_actor", 1)],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            None,
        );

        assert!(matches!(
            CatalogueSnapshot::new_with_functions(
                CatalogueRevisionId::new(),
                vec![schema(1, &["tasks"])],
                vec![],
                vec![first.clone(), same_name]
            ),
            Err(CatalogueSnapshotError::DuplicateFunctionName { .. })
        ));
        assert!(matches!(
            CatalogueSnapshot::new_with_functions(
                CatalogueRevisionId::new(),
                vec![schema(1, &["tasks"])],
                vec![],
                vec![first, same_id]
            ),
            Err(CatalogueSnapshotError::DuplicateFunctionId { .. })
        ));
        assert!(matches!(
            CatalogueSnapshot::new_with_functions(
                CatalogueRevisionId::new(),
                vec![schema(1, &["tasks"])],
                vec![],
                vec![duplicate_parameter_name]
            ),
            Err(CatalogueSnapshotError::DuplicateParameterName { .. })
        ));
        assert!(matches!(
            CatalogueSnapshot::new_with_functions(
                CatalogueRevisionId::new(),
                vec![schema(1, &["tasks"])],
                vec![],
                vec![duplicate_parameter_id]
            ),
            Err(CatalogueSnapshotError::DuplicateParameterId { .. })
        ));
    }

    #[test]
    fn snapshot_rejects_invalid_function_transaction_and_rows_shapes() {
        let client_transaction = function(
            1,
            &["studio", "main"],
            FunctionDomain::Client,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            Some(FunctionTransaction::Atomic),
        );
        let non_contiguous_parameters = function(
            2,
            &["tasks", "assign"],
            FunctionDomain::Server,
            vec![parameter(1, "p_task", 1)],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            None,
        );
        let duplicate_return_column = function(
            3,
            &["tasks", "overdue"],
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Rows(vec![return_column("title", 0), return_column("title", 1)]),
            None,
        );
        let non_contiguous_return_column = function(
            4,
            &["tasks", "recent"],
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Rows(vec![return_column("title", 1)]),
            None,
        );
        let empty_rows = function(
            5,
            &["tasks", "none"],
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Rows(vec![]),
            None,
        );

        assert!(matches!(
            CatalogueSnapshot::new_with_functions(
                CatalogueRevisionId::new(),
                vec![schema(1, &["studio"])],
                vec![],
                vec![client_transaction]
            ),
            Err(CatalogueSnapshotError::ClientFunctionTransaction { .. })
        ));
        assert!(matches!(
            CatalogueSnapshot::new_with_functions(
                CatalogueRevisionId::new(),
                vec![schema(1, &["tasks"])],
                vec![],
                vec![non_contiguous_parameters]
            ),
            Err(CatalogueSnapshotError::ParameterOrdinalOutOfSequence { .. })
        ));
        assert!(matches!(
            CatalogueSnapshot::new_with_functions(
                CatalogueRevisionId::new(),
                vec![schema(1, &["tasks"])],
                vec![],
                vec![duplicate_return_column]
            ),
            Err(CatalogueSnapshotError::DuplicateReturnColumnName { .. })
        ));
        assert!(matches!(
            CatalogueSnapshot::new_with_functions(
                CatalogueRevisionId::new(),
                vec![schema(1, &["tasks"])],
                vec![],
                vec![non_contiguous_return_column]
            ),
            Err(CatalogueSnapshotError::ReturnColumnOrdinalOutOfSequence { .. })
        ));
        assert!(matches!(
            CatalogueSnapshot::new_with_functions(
                CatalogueRevisionId::new(),
                vec![schema(1, &["tasks"])],
                vec![],
                vec![empty_rows]
            ),
            Err(CatalogueSnapshotError::EmptyRowsReturn { .. })
        ));
    }
}
