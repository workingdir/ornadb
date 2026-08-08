//! Checked-result values produced by semantic resolution.

use orna_core::{
    ExpressionId, FieldId, FunctionId, SchemaId, TypeId,
    catalogue::{CatalogueSnapshot, FunctionDefinition, OnDeleteAction, QualifiedSemanticName},
    types::ResolvedType,
};

use crate::{CompilerDiagnostic, ParseReport, SourceLocation, relational::RelationalQueryIr};

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
