//! Installed one-file source checking, preparation, and atomic activation.

use std::{
    error::Error,
    fmt, fs,
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
};

use orna_compiler::{
    CompilerDiagnostic, PrepareStandardApplicationError, StandardApplicationCheckContext,
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
    StandardLibraryError, retained_standard_library_snapshot, verify_standard_library_snapshot,
};
use serde::Serialize;

use crate::{EmbeddedHostError, inspect_ready_embedded_host};

/// The result of checking and applying one installed application source file.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledSourceApplyOutcome {
    /// The source had compiler diagnostics and no candidate was prepared or applied.
    Diagnostics(InstalledSourceApplyDiagnostics),
    /// The exact candidate committed and its complete discovery document is ready.
    Applied(InstalledSourceApplySuccess),
}

/// Ordered compiler diagnostics rendered with the source-check contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledSourceApplyDiagnostics {
    bytes: Vec<u8>,
}

impl InstalledSourceApplyDiagnostics {
    /// Returns the exact diagnostic lines, including their final line feeds.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
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
    /// The process is not the installed Orna service account.
    ServiceAccountRequired,
    /// Package maintenance is incomplete or excludes new readers.
    PackageIncomplete,
    /// The default managed instance is absent.
    InstanceNotInstalled,
    /// The default managed instance or its readiness evidence is invalid.
    InstanceInvalid,
    /// The running executable cannot verify the installed embedded engine.
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
            Self::SourceRead { path, .. } => {
                write!(formatter, "orna: could not read source file: {path}")
            }
            Self::SourceUtf8 { path } => {
                write!(formatter, "orna: source file is not valid UTF-8: {path}")
            }
            Self::SourceBundle { .. } => {
                formatter.write_str("orna: source apply received an invalid source path")
            }
            Self::Host { failure, .. } => formatter.write_str(match failure {
                InstalledSourceApplyHostFailure::ServiceAccountRequired => {
                    "orna: source apply must run as the orna service account"
                }
                InstalledSourceApplyHostFailure::PackageIncomplete => {
                    "orna: package maintenance is incomplete"
                }
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
    let bundle = read_source_bundle(path)?;
    let host = inspect_ready_embedded_host().map_err(map_host_error)?;
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
    let accepted = retained_standard_library_snapshot()
        .and_then(verify_standard_library_snapshot)
        .map_err(|source| InstalledSourceApplyError::StandardLibrary { source })?;
    require_accepted_active_standard(&active, &accepted)?;
    let standard = check_standard_library_source(&accepted)
        .map_err(|source| InstalledSourceApplyError::StandardSource { source })?;
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard)
        .map_err(|source| InstalledSourceApplyError::ApplicationContext { source })?;
    let report = check_standard_application(&bundle, &context);

    if !report.diagnostics().is_empty() {
        return Ok(InstalledSourceApplyOutcome::Diagnostics(
            InstalledSourceApplyDiagnostics {
                bytes: render_diagnostics(report.diagnostics()),
            },
        ));
    }

    let candidate = prepare_standard_application(&report, active.pair(), &active)
        .map_err(|source| InstalledSourceApplyError::Preparation { source })?;
    let expected_pair = candidate.candidate_pair();
    let document = build_success_document(expected_pair, candidate.candidate())?;
    let committed = kernel.apply(&candidate).await.map_err(map_apply_error)?;
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

fn map_host_error(source: EmbeddedHostError) -> InstalledSourceApplyError {
    let failure = match &source {
        EmbeddedHostError::InvalidServiceIdentity => {
            InstalledSourceApplyHostFailure::ServiceAccountRequired
        }
        EmbeddedHostError::InvalidPackageState => {
            InstalledSourceApplyHostFailure::PackageIncomplete
        }
        EmbeddedHostError::Engine(_)
        | EmbeddedHostError::InvalidEngineManifest
        | EmbeddedHostError::InvalidDistributionManifest => {
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
        source @ (PostgresKernelError::Configuration(_)
        | PostgresKernelError::Database(_)
        | PostgresKernelError::DriverTask(_)) => InstalledSourceApplyError::Attach { source },
        source => InstalledSourceApplyError::Recovery { source },
    }
}

fn map_apply_error(source: PostgresKernelError) -> InstalledSourceApplyError {
    match source {
        PostgresKernelError::ExpectedBaseMismatch { expected, active } => {
            InstalledSourceApplyError::ExpectedBaseMismatch { expected, active }
        }
        source @ PostgresKernelError::DriverTask(_) => {
            InstalledSourceApplyError::SessionClose { source }
        }
        PostgresKernelError::CatalogueInvariant(
            "post-apply recovery must exactly reproduce the candidate hashes",
        ) => InstalledSourceApplyError::RecoveryMismatch,
        source => InstalledSourceApplyError::Apply { source },
    }
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
}

fn build_success_document(
    pair: RevisionPair,
    catalogue: &orna_core::catalogue::CatalogueSnapshot,
) -> Result<InstalledSourceApplySuccess, InstalledSourceApplyError> {
    let mut functions = catalogue
        .functions()
        .iter()
        .filter(|function| function.id() != CATALOGUE_HEALTH_FUNCTION_ID)
        .map(|function| (function.name().parts().to_vec(), function.id()))
        .collect::<Vec<_>>();
    functions.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let functions = functions
        .into_iter()
        .map(
            |(qualified_name, function_id): (Vec<String>, FunctionId)| SuccessFunction {
                qualified_name,
                function_id: function_id.canonical(),
            },
        )
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

fn render_diagnostics(diagnostics: &[CompilerDiagnostic]) -> Vec<u8> {
    let mut output = Vec::new();
    for diagnostic in diagnostics {
        let location = diagnostic.location();
        let span = location.span();
        let _ = writeln!(
            output,
            "{}:{}..{}: {}: {}",
            location.logical_path(),
            span.start(),
            span.end(),
            diagnostic.code().as_str(),
            escape_message(diagnostic.message()),
        );
    }
    output
}

fn escape_message(message: &str) -> String {
    let mut escaped = String::with_capacity(message.len());
    for character in message.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{2028}' | '\u{2029}' => {
                use fmt::Write as _;
                let _ = write!(escaped, "\\u{{{:04X}}}", character as u32);
            }
            character if character.is_control() => {
                use fmt::Write as _;
                let _ = write!(escaped, "\\u{{{:04X}}}", character as u32);
            }
            character => escaped.push(character),
        }
    }
    escaped
}
