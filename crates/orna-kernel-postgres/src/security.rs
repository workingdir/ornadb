use orna_core::{
    FunctionId, PrincipalId,
    revision::RevisionPair,
    security::{
        ExecuteGrant, Principal, PrincipalKind, PrincipalStatus, RoleMembership, SecuritySnapshot,
    },
};
use tokio_postgres::{IsolationLevel, Row, Transaction};

use crate::{
    PostgresKernel, PostgresKernelError, bootstrap::require_current_migrations,
    recovery::recover_active_revision,
};

impl PostgresKernel {
    /// Recovers the security decision snapshot for the active revision.
    pub async fn recover_security_snapshot(&self) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let snapshot = recover_security_snapshot(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(snapshot)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Atomically replaces all durable security decision records.
    pub async fn replace_security_snapshot(
        &self,
        snapshot: &SecuritySnapshot,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::Serializable)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            lock_active_revision(&transaction, snapshot.revision()).await?;
            require_complete_function_set(&transaction, snapshot).await?;
            replace_security_rows(&transaction, snapshot).await?;
            let recovered = recover_security_snapshot(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recovered)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }
}

fn finish_security_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

async fn lock_active_revision(
    transaction: &Transaction<'_>,
    expected: RevisionPair,
) -> Result<(), PostgresKernelError> {
    let row = transaction
        .query_one(
            "SELECT source_revision_id, catalogue_revision_id
             FROM _orna_kernel.active_revision
             WHERE singleton = true
             FOR UPDATE",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let active = RevisionPair::new(
        orna_core::SourceRevisionId::from_bytes(exact_id(
            &row,
            "source_revision_id",
            "active source revision is not exactly 16 bytes",
        )?),
        orna_core::CatalogueRevisionId::from_bytes(exact_id(
            &row,
            "catalogue_revision_id",
            "active catalogue revision is not exactly 16 bytes",
        )?),
    );
    if expected != active {
        return Err(PostgresKernelError::SecurityRevisionMismatch { expected, active });
    }
    Ok(())
}

async fn require_complete_function_set(
    transaction: &Transaction<'_>,
    snapshot: &SecuritySnapshot,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT function_id
             FROM _orna_kernel.catalogue_functions
             WHERE catalogue_revision_id = (
                 SELECT catalogue_revision_id
                 FROM _orna_kernel.active_revision
                 WHERE singleton = true
             )
             ORDER BY function_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let active = rows
        .iter()
        .map(|row| {
            exact_id(
                row,
                "function_id",
                "active function identity is not exactly 16 bytes",
            )
            .map(FunctionId::from_bytes)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if active != snapshot.functions().collect::<Vec<_>>() {
        return Err(PostgresKernelError::SecurityFunctionSetMismatch);
    }
    Ok(())
}

async fn replace_security_rows(
    transaction: &Transaction<'_>,
    snapshot: &SecuritySnapshot,
) -> Result<(), PostgresKernelError> {
    transaction
        .batch_execute(
            "DELETE FROM _orna_kernel.security_execute_grants;
             DELETE FROM _orna_kernel.security_role_memberships;
             DELETE FROM _orna_kernel.security_principals;",
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    for principal in snapshot.principals() {
        let id = principal.id().to_bytes().to_vec();
        let kind = encode_principal_kind(principal.kind());
        let status = encode_principal_status(principal.status());
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                 VALUES ($1, $2, $3)",
                &[&id, &kind, &status],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for membership in snapshot.memberships() {
        let role = membership.role().to_bytes().to_vec();
        let member = membership.member().to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_role_memberships (role_id, member_id)
                 VALUES ($1, $2)",
                &[&role, &member],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for grant in snapshot.execute_grants() {
        let grantee = grant.grantee().to_bytes().to_vec();
        let function = grant.function().to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_execute_grants (grantee_id, function_id)
                 VALUES ($1, $2)",
                &[&grantee, &function],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    Ok(())
}

async fn recover_security_snapshot(
    transaction: &Transaction<'_>,
) -> Result<SecuritySnapshot, PostgresKernelError> {
    let active = recover_active_revision(transaction).await?;
    let functions = active
        .catalogue()
        .functions()
        .iter()
        .map(|function| function.id())
        .collect::<Vec<_>>();
    let principals = load_principals(transaction).await?;
    let memberships = load_memberships(transaction).await?;
    let grants = load_grants(transaction).await?;

    SecuritySnapshot::new(active.pair(), functions, principals, memberships, grants)
        .map_err(PostgresKernelError::SecuritySnapshot)
}

async fn load_principals(
    transaction: &Transaction<'_>,
) -> Result<Vec<Principal>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT id, kind, status
             FROM _orna_kernel.security_principals
             ORDER BY id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .map(|row| {
            let id = PrincipalId::from_bytes(exact_id(
                row,
                "id",
                "security principal identity is not exactly 16 bytes",
            )?);
            let kind = decode_principal_kind(row.try_get("kind").map_err(|source| {
                row_decode(
                    "_orna_kernel.security_principals",
                    id.canonical(),
                    "kind",
                    source,
                )
            })?)?;
            let status = decode_principal_status(row.try_get("status").map_err(|source| {
                row_decode(
                    "_orna_kernel.security_principals",
                    id.canonical(),
                    "status",
                    source,
                )
            })?)?;
            Ok(Principal::new(id, kind, status))
        })
        .collect()
}

async fn load_memberships(
    transaction: &Transaction<'_>,
) -> Result<Vec<RoleMembership>, PostgresKernelError> {
    transaction
        .query(
            "SELECT role_id, member_id
             FROM _orna_kernel.security_role_memberships
             ORDER BY member_id, role_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(|row| {
            Ok(RoleMembership::new(
                PrincipalId::from_bytes(exact_id(
                    row,
                    "role_id",
                    "security role identity is not exactly 16 bytes",
                )?),
                PrincipalId::from_bytes(exact_id(
                    row,
                    "member_id",
                    "security member identity is not exactly 16 bytes",
                )?),
            ))
        })
        .collect()
}

async fn load_grants(
    transaction: &Transaction<'_>,
) -> Result<Vec<ExecuteGrant>, PostgresKernelError> {
    transaction
        .query(
            "SELECT grantee_id, function_id
             FROM _orna_kernel.security_execute_grants
             ORDER BY grantee_id, function_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(|row| {
            Ok(ExecuteGrant::new(
                PrincipalId::from_bytes(exact_id(
                    row,
                    "grantee_id",
                    "security grantee identity is not exactly 16 bytes",
                )?),
                FunctionId::from_bytes(exact_id(
                    row,
                    "function_id",
                    "security grant function identity is not exactly 16 bytes",
                )?),
            ))
        })
        .collect()
}

fn exact_id(
    row: &Row,
    column: &'static str,
    rule: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = row.try_get(column).map_err(|source| {
        row_decode(
            "_orna_kernel security snapshot",
            "selected row".to_owned(),
            column,
            source,
        )
    })?;
    bytes
        .try_into()
        .map_err(|_| PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel security snapshot",
            record: "selected row".to_owned(),
            rule,
        })
}

fn row_decode(
    relation: &'static str,
    record: String,
    column: &'static str,
    source: tokio_postgres::Error,
) -> PostgresKernelError {
    PostgresKernelError::RowDecode {
        relation,
        record,
        column,
        rule: "security snapshot column must use its exact PostgreSQL type",
        source,
    }
}

fn encode_principal_kind(kind: PrincipalKind) -> &'static str {
    match kind {
        PrincipalKind::User => "user",
        PrincipalKind::Role => "role",
        PrincipalKind::Service => "service",
    }
}

fn decode_principal_kind(value: String) -> Result<PrincipalKind, PostgresKernelError> {
    match value.as_str() {
        "user" => Ok(PrincipalKind::User),
        "role" => Ok(PrincipalKind::Role),
        "service" => Ok(PrincipalKind::Service),
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_principals",
            record: value,
            rule: "principal kind must be user, role, or service",
        }),
    }
}

fn encode_principal_status(status: PrincipalStatus) -> &'static str {
    match status {
        PrincipalStatus::Active => "active",
        PrincipalStatus::Disabled => "disabled",
    }
}

fn decode_principal_status(value: String) -> Result<PrincipalStatus, PostgresKernelError> {
    match value.as_str() {
        "active" => Ok(PrincipalStatus::Active),
        "disabled" => Ok(PrincipalStatus::Disabled),
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_principals",
            record: value,
            rule: "principal status must be active or disabled",
        }),
    }
}
