use super::*;

#[test]
fn vm_admission_resolves_and_decodes_an_authorised_client_revision() {
    let (active, function, pair, _) = version_one_active(true);
    let authorisation = authorise(pair, function);
    let limits =
        super::super::vm::ClientVmArtifactLimits::new(1024, 64, 1024).expect("valid VM limits");
    let runtime_offer = super::super::vm::RuntimeOfferWitness::from_parts(
        1,
        0,
        "orna-runtime-test",
        "0.1.0",
        "test-build",
        "linux-x86_64",
        3,
        1,
        &[],
        &[],
    )
    .expect("valid runtime offer");
    let registry = super::super::vm::ClientVmInvocationRegistry::new();
    let mut host = super::super::vm::ClientVmHostContext::new(&registry, runtime_offer, limits)
        .expect("valid VM host");

    let admission = super::super::vm::admit_client_function(
        &active,
        &authorisation,
        &mut host,
        limits,
        &[],
        &[],
    )
    .expect("authorised client revision should be admitted");

    assert!(matches!(
        admission.plan(),
        super::super::vm::ClientVmDecodedPlan::Boolean(_)
    ));
    assert_eq!(admission.identity().function(), function.to_bytes());
    assert_eq!(
        admission.identity().function_revision(),
        active.function_revisions()[0].id().to_bytes()
    );
    assert_eq!(
        admission.host().security_context_digest(),
        authorisation.security_context_digest().to_bytes()
    );
    assert!(host.admission_is_current(&admission));
    host.advance_policy_epoch().expect("policy epoch");
    assert!(!host.admission_is_current(&admission));
}

#[test]
fn vm_admission_binds_full_capability_arguments_and_rejects_missing_parameters() {
    let capability_payload = |argument| {
        orna_artifact::client_plan::CapabilityClientPlan::new(
            orna_artifact::client_plan::InnerClientPlan::Boolean(
                orna_artifact::client_plan::ClientPlan::return_boolean(true),
            ),
            vec![orna_artifact::client_plan::CapabilityRequirement::new(
                "std.fs.read",
                argument,
            )],
        )
        .encode()
        .expect("capability payload")
    };
    let (active, function, pair, _) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        capability_payload(orna_artifact::client_plan::CapabilityArgumentSource::Text(
            "scope".to_owned(),
        )),
    );
    let authorisation = authorise(pair, function);
    let limits =
        super::super::vm::ClientVmArtifactLimits::new(1024, 64, 1024).expect("valid VM limits");
    let runtime_offer = || {
        super::super::vm::RuntimeOfferWitness::from_parts(
            1,
            0,
            "orna-runtime-test",
            "0.1.0",
            "test-build",
            "linux-x86_64",
            3,
            1,
            &[],
            &[],
        )
        .expect("valid runtime offer")
    };
    let registry = super::super::vm::ClientVmInvocationRegistry::new();
    let mut host = super::super::vm::ClientVmHostContext::new(&registry, runtime_offer(), limits)
        .expect("valid VM host");
    let declarations = [super::super::vm::ClientVmCapabilityDeclaration::new(
        "std.fs.read",
        super::super::vm::ClientVmCapabilityArgument::Text("scope".to_owned()),
    )];
    let admission = super::super::vm::admit_client_function(
        &active,
        &authorisation,
        &mut host,
        limits,
        &declarations,
        &[],
    )
    .expect("text capability argument should be admitted");
    assert!(matches!(
        admission.plan(),
        super::super::vm::ClientVmDecodedPlan::Capability(_)
    ));

    let (missing_active, _, missing_pair, _) = version_two_active_with_artifact(
        standard_v6(),
        orna_standard::BOOLEAN_TYPE_ID,
        DefinitionReferenceTarget::ValueType(orna_standard::BOOLEAN_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        orna_artifact::client_plan::CAPABILITY_FORMAT_VERSION,
        capability_payload(
            orna_artifact::client_plan::CapabilityArgumentSource::Parameter("missing".to_owned()),
        ),
    );
    let missing_authorisation = authorise(missing_pair, function);
    let mut missing_host =
        super::super::vm::ClientVmHostContext::new(&registry, runtime_offer(), limits)
            .expect("valid second VM host");
    let missing_declarations = [super::super::vm::ClientVmCapabilityDeclaration::new(
        "std.fs.read",
        super::super::vm::ClientVmCapabilityArgument::Parameter("missing".to_owned()),
    )];
    assert!(matches!(
        super::super::vm::admit_client_function(
            &missing_active,
            &missing_authorisation,
            &mut missing_host,
            limits,
            &missing_declarations,
            &[],
        ),
        Err(super::super::vm::ClientVmAdmissionError::SemanticRejected)
    ));
}

#[test]
fn evaluates_version_one_client_constants() {
    for value in [true, false] {
        let (active, function, pair, function_revision) = version_one_active(value);

        let result = evaluate_client_function(&active, function).unwrap();

        assert_eq!(result.context().pair(), pair);
        assert_eq!(result.context().function(), function);
        assert_eq!(result.context().function_revision(), function_revision);
        assert_eq!(result.value(), &RuntimeValue::Boolean(value));
    }
}

#[test]
fn resource_request_rejects_nul_invocation_context_before_loading() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7b; 16]);
    let digest = super::super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0x23; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));

    for (profile, instance) in [
        ("profile\0invalid", "instance"),
        ("profile", "instance\0invalid"),
    ] {
        let context = super::super::ClientResourceInvocationContext::new(
            InvocationId::from_bytes([0x24; 16]),
            CallSiteId::from_bytes([0x25; 16]),
            profile.to_owned(),
            instance.to_owned(),
        );
        assert!(matches!(
            resource.begin_request_with_context(&active, context, Vec::new()),
            Err(super::super::ClientResourceError::InvalidInvocationContext)
        ));
        assert_eq!(resource.status(), super::super::ClientResourceStatus::Idle);
        assert_eq!(resource.generation().value(), 0);
    }
}

#[test]
fn resource_request_rejects_zero_lineage_before_loading() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7c; 16]);
    let digest = super::super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0x24; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));

    for (parent_invocation_id, call_site_id) in [
        (
            InvocationId::from_bytes([0; 16]),
            CallSiteId::from_bytes([0x25; 16]),
        ),
        (
            InvocationId::from_bytes([0x24; 16]),
            CallSiteId::from_bytes([0; 16]),
        ),
    ] {
        let context = super::super::ClientResourceInvocationContext::new(
            parent_invocation_id,
            call_site_id,
            "profile".to_owned(),
            "instance".to_owned(),
        );
        assert_eq!(
            resource.begin_request_with_context(&active, context, Vec::new()),
            Err(super::super::ClientResourceError::InvalidInvocationContext),
        );
        assert_eq!(resource.status(), super::super::ClientResourceStatus::Idle);
        assert_eq!(resource.generation().value(), 0);
        assert_eq!(resource.request_id(), None);
    }

    let context = super::super::ClientResourceInvocationContext::new(
        InvocationId::from_bytes([0x24; 16]),
        CallSiteId::from_bytes([0x25; 16]),
        "profile".to_owned(),
        "instance".to_owned(),
    );
    let request = resource
        .begin_request_with_context(&active, context.clone(), Vec::new())
        .unwrap();
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
    assert_eq!(resource.generation().value(), 1);
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(request.invocation_context(), Some(context));
}

#[test]
fn client_resource_lifecycle_rejects_stale_and_invalid_results() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        Sha256Digest::from_bytes([0x11; 32]),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));

    assert_eq!(resource.status(), super::super::ClientResourceStatus::Idle);
    assert_eq!(resource.generation().value(), 0);

    let first = resource.begin_loading().unwrap();
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
    assert_eq!(first.value(), 1);
    assert_eq!(
        resource.publish_ready(
            &active,
            super::super::ClientResourceGeneration(0),
            RuntimeValue::Boolean(true),
        ),
        Err(super::super::ClientResourceError::StaleGeneration {
            expected: first,
            actual: super::super::ClientResourceGeneration(0),
        }),
    );
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );

    resource
        .publish_ready(&active, first, RuntimeValue::Boolean(true))
        .unwrap();
    assert_eq!(resource.status(), super::super::ClientResourceStatus::Ready);
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

    let second = resource.begin_loading().unwrap();
    assert_eq!(resource.value(), None);
    assert_eq!(
        resource.publish_failure(second, String::new()),
        Err(super::super::ClientResourceError::InvalidFailureCode),
    );
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
    resource
        .publish_failure(second, "network.timeout".to_owned())
        .unwrap();
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Failed
    );
    assert_eq!(
        resource
            .failure()
            .map(super::super::ClientResourceFailure::code),
        Some("network.timeout"),
    );

    let third = resource.begin_loading().unwrap();
    resource.cancel(third).unwrap();
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Cancelled
    );
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);
    assert_eq!(
        resource.publish_failure(third, "late".to_owned()),
        Err(super::super::ClientResourceError::InvalidTransition {
            status: super::super::ClientResourceStatus::Cancelled,
        }),
    );

    resource.invalidate().unwrap();
    assert_eq!(resource.status(), super::super::ClientResourceStatus::Idle);
    assert_eq!(resource.generation().value(), 4);
}

#[test]
fn client_action_argument_error_preserves_display_and_equality() {
    let resource_error = super::super::ClientResourceError::DuplicateArgument {
        parameter: ParameterId::from_bytes([0x7b; 16]),
    };
    let action_error = super::super::ClientActionError::Arguments(Box::new(resource_error.clone()));

    assert_eq!(action_error.to_string(), resource_error.to_string());
    assert_eq!(
        action_error,
        super::super::ClientActionError::Arguments(Box::new(resource_error)),
    );
}

#[test]
fn client_resource_rejects_completion_with_mismatched_request_key() {
    let (active, function, pair, _) = version_one_active(true);
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0x11; 32]),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let wrong_key = super::super::ClientResourceKey::new(
        key.target(),
        key.principal(),
        Sha256Digest::from_bytes([0xaa; 32]),
        key.invalidation_token(),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let generation = resource.begin_loading().unwrap();
    let request_id = resource.request_id().unwrap();
    let completion = super::super::ClientResourceCompletion::Ready {
        request_id,
        key: wrong_key,
        generation,
        value: RuntimeValue::Boolean(true),
    };
    let before = resource.clone();

    let error = resource
        .apply_completion(&active, completion)
        .expect_err("the completion key must be rejected");
    assert_eq!(
        error,
        super::super::ClientResourceError::RequestKeyMismatch {
            expected: Box::new(key),
            actual: Box::new(wrong_key),
        }
    );
    assert_eq!(
        error.to_string(),
        format!(
            "CLIENT resource completion uses key {:?}, expected {:?}",
            wrong_key, key,
        ),
    );
    assert_eq!(resource, before);
}

#[test]
fn client_resource_rejects_completion_with_mismatched_request_id() {
    let (active, function, pair, _) = version_one_active(true);
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        super::super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, Vec::new()).unwrap();
    let completion = super::super::ClientResourceCompletion::Ready {
        request_id: InvocationId::from_bytes([0xff; 16]),
        key,
        generation: request.generation(),
        value: RuntimeValue::Boolean(true),
    };
    let before = resource.clone();

    assert_eq!(
        resource.apply_completion(&active, completion),
        Err(super::super::ClientResourceError::RequestIdMismatch {
            expected: request.request_id(),
            actual: InvocationId::from_bytes([0xff; 16]),
        })
    );
    assert_eq!(resource, before);
}

#[test]
fn client_resource_ready_value_must_match_declared_type() {
    let (active, function, pair, _) = version_one_active(true);
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0x31; 32]),
        Sha256Digest::from_bytes([0x32; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let generation = resource.begin_loading().unwrap();

    assert_eq!(
        resource.publish_ready(&active, generation, RuntimeValue::Integer(4)),
        Err(super::super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
    assert_eq!(resource.value(), None);
}

#[test]
fn client_resource_rejects_expected_type_that_differs_from_target_declaration() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0x33; 32]),
        Sha256Digest::from_bytes([0x34; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Integer));
    let generation = resource.begin_loading().unwrap();

    assert_eq!(
        resource.publish_ready(&active, generation, RuntimeValue::Integer(7)),
        Err(super::super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
    assert_eq!(resource.value(), None);
}

#[test]
fn client_resource_rejects_completion_from_a_different_revision() {
    let (active, function, _, _) = version_one_active(true);
    let resource_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x7b; 16]),
        CatalogueRevisionId::from_bytes([0x7c; 16]),
    );
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, resource_pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0x41; 32]),
        Sha256Digest::from_bytes([0x42; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let generation = resource.begin_loading().unwrap();

    assert_eq!(
        resource.publish_ready(&active, generation, RuntimeValue::Boolean(true)),
        Err(super::super::ClientResourceError::RevisionMismatch {
            expected: resource_pair,
            actual: active.pair(),
        }),
    );
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
    assert_eq!(resource.value(), None);
}

#[test]
fn client_resource_rejects_terminal_completion_after_active_revision_changes() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x7d; 16]),
        CatalogueRevisionId::from_bytes([0x7e; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let arguments_digest =
        super::super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        arguments_digest,
        Sha256Digest::from_bytes([0x52; 32]),
    );

    let mut pending_resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let pending_request = pending_resource.begin_request(&active, Vec::new()).unwrap();
    let before_pending = pending_resource.clone();
    assert_eq!(
        pending_resource.apply_completion(&changed_active, pending_request.pending()),
        Err(super::super::ClientResourceError::RevisionMismatch {
            expected: pair,
            actual: changed_pair,
        }),
    );
    assert_eq!(pending_resource, before_pending);

    let mut failed_resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let failed_request = failed_resource.begin_request(&active, Vec::new()).unwrap();
    let before_failed = failed_resource.clone();
    assert_eq!(
        failed_resource
            .apply_completion(&changed_active, failed_request.failed("stale".to_owned()),),
        Err(super::super::ClientResourceError::RevisionMismatch {
            expected: pair,
            actual: changed_pair,
        }),
    );
    assert_eq!(failed_resource, before_failed);

    let mut cancelled_resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let cancelled_request = cancelled_resource
        .begin_request(&active, Vec::new())
        .unwrap();
    let before_cancelled = cancelled_resource.clone();
    assert_eq!(
        cancelled_resource.apply_completion(&changed_active, cancelled_request.cancelled()),
        Err(super::super::ClientResourceError::RevisionMismatch {
            expected: pair,
            actual: changed_pair,
        }),
    );
    assert_eq!(cancelled_resource, before_cancelled);
}

#[test]
fn client_resource_executor_validates_arguments_and_applies_completion() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Client,
        vec![
            ParameterDefinition::new(
                ParameterId::from_bytes([0x02; 16]),
                "count",
                0,
                ResolvedType::Scalar(StandardScalar::Integer),
                None,
            ),
            ParameterDefinition::new(
                ParameterId::from_bytes([0x01; 16]),
                "enabled",
                1,
                ResolvedType::Scalar(StandardScalar::Boolean),
                None,
            ),
        ],
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let first = FunctionArgument::new(
        ParameterId::from_bytes([0x02; 16]),
        RuntimeValue::Integer(7),
    )
    .unwrap();
    let second = FunctionArgument::new(
        ParameterId::from_bytes([0x01; 16]),
        RuntimeValue::Boolean(true),
    )
    .unwrap();
    let arguments = vec![first.clone(), second.clone()];
    let digest =
        super::super::ClientResourceKey::canonical_arguments_digest(&active, &arguments).unwrap();
    assert_eq!(
        digest,
        super::super::ClientResourceKey::canonical_arguments_digest(
            &active,
            &[second.clone(), first.clone()],
        )
        .unwrap(),
    );
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let mut executor = super::super::DeterministicClientResourceExecutor::new(
        |request: &super::super::ClientResourceRequest| {
            assert_eq!(request.arguments()[0].parameter(), second.parameter());
            assert_eq!(request.arguments()[1].parameter(), first.parameter());
            Ok(RuntimeValue::Boolean(true))
        },
    );

    let request = resource
        .begin_request(&active, vec![first.clone(), second.clone()])
        .unwrap();
    assert_eq!(request.arguments()[0].parameter(), second.parameter());
    let first_request_id = request.request_id();
    let completion = super::super::ClientResourceExecutor::execute(&mut executor, request);
    resource.apply_completion(&active, completion).unwrap();
    assert_eq!(resource.status(), super::super::ClientResourceStatus::Ready);
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

    let second_request = resource
        .begin_request(&active, vec![first, second])
        .unwrap();
    assert_ne!(second_request.request_id(), first_request_id);
    let failed = second_request.failed("resource.denied".to_owned());
    resource.apply_completion(&active, failed).unwrap();
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Failed
    );
    assert_eq!(
        resource
            .failure()
            .map(super::super::ClientResourceFailure::code),
        Some("resource.denied"),
    );
}

#[test]
fn client_resource_rejects_over_limit_arguments_before_cloning_or_hashing() {
    let (active, function, pair, _) = version_one_active(true);
    let arguments = (0..=super::super::MAX_RESOURCE_ARGUMENTS)
        .map(|index| {
            let mut bytes = [0_u8; 16];
            bytes[14..].copy_from_slice(&(index as u16).to_be_bytes());
            FunctionArgument::new(
                ParameterId::from_bytes(bytes),
                RuntimeValue::Boolean(index % 2 == 0),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let expected_error = super::super::ClientResourceError::ResourceArgumentLimitExceeded {
        limit: super::super::MAX_RESOURCE_ARGUMENTS,
    };

    assert_eq!(
        super::super::ClientResourceKey::canonical_arguments_digest(&active, &arguments),
        Err(expected_error.clone()),
    );

    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0x61; 32]),
        Sha256Digest::from_bytes([0x62; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    assert_eq!(
        resource.begin_request(&active, arguments),
        Err(expected_error)
    );
    assert_eq!(resource.status(), super::super::ClientResourceStatus::Idle);
    assert_eq!(resource.generation().value(), 0);
}

#[test]
fn client_resource_pending_completion_preserves_loading_until_resume() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let digest = super::super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, Vec::new()).unwrap();
    let generation = request.generation();
    let request_id = request.request_id();

    resource
        .apply_completion(&active, request.pending())
        .expect("pending completion should retain the active generation");
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);

    resource
        .apply_completion(
            &active,
            super::super::ClientResourceCompletion::Ready {
                request_id,
                key,
                generation,
                value: RuntimeValue::Boolean(true),
            },
        )
        .expect("the matching completion should resume the resource");
    assert_eq!(resource.status(), super::super::ClientResourceStatus::Ready);
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));
}

#[test]
fn resource_executor_poll_surfaces_pending_completion_without_affecting_immediate_executor() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );

    let mut pending_resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let pending_request = pending_resource.begin_request(&active, vec![]).unwrap();
    let pending_request_id = pending_request.request_id();
    let expected_pending = pending_request.clone().pending();
    let mut polling = PollingTestExecutor::default();
    assert_eq!(polling.execute(pending_request), expected_pending);
    assert_eq!(
        polling.poll(),
        Some(ClientResourceCompletion::Ready {
            request_id: pending_request_id,
            key,
            generation: pending_resource.generation(),
            value: RuntimeValue::Boolean(true)
        })
    );
    assert_eq!(polling.poll(), None);

    let mut immediate_resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let immediate_request = immediate_resource.begin_request(&active, vec![]).unwrap();
    let mut immediate = DeterministicClientResourceExecutor::new(|_: &ClientResourceRequest| {
        Ok(RuntimeValue::Boolean(true))
    });
    assert!(matches!(
        immediate.execute(immediate_request),
        ClientResourceCompletion::Ready { .. }
    ));
    assert_eq!(immediate.poll(), None);
}
#[test]
fn default_executor_cancel_keeps_pending_ownership() {
    let (active, function, pair, _) = version_one_active(true);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0x23; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, vec![]).unwrap();
    let mut executor = PollingTestExecutor {
        pending: Some(request.clone()),
    };

    assert_eq!(executor.cancel(request.clone()), request.clone().pending());
    assert_eq!(executor.pending, Some(request));
}

#[test]
fn client_resource_cancelled_completion_terminates_current_generation() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let digest = super::super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, Vec::new()).unwrap();

    resource
        .apply_completion(&active, request.cancelled())
        .expect("matching cancellation should terminate the active generation");

    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Cancelled
    );
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);
}

#[test]
fn client_stream_request_preserves_batch_order_and_returns_terminal_option() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    assert_eq!(request.kind(), ResourceKind::Stream);

    resource
        .apply_completion(
            &active,
            request.clone().stream_values(vec![
                RuntimeValue::Boolean(true),
                RuntimeValue::Boolean(false),
            ]),
        )
        .unwrap();
    resource
        .apply_completion(
            &active,
            request.clone().stream_values(vec![
                RuntimeValue::Boolean(false),
                RuntimeValue::Boolean(true),
            ]),
        )
        .unwrap();
    resource
        .apply_completion(&active, request.stream_completed())
        .unwrap();

    let first = resource.take_stream_value(&active).unwrap().unwrap();
    assert_boolean_stream_batch(first, &[true, false]);
    let second = resource.take_stream_value(&active).unwrap().unwrap();
    assert_boolean_stream_batch(second, &[false, true]);
    let terminal = resource.take_stream_value(&active).unwrap().unwrap();
    assert_boolean_stream_terminal(terminal);
}

#[test]
fn client_record_stream_batches_append_and_take_nominal_type_ids() {
    let (active, function, pair, _, record_type, other_record_type) =
        version_two_server_record_stream_active();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x26; 32]),
    );
    let mut resource = ClientResource::new_stream(key, ResolvedType::Named(record_type));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    let record = RuntimeValue::Record(
        RecordValue::new(
            &active,
            record_type,
            [(String::from("title"), RuntimeValue::Boolean(true))],
        )
        .unwrap(),
    );
    let mismatched = RuntimeValue::Record(
        RecordValue::new(
            &active,
            other_record_type,
            [(String::from("title"), RuntimeValue::Boolean(false))],
        )
        .unwrap(),
    );

    resource
        .apply_completion(&active, request.clone().stream_values(vec![record.clone()]))
        .unwrap();
    let before_mismatch = resource.clone();
    assert_eq!(
        resource.apply_completion(&active, request.clone().stream_values(vec![mismatched])),
        Err(super::super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(resource, before_mismatch);

    resource
        .apply_completion(&active, request.stream_completed())
        .unwrap();
    let batch = resource.take_stream_value(&active).unwrap().unwrap();
    let RuntimeValue::Constructed(option) = batch else {
        panic!("record stream batch must be a constructed OPTION");
    };
    let ConstructedValueKind::Option(Some(list)) = option.kind() else {
        panic!("record stream batch must contain a present LIST");
    };
    let RuntimeValue::Constructed(list) = list else {
        panic!("record stream OPTION must contain a constructed LIST");
    };
    let ConstructedValueKind::List(values) = list.kind() else {
        panic!("record stream OPTION must contain a LIST");
    };
    assert_eq!(values, [record].as_slice());

    let terminal = resource.take_stream_value(&active).unwrap().unwrap();
    assert_boolean_stream_terminal(terminal);
}

#[test]
fn client_stream_rejects_scalar_ready_completion() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x23; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();

    assert_eq!(
        resource.publish_ready(&active, request.generation(), RuntimeValue::Boolean(true)),
        Err(super::super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
    assert_eq!(resource.value(), None);
    assert!(!resource.stream_complete());
}

#[test]
fn client_stream_rejects_oversized_batches_and_totals_before_queueing() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x23; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();

    let oversized_batch =
        vec![RuntimeValue::Boolean(true); super::super::MAX_RESOURCE_BATCH_ITEMS + 1];
    assert_eq!(
        resource.apply_completion(&active, request.clone().stream_values(oversized_batch),),
        Err(super::super::ClientResourceError::TypeMismatch),
    );
    assert!(resource.stream_batches.is_empty());
    assert_eq!(resource.stream_total_items, 0);

    resource.stream_total_items = super::super::MAX_RESOURCE_TOTAL_ITEMS;
    assert_eq!(
        resource.apply_completion(
            &active,
            request.stream_values(vec![RuntimeValue::Boolean(true)]),
        ),
        Err(super::super::ClientResourceError::TypeMismatch),
    );
    assert!(resource.stream_batches.is_empty());
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
}

#[test]
fn client_stream_queue_overflow_preserves_existing_batches() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x24; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    let batch = vec![RuntimeValue::Boolean(true); super::super::MAX_RESOURCE_BATCH_ITEMS];
    resource
        .apply_completion(&active, request.clone().stream_values(batch))
        .unwrap();
    let before = resource.clone();

    assert_eq!(
        resource.apply_completion(
            &active,
            request.stream_values(vec![RuntimeValue::Boolean(false)]),
        ),
        Err(super::super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(resource, before);
}

#[test]
fn client_stream_queue_dequeue_releases_capacity() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x25; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    for _ in 0..super::super::MAX_RESOURCE_BATCH_ITEMS {
        resource
            .apply_completion(
                &active,
                request
                    .clone()
                    .stream_values(vec![RuntimeValue::Boolean(true)]),
            )
            .unwrap();
    }
    assert_eq!(
        resource.stream_queued_items,
        super::super::MAX_RESOURCE_QUEUED_ITEMS
    );
    resource.take_stream_value(&active).unwrap().unwrap();
    assert_eq!(
        resource.stream_queued_items,
        super::super::MAX_RESOURCE_QUEUED_ITEMS - 1
    );

    resource
        .apply_completion(
            &active,
            request.stream_values(vec![RuntimeValue::Boolean(false)]),
        )
        .unwrap();
    assert_eq!(
        resource.stream_queued_items,
        super::super::MAX_RESOURCE_QUEUED_ITEMS
    );
}

#[test]
fn client_stream_failure_drains_queued_batches_before_evaluator_reports_failure() {
    let (active, function, pair, function_revision) = version_two_server_stream_active();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let request = resource.begin_stream_request(&active, Vec::new()).unwrap();
    resource
        .apply_completion(
            &active,
            request
                .clone()
                .stream_values(vec![RuntimeValue::Boolean(true)]),
        )
        .unwrap();
    resource
        .apply_completion(
            &active,
            request
                .clone()
                .stream_values(vec![RuntimeValue::Boolean(false)]),
        )
        .unwrap();
    resource
        .apply_completion(&active, request.failed("stream.failed".to_owned()))
        .unwrap();

    let context = ClientExecutionContext {
        pair,
        function,
        function_revision,
        parent_invocation_id: InvocationId::from_bytes([0xf6; 16]),
        observer_lineage: None,
    };
    let first = super::super::read_stream_resource_value(&active, &mut resource, context).unwrap();
    assert_boolean_stream_batch(first, &[true]);
    let second = super::super::read_stream_resource_value(&active, &mut resource, context).unwrap();
    assert_boolean_stream_batch(second, &[false]);
    assert!(matches!(
        super::super::read_stream_resource_value(&active, &mut resource, context),
        Err(super::super::ClientExecutionError::ResourceEvaluation {
            source: super::super::ClientResourceExecutionError::Failed(code),
            ..
        }) if code == "stream.failed"
    ));
}

#[test]
fn client_stream_cancellation_clears_batches_and_rejects_stale_completions() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        ClientResource::new_stream(key, ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID));
    let first = resource.begin_stream_request(&active, Vec::new()).unwrap();
    let second = resource.begin_stream_request(&active, Vec::new()).unwrap();
    resource
        .apply_completion(
            &active,
            second
                .clone()
                .stream_values(vec![RuntimeValue::Boolean(true)]),
        )
        .unwrap();
    resource
        .apply_completion(&active, second.clone().cancelled())
        .unwrap();

    assert_eq!(
        resource.take_stream_value(&active),
        Err(super::super::ClientResourceError::InvalidTransition {
            status: super::super::ClientResourceStatus::Cancelled,
        })
    );
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Cancelled
    );
    assert_eq!(resource.failure(), None);
    assert!(matches!(
        resource.apply_completion(
            &active,
            first.stream_values(vec![RuntimeValue::Boolean(false)]),
        ),
        Err(super::super::ClientResourceError::StaleGeneration { .. })
    ));
    assert_eq!(
        resource.apply_completion(&active, second.stream_completed()),
        Err(super::super::ClientResourceError::InvalidTransition {
            status: super::super::ClientResourceStatus::Cancelled,
        })
    );
}

fn assert_boolean_stream_batch(value: RuntimeValue, expected: &[bool]) {
    let RuntimeValue::Constructed(option) = value else {
        panic!("stream value must be a constructed OPTION");
    };
    let orna_core::value::ConstructedValueKind::Option(Some(list)) = option.kind() else {
        panic!("stream value must contain a present LIST");
    };
    let RuntimeValue::Constructed(list) = list else {
        panic!("stream OPTION must contain a constructed LIST");
    };
    let orna_core::value::ConstructedValueKind::List(values) = list.kind() else {
        panic!("stream OPTION must contain a LIST");
    };
    let expected = expected
        .iter()
        .copied()
        .map(RuntimeValue::Boolean)
        .collect::<Vec<_>>();
    assert_eq!(values, expected.as_slice());
}

fn assert_boolean_stream_terminal(value: RuntimeValue) {
    let RuntimeValue::Constructed(option) = value else {
        panic!("stream terminal must be a constructed OPTION");
    };
    assert_eq!(
        option.kind(),
        orna_core::value::ConstructedValueKind::Option(None)
    );
}

#[test]
fn stream_descriptor_rejects_unsupported_scalar_items() {
    for scalar in [
        StandardScalar::Decimal,
        StandardScalar::Uuid,
        StandardScalar::Date,
        StandardScalar::Time,
        StandardScalar::Timestamp,
        StandardScalar::Duration,
        StandardScalar::Void,
    ] {
        assert!(super::super::stream_item_descriptor(ResolvedType::Scalar(scalar)).is_none());
    }
}

#[test]
fn stream_await_expression_and_procedural_local_return_option_list_values() {
    let (active, target, pair, target_revision) = version_two_server_stream_active();
    let item_type = ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID);
    let operation = orna_artifact::client_plan::ResourceOperationNode::new(
        ResourceKind::Stream,
        target,
        pair,
        CallSiteId::from_bytes([0x91; 16]),
        Vec::new(),
        orna_standard::BOOLEAN_TYPE_ID,
    );
    let expression = orna_artifact::client_plan::ClientExpressionNode::Await {
        expression: Box::new(orna_artifact::client_plan::ClientExpressionNode::Resource {
            operation: operation.clone(),
        }),
    };
    let context = super::super::ClientExecutionContext {
        pair,
        function: target,
        function_revision: target_revision,
        parent_invocation_id: InvocationId::from_bytes([0x92; 16]),
        observer_lineage: None,
    };
    let grants = capability::LocalCapabilityGrantSet::new();
    let mut state = ClientStateStore::new();
    let mut executor = StreamBatchTestExecutor { value: true };
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
    let mut locals = std::collections::HashMap::new();
    let value = super::super::evaluate_expression_plan(
        &active,
        &expression,
        context,
        super::super::ObserverLineage::compatibility(context),
        item_type,
        &[],
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x93; 16]),
        &mut executor_slot,
        &mut locals,
    )
    .expect("stream AWAIT must be checked against its OPTION<LIST<T>> result");
    assert_boolean_stream_batch(value, &[true]);

    let local = LocalId::from_bytes([0x94; 16]);
    let procedural = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![orna_artifact::client_plan::ClientLocal::new(
            local,
            orna_standard::BOOLEAN_TYPE_ID,
            orna_artifact::client_plan::ClientLocalKind::Resource(ResourceKind::Stream),
        )],
        vec![
            orna_artifact::client_plan::ClientStatement::let_(
                local,
                orna_artifact::client_plan::ClientExpressionNode::Resource {
                    operation: operation.clone(),
                },
            ),
            orna_artifact::client_plan::ClientStatement::assignment(
                local,
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
            ),
        ],
        orna_artifact::client_plan::ClientExpressionNode::Await {
            expression: Box::new(
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local },
            ),
        },
    );
    let mut state = ClientStateStore::new();
    let mut executor = StreamBatchTestExecutor { value: false };
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
    let mut locals = std::collections::HashMap::new();
    let value = super::super::evaluate_procedural_plan(
        &active,
        &procedural,
        context,
        super::super::ObserverLineage::compatibility(context),
        item_type,
        false,
        &[],
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x93; 16]),
        &mut executor_slot,
        &mut locals,
    )
    .expect("procedural stream AWAIT must preserve the outer result shape");
    assert_boolean_stream_batch(value, &[false]);

    let value_local = LocalId::from_bytes([0x95; 16]);
    let copy_local = LocalId::from_bytes([0x96; 16]);
    let value_procedural = orna_artifact::client_plan::ProceduralClientPlan::new(
        vec![
            orna_artifact::client_plan::ClientLocal::new(
                value_local,
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::ClientLocalKind::Value,
            ),
            orna_artifact::client_plan::ClientLocal::new(
                copy_local,
                orna_standard::BOOLEAN_TYPE_ID,
                orna_artifact::client_plan::ClientLocalKind::Value,
            ),
        ],
        vec![
            orna_artifact::client_plan::ClientStatement::let_(
                value_local,
                orna_artifact::client_plan::ClientExpressionNode::Await {
                    expression: Box::new(
                        orna_artifact::client_plan::ClientExpressionNode::Resource {
                            operation: operation.clone(),
                        },
                    ),
                },
            ),
            orna_artifact::client_plan::ClientStatement::let_(
                copy_local,
                orna_artifact::client_plan::ClientExpressionNode::Boolean { value: false },
            ),
            orna_artifact::client_plan::ClientStatement::assignment(
                copy_local,
                orna_artifact::client_plan::ClientExpressionNode::LocalRead { local: value_local },
            ),
        ],
        orna_artifact::client_plan::ClientExpressionNode::LocalRead { local: copy_local },
    );
    let mut state = ClientStateStore::new();
    let mut executor = StreamBatchTestExecutor { value: true };
    let mut executor_slot: Option<&mut dyn ClientResourceExecutor> = Some(&mut executor);
    let mut locals = std::collections::HashMap::new();
    let value = super::super::evaluate_procedural_plan(
        &active,
        &value_procedural,
        context,
        super::super::ObserverLineage::compatibility(context),
        item_type,
        false,
        &[],
        &[],
        &grants,
        &mut state,
        0,
        PrincipalId::from_bytes([0x93; 16]),
        &mut executor_slot,
        &mut locals,
    )
    .expect("a value local containing stream AWAIT must preserve its outer result shape");
    assert_boolean_stream_batch(value, &[true]);
}

#[test]
fn client_resource_ready_completion_wins_over_late_cancellation() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Client,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let digest = super::super::ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));
    let request = resource.begin_request(&active, Vec::new()).unwrap();
    let generation = request.generation();
    let late_cancellation = request.clone().cancelled();

    resource
        .apply_completion(&active, request.ready(RuntimeValue::Boolean(true)))
        .expect("the accepted completion should make the resource ready");
    assert_eq!(resource.status(), super::super::ClientResourceStatus::Ready);
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

    assert_eq!(
        resource.cancel(generation),
        Err(super::super::ClientResourceError::InvalidTransition {
            status: super::super::ClientResourceStatus::Ready,
        }),
    );
    assert_eq!(
        resource.apply_completion(&active, late_cancellation),
        Err(super::super::ClientResourceError::InvalidTransition {
            status: super::super::ClientResourceStatus::Ready,
        }),
    );
    assert_eq!(resource.status(), super::super::ClientResourceStatus::Ready);
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));
}

#[test]
fn client_resource_executor_rejects_digest_duplicates_stale_and_cancelled() {
    let (active, function, pair, _) = version_one_active_with_shape(
        FunctionDomain::Client,
        vec![ParameterDefinition::new(
            ParameterId::from_bytes([0x01; 16]),
            "enabled",
            0,
            ResolvedType::Scalar(StandardScalar::Boolean),
            None,
        )],
        FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0x01; 16]),
        RuntimeValue::Boolean(true),
    )
    .unwrap();
    let digest = super::super::ClientResourceKey::canonical_arguments_digest(
        &active,
        std::slice::from_ref(&argument),
    )
    .unwrap();
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0x22; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Boolean));

    let wrong_key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        key.principal(),
        Sha256Digest::from_bytes([0xaa; 32]),
        key.invalidation_token(),
    );
    let mut wrong_resource =
        super::super::ClientResource::new(wrong_key, ResolvedType::Scalar(StandardScalar::Boolean));
    assert!(matches!(
        wrong_resource.begin_request(&active, vec![argument.clone()]),
        Err(super::super::ClientResourceError::ArgumentDigestMismatch { .. }),
    ));
    assert_eq!(
        wrong_resource.status(),
        super::super::ClientResourceStatus::Idle
    );

    assert_eq!(
        resource.begin_request(&active, vec![argument.clone(), argument.clone()]),
        Err(super::super::ClientResourceError::DuplicateArgument {
            parameter: argument.parameter(),
        }),
    );
    assert_eq!(resource.status(), super::super::ClientResourceStatus::Idle);

    let first = resource
        .begin_request(&active, vec![argument.clone()])
        .unwrap();
    let second = resource.begin_request(&active, vec![argument]).unwrap();
    let first_completion = first.ready(RuntimeValue::Boolean(false));
    assert!(matches!(
        resource.apply_completion(&active, first_completion),
        Err(super::super::ClientResourceError::StaleGeneration { .. }),
    ));
    let second_generation = second.generation();
    resource.cancel(second_generation).unwrap();
    assert!(matches!(
        resource.apply_completion(&active, second.ready(RuntimeValue::Boolean(true))),
        Err(super::super::ClientResourceError::InvalidTransition {
            status: super::super::ClientResourceStatus::Cancelled,
        }),
    ));
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Cancelled
    );
    assert_eq!(resource.value(), None);
}

#[test]
fn client_resource_accepts_supported_scalar_runtime_values() {
    let cases = [
        (
            ResolvedType::Scalar(StandardScalar::BigInt),
            RuntimeValue::BigInt(42),
        ),
        (
            ResolvedType::Scalar(StandardScalar::Float),
            RuntimeValue::Float(RuntimeFloat::new(4.25).unwrap()),
        ),
        (
            ResolvedType::Scalar(StandardScalar::BinaryLargeObject),
            RuntimeValue::Bytes(vec![0x01, 0x02]),
        ),
    ];

    for (index, (expected, value)) in cases.into_iter().enumerate() {
        let (active, function, pair, _) = version_one_active_with_shape(
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(expected),
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        );
        let key = super::super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0x50 + index as u8; 32]),
            Sha256Digest::from_bytes([0x60 + index as u8; 32]),
        );
        let mut resource = super::super::ClientResource::new(key, expected);
        let generation = resource.begin_loading().unwrap();
        resource
            .publish_ready(&active, generation, value)
            .expect("supported scalar value should publish");
        assert_eq!(resource.status(), super::super::ClientResourceStatus::Ready);
    }
}

#[test]
fn client_resource_accepts_standard_value_contracts() {
    let cases = [
        (orna_standard::BIGINT_TYPE_ID, RuntimeValue::BigInt(42)),
        (
            orna_standard::FLOAT_TYPE_ID,
            RuntimeValue::Float(RuntimeFloat::new(4.25).unwrap()),
        ),
        (
            orna_standard::BINARY_LARGE_OBJECT_TYPE_ID,
            RuntimeValue::Bytes(vec![0x01, 0x02]),
        ),
    ];

    for (index, (type_id, value)) in cases.into_iter().enumerate() {
        let (active, function, pair, _) = version_two_value_active(type_id, type_id);
        let key = super::super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0x90 + index as u8; 32]),
            Sha256Digest::from_bytes([0xa0 + index as u8; 32]),
        );
        let mut resource = super::super::ClientResource::new(key, ResolvedType::Value(type_id));
        let generation = resource.begin_loading().unwrap();

        resource
            .publish_ready(&active, generation, value)
            .expect("standard value contract should publish");
        assert_eq!(resource.status(), super::super::ClientResourceStatus::Ready);
    }
}

#[test]
fn client_resource_requires_the_full_verified_standard_target_pin() {
    let (active, function, pair, _) = version_two_value_active(
        orna_standard::BOOLEAN_TYPE_ID,
        orna_standard::BOOLEAN_TYPE_ID,
    );
    let wrong_target = InvocationTarget::verified_standard(
        function,
        pair,
        orna_core::StandardLibraryRevisionId::from_bytes([0xee; 16]),
        FunctionRevisionId::from_bytes([0xef; 16]),
    );
    let wrong_key = super::super::ClientResourceKey::new(
        wrong_target,
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0xb1; 32]),
        Sha256Digest::from_bytes([0xb2; 32]),
    );
    let mut resource =
        super::super::ClientResource::new(wrong_key, ResolvedType::Scalar(StandardScalar::Boolean));
    let generation = resource.begin_loading().unwrap();

    assert_eq!(
        resource.publish_ready(&active, generation, RuntimeValue::Boolean(true)),
        Err(super::super::ClientResourceError::TargetMismatch {
            expected: wrong_target,
        }),
    );
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
}

#[test]
fn client_resource_resolves_compiled_verified_standard_server_target() {
    let (active, _, pair, _) = version_two_client_call_active();
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        active.catalogue_hash(),
    );
    let mut resource = ClientResource::new(key, ResolvedType::Scalar(StandardScalar::Integer));

    let request = resource
        .begin_request(&active, vec![argument])
        .expect("the pinned standard resource target should validate");

    assert_eq!(request.target(), target);
    assert_eq!(
        request.expected_type(),
        ResolvedType::Scalar(StandardScalar::Integer)
    );
}

#[test]
fn client_resource_validates_named_and_reference_catalogue_membership() {
    let (active, function, pair, _) = version_one_active(true);
    let unknown = TypeId::from_bytes([0xee; 16]);
    let cases = [
        (
            ResolvedType::Named(unknown),
            RuntimeValue::null(ResolvedType::Named(unknown)).unwrap(),
        ),
        (
            ResolvedType::Reference { target: unknown },
            RuntimeValue::Reference {
                target: unknown,
                object: orna_core::ObjectId::from_bytes([0xef; 16]),
            },
        ),
    ];

    for (index, (expected, value)) in cases.into_iter().enumerate() {
        let key = super::super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7a; 16]),
            Sha256Digest::from_bytes([0x70 + index as u8; 32]),
            Sha256Digest::from_bytes([0x80 + index as u8; 32]),
        );
        let mut resource = super::super::ClientResource::new(key, expected);
        let generation = resource.begin_loading().unwrap();
        assert_eq!(
            resource.publish_ready(&active, generation, value),
            Err(super::super::ClientResourceError::TypeMismatch),
        );
        assert_eq!(
            resource.status(),
            super::super::ClientResourceStatus::Loading
        );
    }
}

#[test]
fn client_resource_cache_keeps_key_and_transitions() {
    let (active, function, pair, _) = version_one_active(true);
    let key = super::super::ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        Sha256Digest::from_bytes([0xc1; 32]),
        Sha256Digest::from_bytes([0xc2; 32]),
    );
    let mut state = super::super::ClientStateStore::new();

    assert!(state.resource(key).is_none());
    {
        let resource =
            state.get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Boolean));
        let generation = resource.begin_loading().unwrap();
        resource
            .publish_ready(&active, generation, RuntimeValue::Boolean(true))
            .unwrap();
    }
    assert_eq!(
        state
            .resource(key)
            .and_then(super::super::ClientResource::value),
        Some(&RuntimeValue::Boolean(true)),
    );

    // A duplicate lookup returns the existing resource and keeps its
    // original type and published value.
    let resource = state.get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer));
    assert_eq!(
        resource.expected_type(),
        ResolvedType::Scalar(StandardScalar::Boolean),
    );
    assert_eq!(resource.value(), Some(&RuntimeValue::Boolean(true)));

    let first = resource.begin_loading().unwrap();
    let second = resource.begin_loading().unwrap();
    assert_eq!(
        state
            .resource_mut(key)
            .expect("resource remains in the cache")
            .publish_failure(first, "stale".to_owned()),
        Err(super::super::ClientResourceError::StaleGeneration {
            expected: second,
            actual: first,
        }),
    );
    assert_eq!(
        state
            .resource(key)
            .map(super::super::ClientResource::status),
        Some(super::super::ClientResourceStatus::Loading),
    );

    state
        .resource_mut(key)
        .expect("resource remains in the cache")
        .cancel(second)
        .unwrap();
    assert_eq!(
        state
            .resource(key)
            .map(super::super::ClientResource::status),
        Some(super::super::ClientResourceStatus::Cancelled),
    );
    let generation_before_invalidation = state
        .resource(key)
        .expect("cancelled resource remains in the cache")
        .generation();
    assert_eq!(state.invalidate_resource(key), Ok(true));
    let resource = state
        .resource(key)
        .expect("invalidated resource remains cached");
    assert_eq!(resource.key(), key);
    assert_eq!(
        resource.expected_type(),
        ResolvedType::Scalar(StandardScalar::Boolean),
    );
    assert_eq!(
        resource.generation().value(),
        generation_before_invalidation.value() + 1,
    );
    assert_eq!(resource.status(), super::super::ClientResourceStatus::Idle);
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);
    assert_eq!(
        state.invalidate_resource(super::super::ClientResourceKey::new(
            InvocationTarget::new(function, pair),
            PrincipalId::from_bytes([0x7b; 16]),
            Sha256Digest::from_bytes([0xc1; 32]),
            Sha256Digest::from_bytes([0xc2; 32]),
        )),
        Ok(false)
    );
}

#[test]
fn resource_invalidation_cancels_owned_request_and_rejects_late_completion() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xc2; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let late_completion = request.clone().ready(RuntimeValue::Integer(42));
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Ok(true),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert!(executor.pending.is_none());
    assert_eq!(executor.poll(), None);
    assert_eq!(executor.late_dropped, 1);
    assert_eq!(
        state
            .resource(key)
            .expect("invalidated resource remains cached")
            .status(),
        super::super::ClientResourceStatus::Idle,
    );
    assert_eq!(
        state
            .resource_mut(key)
            .expect("invalidated resource remains cached")
            .apply_completion(&active, late_completion),
        Err(super::super::ClientResourceError::StaleGeneration {
            expected: super::super::ClientResourceGeneration(2),
            actual: request.generation(),
        }),
    );
}

#[test]
fn resource_stream_invalidation_abandons_nonterminal_values_before_invalidation() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0xc7; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource_with_kind(
            key,
            ResourceKind::Stream,
            ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID),
        )
        .begin_stream_request(&active, Vec::new())
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None).with_cancel_stream_values();
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Ok(true),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert!(executor.pending.is_none());
    assert_eq!(executor.poll(), None);
    assert_eq!(executor.late_dropped, 1);
    let resource = state
        .resource(key)
        .expect("invalidated stream resource remains cached");
    assert_eq!(resource.status(), ClientResourceStatus::Idle);
    assert_eq!(
        resource.generation().value(),
        request.generation().value() + 1
    );
    assert_eq!(resource.request_id(), None);
}

#[test]
fn resource_stream_invalidation_keeps_state_when_abandon_fails() {
    let (active, function, pair, _) = version_two_server_stream_active();
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0xc8; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource_with_kind(
            key,
            ResourceKind::Stream,
            ResolvedType::Value(orna_standard::BOOLEAN_TYPE_ID),
        )
        .begin_stream_request(&active, Vec::new())
        .unwrap();
    let before = state
        .resource(key)
        .expect("pending stream resource remains cached")
        .clone();
    let mut executor = RecordingActionExecutor::new(None)
        .with_cancel_stream_values()
        .with_abandon_failure();
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Err(super::super::ClientResourceError::Executor(
            "resource executor cannot abandon a pending request".to_owned(),
        )),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    assert_eq!(state.resource(key), Some(&before));
}

#[test]
fn resource_invalidation_keeps_terminal_ready_completion() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xc5; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::Integer(99));
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Ok(true),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert!(executor.abandoned.is_empty());
    assert!(executor.pending.is_none());
    let resource = state
        .resource(key)
        .expect("terminal resource remains cached");
    assert_eq!(resource.status(), super::super::ClientResourceStatus::Ready);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.value(), Some(&RuntimeValue::Integer(99)));
    assert_eq!(resource.failure(), None);
}

#[test]
fn resource_invalidation_rejects_wrong_typed_terminal_cancellation() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xc6; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None)
        .with_cancel_value(RuntimeValue::Text("wrong cancellation type".to_owned()));
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Err(super::super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    // A malformed terminal cancellation consumed the executor request, so
    // the resource must not remain Loading without an owner.
    assert!(executor.pending.is_none());
    let resource = state
        .resource(key)
        .expect("malformed cancellation leaves a safe resource state");
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Cancelled
    );
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), None);
    assert_eq!(resource.value(), None);
    assert_eq!(resource.failure(), None);
}

#[test]
fn resource_invalidation_rejects_mismatched_terminal_cancellation_without_losing_owner() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xd1; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut mismatched = request.clone();
    mismatched.request_id = InvocationId::from_bytes([0xfd; 16]);
    let mut executor = RecordingActionExecutor::new(None).with_cancel_terminal_identity(mismatched);
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Err(super::super::ClientResourceError::RequestIdMismatch {
            expected: request.request_id(),
            actual: InvocationId::from_bytes([0xfd; 16]),
        }),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    let resource = state
        .resource(key)
        .expect("mismatched cancellation leaves the resource cached");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn resource_invalidation_rejects_active_mismatch_before_consuming_request() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x93; 16]),
        CatalogueRevisionId::from_bytes([0x94; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        PrincipalId::from_bytes([0x7a; 16]),
        ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap(),
        Sha256Digest::from_bytes([0xc9; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None);
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&changed_active, key, &mut executor),
        Err(super::super::ClientResourceError::RevisionMismatch {
            expected: pair,
            actual: changed_pair,
        }),
    );
    assert!(executor.cancelled.is_empty());
    assert_eq!(executor.pending.as_ref(), Some(&request));
    let resource = state
        .resource(key)
        .expect("mismatched resource remains cached");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn stale_replacement_rejects_local_request_mismatch_before_cancel() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x95; 16]),
        CatalogueRevisionId::from_bytes([0x96; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xca; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xcb; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    state
        .resource_mut(old_key)
        .expect("stale resource remains cached")
        .expected_type = ResolvedType::Scalar(StandardScalar::Integer);
    let mut executor = RecordingActionExecutor::new(None);
    executor.pending = Some(request.clone());

    assert_eq!(
        state.get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        ),
        Err(super::super::ClientResourceError::TypeMismatch),
    );
    assert!(executor.cancelled.is_empty());
    assert_eq!(executor.pending.as_ref(), Some(&request));
    assert!(state.resource(new_key).is_none());
    let resource = state
        .resource(old_key)
        .expect("stale resource remains cached");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn stale_replacement_rejects_mismatched_pending_without_losing_owner() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x99; 16]),
        CatalogueRevisionId::from_bytes([0x9a; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xce; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xcf; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut mismatched = request.clone();
    mismatched.request_id = InvocationId::from_bytes([0xfe; 16]);
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending_identity(mismatched);
    executor.pending = Some(request.clone());

    assert_eq!(
        state.get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        ),
        Err(super::super::ClientResourceError::RequestIdMismatch {
            expected: request.request_id(),
            actual: InvocationId::from_bytes([0xfe; 16]),
        }),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    assert!(state.resource(new_key).is_none());
    let resource = state
        .resource(old_key)
        .expect("stale resource remains cached");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn stale_replacement_rejects_mismatched_terminal_cancellation_without_losing_owner() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0xda; 16]),
        CatalogueRevisionId::from_bytes([0xdb; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xdc; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xdd; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut mismatched = request.clone();
    mismatched.key = new_key;
    let mut executor = RecordingActionExecutor::new(None).with_cancel_terminal_identity(mismatched);
    executor.pending = Some(request.clone());

    assert_eq!(
        state.get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        ),
        Err(super::super::ClientResourceError::RequestKeyMismatch {
            expected: Box::new(old_key),
            actual: Box::new(new_key),
        }),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    assert!(state.resource(new_key).is_none());
    let resource = state
        .resource(old_key)
        .expect("mismatched cancellation leaves the stale resource cached");
    assert_eq!(resource.status(), ClientResourceStatus::Loading);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn stale_replacement_malformed_terminal_cancellation_is_safe() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x97; 16]),
        CatalogueRevisionId::from_bytes([0x98; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xcc; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xcd; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::Integer(7));
    executor.pending = Some(request.clone());

    assert_eq!(
        state.get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        ),
        Err(super::super::ClientResourceError::TypeMismatch),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert!(executor.pending.is_none());
    assert!(state.resource(new_key).is_none());
    let resource = state
        .resource(old_key)
        .expect("stale resource remains cached");
    assert_eq!(resource.status(), ClientResourceStatus::Cancelled);
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
    assert_eq!(resource.active_request(), None);
}

#[test]
fn stale_replacement_uses_pinned_validation_after_revision_changes() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0xa1; 16]),
        CatalogueRevisionId::from_bytes([0xa2; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xa3; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xa4; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::Boolean(true));
    executor.pending = Some(request.clone());

    state
        .get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        )
        .unwrap();

    assert_eq!(executor.cancelled, vec![request]);
    assert_eq!(
        state.resource(old_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
    assert_eq!(
        state.resource(new_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
}

#[test]
fn stale_replacement_accepts_typed_null_for_primitive_value_type() {
    let (active, _function, pair, _, _parameter) = version_six_client_resource_action_active();
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0xa7; 16]),
        CatalogueRevisionId::from_bytes([0xa8; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let argument = FunctionArgument::new(
        ParameterId::from_bytes([0xd3; 16]),
        RuntimeValue::Text("/tmp/typed-null".to_owned()),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let target_function = FunctionId::from_bytes([0xd1; 16]);
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(target_function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xa9; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(target_function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xaa; 32]),
    );
    let expected = ResolvedType::Value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID);
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, expected)
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::null(expected).unwrap());
    executor.pending = Some(request.clone());

    state
        .get_or_create_resource_with_executor(&changed_active, new_key, expected, &mut executor)
        .unwrap();

    assert_eq!(executor.cancelled, vec![request]);
    assert_eq!(
        state.resource(old_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
    assert_eq!(
        state.resource(new_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
}

#[test]
fn resource_invalidation_preflights_generation_before_releasing_request() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xc4; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = {
        let resource =
            state.get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer));
        resource.generation = super::super::ClientResourceGeneration(u64::MAX - 1);
        resource.begin_request(&active, vec![argument]).unwrap()
    };
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Err(super::super::ClientResourceError::GenerationExhausted),
    );
    assert!(executor.cancelled.is_empty());
    assert!(executor.abandoned.is_empty());
    assert_eq!(executor.pending.as_ref(), Some(&request));
    let resource = state
        .resource(key)
        .expect("exhausted resource remains cached");
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
    assert_eq!(
        resource.generation(),
        super::super::ClientResourceGeneration(u64::MAX)
    );
    assert_eq!(resource.active_request(), Some(request));
}

#[test]
fn resource_invalidation_retains_owned_request_when_abandon_fails() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xc3; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let before = state
        .resource(key)
        .expect("pending resource remains cached")
        .clone();
    let mut executor = RecordingActionExecutor::new(None)
        .with_cancel_pending()
        .with_abandon_failure();
    executor.pending = Some(request.clone());

    assert_eq!(
        state.invalidate_resource_with_executor(&active, key, &mut executor),
        Err(super::super::ClientResourceError::Executor(
            "resource executor cannot abandon a pending request".to_owned(),
        )),
    );
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    let resource = state
        .resource(key)
        .expect("failed invalidation retains the resource");
    assert_eq!(resource, &before);
    assert_eq!(
        resource.status(),
        super::super::ClientResourceStatus::Loading
    );
    assert_eq!(resource.generation(), request.generation());
    assert_eq!(resource.request_id(), Some(request.request_id()));
}

#[test]
fn replacing_complete_resource_key_cancels_previous_generation() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key_a = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xd2; 32]),
    );
    let key_b = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xd3; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key_a, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None);

    state
        .get_or_create_resource_with_executor(
            &active,
            key_b,
            ResolvedType::Scalar(StandardScalar::Integer),
            &mut executor,
        )
        .unwrap();

    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(
        state
            .resource(key_a)
            .map(super::super::ClientResource::status),
        Some(super::super::ClientResourceStatus::Idle),
    );
    assert!(matches!(
        state
            .resource_mut(key_a)
            .expect("replaced resource remains cached")
            .apply_completion(&active, request.ready(RuntimeValue::Integer(42))),
        Err(super::super::ClientResourceError::StaleGeneration { .. }),
    ));
    assert_eq!(
        state
            .resource(key_b)
            .map(super::super::ClientResource::status),
        Some(super::super::ClientResourceStatus::Idle),
    );
}

#[test]
fn replacing_same_revision_keeps_terminal_executor_completion() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let old_key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xe8; 32]),
    );
    let new_key = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xe9; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor =
        RecordingActionExecutor::new(None).with_cancel_value(RuntimeValue::Integer(99));
    executor.pending = Some(request.clone());

    state
        .get_or_create_resource_with_executor(
            &active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Integer),
            &mut executor,
        )
        .unwrap();

    let old_resource = state.resource(old_key).expect("old key remains cached");
    assert_eq!(old_resource.status(), ClientResourceStatus::Ready);
    assert_eq!(old_resource.value(), Some(&RuntimeValue::Integer(99)));
    assert_eq!(old_resource.generation(), request.generation());
    assert_eq!(
        state.resource(new_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
}

#[test]
fn replacing_resource_key_across_revision_releases_old_executor_request() {
    let (active, function, pair, _) = version_one_active(true);
    let changed_pair = RevisionPair::new(
        SourceRevisionId::from_bytes([0x91; 16]),
        CatalogueRevisionId::from_bytes([0x92; 16]),
    );
    let changed_active = active_with_revision_pair(&active, changed_pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let digest = ClientResourceKey::canonical_arguments_digest(&active, &[]).unwrap();
    let old_key = ClientResourceKey::new(
        InvocationTarget::new(function, pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xe6; 32]),
    );
    let new_key = ClientResourceKey::new(
        InvocationTarget::new(function, changed_pair),
        principal,
        digest,
        Sha256Digest::from_bytes([0xe7; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(old_key, ResolvedType::Scalar(StandardScalar::Boolean))
        .begin_request(&active, Vec::new())
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None).with_cancel_pending();
    executor.pending = Some(request.clone());

    state
        .get_or_create_resource_with_executor(
            &changed_active,
            new_key,
            ResolvedType::Scalar(StandardScalar::Boolean),
            &mut executor,
        )
        .unwrap();

    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert!(executor.pending.is_none());
    assert_eq!(executor.poll(), None);
    assert_eq!(executor.late_dropped, 1);
    assert!(matches!(
        state
            .resource_mut(old_key)
            .expect("old key remains cached")
            .apply_completion(&changed_active, request.ready(RuntimeValue::Boolean(true))),
        Err(super::super::ClientResourceError::StaleGeneration { .. }),
    ));
    assert_eq!(
        state.resource(old_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
    assert_eq!(
        state.resource(new_key).map(ClientResource::status),
        Some(ClientResourceStatus::Idle),
    );
}

#[test]
fn replacing_resource_key_retains_nested_request_when_abandon_fails() {
    let (active, _, pair, _) = version_two_client_call_active();
    let standard = active
        .catalogue_hash_context()
        .standard()
        .expect("version-two fixture pins the verified standard snapshot");
    let target = InvocationTarget::verified_standard(
        orna_standard::STD_INVOKE_ECHO_FUNCTION_ID,
        pair,
        standard.revision(),
        orna_standard::STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    );
    let argument = FunctionArgument::new(
        orna_standard::STD_INVOKE_ECHO_PARAMETER_ID,
        RuntimeValue::Integer(42),
    )
    .unwrap();
    let digest =
        ClientResourceKey::canonical_arguments_digest(&active, std::slice::from_ref(&argument))
            .unwrap();
    let key_a = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xe4; 32]),
    );
    let key_b = ClientResourceKey::new(
        target,
        PrincipalId::from_bytes([0x7a; 16]),
        digest,
        Sha256Digest::from_bytes([0xe5; 32]),
    );
    let mut state = ClientStateStore::new();
    let request = state
        .get_or_create_resource(key_a, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, vec![argument])
        .unwrap();
    let mut executor = RecordingActionExecutor::new(None)
        .with_cancel_pending()
        .with_abandon_failure();
    executor.pending = Some(request.clone());
    let pending_identity = (request.request_id(), request.key(), request.generation());
    let mut nested = super::super::ClientActionNestedExecutor {
        inner: &mut executor,
        pending_request: None,
    };

    let result = state.get_or_create_resource_with_executor(
        &active,
        key_b,
        ResolvedType::Scalar(StandardScalar::Integer),
        &mut nested,
    );

    assert!(matches!(
        result,
        Err(super::super::ClientResourceError::Executor(message))
            if message == "resource executor cannot abandon a pending request"
    ));
    let mut mismatch_state = ClientStateStore::new();
    let mismatched_request = mismatch_state
        .get_or_create_resource(key_b, ResolvedType::Scalar(StandardScalar::Integer))
        .begin_request(&active, request.arguments().to_vec())
        .unwrap();
    assert_eq!(
        nested.abandon(mismatched_request),
        Err("resource executor request mismatch".to_owned()),
    );
    assert_eq!(nested.pending_request_identity(), Some(pending_identity));
    assert!(nested.release_failed());
    assert_eq!(nested.pending_request_identity(), Some(pending_identity));
    drop(nested);
    assert_eq!(executor.cancelled, vec![request.clone()]);
    assert_eq!(executor.abandoned, vec![request.clone()]);
    assert_eq!(executor.pending.as_ref(), Some(&request));
    assert!(state.resource(key_b).is_none());
    assert_eq!(
        state
            .resource(key_a)
            .map(super::super::ClientResource::status),
        Some(super::super::ClientResourceStatus::Loading),
    );
}

#[test]
fn client_resource_cache_keeps_distinct_complete_keys_independent() {
    let (_, function, pair, _) = version_one_active(true);
    let target = InvocationTarget::new(function, pair);
    let principal = PrincipalId::from_bytes([0x7a; 16]);
    let key_a = super::super::ClientResourceKey::new(
        target,
        principal,
        Sha256Digest::from_bytes([0xd1; 32]),
        Sha256Digest::from_bytes([0xd2; 32]),
    );
    let key_b = super::super::ClientResourceKey::new(
        target,
        principal,
        Sha256Digest::from_bytes([0xd1; 32]),
        Sha256Digest::from_bytes([0xd3; 32]),
    );
    assert_ne!(key_a, key_b);
    let mut state = super::super::ClientStateStore::new();

    state.get_or_create_resource(key_a, ResolvedType::Scalar(StandardScalar::Boolean));
    state.get_or_create_resource(key_b, ResolvedType::Scalar(StandardScalar::Boolean));

    let resource_a = state.resource(key_a).expect("first resource is cached");
    let resource_b = state.resource(key_b).expect("second resource is cached");
    assert_eq!(resource_a.key(), key_a);
    assert_eq!(resource_b.key(), key_b);

    let generation = state
        .resource_mut(key_a)
        .expect("first resource is cached")
        .begin_loading()
        .unwrap();
    assert_eq!(
        state
            .resource(key_a)
            .map(super::super::ClientResource::status),
        Some(super::super::ClientResourceStatus::Loading),
    );
    assert_eq!(
        state
            .resource(key_b)
            .map(super::super::ClientResource::status),
        Some(super::super::ClientResourceStatus::Idle),
    );
    state
        .resource_mut(key_a)
        .expect("first resource is cached")
        .cancel(generation)
        .unwrap();
}
