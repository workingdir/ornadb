use super::*;
#[test]
fn exposes_the_standard_application_preparation_interface() {
    let _: fn(
        &StandardApplicationCheckReport,
        RevisionPair,
        &ActiveDatabaseRevision,
    ) -> Result<DeployableRevision, PrepareStandardApplicationError> = prepare_standard_application;
}

#[test]
fn prepares_a_standard_backed_server_only_application_as_version_two() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle =
        SourceBundle::new([SourceUnit::new("application.orna", "CREATE SCHEMA app;")]).unwrap();
    let report = check_standard_application(&bundle, &context);

    assert!(report.diagnostics().is_empty());
    assert!(report.checked_bundle().is_some());

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();

    assert_eq!(
        prepared.catalogue_hash_context().version(),
        CatalogueHashVersion::Version2
    );
    assert_eq!(
        prepared
            .catalogue_hash_context()
            .standard()
            .unwrap()
            .digest(),
        verified.digest()
    );
    assert_eq!(prepared.current_function_revisions(), Some([].as_slice()));
    assert_eq!(prepared.candidate().schemas().len(), 1);
}

#[test]
fn prepares_nullable_and_required_unique_text_fields_as_version_two_values() {
    let verified = verified_canonical_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA crm; CREATE TYPE crm.contact AS OBJECT (email TEXT UNIQUE, name TEXT NOT NULL UNIQUE);",
        )])
        .unwrap();

    let report = check_standard_application(&bundle, &context);
    assert!(report.diagnostics().is_empty());

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let contact = prepared
        .candidate()
        .object_type_by_name(&semantic_name(["crm", "contact"]))
        .unwrap();
    let email = contact.field_by_name("email").unwrap();
    let name = contact.field_by_name("name").unwrap();
    let text = ResolvedType::Value(TypeId::from_bytes(CANONICAL_TYPE_IDS[5]));

    assert_eq!(email.resolved_type(), text);
    assert!(email.nullable());
    assert!(email.unique());
    assert_eq!(name.resolved_type(), text);
    assert!(!name.nullable());
    assert!(name.unique());
}

#[test]
fn prepares_a_unique_text_selected_server_plan_from_the_canonical_standard_library() {
    let verified = verified_canonical_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "application.orna",
        "CREATE SCHEMA crm; \
             CREATE TYPE crm.contact AS OBJECT (email TEXT UNIQUE, name TEXT NOT NULL); \
             CREATE SERVER FUNCTION crm.by_email(p_email TEXT) \
             RETURNS ROWS (contact REF crm.contact, name TEXT) \
             SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE \
             AS SELECT REF(selected), selected.name FROM crm.contact selected \
             WHERE selected.email = p_email;",
    )])
    .unwrap();

    let report = check_standard_application(&bundle, &context);
    assert!(report.diagnostics().is_empty());
    assert!(report.checked_bundle().is_some());

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let candidate = prepared.candidate();
    let contact = candidate
        .object_type_by_name(&semantic_name(["crm", "contact"]))
        .unwrap();
    let email = contact.field_by_name("email").unwrap();
    let function = &candidate.functions()[0];
    let parameter = &function.parameters()[0];
    let revision = &prepared.new_function_revisions()[0];

    assert_eq!(function.domain(), FunctionDomain::Server);
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
    assert_eq!(revision.artifact().format(), "orna.server-plan");
    assert_eq!(revision.artifact().version(), 4);
    let plan = UniqueTextSelectedServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.scan().object_type, contact.id());
    assert_eq!(
        plan.selector(),
        &SelectBindValue::Text {
            scan_object_type: contact.id(),
            field_owner: contact.id(),
            field: email.id(),
            parameter_owner: function.id(),
            parameter: parameter.id(),
            resolved_type: ResolvedType::Value(TypeId::from_bytes(CANONICAL_TYPE_IDS[5])),
            field_nullable: true,
            parameter_required_non_null: true,
        }
    );
}

#[test]
fn prepares_a_checked_client_boolean_constant() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    assert_eq!(standard.value_types().len(), 1);
    assert_ne!(
        standard.value_types()[0].id(),
        TypeId::from_bytes(CANONICAL_TYPE_IDS[0]),
        "the fixture must retain a self-consistent non-golden Boolean identity"
    );
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source =
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);

    assert!(report.diagnostics().is_empty());
    assert!(report.checked_bundle().is_some());
    let prepared = prepare_standard_application_with_allocator(
        &report,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap();
    assert_eq!(prepared.candidate().functions().len(), 1);
    let function = &prepared.candidate().functions()[0];
    assert_eq!(function.name().to_string(), "app.enabled");
    assert_eq!(function.domain(), FunctionDomain::Client);
    assert_eq!(function.parameters(), []);
    assert_eq!(
        function.return_type(),
        &FunctionReturn::Single(ResolvedType::Value(TypeId::from_bytes([3; 16])))
    );
    assert_eq!(prepared.new_function_revisions().len(), 1);
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.function(), function.id());
    assert_eq!(function.current_revision(), revision.id());
    assert_eq!(
        revision.semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
    assert_eq!(revision.language_version(), "orna.language/1");
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Client);
    assert_eq!(revision.artifact().format(), "orna.client-plan");
    assert_eq!(revision.artifact().version(), 1);
    assert_eq!(
        revision.artifact().payload(),
        b"ORNACP\0\0\0\0\0\x01\x01\x01"
    );
    assert_eq!(prepared.references().len(), 1);
    let reference = &prepared.references()[0];
    assert_eq!(reference.source_function(), function.id());
    assert_eq!(reference.source_revision(), revision.id());
    assert_eq!(reference.ordinal(), 0);
    assert_eq!(reference.kind(), DefinitionReferenceKind::NamedType);
    assert_eq!(
        reference.target(),
        DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
    );
    let return_start = source.find("BOOLEAN");
    assert!(return_start.is_some());
    let return_start = return_start.unwrap_or_default();
    assert_eq!(
        reference.source_origin().byte_start(),
        u32::try_from(return_start).unwrap_or_default()
    );
    assert_eq!(
        reference.source_origin().byte_end(),
        u32::try_from(return_start + "BOOLEAN".len()).unwrap_or_default()
    );
    assert_eq!(
        reference.source_origin().source_unit(),
        prepared.source().units()[0].id()
    );

    let declaration_start = source.find("CREATE CLIENT FUNCTION").unwrap_or_default();
    let client_origins = prepared
        .origins()
        .iter()
        .filter(|origin| origin.identity() == DefinitionIdentity::Function(function.id()))
        .collect::<Vec<_>>();
    assert_eq!(client_origins.len(), 1);
    assert_eq!(
        client_origins[0].source(),
        SourceOrigin::new(
            prepared.source().units()[0].id(),
            u32::try_from(declaration_start).unwrap_or_default(),
            u32::try_from(source.len()).unwrap_or_default(),
        )
        .unwrap(),
    );
    assert!(prepared.origins().iter().all(|origin| {
        !matches!(
            origin.identity(),
            DefinitionIdentity::Parameter { owner, .. }
                | DefinitionIdentity::FunctionReturnColumn { owner, .. }
                if owner == function.id()
        )
    }));
}

#[test]
fn standard_preparation_reuses_the_lowest_historical_client_true_revision() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let true_source =
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let false_source =
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN FALSE;";
    let true_bundle =
        SourceBundle::new([SourceUnit::new("application.orna", true_source)]).unwrap();
    let false_bundle =
        SourceBundle::new([SourceUnit::new("application.orna", false_source)]).unwrap();

    let initial = empty_version_two_active(&verified);
    let initial_context =
        StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let first_true_report = check_standard_application(&true_bundle, &initial_context);
    assert_eq!(first_true_report.diagnostics(), &[]);
    let first_true =
        prepare_standard_application(&first_true_report, initial.pair(), &initial).unwrap();
    assert_eq!(first_true.new_function_revisions().len(), 1);
    let true_revision = first_true.new_function_revisions()[0].clone();
    assert_eq!(
        true_revision.semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
    assert_eq!(
        first_true.references()[0].target(),
        DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
    );
    assert_eq!(
        true_revision.artifact().payload(),
        b"ORNACP\0\0\0\0\0\x01\x01\x01"
    );

    let true_active = active_from_prepared_standard_candidate(&first_true, Vec::new());
    let true_context =
        StandardApplicationCheckContext::try_new(true_active.catalogue(), &standard).unwrap();
    let false_report = check_standard_application(&false_bundle, &true_context);
    assert_eq!(false_report.diagnostics(), &[]);
    let false_prepared =
        prepare_standard_application(&false_report, true_active.pair(), &true_active).unwrap();
    assert_eq!(false_prepared.new_function_revisions().len(), 1);
    assert_ne!(
        false_prepared.new_function_revisions()[0].id(),
        true_revision.id()
    );
    assert_eq!(
        false_prepared.references()[0].target(),
        DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
    );
    assert_ne!(
        false_prepared.new_function_revisions()[0].semantic_hash(),
        true_revision.semantic_hash()
    );

    let false_active =
        active_from_prepared_standard_candidate(&false_prepared, vec![true_revision.clone()]);
    let false_context =
        StandardApplicationCheckContext::try_new(false_active.catalogue(), &standard).unwrap();
    let reused_true_report = check_standard_application(&true_bundle, &false_context);
    assert_eq!(reused_true_report.diagnostics(), &[]);
    let reused_true =
        prepare_standard_application(&reused_true_report, false_active.pair(), &false_active)
            .unwrap();

    assert_eq!(reused_true.new_function_revisions(), &[]);
    let current_revisions = reused_true.current_function_revisions();
    assert!(current_revisions.is_some());
    let current_revisions = current_revisions.unwrap_or_default();
    assert_eq!(current_revisions.len(), 1);
    assert_eq!(current_revisions[0].id(), true_revision.id());
    assert_eq!(current_revisions[0].revision_number(), 1);
    assert_eq!(
        current_revisions[0].artifact().payload(),
        b"ORNACP\0\0\0\0\0\x01\x01\x01"
    );
    assert_eq!(
        current_revisions[0].semantic_hash(),
        true_revision.semantic_hash()
    );
    let function = &reused_true.candidate().functions()[0];
    assert_eq!(function.id(), true_revision.function());
    assert_eq!(function.current_revision(), true_revision.id());
    assert_eq!(reused_true.references().len(), 1);
    assert_eq!(reused_true.references()[0].source_function(), function.id());
    assert_eq!(
        reused_true.references()[0].source_revision(),
        true_revision.id()
    );
}

#[test]
fn standard_preparation_reuses_client_boolean_across_formatting_and_spelling() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let initial = empty_version_two_active(&verified);
    let initial_context =
        StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let canonical = SourceBundle::new([SourceUnit::new(
        "canonical.orna",
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
    )])
    .unwrap();
    let canonical_report = check_standard_application(&canonical, &initial_context);
    assert_eq!(canonical_report.diagnostics(), &[]);
    let initial_prepared =
        prepare_standard_application(&canonical_report, initial.pair(), &initial).unwrap();
    let initial_revision = initial_prepared.new_function_revisions()[0].clone();
    let active = active_from_prepared_standard_candidate(&initial_prepared, Vec::new());
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let equivalent = SourceBundle::new([SourceUnit::new(
            "formatted.orna",
            "CREATE SCHEMA app;\n\nCREATE CLIENT FUNCTION app.enabled() RETURNS std.BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
    let report = check_standard_application(&equivalent, &context);
    assert_eq!(report.diagnostics(), &[]);
    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();

    assert_eq!(prepared.new_function_revisions(), &[]);
    let current = prepared.current_function_revisions().unwrap_or_default();
    assert_eq!(current, [initial_revision]);
    assert_eq!(prepared.references().len(), 1);
    assert_eq!(
        prepared.references()[0].target(),
        DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16]))
    );
}

fn assert_no_standard_preparation_allocations() {
    for counter in [
        &PREPARE_CATALOGUE_ALLOCATIONS,
        &PREPARE_BUNDLE_ALLOCATIONS,
        &PREPARE_REVISION_ALLOCATIONS,
        &PREPARE_UNIT_ALLOCATIONS,
        &PREPARE_SCHEMA_ALLOCATIONS,
        &PREPARE_TYPE_ALLOCATIONS,
    ] {
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "CLIENT Gate 11 must reject before allocating a candidate identity"
        );
    }
}

fn assert_client_gate_eleven_failure(error: PrepareStandardApplicationError, reason: &'static str) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::Prepare {
            source: PrepareError::InvalidCheckedBundle { .. }
        }
    ));
    if let PrepareStandardApplicationError::Prepare { source } = &error {
        assert!(matches!(
            source,
            PrepareError::InvalidCheckedBundle { reason: actual } if *actual == reason
        ));
        assert_eq!(source.to_string(), reason);
    }
    assert_eq!(
        error.to_string(),
        format!("the standard application could not be prepared: {reason}")
    );
    assert!(std::error::Error::source(&error).is_some());
}

fn assert_existing_function_mismatch(error: PrepareStandardApplicationError, expected: FunctionId) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::Prepare {
            source: PrepareError::ExistingDefinitionMismatch {
                definition: DefinitionIdentity::Function(id),
            }
        } if *id == expected
    ));
    assert_eq!(
        error.to_string(),
        "the standard application could not be prepared: existing checked definition differs from active catalogue"
    );
    assert!(std::error::Error::source(&error).is_some());
}

#[derive(Clone, Copy)]
enum HostileClientFact {
    Domain,
    Parameter,
    Return,
    Security,
    Transaction,
    Volatility,
    Body,
    Reference,
}

impl HostileClientFact {
    const fn reason(self) -> &'static str {
        match self {
            Self::Domain => "checked CLIENT function has an unsupported domain",
            Self::Parameter => "checked CLIENT function declares parameters",
            Self::Return => {
                "checked CLIENT function does not return BOOLEAN from the checked standard library"
            }
            Self::Security => "checked CLIENT function has an unsupported security mode",
            Self::Transaction => "checked CLIENT function has an unsupported transaction mode",
            Self::Volatility => "checked CLIENT function has an unsupported volatility mode",
            Self::Body => "checked CLIENT function has an unsupported body",
            Self::Reference => {
                "checked CLIENT function contains unsupported application definition references"
            }
        }
    }

    fn apply(self, report: &mut StandardApplicationCheckReport) -> bool {
        match self {
            Self::Domain => report.replace_first_client_domain_for_test(FunctionDomain::Server),
            Self::Parameter => report.append_first_client_parameter_for_test(),
            Self::Return => report.replace_first_client_return_with_integer_for_test(),
            Self::Security => {
                report.replace_first_client_security_for_test(FunctionSecurity::Definer)
            }
            Self::Transaction => {
                report.replace_first_client_transaction_for_test(Some(FunctionTransaction::Atomic))
            }
            Self::Volatility => {
                report.replace_first_client_volatility_for_test(FunctionVolatility::Stable)
            }
            Self::Body => report.replace_first_client_body_with_unsupported_for_test(),
            Self::Reference => report.append_first_client_reference_for_test(),
        }
    }
}

#[test]
fn standard_preparation_rejects_every_client_gate_eleven_fact_before_allocation() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "application.orna",
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    for fact in [
        HostileClientFact::Domain,
        HostileClientFact::Parameter,
        HostileClientFact::Return,
        HostileClientFact::Security,
        HostileClientFact::Transaction,
        HostileClientFact::Volatility,
        HostileClientFact::Body,
        HostileClientFact::Reference,
    ] {
        let mut hostile = report.clone();
        let changed = fact.apply(&mut hostile);
        assert!(changed);

        let error = prepare_standard_application_with_allocator(
            &hostile,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_client_gate_eleven_failure(error, fact.reason());
        assert_no_standard_preparation_allocations();
    }
}

#[test]
fn standard_preparation_orders_every_adjacent_client_gate_eleven_pair_before_allocation() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "application.orna",
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let facts = [
        HostileClientFact::Domain,
        HostileClientFact::Parameter,
        HostileClientFact::Return,
        HostileClientFact::Security,
        HostileClientFact::Transaction,
        HostileClientFact::Volatility,
        HostileClientFact::Body,
        HostileClientFact::Reference,
    ];

    for pair in facts.windows(2) {
        let mut hostile = report.clone();
        assert!(pair[0].apply(&mut hostile));
        assert!(pair[1].apply(&mut hostile));
        let error = prepare_standard_application_with_allocator(
            &hostile,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_client_gate_eleven_failure(error, pair[0].reason());
        assert_no_standard_preparation_allocations();
    }
}

#[test]
fn standard_preparation_orders_gate_ten_and_common_preflight_before_client_semantics() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "application.orna",
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    let mut gate_ten = report.clone();
    assert!(gate_ten.replace_standard_type_reference_for_test(
        0,
        CheckedFunctionId::Existing(FunctionId::from_bytes([0xc1; 16])),
        0,
        TypeId::from_bytes([3; 16]),
        report.checked_bundle().unwrap().standard_type_references()[0]
            .location()
            .clone(),
    ));
    assert!(gate_ten.replace_first_client_body_with_unsupported_for_test());
    let error = prepare_standard_application_with_allocator(
        &gate_ten,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_function_reference_evidence_mismatch(
        error,
        report.checked_bundle().unwrap().standard_type_references()[0].owner(),
    );
    assert_no_standard_preparation_allocations();

    let retained_owner = CheckedFunctionId::Existing(FunctionId::from_bytes([0xc2; 16]));
    let mut retained_client_return = report.clone();
    assert!(retained_client_return.replace_first_client_id_for_test(retained_owner));
    assert!(retained_client_return.replace_first_client_body_with_unsupported_for_test());
    let error = prepare_standard_application_with_allocator(
        &retained_client_return,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_function_reference_evidence_mismatch(error, retained_owner);
    assert_no_standard_preparation_allocations();

    let mut common_preflight = report.clone();
    assert!(common_preflight.replace_first_client_location_for_test(
        crate::SourceLocation::from_syntax(
            "missing.orna",
            &orna_syntax::SourceSpan { start: 0, end: 1 },
        ),
    ));
    assert!(common_preflight.replace_first_client_body_with_unsupported_for_test());
    let error = prepare_standard_application_with_allocator(
        &common_preflight,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::Prepare {
            source: PrepareError::InvalidSourceLocation {
                logical_path,
                byte_start: 0,
                byte_end: 1,
            }
        } if logical_path == "missing.orna"
    ));
    assert_eq!(
        error.to_string(),
        "the standard application could not be prepared: checked source location is invalid"
    );
    assert!(std::error::Error::source(&error).is_some());
    assert_no_standard_preparation_allocations();
}

#[test]
fn standard_preparation_materialises_exact_client_return_evidence_at_gate_ten() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "application.orna",
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let client = checked.client_functions().next().unwrap();
    let owner = client.id();
    let reference = checked.standard_type_references()[0].clone();
    let return_location = reference.location().clone();

    let mut hostile_cases = Vec::new();
    let mut missing = report.clone();
    assert!(missing.replace_standard_type_references_for_test(Vec::new()));
    hostile_cases.push((missing, owner));

    let mut extra = report.clone();
    assert!(extra
        .replace_standard_type_references_for_test(vec![reference.clone(), reference.clone(),]));
    hostile_cases.push((extra, owner));

    let mut wrong_owner = report.clone();
    assert!(wrong_owner.replace_standard_type_reference_for_test(
        0,
        CheckedFunctionId::Existing(FunctionId::from_bytes([0xc3; 16])),
        0,
        reference.target(),
        return_location.clone(),
    ));
    hostile_cases.push((wrong_owner, owner));

    let mut wrong_ordinal = report.clone();
    assert!(wrong_ordinal.replace_standard_type_reference_for_test(
        0,
        owner,
        1,
        reference.target(),
        return_location.clone(),
    ));
    hostile_cases.push((wrong_ordinal, owner));

    let mut wrong_target = report.clone();
    assert!(wrong_target.replace_standard_type_reference_for_test(
        0,
        owner,
        0,
        TypeId::from_bytes([0xc4; 16]),
        return_location.clone(),
    ));
    hostile_cases.push((wrong_target, owner));

    let mut wrong_reference_location = report.clone();
    assert!(
        wrong_reference_location.replace_standard_type_reference_for_test(
            0,
            owner,
            0,
            reference.target(),
            crate::SourceLocation::from_syntax(
                "other.orna",
                &orna_syntax::SourceSpan { start: 0, end: 1 },
            ),
        )
    );
    hostile_cases.push((wrong_reference_location, owner));

    let mut wrong_class = report.clone();
    assert!(
        wrong_class.replace_first_client_return_kind_for_test(CheckedTypeUseKind::Parameter {
            owner,
            parameter: CheckedParameterId::Existing(orna_core::ParameterId::from_bytes([0xc5; 16])),
        },)
    );
    hostile_cases.push((wrong_class, owner));

    let mut wrong_kind_ordinal = report.clone();
    assert!(
        wrong_kind_ordinal.replace_first_client_return_kind_for_test(CheckedTypeUseKind::Return {
            owner,
            ordinal: 1
        },)
    );
    hostile_cases.push((wrong_kind_ordinal, owner));

    let mut wrong_retained_target = report.clone();
    assert!(wrong_retained_target
        .replace_first_client_return_type_id_for_test(TypeId::from_bytes([0xc6; 16])));
    hostile_cases.push((wrong_retained_target, owner));

    let mut wrong_retained_location = report.clone();
    assert!(
        wrong_retained_location.replace_first_client_return_use_location_for_test(
            crate::SourceLocation::from_syntax(
                "other.orna",
                &orna_syntax::SourceSpan { start: 0, end: 1 },
            ),
        )
    );
    hostile_cases.push((wrong_retained_location, owner));

    for (hostile, expected_owner) in hostile_cases {
        let error = prepare_standard_application_with_allocator(
            &hostile,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert_function_reference_evidence_mismatch(error, expected_owner);
        assert_no_standard_preparation_allocations();
    }

    let two_clients = SourceBundle::new([SourceUnit::new(
        "two-clients.orna",
        "CREATE SCHEMA app; \
             CREATE CLIENT FUNCTION app.first() RETURNS BOOLEAN RETURN TRUE; \
             CREATE CLIENT FUNCTION app.second() RETURNS BOOLEAN RETURN FALSE;",
    )])
    .unwrap();
    let ordered_report = check_standard_application(&two_clients, &context);
    assert_eq!(ordered_report.diagnostics(), &[]);
    let ordered = ordered_report.checked_bundle().unwrap();
    let expected_owner = ordered.standard_type_references()[0].owner();
    let mut reordered = ordered_report.clone();
    let mut references = ordered.standard_type_references().to_vec();
    references.swap(0, 1);
    assert!(reordered.replace_standard_type_references_for_test(references));
    let error = prepare_standard_application_with_allocator(
        &reordered,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_function_reference_evidence_mismatch(error, expected_owner);
    assert_no_standard_preparation_allocations();
}

#[test]
fn standard_preparation_validates_every_gate_eleven_location_in_nested_order() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (flag BOOLEAN NOT NULL DEFAULT TRUE); \
            CREATE SERVER FUNCTION app.read(p_ref REF app.item) RETURNS ROWS (item REF app.item) \
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT REF(i) FROM app.item i WHERE REF(i) = p_ref; \
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let invalid_location = crate::SourceLocation::from_syntax(
        "missing.orna",
        &orna_syntax::SourceSpan { start: 0, end: 1 },
    );

    for selector in [
        "schema",
        "object",
        "field",
        "default",
        "server",
        "server parameter",
        "server return",
        "server reference",
        "client",
        "client parameter",
        "client return",
        "client body",
        "client reference",
    ] {
        let mut hostile = report.clone();
        if selector == "client parameter" {
            assert!(hostile.append_first_client_parameter_for_test());
        }
        if selector == "client reference" {
            assert!(hostile.append_first_client_reference_for_test());
        }
        assert!(hostile
            .replace_standard_preparation_location_for_test(selector, invalid_location.clone(),));
        let error = prepare_standard_application_with_allocator(
            &hostile,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::Prepare {
                source: PrepareError::InvalidSourceLocation {
                    logical_path,
                    byte_start: 0,
                    byte_end: 1,
                }
            } if logical_path == "missing.orna"
        ));
        assert_eq!(
            error.to_string(),
            "the standard application could not be prepared: checked source location is invalid"
        );
        assert!(std::error::Error::source(&error).is_some());
        assert_no_standard_preparation_allocations();
    }

    let selectors = [
        "schema",
        "object",
        "field",
        "default",
        "server",
        "server parameter",
        "server return",
        "server reference",
        "client",
        "client parameter",
        "client return",
        "client body",
        "client reference",
    ];
    for (index, pair) in selectors.windows(2).enumerate() {
        let mut hostile = report.clone();
        if pair.contains(&"client parameter") {
            assert!(hostile.append_first_client_parameter_for_test());
        }
        if pair.contains(&"client reference") {
            assert!(hostile.append_first_client_reference_for_test());
        }
        let first_location = crate::SourceLocation::from_syntax(
            "first-missing.orna",
            &orna_syntax::SourceSpan {
                start: index,
                end: index + 1,
            },
        );
        let second_location = crate::SourceLocation::from_syntax(
            "second-missing.orna",
            &orna_syntax::SourceSpan {
                start: index + 20,
                end: index + 21,
            },
        );
        assert!(hostile.replace_standard_preparation_location_for_test(pair[0], first_location,));
        assert!(hostile.replace_standard_preparation_location_for_test(pair[1], second_location,));
        let error = prepare_standard_application_with_allocator(
            &hostile,
            active.pair(),
            &active,
            retrying_standard_allocator(&verified),
        )
        .unwrap_err();
        assert!(matches!(
            &error,
            PrepareStandardApplicationError::Prepare {
                source: PrepareError::InvalidSourceLocation {
                    logical_path,
                    byte_start,
                    byte_end,
                }
            } if logical_path == "first-missing.orna"
                && *byte_start == index
                && *byte_end == index + 1
        ));
        assert_eq!(
            error.to_string(),
            "the standard application could not be prepared: checked source location is invalid"
        );
        assert!(std::error::Error::source(&error).is_some());
        assert_no_standard_preparation_allocations();
    }

    let mut nested_precedence = report;
    assert!(nested_precedence
        .replace_standard_preparation_location_for_test("schema", invalid_location.clone(),));
    assert!(nested_precedence.replace_first_client_body_with_unsupported_for_test());
    let error = prepare_standard_application_with_allocator(
        &nested_precedence,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::Prepare {
            source: PrepareError::InvalidSourceLocation { logical_path, .. }
        } if logical_path == "missing.orna"
    ));
    assert_no_standard_preparation_allocations();
}

#[test]
fn standard_preparation_orders_server_continuity_client_order_and_owner_completeness() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL); \
            CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) RETURNS ROWS (item REF app.item) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT REF(item) FROM app.item item WHERE REF(item) = p_ref; \
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let server_id = FunctionId::from_bytes([0xc2; 16]);

    let mut server_before_client = report.clone();
    assert!(server_before_client
        .replace_first_server_id_for_test(CheckedFunctionId::Existing(server_id)));
    assert!(server_before_client.replace_first_client_body_with_unsupported_for_test());
    let error = prepare_standard_application_with_allocator(
        &server_before_client,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_existing_function_mismatch(error, server_id);
    assert_no_standard_preparation_allocations();

    let checked = report.checked_bundle().unwrap();
    let client_id = checked.client_functions().next().unwrap().id();
    let mut client_semantics_before_duplicate = report.clone();
    assert!(client_semantics_before_duplicate.replace_first_server_id_for_test(client_id));
    assert!(client_semantics_before_duplicate.replace_first_client_body_with_unsupported_for_test());
    let error = prepare_standard_application_with_allocator(
        &client_semantics_before_duplicate,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_client_gate_eleven_failure(error, "checked CLIENT function has an unsupported body");
    assert_no_standard_preparation_allocations();

    let client_source = "CREATE SCHEMA app; \
            CREATE CLIENT FUNCTION app.first() RETURNS BOOLEAN RETURN TRUE; \
            CREATE CLIENT FUNCTION app.second() RETURNS BOOLEAN RETURN FALSE;";
    let client_bundle =
        SourceBundle::new([SourceUnit::new("clients.orna", client_source)]).unwrap();
    let client_report = check_standard_application(&client_bundle, &context);
    assert_eq!(client_report.diagnostics(), &[]);
    let mut first_before_second = client_report.clone();
    assert!(first_before_second.replace_first_client_body_with_unsupported_for_test());
    assert!(first_before_second.replace_client_domain_for_test(1, FunctionDomain::Server));
    let error = prepare_standard_application_with_allocator(
        &first_before_second,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_client_gate_eleven_failure(error, "checked CLIENT function has an unsupported body");
    assert_no_standard_preparation_allocations();

    let mut duplicate_domain = report.clone();
    assert!(duplicate_domain.replace_first_server_id_for_test(client_id));
    let error = prepare_standard_application_with_allocator(
        &duplicate_domain,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_client_gate_eleven_failure(error, "duplicate checked function");
    assert_no_standard_preparation_allocations();

    let server_only_source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL); \
            CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) RETURNS ROWS (item REF app.item) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT REF(item) FROM app.item item WHERE REF(item) = p_ref;";
    let server_only_bundle =
        SourceBundle::new([SourceUnit::new("server.orna", server_only_source)]).unwrap();
    let server_only = check_standard_application(&server_only_bundle, &context);
    assert_eq!(server_only.diagnostics(), &[]);
    let mut owner_mismatch = server_only.clone();
    assert!(owner_mismatch.remove_first_server_declaration_evidence_for_test());
    let error = prepare_standard_application_with_allocator(
        &owner_mismatch,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_client_gate_eleven_failure(
        error,
        "checked standard function owners do not match declaration evidence",
    );
    assert_no_standard_preparation_allocations();
}

#[test]
fn standard_preparation_validates_existing_server_parameters_before_client_semantics() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let initial = empty_version_two_active(&verified);
    let initial_context =
        StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let server_source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL); \
            CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN) \
            RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
            AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made);";
    let server_bundle = SourceBundle::new([SourceUnit::new("server.orna", server_source)]).unwrap();
    let initial_report = check_standard_application(&server_bundle, &initial_context);
    assert_eq!(initial_report.diagnostics(), &[]);
    let prepared = prepare_standard_application(&initial_report, initial.pair(), &initial).unwrap();
    let active = active_from_prepared_standard_candidate(&prepared, Vec::new());
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let mixed_source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL); \
            CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN) \
            RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
            AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made); \
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let mixed_bundle = SourceBundle::new([SourceUnit::new("mixed.orna", mixed_source)]).unwrap();
    let report = check_standard_application(&mixed_bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    let function = active.catalogue().functions()[0].id();
    let parameter = active.catalogue().functions()[0].parameters()[0].id();
    let mut hostile = report;
    assert!(hostile.replace_server_parameter_name_for_test(0, "first-renamed".to_owned()));
    assert!(hostile.replace_server_parameter_name_for_test(1, "second-renamed".to_owned()));
    assert!(hostile.replace_first_client_body_with_unsupported_for_test());
    let error = prepare_standard_application_with_allocator(
        &hostile,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::Prepare {
            source: PrepareError::ExistingDefinitionMismatch {
                definition: DefinitionIdentity::Parameter { owner, parameter: actual },
            }
        } if *owner == function && *actual == parameter
    ));
    assert_eq!(
        error.to_string(),
        "the standard application could not be prepared: existing checked definition differs from active catalogue"
    );
    assert!(std::error::Error::source(&error).is_some());
    assert_no_standard_preparation_allocations();
}

#[test]
fn standard_preparation_checks_both_active_function_domain_directions_and_name_continuity() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let initial = empty_version_two_active(&verified);
    let initial_context =
        StandardApplicationCheckContext::try_new(initial.catalogue(), &standard).unwrap();
    let client_source =
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let client_bundle = SourceBundle::new([SourceUnit::new("client.orna", client_source)]).unwrap();
    let client_report = check_standard_application(&client_bundle, &initial_context);
    assert_eq!(client_report.diagnostics(), &[]);
    let prepared_client =
        prepare_standard_application(&client_report, initial.pair(), &initial).unwrap();
    let client_active = active_from_prepared_standard_candidate(&prepared_client, Vec::new());
    let active_client_id = client_active.catalogue().functions()[0].id();

    let server_source = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL); \
            CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) RETURNS ROWS (item REF app.item) \
            TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT REF(item) FROM app.item item WHERE REF(item) = p_ref;";
    let server_bundle = SourceBundle::new([SourceUnit::new("server.orna", server_source)]).unwrap();
    let client_context =
        StandardApplicationCheckContext::try_new(client_active.catalogue(), &standard).unwrap();
    let server_report = check_standard_application(&server_bundle, &client_context);
    assert_eq!(server_report.diagnostics(), &[]);
    let mut server_as_client = server_report.clone();
    assert!(server_as_client
        .replace_first_server_id_for_test(CheckedFunctionId::Existing(active_client_id)));
    let error = prepare_standard_application_with_allocator(
        &server_as_client,
        client_active.pair(),
        &client_active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_existing_function_mismatch(error, active_client_id);
    assert_no_standard_preparation_allocations();

    let prepared_server =
        prepare_standard_application(&server_report, client_active.pair(), &client_active).unwrap();
    let server_active = active_from_prepared_standard_candidate(&prepared_server, Vec::new());
    let active_server_id = server_active.catalogue().functions()[0].id();
    let server_context =
        StandardApplicationCheckContext::try_new(server_active.catalogue(), &standard).unwrap();
    let client_report = check_standard_application(&client_bundle, &server_context);
    assert_eq!(client_report.diagnostics(), &[]);
    let mut client_as_server = client_report.clone();
    assert!(
        client_as_server.replace_first_client_id_with_evidence_for_test(
            CheckedFunctionId::Existing(active_server_id)
        )
    );
    let error = prepare_standard_application_with_allocator(
        &client_as_server,
        server_active.pair(),
        &server_active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_existing_function_mismatch(error, active_server_id);
    assert_no_standard_preparation_allocations();

    let client_context =
        StandardApplicationCheckContext::try_new(client_active.catalogue(), &standard).unwrap();
    let existing_client_report = check_standard_application(&client_bundle, &client_context);
    assert_eq!(existing_client_report.diagnostics(), &[]);
    let mut renamed_client = existing_client_report.clone();
    assert!(renamed_client.replace_first_client_name_for_test(semantic_name(["app", "renamed",])));
    let error = prepare_standard_application_with_allocator(
        &renamed_client,
        client_active.pair(),
        &client_active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_existing_function_mismatch(error, active_client_id);
    assert_no_standard_preparation_allocations();

    let ordered_client_source = "CREATE SCHEMA app; \
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE; \
            CREATE CLIENT FUNCTION app.later() RETURNS BOOLEAN RETURN FALSE;";
    let ordered_client_bundle = SourceBundle::new([SourceUnit::new(
        "ordered-clients.orna",
        ordered_client_source,
    )])
    .unwrap();
    let ordered_client_report = check_standard_application(&ordered_client_bundle, &client_context);
    assert_eq!(ordered_client_report.diagnostics(), &[]);
    let mut first_continuity_before_second = ordered_client_report.clone();
    assert!(first_continuity_before_second
        .replace_first_client_name_for_test(semantic_name(["app", "renamed",])));
    assert!(
        first_continuity_before_second.replace_client_domain_for_test(1, FunctionDomain::Server)
    );
    let error = prepare_standard_application_with_allocator(
        &first_continuity_before_second,
        client_active.pair(),
        &client_active,
        retrying_standard_allocator(&verified),
    )
    .unwrap_err();
    assert_existing_function_mismatch(error, active_client_id);
    assert_no_standard_preparation_allocations();
}

#[test]
fn standard_preparation_orders_the_first_seven_gates() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let valid_bundle =
        SourceBundle::new([SourceUnit::new("application.orna", "CREATE SCHEMA app;")]).unwrap();
    let valid_report = check_standard_application(&valid_bundle, &context);
    assert!(valid_report.diagnostics().is_empty());

    let incomplete_bundle =
        SourceBundle::new([SourceUnit::new("invalid.orna", "CREATE SCHEMA ;")]).unwrap();
    let incomplete = check_standard_application(&incomplete_bundle, &context);
    let incomplete_expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes([0xe1; 16]),
        active.pair().catalogue(),
    );
    let error =
        prepare_standard_application(&incomplete, incomplete_expected_base, &active).unwrap_err();
    assert_check_not_complete(error, incomplete.diagnostics().len());

    let mut wrong_base_after_expected = valid_report.clone();
    assert!(wrong_base_after_expected
        .replace_base_catalogue_revision_for_test(CatalogueRevisionId::from_bytes([0xe2; 16])));
    let wrong_expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes([0xe3; 16]),
        active.pair().catalogue(),
    );
    let error =
        prepare_standard_application(&wrong_base_after_expected, wrong_expected_base, &active)
            .unwrap_err();
    assert_expected_base_mismatch(error, wrong_expected_base, active.pair());

    let mut wrong_base = valid_report.clone();
    assert!(wrong_base
        .replace_base_catalogue_revision_for_test(CatalogueRevisionId::from_bytes([0xe4; 16])));
    let no_standard = empty_version_one_active();
    let error =
        prepare_standard_application(&wrong_base, no_standard.pair(), &no_standard).unwrap_err();
    assert_checked_base_mismatch(
        error,
        CatalogueRevisionId::from_bytes([0xe4; 16]),
        no_standard.pair().catalogue(),
    );

    let mut report_without_standard = valid_report.clone();
    assert!(report_without_standard
        .replace_base_catalogue_revision_for_test(no_standard.pair().catalogue()));
    let error =
        prepare_standard_application(&report_without_standard, no_standard.pair(), &no_standard)
            .unwrap_err();
    assert_standard_library_unavailable(error);

    let mut wrong_catalogue = valid_report.clone();
    assert!(wrong_catalogue.replace_standard_context_for_test(
        CatalogueRevisionId::from_bytes([0xe5; 16]),
        StandardLibraryRevisionId::from_bytes([0xe6; 16]),
        Sha256Digest::from_bytes([0xe7; 32]),
    ));
    let error = prepare_standard_application(&wrong_catalogue, active.pair(), &active).unwrap_err();
    assert_standard_catalogue_mismatch(
        error,
        CatalogueRevisionId::from_bytes([0xe5; 16]),
        verified.catalogue().revision(),
    );

    let mut wrong_revision = valid_report.clone();
    assert!(wrong_revision.replace_standard_context_for_test(
        verified.catalogue().revision(),
        StandardLibraryRevisionId::from_bytes([0xe8; 16]),
        Sha256Digest::from_bytes([0xe9; 32]),
    ));
    let error = prepare_standard_application(&wrong_revision, active.pair(), &active).unwrap_err();
    assert_standard_revision_mismatch(
        error,
        StandardLibraryRevisionId::from_bytes([0xe8; 16]),
        verified.revision(),
    );

    let mut wrong_digest = valid_report;
    assert!(wrong_digest.replace_standard_context_for_test(
        verified.catalogue().revision(),
        verified.revision(),
        Sha256Digest::from_bytes([0xea; 32]),
    ));
    let error = prepare_standard_application(&wrong_digest, active.pair(), &active).unwrap_err();
    assert_standard_digest_mismatch(
        error,
        Sha256Digest::from_bytes([0xea; 32]),
        verified.digest(),
    );
}

#[test]
fn standard_preparation_retries_reserved_candidate_ids_before_hash_construction() {
    let _allocation_lock = PREPARE_ALLOCATION_LOCK.lock().unwrap();
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "application.orna",
        "CREATE SCHEMA app; \
             CREATE TYPE app.flag AS OBJECT (value BOOLEAN NOT NULL); \
             CREATE SERVER FUNCTION app.read() RETURNS ROWS (value BOOLEAN) \
             TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT f.value FROM app.flag f;",
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    let prepared = prepare_standard_application_with_allocator(
        &report,
        active.pair(),
        &active,
        retrying_standard_allocator(&verified),
    )
    .unwrap();

    for counter in [
        &PREPARE_CATALOGUE_ALLOCATIONS,
        &PREPARE_BUNDLE_ALLOCATIONS,
        &PREPARE_REVISION_ALLOCATIONS,
        &PREPARE_UNIT_ALLOCATIONS,
        &PREPARE_SCHEMA_ALLOCATIONS,
        &PREPARE_TYPE_ALLOCATIONS,
    ] {
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }
    assert_eq!(prepared.candidate().revision().to_bytes(), [0x81; 16]);
    assert_eq!(prepared.source().bundle().to_bytes(), [0x82; 16]);
    assert_eq!(prepared.source().id().to_bytes(), [0x83; 16]);
    assert_eq!(prepared.source().units()[0].id().to_bytes(), [0x84; 16]);
    assert_eq!(
        prepared.candidate().schemas()[0].id().to_bytes(),
        [0x85; 16]
    );
    assert_eq!(
        prepared.candidate().object_types()[0].id().to_bytes(),
        [0x86; 16]
    );
}

#[test]
fn standard_preparation_keeps_reference_only_function_hashes_at_version_one() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; \
             CREATE TYPE app.flag AS OBJECT (value BOOLEAN NOT NULL); \
             CREATE SERVER FUNCTION app.read(p_flag REF app.flag) RETURNS ROWS (value REF app.flag) \
             TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT REF(f) FROM app.flag f WHERE REF(f) = p_flag;",
        )])
        .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();

    assert_eq!(prepared.new_function_revisions().len(), 1);
    assert_eq!(
        prepared.new_function_revisions()[0].semantic_hash_version(),
        orna_core::revision::FunctionSemanticHashVersion::Version1
    );
    assert!(prepared.references().iter().all(|reference| {
        !matches!(
            reference.target(),
            orna_core::revision::DefinitionReferenceTarget::ValueType(_)
        )
    }));
    assert_eq!(
        prepared.references()[..2]
            .iter()
            .map(|reference| reference.kind())
            .collect::<Vec<_>>(),
        vec![
            orna_core::revision::DefinitionReferenceKind::ObjectReference,
            orna_core::revision::DefinitionReferenceKind::ObjectReference,
        ]
    );
    assert_eq!(
        prepared.references()[..2]
            .iter()
            .map(|reference| reference.ordinal())
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn standard_preparation_retains_checked_value_type_references() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.flag AS OBJECT (value BOOLEAN NOT NULL);\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (value BOOLEAN) \
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT f.value FROM app.flag f WHERE f.value = TRUE;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);

    assert!(report.diagnostics().is_empty());
    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let standard_boolean = TypeId::from_bytes([3; 16]);
    assert_eq!(
        prepared.candidate().object_types()[0].fields()[0].resolved_type(),
        ResolvedType::Value(TypeId::from_bytes([3; 16]))
    );
    assert_eq!(
        (
            prepared.references()[0].kind(),
            prepared.references()[0].target(),
        ),
        (
            orna_core::revision::DefinitionReferenceKind::NamedType,
            orna_core::revision::DefinitionReferenceTarget::ValueType(standard_boolean),
        )
    );
    assert_eq!(
        prepared.new_function_revisions()[0].semantic_hash_version(),
        orna_core::revision::FunctionSemanticHashVersion::Version2
    );
}

#[test]
fn standard_preparation_lowers_sealed_signature_references_before_body_references() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let first_source = "CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN, p_alias std.BOOLEAN) \
             RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
             AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made);";
    let second_source = "CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) \
             RETURNS ROWS (visible std.BOOLEAN) TRANSACTION READ ONLY VOLATILITY STABLE \
             AS SELECT TRUE FROM app.item item WHERE REF(item) = p_ref;";
    let declarations_source =
        "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);";
    let bundle = SourceBundle::new([
        SourceUnit::new("z-first-server.orna", first_source),
        SourceUnit::new("a-second-server.orna", second_source),
        SourceUnit::new("m-declarations.orna", declarations_source),
    ])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let checked_bundle = report.checked_bundle();
    assert!(checked_bundle.is_some());
    let checked = checked_bundle.expect("a diagnostic-free report has a checked bundle");
    let checked_functions = checked.server_functions().collect::<Vec<_>>();
    assert_eq!(checked_functions.len(), 2);
    let standard_boolean = TypeId::from_bytes([3; 16]);
    let first_boolean = first_source.find("p_boolean BOOLEAN").unwrap() + "p_boolean ".len();
    let first_alias = first_source.find("p_alias std.BOOLEAN").unwrap() + "p_alias ".len();
    let second_boolean = second_source.find("visible std.BOOLEAN").unwrap() + "visible ".len();
    let sealed_references = checked.standard_type_references();
    assert_eq!(sealed_references.len(), 3);
    assert_eq!(
        sealed_references
            .iter()
            .map(|reference| {
                (
                    reference.owner(),
                    reference.ordinal(),
                    reference.target(),
                    reference.location().logical_path(),
                    reference.location().span().start(),
                    reference.location().span().end(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                checked_functions[0].id(),
                1,
                standard_boolean,
                "z-first-server.orna",
                first_boolean,
                first_boolean + "BOOLEAN".len(),
            ),
            (
                checked_functions[0].id(),
                2,
                standard_boolean,
                "z-first-server.orna",
                first_alias,
                first_alias + "std.BOOLEAN".len(),
            ),
            (
                checked_functions[1].id(),
                1,
                standard_boolean,
                "a-second-server.orna",
                second_boolean,
                second_boolean + "std.BOOLEAN".len(),
            ),
        ]
    );

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    assert_eq!(
        prepared.catalogue_hash_context().version(),
        CatalogueHashVersion::Version2
    );
    assert!(orna_core::revision::validate_persistable_catalogue(&prepared).is_ok());
    let candidate_functions = prepared.candidate().functions();
    assert_eq!(candidate_functions.len(), 2);
    assert_eq!(
        candidate_functions
            .iter()
            .map(|function| function.name().to_string())
            .collect::<Vec<_>>(),
        vec!["app.create", "app.by_ref"]
    );
    let checked_object_types = checked.object_types().collect::<Vec<_>>();
    assert_eq!(checked_object_types.len(), 1);
    let durable_item = prepared.candidate().object_types()[0].id();
    let source_unit = |path| {
        prepared
            .source()
            .units()
            .iter()
            .find(|unit| unit.logical_path() == path)
            .expect("the prepared source retains every submitted unit")
            .id()
    };
    let first_unit = source_unit("z-first-server.orna");
    let second_unit = source_unit("a-second-server.orna");
    let first_ref = first_source.find("p_ref REF app.item").unwrap() + "p_ref REF ".len();
    let first_return = first_source.find("created REF app.item").unwrap() + "created REF ".len();
    let first_body = first_source.find("INSERT INTO app.item").unwrap() + "INSERT INTO ".len();
    let second_ref = second_source.find("p_ref REF app.item").unwrap() + "p_ref REF ".len();
    let second_body = second_source.find("FROM app.item").unwrap() + "FROM ".len();
    let object_target = orna_core::revision::DefinitionReferenceTarget::ObjectType(durable_item);
    let value_target = orna_core::revision::DefinitionReferenceTarget::ValueType(standard_boolean);
    let object_kind = orna_core::revision::DefinitionReferenceKind::ObjectReference;
    let value_kind = orna_core::revision::DefinitionReferenceKind::NamedType;
    let byte = |offset: usize| u32::try_from(offset).unwrap();
    let reference_details = |function: FunctionId| {
        prepared
            .references()
            .iter()
            .filter(|reference| reference.source_function() == function)
            .map(|reference| {
                (
                    reference.ordinal(),
                    reference.target(),
                    reference.kind(),
                    reference.source_origin().source_unit(),
                    reference.source_origin().byte_start(),
                    reference.source_origin().byte_end(),
                )
            })
            .collect::<Vec<_>>()
    };

    let first_references = reference_details(candidate_functions[0].id());
    let first_prefix = vec![
        (
            0,
            object_target,
            object_kind,
            first_unit,
            byte(first_ref),
            byte(first_ref + "app.item".len()),
        ),
        (
            1,
            value_target,
            value_kind,
            first_unit,
            byte(first_boolean),
            byte(first_boolean + "BOOLEAN".len()),
        ),
        (
            2,
            value_target,
            value_kind,
            first_unit,
            byte(first_alias),
            byte(first_alias + "std.BOOLEAN".len()),
        ),
        (
            3,
            object_target,
            object_kind,
            first_unit,
            byte(first_return),
            byte(first_return + "app.item".len()),
        ),
    ];
    assert!(first_references.len() > first_prefix.len());
    assert_eq!(first_references[..first_prefix.len()], first_prefix);
    assert_eq!(
        (
            first_references[first_prefix.len()].0,
            first_references[first_prefix.len()].3,
            first_references[first_prefix.len()].4,
            first_references[first_prefix.len()].5,
        ),
        (
            u32::try_from(first_prefix.len()).unwrap(),
            first_unit,
            byte(first_body),
            byte(first_body + "app.item".len()),
        ),
        "body references must begin after the complete interleaved signature prefix"
    );

    let second_references = reference_details(candidate_functions[1].id());
    let second_prefix = vec![
        (
            0,
            object_target,
            object_kind,
            second_unit,
            byte(second_ref),
            byte(second_ref + "app.item".len()),
        ),
        (
            1,
            value_target,
            value_kind,
            second_unit,
            byte(second_boolean),
            byte(second_boolean + "std.BOOLEAN".len()),
        ),
    ];
    assert!(second_references.len() > second_prefix.len());
    assert_eq!(second_references[..second_prefix.len()], second_prefix);
    assert_eq!(
        (
            second_references[second_prefix.len()].0,
            second_references[second_prefix.len()].3,
            second_references[second_prefix.len()].4,
            second_references[second_prefix.len()].5,
        ),
        (
            u32::try_from(second_prefix.len()).unwrap(),
            second_unit,
            byte(second_body),
            byte(second_body + "app.item".len()),
        ),
        "the second function body must begin after its signature prefix"
    );
}

#[test]
fn standard_preparation_drives_declaration_body_and_reference_evidence_gates() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app;\
             CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);\
             CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN, p_alias std.BOOLEAN) \
             RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
             AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made);",
        )])
        .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let canonical_uses = checked.uses().to_vec();
    let canonical_references = checked.standard_type_references().to_vec();
    let server_functions = checked.server_functions().collect::<Vec<_>>();
    assert_eq!(server_functions.len(), 1);
    let body_function = server_functions[0].id();
    let declaration_value_index = canonical_uses
        .iter()
        .position(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Field { .. }
                    | CheckedTypeUseKind::Parameter { .. }
                    | CheckedTypeUseKind::Return { .. }
            ) && type_use.value().is_some()
        })
        .unwrap();
    let declaration_reference_index = canonical_uses
        .iter()
        .position(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Field { .. }
                    | CheckedTypeUseKind::Parameter { .. }
                    | CheckedTypeUseKind::Return { .. }
            ) && type_use.object_reference().is_some()
        })
        .unwrap();
    let body_value_index = canonical_uses
        .iter()
        .position(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
            ) && type_use.value().is_some()
        })
        .unwrap();
    let body_reference_index = canonical_uses
        .iter()
        .position(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
            ) && type_use.object_reference().is_some()
        })
        .unwrap();

    let mut declaration_and_body_hostile = report.clone();
    let mut uses = canonical_uses.clone();
    let direct_index = uses
        .iter()
        .position(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Field { .. }
                    | CheckedTypeUseKind::Parameter { .. }
                    | CheckedTypeUseKind::Return { .. }
            )
        })
        .unwrap();
    uses.remove(direct_index);
    let mut body_indices = uses
        .iter()
        .enumerate()
        .filter_map(|(index, type_use)| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    body_indices.reverse();
    assert!(body_indices.len() >= 2);
    uses.swap(body_indices[0], body_indices[1]);
    assert!(declaration_and_body_hostile.replace_type_uses_for_test(uses));
    let mut references = canonical_references.clone();
    references.swap(0, 1);
    assert!(declaration_and_body_hostile.replace_standard_type_references_for_test(references));
    let error = prepare_standard_application(&declaration_and_body_hostile, active.pair(), &active)
        .unwrap_err();
    assert_declaration_evidence_mismatch(error, canonical_uses[direct_index].kind());

    let mut reordered_declarations = report.clone();
    let mut uses = canonical_uses.clone();
    uses.swap(declaration_value_index, declaration_reference_index);
    assert!(reordered_declarations.replace_type_uses_for_test(uses));
    let error =
        prepare_standard_application(&reordered_declarations, active.pair(), &active).unwrap_err();
    assert_declaration_evidence_mismatch(error, canonical_uses[declaration_value_index].kind());

    let mut wrong_declaration_kind = report.clone();
    assert!(wrong_declaration_kind.replace_type_use_kind_for_test(
        declaration_value_index,
        canonical_uses[body_value_index].kind(),
    ));
    let error =
        prepare_standard_application(&wrong_declaration_kind, active.pair(), &active).unwrap_err();
    assert_declaration_evidence_mismatch(error, canonical_uses[declaration_value_index].kind());

    let mut wrong_declaration_type = report.clone();
    assert!(wrong_declaration_type
        .replace_value_type_id_for_test(declaration_value_index, TypeId::from_bytes([0xd1; 16]),));
    let error =
        prepare_standard_application(&wrong_declaration_type, active.pair(), &active).unwrap_err();
    assert_declaration_evidence_mismatch(error, canonical_uses[declaration_value_index].kind());

    let mut gate_seven_before_eight = wrong_declaration_type.clone();
    let hostile_digest = Sha256Digest::from_bytes([0xd0; 32]);
    assert!(gate_seven_before_eight.replace_standard_context_for_test(
        verified.catalogue().revision(),
        verified.revision(),
        hostile_digest,
    ));
    let error =
        prepare_standard_application(&gate_seven_before_eight, active.pair(), &active).unwrap_err();
    assert_standard_digest_mismatch(error, hostile_digest, verified.digest());

    let mut wrong_declaration_target = report.clone();
    assert!(
        wrong_declaration_target.replace_object_reference_target_for_test(
            declaration_reference_index,
            CheckedTypeId::Existing(TypeId::from_bytes([0xd2; 16])),
        )
    );
    let error = prepare_standard_application(&wrong_declaration_target, active.pair(), &active)
        .unwrap_err();
    assert_declaration_evidence_mismatch(error, canonical_uses[declaration_reference_index].kind());

    let mut wrong_declaration_location = report.clone();
    assert!(
        wrong_declaration_location.replace_type_use_location_for_test(
            declaration_value_index,
            canonical_uses[declaration_reference_index]
                .location()
                .clone(),
        )
    );
    let error = prepare_standard_application(&wrong_declaration_location, active.pair(), &active)
        .unwrap_err();
    assert_declaration_evidence_mismatch(error, canonical_uses[declaration_value_index].kind());

    let mut wrong_declaration_class = report.clone();
    assert!(
        wrong_declaration_class.replace_value_with_object_reference_for_test(
            declaration_value_index,
            CheckedTypeId::Existing(TypeId::from_bytes([0xd3; 16])),
        )
    );
    let error =
        prepare_standard_application(&wrong_declaration_class, active.pair(), &active).unwrap_err();
    assert_declaration_evidence_mismatch(error, canonical_uses[declaration_value_index].kind());

    let mut duplicate_declaration = report.clone();
    let mut uses = canonical_uses.clone();
    uses.push(canonical_uses[direct_index].clone());
    assert!(duplicate_declaration.replace_type_uses_for_test(uses));
    let error =
        prepare_standard_application(&duplicate_declaration, active.pair(), &active).unwrap_err();
    assert_declaration_evidence_mismatch(error, canonical_uses[direct_index].kind());

    let mut body_hostile = report.clone();
    let mut uses = canonical_uses.clone();
    let mut body_indices = uses
        .iter()
        .enumerate()
        .filter_map(|(index, type_use)| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    body_indices.reverse();
    assert!(body_indices.len() >= 2);
    uses.swap(body_indices[0], body_indices[1]);
    assert!(body_hostile.replace_type_uses_for_test(uses));
    let error = prepare_standard_application(&body_hostile, active.pair(), &active).unwrap_err();
    assert_body_evidence_mismatch(error, body_function);

    let mut body_before_declaration = report.clone();
    let mut uses = canonical_uses.clone();
    let body = uses.remove(body_value_index);
    uses.insert(direct_index, body);
    assert!(body_before_declaration.replace_type_uses_for_test(uses));
    let error =
        prepare_standard_application(&body_before_declaration, active.pair(), &active).unwrap_err();
    assert_body_evidence_mismatch(error, body_function);

    let mut declaration_after_body = report.clone();
    let mut uses = canonical_uses.clone();
    let last_declaration_index = uses
        .iter()
        .enumerate()
        .filter_map(|(index, type_use)| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Field { .. }
                    | CheckedTypeUseKind::Parameter { .. }
                    | CheckedTypeUseKind::Return { .. }
            )
            .then_some(index)
        })
        .next_back()
        .unwrap();
    let declaration = uses.remove(last_declaration_index);
    uses.insert(body_value_index, declaration);
    assert!(declaration_after_body.replace_type_uses_for_test(uses));
    let error =
        prepare_standard_application(&declaration_after_body, active.pair(), &active).unwrap_err();
    assert_body_evidence_mismatch(error, body_function);

    let mut gate_nine_before_ten = body_hostile.clone();
    let mut references = canonical_references.clone();
    references.swap(0, 1);
    assert!(gate_nine_before_ten.replace_standard_type_references_for_test(references));
    let error =
        prepare_standard_application(&gate_nine_before_ten, active.pair(), &active).unwrap_err();
    assert_body_evidence_mismatch(error, body_function);

    let mut wrong_body_type = report.clone();
    assert!(wrong_body_type
        .replace_value_type_id_for_test(body_value_index, TypeId::from_bytes([0xd4; 16]),));
    let error = prepare_standard_application(&wrong_body_type, active.pair(), &active).unwrap_err();
    assert_body_evidence_mismatch(error, body_function);

    let mut wrong_body_target = report.clone();
    assert!(wrong_body_target.replace_object_reference_target_for_test(
        body_reference_index,
        CheckedTypeId::Existing(TypeId::from_bytes([0xd5; 16])),
    ));
    let error =
        prepare_standard_application(&wrong_body_target, active.pair(), &active).unwrap_err();
    assert_body_evidence_mismatch(error, body_function);

    let mut wrong_body_location = report.clone();
    assert!(wrong_body_location.replace_type_use_location_for_test(
        body_value_index,
        canonical_uses[body_reference_index].location().clone(),
    ));
    let error =
        prepare_standard_application(&wrong_body_location, active.pair(), &active).unwrap_err();
    assert_body_evidence_mismatch(error, body_function);

    let mut wrong_body_class = report.clone();
    assert!(
        wrong_body_class.replace_value_with_object_reference_for_test(
            body_value_index,
            CheckedTypeId::Existing(TypeId::from_bytes([0xd6; 16])),
        )
    );
    let error =
        prepare_standard_application(&wrong_body_class, active.pair(), &active).unwrap_err();
    assert_body_evidence_mismatch(error, body_function);

    let mut wrong_body_kind = report.clone();
    assert!(wrong_body_kind.replace_type_use_kind_for_test(
        body_value_index,
        canonical_uses[body_reference_index].kind(),
    ));
    let error = prepare_standard_application(&wrong_body_kind, active.pair(), &active).unwrap_err();
    assert_body_evidence_mismatch(error, body_function);

    let mut empty_body = report.clone();
    let uses = canonical_uses
        .iter()
        .filter(|type_use| {
            !matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
            )
        })
        .cloned()
        .collect();
    assert!(empty_body.replace_type_uses_for_test(uses));
    let error = prepare_standard_application(&empty_body, active.pair(), &active).unwrap_err();
    assert_body_evidence_mismatch(error, body_function);

    let mut extra_body = report.clone();
    let mut uses = canonical_uses.clone();
    let extra = uses
        .iter()
        .find(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { .. } | CheckedTypeUseKind::Result { .. }
            )
        })
        .cloned()
        .unwrap();
    uses.push(extra);
    assert!(extra_body.replace_type_uses_for_test(uses));
    let error = prepare_standard_application(&extra_body, active.pair(), &active).unwrap_err();
    assert_body_evidence_mismatch(error, body_function);

    let mut references_hostile = report.clone();
    let mut references = canonical_references.clone();
    assert_eq!(references.len(), 2);
    references.swap(0, 1);
    assert!(references_hostile.replace_standard_type_references_for_test(references));
    let error =
        prepare_standard_application(&references_hostile, active.pair(), &active).unwrap_err();
    assert_function_reference_evidence_mismatch(error, canonical_references[0].owner());

    let mut wrong_reference = report.clone();
    let first_reference = &checked.standard_type_references()[0];
    assert!(wrong_reference.replace_standard_type_reference_for_test(
        0,
        CheckedFunctionId::Existing(FunctionId::from_bytes([0xd7; 16])),
        first_reference.ordinal() + 1,
        TypeId::from_bytes([0xd8; 16]),
        canonical_uses[body_value_index].location().clone(),
    ));
    let error = prepare_standard_application(&wrong_reference, active.pair(), &active).unwrap_err();
    assert_function_reference_evidence_mismatch(error, first_reference.owner());

    let mut missing_reference = report.clone();
    let mut references = checked.standard_type_references().to_vec();
    references.pop();
    assert!(missing_reference.replace_standard_type_references_for_test(references));
    let error =
        prepare_standard_application(&missing_reference, active.pair(), &active).unwrap_err();
    assert_function_reference_evidence_mismatch(error, first_reference.owner());
}

#[test]
fn standard_preparation_checks_relational_mutation_and_client_body_uses_before_client_staging() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "application.orna",
        "CREATE SCHEMA app; \
             CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL); \
             CREATE SERVER FUNCTION app.read(p_task REF app.task) RETURNS ROWS (done BOOLEAN) \
             TRANSACTION READ ONLY VOLATILITY STABLE \
             AS SELECT task.done FROM app.task task WHERE REF(task) = p_task; \
             CREATE SERVER FUNCTION app.create(p_done BOOLEAN) RETURNS ROWS (created REF app.task) \
             TRANSACTION ATOMIC \
             AS INSERT INTO app.task AS made (done) VALUES (p_done) RETURNING REF(made); \
             CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let checked_bundle = report.checked_bundle();
    assert!(
        checked_bundle.is_some(),
        "a diagnostic-free standard application report must contain a checked bundle"
    );
    let checked = checked_bundle.expect("the asserted checked bundle must be present");
    let servers = checked.server_functions().collect::<Vec<_>>();
    let clients = checked.client_functions().collect::<Vec<_>>();
    assert_eq!(servers.len(), 2);
    assert_eq!(clients.len(), 1);
    let read = servers[0];
    let create = servers[1];
    let enabled = clients[0];
    let uses = checked.uses();
    let value_body_index = |owner| {
        uses.iter().position(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression { owner: actual, .. }
                    | CheckedTypeUseKind::Result { owner: actual, .. }
                    if actual == owner
            ) && type_use.value().is_some()
        })
    };
    let read_body =
        value_body_index(read.id()).expect("the relational function must retain a value body use");
    let create_body =
        value_body_index(create.id()).expect("the mutation function must retain a value body use");
    let client_body =
        value_body_index(enabled.id()).expect("the CLIENT function must retain a value body use");

    for (index, owner) in [
        (read_body, read.id()),
        (create_body, create.id()),
        (client_body, enabled.id()),
    ] {
        let mut hostile = report.clone();
        assert!(hostile.replace_value_type_id_for_test(index, TypeId::from_bytes([0xdd; 16])));
        let error = prepare_standard_application(&hostile, active.pair(), &active).unwrap_err();
        assert_body_evidence_mismatch(error, owner);
    }
}

#[test]
fn standard_preparation_preserves_multi_unit_signature_references_and_mixed_owner_order() {
    let verified = verified_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    assert_eq!(standard.value_types()[0].id(), TypeId::from_bytes([3; 16]));
    assert_ne!(
        standard.value_types()[0].id(),
        TypeId::from_bytes(CANONICAL_TYPE_IDS[0])
    );
    let active = empty_version_two_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let first_server = "CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_boolean BOOLEAN, p_alias std.BOOLEAN) \
            RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC \
            AS INSERT INTO app.item AS made (done) VALUES (p_boolean) RETURNING REF(made);";
    let client = "CREATE CLIENT FUNCTION app.enabled() RETURNS std.BOOLEAN RETURN TRUE;";
    let second_server = "CREATE SERVER FUNCTION app.by_ref(p_ref REF app.item) \
            RETURNS ROWS (value std.BOOLEAN) TRANSACTION READ ONLY VOLATILITY STABLE \
            AS SELECT TRUE FROM app.item item WHERE REF(item) = p_ref;";
    let declarations = "CREATE SCHEMA app; \
            CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);";
    let bundle = SourceBundle::new([
        SourceUnit::new("z-first-server.orna", first_server),
        SourceUnit::new("a-client.orna", client),
        SourceUnit::new("y-second-server.orna", second_server),
        SourceUnit::new("m-declarations.orna", declarations),
    ])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert_eq!(report.diagnostics(), &[]);
    let checked_bundle = report.checked_bundle();
    assert!(
        checked_bundle.is_some(),
        "a diagnostic-free standard application report must contain a checked bundle"
    );
    let checked = checked_bundle.expect("the asserted checked bundle must be present");
    let references = checked.standard_type_references().to_vec();
    assert_eq!(references.len(), 4);
    assert_eq!(
        references
            .iter()
            .map(|reference| (reference.ordinal(), reference.location().logical_path()))
            .collect::<Vec<_>>(),
        vec![
            (1, "z-first-server.orna"),
            (2, "z-first-server.orna"),
            (0, "a-client.orna"),
            (1, "y-second-server.orna"),
        ],
        "reference order follows source-unit insertion order and preserves REF ordinal gaps"
    );
    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    assert_eq!(
        prepared
            .candidate()
            .functions()
            .iter()
            .map(|function| (function.name().to_string(), function.domain()))
            .collect::<Vec<_>>(),
        vec![
            ("app.create".to_owned(), FunctionDomain::Server),
            ("app.enabled".to_owned(), FunctionDomain::Client),
            ("app.by_ref".to_owned(), FunctionDomain::Server),
        ],
        "CLIENT and SERVER lowering follows canonical declaration-evidence owner order"
    );
    assert_eq!(
        prepared.catalogue_hash_context().version(),
        CatalogueHashVersion::Version2
    );
    let candidate_object = &prepared.candidate().object_types()[0];
    let candidate_item = candidate_object.id();
    assert_eq!(
        candidate_object.fields()[0].resolved_type(),
        ResolvedType::Value(TypeId::from_bytes([3; 16]))
    );
    let candidate_functions = prepared.candidate().functions();
    let create = &candidate_functions[0];
    assert!(matches!(
        create.parameters()[0].resolved_type(),
        ResolvedType::Reference { target } if target == candidate_item
    ));
    assert_eq!(
        create.parameters()[1].resolved_type(),
        ResolvedType::Value(TypeId::from_bytes([3; 16]))
    );
    assert_eq!(
        create.parameters()[2].resolved_type(),
        ResolvedType::Value(TypeId::from_bytes([3; 16]))
    );
    let FunctionReturn::Rows(create_columns) = create.return_type() else {
        panic!("the mutation fixture must retain a ROWS return")
    };
    assert!(matches!(
        create_columns[0].resolved_type(),
        ResolvedType::Reference { target } if target == candidate_item
    ));
    assert_eq!(
        candidate_functions[1].return_type(),
        &FunctionReturn::Single(ResolvedType::Value(TypeId::from_bytes([3; 16])))
    );
    let by_ref = &candidate_functions[2];
    assert!(matches!(
        by_ref.parameters()[0].resolved_type(),
        ResolvedType::Reference { target } if target == candidate_item
    ));
    let FunctionReturn::Rows(by_ref_columns) = by_ref.return_type() else {
        panic!("the SERVER fixture must retain a ROWS return")
    };
    assert_eq!(
        by_ref_columns[0].resolved_type(),
        ResolvedType::Value(TypeId::from_bytes([3; 16]))
    );
    let value_reference_targets = prepared
        .references()
        .iter()
        .filter_map(|reference| {
            if let DefinitionReferenceTarget::ValueType(type_id) = reference.target() {
                Some(type_id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(value_reference_targets.len(), 4);
    assert!(value_reference_targets
        .iter()
        .all(|type_id| *type_id == TypeId::from_bytes([3; 16])));
    assert_eq!(
        prepared
            .references()
            .iter()
            .filter_map(|reference| {
                if !matches!(reference.target(), DefinitionReferenceTarget::ValueType(_)) {
                    return None;
                }
                Some((
                    reference.source_function(),
                    reference.ordinal(),
                    reference.kind(),
                    reference.target(),
                ))
            })
            .collect::<Vec<_>>(),
        vec![
            (
                candidate_functions[0].id(),
                1,
                DefinitionReferenceKind::NamedType,
                DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16])),
            ),
            (
                candidate_functions[0].id(),
                2,
                DefinitionReferenceKind::NamedType,
                DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16])),
            ),
            (
                candidate_functions[1].id(),
                0,
                DefinitionReferenceKind::NamedType,
                DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16])),
            ),
            (
                candidate_functions[2].id(),
                1,
                DefinitionReferenceKind::NamedType,
                DefinitionReferenceTarget::ValueType(TypeId::from_bytes([3; 16])),
            ),
        ]
    );
    assert!(orna_core::revision::validate_persistable_catalogue(&prepared).is_ok());
    let candidate_function_ids = prepared
        .candidate()
        .functions()
        .iter()
        .map(|function| function.id())
        .collect::<Vec<_>>();
    let function_origins = prepared
        .origins()
        .iter()
        .filter_map(|origin| match origin.identity() {
            DefinitionIdentity::Function(id) => Some(id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(function_origins, candidate_function_ids);
    let current = prepared.current_function_revisions().unwrap_or_default();
    assert_eq!(
        current
            .iter()
            .map(|revision| revision.function())
            .collect::<Vec<_>>(),
        candidate_function_ids
    );
    let mut reference_groups = Vec::new();
    for reference in prepared.references() {
        if reference_groups.last().copied() != Some(reference.source_function()) {
            reference_groups.push(reference.source_function());
        }
    }
    assert_eq!(reference_groups, candidate_function_ids);

    let first = &references[0];
    for (owner, ordinal, target, location) in [
        (
            CheckedFunctionId::Existing(FunctionId::from_bytes([0xde; 16])),
            first.ordinal(),
            first.target(),
            first.location().clone(),
        ),
        (
            first.owner(),
            first.ordinal() + 1,
            first.target(),
            first.location().clone(),
        ),
        (
            first.owner(),
            first.ordinal(),
            TypeId::from_bytes([0xdf; 16]),
            first.location().clone(),
        ),
        (
            first.owner(),
            first.ordinal(),
            first.target(),
            references[1].location().clone(),
        ),
    ] {
        let mut hostile = report.clone();
        assert!(
            hostile.replace_standard_type_reference_for_test(0, owner, ordinal, target, location,)
        );
        let error = prepare_standard_application(&hostile, active.pair(), &active).unwrap_err();
        assert_function_reference_evidence_mismatch(error, first.owner());
    }

    let mut reordered = report.clone();
    let mut hostile_references = references.clone();
    hostile_references.swap(0, 2);
    assert!(reordered.replace_standard_type_references_for_test(hostile_references));
    let error = prepare_standard_application(&reordered, active.pair(), &active).unwrap_err();
    assert_function_reference_evidence_mismatch(error, first.owner());

    let mut missing = report.clone();
    let mut hostile_references = references.clone();
    hostile_references.pop();
    assert!(missing.replace_standard_type_references_for_test(hostile_references));
    let error = prepare_standard_application(&missing, active.pair(), &active).unwrap_err();
    assert_function_reference_evidence_mismatch(error, references[3].owner());

    let mut extra = report.clone();
    let mut hostile_references = references.clone();
    hostile_references.push(hostile_references[0].clone());
    assert!(extra.replace_standard_type_references_for_test(hostile_references));
    let error = prepare_standard_application(&extra, active.pair(), &active).unwrap_err();
    assert_function_reference_evidence_mismatch(error, references[0].owner());
}
