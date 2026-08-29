use super::*;

#[test]
fn capability_gate_denies_an_ungranted_declared_capability() {
    let (active, function, _, _) = version_one_active(true);
    let grants = super::super::capability::LocalCapabilityGrantSet::new();
    let declaration = super::super::capability::LocalCapabilityDeclaration::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityArgumentSource::Text("/home/bob".to_owned()),
    );

    let error = super::super::evaluate_client_function_with_grants(
        &active,
        &authorise(active.pair(), function),
        &[declaration],
        &grants,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        super::super::ClientExecutionError::CapabilityDenied {
            context,
            capability,
        } if context.function() == function && capability == "std.fs.read"
    ));
}

#[test]
fn capability_gate_admits_a_granted_declared_capability() {
    let (active, function, pair, _) = version_one_active(true);
    let grant = super::super::capability::LocalCapabilityGrant::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let grants = super::super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let declaration = super::super::capability::LocalCapabilityDeclaration::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityArgumentSource::Text("/home/bob/x".to_owned()),
    );

    let result = super::super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, function),
        &[declaration],
        &grants,
    )
    .unwrap();

    assert_eq!(result.context().function(), function);
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn capability_gate_keeps_zero_declaration_functions_unchanged() {
    let (active, function, pair, _) = version_one_active(true);
    let empty_grants = super::super::capability::LocalCapabilityGrantSet::new();

    let result = super::super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, function),
        &[],
        &empty_grants,
    )
    .unwrap();

    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn version_five_stored_literal_capability_denies_without_grants() {
    let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
        "std.fs.read",
        orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob/x".to_owned()),
    )];
    let (active, function, _, _) =
        version_five_boolean_active(version_five_boolean_envelope(true, requirements));
    let empty_grants = super::super::capability::LocalCapabilityGrantSet::new();
    // A caller-supplied declaration must never replace the stored
    // requirements of a version-5 envelope.
    let declaration = super::super::capability::LocalCapabilityDeclaration::new(
        super::super::capability::LocalCapabilityName::StdSecretUse,
        super::super::capability::LocalCapabilityArgumentSource::Text("secret-1".to_owned()),
    );

    let error = super::super::evaluate_client_function_with_grants(
        &active,
        &authorise(active.pair(), function),
        &[declaration],
        &empty_grants,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        super::super::ClientExecutionError::CapabilityDenied {
            context,
            capability,
        } if context.function() == function && capability == "std.fs.read"
    ));
    assert_eq!(
        error.to_string(),
        "the CLIENT function requires the capability std.fs.read which is not granted"
    );
}

#[test]
fn version_five_artifact_hash_is_checked_before_capability_decode() {
    let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
        "std.fs.read",
        orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob/x".to_owned()),
    )];
    let (active, function, pair, _) =
        version_five_boolean_active(version_five_boolean_envelope(true, requirements));
    let untrusted = active_with_mismatched_function_artifact_payload_hash(&active);
    let mut state = ClientStateStore::new();
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = None;

    let error = super::super::evaluate_function(
        &untrusted,
        function,
        Vec::new(),
        &[],
        &capability::LocalCapabilityGrantSet::new(),
        &mut state,
        0,
        PrincipalId::from_bytes([0x7b; 16]),
        super::super::ObserverLineage::top_level(InvocationId::from_bytes([0xa2; 16])),
        &mut executor_slot,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientExecutionError::InvalidArtifact { context, .. }
            if context.pair() == pair && context.function() == function
    ));
}

#[test]
fn version_five_stored_literal_capability_evaluates_when_covered() {
    let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
        "std.fs.read",
        orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob/x".to_owned()),
    )];
    let (active, function, pair, _) =
        version_five_boolean_active(version_five_boolean_envelope(true, requirements));
    let grant = super::super::capability::LocalCapabilityGrant::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let grants = super::super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();

    let result = super::super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, function),
        &[],
        &grants,
    )
    .unwrap();

    assert_eq!(result.context().function(), function);
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn version_five_unknown_stored_capability_name_fails_closed() {
    let requirements = vec![orna_artifact::client_plan::CapabilityRequirement::new(
        "std.bogus.op",
        orna_artifact::client_plan::CapabilityArgumentSource::Text("anything".to_owned()),
    )];
    let (active, function, _, _) =
        version_five_boolean_active(version_five_boolean_envelope(true, requirements));
    // Every vocabulary grant present: the unknown stored name still fails
    // closed and never falls back to an empty requirement set.
    let grants = super::super::capability::LocalCapabilityGrantSet::from_grants(
        super::super::capability::LocalCapabilityName::ALL
            .into_iter()
            .map(|name| {
                let scope = match name {
                    super::super::capability::LocalCapabilityName::StdFsRead
                    | super::super::capability::LocalCapabilityName::StdFsWrite => {
                        super::super::capability::LocalCapabilityScope::path("/home/bob").unwrap()
                    }
                    super::super::capability::LocalCapabilityName::StdNetConnect => {
                        super::super::capability::LocalCapabilityScope::host("example.com").unwrap()
                    }
                    super::super::capability::LocalCapabilityName::StdSecretUse => {
                        super::super::capability::LocalCapabilityScope::secret("secret-1").unwrap()
                    }
                };
                super::super::capability::LocalCapabilityGrant::new(name, scope).unwrap()
            }),
    )
    .unwrap();

    let error = super::super::evaluate_client_function_with_grants(
        &active,
        &authorise(active.pair(), function),
        &[],
        &grants,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        super::super::ClientExecutionError::CapabilityDenied {
            context,
            capability,
        } if context.function() == function && capability == "std.bogus.op"
    ));
}

#[test]
fn version_five_stored_parameter_capability_resolves_the_invocation_argument() {
    let parameter_id = ParameterId::from_bytes([0xb1; 16]);
    let plan = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Expression(
            orna_artifact::client_plan::ExpressionClientPlan::new(
                orna_artifact::client_plan::ClientExpressionNode::ParameterRead {
                    parameter: parameter_id,
                },
            ),
        ),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Parameter("p_path".to_owned()),
        )],
    );
    let (active, function, pair, _, _) =
        version_five_expression_active_with_parameter(plan.encode().unwrap());
    let argument = orna_core::value::FunctionArgument::new(
        parameter_id,
        RuntimeValue::Text("/home/bob/notes.txt".to_owned()),
    )
    .unwrap();

    let result = super::super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(pair, function),
        &[argument],
        &[],
        &super::super::capability::LocalCapabilityGrantSet::new(),
    )
    .unwrap_err();

    assert!(matches!(
        &result,
        super::super::ClientExecutionError::CapabilityDenied { capability, .. }
            if capability == "std.fs.read"
    ));

    let grant = super::super::capability::LocalCapabilityGrant::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let grants = super::super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let argument = orna_core::value::FunctionArgument::new(
        parameter_id,
        RuntimeValue::Text("/home/bob/notes.txt".to_owned()),
    )
    .unwrap();

    let result = super::super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(pair, function),
        &[argument],
        &[],
        &grants,
    )
    .unwrap();

    assert_eq!(
        result.value(),
        &RuntimeValue::Text("/home/bob/notes.txt".to_owned())
    );
}

#[test]
fn version_five_recursive_calls_enforce_the_callee_capability() {
    let (base, caller_id, pair, caller_revision_id) = version_one_active(true);
    let callee_id = FunctionId::from_bytes([0xc2; 16]);
    let callee_revision_id = FunctionRevisionId::from_bytes([0xc3; 16]);
    let previous_revision = &base.function_revisions()[0];
    let caller_name = base
        .catalogue()
        .function_by_id(caller_id)
        .unwrap()
        .name()
        .clone();
    let caller_plan = orna_artifact::client_plan::ExpressionClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Call {
            function: callee_id,
            arguments: Vec::new(),
        },
    );
    let caller_payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Expression(caller_plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.write",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let callee_plan = orna_artifact::client_plan::ExpressionClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
    );
    let callee_payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Expression(callee_plan),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/home/bob".to_owned()),
        )],
    )
    .encode()
    .unwrap();
    let caller = FunctionDefinition::new(
        caller_id,
        caller_name,
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID)),
        caller_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let callee = FunctionDefinition::new(
        callee_id,
        QualifiedSemanticName::new(["app", "callee"]).unwrap(),
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID)),
        callee_revision_id,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        base.catalogue().revision(),
        base.catalogue().schemas().to_vec(),
        base.catalogue().object_types().to_vec(),
        vec![caller.clone(), callee.clone()],
    )
    .unwrap();
    let caller_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        caller_payload.clone(),
        artifact_payload_digest(&caller_payload).unwrap(),
    )
    .unwrap();
    let callee_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        callee_payload.clone(),
        artifact_payload_digest(&callee_payload).unwrap(),
    )
    .unwrap();
    let caller_reference = DefinitionReference::new(
        caller_id,
        caller_revision_id,
        0,
        DefinitionReferenceTarget::Function(callee_id),
        DefinitionReferenceKind::FunctionCall,
        previous_revision.declaration_origin(),
    );
    let caller_semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &caller,
        previous_revision.language_version(),
        &caller_artifact,
        base.expressions(),
        std::slice::from_ref(&caller_reference),
    )
    .unwrap();
    let callee_semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        &callee,
        previous_revision.language_version(),
        &callee_artifact,
        base.expressions(),
        &[],
    )
    .unwrap();
    let caller_revision = FunctionRevisionRecord::new(
        caller_id,
        caller_revision_id,
        previous_revision.revision_number(),
        previous_revision.declaration_origin(),
        previous_revision.declaration_content_hash(),
        caller_semantic_hash,
        previous_revision.language_version(),
        caller_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let callee_origin = SourceOrigin::new(
        previous_revision.declaration_origin().source_unit(),
        previous_revision.declaration_origin().byte_start(),
        previous_revision.declaration_origin().byte_end(),
    )
    .unwrap();
    let callee_revision = FunctionRevisionRecord::new(
        callee_id,
        callee_revision_id,
        previous_revision.revision_number(),
        callee_origin,
        previous_revision.declaration_content_hash(),
        callee_semantic_hash,
        previous_revision.language_version(),
        callee_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let mut origins = base.origins().to_vec();
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Function(callee_id),
        callee_origin,
    ));
    let revisions = vec![caller_revision, callee_revision];
    let references = vec![caller_reference];
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let context = orna_core::revision::CatalogueHashContext::version_two(standard);
    let catalogue_hash = catalogue_digest_with_context(
        &context,
        &catalogue,
        &revisions,
        base.expressions(),
        &origins,
        &references,
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            base.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(base.expressions().to_vec(), revisions, origins, references),
        ),
        context,
    )
    .unwrap();
    let write_grant = super::super::capability::LocalCapabilityGrant::new(
        super::super::capability::LocalCapabilityName::StdFsWrite,
        super::super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let write_only =
        super::super::capability::LocalCapabilityGrantSet::from_grants([write_grant]).unwrap();
    let error = super::super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, caller_id),
        &[],
        &write_only,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        super::super::ClientExecutionError::CapabilityDenied {
            context,
            capability,
        } if context.function() == callee_id && capability == "std.fs.read"
    ));
    let read_grant = super::super::capability::LocalCapabilityGrant::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let grants = super::super::capability::LocalCapabilityGrantSet::from_grants(
        write_only
            .as_slice()
            .iter()
            .cloned()
            .chain(std::iter::once(read_grant)),
    )
    .unwrap();
    let result = super::super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, caller_id),
        &[],
        &grants,
    )
    .unwrap();
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn nested_call_preserves_caller_bound_capability_parameter() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; \
         CREATE CLIENT FUNCTION app.first(p_path TEXT) RETURNS TEXT RETURN app.second(); \
         CREATE CLIENT FUNCTION app.second() RETURNS TEXT RETURN 'ok';",
    );
    let initial = active_from_prepared_candidate(&prepared);
    let caller = initial
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.first")
        .expect("caller is present")
        .clone();
    let callee = initial
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.second")
        .expect("callee is present")
        .clone();
    let parameter = caller
        .parameters()
        .first()
        .expect("caller path parameter is present")
        .id();
    let payload = orna_artifact::client_plan::ExpressionClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Call {
            function: callee.id(),
            arguments: Vec::new(),
        },
    )
    .encode()
    .expect("caller expression plan encodes");
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let current = initial
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == caller.id())
        .expect("caller revision is present");
    let caller_references = initial
        .references()
        .iter()
        .filter(|reference| reference.source_function() == caller.id())
        .cloned()
        .collect::<Vec<_>>();
    let semantic_hash = function_semantic_digest_with_version(
        current.semantic_hash_version(),
        &caller,
        current.language_version(),
        &artifact,
        initial.expressions(),
        &caller_references,
    )
    .unwrap();
    let replacement = FunctionRevisionRecord::new(
        caller.id(),
        current.id(),
        current.revision_number(),
        current.declaration_origin(),
        current.declaration_content_hash(),
        semantic_hash,
        current.language_version(),
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(current.semantic_hash_version());
    let revisions = initial
        .function_revisions()
        .iter()
        .map(|revision| {
            if revision.function() == caller.id() {
                replacement.clone()
            } else {
                revision.clone()
            }
        })
        .collect::<Vec<_>>();
    let catalogue_hash = catalogue_digest_with_context(
        initial.catalogue_hash_context(),
        initial.catalogue(),
        &revisions,
        initial.expressions(),
        initial.origins(),
        initial.references(),
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            initial.pair(),
            initial.source().clone(),
            initial.catalogue().clone(),
            catalogue_hash,
            ActiveRevisionContent::new(
                initial.expressions().to_vec(),
                revisions,
                initial.origins().to_vec(),
                initial.references().to_vec(),
            ),
        ),
        initial.catalogue_hash_context().clone(),
    )
    .unwrap();
    let declaration = capability::LocalCapabilityDeclaration::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityArgumentSource::Parameter("p_path".to_owned()),
    );
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Text("/home/bob/notes.txt".to_owned()),
    )
    .unwrap();
    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/home/bob").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();

    let result = super::super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(active.pair(), caller.id()),
        std::slice::from_ref(&argument),
        std::slice::from_ref(&declaration),
        &grants,
    )
    .expect("caller-scoped capability remains bound in the nested call");
    assert_eq!(result.value(), &RuntimeValue::Text("ok".to_owned()));

    let mismatched_grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let mismatched_grants =
        capability::LocalCapabilityGrantSet::from_grants([mismatched_grant]).unwrap();
    let error = super::super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorise(active.pair(), caller.id()),
        &[argument],
        &[declaration],
        &mismatched_grants,
    )
    .expect_err("a mismatched caller scope still fails closed");
    assert!(matches!(
        error,
        super::super::ClientExecutionError::CapabilityDenied { context, capability }
            if context.function() == caller.id() && capability == "std.fs.read"
    ));
}

#[test]
fn expression_calls_reject_targets_absent_from_the_active_reference_set() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; \
         CREATE CLIENT FUNCTION app.first() RETURNS BOOLEAN RETURN app.second(); \
         CREATE CLIENT FUNCTION app.second() RETURNS BOOLEAN RETURN TRUE;",
    );
    let first = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.first")
        .expect("first function is present");
    let second = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.second")
        .expect("second function is present");
    let mut references = prepared.references().to_vec();
    let index = references
        .iter()
        .position(|reference| {
            reference.source_function() == first.id()
                && reference.target() == DefinitionReferenceTarget::Function(second.id())
        })
        .expect("first call reference is present");
    let original = references[index].clone();
    references[index] = DefinitionReference::new(
        original.source_function(),
        original.source_revision(),
        original.ordinal(),
        DefinitionReferenceTarget::Function(first.id()),
        original.kind(),
        original.source_origin(),
    );
    let active = active_from_prepared_with_references(&prepared, references);

    let error = evaluate_client_function(&active, first.id()).unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientExecutionError::ExpressionEvaluation {
            context,
            source: super::super::ClientExpressionError::InvalidCall,
        } if context.function() == first.id()
    ));
}

#[test]
fn client_expression_call_depth_is_bounded_by_artifact_limit() {
    let (active, function, pair, function_revision) = version_one_active(true);
    let context = super::super::ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::new(),
        observer_lineage: None,
    };
    let expression = orna_artifact::client_plan::ClientExpressionNode::Call {
        function,
        arguments: Vec::new(),
    };
    let mut state = super::super::ClientStateStore::new();
    let mut executor: Option<&mut dyn super::super::ClientResourceExecutor> = None;
    let mut local_environment = super::super::ClientLocalEnvironment::new();

    let error = super::super::evaluate_expression(
        &active,
        &expression,
        context,
        super::super::ObserverLineage::top_level(context.parent_invocation_id()),
        &[],
        &[],
        &super::super::capability::LocalCapabilityGrantSet::new(),
        &mut state,
        orna_artifact::client_plan::MAX_EXPRESSION_DEPTH + 1,
        PrincipalId::from_bytes([0x7a; 16]),
        &mut executor,
        &mut local_environment,
    )
    .expect_err("recursive CLIENT calls must stop at the closed depth cap");

    assert!(matches!(
        error,
        super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::RecursionLimit,
            ..
        }
    ));
}

#[test]
fn client_expression_call_depth_accepts_boundary_and_rejects_next_edge() {
    std::thread::Builder::new()
        .name("client-expression-depth-boundary".to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let boundary_edges = orna_artifact::client_plan::MAX_EXPRESSION_DEPTH + 1;
            let (boundary_prepared, boundary_function) =
                prepared_client_call_chain_with_state_root(boundary_edges);
            let boundary_active = active_from_prepared_candidate(&boundary_prepared);
            let mut boundary_state = ClientStateStore::new();

            let boundary_result = super::super::evaluate_client_function_with_state(
                &boundary_active,
                &authorise(boundary_active.pair(), boundary_function),
                &mut boundary_state,
            )
            .expect("the call at MAX_EXPRESSION_DEPTH must be accepted");
            assert_eq!(boundary_result.value(), &RuntimeValue::Boolean(true));
            assert_eq!(boundary_state.local().len(), 1);

            let (overflow_prepared, overflow_function) =
                prepared_client_call_chain_with_state_root(boundary_edges + 1);
            let overflow_active = active_from_prepared_candidate(&overflow_prepared);
            let mut overflow_state = ClientStateStore::new();
            let state_before_overflow = overflow_state.clone();

            let error = super::super::evaluate_client_function_with_state(
                &overflow_active,
                &authorise(overflow_active.pair(), overflow_function),
                &mut overflow_state,
            )
            .expect_err("the call after MAX_EXPRESSION_DEPTH must fail closed");

            assert!(matches!(
                error,
                super::super::ClientExecutionError::ExpressionEvaluation {
                    source: super::super::ClientExpressionError::RecursionLimit,
                    ..
                }
            ));
            assert_eq!(
                overflow_state, state_before_overflow,
                "a recursion-limit error must not commit staged state or resources"
            );
        })
        .expect("the depth-boundary test thread must start")
        .join()
        .expect("the depth-boundary test thread must complete");
}

#[test]
fn reference_root_field_path_loads_nested_records_under_authenticated_context() {
    let (
        active,
        context,
        parameter,
        outer_type,
        outer_object,
        outer_record,
        inner_object,
        inner_record,
        outer_field,
        inner_field,
        authorisation,
    ) = reference_field_path_fixture();
    let mut objects = HashMap::new();
    objects.insert(
        (outer_object, ObjectId::from_bytes([0x31; 16])),
        RuntimeValue::Record(outer_record),
    );
    objects.insert(
        (inner_object, ObjectId::from_bytes([0x32; 16])),
        RuntimeValue::Record(inner_record),
    );
    let mut state = ClientStateStore::new();
    state.set_security_context_digest(super::super::security_context_digest(&authorisation));
    state.set_reference_loader_fixture(ClientReferenceLoaderFixture {
        revision: active.pair(),
        principal: authorisation.session_principal(),
        security_context_digest: super::super::security_context_digest(&authorisation),
        objects,
    });
    let expression = orna_artifact::client_plan::ClientExpressionNode::FieldPath {
        root: parameter,
        fields: vec![outer_field, inner_field],
    };
    let mut executor: Option<&mut dyn super::super::ClientResourceExecutor> = None;
    let mut locals = super::super::ClientLocalEnvironment::new();
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Reference {
            target: outer_type,
            object: ObjectId::from_bytes([0x31; 16]),
        },
    )
    .unwrap();

    let value = super::super::evaluate_expression(
        &active,
        &expression,
        context,
        super::super::ObserverLineage::top_level(context.parent_invocation_id()),
        &[(argument.parameter(), argument.value().clone())],
        &[],
        &super::super::capability::LocalCapabilityGrantSet::new(),
        &mut state,
        0,
        authorisation.session_principal(),
        &mut executor,
        &mut locals,
    )
    .expect("trusted reference loader resolves nested field paths");

    assert_eq!(value, RuntimeValue::Text("Ada".to_owned()));
    let digest = super::super::client_security_context_digest(&authorisation);
    let mut host_state = ClientStateStore::new();
    host_state.set_security_context_digest(digest);
    host_state.install_reference_loader(
        ClientReferenceLoader::new(
            active.pair(),
            authorisation.session_principal(),
            digest,
            [
                ClientReferenceObject::new(
                    outer_object,
                    ObjectId::from_bytes([0x31; 16]),
                    vec![(
                        outer_field,
                        RuntimeValue::Reference {
                            target: inner_object,
                            object: ObjectId::from_bytes([0x32; 16]),
                        },
                    )],
                ),
                ClientReferenceObject::new(
                    inner_object,
                    ObjectId::from_bytes([0x32; 16]),
                    vec![(inner_field, RuntimeValue::Text("Ada".to_owned()))],
                ),
            ],
        )
        .unwrap(),
    );
    let mut host_executor: Option<&mut dyn super::super::ClientResourceExecutor> = None;
    let mut host_locals = super::super::ClientLocalEnvironment::new();
    let host_value = super::super::evaluate_expression(
        &active,
        &expression,
        context,
        super::super::ObserverLineage::top_level(context.parent_invocation_id()),
        &[(argument.parameter(), argument.value().clone())],
        &[],
        &super::super::capability::LocalCapabilityGrantSet::new(),
        &mut host_state,
        0,
        authorisation.session_principal(),
        &mut host_executor,
        &mut host_locals,
    )
    .expect("host-installed reference loader resolves nested field paths");
    assert_eq!(host_value, RuntimeValue::Text("Ada".to_owned()));
    let direct = super::super::evaluate_field_path(
        &active,
        &RuntimeValue::Record(
            state
                .reference_loader
                .as_ref()
                .unwrap()
                .objects
                .get(&(inner_object, ObjectId::from_bytes([0x32; 16])))
                .and_then(|value| match value {
                    RuntimeValue::Record(record) => Some(record.clone()),
                    _ => None,
                })
                .unwrap(),
        ),
        &[inner_field],
        context,
        authorisation.session_principal(),
        &state,
    )
    .expect("direct record field paths retain their existing behaviour");
    assert_eq!(direct, RuntimeValue::Text("Ada".to_owned()));
}

#[test]
fn client_reference_loader_rejects_duplicate_object_identities() {
    let target = TypeId::from_bytes([0xd1; 16]);
    let object = ObjectId::from_bytes([0xd2; 16]);
    let error = ClientReferenceLoader::new(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0xd3; 16]),
            CatalogueRevisionId::from_bytes([0xd4; 16]),
        ),
        PrincipalId::from_bytes([0xd5; 16]),
        Sha256Digest::from_bytes([0xd6; 32]),
        [
            ClientReferenceObject::new(target, object, Vec::new()),
            ClientReferenceObject::new(target, object, Vec::new()),
        ],
    )
    .expect_err("duplicate reference-object identities must fail closed");

    assert_eq!(
        error,
        ClientReferenceLoaderError::DuplicateIdentity { target, object }
    );
}

#[test]
fn client_function_arguments_match_requires_exact_ids_and_active_types() {
    let first_id = ParameterId::from_bytes([0xd7; 16]);
    let second_id = ParameterId::from_bytes([0xd8; 16]);
    let unknown_id = ParameterId::from_bytes([0xd9; 16]);
    let (active, function, _pair, _revision) = version_one_active_with_shape(
        FunctionDomain::Client,
        vec![
            ParameterDefinition::new(
                first_id,
                "first",
                0,
                ResolvedType::Scalar(StandardScalar::Integer),
                None,
            ),
            ParameterDefinition::new(
                second_id,
                "second",
                1,
                ResolvedType::Scalar(StandardScalar::Boolean),
                None,
            ),
        ],
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let definition = active
        .catalogue()
        .function_by_id(function)
        .expect("argument matcher fixture function is active");
    let first = FunctionArgument::new(first_id, RuntimeValue::Integer(7)).unwrap();
    let second = FunctionArgument::new(second_id, RuntimeValue::Boolean(true)).unwrap();

    assert!(super::super::client_function_arguments_match(
        &active,
        definition,
        &[first.clone(), second.clone()],
    ));
    assert!(!super::super::client_function_arguments_match(
        &active,
        definition,
        std::slice::from_ref(&first),
    ));
    assert!(!super::super::client_function_arguments_match(
        &active,
        definition,
        &[first.clone(), first.clone()],
    ));
    assert!(!super::super::client_function_arguments_match(
        &active,
        definition,
        &[
            first.clone(),
            FunctionArgument::new(unknown_id, RuntimeValue::Boolean(true)).unwrap(),
        ],
    ));
    assert!(!super::super::client_function_arguments_match(
        &active,
        definition,
        &[
            FunctionArgument::new(first_id, RuntimeValue::Boolean(true)).unwrap(),
            second,
        ],
    ));
}

#[test]
fn host_reference_loader_accepts_partial_fields_but_missing_requested_field_fails() {
    let (
        active,
        context,
        _parameter,
        outer_type,
        outer_object,
        _outer_record,
        _inner_object,
        _inner_record,
        outer_field,
        _inner_field,
        authorisation,
    ) = reference_field_path_fixture();
    let object = ObjectId::from_bytes([0x31; 16]);
    let digest = super::super::client_security_context_digest(&authorisation);
    let partial = ClientReferenceObject::new(outer_object, object, Vec::new());

    assert!(super::super::client_reference_object_is_active(
        &active,
        outer_object,
        object,
        &partial,
    ));
    assert!(!super::super::client_reference_object_is_active(
        &active,
        outer_object,
        object,
        &ClientReferenceObject::new(
            outer_object,
            object,
            vec![(
                FieldId::from_bytes([0xff; 16]),
                RuntimeValue::Reference {
                    target: outer_type,
                    object,
                },
            )],
        ),
    ));
    let field_value = RuntimeValue::Reference {
        target: _inner_object,
        object: ObjectId::from_bytes([0x32; 16]),
    };
    assert!(!super::super::client_reference_object_is_active(
        &active,
        outer_object,
        object,
        &ClientReferenceObject::new(
            outer_object,
            object,
            vec![
                (outer_field, field_value.clone()),
                (outer_field, field_value)
            ],
        ),
    ));
    assert!(!super::super::client_reference_object_is_active(
        &active,
        outer_object,
        object,
        &ClientReferenceObject::new(
            outer_object,
            object,
            vec![(outer_field, RuntimeValue::Text("wrong".to_owned()))],
        ),
    ));

    let mut state = ClientStateStore::new();
    state.set_security_context_digest(digest);
    state.install_reference_loader(
        ClientReferenceLoader::new(
            active.pair(),
            authorisation.session_principal(),
            digest,
            [partial],
        )
        .unwrap(),
    );
    let error = super::super::evaluate_field_path(
        &active,
        &RuntimeValue::Reference {
            target: outer_type,
            object,
        },
        &[outer_field],
        context,
        authorisation.session_principal(),
        &state,
    )
    .expect_err("an omitted requested field must remain a FieldPath failure");
    assert!(matches!(
        error,
        super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::FieldPath,
            ..
        }
    ));
}

#[test]
#[allow(clippy::result_large_err)]
fn reference_root_field_path_fails_closed_without_loader_or_object() {
    let (
        active,
        context,
        parameter,
        outer_type,
        _outer_object,
        _outer_record,
        _inner_object,
        _inner_record,
        outer_field,
        _inner_field,
        authorisation,
    ) = reference_field_path_fixture();
    let expression = orna_artifact::client_plan::ClientExpressionNode::FieldPath {
        root: parameter,
        fields: vec![outer_field],
    };
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Reference {
            target: outer_type,
            object: ObjectId::from_bytes([0x31; 16]),
        },
    )
    .unwrap();
    let evaluate = |state: &mut ClientStateStore, principal| {
        let mut executor: Option<&mut dyn super::super::ClientResourceExecutor> = None;
        let mut locals = super::super::ClientLocalEnvironment::new();
        super::super::evaluate_expression(
            &active,
            &expression,
            context,
            super::super::ObserverLineage::top_level(context.parent_invocation_id()),
            &[(argument.parameter(), argument.value().clone())],
            &[],
            &super::super::capability::LocalCapabilityGrantSet::new(),
            state,
            0,
            principal,
            &mut executor,
            &mut locals,
        )
    };

    let mut absent = ClientStateStore::new();
    assert!(matches!(
        evaluate(&mut absent, authorisation.session_principal()),
        Err(super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::FieldPath,
            ..
        })
    ));

    let mut objects = HashMap::new();
    objects.insert(
        (outer_type, ObjectId::from_bytes([0x31; 16])),
        RuntimeValue::Text("not a record".to_owned()),
    );
    let digest = super::super::security_context_digest(&authorisation);
    let mut wrong_type = ClientStateStore::new();
    wrong_type.set_security_context_digest(digest);
    wrong_type.set_reference_loader_fixture(ClientReferenceLoaderFixture {
        revision: active.pair(),
        principal: authorisation.session_principal(),
        security_context_digest: digest,
        objects,
    });
    assert!(matches!(
        evaluate(&mut wrong_type, authorisation.session_principal()),
        Err(super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::FieldPath,
            ..
        })
    ));

    let mut missing = ClientStateStore::new();
    missing.set_security_context_digest(digest);
    missing.set_reference_loader_fixture(ClientReferenceLoaderFixture {
        revision: active.pair(),
        principal: authorisation.session_principal(),
        security_context_digest: digest,
        objects: HashMap::new(),
    });
    assert!(matches!(
        evaluate(&mut missing, authorisation.session_principal()),
        Err(super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::FieldPath,
            ..
        })
    ));
}

#[test]
#[allow(clippy::result_large_err)]
fn reference_root_loader_isolated_by_principal_revision_and_unknown_field() {
    let (
        active,
        context,
        parameter,
        outer_type,
        outer_object,
        outer_record,
        _inner_object,
        _inner_record,
        outer_field,
        _inner_field,
        authorisation,
    ) = reference_field_path_fixture();
    let digest = super::super::security_context_digest(&authorisation);
    let mut objects = HashMap::new();
    objects.insert(
        (outer_object, ObjectId::from_bytes([0x31; 16])),
        RuntimeValue::Record(outer_record),
    );
    let fixture = ClientReferenceLoaderFixture {
        revision: active.pair(),
        principal: authorisation.session_principal(),
        security_context_digest: digest,
        objects,
    };
    let expression = |field| orna_artifact::client_plan::ClientExpressionNode::FieldPath {
        root: parameter,
        fields: vec![field],
    };
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Reference {
            target: outer_type,
            object: ObjectId::from_bytes([0x31; 16]),
        },
    )
    .unwrap();
    let evaluate = |state: &mut ClientStateStore, principal, field| {
        let mut executor: Option<&mut dyn super::super::ClientResourceExecutor> = None;
        let mut locals = super::super::ClientLocalEnvironment::new();
        super::super::evaluate_expression(
            &active,
            &expression(field),
            context,
            super::super::ObserverLineage::top_level(context.parent_invocation_id()),
            &[(argument.parameter(), argument.value().clone())],
            &[],
            &super::super::capability::LocalCapabilityGrantSet::new(),
            state,
            0,
            principal,
            &mut executor,
            &mut locals,
        )
    };

    let mut principal_mismatch = ClientStateStore::new();
    principal_mismatch.set_security_context_digest(digest);
    principal_mismatch.set_reference_loader_fixture(fixture.clone());
    assert!(matches!(
        evaluate(
            &mut principal_mismatch,
            PrincipalId::from_bytes([0x7b; 16]),
            outer_field
        ),
        Err(super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::FieldPath,
            ..
        })
    ));

    let mut revision_mismatch = ClientStateStore::new();
    revision_mismatch.set_security_context_digest(digest);
    revision_mismatch.set_reference_loader_fixture(ClientReferenceLoaderFixture {
        revision: RevisionPair::new(
            SourceRevisionId::from_bytes([0xf1; 16]),
            active.pair().catalogue(),
        ),
        ..fixture.clone()
    });
    assert!(matches!(
        evaluate(
            &mut revision_mismatch,
            authorisation.session_principal(),
            outer_field
        ),
        Err(super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::FieldPath,
            ..
        })
    ));

    let mut unknown_field = ClientStateStore::new();
    unknown_field.set_security_context_digest(digest);
    unknown_field.set_reference_loader_fixture(fixture);
    assert!(matches!(
        evaluate(
            &mut unknown_field,
            authorisation.session_principal(),
            FieldId::from_bytes([0xff; 16])
        ),
        Err(super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::FieldPath,
            ..
        })
    ));
}

fn assert_reordered_client_plan_rejects_before_executor(source: &str, function_name: &str) {
    let prepared = prepared_client_source_v6(source);
    let (active, function) = active_with_reordered_client_call_references(&prepared, function_name);
    let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Integer(1)));
    let error = super::super::evaluate_client_function_with_executor(
        &active,
        &authorise(active.pair(), function),
        &mut executor,
    )
    .expect_err("the durable call sequence must be checked before execution");

    assert!(matches!(
        error,
        super::super::ClientExecutionError::ExpressionEvaluation {
            context,
            source: super::super::ClientExpressionError::InvalidCall,
        } if context.function() == function
    ));
    assert!(executor.executed.is_empty());
}

#[test]
fn state_plan_preflights_defaults_before_return_expression() {
    assert_reordered_client_plan_rejects_before_executor(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.second() RETURNS INTEGER RETURN 2;
CREATE CLIENT FUNCTION app.owner() RETURNS INTEGER IS
  STATE value INTEGER DEFAULT app.first();
  BEGIN RETURN app.second(); END;"#,
        "app.owner",
    );
}

#[test]
fn procedural_plan_preflights_statements_before_return_expression() {
    assert_reordered_client_plan_rejects_before_executor(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.second() RETURNS INTEGER RETURN 2;
CREATE CLIENT FUNCTION app.owner() RETURNS INTEGER IS
  BEGIN
LET value INTEGER := app.first();
value := app.second();
RETURN value;
  END;"#,
        "app.owner",
    );
}

#[test]
fn programmable_client_control_flow_executes_compiled_source() {
    let prepared = prepared_client_source_v6(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.counter() RETURNS INTEGER IS
  LET total INTEGER := 0;
  BEGIN
WHILE total < 5 LOOP
  LET next INTEGER := total + 1;
  total := next;
END LOOP;
IF total = 5 THEN
  RETURN total;
ELSE
  RETURN 0;
END IF;
  END;"#,
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == "app.counter")
        .expect("the control-flow function is present")
        .id();

    let result = evaluate_client_function(&active, function)
        .expect("the compiled control-flow function evaluates successfully");

    assert_eq!(result.value(), &RuntimeValue::Integer(5));
}

#[test]
fn recursive_client_control_flow_uses_shared_execution_fuel() {
    let prepared = prepared_client_source_v6(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.factorial(p_n INTEGER) RETURNS INTEGER IS
  BEGIN
IF p_n <= 1 THEN
  RETURN 1;
ELSE
  RETURN p_n * app.factorial(p_n - 1);
END IF;
  END;"#,
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == "app.factorial")
        .expect("the recursive function is present")
        .id();
    let parameter = active
        .catalogue()
        .function_by_id(function)
        .expect("the recursive function definition is present")
        .parameters()[0]
        .id();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Integer(3)).expect("integer argument");

    let result = super::super::evaluate_client_function_with_arguments(
        &active,
        &authorise(active.pair(), function),
        &[argument],
    )
    .expect("the recursive control-flow function evaluates successfully");

    assert_eq!(result.value(), &RuntimeValue::Integer(6));
}
#[test]
fn recursive_client_control_flow_stops_at_depth_limit() {
    let prepared = prepared_client_source_v6(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.loop(p_n INTEGER) RETURNS INTEGER IS
  BEGIN
RETURN app.loop(p_n);
  END;"#,
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == "app.loop")
        .expect("the recursive function is present")
        .id();
    let parameter = active
        .catalogue()
        .function_by_id(function)
        .expect("the recursive function definition is present")
        .parameters()[0]
        .id();
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Integer(0)).expect("integer argument");

    let error = super::super::evaluate_client_function_with_arguments(
        &active,
        &authorise(active.pair(), function),
        &[argument],
    )
    .expect_err("recursive control flow must stop at the depth limit");

    assert!(matches!(
        error,
        super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::RecursionLimit,
            ..
        }
    ));
}

#[test]
fn rejects_non_boolean_short_circuit_operands_before_execution() {
    for (operator, left) in [
        (ControlFlowBinaryOperator::And, false),
        (ControlFlowBinaryOperator::Or, true),
    ] {
        let plan = ControlFlowClientPlan::new(
            Vec::new(),
            vec![orna_artifact::client_plan::ControlFlowStatement::return_(
                Some(ClientExpressionNode::Binary {
                    operator,
                    left: Box::new(ClientExpressionNode::Boolean { value: left }),
                    right: Box::new(ClientExpressionNode::Binary {
                        operator: ControlFlowBinaryOperator::And,
                        left: Box::new(ClientExpressionNode::Call {
                            function: FunctionId::from_bytes([6; 16]),
                            arguments: Vec::new(),
                        }),
                        right: Box::new(ClientExpressionNode::Integer { value: 1 }),
                    }),
                }),
            )],
        );
        let payload = plan
            .encode()
            .expect("malformed Boolean plan encodes structurally");
        let (active, function, pair, _) = version_two_active_with_artifact(
            standard_v6(),
            orna_standard::BOOLEAN_TYPE_ID,
            DefinitionReferenceTarget::Function(FunctionId::from_bytes([6; 16])),
            DefinitionReferenceKind::FunctionCall,
            orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION,
            payload,
        );
        let error = evaluate_client_function(&active, function)
            .expect_err("strict Boolean operands must be checked before short-circuiting");

        assert!(matches!(
            error,
            ClientExecutionError::ExpressionEvaluation {
                context,
                source: ClientExpressionError::TypeMismatch,
            } if context.pair() == pair && context.function() == function
        ));
    }
}

#[test]
fn action_plan_preflights_arguments_before_operation_target() {
    assert_reordered_client_plan_rejects_before_executor(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.owner() RETURNS std.Action AS
  std.action.call(
target => std.invoke.echo,
arguments => std.call.args(p_value => app.first())
  );"#,
        "app.owner",
    );
}

#[test]
fn action_plan_accepts_untampered_call_reference_order_and_builds_action() {
    let prepared = prepared_client_source_v6(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.owner() RETURNS std.Action AS
  std.action.call(
target => std.invoke.echo,
arguments => std.call.args(p_value => app.first())
  );"#,
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == "app.owner")
        .expect("the action owner is present")
        .id();
    let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Integer(7)));

    let result = super::super::evaluate_client_function_with_executor(
        &active,
        &authorise(active.pair(), function),
        &mut executor,
    )
    .expect("an untampered action plan evaluates successfully");

    assert!(matches!(result.value(), RuntimeValue::Opaque(_)));
    assert!(executor.executed.is_empty());
}

#[test]
fn source_introspection_exposes_parameters_and_function_references() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; \
         CREATE CLIENT FUNCTION app.target(p_value INTEGER) RETURNS INTEGER RETURN p_value; \
         CREATE CLIENT FUNCTION app.describe() RETURNS sys.source.function \
         RETURN sys.source.current();",
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active
        .catalogue()
        .functions()
        .iter()
        .find(|candidate| candidate.name().to_string() == "app.describe")
        .expect("the source-authored function is present")
        .id();

    let result = evaluate_client_function(&active, function)
        .expect("source introspection must execute through the public entry point");
    let RuntimeValue::Opaque(value) = result.value() else {
        panic!("source introspection must return an opaque metadata value");
    };
    let metadata =
        orna_core::source_metadata::SourceFunctionMetadata::decode(value.canonical_payload())
            .expect("the returned payload must decode as source metadata");
    assert_eq!(metadata.function(), function);
    assert_eq!(metadata.function_name(), "app.describe");
    assert!(metadata.parameters().is_empty());
    assert_eq!(metadata.references().len(), 1);
    assert_eq!(
        metadata.references()[0].target_name(),
        "sys.source.function"
    );
}
#[test]
fn source_reference_names_qualify_standard_parameter() {
    let standard = orna_standard::verify_standard_library_v9_snapshot(
        orna_standard::retained_standard_library_v9_snapshot().unwrap(),
    )
    .unwrap();
    let active = empty_version_two_active(&standard);
    let function = standard
        .catalogue()
        .function_by_id(orna_standard::STD_UI_TEXT_FUNCTION_ID)
        .expect("the standard text function is present");
    let parameter = function.parameters()[0].id();

    assert_eq!(
        super::super::source_reference_target_name(
            &active,
            DefinitionReferenceTarget::Parameter {
                owner: function.id(),
                parameter,
            },
        )
        .as_deref(),
        Some("std.ui.text.text"),
    );
}

#[test]
fn resource_plan_preflights_arguments_before_operation_target() {
    assert_reordered_client_plan_rejects_before_executor(
        r#"CREATE SCHEMA app;
CREATE CLIENT FUNCTION app.first() RETURNS INTEGER RETURN 1;
CREATE CLIENT FUNCTION app.owner() RETURNS INTEGER IS
  BEGIN
RETURN AWAIT std.data.resource(
  target => std.invoke.echo,
  arguments => std.call.args(p_value => app.first())
);
  END;"#,
        "app.owner",
    );
}

#[test]
fn capability_expression_calls_reject_reference_sequence_mismatch() {
    let function = FunctionId::from_bytes([6; 16]);
    let call = || orna_artifact::client_plan::ClientExpressionNode::Call {
        function,
        arguments: Vec::new(),
    };
    let expression = orna_artifact::client_plan::ClientExpressionNode::Concat {
        left: Box::new(call()),
        right: Box::new(call()),
    };
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        orna_artifact::client_plan::InnerClientPlan::Expression(
            orna_artifact::client_plan::ExpressionClientPlan::new(expression),
        ),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.read",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .expect("the capability expression plan encodes");
    let (active, function, pair, _) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::Function(function),
        DefinitionReferenceKind::FunctionCall,
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        payload,
    );

    let grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    let error = super::super::evaluate_client_function_with_grants(
        &active,
        &authorise(pair, function),
        &[],
        &grants,
    )
    .expect_err("the decoded call sequence must match durable references");

    assert!(matches!(
        error,
        super::super::ClientExecutionError::ExpressionEvaluation {
            context,
            source: super::super::ClientExpressionError::InvalidCall,
        } if context.function() == function
    ));
}

fn capability_direct_callee_denies_ungranted_declaration<F>(make_plan: F)
where
    F: FnOnce(FunctionId) -> orna_artifact::client_plan::InnerClientPlan,
{
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.first() RETURNS TEXT RETURN app.second(); CREATE CLIENT FUNCTION app.second() RETURNS TEXT RETURN 'ok';",
    );
    let initial = active_from_prepared_candidate(&prepared);
    let caller = initial
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.first")
        .expect("caller is present")
        .clone();
    let callee = initial
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().to_string() == "app.second")
        .expect("callee is present")
        .clone();
    let payload = orna_artifact::client_plan::CapabilityClientPlan::new(
        make_plan(callee.id()),
        vec![orna_artifact::client_plan::CapabilityRequirement::new(
            "std.fs.write",
            orna_artifact::client_plan::CapabilityArgumentSource::Text("/tmp".to_owned()),
        )],
    )
    .encode()
    .expect("the capability plan encodes");
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        "orna.client-plan",
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let current = initial
        .function_revisions()
        .iter()
        .find(|revision| revision.function() == caller.id())
        .expect("caller revision is present");
    let caller_references = initial
        .references()
        .iter()
        .filter(|reference| reference.source_function() == caller.id())
        .cloned()
        .collect::<Vec<_>>();
    let semantic_hash = function_semantic_digest_with_version(
        current.semantic_hash_version(),
        &caller,
        current.language_version(),
        &artifact,
        initial.expressions(),
        &caller_references,
    )
    .unwrap();
    let replacement = FunctionRevisionRecord::new(
        caller.id(),
        current.id(),
        current.revision_number(),
        current.declaration_origin(),
        current.declaration_content_hash(),
        semantic_hash,
        current.language_version(),
        artifact,
    )
    .unwrap()
    .with_semantic_hash_version(current.semantic_hash_version());
    let revisions = initial
        .function_revisions()
        .iter()
        .map(|revision| {
            if revision.function() == caller.id() {
                replacement.clone()
            } else {
                revision.clone()
            }
        })
        .collect::<Vec<_>>();
    let catalogue_hash = catalogue_digest_with_context(
        initial.catalogue_hash_context(),
        initial.catalogue(),
        &revisions,
        initial.expressions(),
        initial.origins(),
        initial.references(),
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            initial.pair(),
            initial.source().clone(),
            initial.catalogue().clone(),
            catalogue_hash,
            ActiveRevisionContent::new(
                initial.expressions().to_vec(),
                revisions,
                initial.origins().to_vec(),
                initial.references().to_vec(),
            ),
        ),
        initial.catalogue_hash_context().clone(),
    )
    .unwrap();
    let declaration = capability::LocalCapabilityDeclaration::new(
        capability::LocalCapabilityName::StdFsRead,
        capability::LocalCapabilityArgumentSource::Text("/tmp".to_owned()),
    );
    let write_grant = capability::LocalCapabilityGrant::new(
        capability::LocalCapabilityName::StdFsWrite,
        capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = capability::LocalCapabilityGrantSet::from_grants([write_grant]).unwrap();
    let error = super::super::evaluate_client_function_with_grants(
        &active,
        &authorise(active.pair(), caller.id()),
        &[declaration],
        &grants,
    )
    .expect_err("the direct callee must inherit the checked declaration context");
    assert!(matches!(
        error,
        super::super::ClientExecutionError::CapabilityDenied { context, capability }
            if context.function() == callee.id() && capability == "std.fs.read"
    ));
}

#[test]
fn capability_expression_calls_preserve_declarations_for_direct_callees() {
    capability_direct_callee_denies_ungranted_declaration(|callee| {
        orna_artifact::client_plan::InnerClientPlan::Expression(
            orna_artifact::client_plan::ExpressionClientPlan::new(
                orna_artifact::client_plan::ClientExpressionNode::Call {
                    function: callee,
                    arguments: Vec::new(),
                },
            ),
        )
    });
}

#[test]
fn capability_procedural_calls_preserve_declarations_for_direct_callees() {
    capability_direct_callee_denies_ungranted_declaration(|callee| {
        orna_artifact::client_plan::InnerClientPlan::Procedural(
            orna_artifact::client_plan::ProceduralClientPlan::new(
                Vec::new(),
                Vec::new(),
                orna_artifact::client_plan::ClientExpressionNode::Call {
                    function: callee,
                    arguments: Vec::new(),
                },
            ),
        )
    });
}

#[test]
fn transfers_the_evaluated_value_without_cloning_its_payload() {
    let (active, function, _, _) = version_one_active(true);

    assert_eq!(
        evaluate_client_function(&active, function)
            .unwrap()
            .into_value(),
        RuntimeValue::Boolean(true),
    );
}

#[test]
fn rejects_mismatched_authorisation_before_active_revision_validation() {
    let (active, function, pair, _) = version_one_active(true);
    let other_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x7b; 16]),
        CatalogueRevisionId::from_bytes([0x7c; 16]),
    );
    let untrusted = ActiveDatabaseRevision::new(
        active.pair(),
        active.source().clone(),
        active.catalogue().clone(),
        orna_core::revision::Sha256Digest::from_bytes([0x7d; 32]),
        active.expressions().to_vec(),
        active.function_revisions().to_vec(),
        active.origins().to_vec(),
        active.references().to_vec(),
    )
    .expect("tampered hash remains structurally valid");

    let error =
        super::super::evaluate_client_function(&untrusted, &authorise(other_pair, function))
            .expect_err("mismatched authorisation must fail");

    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), function);
    assert_eq!(error.context(), None);
    assert_eq!(
        error.to_string(),
        "the CLIENT authorisation does not match the active revision"
    );
    assert!(matches!(
        error,
        super::super::ClientExecutionError::AuthorisationMismatch {
            authorised,
            active,
        } if authorised == InvocationTarget::new(function, other_pair) && active == pair
    ));
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn rejects_an_active_revision_with_a_stale_catalogue_hash_before_function_checks() {
    let (active, _, pair, _) = version_one_active(true);
    let requested = FunctionId::from_bytes([0x8c; 16]);
    let stale = ActiveDatabaseRevision::new(
        active.pair(),
        active.source().clone(),
        active.catalogue().clone(),
        orna_core::revision::Sha256Digest::from_bytes([0x8a; 32]),
        active.expressions().to_vec(),
        active.function_revisions().to_vec(),
        active.origins().to_vec(),
        active.references().to_vec(),
    )
    .unwrap();

    let error = evaluate_client_function(&stale, requested).unwrap_err();

    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), requested);
    assert_eq!(error.context(), None);
    assert_eq!(error.to_string(), "the active revision cannot be trusted");
    assert!(matches!(
        error,
        super::super::ClientExecutionError::InvalidActiveRevision {
            source: super::super::ClientActiveRevisionError::CatalogueHashMismatch,
            ..
        }
    ));
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn wraps_a_canonical_active_semantics_failure_before_function_checks() {
    let (active, function, pair, function_revision) = version_one_active(true);
    let original = &active.function_revisions()[0];
    let inconsistent_revision = FunctionRevisionRecord::new(
        function,
        function_revision,
        original.revision_number(),
        original.declaration_origin(),
        original.declaration_content_hash(),
        orna_core::revision::Sha256Digest::from_bytes([0x8b; 32]),
        original.language_version(),
        original.artifact().clone(),
    )
    .unwrap();
    let untrusted = ActiveDatabaseRevision::new(
        active.pair(),
        active.source().clone(),
        active.catalogue().clone(),
        active.catalogue_hash(),
        active.expressions().to_vec(),
        vec![inconsistent_revision],
        active.origins().to_vec(),
        active.references().to_vec(),
    )
    .unwrap();

    let error = evaluate_client_function(&untrusted, function).unwrap_err();

    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), function);
    assert_eq!(error.context(), None);
    assert_eq!(error.to_string(), "the active revision cannot be trusted");
    assert!(matches!(
        error,
        super::super::ClientExecutionError::InvalidActiveRevision {
            source: super::super::ClientActiveRevisionError::Canonical(
                orna_core::canonical_hash::CanonicalHashError::FunctionSemanticHashMismatch {
                    function: actual_function,
                    revision: actual_revision,
                }
            ),
            ..
        } if actual_function == function && actual_revision == function_revision
    ));
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn rejects_a_mismatched_function_artifact_payload_hash_before_function_checks() {
    let (active, _, pair, _) = version_one_active(true);
    let requested = FunctionId::from_bytes([0x8d; 16]);
    let untrusted = active_with_mismatched_function_artifact_payload_hash(&active);

    let error = evaluate_client_function(&untrusted, requested).unwrap_err();

    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), requested);
    assert_eq!(error.context(), None);
    assert!(matches!(
        error,
        super::super::ClientExecutionError::InvalidActiveRevision {
            source: super::super::ClientActiveRevisionError::Canonical(
                orna_core::canonical_hash::CanonicalHashError::ArtifactPayloadHashMismatch {
                    artifact: "function artifact",
                }
            ),
            ..
        }
    ));
    assert_eq!(error.to_string(), "the active revision cannot be trusted");
    let source = std::error::Error::source(&error).unwrap();
    assert_eq!(
        source.to_string(),
        "function artifact payload hash differs from exact payload"
    );
    assert!(std::error::Error::source(source).is_some());
}

#[test]
fn client_evaluator_rejects_mismatched_payload_hash_before_resource_execution() {
    let (active, function, pair, _) = version_one_active(true);
    let untrusted = active_with_mismatched_function_artifact_payload_hash(&active);
    let mut state = ClientStateStore::new();
    let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Boolean(true)));
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);

    let error = super::super::evaluate_function(
        &untrusted,
        function,
        Vec::new(),
        &[],
        &capability::LocalCapabilityGrantSet::default(),
        &mut state,
        0,
        PrincipalId::from_bytes([0x7a; 16]),
        super::super::ObserverLineage::top_level(InvocationId::from_bytes([0xa1; 16])),
        &mut executor_slot,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientExecutionError::InvalidArtifact { context, .. }
            if context.pair() == pair && context.function() == function
    ));
    assert!(executor.executed.is_empty());
    assert!(executor.cancelled.is_empty());
}

#[test]
fn client_artifact_guard_rejects_server_kind_with_client_payload() {
    let (_active, function, pair, function_revision) = version_one_active(true);
    let payload = orna_artifact::client_plan::ClientPlan::return_boolean(true).encode();
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.client-plan",
        orna_artifact::client_plan::FORMAT_VERSION,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let context = super::super::ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::from_bytes([0xa2; 16]),
        observer_lineage: None,
    };

    let error = super::super::validate_artifact_identity(&artifact, context).unwrap_err();

    assert!(matches!(
        error,
        super::super::ClientExecutionError::InvalidArtifact { context: actual, .. }
            if actual == context
    ));
    assert_eq!(
        error.to_string(),
        "the saved CLIENT function cannot be evaluated"
    );
}

#[test]
fn public_active_revision_construction_preserves_client_evaluator_boundaries() {
    let (version_one, function, _, function_revision) = version_one_active(true);
    let value_type = TypeId::from_bytes([0x93; 16]);
    let value_reference = DefinitionReference::new(
        function,
        function_revision,
        0,
        DefinitionReferenceTarget::ValueType(value_type),
        DefinitionReferenceKind::NamedType,
        version_one.function_revisions()[0].declaration_origin(),
    );
    let version_two_revision = version_one.function_revisions()[0]
        .clone()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let error = ActiveDatabaseRevision::new(
        version_one.pair(),
        version_one.source().clone(),
        version_one.catalogue().clone(),
        version_one.catalogue_hash(),
        version_one.expressions().to_vec(),
        vec![version_two_revision],
        version_one.origins().to_vec(),
        vec![value_reference],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ValueTypeReferenceRequiresCatalogueHashVersionTwo {
            function: actual_function,
            revision: actual_revision,
            target,
        } if actual_function == function && actual_revision == function_revision && target == value_type
    ));
    assert_eq!(
        error.to_string(),
        "value-type references require catalogue hash version 2"
    );
    assert!(std::error::Error::source(&error).is_none());

    let error = ActiveDatabaseRevision::new(
        version_one.pair(),
        version_one.source().clone(),
        version_one.catalogue().clone(),
        version_one.catalogue_hash(),
        version_one.expressions().to_vec(),
        vec![
            version_one.function_revisions()[0]
                .clone()
                .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
        ],
        version_one.origins().to_vec(),
        Vec::new(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo {
            function: actual_function,
            revision: actual_revision,
        } if actual_function == function && actual_revision == function_revision
    ));
    assert_eq!(
        error.to_string(),
        "function semantic hash version 2 requires catalogue hash version 2"
    );
    assert!(std::error::Error::source(&error).is_none());

    let missing_target = TypeId::from_bytes([0x92; 16]);
    let error = ActiveDatabaseRevision::new(
        version_one.pair(),
        version_one.source().clone(),
        version_one.catalogue().clone(),
        version_one.catalogue_hash(),
        version_one.expressions().to_vec(),
        version_one.function_revisions().to_vec(),
        version_one.origins().to_vec(),
        vec![DefinitionReference::new(
            function,
            function_revision,
            0,
            DefinitionReferenceTarget::ObjectType(missing_target),
            DefinitionReferenceKind::ObjectReference,
            version_one.function_revisions()[0].declaration_origin(),
        )],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ReferenceTargetNotInRevision {
            target: DefinitionReferenceTarget::ObjectType(target),
        } if target == missing_target
    ));
    assert_eq!(
        error.to_string(),
        "reference target is absent from revision"
    );
    assert!(std::error::Error::source(&error).is_none());

    let prepared = prepared_client_functions();
    let active = active_from_prepared_candidate(&prepared);
    let prepared_function = active.catalogue().functions()[0].id();
    let current_revision = active.catalogue().functions()[0].current_revision();
    let selected = active
        .references()
        .iter()
        .find(|reference| reference.source_function() == prepared_function)
        .unwrap();
    assert!(matches!(
        selected.target(),
        DefinitionReferenceTarget::ValueType(_)
    ));
    let selected_target = match selected.target() {
        DefinitionReferenceTarget::ValueType(target) => target,
        _ => TypeId::from_bytes([0; 16]),
    };
    let unavailable_revision = FunctionRevisionId::from_bytes([0x94; 16]);
    let unavailable_reference = DefinitionReference::new(
        prepared_function,
        unavailable_revision,
        selected.ordinal(),
        selected.target(),
        selected.kind(),
        selected.source_origin(),
    );
    let error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            active.catalogue_hash(),
            ActiveRevisionContent::new(
                active.expressions().to_vec(),
                active.function_revisions().to_vec(),
                active.origins().to_vec(),
                vec![unavailable_reference],
            ),
        ),
        active.catalogue_hash_context().clone(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ValueTypeReferenceFunctionRevisionUnavailable {
            function: actual_function,
            revision,
            target,
        } if actual_function == prepared_function && revision == unavailable_revision && target == selected_target
    ));
    assert_eq!(
        error.to_string(),
        "cannot verify a value-type reference without its function revision record"
    );
    assert!(std::error::Error::source(&error).is_none());

    let version_one_revisions = active
        .function_revisions()
        .iter()
        .cloned()
        .map(|revision| revision.with_semantic_hash_version(FunctionSemanticHashVersion::Version1))
        .collect::<Vec<_>>();
    let error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            active.catalogue_hash(),
            ActiveRevisionContent::new(
                active.expressions().to_vec(),
                version_one_revisions,
                active.origins().to_vec(),
                active.references().to_vec(),
            ),
        ),
        active.catalogue_hash_context().clone(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo {
            function: actual_function,
            revision,
            target,
        } if actual_function == prepared_function && revision == current_revision && target == selected_target
    ));
    assert_eq!(
        error.to_string(),
        "value-type references require function semantic hash version 2"
    );
    assert!(std::error::Error::source(&error).is_none());

    let object = active.catalogue().object_types()[0].id();
    let kind_mismatch = DefinitionReference::new(
        prepared_function,
        current_revision,
        97,
        DefinitionReferenceTarget::ValueType(selected_target),
        DefinitionReferenceKind::ObjectReference,
        selected.source_origin(),
    );
    let error = active_with_extra_reference(&active, kind_mismatch).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ReferenceKindTargetMismatch {
            kind: DefinitionReferenceKind::ObjectReference,
            target: DefinitionReferenceTarget::ValueType(target),
        } if target == selected_target
    ));
    assert_eq!(
        error.to_string(),
        "reference kind cannot target that definition kind"
    );
    assert!(std::error::Error::source(&error).is_none());

    let duplicate = DefinitionReference::new(
        selected.source_function(),
        selected.source_revision(),
        selected.ordinal(),
        selected.target(),
        selected.kind(),
        selected.source_origin(),
    );
    let error = active_with_extra_reference(&active, duplicate).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::DuplicateReferenceOrdinal { revision, ordinal }
            if revision == current_revision && ordinal == selected.ordinal()
    ));
    assert_eq!(error.to_string(), "duplicate reference ordinal");
    assert!(std::error::Error::source(&error).is_none());

    let reference_not_in_catalogue = DefinitionReference::new(
        FunctionId::from_bytes([0x95; 16]),
        FunctionRevisionId::from_bytes([0x96; 16]),
        99,
        DefinitionReferenceTarget::ObjectType(object),
        DefinitionReferenceKind::ObjectReference,
        selected.source_origin(),
    );
    let error = active_with_extra_reference(&active, reference_not_in_catalogue).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ReferenceFunctionNotInCatalogue {
            function: actual_function,
            revision,
        } if actual_function == FunctionId::from_bytes([0x95; 16])
            && revision == FunctionRevisionId::from_bytes([0x96; 16])
    ));
    assert_eq!(
        error.to_string(),
        "reference function is absent from catalogue"
    );
    assert!(std::error::Error::source(&error).is_none());

    let stale_revision = FunctionRevisionId::from_bytes([0x97; 16]);
    let non_current_reference = DefinitionReference::new(
        prepared_function,
        stale_revision,
        99,
        DefinitionReferenceTarget::ObjectType(object),
        DefinitionReferenceKind::ObjectReference,
        selected.source_origin(),
    );
    let error = active_with_extra_reference(&active, non_current_reference).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::ReferenceRevisionNotCurrent {
            function: actual_function,
            expected,
            actual,
        } if actual_function == prepared_function && expected == current_revision && actual == stale_revision
    ));
    assert_eq!(
        error.to_string(),
        "reference revision is not catalogue current revision"
    );
    assert!(std::error::Error::source(&error).is_none());

    let unit_not_in_revision =
        SourceOrigin::new(SourceUnitId::from_bytes([0x98; 16]), 0, 0).unwrap();
    let error = active_with_replaced_first_origin(&version_one, unit_not_in_revision).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginUnitNotInRevision { source_unit }
            if source_unit == SourceUnitId::from_bytes([0x98; 16])
    ));
    assert_eq!(
        error.to_string(),
        "source origin unit is absent from stored revision"
    );
    assert!(std::error::Error::source(&error).is_none());

    let source_unit = version_one.source().units()[0].id();
    let out_of_bounds = SourceOrigin::new(
        source_unit,
        0,
        u32::try_from(version_one.source().units()[0].content().len() + 1).unwrap(),
    )
    .unwrap();
    let error = active_with_replaced_first_origin(&version_one, out_of_bounds).unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginOutOfBounds {
            source_unit: actual_unit,
            byte_start: 0,
            ..
        } if actual_unit == source_unit
    ));
    assert_eq!(
        error.to_string(),
        "source origin is outside stored source content"
    );
    assert!(std::error::Error::source(&error).is_none());

    let unicode_source = replacement_source(&version_one, "é");
    let split_character = SourceOrigin::new(source_unit, 1, 1).unwrap();
    let error = active_with_source_and_first_origin(&version_one, unicode_source, split_character)
        .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginNotCharacterBoundary {
            source_unit: actual_unit,
            byte_start: 1,
            byte_end: 1,
        } if actual_unit == source_unit
    ));
    assert_eq!(error.to_string(), "source origin splits a UTF-8 character");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn public_active_revision_construction_rejects_invalid_reference_source_origins() {
    let prepared = prepared_client_functions();
    let active = active_from_prepared_candidate(&prepared);
    let function = active.catalogue().functions()[0].id();
    let source_unit = active.source().units()[0].id();

    let error = active_with_replaced_reference_origin(
        &active,
        active.source().clone(),
        function,
        SourceOrigin::new(SourceUnitId::from_bytes([0x99; 16]), 0, 0).unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginUnitNotInRevision { source_unit: actual }
            if actual == SourceUnitId::from_bytes([0x99; 16])
    ));
    assert_eq!(
        error.to_string(),
        "source origin unit is absent from stored revision"
    );
    assert!(std::error::Error::source(&error).is_none());

    let error = active_with_replaced_reference_origin(
        &active,
        active.source().clone(),
        function,
        SourceOrigin::new(
            source_unit,
            0,
            u32::try_from(active.source().units()[0].content().len() + 1).unwrap(),
        )
        .unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginOutOfBounds {
            source_unit: actual,
            byte_start: 0,
            ..
        } if actual == source_unit
    ));
    assert_eq!(
        error.to_string(),
        "source origin is outside stored source content"
    );
    assert!(std::error::Error::source(&error).is_none());

    let unicode_source = replacement_source(
        &active,
        &format!("{}é", active.source().units()[0].content()),
    );
    let original_length = active.source().units()[0].content().len();
    let error = active_with_replaced_reference_origin(
        &active,
        unicode_source,
        function,
        SourceOrigin::new(
            source_unit,
            u32::try_from(original_length + 1).unwrap(),
            u32::try_from(original_length + 1).unwrap(),
        )
        .unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        RevisionInvariantError::SourceOriginNotCharacterBoundary {
            source_unit: actual,
            byte_start,
            byte_end,
        } if actual == source_unit
            && byte_start == u32::try_from(original_length + 1).unwrap()
            && byte_end == u32::try_from(original_length + 1).unwrap()
    ));
    assert_eq!(error.to_string(), "source origin splits a UTF-8 character");
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn stream_expression_rejects_scalar_literal_plan() {
    let payload = orna_artifact::client_plan::ExpressionClientPlan::new(
        orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true },
    )
    .encode()
    .unwrap();
    let (active, function, _, _) = version_two_client_stream_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        orna_artifact::client_plan::EXPRESSION_FORMAT_VERSION,
        payload,
    );
    let error = evaluate_client_function(&active, function).unwrap_err();
    assert!(matches!(
        error,
        super::super::ClientExecutionError::ExpressionEvaluation {
            source: super::super::ClientExpressionError::TypeMismatch,
            ..
        }
    ));
}

#[test]
fn stream_artifact_versions_reject_scalar_roots() {
    let scalar = orna_artifact::client_plan::ClientExpressionNode::Boolean { value: true };
    for (artifact_version, payload) in [
        (
            orna_artifact::client_plan::STATE_FORMAT_VERSION,
            orna_artifact::client_plan::StateClientPlan::new(
                scalar.clone(),
                vec![orna_artifact::client_plan::StateSlot::new(
                    StateSlotId::from_bytes([0x21; 16]),
                    orna_standard::BOOLEAN_TYPE_ID,
                    orna_artifact::client_plan::StateScope::User,
                    orna_artifact::client_plan::StateDefault::Unset,
                )],
            )
            .encode()
            .expect("the state plan encodes"),
        ),
        (
            orna_artifact::client_plan::RESOURCE_FORMAT_VERSION,
            orna_artifact::client_plan::ResourceClientPlan::new(
                orna_artifact::client_plan::ClientExpressionNode::Await {
                    expression: Box::new(
                        orna_artifact::client_plan::ClientExpressionNode::Resource {
                            operation: orna_artifact::client_plan::ResourceOperationNode::new(
                                orna_artifact::client_plan::ResourceKind::Scalar,
                                FunctionId::from_bytes([6; 16]),
                                RevisionPair::new(
                                    SourceRevisionId::from_bytes([1; 16]),
                                    CatalogueRevisionId::from_bytes([2; 16]),
                                ),
                                CallSiteId::from_bytes([0xe1; 16]),
                                Vec::new(),
                                orna_standard::BOOLEAN_TYPE_ID,
                            ),
                        },
                    ),
                },
            )
            .encode()
            .expect("the resource plan encodes"),
        ),
        (
            orna_artifact::client_plan::PROCEDURAL_FORMAT_VERSION,
            orna_artifact::client_plan::ProceduralClientPlan::new(Vec::new(), Vec::new(), scalar)
                .encode()
                .expect("the procedural plan encodes"),
        ),
    ] {
        let (active, function, _, _) = version_two_client_stream_active_with_artifact(
            standard_v6(),
            orna_standard::BOOLEAN_TYPE_ID,
            DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            artifact_version,
            payload,
        );
        let error = evaluate_client_function(&active, function).unwrap_err();
        if artifact_version == orna_artifact::client_plan::RESOURCE_FORMAT_VERSION {
            assert!(matches!(
                error,
                super::super::ClientExecutionError::ExpressionEvaluation {
                    source: super::super::ClientExpressionError::InvalidCall,
                    ..
                }
            ));
        } else {
            assert!(matches!(
                error,
                super::super::ClientExecutionError::ExpressionEvaluation {
                    source: super::super::ClientExpressionError::TypeMismatch,
                    ..
                }
            ));
        }
    }
}

#[test]
fn prepared_client_stream_shape_reaches_runtime_contract_boundary() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; \
         CREATE EXTERNAL CLIENT FUNCTION app.events() \
         RETURNS STREAM<BOOLEAN> RUNTIME CONTRACT 'app.events@1';",
    );
    let active = active_from_prepared_candidate(&prepared);
    let definition = &active.catalogue().functions()[0];
    assert!(matches!(
        definition.return_type(),
        FunctionReturn::Stream(ResolvedType::Value(type_id))
            if *type_id == orna_standard::BOOLEAN_TYPE_ID
    ));
    let function = definition.id();
    let error = evaluate_client_function(&active, function).unwrap_err();
    assert!(matches!(
        error,
        super::super::ClientExecutionError::ExternalContract { identity, .. }
            if identity == "app.events@1"
    ));
}

#[test]
fn compiler_emitted_v5_capability_gate_fails_closed_before_runtime() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; \
         CREATE EXTERNAL CLIENT FUNCTION app.read() \
         RETURNS BOOLEAN RUNTIME CONTRACT 'std.fs.read@1' \
         REQUIRES CAPABILITY std.fs.read('/tmp/input');",
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active.catalogue().functions()[0].id();
    let authorisation = authorise(active.pair(), function);

    let missing = super::super::evaluate_client_function_with_grants(
        &active,
        &authorisation,
        &[],
        &super::super::capability::LocalCapabilityGrantSet::new(),
    )
    .unwrap_err();
    assert!(matches!(
        missing,
        super::super::ClientExecutionError::CapabilityDenied { capability, .. }
            if capability == "std.fs.read"
    ));

    let grant = super::super::capability::LocalCapabilityGrant::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();
    // The local grant passes. The runtime contract is not installed in this evaluator,
    // so the next error must be the external-contract boundary.
    let unavailable =
        super::super::evaluate_client_function_with_grants(&active, &authorisation, &[], &grants)
            .unwrap_err();

    assert!(matches!(
        unavailable,
        super::super::ClientExecutionError::ExternalContract { identity, .. }
            if identity == "std.fs.read@1"
    ));
}

#[test]
fn evaluates_a_version_five_expression_parameter_read() {
    use orna_artifact::client_plan::{
        CapabilityArgumentSource, CapabilityClientPlan, CapabilityRequirement,
        ClientExpressionNode, ExpressionClientPlan, InnerClientPlan,
    };

    let parameter = ParameterId::from_bytes([0xb1; 16]);
    let payload = CapabilityClientPlan::new(
        InnerClientPlan::Expression(ExpressionClientPlan::new(
            ClientExpressionNode::ParameterRead { parameter },
        )),
        vec![CapabilityRequirement::new(
            "std.fs.read",
            CapabilityArgumentSource::Parameter("p_path".to_owned()),
        )],
    )
    .encode()
    .expect("the expression capability plan encodes");
    let (active, function, pair, _, parameter) =
        version_five_expression_active_with_parameter(payload);
    let authorisation = authorise(pair, function);
    let argument =
        FunctionArgument::new(parameter, RuntimeValue::Text("/tmp/input".to_owned())).unwrap();
    let grant = super::super::capability::LocalCapabilityGrant::new(
        super::super::capability::LocalCapabilityName::StdFsRead,
        super::super::capability::LocalCapabilityScope::path("/tmp").unwrap(),
    )
    .unwrap();
    let grants = super::super::capability::LocalCapabilityGrantSet::from_grants([grant]).unwrap();

    let result = super::super::evaluate_client_function_with_grants_and_arguments(
        &active,
        &authorisation,
        std::slice::from_ref(&argument),
        &[],
        &grants,
    )
    .expect("the version-five expression evaluates");

    assert_eq!(result.value(), &RuntimeValue::Text("/tmp/input".to_owned()));
    assert_eq!(result.context().function(), function);
    assert_eq!(result.context().pair(), active.pair());
}

#[test]
fn evaluates_native_session_input_expression() {
    let prepared = prepared_client_source(
        "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.prompt() RETURNS TEXT RETURN std.cli.input();",
    );
    let active = active_from_prepared_candidate(&prepared);
    let function = active.catalogue().functions()[0].id();
    let authorisation = authorise(active.pair(), function);
    let mut executor = RecordingActionExecutor::new(Some(RuntimeValue::Boolean(true)));

    let result = evaluate_client_function_with_executor(&active, &authorisation, &mut executor)
        .expect("native session input evaluates");

    assert_eq!(
        result.value(),
        &RuntimeValue::Text("from session".to_owned())
    );
}
#[test]
fn evaluates_prepared_version_two_client_constants() {
    for (literal, expected) in [("TRUE", true), ("FALSE", false)] {
        let prepared = prepared_client_constant(literal);
        let active = active_from_prepared_candidate(&prepared);
        let function = active.catalogue().functions()[0].id();

        let result = evaluate_client_function(&active, function).unwrap();

        assert_eq!(result.context().pair(), active.pair());
        assert_eq!(result.context().function(), function);
        assert_eq!(
            result.context().function_revision(),
            active.catalogue().functions()[0].current_revision()
        );
        assert_eq!(result.value(), &RuntimeValue::Boolean(expected));
    }
}

#[test]
fn evaluates_a_hand_built_version_two_value_return() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let boolean_type = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|definition| definition.representation_contract() == "orna.kernel.value.boolean@1")
        .unwrap()
        .id();
    let (active, function, pair, function_revision) =
        version_two_value_active(boolean_type, boolean_type);
    assert_eq!(
        active.function_revisions()[0].artifact().payload(),
        b"ORNACP\0\0\0\0\0\x01\x01\x01"
    );

    let result = evaluate_client_function(&active, function).unwrap();

    assert_eq!(result.context().pair(), pair);
    assert_eq!(result.context().function(), function);
    assert_eq!(result.context().function_revision(), function_revision);
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn evaluates_a_registered_opaque_client_result() {
    let payload = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let (active, function, pair, function_revision) =
        version_two_opaque_active(orna_standard::OPAQUE_TOKEN_TYPE_ID, payload);

    let result = evaluate_client_function(&active, function).unwrap();

    assert_eq!(result.context().pair(), pair);
    assert_eq!(result.context().function(), function);
    assert_eq!(result.context().function_revision(), function_revision);
    let RuntimeValue::Opaque(value) = result.value() else {
        panic!("opaque plan must produce one opaque value");
    };
    assert_eq!(value.opaque_type(), orna_standard::OPAQUE_TOKEN_TYPE_ID);
    assert_eq!(value.canonical_payload(), payload);
}

#[test]
fn evaluates_a_registered_opaque_ui_client_result() {
    let body = br#"{"kind":"empty"}"#;
    let mut payload = Vec::from(b"ORNA-UI/1 ".as_slice());
    payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
    payload.extend_from_slice(body);
    let plan = orna_artifact::client_plan::OpaqueClientPlan::return_opaque(
        orna_standard::STD_UI_TYPE_ID,
        payload.clone(),
    )
    .encode()
    .expect("opaque UI plan encodes");
    let (active, function, _, _) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::STD_UI_TYPE_ID,
        DefinitionReferenceTarget::ValueType(orna_standard::STD_UI_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        orna_artifact::client_plan::OPAQUE_FORMAT_VERSION,
        plan,
    );

    let result = evaluate_client_function(&active, function).unwrap();

    let RuntimeValue::Opaque(value) = result.value() else {
        panic!("opaque UI plan must produce one opaque value");
    };
    assert_eq!(value.opaque_type(), orna_standard::STD_UI_TYPE_ID);
    assert_eq!(value.canonical_payload(), payload);
}
#[test]
fn evaluates_v7_standard_client_external_contract_with_ordered_arguments() {
    let standard = standard_v7();
    let active = empty_version_two_active(&standard);
    let body = br#"{"kind":"empty"}"#;
    let mut payload = Vec::from(b"ORNA-UI/1 ".as_slice());
    payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
    payload.extend_from_slice(body);
    let registry = orna_standard::registered_opaque_codecs(
        active
            .catalogue_hash_context()
            .standard()
            .expect("the V7 fixture pins a standard snapshot"),
    )
    .expect("the V7 fixture has a registered UI codec");
    let content = RuntimeValue::Opaque(
        OpaqueValue::new(&active, &registry, orna_standard::STD_UI_TYPE_ID, payload)
            .expect("the UI argument has a valid opaque payload"),
    );
    let arguments = vec![
        (
            orna_standard::STD_UI_WINDOW_TITLE_PARAMETER_ID,
            RuntimeValue::Text("title".to_owned()),
        ),
        (
            orna_standard::STD_UI_WINDOW_CONTENT_PARAMETER_ID,
            content.clone(),
        ),
    ];
    let expected_arguments = arguments.clone();
    let returned = content.clone();
    let mut executor = DeterministicClientResourceExecutor::new(
        |_request: &ClientResourceRequest| -> Result<RuntimeValue, String> {
            Err("resource executor was not used".to_owned())
        },
    )
    .with_external_contract(
        move |request: &ClientExternalContractRequest| -> Result<RuntimeValue, String> {
            assert_eq!(
                request.identity(),
                orna_standard::STD_UI_WINDOW_RUNTIME_CONTRACT
            );
            assert_eq!(request.arguments(), expected_arguments.as_slice());
            Ok(returned.clone())
        },
    );
    let grants = capability::LocalCapabilityGrantSet::new();
    let mut state = ClientStateStore::new();
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
    let (_, value) = super::super::evaluate_function(
        &active,
        orna_standard::STD_UI_WINDOW_FUNCTION_ID,
        arguments,
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x5a; 16]),
        super::super::ObserverLineage::top_level(InvocationId::from_bytes([0x5b; 16])),
        &mut executor_slot,
    )
    .expect("the pinned V7 standard executable evaluates");
    assert_eq!(value, content);
}

#[test]
fn opaque_client_result_rejects_plan_type_and_structure_before_value_creation() {
    let payload = [0x5a; 16];
    let wrong_type = TypeId::from_bytes([0xa7; 16]);
    let (active, function, pair, function_revision) =
        version_two_opaque_active(wrong_type, payload);

    let error = evaluate_client_function(&active, function).unwrap_err();
    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), function);
    assert_eq!(
        error.context().map(|context| context.function_revision()),
        Some(function_revision)
    );
    assert!(matches!(
        error,
        super::super::ClientExecutionError::InvalidOpaqueValue {
            source: super::super::ClientOpaqueValueError::TypeMismatch {
                expected,
                actual,
            },
            ..
        } if expected == orna_standard::OPAQUE_TOKEN_TYPE_ID && actual == wrong_type
    ));
    assert_eq!(
        error.to_string(),
        "the saved CLIENT function cannot be evaluated"
    );
    let source = std::error::Error::source(&error).unwrap();
    assert_eq!(
        source.to_string(),
        "opaque CLIENT plan type does not match its function return"
    );
    assert!(std::error::Error::source(source).is_none());

    let mut malformed = orna_artifact::client_plan::OpaqueClientPlan::return_opaque(
        orna_standard::OPAQUE_TOKEN_TYPE_ID,
        payload,
    )
    .encode()
    .expect("opaque plan encodes");
    malformed[29..33].copy_from_slice(&15_u32.to_be_bytes());
    malformed.truncate(malformed.len() - 1);
    let (active, function, _, _) = version_two_value_active_with_artifact(
        orna_standard::OPAQUE_TOKEN_TYPE_ID,
        orna_standard::OPAQUE_TOKEN_TYPE_ID,
        2,
        malformed,
    );
    let error = evaluate_client_function(&active, function).unwrap_err();
    assert!(matches!(
        error,
        super::super::ClientExecutionError::InvalidOpaqueValue {
            source:
                super::super::ClientOpaqueValueError::Value(
                    super::super::OpaqueValueError::WrongPayloadLength {
                        opaque_type,
                        expected: 16,
                        actual: 15,
                    },
                ),
            ..
        } if opaque_type == orna_standard::OPAQUE_TOKEN_TYPE_ID
    ));
}

#[test]
fn rejects_a_value_return_that_disagrees_with_its_selected_reference() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let boolean_type = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|definition| definition.representation_contract() == "orna.kernel.value.boolean@1")
        .unwrap()
        .id();
    let alternate_type = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|definition| definition.id() != boolean_type)
        .unwrap()
        .id();
    let (active, function, pair, function_revision) =
        version_two_value_active(alternate_type, boolean_type);

    let error = evaluate_client_function(&active, function).unwrap_err();

    assert_eq!(error.pair(), pair);
    assert_eq!(error.function(), function);
    let context = error.context().expect("invalid function error context");
    assert_eq!(context.pair(), pair);
    assert_eq!(context.function(), function);
    assert_eq!(context.function_revision(), function_revision);
    assert!(matches!(
        error,
        super::super::ClientExecutionError::InvalidFunction {
            rule: super::super::ClientExecutionRule::References,
            ..
        }
    ));
    assert_eq!(
        error.to_string(),
        "this CLIENT function depends on unsupported definitions"
    );
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn version_two_reference_validation_uses_only_the_selected_current_function() {
    let prepared = prepared_client_functions();
    let active = active_from_prepared_candidate(&prepared);
    let functions = active.catalogue().functions();
    let first = functions[0].id();
    let second = functions[1].id();

    let result = evaluate_client_function(&active, first).unwrap();
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));

    let references = active
        .references()
        .iter()
        .filter(|reference| reference.source_function() == second)
        .cloned()
        .collect::<Vec<_>>();
    let b_only = active_from_prepared_with_references(&prepared, references);

    assert_references_rule(evaluate_client_function(&b_only, first), first);
    assert_eq!(
        evaluate_client_function(&b_only, second).unwrap().value(),
        &RuntimeValue::Boolean(true)
    );
}

#[test]
fn accepts_a_rehashed_self_consistent_selected_reference_origin() {
    let prepared = prepared_client_functions();
    let active = active_from_prepared_candidate(&prepared);
    let function = active.catalogue().functions()[0].id();
    let revision = active.catalogue().functions()[0].current_revision();
    let source = active.source().units()[0].content();
    let body_start = source.find("TRUE").unwrap();
    let replacement_origin = SourceOrigin::new(
        active.source().units()[0].id(),
        u32::try_from(body_start).unwrap(),
        u32::try_from(body_start + "TRUE".len()).unwrap(),
    )
    .unwrap();
    let mut references = active.references().to_vec();
    replace_reference(&mut references, function, |reference| {
        DefinitionReference::new(
            reference.source_function(),
            reference.source_revision(),
            reference.ordinal(),
            reference.target(),
            reference.kind(),
            replacement_origin,
        )
    });

    let stale = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            active.pair(),
            active.source().clone(),
            active.catalogue().clone(),
            active.catalogue_hash(),
            ActiveRevisionContent::new(
                active.expressions().to_vec(),
                active.function_revisions().to_vec(),
                active.origins().to_vec(),
                references.clone(),
            ),
        ),
        active.catalogue_hash_context().clone(),
    )
    .unwrap();
    let error = evaluate_client_function(&stale, function).unwrap_err();
    assert!(matches!(
        error,
        super::super::ClientExecutionError::InvalidActiveRevision {
            source: super::super::ClientActiveRevisionError::CatalogueHashMismatch,
            ..
        }
    ));
    assert_eq!(error.pair(), active.pair());
    assert_eq!(error.function(), function);
    assert_eq!(error.context(), None);
    assert_eq!(error.to_string(), "the active revision cannot be trusted");
    assert!(std::error::Error::source(&error).is_some());

    let repaired = active_from_prepared_with_references(&prepared, references);
    let result = evaluate_client_function(&repaired, function).unwrap();
    assert_eq!(result.context().pair(), repaired.pair());
    assert_eq!(result.context().function(), function);
    assert_eq!(result.context().function_revision(), revision);
    assert_eq!(result.value(), &RuntimeValue::Boolean(true));
}

#[test]
fn version_two_rejects_each_publicly_constructible_selected_reference_mismatch() {
    let prepared = prepared_client_functions();
    let active = active_from_prepared_candidate(&prepared);
    let function = active.catalogue().functions()[0].id();
    let reference = active
        .references()
        .iter()
        .find(|reference| reference.source_function() == function)
        .unwrap();
    assert!(matches!(
        active.catalogue_hash_context(),
        orna_core::revision::CatalogueHashContext::Version2 { .. }
    ));
    let standard = active.catalogue_hash_context().standard().unwrap();
    let alternate_value_type = standard
        .catalogue()
        .value_types()
        .iter()
        .find(|value_type| value_type.representation_contract() != "orna.kernel.value.boolean@1")
        .unwrap()
        .id();
    let object = active.catalogue().object_types()[0].id();

    let missing = active
        .references()
        .iter()
        .filter(|candidate| candidate.source_function() != function)
        .cloned()
        .collect::<Vec<_>>();
    assert_references_rule(
        evaluate_client_function(
            &active_from_prepared_with_references(&prepared, missing),
            function,
        ),
        function,
    );

    let mut extra = active.references().to_vec();
    extra.push(DefinitionReference::new(
        reference.source_function(),
        reference.source_revision(),
        1,
        reference.target(),
        reference.kind(),
        reference.source_origin(),
    ));
    assert_references_rule(
        evaluate_client_function(
            &active_from_prepared_with_references(&prepared, extra),
            function,
        ),
        function,
    );

    let mut wrong_ordinal = active.references().to_vec();
    replace_reference(&mut wrong_ordinal, function, |candidate| {
        DefinitionReference::new(
            candidate.source_function(),
            candidate.source_revision(),
            1,
            candidate.target(),
            candidate.kind(),
            candidate.source_origin(),
        )
    });
    let error = active_from_prepared_with_references_result(&prepared, wrong_ordinal).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<RevisionInvariantError>(),
        Some(RevisionInvariantError::ReferenceOrdinalOutOfSequence {
            expected: 0,
            actual: 1,
            ..
        })
    ));

    let mut wrong_target = active.references().to_vec();
    replace_reference(&mut wrong_target, function, |candidate| {
        DefinitionReference::new(
            candidate.source_function(),
            candidate.source_revision(),
            candidate.ordinal(),
            DefinitionReferenceTarget::ValueType(alternate_value_type),
            candidate.kind(),
            candidate.source_origin(),
        )
    });
    assert_references_rule(
        evaluate_client_function(
            &active_from_prepared_with_references(&prepared, wrong_target),
            function,
        ),
        function,
    );

    let mut wrong_kind_and_target = active.references().to_vec();
    replace_reference(&mut wrong_kind_and_target, function, |candidate| {
        DefinitionReference::new(
            candidate.source_function(),
            candidate.source_revision(),
            candidate.ordinal(),
            DefinitionReferenceTarget::ObjectType(object),
            DefinitionReferenceKind::ObjectReference,
            candidate.source_origin(),
        )
    });
    assert_references_rule(
        evaluate_client_function(
            &active_from_prepared_with_references(&prepared, wrong_kind_and_target),
            function,
        ),
        function,
    );

    let semantic_version_one = active_from_prepared_with_semantic_versions(
        &prepared,
        FunctionSemanticHashVersion::Version1,
        Vec::new(),
    );
    assert_references_rule(
        evaluate_client_function(&semantic_version_one, function),
        function,
    );
}

#[test]
fn expression_like_reference_validation_accepts_declared_ref_parameter_object_references() {
    let function_id = FunctionId::from_bytes([0xd1; 16]);
    let function_revision = FunctionRevisionId::from_bytes([0xd2; 16]);
    let parameter_id = ParameterId::from_bytes([0xd3; 16]);
    let object_type = TypeId::from_bytes([0xd4; 16]);
    let function = FunctionDefinition::new(
        function_id,
        QualifiedSemanticName::new(["action_fixture", "call"]).unwrap(),
        FunctionDomain::Client,
        vec![ParameterDefinition::new(
            parameter_id,
            "p_value",
            0,
            ResolvedType::reference(object_type),
            None,
        )],
        FunctionReturn::Single(ResolvedType::Value(orna_standard::STD_ACTION_TYPE_ID)),
        function_revision,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    let source_origin = SourceOrigin::new(SourceUnitId::from_bytes([0xd5; 16]), 0, 0).unwrap();
    let reference = |kind, target| {
        DefinitionReference::new(
            function_id,
            function_revision,
            0,
            target,
            kind,
            source_origin,
        )
    };

    assert!(super::super::is_expression_reference_allowed(
        Some(&function),
        &reference(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(object_type),
        ),
    ));
    assert!(!super::super::is_expression_reference_allowed(
        Some(&function),
        &reference(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(TypeId::from_bytes([0xd6; 16])),
        ),
    ));
    assert!(!super::super::is_expression_reference_allowed(
        Some(&function),
        &reference(
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(object_type),
        ),
    ));
    assert!(!super::super::is_expression_reference_allowed(
        Some(&function),
        &reference(
            DefinitionReferenceKind::QueryObject,
            DefinitionReferenceTarget::ObjectType(object_type),
        ),
    ));
}

#[test]
fn public_errors_and_rules_preserve_the_closed_adr0015_surface() {
    use orna_artifact::client_plan::ClientPlan;

    use super::super::{
        ClientActiveRevisionError, ClientExecutionContext, ClientExecutionError,
        ClientExecutionRule, ClientOpaqueValueError,
    };

    let (active, function, pair, function_revision) = version_one_active(true);
    let context = ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: orna_core::InvocationId::from_bytes([0; 16]),
        observer_lineage: None,
    };
    let rules = [
        (
            ClientExecutionRule::FunctionDomain,
            "this function does not run on the client",
        ),
        (
            ClientExecutionRule::Parameters,
            "this CLIENT function requires unsupported parameters",
        ),
        (
            ClientExecutionRule::ReturnType,
            "this CLIENT function has an unsupported return type",
        ),
        (
            ClientExecutionRule::Security,
            "this CLIENT function has an unsupported security mode",
        ),
        (
            ClientExecutionRule::Volatility,
            "this CLIENT function is not an immutable constant",
        ),
        (
            ClientExecutionRule::References,
            "this CLIENT function depends on unsupported definitions",
        ),
        (
            ClientExecutionRule::ArtifactFormat,
            "the saved CLIENT function uses an unsupported artefact format",
        ),
        (
            ClientExecutionRule::ArtifactVersion,
            "the saved CLIENT function uses an unsupported artefact version",
        ),
        (
            ClientExecutionRule::LanguageVersion,
            "the saved CLIENT function uses an unsupported language version",
        ),
    ];
    for (rule, display) in rules {
        assert_eq!(rule.to_string(), display);
        assert!(std::error::Error::source(&rule).is_none());
    }

    let mismatch = ClientActiveRevisionError::CatalogueHashMismatch;
    assert_eq!(
        mismatch.to_string(),
        "active revision catalogue hash differs from its canonical semantics"
    );
    assert!(std::error::Error::source(&mismatch).is_none());

    let not_found =
        evaluate_client_function(&active, FunctionId::from_bytes([0x77; 16])).unwrap_err();
    assert_eq!(not_found.pair(), pair);
    assert_eq!(not_found.function(), FunctionId::from_bytes([0x77; 16]));
    assert_eq!(not_found.context(), None);
    assert_eq!(
        not_found.to_string(),
        "the active revision does not contain this function"
    );
    assert!(std::error::Error::source(&not_found).is_none());

    let invalid = ClientExecutionError::InvalidFunction {
        context,
        rule: ClientExecutionRule::Security,
    };
    assert_eq!(invalid.pair(), pair);
    assert_eq!(invalid.function(), function);
    assert_eq!(invalid.context(), Some(&context));
    assert_eq!(
        invalid.to_string(),
        "this CLIENT function has an unsupported security mode"
    );
    assert!(std::error::Error::source(&invalid).is_none());

    let active_error = ClientExecutionError::InvalidActiveRevision {
        pair,
        function,
        source: mismatch,
    };
    assert_eq!(
        active_error.to_string(),
        "the active revision cannot be trusted"
    );
    assert!(std::error::Error::source(&active_error).is_some());

    let artifact_error = ClientPlan::decode(b"invalid").unwrap_err();
    let invalid_artifact = ClientExecutionError::InvalidArtifact {
        context,
        source: artifact_error,
    };
    assert!(invalid_artifact.context().is_some());
    assert!(std::error::Error::source(&invalid_artifact).is_some());
    assert_eq!(
        invalid_artifact.to_string(),
        "the saved CLIENT function cannot be evaluated"
    );

    let opaque_error = ClientOpaqueValueError::TypeMismatch {
        expected: orna_standard::OPAQUE_TOKEN_TYPE_ID,
        actual: TypeId::from_bytes([0x78; 16]),
    };
    assert_eq!(
        opaque_error.to_string(),
        "opaque CLIENT plan type does not match its function return"
    );
    assert!(std::error::Error::source(&opaque_error).is_none());
    let invalid_opaque = ClientExecutionError::InvalidOpaqueValue {
        context,
        source: opaque_error,
    };
    assert_eq!(invalid_opaque.pair(), pair);
    assert_eq!(invalid_opaque.function(), function);
    assert_eq!(invalid_opaque.context(), Some(&context));
    assert_eq!(
        invalid_opaque.to_string(),
        "the saved CLIENT function cannot be evaluated"
    );
    assert!(std::error::Error::source(&invalid_opaque).is_some());
}

#[test]
fn artefact_contract_failures_follow_closed_validation_after_active_trust() {
    let valid_payload = b"ORNACP\0\0\0\0\0\x01\x01\x01";
    let cases = [
        (
            "unsupported format",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                "other.format",
                1,
                valid_payload.to_vec(),
                artifact_payload_digest(valid_payload).unwrap(),
            )
            .unwrap(),
            "orna.language/1",
            Some(super::super::ClientExecutionRule::ArtifactFormat),
        ),
        (
            "unsupported version",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                "orna.client-plan",
                orna_artifact::client_plan::OPAQUE_FORMAT_VERSION,
                valid_payload.to_vec(),
                artifact_payload_digest(valid_payload).unwrap(),
            )
            .unwrap(),
            "orna.language/1",
            Some(super::super::ClientExecutionRule::ArtifactVersion),
        ),
        (
            "unsupported language",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                "orna.client-plan",
                1,
                valid_payload.to_vec(),
                artifact_payload_digest(valid_payload).unwrap(),
            )
            .unwrap(),
            "orna.language/2",
            Some(super::super::ClientExecutionRule::LanguageVersion),
        ),
        (
            "undecodable plan",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                "orna.client-plan",
                1,
                b"not a client plan".to_vec(),
                artifact_payload_digest(b"not a client plan").unwrap(),
            )
            .unwrap(),
            "orna.language/1",
            None,
        ),
    ];

    for (name, artifact, language, expected_rule) in cases {
        let (active, function, _, _) = version_one_active_with_artifact(artifact, language);
        let error = evaluate_client_function(&active, function).unwrap_err();

        assert_eq!(error.function(), function, "{name}");
        assert!(error.context().is_some(), "{name}");
        match expected_rule {
            Some(rule) => {
                assert!(matches!(
                    error,
                    super::super::ClientExecutionError::InvalidFunction { rule: actual, .. }
                        if actual == rule
                ));
                assert_eq!(error.to_string(), rule.to_string(), "{name}");
                assert!(std::error::Error::source(&error).is_none(), "{name}");
            }
            None => {
                assert!(matches!(
                    error,
                    super::super::ClientExecutionError::InvalidArtifact { .. }
                ));
                assert_eq!(
                    error.to_string(),
                    "the saved CLIENT function cannot be evaluated"
                );
                assert!(std::error::Error::source(&error).is_some());
            }
        }
    }
}

#[test]
fn function_shape_rules_are_public_and_follow_the_closed_precedence_order() {
    let cases = [
        (
            "domain before parameters",
            FunctionDomain::Server,
            vec![boolean_parameter()],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
            super::super::ClientExecutionRule::FunctionDomain,
        ),
        (
            "parameters before return type",
            FunctionDomain::Client,
            vec![boolean_parameter()],
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
            super::super::ClientExecutionRule::Parameters,
        ),
        (
            "return type before security",
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
            FunctionSecurity::Definer,
            FunctionVolatility::Immutable,
            super::super::ClientExecutionRule::ReturnType,
        ),
        (
            "security before volatility",
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Definer,
            FunctionVolatility::Stable,
            super::super::ClientExecutionRule::Security,
        ),
        (
            "volatility",
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Stable,
            super::super::ClientExecutionRule::Volatility,
        ),
    ];

    for (name, domain, parameters, return_type, security, volatility, rule) in cases {
        let (active, function, pair, function_revision) =
            version_one_active_with_shape(domain, parameters, return_type, security, volatility);
        let error = evaluate_client_function(&active, function).unwrap_err();

        assert_eq!(error.pair(), pair, "{name}");
        assert_eq!(error.function(), function, "{name}");
        let context = error.context().expect("invalid function error context");
        assert_eq!(context.pair(), pair, "{name}");
        assert_eq!(context.function(), function, "{name}");
        assert_eq!(context.function_revision(), function_revision, "{name}");
        assert!(matches!(
            error,
            super::super::ClientExecutionError::InvalidFunction { rule: actual, .. }
                if actual == rule
        ));
        assert_eq!(error.to_string(), rule.to_string(), "{name}");
        assert!(std::error::Error::source(&error).is_none(), "{name}");
    }
}

#[test]
fn version_one_public_evaluation_accepts_only_a_legacy_boolean_single_return() {
    for scalar in StandardScalar::ALL {
        let (active, function, _, _) = version_one_active_with_shape(
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(scalar)),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let result = evaluate_client_function(&active, function);
        if scalar == StandardScalar::Boolean {
            assert_eq!(result.unwrap().value(), &RuntimeValue::Boolean(true));
            continue;
        }
        let error = result.unwrap_err();
        assert_return_type_rule(error);
    }

    for return_type in [
        FunctionReturn::Single(ResolvedType::named(TypeId::from_bytes([0x71; 16]))),
        FunctionReturn::Single(ResolvedType::reference(TypeId::from_bytes([0x72; 16]))),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "value",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
        )]),
    ] {
        let (active, function, _, _) = version_one_active_with_shape(
            FunctionDomain::Client,
            Vec::new(),
            return_type,
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        assert_return_type_rule(evaluate_client_function(&active, function).unwrap_err());
    }
}
