//! Shared mechanics for installed source commands.

use std::{
    fs,
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
};

use orna_compiler::{
    CompilerDiagnostic, ParseReport, StandardApplicationCheckContext,
    StandardApplicationCheckReport, StandardApplicationContextError, StandardLibraryCheckError,
    check_standard_application, check_standard_library_source,
};
use orna_core::{
    revision::{ActiveDatabaseRevision, VerifiedStandardLibrarySnapshot},
    source::{SourceBundle, SourceBundleError, SourceUnit},
};
use orna_postgres::PostgresKernelError;
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

use crate::{EmbeddedHostError, source_diagnostics};

/// A source-file failure before compiler checking begins.
#[derive(Debug)]
pub(crate) enum SourceFileError {
    Read {
        path: String,
        source: Option<io::Error>,
    },
    Utf8 {
        path: String,
    },
    Bundle {
        source: SourceBundleError,
    },
}

/// Reads exactly one regular source file without permitting a blocking special file.
pub(crate) fn read_source_bundle(path: &str) -> Result<SourceBundle, SourceFileError> {
    validate_source_path(path)?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| SourceFileError::Read {
            path: path.to_owned(),
            source: Some(source),
        })?;
    if !file
        .metadata()
        .map_err(|source| SourceFileError::Read {
            path: path.to_owned(),
            source: Some(source),
        })?
        .is_file()
    {
        return Err(SourceFileError::Read {
            path: path.to_owned(),
            source: None,
        });
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| SourceFileError::Read {
            path: path.to_owned(),
            source: Some(source),
        })?;
    let source = String::from_utf8(bytes).map_err(|_| SourceFileError::Utf8 {
        path: path.to_owned(),
    })?;
    SourceBundle::new([SourceUnit::new(path, source)])
        .map_err(|source| SourceFileError::Bundle { source })
}

fn validate_source_path(path: &str) -> Result<(), SourceFileError> {
    if path.is_empty()
        || path.starts_with('-')
        || path
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
    {
        return Err(SourceFileError::Bundle {
            source: SourceBundleError::EmptyLogicalPath { index: 0 },
        });
    }
    Ok(())
}

/// Stable classification shared by installed source command adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostFailureClass {
    InstanceNotInstalled,
    InstanceInvalid,
    EngineInvalid,
}

pub(crate) fn classify_host_error(source: &EmbeddedHostError) -> HostFailureClass {
    match source {
        EmbeddedHostError::Engine(_) | EmbeddedHostError::InvalidEngineManifest => {
            HostFailureClass::EngineInvalid
        }
        EmbeddedHostError::Io(error) if error.kind() == io::ErrorKind::NotFound => {
            HostFailureClass::InstanceNotInstalled
        }
        _ => HostFailureClass::InstanceInvalid,
    }
}

/// Detailed kernel failure class; each command maps it to its accepted public error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KernelFailureClass {
    Configuration,
    Database,
    DriverTask,
    SessionClose,
    RecoveryDatabase,
    Other,
}

pub(crate) fn classify_kernel_error(source: &PostgresKernelError) -> KernelFailureClass {
    match source {
        PostgresKernelError::Configuration(_) => KernelFailureClass::Configuration,
        PostgresKernelError::Database(_) => KernelFailureClass::Database,
        PostgresKernelError::DriverTask(_) => KernelFailureClass::DriverTask,
        PostgresKernelError::SessionClose(_) => KernelFailureClass::SessionClose,
        PostgresKernelError::RecoveryDatabase(_) => KernelFailureClass::RecoveryDatabase,
        _ => KernelFailureClass::Other,
    }
}

/// Failure while reconstructing the accepted standard application check context.
#[derive(Debug)]
pub(crate) enum ApplicationCheckError {
    ActiveStandardMismatch,
    StandardLibrary(StandardLibraryError),
    StandardSource(StandardLibraryCheckError),
    ApplicationContext(StandardApplicationContextError),
}

/// Checks application source against the exact retained standard installed in the active revision.
pub(crate) fn check_application_source(
    active: &ActiveDatabaseRevision,
    bundle: &SourceBundle,
) -> Result<StandardApplicationCheckReport, ApplicationCheckError> {
    let installed = active
        .catalogue_hash_context()
        .standard()
        .ok_or(ApplicationCheckError::ActiveStandardMismatch)?;
    let accepted = select_accepted_standard(installed).map_err(|error| match error {
        StandardSelectionError::UnknownRevision => ApplicationCheckError::ActiveStandardMismatch,
        StandardSelectionError::Verification(source) => {
            ApplicationCheckError::StandardLibrary(source)
        }
    })?;
    require_accepted_active_standard(active, &accepted)?;
    let standard =
        check_standard_library_source(&accepted).map_err(ApplicationCheckError::StandardSource)?;
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard)
        .map_err(ApplicationCheckError::ApplicationContext)?;
    Ok(check_standard_application(bundle, &context))
}

#[derive(Debug)]
pub(crate) enum StandardSelectionError {
    UnknownRevision,
    Verification(StandardLibraryError),
}

pub(crate) fn select_accepted_standard(
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
) -> Result<(), ApplicationCheckError> {
    let Some(installed) = active.catalogue_hash_context().standard() else {
        return Err(ApplicationCheckError::ActiveStandardMismatch);
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
        return Err(ApplicationCheckError::ActiveStandardMismatch);
    }
    Ok(())
}

/// The three stable diagnostic renderings used by installed source commands.
pub(crate) struct RenderedSourceDiagnostics {
    machine: Vec<u8>,
    human: Vec<u8>,
    coloured: Vec<u8>,
}

impl RenderedSourceDiagnostics {
    pub(crate) fn into_parts(self) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (self.machine, self.human, self.coloured)
    }
}

pub(crate) fn render_source_diagnostics(
    parse_report: &ParseReport,
    diagnostics: &[CompilerDiagnostic],
) -> RenderedSourceDiagnostics {
    RenderedSourceDiagnostics {
        machine: source_diagnostics::render_diagnostics(diagnostics),
        human: source_diagnostics::render_human_diagnostics(parse_report, diagnostics, false),
        coloured: source_diagnostics::render_human_diagnostics(parse_report, diagnostics, true),
    }
}
