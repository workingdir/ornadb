use super::*;

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

    /// Recovers protected security audit history in database sequence order.
    pub async fn recover_security_audit_events(
        &self,
    ) -> Result<Vec<SecurityAuditEvent>, PostgresKernelError> {
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
            establish_trusted_search_path(&transaction).await?;
            require_current_migrations(&transaction).await?;
            let events = load_security_audit_events(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(events)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }
}
