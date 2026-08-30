use super::*;

impl PostgresKernel {
    /// Authenticates a kernel-supplied Linux peer UID with no selected roles.
    ///
    /// The operation appends and commits one protected audit record before it
    /// returns either the authenticated session or an expected typed denial.
    /// Database insertion, commit, or session shutdown failure replaces the
    /// authentication result with a kernel failure.
    pub async fn authenticate_local_peer(
        &self,
        uid: u32,
    ) -> Result<AuthenticatedSession, PostgresKernelError> {
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
            let security = recover_security_snapshot(&transaction).await?;
            let mapped_principal = security
                .local_peer_credentials()
                .find(|credential| credential.uid() == uid)
                .map(LocalPeerCredential::principal);
            let authentication = security.authenticate_local_peer(uid);
            let decision = match &authentication {
                Ok(session) => SecurityAuditDecision::authentication_allowed(session),
                Err(reason) => SecurityAuditDecision::authentication_denied(
                    mapped_principal,
                    *reason,
                )
                .map_err(|_| PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel security snapshot",
                    record: "local peer authentication".to_owned(),
                    rule: "mapped principal evidence must agree with the authentication result",
                })?,
            };
            append_security_audit_event(&transaction, decision).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(authentication)
        }
        .await;
        finish_security_session(operation, database_session.shutdown().await)?
            .map_err(PostgresKernelError::LocalPeerAuthentication)
    }
}
