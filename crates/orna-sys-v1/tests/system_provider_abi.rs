use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use orna_sys_v1::{
    AbiType, AbiVersion, Argument, ArgumentMap, EffectSet, FailureCode, OperationId,
    ProviderDiagnostic, ProviderFailure, ProviderId, ProviderOffer, ProviderRoleRegistry,
    SemanticRoleId, SystemDispatchTable, SystemEffect, SystemOperationProvider, SystemProviderAbi,
    TypeId, TypedValue, system_api_json, system_binding_stubs, system_dispatch_table,
    system_function_descriptor, system_provider_abi, system_provider_abi_json,
    system_provider_abi_schema_json, validate_provider_offer,
};
use serde_json::Value;

#[path = "../build_host.rs"]
#[allow(dead_code)]
mod build_host;
#[path = "../build_provider.rs"]
#[allow(dead_code)]
mod build_provider;
#[path = "../build_support.rs"]
#[allow(dead_code)]
mod build_support;

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
    calls: AtomicUsize,
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
        self.calls.fetch_add(1, Ordering::SeqCst);
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

struct ArgumentMapEdgeProvider {
    offer: ProviderOffer,
    operation: OperationId,
    expected_argument_types: Vec<String>,
    expected_map_payload: Vec<u8>,
    result: TypedValue,
    calls: AtomicUsize,
}

impl SystemOperationProvider for ArgumentMapEdgeProvider {
    fn offer(&self) -> &ProviderOffer {
        &self.offer
    }

    fn invoke(
        &self,
        operation: &OperationId,
        arguments: &[TypedValue],
    ) -> Result<TypedValue, ProviderFailure> {
        assert_eq!(operation, &self.operation);
        assert_eq!(arguments.len(), self.expected_argument_types.len());
        assert_eq!(
            arguments
                .iter()
                .map(|argument| argument.static_type().as_str().to_owned())
                .collect::<Vec<_>>(),
            self.expected_argument_types,
            "provider sees the fixed typed outer argument list"
        );
        assert_eq!(
            arguments[1].canonical(),
            Some(self.expected_map_payload.as_slice()),
            "provider receives the argument-map payload unchanged in its one slot"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.result.clone())
    }
}

fn round_trip_generated_provider_artifacts() -> SystemProviderAbi {
    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("provider schema regenerates from its generator");
    assert_eq!(
        generated_schema,
        orna_sys_v1::system_provider_abi_schema_json(),
        "generated provider schema matches its embedded build output"
    );
    let schema_value: Value =
        serde_json::from_str(&generated_schema).expect("generated provider schema is valid JSON");
    let round_tripped_schema = build_support::canonical_pretty_json(&schema_value)
        .expect("generated provider schema serializes canonically")
        + "\n";
    assert_eq!(
        round_tripped_schema, generated_schema,
        "provider schema survives canonical JSON round-trip"
    );

    let registry_json = system_provider_abi_json();
    let registry_value: Value =
        serde_json::from_str(registry_json).expect("embedded generated provider registry");
    let round_tripped_registry = build_support::canonical_pretty_json(&registry_value)
        .expect("generated provider registry serializes canonically")
        + "\n";
    assert_eq!(
        round_tripped_registry, registry_json,
        "provider registry survives canonical JSON round-trip"
    );
    build_host::validate_json_against_schema(&round_tripped_registry, &round_tripped_schema)
        .expect("round-tripped provider registry conforms to the round-tripped schema");
    let parsed = SystemProviderAbi::from_json(&round_tripped_registry)
        .expect("round-tripped registry deserializes into the typed table");
    assert_eq!(
        &parsed,
        system_dispatch_table(),
        "round-tripped typed table equals the baked runtime registry"
    );
    parsed
}

fn assert_generated_edge_diagnostic_parity(
    table: &SystemProviderAbi,
    registry: &ProviderRoleRegistry,
    contract: &orna_sys_v1::OperationContract,
    offer: ProviderOffer,
    expected: ProviderDiagnostic,
    expected_code: &str,
) {
    let operation_name = contract.id.as_str();
    let generated = system_function_descriptor(operation_name)
        .unwrap_or_else(|| panic!("missing generated binding for {operation_name}"));
    assert_eq!(generated.name, operation_name);
    assert_eq!(generated.signature, contract.signature.source);
    assert_eq!(
        contract.effects.iter().next(),
        Some(generated.effect),
        "generated binding and typed provider edge have matching effects"
    );

    let provider_for = || InvokeValueProvider {
        offer: offer.clone(),
        operation: contract.id.clone(),
        argument_types: contract
            .signature
            .parameters
            .iter()
            .map(|parameter| parameter.ty.canonical())
            .collect(),
        response: Ok(TypedValue::public(
            TypeId::new("sys.conformance.Edge"),
            b"must-not-run".to_vec(),
        )),
        calls: AtomicUsize::new(0),
    };
    let direct_provider = provider_for();
    let registry_provider = provider_for();
    let direct = table
        .dispatch_to_provider(generated.name, &direct_provider, &[], |_| Ok(()))
        .expect_err("direct dispatch rejects the invalid provider edge");
    let mediated = registry
        .dispatch_to_provider(table, generated.name, &registry_provider, &[], |_| Ok(()))
        .expect_err("selected-role dispatch rejects the invalid provider edge");

    assert_eq!(direct, expected, "direct diagnostic for {operation_name}");
    assert_eq!(
        mediated, expected,
        "registry diagnostic for {operation_name}"
    );
    assert_eq!(direct.code(), expected_code);
    assert_eq!(mediated.code(), expected_code);
    assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 0);
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
fn generated_provider_alias_edges_dispatch_through_both_routes() {
    const PROVIDER_ALIASES: [&str; 4] = ["-", "_", "9", "_9.edge-name"];

    let schema_json = system_provider_abi_schema_json();
    let baseline: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded provider registry");
    let baseline_table = SystemDispatchTable::from_json(system_provider_abi_json())
        .expect("embedded registry parses through the dispatch-table alias");
    let role = baseline_table
        .roles()
        .find(|role| role.builtin_provider.is_some())
        .expect("generated registry has a built-in provider role");
    let role_name = role.id.as_str().to_owned();
    let operation_name = role
        .operations
        .first()
        .expect("provider role declares a generated operation")
        .as_str()
        .to_owned();
    let role_index = baseline["roles"]
        .as_array()
        .unwrap()
        .iter()
        .position(|raw_role| raw_role["name"] == role_name)
        .expect("built-in provider role has a generated JSON row");

    let mut schema_acceptances = 0;
    let mut typed_alias_parses = 0;
    let mut generated_binding_cases = 0;
    let mut direct_routes = 0;
    let mut registry_routes = 0;
    for provider_alias in PROVIDER_ALIASES {
        let mut registry_json = baseline.clone();
        registry_json["roles"][role_index]["builtin_provider"] =
            Value::String(provider_alias.to_owned());
        let registry_json = registry_json.to_string();
        build_host::validate_json_against_schema(&registry_json, schema_json).unwrap_or_else(
            |error| {
                panic!("provider alias {provider_alias:?} rejected by generated schema: {error}")
            },
        );
        schema_acceptances += 1;

        let table = SystemDispatchTable::from_json(&registry_json)
            .expect("schema-valid provider alias parses through dispatch-table name");
        let contract = table
            .operation(&operation_name)
            .expect("provider role operation remains in generated typed table");
        assert_eq!(
            contract.role.as_ref().map(|role| role.as_str()),
            Some(role_name.as_str())
        );
        let generated = system_function_descriptor(&operation_name)
            .expect("provider role operation has a generated binding");
        assert_eq!(generated.name, operation_name);
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(
            contract.effects.iter().next(),
            Some(generated.effect),
            "provider alias route agrees with generated binding effect"
        );
        generated_binding_cases += 1;

        let registry = ProviderRoleRegistry::from_baked_abi(&table)
            .expect("provider alias registry satisfies required role contracts");
        let selected_offer = registry
            .resolve(&role_name)
            .expect("provider alias is selected for its role")
            .clone();
        assert_eq!(selected_offer.provider.as_str(), provider_alias);
        typed_alias_parses += 1;

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
        let result = TypedValue::public(
            TypeId::new(contract.signature.result.canonical()),
            provider_alias.as_bytes().to_vec(),
        );
        let provider = InvokeValueProvider {
            offer: selected_offer,
            operation: contract.id.clone(),
            argument_types: contract
                .signature
                .parameters
                .iter()
                .map(|parameter| parameter.ty.canonical())
                .collect(),
            response: Ok(result.clone()),
            calls: AtomicUsize::new(0),
        };
        assert_eq!(
            table.dispatch_to_provider(&operation_name, &provider, &arguments, |_| Ok(())),
            Ok(orna_sys_v1::SystemDispatchResult::Returned(result.clone())),
            "direct dispatch accepts provider alias {provider_alias:?}"
        );
        direct_routes += 1;
        assert_eq!(
            registry.dispatch_to_provider(
                &table,
                generated.name,
                &provider,
                &arguments,
                |_| Ok(())
            ),
            Ok(orna_sys_v1::SystemDispatchResult::Returned(result)),
            "registry dispatch accepts provider alias {provider_alias:?}"
        );
        registry_routes += 1;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    println!(
        "generated_provider_alias_dispatch_parity aliases={} schema_acceptances={schema_acceptances} typed_parses={typed_alias_parses} generated_bindings={generated_binding_cases} direct_routes={direct_routes} registry_routes={registry_routes} total_cases={}",
        PROVIDER_ALIASES.len(),
        schema_acceptances
            + typed_alias_parses
            + generated_binding_cases
            + direct_routes
            + registry_routes
    );
}

#[test]
fn generated_provider_schema_and_registry_round_trip_before_dispatch() {
    let round_tripped = round_trip_generated_provider_artifacts();
    let operation_count = round_tripped.operations().count();
    let role_count = round_tripped.roles().count();
    assert_eq!(
        operation_count,
        system_dispatch_table().operations().count()
    );
    assert_eq!(role_count, system_dispatch_table().roles().count());
    println!(
        "generated_provider_schema_registry_round_trip operations={operation_count} roles={role_count} schema_canonical=true registry_canonical=true schema_validated=true typed_table_equal=true total_cases=4"
    );
}

#[test]
fn round_tripped_registry_matches_direct_undeclared_failure_diagnostics() {
    let baked = system_dispatch_table();
    let round_tripped = round_trip_generated_provider_artifacts();
    let registry = ProviderRoleRegistry::from_baked_abi(&round_tripped)
        .expect("round-tripped typed registry resolves generated provider offers");
    let undeclared = FailureCode::new("sys.conformance.dispatch_error").unwrap();
    let mut generated_bindings = 0;
    let mut provider_bound_cases = 0;

    for contract in baked.operations() {
        let generated = system_function_descriptor(contract.id.as_str())
            .unwrap_or_else(|| panic!("missing generated binding for {}", contract.id.as_str()));
        assert_eq!(generated.name, contract.id.as_str());
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(
            contract.effects.iter().next(),
            Some(generated.effect),
            "generated binding effect matches {}",
            contract.id.as_str()
        );
        generated_bindings += 1;

        let Some(role_id) = &contract.role else {
            continue;
        };
        let round_tripped_contract = round_tripped
            .operation(contract.id.as_str())
            .expect("round-tripped typed table retains every provider-bound contract");
        assert_eq!(round_tripped_contract, contract);
        assert!(
            !contract.declares_failure(&undeclared),
            "{} does not declare the test failure",
            contract.id.as_str()
        );
        let offer = registry
            .resolve(role_id.as_str())
            .expect("round-tripped registry selects the generated provider")
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
        let provider_for = || InvokeValueProvider {
            offer: offer.clone(),
            operation: contract.id.clone(),
            argument_types: argument_types.clone(),
            response: Err(undeclared.clone()),
            calls: AtomicUsize::new(0),
        };
        let direct_provider = provider_for();
        let registry_provider = provider_for();
        let expected = Err(ProviderDiagnostic::UndeclaredFailure {
            operation: contract.id.clone(),
            code: undeclared.clone(),
        });
        let direct =
            baked.dispatch_to_provider(generated.name, &direct_provider, &arguments, |_| Ok(()));
        let mediated = registry.dispatch_to_provider(
            &round_tripped,
            generated.name,
            &registry_provider,
            &arguments,
            |_| Ok(()),
        );

        assert_eq!(
            direct,
            expected,
            "direct dispatch diagnostic for {}",
            contract.id.as_str()
        );
        assert_eq!(
            mediated,
            expected,
            "round-tripped registry diagnostic for {}",
            contract.id.as_str()
        );
        assert_eq!(direct.unwrap_err().code(), "sys.abi.undeclared_failure");
        assert_eq!(mediated.unwrap_err().code(), "sys.abi.undeclared_failure");
        assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 1);
        provider_bound_cases += 1;
    }

    assert_eq!(generated_bindings, baked.operations().count());
    assert!(provider_bound_cases > 0);
    println!(
        "generated_binding_undeclared_failure_parity bindings={generated_bindings} provider_routes={provider_bound_cases} direct_registry_pairs={provider_bound_cases} diagnostic_code=sys.abi.undeclared_failure total_cases={}",
        generated_bindings + provider_bound_cases * 2
    );
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
fn generated_binding_version_edge_diagnostics_match_direct_and_registry_routes() {
    let table = system_dispatch_table();
    let registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("generated provider offers resolve from the typed registry");
    let mut provider_bound_cases = 0;

    for contract in table.operations() {
        let Some(role_id) = &contract.role else {
            continue;
        };
        let selected = registry
            .resolve(role_id.as_str())
            .expect("generated binding role has a selected provider");
        let incompatible_version = AbiVersion {
            major: selected.version.major ^ 1,
            minor: selected.version.minor,
        };
        let invalid_offer = ProviderOffer {
            provider: selected.provider.clone(),
            role: selected.role.clone(),
            version: incompatible_version,
            effects: selected.effects.clone(),
        };
        assert_generated_edge_diagnostic_parity(
            table,
            &registry,
            contract,
            invalid_offer,
            ProviderDiagnostic::RoleVersionMismatch {
                role: role_id.clone(),
                required: selected.version,
                provided: incompatible_version,
            },
            "sys.abi.role_version_mismatch",
        );
        provider_bound_cases += 1;
    }

    assert!(provider_bound_cases > 0);
    println!(
        "generated_binding_provider_version_edge_parity operations={} provider_bound_cases={provider_bound_cases} direct_registry_pairs={provider_bound_cases} code=sys.abi.role_version_mismatch providers_called=0 total_cases={}",
        table.operations().count(),
        provider_bound_cases * 2
    );
}

#[test]
fn generated_binding_effect_edge_diagnostics_match_direct_and_registry_routes() {
    let table = system_dispatch_table();
    let registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("generated provider offers resolve from the typed registry");
    let mut provider_bound_cases = 0;

    for contract in table.operations() {
        let Some(role_id) = &contract.role else {
            continue;
        };
        let selected = registry
            .resolve(role_id.as_str())
            .expect("generated binding role has a selected provider");
        let extra_effect = [
            SystemEffect::Read,
            SystemEffect::Invoke,
            SystemEffect::Admin,
        ]
        .into_iter()
        .find(|effect| {
            !selected
                .effects
                .iter()
                .any(|registered| registered == *effect)
        })
        .expect("typed provider roles leave at least one effect outside their ceiling");
        let widened_effects = EffectSet::new(selected.effects.iter().chain([extra_effect]));
        let invalid_offer = ProviderOffer {
            provider: selected.provider.clone(),
            role: selected.role.clone(),
            version: selected.version,
            effects: widened_effects,
        };
        assert_generated_edge_diagnostic_parity(
            table,
            &registry,
            contract,
            invalid_offer,
            ProviderDiagnostic::EffectIncompatible(role_id.clone()),
            "sys.abi.effect_incompatible",
        );
        provider_bound_cases += 1;
    }

    assert!(provider_bound_cases > 0);
    println!(
        "generated_binding_provider_effect_edge_parity operations={} provider_bound_cases={provider_bound_cases} direct_registry_pairs={provider_bound_cases} code=sys.abi.effect_incompatible providers_called=0 total_cases={}",
        table.operations().count(),
        provider_bound_cases * 2
    );
}

#[test]
fn round_tripped_generated_bindings_preserve_provider_edge_diagnostics() {
    let baked = system_dispatch_table();
    let round_tripped = round_trip_generated_provider_artifacts();
    let registry = ProviderRoleRegistry::from_baked_abi(&round_tripped)
        .expect("round-tripped typed registry resolves generated provider offers");
    let mut generated_bindings = 0;
    let mut provider_routes = 0;
    let mut diagnostic_pairs = 0;

    for contract in baked.operations() {
        let generated = system_function_descriptor(contract.id.as_str())
            .unwrap_or_else(|| panic!("missing generated binding for {}", contract.id.as_str()));
        assert_eq!(generated.name, contract.id.as_str());
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(
            contract.effects.iter().next(),
            Some(generated.effect),
            "generated binding effect matches {}",
            contract.id.as_str()
        );
        generated_bindings += 1;

        let Some(role_id) = &contract.role else {
            continue;
        };
        assert_eq!(
            round_tripped.operation(contract.id.as_str()),
            Some(contract),
            "round-tripped typed metadata retains generated provider edge {}",
            contract.id.as_str()
        );
        let selected = registry
            .resolve(role_id.as_str())
            .expect("round-tripped registry selects the generated provider")
            .clone();

        let mut invalid_offers = Vec::new();
        let incompatible_version = AbiVersion {
            major: selected.version.major ^ 1,
            minor: selected.version.minor,
        };
        invalid_offers.push((
            "version",
            ProviderOffer {
                version: incompatible_version,
                ..selected.clone()
            },
            ProviderDiagnostic::RoleVersionMismatch {
                role: role_id.clone(),
                required: selected.version,
                provided: incompatible_version,
            },
            "sys.abi.role_version_mismatch",
        ));

        let extra_effect = [
            SystemEffect::Read,
            SystemEffect::Invoke,
            SystemEffect::Admin,
        ]
        .into_iter()
        .find(|effect| {
            !selected
                .effects
                .iter()
                .any(|registered| registered == *effect)
        })
        .expect("typed provider role has an effect outside its declared ceiling");
        invalid_offers.push((
            "effect",
            ProviderOffer {
                effects: EffectSet::new(selected.effects.iter().chain([extra_effect])),
                ..selected.clone()
            },
            ProviderDiagnostic::EffectIncompatible(role_id.clone()),
            "sys.abi.effect_incompatible",
        ));

        let unselected = ProviderOffer {
            provider: ProviderId::new("fixture.unselected").unwrap(),
            ..selected.clone()
        };
        invalid_offers.push((
            "selection",
            unselected.clone(),
            ProviderDiagnostic::ProviderNotSelected {
                role: role_id.clone(),
                expected: selected.clone(),
                provided: unselected,
            },
            "sys.abi.provider_not_selected",
        ));

        for (edge, offer, expected, expected_code) in invalid_offers {
            let provider_for = || InvokeValueProvider {
                offer: offer.clone(),
                operation: contract.id.clone(),
                argument_types: Vec::new(),
                response: Ok(TypedValue::public(
                    TypeId::new("sys.conformance.MustNotInvoke"),
                    b"must-not-run".to_vec(),
                )),
                calls: AtomicUsize::new(0),
            };
            let direct_provider = provider_for();
            let round_tripped_provider = provider_for();
            let direct =
                baked.dispatch_to_provider(generated.name, &direct_provider, &[], |_| Ok(()));
            let mediated = registry.dispatch_to_provider(
                &round_tripped,
                generated.name,
                &round_tripped_provider,
                &[],
                |_| Ok(()),
            );

            assert_eq!(
                direct,
                Err(expected.clone()),
                "direct {edge} edge diagnostic for {}",
                contract.id.as_str()
            );
            assert_eq!(
                mediated,
                Err(expected.clone()),
                "round-tripped registry {edge} edge diagnostic for {}",
                contract.id.as_str()
            );
            assert_eq!(direct.unwrap_err().code(), expected_code);
            assert_eq!(mediated.unwrap_err().code(), expected_code);
            assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 0);
            assert_eq!(round_tripped_provider.calls.load(Ordering::SeqCst), 0);
            diagnostic_pairs += 1;
        }
        provider_routes += 1;
    }

    assert_eq!(generated_bindings, baked.operations().count());
    assert!(provider_routes > 0);
    assert_eq!(diagnostic_pairs, provider_routes * 3);
    println!(
        "generated_binding_round_trip_provider_edge_diagnostics bindings={generated_bindings} provider_routes={provider_routes} edge_categories=version,effect,selection direct_registry_pairs={diagnostic_pairs} codes=role_version_mismatch,effect_incompatible,provider_not_selected providers_called=0 total_cases={}",
        generated_bindings + diagnostic_pairs * 2
    );
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
        ProviderDiagnostic::ArgumentCountMismatch {
            operation: operation.clone(),
            expected: 1,
            actual: 0,
        },
        ProviderDiagnostic::ArgumentTypeMismatch {
            operation: operation.clone(),
            parameter: "value".into(),
            expected: "sys.Value".into(),
            actual: "sys.String".into(),
        },
        ProviderDiagnostic::ResultTypeMismatch {
            operation: operation.clone(),
            expected: "sys.Value".into(),
            actual: "sys.String".into(),
        },
        ProviderDiagnostic::UndeclaredFailure {
            operation,
            code: FailureCode::new("sys.storage.corrupt").unwrap(),
        },
        ProviderDiagnostic::ProviderNotExecutable(
            SemanticRoleId::new("langitem.fixture.provider").unwrap(),
        ),
        ProviderDiagnostic::ProviderNotSelected {
            role: SemanticRoleId::new("langitem.fixture.role").unwrap(),
            expected: ProviderOffer {
                provider: ProviderId::new("fixture.selected").unwrap(),
                role: SemanticRoleId::new("langitem.fixture.role").unwrap(),
                version: AbiVersion::V1_0,
                effects: EffectSet::one(SystemEffect::Read),
            },
            provided: ProviderOffer {
                provider: ProviderId::new("fixture.unselected").unwrap(),
                role: SemanticRoleId::new("langitem.fixture.role").unwrap(),
                version: AbiVersion::V1_0,
                effects: EffectSet::one(SystemEffect::Read),
            },
        },
    ];
    let diagnostic_codes = diagnostics
        .iter()
        .map(ProviderDiagnostic::code)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        diagnostic_codes,
        BTreeSet::from([
            "sys.abi.argument_count_mismatch",
            "sys.abi.argument_type_mismatch",
            "sys.abi.duplicate_role_contract",
            "sys.abi.duplicate_role_provider",
            "sys.abi.effect_incompatible",
            "sys.abi.precondition_failed",
            "sys.abi.provider_not_executable",
            "sys.abi.provider_not_selected",
            "sys.abi.role_unavailable",
            "sys.abi.role_version_mismatch",
            "sys.abi.result_type_mismatch",
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
fn provider_default_edges_require_materialized_dispatch_arguments() {
    let table = system_dispatch_table();
    let registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("generated built-in provider offers resolve");
    let mut defaulted_operations = 0;
    let mut defaulted_parameters = 0;
    let mut omitted_direct_rejections = 0;
    let mut omitted_registry_rejections = 0;
    let mut materialized_direct_routes = 0;
    let mut materialized_registry_routes = 0;

    for contract in table.operations() {
        if contract.role.is_none() {
            continue;
        }
        let Some(first_default) = contract
            .signature
            .parameters
            .iter()
            .position(|parameter| parameter.default.is_some())
        else {
            continue;
        };
        assert!(
            contract.signature.parameters[first_default..]
                .iter()
                .all(|parameter| parameter.default.is_some()),
            "provider defaults remain a trailing suffix for {}",
            contract.id.as_str()
        );
        let generated = system_function_descriptor(contract.id.as_str())
            .expect("defaulted provider operation has a generated binding");
        assert_eq!(generated.signature, contract.signature.source);

        let role = contract
            .role
            .as_ref()
            .expect("provider operation has a role");
        let offer = registry
            .resolve(role.as_str())
            .expect("defaulted provider role has a selected built-in offer")
            .clone();
        let argument_types = contract
            .signature
            .parameters
            .iter()
            .map(|parameter| parameter.ty.canonical())
            .collect::<Vec<_>>();
        let omitted_arguments = contract.signature.parameters[..first_default]
            .iter()
            .map(|parameter| {
                TypedValue::public(
                    TypeId::new(parameter.ty.canonical()),
                    parameter.name.as_bytes().to_vec(),
                )
            })
            .collect::<Vec<_>>();
        let supplied = TypedValue::public(
            TypeId::new(contract.signature.result.canonical()),
            b"materialized-provider-defaults".to_vec(),
        );
        let provider = InvokeValueProvider {
            offer,
            operation: contract.id.clone(),
            argument_types,
            response: Ok(supplied.clone()),
            calls: AtomicUsize::new(0),
        };
        let omitted = ProviderDiagnostic::ArgumentCountMismatch {
            operation: contract.id.clone(),
            expected: contract.signature.parameters.len(),
            actual: first_default,
        };

        assert_eq!(
            table.dispatch_to_provider(generated.name, &provider, &omitted_arguments, |_| Ok(())),
            Err(omitted.clone()),
            "direct provider route rejects omitted defaults for {}",
            contract.id.as_str()
        );
        omitted_direct_rejections += 1;
        assert_eq!(
            registry.dispatch_to_provider(
                table,
                generated.name,
                &provider,
                &omitted_arguments,
                |_| Ok(())
            ),
            Err(omitted),
            "registry provider route rejects omitted defaults for {}",
            contract.id.as_str()
        );
        omitted_registry_rejections += 1;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

        let materialized_arguments = contract
            .signature
            .parameters
            .iter()
            .map(|parameter| {
                let value = parameter.default.as_deref().unwrap_or(&parameter.name);
                TypedValue::public(
                    TypeId::new(parameter.ty.canonical()),
                    value.as_bytes().to_vec(),
                )
            })
            .collect::<Vec<_>>();
        defaulted_parameters += contract.signature.parameters.len() - first_default;
        assert_eq!(
            table.dispatch_to_provider(generated.name, &provider, &materialized_arguments, |_| Ok(
                ()
            )),
            Ok(orna_sys_v1::SystemDispatchResult::Returned(
                supplied.clone()
            )),
            "direct provider route accepts materialized defaults for {}",
            contract.id.as_str()
        );
        materialized_direct_routes += 1;
        assert_eq!(
            registry.dispatch_to_provider(
                table,
                generated.name,
                &provider,
                &materialized_arguments,
                |_| Ok(())
            ),
            Ok(orna_sys_v1::SystemDispatchResult::Returned(supplied)),
            "registry provider route accepts materialized defaults for {}",
            contract.id.as_str()
        );
        materialized_registry_routes += 1;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        defaulted_operations += 1;
    }

    assert_eq!(defaulted_operations, 6);
    assert_eq!(defaulted_parameters, 14);
    assert_eq!(omitted_direct_rejections, defaulted_operations);
    assert_eq!(omitted_registry_rejections, defaulted_operations);
    assert_eq!(materialized_direct_routes, defaulted_operations);
    assert_eq!(materialized_registry_routes, defaulted_operations);
    println!(
        "provider_default_dispatch_parity operations={defaulted_operations} defaults={defaulted_parameters} omitted_direct={omitted_direct_rejections} omitted_registry={omitted_registry_rejections} materialized_direct={materialized_direct_routes} materialized_registry={materialized_registry_routes} total_cases={}",
        defaulted_parameters
            + omitted_direct_rejections
            + omitted_registry_rejections
            + materialized_direct_routes
            + materialized_registry_routes
    );
}

#[test]
fn provider_optional_argument_edges_match_schema_and_dispatch_diagnostics() {
    let schema_json = build_provider::generate_provider_registry_schema()
        .expect("provider optional-argument schema regenerates");
    assert_eq!(schema_json, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &schema_json)
        .expect("embedded optional provider arguments conform to the generated schema");

    let table = system_dispatch_table();
    let registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("generated built-in provider offers resolve");
    let mut optional_operations = 0;
    let mut optional_parameters = 0;
    let mut generated_binding_cases = 0;
    let mut null_direct_routes = 0;
    let mut null_registry_routes = 0;
    let mut present_direct_routes = 0;
    let mut present_registry_routes = 0;
    let mut bare_inner_rejections = 0;
    let mut wrong_optional_rejections = 0;

    for contract in table.operations() {
        if contract.role.is_none() {
            continue;
        }
        let optional_indexes = contract
            .signature
            .parameters
            .iter()
            .enumerate()
            .filter_map(|(index, parameter)| {
                matches!(&parameter.ty, AbiType::Optional(_)).then_some(index)
            })
            .collect::<Vec<_>>();
        if optional_indexes.is_empty() {
            continue;
        }

        let generated = system_function_descriptor(contract.id.as_str())
            .expect("optional provider operation has a generated binding");
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(
            contract.effects.iter().next(),
            Some(generated.effect),
            "generated binding effect matches {}",
            contract.id.as_str()
        );
        generated_binding_cases += 1;

        let role = contract
            .role
            .as_ref()
            .expect("provider operation has a role");
        let offer = registry
            .resolve(role.as_str())
            .expect("optional provider role has a selected offer")
            .clone();
        let argument_types = contract
            .signature
            .parameters
            .iter()
            .map(|parameter| parameter.ty.canonical())
            .collect::<Vec<_>>();
        let provider = InvokeValueProvider {
            offer,
            operation: contract.id.clone(),
            argument_types,
            response: Ok(TypedValue::public(
                TypeId::new(contract.signature.result.canonical()),
                b"optional-provider-result".to_vec(),
            )),
            calls: AtomicUsize::new(0),
        };
        let null_arguments = contract
            .signature
            .parameters
            .iter()
            .map(|parameter| {
                let value = parameter.default.as_deref().unwrap_or(&parameter.name);
                TypedValue::public(
                    TypeId::new(parameter.ty.canonical()),
                    value.as_bytes().to_vec(),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            optional_indexes.iter().all(|index| {
                contract.signature.parameters[*index].default.as_deref() == Some("null")
            }),
            "optional provider parameters use explicit null defaults for {}",
            contract.id.as_str()
        );
        let expected_result = provider.response.as_ref().unwrap().clone();
        assert_eq!(
            table.dispatch_to_provider(generated.name, &provider, &null_arguments, |_| Ok(())),
            Ok(orna_sys_v1::SystemDispatchResult::Returned(
                expected_result.clone()
            )),
            "direct route accepts explicit null optional arguments for {}",
            contract.id.as_str()
        );
        null_direct_routes += 1;
        assert_eq!(
            registry.dispatch_to_provider(
                table,
                generated.name,
                &provider,
                &null_arguments,
                |_| Ok(())
            ),
            Ok(orna_sys_v1::SystemDispatchResult::Returned(
                expected_result.clone()
            )),
            "registry route accepts explicit null optional arguments for {}",
            contract.id.as_str()
        );
        null_registry_routes += 1;

        let mut present_arguments = null_arguments.clone();
        for index in &optional_indexes {
            let parameter = &contract.signature.parameters[*index];
            present_arguments[*index] = TypedValue::public(
                TypeId::new(parameter.ty.canonical()),
                b"present-optional-value".to_vec(),
            );
        }
        assert_eq!(
            table.dispatch_to_provider(generated.name, &provider, &present_arguments, |_| Ok(())),
            Ok(orna_sys_v1::SystemDispatchResult::Returned(
                expected_result.clone()
            )),
            "direct route accepts present optional arguments for {}",
            contract.id.as_str()
        );
        present_direct_routes += 1;
        assert_eq!(
            registry.dispatch_to_provider(
                table,
                generated.name,
                &provider,
                &present_arguments,
                |_| Ok(())
            ),
            Ok(orna_sys_v1::SystemDispatchResult::Returned(expected_result)),
            "registry route accepts present optional arguments for {}",
            contract.id.as_str()
        );
        present_registry_routes += 1;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 4);

        for index in &optional_indexes {
            let parameter = &contract.signature.parameters[*index];
            let AbiType::Optional(inner) = &parameter.ty else {
                unreachable!("selected optional index has an optional type")
            };
            let inner_type = inner.canonical();
            for (invalid_type, counter) in [
                (inner_type.clone(), &mut bare_inner_rejections),
                (
                    "sys.cont38.Wrong?".to_owned(),
                    &mut wrong_optional_rejections,
                ),
            ] {
                let mut invalid_arguments = null_arguments.clone();
                invalid_arguments[*index] = TypedValue::public(
                    TypeId::new(invalid_type.clone()),
                    b"invalid-optional-type".to_vec(),
                );
                let expected = ProviderDiagnostic::ArgumentTypeMismatch {
                    operation: contract.id.clone(),
                    parameter: parameter.name.clone(),
                    expected: parameter.ty.canonical(),
                    actual: invalid_type,
                };
                assert_eq!(
                    table.dispatch_to_provider(
                        generated.name,
                        &provider,
                        &invalid_arguments,
                        |_| Ok(())
                    ),
                    Err(expected.clone()),
                    "direct route rejects invalid optional type for {}.{}",
                    contract.id.as_str(),
                    parameter.name
                );
                assert_eq!(
                    registry.dispatch_to_provider(
                        table,
                        generated.name,
                        &provider,
                        &invalid_arguments,
                        |_| Ok(())
                    ),
                    Err(expected),
                    "registry route rejects invalid optional type for {}.{}",
                    contract.id.as_str(),
                    parameter.name
                );
                *counter += 1;
                assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
            }
            optional_parameters += 1;
        }
        optional_operations += 1;
    }

    assert_eq!(optional_operations, 6);
    assert_eq!(optional_parameters, 10);
    assert_eq!(generated_binding_cases, optional_operations);
    assert_eq!(null_direct_routes, optional_operations);
    assert_eq!(null_registry_routes, optional_operations);
    assert_eq!(present_direct_routes, optional_operations);
    assert_eq!(present_registry_routes, optional_operations);
    assert_eq!(bare_inner_rejections, optional_parameters);
    assert_eq!(wrong_optional_rejections, optional_parameters);
    println!(
        "provider_optional_dispatch_parity operations={optional_operations} optional_arguments={optional_parameters} null_direct={null_direct_routes} null_registry={null_registry_routes} present_direct={present_direct_routes} present_registry={present_registry_routes} bare_inner_rejected={bare_inner_rejections} wrong_optional_rejected={wrong_optional_rejections} total_cases={}",
        generated_binding_cases
            + null_direct_routes
            + null_registry_routes
            + present_direct_routes
            + present_registry_routes
            + bare_inner_rejections
            + wrong_optional_rejections
    );
}

#[test]
fn provider_argument_map_variadic_cardinalities_match_both_dispatch_routes() {
    const VARIADIC_OPERATIONS: [&str; 4] = [
        "sys.invoke(Value)",
        "sys.invoke<T>",
        "sys.start(Value)",
        "sys.start<T>",
    ];
    const EDGE_CARDINALITIES: [usize; 3] = [0, 1, 4];

    let table = system_dispatch_table();
    let generated_schema = build_provider::generate_provider_registry_schema()
        .expect("provider schema regenerates for argument-map dispatch parity");
    assert_eq!(generated_schema, system_provider_abi_schema_json());
    build_host::validate_json_against_schema(system_provider_abi_json(), &generated_schema)
        .expect("embedded registry conforms to the regenerated provider schema");

    let registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("typed provider registry resolves baked role offers");
    let mut direct_routes = 0;
    let mut registry_routes = 0;
    let mut map_cases = 0;

    for operation_name in VARIADIC_OPERATIONS {
        let contract = table
            .operation(operation_name)
            .expect("argument-map overload is in the typed provider table");
        let generated = system_function_descriptor(operation_name)
            .expect("argument-map overload has a macro-generated binding");
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(
            contract.effects.iter().next(),
            Some(generated.effect),
            "generated effect matches {operation_name}"
        );
        let map_indexes = contract
            .signature
            .parameters
            .iter()
            .enumerate()
            .filter_map(|(index, parameter)| (parameter.name == "arguments").then_some(index))
            .collect::<Vec<_>>();
        assert_eq!(map_indexes, [1], "one fixed map slot in {operation_name}");
        assert_eq!(
            contract.signature.parameters[map_indexes[0]].ty,
            AbiType::Named("sys.ArgumentMap".to_owned())
        );

        let role = contract
            .role
            .as_ref()
            .expect("argument-map overload has a provider role");
        let offer = registry
            .resolve(role.as_str())
            .expect("argument-map operation provider offer resolves")
            .clone();
        let expected_argument_types = contract
            .signature
            .parameters
            .iter()
            .map(|parameter| parameter.ty.canonical())
            .collect::<Vec<_>>();

        for cardinality in EDGE_CARDINALITIES {
            let argument_map = ArgumentMap::new((0..cardinality).map(|index| Argument {
                name: format!("arg_{index:02}"),
                value: TypedValue::public(
                    TypeId::new("Str"),
                    format!("value-{index}").into_bytes(),
                ),
            }))
            .expect("distinct argument names form a map");
            assert_eq!(argument_map.entries().count(), cardinality);
            let encoded_entries = argument_map
                .entries()
                .map(|(name, value)| {
                    (
                        name,
                        value.static_type().as_str(),
                        value.canonical().expect("test map values are public"),
                    )
                })
                .collect::<Vec<_>>();
            let map_payload = serde_json::to_vec(&encoded_entries)
                .expect("test argument-map payload has deterministic JSON bytes");

            let arguments = contract
                .signature
                .parameters
                .iter()
                .enumerate()
                .map(|(index, parameter)| {
                    let canonical = if index == map_indexes[0] {
                        map_payload.clone()
                    } else {
                        format!("{}-{operation_name}-{cardinality}", parameter.name).into_bytes()
                    };
                    TypedValue::public(TypeId::new(parameter.ty.canonical()), canonical)
                })
                .collect::<Vec<_>>();
            let result = TypedValue::public(
                TypeId::new(contract.signature.result.canonical()),
                format!("result-{operation_name}-{cardinality}").into_bytes(),
            );
            let provider_for = || ArgumentMapEdgeProvider {
                offer: offer.clone(),
                operation: contract.id.clone(),
                expected_argument_types: expected_argument_types.clone(),
                expected_map_payload: map_payload.clone(),
                result: result.clone(),
                calls: AtomicUsize::new(0),
            };

            let direct_provider = provider_for();
            assert_eq!(
                table
                    .dispatch_to_provider(generated.name, &direct_provider, &arguments, |_| Ok(()))
                    .expect("direct dispatch accepts the map cardinality edge"),
                orna_sys_v1::SystemDispatchResult::Returned(result.clone()),
                "direct route preserves {operation_name} with {cardinality} map entries"
            );
            assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 1);
            direct_routes += 1;

            let registry_provider = provider_for();
            assert_eq!(
                registry
                    .dispatch_to_provider(
                        table,
                        generated.name,
                        &registry_provider,
                        &arguments,
                        |_| Ok(()),
                    )
                    .expect("selected-provider dispatch accepts the map cardinality edge"),
                orna_sys_v1::SystemDispatchResult::Returned(result),
                "registry route preserves {operation_name} with {cardinality} map entries"
            );
            assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 1);
            registry_routes += 1;
            map_cases += 1;
        }
    }

    let operation_count = VARIADIC_OPERATIONS.len();
    assert_eq!(operation_count, 4);
    assert_eq!(map_cases, operation_count * EDGE_CARDINALITIES.len());
    assert_eq!(direct_routes, map_cases);
    assert_eq!(registry_routes, map_cases);
    println!(
        "provider_argument_map_variadic_parity operations={operation_count} cardinalities=0,1,4 map_cases={map_cases} direct_routes={direct_routes} registry_routes={registry_routes} generated_bindings={operation_count} schema_validated=1 total_cases={}",
        operation_count + map_cases + direct_routes + registry_routes + 1
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
        calls: AtomicUsize::new(0),
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
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    let failure_code = FailureCode::new("sys.invoke.argument_missing").unwrap();
    assert!(contract.declares_failure(&failure_code));
    let failing_provider = InvokeValueProvider {
        offer,
        operation: contract.id.clone(),
        argument_types,
        response: Err(failure_code.clone()),
        calls: AtomicUsize::new(0),
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

#[test]
fn generated_dispatch_diagnostics_match_registry_and_direct_paths() {
    fn expect_diagnostic<T>(result: Result<T, ProviderDiagnostic>) -> ProviderDiagnostic {
        match result {
            Err(diagnostic) => diagnostic,
            Ok(_) => panic!("invalid provider dispatch unexpectedly succeeded"),
        }
    }

    let table = system_dispatch_table();
    let operation_name = "sys.invoke(Value)";
    let contract = table
        .operation(operation_name)
        .expect("generated typed registry contains sys.invoke(Value)");
    let generated = system_function_descriptor(operation_name)
        .expect("macro-generated binding contains sys.invoke(Value)");
    assert_eq!(generated.name, contract.id.as_str());
    assert_eq!(generated.signature, contract.signature.source);
    assert_eq!(
        contract.effects.iter().next(),
        Some(generated.effect),
        "generated binding and typed dispatch effects match"
    );
    let role_id = contract
        .role
        .as_ref()
        .expect("generated invoke binding selects a provider role");
    let registry = ProviderRoleRegistry::from_baked_abi(table).unwrap();
    let offer = registry
        .resolve(role_id.as_str())
        .expect("generated invoke role resolves a baked provider")
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
    let expected_result = contract.signature.result.canonical();
    let provider_for = |response| InvokeValueProvider {
        offer: offer.clone(),
        operation: contract.id.clone(),
        argument_types: argument_types.clone(),
        response,
        calls: AtomicUsize::new(0),
    };
    let mut diagnostic_cases = 0;

    let mut wrong_arity = arguments.clone();
    if wrong_arity.is_empty() {
        wrong_arity.push(TypedValue::public(
            TypeId::new("sys.conformance.Unexpected"),
            b"extra".to_vec(),
        ));
    } else {
        wrong_arity.pop();
    }
    let expected_count = ProviderDiagnostic::ArgumentCountMismatch {
        operation: contract.id.clone(),
        expected: contract.signature.parameters.len(),
        actual: wrong_arity.len(),
    };
    let count_registry_provider = provider_for(Ok(TypedValue::public(
        TypeId::new(expected_result.clone()),
        b"unused".to_vec(),
    )));
    let count_direct_provider = provider_for(Ok(TypedValue::public(
        TypeId::new(expected_result.clone()),
        b"unused".to_vec(),
    )));
    let registry_count = expect_diagnostic(registry.dispatch_to_provider(
        table,
        operation_name,
        &count_registry_provider,
        &wrong_arity,
        |_| Ok(()),
    ));
    let direct_count = expect_diagnostic(table.dispatch_to_provider(
        operation_name,
        &count_direct_provider,
        &wrong_arity,
        |_| Ok(()),
    ));
    assert_eq!(registry_count, expected_count);
    assert_eq!(direct_count, expected_count);
    assert_eq!(registry_count, direct_count);
    assert_eq!(registry_count.code(), "sys.abi.argument_count_mismatch");
    assert_eq!(count_registry_provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(count_direct_provider.calls.load(Ordering::SeqCst), 0);
    diagnostic_cases += 1;

    assert!(
        !arguments.is_empty(),
        "sys.invoke(Value) has a typed argument"
    );
    let mismatched_type = "sys.conformance.DispatchMismatch";
    let expected_argument = contract.signature.parameters[0].ty.canonical();
    assert_ne!(expected_argument, mismatched_type);
    let mut wrong_type_arguments = arguments.clone();
    wrong_type_arguments[0] =
        TypedValue::public(TypeId::new(mismatched_type), b"wrong-type".to_vec());
    let expected_argument_diagnostic = ProviderDiagnostic::ArgumentTypeMismatch {
        operation: contract.id.clone(),
        parameter: contract.signature.parameters[0].name.clone(),
        expected: expected_argument,
        actual: mismatched_type.to_owned(),
    };
    let argument_registry_provider = provider_for(Ok(TypedValue::public(
        TypeId::new(expected_result.clone()),
        b"unused".to_vec(),
    )));
    let argument_direct_provider = provider_for(Ok(TypedValue::public(
        TypeId::new(expected_result.clone()),
        b"unused".to_vec(),
    )));
    let registry_argument = expect_diagnostic(registry.dispatch_to_provider(
        table,
        operation_name,
        &argument_registry_provider,
        &wrong_type_arguments,
        |_| Ok(()),
    ));
    let direct_argument = expect_diagnostic(table.dispatch_to_provider(
        operation_name,
        &argument_direct_provider,
        &wrong_type_arguments,
        |_| Ok(()),
    ));
    assert_eq!(registry_argument, expected_argument_diagnostic);
    assert_eq!(direct_argument, expected_argument_diagnostic);
    assert_eq!(registry_argument, direct_argument);
    assert_eq!(registry_argument.code(), "sys.abi.argument_type_mismatch");
    assert_eq!(argument_registry_provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(argument_direct_provider.calls.load(Ordering::SeqCst), 0);
    diagnostic_cases += 1;

    let mismatched_result = "sys.conformance.DispatchMismatch";
    assert_ne!(expected_result, mismatched_result);
    let expected_result_diagnostic = ProviderDiagnostic::ResultTypeMismatch {
        operation: contract.id.clone(),
        expected: expected_result,
        actual: mismatched_result.to_owned(),
    };
    let result_registry_provider = provider_for(Ok(TypedValue::public(
        TypeId::new(mismatched_result),
        b"wrong-result".to_vec(),
    )));
    let result_direct_provider = provider_for(Ok(TypedValue::public(
        TypeId::new(mismatched_result),
        b"wrong-result".to_vec(),
    )));
    let registry_result = expect_diagnostic(registry.dispatch_to_provider(
        table,
        operation_name,
        &result_registry_provider,
        &arguments,
        |_| Ok(()),
    ));
    let direct_result = expect_diagnostic(table.dispatch_to_provider(
        operation_name,
        &result_direct_provider,
        &arguments,
        |_| Ok(()),
    ));
    assert_eq!(registry_result, expected_result_diagnostic);
    assert_eq!(direct_result, expected_result_diagnostic);
    assert_eq!(registry_result, direct_result);
    assert_eq!(registry_result.code(), "sys.abi.result_type_mismatch");
    assert_eq!(result_registry_provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(result_direct_provider.calls.load(Ordering::SeqCst), 1);
    diagnostic_cases += 1;

    println!(
        "generated_dispatch_diagnostic_parity operation={operation_name} diagnostics=argument_count,argument_type,result_type direct_registry_pairs={diagnostic_cases} provider_not_called_on_invalid_arguments=true total_cases={}",
        diagnostic_cases * 2
    );
}

#[test]
fn dispatch_to_provider_checks_arity_argument_types_and_generic_result_binding() {
    let table = system_dispatch_table();
    let operation_name = "sys.invoke(Value)";
    let contract = table.operation(operation_name).expect("typed overload");
    let role_id = contract.role.as_ref().expect("provider-bound operation");
    let registry = ProviderRoleRegistry::from_baked_abi(table).unwrap();
    let offer = registry.resolve(role_id.as_str()).unwrap().clone();
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
    let provider = InvokeValueProvider {
        offer: offer.clone(),
        operation: contract.id.clone(),
        argument_types: argument_types.clone(),
        response: Ok(TypedValue::public(
            TypeId::new(contract.signature.result.canonical()),
            b"typed-provider-result".to_vec(),
        )),
        calls: AtomicUsize::new(0),
    };

    assert_eq!(
        table.dispatch_to_provider(
            operation_name,
            &provider,
            &arguments[..arguments.len() - 1],
            |_| Ok(())
        ),
        Err(ProviderDiagnostic::ArgumentCountMismatch {
            operation: contract.id.clone(),
            expected: arguments.len(),
            actual: arguments.len() - 1,
        })
    );

    let mut wrong_argument_type = arguments.clone();
    wrong_argument_type[0] = TypedValue::public(
        TypeId::new("sys.NotTheDeclaredArgument"),
        b"invalid-argument".to_vec(),
    );
    assert_eq!(
        table.dispatch_to_provider(operation_name, &provider, &wrong_argument_type, |_| Ok(())),
        Err(ProviderDiagnostic::ArgumentTypeMismatch {
            operation: contract.id.clone(),
            parameter: contract.signature.parameters[0].name.clone(),
            expected: contract.signature.parameters[0].ty.canonical(),
            actual: "sys.NotTheDeclaredArgument".to_owned(),
        })
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "arity and argument-type violations stop before invoking a provider"
    );

    let wrong_result_provider = InvokeValueProvider {
        offer,
        operation: contract.id.clone(),
        argument_types,
        response: Ok(TypedValue::public(
            TypeId::new("sys.NotTheDeclaredResult"),
            b"invalid-result".to_vec(),
        )),
        calls: AtomicUsize::new(0),
    };
    assert_eq!(
        table.dispatch_to_provider(operation_name, &wrong_result_provider, &arguments, |_| Ok(
            ()
        )),
        Err(ProviderDiagnostic::ResultTypeMismatch {
            operation: contract.id.clone(),
            expected: contract.signature.result.canonical(),
            actual: "sys.NotTheDeclaredResult".to_owned(),
        })
    );
    assert_eq!(wrong_result_provider.calls.load(Ordering::SeqCst), 1);

    let generic_abi = serde_json::json!({
        "abi_version": {"major": 1, "minor": 0},
        "operations": [{
            "name": "sys.meta<T>",
            "version": {"major": 1, "minor": 0},
            "signature": "fn sys.meta<T>(value: T): sys.ValueMetadata<T>",
            "effect": "read",
            "preconditions": [],
            "failures": [],
            "role": "langitem.sys.meta@1.0"
        }],
        "roles": [{
            "name": "langitem.sys.meta",
            "version": {"major": 1, "minor": 0},
            "effects": ["read"],
            "operations": ["sys.meta<T>"],
            "required": false,
            "replaceable": true,
            "builtin_provider": null
        }]
    });
    let generic_table = SystemProviderAbi::from_json(&generic_abi.to_string()).unwrap();
    let generic_operation_name = "sys.meta<T>";
    let generic_contract = generic_table.operation(generic_operation_name).unwrap();
    let generic_role = generic_contract.role.as_ref().unwrap();
    let generic_offer = ProviderOffer {
        provider: ProviderId::new("fixture.metadata").unwrap(),
        role: generic_role.clone(),
        version: AbiVersion::V1_0,
        effects: EffectSet::one(SystemEffect::Read),
    };
    let generic_arguments = [TypedValue::public(
        TypeId::new("sys.Example"),
        b"example".to_vec(),
    )];
    let unbound_registry = ProviderRoleRegistry::from_baked_abi(&generic_table).unwrap();
    let mut generic_registry = ProviderRoleRegistry::from_baked_abi(&generic_table).unwrap();
    generic_registry.bind(generic_offer.clone()).unwrap();
    let generic_result = TypedValue::public(
        TypeId::new("sys.ValueMetadata<sys.Example>"),
        b"metadata".to_vec(),
    );
    let generic_provider = InvokeValueProvider {
        offer: generic_offer.clone(),
        operation: generic_contract.id.clone(),
        argument_types: vec!["sys.Example".to_owned()],
        response: Ok(generic_result.clone()),
        calls: AtomicUsize::new(0),
    };
    assert_eq!(
        generic_table.dispatch_to_provider(
            generic_operation_name,
            &generic_provider,
            &generic_arguments,
            |_| Ok(())
        ),
        Err(ProviderDiagnostic::RoleUnavailable {
            role: generic_role.clone(),
            version: AbiVersion::V1_0,
        }),
        "direct dispatch has no baked provider to select for this optional role"
    );
    assert_eq!(generic_provider.calls.load(Ordering::SeqCst), 0);

    assert_eq!(
        unbound_registry.dispatch_to_provider(
            &generic_table,
            generic_operation_name,
            &generic_provider,
            &generic_arguments,
            |_| Ok(())
        ),
        Err(ProviderDiagnostic::RoleUnavailable {
            role: generic_role.clone(),
            version: AbiVersion::V1_0,
        }),
        "registry dispatch diagnoses a role without a selected provider"
    );
    assert_eq!(generic_provider.calls.load(Ordering::SeqCst), 0);

    assert_eq!(
        generic_registry.dispatch_to_provider(
            &generic_table,
            generic_operation_name,
            &generic_provider,
            &generic_arguments,
            |_| Ok(())
        ),
        Ok(orna_sys_v1::SystemDispatchResult::Returned(
            generic_result.clone()
        ))
    );
    assert_eq!(generic_provider.calls.load(Ordering::SeqCst), 1);

    let selected_generic_offer = generic_registry
        .resolve(generic_role.as_str())
        .unwrap()
        .clone();
    let alternate_generic_offer = ProviderOffer {
        provider: ProviderId::new("fixture.generic_alternate").unwrap(),
        ..selected_generic_offer.clone()
    };
    let mut alternate_registry = ProviderRoleRegistry::from_baked_abi(&generic_table).unwrap();
    alternate_registry
        .bind(alternate_generic_offer.clone())
        .expect("replaceable generic role selects a compatible alternate provider");
    let alternate_result = TypedValue::public(
        TypeId::new("sys.ValueMetadata<sys.Example>"),
        b"alternate-metadata".to_vec(),
    );
    let alternate_provider = InvokeValueProvider {
        offer: alternate_generic_offer.clone(),
        operation: generic_contract.id.clone(),
        argument_types: vec!["sys.Example".to_owned()],
        response: Ok(alternate_result.clone()),
        calls: AtomicUsize::new(0),
    };
    assert_eq!(
        generic_table.dispatch_to_provider(
            generic_operation_name,
            &alternate_provider,
            &generic_arguments,
            |_| Ok(())
        ),
        Err(ProviderDiagnostic::RoleUnavailable {
            role: generic_role.clone(),
            version: AbiVersion::V1_0,
        }),
        "direct dispatch cannot bypass the registry for a replaceable role"
    );
    assert_eq!(alternate_provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        alternate_registry.dispatch_to_provider(
            &generic_table,
            generic_operation_name,
            &alternate_provider,
            &generic_arguments,
            |_| Ok(())
        ),
        Ok(orna_sys_v1::SystemDispatchResult::Returned(
            alternate_result
        ))
    );
    assert_eq!(alternate_provider.calls.load(Ordering::SeqCst), 1);

    println!(
        "dispatch_to_provider_type_matrix operation={operation_name} rejected=argument_count,argument_type,result_type,direct_unselected_provider,direct_unbound_role registry_unbound=1 generic_bindings=1 selected_replacement=1 total_cases=8"
    );
}

#[test]
fn registry_dispatch_enforces_provider_selection_and_generated_binding_parity() {
    let table = system_dispatch_table();
    let registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("generated provider offers resolve from the typed registry");
    let mut generated_operation_cases = 0;
    let mut provider_route_cases = 0;
    let mut unselected_provider_rejections = 0;
    let mut direct_provider_rejections = 0;
    let mut replaceable_provider_routes = 0;

    for contract in table.operations() {
        let operation_name = contract.id.as_str();
        let generated = system_function_descriptor(operation_name)
            .unwrap_or_else(|| panic!("missing generated binding for {operation_name}"));
        assert_eq!(generated.name, operation_name);
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(
            contract.effects.iter().next(),
            Some(generated.effect),
            "typed dispatch effect matches the generated binding for {operation_name}"
        );
        generated_operation_cases += 1;

        let Some(role_id) = &contract.role else {
            continue;
        };
        let selected_offer = registry.resolve(role_id.as_str()).unwrap().clone();
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
        let result = TypedValue::public(
            TypeId::new(contract.signature.result.canonical()),
            b"generated-binding-result".to_vec(),
        );
        let provider = InvokeValueProvider {
            offer: selected_offer.clone(),
            operation: contract.id.clone(),
            argument_types: argument_types.clone(),
            response: Ok(result.clone()),
            calls: AtomicUsize::new(0),
        };
        assert_eq!(
            registry.dispatch_to_provider(table, generated.name, &provider, &arguments, |_| Ok(())),
            Ok(orna_sys_v1::SystemDispatchResult::Returned(result)),
            "registry-selected provider dispatches generated operation {operation_name}"
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        provider_route_cases += 1;

        let unselected_offer = ProviderOffer {
            provider: ProviderId::new("fixture.unselected").unwrap(),
            ..selected_offer.clone()
        };
        let unselected_provider = InvokeValueProvider {
            offer: unselected_offer.clone(),
            operation: contract.id.clone(),
            argument_types,
            response: Ok(TypedValue::public(
                TypeId::new(contract.signature.result.canonical()),
                b"must-not-run".to_vec(),
            )),
            calls: AtomicUsize::new(0),
        };
        assert_eq!(
            registry.dispatch_to_provider(
                table,
                operation_name,
                &unselected_provider,
                &arguments,
                |_| Ok(())
            ),
            Err(ProviderDiagnostic::ProviderNotSelected {
                role: role_id.clone(),
                expected: selected_offer.clone(),
                provided: unselected_offer.clone(),
            }),
            "compatible but unselected provider cannot dispatch {operation_name}"
        );
        assert_eq!(unselected_provider.calls.load(Ordering::SeqCst), 0);
        unselected_provider_rejections += 1;

        assert_eq!(
            table.dispatch_to_provider(
                operation_name,
                &unselected_provider,
                &arguments,
                |_| Ok(())
            ),
            Err(ProviderDiagnostic::ProviderNotSelected {
                role: role_id.clone(),
                expected: selected_offer.clone(),
                provided: unselected_offer.clone(),
            }),
            "direct dispatch cannot bypass selected-offer identity for {operation_name}"
        );
        assert_eq!(unselected_provider.calls.load(Ordering::SeqCst), 0);
        direct_provider_rejections += 1;

        let role = table.role(role_id.as_str()).unwrap();
        if role.replaceable {
            let mut replacement_registry = ProviderRoleRegistry::from_baked_abi(table).unwrap();
            replacement_registry
                .bind(unselected_offer.clone())
                .expect("replaceable role accepts the selected alternate provider");
            let replacement_result = TypedValue::public(
                TypeId::new(contract.signature.result.canonical()),
                b"selected-alternate-result".to_vec(),
            );
            let replacement_provider = InvokeValueProvider {
                offer: unselected_offer.clone(),
                operation: contract.id.clone(),
                argument_types: contract
                    .signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter.ty.canonical())
                    .collect(),
                response: Ok(replacement_result.clone()),
                calls: AtomicUsize::new(0),
            };
            assert_eq!(
                replacement_registry.dispatch_to_provider(
                    table,
                    operation_name,
                    &replacement_provider,
                    &arguments,
                    |_| Ok(())
                ),
                Ok(orna_sys_v1::SystemDispatchResult::Returned(
                    replacement_result
                )),
                "registry-selected replacement dispatches {operation_name}"
            );
            assert_eq!(replacement_provider.calls.load(Ordering::SeqCst), 1);
            replaceable_provider_routes += 1;
        }
    }

    assert_eq!(generated_operation_cases, table.operations().count());
    assert!(provider_route_cases > 0);
    assert_eq!(provider_route_cases, unselected_provider_rejections);
    assert_eq!(provider_route_cases, direct_provider_rejections);
    println!(
        "generated_binding_provider_dispatch_parity operations={generated_operation_cases} provider_routes={provider_route_cases} unselected_rejections={unselected_provider_rejections} direct_rejections={direct_provider_rejections} replaceable_routes={replaceable_provider_routes} total_cases={}",
        generated_operation_cases
            + provider_route_cases
            + unselected_provider_rejections
            + direct_provider_rejections
            + replaceable_provider_routes
    );
}

#[test]
fn generated_bindings_round_trip_typed_results_through_both_dispatch_paths() {
    let table = system_dispatch_table();
    let registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("generated provider offers resolve from the typed registry");
    let mut generated_binding_cases = 0;
    let mut provider_route_cases = 0;

    for contract in table.operations() {
        let operation_name = contract.id.as_str();
        let generated = system_function_descriptor(operation_name)
            .unwrap_or_else(|| panic!("missing generated binding for {operation_name}"));
        assert_eq!(generated.name, operation_name);
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(
            contract.effects.iter().next(),
            Some(generated.effect),
            "generated binding effect round-trips for {operation_name}"
        );
        generated_binding_cases += 1;

        let Some(role_id) = &contract.role else {
            continue;
        };
        let offer = registry
            .resolve(role_id.as_str())
            .expect("generated binding role has a selected provider")
            .clone();
        let argument_types = contract
            .signature
            .parameters
            .iter()
            .map(|parameter| parameter.ty.canonical())
            .collect::<Vec<_>>();
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
        let result = TypedValue::public(
            TypeId::new(contract.signature.result.canonical()),
            b"generated-binding-round-trip".to_vec(),
        );
        let provider_for = || InvokeValueProvider {
            offer: offer.clone(),
            operation: contract.id.clone(),
            argument_types: argument_types.clone(),
            response: Ok(result.clone()),
            calls: AtomicUsize::new(0),
        };
        let direct_provider = provider_for();
        let direct_result = table
            .dispatch_to_provider(operation_name, &direct_provider, &arguments, |_| Ok(()))
            .expect("direct dispatch accepts the generated typed call");
        assert_eq!(
            direct_result,
            orna_sys_v1::SystemDispatchResult::Returned(result.clone()),
            "direct dispatch returns the generated operation result for {operation_name}"
        );
        assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 1);

        let registry_provider = provider_for();
        let registry_result = registry
            .dispatch_to_provider(
                table,
                generated.name,
                &registry_provider,
                &arguments,
                |_| Ok(()),
            )
            .expect("selected-role registry accepts the generated typed call");
        assert_eq!(
            registry_result, direct_result,
            "selected-role and direct results match for {operation_name}"
        );
        assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 1);
        provider_route_cases += 1;
    }

    assert_eq!(generated_binding_cases, table.operations().count());
    assert!(provider_route_cases > 0);
    println!(
        "generated_binding_typed_result_round_trip bindings={generated_binding_cases} provider_routes={provider_route_cases} direct_registry_pairs={provider_route_cases} total_cases={}",
        generated_binding_cases + provider_route_cases * 2
    );
}

#[test]
fn generated_binding_argument_count_diagnostics_match_every_dispatch_route() {
    fn expect_diagnostic<T>(result: Result<T, ProviderDiagnostic>) -> ProviderDiagnostic {
        match result {
            Err(diagnostic) => diagnostic,
            Ok(_) => panic!("invalid generated binding dispatch unexpectedly succeeded"),
        }
    }

    let table = system_dispatch_table();
    let registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("generated provider offers resolve from the typed registry");
    let fallback_role = table
        .roles()
        .next()
        .expect("the dispatch table has at least one provider role");
    let fallback_offer = ProviderOffer {
        provider: ProviderId::new("fixture.binding_diagnostic").unwrap(),
        role: fallback_role.id.clone(),
        version: fallback_role.version,
        effects: fallback_role.effects.clone(),
    };
    let mut generated_binding_cases = 0;
    let mut provider_bound_diagnostic_cases = 0;
    let mut unbound_provider_rejections = 0;

    for contract in table.operations() {
        let operation_name = contract.id.as_str();
        let generated = system_function_descriptor(operation_name)
            .unwrap_or_else(|| panic!("missing generated binding for {operation_name}"));
        assert_eq!(generated.name, operation_name);
        assert_eq!(generated.signature, contract.signature.source);
        let offer = match &contract.role {
            Some(role_id) => registry
                .resolve(role_id.as_str())
                .expect("generated binding role has a selected provider")
                .clone(),
            None => fallback_offer.clone(),
        };
        let mut wrong_arguments = contract
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
        if wrong_arguments.is_empty() {
            wrong_arguments.push(TypedValue::public(
                TypeId::new("sys.conformance.Unexpected"),
                b"extra".to_vec(),
            ));
        } else {
            wrong_arguments.pop();
        }
        let provider = || InvokeValueProvider {
            offer: offer.clone(),
            operation: contract.id.clone(),
            argument_types: contract
                .signature
                .parameters
                .iter()
                .map(|parameter| parameter.ty.canonical())
                .collect(),
            response: Ok(TypedValue::public(
                TypeId::new(contract.signature.result.canonical()),
                b"must-not-be-returned".to_vec(),
            )),
            calls: AtomicUsize::new(0),
        };
        let direct_provider = provider();
        let registry_provider = provider();
        let direct_diagnostic = expect_diagnostic(table.dispatch_to_provider(
            generated.name,
            &direct_provider,
            &wrong_arguments,
            |_| Ok(()),
        ));
        let registry_diagnostic = expect_diagnostic(registry.dispatch_to_provider(
            table,
            generated.name,
            &registry_provider,
            &wrong_arguments,
            |_| Ok(()),
        ));

        let expected_code = if contract.role.is_some() {
            provider_bound_diagnostic_cases += 1;
            assert_eq!(
                direct_diagnostic,
                ProviderDiagnostic::ArgumentCountMismatch {
                    operation: contract.id.clone(),
                    expected: contract.signature.parameters.len(),
                    actual: wrong_arguments.len(),
                },
                "generated operation reports its typed arity diagnostic for {operation_name}"
            );
            "sys.abi.argument_count_mismatch"
        } else {
            unbound_provider_rejections += 1;
            assert_eq!(
                direct_diagnostic,
                ProviderDiagnostic::ProviderNotExecutable(fallback_offer.role.clone()),
                "unroled generated operation reports provider-not-executable for {operation_name}"
            );
            "sys.abi.provider_not_executable"
        };
        assert_eq!(registry_diagnostic, direct_diagnostic);
        assert_eq!(direct_diagnostic.code(), expected_code);
        assert_eq!(registry_diagnostic.code(), expected_code);
        assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 0);
        generated_binding_cases += 1;
    }

    assert_eq!(generated_binding_cases, table.operations().count());
    assert!(provider_bound_diagnostic_cases > 0);
    assert!(unbound_provider_rejections > 0);
    println!(
        "generated_binding_dispatch_diagnostic_parity operations={generated_binding_cases} argument_count_pairs={provider_bound_diagnostic_cases} provider_not_executable_pairs={unbound_provider_rejections} direct_registry_equal=true providers_called=0 total_cases={}",
        generated_binding_cases * 2
    );
}

#[test]
fn generated_idl_dispatch_type_diagnostic_edges_match_every_provider_binding() {
    let table = system_dispatch_table();
    let registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("generated provider offers resolve from the typed registry");
    let idl_operation_ids = system_binding_stubs()
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    let typed_operation_ids = table
        .operations()
        .map(|contract| contract.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        idl_operation_ids, typed_operation_ids,
        "generated IDL markers preserve typed registry order and identity"
    );

    let mut generated_binding_cases = 0;
    let mut provider_routes = 0;
    let mut argument_type_pairs = 0;
    let mut result_type_pairs = 0;
    let mut unconstrained_generic_parameters = 0;

    for contract in table.operations() {
        let operation_name = contract.id.as_str();
        let generated = system_function_descriptor(operation_name)
            .unwrap_or_else(|| panic!("missing generated binding for {operation_name}"));
        assert_eq!(generated.name, operation_name);
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(contract.effects.iter().next(), Some(generated.effect));
        generated_binding_cases += 1;

        let Some(role_id) = &contract.role else {
            continue;
        };
        let offer = registry
            .resolve(role_id.as_str())
            .expect("generated binding role has a selected provider")
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
        let expected_result_type = contract.signature.result.canonical();

        for (parameter_index, parameter) in contract.signature.parameters.iter().enumerate() {
            if matches!(
                &parameter.ty,
                AbiType::Named(name)
                    if contract.signature.type_parameters.iter().any(|generic| generic == name)
            ) {
                unconstrained_generic_parameters += 1;
                continue;
            }

            let actual_type = format!(
                "sys.conformance.DispatchMismatch{generated_binding_cases}_{parameter_index}"
            );
            assert_ne!(parameter.ty.canonical(), actual_type);
            let mut wrong_arguments = arguments.clone();
            wrong_arguments[parameter_index] = TypedValue::public(
                TypeId::new(actual_type.clone()),
                b"wrong-typed-idl-argument".to_vec(),
            );
            let expected = ProviderDiagnostic::ArgumentTypeMismatch {
                operation: contract.id.clone(),
                parameter: parameter.name.clone(),
                expected: parameter.ty.canonical(),
                actual: actual_type,
            };
            let provider_for = || InvokeValueProvider {
                offer: offer.clone(),
                operation: contract.id.clone(),
                argument_types: argument_types.clone(),
                response: Ok(TypedValue::public(
                    TypeId::new(expected_result_type.clone()),
                    b"must-not-run".to_vec(),
                )),
                calls: AtomicUsize::new(0),
            };
            let direct_provider = provider_for();
            let registry_provider = provider_for();
            let direct = table.dispatch_to_provider(
                generated.name,
                &direct_provider,
                &wrong_arguments,
                |_| Ok(()),
            );
            let mediated = registry.dispatch_to_provider(
                table,
                generated.name,
                &registry_provider,
                &wrong_arguments,
                |_| Ok(()),
            );
            assert_eq!(direct, Err(expected.clone()));
            assert_eq!(mediated, Err(expected));
            assert_eq!(direct.unwrap_err().code(), "sys.abi.argument_type_mismatch");
            assert_eq!(
                mediated.unwrap_err().code(),
                "sys.abi.argument_type_mismatch"
            );
            assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 0);
            assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 0);
            argument_type_pairs += 1;
        }

        let wrong_result_type = "sys.conformance.DispatchMismatchResult";
        assert_ne!(expected_result_type, wrong_result_type);
        let expected_result_diagnostic = ProviderDiagnostic::ResultTypeMismatch {
            operation: contract.id.clone(),
            expected: expected_result_type.clone(),
            actual: wrong_result_type.to_owned(),
        };
        let provider_for_wrong_result = || InvokeValueProvider {
            offer: offer.clone(),
            operation: contract.id.clone(),
            argument_types: argument_types.clone(),
            response: Ok(TypedValue::public(
                TypeId::new(wrong_result_type),
                b"wrong-typed-idl-result".to_vec(),
            )),
            calls: AtomicUsize::new(0),
        };
        let direct_provider = provider_for_wrong_result();
        let registry_provider = provider_for_wrong_result();
        let direct =
            table.dispatch_to_provider(generated.name, &direct_provider, &arguments, |_| Ok(()));
        let mediated = registry.dispatch_to_provider(
            table,
            generated.name,
            &registry_provider,
            &arguments,
            |_| Ok(()),
        );
        assert_eq!(direct, Err(expected_result_diagnostic.clone()));
        assert_eq!(mediated, Err(expected_result_diagnostic));
        assert_eq!(direct.unwrap_err().code(), "sys.abi.result_type_mismatch");
        assert_eq!(mediated.unwrap_err().code(), "sys.abi.result_type_mismatch");
        assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 1);
        result_type_pairs += 1;
        provider_routes += 1;
    }

    assert_eq!(generated_binding_cases, typed_operation_ids.len());
    assert!(provider_routes > 0);
    assert_eq!(result_type_pairs, provider_routes);
    println!(
        "generated_idl_dispatch_type_diagnostic_parity bindings={generated_binding_cases} provider_routes={provider_routes} argument_type_pairs={argument_type_pairs} result_type_pairs={result_type_pairs} unconstrained_generic_parameters={unconstrained_generic_parameters} direct_registry_equal=true total_cases={}",
        generated_binding_cases + (argument_type_pairs + result_type_pairs) * 2
    );
}

#[test]
fn generated_bindings_preserve_registry_precondition_failure_edges() {
    let table = system_dispatch_table();
    let registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("generated provider offers resolve from the typed registry");
    let idl_operation_ids = system_binding_stubs()
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    let typed_operation_ids = table
        .operations()
        .map(|contract| contract.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(idl_operation_ids, typed_operation_ids);

    let declared_precondition = FailureCode::new("sys.abi.precondition_failed").unwrap();
    let undeclared_precondition =
        FailureCode::new("sys.conformance.registry_precondition").unwrap();
    let fallback_role = table
        .roles()
        .next()
        .expect("generated registry has a provider role");
    let fallback_offer = ProviderOffer {
        provider: ProviderId::new("fixture.precondition").unwrap(),
        role: fallback_role.id.clone(),
        version: fallback_role.version,
        effects: fallback_role.effects.clone(),
    };
    let mut generated_bindings = 0;
    let mut provider_routes = 0;
    let mut precondition_routes = 0;
    let mut diagnostic_pairs = 0;

    for contract in table.operations() {
        let generated = system_function_descriptor(contract.id.as_str())
            .unwrap_or_else(|| panic!("missing generated binding for {}", contract.id.as_str()));
        assert_eq!(generated.name, contract.id.as_str());
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(contract.effects.iter().next(), Some(generated.effect));
        generated_bindings += 1;

        let offer = match &contract.role {
            Some(role_id) => {
                provider_routes += 1;
                registry
                    .resolve(role_id.as_str())
                    .expect("generated binding role has a selected provider")
                    .clone()
            }
            None => fallback_offer.clone(),
        };
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
        let result = TypedValue::public(
            TypeId::new(contract.signature.result.canonical()),
            b"must-not-run-after-precondition-failure".to_vec(),
        );

        if contract.preconditions.is_empty() {
            continue;
        }
        precondition_routes += 1;
        assert!(contract.declares_failure(&declared_precondition));
        assert!(!contract.declares_failure(&undeclared_precondition));

        for failure in [&declared_precondition, &undeclared_precondition] {
            let provider_for = || InvokeValueProvider {
                offer: offer.clone(),
                operation: contract.id.clone(),
                argument_types: argument_types.clone(),
                response: Ok(result.clone()),
                calls: AtomicUsize::new(0),
            };
            let direct_provider = provider_for();
            let registry_provider = provider_for();
            let mut direct_checks = 0;
            let mut registry_checks = 0;
            let direct = table.dispatch_to_provider(
                generated.name,
                &direct_provider,
                &arguments,
                |precondition| {
                    assert!(contract.preconditions.contains(precondition));
                    direct_checks += 1;
                    Err(failure.clone())
                },
            );
            let mediated = registry.dispatch_to_provider(
                table,
                generated.name,
                &registry_provider,
                &arguments,
                |precondition| {
                    assert!(contract.preconditions.contains(precondition));
                    registry_checks += 1;
                    Err(failure.clone())
                },
            );
            let expected = if failure == &declared_precondition {
                Ok(orna_sys_v1::SystemDispatchResult::Failed(failure.clone()))
            } else {
                Err(ProviderDiagnostic::UndeclaredFailure {
                    operation: contract.id.clone(),
                    code: failure.clone(),
                })
            };
            assert_eq!(direct, expected);
            assert_eq!(mediated, expected);
            assert_eq!(direct_checks, 1);
            assert_eq!(registry_checks, 1);
            assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 0);
            assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 0);
            if failure == &undeclared_precondition {
                assert_eq!(direct.unwrap_err().code(), "sys.abi.undeclared_failure");
                assert_eq!(mediated.unwrap_err().code(), "sys.abi.undeclared_failure");
            }
            diagnostic_pairs += 1;
        }
    }

    assert_eq!(generated_bindings, typed_operation_ids.len());
    assert!(provider_routes > 0);
    assert!(precondition_routes > 0);
    assert_eq!(diagnostic_pairs, precondition_routes * 2);
    println!(
        "generated_binding_registry_precondition_edges bindings={generated_bindings} provider_routes={provider_routes} precondition_routes={precondition_routes} declared_pairs={precondition_routes} undeclared_pairs={precondition_routes} codes=precondition_failed,undeclared_failure providers_called=0 total_cases={}",
        generated_bindings + diagnostic_pairs * 2
    );
}
#[test]
fn generated_bindings_preserve_registry_provider_failure_edges() {
    const SHARED_FAILURES: [&str; 3] = [
        "sys.abi.precondition_failed",
        "sys.abi.unavailable",
        "sys.abi.provider_failed",
    ];

    let table = system_dispatch_table();
    let registry = ProviderRoleRegistry::from_baked_abi(table)
        .expect("generated provider offers resolve from the typed registry");
    let idl_operation_ids = system_binding_stubs()
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    let typed_operation_ids = table
        .operations()
        .map(|contract| contract.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(idl_operation_ids, typed_operation_ids);
    let undeclared = FailureCode::new("sys.conformance.registry_provider_failure").unwrap();
    let response_codes = SHARED_FAILURES
        .into_iter()
        .map(|code| FailureCode::new(code).unwrap())
        .chain([undeclared])
        .collect::<Vec<_>>();
    let mut generated_bindings = 0;
    let mut provider_routes = 0;
    let mut shared_failure_pairs = 0;
    let mut undeclared_failure_pairs = 0;

    for contract in table.operations() {
        let generated = system_function_descriptor(contract.id.as_str())
            .unwrap_or_else(|| panic!("missing generated binding for {}", contract.id.as_str()));
        assert_eq!(generated.name, contract.id.as_str());
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(contract.effects.iter().next(), Some(generated.effect));
        generated_bindings += 1;

        let Some(role_id) = &contract.role else {
            continue;
        };
        provider_routes += 1;
        let offer = registry
            .resolve(role_id.as_str())
            .expect("generated binding role has a selected provider")
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

        for code in &response_codes {
            let provider_for = || InvokeValueProvider {
                offer: offer.clone(),
                operation: contract.id.clone(),
                argument_types: argument_types.clone(),
                response: Err(code.clone()),
                calls: AtomicUsize::new(0),
            };
            let direct_provider = provider_for();
            let registry_provider = provider_for();
            let direct = table.dispatch_to_provider(
                generated.name,
                &direct_provider,
                &arguments,
                |_| Ok(()),
            );
            let mediated = registry.dispatch_to_provider(
                table,
                generated.name,
                &registry_provider,
                &arguments,
                |_| Ok(()),
            );
            let expected = if contract.declares_failure(code) {
                Ok(orna_sys_v1::SystemDispatchResult::Failed(code.clone()))
            } else {
                Err(ProviderDiagnostic::UndeclaredFailure {
                    operation: contract.id.clone(),
                    code: code.clone(),
                })
            };
            assert_eq!(direct, expected);
            assert_eq!(mediated, expected);
            assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 1);
            assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 1);
            if contract.declares_failure(code) {
                shared_failure_pairs += 1;
            } else {
                assert_eq!(direct.unwrap_err().code(), "sys.abi.undeclared_failure");
                assert_eq!(mediated.unwrap_err().code(), "sys.abi.undeclared_failure");
                undeclared_failure_pairs += 1;
            }
        }
    }

    assert_eq!(generated_bindings, typed_operation_ids.len());
    assert!(provider_routes > 0);
    assert_eq!(
        shared_failure_pairs,
        provider_routes * SHARED_FAILURES.len()
    );
    assert_eq!(undeclared_failure_pairs, provider_routes);
    println!(
        "generated_binding_registry_provider_failure_edges bindings={generated_bindings} provider_routes={provider_routes} shared_failure_pairs={shared_failure_pairs} undeclared_failure_pairs={undeclared_failure_pairs} codes=precondition_failed,unavailable,provider_failed,undeclared_failure direct_registry_equal=true providers_called=1 total_cases={}",
        generated_bindings + (shared_failure_pairs + undeclared_failure_pairs) * 2
    );
}

#[test]
fn generated_idl_unknown_operation_diagnostics_match_round_tripped_registry() {
    let baked = system_dispatch_table();
    let round_tripped = round_trip_generated_provider_artifacts();
    let registry = ProviderRoleRegistry::from_baked_abi(&round_tripped)
        .expect("round-tripped typed registry resolves generated provider offers");
    let idl_operation_ids = system_binding_stubs()
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    let typed_operation_ids = baked
        .operations()
        .map(|contract| contract.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(idl_operation_ids, typed_operation_ids);

    let fallback_role = baked
        .roles()
        .next()
        .expect("generated typed registry has a provider role");
    let fallback_offer = ProviderOffer {
        provider: ProviderId::new("fixture.unknown_operation").unwrap(),
        role: fallback_role.id.clone(),
        version: fallback_role.version,
        effects: fallback_role.effects.clone(),
    };
    let mut generated_bindings = 0;
    let mut unknown_selector_pairs = 0;

    for contract in baked.operations() {
        let operation_name = contract.id.as_str();
        let generated = system_function_descriptor(operation_name)
            .unwrap_or_else(|| panic!("missing generated binding for {operation_name}"));
        assert_eq!(generated.name, operation_name);
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(contract.effects.iter().next(), Some(generated.effect));
        generated_bindings += 1;

        let separator = operation_name.find(['(', '<']);
        let unregistered = match separator {
            Some(index) if operation_name[index..].starts_with('(') => {
                format!("{}(sys.conformance.RegistryMiss)", &operation_name[..index])
            }
            Some(index) => format!("{}<sys.conformance.RegistryMiss>", &operation_name[..index]),
            None => format!("{operation_name}.registry_miss"),
        };
        let unknown_id = OperationId::new(unregistered.clone())
            .expect("generated negative selector remains a valid operation ID");
        assert!(baked.operation(&unregistered).is_none());
        assert!(round_tripped.operation(&unregistered).is_none());

        let provider_for = || InvokeValueProvider {
            offer: fallback_offer.clone(),
            operation: unknown_id.clone(),
            argument_types: Vec::new(),
            response: Ok(TypedValue::public(
                TypeId::new("sys.conformance.UnexpectedDispatch"),
                b"must-not-run".to_vec(),
            )),
            calls: AtomicUsize::new(0),
        };
        let direct_provider = provider_for();
        let registry_provider = provider_for();
        let mut direct_preconditions = 0;
        let mut registry_preconditions = 0;
        let direct = baked.dispatch_to_provider(&unregistered, &direct_provider, &[], |_| {
            direct_preconditions += 1;
            Ok(())
        });
        let mediated = registry.dispatch_to_provider(
            &round_tripped,
            &unregistered,
            &registry_provider,
            &[],
            |_| {
                registry_preconditions += 1;
                Ok(())
            },
        );
        let expected = Err(ProviderDiagnostic::UnknownOperation(unknown_id));
        assert_eq!(direct, expected);
        assert_eq!(mediated, expected);
        assert_eq!(direct.unwrap_err().code(), "sys.abi.unknown_operation");
        assert_eq!(mediated.unwrap_err().code(), "sys.abi.unknown_operation");
        assert_eq!(direct_preconditions, 0);
        assert_eq!(registry_preconditions, 0);
        assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 0);
        unknown_selector_pairs += 1;
    }

    assert_eq!(generated_bindings, idl_operation_ids.len());
    assert_eq!(unknown_selector_pairs, generated_bindings);
    println!(
        "generated_idl_unknown_operation_diagnostic_parity bindings={generated_bindings} unknown_selectors={unknown_selector_pairs} direct_registry_pairs={unknown_selector_pairs} code=sys.abi.unknown_operation preconditions_checked=0 providers_called=0 total_cases={}",
        generated_bindings + unknown_selector_pairs * 2
    );
}

#[test]
fn round_tripped_dispatch_metadata_matches_selected_provider_routes() {
    fn effect_name(effect: SystemEffect) -> &'static str {
        match effect {
            SystemEffect::Read => "read",
            SystemEffect::Invoke => "invoke",
            SystemEffect::Admin => "admin",
        }
    }

    let table = round_trip_generated_provider_artifacts();
    let metadata: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded dispatch metadata");
    let registry = ProviderRoleRegistry::from_baked_abi(&table)
        .expect("round-tripped metadata resolves the selected provider registry");
    let operation_rows = metadata["operations"]
        .as_array()
        .expect("generated dispatch metadata lists operations");
    let role_rows = metadata["roles"]
        .as_array()
        .expect("generated dispatch metadata lists provider roles");
    let mut operation_cases = 0;
    let mut role_cases = 0;
    let mut provider_route_pairs = 0;

    for contract in table.operations() {
        let operation_name = contract.id.as_str();
        let generated = system_function_descriptor(operation_name)
            .unwrap_or_else(|| panic!("missing generated binding for {operation_name}"));
        let operation_row = operation_rows
            .iter()
            .find(|row| row["name"] == operation_name)
            .unwrap_or_else(|| panic!("generated metadata omits {operation_name}"));
        assert_eq!(generated.name, operation_name);
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(generated.effect, contract.effects.iter().next().unwrap());
        assert_eq!(
            operation_row["signature"].as_str(),
            Some(generated.signature)
        );
        assert_eq!(
            operation_row["version"]["major"].as_u64(),
            Some(contract.version.major.into())
        );
        assert_eq!(
            operation_row["version"]["minor"].as_u64(),
            Some(contract.version.minor.into())
        );
        assert_eq!(
            operation_row["effect"].as_str(),
            Some(effect_name(generated.effect))
        );
        let expected_role = match (&contract.role, contract.role_version) {
            (Some(role), Some(version)) => Some(format!(
                "{}@{}.{}",
                role.as_str(),
                version.major,
                version.minor
            )),
            (None, None) => None,
            _ => panic!("operation role name and version must be paired for {operation_name}"),
        };
        assert_eq!(operation_row["role"].as_str(), expected_role.as_deref());
        operation_cases += 1;

        let Some(role_id) = &contract.role else {
            continue;
        };
        let role = table
            .role(role_id.as_str())
            .unwrap_or_else(|| panic!("missing typed role {}", role_id.as_str()));
        let role_row = role_rows
            .iter()
            .find(|row| row["name"] == role.id.as_str())
            .unwrap_or_else(|| panic!("generated metadata omits role {}", role.id.as_str()));
        assert_eq!(
            role_row["version"]["major"].as_u64(),
            Some(role.version.major.into())
        );
        assert_eq!(
            role_row["version"]["minor"].as_u64(),
            Some(role.version.minor.into())
        );
        assert_eq!(
            role_row["builtin_provider"].as_str(),
            role.builtin_provider
                .as_ref()
                .map(|provider| provider.as_str())
        );
        assert!(
            role_row["operations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|operation| operation.as_str() == Some(operation_name)),
            "metadata role {} links back to {operation_name}",
            role.id.as_str()
        );
        let offer = registry
            .resolve(role_id.as_str())
            .unwrap_or_else(|_| panic!("missing selected offer for {}", role_id.as_str()))
            .clone();
        assert_eq!(offer.role, role.id);
        assert_eq!(offer.version, role.version);
        assert_eq!(offer.effects, role.effects);
        assert_eq!(offer.provider, role.builtin_provider.clone().unwrap());
        role_cases += 1;

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
        let result = TypedValue::public(
            TypeId::new(contract.signature.result.canonical()),
            b"round-tripped-provider-metadata".to_vec(),
        );
        let provider_for = || InvokeValueProvider {
            offer: offer.clone(),
            operation: contract.id.clone(),
            argument_types: argument_types.clone(),
            response: Ok(result.clone()),
            calls: AtomicUsize::new(0),
        };
        let direct_provider = provider_for();
        let direct = table
            .dispatch_to_provider(operation_name, &direct_provider, &arguments, |_| Ok(()))
            .expect("direct dispatch accepts the generated provider contract");
        let mediated_provider = provider_for();
        let mediated = registry
            .dispatch_to_provider(
                &table,
                generated.name,
                &mediated_provider,
                &arguments,
                |_| Ok(()),
            )
            .expect("round-tripped selected provider dispatch accepts the contract");
        assert_eq!(direct, orna_sys_v1::SystemDispatchResult::Returned(result));
        assert_eq!(mediated, direct);
        assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(mediated_provider.calls.load(Ordering::SeqCst), 1);
        provider_route_pairs += 1;
    }

    assert_eq!(operation_cases, table.operations().count());
    assert!(role_cases > 0);
    assert_eq!(provider_route_pairs, role_cases);
    println!(
        "round_tripped_dispatch_metadata_provider_parity operations={operation_cases} role_links={role_cases} provider_routes={provider_route_pairs} direct_registry_pairs={provider_route_pairs} total_cases={}",
        operation_cases + role_cases + provider_route_pairs * 2
    );
}

#[test]
fn generated_bindings_match_max_u16_provider_version_contracts() {
    let max_version = u16::MAX;
    let mut metadata: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded dispatch metadata");
    metadata["abi_version"] = serde_json::json!({"major": max_version, "minor": max_version});
    let operations = metadata["operations"]
        .as_array_mut()
        .expect("generated metadata operation inventory");
    for operation in operations.iter_mut() {
        operation["version"] = serde_json::json!({"major": max_version, "minor": max_version});
        if let Some(role_annotation) = operation["role"].as_str() {
            let role_name = role_annotation
                .rsplit_once('@')
                .expect("provider role annotation has a version")
                .0;
            operation["role"] = Value::String(format!("{role_name}@{max_version}.{max_version}"));
        }
    }
    for role in metadata["roles"]
        .as_array_mut()
        .expect("generated metadata role inventory")
    {
        role["version"] = serde_json::json!({"major": max_version, "minor": max_version});
    }

    let metadata_json = metadata.to_string();
    let schema_json = orna_sys_v1::system_provider_abi_schema_json();
    build_host::validate_json_against_schema(&metadata_json, schema_json)
        .expect("maximum typed provider versions remain valid generated schema values");
    let table = SystemProviderAbi::from_json(&metadata_json)
        .expect("maximum typed provider versions deserialize and retain their role links");
    assert_eq!(
        table.version(),
        AbiVersion {
            major: max_version,
            minor: max_version
        }
    );
    let registry = ProviderRoleRegistry::from_baked_abi(&table)
        .expect("maximum provider role versions resolve into selected offers");
    let stub_operation_ids = system_binding_stubs()
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    let typed_operation_ids = table
        .operations()
        .map(|operation| operation.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(stub_operation_ids, typed_operation_ids);

    let mut binding_cases = 0;
    let mut provider_route_pairs = 0;
    for contract in table.operations() {
        let operation_name = contract.id.as_str();
        let generated = system_function_descriptor(operation_name)
            .unwrap_or_else(|| panic!("missing generated binding for {operation_name}"));
        assert_eq!(generated.name, operation_name);
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(
            contract.effects.iter().next(),
            Some(generated.effect),
            "generated binding parity survives maximum ABI versions for {operation_name}"
        );
        assert_eq!(contract.version.major, max_version);
        assert_eq!(contract.version.minor, max_version);
        binding_cases += 1;

        let Some(role_id) = &contract.role else {
            continue;
        };
        assert_eq!(
            contract.role_version,
            Some(AbiVersion {
                major: max_version,
                minor: max_version
            })
        );
        let offer = registry
            .resolve(role_id.as_str())
            .expect("maximum-version role has a selected provider")
            .clone();
        assert_eq!(offer.version.major, max_version);
        assert_eq!(offer.version.minor, max_version);
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
        let result = TypedValue::public(
            TypeId::new(contract.signature.result.canonical()),
            b"max-u16-provider-result".to_vec(),
        );
        let provider_for = || InvokeValueProvider {
            offer: offer.clone(),
            operation: contract.id.clone(),
            argument_types: argument_types.clone(),
            response: Ok(result.clone()),
            calls: AtomicUsize::new(0),
        };
        let direct_provider = provider_for();
        let direct = table
            .dispatch_to_provider(operation_name, &direct_provider, &arguments, |_| Ok(()))
            .expect("direct dispatch accepts maximum-version provider contract");
        let registry_provider = provider_for();
        let mediated = registry
            .dispatch_to_provider(
                &table,
                generated.name,
                &registry_provider,
                &arguments,
                |_| Ok(()),
            )
            .expect("selected registry dispatch accepts maximum-version provider contract");
        assert_eq!(direct, orna_sys_v1::SystemDispatchResult::Returned(result));
        assert_eq!(mediated, direct);
        assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 1);
        provider_route_pairs += 1;
    }

    assert_eq!(binding_cases, typed_operation_ids.len());
    assert!(provider_route_pairs > 0);
    println!(
        "generated_binding_u16_boundary_parity bindings={binding_cases} provider_routes={provider_route_pairs} schema_acceptance=1 direct_registry_pairs={provider_route_pairs} max_version={max_version}.{max_version} total_cases={}",
        binding_cases + provider_route_pairs * 3 + 1
    );
}

#[test]
fn generated_bindings_preserve_provider_routes_after_registry_row_reordering() {
    let mut metadata: Value =
        serde_json::from_str(system_provider_abi_json()).expect("embedded dispatch metadata");
    metadata["operations"]
        .as_array_mut()
        .expect("generated operation inventory")
        .reverse();
    metadata["roles"]
        .as_array_mut()
        .expect("generated role inventory")
        .reverse();
    let metadata_json = metadata.to_string();
    let schema_json = orna_sys_v1::system_provider_abi_schema_json();
    build_host::validate_json_against_schema(&metadata_json, schema_json)
        .expect("schema accepts reordered but otherwise valid provider metadata");
    let table = SystemProviderAbi::from_json(&metadata_json)
        .expect("typed registry accepts reordered provider metadata");
    assert_eq!(&table, system_dispatch_table());
    let registry = ProviderRoleRegistry::from_baked_abi(&table)
        .expect("reordered registry metadata preserves selected provider offers");

    let stub_operation_ids = system_binding_stubs()
        .lines()
        .filter_map(|line| line.strip_prefix("// sys-op: "))
        .collect::<Vec<_>>();
    let typed_operation_ids = table
        .operations()
        .map(|operation| operation.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(stub_operation_ids, typed_operation_ids);

    let mut binding_cases = 0;
    let mut provider_route_pairs = 0;
    for contract in table.operations() {
        let operation_name = contract.id.as_str();
        let generated = system_function_descriptor(operation_name)
            .unwrap_or_else(|| panic!("missing generated binding for {operation_name}"));
        assert_eq!(generated.name, operation_name);
        assert_eq!(generated.signature, contract.signature.source);
        assert_eq!(
            contract.effects.iter().next(),
            Some(generated.effect),
            "reordering metadata preserves generated binding effect parity for {operation_name}"
        );
        binding_cases += 1;

        let Some(role_id) = &contract.role else {
            continue;
        };
        let offer = registry
            .resolve(role_id.as_str())
            .expect("reordered role metadata resolves its provider offer")
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
        let result = TypedValue::public(
            TypeId::new(contract.signature.result.canonical()),
            b"reordered-provider-result".to_vec(),
        );
        let provider_for = || InvokeValueProvider {
            offer: offer.clone(),
            operation: contract.id.clone(),
            argument_types: argument_types.clone(),
            response: Ok(result.clone()),
            calls: AtomicUsize::new(0),
        };
        let direct_provider = provider_for();
        let direct = table
            .dispatch_to_provider(operation_name, &direct_provider, &arguments, |_| Ok(()))
            .expect("direct dispatch accepts the reordered typed contract");
        let registry_provider = provider_for();
        let mediated = registry
            .dispatch_to_provider(
                &table,
                generated.name,
                &registry_provider,
                &arguments,
                |_| Ok(()),
            )
            .expect("selected provider dispatch accepts the reordered typed contract");
        assert_eq!(direct, orna_sys_v1::SystemDispatchResult::Returned(result));
        assert_eq!(mediated, direct);
        assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(registry_provider.calls.load(Ordering::SeqCst), 1);
        provider_route_pairs += 1;
    }

    assert_eq!(binding_cases, typed_operation_ids.len());
    assert!(provider_route_pairs > 0);
    println!(
        "generated_binding_provider_row_reorder_parity bindings={binding_cases} provider_routes={provider_route_pairs} schema_valid=1 direct_registry_pairs={provider_route_pairs} total_cases={}",
        binding_cases + provider_route_pairs * 3 + 1
    );
}
