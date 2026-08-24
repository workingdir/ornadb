//! Installed ordinary CLIENT Inspector recursion proof (ADR 0080).
//!
//! This proof uses the Compose PostgreSQL host flow and the real installed
//! resource executor. It creates one ordinary target invocation, binds the
//! Inspector's observer root to that invocation, and rejects both self and
//! descendant inspection before a target/UI result can be produced.

#![cfg(unix)]
#![allow(dead_code)]

#[path = "../../orna-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use orna_client::{
    ClientExecutionError, ClientExternalContractRequest, ClientInspectError, ClientInspectRequest,
    ClientResourceCompletion, ClientResourceExecutor, ClientResourceRequest, ClientStateStore,
    capability::LocalCapabilityGrantSet,
    evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation,
};
use orna_compiler::{
    STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    STD_INVOKE_ECHO_PARAMETER_ID, StandardApplicationCheckContext, check,
    check_standard_application, prepare, prepare_standard_application,
};
use orna_core::{
    FunctionId, InvocationId, ObjectId, PrincipalId,
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationParameterSelector, InvocationTarget as InvocationRequestTarget,
        InvocationTracePolicy, InvokeRequest, InvokeRequestInput, InvokeValue,
    },
    revision::ActiveDatabaseRevision,
    security::{
        AuthorisedInvocation, ExecuteDecision, ExecuteGrant, InvocationTarget, LocalPeerCredential,
        Principal, PrincipalKind, PrincipalStatus, SecurityFunctionTarget, SecuritySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    system::SYS_INSPECT_INVOCATION_TYPE_ID,
    value::{FunctionArgument, RuntimeValue},
};
use orna_postgres::{PostgresKernel, SealedInvocationResult};
use orna_protocol::encode_invoke_request;
use orna_server::InstalledClientResourceExecutor;
use postgres_test_support::{TestDatabase, TestResult, failure, with_test_database};

const CONNECTION_PROTOCOL_MAJOR: u16 = 5;
const PROOF_USER: PrincipalId = PrincipalId::from_bytes([0x91; 16]);

const RAW_SCHEMA_SOURCE: &str = "CREATE SCHEMA recursion_fixture;\n";
const RAW_RECURSION_SOURCE: &str = include_str!("fixtures/client_inspector_dogfood.orna");
const RAW_RESOURCE_SOURCE: &str = "CREATE SCHEMA recursion_fixture;\n\
    CREATE CLIENT FUNCTION recursion_fixture.call() RETURNS INTEGER IS\n\
    BEGIN\n\
        RETURN AWAIT std.data.resource(\n\
          target => std.invoke.echo,\n\
          arguments => std.call.args(p_value => 43)\n\
        );\n\
    END;\n";

fn require(condition: bool, message: impl Into<String>) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}

fn kernel(database: &TestDatabase) -> PostgresKernel {
    database.connection_string().parse().expect("kernel URL")
}

async fn install_fixture(database: &TestDatabase) -> TestResult<ActiveDatabaseRevision> {
    let kernel = kernel(database);
    kernel.bootstrap().await?;
    let empty = kernel.recover().await?;
    let schema = SourceBundle::new([SourceUnit::new("schema.orna", RAW_SCHEMA_SOURCE)])?;
    let report = check(&schema, empty.catalogue());
    require(
        report.diagnostics().is_empty(),
        format!(
            "recursion proof schema did not compile: {:?}",
            report.diagnostics()
        ),
    )?;
    let version_one = kernel
        .apply(&prepare(&report, empty.pair(), &empty)?)
        .await?;
    let upgrade_two = orna_standard::prepare_standard_upgrade_v1_to_v2(&version_one)?;
    let version_two = kernel.apply_standard_upgrade(&upgrade_two).await?;
    let upgrade_three = orna_standard::prepare_standard_upgrade_v2_to_v3(&version_two)?;
    let version_three = kernel.apply_standard_upgrade(&upgrade_three).await?;
    let upgrade_four = orna_standard::prepare_standard_upgrade_v3_to_v4(&version_three)?;
    let version_four = kernel.apply_standard_upgrade(&upgrade_four).await?;

    let context = StandardApplicationCheckContext::try_new(
        version_four.catalogue(),
        upgrade_four.checked_standard_library(),
    )?;
    let source = SourceBundle::new([SourceUnit::new(
        "inspect-recursion.orna",
        format!("{RAW_RESOURCE_SOURCE}\n{RAW_RECURSION_SOURCE}"),
    )])?;
    let report = check_standard_application(&source, &context);
    require(
        report.diagnostics().is_empty(),
        format!(
            "recursion proof functions did not compile: {:?}",
            report.diagnostics()
        ),
    )?;
    Ok(kernel
        .apply(&prepare_standard_application(
            &report,
            version_four.pair(),
            &version_four,
        )?)
        .await?)
}

fn sealed_request(
    target: FunctionId,
    arguments: Vec<InvocationArgument>,
) -> TestResult<InvokeRequest> {
    Ok(InvokeRequest::new(InvokeRequestInput {
        target: InvocationRequestTarget::function_id(target),
        arguments,
        caller_context: InvocationCallerContext::new(
            InvocationCallerKind::TestRunner,
            false,
            false,
            None,
            None,
            "en-GB",
            "UTC",
            None,
        )?,
        client_offer: InvocationClientOffer::new(
            CONNECTION_PROTOCOL_MAJOR,
            "en-GB",
            "UTC",
            Vec::new(),
            Vec::new(),
            1_024,
            0,
            None,
            None,
        )?,
        output_requirement: None,
        state_profile: None,
        trace_policy: InvocationTracePolicy::Off,
        idempotency_key: None,
        parent_invocation_id: None,
        observer_context: None,
    })?)
}

fn echo_request() -> TestResult<InvokeRequest> {
    sealed_request(
        STD_INVOKE_ECHO_FUNCTION_ID,
        vec![InvocationArgument::new(
            InvocationParameterSelector::parameter_id(STD_INVOKE_ECHO_PARAMETER_ID),
            InvokeValue::new(RuntimeValue::Integer(41))?,
        )],
    )
}

fn no_argument_request(target: FunctionId) -> TestResult<InvokeRequest> {
    sealed_request(target, Vec::new())
}

fn completed_invocation(result: &SealedInvocationResult) -> TestResult<InvocationId> {
    match result {
        SealedInvocationResult::Completed { invocation, .. } => Ok(*invocation),
        _ => Err(failure("the installed target invocation did not complete")),
    }
}

async fn nested_resource_invocation(
    database: &TestDatabase,
    parent: InvocationId,
) -> TestResult<InvocationId> {
    let session = database.open().await?;
    let row = session
        .client()
        .query_opt(
            "SELECT nested_invocation_id\n             FROM _orna_kernel.resource_audit_events\n             WHERE parent_invocation_id = $1\n             ORDER BY sequence DESC\n             LIMIT 1",
            &[&parent.to_bytes().to_vec()],
        )
        .await?;
    let nested = row
        .ok_or_else(|| failure("the resource-backed target did not record a nested invocation"))?
        .try_get::<_, Vec<u8>>("nested_invocation_id")?;
    session.shutdown().await?;
    require(
        nested.len() == 16,
        "the nested resource invocation identity was not 16 bytes",
    )?;
    Ok(InvocationId::from_bytes(nested.try_into().map_err(
        |_| failure("nested invocation identity was truncated"),
    )?))
}

async fn invocation_audit_count(database: &TestDatabase) -> TestResult<i64> {
    let session = database.open().await?;
    let count = session
        .client()
        .query_one(
            "SELECT COUNT(*) FROM _orna_kernel.invocation_audit_events",
            &[],
        )
        .await?
        .get::<_, i64>(0);
    session.shutdown().await?;
    Ok(count)
}

struct RecordingExecutor {
    inner: InstalledClientResourceExecutor,
    execute_count: usize,
    inspect_count: usize,
    external_contract_count: usize,
}

impl ClientResourceExecutor for RecordingExecutor {
    fn execute(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.execute_count += 1;
        self.inner.execute(request)
    }

    fn poll(&mut self) -> Option<ClientResourceCompletion> {
        self.inner.poll()
    }

    fn cancel(&mut self, request: ClientResourceRequest) -> ClientResourceCompletion {
        self.inner.cancel(request)
    }
    fn abandon(&mut self, request: ClientResourceRequest) -> Result<(), String> {
        self.inner.abandon(request)
    }

    fn cancel_pending(&mut self) -> Option<ClientResourceCompletion> {
        self.inner.cancel_pending()
    }

    fn inspect(&mut self, request: ClientInspectRequest) -> Result<RuntimeValue, String> {
        self.inspect_count += 1;
        self.inner.inspect(request)
    }

    fn external_contract(
        &mut self,
        request: ClientExternalContractRequest,
    ) -> Result<RuntimeValue, String> {
        self.external_contract_count += 1;
        self.inner.external_contract(request)
    }
}

fn recursion_error(
    result: Result<orna_client::ClientExecutionResult, ClientExecutionError>,
) -> TestResult<()> {
    require(
        matches!(
            &result,
            Err(ClientExecutionError::Inspect {
                source: ClientInspectError::Failed(code),
                ..
            }) if code == "inspect.recursion"
        ),
        format!("Inspector recursion returned the wrong typed error: {result:?}"),
    )
}

async fn evaluate_inspector(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    parameter: orna_core::ParameterId,
    observer: InvocationId,
    target: InvocationId,
    executor: &mut RecordingExecutor,
) -> TestResult<Result<orna_client::ClientExecutionResult, ClientExecutionError>> {
    let argument = FunctionArgument::new(
        parameter,
        RuntimeValue::Reference {
            target: SYS_INSPECT_INVOCATION_TYPE_ID,
            object: ObjectId::from_bytes(target.to_bytes()),
        },
    )?;
    let mut state = ClientStateStore::new();
    let grants = LocalCapabilityGrantSet::new();
    Ok(evaluate_client_function_with_state_and_grants_and_arguments_and_executor_with_parent_invocation(
        active,
        authorisation,
        std::slice::from_ref(&argument),
        &[],
        &grants,
        &mut state,
        observer,
        executor,
    ))
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_installed_inspector_self_and_descendant_recursion_without_execution_or_ui()
-> TestResult<()> {
    with_test_database(|database| async move {
        let active = install_fixture(&database).await?;
        let kernel = kernel(&database);
        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("the V4 standard snapshot was not pinned"))?;
        let registry = orna_standard::registered_opaque_codecs(standard)?;
        let uid = nix::unistd::geteuid().as_raw();
        let inspector = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["inspector_app", "inspector"])
            .ok_or_else(|| failure("the ordinary Inspector function is missing"))?;
        let inspector_parameter = inspector
            .parameter_by_name("p_target")
            .ok_or_else(|| failure("the ordinary Inspector target parameter is missing"))?
            .id();
        let recursion_target = active
            .catalogue()
            .functions()
            .iter()
            .find(|function| function.name().parts() == ["recursion_fixture", "call"])
            .ok_or_else(|| failure("the resource-backed recursion target is missing"))?;
        let targets = active
            .catalogue()
            .functions()
            .iter()
            .map(|function| SecurityFunctionTarget::application(function.id()))
            .chain(std::iter::once(SecurityFunctionTarget::verified_standard(
                STD_INVOKE_ECHO_FUNCTION_ID,
                standard.revision(),
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )))
            .collect();
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            active.pair(),
            targets,
            vec![Principal::new(
                PROOF_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![
                ExecuteGrant::new(PROOF_USER, inspector.id()),
                ExecuteGrant::new(PROOF_USER, recursion_target.id()),
                ExecuteGrant::new(PROOF_USER, STD_INVOKE_ECHO_FUNCTION_ID),
            ],
            vec![LocalPeerCredential::new(uid, PROOF_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let session = security.bind_authenticated_session(PROOF_USER, vec![])?;
        let inspector_authorisation = match security.authorise_execute(
            &session,
            InvocationTarget::new(inspector.id(), active.pair()),
        ) {
            ExecuteDecision::Allowed(authorisation) => authorisation,
            ExecuteDecision::Denied(denial) => {
                return Err(failure(format!(
                    "ordinary Inspector grant was denied: {denial:?}"
                )));
            }
        };

        // Create the real target invocation through installed sys.invoke.
        let request = echo_request()?;
        let retained = encode_invoke_request(&active, &registry, &request)?;
        let target = completed_invocation(
            &kernel
                .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
                .await?,
        )?;
        let before_self = invocation_audit_count(&database).await?;
        let mut self_executor = RecordingExecutor {
            inner: InstalledClientResourceExecutor::new(
                kernel.clone(),
                session.clone(),
                active.clone(),
            ),
            execute_count: 0,
            inspect_count: 0,
            external_contract_count: 0,
        };
        let self_result = evaluate_inspector(
            &active,
            &inspector_authorisation,
            inspector_parameter,
            target,
            target,
            &mut self_executor,
        )
        .await?;
        recursion_error(self_result)?;
        require(
            self_executor.execute_count == 0
                && self_executor.inspect_count == 0
                && self_executor.external_contract_count == 0,
            "self recursion must not execute a target, call the Inspector provider, or emit UI",
        )?;
        require(
            invocation_audit_count(&database).await? == before_self,
            "self recursion must not append another target invocation",
        )?;

        // Create a real descendant through the installed CLIENT resource path.
        let mut target_executor = RecordingExecutor {
            inner: InstalledClientResourceExecutor::new(
                kernel.clone(),
                session.clone(),
                active.clone(),
            ),
            execute_count: 0,
            inspect_count: 0,
            external_contract_count: 0,
        };
        let resource_request = no_argument_request(recursion_target.id())?;
        let retained = encode_invoke_request(&active, &registry, &resource_request)?;
        let observer = completed_invocation(
            &kernel
                .dispatch_sealed_sys_invoke_with_resource_executor(
                    &session,
                    CONNECTION_PROTOCOL_MAJOR,
                    &retained,
                    Some(&mut target_executor),
                )
                .await?,
        )?;
        require(
            target_executor.execute_count == 1,
            "the resource-backed target must execute exactly once to create its descendant",
        )?;
        let descendant = nested_resource_invocation(&database, observer).await?;
        let before_descendant = invocation_audit_count(&database).await?;
        let execute_before = target_executor.execute_count;
        let inspect_before = target_executor.inspect_count;
        let external_before = target_executor.external_contract_count;
        let descendant_result = evaluate_inspector(
            &active,
            &inspector_authorisation,
            inspector_parameter,
            observer,
            descendant,
            &mut target_executor,
        )
        .await?;
        recursion_error(descendant_result)?;
        require(
            target_executor.execute_count == execute_before
                && target_executor.inspect_count == inspect_before + 1
                && target_executor.external_contract_count == external_before,
            "descendant recursion must inspect only, without executing the target or rendering UI",
        )?;
        require(
            invocation_audit_count(&database).await? == before_descendant,
            "descendant recursion must not append another target invocation",
        )?;
        Ok(())
    })
    .await
}
