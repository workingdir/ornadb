//! Installed read-only semantic source diff (work ADR 0066).
//!
//! [`run_installed_source_diff`] checks one application source file against
//! the fixed private instance, prepares its candidate revision without
//! applying it, and renders semantic changes between the candidate and active
//! catalogues and their executable function revisions. The command never
//! writes a standard stream, never installs a candidate, and never changes the
//! active revision pair.

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
    catalogue_diff::{CatalogueSemanticDiff, SemanticChange},
    revision::VerifiedStandardLibrarySnapshot,
    source::{SourceBundle, SourceBundleError, SourceUnit},
};
use orna_postgres::{PostgresKernel, PostgresKernelError};
use orna_standard::StandardLibraryError;

use crate::{
    EmbeddedHostError, inspect_ready_embedded_host,
    source_apply::{StandardSelectionError, select_accepted_standard},
    source_diagnostics,
};

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
            Self::SourceRead { .. } => formatter.write_str("orna: could not read source file"),
            Self::SourceUtf8 { .. } => formatter.write_str("orna: source file is not valid UTF-8"),
            Self::SourceBundle { .. } => {
                formatter.write_str("orna: source diff received an invalid source path")
            }
            Self::Host { failure, .. } => formatter.write_str(match failure {
                InstalledSourceDiffHostFailure::ServiceAccountRequired => {
                    "orna: source diff must run as the orna service account"
                }
                InstalledSourceDiffHostFailure::PackageIncomplete => {
                    "orna: package maintenance is incomplete"
                }
                InstalledSourceDiffHostFailure::InstanceNotInstalled => {
                    "orna: the default Orna instance is not installed"
                }
                InstalledSourceDiffHostFailure::InstanceInvalid => {
                    "orna: the default Orna instance is invalid"
                }
                InstalledSourceDiffHostFailure::EngineInvalid => {
                    "orna: the embedded PostgreSQL engine is not valid"
                }
            }),
            Self::Attach { .. } => formatter
                .write_str("orna: source diff could not attach to the default Orna instance"),
            Self::Recovery { .. } => {
                formatter.write_str("orna: source diff could not recover the active revision")
            }
            Self::StandardLibrary { .. }
            | Self::StandardSource { .. }
            | Self::ApplicationContext { .. }
            | Self::ActiveStandardMismatch => {
                formatter.write_str("orna: embedded standard library could not be verified")
            }
            Self::Preparation { .. } => {
                formatter.write_str("orna: source diff could not prepare the source")
            }
            Self::Output { .. } => {
                formatter.write_str("orna: source diff could not write its report")
            }
            Self::Runtime { .. } => {
                formatter.write_str("orna: source diff runtime could not start")
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
    runtime.block_on(run_source_diff_with_kernel(kernel, bundle))
}

/// Runs one installed `orna source diff` against a caller-supplied kernel
/// (work ADR 0066 live-proof seam).
///
/// The public entry [`run_installed_source_diff`] inspects the fixed private
/// instance and delegates here; the live proof drives the exact
/// check-prepare-render path against the Compose PostgreSQL test kernel.
/// Public consumers keep [`run_installed_source_diff`]; this seam is hidden
/// from the documented API surface.
#[doc(hidden)]
pub async fn run_source_diff_with_kernel(
    kernel: PostgresKernel,
    bundle: SourceBundle,
) -> Result<InstalledSourceDiffOutcome, InstalledSourceDiffError> {
    diff_source_bundle(kernel, bundle).await
}

async fn diff_source_bundle(
    kernel: PostgresKernel,
    bundle: SourceBundle,
) -> Result<InstalledSourceDiffOutcome, InstalledSourceDiffError> {
    let active = kernel.recover().await.map_err(map_recovery_error)?;
    let installed = active
        .catalogue_hash_context()
        .standard()
        .ok_or(InstalledSourceDiffError::ActiveStandardMismatch)?;
    let accepted = select_accepted_standard(installed).map_err(|error| match error {
        StandardSelectionError::UnknownRevision => InstalledSourceDiffError::ActiveStandardMismatch,
        StandardSelectionError::Verification(source) => {
            InstalledSourceDiffError::StandardLibrary { source }
        }
    })?;
    require_accepted_active_standard(&active, &accepted)?;
    let standard = check_standard_library_source(&accepted)
        .map_err(|source| InstalledSourceDiffError::StandardSource { source })?;
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard)
        .map_err(|source| InstalledSourceDiffError::ApplicationContext { source })?;
    let report = check_standard_application(&bundle, &context);

    if !report.diagnostics().is_empty() {
        return Ok(InstalledSourceDiffOutcome::Diagnostics(
            InstalledSourceDiffDiagnostics {
                bytes: source_diagnostics::render_diagnostics(report.diagnostics()),
            },
        ));
    }

    let candidate = prepare_standard_application(&report, active.pair(), &active)
        .map_err(|source| InstalledSourceDiffError::Preparation { source })?;
    let diff = orna_core::catalogue_diff::catalogue_diff(active.catalogue(), candidate.candidate());
    let revision_changes = function_revision_changes(&active, &candidate);
    let bytes = render_diff_document(&active, &candidate, &diff, &revision_changes)?;
    Ok(InstalledSourceDiffOutcome::Diff(
        InstalledSourceDiffReport { bytes },
    ))
}

fn function_revision_changes(
    active: &orna_core::revision::ActiveDatabaseRevision,
    candidate: &orna_core::revision::DeployableRevision,
) -> Vec<FunctionRevisionChange> {
    let candidate_revisions = candidate
        .current_function_revisions()
        .unwrap_or_else(|| candidate.new_function_revisions());
    changed_function_revisions(active.function_revisions(), candidate_revisions)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FunctionRevisionChange {
    function: orna_core::FunctionId,
    old_id: orna_core::FunctionRevisionId,
    new_id: orna_core::FunctionRevisionId,
    old_semantic_hash: orna_core::revision::Sha256Digest,
    new_semantic_hash: orna_core::revision::Sha256Digest,
}

fn changed_function_revisions(
    active: &[orna_core::revision::FunctionRevisionRecord],
    candidate: &[orna_core::revision::FunctionRevisionRecord],
) -> Vec<FunctionRevisionChange> {
    let mut changes = Vec::new();
    for candidate_revision in candidate {
        let Some(active_revision) = active
            .iter()
            .find(|revision| revision.function() == candidate_revision.function())
        else {
            continue;
        };
        if active_revision.id() == candidate_revision.id()
            && active_revision.semantic_hash() == candidate_revision.semantic_hash()
        {
            continue;
        }
        changes.push(FunctionRevisionChange {
            function: candidate_revision.function(),
            old_id: active_revision.id(),
            new_id: candidate_revision.id(),
            old_semantic_hash: active_revision.semantic_hash(),
            new_semantic_hash: candidate_revision.semantic_hash(),
        });
    }
    changes.sort_unstable_by_key(|change| change.function);
    changes
}

fn render_function_revision_change(
    change: &FunctionRevisionChange,
    candidate: &orna_core::catalogue::CatalogueSnapshot,
) -> String {
    use std::fmt::Write as _;

    let name = candidate
        .function_by_id(change.function)
        .map(|definition| qualified(definition.name()))
        .unwrap_or_else(|| change.function.canonical());
    let mut line = String::new();
    let _ = write!(
        line,
        "! function {name} executable revision {} -> {} semantic hash {} -> {} [{}]",
        change.old_id.canonical(),
        change.new_id.canonical(),
        digest_hex(change.old_semantic_hash),
        digest_hex(change.new_semantic_hash),
        change.function.canonical(),
    );
    line
}

fn digest_hex(digest: orna_core::revision::Sha256Digest) -> String {
    use std::fmt::Write as _;

    let mut text = String::with_capacity(64);
    for byte in digest.to_bytes() {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn render_diff_document(
    active: &orna_core::revision::ActiveDatabaseRevision,
    candidate: &orna_core::revision::DeployableRevision,
    diff: &CatalogueSemanticDiff,
    revision_changes: &[FunctionRevisionChange],
) -> Result<Vec<u8>, InstalledSourceDiffError> {
    use std::fmt::Write as _;

    let mut document = String::new();
    let _ = writeln!(
        document,
        "semantic diff {} -> {}",
        active.pair().source().canonical(),
        candidate.candidate_pair().source().canonical(),
    );
    if diff.is_empty() && revision_changes.is_empty() {
        let _ = writeln!(document, "no semantic changes");
        return Ok(document.into_bytes());
    }
    for change in diff.changes() {
        let _ = writeln!(
            document,
            "{}",
            render_change_with_catalogues(change, Some(active.catalogue()), candidate.candidate()),
        );
    }
    for change in revision_changes {
        let _ = writeln!(
            document,
            "{}",
            render_function_revision_change(change, candidate.candidate())
        );
    }
    Ok(document.into_bytes())
}

fn render_change(
    change: &SemanticChange,
    candidate: &orna_core::catalogue::CatalogueSnapshot,
) -> String {
    render_change_with_catalogues(change, None, candidate)
}

fn render_change_with_catalogues(
    change: &SemanticChange,
    active: Option<&orna_core::catalogue::CatalogueSnapshot>,
    candidate: &orna_core::catalogue::CatalogueSnapshot,
) -> String {
    use std::fmt::Write as _;

    let mut line = String::new();
    match change {
        SemanticChange::SchemaAdded { id, name } => {
            let _ = write!(
                line,
                "+ schema {} [{}]",
                render_schema_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::SchemaDropped { id, name } => {
            let _ = write!(
                line,
                "- schema {} [{}]",
                render_schema_name(active.or(Some(candidate)), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::SchemaRenamed { id, from, to } => {
            let _ = write!(
                line,
                "~ schema {} -> {} [{}]",
                render_schema_name(active.or(Some(candidate)), *id, from),
                render_schema_name(Some(candidate), *id, to),
                id.canonical(),
            );
        }
        SemanticChange::ObjectTypeAdded { id, name } => {
            let _ = write!(
                line,
                "+ object type {} [{}]",
                render_type_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::ObjectTypeDropped { id, name } => {
            let _ = write!(
                line,
                "- object type {} [{}]",
                render_type_name(active.or(Some(candidate)), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::ObjectTypeRenamed { id, from, to } => {
            let _ = write!(
                line,
                "~ object type {} -> {} [{}]",
                render_type_name(active.or(Some(candidate)), *id, from),
                render_type_name(Some(candidate), *id, to),
                id.canonical(),
            );
        }
        SemanticChange::ValueTypeAdded { id, name } => {
            let _ = write!(
                line,
                "+ value type {} [{}]",
                render_type_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::ValueTypeDropped { id, name } => {
            let _ = write!(
                line,
                "- value type {} [{}]",
                render_type_name(active.or(Some(candidate)), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::ValueTypeRenamed { id, from, to } => {
            let _ = write!(
                line,
                "~ value type {} -> {} [{}]",
                render_type_name(active.or(Some(candidate)), *id, from),
                render_type_name(Some(candidate), *id, to),
                id.canonical(),
            );
        }
        SemanticChange::ValueTypeKindChanged { id, name } => {
            let _ = write!(
                line,
                "! value type {} kind [{}]",
                render_type_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::ValueTypeMutabilityChanged { id, name } => {
            let _ = write!(
                line,
                "! value type {} mutability [{}]",
                render_type_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::ValueTypePersistenceChanged { id, name } => {
            let _ = write!(
                line,
                "! value type {} persistence [{}]",
                render_type_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::ValueTypeRepresentationChanged { id, name } => {
            let _ = write!(
                line,
                "! value type {} representation [{}]",
                render_type_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::RecordValueTypeAdded { id, name } => {
            let _ = write!(
                line,
                "+ record value type {} [{}]",
                render_type_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::RecordValueTypeDropped { id, name } => {
            let _ = write!(
                line,
                "- record value type {} [{}]",
                render_type_name(active.or(Some(candidate)), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::RecordValueTypeRenamed { id, from, to } => {
            let _ = write!(
                line,
                "~ record value type {} -> {} [{}]",
                render_type_name(active.or(Some(candidate)), *id, from),
                render_type_name(Some(candidate), *id, to),
                id.canonical(),
            );
        }
        SemanticChange::FieldAdded { owner, name, id } => {
            let owner_name = render_field_owner_name(Some(candidate), *owner);
            let member_name = render_field_member_name(Some(candidate), *owner, *id, name);
            let _ = write!(
                line,
                "+ field {owner_name}.{member_name} [{}]",
                id.canonical()
            );
        }
        SemanticChange::FieldDropped { owner, name, id } => {
            let owner_name = render_field_owner_name(active.or(Some(candidate)), *owner);
            let member_name =
                render_field_member_name(active.or(Some(candidate)), *owner, *id, name);
            let _ = write!(
                line,
                "- field {owner_name}.{member_name} [{}]",
                id.canonical()
            );
        }
        SemanticChange::FieldRenamed {
            owner,
            id,
            from,
            to,
        } => {
            let from_owner = render_field_owner_name(active.or(Some(candidate)), *owner);
            let to_owner = render_field_owner_name(Some(candidate), *owner);
            let from_name = render_field_member_name(active.or(Some(candidate)), *owner, *id, from);
            let to_name = render_field_member_name(Some(candidate), *owner, *id, to);
            let _ = write!(
                line,
                "~ field {from_owner}.{from_name} -> {to_owner}.{to_name} [{}]",
                id.canonical(),
            );
        }
        SemanticChange::EnumTypeAdded { id, name } => {
            let _ = write!(
                line,
                "+ enum type {} [{}]",
                render_type_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::EnumTypeDropped { id, name } => {
            let _ = write!(
                line,
                "- enum type {} [{}]",
                render_type_name(active.or(Some(candidate)), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::EnumTypeRenamed { id, from, to } => {
            let _ = write!(
                line,
                "~ enum type {} -> {} [{}]",
                render_type_name(active.or(Some(candidate)), *id, from),
                render_type_name(Some(candidate), *id, to),
                id.canonical(),
            );
        }
        SemanticChange::FunctionAdded { id, name } => {
            let _ = write!(
                line,
                "+ function {} [{}]",
                render_function_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::FunctionDropped { id, name } => {
            let _ = write!(
                line,
                "- function {} [{}]",
                render_function_name(active.or(Some(candidate)), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::FunctionRenamed { id, from, to } => {
            let _ = write!(
                line,
                "~ function {} -> {} [{}]",
                render_function_name(active.or(Some(candidate)), *id, from),
                render_function_name(Some(candidate), *id, to),
                id.canonical(),
            );
        }
        SemanticChange::ParameterAdded { owner, name, id } => {
            let owner_name = render_parameter_owner_name(Some(candidate), *owner);
            let member_name = render_parameter_member_name(Some(candidate), *owner, *id, name);
            let _ = write!(
                line,
                "+ parameter {owner_name}.{member_name} [{}]",
                id.canonical(),
            );
        }
        SemanticChange::ParameterDropped { owner, name, id } => {
            let owner_name = render_parameter_owner_name(active.or(Some(candidate)), *owner);
            let member_name =
                render_parameter_member_name(active.or(Some(candidate)), *owner, *id, name);
            let _ = write!(
                line,
                "- parameter {owner_name}.{member_name} [{}]",
                id.canonical(),
            );
        }
        SemanticChange::ParameterRenamed {
            owner,
            id,
            from,
            to,
        } => {
            let from_owner = render_parameter_owner_name(active.or(Some(candidate)), *owner);
            let to_owner = render_parameter_owner_name(Some(candidate), *owner);
            let from_name =
                render_parameter_member_name(active.or(Some(candidate)), *owner, *id, from);
            let to_name = render_parameter_member_name(Some(candidate), *owner, *id, to);
            let _ = write!(
                line,
                "~ parameter {from_owner}.{from_name} -> {to_owner}.{to_name} [{}]",
                id.canonical(),
            );
        }
        SemanticChange::ParameterOrdinalChanged {
            owner,
            id,
            name,
            from,
            to,
        } => {
            let owner_name = render_parameter_owner_name(Some(candidate), *owner);
            let member_name = render_parameter_member_name(Some(candidate), *owner, *id, name);
            let _ = write!(
                line,
                "! parameter {owner_name}.{member_name} ordinal {from} -> {to} [{}]",
                id.canonical(),
            );
        }
        SemanticChange::FieldTypeChanged { owner, name, id } => {
            let owner_name = render_field_owner_name(Some(candidate), *owner);
            let member_name = render_field_member_name(Some(candidate), *owner, *id, name);
            let _ = write!(
                line,
                "! field {owner_name}.{member_name} type [{}]",
                id.canonical(),
            );
        }
        SemanticChange::FieldOrdinalChanged { owner, name, id } => {
            let owner_name = render_field_owner_name(Some(candidate), *owner);
            let member_name = render_field_member_name(Some(candidate), *owner, *id, name);
            let _ = write!(
                line,
                "! field {owner_name}.{member_name} ordinal [{}]",
                id.canonical(),
            );
        }
        SemanticChange::FieldNullabilityChanged { owner, name, id } => {
            let owner_name = render_field_owner_name(Some(candidate), *owner);
            let member_name = render_field_member_name(Some(candidate), *owner, *id, name);
            let _ = write!(
                line,
                "! field {owner_name}.{member_name} nullability [{}]",
                id.canonical(),
            );
        }
        SemanticChange::FieldUniquenessChanged { owner, name, id } => {
            let owner_name = render_field_owner_name(Some(candidate), *owner);
            let member_name = render_field_member_name(Some(candidate), *owner, *id, name);
            let _ = write!(
                line,
                "! field {owner_name}.{member_name} uniqueness [{}]",
                id.canonical(),
            );
        }
        SemanticChange::FieldConstraintChanged { owner, name, id } => {
            let owner_name = render_field_owner_name(Some(candidate), *owner);
            let member_name = render_field_member_name(Some(candidate), *owner, *id, name);
            let _ = write!(
                line,
                "! field {owner_name}.{member_name} default/on-delete [{}]",
                id.canonical(),
            );
        }
        SemanticChange::EnumLabelsChanged { id, name } => {
            let _ = write!(
                line,
                "! enum type {} labels [{}]",
                render_type_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::FunctionReturnChanged { id, name } => {
            let _ = write!(
                line,
                "! function {} return type [{}]",
                render_function_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::FunctionDomainChanged { id, name } => {
            let _ = write!(
                line,
                "! function {} domain [{}]",
                render_function_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::FunctionSecurityChanged { id, name } => {
            let _ = write!(
                line,
                "! function {} security [{}]",
                render_function_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::FunctionTransactionChanged { id, name } => {
            let _ = write!(
                line,
                "! function {} transaction [{}]",
                render_function_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::FunctionVolatilityChanged { id, name } => {
            let _ = write!(
                line,
                "! function {} volatility [{}]",
                render_function_name(Some(candidate), *id, name),
                id.canonical(),
            );
        }
        SemanticChange::ParameterTypeChanged { owner, name, id } => {
            let owner_name = render_parameter_owner_name(Some(candidate), *owner);
            let member_name = render_parameter_member_name(Some(candidate), *owner, *id, name);
            let _ = write!(
                line,
                "! parameter {owner_name}.{member_name} type [{}]",
                id.canonical(),
            );
        }
        SemanticChange::ParameterDefaultChanged { owner, name, id } => {
            let owner_name = render_parameter_owner_name(Some(candidate), *owner);
            let member_name = render_parameter_member_name(Some(candidate), *owner, *id, name);
            let _ = write!(
                line,
                "! parameter {owner_name}.{member_name} default [{}]",
                id.canonical(),
            );
        }
        change @ _ => {
            let _ = write!(line, "! unsupported change ({})", change.category());
        }
    }
    line
}

fn render_schema_name(
    catalogue: Option<&orna_core::catalogue::CatalogueSnapshot>,
    id: orna_core::SchemaId,
    fallback: &str,
) -> String {
    catalogue
        .and_then(|catalogue| catalogue.schema_by_id(id))
        .map(|definition| qualified(definition.name()))
        .unwrap_or_else(|| render_qualified_text(fallback))
}

fn render_type_name(
    catalogue: Option<&orna_core::catalogue::CatalogueSnapshot>,
    id: orna_core::TypeId,
    fallback: &str,
) -> String {
    catalogue
        .and_then(|catalogue| catalogue.type_definition_by_id(id))
        .map(|definition| qualified(definition.name()))
        .unwrap_or_else(|| render_qualified_text(fallback))
}

fn render_function_name(
    catalogue: Option<&orna_core::catalogue::CatalogueSnapshot>,
    id: orna_core::FunctionId,
    fallback: &str,
) -> String {
    catalogue
        .and_then(|catalogue| catalogue.function_by_id(id))
        .map(|definition| qualified(definition.name()))
        .unwrap_or_else(|| render_qualified_text(fallback))
}

fn render_field_owner_name(
    catalogue: Option<&orna_core::catalogue::CatalogueSnapshot>,
    owner: orna_core::TypeId,
) -> String {
    catalogue
        .and_then(|catalogue| {
            catalogue
                .object_type_by_id(owner)
                .map(|definition| qualified(definition.name()))
                .or_else(|| {
                    catalogue
                        .record_value_type_by_id(owner)
                        .map(|definition| qualified(definition.name()))
                })
        })
        .unwrap_or_else(|| owner.canonical())
}

fn render_field_member_name(
    catalogue: Option<&orna_core::catalogue::CatalogueSnapshot>,
    owner: orna_core::TypeId,
    id: orna_core::FieldId,
    fallback: &str,
) -> String {
    catalogue
        .and_then(|catalogue| {
            catalogue
                .object_type_by_id(owner)
                .and_then(|definition| definition.field_by_id(id))
                .map(|field| field.name())
                .or_else(|| {
                    catalogue
                        .record_value_type_by_id(owner)
                        .and_then(|definition| definition.field_by_id(id))
                        .map(|field| field.name())
                })
        })
        .map(render_identifier_part)
        .unwrap_or_else(|| render_identifier_part(fallback))
}

fn render_parameter_owner_name(
    catalogue: Option<&orna_core::catalogue::CatalogueSnapshot>,
    owner: orna_core::FunctionId,
) -> String {
    catalogue
        .and_then(|catalogue| catalogue.function_by_id(owner))
        .map(|definition| qualified(definition.name()))
        .unwrap_or_else(|| owner.canonical())
}

fn render_parameter_member_name(
    catalogue: Option<&orna_core::catalogue::CatalogueSnapshot>,
    owner: orna_core::FunctionId,
    id: orna_core::ParameterId,
    fallback: &str,
) -> String {
    catalogue
        .and_then(|catalogue| {
            catalogue
                .function_by_id(owner)
                .and_then(|definition| definition.parameter_by_id(id))
                .map(|parameter| parameter.name())
        })
        .map(render_identifier_part)
        .unwrap_or_else(|| render_identifier_part(fallback))
}

fn render_qualified_text(name: &str) -> String {
    name.split('.')
        .map(render_identifier_part)
        .collect::<Vec<_>>()
        .join(".")
}

/// Escapes one semantic name part for the human-readable report grammar.
///
/// Bare identifier parts retain the existing compact spelling. Other parts use
/// doubled quotes for embedded quotes and backslash escapes for controls so a
/// name can never introduce another physical report line.
fn render_identifier_part(part: &str) -> String {
    let mut characters = part.chars();
    let is_unquoted = characters
        .next()
        .is_some_and(|first| first == '_' || first.is_alphabetic())
        && characters.all(|character| {
            character == '_' || character.is_alphabetic() || character.is_numeric()
        });
    if is_unquoted {
        return part.to_owned();
    }

    use std::fmt::Write as _;

    let mut rendered = String::with_capacity(part.len() + 2);
    rendered.push('"');
    for character in part.chars() {
        match character {
            '"' => rendered.push_str("\"\""),
            '\\' => rendered.push_str("\\\\"),
            '\u{0008}' => rendered.push_str("\\b"),
            '\n' => rendered.push_str("\\n"),
            '\u{000c}' => rendered.push_str("\\f"),
            '\r' => rendered.push_str("\\r"),
            '\t' => rendered.push_str("\\t"),
            character if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') => {
                let _ = write!(rendered, "\\u{{{:04X}}}", character as u32);
            }
            character => rendered.push(character),
        }
    }
    rendered.push('"');
    rendered
}

fn qualified(name: &orna_core::catalogue::QualifiedSemanticName) -> String {
    name.parts()
        .iter()
        .map(|part| render_identifier_part(part))
        .collect::<Vec<_>>()
        .join(".")
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

fn read_source_bundle(path: &str) -> Result<SourceBundle, InstalledSourceDiffError> {
    validate_source_path(path)?;
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

fn validate_source_path(path: &str) -> Result<(), InstalledSourceDiffError> {
    if path.is_empty()
        || path.starts_with('-')
        || path
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
    {
        return Err(InstalledSourceDiffError::SourceBundle {
            source: SourceBundleError::EmptyLogicalPath { index: 0 },
        });
    }

    Ok(())
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
        source @ PostgresKernelError::RecoveryDatabase(_) => {
            InstalledSourceDiffError::Recovery { source }
        }
        source => InstalledSourceDiffError::Recovery { source },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FunctionRevisionChange, InstalledSourceDiffError, changed_function_revisions, digest_hex,
        map_recovery_error, qualified, read_source_bundle, render_change,
        render_change_with_catalogues, render_function_revision_change, render_identifier_part,
        run_installed_source_diff,
    };
    use orna_core::{
        CatalogueRevisionId, FieldId, FunctionId, FunctionRevisionId, SchemaId, SourceUnitId,
        TypeId,
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName,
            RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition,
        },
        catalogue_diff::SemanticChange,
        revision::{
            ExecutableArtifact, ExecutableArtifactKind, FunctionRevisionRecord, Sha256Digest,
            SourceOrigin,
        },
        source::SourceBundleError,
        types::TypeDescriptor,
    };
    use orna_postgres::PostgresKernelError;

    fn function_revision(
        function: FunctionId,
        revision: FunctionRevisionId,
        semantic_hash_byte: u8,
    ) -> FunctionRevisionRecord {
        let artifact = ExecutableArtifact::new(
            ExecutableArtifactKind::Server,
            "test.server-plan",
            1,
            vec![semantic_hash_byte],
            Sha256Digest::from_bytes([semantic_hash_byte; 32]),
        )
        .unwrap();
        FunctionRevisionRecord::new(
            function,
            revision,
            1,
            SourceOrigin::new(SourceUnitId::from_bytes([8; 16]), 0, 1).unwrap(),
            Sha256Digest::from_bytes([9; 32]),
            Sha256Digest::from_bytes([semantic_hash_byte; 32]),
            "orna.language/1",
            artifact,
        )
        .unwrap()
    }

    #[test]
    fn reports_body_only_revision_changes_in_function_id_order() {
        let first = FunctionId::from_bytes([1; 16]);
        let second = FunctionId::from_bytes([2; 16]);
        let first_old = function_revision(first, FunctionRevisionId::from_bytes([3; 16]), 4);
        let second_old = function_revision(second, FunctionRevisionId::from_bytes([5; 16]), 6);
        let first_new = function_revision(first, FunctionRevisionId::from_bytes([7; 16]), 8);
        let second_new = function_revision(second, FunctionRevisionId::from_bytes([9; 16]), 10);

        let changes = changed_function_revisions(
            &[second_old.clone(), first_old.clone()],
            &[second_new, first_new],
        );
        assert_eq!(
            changes,
            vec![
                FunctionRevisionChange {
                    function: first,
                    old_id: first_old.id(),
                    new_id: FunctionRevisionId::from_bytes([7; 16]),
                    old_semantic_hash: first_old.semantic_hash(),
                    new_semantic_hash: Sha256Digest::from_bytes([8; 32]),
                },
                FunctionRevisionChange {
                    function: second,
                    old_id: FunctionRevisionId::from_bytes([5; 16]),
                    new_id: FunctionRevisionId::from_bytes([9; 16]),
                    old_semantic_hash: Sha256Digest::from_bytes([6; 32]),
                    new_semantic_hash: Sha256Digest::from_bytes([10; 32]),
                },
            ]
        );
        assert!(changed_function_revisions(&[first_old.clone()], &[first_old]).is_empty());

        let rendered = render_function_revision_change(
            &changes[0],
            &candidate_with_record(TypeId::from_bytes([4; 16])),
        );
        assert_eq!(
            rendered,
            format!(
                "! function {} executable revision {} -> {} semantic hash {} -> {} [{}]",
                first.canonical(),
                FunctionRevisionId::from_bytes([3; 16]).canonical(),
                FunctionRevisionId::from_bytes([7; 16]).canonical(),
                digest_hex(Sha256Digest::from_bytes([4; 32])),
                digest_hex(Sha256Digest::from_bytes([8; 32])),
                first.canonical(),
            )
        );
    }

    fn candidate_with_record(record_id: TypeId) -> CatalogueSnapshot {
        candidate_with_record_name(
            record_id,
            QualifiedSemanticName::new(["app", "point"]).unwrap(),
        )
    }

    fn candidate_with_record_name(
        record_id: TypeId,
        record_name: QualifiedSemanticName,
    ) -> CatalogueSnapshot {
        let enum_id = TypeId::from_bytes([6; 16]);
        let field = RecordValueFieldDefinition::try_new_descriptor(
            FieldId::from_bytes([7; 16]),
            "longitude",
            0,
            TypeDescriptor::named(enum_id),
        )
        .unwrap();
        CatalogueSnapshot::new_with_record_value_types(
            CatalogueRevisionId::from_bytes([1; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                QualifiedSemanticName::new(["app"]).unwrap(),
            )],
            Vec::new(),
            Vec::new(),
            vec![EnumTypeDefinition::new(
                enum_id,
                QualifiedSemanticName::new(["app", "axis"]).unwrap(),
                ["horizontal", "vertical"],
            )],
            vec![RecordValueTypeDefinition::new(
                record_id,
                record_name,
                vec![field],
            )],
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn renders_all_known_changes_with_stable_ids() {
        let schema_id = SchemaId::from_bytes([1; 16]);
        let object_id = TypeId::from_bytes([2; 16]);
        let value_id = TypeId::from_bytes([3; 16]);
        let record_id = TypeId::from_bytes([4; 16]);
        let field_id = FieldId::from_bytes([5; 16]);
        let enum_id = TypeId::from_bytes([6; 16]);
        let function_id = orna_core::FunctionId::from_bytes([7; 16]);
        let parameter_id = orna_core::ParameterId::from_bytes([8; 16]);
        let candidate = candidate_with_record(record_id);
        let changes = [
            SemanticChange::SchemaAdded {
                id: schema_id,
                name: "app".to_owned(),
            },
            SemanticChange::SchemaDropped {
                id: schema_id,
                name: "app".to_owned(),
            },
            SemanticChange::SchemaRenamed {
                id: schema_id,
                from: "app".to_owned(),
                to: "core".to_owned(),
            },
            SemanticChange::ObjectTypeAdded {
                id: object_id,
                name: "app.widget".to_owned(),
            },
            SemanticChange::ObjectTypeDropped {
                id: object_id,
                name: "app.widget".to_owned(),
            },
            SemanticChange::ObjectTypeRenamed {
                id: object_id,
                from: "app.widget".to_owned(),
                to: "app.gadget".to_owned(),
            },
            SemanticChange::ValueTypeAdded {
                id: value_id,
                name: "app.money".to_owned(),
            },
            SemanticChange::ValueTypeDropped {
                id: value_id,
                name: "app.money".to_owned(),
            },
            SemanticChange::ValueTypeRenamed {
                id: value_id,
                from: "app.money".to_owned(),
                to: "app.currency".to_owned(),
            },
            SemanticChange::ValueTypeKindChanged {
                id: value_id,
                name: "app.currency".to_owned(),
            },
            SemanticChange::ValueTypeMutabilityChanged {
                id: value_id,
                name: "app.currency".to_owned(),
            },
            SemanticChange::ValueTypePersistenceChanged {
                id: value_id,
                name: "app.currency".to_owned(),
            },
            SemanticChange::ValueTypeRepresentationChanged {
                id: value_id,
                name: "app.currency".to_owned(),
            },
            SemanticChange::RecordValueTypeAdded {
                id: record_id,
                name: "app.point".to_owned(),
            },
            SemanticChange::RecordValueTypeDropped {
                id: record_id,
                name: "app.point".to_owned(),
            },
            SemanticChange::RecordValueTypeRenamed {
                id: record_id,
                from: "app.point".to_owned(),
                to: "app.coordinate".to_owned(),
            },
            SemanticChange::FieldAdded {
                owner: record_id,
                id: field_id,
                name: "longitude".to_owned(),
            },
            SemanticChange::FieldDropped {
                owner: record_id,
                id: field_id,
                name: "longitude".to_owned(),
            },
            SemanticChange::FieldRenamed {
                owner: record_id,
                id: field_id,
                from: "longitude".to_owned(),
                to: "east".to_owned(),
            },
            SemanticChange::FieldTypeChanged {
                owner: record_id,
                id: field_id,
                name: "longitude".to_owned(),
            },
            SemanticChange::FieldOrdinalChanged {
                owner: record_id,
                id: field_id,
                name: "longitude".to_owned(),
            },
            SemanticChange::FieldNullabilityChanged {
                owner: record_id,
                id: field_id,
                name: "longitude".to_owned(),
            },
            SemanticChange::FieldUniquenessChanged {
                owner: record_id,
                id: field_id,
                name: "longitude".to_owned(),
            },
            SemanticChange::FieldConstraintChanged {
                owner: record_id,
                id: field_id,
                name: "longitude".to_owned(),
            },
            SemanticChange::EnumTypeAdded {
                id: enum_id,
                name: "app.stage".to_owned(),
            },
            SemanticChange::EnumTypeDropped {
                id: enum_id,
                name: "app.stage".to_owned(),
            },
            SemanticChange::EnumTypeRenamed {
                id: enum_id,
                from: "app.stage".to_owned(),
                to: "app.phase".to_owned(),
            },
            SemanticChange::EnumLabelsChanged {
                id: enum_id,
                name: "app.phase".to_owned(),
            },
            SemanticChange::FunctionAdded {
                id: function_id,
                name: "app.read".to_owned(),
            },
            SemanticChange::FunctionDropped {
                id: function_id,
                name: "app.read".to_owned(),
            },
            SemanticChange::FunctionRenamed {
                id: function_id,
                from: "app.read".to_owned(),
                to: "app.load".to_owned(),
            },
            SemanticChange::FunctionReturnChanged {
                id: function_id,
                name: "app.load".to_owned(),
            },
            SemanticChange::FunctionDomainChanged {
                id: function_id,
                name: "app.load".to_owned(),
            },
            SemanticChange::FunctionSecurityChanged {
                id: function_id,
                name: "app.load".to_owned(),
            },
            SemanticChange::FunctionTransactionChanged {
                id: function_id,
                name: "app.load".to_owned(),
            },
            SemanticChange::FunctionVolatilityChanged {
                id: function_id,
                name: "app.load".to_owned(),
            },
            SemanticChange::ParameterAdded {
                owner: function_id,
                id: parameter_id,
                name: "query".to_owned(),
            },
            SemanticChange::ParameterDropped {
                owner: function_id,
                id: parameter_id,
                name: "query".to_owned(),
            },
            SemanticChange::ParameterRenamed {
                owner: function_id,
                id: parameter_id,
                from: "query".to_owned(),
                to: "search".to_owned(),
            },
            SemanticChange::ParameterOrdinalChanged {
                owner: function_id,
                id: parameter_id,
                name: "search".to_owned(),
                from: 1,
                to: 0,
            },
            SemanticChange::ParameterTypeChanged {
                owner: function_id,
                id: parameter_id,
                name: "search".to_owned(),
            },
            SemanticChange::ParameterDefaultChanged {
                owner: function_id,
                id: parameter_id,
                name: "search".to_owned(),
            },
        ];
        let rendered: Vec<_> = changes
            .iter()
            .map(|change| render_change(change, &candidate))
            .collect();
        let expected = vec![
            format!("+ schema app [{}]", schema_id.canonical()),
            format!("- schema app [{}]", schema_id.canonical()),
            format!("~ schema app -> core [{}]", schema_id.canonical()),
            format!("+ object type app.widget [{}]", object_id.canonical()),
            format!("- object type app.widget [{}]", object_id.canonical()),
            format!(
                "~ object type app.widget -> app.gadget [{}]",
                object_id.canonical()
            ),
            format!("+ value type app.money [{}]", value_id.canonical()),
            format!("- value type app.money [{}]", value_id.canonical()),
            format!(
                "~ value type app.money -> app.currency [{}]",
                value_id.canonical()
            ),
            format!("! value type app.currency kind [{}]", value_id.canonical()),
            format!(
                "! value type app.currency mutability [{}]",
                value_id.canonical()
            ),
            format!(
                "! value type app.currency persistence [{}]",
                value_id.canonical()
            ),
            format!(
                "! value type app.currency representation [{}]",
                value_id.canonical()
            ),
            format!("+ record value type app.point [{}]", record_id.canonical()),
            format!("- record value type app.point [{}]", record_id.canonical()),
            format!(
                "~ record value type app.point -> app.point [{}]",
                record_id.canonical()
            ),
            format!("+ field app.point.longitude [{}]", field_id.canonical()),
            format!("- field app.point.longitude [{}]", field_id.canonical()),
            format!(
                "~ field app.point.longitude -> app.point.east [{}]",
                field_id.canonical()
            ),
            format!(
                "! field app.point.longitude type [{}]",
                field_id.canonical()
            ),
            format!(
                "! field app.point.longitude ordinal [{}]",
                field_id.canonical()
            ),
            format!(
                "! field app.point.longitude nullability [{}]",
                field_id.canonical()
            ),
            format!(
                "! field app.point.longitude uniqueness [{}]",
                field_id.canonical()
            ),
            format!(
                "! field app.point.longitude default/on-delete [{}]",
                field_id.canonical()
            ),
            format!("+ enum type app.axis [{}]", enum_id.canonical()),
            format!("- enum type app.axis [{}]", enum_id.canonical()),
            format!("~ enum type app.axis -> app.axis [{}]", enum_id.canonical()),
            format!("! enum type app.axis labels [{}]", enum_id.canonical()),
            format!("+ function app.read [{}]", function_id.canonical()),
            format!("- function app.read [{}]", function_id.canonical()),
            format!(
                "~ function app.read -> app.load [{}]",
                function_id.canonical()
            ),
            format!(
                "! function app.load return type [{}]",
                function_id.canonical()
            ),
            format!("! function app.load domain [{}]", function_id.canonical()),
            format!("! function app.load security [{}]", function_id.canonical()),
            format!(
                "! function app.load transaction [{}]",
                function_id.canonical()
            ),
            format!(
                "! function app.load volatility [{}]",
                function_id.canonical()
            ),
            format!(
                "+ parameter {}.query [{}]",
                function_id.canonical(),
                parameter_id.canonical()
            ),
            format!(
                "- parameter {}.query [{}]",
                function_id.canonical(),
                parameter_id.canonical()
            ),
            format!(
                "~ parameter {}.query -> {}.search [{}]",
                function_id.canonical(),
                function_id.canonical(),
                parameter_id.canonical()
            ),
            format!(
                "! parameter {}.search ordinal 1 -> 0 [{}]",
                function_id.canonical(),
                parameter_id.canonical()
            ),
            format!(
                "! parameter {}.search type [{}]",
                function_id.canonical(),
                parameter_id.canonical()
            ),
            format!(
                "! parameter {}.search default [{}]",
                function_id.canonical(),
                parameter_id.canonical()
            ),
        ];
        assert_eq!(rendered, expected);
    }

    #[test]
    fn renders_qualified_parts_without_ambiguity_or_control_lines() {
        let name =
            QualifiedSemanticName::new(["app", "metric.value", "quoted\"part", "line\nbreak"])
                .unwrap();

        assert_eq!(
            qualified(&name),
            r#"app."metric.value"."quoted""part"."line\nbreak""#
        );
        assert_eq!(render_identifier_part("field.name"), r#""field.name""#);
        assert_eq!(
            render_identifier_part("line\nbreak\u{001b}"),
            r#""line\nbreak\u{001B}""#,
        );
    }

    #[test]
    fn escapes_qualified_change_names_and_member_names() {
        let function_id = FunctionId::from_bytes([7; 16]);
        let field_id = FieldId::from_bytes([5; 16]);
        let record_id = TypeId::from_bytes([4; 16]);
        let candidate = candidate_with_record(record_id);
        let rendered = render_change(
            &SemanticChange::FunctionAdded {
                id: function_id,
                name: "app.line\nbreak".to_owned(),
            },
            &candidate,
        );
        assert_eq!(
            rendered,
            format!(
                "+ function app.\"line\\nbreak\" [{}]",
                function_id.canonical()
            )
        );

        let rendered = render_change(
            &SemanticChange::FieldAdded {
                owner: record_id,
                id: field_id,
                name: "quoted\"field".to_owned(),
            },
            &candidate,
        );
        assert_eq!(
            rendered,
            format!(
                "+ field app.point.\"quoted\"\"field\" [{}]",
                field_id.canonical()
            )
        );
    }

    #[test]
    fn render_change_recovers_dotted_candidate_parts_by_identity() {
        let record_id = TypeId::from_bytes([4; 16]);
        let field_id = FieldId::from_bytes([7; 16]);
        let candidate = candidate_with_record_name(
            record_id,
            QualifiedSemanticName::new(["app", "metric.value"]).unwrap(),
        );
        let change = SemanticChange::FieldAdded {
            owner: record_id,
            id: field_id,
            name: "longitude".to_owned(),
        };

        assert_eq!(
            render_change_with_catalogues(&change, Some(&candidate), &candidate),
            format!(
                "+ field app.\"metric.value\".longitude [{}]",
                field_id.canonical()
            )
        );
    }

    #[test]
    fn recovery_database_maps_to_recovery_stage_not_attach() {
        let source = "port=invalid"
            .parse::<tokio_postgres::Config>()
            .expect_err("invalid port must produce a PostgreSQL error");
        let error = map_recovery_error(PostgresKernelError::RecoveryDatabase(source));

        assert!(matches!(&error, &InstalledSourceDiffError::Recovery { .. }));
        assert!(!matches!(&error, &InstalledSourceDiffError::Attach { .. }));
    }

    fn assert_invalid_source_path_fails_before_io(path: &str) {
        let error = run_installed_source_diff(path)
            .expect_err("invalid source path must fail before file or host access");
        assert!(
            matches!(
                &error,
                InstalledSourceDiffError::SourceBundle {
                    source: SourceBundleError::EmptyLogicalPath { index: 0 }
                }
            ),
            "invalid path {path:?} must use the closed source-bundle boundary: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "orna: source diff received an invalid source path"
        );
    }

    #[test]
    fn invalid_source_paths_fail_before_missing_file_or_host_access() {
        for path in [
            "",
            "-leading.orna",
            "control\u{0007}.orna",
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
            "./-orna-source-diff-valid-path-{}-missing.orna",
            std::process::id()
        );
        let error = run_installed_source_diff(&path)
            .expect_err("the missing valid path must fail at source read before host access");
        assert!(
            matches!(
                &error,
                InstalledSourceDiffError::SourceRead {
                    path: submitted,
                    source: Some(source),
                } if submitted == &path && source.kind() == std::io::ErrorKind::NotFound
            ),
            "valid path must reach filesystem read: {error:?}"
        );
    }

    #[test]
    fn rejects_invalid_utf8_at_the_diff_entry_boundary_without_mutating_target() {
        use std::fs;

        let root = std::env::temp_dir();
        let stem = format!("orna-source-diff-invalid-utf8-{}", std::process::id());
        let source = root.join(format!("{stem}-source"));
        let target = root.join(format!("{stem}-target"));
        let _ = fs::remove_file(&source);
        let _ = fs::remove_file(&target);
        fs::write(&source, [0xff, 0xfe, 0xfd]).unwrap();
        fs::write(&target, b"target sentinel").unwrap();
        let target_before = fs::read(&target).unwrap();

        let result = run_installed_source_diff(source.to_str().unwrap());

        assert!(matches!(
            result,
            Err(InstalledSourceDiffError::SourceUtf8 { path }) if path == source.to_str().unwrap()
        ));
        assert_eq!(fs::read(&target).unwrap(), target_before);
        assert_eq!(fs::read(&source).unwrap(), [0xff, 0xfe, 0xfd]);
        fs::remove_file(source).unwrap();
        fs::remove_file(target).unwrap();
    }

    #[test]
    fn rejects_directory_at_the_diff_entry_boundary_without_mutating_contents() {
        use std::fs;

        let root = std::env::temp_dir();
        let stem = format!("orna-source-diff-directory-{}", std::process::id());
        let source = root.join(stem);
        let marker = source.join("marker");
        let _ = fs::remove_dir_all(&source);
        fs::create_dir(&source).unwrap();
        fs::write(&marker, b"directory sentinel").unwrap();
        let marker_before = fs::read(&marker).unwrap();

        let result = run_installed_source_diff(source.to_str().unwrap());

        assert!(matches!(
            result,
            Err(InstalledSourceDiffError::SourceRead { path, source: None })
                if path == source.to_str().unwrap()
        ));
        assert!(source.is_dir());
        assert_eq!(fs::read(&marker).unwrap(), marker_before);
        fs::remove_dir_all(source).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn accepts_regular_symlink_source_paths_and_preserves_logical_path() {
        use std::{fs, os::unix::fs::symlink};

        let root = std::env::temp_dir();
        let stem = format!("orna-source-diff-symlink-{}", std::process::id());
        let target = root.join(format!("{stem}-target"));
        let link = root.join(format!("{stem}-link"));
        let _ = fs::remove_file(&target);
        let _ = fs::remove_file(&link);
        fs::write(&target, "CREATE SCHEMA app;").unwrap();
        symlink(&target, &link).unwrap();

        let result = read_source_bundle(link.to_str().unwrap()).unwrap();
        let unit = &result.units()[0];
        assert_eq!(unit.logical_path(), link.to_str().unwrap());
        assert_eq!(unit.content(), "CREATE SCHEMA app;");

        fs::remove_file(target).unwrap();
        fs::remove_file(link).unwrap();
    }
}
