//! Candidate base, standard-context, and durable-grant preflight validation.

use super::*;

pub(super) fn validate_expected_base(
    active: &ActiveDatabaseRevision,
    candidate: &DeployableRevision,
) -> Result<(), PostgresKernelError> {
    if candidate.expected_base() != active.pair() {
        return Err(PostgresKernelError::ExpectedBaseMismatch {
            expected: candidate.expected_base(),
            active: active.pair(),
        });
    }
    Ok(())
}

pub(super) fn guard_standard_context_transition(
    active: &CatalogueHashContext,
    candidate: &CatalogueHashContext,
) -> Result<(), PostgresKernelError> {
    match (active, candidate) {
        (CatalogueHashContext::Version1, CatalogueHashContext::Version1) => Ok(()),
        (
            CatalogueHashContext::Version2 { standard: active },
            CatalogueHashContext::Version2 {
                standard: candidate,
            },
        ) => {
            let active = StandardContextIdentity::from_verified_snapshot(active);
            let candidate = StandardContextIdentity::from_verified_snapshot(candidate);
            standard_context_mismatch(active, candidate).map_or(Ok(()), Err)
        }
        _ => Err(PostgresKernelError::StandardContextTransitionRequired {
            active: active.version(),
            candidate: candidate.version(),
        }),
    }
}

pub(super) fn validate_candidate_preflight(
    active: &ActiveDatabaseRevision,
    candidate: &DeployableRevision,
) -> Result<(), PostgresKernelError> {
    validate_expected_base(active, candidate)?;
    guard_standard_context_transition(
        active.catalogue_hash_context(),
        candidate.catalogue_hash_context(),
    )?;
    validate_persistable_catalogue(candidate)
        .map_err(PostgresKernelError::CandidateRevisionInvariant)
}

pub(super) async fn validate_durable_grant_targets(
    transaction: &Transaction<'_>,
    candidate: &DeployableRevision,
) -> Result<(), PostgresKernelError> {
    const EXECUTE_RELATION: &str = "_orna_kernel.security_execute_grants";
    const PRIVILEGE_RELATION: &str = "_orna_kernel.security_privilege_grants";
    let execute_rows = transaction
        .query(
            "SELECT function_id
             FROM _orna_kernel.security_execute_grants
             ORDER BY grantee_id, function_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    for row in &execute_rows {
        let record = DurableRecord::new(EXECUTE_RELATION, "candidate");
        let function = FunctionId::from_bytes(identity_bytes(
            record.column(
                row,
                "function_id",
                "durable EXECUTE grant target identity is not exactly 16 bytes",
            )?,
            &record,
            "durable EXECUTE grant target identity is not exactly 16 bytes",
        )?);
        if !candidate_retains_function_target(candidate, function) {
            return Err(PostgresKernelError::DurableInvariant {
                relation: EXECUTE_RELATION,
                record: "candidate".to_owned(),
                rule: "candidate source must retain every durable EXECUTE grant target",
            });
        }
    }

    let privilege_rows = transaction
        .query(
            "SELECT object_id
             FROM _orna_kernel.security_privilege_grants
             ORDER BY grantee_id, privilege_class, object_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    for row in &privilege_rows {
        let record = DurableRecord::new(PRIVILEGE_RELATION, "candidate");
        let object: Vec<u8> = record.column(
            row,
            "object_id",
            "durable privilege grant object identity must be empty or exactly 16 bytes",
        )?;
        if object.is_empty() {
            continue;
        }
        let function = FunctionId::from_bytes(identity_bytes(
            object,
            &record,
            "durable privilege grant object identity must be empty or exactly 16 bytes",
        )?);
        if !candidate_retains_privilege_target(candidate, function) {
            return Err(PostgresKernelError::DurableInvariant {
                relation: PRIVILEGE_RELATION,
                record: "candidate".to_owned(),
                rule: "candidate source must retain every durable privilege grant object target",
            });
        }
    }
    Ok(())
}

pub(super) fn candidate_retains_function_target(
    candidate: &DeployableRevision,
    function: FunctionId,
) -> bool {
    candidate.candidate().function_by_id(function).is_some()
        || candidate
            .catalogue_hash_context()
            .standard()
            .is_some_and(|standard| standard.catalogue().function_by_id(function).is_some())
}

pub(super) fn candidate_retains_privilege_target(
    candidate: &DeployableRevision,
    function: FunctionId,
) -> bool {
    candidate_retains_function_target(candidate, function)
        || system_function_by_id(function).is_some()
}

pub(super) fn standard_context_mismatch(
    active: StandardContextIdentity,
    candidate: StandardContextIdentity,
) -> Option<PostgresKernelError> {
    (active != candidate).then(|| PostgresKernelError::StandardContextMismatch {
        active: Box::new(active),
        candidate: Box::new(candidate),
    })
}
