use super::*;
#[test]
fn exposes_checked_standard_upgrade_preparation_seam() {
    let _: fn(
        &CheckedStandardLibrary,
        &ActiveDatabaseRevision,
    ) -> Result<PreparedStandardUpgrade, PrepareStandardUpgradeError> =
        prepare_checked_standard_upgrade;
}

#[test]
fn standard_upgrade_rejects_an_already_installed_standard_before_source_work() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let source_text = "CREATE SCHEMA std;";
    let source_unit = SourceUnitId::from_bytes([0xc1; 16]);
    let source = stored_source_with_ids(
        source_text,
        source_unit,
        SourceBundleId::from_bytes([0xc2; 16]),
        SourceRevisionId::from_bytes([0xc3; 16]),
    );
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0xc4; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0xc5; 16]),
            semantic_name(["std"]),
        )],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let origins = vec![DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes([0xc5; 16])),
        SourceOrigin::new(source_unit, 0, source_text.len() as u32).unwrap(),
    )];
    let context = CatalogueHashContext::version_two(verified.clone());
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), origins, Vec::new()),
        ),
        context,
    )
    .unwrap();

    let error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled { revision }
            if revision == verified.revision()
    ));
    assert_eq!(
        error.to_string(),
        format!(
            "standard library {} is already installed",
            verified.revision()
        )
    );
    assert!(std::error::Error::source(&error).is_none());
    assert_no_standard_upgrade_allocations();
}

#[test]
fn prepares_an_empty_version_one_application_for_a_checked_standard_upgrade() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_one_active();

    let prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();

    assert_eq!(
        prepared.standard_library().verified_snapshot().digest(),
        verified.digest()
    );
    assert_eq!(
        prepared.application_revision().expected_base(),
        active.pair()
    );
    assert_eq!(
        prepared
            .application_revision()
            .catalogue_hash_context()
            .standard()
            .map(|snapshot| snapshot.digest()),
        Some(verified.digest())
    );
}

#[test]
fn standard_upgrade_retries_every_companion_identity_before_constructing_version_two() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_one_active();

    let prepared = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap();

    for counter in [
        &PREPARE_CATALOGUE_ALLOCATIONS,
        &PREPARE_BUNDLE_ALLOCATIONS,
        &PREPARE_REVISION_ALLOCATIONS,
        &PREPARE_UNIT_ALLOCATIONS,
    ] {
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }
    assert_eq!(PREPARE_SCHEMA_ALLOCATIONS.load(Ordering::SeqCst), 0);
    assert_eq!(PREPARE_TYPE_ALLOCATIONS.load(Ordering::SeqCst), 0);
    let application = prepared.application_revision();
    assert_eq!(application.candidate().revision().to_bytes(), [0x81; 16]);
    assert_eq!(application.source().bundle().to_bytes(), [0x82; 16]);
    assert_eq!(application.source().id().to_bytes(), [0x83; 16]);
    assert_eq!(application.source().units()[0].id().to_bytes(), [0x84; 16]);
}

#[test]
fn standard_upgrade_retries_and_copies_every_retained_source_unit() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let empty = empty_version_one_active();
    let bundle = SourceBundle::new([
        SourceUnit::new("first.orna", "CREATE SCHEMA app;"),
        SourceUnit::new("second.orna", "-- retained empty unit\n"),
    ])
    .unwrap();
    let version_one = prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
    let active = active_from_prepared_version_one_candidate(&version_one);

    let prepared = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap();

    assert_eq!(PREPARE_UNIT_ALLOCATIONS.load(Ordering::SeqCst), 3);
    assert_eq!(
        prepared
            .application_revision()
            .source()
            .units()
            .iter()
            .map(|unit| unit.id().to_bytes())
            .collect::<Vec<_>>(),
        vec![[0x84; 16], [0x85; 16]]
    );
}

#[test]
fn matches_nonempty_version_one_source_before_preparing_the_upgrade() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let empty = empty_version_one_active();
    let bundle =
        SourceBundle::new([SourceUnit::new("application.orna", "CREATE SCHEMA app;")]).unwrap();
    let report = check(&bundle, empty.catalogue());
    let version_one = prepare(&report, empty.pair(), &empty).unwrap();
    let active = active_from_prepared_version_one_candidate(&version_one);

    let prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();

    assert_eq!(
        prepared.application_revision().expected_base(),
        active.pair()
    );
    assert_eq!(
        prepared.application_revision().candidate().schemas().len(),
        1
    );
    assert_eq!(
        prepared.application_revision().candidate().schemas()[0].name(),
        &semantic_name(["app"])
    );
}

#[test]
fn standard_upgrade_requires_an_exact_allocation_free_version_one_match() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let empty = empty_version_one_active();
    let bundle =
        SourceBundle::new([SourceUnit::new("application.orna", "CREATE SCHEMA app;")]).unwrap();
    let version_one = prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
    let origins = version_one
        .origins()
        .iter()
        .map(|origin| {
            DefinitionOrigin::new(
                origin.identity(),
                SourceOrigin::new(
                    SourceUnitId::from_bytes([0xa1; 16]),
                    origin.source().byte_start(),
                    origin.source().byte_end(),
                )
                .unwrap(),
            )
        })
        .collect();
    let active = version_one_active_with_origins(
        "CREATE SCHEMA changed;",
        version_one.candidate().clone(),
        origins,
    );

    let error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        PrepareStandardUpgradeError::ActiveSourceMismatch
    ));
    assert_eq!(
        error.to_string(),
        "the active application source does not match the active catalogue"
    );
    assert!(std::error::Error::source(&error).is_none());
    assert_no_standard_upgrade_allocations();
}

#[test]
fn standard_upgrade_compares_the_complete_current_function_revision_record() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let empty = empty_version_one_active();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let version_one = prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
    let active = active_from_prepared_version_one_candidate(&version_one);
    let current = active.function_revisions()[0].clone();
    let origin = current.declaration_origin();
    let changed_origin = FunctionRevisionRecord::new(
        current.function(),
        current.id(),
        current.revision_number(),
        SourceOrigin::new(
            origin.source_unit(),
            origin.byte_start() + 1,
            origin.byte_end(),
        )
        .unwrap(),
        current.declaration_content_hash(),
        current.semantic_hash(),
        current.language_version(),
        current.artifact().clone(),
    )
    .unwrap();
    let changed_declaration_hash = FunctionRevisionRecord::new(
        current.function(),
        current.id(),
        current.revision_number(),
        current.declaration_origin(),
        Sha256Digest::from_bytes([0xe1; 32]),
        current.semantic_hash(),
        current.language_version(),
        current.artifact().clone(),
    )
    .unwrap();
    let changed_language = "orna.language/changed";
    let changed_language_version = FunctionRevisionRecord::new(
        current.function(),
        current.id(),
        current.revision_number(),
        current.declaration_origin(),
        current.declaration_content_hash(),
        function_semantic_digest(
            active
                .catalogue()
                .function_by_id(current.function())
                .unwrap(),
            changed_language,
            current.artifact(),
            active.expressions(),
            active.references(),
        )
        .unwrap(),
        changed_language,
        current.artifact().clone(),
    )
    .unwrap();
    for (label, changed) in [
        ("origin", changed_origin),
        ("declaration hash", changed_declaration_hash),
        ("language version", changed_language_version),
    ] {
        let catalogue_hash = catalogue_digest(
            active.catalogue(),
            std::slice::from_ref(&changed),
            active.expressions(),
            active.origins(),
            active.references(),
        )
        .unwrap();
        let hostile = ActiveDatabaseRevision::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            catalogue_hash,
            active.expressions().to_vec(),
            vec![changed],
            active.origins().to_vec(),
            active.references().to_vec(),
        )
        .unwrap();

        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &hostile,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();

        assert!(
            matches!(&error, PrepareStandardUpgradeError::ActiveSourceMismatch),
            "Gate 6 accepted changed {label}"
        );
        assert_eq!(
            error.to_string(),
            "the active application source does not match the active catalogue"
        );
        assert!(std::error::Error::source(&error).is_none());
        assert_no_standard_upgrade_allocations();
    }

    let function = active
        .catalogue()
        .function_by_id(current.function())
        .unwrap();
    let record_with_artifact = |artifact: ExecutableArtifact| {
        FunctionRevisionRecord::new(
            current.function(),
            current.id(),
            current.revision_number(),
            current.declaration_origin(),
            current.declaration_content_hash(),
            function_semantic_digest(
                function,
                current.language_version(),
                &artifact,
                active.expressions(),
                active.references(),
            )
            .unwrap(),
            current.language_version(),
            artifact,
        )
        .unwrap()
    };
    let changed_format = ExecutableArtifact::new(
        current.artifact().kind(),
        "orna.hostile-format",
        current.artifact().version(),
        current.artifact().payload().to_vec(),
        current.artifact().content_hash(),
    )
    .unwrap();
    let changed_version = ExecutableArtifact::new(
        current.artifact().kind(),
        current.artifact().format(),
        current.artifact().version() + 1,
        current.artifact().payload().to_vec(),
        current.artifact().content_hash(),
    )
    .unwrap();
    let mut changed_payload = current.artifact().payload().to_vec();
    changed_payload.push(0xff);
    let changed_payload = ExecutableArtifact::new(
        current.artifact().kind(),
        current.artifact().format(),
        current.artifact().version(),
        changed_payload.clone(),
        artifact_payload_digest(&changed_payload).unwrap(),
    )
    .unwrap();
    for (label, changed) in [
        ("artifact format", record_with_artifact(changed_format)),
        ("artifact version", record_with_artifact(changed_version)),
        ("artifact payload", record_with_artifact(changed_payload)),
    ] {
        let catalogue_hash = catalogue_digest(
            active.catalogue(),
            std::slice::from_ref(&changed),
            active.expressions(),
            active.origins(),
            active.references(),
        )
        .unwrap();
        let hostile = ActiveDatabaseRevision::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            catalogue_hash,
            active.expressions().to_vec(),
            vec![changed],
            active.origins().to_vec(),
            active.references().to_vec(),
        )
        .unwrap();
        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &hostile,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert!(
            matches!(&error, PrepareStandardUpgradeError::ActiveSourceMismatch),
            "Gate 6 accepted changed {label}"
        );
        assert_no_standard_upgrade_allocations();
    }

    let assert_content_mismatch =
        |catalogue: CatalogueSnapshot,
         expressions: Vec<ExpressionArtifact>,
         origins: Vec<DefinitionOrigin>,
         references: Vec<orna_core::revision::DefinitionReference>| {
            let catalogue_hash = catalogue_digest(
                &catalogue,
                active.function_revisions(),
                &expressions,
                &origins,
                &references,
            )
            .unwrap();
            let hostile = ActiveDatabaseRevision::new(
                active.pair(),
                active.source().clone(),
                catalogue,
                catalogue_hash,
                expressions,
                active.function_revisions().to_vec(),
                origins,
                references,
            )
            .unwrap();
            let error = prepare_checked_standard_upgrade_with_allocator(
                &standard,
                &hostile,
                retrying_standard_allocator(&verified),
            )
            .unwrap_err();
            assert!(matches!(
                error,
                PrepareStandardUpgradeError::ActiveSourceMismatch
            ));
            assert_no_standard_upgrade_allocations();
        };

    let mut changed_origins = active.origins().to_vec();
    let first_origin = changed_origins[0].source();
    changed_origins[0] = DefinitionOrigin::new(
        changed_origins[0].identity(),
        SourceOrigin::new(
            first_origin.source_unit(),
            first_origin.byte_start() + 1,
            first_origin.byte_end(),
        )
        .unwrap(),
    );
    assert_content_mismatch(
        active.catalogue().clone(),
        active.expressions().to_vec(),
        changed_origins,
        active.references().to_vec(),
    );

    let first_reference = active.references()[0].clone();
    let changed_reference = orna_core::revision::DefinitionReference::new(
        first_reference.source_function(),
        first_reference.source_revision(),
        first_reference.ordinal(),
        first_reference.target(),
        first_reference.kind(),
        SourceOrigin::new(
            first_reference.source_origin().source_unit(),
            first_reference.source_origin().byte_start() + 1,
            first_reference.source_origin().byte_end(),
        )
        .unwrap(),
    );
    assert_content_mismatch(
        active.catalogue().clone(),
        active.expressions().to_vec(),
        active.origins().to_vec(),
        vec![changed_reference]
            .into_iter()
            .chain(active.references()[1..].iter().cloned())
            .collect(),
    );

    let expression_payload = b"hostile-expression".to_vec();
    let expression = ExpressionArtifact::new(
        ExpressionId::from_bytes([0xe9; 16]),
        "orna.constant-expression",
        1,
        expression_payload.clone(),
        artifact_payload_digest(&expression_payload).unwrap(),
    )
    .unwrap();
    let mut expression_origins = active.origins().to_vec();
    expression_origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Expression(expression.id()),
        SourceOrigin::new(active.source().units()[0].id(), 0, 1).unwrap(),
    ));
    assert_content_mismatch(
        active.catalogue().clone(),
        vec![expression],
        expression_origins,
        active.references().to_vec(),
    );

    let mut changed_object_types = active.catalogue().object_types().to_vec();
    changed_object_types[0] = ObjectTypeDefinition::new(
        changed_object_types[0].id(),
        semantic_name(["app", "changed"]),
        changed_object_types[0].fields().to_vec(),
    );
    let changed_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        active.catalogue().revision(),
        active.catalogue().schemas().to_vec(),
        changed_object_types,
        active.catalogue().value_types().to_vec(),
        active.catalogue().type_bindings().to_vec(),
        active.catalogue().functions().to_vec(),
    )
    .unwrap();
    assert_content_mismatch(
        changed_catalogue,
        active.expressions().to_vec(),
        active.origins().to_vec(),
        active.references().to_vec(),
    );
}

#[test]
fn standard_upgrade_rejects_active_source_mismatch_before_revision_exhaustion() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let empty = empty_version_one_active();
    let bundle = SourceBundle::new([SourceUnit::new(
        "application.orna",
        "CREATE SCHEMA app; CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL); \
             CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN) \
             TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;",
    )])
    .unwrap();
    let version_one = prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
    let active = active_from_prepared_version_one_candidate(&version_one);
    let original = &active.source().units()[0];
    let changed_content = format!(
        "-- shifts every declaration location\n{}",
        original.content()
    );
    let shifted_unit = StoredSourceUnit::new(
        original.id(),
        original.ordinal(),
        original.logical_path(),
        &changed_content,
        source_unit_content_digest(&changed_content).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&shifted_unit)).unwrap();
    let shifted_source = StoredSourceRevision::new(
        active.source().bundle(),
        active.source().id(),
        active.source().parent(),
        vec![shifted_unit],
        bundle_hash,
        source_revision_record_digest(
            active.source().bundle(),
            active.source().parent(),
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap();
    let current = &active.function_revisions()[0];
    let exhausted = FunctionRevisionRecord::new(
        current.function(),
        current.id(),
        u64::MAX,
        current.declaration_origin(),
        current.declaration_content_hash(),
        current.semantic_hash(),
        current.language_version(),
        current.artifact().clone(),
    )
    .unwrap();
    let catalogue_hash = catalogue_digest(
        active.catalogue(),
        std::slice::from_ref(&exhausted),
        active.expressions(),
        active.origins(),
        active.references(),
    )
    .unwrap();
    let hostile = ActiveDatabaseRevision::new(
        active.pair(),
        shifted_source,
        active.catalogue().clone(),
        catalogue_hash,
        active.expressions().to_vec(),
        vec![exhausted],
        active.origins().to_vec(),
        active.references().to_vec(),
    )
    .unwrap();

    let error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &hostile,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        PrepareStandardUpgradeError::ActiveSourceMismatch
    ));
    assert_eq!(
        error.to_string(),
        "the active application source does not match the active catalogue"
    );
    assert!(std::error::Error::source(&error).is_none());
    assert_no_standard_upgrade_allocations();
}

#[test]
fn prepares_version_two_server_semantics_after_matching_version_one_source() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let empty = empty_version_one_active();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check(&bundle, empty.catalogue());
    assert!(report.diagnostics().is_empty());
    let version_one = prepare(&report, empty.pair(), &empty).unwrap();
    assert_eq!(
        version_one.candidate().object_types()[0].fields()[0].resolved_type(),
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    let version_one_function = &version_one.candidate().functions()[0];
    let FunctionReturn::Rows(version_one_columns) = version_one_function.return_type() else {
        panic!("the legacy server fixture must retain a ROWS return")
    };
    assert_eq!(
        version_one_columns[0].resolved_type(),
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    let active = active_from_prepared_version_one_candidate(&version_one);
    assert_eq!(
        active.function_revisions()[0].semantic_hash_version(),
        FunctionSemanticHashVersion::Version1
    );
    let legacy_payload = active.function_revisions()[0].artifact().payload().to_vec();
    let legacy_payload_hash = active.function_revisions()[0].artifact().content_hash();

    let public_prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();
    assert_eq!(
        public_prepared
            .application_revision()
            .catalogue_hash_context()
            .version(),
        CatalogueHashVersion::Version2
    );
    assert_eq!(
        public_prepared
            .application_revision()
            .candidate()
            .object_types()[0]
            .fields()[0]
            .resolved_type(),
        ResolvedType::Value(TypeId::from_bytes([3; 16]))
    );
    let public_function = &public_prepared
        .application_revision()
        .candidate()
        .functions()[0];
    let FunctionReturn::Rows(public_columns) = public_function.return_type() else {
        panic!("server fixture must retain a ROWS return")
    };
    assert_eq!(public_columns.len(), 1);
    assert_eq!(
        public_columns[0].resolved_type(),
        ResolvedType::Value(TypeId::from_bytes([3; 16]))
    );
    assert!(
        orna_core::revision::validate_persistable_catalogue(public_prepared.application_revision())
            .is_ok()
    );
    assert!(
        public_prepared
            .application_revision()
            .references()
            .iter()
            .any(|reference| {
                reference.target()
                    == DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
                    && reference.kind() == DefinitionReferenceKind::NamedType
            })
    );

    let prepared = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap();

    assert_eq!(
        prepared
            .application_revision()
            .new_function_revisions()
            .len(),
        1
    );
    assert_eq!(
        prepared.application_revision().new_function_revisions()[0].semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
    assert_eq!(
        prepared.application_revision().new_function_revisions()[0]
            .artifact()
            .payload(),
        legacy_payload
    );
    assert_eq!(
        prepared.application_revision().new_function_revisions()[0]
            .artifact()
            .content_hash(),
        legacy_payload_hash
    );
    assert!(
        prepared
            .application_revision()
            .references()
            .iter()
            .any(|reference| {
                reference.target()
                    == DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
                    && reference.kind() == DefinitionReferenceKind::NamedType
            })
    );
    assert_eq!(
        PREPARE_FUNCTION_REVISION_ALLOCATIONS.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        prepared.application_revision().new_function_revisions()[0]
            .id()
            .to_bytes(),
        [0x90; 16]
    );
}

#[test]
fn prepares_version_two_mutation_parameter_and_reference_return_with_value_identity() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let empty = empty_version_one_active();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.create(p_done BOOLEAN) RETURNS ROWS (created REF app.item)\
            TRANSACTION ATOMIC AS INSERT INTO app.item AS made (done) VALUES (p_done) RETURNING REF(made);";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let version_one = prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
    let active = active_from_prepared_version_one_candidate(&version_one);

    let prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();
    assert_eq!(
        prepared
            .application_revision()
            .catalogue_hash_context()
            .version(),
        CatalogueHashVersion::Version2
    );
    let candidate = prepared.application_revision().candidate();
    assert_eq!(
        candidate.object_types()[0].fields()[0].resolved_type(),
        ResolvedType::Value(TypeId::from_bytes([3; 16]))
    );
    let function = &candidate.functions()[0];
    assert_eq!(
        function.parameters()[0].resolved_type(),
        ResolvedType::Value(TypeId::from_bytes([3; 16]))
    );
    let item = candidate.object_types()[0].id();
    let FunctionReturn::Rows(columns) = function.return_type() else {
        panic!("the mutation fixture must retain a ROWS return")
    };
    assert!(matches!(
        columns[0].resolved_type(),
        ResolvedType::Reference { target } if target == item
    ));
    assert!(
        prepared
            .application_revision()
            .references()
            .iter()
            .any(|reference| {
                reference.target()
                    == DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
            })
    );
    assert!(
        orna_core::revision::validate_persistable_catalogue(prepared.application_revision())
            .is_ok()
    );
    let revision = &prepared.application_revision().new_function_revisions()[0];
    assert_eq!(
        revision.semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
    assert_eq!(
        revision.artifact().payload(),
        active.function_revisions()[0].artifact().payload()
    );
    assert_eq!(
        revision.artifact().content_hash(),
        active.function_revisions()[0].artifact().content_hash()
    );
}

#[test]
fn standard_upgrade_checks_function_revision_exhaustion_before_allocation() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let empty = empty_version_one_active();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let version_one = prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
    let active = active_from_prepared_version_one_candidate(&version_one);
    let current = active.function_revisions()[0].clone();
    let exhausted = FunctionRevisionRecord::new(
        current.function(),
        current.id(),
        u64::MAX,
        current.declaration_origin(),
        current.declaration_content_hash(),
        current.semantic_hash(),
        current.language_version(),
        current.artifact().clone(),
    )
    .unwrap();
    let catalogue_hash = catalogue_digest(
        active.catalogue(),
        std::slice::from_ref(&exhausted),
        active.expressions(),
        active.origins(),
        active.references(),
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new(
        active.pair(),
        active.source().clone(),
        active.catalogue().clone(),
        catalogue_hash,
        active.expressions().to_vec(),
        vec![exhausted],
        active.origins().to_vec(),
        active.references().to_vec(),
    )
    .unwrap();

    let error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        PrepareStandardUpgradeError::FunctionRevisionNumberExhausted { function }
            if function == current.function()
    ));
    assert_eq!(error.to_string(), "function revision number is exhausted");
    assert!(std::error::Error::source(&error).is_none());
    assert_no_standard_upgrade_allocations();
}

#[test]
fn prepares_version_two_client_semantics_after_matching_version_one_source() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let initial = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "application.orna",
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    let seeded = prepare_standard_application(&report, initial.pair(), &initial).unwrap();
    let active = version_one_client_active_from_standard_candidate(&seeded);
    assert_eq!(
        active.catalogue().functions()[0].return_type(),
        &FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean))
    );
    assert_eq!(
        active.function_revisions()[0].semantic_hash_version(),
        FunctionSemanticHashVersion::Version1
    );

    let prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();
    assert_eq!(
        prepared
            .application_revision()
            .catalogue_hash_context()
            .version(),
        CatalogueHashVersion::Version2
    );

    let function = &prepared.application_revision().candidate().functions()[0];
    let revision = &prepared.application_revision().new_function_revisions()[0];
    assert_eq!(function.domain(), FunctionDomain::Client);
    assert_eq!(
        function.return_type(),
        &FunctionReturn::Single(ResolvedType::Value(TypeId::from_bytes([3; 16])))
    );
    assert!(
        orna_core::revision::validate_persistable_catalogue(prepared.application_revision())
            .is_ok()
    );
    assert_eq!(
        revision.semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Client);
    assert_eq!(
        revision.artifact().payload(),
        b"ORNACP\0\0\0\0\0\x01\x01\x01"
    );
    assert_eq!(
        revision.artifact().payload(),
        active.function_revisions()[0].artifact().payload()
    );
    assert_eq!(
        revision.artifact().content_hash(),
        active.function_revisions()[0].artifact().content_hash()
    );
    assert_eq!(prepared.application_revision().references().len(), 1);
    assert_eq!(
        prepared.application_revision().references()[0].target(),
        DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
    );
}

#[test]
fn standard_upgrade_reuses_an_exact_historical_version_two_revision() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let empty = empty_version_one_active();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let version_one = prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
    let active = active_from_prepared_version_one_candidate(&version_one);
    let first = prepare_checked_standard_upgrade(&standard, &active).unwrap();
    let historical = first.application_revision().new_function_revisions()[0].clone();
    let active = active_with_history(&active, vec![historical.clone()]);

    let reused = prepare_checked_standard_upgrade(&standard, &active).unwrap();

    assert!(
        reused
            .application_revision()
            .new_function_revisions()
            .is_empty()
    );
    assert_eq!(
        reused.application_revision().candidate().functions()[0].current_revision(),
        historical.id()
    );
}

#[test]
fn standard_upgrade_rejects_near_matching_historical_version_two_revisions_for_reuse() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let empty = empty_version_one_active();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let version_one = prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
    let active = active_from_prepared_version_one_candidate(&version_one);
    let first = prepare_checked_standard_upgrade(&standard, &active).unwrap();
    let historical = first.application_revision().new_function_revisions()[0].clone();
    let mut payload = historical.artifact().payload().to_vec();
    payload.push(0);
    let wrong_artifact = ExecutableArtifact::new(
        historical.artifact().kind(),
        historical.artifact().format(),
        historical.artifact().version(),
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let revisions = [
        FunctionRevisionRecord::new(
            historical.function(),
            historical.id(),
            historical.revision_number(),
            historical.declaration_origin(),
            historical.declaration_content_hash(),
            historical.semantic_hash(),
            "orna.language/changed",
            historical.artifact().clone(),
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
        FunctionRevisionRecord::new(
            historical.function(),
            historical.id(),
            historical.revision_number(),
            historical.declaration_origin(),
            historical.declaration_content_hash(),
            historical.semantic_hash(),
            historical.language_version(),
            wrong_artifact,
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
        FunctionRevisionRecord::new(
            historical.function(),
            historical.id(),
            historical.revision_number(),
            historical.declaration_origin(),
            historical.declaration_content_hash(),
            Sha256Digest::from_bytes([0xf1; 32]),
            historical.language_version(),
            historical.artifact().clone(),
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
    ];

    for historical in revisions {
        let active = active_with_history(&active, vec![historical.clone()]);
        let prepared = prepare_checked_standard_upgrade(&standard, &active).unwrap();

        assert_eq!(
            prepared
                .application_revision()
                .new_function_revisions()
                .len(),
            1
        );
        assert_ne!(
            prepared.application_revision().new_function_revisions()[0].id(),
            historical.id()
        );
    }
}

#[test]
fn standard_upgrade_checks_history_for_reuse_before_revision_number_exhaustion() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let empty = empty_version_one_active();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (done BOOLEAN)\
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT t.done FROM app.task t;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let version_one = prepare(&check(&bundle, empty.catalogue()), empty.pair(), &empty).unwrap();
    let active = active_from_prepared_version_one_candidate(&version_one);
    let first = prepare_checked_standard_upgrade(&standard, &active).unwrap();
    let historical = first.application_revision().new_function_revisions()[0].clone();
    let exact_maximum = FunctionRevisionRecord::new(
        historical.function(),
        historical.id(),
        u64::MAX,
        historical.declaration_origin(),
        historical.declaration_content_hash(),
        historical.semantic_hash(),
        historical.language_version(),
        historical.artifact().clone(),
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let reusable = active_with_history(&active, vec![exact_maximum.clone()]);

    let prepared = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &reusable,
        retrying_standard_allocator(&verified),
    )
    .unwrap();

    assert!(
        prepared
            .application_revision()
            .new_function_revisions()
            .is_empty()
    );
    assert_eq!(
        prepared.application_revision().candidate().functions()[0].current_revision(),
        exact_maximum.id()
    );
    assert_eq!(
        PREPARE_FUNCTION_REVISION_ALLOCATIONS.load(Ordering::SeqCst),
        0
    );

    let non_reusable_maximum = FunctionRevisionRecord::new(
        historical.function(),
        historical.id(),
        u64::MAX,
        historical.declaration_origin(),
        historical.declaration_content_hash(),
        historical.semantic_hash(),
        "orna.language/changed",
        historical.artifact().clone(),
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let exhausted = active_with_history(&active, vec![non_reusable_maximum]);

    let error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &exhausted,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        PrepareStandardUpgradeError::FunctionRevisionNumberExhausted { function }
            if function == historical.function()
    ));
    assert_eq!(error.to_string(), "function revision number is exhausted");
    assert!(std::error::Error::source(&error).is_none());
    assert_no_standard_upgrade_allocations();
}

#[test]
fn standard_upgrade_rejects_std_namespace_before_reserved_identities() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let catalogue = CatalogueSnapshot::new(
        verified.catalogue().revision(),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0xb1; 16]),
            semantic_name(["std"]),
        )],
        Vec::new(),
    )
    .unwrap();
    let active = version_one_active_with_origins(
        "CREATE SCHEMA std;",
        catalogue,
        vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0xb1; 16])),
            SourceOrigin::new(SourceUnitId::from_bytes([0xa1; 16]), 0, 18).unwrap(),
        )],
    );

    let error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        PrepareStandardUpgradeError::NamespaceOccupied { name }
            if name == &semantic_name(["std"])
    ));
    assert_eq!(
        error.to_string(),
        "the application catalogue already uses the reserved std namespace"
    );
    assert!(std::error::Error::source(&error).is_none());
    assert_no_standard_upgrade_allocations();
}

#[test]
fn standard_upgrade_namespace_gate_uses_snapshot_family_order_and_first_name() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let first_schema = SchemaDefinition::new(
        SchemaId::from_bytes([0xd1; 16]),
        semantic_name(["std", "first"]),
    );
    let second_schema = SchemaDefinition::new(
        SchemaId::from_bytes([0xd2; 16]),
        semantic_name(["std", "second"]),
    );
    let object = ObjectTypeDefinition::new(
        TypeId::from_bytes([0xd3; 16]),
        semantic_name(["std", "first", "object"]),
        Vec::new(),
    );
    let function_id = FunctionId::from_bytes([0xd5; 16]);
    let function_revision_id = FunctionRevisionId::from_bytes([0xd6; 16]);
    let function = FunctionDefinition::new(
        function_id,
        semantic_name(["std", "first", "function"]),
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
        function_revision_id,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let function_payload = b"namespace-function".to_vec();
    let function_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-plan",
        1,
        function_payload.clone(),
        artifact_payload_digest(&function_payload).unwrap(),
    )
    .unwrap();
    let function_revision = FunctionRevisionRecord::new(
        function_id,
        function_revision_id,
        1,
        SourceOrigin::new(SourceUnitId::from_bytes([0xd8; 16]), 3, 4).unwrap(),
        Sha256Digest::from_bytes([0xd9; 32]),
        function_semantic_digest(&function, "orna.language/1", &function_artifact, &[], &[])
            .unwrap(),
        "orna.language/1",
        function_artifact,
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0xd7; 16]),
        vec![first_schema.clone(), second_schema],
        vec![object],
        Vec::new(),
        Vec::new(),
        vec![function],
    )
    .unwrap();
    assert_eq!(catalogue.schemas()[0].name(), first_schema.name());
    assert_eq!(catalogue.object_types().len(), 1);
    assert_eq!(catalogue.value_types().len(), 0);
    assert_eq!(catalogue.type_bindings().len(), 0);
    assert_eq!(catalogue.functions().len(), 1);

    let source_unit = SourceUnitId::from_bytes([0xd8; 16]);
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(first_schema.id()),
            SourceOrigin::new(source_unit, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0xd2; 16])),
            SourceOrigin::new(source_unit, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ObjectType(TypeId::from_bytes([0xd3; 16])),
            SourceOrigin::new(source_unit, 2, 3).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Function(function_id),
            SourceOrigin::new(source_unit, 3, 4).unwrap(),
        ),
    ];
    let source = stored_source_with_ids(
        "0123456789",
        source_unit,
        SourceBundleId::from_bytes([0xd9; 16]),
        SourceRevisionId::from_bytes([0xda; 16]),
    );
    let catalogue_hash = catalogue_digest(
        &catalogue,
        std::slice::from_ref(&function_revision),
        &[],
        &origins,
        &[],
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new(
        RevisionPair::new(source.id(), catalogue.revision()),
        source,
        catalogue,
        catalogue_hash,
        Vec::new(),
        vec![function_revision],
        origins,
        Vec::new(),
    )
    .unwrap();
    let error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        PrepareStandardUpgradeError::NamespaceOccupied { name }
            if name == first_schema.name()
    ));
    assert_eq!(
        error.to_string(),
        "the application catalogue already uses the reserved std namespace"
    );
    assert!(std::error::Error::source(&error).is_none());
    assert_no_standard_upgrade_allocations();
}

#[test]
fn standard_upgrade_reaches_schema_name_conflict_for_a_non_std_checked_standard() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_non_std_schema_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let source_text = "CREATE SCHEMA library;";
    let source_unit = SourceUnitId::from_bytes([0xa0; 16]);
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0xa3; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0xa4; 16]),
            semantic_name(["library"]),
        )],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let active = version_one_active_with_source(
        stored_source_with_ids(
            source_text,
            source_unit,
            SourceBundleId::from_bytes([0xa1; 16]),
            SourceRevisionId::from_bytes([0xa2; 16]),
        ),
        catalogue,
        vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes([0xa4; 16])),
            SourceOrigin::new(source_unit, 0, source_text.len() as u32).unwrap(),
        )],
    );

    let error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();

    let expected = StandardApplicationContextError::SchemaNameConflict {
        name: semantic_name(["library"]),
    };
    assert!(matches!(
        &error,
        PrepareStandardUpgradeError::Context { source } if source == &expected
    ));
    assert_eq!(
        error.to_string(),
        "the checked standard library cannot form an application context: the application catalogue conflicts with standard schema name library"
    );
    assert!(std::error::Error::source(&error).is_some());
    let nested = std::error::Error::source(&error).unwrap();
    assert_eq!(nested.to_string(), expected.to_string());
    assert!(std::error::Error::source(nested).is_none());
    assert_no_standard_upgrade_allocations();
}

#[test]
fn standard_upgrade_reserved_schema_identity_precedes_non_std_schema_name_conflict() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_non_std_schema_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let source_text = "CREATE SCHEMA library;";
    let source_unit = SourceUnitId::from_bytes([0xa5; 16]);
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0xa6; 16]),
        vec![SchemaDefinition::new(
            verified.catalogue().schemas()[0].id(),
            semantic_name(["library"]),
        )],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let active = version_one_active_with_source(
        stored_source_with_ids(
            source_text,
            source_unit,
            SourceBundleId::from_bytes([0xa7; 16]),
            SourceRevisionId::from_bytes([0xa8; 16]),
        ),
        catalogue,
        vec![DefinitionOrigin::new(
            DefinitionIdentity::Schema(verified.catalogue().schemas()[0].id()),
            SourceOrigin::new(source_unit, 0, source_text.len() as u32).unwrap(),
        )],
    );

    let error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        PrepareStandardUpgradeError::ReservedIdentity {
            identity: crate::StandardUpgradeIdentity::Schema(id),
        } if id == verified.catalogue().schemas()[0].id()
    ));
    assert_eq!(
        error.to_string(),
        "the application state conflicts with a reserved standard library identity"
    );
    assert!(std::error::Error::source(&error).is_none());
    assert_no_standard_upgrade_allocations();
}

#[test]
fn standard_upgrade_rejects_reserved_catalogue_identity_before_context_and_source() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let catalogue =
        CatalogueSnapshot::new(verified.catalogue().revision(), Vec::new(), Vec::new()).unwrap();
    let active = version_one_active_with("CREATE SCHEMA ;", catalogue);

    let error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        PrepareStandardUpgradeError::ReservedIdentity {
            identity: crate::StandardUpgradeIdentity::CatalogueRevision(id),
        } if id == verified.catalogue().revision()
    ));
    assert_eq!(
        error.to_string(),
        "the application state conflicts with a reserved standard library identity"
    );
    assert!(std::error::Error::source(&error).is_none());
    assert_no_standard_upgrade_allocations();
}

#[test]
fn standard_upgrade_reserved_identity_gate_checks_every_visible_class_in_order() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let safe_source = |unit_id, bundle_id, revision_id| {
        stored_source_with_ids("", unit_id, bundle_id, revision_id)
    };
    let empty_catalogue = |revision| {
        CatalogueSnapshot::new_with_types(revision, Vec::new(), Vec::new(), Vec::new(), Vec::new())
            .unwrap()
    };
    let source_unit = SourceUnitId::from_bytes([0x91; 16]);
    let source_bundle = SourceBundleId::from_bytes([0x92; 16]);
    let source_revision = SourceRevisionId::from_bytes([0x93; 16]);
    let application_catalogue = CatalogueRevisionId::from_bytes([0x94; 16]);
    let cases = [
        (
            version_one_active_with_source(
                safe_source(
                    verified.source().units()[0].id(),
                    verified.source().bundle(),
                    verified.source().id(),
                ),
                empty_catalogue(verified.catalogue().revision()),
                Vec::new(),
            ),
            crate::StandardUpgradeIdentity::CatalogueRevision(verified.catalogue().revision()),
        ),
        (
            version_one_active_with_source(
                safe_source(
                    verified.source().units()[0].id(),
                    verified.source().bundle(),
                    verified.source().id(),
                ),
                empty_catalogue(application_catalogue),
                Vec::new(),
            ),
            crate::StandardUpgradeIdentity::SourceBundle(verified.source().bundle()),
        ),
        (
            version_one_active_with_source(
                safe_source(
                    verified.source().units()[0].id(),
                    source_bundle,
                    verified.source().id(),
                ),
                empty_catalogue(application_catalogue),
                Vec::new(),
            ),
            crate::StandardUpgradeIdentity::SourceRevision(verified.source().id()),
        ),
        (
            version_one_active_with_source(
                safe_source(
                    verified.source().units()[0].id(),
                    source_bundle,
                    source_revision,
                ),
                empty_catalogue(application_catalogue),
                Vec::new(),
            ),
            crate::StandardUpgradeIdentity::SourceUnit(verified.source().units()[0].id()),
        ),
    ];

    for (active, expected) in cases {
        let error = prepare_checked_standard_upgrade_with_allocator(
            &standard,
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PrepareStandardUpgradeError::ReservedIdentity { identity } if identity == expected
        ));
        assert_no_standard_upgrade_allocations();
    }

    let schema_source = "CREATE SCHEMA app;CREATE TYPE app.schema_first AS OBJECT ();";
    let schema_id = verified.catalogue().schemas()[0].id();
    let schema_type = verified.catalogue().value_types()[0].id();
    let schema_type_start = "CREATE SCHEMA app;".len() as u32;
    let schema_catalogue = CatalogueSnapshot::new(
        application_catalogue,
        vec![SchemaDefinition::new(schema_id, semantic_name(["app"]))],
        vec![ObjectTypeDefinition::new(
            schema_type,
            semantic_name(["app", "schema_first"]),
            Vec::new(),
        )],
    )
    .unwrap();
    let schema_active = version_one_active_with_source(
        stored_source_with_ids(schema_source, source_unit, source_bundle, source_revision),
        schema_catalogue,
        vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(schema_id),
                SourceOrigin::new(source_unit, 0, schema_type_start).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(schema_type),
                SourceOrigin::new(source_unit, schema_type_start, schema_source.len() as u32)
                    .unwrap(),
            ),
        ],
    );
    let schema_error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &schema_active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert!(matches!(
        schema_error,
        PrepareStandardUpgradeError::ReservedIdentity {
            identity: crate::StandardUpgradeIdentity::Schema(id),
        } if id == schema_id
    ));
    assert_no_standard_upgrade_allocations();

    let type_source = "CREATE SCHEMA app;CREATE TYPE app.item AS OBJECT ();";
    let application_schema = SchemaId::from_bytes([0x95; 16]);
    let type_id = verified.catalogue().value_types()[0].id();
    let type_start = "CREATE SCHEMA app;".len() as u32;
    let type_catalogue = CatalogueSnapshot::new(
        application_catalogue,
        vec![SchemaDefinition::new(
            application_schema,
            semantic_name(["app"]),
        )],
        vec![ObjectTypeDefinition::new(
            type_id,
            semantic_name(["app", "item"]),
            Vec::new(),
        )],
    )
    .unwrap();
    let type_active = version_one_active_with_source(
        stored_source_with_ids(type_source, source_unit, source_bundle, source_revision),
        type_catalogue,
        vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(application_schema),
                SourceOrigin::new(source_unit, 0, type_start).unwrap(),
            ),
            DefinitionOrigin::new(
                DefinitionIdentity::ObjectType(type_id),
                SourceOrigin::new(source_unit, type_start, type_source.len() as u32).unwrap(),
            ),
        ],
    );
    let type_error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &type_active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert!(matches!(
        type_error,
        PrepareStandardUpgradeError::ReservedIdentity {
            identity: crate::StandardUpgradeIdentity::Type(id),
        } if id == type_id
    ));
    assert_no_standard_upgrade_allocations();

    let binding_source = "CREATE SCHEMA app;CREATE TYPE app.flag AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE app.flag TO PRELUDE AS BOOLEAN;";
    let binding_schema = SchemaId::from_bytes([0x96; 16]);
    let binding_type = TypeId::from_bytes([0x97; 16]);
    let binding_value = ValueTypeDefinition::primitive(
        binding_type,
        semantic_name(["app", "flag"]),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let binding = TypeBinding::prelude(
        PreludeTypeName::new(["boolean"]).unwrap(),
        binding_value.id(),
    )
    .unwrap();
    let binding_start = binding_source.find("EXPORT TYPE").unwrap() as u32;
    let binding_catalogue = CatalogueSnapshot::new_with_types(
        application_catalogue,
        vec![SchemaDefinition::new(
            binding_schema,
            semantic_name(["app"]),
        )],
        Vec::new(),
        vec![binding_value],
        vec![binding.clone()],
    )
    .unwrap();
    let binding_origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(binding_schema),
            SourceOrigin::new(source_unit, 0, type_start).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(binding_type),
            SourceOrigin::new(source_unit, type_start, binding_start).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::TypeBinding(binding.id()),
            SourceOrigin::new(source_unit, binding_start, binding_source.len() as u32).unwrap(),
        ),
    ];
    let binding_source_revision =
        stored_source_with_ids(binding_source, source_unit, source_bundle, source_revision);
    let binding_context = CatalogueHashContext::version_two(verified.clone());
    let binding_hash = catalogue_digest_with_context(
        &binding_context,
        &binding_catalogue,
        &[],
        &[],
        &binding_origins,
        &[],
    )
    .unwrap();
    let binding_active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(binding_source_revision.id(), binding_catalogue.revision()),
            binding_source_revision.clone(),
            binding_catalogue,
            binding_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), binding_origins, Vec::new()),
        ),
        binding_context.clone(),
    )
    .unwrap();
    assert_eq!(
        crate::prepare::active_reserved_standard_identity(&standard, &binding_active),
        Some(crate::StandardUpgradeIdentity::TypeBinding(binding.id()))
    );

    let binding_type_collision = ValueTypeDefinition::primitive(
        type_id,
        semantic_name(["app", "type_first"]),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let binding_after_type = TypeBinding::prelude(
        PreludeTypeName::new(["boolean"]).unwrap(),
        binding_type_collision.id(),
    )
    .unwrap();
    let type_before_binding_catalogue = CatalogueSnapshot::new_with_types(
        application_catalogue,
        vec![SchemaDefinition::new(
            binding_schema,
            semantic_name(["app"]),
        )],
        Vec::new(),
        vec![binding_type_collision],
        vec![binding_after_type.clone()],
    )
    .unwrap();
    let type_before_binding_origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(binding_schema),
            SourceOrigin::new(source_unit, 0, type_start).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(type_id),
            SourceOrigin::new(source_unit, type_start, binding_start).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::TypeBinding(binding_after_type.id()),
            SourceOrigin::new(source_unit, binding_start, binding_source.len() as u32).unwrap(),
        ),
    ];
    let type_before_binding_hash = catalogue_digest_with_context(
        &binding_context,
        &type_before_binding_catalogue,
        &[],
        &[],
        &type_before_binding_origins,
        &[],
    )
    .unwrap();
    let type_before_binding_active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(
                binding_source_revision.id(),
                type_before_binding_catalogue.revision(),
            ),
            binding_source_revision,
            type_before_binding_catalogue,
            type_before_binding_hash,
            ActiveRevisionContent::new(
                Vec::new(),
                Vec::new(),
                type_before_binding_origins,
                Vec::new(),
            ),
        ),
        binding_context,
    )
    .unwrap();
    assert_eq!(
        crate::prepare::active_reserved_standard_identity(&standard, &type_before_binding_active),
        Some(crate::StandardUpgradeIdentity::Type(type_id))
    );
}

#[test]
fn standard_upgrade_maps_reachable_context_contract_failures_before_source_work() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let active = version_one_active_with(
        "CREATE SCHEMA ;",
        CatalogueSnapshot::new(
            CatalogueRevisionId::from_bytes([0xb3; 16]),
            Vec::new(),
            Vec::new(),
        )
        .unwrap(),
    );
    let unsupported = crate::resolver::checked_standard_library_with_contract_overrides_for_test(
        &verified,
        &[(0, "unsupported@1")],
    )
    .unwrap();
    let duplicate = crate::resolver::checked_standard_library_with_contract_overrides_for_test(
        &verified_canonical_standard_source_fixture(),
        &[(1, "orna.kernel.value.boolean@1")],
    )
    .unwrap();
    let cases = [
        (
            &unsupported,
            StandardApplicationContextError::UnsupportedCompatibilityContract {
                type_id: TypeId::from_bytes([3; 16]),
                contract: "unsupported@1".to_owned(),
            },
        ),
        (
            &duplicate,
            StandardApplicationContextError::CompatibilityContractConflict {
                contract: "orna.kernel.value.boolean@1".to_owned(),
            },
        ),
    ];

    for (standard, expected) in cases {
        let error = prepare_checked_standard_upgrade_with_allocator(
            standard,
            &active,
            retrying_standard_allocator(standard.verified_snapshot()),
        )
        .unwrap_err();

        assert!(matches!(
            &error,
            PrepareStandardUpgradeError::Context { source } if source == &expected
        ));
        assert_eq!(
            error.to_string(),
            format!("the checked standard library cannot form an application context: {expected}")
        );
        assert!(std::error::Error::source(&error).is_some());
        assert_no_standard_upgrade_allocations();
    }
}

#[test]
fn standard_upgrade_returns_parser_diagnostics_before_active_source_matching() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0xb2; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let active = version_one_active_with("CREATE SCHEMA ;", catalogue);
    let expected = parse_bundle(
        &SourceBundle::new([SourceUnit::new("active.orna", "CREATE SCHEMA ;")]).unwrap(),
    )
    .diagnostics()
    .to_vec();

    let error = prepare_checked_standard_upgrade_with_allocator(
        &standard,
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        PrepareStandardUpgradeError::ActiveSourceDiagnostics { .. }
    ));
    if let PrepareStandardUpgradeError::ActiveSourceDiagnostics { diagnostics } = &error {
        assert_eq!(diagnostics.as_slice(), expected.as_slice());
    }
    assert_no_standard_upgrade_allocations();
}
