//! Optimistic USER-state write planning.

use super::*;

#[derive(Debug)]
pub(super) struct PendingStateWrite {
    pub(super) key: UserStateKey,
    pub(super) value: orna_core::value::RuntimeValue,
    pub(super) value_type: TypeId,
    pub(super) revision: u64,
    pub(super) existing: bool,
}

pub(super) fn plan_user_state_changes(
    changes: &[UserStateChange],
    principal: orna_core::PrincipalId,
    current_cells: &mut HashMap<UserStateKey, Option<UserStateCell>>,
) -> Result<(Vec<UserStateWriteResult>, Vec<PendingStateWrite>), PostgresKernelError> {
    reject_duplicate_user_state_keys(changes)?;
    let mut results = Vec::with_capacity(changes.len());
    let mut pending = Vec::new();
    let mut staged_cells = HashMap::<UserStateKey, UserStateCell>::new();
    let mut has_conflict = false;
    for change in changes {
        let key = change.key_without_principal().with_principal(principal);
        let current = if let Some(staged) = staged_cells.get(&key) {
            Some(staged)
        } else {
            current_cells
                .get(&key)
                .expect("write planner receives every requested key")
                .as_ref()
        };
        let result = match apply_change(current, change, principal) {
            Ok(result) => result,
            Err(error) if error.code() == Some("ORNA0902") => {
                has_conflict = true;
                continue;
            }
            Err(error) => return Err(PostgresKernelError::UserState(error)),
        };
        let UserStateWriteOutcome::Written { revision } = result.outcome() else {
            unreachable!("apply_change only returns Written outcomes")
        };
        let existing = current.is_some();
        let updated = UserStateCell::new(
            key.clone(),
            change.value().clone(),
            change.value_type(),
            revision,
            SystemTime::now(),
        );
        staged_cells.insert(key.clone(), updated);
        pending.push(PendingStateWrite {
            key,
            value: change.value().clone(),
            value_type: change.value_type(),
            revision,
            existing,
        });
        results.push(result);
    }
    if has_conflict {
        let conflicts = changes
            .iter()
            .map(|change| {
                let key = change.key_without_principal().with_principal(principal);
                let current_revision = current_cells
                    .get(&key)
                    .expect("write planner receives every requested key")
                    .as_ref()
                    .map_or(0, UserStateCell::revision);
                UserStateWriteResult::new(
                    change.key_without_principal(),
                    UserStateWriteOutcome::Conflict { current_revision },
                )
            })
            .collect();
        return Ok((conflicts, Vec::new()));
    }
    for (key, cell) in staged_cells {
        current_cells.insert(key, Some(cell));
    }
    Ok((results, pending))
}

pub(super) fn reject_duplicate_user_state_keys(
    changes: &[UserStateChange],
) -> Result<(), PostgresKernelError> {
    let mut keys = HashSet::with_capacity(changes.len());
    for change in changes {
        let key = change.key_without_principal();
        if !keys.insert(key.clone()) {
            return Err(PostgresKernelError::UserState(
                UserStateError::InvalidChange {
                    reason: format!("USER state write batch contains duplicate key {key}"),
                },
            ));
        }
    }
    Ok(())
}
