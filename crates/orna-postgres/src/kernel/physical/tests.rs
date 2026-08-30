use orna_core::{
    CatalogueRevisionId, FieldId, SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId,
    canonical_hash::{
        catalogue_digest_with_context, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, ObjectTypeDefinition,
        QualifiedSemanticName, RecordValueFieldDefinition, RecordValueTypeDefinition,
        SchemaDefinition,
    },
    physical::plan_physical_changes,
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, DeployableRevision,
        DeployableRevisionContent, DeployableRevisionInput, RevisionPair, SourceOrigin,
        StoredSourceRevision, StoredSourceUnit,
    },
    types::{ResolvedType, TypeDescriptor},
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
fn lowers_unique_text_with_c_collation_without_an_add_field_migration() {
    let active = empty_active();
    let object = TypeId::from_bytes([0x61; 16]);
    let nullable_unique_text = FieldId::from_bytes([0x62; 16]);
    let required_unique_text = FieldId::from_bytes([0x63; 16]);
    let plain_text = FieldId::from_bytes([0x64; 16]);
    let candidate = candidate_with_objects(
        &active,
        vec![ObjectTypeDefinition::new(
            object,
            semantic_name(&["private_words", "unique_text"]),
            vec![
                FieldDefinition::new(
                    nullable_unique_text,
                    "semantic_nullable_unique_text",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    true,
                    true,
                    None,
                    None,
                ),
                FieldDefinition::new(
                    required_unique_text,
                    "semantic_required_unique_text",
                    1,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                    true,
                    None,
                    None,
                ),
                FieldDefinition::new(
                    plain_text,
                    "semantic_plain_text",
                    2,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    false,
                    false,
                    None,
                    None,
                ),
            ],
        )],
    );
    let plan = plan_physical_changes(&active, &candidate).expect("unique Text creation plan");
    let statements = lower_physical_plan(&plan).expect("unique Text PostgreSQL statements");

    assert!(plan.add_field().is_none());
    assert_eq!(statements.references, Vec::<String>::new());
    assert_eq!(
            statements.creates,
            vec![
                concat!(
                    "CREATE TABLE _orna_data.t_61616161616161616161616161616161 (\n",
                    "    _orna_object_id bytea NOT NULL,\n",
                    "    CONSTRAINT pk_61616161616161616161616161616161 PRIMARY KEY (_orna_object_id),\n",
                    "    CONSTRAINT ck_61616161616161616161616161616161_object_id CHECK (octet_length(_orna_object_id) = 16),\n",
                    "    f_62626262626262626262626262626262 text COLLATE pg_catalog.\"C\",\n",
                    "    CONSTRAINT uq_62626262626262626262626262626262 UNIQUE (f_62626262626262626262626262626262),\n",
                    "    f_63636363636363636363636363636363 text COLLATE pg_catalog.\"C\" NOT NULL,\n",
                    "    CONSTRAINT uq_63636363636363636363636363636363 UNIQUE (f_63636363636363636363636363636363),\n",
                    "    f_64646464646464646464646464646464 text NOT NULL\n",
                    ");"
                )
                .to_owned(),
                "REVOKE ALL ON TABLE _orna_data.t_61616161616161616161616161616161 FROM PUBLIC;"
                    .to_owned(),
            ]
        );
}

#[test]
fn mixed_plan_orders_new_tables_before_the_existing_object_column() {
    let existing = TypeId::from_bytes([0x11; 16]);
    let first_field = FieldId::from_bytes([0x20; 16]);
    let added_field = FieldId::from_bytes([0x21; 16]);
    let new_object = TypeId::from_bytes([0x22; 16]);
    let reference_field = FieldId::from_bytes([0x23; 16]);
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot()
            .expect("retained standard-library snapshot"),
    )
    .expect("verified standard-library snapshot");
    let boolean = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|value| value.representation_contract() == "orna.kernel.value.boolean@1")
        .expect("verified Boolean value type")
        .id();
    let schema = SchemaDefinition::new(SchemaId::new(), semantic_name(&["private_words"]));
    let active_catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::new(),
        vec![schema.clone()],
        vec![ObjectTypeDefinition::new(
            existing,
            semantic_name(&["private_words", "existing"]),
            vec![FieldDefinition::new(
                first_field,
                "semantic_stored",
                0,
                ResolvedType::value(boolean),
                false,
                false,
                None,
                None,
            )],
        )],
    )
    .unwrap();
    let source_unit = SourceUnitId::new();
    let unit = StoredSourceUnit::new(
        source_unit,
        0,
        "physical.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle = SourceBundleId::new();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::new(),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(bundle, None, bundle_hash).unwrap(),
    )
    .unwrap();
    let source_origin = SourceOrigin::new(source_unit, 0, 0).unwrap();
    let origins = vec![
        DefinitionOrigin::new(DefinitionIdentity::Schema(schema.id()), source_origin),
        DefinitionOrigin::new(DefinitionIdentity::ObjectType(existing), source_origin),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: existing,
                field: first_field,
            },
            source_origin,
        ),
    ];
    let context = CatalogueHashContext::version_two(standard);
    let catalogue_hash =
        catalogue_digest_with_context(&context, &active_catalogue, &[], &[], &origins, &[])
            .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), active_catalogue.revision()),
            source,
            active_catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
        ),
        context,
    )
    .unwrap();

    let candidate = candidate_with_objects(
        &active,
        vec![
            ObjectTypeDefinition::new(
                existing,
                semantic_name(&["private_words", "existing"]),
                vec![
                    FieldDefinition::new(
                        first_field,
                        "semantic_stored",
                        0,
                        ResolvedType::value(boolean),
                        false,
                        false,
                        None,
                        None,
                    ),
                    FieldDefinition::new(
                        added_field,
                        "semantic_added",
                        1,
                        ResolvedType::value(boolean),
                        true,
                        false,
                        None,
                        None,
                    ),
                ],
            ),
            ObjectTypeDefinition::new(
                new_object,
                semantic_name(&["private_words", "new"]),
                vec![FieldDefinition::new(
                    reference_field,
                    "semantic_owner",
                    0,
                    ResolvedType::reference(existing),
                    true,
                    false,
                    None,
                    None,
                )],
            ),
        ],
    );
    let plan = plan_physical_changes(&active, &candidate).unwrap();
    let statements = lower_physical_plan(&plan).unwrap();

    // The new object's CREATE TABLE and REVOKE precede the existing
    // object's ALTER TABLE ADD COLUMN.
    assert_eq!(statements.creates.len(), 3);
    assert_eq!(statements.references.len(), 1);
    assert_eq!(
        statements.creates[0],
        concat!(
            "CREATE TABLE _orna_data.t_22222222222222222222222222222222 (\n",
            "    _orna_object_id bytea NOT NULL,\n",
            "    CONSTRAINT pk_22222222222222222222222222222222 PRIMARY KEY (_orna_object_id),\n",
            "    CONSTRAINT ck_22222222222222222222222222222222_object_id CHECK (octet_length(_orna_object_id) = 16),\n",
            "    f_23232323232323232323232323232323 bytea,\n",
            "    CONSTRAINT ck_23232323232323232323232323232323_object_id CHECK (octet_length(f_23232323232323232323232323232323) = 16)\n",
            ");"
        )
    );
    assert_eq!(
        statements.creates[1],
        "REVOKE ALL ON TABLE _orna_data.t_22222222222222222222222222222222 FROM PUBLIC;"
    );
    assert_eq!(
        statements.creates[2],
        "ALTER TABLE _orna_data.t_11111111111111111111111111111111\n    ADD COLUMN f_21212121212121212121212121212121 boolean;"
    );
    assert_eq!(
        statements.references[0],
        concat!(
            "ALTER TABLE _orna_data.t_22222222222222222222222222222222\n",
            "    ADD CONSTRAINT fk_23232323232323232323232323232323\n",
            "    FOREIGN KEY (f_23232323232323232323232323232323)\n",
            "    REFERENCES _orna_data.t_11111111111111111111111111111111 (_orna_object_id)\n",
            "    ON DELETE NO ACTION;"
        )
    );

    // install_physical_plan chains every create before every reference,
    // so the new-object foreign key follows the three create statements.
    let chained = statements
        .creates
        .iter()
        .chain(&statements.references)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(chained.len(), 4);
    assert_eq!(
        chained[3], statements.references[0],
        "install_physical_plan must chain the new-object FK after the three create statements"
    );
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

#[test]
fn standard_value_and_legacy_scalar_lower_to_identical_postgres_bytes() {
    let scalar_active = empty_active();
    let scalar_field = FieldId::from_bytes([0x41; 16]);
    let scalar_object = TypeId::from_bytes([0x42; 16]);
    let scalar_candidate = candidate_with_objects(
        &scalar_active,
        vec![ObjectTypeDefinition::new(
            scalar_object,
            semantic_name(&["private_words", "scalar"]),
            vec![FieldDefinition::new(
                scalar_field,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                false,
                false,
                None,
                None,
            )],
        )],
    );
    let scalar_plan = plan_physical_changes(&scalar_active, &scalar_candidate).unwrap();

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
    let value_active = empty_active_with_context(CatalogueHashContext::version_two(standard));
    let value_candidate = candidate_with_objects(
        &value_active,
        vec![ObjectTypeDefinition::new(
            scalar_object,
            semantic_name(&["private_words", "scalar"]),
            vec![FieldDefinition::new(
                scalar_field,
                "value",
                0,
                ResolvedType::value(value_type),
                false,
                false,
                None,
                None,
            )],
        )],
    );
    let value_plan = plan_physical_changes(&value_active, &value_candidate).unwrap();

    assert_eq!(scalar_plan, value_plan);
    let scalar_statements = lower_physical_plan(&scalar_plan).unwrap();
    let value_statements = lower_physical_plan(&value_plan).unwrap();
    assert_eq!(scalar_statements.creates, value_statements.creates);
    assert_eq!(scalar_statements.references, value_statements.references);
}

#[test]
fn lowers_catalogue_enum_fields_to_text_without_reference_constraints() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot()
            .expect("retained standard-library snapshot"),
    )
    .expect("verified standard-library snapshot");
    let active = empty_active_with_context(CatalogueHashContext::version_two(standard));
    let enum_type = TypeId::from_bytes([0x51; 16]);
    let object = TypeId::from_bytes([0x52; 16]);
    let field = FieldId::from_bytes([0x53; 16]);
    let candidate = candidate_with_objects_and_enums(
        &active,
        vec![ObjectTypeDefinition::new(
            object,
            semantic_name(&["private_words", "enum_holder"]),
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
            semantic_name(&["private_words", "stage"]),
            ["lead", "qualified"],
        )],
    );
    let statements = lower_physical_plan(
        &plan_physical_changes(&active, &candidate).expect("enum physical plan"),
    )
    .expect("enum PostgreSQL statements");

    assert_eq!(statements.references, Vec::<String>::new());
    assert_eq!(
        statements.creates[0],
        concat!(
            "CREATE TABLE _orna_data.t_52525252525252525252525252525252 (\n",
            "    _orna_object_id bytea NOT NULL,\n",
            "    CONSTRAINT pk_52525252525252525252525252525252 PRIMARY KEY (_orna_object_id),\n",
            "    CONSTRAINT ck_52525252525252525252525252525252_object_id CHECK (octet_length(_orna_object_id) = 16),\n",
            "    f_53535353535353535353535353535353 text NOT NULL\n",
            ");"
        )
    );
}

#[test]
fn lowers_record_fields_to_canonical_bytea_without_reference_constraints() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot()
            .expect("retained standard-library snapshot"),
    )
    .expect("verified standard-library snapshot");
    let boolean = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|value| value.representation_contract() == "orna.kernel.value.boolean@1")
        .expect("verified Boolean value type")
        .id();
    let active = empty_active_with_context(CatalogueHashContext::version_two(standard));
    let record_type = TypeId::from_bytes([0x54; 16]);
    let object = TypeId::from_bytes([0x55; 16]);
    let field = FieldId::from_bytes([0x56; 16]);
    let candidate = candidate_with_objects_and_records(
        &active,
        vec![ObjectTypeDefinition::new(
            object,
            semantic_name(&["private_words", "record_holder"]),
            vec![FieldDefinition::new(
                field,
                "status",
                0,
                ResolvedType::named(record_type),
                false,
                false,
                None,
                None,
            )],
        )],
        vec![RecordValueTypeDefinition::new(
            record_type,
            semantic_name(&["private_words", "status"]),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes([0x57; 16]),
                    "active",
                    0,
                    TypeDescriptor::named(boolean),
                )
                .unwrap(),
            ],
        )],
    );
    let statements = lower_physical_plan(
        &plan_physical_changes(&active, &candidate).expect("record physical plan"),
    )
    .expect("record PostgreSQL statements");

    assert_eq!(statements.references, Vec::<String>::new());
    assert!(statements.creates[0].contains("f_56565656565656565656565656565656 bytea NOT NULL"));
    assert!(!statements.creates[0].contains("octet_length(f_56565656565656565656565656565656)"));
}

fn empty_active() -> ActiveDatabaseRevision {
    empty_active_with_context(CatalogueHashContext::version_one())
}

fn empty_active_with_context(context: CatalogueHashContext) -> ActiveDatabaseRevision {
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
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        context,
    )
    .unwrap()
}

fn candidate_with_objects(
    active: &ActiveDatabaseRevision,
    object_types: Vec<ObjectTypeDefinition>,
) -> DeployableRevision {
    candidate_with_objects_and_enums(active, object_types, Vec::new())
}

fn candidate_with_objects_and_enums(
    active: &ActiveDatabaseRevision,
    object_types: Vec<ObjectTypeDefinition>,
    enum_types: Vec<EnumTypeDefinition>,
) -> DeployableRevision {
    candidate_with_types(active, object_types, enum_types, Vec::new())
}

fn candidate_with_objects_and_records(
    active: &ActiveDatabaseRevision,
    object_types: Vec<ObjectTypeDefinition>,
    record_value_types: Vec<RecordValueTypeDefinition>,
) -> DeployableRevision {
    candidate_with_types(active, object_types, Vec::new(), record_value_types)
}

fn candidate_with_types(
    active: &ActiveDatabaseRevision,
    object_types: Vec<ObjectTypeDefinition>,
    enum_types: Vec<EnumTypeDefinition>,
    record_value_types: Vec<RecordValueTypeDefinition>,
) -> DeployableRevision {
    let schema = SchemaDefinition::new(SchemaId::new(), semantic_name(&["private_words"]));
    let catalogue = CatalogueSnapshot::new_with_functions_and_record_value_types(
        CatalogueRevisionId::new(),
        vec![schema.clone()],
        object_types,
        Vec::new(),
        enum_types,
        record_value_types,
        Vec::new(),
        Vec::new(),
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
        source_revision_record_digest(bundle, Some(active.pair().source()), bundle_hash).unwrap(),
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
    origins.extend(catalogue.enum_types().iter().map(|enum_type| {
        DefinitionOrigin::new(DefinitionIdentity::ValueType(enum_type.id()), source_origin)
    }));
    for record_type in catalogue.record_value_types() {
        origins.push(DefinitionOrigin::new(
            DefinitionIdentity::ValueType(record_type.id()),
            source_origin,
        ));
        origins.extend(record_type.fields().iter().map(|field| {
            DefinitionOrigin::new(
                DefinitionIdentity::Field {
                    owner: record_type.id(),
                    field: field.id(),
                },
                source_origin,
            )
        }));
    }
    let context = active.catalogue_hash_context().clone();
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();

    DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            active.pair(),
            source,
            active.pair().catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(origins, Vec::new(), Vec::new(), Vec::new())
                .with_current_function_revisions(Vec::new()),
        ),
        context,
    )
    .unwrap()
}

fn semantic_name(parts: &[&str]) -> QualifiedSemanticName {
    QualifiedSemanticName::new(parts.iter().copied()).unwrap()
}
