//! Security snapshot persistence and recovery.

use super::*;

pub(super) async fn replace_security_rows(
    transaction: &Transaction<'_>,
    snapshot: &SecuritySnapshot,
) -> Result<(), PostgresKernelError> {
    transaction
        .batch_execute(
            "DELETE FROM _orna_kernel.security_local_peer_credentials;
         DELETE FROM _orna_kernel.security_execute_grants;
         DELETE FROM _orna_kernel.security_privilege_grants;
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
    for credential in snapshot.local_peer_credentials() {
        let uid = i64::from(credential.uid());
        let principal = credential.principal().to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_local_peer_credentials (uid, principal_id)
             VALUES ($1, $2)",
                &[&uid, &principal],
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
    for grant in snapshot.privilege_grants() {
        let grantee = grant.grantee().to_bytes().to_vec();
        let class = encode_privilege_class(grant.class());
        let object = grant
            .object()
            .map(|function| function.to_bytes().to_vec())
            .unwrap_or_default();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_privilege_grants
                 (grantee_id, privilege_class, object_id)
             VALUES ($1, $2, $3)",
                &[&grantee, &class, &object],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    Ok(())
}

pub(super) async fn insert_execute_grant_if_absent(
    transaction: &Transaction<'_>,
    grant: ExecuteGrant,
) -> Result<(), PostgresKernelError> {
    let grantee = grant.grantee().to_bytes().to_vec();
    let function = grant.function().to_bytes().to_vec();
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_execute_grants (grantee_id, function_id)
         VALUES ($1, $2)
         ON CONFLICT (grantee_id, function_id) DO NOTHING",
            &[&grantee, &function],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

pub(super) async fn recover_security_snapshot(
    transaction: &Transaction<'_>,
) -> Result<SecuritySnapshot, PostgresKernelError> {
    let active = recover_active_revision(transaction).await?;
    recover_security_snapshot_for_active(transaction, &active).await
}

pub(crate) async fn recover_security_snapshot_for_active(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
) -> Result<SecuritySnapshot, PostgresKernelError> {
    let function_targets = load_invocation_target_authorities(transaction, active).await?;
    let principals = load_principals(transaction).await?;
    let memberships = load_memberships(transaction).await?;
    let grants = load_grants(transaction).await?;
    let privilege_grants = load_privilege_grants(transaction).await?;
    let local_peer_credentials = load_local_peer_credentials(transaction).await?;

    let snapshot =
        SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
            active.pair(),
            function_targets,
            principals,
            memberships,
            grants,
            local_peer_credentials,
            privilege_grants,
        )
        .map_err(PostgresKernelError::SecuritySnapshot)?;
    require_complete_function_set(active, &snapshot)?;
    Ok(snapshot)
}

/// Loads the closed two-class `EXECUTE` target union for the active catalogue
/// revision from the durable target-authority relation.
///
/// Apply is the only writer of this relation, so every application and
/// standard row carries its exact pinned executable revision. Sealed system
/// identity rows are audit anchors only and do not enter the in-memory
/// application/standard target set.
///
/// Recovery validates the standard rows against the already-verified active
/// standard snapshot and fails closed on any absent, duplicated, mismatched,
/// or unverified standard target.
pub(super) async fn load_invocation_target_authorities(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
) -> Result<Vec<SecurityFunctionTarget>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.invocation_target_authorities";
    let admitted_system_identities = [
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
    ];
    let mut seen_system_identities = [false; 3];
    let catalogue = active.pair().catalogue().to_bytes().to_vec();
    let rows = transaction
        .query(
            "SELECT authority.function_id AS function_id,
                authority.target_class AS target_class,
                authority.function_revision_id AS function_revision_id,
                authority.standard_library_revision_id AS standard_library_revision_id,
                function.current_function_revision_id AS catalogue_current_revision_id
         FROM _orna_kernel.invocation_target_authorities AS authority
         LEFT JOIN _orna_kernel.catalogue_functions AS function
           ON function.catalogue_revision_id = authority.catalogue_revision_id
          AND function.function_id = authority.function_id
         WHERE authority.catalogue_revision_id = $1
         ORDER BY authority.function_id",
            &[&catalogue],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut targets = Vec::with_capacity(rows.len());
    for row in &rows {
        let function = FunctionId::from_bytes(exact_id(
            row,
            "function_id",
            "invocation target function identity is not exactly 16 bytes",
        )?);
        let executable = FunctionRevisionId::from_bytes(exact_id(
            row,
            "function_revision_id",
            "invocation target function revision identity is not exactly 16 bytes",
        )?);
        let class: String = row
            .try_get("target_class")
            .map_err(|source| row_decode(RELATION, function.canonical(), "target_class", source))?;
        let standard: Option<Vec<u8>> =
            row.try_get("standard_library_revision_id")
                .map_err(|source| {
                    row_decode(
                        RELATION,
                        function.canonical(),
                        "standard_library_revision_id",
                        source,
                    )
                })?;
        let catalogue_revision: Option<Vec<u8>> = row
            .try_get("catalogue_current_revision_id")
            .map_err(|source| {
                row_decode(
                    RELATION,
                    function.canonical(),
                    "catalogue_current_revision_id",
                    source,
                )
            })?;
        let missing = || PostgresKernelError::DurableInvariant {
            relation: RELATION,
            record: function.canonical(),
            rule: "application invocation targets must resolve in the pinned application catalogue",
        };
        match class.as_str() {
            "application" => {
                if standard.is_some() {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: function.canonical(),
                        rule: "application invocation targets must not pin a standard library revision",
                    });
                }
                if catalogue_revision.as_deref() != Some(executable.to_bytes().as_slice()) {
                    return Err(missing());
                }
                targets.push(SecurityFunctionTarget::application(function));
            }
            "standard" => {
                if catalogue_revision.is_some() {
                    // The same function identity cannot be both an application
                    // catalogue function and a standard executable target.
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: function.canonical(),
                        rule: "standard invocation targets must not duplicate an application catalogue function",
                    });
                }
                let bytes = standard.ok_or_else(|| PostgresKernelError::DurableInvariant {
                    relation: RELATION,
                    record: function.canonical(),
                    rule: "standard invocation target must pin an exact standard library revision",
                })?;
                let standard_revision = StandardLibraryRevisionId::from_bytes(
                bytes.try_into().map_err(|_| PostgresKernelError::DurableInvariant {
                    relation: RELATION,
                    record: function.canonical(),
                    rule: "standard invocation target standard revision identity is not exactly 16 bytes",
                })?,
            );
                targets.push(SecurityFunctionTarget::verified_standard(
                    function,
                    standard_revision,
                    executable,
                ));
            }
            "system" => {
                if catalogue_revision.is_some()
                    || standard.is_some()
                    || executable.to_bytes() != function.to_bytes()
                    || !system_function_by_id(function).is_some_and(is_admitted_security_identity)
                {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: function.canonical(),
                        rule: "system invocation targets must be sealed audit anchors",
                    });
                }
                let Some(index) = admitted_system_identities
                    .iter()
                    .position(|identity| *identity == function)
                else {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: function.canonical(),
                        rule: "system invocation targets must use exactly the admitted sealed identities",
                    });
                };
                if seen_system_identities[index] {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: function.canonical(),
                        rule: "system invocation targets must contain each admitted sealed identity exactly once",
                    });
                }
                seen_system_identities[index] = true;
            }
            _ => {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: RELATION,
                    record: function.canonical(),
                    rule: "invocation target class must be application, standard, or system",
                });
            }
        }
    }
    if seen_system_identities.iter().any(|seen| !seen) {
        return Err(PostgresKernelError::DurableInvariant {
            relation: RELATION,
            record: active.pair().catalogue().canonical(),
            rule: "system invocation targets must contain exactly the three admitted sealed identities",
        });
    }
    require_authorised_standard_targets(active, &targets)?;
    Ok(targets)
}

/// Fails closed unless every standard target resolves exactly once in the
/// exact verified standard snapshot pinned by the active application revision.
///
/// The active catalogue hash context already verified one standard snapshot.
/// A standard authority row must select that exact snapshot, name a function
/// present exactly once among its executables, and pin the executable revision
/// stored by that snapshot. The set of standard targets must also cover every
/// executable in that snapshot; a missing standard target fails recovery
/// closed, exactly as a duplicated or unverified one does.
pub(super) fn require_authorised_standard_targets(
    active: &ActiveDatabaseRevision,
    targets: &[SecurityFunctionTarget],
) -> Result<(), PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.invocation_target_authorities";
    let standard_targets = targets
        .iter()
        .filter(|target| target.class() == TargetClass::VerifiedStandard)
        .collect::<Vec<_>>();
    let Some(standard) = active.catalogue_hash_context().standard() else {
        if standard_targets.is_empty() {
            return Ok(());
        }
        return Err(PostgresKernelError::DurableInvariant {
            relation: RELATION,
            record: "active catalogue".to_owned(),
            rule: "standard invocation targets require a pinned verified standard snapshot",
        });
    };
    let standard_revision = standard.revision();
    let mut executable_functions = standard
        .executables()
        .iter()
        .map(|executable| (executable.function(), executable.revision().id()))
        .collect::<Vec<_>>();
    executable_functions.sort_unstable_by_key(|(function, _)| *function);
    if standard_targets.len() != executable_functions.len() {
        return Err(PostgresKernelError::DurableInvariant {
            relation: RELATION,
            record: "active catalogue".to_owned(),
            rule: "standard invocation targets must exactly match the pinned verified standard executables",
        });
    }
    for (target, (function, executable)) in standard_targets.iter().zip(executable_functions) {
        if target.function() != function
            || target.standard_revision() != Some(standard_revision)
            || target.executable_revision() != Some(executable)
        {
            return Err(PostgresKernelError::DurableInvariant {
                relation: RELATION,
                record: target.function().canonical(),
                rule: "standard invocation target must resolve exactly once in the pinned verified standard snapshot",
            });
        }
    }
    Ok(())
}

pub(super) async fn load_principals(
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

pub(super) async fn load_memberships(
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

pub(super) async fn load_grants(
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

/// Encodes one closed privilege class exactly as the pure model displays it.
pub(crate) fn encode_privilege_class(class: PrivilegeClass) -> &'static str {
    match class {
        PrivilegeClass::Execute => "execute",
        PrivilegeClass::SecurityAdmin => "security_admin",
        PrivilegeClass::Inspect(privilege) => match privilege {
            InspectPrivilege::OwnInvocation => "inspect:own-invocation",
            InspectPrivilege::SessionInvocations => "inspect:session-invocations",
            InspectPrivilege::AnyInvocation => "inspect:any-invocation",
            InspectPrivilege::Values => "inspect:values",
            InspectPrivilege::Source => "inspect:source",
            InspectPrivilege::SecurityDetails => "inspect:security-details",
            InspectPrivilege::RuntimeInternals => "inspect:runtime-internals",
        },
    }
}

/// Decodes one closed privilege-class display string from protected storage.
pub(crate) fn decode_privilege_class(value: &str) -> Result<PrivilegeClass, PostgresKernelError> {
    match value {
        "execute" => Ok(PrivilegeClass::Execute),
        "security_admin" => Ok(PrivilegeClass::SecurityAdmin),
        "inspect:own-invocation" => Ok(PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation)),
        "inspect:session-invocations" => Ok(PrivilegeClass::Inspect(
            InspectPrivilege::SessionInvocations,
        )),
        "inspect:any-invocation" => Ok(PrivilegeClass::Inspect(InspectPrivilege::AnyInvocation)),
        "inspect:values" => Ok(PrivilegeClass::Inspect(InspectPrivilege::Values)),
        "inspect:source" => Ok(PrivilegeClass::Inspect(InspectPrivilege::Source)),
        "inspect:security-details" => {
            Ok(PrivilegeClass::Inspect(InspectPrivilege::SecurityDetails))
        }
        "inspect:runtime-internals" => {
            Ok(PrivilegeClass::Inspect(InspectPrivilege::RuntimeInternals))
        }
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_privilege_grants",
            record: value.to_owned(),
            rule: "privilege class must be execute, security_admin, or one closed inspect sub-privilege",
        }),
    }
}

/// Loads the durable privilege-class grants in canonical key order.
///
/// The class-wide sentinel `''` stored in `object_id` recovers as no object.
pub(crate) async fn load_privilege_grants(
    transaction: &Transaction<'_>,
) -> Result<Vec<PrivilegeGrant>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.security_privilege_grants";
    let rows = transaction
        .query(
            "SELECT grantee_id, privilege_class, object_id
             FROM _orna_kernel.security_privilege_grants
             ORDER BY grantee_id, privilege_class, object_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut grants = Vec::with_capacity(rows.len());
    for row in &rows {
        let grantee = PrincipalId::from_bytes(exact_id(
            row,
            "grantee_id",
            "security privilege grantee identity is not exactly 16 bytes",
        )?);
        let class: String = row.try_get("privilege_class").map_err(|source| {
            row_decode(RELATION, grantee.canonical(), "privilege_class", source)
        })?;
        let class = decode_privilege_class(&class)?;
        let object: Vec<u8> = row
            .try_get("object_id")
            .map_err(|source| row_decode(RELATION, grantee.canonical(), "object_id", source))?;
        let object = if object.is_empty() {
            None
        } else {
            let function: [u8; 16] =
                object
                    .try_into()
                    .map_err(|_| PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: grantee.canonical(),
                        rule: "security privilege grant object identity is not exactly 16 bytes",
                    })?;
            Some(FunctionId::from_bytes(function))
        };
        grants.push(
            PrivilegeGrant::new(grantee, class, object).map_err(|error| {
                PostgresKernelError::DurableInvariant {
                    relation: RELATION,
                    record: grantee.canonical(),
                    rule: match error {
                        orna_core::security::PrivilegeGrantError::EmptyGrantee => {
                            "security privilege grant must carry a non-empty grantee"
                        }
                        orna_core::security::PrivilegeGrantError::EmptyObject => {
                            "security privilege grant object identity must be non-empty"
                        }
                        orna_core::security::PrivilegeGrantError::SecurityAdminObject => {
                            "security_admin privilege grant must be class-wide"
                        }
                    },
                }
            })?,
        );
    }
    Ok(grants)
}

pub(super) async fn load_local_peer_credentials(
    transaction: &Transaction<'_>,
) -> Result<Vec<LocalPeerCredential>, PostgresKernelError> {
    transaction
        .query(
            "SELECT uid, principal_id
         FROM _orna_kernel.security_local_peer_credentials
         ORDER BY uid",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(|row| {
            let stored_uid: i64 = row.try_get("uid").map_err(|source| {
                row_decode(
                    "_orna_kernel.security_local_peer_credentials",
                    "selected row".to_owned(),
                    "uid",
                    source,
                )
            })?;
            let uid =
                u32::try_from(stored_uid).map_err(|_| PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_local_peer_credentials",
                    record: stored_uid.to_string(),
                    rule: "local peer UID must fit the unsigned 32-bit range",
                })?;
            let principal = PrincipalId::from_bytes(exact_id(
                row,
                "principal_id",
                "local peer principal identity is not exactly 16 bytes",
            )?);
            Ok(LocalPeerCredential::new(uid, principal))
        })
        .collect()
}
