use std::collections::BTreeSet;

use orna_sys_v1::{
    AbiType, AbiVersion, EffectSet, FailureCode, ProviderDiagnostic, ProviderId, ProviderOffer,
    ProviderRoleRegistry, SemanticRoleId, SystemEffect, system_api_json, system_dispatch_table,
    system_provider_abi,
};
use serde_json::Value;

const SHARED_PROVIDER_FAILURES: [&str; 3] = [
    "sys.abi.precondition_failed",
    "sys.abi.unavailable",
    "sys.abi.provider_failed",
];

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
        .dispatch(operation, |_| Ok(()), |selected| {
            assert_eq!(selected.id, contract.id);
            Ok("native")
        })
        .expect("dispatch invokes the handler with its selected contract");
    assert_eq!(dispatched, orna_sys_v1::SystemDispatchResult::Returned("native"));

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
                contract
                    .declares_failure(&FailureCode::new(code).unwrap()),
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
                    Ok(orna_sys_v1::SystemDispatchResult::<()>::Failed(code.clone())),
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
        assert_eq!(successful, Ok(orna_sys_v1::SystemDispatchResult::Returned(())));
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
