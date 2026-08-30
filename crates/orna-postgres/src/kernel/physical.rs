//! PostgreSQL lowering for backend-neutral physical catalogue changes.

// Result APIs intentionally preserve the accepted public `PostgresKernelError` layout.
#![allow(clippy::result_large_err)]
#[path = "physical/verify.rs"]
mod verify;

use orna_core::{
    TypeId,
    catalogue::OnDeleteAction,
    physical::{AddField, CreateField, CreateObject, PhysicalFieldType, PhysicalPlan},
    types::StandardScalar,
};
use tokio_postgres::Transaction;

use crate::{
    PostgresKernelError,
    storage::{
        DATA_SCHEMA, OBJECT_ID_COLUMN, field_id_hex, field_name, relation_name, type_id_hex,
        unique_constraint_name,
    },
};

pub(crate) use verify::verify_physical_catalogue;

const TRUSTED_SEARCH_PATH_STATEMENT: &str = "SET LOCAL search_path = pg_catalog";

/// Installs one validated physical plan in the caller's transaction.
///
/// This function does not open, commit, or roll back the transaction. It
/// creates all relations and columns before it adds any reference constraint.
pub(crate) async fn install_physical_plan(
    transaction: &Transaction<'_>,
    plan: &PhysicalPlan,
) -> Result<(), PostgresKernelError> {
    establish_trusted_search_path(transaction).await?;
    let statements = lower_physical_plan(plan)?;
    for statement in statements.creates.iter().chain(&statements.references) {
        transaction
            .batch_execute(statement)
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    Ok(())
}

pub(crate) async fn establish_trusted_search_path(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    transaction
        .batch_execute(TRUSTED_SEARCH_PATH_STATEMENT)
        .await
        .map_err(PostgresKernelError::Database)
}

struct PhysicalStatements {
    creates: Vec<String>,
    references: Vec<String>,
}

fn lower_physical_plan(plan: &PhysicalPlan) -> Result<PhysicalStatements, PostgresKernelError> {
    let mut creates = Vec::with_capacity(
        plan.create_objects()
            .len()
            .saturating_mul(2)
            .saturating_add(usize::from(plan.add_field().is_some())),
    );
    let mut references = Vec::new();

    for object in plan.create_objects() {
        creates.push(create_table_statement(object)?);
        creates.push(revoke_table_statement(object.type_id()));
        references.extend(reference_statements(object));
    }
    if let Some(add_field) = plan.add_field() {
        creates.push(add_field_statement(add_field)?);
    }

    Ok(PhysicalStatements {
        creates,
        references,
    })
}

fn create_table_statement(object: &CreateObject) -> Result<String, PostgresKernelError> {
    let table = relation_name(object.type_id());
    let type_hex = type_id_hex(object.type_id());
    let mut definitions = vec![
        format!("{OBJECT_ID_COLUMN} bytea NOT NULL"),
        format!("CONSTRAINT pk_{type_hex} PRIMARY KEY ({OBJECT_ID_COLUMN})"),
        format!("CONSTRAINT ck_{type_hex}_object_id CHECK (octet_length({OBJECT_ID_COLUMN}) = 16)"),
    ];

    for field in object.fields() {
        definitions.push(field_definition(field)?);
        if matches!(field.field_type(), PhysicalFieldType::Reference { .. }) {
            let column = field_name(field.field_id());
            let field_hex = field_id_hex(field.field_id());
            definitions.push(format!(
                "CONSTRAINT ck_{field_hex}_object_id CHECK (octet_length({column}) = 16)"
            ));
        }
        if field.unique() {
            let column = field_name(field.field_id());
            definitions.push(format!(
                "CONSTRAINT {} UNIQUE ({column})",
                unique_constraint_name(field.field_id())
            ));
        }
    }

    Ok(format!(
        "CREATE TABLE {DATA_SCHEMA}.{table} (\n    {}\n);",
        definitions.join(",\n    ")
    ))
}

fn add_field_statement(add_field: &AddField) -> Result<String, PostgresKernelError> {
    Ok(format!(
        "ALTER TABLE {DATA_SCHEMA}.{}\n    ADD COLUMN {};",
        relation_name(add_field.object_type()),
        field_definition(add_field.field())?,
    ))
}

fn field_definition(field: &CreateField) -> Result<String, PostgresKernelError> {
    let column = field_name(field.field_id());
    let storage_type = match field.field_type() {
        PhysicalFieldType::Scalar(StandardScalar::CharacterLargeObject) if field.unique() => {
            "text COLLATE pg_catalog.\"C\""
        }
        PhysicalFieldType::Scalar(scalar) => scalar_storage_type(scalar)?,
        PhysicalFieldType::Enum(_) => "text",
        PhysicalFieldType::Record(_) => "bytea",
        PhysicalFieldType::Reference { .. } => "bytea",
    };
    let nullability = if field.nullable() { "" } else { " NOT NULL" };
    Ok(format!("{column} {storage_type}{nullability}"))
}

fn reference_statements(object: &CreateObject) -> Vec<String> {
    object
        .fields()
        .iter()
        .filter_map(|field| {
            let PhysicalFieldType::Reference { target, on_delete } = field.field_type() else {
                return None;
            };
            let table = relation_name(object.type_id());
            let target_table = relation_name(target);
            let column = field_name(field.field_id());
            let field_hex = field_id_hex(field.field_id());
            Some(format!(
                "ALTER TABLE {DATA_SCHEMA}.{table}\n    ADD CONSTRAINT fk_{field_hex}\n    FOREIGN KEY ({column})\n    REFERENCES {DATA_SCHEMA}.{target_table} ({OBJECT_ID_COLUMN})\n    ON DELETE {};",
                on_delete_sql(on_delete)
            ))
        })
        .collect()
}

fn revoke_table_statement(type_id: TypeId) -> String {
    format!(
        "REVOKE ALL ON TABLE {DATA_SCHEMA}.{} FROM PUBLIC;",
        relation_name(type_id)
    )
}

fn scalar_storage_type(scalar: StandardScalar) -> Result<&'static str, PostgresKernelError> {
    match scalar {
        StandardScalar::Boolean => Ok("boolean"),
        StandardScalar::Integer => Ok("integer"),
        StandardScalar::BigInt => Ok("bigint"),
        StandardScalar::Float => Ok("double precision"),
        StandardScalar::Decimal => Ok("numeric"),
        StandardScalar::CharacterLargeObject => Ok("text"),
        StandardScalar::BinaryLargeObject => Ok("bytea"),
        StandardScalar::Uuid => Ok("uuid"),
        StandardScalar::Date => Ok("date"),
        StandardScalar::Time => Ok("time without time zone"),
        StandardScalar::Timestamp => Ok("timestamp with time zone"),
        StandardScalar::Duration => Ok("interval"),
        StandardScalar::Void => Err(PostgresKernelError::CatalogueInvariant(
            "VOID cannot lower to a physical PostgreSQL column",
        )),
    }
}

const fn on_delete_sql(action: Option<OnDeleteAction>) -> &'static str {
    match action {
        None => "NO ACTION",
        Some(OnDeleteAction::Restrict) => "RESTRICT",
        Some(OnDeleteAction::SetNull) => "SET NULL",
        Some(OnDeleteAction::Cascade) => "CASCADE",
    }
}

#[cfg(test)]
#[path = "physical/tests.rs"]
mod tests;
