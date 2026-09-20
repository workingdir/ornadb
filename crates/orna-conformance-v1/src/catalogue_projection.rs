//! Exact compiler-to-runtime source catalogue projection.
//!
//! This module deliberately consumes only facts retained by
//! `ResolvedSourceCatalogue`; it never derives identities from source text or
//! allocates runtime object identifiers.

use orna_compiler::ResolvedSourceCatalogue;
use orna_foundation_v1::CwdCapture;
use orna_runtime_v1::{
    CatalogueAdmission, CatalogueError, RequestActivationCommit, RuntimeState,
    TableActivationError, ValidatedTableRequestActivationCommit,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogueProjectionError {
    TypeWitness(String),
    MissingType(String),
    UnsupportedType { function: String, type_name: String },
    UnsupportedReturn { function: String },
    ParameterDefault { function: String, parameter: String },
    MissingFunctionRevision(String),
    Producer(String),
    RuntimeAdmission(CatalogueError),
}

#[derive(Debug)]
pub enum SourceCatalogueActivationError {
    Projection(CatalogueProjectionError),
    Runtime(TableActivationError),
}

impl From<CatalogueProjectionError> for SourceCatalogueActivationError {
    fn from(error: CatalogueProjectionError) -> Self {
        Self::Projection(error)
    }
}

impl From<TableActivationError> for SourceCatalogueActivationError {
    fn from(error: TableActivationError) -> Self {
        Self::Runtime(error)
    }
}

/// Projects a compiler candidate into the runtime's source-only admission.
///
/// `predecessor_capture` is caller-supplied and passed through unchanged,
/// including `None` for the first catalogue admission. The
/// projection is closed over witnessed object types and newly compiled
/// functions; scalar, value, stream, row, default, missing-standard, and
/// unwitnessed forms fail closed.
pub fn project_source_catalogue(
    catalogue: &ResolvedSourceCatalogue,
    predecessor_capture: Option<CwdCapture>,
) -> Result<CatalogueAdmission, CatalogueProjectionError> {
    let artifact = catalogue
        .admission_artifact()
        .map_err(|error| CatalogueProjectionError::Producer(error.to_string()))?;
    CatalogueAdmission::from_artifact(&artifact, predecessor_capture)
        .map_err(CatalogueProjectionError::RuntimeAdmission)
}

/// Projects and atomically commits one resolved source catalogue as part of
/// an already-admitted request activation.
///
/// The request carries the caller's writer, request, activation, staged table
/// mutations, validator, terminal outcome, and fault policy. This bridge only
/// supplies the source catalogue admission and binds its predecessor to the
/// exact activation capture; it does not derive request, object, reference,
/// or type evidence. Unsupported source facts remain projection errors.
pub async fn commit_resolved_source_catalogue_activation(
    runtime: &RuntimeState,
    catalogue: &ResolvedSourceCatalogue,
    request: ValidatedTableRequestActivationCommit<'_>,
) -> Result<RequestActivationCommit, SourceCatalogueActivationError> {
    let admission = project_source_catalogue(catalogue, Some(request.context.capture().clone()))?;
    runtime
        .commit_validated_catalogue_table_request_activation(request, &admission)
        .await
        .map_err(SourceCatalogueActivationError::Runtime)
}
