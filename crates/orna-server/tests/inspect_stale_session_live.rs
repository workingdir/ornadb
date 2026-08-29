#![cfg(unix)]
#![allow(dead_code)]

#[path = "../../orna-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use orna_compiler::{
    STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    STD_INVOKE_ECHO_PARAMETER_ID, check, prepare,
};
use orna_core::{
    InvocationId, PrincipalId, StandardLibraryRevisionId,
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationParameterSelector, InvocationTarget as InvocationRequestTarget,
        InvocationTracePolicy, InvokeRequest, InvokeRequestInput, InvokeValue,
    },
    revision::{ActiveDatabaseRevision, RevisionPair},
    security::{
        ExecuteGrant, LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus,
        RoleMembership, SecurityFunctionTarget, SecuritySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    value::RuntimeValue,
};
use orna_postgres::{PostgresKernel, PostgresKernelError, SealedInvocationResult};
use orna_protocol::encode_invoke_request;
use orna_standard::registered_opaque_codecs;
use postgres_test_support::{TestDatabase, TestResult, failure, with_test_database};

const PROOF_USER: PrincipalId = PrincipalId::from_bytes([0x71; 16]);
const PROOF_ROLE: PrincipalId = PrincipalId::from_bytes([0x72; 16]);
const CONNECTION_PROTOCOL_MAJOR: u16 = 5;
const ECHO_VALUE: i32 = 41;
const STANDARD_APPLICATION_SOURCE: &str = "CREATE SCHEMA app;\n";

fn kernel(database: &TestDatabase) -> PostgresKernel {
    database.connection_string().parse().expect("kernel URL")
}

async fn install_v3_standard_chain(database: &TestDatabase) -> TestResult<ActiveDatabaseRevision> {
    let kernel = kernel(database);
    kernel.bootstrap().await?;
    let empty = kernel.recover().await?;
    let bundle = SourceBundle::new([SourceUnit::new("main.orna", STANDARD_APPLICATION_SOURCE)])?;
    let report = check(&bundle, empty.catalogue());
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "standard application source did not compile: {:?}",
            report.diagnostics()
        )));
    }
    let version_one = kernel
        .apply(&prepare(&report, empty.pair(), &empty)?)
        .await?;
    let version_two_upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&version_one)?;
    let version_two = kernel.apply_standard_upgrade(&version_two_upgrade).await?;
    let version_three_upgrade = orna_standard::prepare_standard_upgrade_v2_to_v3(&version_two)?;
    Ok(kernel
        .apply_standard_upgrade(&version_three_upgrade)
        .await?)
}

fn proof_security(
    pair: RevisionPair,
    standard_revision: StandardLibraryRevisionId,
    uid: u32,
    memberships: Vec<RoleMembership>,
) -> TestResult<SecuritySnapshot> {
    Ok(
        SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![
                Principal::new(PROOF_USER, PrincipalKind::User, PrincipalStatus::Active),
                Principal::new(PROOF_ROLE, PrincipalKind::Role, PrincipalStatus::Active),
            ],
            memberships,
            vec![ExecuteGrant::new(PROOF_USER, STD_INVOKE_ECHO_FUNCTION_ID)],
            vec![LocalPeerCredential::new(uid, PROOF_USER)],
            Vec::new(),
        )?,
    )
}

fn sealed_echo_request() -> TestResult<InvokeRequest> {
    Ok(InvokeRequest::new(InvokeRequestInput {
        target: InvocationRequestTarget::function_id(STD_INVOKE_ECHO_FUNCTION_ID),
        arguments: vec![InvocationArgument::new(
            InvocationParameterSelector::parameter_id(STD_INVOKE_ECHO_PARAMETER_ID),
            InvokeValue::new(RuntimeValue::Integer(ECHO_VALUE))?,
        )],
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

fn completed_invocation(result: &SealedInvocationResult) -> TestResult<InvocationId> {
    match result {
        SealedInvocationResult::Completed { invocation, .. } => Ok(*invocation),
        _ => Err(failure("the sealed echo invocation did not complete")),
    }
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn denies_inspection_for_stale_active_role_session() -> TestResult<()> {
    with_test_database(|database| async move {
        let active = install_v3_standard_chain(&database).await?;
        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("the v3 standard snapshot was not pinned"))?;
        let registry = registered_opaque_codecs(standard)?;
        let kernel = kernel(&database);
        let uid = nix::unistd::geteuid().as_raw();

        let security = proof_security(
            active.pair(),
            standard.revision(),
            uid,
            vec![RoleMembership::new(PROOF_ROLE, PROOF_USER)],
        )?;
        let security = kernel.replace_security_snapshot(&security).await?;
        let stale_session = security.bind_authenticated_session(PROOF_USER, vec![PROOF_ROLE])?;

        let request = sealed_echo_request()?;
        let retained = encode_invoke_request(&active, &registry, &request)?;
        let result = kernel
            .dispatch_sealed_sys_invoke(&stale_session, CONNECTION_PROTOCOL_MAJOR, &retained)
            .await?;
        let invocation = completed_invocation(&result)?;
        let captured_epoch = kernel
            .find_latest_inspect_epoch(&stale_session, invocation)
            .await?
            .ok_or_else(|| failure("the sealed echo did not auto-capture an Inspector epoch"))?;

        let revoked = proof_security(active.pair(), standard.revision(), uid, Vec::new())?;
        kernel.replace_security_snapshot(&revoked).await?;

        let latest = kernel
            .find_latest_inspect_epoch(&stale_session, invocation)
            .await;
        assert!(matches!(
            latest,
            Err(PostgresKernelError::InspectDenied {
                reason: orna_core::security::InspectDenial::MissingPrivilege,
            })
        ));

        let loaded = kernel
            .load_inspect_snapshot(&stale_session, captured_epoch)
            .await;
        assert!(matches!(
            loaded,
            Err(PostgresKernelError::InspectDenied {
                reason: orna_core::security::InspectDenial::MissingPrivilege,
            })
        ));
        Ok(())
    })
    .await
}
