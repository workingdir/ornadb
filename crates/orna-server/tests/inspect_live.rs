//! Live Inspector proof (ADR 0064 wave 3).
//!
//! This suite drives the exact installed host flow through the
//! `run_inspect_with_kernel` seam against the Compose PostgreSQL development
//! service. The seam authenticates the invoking process's effective UID
//! through `authenticate_local_peer(geteuid())` exactly as the installed
//! product does, so the security snapshot maps that UID to each principal
//! under test via `LocalPeerCredential`.
//!
//! Capture runs through the sealed dispatch: one protected `sys.invoke`
//! echo invocation appends the linked EXECUTE + invocation audit evidence
//! and auto-captures one immutable epoch and its trace rows (the ADR 0064
//! capture seam, `capture_sealed_invocation_snapshot`). This is the only
//! coherent capture path for the full proof: the `security_decisions`
//! projection joins `invocation_audit_events` to its linked security
//! evidence, and `capture_inspect_snapshot` alone never creates that
//! invocation-audit link.
//!
//! What is proved: `orna inspect` resolves the invocation to its captured
//! epoch, renders the root invocation node, streams the trace rows in
//! contiguous sequence order with `p_after_sequence` resume, renders
//! `state_cells` redacted (`value_hex: null`) unless the `Values` classifier
//! is armed, returns the linked EXECUTE security decision, and resolves the
//! same epoch by its exact override identity.

#![cfg(unix)]
#![allow(dead_code)]

mod support;

#[path = "../../orna-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use orna_compiler::{
    STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID, STD_INVOKE_ECHO_PARAMETER_ID,
};
use orna_core::{
    FunctionId, InvocationId, PrincipalId, StateSlotId,
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationEventBody, InvocationEventKind, InvocationParameterSelector,
        InvocationTarget as InvocationRequestTarget, InvocationTracePolicy, InvokeRequest,
        InvokeRequestInput, InvokeValue,
    },
    security::{
        ExecuteGrant, LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus,
        SecurityFunctionTarget, SecuritySnapshot,
    },
    value::RuntimeValue,
};
use orna_postgres::{PostgresKernel, SealedInvocationResult};
use orna_protocol::{encode_constructed_value, encode_invoke_request};
use orna_server::{
    InstalledInspectError, InstalledInspectOutcome, InstalledInspectProjection,
    InstalledInspectRequest, InstalledUserStateChange, InstalledUserStateError,
    InstalledUserStateOperation, InstalledUserStateOutcome, InstalledUserStateRequest,
    run_inspect_with_kernel, run_user_state_with_kernel,
};
use orna_standard::{INTEGER_TYPE_ID, registered_opaque_codecs};
use postgres_test_support::{TestDatabase, TestResult, failure, with_test_database};

const ECHO_VALUE: i32 = 41;
const CONNECTION_PROTOCOL_MAJOR: u16 = 5;
const INSPECT_PRINCIPAL: PrincipalId = PrincipalId::from_bytes([0x5a; 16]);
const SLOT_FUNCTION: FunctionId = FunctionId::from_bytes([0x5b; 16]);
const SLOT: StateSlotId = StateSlotId::from_bytes([0x5c; 16]);

/// Asserts one live condition, failing the whole test with a typed error.
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

/// Encodes one integer to the exact canonical ORV5 hex the renderer emits.
async fn integer_hex(database: &TestDatabase, value: i32) -> TestResult<String> {
    let active = kernel(database).recover().await?;
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| failure("the standard snapshot is pinned by the V1-to-V2 upgrade"))?;
    let registry = registered_opaque_codecs(standard)?;
    let encoded = encode_constructed_value(&active, &registry, &RuntimeValue::Integer(value))?;
    let mut hex = String::with_capacity(encoded.len() * 2);
    for byte in encoded {
        use std::fmt::Write;
        write!(&mut hex, "{byte:02x}").map_err(|_| failure("hex formatting failed"))?;
    }
    Ok(hex)
}

async fn install_standard(database: &TestDatabase) -> TestResult<()> {
    let kernel = kernel(database);
    kernel.bootstrap().await?;
    let empty = kernel.recover().await?;
    let upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&empty)?;
    kernel.apply_standard_upgrade(&upgrade).await?;
    Ok(())
}

/// Writes one integer USER state cell under the echo root function, so the
/// `state_cells` projection of the captured epoch has a live row to read.
async fn write_cell(
    database: &TestDatabase,
    value: i32,
) -> TestResult<Result<InstalledUserStateOutcome, InstalledUserStateError>> {
    let active = kernel(database).recover().await?;
    let standard = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| failure("the standard snapshot is pinned by the V1-to-V2 upgrade"))?;
    let registry = registered_opaque_codecs(standard)?;
    let value_bytes = encode_constructed_value(&active, &registry, &RuntimeValue::Integer(value))?;
    let request = InstalledUserStateRequest::new(InstalledUserStateOperation::Write {
        root_function: STD_INVOKE_ECHO_FUNCTION_ID,
        state_profile: String::new(),
        change: InstalledUserStateChange {
            function: SLOT_FUNCTION,
            instance_key: String::new(),
            state_slot: SLOT,
            expected_revision: None,
            value_type: INTEGER_TYPE_ID,
            value_bytes,
        },
    });
    let mut stdout = Vec::new();
    let outcome = run_user_state_with_kernel(kernel(database).clone(), request, &mut stdout).await;
    Ok(outcome)
}

/// Runs one installed inspect command through the live-proof seam and
/// returns its outcome and rendered stdout.
async fn inspect_run(
    database: &TestDatabase,
    request: InstalledInspectRequest,
) -> TestResult<(
    Result<InstalledInspectOutcome, InstalledInspectError>,
    Vec<u8>,
)> {
    let mut stdout = Vec::new();
    let outcome = run_inspect_with_kernel(kernel(database).clone(), request, &mut stdout).await;
    Ok((outcome, stdout))
}

/// Parses every rendered record line as one JSON object.
fn record_lines(text: &str) -> Result<Vec<serde_json::Value>, String> {
    text.lines()
        .map(|line| {
            serde_json::from_str(line).map_err(|error| format!("record is not JSON: {error}"))
        })
        .collect()
}

/// Builds one complete checked `sys.invoke` Request for `std.invoke.echo`.
fn sealed_echo_request(value: i32) -> TestResult<InvokeRequest> {
    Ok(InvokeRequest::new(InvokeRequestInput {
        target: InvocationRequestTarget::function_id(STD_INVOKE_ECHO_FUNCTION_ID),
        arguments: vec![InvocationArgument::new(
            InvocationParameterSelector::parameter_id(STD_INVOKE_ECHO_PARAMETER_ID),
            InvokeValue::new(RuntimeValue::Integer(value))?,
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
            5,
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

/// Asserts one completed sealed echo invocation carried exactly
/// `InvocationStarted(0)`, `ValueBatch(1)` with the typed integer, and
/// `InvocationCompleted(2)`, and returns its invocation identity.
fn require_echo_completion(
    result: &SealedInvocationResult,
    expected: i32,
) -> TestResult<InvocationId> {
    let SealedInvocationResult::Completed { invocation, events } = result else {
        return Err(failure(
            "the sealed echo invocation did not complete with its Event batch",
        ));
    };
    let records = events.records();
    require(
        records.len() == 3
            && records[0].event().sequence() == 0
            && records[1].event().sequence() == 1
            && records[2].event().sequence() == 2
            && records[0].event().kind() == InvocationEventKind::InvocationStarted
            && records[1].event().kind() == InvocationEventKind::ValueBatch
            && records[2].event().kind() == InvocationEventKind::InvocationCompleted,
        "the sealed echo stream did not carry InvocationStarted(0), ValueBatch(1), InvocationCompleted(2)",
    )?;
    let InvocationEventBody::ValueBatch {
        schema: None,
        values,
    } = records[1].event().body()
    else {
        return Err(failure(
            "the sealed ValueBatch event did not carry a plain typed batch",
        ));
    };
    require(
        values.len() == 1 && values[0].value() == &RuntimeValue::Integer(expected),
        "the sealed ValueBatch did not carry the exact typed integer",
    )?;
    Ok(*invocation)
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_installed_inspect_end_to_end() -> TestResult<()> {
    with_test_database(|database| async move {
        install_standard(&database).await?;
        let db_kernel = kernel(&database);
        let uid = nix::unistd::geteuid().as_raw();
        let active = db_kernel.recover().await?;
        let pair = active.pair();
        let standard = active
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("the standard snapshot is pinned by the V1-to-V2 upgrade"))?;
        let standard_revision = standard.revision();
        let registry = registered_opaque_codecs(standard)?;

        // The local peer maps to the inspect principal, which holds EXECUTE
        // on std.invoke.echo. The same snapshot serves authentication and
        // the sealed dispatch, exactly like the installed product.
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                STD_INVOKE_ECHO_FUNCTION_ID,
                standard_revision,
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            )],
            vec![Principal::new(
                INSPECT_PRINCIPAL,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(
                INSPECT_PRINCIPAL,
                STD_INVOKE_ECHO_FUNCTION_ID,
            )],
            vec![LocalPeerCredential::new(uid, INSPECT_PRINCIPAL)],
        )?;
        db_kernel.replace_security_snapshot(&security).await?;

        // One cell under the echo root for the state_cells redaction proof.
        let write = write_cell(&database, ECHO_VALUE).await?;
        require(
            write == Ok(InstalledUserStateOutcome::Completed),
            "the state-cell write must complete",
        )?;

        // One protected sealed echo invocation: the dispatch appends the
        // linked EXECUTE + invocation audit evidence and auto-captures the
        // epoch and its trace rows.
        let request = sealed_echo_request(ECHO_VALUE)?;
        let retained = encode_invoke_request(&active, &registry, &request)?;
        let session = security.bind_authenticated_session(INSPECT_PRINCIPAL, vec![])?;
        let result = db_kernel
            .dispatch_sealed_sys_invoke(&session, CONNECTION_PROTOCOL_MAJOR, &retained)
            .await?;
        let invocation = require_echo_completion(&result, ECHO_VALUE)?;

        // A bare command resolves the epoch by invocation and renders the
        // closed summary record.
        let (outcome, stdout) = inspect_run(
            &database,
            InstalledInspectRequest::new(
                invocation,
                None,
                None,
                false,
                0,
                false,
                false,
                false,
                false,
                None,
            ),
        )
        .await?;
        require(
            outcome == Ok(InstalledInspectOutcome::Completed),
            "the bare inspect command must complete",
        )?;
        let summary_text = String::from_utf8(stdout)
            .map_err(|_| failure("the inspect stdout was not UTF-8 text"))?;
        let summary = record_lines(&summary_text)
            .map_err(|message| failure(&message))?
            .into_iter()
            .next()
            .ok_or_else(|| failure("the bare inspect command rendered no record"))?;
        require(
            summary["invocation"] == serde_json::json!(invocation.canonical())
                && summary["root_target"]
                    == serde_json::json!(STD_INVOKE_ECHO_FUNCTION_ID.canonical())
                && summary["outcome"] == "allowed"
                && summary["result"] == "value_batch"
                && summary["value_count"] == 1,
            "the bare inspect command did not render the exact epoch summary: {summary_text}",
        )?;
        let epoch = summary["epoch"]
            .as_str()
            .ok_or_else(|| failure("the summary record carries no epoch identity"))?
            .to_owned();

        // The invocation_nodes projection renders the root node.
        let (outcome, stdout) = inspect_run(
            &database,
            InstalledInspectRequest::new(
                invocation,
                None,
                Some(InstalledInspectProjection::InvocationNodes),
                false,
                0,
                false,
                false,
                false,
                false,
                None,
            ),
        )
        .await?;
        require(
            outcome == Ok(InstalledInspectOutcome::Completed),
            "the invocation_nodes projection must complete",
        )?;
        let node_text = String::from_utf8(stdout)
            .map_err(|_| failure("the projection stdout was not UTF-8 text"))?;
        require(
            node_text.contains("\"projection\":\"invocation_nodes\"")
                && node_text.contains(&format!("\"invocation\":\"{}\"", invocation.canonical()))
                && node_text.contains("\"kind\":\"root\"")
                && node_text.contains(&format!(
                    "\"target\":\"{}\"",
                    STD_INVOKE_ECHO_FUNCTION_ID.canonical()
                )),
            "the invocation_nodes projection did not render the root node: {node_text}",
        )?;

        // The trace streams the three model events in contiguous order and
        // resumes after the sequence boundary.
        let (outcome, stdout) = inspect_run(
            &database,
            InstalledInspectRequest::new(
                invocation,
                None,
                None,
                true,
                0,
                false,
                false,
                false,
                false,
                None,
            ),
        )
        .await?;
        require(
            outcome == Ok(InstalledInspectOutcome::Completed),
            "the trace stream must complete",
        )?;
        let trace_text = String::from_utf8(stdout)
            .map_err(|_| failure("the trace stdout was not UTF-8 text"))?;
        let trace = record_lines(&trace_text).map_err(|message| failure(&message))?;
        require(
            trace.len() == 3
                && trace
                    .iter()
                    .map(|record| record["kind"].as_str().unwrap_or("").to_owned())
                    .collect::<Vec<_>>()
                    == ["started", "value_batch", "completed"]
                && trace
                    .iter()
                    .map(|record| record["sequence"].as_u64().unwrap_or(u64::MAX))
                    .collect::<Vec<_>>()
                    == [0, 1, 2]
                && trace[1]["payload"]["value_count"] == 1
                && trace[1]["payload"]["redacted"] == true
                && trace[1]["payload"].get("values_hex").is_none(),
            format!(
                "the trace did not stream started(0), value_batch(1), completed(2): {trace_text}"
            ),
        )?;

        let trace_exact_41 = integer_hex(&database, ECHO_VALUE).await?;
        let (outcome, stdout) = inspect_run(
            &database,
            InstalledInspectRequest::new(
                invocation,
                None,
                None,
                true,
                0,
                true,
                false,
                false,
                false,
                None,
            ),
        )
        .await?;
        require(
            outcome == Ok(InstalledInspectOutcome::Completed),
            "the value-armed trace must complete",
        )?;
        let armed_trace_text = String::from_utf8(stdout)
            .map_err(|_| failure("the value-armed trace stdout was not UTF-8 text"))?;
        let armed_trace = record_lines(&armed_trace_text).map_err(|message| failure(&message))?;
        require(
            armed_trace.len() == 3
                && armed_trace[1]["payload"]["values_hex"]
                    == serde_json::json!([trace_exact_41])
                && armed_trace[1]["payload"].get("redacted").is_none(),
            "the value-armed trace must render the exact typed value: {armed_trace_text}",
        )?;


        let (outcome, stdout) = inspect_run(
            &database,
            InstalledInspectRequest::new(
                invocation,
                None,
                None,
                true,
                1,
                false,
                false,
                false,
                false,
                None,
            ),
        )
        .await?;
        require(
            outcome == Ok(InstalledInspectOutcome::Completed),
            "the resumed trace must complete",
        )?;
        let resumed_text = String::from_utf8(stdout)
            .map_err(|_| failure("the resumed trace stdout was not UTF-8 text"))?;
        let resumed = record_lines(&resumed_text).map_err(|message| failure(&message))?;
        require(
            resumed.len() == 1
                && resumed[0]["sequence"] == 2
                && resumed[0]["kind"] == "completed",
            "the resumed trace must stream only sequence 2: {resumed_text}",
        )?;

        // state_cells renders redacted unless the Values classifier is armed.
        let (outcome, stdout) = inspect_run(
            &database,
            InstalledInspectRequest::new(
                invocation,
                None,
                Some(InstalledInspectProjection::StateCells),
                false,
                0,
                false,
                false,
                false,
                false,
                None,
            ),
        )
        .await?;
        require(
            outcome == Ok(InstalledInspectOutcome::Completed),
            "the redacted state_cells projection must complete",
        )?;
        let redacted_text = String::from_utf8(stdout)
            .map_err(|_| failure("the redacted projection stdout was not UTF-8 text"))?;
        require(
            redacted_text.contains("\"projection\":\"state_cells\"")
                && redacted_text.contains("\"value_hex\":null"),
            "the unarmed state_cells projection must redact the value: {redacted_text}",
        )?;

        let (outcome, stdout) = inspect_run(
            &database,
            InstalledInspectRequest::new(
                invocation,
                None,
                Some(InstalledInspectProjection::StateCells),
                false,
                0,
                true,
                false,
                false,
                false,
                None,
            ),
        )
        .await?;
        require(
            outcome == Ok(InstalledInspectOutcome::Completed),
            "the value-armed state_cells projection must complete",
        )?;
        let armed_text = String::from_utf8(stdout)
            .map_err(|_| failure("the armed projection stdout was not UTF-8 text"))?;
        let exact_41 = integer_hex(&database, ECHO_VALUE).await?;
        require(
            armed_text.contains(&format!("\"value_hex\":\"{exact_41}\"")),
            "the armed state_cells projection must render the exact value: {armed_text}",
        )?;

        // security_decisions returns the linked EXECUTE decision.
        let (outcome, stdout) = inspect_run(
            &database,
            InstalledInspectRequest::new(
                invocation,
                None,
                Some(InstalledInspectProjection::SecurityDecisions),
                false,
                0,
                false,
                false,
                true,
                false,
                None,
            ),
        )
        .await?;
        require(
            outcome == Ok(InstalledInspectOutcome::Completed),
            "the security_decisions projection must complete",
        )?;
        let decisions_text = String::from_utf8(stdout)
            .map_err(|_| failure("the decisions projection stdout was not UTF-8 text"))?;
        let decisions =
            record_lines(&decisions_text).map_err(|message| failure(&message))?;
        require(
            decisions.len() == 1
                && decisions[0]["kind"] == "execute"
                && decisions[0]["outcome"] == "allowed"
                && decisions[0]["target"]
                    == serde_json::json!(STD_INVOKE_ECHO_FUNCTION_ID.canonical()),
            "the security_decisions projection did not return the linked EXECUTE decision: {decisions_text}",
        )?;

        // The exact epoch override resolves the same epoch by identity.
        let (outcome, stdout) = inspect_run(
            &database,
            InstalledInspectRequest::new(
                invocation,
                Some(
                    orna_core::InspectEpochId::from_canonical(&epoch)
                        .map_err(|_| failure("the rendered epoch identity must parse"))?,
                ),
                Some(InstalledInspectProjection::InvocationNodes),
                false,
                0,
                false,
                false,
                false,
                false,
                None,
            ),
        )
        .await?;
        require(
            outcome == Ok(InstalledInspectOutcome::Completed),
            "the epoch-override projection must complete",
        )?;
        let override_text = String::from_utf8(stdout)
            .map_err(|_| failure("the override projection stdout was not UTF-8 text"))?;
        require(
            override_text.contains(&format!("\"invocation\":\"{}\"", invocation.canonical())),
            "the epoch override must resolve the same invocation: {override_text}",
        )?;

        Ok(())
    })
    .await
}
