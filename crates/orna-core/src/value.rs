//! Backend-independent runtime values, typed function arguments, and ordered
//! SERVER results.
//!
//! This module defines the initial runtime subset only. It does not define a
//! canonical or wire encoding. A later protocol slice must define that format.

use std::{error::Error, fmt};

use crate::{
    ObjectId, ParameterId, TypeId,
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
        }
    }

    /// Reports whether this value is null.
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null(_))
    }
}

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
            | RuntimeValue::Reference { .. } => Ok(Self { parameter, value }),
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
}

impl fmt::Display for FunctionArgumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NullValue { .. } => formatter.write_str("function argument value cannot be NULL"),
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
    matches!(
        resolved_type,
        ResolvedType::Scalar(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        ) | ResolvedType::Reference { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: TypeId = TypeId::from_bytes([0x41; 16]);
    const OBJECT: ObjectId = ObjectId::from_bytes([0x42; 16]);

    fn column(name: &str, resolved_type: ResolvedType, nullable: bool) -> ResultColumn {
        ResultColumn::new(name, resolved_type, nullable).unwrap()
    }

    #[test]
    fn accepts_every_current_non_null_runtime_value_as_a_function_argument() {
        let values = [
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
        ];

        for (index, value) in values.into_iter().enumerate() {
            let parameter = ParameterId::from_bytes([index as u8; 16]);
            let argument = FunctionArgument::new(parameter, value.clone()).unwrap();
            assert_eq!(argument.parameter(), parameter);
            assert_eq!(argument.value(), &value);
        }
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
            ResolvedType::named(TARGET),
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
