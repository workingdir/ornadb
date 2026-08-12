//! Fail-closed verification of PostgreSQL object storage.

use std::collections::{BTreeMap, BTreeSet};

use orna_core::{
    FieldId, TypeId,
    catalogue::OnDeleteAction,
    physical::{
        CreateField, CreateObject, PhysicalFieldType, PhysicalPlanError, active_physical_catalogue,
    },
    revision::ActiveDatabaseRevision,
    types::StandardScalar,
};
use tokio_postgres::{Row, Transaction};

use crate::{PostgresKernelError, decode::DurableRecord};

use super::{
    DATA_SCHEMA, OBJECT_ID_COLUMN, establish_trusted_search_path, field_id_hex, field_name,
    on_delete_sql, relation_name, scalar_storage_type, type_id_hex, unique_constraint_name,
};

const DATA_RELATION: &str = "_orna_data";

/// Verifies that PostgreSQL physical storage is exactly the active revision catalogue.
pub(crate) async fn verify_physical_catalogue(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
) -> Result<(), PostgresKernelError> {
    establish_trusted_search_path(transaction).await?;
    let expected = ExpectedCatalogue::from_active(active)?;
    verify_schema_access(transaction).await?;
    let relations = load_relations(transaction).await?;
    expected.verify_relations(&relations)?;

    for table in expected.tables.values() {
        verify_table(transaction, table).await?;
    }

    verify_reference_triggers(transaction, &expected.tables).await?;
    verify_external_dependants(transaction, expected.tables.keys()).await
}

#[derive(Debug, Eq, PartialEq)]
struct ExpectedCatalogue {
    tables: BTreeMap<String, ExpectedTable>,
}

impl ExpectedCatalogue {
    fn from_active(active: &ActiveDatabaseRevision) -> Result<Self, PostgresKernelError> {
        let physical = active_physical_catalogue(active).map_err(map_physical_projection_error)?;
        let mut tables = BTreeMap::new();
        for object in physical.objects() {
            let table = ExpectedTable::from_object(object)?;
            if tables.insert(table.name.clone(), table).is_some() {
                return Err(type_record(object.type_id())
                    .invariant("each object type must lower to one unique private relation name"));
            }
        }
        for table in tables.values() {
            for constraint in table.constraints.values() {
                if let Some(target) = &constraint.foreign_table
                    && !tables.contains_key(target)
                {
                    return Err(relation_record(&table.name).invariant(
                        "each physical reference target must be an object relation in the catalogue",
                    ));
                }
            }
        }
        Ok(Self { tables })
    }

    fn verify_relations(&self, relations: &[ObservedRelation]) -> Result<(), PostgresKernelError> {
        let mut observed = BTreeMap::new();
        for relation in relations {
            let record = relation_record(&relation.name);
            if relation.kind != "r" || relation.persistence != "p" {
                return Err(record
                    .invariant("every _orna_data relation must be an ordinary persistent table"));
            }
            if relation.row_security || relation.force_row_security {
                return Err(
                    record.invariant("_orna_data tables must not enable row-level security")
                );
            }
            if relation.public_privileges.iter().any(|granted| *granted) {
                return Err(
                    record.invariant("PUBLIC must have no table privilege on _orna_data relations")
                );
            }
            if observed.insert(relation.name.as_str(), relation).is_some() {
                return Err(record.invariant("each private relation name must be unique"));
            }
        }

        if observed.len() != self.tables.len() {
            return Err(DurableRecord::new(DATA_RELATION, DATA_SCHEMA).invariant(
                "_orna_data ordinary persistent table set must exactly match the catalogue",
            ));
        }
        for name in self.tables.keys() {
            if !observed.contains_key(name.as_str()) {
                return Err(relation_record(name).invariant(
                    "each catalogue object type must have its exact _orna_data relation",
                ));
            }
        }
        Ok(())
    }
}

fn map_physical_projection_error(error: PhysicalPlanError) -> PostgresKernelError {
    match error {
        PhysicalPlanError::UnsupportedUniqueField { object_type, field } => {
            field_record(object_type, field).invariant("only NOT NULL REF fields can be UNIQUE")
        }
        PhysicalPlanError::UnsupportedFieldDefault { object_type, field } => {
            field_record(object_type, field)
                .invariant("physical storage does not support field defaults")
        }
        PhysicalPlanError::UnsupportedVoidField { object_type, field } => {
            field_record(object_type, field)
                .invariant("VOID cannot lower to a physical PostgreSQL column")
        }
        error @ (PhysicalPlanError::ExpectedBaseMismatch { .. }
        | PhysicalPlanError::UnsupportedObjectDrop { .. }
        | PhysicalPlanError::UnsupportedExistingObjectChange { .. }
        | PhysicalPlanError::UnsupportedNamedFieldType { .. }
        | PhysicalPlanError::MissingValueTypeDefinition { .. }
        | PhysicalPlanError::UnsupportedValueTypeContract { .. }
        | PhysicalPlanError::TransientValueType { .. }
        | PhysicalPlanError::UnknownReferenceTarget { .. }
        | PhysicalPlanError::InvalidDeleteAction { .. }) => {
            PostgresKernelError::PhysicalPlan(error)
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ExpectedTable {
    name: String,
    columns: Vec<ExpectedColumn>,
    constraints: BTreeMap<String, ExpectedConstraint>,
}

impl ExpectedTable {
    fn from_object(object: &CreateObject) -> Result<Self, PostgresKernelError> {
        let type_hex = type_id_hex(object.type_id());
        let mut columns = vec![ExpectedColumn {
            name: OBJECT_ID_COLUMN.to_owned(),
            type_name: "bytea",
            nullable: false,
            reference: None,
        }];
        let mut constraints = BTreeMap::new();
        insert_expected_constraint(
            &mut constraints,
            format!("pk_{type_hex}"),
            ExpectedConstraint::primary_key(1),
            &type_record(object.type_id()),
        )?;
        insert_expected_constraint(
            &mut constraints,
            format!("ck_{type_hex}_object_id"),
            ExpectedConstraint::object_id_check(1, OBJECT_ID_COLUMN),
            &type_record(object.type_id()),
        )?;

        for field in object.fields() {
            let attribute = i16::try_from(columns.len() + 1).map_err(|_| {
                type_record(object.type_id()).invariant("object relation has too many fields")
            })?;
            let column = ExpectedColumn::from_field(object.type_id(), field)?;
            if let Some(reference) = column.reference {
                let field_hex = field_id_hex(field.field_id());
                let record = field_record(object.type_id(), field.field_id());
                insert_expected_constraint(
                    &mut constraints,
                    format!("ck_{field_hex}_object_id"),
                    ExpectedConstraint::object_id_check(attribute, &column.name),
                    &record,
                )?;
                insert_expected_constraint(
                    &mut constraints,
                    format!("fk_{field_hex}"),
                    ExpectedConstraint::reference(attribute, reference),
                    &record,
                )?;
            }
            if field.unique() {
                insert_expected_constraint(
                    &mut constraints,
                    unique_constraint_name(field.field_id()),
                    ExpectedConstraint::unique(attribute),
                    &field_record(object.type_id(), field.field_id()),
                )?;
            }
            columns.push(column);
        }

        Ok(Self {
            name: relation_name(object.type_id()),
            columns,
            constraints,
        })
    }
}

fn insert_expected_constraint(
    constraints: &mut BTreeMap<String, ExpectedConstraint>,
    name: String,
    constraint: ExpectedConstraint,
    record: &DurableRecord,
) -> Result<(), PostgresKernelError> {
    if constraints.insert(name, constraint).is_some() {
        return Err(record.invariant(
            "generated private constraint names must be unique across object and field identities",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExpectedReference {
    target: TypeId,
    on_delete: Option<OnDeleteAction>,
}

#[derive(Debug, Eq, PartialEq)]
struct ExpectedColumn {
    name: String,
    type_name: &'static str,
    nullable: bool,
    reference: Option<ExpectedReference>,
}

impl ExpectedColumn {
    fn from_field(owner: TypeId, field: &CreateField) -> Result<Self, PostgresKernelError> {
        let record = field_record(owner, field.field_id());
        let (type_name, reference) = match field.field_type() {
            PhysicalFieldType::Scalar(scalar) => (postgres_catalogue_type(scalar, &record)?, None),
            PhysicalFieldType::Enum(_) => ("text", None),
            PhysicalFieldType::Reference { target, on_delete } => {
                ("bytea", Some(ExpectedReference { target, on_delete }))
            }
        };

        Ok(Self {
            name: field_name(field.field_id()),
            type_name,
            nullable: field.nullable(),
            reference,
        })
    }
}

fn postgres_catalogue_type(
    scalar: StandardScalar,
    record: &DurableRecord,
) -> Result<&'static str, PostgresKernelError> {
    let storage = scalar_storage_type(scalar)?;
    match storage {
        "boolean" => Ok("bool"),
        "integer" => Ok("int4"),
        "bigint" => Ok("int8"),
        "double precision" => Ok("float8"),
        "numeric" => Ok("numeric"),
        "text" => Ok("text"),
        "bytea" => Ok("bytea"),
        "uuid" => Ok("uuid"),
        "date" => Ok("date"),
        "time without time zone" => Ok("time"),
        "timestamp with time zone" => Ok("timestamptz"),
        "interval" => Ok("interval"),
        _ => {
            Err(record.invariant("lowered scalar storage type must be a PostgreSQL catalogue type"))
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ExpectedConstraint {
    kind: &'static str,
    key: Vec<i16>,
    foreign_table: Option<String>,
    foreign_key: Option<Vec<i16>>,
    delete_action: Option<&'static str>,
    expression: Option<String>,
}

#[derive(Debug)]
struct ExpectedForeignKey {
    source: String,
    target: String,
    constraint: String,
}

impl ExpectedConstraint {
    fn primary_key(attribute: i16) -> Self {
        Self {
            kind: "p",
            key: vec![attribute],
            foreign_table: None,
            foreign_key: None,
            delete_action: None,
            expression: None,
        }
    }

    fn object_id_check(attribute: i16, column: &str) -> Self {
        Self {
            kind: "c",
            key: vec![attribute],
            foreign_table: None,
            foreign_key: None,
            delete_action: None,
            expression: Some(format!("(octet_length({column}) = 16)")),
        }
    }

    fn unique(attribute: i16) -> Self {
        Self {
            kind: "u",
            key: vec![attribute],
            foreign_table: None,
            foreign_key: None,
            delete_action: None,
            expression: None,
        }
    }

    fn reference(attribute: i16, reference: ExpectedReference) -> Self {
        Self {
            kind: "f",
            key: vec![attribute],
            foreign_table: Some(relation_name(reference.target)),
            foreign_key: Some(vec![1]),
            delete_action: Some(on_delete_sql(reference.on_delete)),
            expression: None,
        }
    }
}

#[derive(Debug)]
struct ObservedRelation {
    name: String,
    kind: String,
    persistence: String,
    row_security: bool,
    force_row_security: bool,
    public_privileges: [bool; 8],
}

async fn verify_schema_access(transaction: &Transaction<'_>) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT
                has_schema_privilege('public', namespace.oid, 'USAGE') AS public_usage,
                has_schema_privilege('public', namespace.oid, 'CREATE') AS public_create
             FROM pg_catalog.pg_namespace AS namespace
             WHERE namespace.nspname = '_orna_data'",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let record = DurableRecord::new(DATA_RELATION, DATA_SCHEMA);
    if rows.len() != 1 {
        return Err(record.invariant("_orna_data schema must exist exactly once"));
    }
    let usage: bool = record.column(&rows[0], "public_usage", "schema privilege must decode")?;
    let create: bool = record.column(&rows[0], "public_create", "schema privilege must decode")?;
    if usage || create {
        return Err(record.invariant("PUBLIC must have no USAGE or CREATE privilege on _orna_data"));
    }
    Ok(())
}

async fn load_relations(
    transaction: &Transaction<'_>,
) -> Result<Vec<ObservedRelation>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT
                class.relname AS name,
                class.relkind::text AS kind,
                class.relpersistence::text AS persistence,
                class.relrowsecurity AS row_security,
                class.relforcerowsecurity AS force_row_security,
                has_table_privilege('public', class.oid, 'SELECT') AS public_select,
                has_table_privilege('public', class.oid, 'INSERT') AS public_insert,
                has_table_privilege('public', class.oid, 'UPDATE') AS public_update,
                has_table_privilege('public', class.oid, 'DELETE') AS public_delete,
                has_table_privilege('public', class.oid, 'TRUNCATE') AS public_truncate,
                has_table_privilege('public', class.oid, 'REFERENCES') AS public_references,
                has_table_privilege('public', class.oid, 'TRIGGER') AS public_trigger,
                has_table_privilege('public', class.oid, 'MAINTAIN') AS public_maintain
             FROM pg_catalog.pg_class AS class
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
             WHERE namespace.nspname = '_orna_data'
               AND class.relkind <> 'i'
             ORDER BY class.relname",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .map(|row| {
            let initial = DurableRecord::new(DATA_RELATION, "relation row");
            let name: String = initial.column(row, "name", "relation name must decode")?;
            let record = relation_record(&name);
            Ok(ObservedRelation {
                name,
                kind: record.column(row, "kind", "relation kind must decode")?,
                persistence: record.column(
                    row,
                    "persistence",
                    "relation persistence must decode",
                )?,
                row_security: record.column(
                    row,
                    "row_security",
                    "relation RLS flag must decode",
                )?,
                force_row_security: record.column(
                    row,
                    "force_row_security",
                    "relation forced RLS flag must decode",
                )?,
                public_privileges: [
                    record.column(row, "public_select", "PUBLIC table privilege must decode")?,
                    record.column(row, "public_insert", "PUBLIC table privilege must decode")?,
                    record.column(row, "public_update", "PUBLIC table privilege must decode")?,
                    record.column(row, "public_delete", "PUBLIC table privilege must decode")?,
                    record.column(row, "public_truncate", "PUBLIC table privilege must decode")?,
                    record.column(
                        row,
                        "public_references",
                        "PUBLIC table privilege must decode",
                    )?,
                    record.column(row, "public_trigger", "PUBLIC table privilege must decode")?,
                    record.column(row, "public_maintain", "PUBLIC table privilege must decode")?,
                ],
            })
        })
        .collect()
}

async fn verify_table(
    transaction: &Transaction<'_>,
    table: &ExpectedTable,
) -> Result<(), PostgresKernelError> {
    verify_columns(transaction, table).await?;
    verify_constraints(transaction, table).await?;
    verify_indexes(transaction, table).await
}

async fn verify_columns(
    transaction: &Transaction<'_>,
    table: &ExpectedTable,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT
                attribute.attnum,
                attribute.attname AS name,
                attribute.attisdropped AS dropped,
                COALESCE(type_namespace.nspname, '') AS type_namespace,
                COALESCE(type.typname, '') AS type_name,
                attribute.attnotnull AS not_null,
                attribute.atttypmod AS type_modifier,
                attribute.attndims AS dimensions,
                COALESCE(attribute.attcollation = type.typcollation, false) AS matching_collation,
                attribute.atthasdef AS has_default,
                attribute.attidentity::text AS identity,
                attribute.attgenerated::text AS generated,
                has_column_privilege('public', class.oid, attribute.attnum, 'SELECT') AS public_select,
                has_column_privilege('public', class.oid, attribute.attnum, 'INSERT') AS public_insert,
                has_column_privilege('public', class.oid, attribute.attnum, 'UPDATE') AS public_update,
                has_column_privilege('public', class.oid, attribute.attnum, 'REFERENCES') AS public_references
             FROM pg_catalog.pg_class AS class
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
             JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = class.oid
             LEFT JOIN pg_catalog.pg_type AS type ON type.oid = attribute.atttypid
             LEFT JOIN pg_catalog.pg_namespace AS type_namespace ON type_namespace.oid = type.typnamespace
             WHERE namespace.nspname = '_orna_data'
               AND class.relname = $1
               AND attribute.attnum > 0
             ORDER BY attribute.attnum",
            &[&table.name],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let record = relation_record(&table.name);
    if rows.len() != table.columns.len() {
        return Err(record.invariant("relation columns must exactly match the catalogue field set"));
    }

    for (index, (row, expected)) in rows.iter().zip(&table.columns).enumerate() {
        let observed_record =
            DurableRecord::new(DATA_RELATION, format!("{}.{}", table.name, index + 1));
        let attnum: i16 = observed_record.column(row, "attnum", "column number must decode")?;
        let name: String = observed_record.column(row, "name", "column name must decode")?;
        let dropped: bool =
            observed_record.column(row, "dropped", "dropped column flag must decode")?;
        let type_namespace: String =
            observed_record.column(row, "type_namespace", "column type namespace must decode")?;
        let type_name: String =
            observed_record.column(row, "type_name", "column type name must decode")?;
        let not_null: bool =
            observed_record.column(row, "not_null", "column nullability must decode")?;
        let type_modifier: i32 =
            observed_record.column(row, "type_modifier", "column type modifier must decode")?;
        let dimensions: i16 =
            observed_record.column(row, "dimensions", "column dimensions must decode")?;
        let matching_collation: bool =
            observed_record.column(row, "matching_collation", "column collation must decode")?;
        let has_default: bool =
            observed_record.column(row, "has_default", "column default flag must decode")?;
        let identity: String =
            observed_record.column(row, "identity", "column identity flag must decode")?;
        let generated: String =
            observed_record.column(row, "generated", "column generated flag must decode")?;
        let public_privileges = [
            observed_record.column(row, "public_select", "PUBLIC column privilege must decode")?,
            observed_record.column(row, "public_insert", "PUBLIC column privilege must decode")?,
            observed_record.column(row, "public_update", "PUBLIC column privilege must decode")?,
            observed_record.column(
                row,
                "public_references",
                "PUBLIC column privilege must decode",
            )?,
        ];
        let expected_attnum = i16::try_from(index + 1)
            .map_err(|_| record.invariant("relation has more columns than PostgreSQL supports"))?;
        if attnum != expected_attnum
            || dropped
            || name != expected.name
            || type_namespace != "pg_catalog"
            || type_name != expected.type_name
            || not_null == expected.nullable
            || type_modifier != -1
            || dimensions != 0
            || !matching_collation
            || has_default
            || !identity.is_empty()
            || !generated.is_empty()
            || public_privileges.iter().any(|granted| *granted)
        {
            return Err(observed_record.invariant(
                "column must have the exact private name, PostgreSQL type, shape, and PUBLIC access",
            ));
        }
    }
    Ok(())
}

async fn verify_constraints(
    transaction: &Transaction<'_>,
    table: &ExpectedTable,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT
                catalogue_constraint.conname AS name,
                catalogue_constraint.contype::text AS kind,
                catalogue_constraint.conkey,
                target.relname AS target_table,
                target_namespace.nspname AS target_namespace,
                catalogue_constraint.confkey,
                catalogue_constraint.confdeltype::text AS delete_action,
                catalogue_constraint.confupdtype::text AS update_action,
                catalogue_constraint.confmatchtype::text AS match_type,
                catalogue_constraint.convalidated AS validated,
                catalogue_constraint.conenforced AS enforced,
                catalogue_constraint.conperiod AS period,
                catalogue_constraint.condeferrable AS deferrable,
                catalogue_constraint.condeferred AS deferred,
                catalogue_constraint.connoinherit AS no_inherit,
                pg_catalog.pg_get_expr(
                    catalogue_constraint.conbin,
                    catalogue_constraint.conrelid
                ) AS expression
             FROM pg_catalog.pg_constraint AS catalogue_constraint
             JOIN pg_catalog.pg_class AS class
               ON class.oid = catalogue_constraint.conrelid
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
             LEFT JOIN pg_catalog.pg_class AS target
               ON target.oid = catalogue_constraint.confrelid
             LEFT JOIN pg_catalog.pg_namespace AS target_namespace ON target_namespace.oid = target.relnamespace
             WHERE namespace.nspname = '_orna_data'
               AND class.relname = $1
             ORDER BY catalogue_constraint.contype, catalogue_constraint.conname",
            &[&table.name],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    let mut expected = table
        .constraints
        .iter()
        .map(|(name, constraint)| (name.clone(), constraint))
        .collect::<BTreeMap<_, _>>();
    let required_not_null = table
        .columns
        .iter()
        .enumerate()
        .filter(|(_, column)| !column.nullable)
        .map(|(index, _)| {
            i16::try_from(index + 1).map_err(|_| {
                relation_record(&table.name)
                    .invariant("relation has more columns than PostgreSQL supports")
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut observed_not_null = BTreeSet::new();

    for row in &rows {
        let initial = DurableRecord::new(DATA_RELATION, format!("{} constraint", table.name));
        let name: String = initial.column(row, "name", "constraint name must decode")?;
        let record = DurableRecord::new(DATA_RELATION, format!("{}.{}", table.name, name));
        let kind: String = record.column(row, "kind", "constraint kind must decode")?;
        let key: Option<Vec<i16>> = record.column(row, "conkey", "constraint key must decode")?;
        let target_table: Option<String> =
            record.column(row, "target_table", "constraint target must decode")?;
        let target_namespace: Option<String> = record.column(
            row,
            "target_namespace",
            "constraint target namespace must decode",
        )?;
        let foreign_key: Option<Vec<i16>> =
            record.column(row, "confkey", "constraint foreign key must decode")?;
        let delete_action: String =
            record.column(row, "delete_action", "constraint delete action must decode")?;
        let update_action: String =
            record.column(row, "update_action", "constraint update action must decode")?;
        let match_type: String =
            record.column(row, "match_type", "constraint match type must decode")?;
        let validated: bool =
            record.column(row, "validated", "constraint validation flag must decode")?;
        let enforced: bool =
            record.column(row, "enforced", "constraint enforcement flag must decode")?;
        let period: bool = record.column(row, "period", "constraint period flag must decode")?;
        let deferrable: bool =
            record.column(row, "deferrable", "constraint deferrable flag must decode")?;
        let deferred: bool =
            record.column(row, "deferred", "constraint deferred flag must decode")?;
        let no_inherit: bool =
            record.column(row, "no_inherit", "constraint inheritance flag must decode")?;
        let expression: Option<String> =
            record.column(row, "expression", "constraint expression must decode")?;

        if kind == "n" {
            let Some(key) = key else {
                return Err(record.invariant("NOT NULL constraint must name one relation column"));
            };
            if key.len() != 1 || !observed_not_null.insert(key[0]) {
                return Err(
                    record.invariant("each NOT NULL constraint must name one unique column")
                );
            }
            if !has_non_foreign_constraint_shape(
                target_table.as_deref(),
                target_namespace.as_deref(),
                foreign_key.as_deref(),
                &delete_action,
                &update_action,
                &match_type,
                expression.as_deref(),
            ) || !validated
                || !enforced
                || period
                || deferrable
                || deferred
                || no_inherit
            {
                return Err(record.invariant(
                    "NOT NULL constraints must have the exact local PG18 constraint shape",
                ));
            }
            continue;
        }

        let Some(expected_constraint) = expected.remove(&name) else {
            return Err(record.invariant("relation has an unexpected named constraint"));
        };
        verify_named_constraint(
            &record,
            &kind,
            key.as_deref(),
            target_table.as_deref(),
            target_namespace.as_deref(),
            foreign_key.as_deref(),
            &delete_action,
            &update_action,
            &match_type,
            validated,
            enforced,
            period,
            deferrable,
            deferred,
            no_inherit,
            expression.as_deref(),
            expected_constraint,
        )?;
    }

    if !expected.is_empty() {
        return Err(relation_record(&table.name).invariant(
            "relation is missing a required Orna-owned primary, unique, check, or foreign key constraint",
        ));
    }
    if observed_not_null != required_not_null {
        return Err(relation_record(&table.name)
            .invariant("PG18 NOT NULL constraints must exactly match private column nullability"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_named_constraint(
    record: &DurableRecord,
    kind: &str,
    key: Option<&[i16]>,
    target_table: Option<&str>,
    target_namespace: Option<&str>,
    foreign_key: Option<&[i16]>,
    delete_action: &str,
    update_action: &str,
    match_type: &str,
    validated: bool,
    enforced: bool,
    period: bool,
    deferrable: bool,
    deferred: bool,
    no_inherit: bool,
    expression: Option<&str>,
    expected: &ExpectedConstraint,
) -> Result<(), PostgresKernelError> {
    if kind != expected.kind
        || key != Some(expected.key.as_slice())
        || !validated
        || !enforced
        || period
        || deferrable
        || deferred
        || no_inherit != requires_no_inherit(expected.kind)
    {
        return Err(record.invariant(
            "named constraint must have the exact kind, key, and PostgreSQL enforcement flags",
        ));
    }
    match expected.kind {
        "c" => {
            if !has_non_foreign_constraint_shape(
                target_table,
                target_namespace,
                foreign_key,
                delete_action,
                update_action,
                match_type,
                None,
            ) || expression != expected.expression.as_deref()
            {
                return Err(record.invariant(
                    "object identity checks must use the exact octet_length expression",
                ));
            }
        }
        "f" => {
            let expected_delete = expected.delete_action.ok_or_else(|| {
                record.invariant("reference constraint must have an expected delete action")
            })?;
            let delete_code = delete_action_code(expected_delete).ok_or_else(|| {
                record.invariant(
                    "reference constraint delete action must be one supported Orna action",
                )
            })?;
            if target_table != expected.foreign_table.as_deref()
                || target_namespace != Some(DATA_SCHEMA)
                || foreign_key != expected.foreign_key.as_deref()
                || delete_action != delete_code
                || update_action != "a"
                || match_type != "s"
                || expression.is_some()
            {
                return Err(record.invariant(
                    "reference constraint must have the exact private target, action, and match semantics",
                ));
            }
        }
        "p" | "u" => {
            if !has_non_foreign_constraint_shape(
                target_table,
                target_namespace,
                foreign_key,
                delete_action,
                update_action,
                match_type,
                expression,
            ) {
                return Err(record.invariant(
                    "primary and unique constraints must not contain foreign or check state",
                ));
            }
        }
        _ => return Err(record.invariant("expected constraint kind must be supported")),
    }
    Ok(())
}

fn has_non_foreign_constraint_shape(
    target_table: Option<&str>,
    target_namespace: Option<&str>,
    foreign_key: Option<&[i16]>,
    delete_action: &str,
    update_action: &str,
    match_type: &str,
    expression: Option<&str>,
) -> bool {
    target_table.is_none()
        && target_namespace.is_none()
        && foreign_key.is_none()
        && delete_action == " "
        && update_action == " "
        && match_type == " "
        && expression.is_none()
}

fn requires_no_inherit(kind: &str) -> bool {
    matches!(kind, "p" | "u" | "f")
}

fn delete_action_code(action: &str) -> Option<&'static str> {
    match action {
        "NO ACTION" => Some("a"),
        "RESTRICT" => Some("r"),
        "SET NULL" => Some("n"),
        "CASCADE" => Some("c"),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpectedIndex {
    key: Vec<i16>,
    primary: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedIndex {
    valid: bool,
    ready: bool,
    unique: bool,
    primary: bool,
    immediate: bool,
    exclusion: bool,
    clustered: bool,
    replica_identity: bool,
    nulls_not_distinct: bool,
    key_attributes: i16,
    attributes: i16,
    key: Vec<i16>,
    no_predicate: bool,
    no_expression: bool,
}

impl ObservedIndex {
    fn from_row(row: &Row, record: &DurableRecord) -> Result<Self, PostgresKernelError> {
        Ok(Self {
            valid: record.column(row, "valid", "index valid flag must decode")?,
            ready: record.column(row, "ready", "index ready flag must decode")?,
            unique: record.column(row, "unique_index", "index unique flag must decode")?,
            primary: record.column(row, "primary_index", "index primary flag must decode")?,
            immediate: record.column(row, "immediate", "index immediate flag must decode")?,
            exclusion: record.column(row, "exclusion", "index exclusion flag must decode")?,
            clustered: record.column(row, "clustered", "index clustered flag must decode")?,
            replica_identity: record.column(
                row,
                "replica_identity",
                "index replica identity flag must decode",
            )?,
            nulls_not_distinct: record.column(
                row,
                "nulls_not_distinct",
                "index null treatment flag must decode",
            )?,
            key_attributes: record.column(row, "key_attributes", "index key count must decode")?,
            attributes: record.column(row, "attributes", "index attribute count must decode")?,
            key: record.column(row, "indkey", "index key columns must decode")?,
            no_predicate: record.column(row, "no_predicate", "index predicate flag must decode")?,
            no_expression: record.column(
                row,
                "no_expression",
                "index expression flag must decode",
            )?,
        })
    }

    fn has_expected_shape(&self, expected: &ExpectedIndex) -> bool {
        self.valid
            && self.ready
            && self.unique
            && self.primary == expected.primary
            && self.immediate
            && !self.exclusion
            && !self.clustered
            && !self.replica_identity
            && !self.nulls_not_distinct
            && self.key_attributes == 1
            && self.attributes == 1
            && self.key == expected.key
            && self.no_predicate
            && self.no_expression
    }
}

fn expected_indexes(table: &ExpectedTable) -> BTreeMap<String, ExpectedIndex> {
    table
        .constraints
        .iter()
        .filter_map(|(name, constraint)| {
            let primary = match constraint.kind {
                "p" => true,
                "u" => false,
                _ => return None,
            };
            Some((
                name.clone(),
                ExpectedIndex {
                    key: constraint.key.clone(),
                    primary,
                },
            ))
        })
        .collect()
}

async fn verify_indexes(
    transaction: &Transaction<'_>,
    table: &ExpectedTable,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT
                index_class.relname AS name,
                index.indisvalid AS valid,
                index.indisready AS ready,
                index.indisunique AS unique_index,
                index.indisprimary AS primary_index,
                index.indimmediate AS immediate,
                index.indisexclusion AS exclusion,
                index.indisclustered AS clustered,
                index.indisreplident AS replica_identity,
                index.indnullsnotdistinct AS nulls_not_distinct,
                index.indnkeyatts AS key_attributes,
                index.indnatts AS attributes,
                index.indkey,
                index.indpred IS NULL AS no_predicate,
                index.indexprs IS NULL AS no_expression
             FROM pg_catalog.pg_index AS index
             JOIN pg_catalog.pg_class AS class ON class.oid = index.indrelid
             JOIN pg_catalog.pg_class AS index_class ON index_class.oid = index.indexrelid
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
             WHERE namespace.nspname = '_orna_data'
               AND class.relname = $1
             ORDER BY index_class.relname",
            &[&table.name],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut expected = expected_indexes(table);
    for row in &rows {
        let table_record = relation_record(&table.name);
        let name: String = table_record.column(row, "name", "index name must decode")?;
        let record = DurableRecord::new(DATA_RELATION, format!("{}.{}", table.name, name));
        let Some(expected_index) = expected.remove(&name) else {
            return Err(record.invariant("relation has an unexpected index"));
        };
        let observed = ObservedIndex::from_row(row, &record)?;
        if !observed.has_expected_shape(&expected_index) {
            return Err(record.invariant(
                "index must have the exact one-column immediate private primary or unique shape",
            ));
        }
    }
    if !expected.is_empty() {
        return Err(relation_record(&table.name)
            .invariant("relation is missing a required private primary or unique index"));
    }
    Ok(())
}

async fn verify_reference_triggers(
    transaction: &Transaction<'_>,
    tables: &BTreeMap<String, ExpectedTable>,
) -> Result<(), PostgresKernelError> {
    let expected = expected_foreign_keys(tables)?;
    let table_names = tables.keys().cloned().collect::<Vec<_>>();
    let rows = transaction
        .query(
            "SELECT
                trigger.tgname AS name,
                trigger.tgenabled::text AS enabled,
                triggered.relname AS trigger_table,
                catalogue_constraint.contype::text AS constraint_kind,
                catalogue_constraint.conname AS constraint_name,
                source.relname AS source_table,
                source_namespace.nspname AS source_namespace,
                target.relname AS target_table,
                target_namespace.nspname AS target_namespace,
                COALESCE(trigger.tgrelid = catalogue_constraint.conrelid, false) AS on_source,
                COALESCE(trigger.tgrelid = catalogue_constraint.confrelid, false) AS on_target,
                COALESCE(trigger.tgconstrrelid = catalogue_constraint.confrelid, false) AS constraint_target,
                COALESCE(trigger.tgconstrrelid = catalogue_constraint.conrelid, false) AS constraint_source
             FROM pg_catalog.pg_trigger AS trigger
             JOIN pg_catalog.pg_class AS triggered ON triggered.oid = trigger.tgrelid
             JOIN pg_catalog.pg_namespace AS triggered_namespace
               ON triggered_namespace.oid = triggered.relnamespace
             LEFT JOIN pg_catalog.pg_constraint AS catalogue_constraint
               ON catalogue_constraint.oid = trigger.tgconstraint
             LEFT JOIN pg_catalog.pg_class AS source
               ON source.oid = catalogue_constraint.conrelid
             LEFT JOIN pg_catalog.pg_namespace AS source_namespace
               ON source_namespace.oid = source.relnamespace
             LEFT JOIN pg_catalog.pg_class AS target
               ON target.oid = catalogue_constraint.confrelid
             LEFT JOIN pg_catalog.pg_namespace AS target_namespace
               ON target_namespace.oid = target.relnamespace
             WHERE trigger.tgisinternal
               AND triggered_namespace.nspname = '_orna_data'
               AND triggered.relname = ANY($1::text[])
             ORDER BY triggered.relname, trigger.tgname",
            &[&table_names],
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    let mut counts = BTreeMap::<(String, String), ReferenceTriggerCounts>::new();
    for row in &rows {
        let initial = DurableRecord::new(DATA_RELATION, "internal trigger row");
        let name: String = initial.column(row, "name", "trigger name must decode")?;
        let record = DurableRecord::new(DATA_RELATION, name);
        let enabled: String = record.column(row, "enabled", "trigger enabled state must decode")?;
        let trigger_table: String =
            record.column(row, "trigger_table", "trigger relation must decode")?;
        let constraint_kind: Option<String> = record.column(
            row,
            "constraint_kind",
            "trigger constraint kind must decode",
        )?;
        let constraint_name: Option<String> = record.column(
            row,
            "constraint_name",
            "trigger constraint name must decode",
        )?;
        let source: Option<String> =
            record.column(row, "source_table", "trigger source relation must decode")?;
        let source_namespace: Option<String> = record.column(
            row,
            "source_namespace",
            "trigger source namespace must decode",
        )?;
        let target: Option<String> =
            record.column(row, "target_table", "trigger target relation must decode")?;
        let target_namespace: Option<String> = record.column(
            row,
            "target_namespace",
            "trigger target namespace must decode",
        )?;
        let on_source: bool = record.column(row, "on_source", "trigger source link must decode")?;
        let on_target: bool = record.column(row, "on_target", "trigger target link must decode")?;
        let constraint_target: bool = record.column(
            row,
            "constraint_target",
            "trigger constraint target link must decode",
        )?;
        let constraint_source: bool = record.column(
            row,
            "constraint_source",
            "trigger constraint source link must decode",
        )?;
        let (Some(constraint_name), Some(source), Some(target)) = (constraint_name, source, target)
        else {
            return Err(record.invariant(
                "internal _orna_data triggers must link to an expected foreign key constraint",
            ));
        };
        let key = (source.clone(), constraint_name.clone());
        let Some(expected_trigger) = expected.get(&key) else {
            return Err(record.invariant(
                "internal _orna_data trigger must belong to an expected foreign key constraint",
            ));
        };
        if enabled != "O"
            || constraint_kind.as_deref() != Some("f")
            || source_namespace.as_deref() != Some(DATA_SCHEMA)
            || target_namespace.as_deref() != Some(DATA_SCHEMA)
            || source != expected_trigger.source
            || target != expected_trigger.target
        {
            return Err(record.invariant(
                "foreign key RI trigger must be enabled and link its exact private constraint source and target",
            ));
        }

        let Some(location) = classify_reference_trigger(
            expected_trigger,
            &trigger_table,
            on_source,
            on_target,
            constraint_target,
            constraint_source,
        ) else {
            return Err(record.invariant(
                "foreign key RI trigger must have an exact child or parent relation linkage",
            ));
        };
        let entry = counts.entry(key).or_default();
        entry.total += 1;
        match location {
            ReferenceTriggerLocation::Source => entry.source += 1,
            ReferenceTriggerLocation::Target => entry.target += 1,
            ReferenceTriggerLocation::SelfReference => {}
        }
    }

    for (key, expected_trigger) in expected {
        let count = counts.get(&key).copied().unwrap_or_default();
        let correct_count = if expected_trigger.source == expected_trigger.target {
            count.total == 4
        } else {
            count.total == 4 && count.source == 2 && count.target == 2
        };
        if !correct_count {
            return Err(DurableRecord::new(
                DATA_RELATION,
                format!(
                    "{}.{}",
                    expected_trigger.source, expected_trigger.constraint
                ),
            )
            .invariant("each foreign key must have exactly four enabled PostgreSQL RI triggers"));
        }
    }
    Ok(())
}

fn expected_foreign_keys(
    tables: &BTreeMap<String, ExpectedTable>,
) -> Result<BTreeMap<(String, String), ExpectedForeignKey>, PostgresKernelError> {
    let mut expected = BTreeMap::new();
    for table in tables.values() {
        for (name, constraint) in &table.constraints {
            if constraint.kind != "f" {
                continue;
            }
            let target = constraint.foreign_table.clone().ok_or_else(|| {
                relation_record(&table.name)
                    .invariant("expected foreign key must have a private target relation")
            })?;
            let key = (table.name.clone(), name.clone());
            let value = ExpectedForeignKey {
                source: table.name.clone(),
                target,
                constraint: name.clone(),
            };
            if expected.insert(key, value).is_some() {
                return Err(relation_record(&table.name).invariant(
                    "expected foreign key source and generated constraint name must be unique",
                ));
            }
        }
    }
    Ok(expected)
}

#[derive(Clone, Copy, Default)]
struct ReferenceTriggerCounts {
    total: usize,
    source: usize,
    target: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReferenceTriggerLocation {
    Source,
    Target,
    SelfReference,
}

fn classify_reference_trigger(
    expected: &ExpectedForeignKey,
    trigger_table: &str,
    on_source: bool,
    on_target: bool,
    constraint_target: bool,
    constraint_source: bool,
) -> Option<ReferenceTriggerLocation> {
    if expected.source == expected.target {
        (trigger_table == expected.source
            && on_source
            && on_target
            && constraint_target
            && constraint_source)
            .then_some(ReferenceTriggerLocation::SelfReference)
    } else if trigger_table == expected.source
        && on_source
        && !on_target
        && constraint_target
        && !constraint_source
    {
        Some(ReferenceTriggerLocation::Source)
    } else if trigger_table == expected.target
        && !on_source
        && on_target
        && !constraint_target
        && constraint_source
    {
        Some(ReferenceTriggerLocation::Target)
    } else {
        None
    }
}

async fn verify_external_dependants(
    transaction: &Transaction<'_>,
    tables: impl Iterator<Item = &String>,
) -> Result<(), PostgresKernelError> {
    let names = tables.cloned().collect::<Vec<_>>();
    let external_constraint = transaction
        .query_opt(
            "SELECT catalogue_constraint.conname AS name
             FROM pg_catalog.pg_constraint AS catalogue_constraint
             JOIN pg_catalog.pg_class AS source
               ON source.oid = catalogue_constraint.conrelid
             JOIN pg_catalog.pg_namespace AS source_namespace ON source_namespace.oid = source.relnamespace
             JOIN pg_catalog.pg_class AS target
               ON target.oid = catalogue_constraint.confrelid
             JOIN pg_catalog.pg_namespace AS target_namespace ON target_namespace.oid = target.relnamespace
             WHERE target_namespace.nspname = '_orna_data'
               AND target.relname = ANY($1::text[])
               AND (source_namespace.nspname <> '_orna_data' OR source.relname <> ALL($1::text[]))
             LIMIT 1",
            &[&names],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if external_constraint.is_some() {
        return Err(DurableRecord::new(DATA_RELATION, DATA_SCHEMA).invariant(
            "no constraint outside the private catalogue may reference an _orna_data relation",
        ));
    }

    for (relation, query, rule) in [
        (
            "pg_catalog.pg_inherits",
            "SELECT 1
             FROM pg_catalog.pg_inherits AS inheritance
             JOIN pg_catalog.pg_class AS child ON child.oid = inheritance.inhrelid
             JOIN pg_catalog.pg_namespace AS child_namespace ON child_namespace.oid = child.relnamespace
             JOIN pg_catalog.pg_class AS parent ON parent.oid = inheritance.inhparent
             JOIN pg_catalog.pg_namespace AS parent_namespace ON parent_namespace.oid = parent.relnamespace
             WHERE (child_namespace.nspname = '_orna_data' AND child.relname = ANY($1::text[]))
                OR (parent_namespace.nspname = '_orna_data' AND parent.relname = ANY($1::text[]))
             LIMIT 1",
            "private object relations must not participate in PostgreSQL inheritance",
        ),
        (
            "pg_catalog.pg_trigger",
            "SELECT 1
             FROM pg_catalog.pg_trigger AS trigger
             JOIN pg_catalog.pg_class AS class ON class.oid = trigger.tgrelid
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
             WHERE namespace.nspname = '_orna_data'
               AND class.relname = ANY($1::text[])
               AND NOT trigger.tgisinternal
             LIMIT 1",
            "private object relations must not have non-internal triggers",
        ),
        (
            "pg_catalog.pg_policy",
            "SELECT 1
             FROM pg_catalog.pg_policy AS policy
             JOIN pg_catalog.pg_class AS class ON class.oid = policy.polrelid
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
             WHERE namespace.nspname = '_orna_data'
               AND class.relname = ANY($1::text[])
             LIMIT 1",
            "private object relations must not have row-level policies",
        ),
        (
            "pg_catalog.pg_rewrite",
            "SELECT 1
             FROM pg_catalog.pg_rewrite AS rewrite
             JOIN pg_catalog.pg_class AS class ON class.oid = rewrite.ev_class
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
             WHERE namespace.nspname = '_orna_data'
               AND class.relname = ANY($1::text[])
             LIMIT 1",
            "private object relations must not have rewrite rules",
        ),
    ] {
        if transaction
            .query_opt(query, &[&names])
            .await
            .map_err(PostgresKernelError::Database)?
            .is_some()
        {
            return Err(DurableRecord::new(relation, DATA_SCHEMA).invariant(rule));
        }
    }
    Ok(())
}

fn type_record(type_id: TypeId) -> DurableRecord {
    DurableRecord::new("_orna_kernel.catalogue_object_types", type_id.canonical())
}

fn field_record(owner: TypeId, field: FieldId) -> DurableRecord {
    DurableRecord::new(
        "_orna_kernel.catalogue_fields",
        format!("{}.{}", owner.canonical(), field.canonical()),
    )
}

fn relation_record(name: &str) -> DurableRecord {
    DurableRecord::new(DATA_RELATION, name)
}

#[cfg(test)]
mod tests {
    use orna_core::physical::{PhysicalPlanError, active_physical_catalogue};
    use orna_core::{
        CatalogueRevisionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
        TypeId,
        canonical_hash::{
            catalogue_digest_with_context, source_bundle_digest, source_revision_record_digest,
            source_unit_content_digest,
        },
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, ObjectTypeDefinition,
            OnDeleteAction, QualifiedSemanticName, SchemaDefinition,
        },
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
            RevisionPair, SourceOrigin, StoredSourceRevision, StoredSourceUnit,
        },
        types::{ResolvedType, StandardScalar},
    };

    use super::{
        ExpectedCatalogue, ExpectedConstraint, ExpectedForeignKey, ExpectedIndex, ExpectedTable,
        ObservedIndex, ReferenceTriggerLocation, classify_reference_trigger, expected_indexes,
        field_name, has_non_foreign_constraint_shape, map_physical_projection_error, relation_name,
        requires_no_inherit, unique_constraint_name, verify_named_constraint,
    };
    use crate::decode::DurableRecord;

    #[test]
    fn non_foreign_constraints_require_the_exact_pg18_blank_characters() {
        assert!(has_non_foreign_constraint_shape(
            None, None, None, " ", " ", " ", None,
        ));
        assert!(!has_non_foreign_constraint_shape(
            None, None, None, "a", "a", "s", None,
        ));
        assert!(!has_non_foreign_constraint_shape(
            None,
            None,
            None,
            " ",
            " ",
            " ",
            Some("expression"),
        ));
    }

    #[test]
    fn pg18_constraint_inheritance_flags_are_exact_per_constraint_kind() {
        assert!(requires_no_inherit("p"));
        assert!(requires_no_inherit("u"));
        assert!(requires_no_inherit("f"));
        assert!(!requires_no_inherit("c"));
        assert!(!requires_no_inherit("n"));
    }

    #[test]
    fn unique_constraint_requires_the_exact_pg18_shape() {
        let record = DurableRecord::new("_orna_data", "unique constraint");
        let expected = ExpectedConstraint::unique(2);
        assert!(
            verify_named_constraint(
                &record,
                "u",
                Some(&[2]),
                None,
                None,
                None,
                " ",
                " ",
                " ",
                true,
                true,
                false,
                false,
                false,
                true,
                None,
                &expected,
            )
            .is_ok()
        );
        assert!(
            verify_named_constraint(
                &record,
                "u",
                Some(&[2]),
                None,
                None,
                None,
                " ",
                " ",
                " ",
                true,
                true,
                false,
                true,
                false,
                true,
                None,
                &expected,
            )
            .is_err()
        );
    }

    #[test]
    fn reference_trigger_shape_requires_exact_child_parent_or_self_linkage() {
        let foreign_key = ExpectedForeignKey {
            source: "t_source".to_owned(),
            target: "t_target".to_owned(),
            constraint: "fk_field".to_owned(),
        };
        assert_eq!(
            classify_reference_trigger(&foreign_key, "t_source", true, false, true, false),
            Some(ReferenceTriggerLocation::Source)
        );
        assert_eq!(
            classify_reference_trigger(&foreign_key, "t_target", false, true, false, true),
            Some(ReferenceTriggerLocation::Target)
        );
        assert_eq!(
            classify_reference_trigger(&foreign_key, "t_source", true, false, false, true),
            None
        );

        let self_reference = ExpectedForeignKey {
            source: "t_self".to_owned(),
            target: "t_self".to_owned(),
            constraint: "fk_self".to_owned(),
        };
        assert_eq!(
            classify_reference_trigger(&self_reference, "t_self", true, true, true, true),
            Some(ReferenceTriggerLocation::SelfReference)
        );
    }

    #[test]
    fn expected_builder_uses_the_lowerers_private_names_and_reference_shape() {
        let target = TypeId::from_bytes([1; 16]);
        let object = ObjectTypeDefinition::new(
            TypeId::from_bytes([2; 16]),
            name(&["test", "source"]),
            vec![
                FieldDefinition::new(
                    FieldId::from_bytes([3; 16]),
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    false,
                    false,
                    None,
                    None,
                ),
                FieldDefinition::new(
                    FieldId::from_bytes([4; 16]),
                    "target",
                    1,
                    ResolvedType::reference(target),
                    true,
                    false,
                    None,
                    None,
                ),
                FieldDefinition::new(
                    FieldId::from_bytes([5; 16]),
                    "unique_target",
                    2,
                    ResolvedType::reference(target),
                    false,
                    true,
                    None,
                    None,
                ),
            ],
        );

        let target_object =
            ObjectTypeDefinition::new(target, name(&["test", "target"]), Vec::new());
        let active = active_revision_with_objects(
            CatalogueHashContext::version_one(),
            vec![target_object, object.clone()],
        );
        let physical = active_physical_catalogue(&active).expect("physical catalogue");
        let physical_object = physical
            .objects()
            .iter()
            .find(|candidate| candidate.type_id() == object.id())
            .expect("source physical object");
        let expected = ExpectedTable::from_object(physical_object).expect("supported object");

        assert_eq!(expected.name, relation_name(object.id()));
        assert_eq!(
            expected.columns[1].name,
            field_name(FieldId::from_bytes([3; 16]))
        );
        assert_eq!(expected.columns[1].type_name, "int4");
        assert!(
            expected
                .constraints
                .contains_key(&format!("fk_{}", "04".repeat(16)))
        );
        let unique_name = unique_constraint_name(FieldId::from_bytes([5; 16]));
        let unique = expected
            .constraints
            .get(&unique_name)
            .expect("required unique constraint");
        assert_eq!(unique.kind, "u");
        assert_eq!(unique.key, [4]);

        let indexes = expected_indexes(&expected);
        assert_eq!(indexes.len(), 2);
        assert_eq!(
            indexes.get(&format!("pk_{}", "02".repeat(16))),
            Some(&ExpectedIndex {
                key: vec![1],
                primary: true,
            })
        );
        assert_eq!(
            indexes.get(&unique_name),
            Some(&ExpectedIndex {
                key: vec![4],
                primary: false,
            })
        );
    }

    #[test]
    fn expected_builder_consumes_the_active_revision_catalogue_for_scalar_storage() {
        let (active, object) = scalar_active_revision();

        let expected = ExpectedCatalogue::from_active(&active).expect("supported active revision");
        let table = expected
            .tables
            .get(&relation_name(object))
            .expect("scalar object table");

        assert_eq!(expected.tables.len(), 1);
        assert_eq!(table.columns.len(), 2);
        assert_eq!(
            table.columns[1].name,
            field_name(FieldId::from_bytes([0x34; 16]))
        );
        assert_eq!(table.columns[1].type_name, "int4");
        assert!(!table.columns[1].nullable);
        assert!(table.columns[1].reference.is_none());
    }

    #[test]
    fn expected_builder_requires_text_for_catalogue_enum_fields() {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard-library snapshot"),
        )
        .expect("verified standard-library snapshot");
        let enum_type = TypeId::from_bytes([0x38; 16]);
        let object = TypeId::from_bytes([0x39; 16]);
        let field = FieldId::from_bytes([0x3a; 16]);
        let active = active_revision_with_objects_and_enums(
            CatalogueHashContext::version_two(standard),
            vec![ObjectTypeDefinition::new(
                object,
                name(&["test", "enum_holder"]),
                vec![FieldDefinition::new(
                    field,
                    "stage",
                    0,
                    ResolvedType::named(enum_type),
                    false,
                    false,
                    None,
                    None,
                )],
            )],
            vec![EnumTypeDefinition::new(
                enum_type,
                name(&["test", "stage"]),
                ["lead", "qualified"],
            )],
        );

        let expected = ExpectedCatalogue::from_active(&active).expect("enum expected catalogue");
        let column = &expected
            .tables
            .get(&relation_name(object))
            .expect("enum object table")
            .columns[1];

        assert_eq!(column.name, field_name(field));
        assert_eq!(column.type_name, "text");
        assert!(!column.nullable);
        assert!(column.reference.is_none());
    }

    #[test]
    fn expected_builder_equates_version_one_scalar_and_version_two_value_integer_facts() {
        let (version_one, object) = scalar_active_revision();
        let (version_two, version_two_object, value_type) = value_active_revision();

        let version_one_expected =
            ExpectedCatalogue::from_active(&version_one).expect("version one expected catalogue");
        let version_two_expected =
            ExpectedCatalogue::from_active(&version_two).expect("version two expected catalogue");

        assert_eq!(
            version_one.catalogue_hash_context().version(),
            CatalogueHashVersion::Version1
        );
        assert_eq!(
            version_two.catalogue_hash_context().version(),
            CatalogueHashVersion::Version2
        );
        assert_eq!(object, version_two_object);
        assert_eq!(
            version_two
                .catalogue()
                .object_type_by_id(version_two_object)
                .expect("version two object")
                .fields()[0]
                .resolved_type()
                .value_type(),
            Some(value_type)
        );
        assert_eq!(version_one_expected, version_two_expected);
        assert_eq!(
            version_two_expected
                .tables
                .get(&relation_name(object))
                .expect("version two value table")
                .columns[1]
                .type_name,
            "int4"
        );
    }

    #[test]
    fn expected_builder_lowers_a_verified_value_field_to_the_legacy_integer_catalogue_type() {
        let (active, object, value_type) = value_active_revision();
        let (legacy_active, _) = scalar_active_revision();

        let expected = ExpectedCatalogue::from_active(&active).expect("supported value revision");
        let legacy_expected =
            ExpectedCatalogue::from_active(&legacy_active).expect("supported scalar revision");
        let table = expected
            .tables
            .get(&relation_name(object))
            .expect("value object table");

        assert_eq!(table.columns[1].type_name, "int4");
        assert!(!table.columns[1].nullable);
        assert_eq!(expected, legacy_expected);
        assert_eq!(
            active
                .catalogue()
                .object_type_by_id(object)
                .unwrap()
                .fields()[0]
                .resolved_type()
                .value_type(),
            Some(value_type)
        );
    }

    #[test]
    fn expected_builder_accepts_only_required_unique_references() {
        let owner = TypeId::from_bytes([0x20; 16]);
        let target = TypeId::from_bytes([0x21; 16]);
        let required_reference = unique_object(
            owner,
            ResolvedType::reference(target),
            false,
            FieldId::from_bytes([0x22; 16]),
        );
        assert!(
            ExpectedCatalogue::from_active(&active_revision_with_objects(
                CatalogueHashContext::version_one(),
                vec![empty_object(target), required_reference,],
            ))
            .is_ok()
        );

        let nullable_reference = unique_object(
            owner,
            ResolvedType::reference(target),
            true,
            FieldId::from_bytes([0x23; 16]),
        );
        assert!(
            ExpectedCatalogue::from_active(&active_revision_with_objects(
                CatalogueHashContext::version_one(),
                vec![empty_object(target), nullable_reference],
            ))
            .is_err()
        );

        let set_null_reference = ObjectTypeDefinition::new(
            owner,
            name(&["test", "unique_owner"]),
            vec![FieldDefinition::new(
                FieldId::from_bytes([0x25; 16]),
                "unique_field",
                0,
                ResolvedType::reference(target),
                false,
                true,
                None,
                Some(OnDeleteAction::SetNull),
            )],
        );
        assert!(
            ExpectedCatalogue::from_active(&active_revision_with_objects(
                CatalogueHashContext::version_one(),
                vec![empty_object(target), set_null_reference],
            ))
            .is_err()
        );

        let named = unique_object(
            owner,
            ResolvedType::Named(target),
            false,
            FieldId::from_bytes([0x24; 16]),
        );
        assert!(
            ExpectedCatalogue::from_active(&active_revision_with_objects(
                CatalogueHashContext::version_one(),
                vec![empty_object(target), named],
            ))
            .is_err()
        );

        for (index, scalar) in StandardScalar::ALL.into_iter().enumerate() {
            let field_byte = u8::try_from(index + 0x30).expect("closed scalar set fits byte");
            let scalar = unique_object(
                owner,
                ResolvedType::scalar(scalar),
                false,
                FieldId::from_bytes([field_byte; 16]),
            );
            assert!(
                ExpectedCatalogue::from_active(&active_revision_with_objects(
                    CatalogueHashContext::version_one(),
                    vec![scalar],
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn index_shape_is_closed_to_exact_primary_and_unique_indexes() {
        let unique = ExpectedIndex {
            key: vec![2],
            primary: false,
        };
        let exact = ObservedIndex {
            valid: true,
            ready: true,
            unique: true,
            primary: false,
            immediate: true,
            exclusion: false,
            clustered: false,
            replica_identity: false,
            nulls_not_distinct: false,
            key_attributes: 1,
            attributes: 1,
            key: vec![2],
            no_predicate: true,
            no_expression: true,
        };
        assert!(exact.has_expected_shape(&unique));
        assert!(
            ObservedIndex {
                primary: true,
                ..exact.clone()
            }
            .has_expected_shape(&ExpectedIndex {
                key: vec![2],
                primary: true,
            })
        );

        for malformed in [
            ObservedIndex {
                valid: false,
                ..exact.clone()
            },
            ObservedIndex {
                ready: false,
                ..exact.clone()
            },
            ObservedIndex {
                unique: false,
                ..exact.clone()
            },
            ObservedIndex {
                primary: true,
                ..exact.clone()
            },
            ObservedIndex {
                immediate: false,
                ..exact.clone()
            },
            ObservedIndex {
                exclusion: true,
                ..exact.clone()
            },
            ObservedIndex {
                clustered: true,
                ..exact.clone()
            },
            ObservedIndex {
                replica_identity: true,
                ..exact.clone()
            },
            ObservedIndex {
                nulls_not_distinct: true,
                ..exact.clone()
            },
            ObservedIndex {
                key_attributes: 2,
                ..exact.clone()
            },
            ObservedIndex {
                attributes: 2,
                ..exact.clone()
            },
            ObservedIndex {
                key: vec![3],
                ..exact.clone()
            },
            ObservedIndex {
                no_predicate: false,
                ..exact.clone()
            },
            ObservedIndex {
                no_expression: false,
                ..exact.clone()
            },
        ] {
            assert!(!malformed.has_expected_shape(&unique));
        }
    }

    #[test]
    fn expected_builder_rejects_non_storable_semantic_fields() {
        let object = ObjectTypeDefinition::new(
            TypeId::from_bytes([5; 16]),
            name(&["test", "invalid"]),
            vec![FieldDefinition::new(
                FieldId::from_bytes([6; 16]),
                "invalid",
                0,
                ResolvedType::scalar(StandardScalar::Void),
                true,
                false,
                None,
                None,
            )],
        );

        let active =
            active_revision_with_objects(CatalogueHashContext::version_one(), vec![object]);
        let error = ExpectedCatalogue::from_active(&active).expect_err("VOID must fail closed");
        assert!(matches!(
            error,
            crate::PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.catalogue_fields",
                rule: "VOID cannot lower to a physical PostgreSQL column",
                ..
            }
        ));
    }

    #[test]
    fn physical_projection_adapter_preserves_legacy_error_boundaries() {
        let object_type = TypeId::from_bytes([0x51; 16]);
        let field = FieldId::from_bytes([0x52; 16]);
        let expected_record = format!("{}.{}", object_type.canonical(), field.canonical());

        for (error, expected_rule) in [
            (
                PhysicalPlanError::UnsupportedUniqueField { object_type, field },
                "only NOT NULL REF fields can be UNIQUE",
            ),
            (
                PhysicalPlanError::UnsupportedFieldDefault { object_type, field },
                "physical storage does not support field defaults",
            ),
            (
                PhysicalPlanError::UnsupportedVoidField { object_type, field },
                "VOID cannot lower to a physical PostgreSQL column",
            ),
        ] {
            let mapped = map_physical_projection_error(error);
            assert!(matches!(
                &mapped,
                crate::PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.catalogue_fields",
                    record,
                    rule,
                } if record == &expected_record && *rule == expected_rule
            ));
            assert!(std::error::Error::source(&mapped).is_none());
        }
    }

    #[test]
    fn physical_projection_adapter_passes_preempted_and_value_errors_through() {
        let object_type = TypeId::from_bytes([0x51; 16]);
        let field = FieldId::from_bytes([0x52; 16]);
        let named_error =
            map_physical_projection_error(PhysicalPlanError::UnsupportedNamedFieldType {
                object_type,
                field,
            });
        assert!(matches!(
            named_error,
            crate::PostgresKernelError::PhysicalPlan(
                PhysicalPlanError::UnsupportedNamedFieldType { .. }
            )
        ));

        let unknown_reference =
            map_physical_projection_error(PhysicalPlanError::UnknownReferenceTarget {
                object_type,
                field,
                target: TypeId::from_bytes([0x53; 16]),
            });
        assert!(matches!(
            unknown_reference,
            crate::PostgresKernelError::PhysicalPlan(
                PhysicalPlanError::UnknownReferenceTarget { .. }
            )
        ));

        let invalid_delete =
            map_physical_projection_error(PhysicalPlanError::InvalidDeleteAction {
                object_type,
                field,
            });
        assert!(matches!(
            invalid_delete,
            crate::PostgresKernelError::PhysicalPlan(PhysicalPlanError::InvalidDeleteAction { .. })
        ));

        let value_error =
            map_physical_projection_error(PhysicalPlanError::MissingValueTypeDefinition {
                object_type,
                field,
                value_type: TypeId::from_bytes([0x54; 16]),
            });
        assert!(matches!(
            value_error,
            crate::PostgresKernelError::PhysicalPlan(
                PhysicalPlanError::MissingValueTypeDefinition { .. }
            )
        ));
    }

    #[test]
    fn expected_builder_rejects_a_type_and_reference_field_with_matching_raw_bytes() {
        let shared = [7; 16];
        let object = ObjectTypeDefinition::new(
            TypeId::from_bytes(shared),
            name(&["test", "collision"]),
            vec![FieldDefinition::new(
                FieldId::from_bytes(shared),
                "self_ref",
                0,
                ResolvedType::reference(TypeId::from_bytes(shared)),
                true,
                false,
                None,
                None,
            )],
        );

        let active =
            active_revision_with_objects(CatalogueHashContext::version_one(), vec![object]);
        let physical = active_physical_catalogue(&active).expect("physical catalogue");
        assert!(ExpectedTable::from_object(&physical.objects()[0]).is_err());
    }

    fn unique_object(
        owner: TypeId,
        resolved_type: ResolvedType,
        nullable: bool,
        field: FieldId,
    ) -> ObjectTypeDefinition {
        ObjectTypeDefinition::new(
            owner,
            name(&["test", "unique_owner"]),
            vec![FieldDefinition::new(
                field,
                "unique_field",
                0,
                resolved_type,
                nullable,
                true,
                None,
                None,
            )],
        )
    }

    fn scalar_active_revision() -> (ActiveDatabaseRevision, TypeId) {
        scalar_active_revision_with_context(CatalogueHashContext::version_one())
    }

    fn scalar_active_revision_with_context(
        context: CatalogueHashContext,
    ) -> (ActiveDatabaseRevision, TypeId) {
        let object = TypeId::from_bytes([0x36; 16]);
        let field = FieldId::from_bytes([0x34; 16]);
        let scalar = ObjectTypeDefinition::new(
            object,
            name(&["test", "scalar"]),
            vec![FieldDefinition::new(
                field,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                false,
                false,
                None,
                None,
            )],
        );
        (active_revision_with_objects(context, vec![scalar]), object)
    }

    fn value_active_revision() -> (ActiveDatabaseRevision, TypeId, TypeId) {
        let standard = orna_standard::verify_standard_library_snapshot(
            orna_standard::retained_standard_library_snapshot()
                .expect("retained standard-library snapshot"),
        )
        .expect("verified standard-library snapshot");
        let value_type = standard
            .catalogue()
            .value_types()
            .iter()
            .find(|value| value.representation_contract() == "orna.kernel.value.integer@1")
            .expect("verified integer value type")
            .id();
        let object = TypeId::from_bytes([0x36; 16]);
        let field = FieldId::from_bytes([0x34; 16]);
        let value = ObjectTypeDefinition::new(
            object,
            name(&["test", "value"]),
            vec![FieldDefinition::new(
                field,
                "value",
                0,
                ResolvedType::value(value_type),
                false,
                false,
                None,
                None,
            )],
        );
        (
            active_revision_with_objects(CatalogueHashContext::version_two(standard), vec![value]),
            object,
            value_type,
        )
    }

    fn empty_object(object: TypeId) -> ObjectTypeDefinition {
        ObjectTypeDefinition::new(object, name(&["test", "target"]), Vec::new())
    }

    fn active_revision_with_objects(
        context: CatalogueHashContext,
        objects: Vec<ObjectTypeDefinition>,
    ) -> ActiveDatabaseRevision {
        active_revision_with_objects_and_enums(context, objects, Vec::new())
    }

    fn active_revision_with_objects_and_enums(
        context: CatalogueHashContext,
        objects: Vec<ObjectTypeDefinition>,
        enum_types: Vec<EnumTypeDefinition>,
    ) -> ActiveDatabaseRevision {
        let source_unit = SourceUnitId::from_bytes([0x31; 16]);
        let unit = StoredSourceUnit::new(
            source_unit,
            0,
            "physical-fixture.orna",
            "",
            source_unit_content_digest("").expect("source unit digest"),
        )
        .expect("source unit");
        let bundle = SourceBundleId::from_bytes([0x32; 16]);
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).expect("bundle digest");
        let source = StoredSourceRevision::new(
            bundle,
            SourceRevisionId::from_bytes([0x33; 16]),
            None,
            vec![unit],
            bundle_hash,
            source_revision_record_digest(bundle, None, bundle_hash).expect("source digest"),
        )
        .expect("source revision");
        let schema = SchemaDefinition::new(SchemaId::from_bytes([0x35; 16]), name(&["test"]));
        let catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::from_bytes([0x37; 16]),
            vec![schema.clone()],
            objects.clone(),
            Vec::new(),
            enum_types,
            Vec::new(),
        )
        .expect("catalogue");
        let origin = SourceOrigin::new(source_unit, 0, 0).expect("empty source origin");
        let mut origins = vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema.id()),
            origin,
        )];
        for object in &objects {
            origins.push(DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(object.id()),
                origin,
            ));
            for field in object.fields() {
                origins.push(DefinitionOrigin::new(
                    DefinitionIdentity::Field {
                        owner: object.id(),
                        field: field.id(),
                    },
                    origin,
                ));
            }
        }
        origins.extend(catalogue.enum_types().iter().map(|enum_type| {
            DefinitionOrigin::new(DefinitionIdentity::ValueType(enum_type.id()), origin)
        }));
        let catalogue_hash =
            catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[])
                .expect("catalogue digest");
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                RevisionPair::new(source.id(), catalogue.revision()),
                source,
                catalogue,
                catalogue_hash,
                ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
            ),
            context,
        )
        .expect("active revision")
    }

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).expect("valid semantic name")
    }
}
