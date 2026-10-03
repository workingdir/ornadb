use std::collections::BTreeSet;

use orna_sys_v1::{
    AbiType, AbiVersion, EffectSet, FailureCode, OperationId, ProviderDiagnostic, ProviderFailure,
    ProviderId, ProviderOffer, ProviderRoleRegistry, SemanticRoleId, SystemEffect,
    SystemOperationProvider, SystemProviderAbi, TypeId, TypedValue, system_api_json,
    system_dispatch_table, system_provider_abi, system_provider_abi_json, validate_provider_offer,
};
use serde_json::Value;

const SHARED_PROVIDER_FAILURES: [&str; 3] = [
    "sys.abi.precondition_failed",
    "sys.abi.unavailable",
    "sys.abi.provider_failed",
];

struct InvokeValueProvider {
    offer: ProviderOffer,
    operation: OperationId,
    argument_types: Vec<String>,
    response: Result<TypedValue, FailureCode>,
}

impl SystemOperationProvider for InvokeValueProvider {
    fn offer(&self) -> &ProviderOffer {
        &self.offer
    }

    fn invoke(
        &self,
        operation: &OperationId,
        arguments: &[TypedValue],
    ) -> Result<TypedValue, ProviderFailure> {
        assert_eq!(operation, &self.operation);
        assert_eq!(
            arguments
                .iter()
                .map(|argument| argument.static_type().as_str().to_owned())
                .collect::<Vec<_>>(),
            self.argument_types
        );
        self.response.clone().map_err(|code| ProviderFailure {
            code,
            payload: None,
        })
    }
}

#[test]
fn generated_provider_abi_carries_typed_operation_contracts_and_roles() {
    let abi = system_dispatch_table();
    assert!(std::ptr::eq(abi, system_provider_abi()));
    abi.validate()
        .expect("all required baked roles have compatible providers");
    assert_eq!(
        abi.operations().count(),
        orna_sys_v1::SYSTEM_FUNCTION_DESCRIPTORS.len()
    );

    let checkout = abi
        .operation("sys.admin.checkout(SnapshotRef)")
        .expect("checkout overload is registered");
    assert_eq!(checkout.version, AbiVersion::V1_0);
    assert_eq!(checkout.effects, EffectSet::one(SystemEffect::Admin));
    assert_eq!(checkout.signature.parameters.len(), 3);
    assert_eq!(
        checkout.signature.parameters[0].ty,
        AbiType::Named("sys.SnapshotRef".into())
    );
    assert!(!checkout.preconditions.is_empty());
    assert!(
        checkout.declares_failure(
            &orna_sys_v1::FailureCode::new("sys.abi.precondition_failed").unwrap()
        )
    );
    assert!(
        abi.validate_failure(
            checkout.id.as_str(),
            &orna_sys_v1::FailureCode::new("sys.abi.precondition_failed").unwrap(),
        )
        .is_ok()
    );
    assert_eq!(
        abi.validate_failure(
            checkout.id.as_str(),
            &orna_sys_v1::FailureCode::new("sys.storage.corrupt").unwrap(),
        ),
        Err(ProviderDiagnostic::UndeclaredFailure {
            operation: checkout.id.clone(),
            code: orna_sys_v1::FailureCode::new("sys.storage.corrupt").unwrap(),
        })
    );

    let meta = abi
        .operation("sys.meta")
        .expect("generic metadata primitive");
    assert_eq!(meta.signature.type_parameters, ["T"]);
    assert_eq!(
        meta.signature.result,
        AbiType::Applied {
            constructor: "sys.ValueMetadata".into(),
            arguments: vec![AbiType::Named("T".into())],
        }
    );

    let invoke_role = abi
        .role("langitem.sys.invoke")
        .expect("invoke semantic role is collected from implementations");
    assert_eq!(invoke_role.version, AbiVersion::V1_0);
    assert_eq!(invoke_role.effects, EffectSet::one(SystemEffect::Invoke));
    assert!(
        invoke_role
            .operations
            .contains(&orna_sys_v1::OperationId::new("sys.invoke(Value)").unwrap())
    );
    assert!(
        abi.validate_failure(
            "sys.invoke(Value)",
            &orna_sys_v1::FailureCode::new("sys.invoke.argument_missing").unwrap(),
        )
        .is_ok()
    );
    assert!(ProviderRoleRegistry::from_baked_abi(abi).is_ok());
}

#[test]
fn provider_registry_role_edges_are_a_bijective_effect_compatible_sweep() {
    let abi = system_dispatch_table();
    let mut role_link_count = BTreeSet::new();

    for role in abi.roles() {
        assert!(
            !role.operations.is_empty(),
            "{} has operations",
            role.id.as_str()
        );
        assert!(
            !role.required || role.builtin_provider.is_some(),
            "required role {} resolves to a baked provider",
            role.id.as_str()
        );
        let declared = role.operations.iter().cloned().collect::<BTreeSet<_>>();
        assert_eq!(
            declared.len(),
            role.operations.len(),
            "{} lists each operation once",
            role.id.as_str()
        );

        let linked = abi
            .operations()
            .filter(|operation| operation.role.as_ref() == Some(&role.id))
            .collect::<Vec<_>>();
        assert_eq!(
            linked
                .iter()
                .map(|operation| operation.id.clone())
                .collect::<BTreeSet<_>>(),
            declared,
            "{} has complete forward and reverse operation links",
            role.id.as_str()
        );
        for operation in linked {
            assert_eq!(operation.role_version, Some(role.version));
            assert!(
                operation.effects.is_subset_of(&role.effects),
                "{} effects fit role {}",
                operation.id.as_str(),
                role.id.as_str()
            );
            assert!(role_link_count.insert(operation.id.clone()));
        }
    }

    for operation in abi.operations() {
        assert_eq!(
            operation.role.is_some(),
            role_link_count.contains(&operation.id),
            "{} is linked exactly when it declares a semantic role",
            operation.id.as_str()
        );
    }
}

#[test]
fn every_baked_role_resolves_its_offer_and_rejects_version_or_effect_widening() {
    let abi = system_dispatch_table();
    let registry = ProviderRoleRegistry::from_baked_abi(abi)
        .expect("all baked semantic roles have compatible provider offers");
    registry
        .validate_required()
        .expect("every required role resolves from its baked provider");

    for role in abi.roles() {
        let built_in_provider = role
            .builtin_provider
            .clone()
            .expect("each current baked role declares its provider");
        let resolved = registry
            .resolve(role.id.as_str())
            .expect("each baked role resolves to an offer");
        assert_eq!(resolved.provider, built_in_provider);
        assert_eq!(resolved.role, role.id);
        assert_eq!(resolved.version, role.version);
        assert_eq!(resolved.effects, role.effects);

        let incompatible_version = AbiVersion {
            major: role.version.major ^ 1,
            minor: role.version.minor,
        };
        let wrong_version = ProviderOffer {
            provider: built_in_provider.clone(),
            role: role.id.clone(),
            version: incompatible_version,
            effects: role.effects.clone(),
        };
        assert_eq!(
            validate_provider_offer(role, &wrong_version),
            Err(ProviderDiagnostic::RoleVersionMismatch {
                role: role.id.clone(),
                required: role.version,
                provided: incompatible_version,
            }),
            "every baked role rejects a major-version mismatch: {}",
            role.id.as_str()
        );

        let extra_effect = [
            SystemEffect::Read,
            SystemEffect::Invoke,
            SystemEffect::Admin,
        ]
        .into_iter()
        .find(|effect| !role.effects.iter().any(|registered| registered == *effect))
        .expect("each baked role has an effect ceiling below the full effect set");
        let widened_effects = EffectSet::new(role.effects.iter().chain([extra_effect]));
        let widened_offer = ProviderOffer {
            provider: built_in_provider.clone(),
            role: role.id.clone(),
            version: role.version,
            effects: widened_effects,
        };
        assert_eq!(
            validate_provider_offer(role, &widened_offer),
            Err(ProviderDiagnostic::EffectIncompatible(role.id.clone())),
            "every baked role rejects an effect-ceiling widening: {}",
            role.id.as_str()
        );

        let alternate_offer = ProviderOffer {
            provider: ProviderId::new("fixture.alternate").unwrap(),
            role: role.id.clone(),
            version: role.version,
            effects: role.effects.clone(),
        };
        let mut link_registry = ProviderRoleRegistry::from_baked_abi(abi).unwrap();
        if role.replaceable {
            link_registry
                .bind(alternate_offer.clone())
                .expect("replaceable roles accept compatible alternate providers");
            assert_eq!(
                link_registry.resolve(role.id.as_str()).unwrap(),
                &alternate_offer
            );
        } else {
            assert_eq!(
                link_registry.bind(alternate_offer),
                Err(ProviderDiagnostic::DuplicateRoleProvider(role.id.clone())),
                "nonreplaceable baked role keeps its declared provider: {}",
                role.id.as_str()
            );
            assert_eq!(link_registry.resolve(role.id.as_str()).unwrap(), resolved);
        }
    }
}

#[test]
fn provider_abi_rejects_nonconformant_role_edges_and_effects() {
    let baseline: Value = serde_json::from_str(system_provider_abi_json()).unwrap();
    let first_role = baseline["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|role| {
            role["operations"]
                .as_array()
                .is_some_and(|ops| ops.len() > 1)
        })
        .expect("one provider role owns multiple registered operations");
    let role_name = first_role["name"].as_str().unwrap().to_owned();
    let first_operation = first_role["operations"].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_owned();

    let mut duplicate = baseline.clone();
    let role = duplicate["roles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|role| role["name"] == role_name)
        .unwrap();
    role["operations"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(first_operation));
    assert_eq!(
        SystemProviderAbi::from_json(&duplicate.to_string()),
        Err(orna_sys_v1::ProviderAbiError::DuplicateRoleOperation)
    );

    let mut missing_reverse_edge = baseline.clone();
    let role = missing_reverse_edge["roles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|role| role["name"] == role_name)
        .unwrap();
    role["operations"]
        .as_array_mut()
        .unwrap()
        .retain(|operation| operation.as_str() != Some(first_operation.as_str()));
    assert_eq!(
        SystemProviderAbi::from_json(&missing_reverse_edge.to_string()),
        Err(orna_sys_v1::ProviderAbiError::OperationRoleMismatch)
    );

    let mut wrong_role_version = baseline.clone();
    let operation = wrong_role_version["operations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|operation| operation["name"] == first_operation)
        .unwrap();
    operation["role"] = serde_json::json!(format!("{role_name}@2.0"));
    assert_eq!(
        SystemProviderAbi::from_json(&wrong_role_version.to_string()),
        Err(orna_sys_v1::ProviderAbiError::OperationRoleVersionMismatch)
    );

    let mut incompatible_effects = baseline;
    let role = incompatible_effects["roles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|role| role["name"] == role_name)
        .unwrap();
    role["effects"] = serde_json::json!([]);
    let incompatible = SystemProviderAbi::from_json(&incompatible_effects.to_string())
        .expect("well-formed role metadata reaches semantic compatibility validation");
    assert_eq!(
        incompatible.validate(),
        Err(ProviderDiagnostic::EffectIncompatible(
            SemanticRoleId::new(role_name).unwrap()
        ))
    );
}

#[test]
fn dispatch_table_enforces_registered_preconditions_and_failure_vocabularies() {
    let table = system_dispatch_table();
    let operation = "sys.admin.checkout(SnapshotRef)";
    let contract = table.operation(operation).expect("typed operation lookup");
    assert!(!contract.preconditions.is_empty());

    let accepted = table
        .check_preconditions(operation, |_| Ok(()))
        .expect("every registered precondition is checked before dispatch");
    assert_eq!(accepted.id, contract.id);

    let dispatched = table
        .dispatch(
            operation,
            |_| Ok(()),
            |selected| {
                assert_eq!(selected.id, contract.id);
                Ok("native")
            },
        )
        .expect("dispatch invokes the handler with its selected contract");
    assert_eq!(
        dispatched,
        orna_sys_v1::SystemDispatchResult::Returned("native")
    );

    let precondition_failure = FailureCode::new("sys.abi.precondition_failed").unwrap();
    assert_eq!(
        table.check_preconditions(operation, |_| Err(precondition_failure.clone())),
        Err(ProviderDiagnostic::PreconditionFailed {
            operation: contract.id.clone(),
            code: precondition_failure.clone(),
        })
    );
    let mut handler_called = false;
    assert_eq!(
        table
            .dispatch(
                operation,
                |_| Err(precondition_failure.clone()),
                |_| {
                    handler_called = true;
                    Ok(())
                }
            )
            .unwrap(),
        orna_sys_v1::SystemDispatchResult::Failed(precondition_failure)
    );
    assert!(!handler_called, "a failed precondition prevents dispatch");

    let undeclared = FailureCode::new("sys.storage.corrupt").unwrap();
    assert_eq!(
        table.validate_failure(operation, &undeclared),
        Err(ProviderDiagnostic::UndeclaredFailure {
            operation: contract.id.clone(),
            code: undeclared.clone(),
        })
    );
    assert_eq!(
        table.dispatch(operation, |_| Ok(()), |_| Err::<(), _>(undeclared.clone())),
        Err(ProviderDiagnostic::UndeclaredFailure {
            operation: contract.id.clone(),
            code: undeclared,
        })
    );
}

#[test]
fn provider_linkage_reports_missing_version_effect_and_duplicate_gaps() {
    let abi = system_dispatch_table();
    let role = abi.role("langitem.sys.invoke").unwrap().clone();
    let role_id = SemanticRoleId::new("langitem.sys.invoke").unwrap();
    let alternate = ProviderId::new("thirdparty.invoke").unwrap();

    assert_eq!(
        ProviderRoleRegistry::with_roles([role.clone(), role.clone()]),
        Err(ProviderDiagnostic::DuplicateRoleContract(role_id.clone()))
    );
    let mut missing = ProviderRoleRegistry::with_roles([role.clone()]).unwrap();
    assert_eq!(
        missing.validate_required(),
        Err(ProviderDiagnostic::RoleUnavailable {
            role: role_id.clone(),
            version: AbiVersion::V1_0,
        })
    );
    assert_eq!(
        missing.bind(ProviderOffer {
            provider: alternate.clone(),
            role: role_id.clone(),
            version: AbiVersion { major: 2, minor: 0 },
            effects: role.effects.clone(),
        }),
        Err(ProviderDiagnostic::RoleVersionMismatch {
            role: role_id.clone(),
            required: AbiVersion::V1_0,
            provided: AbiVersion { major: 2, minor: 0 },
        })
    );
    assert_eq!(
        missing.bind(ProviderOffer {
            provider: alternate.clone(),
            role: role_id.clone(),
            version: AbiVersion::V1_0,
            effects: EffectSet::one(SystemEffect::Admin),
        }),
        Err(ProviderDiagnostic::EffectIncompatible(role_id.clone()))
    );

    let mut baked = ProviderRoleRegistry::from_baked_abi(abi).unwrap();
    assert_eq!(
        baked.bind(ProviderOffer {
            provider: alternate,
            role: role_id.clone(),
            version: AbiVersion::V1_0,
            effects: role.effects,
        }),
        Err(ProviderDiagnostic::DuplicateRoleProvider(role_id))
    );
}

#[test]
fn every_dispatch_operation_matches_its_published_failure_vocabulary() {
    let api: Value = serde_json::from_str(&system_api_json()).unwrap();
    let declared_failures = api["failure_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|code| code.as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    let functions = api["functions"].as_array().unwrap();
    let table = system_dispatch_table();
    assert_eq!(table.operations().count(), functions.len());

    let mut public_operation_ids = BTreeSet::new();
    for function in functions {
        let operation_id = function["name"].as_str().unwrap();
        public_operation_ids.insert(operation_id);
        let contract = table
            .operation(operation_id)
            .unwrap_or_else(|| panic!("missing typed operation `{operation_id}`"));
        assert_eq!(contract.signature.source, function["signature"]);

        let namespace = operation_id
            .find(['(', '<'])
            .map_or(operation_id, |end| &operation_id[..end]);
        let expected_failures = declared_failures
            .iter()
            .filter(|code| {
                *code == namespace
                    || code
                        .strip_prefix(namespace)
                        .is_some_and(|tail| tail.starts_with('.'))
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        let registered_failures = contract
            .failures
            .iter()
            .filter(|code| !SHARED_PROVIDER_FAILURES.contains(&code.as_str()))
            .map(|code| code.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            registered_failures, expected_failures,
            "portable failure vocabulary for `{operation_id}`"
        );
        for code in SHARED_PROVIDER_FAILURES {
            assert!(
                contract.declares_failure(&FailureCode::new(code).unwrap()),
                "shared provider boundary code `{code}` is absent from `{operation_id}`"
            );
        }
    }
    assert_eq!(
        public_operation_ids,
        table
            .operations()
            .map(|operation| operation.id.as_str())
            .collect(),
        "published operations and typed dispatch entries remain 1:1"
    );
}

#[test]
fn provider_diagnostic_codes_stay_outside_the_public_failure_catalog() {
    let api: Value = serde_json::from_str(&system_api_json()).unwrap();
    let public_failures = api["failure_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|code| code.as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let role = SemanticRoleId::new("langitem.fixture.role").unwrap();
    let operation = orna_sys_v1::OperationId::new("sys.fixture.operation").unwrap();
    let failure = FailureCode::new("sys.abi.precondition_failed").unwrap();
    let diagnostics = [
        ProviderDiagnostic::UnknownRole(role.clone()),
        ProviderDiagnostic::UnknownOperation(operation.clone()),
        ProviderDiagnostic::PreconditionFailed {
            operation: operation.clone(),
            code: failure,
        },
        ProviderDiagnostic::DuplicateRoleContract(role.clone()),
        ProviderDiagnostic::RoleUnavailable {
            role: role.clone(),
            version: AbiVersion::V1_0,
        },
        ProviderDiagnostic::DuplicateRoleProvider(role.clone()),
        ProviderDiagnostic::RoleVersionMismatch {
            role: role.clone(),
            required: AbiVersion::V1_0,
            provided: AbiVersion { major: 2, minor: 0 },
        },
        ProviderDiagnostic::EffectIncompatible(role),
        ProviderDiagnostic::UndeclaredFailure {
            operation,
            code: FailureCode::new("sys.storage.corrupt").unwrap(),
        },
        ProviderDiagnostic::ProviderNotExecutable(
            SemanticRoleId::new("langitem.fixture.provider").unwrap(),
        ),
    ];
    let diagnostic_codes = diagnostics
        .iter()
        .map(ProviderDiagnostic::code)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        diagnostic_codes,
        BTreeSet::from([
            "sys.abi.duplicate_role_contract",
            "sys.abi.duplicate_role_provider",
            "sys.abi.effect_incompatible",
            "sys.abi.precondition_failed",
            "sys.abi.provider_not_executable",
            "sys.abi.role_unavailable",
            "sys.abi.role_version_mismatch",
            "sys.abi.undeclared_failure",
            "sys.abi.unknown_operation",
            "sys.abi.unknown_role",
        ])
    );
    assert!(
        diagnostic_codes.is_disjoint(&public_failures),
        "provider linkage diagnostics are not portable 1.0 operation failures"
    );
}

#[test]
fn dispatch_enforces_the_full_failure_set_for_every_operation() {
    let api: Value = serde_json::from_str(&system_api_json()).unwrap();
    let mut candidate_failures = api["failure_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|code| FailureCode::new(code.as_str().unwrap()).unwrap())
        .collect::<BTreeSet<_>>();
    candidate_failures.extend(
        SHARED_PROVIDER_FAILURES
            .into_iter()
            .map(|code| FailureCode::new(code).unwrap()),
    );
    let arbitrary = FailureCode::new("vendor.unregistered.failure").unwrap();
    candidate_failures.insert(arbitrary);

    let table = system_dispatch_table();
    for contract in table.operations() {
        for code in &candidate_failures {
            let mut handler_called = false;
            let result = table.dispatch(
                contract.id.as_str(),
                |_| Ok(()),
                |selected| -> Result<(), FailureCode> {
                    handler_called = true;
                    assert_eq!(selected.id, contract.id);
                    Err(code.clone())
                },
            );
            if contract.declares_failure(code) {
                assert_eq!(
                    result,
                    Ok(orna_sys_v1::SystemDispatchResult::<()>::Failed(
                        code.clone()
                    )),
                    "declared failure should cross dispatch for `{}`",
                    contract.id.as_str()
                );
                assert!(handler_called);
            } else {
                assert_eq!(
                    result,
                    Err(ProviderDiagnostic::UndeclaredFailure {
                        operation: contract.id.clone(),
                        code: code.clone(),
                    }),
                    "undeclared failure must be rejected for `{}`",
                    contract.id.as_str()
                );
                assert!(handler_called);
            }
        }
    }

    let missing_operation = "sys.fixture.missing";
    let mut missing_precondition_called = false;
    let mut missing_handler_called = false;
    assert_eq!(
        table.dispatch(
            missing_operation,
            |_| {
                missing_precondition_called = true;
                Ok(())
            },
            |_| {
                missing_handler_called = true;
                Ok::<(), FailureCode>(())
            }
        ),
        Err(ProviderDiagnostic::UnknownOperation(
            orna_sys_v1::OperationId::new(missing_operation).unwrap()
        ))
    );
    assert!(!missing_precondition_called);
    assert!(!missing_handler_called);
}

#[test]
fn dispatch_checks_each_precondition_and_stops_at_the_failing_position() {
    let api: Value = serde_json::from_str(&system_api_json()).unwrap();
    let mut candidate_failures = api["failure_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|code| FailureCode::new(code.as_str().unwrap()).unwrap())
        .collect::<BTreeSet<_>>();
    candidate_failures.extend(
        SHARED_PROVIDER_FAILURES
            .into_iter()
            .map(|code| FailureCode::new(code).unwrap()),
    );
    candidate_failures.insert(FailureCode::new("vendor.unregistered.failure").unwrap());

    let table = system_dispatch_table();
    for contract in table.operations() {
        let mut checked = 0;
        let mut invoked = 0;
        let successful = table.dispatch(
            contract.id.as_str(),
            |_| {
                checked += 1;
                Ok(())
            },
            |_| {
                invoked += 1;
                Ok(())
            },
        );
        assert_eq!(
            successful,
            Ok(orna_sys_v1::SystemDispatchResult::Returned(()))
        );
        assert_eq!(checked, contract.preconditions.len());
        assert_eq!(invoked, 1);

        if contract.preconditions.is_empty() {
            continue;
        }
        for failure_index in 0..contract.preconditions.len() {
            for code in &candidate_failures {
                let mut checks_before_failure = 0;
                let precondition_result = table.check_preconditions(contract.id.as_str(), |_| {
                    let current_index = checks_before_failure;
                    checks_before_failure += 1;
                    if current_index == failure_index {
                        Err(code.clone())
                    } else {
                        Ok(())
                    }
                });
                assert_eq!(checks_before_failure, failure_index + 1);

                let mut dispatch_checks = 0;
                let mut handler_called = false;
                let dispatch_result = table.dispatch(
                    contract.id.as_str(),
                    |_| {
                        let current_index = dispatch_checks;
                        dispatch_checks += 1;
                        if current_index == failure_index {
                            Err(code.clone())
                        } else {
                            Ok(())
                        }
                    },
                    |_| {
                        handler_called = true;
                        Ok(())
                    },
                );
                assert_eq!(dispatch_checks, failure_index + 1);

                if contract.declares_failure(code) {
                    assert_eq!(
                        precondition_result,
                        Err(ProviderDiagnostic::PreconditionFailed {
                            operation: contract.id.clone(),
                            code: code.clone(),
                        })
                    );
                    assert_eq!(
                        dispatch_result,
                        Ok(orna_sys_v1::SystemDispatchResult::Failed(code.clone()))
                    );
                } else {
                    assert_eq!(
                        precondition_result,
                        Err(ProviderDiagnostic::UndeclaredFailure {
                            operation: contract.id.clone(),
                            code: code.clone(),
                        })
                    );
                    assert_eq!(
                        dispatch_result,
                        Err(ProviderDiagnostic::UndeclaredFailure {
                            operation: contract.id.clone(),
                            code: code.clone(),
                        })
                    );
                }
                assert!(
                    !handler_called,
                    "failed preconditions block `{}`",
                    contract.id.as_str()
                );
            }
        }
    }
}

#[test]
fn dispatch_selector_conformance_matrix_covers_registry_routes_and_misses() {
    let api: Value = serde_json::from_str(&system_api_json()).expect("generated sys API JSON");
    let api_functions = api["functions"]
        .as_array()
        .expect("generated API function inventory");
    let table = system_dispatch_table();
    let typed_operation_ids = table
        .operations()
        .map(|contract| contract.id.as_str())
        .collect::<BTreeSet<_>>();
    let api_operation_ids = api_functions
        .iter()
        .map(|function| function["name"].as_str().expect("generated function name"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        typed_operation_ids, api_operation_ids,
        "dispatch selectors and macro-generated public operations have identical coverage"
    );

    let mut exact_route_cases = 0;
    for function in api_functions {
        let operation_id = function["name"].as_str().expect("generated function name");
        let contract = table.operation(operation_id).unwrap_or_else(|| {
            panic!("missing typed route for generated operation {operation_id}")
        });
        let mut checked_preconditions = 0;
        let mut invoked = false;
        let result = table.dispatch(
            operation_id,
            |_| {
                checked_preconditions += 1;
                Ok(())
            },
            |selected| {
                invoked = true;
                assert!(
                    std::ptr::eq(selected, contract),
                    "selector `{operation_id}` resolves to its exact typed contract"
                );
                assert_eq!(selected.signature.source, function["signature"]);
                if let Some(role_id) = &selected.role {
                    let role = table
                        .role(role_id.as_str())
                        .unwrap_or_else(|| panic!("missing linked role for {operation_id}"));
                    assert_eq!(selected.role_version, Some(role.version));
                    assert!(
                        role.operations.contains(&selected.id),
                        "selected operation `{operation_id}` belongs to its selected role"
                    );
                }
                Ok(selected.id.as_str().to_owned())
            },
        );
        assert_eq!(
            result,
            Ok(orna_sys_v1::SystemDispatchResult::Returned(
                operation_id.to_owned()
            )),
            "exact registry selector `{operation_id}` dispatches"
        );
        assert_eq!(checked_preconditions, contract.preconditions.len());
        assert!(
            invoked,
            "exact registry selector `{operation_id}` invokes once"
        );
        exact_route_cases += 1;
    }

    let mut unregistered_selectors = BTreeSet::new();
    for contract in table.operations() {
        let operation_id = contract.id.as_str();
        let separator = operation_id.find(['(', '<']);
        let unregistered = match separator {
            Some(index) if operation_id[index..].starts_with('(') => {
                format!("{}(sys.conformance.Unregistered)", &operation_id[..index])
            }
            Some(index) => format!("{}<sys.conformance.Unregistered>", &operation_id[..index]),
            None => format!("{operation_id}.conformance_missing"),
        };
        assert_ne!(unregistered, operation_id);
        assert!(
            orna_sys_v1::OperationId::new(unregistered.clone()).is_ok(),
            "negative selector `{unregistered}` is syntactically valid"
        );
        assert!(
            table.operation(&unregistered).is_none(),
            "negative selector `{unregistered}` is absent from the typed registry"
        );
        unregistered_selectors.insert(unregistered);
    }

    let mut rejected_selector_cases = 0;
    for operation_id in &unregistered_selectors {
        let id = orna_sys_v1::OperationId::new(operation_id.clone()).unwrap();
        let mut checked_preconditions = false;
        let mut invoked = false;
        assert_eq!(
            table.dispatch(
                operation_id,
                |_| {
                    checked_preconditions = true;
                    Ok(())
                },
                |_| {
                    invoked = true;
                    Ok::<(), FailureCode>(())
                },
            ),
            Err(ProviderDiagnostic::UnknownOperation(id)),
            "unregistered selector `{operation_id}` is rejected without fallback"
        );
        assert!(!checked_preconditions);
        assert!(!invoked);
        rejected_selector_cases += 1;
    }

    assert_eq!(exact_route_cases, typed_operation_ids.len());
    assert!(!unregistered_selectors.is_empty());
    println!(
        "dispatch_selector_conformance_matrix exact_routes={exact_route_cases} rejected_unregistered_selectors={rejected_selector_cases} total_cases={}",
        exact_route_cases + rejected_selector_cases
    );
}

#[test]
fn dispatch_signature_depth_matrix_matches_every_typed_parameter_and_result() {
    fn reconstructed_signature(signature: &orna_sys_v1::FunctionSignature) -> String {
        let type_parameters = if signature.type_parameters.is_empty() {
            String::new()
        } else {
            format!("<{}>", signature.type_parameters.join(", "))
        };
        let parameters = signature
            .parameters
            .iter()
            .map(|parameter| {
                let ty = parameter.ty.canonical();
                match &parameter.default {
                    Some(default) => format!("{}: {ty} = {default}", parameter.name),
                    None => format!("{}: {ty}", parameter.name),
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "fn {}{}({parameters}): {}",
            signature.callable,
            type_parameters,
            signature.result.canonical()
        )
    }

    fn collect_type_shapes(ty: &AbiType, shapes: &mut BTreeSet<&'static str>) {
        match ty {
            AbiType::Named(_) => {
                shapes.insert("named");
            }
            AbiType::Applied { arguments, .. } => {
                shapes.insert("applied");
                for argument in arguments {
                    collect_type_shapes(argument, shapes);
                }
            }
            AbiType::List(element) => {
                shapes.insert("list");
                collect_type_shapes(element, shapes);
            }
            AbiType::Optional(inner) => {
                shapes.insert("optional");
                collect_type_shapes(inner, shapes);
            }
        }
    }

    let api: Value = serde_json::from_str(&system_api_json()).expect("generated sys API JSON");
    let api_functions = api["functions"]
        .as_array()
        .expect("generated API function inventory");
    let table = system_dispatch_table();
    let mut operation_cases = 0;
    let mut parameter_type_cases = 0;
    let mut result_type_cases = 0;
    let mut defaulted_parameter_cases = 0;
    let mut type_shapes = BTreeSet::new();

    for contract in table.operations() {
        let operation_id = contract.id.as_str();
        let function = api_functions
            .iter()
            .find(|function| function["name"] == operation_id)
            .unwrap_or_else(|| panic!("missing macro-generated descriptor for {operation_id}"));
        let source_signature = function["signature"]
            .as_str()
            .expect("macro-generated signature string");
        let callable_end = operation_id.find(['(', '<']).unwrap_or(operation_id.len());
        assert_eq!(
            contract.signature.callable,
            &operation_id[..callable_end],
            "selected overload `{operation_id}` preserves its parsed callable"
        );
        assert_eq!(
            reconstructed_signature(&contract.signature),
            source_signature,
            "every parsed parameter, default, generic, and result type round-trips for `{operation_id}`"
        );

        for parameter in &contract.signature.parameters {
            parameter_type_cases += 1;
            defaulted_parameter_cases += usize::from(parameter.default.is_some());
            collect_type_shapes(&parameter.ty, &mut type_shapes);
        }
        collect_type_shapes(&contract.signature.result, &mut type_shapes);
        result_type_cases += 1;

        let mut checked_preconditions = 0;
        let selected_signature = table.dispatch(
            operation_id,
            |_| {
                checked_preconditions += 1;
                Ok(())
            },
            |selected| {
                assert!(
                    std::ptr::eq(selected, contract),
                    "dispatch selects the exact typed tree for `{operation_id}`"
                );
                Ok(reconstructed_signature(&selected.signature))
            },
        );
        assert_eq!(
            selected_signature,
            Ok(orna_sys_v1::SystemDispatchResult::Returned(
                source_signature.to_owned()
            )),
            "dispatch returns the exact macro signature for `{operation_id}`"
        );
        assert_eq!(checked_preconditions, contract.preconditions.len());
        operation_cases += 1;
    }

    assert_eq!(operation_cases, table.operations().count());
    assert_eq!(result_type_cases, operation_cases);
    assert!(type_shapes.contains("named"));
    assert!(type_shapes.contains("applied"));
    assert!(type_shapes.contains("list"));
    assert!(type_shapes.contains("optional"));
    println!(
        "dispatch_signature_depth_matrix operations={operation_cases} parameter_types={parameter_type_cases} result_types={result_type_cases} defaulted_parameters={defaulted_parameter_cases} type_shapes={type_shapes:?} total_cases={}",
        operation_cases + parameter_type_cases + result_type_cases
    );
}

#[test]
fn dispatch_to_provider_routes_typed_value_and_declared_failure() {
    let table = system_dispatch_table();
    let operation_name = "sys.invoke(Value)";
    let contract = table
        .operation(operation_name)
        .expect("typed registry contains the erased invoke overload");
    let role_id = contract
        .role
        .as_ref()
        .expect("invoke overload is bound to a semantic provider role");
    let role_registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("baked provider roles link successfully");
    let offer = role_registry
        .resolve(role_id.as_str())
        .expect("invoke provider offer resolves from the typed role registry")
        .clone();
    let arguments = contract
        .signature
        .parameters
        .iter()
        .map(|parameter| {
            TypedValue::public(
                TypeId::new(parameter.ty.canonical()),
                parameter.name.as_bytes().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let argument_types = contract
        .signature
        .parameters
        .iter()
        .map(|parameter| parameter.ty.canonical())
        .collect::<Vec<_>>();
    let returned_value = TypedValue::public(
        TypeId::new(contract.signature.result.canonical()),
        b"typed-provider-result".to_vec(),
    );
    let provider = InvokeValueProvider {
        offer: offer.clone(),
        operation: contract.id.clone(),
        argument_types: argument_types.clone(),
        response: Ok(returned_value.clone()),
    };
    let mut checked_preconditions = 0;
    assert_eq!(
        table.dispatch_to_provider(operation_name, &provider, &arguments, |_| {
            checked_preconditions += 1;
            Ok(())
        }),
        Ok(orna_sys_v1::SystemDispatchResult::Returned(
            returned_value.clone()
        ))
    );
    assert_eq!(checked_preconditions, contract.preconditions.len());

    let failure_code = FailureCode::new("sys.invoke.argument_missing").unwrap();
    assert!(contract.declares_failure(&failure_code));
    let failing_provider = InvokeValueProvider {
        offer,
        operation: contract.id.clone(),
        argument_types,
        response: Err(failure_code.clone()),
    };
    assert_eq!(
        table.dispatch_to_provider(operation_name, &failing_provider, &arguments, |_| Ok(())),
        Ok(orna_sys_v1::SystemDispatchResult::Failed(failure_code))
    );
    println!(
        "dispatch_to_provider operation={operation_name} typed_arguments={} outcomes=returned_typed_value,declared_failure total_cases=2",
        arguments.len()
    );
}
