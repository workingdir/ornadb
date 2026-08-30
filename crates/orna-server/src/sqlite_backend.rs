//! Direct local-file command support for the SQLite backend.

use orna_compiler::{check, prepare};
use orna_core::{
    CatalogueRevisionId, FunctionId, InvocationId, ParameterId, SourceBundleId, SourceRevisionId,
    canonical_hash::{catalogue_digest, source_bundle_digest, source_revision_record_digest},
    catalogue::{CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn},
    invocation::{InvocationParameterSelector, InvocationTarget},
    physical::PhysicalMigrationArtifact,
    revision::{
        ActiveDatabaseRevision, CatalogueHashVersion, DeployableRevision, RevisionPair,
        StoredSourceRevision,
    },
    security::CATALOGUE_HEALTH_FUNCTION_ID,
    source::{SourceBundle, SourceUnit},
    types::StandardScalar,
    value::RuntimeValue,
};
use orna_protocol::{CallFailure, MAX_FRAME_PAYLOAD_LENGTH, decode_value, encode_value};
use orna_sqlite::{SqliteCapability, SqliteConfig, SqliteError, SqliteRevisionStore};
use orna_storage::{ApplicationRevisionStore, StorageError};
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::{
    error::Error,
    fmt, fs,
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{
    InstalledInvokeError, InstalledInvokeErrorKind, InstalledInvokeOutcome, InstalledInvokeRequest,
    LocalRawCallError, LocalRawCallOutcome, source_diagnostics, source_diff,
};
/// The result of checking or applying one source file against a local SQLite file.
#[derive(Debug)]
#[non_exhaustive]
pub enum SqliteSourceApplyOutcome {
    /// Compiler diagnostics prevented preparation or mutation.
    Diagnostics {
        /// Stable machine-readable diagnostics.
        bytes: Vec<u8>,
        /// Human-readable diagnostics without colour.
        human_bytes: Vec<u8>,
        /// Human-readable diagnostics with colour.
        coloured_bytes: Vec<u8>,
    },
    /// The candidate committed and the discovery document is ready.
    Applied(Vec<u8>),
}

/// The result of checking and diffing one source file against a local SQLite file.
#[derive(Debug)]
#[non_exhaustive]
pub enum SqliteSourceDiffOutcome {
    /// Compiler diagnostics prevented preparation or mutation.
    Diagnostics {
        /// Stable machine-readable diagnostics.
        bytes: Vec<u8>,
        /// Human-readable diagnostics without colour.
        human_bytes: Vec<u8>,
        /// Human-readable diagnostics with colour.
        coloured_bytes: Vec<u8>,
    },
    /// The deterministic semantic diff document.
    Diff(Vec<u8>),
}

/// A local SQLite command failed before it could produce a command result.
#[derive(Debug)]
pub struct SqliteBackendError {
    message: String,
}

impl SqliteBackendError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn from_error(error: impl Error) -> Self {
        Self::new(error.to_string())
    }
}

impl fmt::Display for SqliteBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SqliteBackendError {}

/// Runs `source apply` directly against a local SQLite path.
pub fn run_sqlite_source_apply(
    database_path: impl Into<PathBuf>,
    source_path: &str,
) -> Result<SqliteSourceApplyOutcome, SqliteBackendError> {
    let source = read_source(source_path)?;
    let bundle = SourceBundle::new([SourceUnit::new(source_path, source)])
        .map_err(SqliteBackendError::from_error)?;
    let database_path = database_path.into();
    if database_path_is_fresh(&database_path)?
        && let Some(outcome) = preflight_fresh_source_apply(&bundle)?
    {
        return Ok(outcome);
    }
    match run_with_runtime(database_path, bundle, SqliteCommand::Apply)? {
        SqliteCommandOutcome::Apply(outcome) => Ok(outcome),
        SqliteCommandOutcome::Diff(_) => Err(SqliteBackendError::new(
            "orna: local SQLite backend returned the wrong source result",
        )),
    }
}

fn database_path_is_fresh(path: &Path) -> Result<bool, SqliteBackendError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len() == 0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(SqliteBackendError::new(format!(
            "orna: could not inspect SQLite database: {error}"
        ))),
    }
}

fn preflight_fresh_source_apply(
    bundle: &SourceBundle,
) -> Result<Option<SqliteSourceApplyOutcome>, SqliteBackendError> {
    let active = fresh_validation_active()?;
    let report = check(bundle, active.catalogue());
    if !report.diagnostics().is_empty() {
        return Ok(Some(SqliteSourceApplyOutcome::Diagnostics {
            bytes: source_diagnostics::render_diagnostics(report.diagnostics()),
            human_bytes: source_diagnostics::render_human_diagnostics(
                report.parse_report(),
                report.diagnostics(),
                false,
            ),
            coloured_bytes: source_diagnostics::render_human_diagnostics(
                report.parse_report(),
                report.diagnostics(),
                true,
            ),
        }));
    }
    let candidate = prepare(&report, active.pair(), &active).map_err(|error| {
        SqliteBackendError::new(format!(
            "orna: source apply could not prepare the source: {error}"
        ))
    })?;
    PhysicalMigrationArtifact::from_revisions(&active, &candidate).map_err(|error| {
        SqliteBackendError::new(format!(
            "orna: source apply could not prepare the source: {error}"
        ))
    })?;
    ensure_sqlite_candidate_supported(&candidate)?;
    Ok(None)
}

fn fresh_validation_active() -> Result<ActiveDatabaseRevision, SqliteBackendError> {
    let source_bundle = SourceBundleId::new();
    let source_revision = SourceRevisionId::new();
    let catalogue_revision = CatalogueRevisionId::new();
    let source_bundle_hash = source_bundle_digest(&[]).map_err(SqliteBackendError::from_error)?;
    let source_revision_hash =
        source_revision_record_digest(source_bundle, None, source_bundle_hash)
            .map_err(SqliteBackendError::from_error)?;
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        None,
        Vec::new(),
        source_bundle_hash,
        source_revision_hash,
    )
    .map_err(SqliteBackendError::from_error)?;
    let catalogue = CatalogueSnapshot::new(catalogue_revision, Vec::new(), Vec::new())
        .map_err(SqliteBackendError::from_error)?;
    let catalogue_hash =
        catalogue_digest(&catalogue, &[], &[], &[], &[]).map_err(SqliteBackendError::from_error)?;
    ActiveDatabaseRevision::new(
        RevisionPair::new(source_revision, catalogue_revision),
        source,
        catalogue,
        catalogue_hash,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .map_err(SqliteBackendError::from_error)
}

/// Runs `source diff` directly against a local SQLite path.
pub fn run_sqlite_source_diff(
    database_path: impl Into<PathBuf>,
    source_path: &str,
) -> Result<SqliteSourceDiffOutcome, SqliteBackendError> {
    let database_path = database_path.into();
    ensure_existing_database(&database_path)?;
    let source = read_source(source_path)?;
    let bundle = SourceBundle::new([SourceUnit::new(source_path, source)])
        .map_err(SqliteBackendError::from_error)?;
    match run_with_runtime(database_path, bundle, SqliteCommand::Diff)? {
        SqliteCommandOutcome::Apply(_) => Err(SqliteBackendError::new(
            "orna: local SQLite backend returned the wrong source result",
        )),
        SqliteCommandOutcome::Diff(outcome) => Ok(outcome),
    }
}

/// Runs one raw local call directly against a SQLite database.
///
/// The parameter identifiers retain the raw-call ordering contract and the
/// values are read as bounded canonical ORV5 envelopes from `stdin`.
pub fn run_sqlite_raw_call(
    database_path: impl Into<PathBuf>,
    function: FunctionId,
    parameters: &[ParameterId],
    stdin: &mut impl Read,
    stdout: &mut impl Write,
) -> Result<LocalRawCallOutcome, LocalRawCallError> {
    let values = read_sqlite_raw_arguments(parameters.len(), stdin)?;
    if parameters.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(LocalRawCallError::Input);
    }
    if values.len() != parameters.len() {
        return Err(LocalRawCallError::Input);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| LocalRawCallError::Connection)?;
    runtime.block_on(async {
        let store = SqliteRevisionStore::open(&SqliteConfig::new(database_path.into()))
            .await
            .map_err(|_| LocalRawCallError::Connection)?;
        let active = store
            .recover()
            .await
            .map_err(|_| LocalRawCallError::Connection)?;
        let uid = nix::unistd::geteuid().as_raw();
        store
            .provision_local_peer(uid)
            .await
            .map_err(|_| LocalRawCallError::Connection)?;
        let bound = parameters.iter().copied().zip(values).collect::<Vec<_>>();
        let invocation = InvocationId::new();
        let execution = match store
            .execute_local_peer_server_function_at(&active, uid, invocation, function, &bound)
            .await
        {
            Ok(execution) => execution,
            Err(SqliteError::Domain(_)) => {
                return Ok(LocalRawCallOutcome::Failed(
                    CallFailure::ClientEvaluationFailed,
                ));
            }
            Err(_) => return Err(LocalRawCallError::Connection),
        };
        let values = match execution {
            orna_sqlite::SqliteExecutionResult::Denied { .. } => {
                return Ok(LocalRawCallOutcome::Failed(CallFailure::ExecuteDenied));
            }
            orna_sqlite::SqliteExecutionResult::Failed { error, .. } => {
                let _ = error;
                return Ok(LocalRawCallOutcome::Failed(
                    CallFailure::ClientEvaluationFailed,
                ));
            }
            orna_sqlite::SqliteExecutionResult::Allowed { values, .. } => values,
        };
        for value in values {
            let encoded = encode_value(&value).map_err(|_| LocalRawCallError::Output)?;
            stdout
                .write_all(&encoded)
                .and_then(|()| stdout.write_all(b"\n"))
                .map_err(|_| LocalRawCallError::Output)?;
        }
        Ok(LocalRawCallOutcome::Completed)
    })
}

fn read_sqlite_raw_arguments(
    parameter_count: usize,
    stdin: &mut impl Read,
) -> Result<Vec<RuntimeValue>, LocalRawCallError> {
    if parameter_count == 0 {
        return Ok(Vec::new());
    }
    if parameter_count > 2 {
        return Err(LocalRawCallError::Input);
    }
    let maximum = u64::try_from(MAX_FRAME_PAYLOAD_LENGTH)
        .map_err(|_| LocalRawCallError::Input)?
        .saturating_add(1);
    let mut bytes = Vec::new();
    stdin
        .take(maximum)
        .read_to_end(&mut bytes)
        .map_err(|_| LocalRawCallError::Input)?;
    if bytes.len() > MAX_FRAME_PAYLOAD_LENGTH {
        return Err(LocalRawCallError::Input);
    }
    let mut offset = 0;
    let mut values = Vec::with_capacity(parameter_count);
    for _ in 0..parameter_count {
        let (value, consumed) = decode_sqlite_raw_value(&bytes[offset..])?;
        offset = offset
            .checked_add(consumed)
            .ok_or(LocalRawCallError::Input)?;
        values.push(value);
    }
    if offset != bytes.len() {
        return Err(LocalRawCallError::Input);
    }
    Ok(values)
}

fn decode_sqlite_raw_value(bytes: &[u8]) -> Result<(RuntimeValue, usize), LocalRawCallError> {
    const VALUE_HEADER_LENGTH: usize = 25;
    if bytes.len() < VALUE_HEADER_LENGTH {
        return Err(LocalRawCallError::Input);
    }
    let payload_length = u32::from_be_bytes(
        bytes[VALUE_HEADER_LENGTH - 4..VALUE_HEADER_LENGTH]
            .try_into()
            .map_err(|_| LocalRawCallError::Input)?,
    ) as usize;
    let end = VALUE_HEADER_LENGTH
        .checked_add(payload_length)
        .ok_or(LocalRawCallError::Input)?;
    if end > bytes.len() {
        return Err(LocalRawCallError::Input);
    }
    let value = decode_value(&bytes[..end]).map_err(|_| LocalRawCallError::Input)?;
    Ok((value, end))
}

/// Runs `invoke` directly against a local SQLite path.
///
/// The route keeps the installed CLI contract (target reflection, CLI
/// binding, local peer authentication, and clean stdout/stderr channels) but
/// executes the supported SQLite server-artifact subset without constructing a
/// PostgreSQL socket or silently falling back to the embedded backend.
pub fn run_sqlite_invoke(
    database_path: impl Into<PathBuf>,
    request: InstalledInvokeRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            sqlite_invoke_error(
                InstalledInvokeErrorKind::Internal,
                format!("the private runtime could not start: {error}"),
            )
        })?;
    runtime.block_on(run_sqlite_invoke_async(
        database_path.into(),
        request,
        stdout,
        stderr,
    ))
}

async fn run_sqlite_invoke_async(
    database_path: PathBuf,
    request: InstalledInvokeRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<InstalledInvokeOutcome, InstalledInvokeError> {
    if matches!(request.runtime, Some(crate::RuntimeFamily::Qt)) {
        return Err(sqlite_invoke_error(
            InstalledInvokeErrorKind::Usage,
            "the SQLite backend does not provide a Qt runtime",
        ));
    }
    if request.trace.is_some() {
        return Err(sqlite_invoke_error(
            InstalledInvokeErrorKind::Usage,
            "the SQLite backend does not support --trace",
        ));
    }

    let store = SqliteRevisionStore::open(&SqliteConfig::new(&database_path))
        .await
        .map_err(|error| sqlite_invoke_sqlite_error(error, InstalledInvokeErrorKind::Internal))?;
    let uid = nix::unistd::geteuid().as_raw();
    store.provision_local_peer(uid).await.map_err(|error| {
        sqlite_invoke_sqlite_error(error, InstalledInvokeErrorKind::Authentication)
    })?;
    let active = ApplicationRevisionStore::recover(&store)
        .await
        .map_err(|error| {
            sqlite_invoke_error(
                InstalledInvokeErrorKind::Internal,
                format!("the active SQLite revision could not be recovered: {error}"),
            )
        })?;
    let function = resolve_sqlite_function(&active, &request.target).ok_or_else(|| {
        sqlite_invoke_error(
            InstalledInvokeErrorKind::Usage,
            "the requested function is not installed in the SQLite catalogue",
        )
    })?;
    if function.domain() != FunctionDomain::Server {
        return Err(sqlite_invoke_error(
            InstalledInvokeErrorKind::Usage,
            "the SQLite invoke route accepts SERVER functions only",
        ));
    }
    let arguments = crate::invoke::bind_installed_cli_arguments(
        active.catalogue(),
        None,
        function,
        &request.arguments,
    )
    .map_err(|error| sqlite_invoke_error(InstalledInvokeErrorKind::Usage, error.to_string()))?;
    let output = parse_sqlite_output(request.output.as_deref())?;

    if request.explain {
        render_sqlite_explain(stdout, function, &request.target, output)?;
        return Ok(InstalledInvokeOutcome::Completed);
    }

    let invocation_id = InvocationId::new();

    let bound = arguments
        .into_iter()
        .map(|argument| {
            let InvocationParameterSelector::ParameterId(parameter) = argument.selector() else {
                return Err(sqlite_invoke_error(
                    InstalledInvokeErrorKind::Internal,
                    "SQLite CLI binding returned a non-canonical parameter selector",
                ));
            };
            Ok((*parameter, argument.value().value().clone()))
        })
        .collect::<Result<Vec<_>, InstalledInvokeError>>()?;

    let started = Instant::now();
    if !request.no_progress {
        writeln!(stderr, "orna: invoke: invocation started").map_err(|error| {
            sqlite_invoke_error(
                InstalledInvokeErrorKind::Presentation,
                format!("could not write invocation progress: {error}"),
            )
        })?;
    }
    let values = match store
        .execute_local_peer_server_function_at(&active, uid, invocation_id, function.id(), &bound)
        .await
        .map_err(|error| sqlite_invoke_sqlite_error(error, InstalledInvokeErrorKind::Internal))?
    {
        orna_sqlite::SqliteExecutionResult::Allowed { values, .. } => values,
        orna_sqlite::SqliteExecutionResult::Denied { .. } => {
            writeln!(stderr, "orna: invoke: invocation denied").map_err(|error| {
                sqlite_invoke_error(
                    InstalledInvokeErrorKind::Presentation,
                    format!("could not write invocation denial: {error}"),
                )
            })?;
            return Ok(InstalledInvokeOutcome::Denied);
        }
        orna_sqlite::SqliteExecutionResult::Failed { error, .. } => {
            writeln!(stderr, "orna: invoke: invocation failed").map_err(|write_error| {
                sqlite_invoke_error(
                    InstalledInvokeErrorKind::Presentation,
                    format!("could not write invocation failure: {write_error}"),
                )
            })?;
            let _ = error;
            return Ok(InstalledInvokeOutcome::TargetFailure);
        }
    };
    render_sqlite_values(function, &values, output, stdout)?;
    if !request.no_progress {
        writeln!(
            stderr,
            "orna: invoke: invocation completed in {}ns",
            started.elapsed().as_nanos()
        )
        .map_err(|error| {
            sqlite_invoke_error(
                InstalledInvokeErrorKind::Presentation,
                format!("could not write invocation progress: {error}"),
            )
        })?;
    }
    Ok(InstalledInvokeOutcome::Completed)
}

fn resolve_sqlite_function<'a>(
    active: &'a ActiveDatabaseRevision,
    target: &InvocationTarget,
) -> Option<&'a FunctionDefinition> {
    match target {
        InvocationTarget::FunctionId(function) => active.catalogue().function_by_id(*function),
        InvocationTarget::QualifiedName(name) => active.catalogue().function_by_name(name),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum SqliteOutput {
    Canonical,
    Json,
    Table,
    Csv,
}

fn parse_sqlite_output(value: Option<&str>) -> Result<SqliteOutput, InstalledInvokeError> {
    match value {
        None => Ok(SqliteOutput::Canonical),
        Some("json" | "application/json") => Ok(SqliteOutput::Json),
        Some("table" | "text/plain") => Ok(SqliteOutput::Table),
        Some("csv" | "text/csv") => Ok(SqliteOutput::Csv),
        Some(value) => Err(sqlite_invoke_error(
            InstalledInvokeErrorKind::Presentation,
            format!("the SQLite backend has no presenter for `{value}`"),
        )),
    }
}

fn render_sqlite_explain(
    stdout: &mut impl Write,
    function: &FunctionDefinition,
    target: &InvocationTarget,
    output: SqliteOutput,
) -> Result<(), InstalledInvokeError> {
    let output = match output {
        SqliteOutput::Canonical => "canonical",
        SqliteOutput::Json => "json",
        SqliteOutput::Table => "table",
        SqliteOutput::Csv => "csv",
    };
    let document = serde_json::json!({
        "backend": "sqlite",
        "target": invocation_target_text(target),
        "function_id": function.id().canonical(),
        "qualified_name": function.name().parts(),
        "output": output,
    });
    serde_json::to_writer(&mut *stdout, &document).map_err(|error| {
        sqlite_invoke_error(
            InstalledInvokeErrorKind::Presentation,
            format!("could not render invocation explanation: {error}"),
        )
    })?;
    stdout.write_all(b"\n").map_err(|error| {
        sqlite_invoke_error(
            InstalledInvokeErrorKind::Presentation,
            format!("could not render invocation explanation: {error}"),
        )
    })
}

fn invocation_target_text(target: &InvocationTarget) -> String {
    match target {
        InvocationTarget::FunctionId(function) => function.canonical(),
        InvocationTarget::QualifiedName(name) => name.to_string(),
        _ => "<unknown>".to_owned(),
    }
}

fn render_sqlite_values(
    function: &FunctionDefinition,
    values: &[RuntimeValue],
    output: SqliteOutput,
    stdout: &mut impl Write,
) -> Result<(), InstalledInvokeError> {
    if matches!(output, SqliteOutput::Canonical) {
        for value in values {
            let encoded = encode_value(value).map_err(|error| {
                sqlite_invoke_error(
                    InstalledInvokeErrorKind::Presentation,
                    format!("a result value could not be encoded canonically: {error}"),
                )
            })?;
            stdout
                .write_all(&encoded)
                .map_err(sqlite_presentation_error)?;
            stdout.write_all(b"\n").map_err(sqlite_presentation_error)?;
        }
        return Ok(());
    }

    let rows = result_rows(function, values)?;
    match output {
        SqliteOutput::Json => render_json_rows(function, &rows, stdout),
        SqliteOutput::Table => render_table_rows(function, &rows, stdout),
        SqliteOutput::Csv => render_csv_rows(function, &rows, stdout),
        SqliteOutput::Canonical => unreachable!("canonical output returned above"),
    }
}

fn result_rows<'a>(
    function: &FunctionDefinition,
    values: &'a [RuntimeValue],
) -> Result<Vec<Vec<&'a RuntimeValue>>, InstalledInvokeError> {
    let width = match function.return_type() {
        FunctionReturn::Single(_) | FunctionReturn::Stream(_) => 1,
        FunctionReturn::Rows(columns) => columns.len(),
    };
    if width == 0 || !values.len().is_multiple_of(width) {
        return Err(sqlite_invoke_error(
            InstalledInvokeErrorKind::Presentation,
            "the SQLite result does not match the function return shape",
        ));
    }
    Ok(values
        .chunks(width)
        .map(|row| row.iter().collect())
        .collect())
}

fn render_json_rows(
    function: &FunctionDefinition,
    rows: &[Vec<&RuntimeValue>],
    stdout: &mut impl Write,
) -> Result<(), InstalledInvokeError> {
    let document = match function.return_type() {
        FunctionReturn::Single(_) => rows
            .first()
            .and_then(|row| row.first())
            .map(|value| runtime_value_json(value))
            .transpose()?
            .unwrap_or(JsonValue::Null),
        FunctionReturn::Stream(_) => JsonValue::Array(
            rows.iter()
                .map(|row| runtime_value_json(row[0]))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        FunctionReturn::Rows(columns) => JsonValue::Array(
            rows.iter()
                .map(|row| {
                    let mut object = serde_json::Map::new();
                    for (column, value) in columns.iter().zip(row) {
                        object.insert(column.name().to_owned(), runtime_value_json(value)?);
                    }
                    Ok(JsonValue::Object(object))
                })
                .collect::<Result<Vec<_>, InstalledInvokeError>>()?,
        ),
    };
    serde_json::to_writer(&mut *stdout, &document).map_err(|error| {
        sqlite_invoke_error(
            InstalledInvokeErrorKind::Presentation,
            format!("could not render JSON output: {error}"),
        )
    })?;
    stdout.write_all(b"\n").map_err(sqlite_presentation_error)
}

fn render_table_rows(
    function: &FunctionDefinition,
    rows: &[Vec<&RuntimeValue>],
    stdout: &mut impl Write,
) -> Result<(), InstalledInvokeError> {
    if let FunctionReturn::Rows(columns) = function.return_type() {
        for (index, column) in columns.iter().enumerate() {
            if index > 0 {
                stdout.write_all(b"\t").map_err(sqlite_presentation_error)?;
            }
            write!(stdout, "{}", column.name()).map_err(sqlite_presentation_error)?;
        }
        stdout.write_all(b"\n").map_err(sqlite_presentation_error)?;
    }
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            if index > 0 {
                stdout.write_all(b"\t").map_err(sqlite_presentation_error)?;
            }
            write!(stdout, "{}", runtime_value_text(value)).map_err(sqlite_presentation_error)?;
        }
        stdout.write_all(b"\n").map_err(sqlite_presentation_error)?;
    }
    Ok(())
}

fn render_csv_rows(
    function: &FunctionDefinition,
    rows: &[Vec<&RuntimeValue>],
    stdout: &mut impl Write,
) -> Result<(), InstalledInvokeError> {
    let columns = match function.return_type() {
        FunctionReturn::Rows(columns) => columns.iter().map(|column| column.name()).collect(),
        FunctionReturn::Single(_) | FunctionReturn::Stream(_) => vec!["value"],
    };
    write_csv_record(stdout, &columns)?;
    for row in rows {
        let values = row
            .iter()
            .map(|value| runtime_value_text(value))
            .collect::<Vec<_>>();
        write_csv_record(stdout, &values)?;
    }
    Ok(())
}

fn write_csv_record(
    stdout: &mut impl Write,
    values: &[impl AsRef<str>],
) -> Result<(), InstalledInvokeError> {
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            stdout.write_all(b",").map_err(sqlite_presentation_error)?;
        }
        let value = value.as_ref();
        if value.contains([',', '"', '\n', '\r']) {
            write!(stdout, "\"{}\"", value.replace('"', "\"\""))
                .map_err(sqlite_presentation_error)?;
        } else {
            stdout
                .write_all(value.as_bytes())
                .map_err(sqlite_presentation_error)?;
        }
    }
    stdout.write_all(b"\n").map_err(sqlite_presentation_error)
}

fn runtime_value_json(value: &RuntimeValue) -> Result<JsonValue, InstalledInvokeError> {
    match value {
        RuntimeValue::Null(_) => Ok(JsonValue::Null),
        RuntimeValue::Boolean(value) => Ok(JsonValue::Bool(*value)),
        RuntimeValue::Integer(value) => Ok(JsonValue::from(*value)),
        RuntimeValue::BigInt(value) => Ok(JsonValue::from(*value)),
        RuntimeValue::Float(value) => serde_json::Number::from_f64(value.value())
            .map(JsonValue::Number)
            .ok_or_else(|| {
                sqlite_invoke_error(
                    InstalledInvokeErrorKind::Presentation,
                    "a FLOAT result is not representable as JSON",
                )
            }),
        RuntimeValue::Text(value) => Ok(JsonValue::String(value.clone())),
        RuntimeValue::Bytes(value) => Ok(JsonValue::String(hex_encode(value))),
        RuntimeValue::Reference { target, object } => Ok(serde_json::json!({
            "target": target.canonical(),
            "object": object.canonical(),
        })),
        _ => Err(sqlite_invoke_error(
            InstalledInvokeErrorKind::Presentation,
            "the SQLite JSON presenter does not support this runtime value",
        )),
    }
}

fn runtime_value_text(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::Null(_) => "NULL".to_owned(),
        RuntimeValue::Boolean(value) => value.to_string(),
        RuntimeValue::Integer(value) => value.to_string(),
        RuntimeValue::BigInt(value) => value.to_string(),
        RuntimeValue::Float(value) => value.value().to_string(),
        RuntimeValue::Text(value) => value.clone(),
        RuntimeValue::Bytes(value) => hex_encode(value),
        RuntimeValue::Reference { target, object } => {
            format!("@{}/{}", target.canonical(), object.canonical())
        }
        _ => "<unsupported>".to_owned(),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[usize::from(byte >> 4)] as char);
        output.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn sqlite_invoke_error(
    kind: InstalledInvokeErrorKind,
    message: impl Into<String>,
) -> InstalledInvokeError {
    InstalledInvokeError::new(kind, message.into())
}

fn sqlite_presentation_error(error: io::Error) -> InstalledInvokeError {
    sqlite_invoke_error(
        InstalledInvokeErrorKind::Presentation,
        format!("could not render SQLite invocation output: {error}"),
    )
}

fn sqlite_invoke_sqlite_error(
    error: SqliteError,
    kind: InstalledInvokeErrorKind,
) -> InstalledInvokeError {
    sqlite_invoke_error(kind, format!("local SQLite backend error: {error}"))
}

fn ensure_existing_database(path: &Path) -> Result<(), SqliteBackendError> {
    let metadata = fs::metadata(path).map_err(|error| {
        SqliteBackendError::new(format!("orna: could not open SQLite database: {error}"))
    })?;
    if !metadata.is_file() {
        return Err(SqliteBackendError::new(format!(
            "orna: SQLite database path is not a regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn read_source(path: &str) -> Result<String, SqliteBackendError> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            SqliteBackendError::new(format!("orna: could not read source file: {error}"))
        })?;
    if !file
        .metadata()
        .map_err(|error| {
            SqliteBackendError::new(format!("orna: could not read source file: {error}"))
        })?
        .is_file()
    {
        return Err(SqliteBackendError::new(format!(
            "orna: source path is not a regular file: {path}"
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| {
        SqliteBackendError::new(format!("orna: could not read source file: {error}"))
    })?;
    String::from_utf8(bytes)
        .map_err(|_| SqliteBackendError::new("orna: source file is not valid UTF-8"))
}

#[derive(Clone, Copy)]
enum SqliteCommand {
    Apply,
    Diff,
}

enum SqliteCommandOutcome {
    Apply(SqliteSourceApplyOutcome),
    Diff(SqliteSourceDiffOutcome),
}

fn run_with_runtime(
    database_path: PathBuf,
    bundle: SourceBundle,
    command: SqliteCommand,
) -> Result<SqliteCommandOutcome, SqliteBackendError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| SqliteBackendError::new(format!("orna: local runtime failed: {error}")))?;
    runtime.block_on(run_async(database_path, bundle, command))
}

async fn run_async(
    database_path: PathBuf,
    bundle: SourceBundle,
    command: SqliteCommand,
) -> Result<SqliteCommandOutcome, SqliteBackendError> {
    let store = match command {
        SqliteCommand::Apply => {
            let store = SqliteRevisionStore::open(&SqliteConfig::new(database_path.clone()))
                .await
                .map_err(map_sqlite_error)?;
            ApplicationRevisionStore::bootstrap(&store)
                .await
                .map_err(map_storage_error)?;
            store
        }
        SqliteCommand::Diff => {
            ensure_existing_database(&database_path)?;
            SqliteRevisionStore::open_read_only(&SqliteConfig::new(database_path))
                .await
                .map_err(map_sqlite_error)?
        }
    };
    let active = ApplicationRevisionStore::recover(&store)
        .await
        .map_err(map_storage_error)?;
    let report = check(&bundle, active.catalogue());
    if !report.diagnostics().is_empty() {
        let bytes = source_diagnostics::render_diagnostics(report.diagnostics());
        let human_bytes = source_diagnostics::render_human_diagnostics(
            report.parse_report(),
            report.diagnostics(),
            false,
        );
        let coloured_bytes = source_diagnostics::render_human_diagnostics(
            report.parse_report(),
            report.diagnostics(),
            true,
        );
        return Ok(match command {
            SqliteCommand::Apply => {
                SqliteCommandOutcome::Apply(SqliteSourceApplyOutcome::Diagnostics {
                    bytes,
                    human_bytes,
                    coloured_bytes,
                })
            }
            SqliteCommand::Diff => {
                SqliteCommandOutcome::Diff(SqliteSourceDiffOutcome::Diagnostics {
                    bytes,
                    human_bytes,
                    coloured_bytes,
                })
            }
        });
    }
    let operation = match command {
        SqliteCommand::Apply => "source apply",
        SqliteCommand::Diff => "source diff",
    };
    let candidate = prepare(&report, active.pair(), &active).map_err(|error| {
        SqliteBackendError::new(format!(
            "orna: {operation} could not prepare the source: {error}"
        ))
    })?;
    let artifact =
        PhysicalMigrationArtifact::from_revisions(&active, &candidate).map_err(|error| {
            SqliteBackendError::new(format!(
                "orna: {operation} could not prepare the source: {error}"
            ))
        })?;
    ensure_sqlite_candidate_supported(&candidate)?;

    match command {
        SqliteCommand::Apply => {
            let committed =
                ApplicationRevisionStore::apply_source_apply(&store, &candidate, &artifact)
                    .await
                    .map_err(map_storage_error)?;
            let bytes = success_document(&committed)?;
            Ok(SqliteCommandOutcome::Apply(
                SqliteSourceApplyOutcome::Applied(bytes),
            ))
        }
        SqliteCommand::Diff => {
            let bytes = source_diff::render_prepared_source_diff(&active, &candidate)
                .map_err(|error| SqliteBackendError::new(error.to_string()))?;
            Ok(SqliteCommandOutcome::Diff(SqliteSourceDiffOutcome::Diff(
                bytes,
            )))
        }
    }
}

fn ensure_sqlite_candidate_supported(
    candidate: &DeployableRevision,
) -> Result<(), SqliteBackendError> {
    let catalogue = candidate.candidate();
    if !catalogue.value_types().is_empty() {
        return Err(map_sqlite_error(SqliteError::UnsupportedCapability(
            SqliteCapability::ValueType,
        )));
    }
    if !catalogue.enum_types().is_empty() {
        return Err(map_sqlite_error(SqliteError::UnsupportedCapability(
            SqliteCapability::EnumType,
        )));
    }
    if !catalogue.record_value_types().is_empty() {
        return Err(map_sqlite_error(SqliteError::UnsupportedCapability(
            SqliteCapability::RecordValueType,
        )));
    }
    if !catalogue.type_bindings().is_empty() {
        return Err(map_sqlite_error(SqliteError::UnsupportedCapability(
            SqliteCapability::TypeBinding,
        )));
    }
    if catalogue.object_types().iter().any(|object| {
        object.fields().iter().any(|field| {
            matches!(
                field.resolved_type().legacy_scalar(),
                Some(scalar)
                    if !matches!(
                        scalar,
                        StandardScalar::Boolean
                            | StandardScalar::Integer
                            | StandardScalar::BigInt
                            | StandardScalar::Float
                            | StandardScalar::CharacterLargeObject
                            | StandardScalar::BinaryLargeObject
                    )
            )
        })
    }) {
        return Err(map_sqlite_error(SqliteError::UnsupportedCapability(
            SqliteCapability::ScalarType,
        )));
    }
    if candidate.catalogue_hash_context().version() != CatalogueHashVersion::Version1 {
        return Err(map_sqlite_error(SqliteError::UnsupportedCapability(
            SqliteCapability::CatalogueHashVersion,
        )));
    }
    Ok(())
}

fn success_document(active: &ActiveDatabaseRevision) -> Result<Vec<u8>, SqliteBackendError> {
    #[derive(Serialize)]
    struct Document {
        source_revision: String,
        catalogue_revision: String,
        functions: Vec<FunctionDocument>,
    }
    #[derive(Serialize)]
    struct FunctionDocument {
        qualified_name: Vec<String>,
        function_id: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        parameters: Vec<ParameterDocument>,
    }
    #[derive(Serialize)]
    struct ParameterDocument {
        name: String,
        parameter_id: String,
    }

    let mut functions = active
        .catalogue()
        .functions()
        .iter()
        .filter(|function| function.id() != CATALOGUE_HEALTH_FUNCTION_ID)
        .collect::<Vec<_>>();
    functions.sort_by(|left, right| {
        left.name()
            .parts()
            .cmp(right.name().parts())
            .then_with(|| left.id().cmp(&right.id()))
    });
    let document = Document {
        source_revision: active.pair().source().canonical(),
        catalogue_revision: active.pair().catalogue().canonical(),
        functions: functions
            .into_iter()
            .map(|function| FunctionDocument {
                qualified_name: function.name().parts().to_vec(),
                function_id: function.id().canonical(),
                parameters: function
                    .parameters()
                    .iter()
                    .map(|parameter| ParameterDocument {
                        name: parameter.name().to_owned(),
                        parameter_id: parameter.id().canonical(),
                    })
                    .collect(),
            })
            .collect(),
    };
    let mut bytes = serde_json::to_vec(&document).map_err(|error| {
        SqliteBackendError::new(format!(
            "orna: source apply could not construct its result: {error}"
        ))
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn map_sqlite_error(error: SqliteError) -> SqliteBackendError {
    SqliteBackendError::new(format!("orna: local SQLite backend error: {error}"))
}

fn map_storage_error(error: StorageError<SqliteError>) -> SqliteBackendError {
    match error {
        StorageError::Backend(error) => map_sqlite_error(error),
        StorageError::InvalidRequest(error) => {
            SqliteBackendError::new(format!("orna: local SQLite request was rejected: {error}"))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "orna-sqlite-source-{label}-{}-{}",
                std::process::id(),
                NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&path).expect("create SQLite backend test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn fresh_invalid_apply_does_not_create_database() {
        let directory = TestDirectory::new("invalid-apply");
        let database = directory.path().join("fresh.sqlite");
        let source = directory.path().join("invalid.orna");
        fs::write(&source, b"CREATE SCHEMA ;\n").expect("write invalid source");

        let outcome = run_sqlite_source_apply(&database, source.to_str().unwrap())
            .expect("invalid source should return compiler diagnostics");
        assert!(matches!(
            outcome,
            SqliteSourceApplyOutcome::Diagnostics { .. }
        ));
        assert!(
            !database.exists(),
            "diagnostic-only fresh apply must not create SQLite state"
        );
    }

    #[test]
    fn source_diff_rejects_unsupported_physical_candidate() {
        let directory = TestDirectory::new("unsupported-diff");
        let database = directory.path().join("database.sqlite");
        let valid_source = directory.path().join("valid.orna");
        fs::write(
            &valid_source,
            b"CREATE SCHEMA app;\n\
              CREATE TYPE app.item AS OBJECT (value INTEGER NOT NULL);",
        )
        .expect("write valid source");
        let applied = run_sqlite_source_apply(&database, valid_source.to_str().unwrap())
            .expect("valid source apply");
        assert!(matches!(applied, SqliteSourceApplyOutcome::Applied(_)));

        let unsupported_source = directory.path().join("unsupported.orna");
        fs::write(
            &unsupported_source,
            b"CREATE SCHEMA unsupported;\n\
              CREATE TYPE unsupported.kind AS ENUM ('one');",
        )
        .expect("write unsupported source");
        let error = run_sqlite_source_diff(&database, unsupported_source.to_str().unwrap())
            .expect_err("unsupported SQLite physical candidate must fail closed");
        assert!(
            error.to_string().contains("enum type"),
            "unsupported diff error should identify the rejected capability: {error}"
        );
    }
}
