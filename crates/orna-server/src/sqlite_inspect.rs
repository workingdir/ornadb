//! Direct local SQLite adapter for redacted invocation inspection.

use std::{io::Write, path::PathBuf};

use orna_core::{
    PrincipalId,
    inspect::InspectPrivilege,
    security::{InspectDecision, PrivilegeClass, SecuritySnapshot},
};
use orna_sqlite::{SqliteConfig, SqliteError, SqliteInspectSnapshotRecord, SqliteRevisionStore};

use crate::{
    InstalledInspectError, InstalledInspectErrorKind, InstalledInspectOutcome,
    InstalledInspectProjection, InstalledInspectRequest,
};

/// Runs one local `orna inspect` request against a SQLite database.
pub fn run_sqlite_inspect(
    database_path: impl Into<PathBuf>,
    request: InstalledInspectRequest,
    stdout: &mut impl Write,
) -> Result<InstalledInspectOutcome, InstalledInspectError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| inspect_error(InstalledInspectErrorKind::Internal, error.to_string()))?;
    runtime.block_on(run_sqlite_inspect_async(
        database_path.into(),
        request,
        stdout,
    ))
}

async fn run_sqlite_inspect_async(
    database_path: PathBuf,
    request: InstalledInspectRequest,
    stdout: &mut impl Write,
) -> Result<InstalledInspectOutcome, InstalledInspectError> {
    let store = SqliteRevisionStore::open(&SqliteConfig::new(database_path))
        .await
        .map_err(|error| inspect_error(InstalledInspectErrorKind::Internal, error.to_string()))?;
    let uid = nix::unistd::geteuid().as_raw();
    store
        .provision_local_peer(uid)
        .await
        .map_err(|error| inspect_error(InstalledInspectErrorKind::Internal, error.to_string()))?;
    let active = store
        .recover()
        .await
        .map_err(|error| inspect_error(InstalledInspectErrorKind::Internal, error.to_string()))?;
    let security = store
        .security_snapshot(&active)
        .await
        .map_err(|error| inspect_error(InstalledInspectErrorKind::Internal, error.to_string()))?;
    let session = security.authenticate_local_peer(uid).map_err(|error| {
        inspect_error(
            InstalledInspectErrorKind::Internal,
            format!("local peer authentication failed: {error}"),
        )
    })?;
    let (snapshot, events) = store
        .read_inspect_at(
            &active,
            &security,
            &session,
            request.invocation,
            request.epoch,
            request.after_sequence,
            request.trace,
        )
        .await
        .map_err(map_inspect_read_error)?;
    let snapshot = snapshot.ok_or_else(|| {
        InstalledInspectError::with_code(
            InstalledInspectErrorKind::Kernel,
            "the requested invocation has no captured inspection epoch".to_owned(),
            "inspect.missing_epoch",
        )
    })?;
    validate_snapshot(&snapshot, &active.pair())?;
    let requested = requested_privilege(&request);
    let granted = inspect_grants(&security, session.principal());
    match orna_core::security::authorise_inspect(
        session.principal(),
        requested,
        Some(snapshot.owner),
        &granted,
    ) {
        InspectDecision::Allowed { .. } => {}
        InspectDecision::Denied(reason) => {
            return Err(InstalledInspectError::with_code(
                InstalledInspectErrorKind::Kernel,
                "inspection was denied".to_owned(),
                reason.audit_reason(),
            ));
        }
    }

    let summary: serde_json::Value =
        serde_json::from_slice(&snapshot.summary).map_err(|error| {
            inspect_error(
                InstalledInspectErrorKind::Kernel,
                format!("the persisted inspection summary is invalid: {error}"),
            )
        })?;
    if let Some(projection) = request.projection {
        render_projection(stdout, projection, &summary)?;
    }
    if request.trace {
        for event in events {
            write_json_bytes(stdout, &event.payload)?;
        }
    }
    if request.projection.is_none() && !request.trace {
        write_json_bytes(stdout, &snapshot.summary)?;
    }
    Ok(InstalledInspectOutcome::Completed)
}

fn map_inspect_read_error(error: SqliteError) -> InstalledInspectError {
    let epoch_mismatch = match &error {
        SqliteError::Domain(message) => {
            message.contains("active SQLite revision")
                || message.contains("security snapshot")
                || message.contains("pinned to the active revision")
        }
        SqliteError::InvalidPersistedData(message) => {
            *message == "inspection snapshot is not pinned to the active revision"
        }
        _ => false,
    };
    let message = error.to_string();
    if epoch_mismatch {
        InstalledInspectError::with_code(
            InstalledInspectErrorKind::Kernel,
            message,
            "inspect.epoch_mismatch",
        )
    } else {
        inspect_error(InstalledInspectErrorKind::Internal, message)
    }
}

fn validate_snapshot(
    snapshot: &SqliteInspectSnapshotRecord,
    pair: &orna_core::revision::RevisionPair,
) -> Result<(), InstalledInspectError> {
    if snapshot.source_revision != pair.source() || snapshot.catalogue_revision != pair.catalogue()
    {
        return Err(InstalledInspectError::with_code(
            InstalledInspectErrorKind::Kernel,
            "inspection epoch is not pinned to the active revision".to_owned(),
            "inspect.epoch_mismatch",
        ));
    }
    Ok(())
}

fn requested_privilege(request: &InstalledInspectRequest) -> InspectPrivilege {
    if request.trace && request.include_values {
        return InspectPrivilege::Values;
    }
    match request.projection {
        Some(InstalledInspectProjection::Calls | InstalledInspectProjection::StateCells)
            if request.include_values =>
        {
            InspectPrivilege::Values
        }
        Some(InstalledInspectProjection::SecurityDecisions) if request.include_security => {
            InspectPrivilege::SecurityDetails
        }
        Some(InstalledInspectProjection::RuntimeBindings) if request.include_runtime => {
            InspectPrivilege::RuntimeInternals
        }
        _ => InspectPrivilege::OwnInvocation,
    }
}

fn inspect_grants(snapshot: &SecuritySnapshot, principal: PrincipalId) -> Vec<InspectPrivilege> {
    snapshot
        .privilege_grants()
        .filter(|grant| grant.grantee() == principal && grant.object().is_none())
        .filter_map(|grant| match grant.class() {
            PrivilegeClass::Inspect(privilege) => Some(privilege),
            _ => None,
        })
        .collect()
}

fn render_projection(
    stdout: &mut impl Write,
    projection: InstalledInspectProjection,
    summary: &serde_json::Value,
) -> Result<(), InstalledInspectError> {
    let projection_name = match projection {
        InstalledInspectProjection::InvocationNodes => "invocation_nodes",
        InstalledInspectProjection::Calls => "calls",
        InstalledInspectProjection::Resources => "resources",
        InstalledInspectProjection::StateCells => "state_cells",
        InstalledInspectProjection::UiNodes => "ui_nodes",
        InstalledInspectProjection::PresentationCandidates => "presentation_candidates",
        InstalledInspectProjection::RuntimeBindings => "runtime_bindings",
        InstalledInspectProjection::SecurityDecisions => "security_decisions",
    };
    let Some(rows) = summary
        .get(projection_name)
        .and_then(serde_json::Value::as_array)
    else {
        return Err(InstalledInspectError::with_code(
            InstalledInspectErrorKind::Kernel,
            format!("inspection projection {projection_name} is unavailable"),
            "inspect.projection_failed",
        ));
    };
    for row in rows {
        let record = serde_json::json!({
            "projection": projection_name,
            "record": row,
        });
        let bytes = serde_json::to_vec(&record).map_err(|error| {
            inspect_error(
                InstalledInspectErrorKind::Rendering,
                format!("could not render inspection projection: {error}"),
            )
        })?;
        write_json_bytes(stdout, &bytes)?;
    }
    Ok(())
}

fn write_json_bytes(stdout: &mut impl Write, bytes: &[u8]) -> Result<(), InstalledInspectError> {
    stdout.write_all(bytes).map_err(|error| {
        inspect_error(
            InstalledInspectErrorKind::Rendering,
            format!("could not write inspection output: {error}"),
        )
    })?;
    stdout.write_all(b"\n").map_err(|error| {
        inspect_error(
            InstalledInspectErrorKind::Rendering,
            format!("could not write inspection output: {error}"),
        )
    })
}

fn inspect_error(
    kind: InstalledInspectErrorKind,
    message: impl Into<String>,
) -> InstalledInspectError {
    InstalledInspectError::new(kind, message.into())
}
