//! Authenticated PostgreSQL inspection operations and projections.

use super::*;

impl PostgresKernel {
    /// Appends one protected INSPECT denial before returning it to the caller.
    ///
    /// The audit transaction is deliberately separate from read-only inspection
    /// transactions. This keeps the denial durable even when the lookup itself
    /// is rolled back, and makes an insert or commit failure replace the denied
    /// result with the operational database error.
    pub(super) async fn append_inspect_denial_audit(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch_owner: Option<PrincipalId>,
        reason: InspectDenial,
    ) -> Result<(), PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = Box::pin(async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(false)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            append_security_audit_event(
                &transaction,
                SecurityAuditDecision::inspect_denied(authenticated_session, epoch_owner, reason),
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(())
        })
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Loads one immutable inspection epoch by its exact epoch identity.
    ///
    /// The `summary_bytes` payload decodes through the verified standard's
    /// opaque codec registry (the ORV5 pattern from the USER state kernel)
    /// and the closed epoch envelope, and must round-trip canonically and
    /// agree with the durable identity columns.
    pub async fn load_inspect_snapshot(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch_id: InspectEpochId,
    ) -> Result<Option<AuthenticatedInspectSnapshot>, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = Box::pin(async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let bound_session =
                rebind_inspect_session(self, &security, authenticated_session).await?;
            let mut granted = vec![InspectPrivilege::OwnInvocation];
            granted.extend(inspect_privileges_for_session(&security, &bound_session));
            let registry = inspect_value_registry(&active)?;
            let row = transaction
                .query_opt(INSPECT_SNAPSHOT_SELECT, &[&epoch_id.to_bytes().to_vec()])
                .await
                .map_err(PostgresKernelError::Database)?;
            let Some(row) = row else {
                drop(transaction);
                self.append_inspect_denial_audit(
                    authenticated_session,
                    None,
                    InspectDenial::MissingEpoch,
                )
                .await?;
                return Err(PostgresKernelError::InspectDenied {
                    reason: InspectDenial::MissingEpoch,
                });
            };
            let owner = PrincipalId::from_bytes(inspect_id(
                INSPECT_SNAPSHOT_RELATION,
                &row,
                epoch_id.canonical().as_str(),
                "owner_principal_id",
            )?);
            require_inspect_epoch_access(self, &bound_session, owner, &granted).await?;
            let epoch = decode_inspect_snapshot_row(&row, &active, &registry)?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(Some(AuthenticatedInspectSnapshot {
                epoch,
                session: bound_session,
                granted,
            }))
        })
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Clones an authorised inspection epoch for one trusted observer context.
    ///
    /// The clone is persisted in the same protected transaction that loads and
    /// authorises the target. It receives a fresh epoch identity and capture
    /// time, while preserving the target invocation, owner, revisions, outcome,
    /// summary, and immutable projection rows. No trace row is emitted.
    pub async fn clone_inspect_snapshot_for_observer(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch_id: InspectEpochId,
        observer_context: InspectObserverContext,
    ) -> Result<Option<AuthenticatedInspectSnapshot>, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = Box::pin(async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(false)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            lock_current_active_revision(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let bound_session =
                rebind_inspect_session(self, &security, authenticated_session).await?;
            let observer_root = observer_context
                .observer_root_invocation_id()
                .to_bytes()
                .to_vec();
            let observer_principal = bound_session.principal().to_bytes().to_vec();
            let observer_root_owned = transaction
                .query_opt(
                    "SELECT 1
                     FROM _orna_kernel.invocation_audit_events
                     WHERE invocation_id = $1
                       AND session_principal_id = $2
                       AND outcome = 'allowed'",
                    &[&observer_root, &observer_principal],
                )
                .await
                .map_err(PostgresKernelError::Database)?
                .is_some();
            if !observer_root_owned {
                drop(transaction);
                return Ok(None);
            }
            let mut granted = vec![InspectPrivilege::OwnInvocation];
            granted.extend(inspect_privileges_for_session(&security, &bound_session));
            let registry = inspect_value_registry(&active)?;
            let row = transaction
                .query_opt(INSPECT_SNAPSHOT_SELECT, &[&epoch_id.to_bytes().to_vec()])
                .await
                .map_err(PostgresKernelError::Database)?;
            let Some(row) = row else {
                drop(transaction);
                self.append_inspect_denial_audit(
                    authenticated_session,
                    None,
                    InspectDenial::MissingEpoch,
                )
                .await?;
                return Err(PostgresKernelError::InspectDenied {
                    reason: InspectDenial::MissingEpoch,
                });
            };
            let owner = PrincipalId::from_bytes(inspect_id(
                INSPECT_SNAPSHOT_RELATION,
                &row,
                epoch_id.canonical().as_str(),
                "owner_principal_id",
            )?);
            require_inspect_epoch_access(self, &bound_session, owner, &granted).await?;
            let target = decode_inspect_snapshot_row(&row, &active, &registry)?;
            let snapshot = persist_inspect_snapshot_clone(
                &transaction,
                &active,
                &registry,
                target,
                observer_context,
                bound_session,
                granted,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(Some(snapshot))
        })
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Clones an inspection epoch for the kernel-generated current invocation.
    ///
    /// This internal server bridge requires the observer root to match the
    /// dispatch invocation already authenticated by the caller. Unlike the
    /// external observer path, it does not query invocation audit ownership:
    /// the dispatch has already bound that root before entering CLIENT.
    #[doc(hidden)]
    pub async fn clone_inspect_snapshot_for_current_invocation(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch_id: InspectEpochId,
        observer_context: InspectObserverContext,
        current_invocation: InvocationId,
    ) -> Result<Option<AuthenticatedInspectSnapshot>, PostgresKernelError> {
        if observer_context.observer_root_invocation_id() != current_invocation {
            return Ok(None);
        }
        let mut database_session = self.open().await?;
        let operation = Box::pin(async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(false)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            lock_current_active_revision(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let bound_session =
                rebind_inspect_session(self, &security, authenticated_session).await?;
            let mut granted = vec![InspectPrivilege::OwnInvocation];
            granted.extend(inspect_privileges_for_session(&security, &bound_session));
            let registry = inspect_value_registry(&active)?;
            let row = transaction
                .query_opt(INSPECT_SNAPSHOT_SELECT, &[&epoch_id.to_bytes().to_vec()])
                .await
                .map_err(PostgresKernelError::Database)?;
            let Some(row) = row else {
                drop(transaction);
                self.append_inspect_denial_audit(
                    authenticated_session,
                    None,
                    InspectDenial::MissingEpoch,
                )
                .await?;
                return Err(PostgresKernelError::InspectDenied {
                    reason: InspectDenial::MissingEpoch,
                });
            };
            let owner = PrincipalId::from_bytes(inspect_id(
                INSPECT_SNAPSHOT_RELATION,
                &row,
                epoch_id.canonical().as_str(),
                "owner_principal_id",
            )?);
            require_inspect_epoch_access(self, &bound_session, owner, &granted).await?;
            let target = decode_inspect_snapshot_row(&row, &active, &registry)?;
            let snapshot = persist_inspect_snapshot_clone(
                &transaction,
                &active,
                &registry,
                target,
                observer_context,
                bound_session,
                granted,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(Some(snapshot))
        })
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Resolves one exact inspection epoch after applying the authenticated
    /// ownership/scope gate used by [`Self::find_latest_inspect_epoch`].
    ///
    /// A missing epoch fails closed with `InspectDenial::MissingEpoch`. An epoch
    /// owned by another principal fails closed with the stable INSPECT denial
    /// instead of revealing that the epoch exists.
    pub async fn find_inspect_epoch(
        &self,
        authenticated_session: &AuthenticatedSession,
        epoch_id: InspectEpochId,
    ) -> Result<Option<InspectEpochId>, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = Box::pin(async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let bound_session =
                rebind_inspect_session(self, &security, authenticated_session).await?;
            let granted = inspect_privileges_for_session(&security, &bound_session);
            let row = transaction
                .query_opt(
                    "SELECT owner_principal_id
                     FROM _orna_kernel.inspect_snapshots
                     WHERE epoch_id = $1",
                    &[&epoch_id.to_bytes().to_vec()],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            let Some(row) = row else {
                drop(transaction);
                self.append_inspect_denial_audit(
                    authenticated_session,
                    None,
                    InspectDenial::MissingEpoch,
                )
                .await?;
                return Err(PostgresKernelError::InspectDenied {
                    reason: InspectDenial::MissingEpoch,
                });
            };
            let owner = PrincipalId::from_bytes(inspect_id(
                INSPECT_SNAPSHOT_RELATION,
                &row,
                epoch_id.canonical().as_str(),
                "owner_principal_id",
            )?);
            require_inspect_epoch_access(self, &bound_session, owner, &granted).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(Some(epoch_id))
        })
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Resolves the most recent inspection epoch captured for one invocation.
    ///
    /// The lookup returns the latest epoch for the invocation (the most
    /// recently captured, breaking ties by epoch identity order). A missing
    /// epoch fails closed with `InspectDenial::MissingEpoch`. The result is
    /// gated by the INSPECT privilege ladder against the resolved epoch owner,
    /// so a caller with no privilege that reaches the epoch's scope fails
    /// closed with the closed denial reason and no epoch identity is disclosed.
    /// The sealed dispatch auto-captures one structural epoch for every
    /// completed invocation, so a completed invocation normally resolves.
    pub async fn find_latest_inspect_epoch(
        &self,
        authenticated_session: &AuthenticatedSession,
        invocation: InvocationId,
    ) -> Result<Option<InspectEpochId>, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = Box::pin(async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let bound_session =
                rebind_inspect_session(self, &security, authenticated_session).await?;
            let granted = inspect_privileges_for_session(&security, &bound_session);
            let row = transaction
                .query_opt(
                    "SELECT epoch_id, owner_principal_id
                     FROM _orna_kernel.inspect_snapshots
                     WHERE invocation_id = $1
                     ORDER BY recorded_at DESC, epoch_id DESC
                     LIMIT 1",
                    &[&invocation.to_bytes().to_vec()],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            let Some(row) = row else {
                drop(transaction);
                self.append_inspect_denial_audit(
                    authenticated_session,
                    None,
                    InspectDenial::MissingEpoch,
                )
                .await?;
                return Err(PostgresKernelError::InspectDenied {
                    reason: InspectDenial::MissingEpoch,
                });
            };
            let epoch_id = InspectEpochId::from_bytes(inspect_id(
                INSPECT_SNAPSHOT_RELATION,
                &row,
                invocation.canonical().as_str(),
                "epoch_id",
            )?);
            let owner = PrincipalId::from_bytes(inspect_id(
                INSPECT_SNAPSHOT_RELATION,
                &row,
                invocation.canonical().as_str(),
                "owner_principal_id",
            )?);
            require_inspect_epoch_access(self, &bound_session, owner, &granted).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(Some(epoch_id))
        })
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Resolves the durable, class-wide INSPECT privileges held by one
    /// authenticated session. The structural `OwnInvocation` rung is implicit
    /// for owner access and is included in the returned effective set.
    pub async fn inspect_privileges(
        &self,
        authenticated_session: &AuthenticatedSession,
    ) -> Result<Vec<InspectPrivilege>, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = Box::pin(async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let bound_session =
                rebind_inspect_session(self, &security, authenticated_session).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            let mut privileges = vec![InspectPrivilege::OwnInvocation];
            privileges.extend(inspect_privileges_for_session(&security, &bound_session));
            Ok(privileges)
        })
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Returns whether a target invocation is the observer root or one of
    /// its server-recorded descendants.
    ///
    /// Parent links come from the protected resource audit relation. The
    /// caller supplies only the current execution anchor; it cannot provide
    /// lineage or authority as data.
    pub async fn inspect_target_is_recursive(
        &self,
        observer_root: InvocationId,
        target: InvocationId,
    ) -> Result<bool, PostgresKernelError> {
        if observer_root == target {
            return Ok(true);
        }
        let mut database_session = self.open().await?;
        let operation = Box::pin(async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            establish_trusted_search_path(&transaction).await?;
            require_current_migrations(&transaction).await?;
            let row = transaction
                .query_one(
                    "WITH RECURSIVE descendants(invocation_id) AS (
                         SELECT nested_invocation_id
                         FROM _orna_kernel.resource_audit_events
                         WHERE parent_invocation_id = $1
                         UNION
                         SELECT resource.nested_invocation_id
                         FROM _orna_kernel.resource_audit_events AS resource
                         JOIN descendants
                           ON descendants.invocation_id = resource.parent_invocation_id
                     )
                     SELECT EXISTS(
                         SELECT 1 FROM descendants WHERE invocation_id = $2
                     )",
                    &[
                        &observer_root.to_bytes().to_vec(),
                        &target.to_bytes().to_vec(),
                    ],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            let recursive: bool = row.try_get(0).map_err(PostgresKernelError::Database)?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recursive)
        })
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }

    /// Returns the `invocation_nodes` projection over one epoch.
    ///
    /// The projection is gated by the INSPECT privilege ladder; a denied
    /// request fails closed with the closed denial reason.
    pub async fn inspect_invocation_nodes(
        &self,
        snapshot: &AuthenticatedInspectSnapshot,
        requested: InspectPrivilege,
    ) -> Result<Vec<InvocationNodeRow>, PostgresKernelError> {
        require_inspect_privilege(self, snapshot, requested).await?;
        Ok(snapshot.epoch.invocation_nodes().to_vec())
    }

    /// Returns the `calls` projection over one epoch.
    pub async fn inspect_calls(
        &self,
        snapshot: &AuthenticatedInspectSnapshot,
        requested: InspectPrivilege,
    ) -> Result<Vec<CallRow>, PostgresKernelError> {
        require_inspect_privilege(self, snapshot, requested).await?;
        Ok(snapshot.epoch.calls().to_vec())
    }

    /// Returns the `resources` projection over one epoch.
    /// The bounded rows are copied from the capture boundary: at most one
    /// `State`, `Catalog`, `Standard`, and `Runtime` row is emitted, each with
    /// `Active` status when the corresponding immutable fact is present.
    /// Invalidated and released statuses are never inferred from live state.
    pub async fn inspect_resources(
        &self,
        snapshot: &AuthenticatedInspectSnapshot,
        requested: InspectPrivilege,
    ) -> Result<Vec<ResourceRow>, PostgresKernelError> {
        require_inspect_privilege(self, snapshot, requested).await?;
        Ok(snapshot.epoch.resources().to_vec())
    }

    /// Returns the `state_cells` projection over one epoch.
    ///
    /// State-cell rows come from the immutable epoch payload. Capture options
    /// decide whether typed values were retained, and a caller without the
    /// `Values` classifier receives a further redacted projection.
    pub async fn inspect_state_cells(
        &self,
        snapshot: &AuthenticatedInspectSnapshot,
        requested: InspectPrivilege,
    ) -> Result<Vec<StateCellRow>, PostgresKernelError> {
        require_inspect_privilege(self, snapshot, requested).await?;
        if requested == InspectPrivilege::Values {
            return Ok(snapshot.epoch.state_cells().to_vec());
        }
        Ok(snapshot
            .epoch
            .state_cells()
            .iter()
            .map(|row| {
                StateCellRow::new(
                    row.key().clone(),
                    row.value_type(),
                    row.revision(),
                    row.updated_at(),
                    None,
                )
            })
            .collect())
    }

    /// Returns the `ui_nodes` projection over one epoch.
    /// Rows contain only the owning function, a canonical non-empty call-site,
    /// and the node's runtime-contract identity. Nodes without a call-site are
    /// omitted; properties, slots, actions, keys, source origins, and runtime
    /// handles are not projected.
    pub async fn inspect_ui_nodes(
        &self,
        snapshot: &AuthenticatedInspectSnapshot,
        requested: InspectPrivilege,
    ) -> Result<Vec<UiNodeRow>, PostgresKernelError> {
        require_inspect_privilege(self, snapshot, requested).await?;
        Ok(snapshot.epoch.ui_nodes().to_vec())
    }

    /// Returns the `presentation_candidates` projection over one epoch.
    /// At most one accepted row is copied from a successful sealed output
    /// requirement and its final event value. Failed or unresolved routes do
    /// not fabricate rejected candidates, and capture never resolves a
    /// presenter again.
    pub async fn inspect_presentation_candidates(
        &self,
        snapshot: &AuthenticatedInspectSnapshot,
        requested: InspectPrivilege,
    ) -> Result<Vec<PresentationCandidateRow>, PostgresKernelError> {
        require_inspect_privilege(self, snapshot, requested).await?;
        Ok(snapshot.epoch.presentation_candidates().to_vec())
    }

    /// Returns the `runtime_bindings` projection over one epoch.
    pub async fn inspect_runtime_bindings(
        &self,
        snapshot: &AuthenticatedInspectSnapshot,
        requested: InspectPrivilege,
    ) -> Result<Vec<RuntimeBindingRow>, PostgresKernelError> {
        require_inspect_privilege(self, snapshot, requested).await?;
        Ok(snapshot.epoch.runtime_bindings().to_vec())
    }

    /// Returns the `security_decisions` projection over one epoch.
    pub async fn inspect_security_decisions(
        &self,
        snapshot: &AuthenticatedInspectSnapshot,
        requested: InspectPrivilege,
    ) -> Result<Vec<SecurityDecisionRow>, PostgresKernelError> {
        require_inspect_privilege(self, snapshot, requested).await?;
        Ok(snapshot.epoch.security_decisions().to_vec())
    }

    /// Streams the model-expressible trace events of one invocation.
    ///
    /// The stream is gated by the same INSPECT ladder and classifier rules as
    /// the epoch projections. Without the `Values` classifier, ValueBatch
    /// events retain their sequence and count but carry no decoded schema or
    /// values. Self-observation suppression is the default: when no observer
    /// identity is supplied, the target invocation is used as the observer.
    pub async fn stream_inspect_trace(
        &self,
        snapshot: &AuthenticatedInspectSnapshot,
        requested: InspectPrivilege,
        invocation_id: InvocationId,
        after_sequence: u64,
        observer_invocation: Option<InvocationId>,
        include_observer: bool,
    ) -> Result<Vec<InspectTraceEvent>, PostgresKernelError> {
        require_inspect_privilege(self, snapshot, requested).await?;
        if snapshot.epoch.invocation_id() != invocation_id {
            return Err(PostgresKernelError::DurableInvariant {
                relation: INSPECT_TRACE_RELATION,
                record: invocation_id.canonical(),
                rule: "trace invocation must match its authorised inspection epoch",
            });
        }
        let include_values = requested.classifier() == Some(InspectClassifier::Values);
        let observer = observer_invocation.unwrap_or(invocation_id);
        let after =
            i64::try_from(after_sequence).map_err(|_| PostgresKernelError::DurableInvariant {
                relation: INSPECT_TRACE_RELATION,
                record: invocation_id.canonical(),
                rule: "trace sequence must fit PostgreSQL BIGINT",
            })?;
        let mut database_session = self.open().await?;
        let operation = Box::pin(async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let registry = inspect_value_registry(&active)?;
            // `after_sequence` is a resume cursor: 0 (the spec default) means
            // "from the start" and returns the full stream including sequence
            // 0; any positive value returns only rows strictly after it.
            let rows = if after == 0 {
                transaction
                    .query(
                        "SELECT invocation_id, sequence, kind, payload_bytes,
                                observer_invocation_id, recorded_at
                         FROM _orna_kernel.inspect_trace_events
                         WHERE invocation_id = $1
                         ORDER BY sequence",
                        &[&invocation_id.to_bytes().to_vec()],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?
            } else {
                transaction
                    .query(
                        "SELECT invocation_id, sequence, kind, payload_bytes,
                                observer_invocation_id, recorded_at
                         FROM _orna_kernel.inspect_trace_events
                         WHERE invocation_id = $1 AND sequence > $2
                         ORDER BY sequence",
                        &[&invocation_id.to_bytes().to_vec(), &after],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?
            };
            let mut events = Vec::with_capacity(rows.len());
            for row in &rows {
                let record = row_invocation_record(row)?;
                if !include_observer && record.observer_invocation == Some(observer) {
                    continue;
                }
                if !matches!(
                    record.kind.as_str(),
                    "started" | "value_batch" | "completed"
                ) {
                    // The closed v1 model carries the lifecycle payloads only;
                    // richer durable kinds remain retained for later slices.
                    continue;
                }
                let RuntimeValue::InvokeEvent(event) =
                    decode_constructed_value(&active, &registry, &record.payload_bytes)
                        .map_err(PostgresKernelError::InspectValueCodec)?
                else {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: INSPECT_TRACE_RELATION,
                        record: record.invocation.canonical(),
                        rule: "trace payload must decode as one invocation event",
                    });
                };
                if event.invocation_id() != record.invocation || event.sequence() != record.sequence
                {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: INSPECT_TRACE_RELATION,
                        record: record.invocation.canonical(),
                        rule: "trace row must agree with its canonical event payload",
                    });
                }
                let Some(payload) = model_payload_for(&record.kind, event.body()) else {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: INSPECT_TRACE_RELATION,
                        record: record.invocation.canonical(),
                        rule: "trace kind must agree with its event payload kind",
                    });
                };
                let payload = if include_values {
                    payload
                } else {
                    match payload {
                        InspectTracePayload::ValueBatch { values, .. } => {
                            let value_count = u64::try_from(values.len()).map_err(|_| {
                                PostgresKernelError::DurableInvariant {
                                    relation: INSPECT_TRACE_RELATION,
                                    record: record.invocation.canonical(),
                                    rule: "trace value count must fit BIGINT",
                                }
                            })?;
                            InspectTracePayload::ValueBatchRedacted { value_count }
                        }
                        payload => payload,
                    }
                };
                events.push(
                    InspectTraceEvent::new(
                        record.invocation,
                        record.sequence,
                        payload,
                        record.recorded_at,
                        record.observer_invocation,
                        None,
                    )
                    .map_err(PostgresKernelError::Inspect)?,
                );
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(events)
        })
        .await;
        finish_inspect_session(operation, database_session.shutdown().await)
    }
}
