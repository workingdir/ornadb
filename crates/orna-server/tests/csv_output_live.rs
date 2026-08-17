//! Live proof: `--output csv` renders through the sealed CSV presenter
//! (work ADR 0067).
//!
//! The proof boots a fresh instance through the installed startup path
//! (`open_standard_database`, which installs the retained standard and pins
//! it active), grants EXECUTE on `std.invoke.echo` to the invoking process's
//! local peer principal, then drives one installed `orna invoke` command
//! with `--output csv` through the exact host flow. The sealed route wraps
//! the canonical echo value as the one-column `result` row set (the bounded
//! relational surface `sys.invoke` carries), renders it as one CSV document,
//! and frames the bytes as a `text/csv` ByteStream; the tty runtime writes
//! exactly those bytes to stdout with no envelope and no progress interleave,
//! and the progress diagnostics stay on stderr.

#![cfg(unix)]
#![allow(dead_code)]

mod support;

#[path = "../../orna-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use orna_core::{
    PrincipalId,
    catalogue::QualifiedSemanticName,
    invocation::InvocationTarget as InvocationRequestTarget,
    invocation_binding::CliArgumentInput,
    security::{ExecuteGrant, LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus, SecuritySnapshot},
    value::RuntimeValue,
};
use orna_postgres::PostgresKernel;
use orna_protocol::encode_constructed_value;
use orna_server::{
    InstalledInvokeError, InstalledInvokeOutcome, InstalledInvokeRequest,
    open_standard_database, run_invoke_with_kernel,
};
use orna_standard::{
    STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID, registered_opaque_codecs,
};
use postgres_test_support::{TestDatabase, TestResult, failure, with_test_database};

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

/// The local peer principal the installed host maps the invoking UID to.
const RAW_CLIENT_USER: PrincipalId = PrincipalId::from_bytes([0x71; 16]);

/// Boots a fresh instance through the installed startup path, grants EXECUTE
/// on `std.invoke.echo`, and maps the test process UID to the granted
/// principal exactly as the installed instance would for the invoking user.
async fn install_echo_grant(database: &TestDatabase) -> TestResult<PostgresKernel> {
    let kernel = open_standard_database(kernel(database))
        .await
        .map_err(|error| failure(format!("standard open failed: {error}")))?;
    let active = kernel
        .recover()
        .await
        .map_err(|error| failure(format!("recover failed: {error}")))?;
    let standard = active
        .catalogue_hash_context()
        .standard()
        .cloned()
        .ok_or_else(|| failure("open_standard_database must pin the standard snapshot"))?;
    require(
        standard
            .catalogue()
            .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
            .is_some()
            && standard
                .executables()
                .iter()
                .any(|executable| executable.function() == STD_INVOKE_ECHO_FUNCTION_ID),
        "the installed standard did not retain the std.invoke.echo executable",
    )?;
    let pair = active.pair();
    let standard_revision = standard.revision();
    let _registry = registered_opaque_codecs(&standard)?;

    let uid = nix::unistd::geteuid().as_raw();
    let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
        pair,
        vec![orna_core::security::SecurityFunctionTarget::verified_standard(
            STD_INVOKE_ECHO_FUNCTION_ID,
            standard_revision,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        )],
        vec![Principal::new(
            RAW_CLIENT_USER,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![ExecuteGrant::new(RAW_CLIENT_USER, STD_INVOKE_ECHO_FUNCTION_ID)],
        vec![LocalPeerCredential::new(uid, RAW_CLIENT_USER)],
    )
    .map_err(|error| failure(format!("security snapshot failed: {error}")))?;
    kernel
        .replace_security_snapshot(&security)
        .await
        .map_err(|error| failure(format!("security replace failed: {error}")))?;
    Ok(kernel)
}

/// Builds one installed echo invocation the way the command parser would
/// after stripping option prefixes (ADR 0056 step 4).
fn echo_invoke_request(value: i32, output: Option<String>) -> TestResult<InstalledInvokeRequest> {
    Ok(InstalledInvokeRequest::new(
        InvocationRequestTarget::qualified_name(
            QualifiedSemanticName::new(["std", "invoke", "echo"])
                .expect("the fixed echo name is qualified"),
        )
        .map_err(|error| failure(format!("echo target failed: {error}")))?,
        vec![CliArgumentInput::Canonical {
            parameter: "p_value".to_owned(),
            value: value.to_string(),
        }],
        output,
        None,
        false,
        false,
        None,
    ))
}

/// Runs one installed `orna invoke` command through the exact host flow
/// against the Compose PostgreSQL test kernel, returning the outcome or
/// failure class plus the exact bytes each channel received.
async fn installed_invoke_run(
    database: &TestDatabase,
    request: InstalledInvokeRequest,
) -> TestResult<(
    Result<InstalledInvokeOutcome, InstalledInvokeError>,
    Vec<u8>,
    Vec<u8>,
)> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let outcome =
        run_invoke_with_kernel(kernel(database), request, &mut stdout, &mut stderr).await;
    Ok((outcome, stdout, stderr))
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_csv_output_through_orna_invoke_against_postgres() -> TestResult<()> {
    const ECHO_CSV: i32 = 43;

    with_test_database(|database| async move {
        let kernel = install_echo_grant(&database).await?;

        // `--output csv` resolves the `csv` alias to std.csv.encode, which
        // wraps the canonical INTEGER 43 in a `text/csv` ByteStream: the
        // one-column `result` row set renders as the header row, the value
        // row, and the final newline. The tty runtime writes the raw stream
        // bytes to stdout: exactly `result\n43\n` with no envelope and no
        // progress interleave; the progress diagnostics stay on stderr (ADR
        // 0057 steps 7-10, ADR 0067).
        let (csv_outcome, csv_stdout, csv_stderr) = installed_invoke_run(
            &database,
            echo_invoke_request(ECHO_CSV, Some("csv".to_owned()))?,
        )
        .await?;
        require(
            csv_outcome == Ok(InstalledInvokeOutcome::Completed),
            "the --output csv installed invoke did not complete",
        )?;
        require(
            csv_stdout == b"result\n43\n",
            "the --output csv stdout did not carry exactly the CSV bytes",
        )?;
        let csv_stderr = String::from_utf8(csv_stderr)
            .map_err(|_| failure("the --output csv stderr was not UTF-8 text"))?;
        require(
            csv_stderr.contains("orna: invoke: invocation started")
                && csv_stderr.contains("orna: invoke: invocation completed in"),
            "the --output csv stderr did not carry the progress diagnostics",
        )?;

        // The same invocation without an output requirement keeps the
        // canonical typed value record on stdout (no CSV interference), so
        // the sealed output path did not change the default rendering.
        let (plain_outcome, plain_stdout, _plain_stderr) = installed_invoke_run(
            &database,
            echo_invoke_request(ECHO_CSV, None)?,
        )
        .await?;
        require(
            plain_outcome == Ok(InstalledInvokeOutcome::Completed),
            "the plain installed invoke did not complete",
        )?;
        let active = kernel
            .recover()
            .await
            .map_err(|error| failure(format!("recover failed: {error}")))?;
        let standard = active
            .catalogue_hash_context()
            .standard()
            .cloned()
            .ok_or_else(|| failure("the recovered instance must pin the standard snapshot"))?;
        let registry = registered_opaque_codecs(&standard)?;
        let mut expected_record = encode_constructed_value(
            &active,
            &registry,
            &RuntimeValue::Integer(ECHO_CSV),
        )
        .map_err(|error| failure(format!("record encode failed: {error}")))?;
        expected_record.push(b'\n');
        require(
            plain_stdout == expected_record,
            "the plain invoke stdout did not keep the canonical typed record",
        )?;

        Ok(())
    })
    .await
}
