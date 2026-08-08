//! Fail-closed verification of PostgreSQL object storage.

use std::collections::{BTreeMap, BTreeSet};

use orna_core::{
    FieldId, TypeId,
    catalogue::{CatalogueSnapshot, FieldDefinition, ObjectTypeDefinition, OnDeleteAction},
    types::{ResolvedType, StandardScalar},
};
use tokio_postgres::Transaction;

use crate::{PostgresKernelError, decode::DurableRecord};

use super::{
    DATA_SCHEMA, OBJECT_ID_COLUMN, establish_trusted_search_path, field_id_hex, field_name,
    on_delete_sql, relation_name, scalar_storage_type, type_id_hex,
};

const DATA_RELATION: &str = "_orna_data";

/// Verifies that PostgreSQL physical storage is exactly the supplied catalogue.
pub(crate) async fn verify_physical_catalogue(
    transaction: &Transaction<'_>,
    catalogue: &CatalogueSnapshot,
) -> Result<(), PostgresKernelError> {
    establish_trusted_search_path(transaction).await?;
    let expected = ExpectedCatalogue::from_catalogue(catalogue)?;
    verify_schema_access(transaction).await?;
    let relations = load_relations(transaction).await?;
    expected.verify_relations(&relations)?;

    for table in expected.tables.values() {
        verify_table(transaction, table).await?;
    }

    verify_reference_triggers(transaction, &expected.tables).await?;
    verify_external_dependants(transaction, expected.tables.keys()).await
}

#[derive(Debug)]
struct ExpectedCatalogue {
    tables: BTreeMap<String, ExpectedTable>,
}

impl ExpectedCatalogue {
    fn from_catalogue(catalogue: &CatalogueSnapshot) -> Result<Self, PostgresKernelError> {
        let mut tables = BTreeMap::new();
        for object in catalogue.object_types() {
            let table = ExpectedTable::from_object(object)?;
            if tables.insert(table.name.clone(), table).is_some() {
                return Err(type_record(object.id())
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

#[derive(Debug)]
struct ExpectedTable {
    name: String,
    columns: Vec<ExpectedColumn>,
    constraints: BTreeMap<String, ExpectedConstraint>,
}

impl ExpectedTable {
    fn from_object(object: &ObjectTypeDefinition) -> Result<Self, PostgresKernelError> {
        let type_hex = type_id_hex(object.id());
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
            &type_record(object.id()),
        )?;
        insert_expected_constraint(
            &mut constraints,
            format!("ck_{type_hex}_object_id"),
            ExpectedConstraint::object_id_check(1, OBJECT_ID_COLUMN),
            &type_record(object.id()),
        )?;

        for field in object.fields() {
            let attribute = i16::try_from(columns.len() + 1).map_err(|_| {
                type_record(object.id()).invariant("object relation has too many fields")
            })?;
            let column = ExpectedColumn::from_field(object.id(), field)?;
            if let Some(reference) = column.reference {
                let field_hex = field_id_hex(field.id());
                let record = field_record(object.id(), field.id());
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
            columns.push(column);
        }

        Ok(Self {
            name: relation_name(object.id()),
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

#[derive(Clone, Copy, Debug)]
struct ExpectedReference {
    target: TypeId,
    on_delete: Option<OnDeleteAction>,
}

#[derive(Debug)]
struct ExpectedColumn {
    name: String,
    type_name: &'static str,
    nullable: bool,
    reference: Option<ExpectedReference>,
}

impl ExpectedColumn {
    fn from_field(owner: TypeId, field: &FieldDefinition) -> Result<Self, PostgresKernelError> {
        let record = field_record(owner, field.id());
        if field.unique() {
            return Err(record.invariant("physical storage does not support unique fields"));
        }
        if field.default_expression().is_some() {
            return Err(record.invariant("physical storage does not support field defaults"));
        }

        let (type_name, reference) = match field.resolved_type() {
            ResolvedType::Scalar(StandardScalar::Void) => {
                return Err(record.invariant("VOID cannot lower to a physical PostgreSQL column"));
            }
            ResolvedType::Scalar(scalar) => {
                if field.on_delete().is_some() {
                    return Err(record
                        .invariant("a scalar field must not declare a reference delete action"));
                }
                (postgres_catalogue_type(scalar, &record)?, None)
            }
            ResolvedType::Named(_) => {
                return Err(record.invariant(
                    "named field types do not have a supported physical PostgreSQL storage mapping",
                ));
            }
            ResolvedType::Reference { target } => {
                if field.on_delete() == Some(OnDeleteAction::SetNull) && !field.nullable() {
                    return Err(record.invariant("SET NULL reference fields must be nullable"));
                }
                (
                    "bytea",
                    Some(ExpectedReference {
                        target,
                        on_delete: field.on_delete(),
                    }),
                )
            }
        };

        Ok(Self {
            name: field_name(field.id()),
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

#[derive(Debug)]
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
    verify_primary_index(transaction, table).await
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
                constraint.conname AS name,
                constraint.contype::text AS kind,
                constraint.conkey,
                target.relname AS target_table,
                target_namespace.nspname AS target_namespace,
                constraint.confkey,
                constraint.confdeltype::text AS delete_action,
                constraint.confupdtype::text AS update_action,
                constraint.confmatchtype::text AS match_type,
                constraint.convalidated AS validated,
                constraint.conenforced AS enforced,
                constraint.conperiod AS period,
                constraint.condeferrable AS deferrable,
                constraint.condeferred AS deferred,
                constraint.connoinherit AS no_inherit,
                pg_catalog.pg_get_expr(constraint.conbin, constraint.conrelid) AS expression
             FROM pg_catalog.pg_constraint AS constraint
             JOIN pg_catalog.pg_class AS class ON class.oid = constraint.conrelid
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
             LEFT JOIN pg_catalog.pg_class AS target ON target.oid = constraint.confrelid
             LEFT JOIN pg_catalog.pg_namespace AS target_namespace ON target_namespace.oid = target.relnamespace
             WHERE namespace.nspname = '_orna_data'
               AND class.relname = $1
             ORDER BY constraint.contype, constraint.conname",
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
            "relation is missing a required Orna-owned primary, check, or foreign key constraint",
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
        "p" => {
            if !has_non_foreign_constraint_shape(
                target_table,
                target_namespace,
                foreign_key,
                delete_action,
                update_action,
                match_type,
                expression,
            ) {
                return Err(record.invariant("primary key must not contain foreign or check state"));
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
    matches!(kind, "p" | "f")
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

async fn verify_primary_index(
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
               AND class.relname = $1",
            &[&table.name],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let record = relation_record(&table.name);
    if rows.len() != 1 {
        return Err(
            record.invariant("each private relation must have exactly one primary key index")
        );
    }
    let row = &rows[0];
    let name: String = record.column(row, "name", "index name must decode")?;
    let valid: bool = record.column(row, "valid", "index valid flag must decode")?;
    let ready: bool = record.column(row, "ready", "index ready flag must decode")?;
    let unique: bool = record.column(row, "unique_index", "index unique flag must decode")?;
    let primary: bool = record.column(row, "primary_index", "index primary flag must decode")?;
    let immediate: bool = record.column(row, "immediate", "index immediate flag must decode")?;
    let exclusion: bool = record.column(row, "exclusion", "index exclusion flag must decode")?;
    let clustered: bool = record.column(row, "clustered", "index clustered flag must decode")?;
    let replica_identity: bool = record.column(
        row,
        "replica_identity",
        "index replica identity flag must decode",
    )?;
    let nulls_not_distinct: bool = record.column(
        row,
        "nulls_not_distinct",
        "index null treatment flag must decode",
    )?;
    let key_attributes: i16 =
        record.column(row, "key_attributes", "index key count must decode")?;
    let attributes: i16 = record.column(row, "attributes", "index attribute count must decode")?;
    let key: Vec<i16> = record.column(row, "indkey", "index key columns must decode")?;
    let no_predicate: bool =
        record.column(row, "no_predicate", "index predicate flag must decode")?;
    let no_expression: bool =
        record.column(row, "no_expression", "index expression flag must decode")?;
    if name != format!("pk_{}", &table.name[2..])
        || !valid
        || !ready
        || !unique
        || !primary
        || !immediate
        || exclusion
        || clustered
        || replica_identity
        || nulls_not_distinct
        || key_attributes != 1
        || attributes != 1
        || key != [1]
        || !no_predicate
        || !no_expression
    {
        return Err(record
            .invariant("primary key index must be the exact one-column immediate private index"));
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
                constraint.contype::text AS constraint_kind,
                constraint.conname AS constraint_name,
                source.relname AS source_table,
                source_namespace.nspname AS source_namespace,
                target.relname AS target_table,
                target_namespace.nspname AS target_namespace,
                COALESCE(trigger.tgrelid = constraint.conrelid, false) AS on_source,
                COALESCE(trigger.tgrelid = constraint.confrelid, false) AS on_target,
                COALESCE(trigger.tgconstrrelid = constraint.confrelid, false) AS constraint_target,
                COALESCE(trigger.tgconstrrelid = constraint.conrelid, false) AS constraint_source
             FROM pg_catalog.pg_trigger AS trigger
             JOIN pg_catalog.pg_class AS triggered ON triggered.oid = trigger.tgrelid
             JOIN pg_catalog.pg_namespace AS triggered_namespace
               ON triggered_namespace.oid = triggered.relnamespace
             LEFT JOIN pg_catalog.pg_constraint AS constraint ON constraint.oid = trigger.tgconstraint
             LEFT JOIN pg_catalog.pg_class AS source ON source.oid = constraint.conrelid
             LEFT JOIN pg_catalog.pg_namespace AS source_namespace
               ON source_namespace.oid = source.relnamespace
             LEFT JOIN pg_catalog.pg_class AS target ON target.oid = constraint.confrelid
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
            "SELECT constraint.conname AS name
             FROM pg_catalog.pg_constraint AS constraint
             JOIN pg_catalog.pg_class AS source ON source.oid = constraint.conrelid
             JOIN pg_catalog.pg_namespace AS source_namespace ON source_namespace.oid = source.relnamespace
             JOIN pg_catalog.pg_class AS target ON target.oid = constraint.confrelid
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
    use orna_core::{
        FieldId, TypeId,
        catalogue::{FieldDefinition, ObjectTypeDefinition, QualifiedSemanticName},
        types::{ResolvedType, StandardScalar},
    };

    use super::{
        ExpectedForeignKey, ExpectedTable, ReferenceTriggerLocation, classify_reference_trigger,
        field_name, has_non_foreign_constraint_shape, relation_name, requires_no_inherit,
    };

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
        assert!(requires_no_inherit("f"));
        assert!(!requires_no_inherit("c"));
        assert!(!requires_no_inherit("n"));
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
            ],
        );

        let expected = ExpectedTable::from_object(&object).expect("supported object");

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

        assert!(ExpectedTable::from_object(&object).is_err());
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

        assert!(ExpectedTable::from_object(&object).is_err());
    }

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).expect("valid semantic name")
    }
}
