use super::*;

impl PostgresKernel {
    /// Authorises and evaluates one CLIENT function against one active snapshot.
    ///
    /// The operation appends and commits the protected `EXECUTE` decision
    /// before it returns a value, a typed denial, or a pure evaluator failure.
    /// Database insertion, commit, or session shutdown failure replaces that
    /// operation result with a kernel failure.
    pub async fn evaluate_client_function(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
    ) -> Result<ClientExecutionResult, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = recover_active_revision(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let target = InvocationTarget::new(function, active.pair());
            let (decision, execution) =
                match security.authorise_execute(authenticated_session, target) {
                    ExecuteDecision::Allowed(authorisation) => {
                        let decision = SecurityAuditDecision::execute_allowed(&authorisation);
                        let execution = evaluate_authorised_client_function(
                            &active,
                            &authorisation,
                            &[],
                            &self.capability_grants,
                        )
                        .map_err(PostgresKernelError::ClientExecution);
                        (decision, execution)
                    }
                    ExecuteDecision::Denied(reason) => {
                        let decision = SecurityAuditDecision::execute_denied(
                            authenticated_session,
                            target,
                            reason,
                        );
                        let execution = Err(PostgresKernelError::ClientExecuteDenied {
                            pair: active.pair(),
                            function,
                            reason,
                        });
                        (decision, execution)
                    }
                };
            append_security_audit_event(&transaction, decision).await?;
            append_client_capability_audit(
                &transaction,
                authenticated_session,
                &active,
                target,
                &execution,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(execution)
        }
        .await;
        finish_security_session(operation, database_session.shutdown().await)?
    }
}
