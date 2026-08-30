//! Reserved system identity and name admission tests.

use super::*;
fn invocation_carrier_value_type(id: TypeId, parts: &[&str]) -> ValueTypeDefinition {
    ValueTypeDefinition::primitive(
        id,
        QualifiedSemanticName::new(parts.iter().copied()).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.test.invocation-carrier@1",
    )
}

fn invocation_carrier_catalogue(value_types: Vec<ValueTypeDefinition>) -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<112>()),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes(id::<113>()),
                QualifiedSemanticName::new(["sys"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes(id::<114>()),
                QualifiedSemanticName::new(["sys", "invoke"]).unwrap(),
            ),
        ],
        vec![],
        value_types,
        vec![],
    )
    .unwrap()
}

fn invocation_carrier_origins(catalogue: &CatalogueSnapshot) -> Vec<DefinitionOrigin> {
    let source_unit = SourceUnitId::from_bytes(id::<3>());
    let mut origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<113>())),
            SourceOrigin::new(source_unit, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<114>())),
            SourceOrigin::new(source_unit, 1, 2).unwrap(),
        ),
    ];
    origins.extend(
        catalogue
            .value_types()
            .iter()
            .enumerate()
            .map(|(index, value_type)| {
                DefinitionOrigin::new(
                    DefinitionIdentity::ValueType(value_type.id()),
                    SourceOrigin::new(source_unit, index as u32 + 2, index as u32 + 3).unwrap(),
                )
            }),
    );
    origins
}

fn active_invocation_carrier_admission(
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
    let source = source(None);
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            digest::<115>(),
            ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
        ),
        standard_context(),
    )
}

fn deployable_invocation_carrier_admission(
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
) -> Result<DeployableRevision, RevisionInvariantError> {
    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<117>()),
        CatalogueRevisionId::from_bytes(id::<118>()),
    );
    DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            catalogue,
            digest::<115>(),
            DeployableRevisionContent::new(origins, vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        standard_context(),
    )
}

fn standard_invocation_carrier_admission(
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
) -> Result<StandardLibrarySnapshot, RevisionInvariantError> {
    StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes(id::<115>()),
        StandardLibraryDigestVersion::Version1,
        source(None),
        "orna.language/1",
        catalogue,
        origins,
        digest::<115>(),
    )
}

#[test]
fn public_application_and_standard_admission_reject_each_reserved_carrier_identity() {
    for &carrier in crate::system::INVOCATION_CARRIERS {
        let catalogue = invocation_carrier_catalogue(vec![invocation_carrier_value_type(
            carrier.id(),
            carrier.name_parts(),
        )]);
        let expected = RevisionInvariantError::ReservedInvocationCarrierIdentity {
            carrier: carrier.id(),
        };

        assert_eq!(
            active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
            expected
        );
    }
}

#[test]
fn public_application_and_standard_admission_reject_each_reserved_carrier_name() {
    for (index, &carrier) in crate::system::INVOCATION_CARRIERS.iter().enumerate() {
        let type_id = TypeId::from_bytes([0x80 + index as u8; 16]);
        let catalogue = invocation_carrier_catalogue(vec![invocation_carrier_value_type(
            type_id,
            carrier.name_parts(),
        )]);
        let expected = RevisionInvariantError::ReservedInvocationCarrierName { type_id };

        assert_eq!(
            active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
            expected
        );
    }
}

#[test]
fn carrier_identities_globally_precede_carrier_names_at_public_application_admission() {
    let value_name_only = TypeId::from_bytes([0xa0; 16]);
    let catalogue = invocation_carrier_catalogue(vec![
        invocation_carrier_value_type(value_name_only, &["sys", "invoke", "Value"]),
        invocation_carrier_value_type(
            crate::system::SYS_INVOKE_EVENT_TYPE_ID,
            &["sys", "invoke", "Event2"],
        ),
        invocation_carrier_value_type(
            crate::system::SYS_INVOKE_REQUEST_TYPE_ID,
            &["sys", "invoke", "Request2"],
        ),
    ]);
    let expected = RevisionInvariantError::ReservedInvocationCarrierIdentity {
        carrier: crate::system::SYS_INVOKE_REQUEST_TYPE_ID,
    };

    assert_eq!(
        active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
        expected
    );
    assert_eq!(
        deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
        expected
    );
    assert_eq!(
        standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
        expected
    );
}

#[test]
fn carrier_names_use_the_global_registry_order_at_public_application_admission() {
    let catalogue = invocation_carrier_catalogue(vec![
        invocation_carrier_value_type(TypeId::from_bytes([0x90; 16]), &["sys", "invoke", "Event"]),
        invocation_carrier_value_type(
            TypeId::from_bytes([0x91; 16]),
            &["sys", "invoke", "Request"],
        ),
        invocation_carrier_value_type(TypeId::from_bytes([0x92; 16]), &["sys", "invoke", "Value"]),
    ]);
    let expected = RevisionInvariantError::ReservedInvocationCarrierName {
        type_id: TypeId::from_bytes([0x92; 16]),
    };

    assert_eq!(
        active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
        expected
    );
    assert_eq!(
        deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
        expected
    );
    assert_eq!(
        standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
        expected
    );
}

#[test]
fn neighbouring_invocation_carrier_names_remain_admissible_at_public_boundaries() {
    let catalogue = invocation_carrier_catalogue(vec![invocation_carrier_value_type(
        TypeId::from_bytes(id::<116>()),
        &["sys", "invoke", "Value2"],
    )]);
    let origins = invocation_carrier_origins(&catalogue);

    assert!(active_invocation_carrier_admission(catalogue.clone(), origins.clone()).is_ok());
    assert!(deployable_invocation_carrier_admission(catalogue.clone(), origins.clone()).is_ok());
    assert!(standard_invocation_carrier_admission(catalogue, origins).is_ok());
}

#[test]
fn active_and_deployable_revisions_reject_the_invoke_system_identity() {
    let function = crate::system::SYS_INVOKE_FUNCTION_ID;
    let revision = function_revision_fixture(
        function,
        FunctionRevisionId::from_bytes(id::<92>()),
        digest::<92>(),
        digest::<93>(),
    );
    let catalogue = function_catalogue_with_identity(
        function,
        revision.id(),
        vec![],
        ResolvedType::scalar(StandardScalar::Boolean),
    );
    let origins = function_origins(&revision);
    let expected = RevisionInvariantError::ReservedSystemFunctionIdentity { function };
    let active_source = source(None);

    let active_error = ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue.clone(),
        digest::<7>(),
        vec![],
        vec![revision.clone()],
        origins.clone(),
        vec![],
    )
    .unwrap_err();
    assert_eq!(active_error, expected);

    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let deployable_error = DeployableRevision::new(
        expected_base,
        source(Some(expected_base.source())),
        expected_base.catalogue(),
        catalogue,
        digest::<7>(),
        origins,
        vec![],
        vec![revision],
        vec![],
    )
    .unwrap_err();
    assert_eq!(deployable_error, expected);
    assert_eq!(
        deployable_error.to_string(),
        "the reserved system function identity cannot enter an application catalogue"
    );
}

#[test]
fn active_and_deployable_revisions_reject_the_health_system_name() {
    let function = FunctionId::from_bytes([0x5a; 16]);
    let revision = function_revision_fixture(
        function,
        FunctionRevisionId::from_bytes(id::<94>()),
        digest::<94>(),
        digest::<95>(),
    );
    let catalogue = function_catalogue_with_functions(vec![function_definition_named(
        &["sys", "catalog", "health"],
        function,
        revision.id(),
        ResolvedType::scalar(StandardScalar::Boolean),
    )]);
    let origins = function_origins(&revision);
    let expected = RevisionInvariantError::ReservedSystemFunctionName { function };
    let active_source = source(None);

    let active_error = ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue.clone(),
        digest::<7>(),
        vec![],
        vec![revision.clone()],
        origins.clone(),
        vec![],
    )
    .unwrap_err();
    assert_eq!(active_error, expected);

    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let deployable_error = DeployableRevision::new(
        expected_base,
        source(Some(expected_base.source())),
        expected_base.catalogue(),
        catalogue,
        digest::<7>(),
        origins,
        vec![],
        vec![revision],
        vec![],
    )
    .unwrap_err();
    assert_eq!(deployable_error, expected);
    assert_eq!(
        deployable_error.to_string(),
        "the reserved system function name cannot enter an application catalogue"
    );
}

#[test]
fn active_and_deployable_revisions_reject_the_invoke_system_name() {
    let function = FunctionId::from_bytes([0x5b; 16]);
    let revision = function_revision_fixture(
        function,
        FunctionRevisionId::from_bytes(id::<96>()),
        digest::<96>(),
        digest::<97>(),
    );
    let catalogue = function_catalogue_with_functions(vec![function_definition_named(
        &["sys", "invoke"],
        function,
        revision.id(),
        ResolvedType::scalar(StandardScalar::Boolean),
    )]);
    let origins = function_origins(&revision);
    let expected = RevisionInvariantError::ReservedSystemFunctionName { function };
    let active_source = source(None);

    let active_error = ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue.clone(),
        digest::<7>(),
        vec![],
        vec![revision.clone()],
        origins.clone(),
        vec![],
    )
    .unwrap_err();
    assert_eq!(active_error, expected);

    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let deployable_error = DeployableRevision::new(
        expected_base,
        source(Some(expected_base.source())),
        expected_base.catalogue(),
        catalogue,
        digest::<7>(),
        origins,
        vec![],
        vec![revision],
        vec![],
    )
    .unwrap_err();
    assert_eq!(deployable_error, expected);
    assert_eq!(
        deployable_error.to_string(),
        "the reserved system function name cannot enter an application catalogue"
    );
}

#[test]
fn identity_collisions_precede_name_collisions_in_registry_order() {
    let name_collision = FunctionId::from_bytes([0x5c; 16]);
    let invoke_identity = crate::system::SYS_INVOKE_FUNCTION_ID;
    let health_identity = crate::security::CATALOGUE_HEALTH_FUNCTION_ID;
    let name_revision = function_revision_fixture(
        name_collision,
        FunctionRevisionId::from_bytes(id::<98>()),
        digest::<98>(),
        digest::<99>(),
    );
    let invoke_revision = function_revision_fixture(
        invoke_identity,
        FunctionRevisionId::from_bytes(id::<100>()),
        digest::<100>(),
        digest::<101>(),
    );
    let health_revision = function_revision_fixture(
        health_identity,
        FunctionRevisionId::from_bytes(id::<102>()),
        digest::<102>(),
        digest::<103>(),
    );
    // Reversed application definition input: the reserved name and the
    // invocation identity appear before the health identity. Admission
    // must still select the health identity collision because every
    // identity collision precedes every name collision and the registry
    // order is health then invocation.
    let catalogue = function_catalogue_with_functions(vec![
        function_definition_named(
            &["sys", "invoke"],
            name_collision,
            name_revision.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        ),
        function_definition_named(
            &["sys", "lookup"],
            invoke_identity,
            invoke_revision.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        ),
        function_definition_named(
            &["sys", "probe"],
            health_identity,
            health_revision.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        ),
    ]);
    let mut origins = function_origins(&name_revision);
    origins.extend(function_origins(&invoke_revision));
    origins.extend(function_origins(&health_revision));
    let expected = RevisionInvariantError::ReservedSystemFunctionIdentity {
        function: health_identity,
    };
    let active_source = source(None);

    let active_error = ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue.clone(),
        digest::<7>(),
        vec![],
        vec![
            name_revision.clone(),
            invoke_revision.clone(),
            health_revision.clone(),
        ],
        origins.clone(),
        vec![],
    )
    .unwrap_err();
    assert_eq!(active_error, expected);

    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let deployable_error = DeployableRevision::new(
        expected_base,
        source(Some(expected_base.source())),
        expected_base.catalogue(),
        catalogue,
        digest::<7>(),
        origins,
        vec![],
        vec![name_revision, invoke_revision, health_revision],
        vec![],
    )
    .unwrap_err();
    assert_eq!(deployable_error, expected);
}

#[test]
fn reserved_invoke_identity_beats_a_health_name_collision_in_one_definition() {
    let function_id = crate::system::SYS_INVOKE_FUNCTION_ID;
    let revision = function_revision_fixture(
        function_id,
        FunctionRevisionId::from_bytes(id::<106>()),
        digest::<106>(),
        digest::<107>(),
    );
    // One application definition carries the invocation identity and the
    // health function's exact name. The identity phase is global and runs
    // before the name phase, so admission must report the invocation
    // identity collision even though the same definition also collides
    // with the health name.
    let catalogue = function_catalogue_with_functions(vec![function_definition_named(
        &["sys", "catalog", "health"],
        function_id,
        revision.id(),
        ResolvedType::scalar(StandardScalar::Boolean),
    )]);
    let origins = function_origins(&revision);
    let expected = RevisionInvariantError::ReservedSystemFunctionIdentity {
        function: function_id,
    };
    let active_source = source(None);

    let active_error = ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue.clone(),
        digest::<7>(),
        vec![],
        vec![revision.clone()],
        origins.clone(),
        vec![],
    )
    .unwrap_err();
    assert_eq!(active_error, expected);

    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let deployable_error = DeployableRevision::new(
        expected_base,
        source(Some(expected_base.source())),
        expected_base.catalogue(),
        catalogue,
        digest::<7>(),
        origins,
        vec![],
        vec![revision],
        vec![],
    )
    .unwrap_err();
    assert_eq!(deployable_error, expected);
    assert_eq!(
        deployable_error.to_string(),
        "the reserved system function identity cannot enter an application catalogue"
    );
}

#[test]
fn name_collisions_use_registry_order_independent_of_application_vector_order() {
    let invoke_name_id = FunctionId::from_bytes([0x5e; 16]);
    let health_name_id = FunctionId::from_bytes([0x5f; 16]);
    let invoke_name_revision = function_revision_fixture(
        invoke_name_id,
        FunctionRevisionId::from_bytes(id::<108>()),
        digest::<108>(),
        digest::<109>(),
    );
    let health_name_revision = function_revision_fixture(
        health_name_id,
        FunctionRevisionId::from_bytes(id::<110>()),
        digest::<110>(),
        digest::<111>(),
    );
    // Reversed application definition input: sys.invoke appears before
    // sys.catalog.health. Name collisions must follow registry order, so
    // admission reports the health-name collision from registry position
    // zero even though the invoke-name definition comes first in the
    // application vector. Both schemas are declared exactly because each
    // function must resolve against its own parent namespace.
    let schema_sys = SchemaDefinition::new(
        SchemaId::from_bytes(id::<8>()),
        QualifiedSemanticName::new(["sys"]).unwrap(),
    );
    let schema_sys_catalog = SchemaDefinition::new(
        SchemaId::from_bytes(id::<9>()),
        QualifiedSemanticName::new(["sys", "catalog"]).unwrap(),
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![schema_sys, schema_sys_catalog],
        vec![],
        vec![
            function_definition_named(
                &["sys", "invoke"],
                invoke_name_id,
                invoke_name_revision.id(),
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
            function_definition_named(
                &["sys", "catalog", "health"],
                health_name_id,
                health_name_revision.id(),
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
        ],
    )
    .unwrap();
    // Reserved-name validation is authoritative before origin
    // completeness, so the failing revision's origin fixture suffices.
    let origins = function_origins(&health_name_revision);
    let expected = RevisionInvariantError::ReservedSystemFunctionName {
        function: health_name_id,
    };
    let active_source = source(None);

    let active_error = ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue.clone(),
        digest::<7>(),
        vec![],
        vec![invoke_name_revision.clone(), health_name_revision.clone()],
        origins.clone(),
        vec![],
    )
    .unwrap_err();
    assert_eq!(active_error, expected);

    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let deployable_error = DeployableRevision::new(
        expected_base,
        source(Some(expected_base.source())),
        expected_base.catalogue(),
        catalogue,
        digest::<7>(),
        origins,
        vec![],
        vec![invoke_name_revision, health_name_revision],
        vec![],
    )
    .unwrap_err();
    assert_eq!(deployable_error, expected);
    assert_eq!(
        deployable_error.to_string(),
        "the reserved system function name cannot enter an application catalogue"
    );
}

#[test]
fn neighbouring_sys_names_remain_admissible() {
    let function = FunctionId::from_bytes([0x5d; 16]);
    let revision = function_revision_fixture(
        function,
        FunctionRevisionId::from_bytes(id::<104>()),
        digest::<104>(),
        digest::<105>(),
    );
    let catalogue = function_catalogue_with_functions(vec![function_definition_named(
        &["sys", "probe"],
        function,
        revision.id(),
        ResolvedType::scalar(StandardScalar::Boolean),
    )]);
    let origins = function_origins(&revision);
    let active_source = source(None);
    ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue,
        digest::<7>(),
        vec![],
        vec![revision],
        origins,
        vec![],
    )
    .expect("a neighbouring sys.probe name must remain admissible");
}
