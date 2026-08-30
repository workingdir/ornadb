// Source apply returns the stable embedded-host error boundary.
#![allow(clippy::result_large_err)]
#![allow(clippy::type_complexity)]
//! Installed one-file source checking, preparation, and atomic activation.

use std::{
    error::Error,
    fmt, fs,
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
};

use orna_compiler::{
    PrepareStandardApplicationError, StandardApplicationCheckContext,
    StandardApplicationContextError, StandardLibraryCheckError, check_standard_application,
    check_standard_library_source, prepare_standard_application,
};
use orna_core::{
    FunctionId,
    revision::{ActiveDatabaseRevision, RevisionPair, VerifiedStandardLibrarySnapshot},
    security::CATALOGUE_HEALTH_FUNCTION_ID,
    source::{SourceBundle, SourceBundleError, SourceUnit},
};
use orna_postgres::{PostgresKernel, PostgresKernelError};
use orna_standard::{
    STANDARD_LIBRARY_REVISION_ID, STANDARD_LIBRARY_V2_REVISION_ID, STANDARD_LIBRARY_V3_REVISION_ID,
    STANDARD_LIBRARY_V4_REVISION_ID, STANDARD_LIBRARY_V5_REVISION_ID,
    STANDARD_LIBRARY_V6_REVISION_ID, STANDARD_LIBRARY_V7_REVISION_ID,
    STANDARD_LIBRARY_V8_REVISION_ID, STANDARD_LIBRARY_V9_REVISION_ID,
    STANDARD_LIBRARY_V10_REVISION_ID, STANDARD_LIBRARY_V11_REVISION_ID, StandardLibraryError,
    retained_standard_library_snapshot, retained_standard_library_v2_snapshot,
    retained_standard_library_v3_snapshot, retained_standard_library_v4_snapshot,
    retained_standard_library_v5_snapshot, retained_standard_library_v6_snapshot,
    retained_standard_library_v7_snapshot, retained_standard_library_v8_snapshot,
    retained_standard_library_v9_snapshot, retained_standard_library_v10_snapshot,
    retained_standard_library_v11_snapshot, verify_standard_library_snapshot,
    verify_standard_library_v2_snapshot, verify_standard_library_v3_snapshot,
    verify_standard_library_v4_snapshot, verify_standard_library_v5_snapshot,
    verify_standard_library_v6_snapshot, verify_standard_library_v7_snapshot,
    verify_standard_library_v8_snapshot, verify_standard_library_v9_snapshot,
    verify_standard_library_v10_snapshot, verify_standard_library_v11_snapshot,
};
use serde::Serialize;

use crate::{EmbeddedHostError, inspect_current_embedded_host, source_diagnostics};

/// The result of checking and applying one local application source file.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledSourceApplyOutcome {
    /// The source had compiler diagnostics and no candidate was prepared or applied.
    Diagnostics(InstalledSourceApplyDiagnostics),
    /// The exact candidate committed and its complete discovery document is ready.
    Applied(InstalledSourceApplySuccess),
}

/// Ordered compiler diagnostics rendered for machine and terminal output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledSourceApplyDiagnostics {
    bytes: Vec<u8>,
    human_bytes: Vec<u8>,
    coloured_bytes: Vec<u8>,
}

impl InstalledSourceApplyDiagnostics {
    /// Returns the exact diagnostic lines, including their final line feeds.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the source-context diagnostic report without terminal colour.
    pub fn human_bytes(&self) -> &[u8] {
        &self.human_bytes
    }

    /// Returns the source-context diagnostic report with terminal colour.
    pub fn coloured_bytes(&self) -> &[u8] {
        &self.coloured_bytes
    }
}

/// A committed source-apply discovery document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledSourceApplySuccess {
    bytes: Vec<u8>,
}

impl InstalledSourceApplySuccess {
    /// Returns the compact success JSON and its one final line feed.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Writes the complete success document after the apply has committed.
    ///
    /// A write failure cannot roll back the committed database transaction.
    pub fn write_to(&self, output: &mut impl Write) -> Result<(), InstalledSourceApplyError> {
        output
            .write_all(&self.bytes)
            .and_then(|()| output.flush())
            .map_err(|source| InstalledSourceApplyError::Output { source })
    }
}

/// The closed host failure class for an installed source apply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledSourceApplyHostFailure {
    /// The default local instance is absent.
    InstanceNotInstalled,
    /// The default local instance or its readiness evidence is invalid.
    InstanceInvalid,
    /// The running executable cannot verify the local embedded engine.
    EngineInvalid,
}

/// A failure before or after one installed source apply transaction.
#[derive(Debug)]
#[non_exhaustive]
pub enum InstalledSourceApplyError {
    /// The source path did not open and read as one complete regular file.
    SourceRead {
        /// The exact submitted logical path.
        path: String,
        /// The private file-access failure, when available.
        source: Option<io::Error>,
    },
    /// The exact source bytes were not valid UTF-8.
    SourceUtf8 {
        /// The exact submitted logical path.
        path: String,
    },
    /// The single source unit could not form a source bundle.
    SourceBundle {
        /// The source-bundle validation failure.
        source: SourceBundleError,
    },
    /// Installed ready-host verification failed before attachment.
    Host {
        /// The closed public host failure class.
        failure: InstalledSourceApplyHostFailure,
        /// The private host verification failure.
        source: Box<EmbeddedHostError>,
    },
    /// The command could not attach to the fixed private database.
    Attach {
        /// The private kernel failure.
        source: PostgresKernelError,
    },
    /// The active database revision could not be recovered.
    Recovery {
        /// The private recovery failure.
        source: PostgresKernelError,
    },
    /// The retained accepted standard library could not be reconstructed or verified.
    StandardLibrary {
        /// The retained standard-library failure.
        source: StandardLibraryError,
    },
    /// The active standard context did not equal the accepted retained context.
    ActiveStandardMismatch,
    /// The retained standard source could not be checked against its verified snapshot.
    StandardSource {
        /// The standard source-checking failure.
        source: StandardLibraryCheckError,
    },
    /// The active application catalogue could not form checked-standard context.
    ApplicationContext {
        /// The application-context failure.
        source: StandardApplicationContextError,
    },
    /// The checked source could not be prepared as one complete candidate.
    Preparation {
        /// The compiler preparation failure.
        source: PrepareStandardApplicationError,
    },
    /// A concurrent source apply committed a different active base first.
    ExpectedBaseMismatch {
        /// The source and catalogue pair carried by this candidate.
        expected: RevisionPair,
        /// The source and catalogue pair locked by the apply transaction.
        active: RevisionPair,
    },
    /// The kernel could not atomically apply and confirm the candidate.
    Apply {
        /// The private kernel failure.
        source: PostgresKernelError,
    },
    /// PostgreSQL confirmed the operation but its connection task did not close cleanly.
    SessionClose {
        /// The private connection-task failure.
        source: PostgresKernelError,
    },
    /// The post-apply active revision did not reproduce the prepared candidate.
    RecoveryMismatch,
    /// The deterministic success document could not be constructed before mutation.
    ResultDocument {
        /// The JSON construction failure.
        source: serde_json::Error,
    },
    /// The committed success document could not be written completely.
    Output {
        /// The output descriptor failure.
        source: io::Error,
    },
    /// The private asynchronous runtime could not be created.
    Runtime {
        /// The runtime construction failure.
        source: io::Error,
    },
}

impl fmt::Display for InstalledSourceApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceRead { .. } => formatter.write_str("orna: could not read source file"),
            Self::SourceUtf8 { .. } => formatter.write_str("orna: source file is not valid UTF-8"),
            Self::SourceBundle { .. } => {
                formatter.write_str("orna: source apply received an invalid source path")
            }
            Self::Host { failure, .. } => formatter.write_str(match failure {
                InstalledSourceApplyHostFailure::InstanceNotInstalled => {
                    "orna: the default Orna instance is not installed"
                }
                InstalledSourceApplyHostFailure::InstanceInvalid => {
                    "orna: the default Orna instance is invalid"
                }
                InstalledSourceApplyHostFailure::EngineInvalid => {
                    "orna: the embedded PostgreSQL engine is not valid"
                }
            }),
            Self::Attach { .. } => formatter
                .write_str("orna: source apply could not attach to the default Orna instance"),
            Self::Recovery { .. } => {
                formatter.write_str("orna: source apply could not recover the active revision")
            }
            Self::StandardLibrary { .. }
            | Self::ActiveStandardMismatch
            | Self::StandardSource { .. }
            | Self::ApplicationContext { .. } => {
                formatter.write_str("orna: embedded standard library could not be verified")
            }
            Self::Preparation { .. } => {
                formatter.write_str("orna: source apply could not prepare the source")
            }
            Self::ExpectedBaseMismatch { expected, active } => write!(
                formatter,
                "orna: source apply expected {} {} but active is {} {}",
                expected.source(),
                expected.catalogue(),
                active.source(),
                active.catalogue(),
            ),
            Self::Apply { .. } => formatter.write_str("orna: source apply did not commit"),
            Self::SessionClose { .. } => {
                formatter.write_str("orna: source apply database session did not close cleanly")
            }
            Self::RecoveryMismatch => {
                formatter.write_str("orna: source apply could not verify the committed revision")
            }
            Self::ResultDocument { .. } => {
                formatter.write_str("orna: source apply could not construct its result")
            }
            Self::Output { .. } => {
                formatter.write_str("orna: source apply committed but could not write its result")
            }
            Self::Runtime { .. } => {
                formatter.write_str("orna: source apply runtime could not start")
            }
        }
    }
}

impl Error for InstalledSourceApplyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SourceRead {
                source: Some(source),
                ..
            } => Some(source),
            Self::SourceBundle { source } => Some(source),
            Self::Host { source, .. } => Some(source.as_ref()),
            Self::Attach { source }
            | Self::Recovery { source }
            | Self::Apply { source }
            | Self::SessionClose { source } => Some(source),
            Self::StandardLibrary { source } => Some(source),
            Self::StandardSource { source } => Some(source),
            Self::ApplicationContext { source } => Some(source),
            Self::Preparation { source } => Some(source),
            Self::ResultDocument { source } => Some(source),
            Self::Output { source } | Self::Runtime { source } => Some(source),
            Self::SourceRead { source: None, .. }
            | Self::SourceUtf8 { .. }
            | Self::ActiveStandardMismatch
            | Self::ExpectedBaseMismatch { .. }
            | Self::RecoveryMismatch => None,
        }
    }
}

/// Checks, prepares, and atomically applies one complete source file to the installed database.
///
/// This function reads the file before it inspects or attaches to the installed host. It retains
/// the ready-host locks until the apply has committed or failed. It returns output bytes but does
/// not write a standard stream.
pub fn run_installed_source_apply(
    path: &str,
) -> Result<InstalledSourceApplyOutcome, InstalledSourceApplyError> {
    validate_source_path(path)?;
    let bundle = read_source_bundle(path)?;
    let host = inspect_current_embedded_host().map_err(map_host_error)?;
    let kernel = PostgresKernel::new(host.config().clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| InstalledSourceApplyError::Runtime { source })?;

    runtime.block_on(apply_source_bundle(kernel, bundle))
}

async fn apply_source_bundle(
    kernel: PostgresKernel,
    bundle: SourceBundle,
) -> Result<InstalledSourceApplyOutcome, InstalledSourceApplyError> {
    let active = kernel.recover().await.map_err(map_recovery_error)?;
    let installed = active
        .catalogue_hash_context()
        .standard()
        .ok_or(InstalledSourceApplyError::ActiveStandardMismatch)?;
    let accepted = select_accepted_standard(installed).map_err(|error| match error {
        StandardSelectionError::UnknownRevision => {
            InstalledSourceApplyError::ActiveStandardMismatch
        }
        StandardSelectionError::Verification(source) => {
            InstalledSourceApplyError::StandardLibrary { source }
        }
    })?;
    require_accepted_active_standard(&active, &accepted)?;
    let standard = check_standard_library_source(&accepted)
        .map_err(|source| InstalledSourceApplyError::StandardSource { source })?;
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard)
        .map_err(|source| InstalledSourceApplyError::ApplicationContext { source })?;
    let report = check_standard_application(&bundle, &context);

    if !report.diagnostics().is_empty() {
        return Ok(InstalledSourceApplyOutcome::Diagnostics(
            InstalledSourceApplyDiagnostics {
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
            },
        ));
    }

    let candidate = prepare_standard_application(&report, active.pair(), &active)
        .map_err(|source| InstalledSourceApplyError::Preparation { source })?;
    let expected_pair = candidate.candidate_pair();
    let document = build_success_document(expected_pair, candidate.candidate())?;
    let committed = kernel
        .apply_source_apply(&candidate)
        .await
        .map_err(map_apply_error)?;
    if committed.pair() != expected_pair
        || committed.source().bundle_hash() != candidate.source().bundle_hash()
        || committed.source().revision_hash() != candidate.source().revision_hash()
        || committed.catalogue_hash() != candidate.catalogue_hash()
    {
        return Err(InstalledSourceApplyError::RecoveryMismatch);
    }

    Ok(InstalledSourceApplyOutcome::Applied(document))
}

fn read_source_bundle(path: &str) -> Result<SourceBundle, InstalledSourceApplyError> {
    validate_source_path(path)?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| InstalledSourceApplyError::SourceRead {
            path: path.to_owned(),
            source: Some(source),
        })?;
    if !file
        .metadata()
        .map_err(|source| InstalledSourceApplyError::SourceRead {
            path: path.to_owned(),
            source: Some(source),
        })?
        .is_file()
    {
        return Err(InstalledSourceApplyError::SourceRead {
            path: path.to_owned(),
            source: None,
        });
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| InstalledSourceApplyError::SourceRead {
            path: path.to_owned(),
            source: Some(source),
        })?;
    let source = String::from_utf8(bytes).map_err(|_| InstalledSourceApplyError::SourceUtf8 {
        path: path.to_owned(),
    })?;
    SourceBundle::new([SourceUnit::new(path, source)])
        .map_err(|source| InstalledSourceApplyError::SourceBundle { source })
}

fn validate_source_path(path: &str) -> Result<(), InstalledSourceApplyError> {
    if path.is_empty()
        || path.starts_with('-')
        || path
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
    {
        return Err(InstalledSourceApplyError::SourceBundle {
            source: SourceBundleError::EmptyLogicalPath { index: 0 },
        });
    }

    Ok(())
}

fn map_host_error(source: EmbeddedHostError) -> InstalledSourceApplyError {
    let failure = match &source {
        EmbeddedHostError::Engine(_) | EmbeddedHostError::InvalidEngineManifest => {
            InstalledSourceApplyHostFailure::EngineInvalid
        }
        EmbeddedHostError::Io(error) if error.kind() == io::ErrorKind::NotFound => {
            InstalledSourceApplyHostFailure::InstanceNotInstalled
        }
        _ => InstalledSourceApplyHostFailure::InstanceInvalid,
    };
    InstalledSourceApplyError::Host {
        failure,
        source: Box::new(source),
    }
}

fn map_recovery_error(source: PostgresKernelError) -> InstalledSourceApplyError {
    match source {
        source @ (PostgresKernelError::Configuration(_) | PostgresKernelError::Database(_)) => {
            InstalledSourceApplyError::Attach { source }
        }
        source @ (PostgresKernelError::DriverTask(_) | PostgresKernelError::SessionClose(_)) => {
            InstalledSourceApplyError::SessionClose { source }
        }
        source @ PostgresKernelError::RecoveryDatabase(_) => {
            InstalledSourceApplyError::Recovery { source }
        }
        source => InstalledSourceApplyError::Recovery { source },
    }
}

fn map_apply_error(source: PostgresKernelError) -> InstalledSourceApplyError {
    match source {
        PostgresKernelError::ExpectedBaseMismatch { expected, active } => {
            InstalledSourceApplyError::ExpectedBaseMismatch { expected, active }
        }
        source @ (PostgresKernelError::DriverTask(_) | PostgresKernelError::SessionClose(_)) => {
            InstalledSourceApplyError::SessionClose { source }
        }
        PostgresKernelError::CatalogueInvariant(
            "post-apply recovery must exactly reproduce the candidate hashes",
        ) => InstalledSourceApplyError::RecoveryMismatch,
        source => InstalledSourceApplyError::Apply { source },
    }
}

#[derive(Debug)]
pub(super) enum StandardSelectionError {
    UnknownRevision,
    Verification(StandardLibraryError),
}

pub(super) fn select_accepted_standard(
    installed: &VerifiedStandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, StandardSelectionError> {
    let accepted =
        match installed.revision() {
            STANDARD_LIBRARY_REVISION_ID => {
                retained_standard_library_snapshot().and_then(verify_standard_library_snapshot)
            }
            STANDARD_LIBRARY_V2_REVISION_ID => retained_standard_library_v2_snapshot()
                .and_then(verify_standard_library_v2_snapshot),
            STANDARD_LIBRARY_V3_REVISION_ID => retained_standard_library_v3_snapshot()
                .and_then(verify_standard_library_v3_snapshot),
            STANDARD_LIBRARY_V4_REVISION_ID => retained_standard_library_v4_snapshot()
                .and_then(verify_standard_library_v4_snapshot),
            STANDARD_LIBRARY_V5_REVISION_ID => retained_standard_library_v5_snapshot()
                .and_then(verify_standard_library_v5_snapshot),
            STANDARD_LIBRARY_V6_REVISION_ID => retained_standard_library_v6_snapshot()
                .and_then(verify_standard_library_v6_snapshot),
            STANDARD_LIBRARY_V7_REVISION_ID => retained_standard_library_v7_snapshot()
                .and_then(verify_standard_library_v7_snapshot),
            STANDARD_LIBRARY_V8_REVISION_ID => retained_standard_library_v8_snapshot()
                .and_then(verify_standard_library_v8_snapshot),
            STANDARD_LIBRARY_V9_REVISION_ID => retained_standard_library_v9_snapshot()
                .and_then(verify_standard_library_v9_snapshot),
            STANDARD_LIBRARY_V10_REVISION_ID => retained_standard_library_v10_snapshot()
                .and_then(verify_standard_library_v10_snapshot),
            STANDARD_LIBRARY_V11_REVISION_ID => retained_standard_library_v11_snapshot()
                .and_then(verify_standard_library_v11_snapshot),
            _ => return Err(StandardSelectionError::UnknownRevision),
        };

    accepted.map_err(StandardSelectionError::Verification)
}

fn require_accepted_active_standard(
    active: &ActiveDatabaseRevision,
    accepted: &VerifiedStandardLibrarySnapshot,
) -> Result<(), InstalledSourceApplyError> {
    let Some(installed) = active.catalogue_hash_context().standard() else {
        return Err(InstalledSourceApplyError::ActiveStandardMismatch);
    };
    let installed_source = installed.source();
    let accepted_source = accepted.source();
    if installed.revision() != accepted.revision()
        || installed.catalogue().revision() != accepted.catalogue().revision()
        || installed_source.bundle() != accepted_source.bundle()
        || installed_source.id() != accepted_source.id()
        || installed_source.bundle_hash() != accepted_source.bundle_hash()
        || installed_source.revision_hash() != accepted_source.revision_hash()
        || installed.digest() != accepted.digest()
    {
        return Err(InstalledSourceApplyError::ActiveStandardMismatch);
    }
    Ok(())
}

#[derive(Serialize)]
struct SuccessDocument {
    source_revision: String,
    catalogue_revision: String,
    functions: Vec<SuccessFunction>,
}

#[derive(Serialize)]
struct SuccessFunction {
    qualified_name: Vec<String>,
    function_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    parameters: Vec<SuccessParameter>,
}

#[derive(Serialize)]
struct SuccessParameter {
    name: String,
    parameter_id: String,
}

fn build_success_document(
    pair: RevisionPair,
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
) -> Result<InstalledSourceApplySuccess, InstalledSourceApplyError> {
    let mut functions = catalogue
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
    let functions = functions
        .into_iter()
        .map(|function| {
            let function_id: FunctionId = function.id();
            SuccessFunction {
                qualified_name: function.name().parts().to_vec(),
                function_id: function_id.canonical(),
                parameters: function
                    .parameters()
                    .iter()
                    .map(|parameter| SuccessParameter {
                        name: parameter.name().to_owned(),
                        parameter_id: parameter.id().canonical(),
                    })
                    .collect(),
            }
        })
        .collect();
    let document = SuccessDocument {
        source_revision: pair.source().canonical(),
        catalogue_revision: pair.catalogue().canonical(),
        functions,
    };
    let mut bytes = serde_json::to_vec(&document)
        .map_err(|source| InstalledSourceApplyError::ResultDocument { source })?;
    bytes.push(b'\n');
    Ok(InstalledSourceApplySuccess { bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn,
        FunctionReturnColumnDefinition, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        ParameterDefinition, QualifiedSemanticName, SchemaDefinition,
    };
    use orna_core::types::{ResolvedType, StandardScalar};
    use orna_core::{
        CatalogueRevisionId, FunctionRevisionId, ParameterId, SchemaId, SourceRevisionId,
    };

    const PARAMETERISED_FUNCTION_ID: FunctionId = FunctionId::from_bytes([0x41; 16]);
    const EMPTY_FUNCTION_ID: FunctionId = FunctionId::from_bytes([0x5a; 16]);
    const FIRST_PARAMETER_ID: ParameterId = ParameterId::from_bytes([0x12; 16]);
    const SECOND_PARAMETER_ID: ParameterId = ParameterId::from_bytes([0x11; 16]);

    fn schema(id: u8, parts: &[&str]) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::from_bytes([id; 16]),
            QualifiedSemanticName::new(parts.iter().copied()).expect("valid schema name"),
        )
    }

    fn parameter(id: ParameterId, name: &str, ordinal: u32) -> ParameterDefinition {
        ParameterDefinition::new(
            id,
            name,
            ordinal,
            ResolvedType::scalar(StandardScalar::Boolean),
            None,
        )
    }

    fn function(
        id: FunctionId,
        parts: &[&str],
        parameters: Vec<ParameterDefinition>,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            id,
            QualifiedSemanticName::new(parts.iter().copied()).expect("valid function name"),
            FunctionDomain::Server,
            parameters,
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "created",
                0,
                ResolvedType::scalar(StandardScalar::Boolean),
            )]),
            FunctionRevisionId::from_bytes([0x21; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    }

    fn catalogue(revision: CatalogueRevisionId) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_functions(
            revision,
            vec![schema(0x61, &["a"]), schema(0x7a, &["z"])],
            vec![],
            vec![
                // Supplied in reverse semantic-name order on purpose.
                function(EMPTY_FUNCTION_ID, &["z", "empty"], vec![]),
                function(
                    PARAMETERISED_FUNCTION_ID,
                    &["a", "parameterised"],
                    vec![
                        parameter(FIRST_PARAMETER_ID, "first", 0),
                        parameter(SECOND_PARAMETER_ID, "second", 1),
                    ],
                ),
                // The catalogue health identity must never appear in discovery.
                function(CATALOGUE_HEALTH_FUNCTION_ID, &["a", "health"], vec![]),
            ],
        )
        .expect("catalogue must validate")
    }

    fn assert_selected_standard_matches(installed: &VerifiedStandardLibrarySnapshot) {
        let selected = select_accepted_standard(installed).expect("accepted standard snapshot");
        assert_eq!(selected.revision(), installed.revision());
        assert_eq!(
            selected.catalogue().revision(),
            installed.catalogue().revision()
        );
        assert_eq!(selected.source().bundle(), installed.source().bundle());
        assert_eq!(selected.source().id(), installed.source().id());
        assert_eq!(
            selected.source().bundle_hash(),
            installed.source().bundle_hash()
        );
        assert_eq!(
            selected.source().revision_hash(),
            installed.source().revision_hash()
        );
        assert_eq!(selected.digest(), installed.digest());
    }

    #[test]
    fn source_input_errors_redact_paths_and_source_bytes() {
        let source_read = InstalledSourceApplyError::SourceRead {
            path: "/private/secret-source-bytes.orna".to_owned(),
            source: Some(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "secret source bytes",
            )),
        };
        assert_eq!(source_read.to_string(), "orna: could not read source file");
        assert!(
            !source_read
                .to_string()
                .contains("/private/secret-source-bytes.orna")
        );
        assert!(!source_read.to_string().contains("secret source bytes"));

        let source_utf8 = InstalledSourceApplyError::SourceUtf8 {
            path: "/private/secret-source-bytes.orna".to_owned(),
        };
        assert_eq!(
            source_utf8.to_string(),
            "orna: source file is not valid UTF-8"
        );
        assert!(
            !source_utf8
                .to_string()
                .contains("/private/secret-source-bytes.orna")
        );
        assert!(!source_utf8.to_string().contains("secret source bytes"));
    }

    #[test]
    fn selects_the_accepted_v1_snapshot_for_a_v1_active_standard() {
        let installed = verify_standard_library_snapshot(
            retained_standard_library_snapshot().expect("retained V1 standard"),
        )
        .expect("verified V1 standard");
        assert_selected_standard_matches(&installed);
    }

    #[test]
    fn selects_the_accepted_v6_snapshot_for_a_v6_active_standard() {
        let installed = verify_standard_library_v6_snapshot(
            retained_standard_library_v6_snapshot().expect("retained V6 standard"),
        )
        .expect("verified V6 standard");
        assert_selected_standard_matches(&installed);
        let name = QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("V6 name");
        assert!(
            installed.catalogue().function_by_name(&name).is_some(),
            "V6 active standard must expose std.invoke.echo"
        );
    }

    #[test]
    fn selects_the_accepted_v7_snapshot_for_a_v7_active_standard() {
        let installed = verify_standard_library_v7_snapshot(
            retained_standard_library_v7_snapshot().expect("retained V7 standard"),
        )
        .expect("verified V7 standard");
        assert_selected_standard_matches(&installed);
    }

    #[test]
    fn selects_the_accepted_v8_snapshot_for_a_v8_active_standard() {
        let installed = verify_standard_library_v8_snapshot(
            retained_standard_library_v8_snapshot().expect("retained V8 standard"),
        )
        .expect("verified V8 standard");
        assert_selected_standard_matches(&installed);
    }

    #[test]
    fn selects_the_accepted_v9_snapshot_for_a_v9_active_standard() {
        let installed = verify_standard_library_v9_snapshot(
            retained_standard_library_v9_snapshot().expect("retained V9 standard"),
        )
        .expect("verified V9 standard");
        assert_selected_standard_matches(&installed);
        let name = QualifiedSemanticName::new(["std", "ui", "text"]).expect("V9 name");
        assert!(
            installed.catalogue().function_by_name(&name).is_some(),
            "V9 active standard must expose std.ui.text"
        );
    }

    #[test]
    fn selects_the_accepted_v10_snapshot_for_a_v10_active_standard() {
        let installed = verify_standard_library_v10_snapshot(
            retained_standard_library_v10_snapshot().expect("retained V10 standard"),
        )
        .expect("verified V10 standard");
        let selected = select_accepted_standard(&installed).expect("accepted V10 standard");
        assert_eq!(selected.revision(), installed.revision());
        assert_eq!(
            selected.catalogue().revision(),
            installed.catalogue().revision()
        );
        let name = QualifiedSemanticName::new(["std", "cli", "repl"]).expect("V10 name");
        assert!(
            selected.catalogue().function_by_name(&name).is_some(),
            "V10 active standard must expose std.cli.repl"
        );
    }

    #[test]
    fn selects_the_accepted_v2_through_v5_snapshots() {
        let snapshots = [
            retained_standard_library_v2_snapshot()
                .and_then(verify_standard_library_v2_snapshot)
                .expect("verified V2 standard"),
            retained_standard_library_v3_snapshot()
                .and_then(verify_standard_library_v3_snapshot)
                .expect("verified V3 standard"),
            retained_standard_library_v4_snapshot()
                .and_then(verify_standard_library_v4_snapshot)
                .expect("verified V4 standard"),
            retained_standard_library_v5_snapshot()
                .and_then(verify_standard_library_v5_snapshot)
                .expect("verified V5 standard"),
        ];

        for installed in &snapshots {
            assert_selected_standard_matches(installed);
        }
    }

    fn split_document(bytes: &[u8]) -> (&[u8], &[u8]) {
        let marker = b"\"functions\":[";
        let start = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("success document must carry the functions array");
        (&bytes[..start], &bytes[start..])
    }

    #[test]
    fn post_apply_recovery_invariant_fails_closed_as_recovery_mismatch() {
        let error = map_apply_error(PostgresKernelError::CatalogueInvariant(
            "post-apply recovery must exactly reproduce the candidate hashes",
        ));

        assert!(matches!(error, InstalledSourceApplyError::RecoveryMismatch));
    }

    #[test]
    fn host_error_mapping_uses_closed_public_classes_and_redacts_private_details() {
        let cases: [(
            &str,
            fn() -> EmbeddedHostError,
            InstalledSourceApplyHostFailure,
            &str,
            &str,
        ); 4] = [
            (
                "missing instance",
                || {
                    EmbeddedHostError::Io(io::Error::new(
                        io::ErrorKind::NotFound,
                        "private instance path",
                    ))
                },
                InstalledSourceApplyHostFailure::InstanceNotInstalled,
                "orna: the default Orna instance is not installed",
                "private instance path",
            ),
            (
                "engine manifest",
                || EmbeddedHostError::InvalidEngineManifest,
                InstalledSourceApplyHostFailure::EngineInvalid,
                "orna: the embedded PostgreSQL engine is not valid",
                "embedded PostgreSQL engine manifest is invalid",
            ),
            (
                "engine adapter",
                || EmbeddedHostError::Engine(orna_postgres::EngineError::InvalidArgument),
                InstalledSourceApplyHostFailure::EngineInvalid,
                "orna: the embedded PostgreSQL engine is not valid",
                "embedded PostgreSQL argument is not a C string",
            ),
            (
                "instance verification",
                || EmbeddedHostError::Lifecycle {
                    primary: Box::new(EmbeddedHostError::InvalidInstanceState),
                    cleanup: Box::new(EmbeddedHostError::SupportMismatch("private host detail")),
                },
                InstalledSourceApplyHostFailure::InstanceInvalid,
                "orna: the default Orna instance is invalid",
                "private host detail",
            ),
        ];

        for (name, build, failure, public, private) in cases {
            let error = map_host_error(build());
            assert!(
                matches!(&error, InstalledSourceApplyError::Host { failure: actual, .. } if *actual == failure),
                "{name} must map to its closed host class: {error:?}"
            );
            assert_eq!(error.to_string(), public, "{name} public error changed");
            assert!(
                !error.to_string().contains(private),
                "{name} leaked private host detail"
            );
        }
    }

    #[test]
    fn apply_error_mapping_preserves_stale_identity_and_redacts_private_failures() {
        let expected = RevisionPair::new(
            SourceRevisionId::from_bytes([0x31; 16]),
            CatalogueRevisionId::from_bytes([0x32; 16]),
        );
        let active = RevisionPair::new(
            SourceRevisionId::from_bytes([0x41; 16]),
            CatalogueRevisionId::from_bytes([0x42; 16]),
        );
        let stale = map_apply_error(PostgresKernelError::ExpectedBaseMismatch { expected, active });
        assert!(matches!(
            &stale,
            InstalledSourceApplyError::ExpectedBaseMismatch {
                expected: actual_expected,
                active: actual_active,
            } if *actual_expected == expected && *actual_active == active
        ));
        assert_eq!(
            stale.to_string(),
            format!(
                "orna: source apply expected {} {} but active is {} {}",
                expected.source(),
                expected.catalogue(),
                active.source(),
                active.catalogue(),
            )
        );

        let cases: [(&str, fn() -> PostgresKernelError, &str, &str); 3] = [
            (
                "generic apply",
                || PostgresKernelError::CatalogueInvariant("private apply detail"),
                "orna: source apply did not commit",
                "private apply detail",
            ),
            (
                "session close",
                || {
                    let source = "port=invalid"
                        .parse::<tokio_postgres::Config>()
                        .expect_err("invalid port must produce a PostgreSQL error");
                    PostgresKernelError::SessionClose(source)
                },
                "orna: source apply database session did not close cleanly",
                "invalid port",
            ),
            (
                "driver task",
                || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("test runtime must build");
                    let task = runtime.spawn(async {});
                    task.abort();
                    let source = runtime
                        .block_on(task)
                        .expect_err("aborted task must produce a JoinError");
                    PostgresKernelError::DriverTask(source)
                },
                "orna: source apply database session did not close cleanly",
                "aborted task",
            ),
        ];
        for (name, build, public, private) in cases {
            let error = map_apply_error(build());
            assert!(
                matches!(
                    &error,
                    InstalledSourceApplyError::Apply { .. }
                        | InstalledSourceApplyError::SessionClose { .. }
                ),
                "{name} must map to a closed apply error: {error:?}"
            );
            assert_eq!(error.to_string(), public, "{name} public error changed");
            assert!(
                !error.to_string().contains(private),
                "{name} leaked private detail"
            );
        }
    }

    #[test]
    fn recovery_database_maps_to_recovery_stage() {
        let source = "port=invalid"
            .parse::<tokio_postgres::Config>()
            .expect_err("invalid port must produce a PostgreSQL error");
        let error = map_recovery_error(PostgresKernelError::RecoveryDatabase(source));

        assert!(matches!(
            &error,
            &InstalledSourceApplyError::Recovery { .. }
        ));
        assert_eq!(
            error.to_string(),
            "orna: source apply could not recover the active revision"
        );
    }

    #[test]
    fn initial_recovery_driver_task_maps_to_session_close() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime must build");
        let task = runtime.spawn(async {});
        task.abort();
        let source = runtime
            .block_on(task)
            .expect_err("aborted task must produce a JoinError");
        let error = map_recovery_error(PostgresKernelError::DriverTask(source));

        assert!(matches!(
            &error,
            &InstalledSourceApplyError::SessionClose { .. }
        ));
        assert_eq!(
            error.to_string(),
            "orna: source apply database session did not close cleanly"
        );
    }

    #[test]
    fn shutdown_database_failures_map_to_session_close_stage() {
        let recovery_source = "port=invalid"
            .parse::<tokio_postgres::Config>()
            .expect_err("invalid port must produce a PostgreSQL error");
        let recovery = map_recovery_error(PostgresKernelError::SessionClose(recovery_source));
        assert!(matches!(
            recovery,
            InstalledSourceApplyError::SessionClose { .. }
        ));

        let apply_source = "port=invalid"
            .parse::<tokio_postgres::Config>()
            .expect_err("invalid port must produce a PostgreSQL error");
        let apply = map_apply_error(PostgresKernelError::SessionClose(apply_source));
        assert!(matches!(
            apply,
            InstalledSourceApplyError::SessionClose { .. }
        ));
    }

    #[test]
    fn success_document_sorts_functions_and_renders_ordered_parameters() {
        let source_revision = SourceRevisionId::from_bytes([0x31; 16]);
        let catalogue_revision = CatalogueRevisionId::from_bytes([0x32; 16]);
        let pair = RevisionPair::new(source_revision, catalogue_revision);
        let document = build_success_document(pair, &catalogue(catalogue_revision))
            .expect("success document must build");
        let expected = format!(
            "{{\"source_revision\":\"{}\",\"catalogue_revision\":\"{}\",\"functions\":[{{\"qualified_name\":[\"a\",\"parameterised\"],\"function_id\":\"{}\",\"parameters\":[{{\"name\":\"first\",\"parameter_id\":\"{}\"}},{{\"name\":\"second\",\"parameter_id\":\"{}\"}}]}},{{\"qualified_name\":[\"z\",\"empty\"],\"function_id\":\"{}\"}}]}}\n",
            source_revision.canonical(),
            catalogue_revision.canonical(),
            PARAMETERISED_FUNCTION_ID.canonical(),
            FIRST_PARAMETER_ID.canonical(),
            SECOND_PARAMETER_ID.canonical(),
            EMPTY_FUNCTION_ID.canonical(),
        );
        assert_eq!(document.as_bytes(), expected.as_bytes());
    }

    #[test]
    fn success_document_replay_keeps_the_function_discovery_suffix() {
        let first_revision = CatalogueRevisionId::from_bytes([0x32; 16]);
        let first = build_success_document(
            RevisionPair::new(SourceRevisionId::from_bytes([0x31; 16]), first_revision),
            &catalogue(first_revision),
        )
        .expect("first success document must build");
        let second_revision = CatalogueRevisionId::from_bytes([0x42; 16]);
        let second = build_success_document(
            RevisionPair::new(SourceRevisionId::from_bytes([0x41; 16]), second_revision),
            &catalogue(second_revision),
        )
        .expect("second success document must build");
        let (first_prefix, first_suffix) = split_document(first.as_bytes());
        let (second_prefix, second_suffix) = split_document(second.as_bytes());
        assert_ne!(first_prefix, second_prefix, "revision prefix must advance");
        assert_eq!(
            first_suffix, second_suffix,
            "the function discovery suffix must stay byte-identical"
        );
        assert!(
            second_prefix
                .windows(second_revision.canonical().len())
                .any(|window| window == second_revision.canonical().as_bytes()),
            "the second prefix must carry the exact new catalogue revision"
        );
        assert!(
            !second_prefix
                .windows(first_revision.canonical().len())
                .any(|window| window == first_revision.canonical().as_bytes()),
            "the second prefix must not carry the old catalogue revision"
        );
    }
    fn assert_invalid_source_path_fails_before_io(path: &str) {
        let error = run_installed_source_apply(path)
            .expect_err("invalid source path must fail before filesystem or host access");
        assert!(
            matches!(
                &error,
                InstalledSourceApplyError::SourceBundle {
                    source: SourceBundleError::EmptyLogicalPath { index: 0 }
                }
            ),
            "invalid path {path:?} must use the closed source-bundle boundary: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "orna: source apply received an invalid source path"
        );
    }

    #[test]
    fn invalid_source_paths_fail_before_missing_file_or_host_access() {
        for path in [
            "",
            "-leading.orna",
            "line\nbreak.orna",
            "line\u{2028}break.orna",
            "line\u{2029}break.orna",
        ] {
            assert_invalid_source_path_fails_before_io(path);
        }
    }

    #[test]
    fn valid_relative_source_path_reaches_file_read_boundary() {
        let path = format!(
            "./-orna-source-apply-valid-path-{}-missing.orna",
            std::process::id()
        );
        let error = run_installed_source_apply(&path)
            .expect_err("the missing valid path must fail at source read before host access");
        assert!(
            matches!(
                &error,
                InstalledSourceApplyError::SourceRead {
                    path: submitted,
                    source: Some(source),
                } if submitted == &path && source.kind() == io::ErrorKind::NotFound
            ),
            "valid path must reach filesystem read: {error:?}"
        );
    }
}
