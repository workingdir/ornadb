use super::{
    CheckpointKey, GitCommitRef, Repository, RuntimeError, RuntimeState, StreamCheckpoint,
    StreamHandler, StreamSource, StreamStep, StreamStepError, WriterLease,
    ensure_stream_checkpoint, load_stream_checkpoint, load_stream_lease,
    read_checkpoint_snapshot_watermark, stream_key_id,
};
use libsql::{TransactionBehavior, params};

impl RuntimeState {
    /// Bootstraps from the selected immutable fetched commit and performs the
    /// first provider poll with that checkpoint before any local tail can be
    /// observed by the source.
    pub async fn run_stream_once_from_selected_snapshot<S, H>(
        &self,
        writer: WriterLease,
        repository: &Repository,
        selected_commit: &GitCommitRef,
        key: &CheckpointKey,
        source: &mut S,
        handler: &mut H,
    ) -> Result<StreamStep, StreamStepError>
    where
        S: StreamSource,
        H: StreamHandler,
    {
        self.bootstrap_stream_from_selected_snapshot(writer, repository, selected_commit, key)
            .await
            .map_err(StreamStepError::Runtime)?;
        self.run_stream_once(writer, key, source, handler).await
    }

    /// Seeds a stream from the watermark in the caller's selected immutable
    /// fetched commit. Call this before the first provider poll for that
    /// selection. Repeating the same selection is safe after the stream has
    /// advanced; a different selection installs that snapshot's watermark.
    pub async fn bootstrap_stream_from_selected_snapshot(
        &self,
        writer: WriterLease,
        repository: &Repository,
        selected_commit: &GitCommitRef,
        key: &CheckpointKey,
    ) -> Result<StreamCheckpoint, RuntimeError> {
        let selected_checkpoint =
            read_checkpoint_snapshot_watermark(repository, selected_commit, key)?
                .map(|watermark| watermark.checkpoint)
                .unwrap_or_else(|| StreamCheckpoint {
                    key: key.clone(),
                    version: 0,
                    committed: None,
                });

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        self.require_owner(&transaction, writer).await?;
        ensure_stream_checkpoint(&transaction, key).await?;

        let key_id = stream_key_id(key);
        let mut rows = transaction
            .query(
                "SELECT selected_commit FROM stream_checkpoint_bootstrap WHERE key_id = ?1",
                params![key_id.clone()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        let already_selected = rows
            .next()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?
            .map(|row| {
                row.get::<String>(0)
                    .map_err(|_| RuntimeError::RecoveryInvalid)
            })
            .transpose()?;
        if already_selected.as_deref() == Some(selected_commit.as_str()) {
            let checkpoint = load_stream_checkpoint(&transaction, key).await?;
            transaction
                .commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            return Ok(checkpoint);
        }

        if load_stream_lease(&transaction, key).await?.is_some() {
            return Err(RuntimeError::AdminBusy);
        }

        transaction
            .execute(
                "UPDATE stream_checkpoint
                 SET version = ?2, committed_position = ?3
                 WHERE key_id = ?1",
                params![
                    key_id.clone(),
                    i64::try_from(selected_checkpoint.version)
                        .map_err(|_| RuntimeError::RecoveryInvalid)?,
                    selected_checkpoint
                        .committed
                        .as_ref()
                        .map(|position| position.token.as_str().to_owned()),
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .execute(
                "DELETE FROM stream_checkpoint_history WHERE key_id = ?1",
                params![key_id.clone()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .execute(
                "DELETE FROM stream_provider_failure WHERE key_id = ?1",
                params![key_id.clone()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        transaction
            .execute(
                "INSERT INTO stream_checkpoint_bootstrap (key_id, selected_commit)
                 VALUES (?1, ?2)
                 ON CONFLICT(key_id) DO UPDATE SET selected_commit = excluded.selected_commit",
                params![key_id, selected_commit.as_str().to_owned()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;

        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(selected_checkpoint)
    }
}
