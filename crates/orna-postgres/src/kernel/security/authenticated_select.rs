use super::*;

#[cfg(feature = "test-hooks")]
struct AuthenticatedSelectTestBarrier {
    reached: std::sync::Arc<tokio::sync::Barrier>,
    resume: std::sync::Arc<tokio::sync::Barrier>,
}

#[cfg(feature = "test-hooks")]
async fn pause_after_authenticated_select_recovery(
    test_barrier: Option<&AuthenticatedSelectTestBarrier>,
) {
    if let Some(test_barrier) = test_barrier {
        test_barrier.reached.wait().await;
        test_barrier.resume.wait().await;
    }
}

#[cfg(not(feature = "test-hooks"))]
struct AuthenticatedSelectTestBarrier;

#[cfg(not(feature = "test-hooks"))]
async fn pause_after_authenticated_select_recovery(
    _test_barrier: Option<&AuthenticatedSelectTestBarrier>,
) {
}

impl PostgresKernel {
    /// Executes one authorised SERVER `SELECT` against one active snapshot.
    ///
    /// The operation records and commits its protected `EXECUTE` decision. It
    /// executes an allowed target through a savepoint. A target failure rolls
    /// back only that savepoint, then commits the allowed audit decision before
    /// it returns the original target error.
    pub async fn execute_authenticated_server_select(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_authenticated_server_select_with_options(
            authenticated_session,
            function,
            arguments,
            None,
            false,
        )
        .await
    }

    /// Pauses protected SERVER execution after security recovery for race proof.
    ///
    /// The hook exposes one deterministic point to the integration harness. It
    /// is absent from production builds and deliberately does not alter the
    /// transaction or decision authority.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_authenticated_server_select_with_test_barrier(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_authenticated_server_select_with_options(
            authenticated_session,
            function,
            arguments,
            Some(AuthenticatedSelectTestBarrier { reached, resume }),
            false,
        )
        .await
    }

    /// Forces driver shutdown after commit for cleanup-failure proof.
    ///
    /// The hook lets the integration harness prove that cleanup failure
    /// overrides a committed result. It is absent from production builds.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_authenticated_server_select_with_forced_post_commit_driver_shutdown(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_authenticated_server_select_with_options(
            authenticated_session,
            function,
            arguments,
            None,
            true,
        )
        .await
    }

    async fn execute_authenticated_server_select_with_options(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
        test_barrier: Option<AuthenticatedSelectTestBarrier>,
        force_post_commit_driver_shutdown: bool,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let mut transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            pause_after_authenticated_select_recovery(test_barrier.as_ref()).await;
            let target = InvocationTarget::new(function, active.pair());

            match security.authorise_execute(authenticated_session, target) {
                ExecuteDecision::Denied(reason) => {
                    append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_denied(
                            authenticated_session,
                            target,
                            reason,
                        ),
                    )
                    .await?;
                    transaction
                        .commit()
                        .await
                        .map_err(PostgresKernelError::Database)?;
                    Err(PostgresKernelError::ServerExecuteDenied {
                        pair: active.pair(),
                        function,
                        reason,
                    })
                }
                ExecuteDecision::Allowed(authorisation) => {
                    append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_allowed(&authorisation),
                    )
                    .await?;
                    let savepoint = transaction
                        .savepoint("server_select_execution")
                        .await
                        .map_err(PostgresKernelError::Database)?;
                    let execution = execute_authorised_server_select(
                        &savepoint,
                        &active,
                        &authorisation,
                        arguments,
                    )
                    .await;
                    match execution {
                        Ok(result) => {
                            savepoint
                                .commit()
                                .await
                                .map_err(PostgresKernelError::Database)?;
                            transaction
                                .commit()
                                .await
                                .map_err(PostgresKernelError::Database)?;
                            Ok(result)
                        }
                        Err(error) => {
                            savepoint
                                .rollback()
                                .await
                                .map_err(PostgresKernelError::Database)?;
                            transaction
                                .commit()
                                .await
                                .map_err(PostgresKernelError::Database)?;
                            Err(error)
                        }
                    }
                }
            }
        }
        .await;
        #[cfg(feature = "test-hooks")]
        if operation.is_ok() && force_post_commit_driver_shutdown {
            database_session.abort_driver();
        }
        #[cfg(not(feature = "test-hooks"))]
        let _ = force_post_commit_driver_shutdown;
        finish_authenticated_dispatch_session(operation, database_session.shutdown().await)
    }
}
