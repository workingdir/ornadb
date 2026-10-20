use orna_sys_v1::{
    AbiVersion, EffectSet, ExplainQueryProvider, ExpressionRef, FailureCode, ObjectRef,
    OperationId, ProviderDiagnostic, ProviderFailure, ProviderId, ProviderOffer,
    ProviderRoleRegistry, QueryPlanDescription, QueryPlanResolver, SemanticRoleId, SnapshotRef,
    SystemDispatchResult, SystemEffect, SystemOperationProvider, TypeId, TypedValue,
    system_dispatch_table, system_provider_abi,
};

const OPERATION: &str = "sys.explain(Query)";
const GENERIC_PROVIDER_FAILURES: [&str; 3] = [
    "sys.abi.precondition_failed",
    "sys.abi.unavailable",
    "sys.abi.provider_failed",
];
const PROVIDER_FAILURE: &str = "sys.abi.provider_failed";

struct FixedResolver {
    response: Result<QueryPlanDescription, FailureCode>,
}

impl QueryPlanResolver for FixedResolver {
    fn resolve(&self, _query: &TypedValue) -> Result<QueryPlanDescription, FailureCode> {
        self.response.clone()
    }
}

/// Wraps the explain provider but advertises a different provider identity
/// for the same role, to exercise the registry's selected-offer link check.
struct ForeignProvider {
    offer: ProviderOffer,
    inner: ExplainQueryProvider<FixedResolver>,
}

impl SystemOperationProvider for ForeignProvider {
    fn offer(&self) -> &ProviderOffer {
        &self.offer
    }

    fn invoke(
        &self,
        operation: &OperationId,
        arguments: &[TypedValue],
    ) -> Result<TypedValue, ProviderFailure> {
        self.inner.invoke(operation, arguments)
    }
}

fn scan_query() -> QueryPlanDescription {
    QueryPlanDescription {
        snapshot: SnapshotRef::descriptive("snapshot:explain-provider"),
        source: ObjectRef::descriptive("table:Orders"),
        source_statistics: None,
        joins: Vec::new(),
        predicate: Some(ExpressionRef::descriptive("expr:open-order")),
        projections: vec![ExpressionRef::descriptive("expr:order-total")],
        distinct: false,
        ordering: Vec::new(),
        limit: None,
        mutations: Vec::new(),
        materialize_into: None,
    }
}

fn query_argument() -> TypedValue {
    TypedValue::public(TypeId::new("Query<Int>"), Vec::<u8>::new())
}

fn explain_provider(
    response: Result<QueryPlanDescription, FailureCode>,
) -> ExplainQueryProvider<FixedResolver> {
    ExplainQueryProvider::new(FixedResolver { response }).expect("offer identifiers are valid")
}

fn linked_registry(provider: &ExplainQueryProvider<FixedResolver>) -> ProviderRoleRegistry {
    let mut registry =
        ProviderRoleRegistry::from_baked_abi(system_provider_abi()).expect("baked ABI links");
    registry
        .bind(provider.offer().clone())
        .expect("explain provider offer matches the generated role");
    registry
}

fn invoke_refusal(
    provider: &ExplainQueryProvider<FixedResolver>,
    operation: &str,
    arguments: &[TypedValue],
) -> String {
    let operation = OperationId::new(operation).expect("operation identifier is valid");
    match provider.invoke(&operation, arguments) {
        Err(failure) => failure.code.as_str().to_owned(),
        Ok(value) => panic!("expected the provider to refuse the call, got {value:?}"),
    }
}

#[test]
fn generated_abi_declares_explain_query_on_the_explain_role() {
    let abi = system_provider_abi();
    let operation = abi
        .operation(OPERATION)
        .expect("sys.explain(Query) is generated");
    assert_eq!(
        operation.role.as_ref().map(SemanticRoleId::as_str),
        Some("langitem.sys.explain")
    );
    assert_eq!(operation.role_version, Some(AbiVersion::V1_0));
    assert_eq!(operation.effects, EffectSet::one(SystemEffect::Read));
    for code in &operation.failures {
        assert!(
            GENERIC_PROVIDER_FAILURES.contains(&code.as_str()),
            "sys.explain(Query) declares only generic failures, found {code:?}"
        );
    }

    let role = abi
        .role("langitem.sys.explain")
        .expect("explain role is generated");
    assert!(role.required);
    assert!(!role.replaceable);
    assert_eq!(
        role.builtin_provider.as_ref().map(ProviderId::as_str),
        Some("orna.sys.v1")
    );
    assert_eq!(
        role.operations,
        vec![
            OperationId::new("sys.explain(Diagnostic)").unwrap(),
            OperationId::new("sys.explain(FunctionRef)").unwrap(),
            OperationId::new(OPERATION).unwrap(),
        ]
    );
}

#[test]
fn explain_provider_offer_matches_the_generated_role_contract() {
    let provider = explain_provider(Ok(scan_query()));
    let offer: &ProviderOffer = provider.offer();
    let role = system_provider_abi()
        .role("langitem.sys.explain")
        .expect("explain role is generated");
    assert_eq!(offer.provider, *role.builtin_provider.as_ref().unwrap());
    assert_eq!(offer.role, role.id);
    assert_eq!(offer.version, role.version);
    assert_eq!(offer.effects, role.effects);
}

#[test]
fn dispatch_through_registry_returns_flat_structured_sys_plan() {
    let provider = explain_provider(Ok(scan_query()));
    let registry = linked_registry(&provider);
    let result = registry
        .dispatch_to_provider(
            system_dispatch_table(),
            OPERATION,
            &provider,
            &[query_argument()],
            |_| Ok(()),
        )
        .expect("registry links the bound explain provider");
    let SystemDispatchResult::Returned(value) = result else {
        panic!("explain returned a failure: {result:?}");
    };
    assert_eq!(value.static_type().as_str(), "sys.Plan");
    let bytes = value.canonical().expect("sys.Plan is public");
    let plan: serde_json::Value = serde_json::from_slice(bytes).expect("sys.Plan is JSON");
    assert_eq!(plan["actual_available"], serde_json::json!(false));
    assert!(
        plan["id"].is_string(),
        "plan id is serialized: {plan}"
    );
    for field in [
        "reference",
        "snapshot",
        "root",
        "warnings",
    ] {
        assert!(plan.get(field).is_some(), "sys.Plan carries {field}: {plan}");
    }
    assert!(
        plan.get("plan").is_none() && plan.get("nodes").is_none(),
        "sys.Plan is flat, not the internal explained-plan wrapper: {plan}"
    );
}

#[test]
fn foreign_provider_for_the_explain_role_is_refused() {
    let builtin = explain_provider(Ok(scan_query()));
    let registry = linked_registry(&builtin);
    let foreign = ForeignProvider {
        offer: ProviderOffer {
            provider: ProviderId::new("example.explain").unwrap(),
            role: SemanticRoleId::new("langitem.sys.explain").unwrap(),
            version: AbiVersion::V1_0,
            effects: EffectSet::one(SystemEffect::Read),
        },
        inner: explain_provider(Ok(scan_query())),
    };
    let error = registry
        .dispatch_to_provider(
            system_dispatch_table(),
            OPERATION,
            &foreign,
            &[query_argument()],
            |_| Ok(()),
        )
        .expect_err("a provider not selected in the registry must not run");
    assert!(
        matches!(error, ProviderDiagnostic::ProviderNotSelected { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

#[test]
fn explain_offer_with_wider_effect_is_refused_at_bind() {
    let provider = explain_provider(Ok(scan_query()));
    let mut registry =
        ProviderRoleRegistry::from_baked_abi(system_provider_abi()).expect("baked ABI links");
    let mut widened = provider.offer().clone();
    widened.effects = EffectSet::new([SystemEffect::Read, SystemEffect::Invoke]);
    assert!(
        registry.bind(widened).is_err(),
        "an offer exceeding the role's read effect must not bind"
    );
}

#[test]
fn resolver_failure_outside_the_abi_is_refused_as_undeclared() {
    let provider = explain_provider(Err(
        FailureCode::new("sys.snapshot.not_found").unwrap()
    ));
    let registry = linked_registry(&provider);
    let error = registry
        .dispatch_to_provider(
            system_dispatch_table(),
            OPERATION,
            &provider,
            &[query_argument()],
            |_| Ok(()),
        )
        .expect_err("sys.explain(Query) does not declare sys.snapshot.not_found");
    assert!(
        matches!(error, ProviderDiagnostic::UndeclaredFailure { .. }),
        "unexpected diagnostic: {error:?}"
    );
}

#[test]
fn resolver_failure_with_generic_provider_code_is_reported_as_failure() {
    let provider = explain_provider(Err(FailureCode::new("sys.abi.provider_failed").unwrap()));
    let registry = linked_registry(&provider);
    let result = registry
        .dispatch_to_provider(
            system_dispatch_table(),
            OPERATION,
            &provider,
            &[query_argument()],
            |_| Ok(()),
        )
        .expect("a declared generic failure is a dispatch failure, not a diagnostic");
    match result {
        SystemDispatchResult::Failed(code) => {
            assert_eq!(code.as_str(), "sys.abi.provider_failed");
        }
        SystemDispatchResult::Returned(value) => {
            panic!("resolver failure must not yield a plan: {value:?}")
        }
    }
}

#[test]
fn empty_snapshot_fails_planning_with_provider_failure() {
    let mut query = scan_query();
    query.snapshot = SnapshotRef::descriptive("");
    let provider = explain_provider(Ok(query));
    let registry = linked_registry(&provider);
    let result = registry
        .dispatch_to_provider(
            system_dispatch_table(),
            OPERATION,
            &provider,
            &[query_argument()],
            |_| Ok(()),
        )
        .expect("a declared explain failure is a dispatch failure, not a diagnostic");
    match result {
        SystemDispatchResult::Failed(code) => {
            assert_eq!(code.as_str(), PROVIDER_FAILURE);
        }
        SystemDispatchResult::Returned(value) => {
            panic!("an empty snapshot must not yield a plan: {value:?}")
        }
    }
}

#[test]
fn empty_source_fails_planning_with_provider_failure() {
    let mut query = scan_query();
    query.source = ObjectRef::descriptive("");
    let provider = explain_provider(Ok(query));
    let registry = linked_registry(&provider);
    let result = registry
        .dispatch_to_provider(
            system_dispatch_table(),
            OPERATION,
            &provider,
            &[query_argument()],
            |_| Ok(()),
        )
        .expect("a declared explain failure is a dispatch failure, not a diagnostic");
    match result {
        SystemDispatchResult::Failed(code) => {
            assert_eq!(code.as_str(), PROVIDER_FAILURE);
        }
        SystemDispatchResult::Returned(value) => {
            panic!("an empty source must not yield a plan: {value:?}")
        }
    }
}

#[test]
fn call_without_query_argument_is_not_planned() {
    let provider = explain_provider(Ok(scan_query()));
    let registry = linked_registry(&provider);
    let outcome =
        registry.dispatch_to_provider(system_dispatch_table(), OPERATION, &provider, &[], |_| {
            Ok(())
        });
    let refused = match outcome {
        Err(_) => true,
        Ok(SystemDispatchResult::Failed(_)) => true,
        Ok(SystemDispatchResult::Returned(_)) => false,
    };
    assert!(
        refused,
        "a call without the query argument must not return a plan"
    );
}

#[test]
fn direct_invoke_refuses_a_different_operation() {
    let provider = explain_provider(Ok(scan_query()));
    assert_eq!(
        invoke_refusal(&provider, "sys.explain(FunctionRef)", &[query_argument()]),
        PROVIDER_FAILURE
    );
}

#[test]
fn direct_invoke_refuses_zero_arguments() {
    let provider = explain_provider(Ok(scan_query()));
    assert_eq!(
        invoke_refusal(&provider, OPERATION, &[]),
        PROVIDER_FAILURE
    );
}

#[test]
fn direct_invoke_refuses_two_arguments() {
    let provider = explain_provider(Ok(scan_query()));
    assert_eq!(
        invoke_refusal(&provider, OPERATION, &[query_argument(), query_argument()]),
        PROVIDER_FAILURE
    );
}

#[test]
fn direct_invoke_refuses_a_non_query_argument() {
    let provider = explain_provider(Ok(scan_query()));
    let not_a_query = TypedValue::public(TypeId::new("Int"), Vec::<u8>::new());
    assert_eq!(
        invoke_refusal(&provider, OPERATION, &[not_a_query]),
        PROVIDER_FAILURE
    );
}
