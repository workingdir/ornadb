use orna_sys_v1::{
    AbiType, AbiVersion, EffectSet, ProviderDiagnostic, ProviderId, ProviderOffer,
    ProviderRoleRegistry, SemanticRoleId, SystemEffect, system_provider_abi,
};

#[test]
fn generated_provider_abi_carries_typed_operation_contracts_and_roles() {
    let abi = system_provider_abi();
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
fn provider_linkage_reports_missing_version_effect_and_duplicate_gaps() {
    let abi = system_provider_abi();
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
