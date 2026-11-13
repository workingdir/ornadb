//! Runtime-backed VFS save tests: durable saves, replay, and publication
//! behaviour through the runtime's validated table transaction.
#![allow(clippy::too_many_lines)]

use orna_repository::{CwdGeneration, Repository};
use orna_runtime::{
    NoFault, PublicationCommitId, RuntimeError, RuntimeIdentity, RuntimeState,
    RuntimeTableIdentity, RuntimeTableRows, TableActivationCandidateValidator,
    TableActivationError, TableObjectId, TableSchemaIdentity,
};
use orna_stream::{DiagnosticClass, DiagnosticCode, SafeDiagnostic};
use orna_value::{CanonicalValue, OvbRaw};
use num_bigint::BigInt;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;

    fn id(value: u8) -> [u8; 16] {
        [value; 16]
    }
    fn digest(value: u8) -> [u8; 32] {
        [value; 32]
    }
    fn git(path: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .unwrap()
                .success()
        );
    }

    fn repository() -> (TempDir, Repository) {
        let temp = TempDir::new().unwrap();
        git(temp.path(), &["init"]);
        git(temp.path(), &["config", "user.email", "kieran@drewett.dev"]);
        git(temp.path(), &["config", "user.name", "kierandrewett"]);
        git(temp.path(), &["config", "commit.gpgsign", "false"]);
        let repository = Repository::discover(temp.path()).unwrap();
        (temp, repository)
    }
    fn activation_test_schema() -> TableSchemaIdentity {
        TableSchemaIdentity::new([7; 32])
    }
    async fn open_state(repo: &Repository) -> RuntimeState {
        RuntimeState::open(
            repo,
            RuntimeIdentity {
                database_id: id(1),
                repository_id: id(2),
            },
            digest(3),
        )
        .await
        .unwrap()
    }

    /// Opens a same-worktree handle outside the owner lock. Only for tests that
    /// exercise the database compare-and-swap between handles; production
    /// second local writers are secondary and fail closed (ORNA-TURSO-005).
    /// Reads one admitted relation row through the runtime's committed table
    /// read, so the assertion observes the same durable state a reopen does.
    async fn admitted_row(
        state: &RuntimeState,
        identity: &RuntimeTableIdentity,
        key: &[u8],
    ) -> Option<Vec<u8>> {
        let snapshot = state
            .begin_admitted_table_activation(std::slice::from_ref(identity))
            .await
            .unwrap();
        snapshot
            .query_exact(identity.table(), key)
            .unwrap()
            .map(<[u8]>::to_vec)
    }

    /// A caller-owned candidate validator that accepts exactly one row value
    /// and otherwise fails the way a real schema/assertion authority would.
    struct AcceptingRowValidator {
        tables: Vec<String>,
        table_object_ids: BTreeMap<String, TableObjectId>,
        accepted: Vec<u8>,
        calls: usize,
    }

    impl TableActivationCandidateValidator for AcceptingRowValidator {
        fn tables(&self) -> &[String] {
            &self.tables
        }

        fn table_object_ids(&self) -> BTreeMap<String, TableObjectId> {
            self.table_object_ids.clone()
        }

        fn cwd_generation(&self) -> Option<&CwdGeneration> {
            None
        }

        fn schema_identity(&mut self) -> Option<TableSchemaIdentity> {
            Some(activation_test_schema())
        }

        fn validate(&mut self, rows: &RuntimeTableRows) -> Result<(), SafeDiagnostic> {
            self.calls += 1;
            let seen = rows
                .get(&self.tables[0])
                .and_then(|rows| rows.first())
                .map(|(_, value)| value.as_slice());
            if seen != Some(self.accepted.as_slice()) {
                return Err(SafeDiagnostic {
                    code: DiagnosticCode::TableAssertionFalse,
                    class: DiagnosticClass::Permanent,
                });
            }
            Ok(())
        }
    }

    fn query_test_key(value: u8) -> Vec<u8> {
        CanonicalValue::new(OvbRaw::Int(BigInt::from(value)))
            .unwrap()
            .encode()
            .unwrap()
    }

    fn publication_text_map(fields: Vec<(&str, OvbRaw)>) -> OvbRaw {
        let mut fields = fields;
        fields.sort_by(|(left, _), (right, _)| {
            left.len().cmp(&right.len()).then_with(|| left.cmp(right))
        });
        OvbRaw::Map(
            fields
                .into_iter()
                .map(|(key, value)| (OvbRaw::Text(key.into()), value))
                .collect(),
        )
    }

    /// VFS-021/VFS-009: a VFS row-document save goes through the runtime's own
    /// validated table transaction, so reopening the database sees the accepted
    /// row and never sees a refused one.
    #[tokio::test]
    async fn vfs_durable_save_survives_reopen_and_refusal_leaves_no_row() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let object_id = TableObjectId::new(id(9));
        let identity = RuntimeTableIdentity::new("books", object_id).unwrap();
        let accepted_value = CanonicalValue::new(publication_text_map(vec![
            ("id", OvbRaw::Int(BigInt::from(42_u8))),
            ("title", OvbRaw::Text("persisted".to_owned())),
            ("target", OvbRaw::Int(BigInt::from(7_u8))),
        ]))
        .unwrap();
        let accepted_row = accepted_value.encode().unwrap();
        let accepted_key = query_test_key(42);
        let repository_cache = orna_vfs::VfsRepositoryCache::new();
        let target = repository_cache
            .managed_file(Arc::new(orna_vfs::VfsFileSnapshot::new(
                orna_vfs::SnapshotPin::capture(Arc::new(0_u64)),
                Arc::<[u8]>::from(accepted_row.as_slice()),
            )))
            .await
            .unwrap();

        let draft = target.open_draft(1 << 20).await;
        draft.truncate(0).await.unwrap();
        draft.write_at(0, &accepted_row).await.unwrap();
        let save = orna_vfs::VfsDurableSave::new(
            id(4),
            activation_test_schema(),
            identity.clone(),
            accepted_key.clone(),
        );
        let accepted_key_for_admit = accepted_key.clone();
        let accepted_for_validator = accepted_row.clone();
        let identity_for_admit = identity.clone();
        let applied = target
            .commit_durable_document_save(
                &draft,
                &state,
                save,
                move |coordinates, bytes| {
                    let identity = identity_for_admit.clone();
                    let accepted = accepted_for_validator.clone();
                    let accepted_key = accepted_key_for_admit.clone();
                    async move {
                        assert_eq!(coordinates.table(), &identity);
                        assert_eq!(coordinates.key(), accepted_key.as_slice());
                        let replacement = orna_vfs::VfsTableRowReplacement::new(
                            id(7),
                            identity.clone(),
                            accepted_key,
                            Some(bytes.to_vec()),
                        )
                        .map_err(TableActivationError::Runtime)?;
                        let mut table_object_ids = BTreeMap::new();
                        table_object_ids.insert(identity.table().to_owned(), identity.object_id());
                        Ok(orna_vfs::VfsSaveCandidate::new(
                            replacement,
                            Box::new(AcceptingRowValidator {
                                tables: vec![identity.table().to_owned()],
                                table_object_ids,
                                accepted,
                                calls: 0,
                            }),
                        ))
                    }
                },
                &NoFault,
            )
            .await
            .unwrap();
        assert!(matches!(
            applied,
            orna_vfs::TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));
        assert_eq!(
            admitted_row(&state, &identity, &accepted_key)
                .await
                .as_deref(),
            Some(accepted_row.as_slice())
        );

        // An admission for one committed row cannot be redirected by the
        // candidate builder, even to another otherwise valid relation.
        let redirected_draft = target.open_draft(1 << 20).await;
        redirected_draft.truncate(0).await.unwrap();
        redirected_draft.write_at(0, &accepted_row).await.unwrap();
        let other_identity =
            RuntimeTableIdentity::new("other", TableObjectId::new(id(10))).unwrap();
        let other_for_admit = (other_identity.clone(), accepted_row.clone());
        let redirected = target
            .commit_durable_document_save(
                &redirected_draft,
                &state,
                orna_vfs::VfsDurableSave::new(
                    id(4),
                    activation_test_schema(),
                    identity.clone(),
                    accepted_key.clone(),
                ),
                move |coordinates, bytes| {
                    let (other, accepted) = other_for_admit.clone();
                    async move { admit_vfs_save(other, id(10), accepted, coordinates, bytes).await }
                },
                &NoFault,
            )
            .await;
        assert!(matches!(
            redirected,
            Err(orna_vfs::VfsSaveError::Activation(
                TableActivationError::Runtime(RuntimeError::InvalidTableMutation)
            ))
        ));
        assert_eq!(
            redirected_draft.candidate_bytes().await.unwrap().as_ref(),
            accepted_row.as_slice()
        );
        assert_eq!(
            admitted_row(&state, &identity, &accepted_key)
                .await
                .as_deref(),
            Some(accepted_row.as_slice())
        );
        assert!(
            admitted_row(&state, &other_identity, &accepted_key)
                .await
                .is_none()
        );
        assert_eq!(
            state.latest_checkpoint().await.unwrap().unwrap().generation,
            1
        );

        // The same boundary refuses a candidate the caller's authority rejects:
        // the draft retains the rejection and the accepted row stays in place.
        let refused_draft = target.open_draft(1 << 20).await;
        refused_draft.truncate(0).await.unwrap();
        refused_draft.write_at(0, b"refused").await.unwrap();
        let identity_for_refusal = identity.clone();
        let accepted_for_refusal = accepted_row.clone();
        let refusal = target
            .commit_durable_document_save(
                &refused_draft,
                &state,
                orna_vfs::VfsDurableSave::new(
                    id(4),
                    activation_test_schema(),
                    identity.clone(),
                    accepted_key.clone(),
                ),
                move |coordinates, bytes| {
                    let identity = identity_for_refusal.clone();
                    async move {
                        let replacement = orna_vfs::VfsTableRowReplacement::new(
                            id(8),
                            identity.clone(),
                            coordinates.key().to_vec(),
                            Some(bytes.to_vec()),
                        )
                        .map_err(TableActivationError::Runtime)?;
                        let mut table_object_ids = BTreeMap::new();
                        table_object_ids.insert(identity.table().to_owned(), identity.object_id());
                        Ok(orna_vfs::VfsSaveCandidate::new(
                            replacement,
                            Box::new(AcceptingRowValidator {
                                tables: vec![identity.table().to_owned()],
                                table_object_ids,
                                accepted: accepted_for_refusal,
                                calls: 0,
                            }),
                        ))
                    }
                },
                &NoFault,
            )
            .await;
        assert!(
            matches!(refusal, Err(orna_vfs::VfsSaveError::Rejected { .. })),
            "a refused candidate is reported as a retained rejection"
        );
        assert_eq!(
            refused_draft
                .retained_invalid_draft()
                .await
                .unwrap()
                .replacement_bytes()
                .unwrap(),
            b"refused"
        );
        assert_eq!(state.pending().await.unwrap().len(), 1);
        assert_eq!(
            state.latest_checkpoint().await.unwrap().unwrap().generation,
            1,
            "the rejected transaction must not append a checkpoint"
        );

        // The durable row proves the save crossed the runtime transaction: a
        // fresh runtime over the same repository reads it back, and the refused
        // candidate never replaced it.
        drop(state);
        let reopened = open_state(&repo).await;
        assert_eq!(
            admitted_row(&reopened, &identity, &accepted_key)
                .await
                .as_deref(),
            Some(accepted_row.as_slice()),
            "an accepted save is visible to a reopen of the same database"
        );
        assert_eq!(
            CanonicalValue::decode(
                &admitted_row(&reopened, &identity, &accepted_key)
                    .await
                    .unwrap()
            )
            .unwrap(),
            accepted_value,
            "the persisted row still decodes to the accepted typed value"
        );
        assert_eq!(
            reopened
                .latest_checkpoint()
                .await
                .unwrap()
                .expect("an accepted save publishes its checkpoint")
                .generation,
            1
        );
    }

    /// ORNA-VFS-008/VFS-021: a VFS save whose durable commit succeeded but
    /// whose acknowledgement was lost is not committed a second time. The
    /// replay of the identical candidate publishes the accepted image and
    /// leaves exactly one durable mutation and one checkpoint behind, so one
    /// revision is never admitted twice.
    #[tokio::test]
    async fn replaying_an_already_durable_vfs_save_does_not_commit_twice() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let object_id = TableObjectId::new(id(11));
        let identity = RuntimeTableIdentity::new("books", object_id).unwrap();
        let accepted_row = b"saved".to_vec();
        let repository_cache = orna_vfs::VfsRepositoryCache::new();
        let target = repository_cache
            .managed_file(Arc::new(orna_vfs::VfsFileSnapshot::new(
                orna_vfs::SnapshotPin::capture(Arc::new(0_u64)),
                Arc::<[u8]>::from(b"draft".as_slice()),
            )))
            .await
            .unwrap();
        let durable_save = || {
            orna_vfs::VfsDurableSave::new(
                id(4),
                activation_test_schema(),
                identity.clone(),
                vec![1],
            )
        };
        // Both saves build the same candidate, so the second one is the replay
        // of a save the relation already holds.
        let admit = |mutation_id: [u8; 16], identity: RuntimeTableIdentity, accepted: Vec<u8>| {
            move |coordinates: orna_vfs::VfsSaveCoordinates, bytes: Arc<[u8]>| async move {
                let replacement = orna_vfs::VfsTableRowReplacement::new(
                    mutation_id,
                    identity.clone(),
                    coordinates.key().to_vec(),
                    Some(bytes.to_vec()),
                )
                .map_err(TableActivationError::Runtime)?;
                let mut table_object_ids = BTreeMap::new();
                table_object_ids.insert(identity.table().to_owned(), identity.object_id());
                Ok(orna_vfs::VfsSaveCandidate::new(
                    replacement,
                    Box::new(AcceptingRowValidator {
                        tables: vec![identity.table().to_owned()],
                        table_object_ids,
                        accepted,
                        calls: 0,
                    }),
                ))
            }
        };

        let first_draft = target.open_draft(1 << 20).await;
        first_draft.truncate(0).await.unwrap();
        first_draft.write_at(0, &accepted_row).await.unwrap();
        let applied = target
            .commit_durable_document_save(
                &first_draft,
                &state,
                durable_save(),
                admit(id(7), identity.clone(), accepted_row.clone()),
                &NoFault,
            )
            .await
            .unwrap();
        assert!(matches!(
            applied,
            orna_vfs::TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));
        assert_eq!(
            state
                .latest_checkpoint()
                .await
                .unwrap()
                .expect("the save publishes its checkpoint")
                .generation,
            1
        );

        // The replay carries the same candidate bytes. The relation already
        // holds them, so the accepted image is published again but no second
        // mutation or checkpoint is appended.
        let replay_draft = target.open_draft(1 << 20).await;
        replay_draft.truncate(0).await.unwrap();
        replay_draft.write_at(0, &accepted_row).await.unwrap();
        let replayed = target
            .commit_durable_document_save(
                &replay_draft,
                &state,
                durable_save(),
                admit(id(7), identity.clone(), accepted_row.clone()),
                &NoFault,
            )
            .await
            .unwrap();
        assert!(matches!(
            replayed,
            orna_vfs::TemporaryRenameOutcome::Applied { generation: 2, .. }
        ));
        assert_eq!(
            state
                .latest_checkpoint()
                .await
                .unwrap()
                .expect("the acknowledged save keeps its checkpoint")
                .generation,
            1,
            "the replay must not advance the durable generation"
        );
        assert_eq!(
            state.pending().await.unwrap().len(),
            1,
            "the replay must not append a second durable mutation"
        );
        assert_eq!(
            admitted_row(&state, &identity, &[1]).await.as_deref(),
            Some(accepted_row.as_slice()),
            "the replay leaves the accepted row in place"
        );

        // A different candidate is a new revision, not a replay: it commits.
        let changed_draft = target.open_draft(1 << 20).await;
        changed_draft.truncate(0).await.unwrap();
        changed_draft.write_at(0, b"changed").await.unwrap();
        let changed = target
            .commit_durable_document_save(
                &changed_draft,
                &state,
                durable_save(),
                admit(id(8), identity.clone(), b"changed".to_vec()),
                &NoFault,
            )
            .await
            .unwrap();
        assert!(matches!(
            changed,
            orna_vfs::TemporaryRenameOutcome::Applied { generation: 3, .. }
        ));
        assert_eq!(
            state
                .latest_checkpoint()
                .await
                .unwrap()
                .expect("a changed candidate commits")
                .generation,
            2,
            "a changed candidate is a new revision and does commit"
        );
    }

    /// Publishes the pending mutation of an accepted VFS save through the
    /// worktree publication writer, leaving the checkpoint consumed.
    async fn publish_saved_generation(
        state: &RuntimeState,
        repo: &orna_repository::Repository,
        intent: [u8; 16],
    ) {
        let checkpoint = state
            .latest_checkpoint()
            .await
            .unwrap()
            .expect("the save publishes its checkpoint");
        let freeze = state
            .publication_writer(repo)
            .unwrap()
            .freeze(intent, &checkpoint)
            .await
            .unwrap();
        state
            .publication_writer(repo)
            .unwrap()
            .complete_publication(&freeze, &PublicationCommitId::new(vec![b'a'; 40]).unwrap())
            .await
            .unwrap();
    }

    /// ORNA-VFS-008: publication prunes the pending mutation record. An exact
    /// replay of an already-accepted candidate after publication must not
    /// advance the durable generation or re-admit the mutation. This asserts
    /// durable state only; the replay outcome shape is the open gap covered by
    /// the ignored test below (SPEC-QUESTIONS Q2).
    #[tokio::test]
    async fn replaying_a_published_vfs_save_keeps_the_durable_generation() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let object_id = TableObjectId::new(id(11));
        let identity = RuntimeTableIdentity::new("books", object_id).unwrap();
        let accepted_row = b"saved".to_vec();
        let repository_cache = orna_vfs::VfsRepositoryCache::new();
        let target = repository_cache
            .managed_file(Arc::new(orna_vfs::VfsFileSnapshot::new(
                orna_vfs::SnapshotPin::capture(Arc::new(0_u64)),
                Arc::<[u8]>::from(b"draft".as_slice()),
            )))
            .await
            .unwrap();
        let durable_save = || {
            orna_vfs::VfsDurableSave::new(
                id(4),
                activation_test_schema(),
                identity.clone(),
                vec![1],
            )
        };
        let admit = |mutation_id: [u8; 16], identity: RuntimeTableIdentity, accepted: Vec<u8>| {
            move |coordinates: orna_vfs::VfsSaveCoordinates, bytes: Arc<[u8]>| async move {
                let replacement = orna_vfs::VfsTableRowReplacement::new(
                    mutation_id,
                    identity.clone(),
                    coordinates.key().to_vec(),
                    Some(bytes.to_vec()),
                )
                .map_err(TableActivationError::Runtime)?;
                let mut table_object_ids = BTreeMap::new();
                table_object_ids.insert(identity.table().to_owned(), identity.object_id());
                Ok(orna_vfs::VfsSaveCandidate::new(
                    replacement,
                    Box::new(AcceptingRowValidator {
                        tables: vec![identity.table().to_owned()],
                        table_object_ids,
                        accepted,
                        calls: 0,
                    }),
                ))
            }
        };

        let first_draft = target.open_draft(1 << 20).await;
        first_draft.truncate(0).await.unwrap();
        first_draft.write_at(0, &accepted_row).await.unwrap();
        let applied = target
            .commit_durable_document_save(
                &first_draft,
                &state,
                durable_save(),
                admit(id(7), identity.clone(), accepted_row.clone()),
                &NoFault,
            )
            .await
            .unwrap();
        assert!(matches!(
            applied,
            orna_vfs::TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));

        // Publication consumes the pending mutation record of the accepted save.
        publish_saved_generation(&state, &repo, id(9)).await;
        assert!(state.pending().await.unwrap().is_empty());

        // The replay of the identical candidate arrives after publication.
        let replay_draft = target.open_draft(1 << 20).await;
        replay_draft.truncate(0).await.unwrap();
        replay_draft.write_at(0, &accepted_row).await.unwrap();
        let replayed = target
            .commit_durable_document_save(
                &replay_draft,
                &state,
                durable_save(),
                admit(id(7), identity.clone(), accepted_row.clone()),
                &NoFault,
            )
            .await
            .unwrap();
        // The outcome of a post-publication replay is deliberately not asserted
        // here. The outcome property lives in the ignored twin
        // `replaying_a_published_vfs_save_reports_the_idempotent_outcome`
        // (ornadb-5il1.354). Only the durable invariants below are claimed.
        let _ = replayed;
        assert_eq!(
            state
                .latest_checkpoint()
                .await
                .unwrap()
                .expect("the acknowledged save keeps its checkpoint")
                .generation,
            1,
            "a post-publication replay must not advance the durable generation"
        );
        assert!(
            state.pending().await.unwrap().is_empty(),
            "a post-publication replay must not re-admit the mutation"
        );
        assert_eq!(
            admitted_row(&state, &identity, &[1]).await.as_deref(),
            Some(accepted_row.as_slice()),
            "the replay leaves the accepted row in place"
        );
    }

    /// ORNA-VFS-008 outcome property (open gap, bead ornadb-5il1.354): publication
    /// prunes the pending mutation record, and an exact replay of an accepted
    /// candidate after publication must report the already-durable generation,
    /// not a second acceptance. Ignored until the outcome shape is fixed; the
    /// active test above asserts only durable state.
    #[ignore = "ornadb-5il1.354: ORNA-VFS-008 outcome shape (SPEC-QUESTIONS Q2)"]
    #[tokio::test]
    async fn replaying_a_published_vfs_save_reports_the_idempotent_outcome() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let object_id = TableObjectId::new(id(11));
        let identity = RuntimeTableIdentity::new("books", object_id).unwrap();
        let accepted_row = b"saved".to_vec();
        let repository_cache = orna_vfs::VfsRepositoryCache::new();
        let target = repository_cache
            .managed_file(Arc::new(orna_vfs::VfsFileSnapshot::new(
                orna_vfs::SnapshotPin::capture(Arc::new(0_u64)),
                Arc::<[u8]>::from(b"draft".as_slice()),
            )))
            .await
            .unwrap();
        let durable_save = || {
            orna_vfs::VfsDurableSave::new(
                id(4),
                activation_test_schema(),
                identity.clone(),
                vec![1],
            )
        };
        let admit = |mutation_id: [u8; 16], identity: RuntimeTableIdentity, accepted: Vec<u8>| {
            move |coordinates: orna_vfs::VfsSaveCoordinates, bytes: Arc<[u8]>| async move {
                let replacement = orna_vfs::VfsTableRowReplacement::new(
                    mutation_id,
                    identity.clone(),
                    coordinates.key().to_vec(),
                    Some(bytes.to_vec()),
                )
                .map_err(TableActivationError::Runtime)?;
                let mut table_object_ids = BTreeMap::new();
                table_object_ids.insert(identity.table().to_owned(), identity.object_id());
                Ok(orna_vfs::VfsSaveCandidate::new(
                    replacement,
                    Box::new(AcceptingRowValidator {
                        tables: vec![identity.table().to_owned()],
                        table_object_ids,
                        accepted,
                        calls: 0,
                    }),
                ))
            }
        };

        let first_draft = target.open_draft(1 << 20).await;
        first_draft.truncate(0).await.unwrap();
        first_draft.write_at(0, &accepted_row).await.unwrap();
        let applied = target
            .commit_durable_document_save(
                &first_draft,
                &state,
                durable_save(),
                admit(id(7), identity.clone(), accepted_row.clone()),
                &NoFault,
            )
            .await
            .unwrap();
        assert!(matches!(
            applied,
            orna_vfs::TemporaryRenameOutcome::Applied { generation: 1, .. }
        ));

        // Publication consumes the pending mutation record of the accepted save.
        publish_saved_generation(&state, &repo, id(9)).await;
        assert!(state.pending().await.unwrap().is_empty());

        // The replay of the identical candidate arrives after publication.
        let replay_draft = target.open_draft(1 << 20).await;
        replay_draft.truncate(0).await.unwrap();
        replay_draft.write_at(0, &accepted_row).await.unwrap();
        let replayed = target
            .commit_durable_document_save(
                &replay_draft,
                &state,
                durable_save(),
                admit(id(7), identity.clone(), accepted_row.clone()),
                &NoFault,
            )
            .await
            .unwrap();
        assert!(
            matches!(
                replayed,
                orna_vfs::TemporaryRenameOutcome::Applied { generation: 1, .. }
            ),
            "a post-publication replay reports the already-durable generation (ORNA-VFS-008)"
        );
        assert_eq!(
            state
                .latest_checkpoint()
                .await
                .unwrap()
                .expect("the acknowledged save keeps its checkpoint")
                .generation,
            1,
            "a post-publication replay must not advance the durable generation"
        );
        assert!(
            state.pending().await.unwrap().is_empty(),
            "a post-publication replay must not re-admit the mutation"
        );
        assert_eq!(
            admitted_row(&state, &identity, &[1]).await.as_deref(),
            Some(accepted_row.as_slice()),
            "the replay leaves the accepted row in place"
        );
    }

    /// Reusing a retained mutation identity after publication with different
    /// bytes is caller error: it must be rejected as invalid input, not reported
    /// as storage unavailability, and must leave the accepted row unchanged.
    #[tokio::test]
    async fn reusing_a_published_mutation_id_with_new_bytes_is_invalid() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let object_id = TableObjectId::new(id(11));
        let identity = RuntimeTableIdentity::new("books", object_id).unwrap();
        let repository_cache = orna_vfs::VfsRepositoryCache::new();
        let target = repository_cache
            .managed_file(Arc::new(orna_vfs::VfsFileSnapshot::new(
                orna_vfs::SnapshotPin::capture(Arc::new(0_u64)),
                Arc::<[u8]>::from(b"draft".as_slice()),
            )))
            .await
            .unwrap();
        let durable_save = || {
            orna_vfs::VfsDurableSave::new(
                id(4),
                activation_test_schema(),
                identity.clone(),
                vec![1],
            )
        };
        let admit = |mutation_id: [u8; 16], accepted: Vec<u8>| {
            let identity = identity.clone();
            move |coordinates: orna_vfs::VfsSaveCoordinates, bytes: Arc<[u8]>| async move {
                let replacement = orna_vfs::VfsTableRowReplacement::new(
                    mutation_id,
                    identity.clone(),
                    coordinates.key().to_vec(),
                    Some(bytes.to_vec()),
                )
                .map_err(TableActivationError::Runtime)?;
                let mut table_object_ids = BTreeMap::new();
                table_object_ids.insert(identity.table().to_owned(), identity.object_id());
                Ok(orna_vfs::VfsSaveCandidate::new(
                    replacement,
                    Box::new(AcceptingRowValidator {
                        tables: vec![identity.table().to_owned()],
                        table_object_ids,
                        accepted,
                        calls: 0,
                    }),
                ))
            }
        };
        let first_draft = target.open_draft(1 << 20).await;
        first_draft.truncate(0).await.unwrap();
        first_draft.write_at(0, b"saved").await.unwrap();
        target
            .commit_durable_document_save(
                &first_draft,
                &state,
                durable_save(),
                admit(id(7), b"saved".to_vec()),
                &NoFault,
            )
            .await
            .unwrap();
        let checkpoint = state
            .latest_checkpoint()
            .await
            .unwrap()
            .expect("the save publishes its checkpoint");
        let freeze = state
            .publication_writer(&repo)
            .unwrap()
            .freeze(id(9), &checkpoint)
            .await
            .unwrap();
        state
            .publication_writer(&repo)
            .unwrap()
            .complete_publication(&freeze, &PublicationCommitId::new(vec![b'a'; 40]).unwrap())
            .await
            .unwrap();

        let reused_draft = target.open_draft(1 << 20).await;
        reused_draft.truncate(0).await.unwrap();
        reused_draft.write_at(0, b"changed").await.unwrap();
        let reused = target
            .commit_durable_document_save(
                &reused_draft,
                &state,
                durable_save(),
                admit(id(7), b"changed".to_vec()),
                &NoFault,
            )
            .await;
        assert!(
            matches!(
                reused,
                Err(orna_vfs::VfsSaveError::Activation(
                    TableActivationError::Runtime(RuntimeError::InvalidTableMutation)
                ))
            ),
            "id reuse with different bytes must be InvalidTableMutation"
        );
        assert!(state.pending().await.unwrap().is_empty());
        assert_eq!(
            admitted_row(&state, &identity, &[1]).await.as_deref(),
            Some(b"saved".as_slice()),
            "the rejected reuse leaves the accepted row in place"
        );
    }

    /// One complete VFS row save candidate for `identity`, exactly as a caller's
    /// admission callback must build it: the typed row replacement plus the
    /// validator that carries the relation's schema authority (VFS-002).
    async fn admit_vfs_save(
        identity: RuntimeTableIdentity,
        mutation_id: [u8; 16],
        accepted: Vec<u8>,
        coordinates: orna_vfs::VfsSaveCoordinates,
        bytes: Arc<[u8]>,
    ) -> Result<orna_vfs::VfsSaveCandidate, TableActivationError> {
        let replacement = orna_vfs::VfsTableRowReplacement::new(
            mutation_id,
            identity.clone(),
            coordinates.key().to_vec(),
            Some(bytes.to_vec()),
        )
        .map_err(TableActivationError::Runtime)?;
        let mut table_object_ids = BTreeMap::new();
        table_object_ids.insert(identity.table().to_owned(), identity.object_id());
        Ok(orna_vfs::VfsSaveCandidate::new(
            replacement,
            Box::new(AcceptingRowValidator {
                tables: vec![identity.table().to_owned()],
                table_object_ids,
                accepted,
                calls: 0,
            }),
        ))
    }

    /// Gate E: two editors save one row from the same captured baseline. The
    /// first save commits and stays committed; the second, whose baseline the
    /// first save superseded, is refused with the typed stale-baseline
    /// diagnostic, keeps its own bytes in its retained draft, and leaves the
    /// accepted row and the durable generation exactly as the first save left
    /// them (ORNA-VFS-008, VFS-009, VFS-011).
    #[tokio::test]
    async fn two_vfs_saves_from_one_stale_baseline_refuse_only_the_second() {
        let (_temp, repo) = repository();
        let state = open_state(&repo).await;
        let identity = RuntimeTableIdentity::new("books", TableObjectId::new(id(11))).unwrap();
        let first_row = b"saved-a".to_vec();
        let second_row = b"saved-b".to_vec();
        let repository_cache = orna_vfs::VfsRepositoryCache::new();
        let target = repository_cache
            .managed_file(Arc::new(orna_vfs::VfsFileSnapshot::new(
                orna_vfs::SnapshotPin::capture(Arc::new(3_u64)),
                Arc::<[u8]>::from(b"draft".as_slice()),
            )))
            .await
            .unwrap();
        // The profile pairs the stale-edit cause with EAGAIN, so the host errno
        // and the Orna cause a refusal reports are one story (VFS-011).
        assert_eq!(
            orna_vfs::VfsPathError::StaleBaseline.errno(),
            orna_vfs::VFS_EAGAIN
        );
        assert_eq!(
            orna_vfs::VfsPathError::StaleBaseline.failure_code(),
            orna_vfs::vfs_stale_failure_code(DiagnosticCode::StaleBaseline)
        );

        let save = || {
            orna_vfs::VfsDurableSave::new(
                id(4),
                activation_test_schema(),
                identity.clone(),
                vec![1],
            )
        };
        // Both editors open from the same captured baseline before either save
        // lands, which is what makes the loser stale rather than a replay.
        let first_draft = target.open_draft(1 << 20).await;
        first_draft.truncate(0).await.unwrap();
        first_draft.write_at(0, &first_row).await.unwrap();
        let second_draft = target.open_draft(1 << 20).await;
        second_draft.truncate(0).await.unwrap();
        second_draft.write_at(0, &second_row).await.unwrap();

        let first = target
            .commit_durable_document_save(
                &first_draft,
                &state,
                save(),
                |coordinates, bytes| {
                    admit_vfs_save(
                        identity.clone(),
                        id(7),
                        first_row.clone(),
                        coordinates,
                        bytes,
                    )
                },
                &NoFault,
            )
            .await;
        assert!(matches!(
            first,
            Ok(orna_vfs::TemporaryRenameOutcome::Applied { generation: 1, .. })
        ));
        assert_eq!(
            admitted_row(&state, &identity, &[1]).await.as_deref(),
            Some(first_row.as_slice())
        );

        // The second editor's baseline is no longer the destination's image, so
        // its save is a stale edit and the runtime is never asked to admit it.
        let refusal = target
            .commit_durable_document_save(
                &second_draft,
                &state,
                save(),
                |coordinates, bytes| {
                    admit_vfs_save(
                        identity.clone(),
                        id(7),
                        second_row.clone(),
                        coordinates,
                        bytes,
                    )
                },
                &NoFault,
            )
            .await;
        let Err(orna_vfs::VfsSaveError::Stale {
            generation,
            diagnostic,
        }) = refusal
        else {
            panic!("a save from a superseded baseline must be refused as stale");
        };
        assert_eq!(
            generation, 1,
            "the refusal reports the generation it lost to"
        );
        assert_eq!(diagnostic.code, DiagnosticCode::StaleBaseline);
        assert_eq!(diagnostic.class, DiagnosticClass::Transient);
        assert_eq!(
            orna_vfs::vfs_stale_failure_code(diagnostic.code),
            Some("sys.vfs.stale_edit")
        );

        // The refused save keeps its own bytes and diagnostic, and the first
        // save's accepted row and durable generation are untouched.
        let retained = second_draft
            .retained_invalid_draft()
            .await
            .expect("the refused save retains its draft");
        assert_eq!(retained.replacement_bytes().unwrap(), second_row.as_slice());
        assert_eq!(retained.diagnostic(), diagnostic);
        assert_eq!(
            admitted_row(&state, &identity, &[1]).await.as_deref(),
            Some(first_row.as_slice()),
            "the refused save leaves the first save's row in place"
        );
        assert_eq!(
            state
                .latest_checkpoint()
                .await
                .unwrap()
                .expect("the first save keeps its checkpoint")
                .generation,
            1,
            "the refused save appends no checkpoint"
        );
        assert_eq!(
            state.pending().await.unwrap().len(),
            1,
            "the refused save appends no durable mutation"
        );
    }

