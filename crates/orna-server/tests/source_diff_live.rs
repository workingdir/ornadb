//! Live proof: `orna source diff` renders semantic changes without apply
//! (work ADR 0066).
//!
//! The proof boots the standard chain through the V1-to-V2 upgrade, installs
//! one base application revision with a schema, an object type, and two
//! SERVER functions, then drives `run_source_diff_with_kernel` with a
//! candidate that renames one function, adds one function, and drops another.
//! The rendered report must show the rename with the stable identity
//! preserved, the addition, and the drop — and the active revision pair must
//! be byte-identical before and after the diff command.

#![cfg(unix)]
#![allow(dead_code)]

mod support;

#[path = "../../orna-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use orna_compiler::{
    StandardApplicationCheckContext, check_standard_application, prepare_standard_application,
};
use orna_core::source::{SourceBundle, SourceUnit};
use orna_postgres::PostgresKernel;
use orna_server::{
    InstalledSourceDiffError, InstalledSourceDiffOutcome, run_source_diff_with_kernel,
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

/// The installed base: one schema, one object type with one field, and two
/// SERVER functions.
const BASE_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.widget AS OBJECT (name TEXT NOT NULL);\n\
    CREATE SERVER FUNCTION app.read() RETURNS ROWS (name TEXT)\n\
    TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT widget.name FROM app.widget widget;\n\
    CREATE SERVER FUNCTION app.gone() RETURNS ROWS (name TEXT)\n\
    TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT widget.name FROM app.widget widget;\n";

/// The candidate: the field `name` is renamed to `label` through the
/// identity-preserving `ALTER TYPE ... RENAME FIELD` form (work ADR 0006),
/// `app.fresh` is added, and `app.gone` is dropped.
const CANDIDATE_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.widget AS OBJECT (label TEXT NOT NULL);\n\
    ALTER TYPE app.widget RENAME FIELD name TO label;\n\
    CREATE SERVER FUNCTION app.read() RETURNS ROWS (name TEXT)\n\
    TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT widget.label FROM app.widget widget;\n\
    CREATE SERVER FUNCTION app.fresh() RETURNS ROWS (name TEXT)\n\
    TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT widget.label FROM app.widget widget;\n";

/// A candidate with compiler diagnostics: it references a missing type.
const BROKEN_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE SERVER FUNCTION app.bad() RETURNS ROWS (name TEXT)\n\
    TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT widget.name FROM app.missing widget;\n";

/// Boots a fresh instance through the installed startup path
/// (`open_standard_database`, which installs the retained standard and pins
/// it active) and installs the base application revision.
async fn install_base(database: &TestDatabase) -> TestResult<()> {
    let kernel = orna_server::open_standard_database(kernel(database))
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
    let checked = orna_compiler::check_standard_library_source(&standard)
        .map_err(|error| failure(format!("standard source failed: {error}")))?;
    let bundle = SourceBundle::new([SourceUnit::new("main.orna", BASE_SOURCE)])
        .map_err(|error| failure(format!("base bundle failed: {error}")))?;
    let report = check_standard_application(
        &bundle,
        &StandardApplicationCheckContext::try_new(active.catalogue(), &checked)
            .map_err(|error| failure(format!("base context failed: {error}")))?,
    );
    require(
        report.diagnostics().is_empty(),
        format!("base source did not compile: {:?}", report.diagnostics()),
    )?;
    kernel
        .apply(
            &prepare_standard_application(&report, active.pair(), &active)
                .map_err(|error| failure(format!("base prepare failed: {error}")))?,
        )
        .await
        .map_err(|error| failure(format!("base apply failed: {error}")))?;
    Ok(())
}

/// Runs one installed source diff through the hidden seam and returns its
/// outcome and rendered stdout.
async fn diff_run(
    database: &TestDatabase,
    source: &str,
) -> TestResult<(
    Result<InstalledSourceDiffOutcome, InstalledSourceDiffError>,
    Vec<u8>,
)> {
    let bundle = SourceBundle::new([SourceUnit::new("main.orna", source)])
        .map_err(|error| failure(format!("bundle failed: {error}")))?;
    let outcome = run_source_diff_with_kernel(kernel(database), bundle).await;
    let bytes = match &outcome {
        Ok(InstalledSourceDiffOutcome::Diagnostics(diagnostics)) => diagnostics.as_bytes().to_vec(),
        Ok(InstalledSourceDiffOutcome::Diff(report)) => report.as_bytes().to_vec(),
        Ok(_) => Vec::new(),
        Err(_) => Vec::new(),
    };
    Ok((outcome, bytes))
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_installed_source_diff_end_to_end() -> TestResult<()> {
    with_test_database(|database| async move {
        install_base(&database).await?;

        let before = kernel(&database).recover().await?;
        let before_pair = before.pair();

        // The candidate renames the object field through the identity
        // preserving ALTER form, adds one function, and drops one; the
        // schema and object type identities are unchanged.
        let (outcome, stdout) = diff_run(&database, CANDIDATE_SOURCE).await?;
        require(
            matches!(outcome, Ok(InstalledSourceDiffOutcome::Diff(_))),
            "the source diff did not produce a prepared report",
        )?;
        let text =
            String::from_utf8(stdout).map_err(|_| failure("the diff stdout was not UTF-8 text"))?;
        require(
            text.contains("~ field app.widget.name -> app.widget.label")
                && text.contains("+ function app.fresh")
                && text.contains("- function app.gone"),
            "the diff report did not render the field rename, add, and drop: {text}",
        )?;

        // The diff command must not change the active revision pair.
        let after = kernel(&database).recover().await?;
        require(
            after.pair() == before_pair,
            "the source diff command changed the active revision pair",
        )?;

        // A candidate with diagnostics renders them with the source-check
        // contract and prepares no candidate.
        let (outcome, stdout) = diff_run(&database, BROKEN_SOURCE).await?;
        require(
            matches!(outcome, Ok(InstalledSourceDiffOutcome::Diagnostics(_))),
            "the broken candidate did not return the diagnostics outcome",
        )?;
        let text = String::from_utf8(stdout)
            .map_err(|_| failure("the diagnostics stdout was not UTF-8 text"))?;
        require(
            text.contains("main.orna:") && text.contains("ORNA0101"),
            "the diagnostics did not render with the source-check contract: {text}",
        )?;

        // An identical candidate produces an empty diff report.
        let (outcome, stdout) = diff_run(&database, BASE_SOURCE).await?;
        require(
            matches!(outcome, Ok(InstalledSourceDiffOutcome::Diff(_))),
            "the identical candidate did not produce a diff report",
        )?;
        let text = String::from_utf8(stdout)
            .map_err(|_| failure("the identical diff stdout was not UTF-8 text"))?;
        require(
            text.contains("no semantic changes"),
            "the identical candidate did not report no semantic changes: {text}",
        )?;

        Ok(())
    })
    .await
}
