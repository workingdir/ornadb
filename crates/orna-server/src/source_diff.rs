//! Installed read-only semantic source diff (work ADR 0066).
//!
//! [`run_installed_source_diff`] checks one application source file against
//! the fixed private instance, prepares its candidate revision without
//! applying it, and renders the semantic changes between the candidate and
//! the active catalogue. The command never writes a standard stream, never
//! installs a candidate, and never changes the active revision pair.

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
    catalogue_diff::{CatalogueSemanticDiff, SemanticChange},
    revision::VerifiedStandardLibrarySnapshot,
    source::{SourceBundle, SourceBundleError, SourceUnit},
};
use orna_postgres::{PostgresKernel, PostgresKernelError};
use orna_standard::{
    StandardLibraryError, retained_standard_library_snapshot, verify_standard_library_snapshot,
};

use crate::{EmbeddedHostError, inspect_ready_embedded_host};

/// The closed result of one installed read-only source diff.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledSourceDiffOutcome {
    /// The source had compiler diagnostics; no candidate was prepared.
    Diagnostics(InstalledSourceDiffDiagnostics),
    /// The candidate was prepared and its semantic diff is ready.
    Diff(InstalledSourceDiffReport),
}

/// Ordered compiler diagnostics rendered with the source-check contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledSourceDiffDiagnostics {
    bytes: Vec<u8>,
}

impl InstalledSourceDiffDiagnostics {
    /// Returns the exact diagnostic lines, including their final line feeds.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A prepared-but-not-applied semantic diff report (work ADR 0066).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledSourceDiffReport {
    bytes: Vec<u8>,
}

impl InstalledSourceDiffReport {
    /// Returns the rendered diff document and its one final line feed.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Writes the complete diff document. A write failure cannot change the
    /// active revision pair because the command never applies anything.
    pub fn write_to(&self, output: &mut impl Write) -> Result<(), InstalledSourceDiffError> {
        output
            .write_all(&self.bytes)
            .and_then(|()| output.flush())
            .map_err(|source| InstalledSourceDiffError::Output { source })
    }
}

/// The closed host failure class for an installed source diff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InstalledSourceDiffHostFailure {
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

/// A failure before or during one installed read-only source diff.
#[derive(Debug)]
#[non_exhaustive]
pub enum InstalledSourceDiffError {
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
        /// The closed host failure class.
        failure: InstalledSourceDiffHostFailure,
        /// The private failure detail.
        source: Box<EmbeddedHostError>,
    },
    /// The private database kernel could not be attached.
    Attach {
        /// The kernel configuration or connection failure.
        source: PostgresKernelError,
    },
    /// The active revision or standard library could not be recovered.
    Recovery {
        /// The private recovery failure.
        source: PostgresKernelError,
    },
    /// The retained standard snapshot could not be verified.
    StandardLibrary {
        /// The standard-library failure.
        source: StandardLibraryError,
    },
    /// The retained standard source did not check cleanly.
    StandardSource {
        /// The standard-source check failure.
        source: StandardLibraryCheckError,
    },
    /// The standard application context could not be established.
    ApplicationContext {
        /// The context construction failure.
        source: StandardApplicationContextError,
    },
    /// The retained standard snapshot does not match the installed one.
    ActiveStandardMismatch,
    /// The candidate could not be prepared.
    Preparation {
        /// The candidate preparation failure.
        source: PrepareStandardApplicationError,
    },
    /// The rendered diff document could not reach standard output.
    Output {
        /// The output write failure.
        source: io::Error,
    },
    /// The private asynchronous runtime could not be created.
    Runtime {
        /// The runtime construction failure.
        source: io::Error,
    },
}

impl fmt::Display for InstalledSourceDiffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceRead { path, source } => match source {
                Some(cause) => write!(
                    formatter,
                    "orna source diff: could not read {path:?}: {cause}"
                ),
                None => write!(
                    formatter,
                    "orna source diff: {path:?} is not a regular file"
                ),
            },
            Self::SourceUtf8 { path } => {
                write!(formatter, "orna source diff: {path:?} is not valid UTF-8")
            }
            Self::SourceBundle { source } => {
                write!(
                    formatter,
                    "orna source diff: source bundle invalid: {source}"
                )
            }
            Self::Host { failure, source } => {
                write!(formatter, "orna source diff: {failure:?}: {source}")
            }
            Self::Attach { source } => {
                write!(formatter, "orna source diff: could not attach: {source}")
            }
            Self::Recovery { source } => {
                write!(formatter, "orna source diff: recovery failed: {source}")
            }
            Self::StandardLibrary { source } => {
                write!(
                    formatter,
                    "orna source diff: standard library failed: {source}"
                )
            }
            Self::StandardSource { source } => {
                write!(
                    formatter,
                    "orna source diff: standard source check failed: {source}"
                )
            }
            Self::ApplicationContext { source } => {
                write!(
                    formatter,
                    "orna source diff: application context failed: {source}"
                )
            }
            Self::Preparation { source } => {
                write!(
                    formatter,
                    "orna source diff: candidate preparation failed: {source}"
                )
            }
            Self::ActiveStandardMismatch => write!(
                formatter,
                "orna source diff: the installed standard library does not match the retained snapshot"
            ),
            Self::Output { source } => {
                write!(
                    formatter,
                    "orna source diff: could not write the report: {source}"
                )
            }
            Self::Runtime { source } => {
                write!(
                    formatter,
                    "orna source diff: the private runtime could not start: {source}"
                )
            }
        }
    }
}

impl Error for InstalledSourceDiffError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SourceRead {
                source: Some(source),
                ..
            } => Some(source),
            Self::SourceBundle { source } => Some(source),
            Self::Host { source, .. } => Some(source),
            Self::Attach { source } | Self::Recovery { source } => Some(source),
            Self::StandardLibrary { source } => Some(source),
            Self::StandardSource { source } => Some(source),
            Self::ApplicationContext { source } => Some(source),
            Self::Preparation { source } => Some(source),
            Self::Output { source } | Self::Runtime { source } => Some(source),
            Self::SourceRead { source: None, .. }
            | Self::SourceUtf8 { .. }
            | Self::ActiveStandardMismatch => None,
        }
    }
}

/// Checks one file and renders the semantic diff against the active revision.
///
/// The host inspection retains the package and instance guards for the
/// complete recovery and preparation path. The report is written to
/// `stdout`; compiler diagnostics are returned to the caller for the
/// standard-source channel.
pub fn run_installed_source_diff(
    path: &str,
) -> Result<InstalledSourceDiffOutcome, InstalledSourceDiffError> {
    let bundle = read_source_bundle(path)?;
    let host = inspect_ready_embedded_host().map_err(map_host_error)?;
    let kernel = PostgresKernel::new(host.config().clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| InstalledSourceDiffError::Runtime { source })?;
    runtime.block_on(diff_source_bundle(kernel, bundle))
}

async fn diff_source_bundle(
    kernel: PostgresKernel,
    bundle: SourceBundle,
) -> Result<InstalledSourceDiffOutcome, InstalledSourceDiffError> {
    let active = kernel.recover().await.map_err(map_recovery_error)?;
    let accepted = retained_standard_library_snapshot()
        .and_then(verify_standard_library_snapshot)
        .map_err(|source| InstalledSourceDiffError::StandardLibrary { source })?;
    require_accepted_active_standard(&active, &accepted)?;
    let standard = check_standard_library_source(&accepted)
        .map_err(|source| InstalledSourceDiffError::StandardSource { source })?;
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard)
        .map_err(|source| InstalledSourceDiffError::ApplicationContext { source })?;
    let report = check_standard_application(&bundle, &context);

    if !report.diagnostics().is_empty() {
        return Ok(InstalledSourceDiffOutcome::Diagnostics(
            InstalledSourceDiffDiagnostics {
                bytes: render_diagnostics(report.diagnostics()),
            },
        ));
    }

    let candidate = prepare_standard_application(&report, active.pair(), &active)
        .map_err(|source| InstalledSourceDiffError::Preparation { source })?;
    let diff = orna_core::catalogue_diff::catalogue_diff(active.catalogue(), candidate.candidate());
    let bytes = render_diff_document(&active, &candidate, &diff)?;
    Ok(InstalledSourceDiffOutcome::Diff(
        InstalledSourceDiffReport { bytes },
    ))
}

fn render_diff_document(
    active: &orna_core::revision::ActiveDatabaseRevision,
    candidate: &orna_core::revision::DeployableRevision,
    diff: &CatalogueSemanticDiff,
) -> Result<Vec<u8>, InstalledSourceDiffError> {
    use std::fmt::Write as _;

    let mut document = String::new();
    let _ = writeln!(
        document,
        "semantic diff {} -> {}",
        active.pair().source().canonical(),
        candidate.candidate_pair().source().canonical(),
    );
    if diff.is_empty() {
        let _ = writeln!(document, "no semantic changes");
        let mut bytes = document.into_bytes();
        bytes.push(b'\n');
        return Ok(bytes);
    }
    for change in diff.changes() {
        let _ = writeln!(document, "{}", render_change(change));
    }
    let mut bytes = document.into_bytes();
    bytes.push(b'\n');
    Ok(bytes)
}

fn render_change(change: &SemanticChange) -> String {
    use std::fmt::Write as _;

    let mut line = String::new();
    match change {
        SemanticChange::SchemaAdded { name, .. } => {
            let _ = write!(line, "+ schema {name}");
        }
        SemanticChange::SchemaDropped { name, .. } => {
            let _ = write!(line, "- schema {name}");
        }
        SemanticChange::SchemaRenamed { from, to, .. } => {
            let _ = write!(line, "~ schema {from} -> {to}");
        }
        SemanticChange::ObjectTypeAdded { name, .. } => {
            let _ = write!(line, "+ object type {name}");
        }
        SemanticChange::ObjectTypeDropped { name, .. } => {
            let _ = write!(line, "- object type {name}");
        }
        SemanticChange::ObjectTypeRenamed { from, to, .. } => {
            let _ = write!(line, "~ object type {from} -> {to}");
        }
        SemanticChange::FieldAdded {
            owner, name, id, ..
        } => {
            let _ = write!(line, "+ field {owner:?}.{name} [{id:?}]");
        }
        SemanticChange::FieldDropped {
            owner, name, id, ..
        } => {
            let _ = write!(line, "- field {owner:?}.{name} [{id:?}]");
        }
        SemanticChange::FieldRenamed {
            owner, from, to, ..
        } => {
            let _ = write!(line, "~ field {owner:?}.{from} -> {to}");
        }
        SemanticChange::EnumTypeAdded { name, .. } => {
            let _ = write!(line, "+ enum type {name}");
        }
        SemanticChange::EnumTypeDropped { name, .. } => {
            let _ = write!(line, "- enum type {name}");
        }
        SemanticChange::EnumTypeRenamed { from, to, .. } => {
            let _ = write!(line, "~ enum type {from} -> {to}");
        }
        SemanticChange::FunctionAdded { name, .. } => {
            let _ = write!(line, "+ function {name}");
        }
        SemanticChange::FunctionDropped { name, .. } => {
            let _ = write!(line, "- function {name}");
        }
        SemanticChange::FunctionRenamed { from, to, .. } => {
            let _ = write!(line, "~ function {from} -> {to}");
        }
        SemanticChange::ParameterAdded {
            owner, name, id, ..
        } => {
            let _ = write!(line, "+ parameter {owner:?}.{name} [{id:?}]");
        }
        SemanticChange::ParameterDropped {
            owner, name, id, ..
        } => {
            let _ = write!(line, "- parameter {owner:?}.{name} [{id:?}]");
        }
        SemanticChange::ParameterRenamed {
            owner, from, to, ..
        } => {
            let _ = write!(line, "~ parameter {owner:?}.{from} -> {to}");
        }
        _ => {}
    }
    line
}

fn require_accepted_active_standard(
    active: &orna_core::revision::ActiveDatabaseRevision,
    accepted: &VerifiedStandardLibrarySnapshot,
) -> Result<(), InstalledSourceDiffError> {
    let Some(installed) = active.catalogue_hash_context().standard() else {
        return Err(InstalledSourceDiffError::ActiveStandardMismatch);
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
        return Err(InstalledSourceDiffError::ActiveStandardMismatch);
    }
    Ok(())
}

fn render_diagnostics(diagnostics: &[CompilerDiagnostic]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for diagnostic in diagnostics {
        bytes.extend_from_slice(render_diagnostic(diagnostic).as_bytes());
    }
    bytes
}

fn render_diagnostic(diagnostic: &CompilerDiagnostic) -> String {
    let location = diagnostic.location();
    let span = location.span();
    format!(
        "{}:{}..{}: {}: {}\n",
        location.logical_path(),
        span.start(),
        span.end(),
        diagnostic.code().as_str(),
        escape_message(diagnostic.message()),
    )
}

fn escape_message(message: &str) -> String {
    let mut escaped = String::with_capacity(message.len());
    for character in message.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character => escaped.push(character),
        }
    }
    escaped
}

fn read_source_bundle(path: &str) -> Result<SourceBundle, InstalledSourceDiffError> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| InstalledSourceDiffError::SourceRead {
            path: path.to_owned(),
            source: Some(source),
        })?;
    if !file
        .metadata()
        .map_err(|source| InstalledSourceDiffError::SourceRead {
            path: path.to_owned(),
            source: Some(source),
        })?
        .is_file()
    {
        return Err(InstalledSourceDiffError::SourceRead {
            path: path.to_owned(),
            source: None,
        });
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| InstalledSourceDiffError::SourceRead {
            path: path.to_owned(),
            source: Some(source),
        })?;
    let source = String::from_utf8(bytes).map_err(|_| InstalledSourceDiffError::SourceUtf8 {
        path: path.to_owned(),
    })?;
    SourceBundle::new([SourceUnit::new(path, source)])
        .map_err(|source| InstalledSourceDiffError::SourceBundle { source })
}

fn map_host_error(source: EmbeddedHostError) -> InstalledSourceDiffError {
    let failure = match &source {
        EmbeddedHostError::InvalidServiceIdentity => {
            InstalledSourceDiffHostFailure::ServiceAccountRequired
        }
        EmbeddedHostError::InvalidPackageState => InstalledSourceDiffHostFailure::PackageIncomplete,
        EmbeddedHostError::Engine(_)
        | EmbeddedHostError::InvalidEngineManifest
        | EmbeddedHostError::InvalidDistributionManifest => {
            InstalledSourceDiffHostFailure::EngineInvalid
        }
        EmbeddedHostError::Io(error) if error.kind() == io::ErrorKind::NotFound => {
            InstalledSourceDiffHostFailure::InstanceNotInstalled
        }
        _ => InstalledSourceDiffHostFailure::InstanceInvalid,
    };
    InstalledSourceDiffError::Host {
        failure,
        source: Box::new(source),
    }
}

fn map_recovery_error(source: PostgresKernelError) -> InstalledSourceDiffError {
    match source {
        source @ (PostgresKernelError::Configuration(_)
        | PostgresKernelError::Database(_)
        | PostgresKernelError::DriverTask(_)) => InstalledSourceDiffError::Attach { source },
        source => InstalledSourceDiffError::Recovery { source },
    }
}
