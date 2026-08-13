//! Backend-independent runtime values, typed function arguments, and ordered
//! SERVER results.
//!
//! This module defines the initial runtime subset only. It does not define a
//! canonical or wire encoding. A later protocol slice must define that format.

use std::{error::Error, fmt};

use crate::{
    FieldId, ObjectId, ParameterId, TypeId,
    catalogue::CatalogueSnapshot,
    revision::{ActiveDatabaseRevision, record_value_field_runtime_type},
    types::{ResolvedType, StandardScalar},
};

/// One typed runtime value accepted by the initial SERVER query result subset.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeValue {
    /// A typed null value. The type remains available when the value is null.
    Null(NullValue),
    /// A BOOLEAN value.
    Boolean(bool),
    /// An INTEGER value.
    Integer(i32),
    /// A BIGINT value.
    BigInt(i64),
    /// A FLOAT value.
    Float(RuntimeFloat),
    /// A TEXT or CHARACTER LARGE OBJECT value.
    Text(String),
    /// A BYTES or BINARY LARGE OBJECT value.
    Bytes(Vec<u8>),
    /// A typed durable object reference.
    Reference { target: TypeId, object: ObjectId },
    /// A catalogue-validated enum value.
    Enum(EnumValue),
    /// A catalogue-validated named immutable record value.
    Record(RecordValue),
}

impl RuntimeValue {
    /// Creates a typed null in the initial supported runtime subset.
    pub fn null(resolved_type: ResolvedType) -> Result<Self, ResultRowsError> {
        require_supported_runtime_type(resolved_type)?;
        Ok(Self::Null(NullValue { resolved_type }))
    }

    /// Returns the exact resolved type carried by this value.
    pub const fn resolved_type(&self) -> ResolvedType {
        match self {
            Self::Null(value) => value.resolved_type,
            Self::Boolean(_) => ResolvedType::scalar(StandardScalar::Boolean),
            Self::Integer(_) => ResolvedType::scalar(StandardScalar::Integer),
            Self::BigInt(_) => ResolvedType::scalar(StandardScalar::BigInt),
            Self::Float(_) => ResolvedType::scalar(StandardScalar::Float),
            Self::Text(_) => ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            Self::Bytes(_) => ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            Self::Reference { target, .. } => ResolvedType::reference(*target),
            Self::Enum(value) => ResolvedType::named(value.enum_type),
            Self::Record(value) => ResolvedType::named(value.record_type),
        }
    }

    /// Reports whether this value is null.
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null(_))
    }
}

/// One named immutable record value validated against an active catalogue.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordValue {
    record_type: TypeId,
    fields: Vec<RuntimeValue>,
}

impl RecordValue {
    /// Validates a complete named field set and stores values in declaration order.
    pub fn new(
        active: &ActiveDatabaseRevision,
        record_type: TypeId,
        fields: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> Result<Self, RecordValueError> {
        let catalogue = active.catalogue();
        let definition = catalogue
            .record_value_type_by_id(record_type)
            .ok_or(RecordValueError::UnknownType { record_type })?;
        let standard = active
            .catalogue_hash_context()
            .standard()
            .expect("admitted record catalogue must have a verified standard context")
            .catalogue();
        let mut ordered = vec![None; definition.fields().len()];

        for (name, value) in fields {
            let field = definition
                .field_by_name(&name)
                .ok_or(RecordValueError::UnknownField { record_type, name })?;
            let index = usize::try_from(field.ordinal())
                .expect("validated record field ordinal must fit usize");
            if ordered[index].is_some() {
                return Err(RecordValueError::DuplicateField {
                    record_type,
                    field: field.id(),
                });
            }
            if value.is_null() {
                return Err(RecordValueError::NullField {
                    record_type,
                    field: field.id(),
                });
            }
            let expected =
                record_value_field_runtime_type(catalogue, standard, field.resolved_type()).ok_or(
                    RecordValueError::UnsupportedFieldType {
                        record_type,
                        field: field.id(),
                        resolved_type: field.resolved_type(),
                    },
                )?;
            let actual = value.resolved_type();
            if actual != expected || matches!(value, RuntimeValue::Record(_)) {
                return Err(RecordValueError::FieldTypeMismatch {
                    record_type,
                    field: field.id(),
                    expected,
                    actual,
                });
            }
            if let RuntimeValue::Enum(enum_value) = &value {
                let active_enum = catalogue
                    .enum_type_by_id(enum_value.enum_type())
                    .or_else(|| standard.enum_type_by_id(enum_value.enum_type()));
                if !active_enum.is_some_and(|enum_type| {
                    enum_type
                        .labels()
                        .iter()
                        .any(|label| label == enum_value.label())
                }) {
                    return Err(RecordValueError::InactiveEnumLabel {
                        record_type,
                        field: field.id(),
                        enum_type: enum_value.enum_type(),
                        label: enum_value.label().to_owned(),
                    });
                }
            }
            ordered[index] = Some(value);
        }

        let fields = definition
            .fields()
            .iter()
            .zip(ordered)
            .map(|(field, value)| {
                value.ok_or(RecordValueError::MissingField {
                    record_type,
                    field: field.id(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            record_type,
            fields,
        })
    }

    /// Returns the stable identity of the nominal record type.
    pub const fn record_type(&self) -> TypeId {
        self.record_type
    }

    /// Returns values in declaration ordinal order.
    pub fn fields(&self) -> &[RuntimeValue] {
        &self.fields
    }
}

/// An error from validating a named immutable record value.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordValueError {
    /// The active catalogue does not contain the supplied record type.
    UnknownType {
        /// The unknown record type identity.
        record_type: TypeId,
    },
    /// The active record type does not declare the supplied exact field name.
    UnknownField {
        /// The active record type identity.
        record_type: TypeId,
        /// The unknown exact field name.
        name: String,
    },
    /// One declared field was supplied more than once.
    DuplicateField {
        /// The active record type identity.
        record_type: TypeId,
        /// The duplicated field identity.
        field: FieldId,
    },
    /// One required declared field was not supplied.
    MissingField {
        /// The active record type identity.
        record_type: TypeId,
        /// The missing field identity.
        field: FieldId,
    },
    /// A record field was supplied as a typed null value.
    NullField {
        /// The active record type identity.
        record_type: TypeId,
        /// The field that received NULL.
        field: FieldId,
    },
    /// A declared field type is not available through the selected context.
    UnsupportedFieldType {
        /// The active record type identity.
        record_type: TypeId,
        /// The unsupported field identity.
        field: FieldId,
        /// The unsupported declared type.
        resolved_type: ResolvedType,
    },
    /// A field value does not have the exact declared runtime type.
    FieldTypeMismatch {
        /// The active record type identity.
        record_type: TypeId,
        /// The mismatched field identity.
        field: FieldId,
        /// The runtime type required by the declaration.
        expected: ResolvedType,
        /// The runtime type supplied by the caller.
        actual: ResolvedType,
    },
    /// An enum field label is not present in the active enum definition.
    InactiveEnumLabel {
        /// The active record type identity.
        record_type: TypeId,
        /// The enum field identity.
        field: FieldId,
        /// The active enum type identity.
        enum_type: TypeId,
        /// The inactive label supplied by the caller.
        label: String,
    },
}

impl fmt::Display for RecordValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownType { .. } => formatter.write_str("record value type is not active"),
            Self::UnknownField { .. } => {
                formatter.write_str("record field is not declared by the active type")
            }
            Self::DuplicateField { .. } => formatter.write_str("record field is duplicated"),
            Self::MissingField { .. } => formatter.write_str("record field is missing"),
            Self::NullField { .. } => formatter.write_str("record field cannot be NULL"),
            Self::UnsupportedFieldType { .. } => {
                formatter.write_str("record field type is not available in the active context")
            }
            Self::FieldTypeMismatch { .. } => {
                formatter.write_str("record field value has a type mismatch")
            }
            Self::InactiveEnumLabel { .. } => {
                formatter.write_str("record enum field label is not active")
            }
        }
    }
}

impl Error for RecordValueError {}

/// One enum label validated against an active catalogue snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnumValue {
    enum_type: TypeId,
    label: String,
}

impl EnumValue {
    /// Creates an enum value only when the active type declares the exact label.
    pub fn new(
        catalogue: &CatalogueSnapshot,
        enum_type: TypeId,
        label: impl Into<String>,
    ) -> Result<Self, EnumValueError> {
        let definition = catalogue
            .enum_type_by_id(enum_type)
            .ok_or(EnumValueError::UnknownType { enum_type })?;
        let label = label.into();
        if !definition
            .labels()
            .iter()
            .any(|declared| declared == &label)
        {
            return Err(EnumValueError::UndeclaredLabel { enum_type, label });
        }
        Ok(Self { enum_type, label })
    }

    /// Returns the stable identity of the declaring enum type.
    pub const fn enum_type(&self) -> TypeId {
        self.enum_type
    }

    /// Returns the exact declared label.
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// An error from validating an enum runtime value.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnumValueError {
    /// The active catalogue does not contain the supplied enum type.
    UnknownType {
        /// The unknown enum type identity.
        enum_type: TypeId,
    },
    /// The active enum type does not declare the supplied exact label.
    UndeclaredLabel {
        /// The active enum type identity.
        enum_type: TypeId,
        /// The undeclared label.
        label: String,
    },
}

impl fmt::Display for EnumValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownType { .. } => formatter.write_str("enum type is not active"),
            Self::UndeclaredLabel { .. } => {
                formatter.write_str("enum label is not declared by the active type")
            }
        }
    }
}

impl Error for EnumValueError {}

/// One typed argument supplied to a server function.
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionArgument {
    parameter: ParameterId,
    value: RuntimeValue,
}

impl FunctionArgument {
    /// Creates one argument, rejecting typed null values.
    pub fn new(parameter: ParameterId, value: RuntimeValue) -> Result<Self, FunctionArgumentError> {
        match &value {
            RuntimeValue::Null(null) => Err(FunctionArgumentError::NullValue {
                parameter,
                resolved_type: null.resolved_type(),
            }),
            RuntimeValue::Boolean(_)
            | RuntimeValue::Integer(_)
            | RuntimeValue::BigInt(_)
            | RuntimeValue::Float(_)
            | RuntimeValue::Text(_)
            | RuntimeValue::Bytes(_)
            | RuntimeValue::Reference { .. }
            | RuntimeValue::Enum(_) => Ok(Self { parameter, value }),
            RuntimeValue::Record(value) => Err(FunctionArgumentError::RecordValueNotAccepted {
                parameter,
                record_type: value.record_type(),
            }),
        }
    }

    /// Returns the parameter identity bound to this argument.
    pub const fn parameter(&self) -> ParameterId {
        self.parameter
    }

    /// Returns the runtime value bound to this argument.
    pub const fn value(&self) -> &RuntimeValue {
        &self.value
    }
}

/// An error from constructing a typed server-function argument.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FunctionArgumentError {
    /// A typed null value is not a valid function argument in this slice.
    NullValue {
        /// The parameter identity supplied with the null value.
        parameter: ParameterId,
        /// The resolved type carried by the null value.
        resolved_type: ResolvedType,
    },
    /// A record value is outside the current executable argument subset.
    RecordValueNotAccepted {
        /// The parameter identity supplied with the record value.
        parameter: ParameterId,
        /// The record type carried by the value.
        record_type: TypeId,
    },
}

impl fmt::Display for FunctionArgumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NullValue { .. } => formatter.write_str("function argument value cannot be NULL"),
            Self::RecordValueNotAccepted { .. } => {
                formatter.write_str("record function arguments are not accepted")
            }
        }
    }
}

impl Error for FunctionArgumentError {}

/// An opaque typed null value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NullValue {
    resolved_type: ResolvedType,
}

impl NullValue {
    /// Returns the exact supported type carried by this null value.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }
}

/// A finite FLOAT value with reflexive numeric equality.
///
/// `+0.0` and `-0.0` compare equal. Non-finite IEEE values are not runtime
/// values in this initial subset.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeFloat(f64);

impl RuntimeFloat {
    /// Creates one finite FLOAT value.
    pub fn new(value: f64) -> Result<Self, ResultRowsError> {
        if !value.is_finite() {
            return Err(ResultRowsError::NonFiniteFloat);
        }
        Ok(Self(value))
    }

    /// Returns the finite floating-point value.
    pub const fn value(&self) -> f64 {
        self.0
    }
}

impl PartialEq for RuntimeFloat {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

/// One ordered result column.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultColumn {
    name: String,
    resolved_type: ResolvedType,
    nullable: bool,
}

impl ResultColumn {
    /// Creates one result column in the initial supported runtime subset.
    pub fn new(
        name: impl Into<String>,
        resolved_type: ResolvedType,
        nullable: bool,
    ) -> Result<Self, ResultRowsError> {
        let name = name.into();
        if name.is_empty() {
            return Err(ResultRowsError::EmptyColumnName);
        }
        require_supported_runtime_type(resolved_type)?;
        Ok(Self {
            name,
            resolved_type,
            nullable,
        })
    }

    /// Returns the exact result column name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the resolved result type.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }

    /// Reports whether this result column accepts null values.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }
}

/// One ordered result row before result-set validation.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultRow {
    values: Vec<RuntimeValue>,
}

impl ResultRow {
    /// Creates one ordered row. [`ResultRows::new`] validates it against columns.
    pub fn new(values: impl IntoIterator<Item = RuntimeValue>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }

    /// Returns values in result-column order.
    pub fn values(&self) -> &[RuntimeValue] {
        &self.values
    }

    /// Transfers values in result-column order without cloning their payloads.
    pub fn into_values(self) -> Vec<RuntimeValue> {
        self.values
    }
}

/// A validated ordered set of SERVER query result rows.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultRows {
    columns: Vec<ResultColumn>,
    rows: Vec<ResultRow>,
}

impl ResultRows {
    /// Validates and creates one ordered result set.
    pub fn new(
        columns: impl IntoIterator<Item = ResultColumn>,
        rows: impl IntoIterator<Item = ResultRow>,
    ) -> Result<Self, ResultRowsError> {
        let columns = columns.into_iter().collect::<Vec<_>>();
        validate_columns(&columns)?;

        let rows = rows.into_iter().collect::<Vec<_>>();
        for (row_index, row) in rows.iter().enumerate() {
            if row.values.len() != columns.len() {
                return Err(ResultRowsError::RowWidthMismatch {
                    row: row_index,
                    expected: columns.len(),
                    actual: row.values.len(),
                });
            }
            for (column_index, (column, value)) in columns.iter().zip(&row.values).enumerate() {
                if value.is_null() && !column.nullable {
                    return Err(ResultRowsError::NullInNonNullableColumn {
                        row: row_index,
                        column: column_index,
                    });
                }
                let actual = value.resolved_type();
                if actual != column.resolved_type {
                    return Err(ResultRowsError::ValueTypeMismatch {
                        row: row_index,
                        column: column_index,
                        expected: column.resolved_type,
                        actual,
                    });
                }
            }
        }

        Ok(Self { columns, rows })
    }

    /// Returns result columns in their declared order.
    pub fn columns(&self) -> &[ResultColumn] {
        &self.columns
    }

    /// Returns rows in query result order.
    pub fn rows(&self) -> &[ResultRow] {
        &self.rows
    }

    /// Transfers rows in query result order without cloning their payloads.
    pub fn into_rows(self) -> Vec<ResultRow> {
        self.rows
    }
}

/// A structured error from runtime result construction.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResultRowsError {
    /// A result set has no result columns.
    EmptyColumns,
    /// A result column name is empty.
    EmptyColumnName,
    /// A type has no representation in the initial runtime subset.
    UnsupportedRuntimeType { resolved_type: ResolvedType },
    /// A FLOAT value is not finite.
    NonFiniteFloat,
    /// Two result columns have the same exact name.
    DuplicateColumnName {
        first: usize,
        duplicate: usize,
        name: String,
    },
    /// A row does not have exactly one value per result column.
    RowWidthMismatch {
        row: usize,
        expected: usize,
        actual: usize,
    },
    /// A null value occurred in a non-nullable result column.
    NullInNonNullableColumn { row: usize, column: usize },
    /// A value type does not equal its result-column type.
    ValueTypeMismatch {
        row: usize,
        column: usize,
        expected: ResolvedType,
        actual: ResolvedType,
    },
}

impl fmt::Display for ResultRowsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyColumns => formatter.write_str("result set has no columns"),
            Self::EmptyColumnName => formatter.write_str("result column name is empty"),
            Self::UnsupportedRuntimeType { .. } => {
                formatter.write_str("type is not supported by the runtime subset")
            }
            Self::NonFiniteFloat => formatter.write_str("FLOAT value must be finite"),
            Self::DuplicateColumnName {
                first,
                duplicate,
                name,
            } => write!(
                formatter,
                "result column {duplicate} duplicates result column {first}: {name}"
            ),
            Self::RowWidthMismatch {
                row,
                expected,
                actual,
            } => write!(
                formatter,
                "result row {row} has {actual} values; expected {expected}"
            ),
            Self::NullInNonNullableColumn { row, column } => {
                write!(formatter, "result row {row} column {column} cannot be null")
            }
            Self::ValueTypeMismatch {
                row,
                column,
                expected: _,
                actual: _,
            } => write!(
                formatter,
                "result row {row} column {column} has a type mismatch"
            ),
        }
    }
}

impl Error for ResultRowsError {}

fn validate_columns(columns: &[ResultColumn]) -> Result<(), ResultRowsError> {
    if columns.is_empty() {
        return Err(ResultRowsError::EmptyColumns);
    }
    for (index, column) in columns.iter().enumerate() {
        for (first, earlier) in columns[..index].iter().enumerate() {
            if earlier.name == column.name {
                return Err(ResultRowsError::DuplicateColumnName {
                    first,
                    duplicate: index,
                    name: column.name.clone(),
                });
            }
        }
    }
    Ok(())
}

fn require_supported_runtime_type(resolved_type: ResolvedType) -> Result<(), ResultRowsError> {
    if supports_runtime_value(resolved_type) {
        Ok(())
    } else {
        Err(ResultRowsError::UnsupportedRuntimeType { resolved_type })
    }
}

const fn supports_runtime_value(resolved_type: ResolvedType) -> bool {
    resolved_type.reference_target().is_some()
        || resolved_type.value_type().is_some()
        || resolved_type.named_type().is_some()
        || matches!(
            resolved_type.legacy_scalar(),
            Some(
                StandardScalar::Boolean
                    | StandardScalar::Integer
                    | StandardScalar::BigInt
                    | StandardScalar::Float
                    | StandardScalar::CharacterLargeObject
                    | StandardScalar::BinaryLargeObject
            )
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        CatalogueRevisionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
        StandardLibraryRevisionId, TypeId,
        canonical_hash::{
            calculate_standard_library_digest_for_test, catalogue_digest_with_context,
            source_bundle_digest, source_revision_record_digest, source_unit_content_digest,
            verify_standard_library_snapshot,
        },
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName,
            RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition,
            ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
        },
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, RevisionPair, Sha256Digest,
            SourceOrigin, StandardLibraryDigestVersion, StandardLibrarySnapshot,
            StoredSourceRevision, StoredSourceUnit,
        },
    };

    const TARGET: TypeId = TypeId::from_bytes([0x41; 16]);
    const OBJECT: ObjectId = ObjectId::from_bytes([0x42; 16]);
    const ENUM_TYPE: TypeId = TypeId::from_bytes([0x43; 16]);
    const RECORD_TYPE: TypeId = TypeId::from_bytes([0x47; 16]);
    const STANDARD_BOOLEAN: TypeId = TypeId::from_bytes([0x48; 16]);
    const ENABLED_FIELD: FieldId = FieldId::from_bytes([0x59; 16]);
    const STAGE_FIELD: FieldId = FieldId::from_bytes([0x5a; 16]);

    fn active_record_revision() -> ActiveDatabaseRevision {
        active_record_revision_with_type(RECORD_TYPE)
    }

    fn active_record_revision_with_type(record_type: TypeId) -> ActiveDatabaseRevision {
        let standard_unit_content = "CREATE SCHEMA std; CREATE TYPE std.boolean;";
        let standard_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x50; 16]),
            0,
            "std/types.orna",
            standard_unit_content,
            source_unit_content_digest(standard_unit_content).unwrap(),
        )
        .unwrap();
        let standard_bundle_hash =
            source_bundle_digest(std::slice::from_ref(&standard_unit)).unwrap();
        let standard_source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x51; 16]),
            SourceRevisionId::from_bytes([0x52; 16]),
            None,
            vec![standard_unit],
            standard_bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x51; 16]),
                None,
                standard_bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let standard_schema = SchemaId::from_bytes([0x53; 16]);
        let standard_catalogue = CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes([0x54; 16]),
            vec![SchemaDefinition::new(
                standard_schema,
                QualifiedSemanticName::new(["std"]).unwrap(),
            )],
            vec![],
            vec![ValueTypeDefinition::primitive(
                STANDARD_BOOLEAN,
                QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.kernel.value.boolean@1",
            )],
            vec![],
        )
        .unwrap();
        let standard_origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(standard_schema),
                SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(STANDARD_BOOLEAN),
                SourceOrigin::new(SourceUnitId::from_bytes([0x50; 16]), 1, 2).unwrap(),
            ),
        ];
        let provisional_standard = StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([0x55; 16]),
            StandardLibraryDigestVersion::Version1,
            standard_source.clone(),
            "orna.language/1",
            standard_catalogue.clone(),
            standard_origins.clone(),
            Sha256Digest::from_bytes([0x56; 32]),
        )
        .unwrap();
        let standard_digest =
            calculate_standard_library_digest_for_test(&provisional_standard).unwrap();
        let standard = verify_standard_library_snapshot(
            StandardLibrarySnapshot::new(
                provisional_standard.revision(),
                provisional_standard.digest_version(),
                standard_source,
                provisional_standard.language_version(),
                standard_catalogue,
                standard_origins,
                standard_digest,
            )
            .unwrap(),
        )
        .unwrap();

        let application_schema = SchemaId::from_bytes([0x57; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x58; 16]);
        let catalogue = CatalogueSnapshot::new_with_record_value_types(
            catalogue_revision,
            vec![SchemaDefinition::new(
                application_schema,
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                ENUM_TYPE,
                QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
                ["lead", "qualified"],
            )],
            vec![RecordValueTypeDefinition::new(
                record_type,
                QualifiedSemanticName::new(["crm", "status"]).unwrap(),
                vec![
                    RecordValueFieldDefinition::new(
                        ENABLED_FIELD,
                        "enabled",
                        0,
                        ResolvedType::value(STANDARD_BOOLEAN),
                    ),
                    RecordValueFieldDefinition::new(
                        STAGE_FIELD,
                        "stage",
                        1,
                        ResolvedType::named(ENUM_TYPE),
                    ),
                ],
            )],
            vec![],
        )
        .unwrap();
        let context = CatalogueHashContext::version_two(standard);
        let application_content = "abcde";
        let application_unit = StoredSourceUnit::new(
            SourceUnitId::from_bytes([0x63; 16]),
            0,
            "app/types.orna",
            application_content,
            source_unit_content_digest(application_content).unwrap(),
        )
        .unwrap();
        let application_bundle_hash =
            source_bundle_digest(std::slice::from_ref(&application_unit)).unwrap();
        let application_source_revision = SourceRevisionId::from_bytes([0x64; 16]);
        let application_source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x65; 16]),
            application_source_revision,
            None,
            vec![application_unit],
            application_bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x65; 16]),
                None,
                application_bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let source_unit = SourceUnitId::from_bytes([0x63; 16]);
        let origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(application_schema),
                SourceOrigin::new(source_unit, 0, 1).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(ENUM_TYPE),
                SourceOrigin::new(source_unit, 1, 2).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ValueType(record_type),
                SourceOrigin::new(source_unit, 2, 3).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: record_type,
                    field: ENABLED_FIELD,
                },
                SourceOrigin::new(source_unit, 3, 4).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: record_type,
                    field: STAGE_FIELD,
                },
                SourceOrigin::new(source_unit, 4, 5).unwrap(),
            ),
        ];
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(application_source_revision, catalogue_revision),
                application_source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
            ),
            context,
        )
        .unwrap()
    }

    fn enum_catalogue(labels: &[&str]) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x44; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x45; 16]),
                QualifiedSemanticName::new(["crm"]).unwrap(),
            )],
            vec![],
            vec![],
            vec![EnumTypeDefinition::new(
                ENUM_TYPE,
                QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
                labels.iter().copied(),
            )],
            vec![],
        )
        .unwrap()
    }

    fn column(name: &str, resolved_type: ResolvedType, nullable: bool) -> ResultColumn {
        ResultColumn::new(name, resolved_type, nullable).unwrap()
    }

    #[test]
    fn record_values_validate_named_fields_and_store_declaration_order() {
        let active = active_record_revision();
        let stage =
            RuntimeValue::Enum(EnumValue::new(active.catalogue(), ENUM_TYPE, "qualified").unwrap());

        let record = RecordValue::new(
            &active,
            RECORD_TYPE,
            [
                (String::from("stage"), stage.clone()),
                (String::from("enabled"), RuntimeValue::Boolean(true)),
            ],
        )
        .unwrap();

        assert_eq!(record.record_type(), RECORD_TYPE);
        assert_eq!(
            record.fields(),
            &[RuntimeValue::Boolean(true), stage.clone()]
        );
        assert_eq!(
            RuntimeValue::Record(record).resolved_type(),
            ResolvedType::named(RECORD_TYPE)
        );
    }

    #[test]
    fn record_values_require_an_active_nominal_type_and_exact_field_names() {
        let active = active_record_revision();
        let unknown_type = TypeId::from_bytes([0x60; 16]);
        assert_eq!(
            RecordValue::new(&active, unknown_type, Vec::<(String, RuntimeValue)>::new(),),
            Err(RecordValueError::UnknownType {
                record_type: unknown_type,
            })
        );

        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [(String::from("Enabled"), RuntimeValue::Boolean(true))],
            ),
            Err(RecordValueError::UnknownField {
                record_type: RECORD_TYPE,
                name: String::from("Enabled"),
            })
        );
    }

    #[test]
    fn record_values_require_every_declared_field_exactly_once() {
        let active = active_record_revision();
        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [(String::from("enabled"), RuntimeValue::Boolean(true))],
            ),
            Err(RecordValueError::MissingField {
                record_type: RECORD_TYPE,
                field: STAGE_FIELD,
            })
        );

        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (String::from("enabled"), RuntimeValue::Boolean(false)),
                ],
            ),
            Err(RecordValueError::DuplicateField {
                record_type: RECORD_TYPE,
                field: ENABLED_FIELD,
            })
        );
    }

    #[test]
    fn record_values_reject_null_wrong_type_and_stale_enum_fields() {
        let active = active_record_revision();
        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [(
                    String::from("enabled"),
                    RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap(),
                )],
            ),
            Err(RecordValueError::NullField {
                record_type: RECORD_TYPE,
                field: ENABLED_FIELD,
            })
        );

        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [(String::from("enabled"), RuntimeValue::Integer(1))],
            ),
            Err(RecordValueError::FieldTypeMismatch {
                record_type: RECORD_TYPE,
                field: ENABLED_FIELD,
                expected: ResolvedType::scalar(StandardScalar::Boolean),
                actual: ResolvedType::scalar(StandardScalar::Integer),
            })
        );

        let stale_catalogue = enum_catalogue(&["retired"]);
        let stale =
            RuntimeValue::Enum(EnumValue::new(&stale_catalogue, ENUM_TYPE, "retired").unwrap());
        assert_eq!(
            RecordValue::new(
                &active,
                RECORD_TYPE,
                [
                    (String::from("enabled"), RuntimeValue::Boolean(true)),
                    (String::from("stage"), stale),
                ],
            ),
            Err(RecordValueError::InactiveEnumLabel {
                record_type: RECORD_TYPE,
                field: STAGE_FIELD,
                enum_type: ENUM_TYPE,
                label: String::from("retired"),
            })
        );
    }

    #[test]
    fn record_values_enter_server_results_but_not_the_argument_subset() {
        let active = active_record_revision();
        let record = RecordValue::new(
            &active,
            RECORD_TYPE,
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("stage"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap();
        let parameter = ParameterId::from_bytes([0x61; 16]);
        assert_eq!(
            FunctionArgument::new(parameter, RuntimeValue::Record(record.clone())),
            Err(FunctionArgumentError::RecordValueNotAccepted {
                parameter,
                record_type: RECORD_TYPE,
            })
        );
        let expected = RuntimeValue::Record(record);
        let rows = ResultRows::new(
            [column("status", ResolvedType::named(RECORD_TYPE), false)],
            [ResultRow::new([expected.clone()])],
        )
        .unwrap();
        assert_eq!(rows.rows()[0].values(), &[expected]);
    }

    #[test]
    fn record_value_equality_is_nominal_and_bound_to_one_active_revision() {
        let active = active_record_revision();
        let fields = || {
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("stage"),
                    RuntimeValue::Enum(
                        EnumValue::new(active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ]
        };
        let record = RecordValue::new(&active, RECORD_TYPE, fields()).unwrap();
        assert_eq!(
            RecordValue::new(&active, RECORD_TYPE, fields()).unwrap(),
            record
        );

        let other_type = TypeId::from_bytes([0x62; 16]);
        let other_active = active_record_revision_with_type(other_type);
        let other = RecordValue::new(
            &other_active,
            other_type,
            [
                (String::from("enabled"), RuntimeValue::Boolean(true)),
                (
                    String::from("stage"),
                    RuntimeValue::Enum(
                        EnumValue::new(other_active.catalogue(), ENUM_TYPE, "lead").unwrap(),
                    ),
                ),
            ],
        )
        .unwrap();
        assert_ne!(record, other);
    }

    #[test]
    fn accepts_every_current_non_null_runtime_value_as_a_function_argument() {
        let catalogue = enum_catalogue(&["lead", "qualified"]);
        let values = vec![
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(-7),
            RuntimeValue::BigInt(8),
            RuntimeValue::Float(RuntimeFloat::new(9.5).unwrap()),
            RuntimeValue::Text("value".into()),
            RuntimeValue::Bytes(vec![1, 2, 3]),
            RuntimeValue::Reference {
                target: TARGET,
                object: OBJECT,
            },
            RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "qualified").unwrap()),
        ];

        for (index, value) in values.into_iter().enumerate() {
            let parameter = ParameterId::from_bytes([index as u8; 16]);
            let argument = FunctionArgument::new(parameter, value.clone()).unwrap();
            assert_eq!(argument.parameter(), parameter);
            assert_eq!(argument.value(), &value);
        }
    }

    #[test]
    fn enum_values_require_an_active_type_and_exact_declared_label() {
        let catalogue = enum_catalogue(&["lead", "owner's", "customer"]);
        let value = EnumValue::new(&catalogue, ENUM_TYPE, "owner's").unwrap();

        assert_eq!(value.enum_type(), ENUM_TYPE);
        assert_eq!(value.label(), "owner's");
        assert_eq!(
            RuntimeValue::Enum(value.clone()).resolved_type(),
            ResolvedType::named(ENUM_TYPE)
        );
        assert_eq!(value, value.clone());

        let unknown = TypeId::from_bytes([0x46; 16]);
        let error = EnumValue::new(&catalogue, unknown, "lead").unwrap_err();
        assert_eq!(error, EnumValueError::UnknownType { enum_type: unknown });
        assert_eq!(error.to_string(), "enum type is not active");

        let error = EnumValue::new(&catalogue, ENUM_TYPE, "Lead").unwrap_err();
        assert_eq!(
            error,
            EnumValueError::UndeclaredLabel {
                enum_type: ENUM_TYPE,
                label: String::from("Lead"),
            }
        );
        assert_eq!(
            error.to_string(),
            "enum label is not declared by the active type"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn result_rows_accept_exact_enum_values_and_typed_nulls() {
        let catalogue = enum_catalogue(&["lead", "qualified"]);
        let enum_type = ResolvedType::named(ENUM_TYPE);
        let value = RuntimeValue::Enum(EnumValue::new(&catalogue, ENUM_TYPE, "qualified").unwrap());
        let rows = ResultRows::new(
            [
                column("stage", enum_type, false),
                column("previous_stage", enum_type, true),
            ],
            [ResultRow::new([
                value.clone(),
                RuntimeValue::null(enum_type).unwrap(),
            ])],
        )
        .unwrap();

        assert_eq!(rows.rows()[0].values()[0], value);
        assert!(rows.rows()[0].values()[1].is_null());
    }

    #[test]
    fn rejects_typed_null_function_arguments_with_parameter_and_type() {
        let parameter = ParameterId::from_bytes([0x43; 16]);
        let resolved_type = ResolvedType::reference(TARGET);
        let value = RuntimeValue::null(resolved_type).unwrap();

        let error = FunctionArgument::new(parameter, value).unwrap_err();
        assert_eq!(
            error,
            FunctionArgumentError::NullValue {
                parameter,
                resolved_type,
            }
        );
        assert_eq!(error.to_string(), "function argument value cannot be NULL");
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn function_argument_clone_and_equality_preserve_parameter_and_reference_identity() {
        let parameter = ParameterId::from_bytes([0x44; 16]);
        let value = RuntimeValue::Reference {
            target: TARGET,
            object: OBJECT,
        };
        let argument = FunctionArgument::new(parameter, value.clone()).unwrap();
        let clone = argument.clone();

        assert_eq!(clone, argument);
        assert_eq!(argument.parameter(), parameter);
        assert_eq!(argument.value(), &value);

        let other_parameter = ParameterId::from_bytes([0x45; 16]);
        let other = FunctionArgument::new(other_parameter, value).unwrap();
        assert_ne!(argument, other);
    }

    #[test]
    fn accepts_every_initial_runtime_value_type_and_typed_null() {
        let rows = ResultRows::new(
            [
                column(
                    "boolean",
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                ),
                column(
                    "integer",
                    ResolvedType::scalar(StandardScalar::Integer),
                    false,
                ),
                column(
                    "bigint",
                    ResolvedType::scalar(StandardScalar::BigInt),
                    false,
                ),
                column("float", ResolvedType::scalar(StandardScalar::Float), false),
                column(
                    "text",
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                ),
                column(
                    "optional_text",
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    true,
                ),
                column(
                    "bytes",
                    ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                    false,
                ),
                column("reference", ResolvedType::reference(TARGET), false),
            ],
            [ResultRow::new([
                RuntimeValue::Boolean(true),
                RuntimeValue::Integer(7),
                RuntimeValue::BigInt(8),
                RuntimeValue::Float(RuntimeFloat::new(9.5).unwrap()),
                RuntimeValue::Text("value".into()),
                RuntimeValue::null(ResolvedType::scalar(StandardScalar::CharacterLargeObject))
                    .unwrap(),
                RuntimeValue::Bytes(vec![1, 2, 3]),
                RuntimeValue::Reference {
                    target: TARGET,
                    object: OBJECT,
                },
            ])],
        )
        .unwrap();

        assert_eq!(rows.columns().len(), 8);
        assert_eq!(rows.rows()[0].values().len(), 8);
        assert!(rows.rows()[0].values()[5].is_null());
    }

    #[test]
    fn preserves_column_and_row_order() {
        let rows = ResultRows::new(
            [
                column(
                    "second",
                    ResolvedType::scalar(StandardScalar::Integer),
                    false,
                ),
                column(
                    "first",
                    ResolvedType::scalar(StandardScalar::Boolean),
                    false,
                ),
            ],
            [
                ResultRow::new([RuntimeValue::Integer(2), RuntimeValue::Boolean(false)]),
                ResultRow::new([RuntimeValue::Integer(1), RuntimeValue::Boolean(true)]),
            ],
        )
        .unwrap();

        assert_eq!(rows.columns()[0].name(), "second");
        assert_eq!(rows.columns()[1].name(), "first");
        assert_eq!(rows.rows()[0].values()[0], RuntimeValue::Integer(2));
        assert_eq!(rows.rows()[1].values()[1], RuntimeValue::Boolean(true));
    }

    #[test]
    fn transfers_rows_and_values_in_order_without_cloning_payloads() {
        let bytes = vec![1_u8, 2, 3];
        let rows = ResultRows::new(
            [column(
                "payload",
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                false,
            )],
            [ResultRow::new([RuntimeValue::Bytes(bytes.clone())])],
        )
        .unwrap();

        let rows = rows.into_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows.into_iter().next().unwrap().into_values(),
            [RuntimeValue::Bytes(bytes),]
        );
    }

    #[test]
    fn rejects_empty_duplicate_and_unsupported_columns() {
        assert_eq!(
            ResultColumn::new("", ResolvedType::scalar(StandardScalar::Boolean), false),
            Err(ResultRowsError::EmptyColumnName)
        );
        for resolved_type in [
            ResolvedType::scalar(StandardScalar::Decimal),
            ResolvedType::scalar(StandardScalar::Uuid),
            ResolvedType::scalar(StandardScalar::Date),
            ResolvedType::scalar(StandardScalar::Time),
            ResolvedType::scalar(StandardScalar::Timestamp),
            ResolvedType::scalar(StandardScalar::Duration),
            ResolvedType::scalar(StandardScalar::Void),
        ] {
            assert_eq!(
                ResultColumn::new("unsupported", resolved_type, false),
                Err(ResultRowsError::UnsupportedRuntimeType { resolved_type })
            );
            assert_eq!(
                RuntimeValue::null(resolved_type),
                Err(ResultRowsError::UnsupportedRuntimeType { resolved_type })
            );
        }
        assert_eq!(
            ResultRows::new(
                [
                    column("same", ResolvedType::scalar(StandardScalar::Boolean), false),
                    column("same", ResolvedType::scalar(StandardScalar::Integer), false),
                ],
                [],
            ),
            Err(ResultRowsError::DuplicateColumnName {
                first: 0,
                duplicate: 1,
                name: "same".into(),
            })
        );
    }

    #[test]
    fn rejects_zero_columns_even_when_rows_have_zero_width() {
        assert_eq!(
            ResultRows::new(Vec::<ResultColumn>::new(), [ResultRow::new([])]),
            Err(ResultRowsError::EmptyColumns)
        );
    }

    #[test]
    fn rejects_non_finite_floats_and_preserves_finite_equality() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                RuntimeFloat::new(value),
                Err(ResultRowsError::NonFiniteFloat)
            );
        }

        let finite = RuntimeFloat::new(2.5).unwrap();
        assert_eq!(finite, finite);
        assert_eq!(finite.value(), 2.5);
        assert_eq!(
            RuntimeFloat::new(0.0).unwrap(),
            RuntimeFloat::new(-0.0).unwrap()
        );
    }

    #[test]
    fn null_values_expose_only_the_checked_type() {
        let value = RuntimeValue::null(ResolvedType::scalar(StandardScalar::Boolean)).unwrap();
        let RuntimeValue::Null(null) = value else {
            panic!("runtime null constructor must create a null value");
        };
        assert_eq!(
            null.resolved_type(),
            ResolvedType::scalar(StandardScalar::Boolean)
        );
    }

    #[test]
    fn rejects_width_nullability_and_type_mismatches() {
        let boolean = column(
            "boolean",
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
        );
        assert_eq!(
            ResultRows::new([boolean.clone()], [ResultRow::new([])]),
            Err(ResultRowsError::RowWidthMismatch {
                row: 0,
                expected: 1,
                actual: 0,
            })
        );
        assert_eq!(
            ResultRows::new(
                [boolean.clone()],
                [ResultRow::new([RuntimeValue::null(ResolvedType::scalar(
                    StandardScalar::Boolean
                ))
                .unwrap(),])],
            ),
            Err(ResultRowsError::NullInNonNullableColumn { row: 0, column: 0 })
        );
        assert_eq!(
            ResultRows::new([boolean], [ResultRow::new([RuntimeValue::Integer(1)])]),
            Err(ResultRowsError::ValueTypeMismatch {
                row: 0,
                column: 0,
                expected: ResolvedType::scalar(StandardScalar::Boolean),
                actual: ResolvedType::scalar(StandardScalar::Integer),
            })
        );
    }

    #[test]
    fn rejects_references_with_the_wrong_target_type() {
        let expected = TypeId::from_bytes([0x51; 16]);
        let actual = TypeId::from_bytes([0x52; 16]);
        assert_eq!(
            ResultRows::new(
                [column(
                    "reference",
                    ResolvedType::reference(expected),
                    false
                )],
                [ResultRow::new([RuntimeValue::Reference {
                    target: actual,
                    object: OBJECT,
                }])],
            ),
            Err(ResultRowsError::ValueTypeMismatch {
                row: 0,
                column: 0,
                expected: ResolvedType::reference(expected),
                actual: ResolvedType::reference(actual),
            })
        );
    }
}
