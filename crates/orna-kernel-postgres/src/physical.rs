//! PostgreSQL lowering for backend-neutral physical catalogue changes.

mod verify;

use orna_core::{
    TypeId,
    catalogue::OnDeleteAction,
    physical::{CreateField, CreateObject, PhysicalFieldType, PhysicalPlan},
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
    let mut creates = Vec::with_capacity(plan.create_objects().len().saturating_mul(2));
    let mut references = Vec::new();

    for object in plan.create_objects() {
        creates.push(create_table_statement(object)?);
        creates.push(revoke_table_statement(object.type_id()));
        references.extend(reference_statements(object));
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

fn field_definition(field: &CreateField) -> Result<String, PostgresKernelError> {
    let column = field_name(field.field_id());
    let storage_type = match field.field_type() {
        PhysicalFieldType::Scalar(scalar) => scalar_storage_type(scalar)?,
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
mod tests {
    use orna_core::{
        CatalogueRevisionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
        canonical_hash::{
            catalogue_digest, source_bundle_digest, source_revision_record_digest,
            source_unit_content_digest,
        },
        catalogue::{
            CatalogueSnapshot, FieldDefinition, ObjectTypeDefinition, QualifiedSemanticName,
            SchemaDefinition,
        },
        physical::plan_physical_changes,
        revision::{
            ActiveDatabaseRevision, DefinitionIdentity, DefinitionOrigin, DeployableRevision,
            RevisionPair, SourceOrigin, StoredSourceRevision, StoredSourceUnit,
        },
        types::ResolvedType,
    };

    use super::*;

    #[test]
    fn physical_transactions_set_an_exact_trusted_catalogue_search_path() {
        assert_eq!(
            TRUSTED_SEARCH_PATH_STATEMENT,
            "SET LOCAL search_path = pg_catalog"
        );
    }

    #[test]
    fn maps_the_complete_storable_scalar_set() {
        for (scalar, storage_type) in [
            (StandardScalar::Boolean, "boolean"),
            (StandardScalar::Integer, "integer"),
            (StandardScalar::BigInt, "bigint"),
            (StandardScalar::Float, "double precision"),
            (StandardScalar::Decimal, "numeric"),
            (StandardScalar::CharacterLargeObject, "text"),
            (StandardScalar::BinaryLargeObject, "bytea"),
            (StandardScalar::Uuid, "uuid"),
            (StandardScalar::Date, "date"),
            (StandardScalar::Time, "time without time zone"),
            (StandardScalar::Timestamp, "timestamp with time zone"),
            (StandardScalar::Duration, "interval"),
        ] {
            assert_eq!(scalar_storage_type(scalar).unwrap(), storage_type);
        }

        assert!(matches!(
            scalar_storage_type(StandardScalar::Void),
            Err(PostgresKernelError::CatalogueInvariant(
                "VOID cannot lower to a physical PostgreSQL column"
            ))
        ));
    }

    #[test]
    fn maps_all_reference_delete_actions_exactly() {
        assert_eq!(on_delete_sql(None), "NO ACTION");
        assert_eq!(on_delete_sql(Some(OnDeleteAction::Restrict)), "RESTRICT");
        assert_eq!(on_delete_sql(Some(OnDeleteAction::SetNull)), "SET NULL");
        assert_eq!(on_delete_sql(Some(OnDeleteAction::Cascade)), "CASCADE");
    }

    #[test]
    fn lowers_exact_private_table_constraints_and_access_control() {
        let active = empty_active();
        let target = TypeId::from_bytes([0x11; 16]);
        let source = TypeId::from_bytes([0x22; 16]);
        let required_scalar = FieldId::from_bytes([0x23; 16]);
        let optional_reference = FieldId::from_bytes([0x24; 16]);
        let required_unique_reference = FieldId::from_bytes([0x25; 16]);
        let candidate = candidate_with_objects(
            &active,
            vec![
                ObjectTypeDefinition::new(
                    target,
                    semantic_name(&["private_words", "target"]),
                    Vec::new(),
                ),
                ObjectTypeDefinition::new(
                    source,
                    semantic_name(&["private_words", "source"]),
                    vec![
                        FieldDefinition::new(
                            required_scalar,
                            "semantic_scalar",
                            0,
                            ResolvedType::scalar(StandardScalar::Integer),
                            false,
                            false,
                            None,
                            None,
                        ),
                        FieldDefinition::new(
                            optional_reference,
                            "semantic_reference",
                            1,
                            ResolvedType::reference(target),
                            true,
                            false,
                            None,
                            None,
                        ),
                        FieldDefinition::new(
                            required_unique_reference,
                            "semantic_unique_reference",
                            2,
                            ResolvedType::reference(target),
                            false,
                            true,
                            None,
                            Some(OnDeleteAction::Restrict),
                        ),
                    ],
                ),
            ],
        );
        let plan = plan_physical_changes(&active, &candidate).unwrap();

        let statements = lower_physical_plan(&plan).unwrap();

        assert_eq!(statements.creates.len(), 4);
        assert_eq!(statements.references.len(), 2);
        assert_eq!(
            statements.creates[2],
            concat!(
                "CREATE TABLE _orna_data.t_22222222222222222222222222222222 (\n",
                "    _orna_object_id bytea NOT NULL,\n",
                "    CONSTRAINT pk_22222222222222222222222222222222 PRIMARY KEY (_orna_object_id),\n",
                "    CONSTRAINT ck_22222222222222222222222222222222_object_id CHECK (octet_length(_orna_object_id) = 16),\n",
                "    f_23232323232323232323232323232323 integer NOT NULL,\n",
                "    f_24242424242424242424242424242424 bytea,\n",
                "    CONSTRAINT ck_24242424242424242424242424242424_object_id CHECK (octet_length(f_24242424242424242424242424242424) = 16),\n",
                "    f_25252525252525252525252525252525 bytea NOT NULL,\n",
                "    CONSTRAINT ck_25252525252525252525252525252525_object_id CHECK (octet_length(f_25252525252525252525252525252525) = 16),\n",
                "    CONSTRAINT uq_25252525252525252525252525252525 UNIQUE (f_25252525252525252525252525252525)\n",
                ");"
            )
        );
        assert_eq!(
            statements.creates[3],
            "REVOKE ALL ON TABLE _orna_data.t_22222222222222222222222222222222 FROM PUBLIC;"
        );
        assert_eq!(
            statements.references[0],
            concat!(
                "ALTER TABLE _orna_data.t_22222222222222222222222222222222\n",
                "    ADD CONSTRAINT fk_24242424242424242424242424242424\n",
                "    FOREIGN KEY (f_24242424242424242424242424242424)\n",
                "    REFERENCES _orna_data.t_11111111111111111111111111111111 (_orna_object_id)\n",
                "    ON DELETE NO ACTION;"
            )
        );
        assert_eq!(
            statements.references[1],
            concat!(
                "ALTER TABLE _orna_data.t_22222222222222222222222222222222\n",
                "    ADD CONSTRAINT fk_25252525252525252525252525252525\n",
                "    FOREIGN KEY (f_25252525252525252525252525252525)\n",
                "    REFERENCES _orna_data.t_11111111111111111111111111111111 (_orna_object_id)\n",
                "    ON DELETE RESTRICT;"
            )
        );
        let sql = statements
            .creates
            .iter()
            .chain(&statements.references)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!sql.contains("private_words"));
        assert!(!sql.contains("semantic_scalar"));
        assert!(!sql.contains("semantic_reference"));
        assert!(!sql.contains("semantic_unique_reference"));
    }

    #[test]
    fn emits_every_reference_only_after_all_table_statements() {
        let active = empty_active();
        let left = TypeId::from_bytes([0x31; 16]);
        let right = TypeId::from_bytes([0x32; 16]);
        let actions = [
            (None, true),
            (Some(OnDeleteAction::Restrict), false),
            (Some(OnDeleteAction::SetNull), true),
            (Some(OnDeleteAction::Cascade), false),
        ];
        let fields = actions
            .into_iter()
            .enumerate()
            .map(|(ordinal, (on_delete, nullable))| {
                FieldDefinition::new(
                    FieldId::from_bytes([u8::try_from(ordinal + 1).unwrap(); 16]),
                    format!("reference_{ordinal}"),
                    u32::try_from(ordinal).unwrap(),
                    ResolvedType::reference(right),
                    nullable,
                    false,
                    None,
                    on_delete,
                )
            })
            .collect();
        let candidate = candidate_with_objects(
            &active,
            vec![
                ObjectTypeDefinition::new(left, semantic_name(&["private_words", "left"]), fields),
                ObjectTypeDefinition::new(
                    right,
                    semantic_name(&["private_words", "right"]),
                    Vec::new(),
                ),
            ],
        );
        let plan = plan_physical_changes(&active, &candidate).unwrap();

        let statements = lower_physical_plan(&plan).unwrap();

        assert_eq!(statements.creates.len(), 4);
        assert!(
            statements
                .creates
                .iter()
                .all(|statement| !statement.starts_with("ALTER TABLE"))
        );
        assert_eq!(statements.references.len(), 4);
        assert!(
            statements
                .references
                .iter()
                .all(|statement| statement.starts_with("ALTER TABLE"))
        );
        assert_eq!(
            statements
                .references
                .iter()
                .map(|statement| statement.rsplit_once("ON DELETE ").unwrap().1)
                .collect::<Vec<_>>(),
            vec!["NO ACTION;", "RESTRICT;", "SET NULL;", "CASCADE;"]
        );
    }

    fn empty_active() -> ActiveDatabaseRevision {
        let bundle = SourceBundleId::new();
        let source_revision = SourceRevisionId::new();
        let bundle_hash = source_bundle_digest(&[]).unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            source_revision,
            None,
            Vec::new(),
            bundle_hash,
            source_revision_record_digest(bundle, None, bundle_hash).unwrap(),
        )
        .unwrap();
        let catalogue =
            CatalogueSnapshot::new(CatalogueRevisionId::new(), Vec::new(), Vec::new()).unwrap();
        let pair = RevisionPair::new(source.id(), catalogue.revision());
        let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[]).unwrap();
        ActiveDatabaseRevision::new(
            pair,
            source,
            catalogue,
            catalogue_hash,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn candidate_with_objects(
        active: &ActiveDatabaseRevision,
        object_types: Vec<ObjectTypeDefinition>,
    ) -> DeployableRevision {
        let schema = SchemaDefinition::new(SchemaId::new(), semantic_name(&["private_words"]));
        let catalogue = CatalogueSnapshot::new(
            CatalogueRevisionId::new(),
            vec![schema.clone()],
            object_types,
        )
        .unwrap();

        let bundle = SourceBundleId::new();
        let source_revision = SourceRevisionId::new();
        let source_unit = SourceUnitId::new();
        let unit = StoredSourceUnit::new(
            source_unit,
            0,
            "physical.orna",
            "",
            source_unit_content_digest("").unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
        let source = StoredSourceRevision::new(
            bundle,
            source_revision,
            Some(active.pair().source()),
            vec![unit],
            bundle_hash,
            source_revision_record_digest(bundle, Some(active.pair().source()), bundle_hash)
                .unwrap(),
        )
        .unwrap();
        let source_origin = SourceOrigin::new(source_unit, 0, 0).unwrap();
        let mut origins = vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema.id()),
            source_origin,
        )];
        for object_type in catalogue.object_types() {
            origins.push(DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(object_type.id()),
                source_origin,
            ));
            origins.extend(object_type.fields().iter().map(|field| {
                DefinitionOrigin::new(
                    DefinitionIdentity::Field {
                        owner: object_type.id(),
                        field: field.id(),
                    },
                    source_origin,
                )
            }));
        }
        let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &origins, &[]).unwrap();

        DeployableRevision::new(
            active.pair(),
            source,
            active.pair().catalogue(),
            catalogue,
            catalogue_hash,
            origins,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn semantic_name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }
}
