//! A small, closed migration language and backend-neutral DDL IR.
//!
//! The migration language in this module is deliberately not SQL. It has
//! enough vocabulary for the kernel's durable migration ledger and no escape
//! hatch for arbitrary expressions or statements. Parsing produces a typed
//! [`Migration`] value; renderers then lower that value to the two SQL dialects
//! used by the storage adapters.

use std::{error::Error, fmt, fmt::Write as _};

/// The checked-in typed source for the application's backend migrations.
///
/// This is source data for generation and tests. Adapters consume generated
/// SQL artifacts and never compile this source at runtime.
pub const APPLICATION_MIGRATION_SOURCE: &str =
    include_str!("../migrations/0046_application_migrations.orna");

/// A SQL identifier accepted by the closed migration language.
///
/// Identifiers are intentionally restricted to lower-case ASCII names no longer
/// than PostgreSQL's 63-byte unquoted identifier limit. They are consequently
/// safe to render without quoting or interpolating SQL fragments.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Identifier(String);

impl Identifier {
    /// Validates and constructs one migration identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        let mut bytes = value.bytes();
        let Some(first) = bytes.next() else {
            return Err(IdentifierError::new(value));
        };
        if value.len() > 63
            || !(first == b'_' || first.is_ascii_lowercase())
            || !bytes.all(|byte| byte == b'_' || byte.is_ascii_lowercase() || byte.is_ascii_digit())
            || is_reserved_identifier(&value)
        {
            return Err(IdentifierError::new(value));
        }
        Ok(Self(value))
    }

    /// Returns the identifier's validated SQL spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An identifier that failed the closed migration identifier grammar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentifierError {
    value: String,
}

impl IdentifierError {
    fn new(value: String) -> Self {
        Self { value }
    }
}

impl fmt::Display for IdentifierError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid migration identifier {:?}", self.value)
    }
}

impl Error for IdentifierError {}

/// A schema declaration in a typed migration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationSchema {
    name: Identifier,
}

impl MigrationSchema {
    /// Constructs a schema declaration from a validated identifier.
    pub fn new(name: Identifier) -> Self {
        Self { name }
    }

    /// Returns the schema name.
    pub fn name(&self) -> &Identifier {
        &self.name
    }
}

/// A qualified table name in the neutral migration IR.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct QualifiedTableName {
    schema: Identifier,
    table: Identifier,
}

impl QualifiedTableName {
    /// Constructs a qualified table name from validated identifiers.
    pub fn new(schema: Identifier, table: Identifier) -> Self {
        Self { schema, table }
    }

    /// Returns the schema component.
    pub fn schema(&self) -> &Identifier {
        &self.schema
    }

    /// Returns the table component.
    pub fn table(&self) -> &Identifier {
        &self.table
    }
}

impl fmt::Display for QualifiedTableName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.schema, self.table)
    }
}

/// The closed scalar type vocabulary accepted by a migration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScalarType {
    /// A signed 32-bit logical integer.
    Integer,
    /// A signed 64-bit logical integer.
    BigInt,
    /// UTF-8 text.
    Text,
    /// Opaque bytes.
    Bytes,
    /// A timestamp value whose backend representation is dialect-specific.
    Timestamp,
}

/// Column nullability in the neutral IR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Nullability {
    /// The column rejects SQL NULL.
    NotNull,
    /// The column may contain SQL NULL.
    Nullable,
}

/// The closed default-expression vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefaultValue {
    /// The transaction's current timestamp.
    CurrentTimestamp,
}

/// One typed column declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationColumn {
    name: Identifier,
    scalar_type: ScalarType,
    nullability: Nullability,
    default: Option<DefaultValue>,
}

impl MigrationColumn {
    /// Constructs a column declaration.
    pub fn new(
        name: Identifier,
        scalar_type: ScalarType,
        nullability: Nullability,
        default: Option<DefaultValue>,
    ) -> Self {
        Self {
            name,
            scalar_type,
            nullability,
            default,
        }
    }

    /// Returns the column name.
    pub fn name(&self) -> &Identifier {
        &self.name
    }

    /// Returns the abstract scalar type.
    pub const fn scalar_type(&self) -> ScalarType {
        self.scalar_type
    }

    /// Returns column nullability.
    pub const fn nullability(&self) -> Nullability {
        self.nullability
    }

    /// Returns the optional typed default.
    pub const fn default(&self) -> Option<DefaultValue> {
        self.default
    }
}

/// A typed predicate supported by a particular migration backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckConstraint {
    /// Requires a numeric column to be at least zero.
    NonNegative { column: Identifier },
    /// Requires a numeric column to be greater than zero.
    Positive { column: Identifier },
    /// Requires text or bytes to be non-empty.
    NonEmpty { column: Identifier },
    /// Requires a bytes column to have exactly `bytes` bytes.
    ByteLength { column: Identifier, bytes: u16 },
    /// Requires a nullable bytes column to be NULL or exactly `bytes` bytes.
    NullableByteLength { column: Identifier, bytes: u16 },
    /// Requires an integer column to equal a non-negative literal.
    EqualsInteger { column: Identifier, value: u64 },
    /// Requires one integer column to be greater than or equal to another.
    GreaterOrEqual {
        column: Identifier,
        other: Identifier,
    },
}

impl CheckConstraint {
    /// Returns the primary column constrained by this predicate.
    pub fn column(&self) -> &Identifier {
        match self {
            Self::NonNegative { column }
            | Self::Positive { column }
            | Self::NonEmpty { column }
            | Self::ByteLength { column, .. }
            | Self::NullableByteLength { column, .. }
            | Self::EqualsInteger { column, .. }
            | Self::GreaterOrEqual { column, .. } => column,
        }
    }
}

/// The declaration target represented by a typed source item.
///
/// `Common` declarations are lowered for both supported backends. The
/// backend-specific variants are lowered only for their named backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum MigrationBackendTarget {
    /// Lower this declaration for both PostgreSQL and SQLite.
    Common,
    /// Lower this declaration only for PostgreSQL.
    Postgres,
    /// Lower this declaration only for SQLite.
    Sqlite,
}

impl MigrationBackendTarget {
    /// Returns whether this declaration is lowered for the selected backend.
    pub const fn applies_to(self, backend: MigrationBackend) -> bool {
        matches!(
            (self, backend),
            (
                Self::Common,
                MigrationBackend::Postgres | MigrationBackend::Sqlite
            ) | (Self::Postgres, MigrationBackend::Postgres)
                | (Self::Sqlite, MigrationBackend::Sqlite)
        )
    }
    /// Returns whether this backend target covers every backend in another target.
    fn covers(self, target: Self) -> bool {
        matches!(
            (self, target),
            (Self::Common, Self::Common)
                | (Self::Postgres, Self::Common | Self::Postgres)
                | (Self::Sqlite, Self::Common | Self::Sqlite)
        )
    }
}

/// A table-level constraint in the neutral IR.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TableConstraint {
    /// A primary-key column tuple.
    PrimaryKey(Vec<Identifier>),
    /// A unique candidate-key column tuple.
    Unique(Vec<Identifier>),
    /// A typed check predicate.
    Check(CheckConstraint),
}

/// One table declaration in a typed migration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationTable {
    target: MigrationBackendTarget,
    name: QualifiedTableName,
    columns: Vec<MigrationColumn>,
    constraints: Vec<TableConstraint>,
}

impl MigrationTable {
    /// Constructs a common table declaration.
    pub fn new(
        name: QualifiedTableName,
        columns: Vec<MigrationColumn>,
        constraints: Vec<TableConstraint>,
    ) -> Self {
        Self::for_target(MigrationBackendTarget::Common, name, columns, constraints)
    }

    /// Constructs a table declaration for one backend target.
    pub fn for_target(
        target: MigrationBackendTarget,
        name: QualifiedTableName,
        columns: Vec<MigrationColumn>,
        constraints: Vec<TableConstraint>,
    ) -> Self {
        Self {
            target,
            name,
            columns,
            constraints,
        }
    }

    /// Returns the declaration target.
    pub const fn target(&self) -> MigrationBackendTarget {
        self.target
    }

    /// Returns the qualified table name.
    pub fn name(&self) -> &QualifiedTableName {
        &self.name
    }

    /// Returns columns in source order.
    pub fn columns(&self) -> &[MigrationColumn] {
        &self.columns
    }

    /// Returns constraints in source order.
    pub fn constraints(&self) -> &[TableConstraint] {
        &self.constraints
    }

    fn column(&self, name: &Identifier) -> Option<&MigrationColumn> {
        self.columns.iter().find(|column| column.name() == name)
    }
}

/// The target of a public privilege revocation.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum PublicPrivilegeTarget {
    /// A schema's public privileges.
    Schema(Identifier),
    /// A table's public privileges.
    Table(QualifiedTableName),
}

/// A typed `REVOKE ALL ... FROM PUBLIC` operation.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PublicPrivilegeRevocation {
    target_backend: MigrationBackendTarget,
    target: PublicPrivilegeTarget,
}

impl PublicPrivilegeRevocation {
    /// Constructs a common privilege revocation.
    pub fn new(target: PublicPrivilegeTarget) -> Self {
        Self::for_target(MigrationBackendTarget::Common, target)
    }

    /// Constructs a privilege revocation for one backend target.
    pub fn for_target(
        target_backend: MigrationBackendTarget,
        target: PublicPrivilegeTarget,
    ) -> Self {
        Self {
            target_backend,
            target,
        }
    }

    /// Returns the declaration target.
    pub const fn target_backend(&self) -> MigrationBackendTarget {
        self.target_backend
    }

    /// Returns the revocation target.
    pub fn target(&self) -> &PublicPrivilegeTarget {
        &self.target
    }
}

/// A complete typed migration document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Migration {
    version: u32,
    name: Identifier,
    schemas: Vec<MigrationSchema>,
    tables: Vec<MigrationTable>,
    revocations: Vec<PublicPrivilegeRevocation>,
}

impl Migration {
    /// Constructs and validates a typed migration document.
    pub fn try_new(
        version: u32,
        name: Identifier,
        schemas: Vec<MigrationSchema>,
        tables: Vec<MigrationTable>,
        revocations: Vec<PublicPrivilegeRevocation>,
    ) -> Result<Self, MigrationValidationError> {
        let migration = Self {
            version,
            name,
            schemas,
            tables,
            revocations,
        };
        migration.validate()?;
        Ok(migration)
    }

    /// Parses one typed migration source document.
    pub fn parse(source: &str) -> Result<Self, MigrationParseError> {
        parse_migration(source)
    }

    /// Returns the migration version.
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Returns the migration name.
    pub fn name(&self) -> &Identifier {
        &self.name
    }

    /// Returns schemas in source order.
    pub fn schemas(&self) -> &[MigrationSchema] {
        &self.schemas
    }

    /// Returns tables in source order.
    pub fn tables(&self) -> &[MigrationTable] {
        &self.tables
    }

    /// Returns public privilege revocations in source order.
    pub fn revocations(&self) -> &[PublicPrivilegeRevocation] {
        &self.revocations
    }

    /// Re-validates all neutral-IR invariants.
    pub fn validate(&self) -> Result<(), MigrationValidationError> {
        if self.version == 0 {
            return Err(MigrationValidationError::InvalidVersion);
        }

        let mut schema_names = Vec::with_capacity(self.schemas.len());
        for schema in &self.schemas {
            if schema_names.iter().any(|name| name == schema.name()) {
                return Err(MigrationValidationError::DuplicateSchema(
                    schema.name.clone(),
                ));
            }
            schema_names.push(schema.name.clone());
        }

        let mut table_names = Vec::with_capacity(self.tables.len());
        for table in &self.tables {
            if !schema_names.iter().any(|name| name == table.name.schema()) {
                return Err(MigrationValidationError::UnknownSchema {
                    schema: table.name.schema.clone(),
                });
            }
            if table_names.iter().any(|name| name == table.name()) {
                return Err(MigrationValidationError::DuplicateTable(table.name.clone()));
            }
            table_names.push(table.name.clone());
            self.validate_table(table)?;
        }

        for revocation in &self.revocations {
            let declared_target = match revocation.target() {
                PublicPrivilegeTarget::Schema(schema) => {
                    if !schema_names.iter().any(|name| name == schema) {
                        return Err(MigrationValidationError::UnknownPrivilegeTarget(
                            revocation.target.clone(),
                        ));
                    }
                    MigrationBackendTarget::Common
                }
                PublicPrivilegeTarget::Table(table) => {
                    let Some(declared_table) =
                        self.tables.iter().find(|declared| declared.name() == table)
                    else {
                        return Err(MigrationValidationError::UnknownPrivilegeTarget(
                            revocation.target.clone(),
                        ));
                    };
                    declared_table.target()
                }
            };
            if !revocation.target_backend.covers(declared_target) {
                return Err(MigrationValidationError::PrivilegeTargetBackendMismatch {
                    target_backend: revocation.target_backend,
                    target: revocation.target.clone(),
                });
            }
        }

        Ok(())
    }

    fn validate_table(&self, table: &MigrationTable) -> Result<(), MigrationValidationError> {
        if table.columns.is_empty() {
            return Err(MigrationValidationError::EmptyTable {
                table: table.name.clone(),
            });
        }
        let mut column_names = Vec::with_capacity(table.columns.len());
        for column in &table.columns {
            if column_names.iter().any(|name| name == column.name()) {
                return Err(MigrationValidationError::DuplicateColumn {
                    table: table.name.clone(),
                    column: column.name.clone(),
                });
            }
            if column.default().is_some_and(|default| {
                !matches!(default, DefaultValue::CurrentTimestamp)
                    || column.scalar_type() != ScalarType::Timestamp
            }) {
                return Err(MigrationValidationError::DefaultTypeMismatch {
                    table: table.name.clone(),
                    column: column.name.clone(),
                });
            }
            column_names.push(column.name.clone());
        }

        let mut has_primary_key = false;
        for constraint in &table.constraints {
            match constraint {
                TableConstraint::PrimaryKey(columns) | TableConstraint::Unique(columns) => {
                    if matches!(constraint, TableConstraint::PrimaryKey(_)) {
                        if has_primary_key {
                            return Err(MigrationValidationError::MultiplePrimaryKeys {
                                table: table.name.clone(),
                            });
                        }
                        has_primary_key = true;
                    }
                    if columns.is_empty() {
                        return Err(MigrationValidationError::EmptyConstraint {
                            table: table.name.clone(),
                        });
                    }
                    let mut seen = Vec::with_capacity(columns.len());
                    for column in columns {
                        if !column_names.iter().any(|name| name == column) {
                            return Err(MigrationValidationError::UnknownColumn {
                                table: table.name.clone(),
                                column: column.clone(),
                            });
                        }
                        if seen.iter().any(|name| name == column) {
                            return Err(MigrationValidationError::DuplicateConstraintColumn {
                                table: table.name.clone(),
                                column: column.clone(),
                            });
                        }
                        if matches!(constraint, TableConstraint::PrimaryKey(_))
                            && table
                                .column(column)
                                .is_some_and(|column| column.nullability() == Nullability::Nullable)
                        {
                            return Err(MigrationValidationError::NullablePrimaryKey {
                                table: table.name.clone(),
                                column: column.clone(),
                            });
                        }
                        seen.push(column.clone());
                    }
                }
                TableConstraint::Check(check) => {
                    let column_name = check.column();
                    let Some(column) = table.column(column_name) else {
                        return Err(MigrationValidationError::UnknownColumn {
                            table: table.name.clone(),
                            column: column_name.clone(),
                        });
                    };
                    if let CheckConstraint::GreaterOrEqual { other, .. } = check {
                        let Some(other_column) = table.column(other) else {
                            return Err(MigrationValidationError::UnknownColumn {
                                table: table.name.clone(),
                                column: other.clone(),
                            });
                        };
                        if !matches!(
                            other_column.scalar_type(),
                            ScalarType::Integer | ScalarType::BigInt
                        ) {
                            return Err(MigrationValidationError::CheckTypeMismatch {
                                table: table.name.clone(),
                                column: other.clone(),
                            });
                        }
                        if other_column.nullability() == Nullability::Nullable {
                            return Err(MigrationValidationError::CheckNullabilityMismatch {
                                table: table.name.clone(),
                                column: other.clone(),
                                expected: Nullability::NotNull,
                            });
                        }
                    }
                    let valid = match check {
                        CheckConstraint::NonNegative { .. }
                        | CheckConstraint::Positive { .. }
                        | CheckConstraint::GreaterOrEqual { .. } => {
                            matches!(
                                column.scalar_type(),
                                ScalarType::Integer | ScalarType::BigInt
                            )
                        }
                        CheckConstraint::EqualsInteger { value, .. } => {
                            match column.scalar_type() {
                                ScalarType::Integer => *value <= i32::MAX as u64,
                                ScalarType::BigInt => *value <= i64::MAX as u64,
                                _ => false,
                            }
                        }
                        CheckConstraint::NonEmpty { .. } => {
                            matches!(column.scalar_type(), ScalarType::Text | ScalarType::Bytes)
                        }
                        CheckConstraint::ByteLength { bytes, .. }
                        | CheckConstraint::NullableByteLength { bytes, .. } => {
                            *bytes > 0 && column.scalar_type() == ScalarType::Bytes
                        }
                    };
                    if !valid {
                        return Err(MigrationValidationError::CheckTypeMismatch {
                            table: table.name.clone(),
                            column: column_name.clone(),
                        });
                    }
                    let expected = if matches!(check, CheckConstraint::NullableByteLength { .. }) {
                        Nullability::Nullable
                    } else {
                        Nullability::NotNull
                    };
                    if column.nullability() != expected {
                        return Err(MigrationValidationError::CheckNullabilityMismatch {
                            table: table.name.clone(),
                            column: column_name.clone(),
                            expected,
                        });
                    }
                }
            }
        }

        Ok(())
    }
}

/// Validation failures for a typed migration IR.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationValidationError {
    /// Migration versions are positive ordinals.
    InvalidVersion,
    /// Schema names cannot be repeated.
    DuplicateSchema(Identifier),
    /// A table references a schema not declared in the migration.
    UnknownSchema { schema: Identifier },
    /// Qualified table names cannot be repeated.
    DuplicateTable(QualifiedTableName),
    /// A table must declare at least one column.
    EmptyTable { table: QualifiedTableName },
    /// A table cannot declare more than one primary key.
    MultiplePrimaryKeys { table: QualifiedTableName },
    /// Column names cannot be repeated in one table.
    DuplicateColumn {
        table: QualifiedTableName,
        column: Identifier,
    },
    /// A key or unique constraint must contain at least one column.
    EmptyConstraint { table: QualifiedTableName },
    /// A constraint references a column not declared by its table.
    UnknownColumn {
        table: QualifiedTableName,
        column: Identifier,
    },
    /// One key or unique tuple cannot repeat a column.
    DuplicateConstraintColumn {
        table: QualifiedTableName,
        column: Identifier,
    },
    /// Primary-key columns must be explicitly non-null.
    NullablePrimaryKey {
        table: QualifiedTableName,
        column: Identifier,
    },
    /// A typed default is incompatible with its column type.
    DefaultTypeMismatch {
        table: QualifiedTableName,
        column: Identifier,
    },
    /// A check predicate is incompatible with its column type.
    CheckTypeMismatch {
        table: QualifiedTableName,
        column: Identifier,
    },
    /// A check predicate's nullability semantics are incompatible with its column.
    CheckNullabilityMismatch {
        table: QualifiedTableName,
        column: Identifier,
        expected: Nullability,
    },
    /// A privilege revocation targets a backend where its declaration is not emitted.
    PrivilegeTargetBackendMismatch {
        target_backend: MigrationBackendTarget,
        target: PublicPrivilegeTarget,
    },
    /// A privilege revocation names no declared schema or table.
    UnknownPrivilegeTarget(PublicPrivilegeTarget),
}

impl fmt::Display for MigrationValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidVersion => formatter.write_str("migration version must be positive"),
            Self::DuplicateSchema(schema) => {
                write!(formatter, "duplicate migration schema {schema}")
            }
            Self::UnknownSchema { schema } => {
                write!(
                    formatter,
                    "migration table references undeclared schema {schema}"
                )
            }
            Self::DuplicateTable(table) => write!(formatter, "duplicate migration table {table}"),
            Self::EmptyTable { table } => {
                write!(
                    formatter,
                    "migration table {table} must declare at least one column"
                )
            }
            Self::MultiplePrimaryKeys { table } => {
                write!(
                    formatter,
                    "migration table {table} declares multiple primary keys"
                )
            }
            Self::DuplicateColumn { table, column } => {
                write!(
                    formatter,
                    "duplicate column {column} in migration table {table}"
                )
            }
            Self::EmptyConstraint { table } => {
                write!(
                    formatter,
                    "empty table constraint in migration table {table}"
                )
            }
            Self::UnknownColumn { table, column } => {
                write!(
                    formatter,
                    "unknown column {column} in migration table {table}"
                )
            }
            Self::DuplicateConstraintColumn { table, column } => write!(
                formatter,
                "constraint repeats column {column} in migration table {table}"
            ),
            Self::NullablePrimaryKey { table, column } => write!(
                formatter,
                "primary-key column {column} is nullable in migration table {table}"
            ),
            Self::DefaultTypeMismatch { table, column } => write!(
                formatter,
                "timestamp default is incompatible with column {column} in migration table {table}"
            ),
            Self::CheckTypeMismatch { table, column } => write!(
                formatter,
                "check predicate is incompatible with column {column} in migration table {table}"
            ),
            Self::CheckNullabilityMismatch {
                table,
                column,
                expected,
            } => {
                let expected = match expected {
                    Nullability::NotNull => "NOT NULL",
                    Nullability::Nullable => "NULL",
                };
                write!(
                    formatter,
                    "check predicate requires column {column} to be {expected} in migration table {table}"
                )
            }
            Self::PrivilegeTargetBackendMismatch {
                target_backend,
                target,
            } => write!(
                formatter,
                "privilege revocation target {target:?} is incompatible with {target_backend:?} backend target"
            ),
            Self::UnknownPrivilegeTarget(target) => {
                write!(
                    formatter,
                    "privilege revocation names undeclared target {target:?}"
                )
            }
        }
    }
}

impl Error for MigrationValidationError {}

/// A parse or lexical failure in a typed migration source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationParseError {
    /// The lexer encountered a byte outside the closed source grammar.
    Lexical { position: usize, message: String },
    /// The parser encountered a token other than the expected construct.
    Unexpected {
        position: usize,
        expected: &'static str,
        found: String,
    },
    /// A numeric literal could not be represented by the typed IR.
    InvalidNumber { position: usize, value: String },
    /// A parsed identifier failed identifier validation.
    InvalidIdentifier {
        position: usize,
        source: IdentifierError,
    },
    /// The source used a construct outside the closed DSL.
    Unsupported {
        position: usize,
        message: &'static str,
    },
    /// The parsed document violated neutral-IR invariants.
    Validation(MigrationValidationError),
}

impl fmt::Display for MigrationParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lexical { position, message } => {
                write!(formatter, "migration source byte {position}: {message}")
            }
            Self::Unexpected {
                position,
                expected,
                found,
            } => write!(
                formatter,
                "migration source byte {position}: expected {expected}, found {found:?}"
            ),
            Self::InvalidNumber { position, value } => write!(
                formatter,
                "migration source byte {position}: invalid numeric literal {value:?}"
            ),
            Self::InvalidIdentifier { position, source } => {
                write!(formatter, "migration source byte {position}: {source}")
            }
            Self::Unsupported { position, message } => {
                write!(
                    formatter,
                    "migration source byte {position}: unsupported {message}"
                )
            }
            Self::Validation(error) => error.fmt(formatter),
        }
    }
}

impl Error for MigrationParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidIdentifier { source, .. } => Some(source),
            Self::Validation(error) => Some(error),
            _ => None,
        }
    }
}

/// The backend selected for deterministic SQL lowering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MigrationBackend {
    /// PostgreSQL's kernel schema and bytea/timestamptz types.
    Postgres,
    /// The current Turso/SQLite adapter's unqualified `orna_*` tables.
    Sqlite,
}

/// A backend lowering failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationRenderError {
    /// The neutral IR was invalid.
    Validation(MigrationValidationError),
    /// A valid neutral construct has no mapping for the selected backend.
    Unsupported {
        backend: MigrationBackend,
        message: &'static str,
    },
}

impl fmt::Display for MigrationRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => error.fmt(formatter),
            Self::Unsupported { backend, message } => {
                write!(
                    formatter,
                    "{backend:?} migration lowering does not support {message}"
                )
            }
        }
    }
}

impl Error for MigrationRenderError {}

/// A generated artifact mismatch against a typed source document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GeneratedArtifactMismatch {
    /// The source did not parse or validate.
    Source(MigrationParseError),
    /// The parsed IR could not be lowered for one of the backends.
    Render(MigrationRenderError),
    /// The checked-in PostgreSQL artifact differs from the renderer output.
    Postgres { expected: String, actual: String },
    /// The checked-in SQLite artifact differs from the renderer output.
    Sqlite { expected: String, actual: String },
}
impl fmt::Display for GeneratedArtifactMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => write!(formatter, "typed migration source is invalid: {error}"),
            Self::Render(error) => write!(formatter, "typed migration rendering failed: {error}"),
            Self::Postgres { .. } => formatter
                .write_str("checked-in PostgreSQL migration artifact differs from renderer"),
            Self::Sqlite { .. } => {
                formatter.write_str("checked-in SQLite migration artifact differs from renderer")
            }
        }
    }
}

impl Error for GeneratedArtifactMismatch {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::Render(error) => Some(error),
            _ => None,
        }
    }
}

/// Parses a checked-in typed migration source document.
pub fn parse_migration(source: &str) -> Result<Migration, MigrationParseError> {
    let tokens = lex(source)?;
    Parser::new(tokens).parse()
}

/// Renders a validated migration as deterministic PostgreSQL DDL.
pub fn render_postgres(migration: &Migration) -> Result<String, MigrationRenderError> {
    render(migration, MigrationBackend::Postgres)
}

/// Renders a validated migration as deterministic SQLite/Turso DDL.
pub fn render_sqlite(migration: &Migration) -> Result<String, MigrationRenderError> {
    render(migration, MigrationBackend::Sqlite)
}

/// Parses source and verifies both checked-in backend artifacts.
pub fn verify_generated_artifacts(
    source: &str,
    postgres_artifact: &str,
    sqlite_artifact: &str,
) -> Result<(), GeneratedArtifactMismatch> {
    let migration = parse_migration(source).map_err(GeneratedArtifactMismatch::Source)?;
    let expected_postgres =
        render_postgres(&migration).map_err(GeneratedArtifactMismatch::Render)?;
    if expected_postgres != postgres_artifact {
        return Err(GeneratedArtifactMismatch::Postgres {
            expected: expected_postgres,
            actual: postgres_artifact.to_owned(),
        });
    }
    let expected_sqlite = render_sqlite(&migration).map_err(GeneratedArtifactMismatch::Render)?;
    if expected_sqlite != sqlite_artifact {
        return Err(GeneratedArtifactMismatch::Sqlite {
            expected: expected_sqlite,
            actual: sqlite_artifact.to_owned(),
        });
    }
    Ok(())
}

fn render(
    migration: &Migration,
    backend: MigrationBackend,
) -> Result<String, MigrationRenderError> {
    migration
        .validate()
        .map_err(MigrationRenderError::Validation)?;
    if backend == MigrationBackend::Sqlite
        && migration
            .schemas
            .iter()
            .any(|schema| schema.name().as_str() != "_orna_kernel")
    {
        return Err(MigrationRenderError::Unsupported {
            backend,
            message: "schemas other than _orna_kernel",
        });
    }
    if backend == MigrationBackend::Sqlite
        && migration
            .revocations
            .iter()
            .any(|revocation| revocation.target_backend().applies_to(backend))
    {
        return Err(MigrationRenderError::Unsupported {
            backend,
            message: "SQLite privilege revocations",
        });
    }

    let table_count = migration
        .tables
        .iter()
        .filter(|table| table.target().applies_to(backend))
        .count();
    let revocation_count = migration
        .revocations
        .iter()
        .filter(|revocation| revocation.target_backend().applies_to(backend))
        .count();
    let mut output = String::new();
    if backend == MigrationBackend::Postgres {
        for schema in &migration.schemas {
            writeln!(output, "CREATE SCHEMA IF NOT EXISTS {};", schema.name())
                .expect("writing to String cannot fail");
        }
        if !migration.schemas.is_empty() {
            output.push('\n');
        }
    }

    let mut rendered_tables = 0;
    for table in &migration.tables {
        if !table.target().applies_to(backend) {
            continue;
        }
        let rendered_name = match backend {
            MigrationBackend::Postgres => table.name().to_string(),
            MigrationBackend::Sqlite => format!("orna_{}", table.name().table()),
        };
        let create_prefix = match backend {
            MigrationBackend::Postgres => "CREATE TABLE",
            MigrationBackend::Sqlite => "CREATE TABLE IF NOT EXISTS",
        };
        writeln!(output, "{create_prefix} {rendered_name} (")
            .expect("writing to String cannot fail");
        let has_constraints = !table.constraints().is_empty();
        for (column_index, column) in table.columns().iter().enumerate() {
            write!(
                output,
                "    {} {}",
                column.name(),
                render_scalar(column.scalar_type(), backend)
            )
            .expect("writing to String cannot fail");
            if column.nullability() == Nullability::NotNull {
                output.push_str(" NOT NULL");
            }
            if let Some(default) = column.default() {
                write!(output, " DEFAULT {}", render_default(default, backend))
                    .expect("writing to String cannot fail");
            }
            if column_index + 1 < table.columns().len() || has_constraints {
                output.push(',');
            }
            output.push('\n');
        }
        for (constraint_index, constraint) in table.constraints().iter().enumerate() {
            write!(
                output,
                "    {}",
                render_constraint(table, constraint, backend)
            )
            .expect("writing to String cannot fail");
            if constraint_index + 1 < table.constraints().len() {
                output.push(',');
            }
            output.push('\n');
        }
        output.push_str(");\n");
        rendered_tables += 1;
        if rendered_tables < table_count || (revocation_count > 0 && rendered_tables == table_count)
        {
            output.push('\n');
        }
    }

    let mut rendered_revocations = 0;
    for revocation in &migration.revocations {
        if !revocation.target_backend().applies_to(backend) {
            continue;
        }
        match revocation.target() {
            PublicPrivilegeTarget::Schema(schema) => {
                writeln!(output, "REVOKE ALL ON SCHEMA {schema} FROM PUBLIC;")
                    .expect("writing to String cannot fail");
            }
            PublicPrivilegeTarget::Table(table) => {
                writeln!(output, "REVOKE ALL ON TABLE {table} FROM PUBLIC;")
                    .expect("writing to String cannot fail");
            }
        }
        rendered_revocations += 1;
        if rendered_revocations < revocation_count {
            output.push('\n');
        }
    }

    Ok(output)
}

fn render_scalar(scalar_type: ScalarType, backend: MigrationBackend) -> &'static str {
    match (scalar_type, backend) {
        (ScalarType::Integer, MigrationBackend::Postgres) => "integer",
        (ScalarType::BigInt, MigrationBackend::Postgres) => "bigint",
        (ScalarType::Text, MigrationBackend::Postgres) => "text",
        (ScalarType::Bytes, MigrationBackend::Postgres) => "bytea",
        (ScalarType::Timestamp, MigrationBackend::Postgres) => "timestamp with time zone",
        (ScalarType::Integer | ScalarType::BigInt, MigrationBackend::Sqlite) => "INTEGER",
        (ScalarType::Text | ScalarType::Timestamp, MigrationBackend::Sqlite) => "TEXT",
        (ScalarType::Bytes, MigrationBackend::Sqlite) => "BLOB",
    }
}

fn render_default(default: DefaultValue, backend: MigrationBackend) -> &'static str {
    match (default, backend) {
        (DefaultValue::CurrentTimestamp, MigrationBackend::Postgres) => "transaction_timestamp()",
        (DefaultValue::CurrentTimestamp, MigrationBackend::Sqlite) => "CURRENT_TIMESTAMP",
    }
}

fn render_constraint(
    table: &MigrationTable,
    constraint: &TableConstraint,
    backend: MigrationBackend,
) -> String {
    match constraint {
        TableConstraint::PrimaryKey(columns) => {
            format!("PRIMARY KEY ({})", render_columns(columns))
        }
        TableConstraint::Unique(columns) => format!("UNIQUE ({})", render_columns(columns)),
        TableConstraint::Check(check) => {
            format!("CHECK ({})", render_check(table, check, backend))
        }
    }
}

fn render_columns(columns: &[Identifier]) -> String {
    columns
        .iter()
        .map(Identifier::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_check(
    table: &MigrationTable,
    check: &CheckConstraint,
    backend: MigrationBackend,
) -> String {
    let column = check.column();
    match check {
        CheckConstraint::NonNegative { .. } => format!("{column} >= 0"),
        CheckConstraint::Positive { .. } => format!("{column} > 0"),
        CheckConstraint::EqualsInteger { value, .. } => format!("{column} = {value}"),
        CheckConstraint::GreaterOrEqual { other, .. } => {
            format!("{column} >= {other}")
        }
        CheckConstraint::NonEmpty { .. } => {
            let function = if table
                .column(column)
                .is_some_and(|column| column.scalar_type() == ScalarType::Bytes)
                && backend == MigrationBackend::Postgres
            {
                "octet_length"
            } else {
                "length"
            };
            format!("{function}({column}) > 0")
        }
        CheckConstraint::ByteLength { bytes, .. } => {
            let function = if backend == MigrationBackend::Postgres {
                "octet_length"
            } else {
                "length"
            };
            format!("{function}({column}) = {bytes}")
        }
        CheckConstraint::NullableByteLength { bytes, .. } => {
            let function = if backend == MigrationBackend::Postgres {
                "octet_length"
            } else {
                "length"
            };
            format!("{column} IS NULL OR {function}({column}) = {bytes}")
        }
    }
}

fn is_reserved_identifier(value: &str) -> bool {
    matches!(
        value,
        "abort"
            | "action"
            | "add"
            | "after"
            | "all"
            | "alter"
            | "analyze"
            | "analyse"
            | "and"
            | "any"
            | "array"
            | "as"
            | "asc"
            | "asymmetric"
            | "attach"
            | "autoincrement"
            | "authorization"
            | "before"
            | "begin"
            | "between"
            | "binary"
            | "both"
            | "by"
            | "cascade"
            | "case"
            | "cast"
            | "check"
            | "collate"
            | "collation"
            | "column"
            | "commit"
            | "conflict"
            | "concurrently"
            | "constraint"
            | "copy"
            | "create"
            | "cross"
            | "current"
            | "current_catalog"
            | "current_date"
            | "current_role"
            | "current_schema"
            | "current_time"
            | "current_timestamp"
            | "current_user"
            | "database"
            | "default"
            | "deferrable"
            | "deferred"
            | "delete"
            | "desc"
            | "detach"
            | "distinct"
            | "do"
            | "domain"
            | "drop"
            | "each"
            | "else"
            | "end"
            | "escape"
            | "except"
            | "exclude"
            | "exclusive"
            | "exists"
            | "explain"
            | "fail"
            | "extension"
            | "false"
            | "fetch"
            | "freeze"
            | "filter"
            | "first"
            | "following"
            | "for"
            | "foreign"
            | "from"
            | "full"
            | "generated"
            | "glob"
            | "grant"
            | "group"
            | "groups"
            | "having"
            | "if"
            | "ignore"
            | "ilike"
            | "immediate"
            | "in"
            | "index"
            | "indexed"
            | "initially"
            | "inner"
            | "insert"
            | "instead"
            | "intersect"
            | "into"
            | "is"
            | "isnull"
            | "join"
            | "key"
            | "lateral"
            | "last"
            | "leading"
            | "left"
            | "like"
            | "limit"
            | "localtime"
            | "localtimestamp"
            | "match"
            | "materialized"
            | "migration"
            | "natural"
            | "no"
            | "not"
            | "nothing"
            | "notnull"
            | "null"
            | "nullif"
            | "nulls"
            | "of"
            | "offset"
            | "on"
            | "or"
            | "order"
            | "others"
            | "outer"
            | "over"
            | "overlaps"
            | "owned"
            | "owner"
            | "partition"
            | "placing"
            | "plan"
            | "pragma"
            | "postgres"
            | "only"
            | "preceding"
            | "primary"
            | "privileges"
            | "procedure"
            | "public"
            | "query"
            | "raise"
            | "range"
            | "recursive"
            | "references"
            | "regexp"
            | "reindex"
            | "release"
            | "rename"
            | "replace"
            | "restrict"
            | "returning"
            | "revoke"
            | "right"
            | "rollback"
            | "row"
            | "rows"
            | "savepoint"
            | "schema"
            | "security"
            | "select"
            | "sequence"
            | "server"
            | "session_user"
            | "system_user"
            | "set"
            | "similar"
            | "symmetric"
            | "some"
            | "sqlite"
            | "table"
            | "tablesample"
            | "temp"
            | "temporary"
            | "then"
            | "ties"
            | "to"
            | "trailing"
            | "transaction"
            | "trigger"
            | "true"
            | "type"
            | "unbounded"
            | "union"
            | "unique"
            | "update"
            | "user"
            | "using"
            | "vacuum"
            | "values"
            | "variadic"
            | "verbose"
            | "view"
            | "virtual"
            | "when"
            | "where"
            | "window"
            | "with"
            | "without"
            | "volatile"
            | "write"
            | "common"
            | "bigint"
            | "bytes"
            | "integer"
            | "text"
            | "timestamp"
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TokenKind {
    Identifier,
    Number,
    LBrace,
    RBrace,
    LParen,
    RParen,
    Comma,
    Dot,
    Semicolon,
    Greater,
    Equal,
    End,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Token {
    kind: TokenKind,
    text: String,
    position: usize,
}

fn lex(source: &str) -> Result<Vec<Token>, MigrationParseError> {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut tokens = Vec::new();
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'-' && bytes.get(index + 1) == Some(&b'-') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let start = index;
            index += 2;
            let mut closed = false;
            while index + 1 < bytes.len() {
                if bytes[index] == b'*' && bytes[index + 1] == b'/' {
                    index += 2;
                    closed = true;
                    break;
                }
                index += 1;
            }
            if !closed {
                return Err(MigrationParseError::Lexical {
                    position: start,
                    message: "unterminated block comment".to_owned(),
                });
            }
            continue;
        }
        let kind = match byte {
            b'{' => Some(TokenKind::LBrace),
            b'}' => Some(TokenKind::RBrace),
            b'(' => Some(TokenKind::LParen),
            b')' => Some(TokenKind::RParen),
            b',' => Some(TokenKind::Comma),
            b'.' => Some(TokenKind::Dot),
            b';' => Some(TokenKind::Semicolon),
            b'>' => Some(TokenKind::Greater),
            b'=' => Some(TokenKind::Equal),
            _ => None,
        };
        if let Some(kind) = kind {
            tokens.push(Token {
                kind,
                text: String::from_utf8_lossy(&bytes[index..=index]).into_owned(),
                position: index,
            });
            index += 1;
            continue;
        }
        if byte.is_ascii_digit() {
            let start = index;
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Number,
                text: source[start..index].to_owned(),
                position: start,
            });
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Identifier,
                text: source[start..index].to_owned(),
                position: start,
            });
            continue;
        }
        return Err(MigrationParseError::Lexical {
            position: index,
            message: "character is not part of the typed migration grammar".to_owned(),
        });
    }
    tokens.push(Token {
        kind: TokenKind::End,
        text: String::new(),
        position: source.len(),
    });
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, index: 0 }
    }

    fn parse(mut self) -> Result<Migration, MigrationParseError> {
        self.expect_keyword("MIGRATION")?;
        let version = self.parse_number_u32()?;
        let name = self.parse_identifier()?;
        self.expect_kind(TokenKind::LBrace, "'{' after migration header")?;

        let mut schemas = Vec::new();
        let mut tables = Vec::new();
        let mut revocations = Vec::new();
        while !self.at_kind(TokenKind::RBrace) {
            if self.consume_keyword("SCHEMA") {
                schemas.push(MigrationSchema::new(self.parse_identifier()?));
                self.expect_kind(TokenKind::Semicolon, "';' after schema declaration")?;
            } else if self.consume_keyword("TABLE") {
                tables.push(self.parse_table()?);
            } else if self.at_keyword("REVOKE") {
                revocations.push(self.parse_revocation()?);
            } else {
                return Err(self.unexpected("SCHEMA, TABLE, or REVOKE declaration"));
            }
        }
        self.expect_kind(TokenKind::RBrace, "'}' after migration declarations")?;
        self.expect_kind(TokenKind::End, "end of migration source")?;
        Migration::try_new(version, name, schemas, tables, revocations)
            .map_err(MigrationParseError::Validation)
    }

    fn parse_optional_target(&mut self) -> MigrationBackendTarget {
        if self.consume_keyword("POSTGRES") {
            MigrationBackendTarget::Postgres
        } else if self.consume_keyword("SQLITE") {
            MigrationBackendTarget::Sqlite
        } else {
            let _ = self.consume_keyword("COMMON") || self.consume_keyword("BOTH");
            MigrationBackendTarget::Common
        }
    }

    fn parse_table(&mut self) -> Result<MigrationTable, MigrationParseError> {
        let target = self.parse_optional_target();
        let name = self.parse_qualified_table_name()?;
        self.expect_kind(TokenKind::LBrace, "'{' after table name")?;
        let mut columns = Vec::new();
        let mut constraints = Vec::new();
        while !self.at_kind(TokenKind::RBrace) {
            if self.consume_keyword("COLUMN") {
                columns.push(self.parse_column()?);
            } else if self.consume_keyword("PRIMARY") {
                self.expect_keyword("KEY")?;
                constraints.push(TableConstraint::PrimaryKey(self.parse_column_list()?));
            } else if self.consume_keyword("UNIQUE") {
                constraints.push(TableConstraint::Unique(self.parse_column_list()?));
            } else if self.consume_keyword("CHECK") {
                constraints.push(TableConstraint::Check(self.parse_check()?));
            } else {
                return Err(self.unexpected("COLUMN, PRIMARY KEY, UNIQUE, or CHECK declaration"));
            }
        }
        self.expect_kind(TokenKind::RBrace, "'}' after table declarations")?;
        let _ = self.consume_kind(TokenKind::Semicolon);
        Ok(MigrationTable::for_target(
            target,
            name,
            columns,
            constraints,
        ))
    }

    fn parse_column(&mut self) -> Result<MigrationColumn, MigrationParseError> {
        let name = self.parse_identifier()?;
        let scalar_type = self.parse_scalar_type()?;
        let nullability = if self.consume_keyword("NOT") {
            self.expect_keyword("NULL")?;
            Nullability::NotNull
        } else if self.consume_keyword("NULL") {
            Nullability::Nullable
        } else {
            return Err(self.unexpected("NULL or NOT NULL"));
        };
        let default = if self.consume_keyword("DEFAULT") {
            if !self.consume_keyword("CURRENT_TIMESTAMP") {
                return Err(MigrationParseError::Unsupported {
                    position: self.current().position,
                    message: "a default expression other than CURRENT_TIMESTAMP",
                });
            }
            Some(DefaultValue::CurrentTimestamp)
        } else {
            None
        };
        self.expect_kind(TokenKind::Semicolon, "';' after column declaration")?;
        Ok(MigrationColumn::new(
            name,
            scalar_type,
            nullability,
            default,
        ))
    }

    fn parse_scalar_type(&mut self) -> Result<ScalarType, MigrationParseError> {
        let token = self.current();
        if token.kind != TokenKind::Identifier {
            return Err(self.unexpected("a scalar type"));
        }
        let scalar_type = if token.text.eq_ignore_ascii_case("INTEGER") {
            ScalarType::Integer
        } else if token.text.eq_ignore_ascii_case("BIGINT") {
            ScalarType::BigInt
        } else if token.text.eq_ignore_ascii_case("TEXT") {
            ScalarType::Text
        } else if token.text.eq_ignore_ascii_case("BYTES") {
            ScalarType::Bytes
        } else if token.text.eq_ignore_ascii_case("TIMESTAMP") {
            ScalarType::Timestamp
        } else {
            return Err(MigrationParseError::Unsupported {
                position: token.position,
                message: "a scalar type outside INTEGER, BIGINT, TEXT, BYTES, or TIMESTAMP",
            });
        };
        self.index += 1;
        Ok(scalar_type)
    }

    fn parse_column_list(&mut self) -> Result<Vec<Identifier>, MigrationParseError> {
        self.expect_kind(TokenKind::LParen, "'(' before constraint columns")?;
        let mut columns = vec![self.parse_identifier()?];
        while self.consume_kind(TokenKind::Comma) {
            columns.push(self.parse_identifier()?);
        }
        self.expect_kind(TokenKind::RParen, "')' after constraint columns")?;
        self.expect_kind(TokenKind::Semicolon, "';' after table constraint")?;
        Ok(columns)
    }

    fn parse_check(&mut self) -> Result<CheckConstraint, MigrationParseError> {
        self.expect_kind(TokenKind::LParen, "'(' after CHECK")?;
        let first = self.parse_identifier()?;
        let check = if self.consume_kind(TokenKind::Greater) {
            let equal = self.consume_kind(TokenKind::Equal);
            if equal && self.at_kind(TokenKind::Identifier) {
                CheckConstraint::GreaterOrEqual {
                    column: first,
                    other: self.parse_identifier()?,
                }
            } else {
                let number = self.parse_number_u64()?;
                if number != 0 {
                    return Err(MigrationParseError::Unsupported {
                        position: self.previous().position,
                        message: "a numeric CHECK threshold other than zero",
                    });
                }
                if equal {
                    CheckConstraint::NonNegative { column: first }
                } else {
                    CheckConstraint::Positive { column: first }
                }
            }
        } else if self.consume_kind(TokenKind::Equal) {
            CheckConstraint::EqualsInteger {
                column: first,
                value: self.parse_number_u64()?,
            }
        } else if self.consume_keyword("IS") {
            self.expect_keyword("NULL")?;
            self.expect_keyword("OR")?;
            let function = self.parse_identifier()?;
            if function.as_str() != "length" && function.as_str() != "octet_length" {
                return Err(MigrationParseError::Unsupported {
                    position: self.previous().position,
                    message: "a nullable length CHECK outside the typed predicate vocabulary",
                });
            }
            self.expect_kind(TokenKind::LParen, "'(' after a nullable length function")?;
            let column = self.parse_identifier()?;
            self.expect_kind(
                TokenKind::RParen,
                "')' after a nullable length function argument",
            )?;
            if column != first {
                return Err(MigrationParseError::Unsupported {
                    position: self.previous().position,
                    message: "a nullable length CHECK must repeat its nullable column",
                });
            }
            self.expect_kind(TokenKind::Equal, "'=' in a nullable length CHECK")?;
            let number = self.parse_number_u64()?;
            let bytes = u16::try_from(number).map_err(|_| MigrationParseError::InvalidNumber {
                position: self.previous().position,
                value: number.to_string(),
            })?;
            if bytes == 0 {
                return Err(MigrationParseError::Unsupported {
                    position: self.previous().position,
                    message: "a nullable byte-length CHECK with a zero length",
                });
            }
            CheckConstraint::NullableByteLength {
                column: first,
                bytes,
            }
        } else if first.as_str() == "length" || first.as_str() == "octet_length" {
            self.expect_kind(TokenKind::LParen, "'(' after a length function")?;
            let column = self.parse_identifier()?;
            self.expect_kind(TokenKind::RParen, "')' after a length function argument")?;
            let operator = if self.consume_kind(TokenKind::Greater) {
                ">"
            } else if self.consume_kind(TokenKind::Equal) {
                "="
            } else {
                return Err(self.unexpected("'=' or '>' in a length CHECK"));
            };
            let number = self.parse_number_u64()?;
            if operator == "=" {
                let bytes =
                    u16::try_from(number).map_err(|_| MigrationParseError::InvalidNumber {
                        position: self.previous().position,
                        value: number.to_string(),
                    })?;
                if bytes == 0 || first.as_str() != "octet_length" {
                    return Err(MigrationParseError::Unsupported {
                        position: self.previous().position,
                        message: "a byte-length CHECK other than octet_length(column) = positive",
                    });
                }
                CheckConstraint::ByteLength { column, bytes }
            } else if number == 0
                && (first.as_str() == "length" || first.as_str() == "octet_length")
            {
                CheckConstraint::NonEmpty { column }
            } else {
                return Err(MigrationParseError::Unsupported {
                    position: self.previous().position,
                    message: "a length CHECK threshold other than zero",
                });
            }
        } else {
            return Err(MigrationParseError::Unsupported {
                position: self.previous().position,
                message: "a CHECK predicate outside the typed predicate vocabulary",
            });
        };
        self.expect_kind(TokenKind::RParen, "')' after CHECK predicate")?;
        self.expect_kind(TokenKind::Semicolon, "';' after CHECK constraint")?;
        Ok(check)
    }

    fn parse_revocation(&mut self) -> Result<PublicPrivilegeRevocation, MigrationParseError> {
        self.expect_keyword("REVOKE")?;
        let target_backend = self.parse_optional_target();
        self.expect_keyword("ALL")?;
        self.expect_keyword("PRIVILEGES")?;
        self.expect_keyword("ON")?;
        let target = if self.consume_keyword("SCHEMA") {
            PublicPrivilegeTarget::Schema(self.parse_identifier()?)
        } else if self.consume_keyword("TABLE") {
            PublicPrivilegeTarget::Table(self.parse_qualified_table_name()?)
        } else {
            return Err(self.unexpected("SCHEMA or TABLE after REVOKE ALL PRIVILEGES ON"));
        };
        self.expect_keyword("FROM")?;
        self.expect_keyword("PUBLIC")?;
        self.expect_kind(TokenKind::Semicolon, "';' after privilege revocation")?;
        Ok(PublicPrivilegeRevocation::for_target(
            target_backend,
            target,
        ))
    }

    fn parse_qualified_table_name(&mut self) -> Result<QualifiedTableName, MigrationParseError> {
        let schema = self.parse_identifier()?;
        self.expect_kind(TokenKind::Dot, "'.' between schema and table")?;
        let table = self.parse_identifier()?;
        Ok(QualifiedTableName::new(schema, table))
    }

    fn parse_identifier(&mut self) -> Result<Identifier, MigrationParseError> {
        let token = self.current().clone();
        if token.kind != TokenKind::Identifier {
            return Err(self.unexpected("an identifier"));
        }
        self.index += 1;
        Identifier::new(token.text).map_err(|source| MigrationParseError::InvalidIdentifier {
            position: token.position,
            source,
        })
    }

    fn parse_number_u32(&mut self) -> Result<u32, MigrationParseError> {
        let value = self.parse_number_u64()?;
        u32::try_from(value).map_err(|_| MigrationParseError::InvalidNumber {
            position: self.previous().position,
            value: value.to_string(),
        })
    }

    fn parse_number_u64(&mut self) -> Result<u64, MigrationParseError> {
        let token = self.current().clone();
        if token.kind != TokenKind::Number {
            return Err(self.unexpected("a non-negative integer"));
        }
        self.index += 1;
        token
            .text
            .parse::<u64>()
            .map_err(|_| MigrationParseError::InvalidNumber {
                position: token.position,
                value: token.text,
            })
    }

    fn expect_keyword(&mut self, keyword: &'static str) -> Result<(), MigrationParseError> {
        if self.consume_keyword(keyword) {
            Ok(())
        } else {
            Err(self.unexpected(keyword))
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        if self.at_keyword(keyword) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn at_keyword(&self, keyword: &str) -> bool {
        let token = self.current();
        token.kind == TokenKind::Identifier && token.text.eq_ignore_ascii_case(keyword)
    }

    fn expect_kind(
        &mut self,
        kind: TokenKind,
        expected: &'static str,
    ) -> Result<(), MigrationParseError> {
        if self.consume_kind(kind) {
            Ok(())
        } else {
            Err(self.unexpected(expected))
        }
    }

    fn consume_kind(&mut self, kind: TokenKind) -> bool {
        if self.at_kind(kind) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn at_kind(&self, kind: TokenKind) -> bool {
        self.current().kind == kind
    }

    fn current(&self) -> &Token {
        &self.tokens[self.index]
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.index - 1]
    }

    fn unexpected(&self, expected: &'static str) -> MigrationParseError {
        let token = self.current();
        MigrationParseError::Unexpected {
            position: token.position,
            expected,
            found: if token.kind == TokenKind::End {
                "end of source".to_owned()
            } else {
                token.text.clone()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSTGRES_ARTIFACT: &str =
        include_str!("../../orna-postgres/migrations/0046_application_migrations.sql");
    const SQLITE_ARTIFACT: &str =
        include_str!("../../orna-sqlite/migrations/0001_revision_store.sql");

    #[test]
    fn canonical_source_parses_into_typed_backend_targeted_ir() {
        let migration = parse_migration(APPLICATION_MIGRATION_SOURCE).expect("canonical source");
        assert_eq!(migration.version(), 46);
        assert_eq!(migration.name().as_str(), "application_migrations");
        assert_eq!(migration.schemas().len(), 1);
        assert_eq!(migration.tables().len(), 7);
        assert_eq!(
            migration
                .tables()
                .iter()
                .filter(|table| table.target() == MigrationBackendTarget::Sqlite)
                .count(),
            6
        );
        let ledger = migration
            .tables()
            .iter()
            .find(|table| table.name().table().as_str() == "application_migrations")
            .expect("common application ledger");
        assert_eq!(ledger.target(), MigrationBackendTarget::Common);
        assert_eq!(ledger.columns().len(), 10);
        assert_eq!(ledger.constraints().len(), 11);
        assert_eq!(migration.revocations().len(), 2);
        assert!(
            migration
                .revocations()
                .iter()
                .all(|revocation| revocation.target_backend() == MigrationBackendTarget::Postgres)
        );
        let active = migration
            .tables()
            .iter()
            .find(|table| table.name().table().as_str() == "active_revision")
            .expect("SQLite active revision table");
        assert!(active.constraints().iter().any(|constraint| matches!(
            constraint,
            TableConstraint::Check(CheckConstraint::NullableByteLength { column, bytes })
                if column.as_str() == "source_parent_revision_id" && *bytes == 16
        )));
    }

    #[test]
    fn renderers_are_deterministic() {
        let migration = parse_migration(APPLICATION_MIGRATION_SOURCE).expect("canonical source");
        assert_eq!(render_postgres(&migration), render_postgres(&migration));
        assert_eq!(render_sqlite(&migration), render_sqlite(&migration));
    }

    #[test]
    fn unsupported_sql_semantics_are_rejected() {
        let source = "MIGRATION 1 example { SCHEMA _orna_kernel; TABLE _orna_kernel.example { COLUMN value TEXT NOT NULL DEFAULT 'user'; } }";
        assert!(matches!(
            parse_migration(source),
            Err(MigrationParseError::Unsupported { .. }) | Err(MigrationParseError::Lexical { .. })
        ));
    }

    #[test]
    fn unsupported_constraint_semantics_are_rejected() {
        let source = "MIGRATION 1 example { SCHEMA _orna_kernel; TABLE _orna_kernel.example { COLUMN value INTEGER NOT NULL; CHECK (value >= 1); } }";
        assert!(matches!(
            parse_migration(source),
            Err(MigrationParseError::Unsupported { .. })
        ));
    }

    #[test]
    fn postgres_targeted_revocations_are_not_emitted_in_sqlite() {
        let migration = parse_migration(APPLICATION_MIGRATION_SOURCE).expect("canonical source");
        let sqlite = render_sqlite(&migration).expect("SQLite rendering");
        assert!(!sqlite.contains("REVOKE"));
    }

    #[test]
    fn identifiers_reject_sql_keywords_and_overlong_names() {
        assert!(Identifier::new("select").is_err());
        assert!(Identifier::new("create").is_err());
        assert!(Identifier::new("a".repeat(63)).is_ok());
        assert!(Identifier::new("a".repeat(64)).is_err());
    }

    #[test]
    fn equals_integer_values_must_fit_signed_column_types() {
        let cases = [("INTEGER", "2147483648"), ("BIGINT", "9223372036854775808")];

        for (scalar_type, value) in cases {
            let source = format!(
                "MIGRATION 1 example {{ SCHEMA _orna_kernel; TABLE _orna_kernel.example {{ COLUMN value {scalar_type} NOT NULL; CHECK (value = {value}); }} }}"
            );
            let error = parse_migration(&source).expect_err("out-of-range value must be rejected");
            assert!(matches!(
                error,
                MigrationParseError::Validation(MigrationValidationError::CheckTypeMismatch {
                    column,
                    ..
                }) if column.as_str() == "value"
            ));
        }
    }

    #[test]
    fn non_nullable_check_predicates_reject_nullable_columns() {
        let cases = [
            ("COLUMN value INTEGER NULL;", "CHECK (value >= 0);", "value"),
            ("COLUMN value INTEGER NULL;", "CHECK (value > 0);", "value"),
            (
                "COLUMN value TEXT NULL;",
                "CHECK (length(value) > 0);",
                "value",
            ),
            (
                "COLUMN value BYTES NULL;",
                "CHECK (octet_length(value) = 16);",
                "value",
            ),
            ("COLUMN value INTEGER NULL;", "CHECK (value = 1);", "value"),
            (
                "COLUMN value INTEGER NULL; COLUMN other INTEGER NOT NULL;",
                "CHECK (value >= other);",
                "value",
            ),
            (
                "COLUMN value INTEGER NOT NULL; COLUMN other INTEGER NULL;",
                "CHECK (value >= other);",
                "other",
            ),
        ];

        for (columns, check, column) in cases {
            let source = format!(
                "MIGRATION 1 example {{ SCHEMA _orna_kernel; TABLE _orna_kernel.example {{ {columns} {check} }} }}"
            );
            let error = parse_migration(&source).expect_err("nullable check must be rejected");
            assert!(matches!(
                error,
                MigrationParseError::Validation(
                    MigrationValidationError::CheckNullabilityMismatch {
                        column: ref actual,
                        expected: Nullability::NotNull,
                        ..
                    }
                ) if actual.as_str() == column
            ));
        }
    }

    #[test]
    fn nullable_byte_length_requires_nullable_bytes_column() {
        let source = "MIGRATION 1 example { SCHEMA _orna_kernel; TABLE _orna_kernel.example { COLUMN value BYTES NOT NULL; CHECK (value IS NULL OR length(value) = 16); } }";
        let error = parse_migration(source).expect_err("nullable byte-length must require NULL");
        assert!(matches!(
            error,
            MigrationParseError::Validation(MigrationValidationError::CheckNullabilityMismatch {
                expected: Nullability::Nullable,
                ..
            })
        ));
    }

    #[test]
    fn privilege_revocations_must_match_table_backend_targets() {
        let cases = [
            ("SQLITE", "POSTGRES", MigrationBackendTarget::Postgres),
            ("POSTGRES", "SQLITE", MigrationBackendTarget::Sqlite),
            ("POSTGRES", "COMMON", MigrationBackendTarget::Common),
        ];

        for (table_backend, revoke_backend, expected_backend) in cases {
            let source = format!(
                "MIGRATION 1 example {{ SCHEMA _orna_kernel; TABLE {table_backend} _orna_kernel.example {{ COLUMN value INTEGER NOT NULL; }} REVOKE {revoke_backend} ALL PRIVILEGES ON TABLE _orna_kernel.example FROM PUBLIC; }}"
            );
            let error = parse_migration(&source).expect_err("mismatched revoke must be rejected");
            assert!(matches!(
                error,
                MigrationParseError::Validation(
                    MigrationValidationError::PrivilegeTargetBackendMismatch {
                        target_backend,
                        target: PublicPrivilegeTarget::Table(_),
                    }
                ) if target_backend == expected_backend
            ));
        }
    }

    #[test]
    fn tables_must_have_at_most_one_primary_key() {
        let source = "MIGRATION 1 example { SCHEMA _orna_kernel; TABLE _orna_kernel.example { COLUMN value INTEGER NOT NULL; PRIMARY KEY (value); PRIMARY KEY (value); } }";
        let error = parse_migration(source).expect_err("multiple primary keys must be rejected");
        assert!(matches!(
            error,
            MigrationParseError::Validation(MigrationValidationError::MultiplePrimaryKeys { .. })
        ));
    }

    #[test]
    fn tables_must_declare_at_least_one_column() {
        let source = "MIGRATION 1 example { SCHEMA _orna_kernel; TABLE _orna_kernel.example { } }";
        let error = parse_migration(source).expect_err("empty table must be rejected");
        assert!(matches!(
            error,
            MigrationParseError::Validation(MigrationValidationError::EmptyTable { .. })
        ));
    }

    #[test]
    fn checked_in_artifacts_match_typed_source() {
        verify_generated_artifacts(
            APPLICATION_MIGRATION_SOURCE,
            POSTGRES_ARTIFACT,
            SQLITE_ARTIFACT,
        )
        .expect("generated artifacts match typed source");
    }

    #[test]
    fn artifact_mismatch_is_reported() {
        let migration = parse_migration(APPLICATION_MIGRATION_SOURCE).expect("canonical source");
        let expected = render_postgres(&migration).expect("postgres rendering");
        let mut changed = expected.clone();
        changed.push_str("-- drift\n");
        let error =
            verify_generated_artifacts(APPLICATION_MIGRATION_SOURCE, &changed, SQLITE_ARTIFACT)
                .expect_err("drift must be rejected");
        assert!(matches!(error, GeneratedArtifactMismatch::Postgres { .. }));
    }
}
