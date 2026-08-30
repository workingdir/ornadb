//! Direct local-file command support for the SQLite backend.

use std::{error::Error, fmt, fs, path::PathBuf};

use orna_compiler::{check, prepare};
use orna_core::{
    physical::PhysicalMigrationArtifact,
    revision::ActiveDatabaseRevision,
    security::CATALOGUE_HEALTH_FUNCTION_ID,
    source::{SourceBundle, SourceUnit},
};
use orna_sqlite::{SqliteConfig, SqliteError, SqliteRevisionStore};
use orna_storage::{ApplicationRevisionStore, StorageError};
use serde::Serialize;

use crate::{source_diagnostics, source_diff};

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
    match run_with_runtime(database_path.into(), bundle, SqliteCommand::Apply)? {
        SqliteCommandOutcome::Apply(outcome) => Ok(outcome),
        SqliteCommandOutcome::Diff(_) => Err(SqliteBackendError::new(
            "orna: local SQLite backend returned the wrong source result",
        )),
    }
}

/// Runs `source diff` directly against a local SQLite path.
pub fn run_sqlite_source_diff(
    database_path: impl Into<PathBuf>,
    source_path: &str,
) -> Result<SqliteSourceDiffOutcome, SqliteBackendError> {
    let source = read_source(source_path)?;
    let bundle = SourceBundle::new([SourceUnit::new(source_path, source)])
        .map_err(SqliteBackendError::from_error)?;
    match run_with_runtime(database_path.into(), bundle, SqliteCommand::Diff)? {
        SqliteCommandOutcome::Apply(_) => Err(SqliteBackendError::new(
            "orna: local SQLite backend returned the wrong source result",
        )),
        SqliteCommandOutcome::Diff(outcome) => Ok(outcome),
    }
}

fn read_source(path: &str) -> Result<String, SqliteBackendError> {
    let bytes = fs::read(path).map_err(|error| {
        SqliteBackendError::new(format!("orna: could not read source file: {error}"))
    })?;
    String::from_utf8(bytes)
        .map_err(|_| SqliteBackendError::new("orna: source file is not valid UTF-8"))
}

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
    let store = SqliteRevisionStore::open(&SqliteConfig::new(database_path))
        .await
        .map_err(map_sqlite_error)?;
    ApplicationRevisionStore::bootstrap(&store)
        .await
        .map_err(map_storage_error)?;
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
    let candidate = prepare(&report, active.pair(), &active).map_err(|error| {
        SqliteBackendError::new(format!(
            "orna: source apply could not prepare the source: {error}"
        ))
    })?;
    let artifact =
        PhysicalMigrationArtifact::from_revisions(&active, &candidate).map_err(|error| {
            SqliteBackendError::new(format!(
                "orna: source apply could not prepare the source: {error}"
            ))
        })?;

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
