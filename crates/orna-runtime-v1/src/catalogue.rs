//! Durable current-runtime catalogue identity and revision admission.
//!
//! This module is the v1 runtime's identity authority.  It allocates an
//! opaque ObjectId only when a declaration is first admitted, then records the
//! declaration's kind, name, semantic revision, and snapshot together.  A
//! caller supplies names and semantic hashes; it never supplies an ObjectRef
//! or an ObjectId to this boundary.

use libsql::{Connection, Transaction, params};
use orna_foundation_v1::{
    FunctionRef, ObjectRef, Snapshot, TypeRef, Value, function_reference, object_reference,
    type_reference, validate_type_reference,
};
use uuid::Uuid;

use crate::{
    RuntimeError as RuntimeFailure, RuntimeState, capture_tx, fixed, now_ms,
    validate_observation_text,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogueError {
    CatalogueCorrupt,
    CatalogueKindMismatch,
    CatalogueNameConflict,
    CatalogueRevisionConflict,
    CatalogueRenameSourceMissing,
    CataloguePredecessorRequired,
    CataloguePredecessorInvalid,
    CatalogueCaptureInvalid,
    CatalogueTypeMismatch,
    CatalogueTypeMissing,
    CatalogueFunctionMissing,
    CatalogueParameterConflict,
    CatalogueIdentityMissing,
    CatalogueAllocationExhausted,
    InvalidObservationReference,
    StorageUnavailable,
    StaleCapture {
        current: Box<orna_foundation_v1::CwdCapture>,
    },
    RuntimeFailure,
}

impl From<RuntimeFailure> for CatalogueError {
    fn from(error: RuntimeFailure) -> Self {
        match error {
            RuntimeFailure::StaleCapture { current } => Self::StaleCapture { current },
            _ => Self::RuntimeFailure,
        }
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use crate::{NoFault, RuntimeIdentity, TableMutation};
    use orna_foundation_v1::CwdCapture;
    use tempfile::tempdir;

    fn digest(value: u8) -> [u8; 32] {
        [value; 32]
    }

    fn declaration(
        name: &str,
        kind: CatalogueObjectKind,
        revision: u8,
        semantic: u8,
        rename_from: Option<&str>,
    ) -> CatalogueDeclaration {
        CatalogueDeclaration {
            qualified_name: name.into(),
            kind,
            revision_id: digest(revision),
            semantic_hash: digest(semantic),
            rename_from: rename_from.map(str::to_owned),
        }
    }

    fn type_declaration(
        name: &str,
        revision: u8,
        semantic: u8,
        form: CatalogueTypeSpec,
        rename_from: Option<&str>,
    ) -> CatalogueTypeDeclaration {
        CatalogueTypeDeclaration {
            declaration: declaration(
                name,
                CatalogueObjectKind::Type,
                revision,
                semantic,
                rename_from,
            ),
            form,
        }
    }

    fn function_declaration(
        parameters: Vec<CatalogueParameterDeclaration>,
        result: &str,
    ) -> CatalogueFunctionDeclaration {
        CatalogueFunctionDeclaration {
            declaration: declaration("pkg.f", CatalogueObjectKind::Function, 12, 13, None),
            parameters,
            result_type_name: result.into(),
        }
    }

    async fn state() -> (tempfile::TempDir, RuntimeState) {
        let directory = tempdir().unwrap();
        let state = RuntimeState::open_path(
            &directory.path().join("state.db"),
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            digest(3),
            None,
        )
        .await
        .unwrap();
        (directory, state)
    }

    async fn admit(
        state: &RuntimeState,
        writer: crate::WriterLease,
        admission: CatalogueAdmission,
    ) -> Result<CatalogueAdmissionResult, CatalogueError> {
        let capture = capture_tx(&state.connection).await?;
        state.admit_catalogue_at(writer, &capture, admission).await
    }

    async fn admit_without_explicit_lease(
        state: &RuntimeState,
        admission: CatalogueAdmission,
    ) -> Result<CatalogueAdmissionResult, CatalogueError> {
        let writer = state.acquire_lease([15; 16]).await.unwrap();
        admit(state, writer, admission).await
    }

    async fn advance(state: &RuntimeState, lease: crate::WriterLease, value: u8) -> CwdCapture {
        let context = state.begin_activation().await.unwrap();
        let mutation = TableMutation::new(
            [value; 16],
            "catalogue-test",
            vec![value],
            Some(vec![value]),
        )
        .unwrap();
        state
            .commit_table_activation(lease, &context, &[mutation], digest(value), &NoFault)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn exact_type_form_is_persisted_and_reference_is_not_named_type() {
        let (_directory, state) = state().await;
        let result = admit_without_explicit_lease(
            &state,
            CatalogueAdmission {
                predecessor_capture: None,
                types: vec![
                    type_declaration("pkg.T", 4, 5, CatalogueTypeSpec::Named, None),
                    type_declaration(
                        "pkg.RefT",
                        6,
                        7,
                        CatalogueTypeSpec::Reference {
                            target: "pkg.T".into(),
                        },
                        None,
                    ),
                ],
                functions: Vec::new(),
            },
        )
        .await
        .unwrap();
        let named = &result.types[0];
        let reference = &result.types[1];
        assert_ne!(named.object_id(), reference.object_id());
        assert_eq!(named.form(), CatalogueTypeForm::Named);
        assert_eq!(
            reference.form(),
            CatalogueTypeForm::Reference {
                target: named.object_id()
            }
        );
        assert_eq!(
            state
                .catalogue_type(
                    "pkg.T",
                    CatalogueTypeForm::Reference {
                        target: named.object_id()
                    }
                )
                .await,
            Err(CatalogueError::CatalogueTypeMismatch)
        );
    }

    #[tokio::test]
    async fn admission_requires_owned_lease_without_mutating_catalogue() {
        let (_directory, state) = state().await;
        let owned = state.acquire_lease([5; 16]).await.unwrap();
        let current = capture_tx(&state.connection).await.unwrap();
        let unowned = crate::WriterLease {
            owner_id: [6; 16],
            epoch: owned.epoch,
        };
        let admission = CatalogueAdmission {
            predecessor_capture: None,
            types: vec![type_declaration(
                "pkg.Unowned",
                35,
                36,
                CatalogueTypeSpec::Named,
                None,
            )],
            functions: Vec::new(),
        };
        assert_eq!(
            state
                .admit_catalogue_at(unowned, &current, admission.clone())
                .await,
            Err(CatalogueError::RuntimeFailure)
        );
        let mut rows = state
            .connection
            .query("SELECT COUNT(*) FROM runtime_catalogue_identity", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );

        let stale = CwdCapture::new(current.snapshot().clone(), [99; 32]).unwrap();
        assert!(matches!(
            state.admit_catalogue_at(owned, &stale, admission).await,
            Err(CatalogueError::StaleCapture { .. })
        ));
        let mut rows = state
            .connection
            .query("SELECT COUNT(*) FROM runtime_catalogue_identity", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn predecessor_digest_and_existing_name_rename_are_verified() {
        let (_directory, state) = state().await;
        let lease = state.acquire_lease([6; 16]).await.unwrap();
        let first = admit(
            &state,
            lease,
            CatalogueAdmission {
                predecessor_capture: None,
                types: vec![type_declaration(
                    "pkg.T",
                    4,
                    5,
                    CatalogueTypeSpec::Named,
                    None,
                )],
                functions: Vec::new(),
            },
        )
        .await
        .unwrap();
        advance(&state, lease, 7).await;
        // A carried-forward admitted catalogue is immutable at this capture.
        // Source edits must use the combined new-generation activation boundary.
        let forged = CwdCapture::new(first.capture.snapshot().clone(), [99; 32]).unwrap();
        assert_eq!(
            admit(
                &state,
                lease,
                CatalogueAdmission {
                    predecessor_capture: Some(forged),
                    types: vec![type_declaration(
                        "pkg.T",
                        8,
                        9,
                        CatalogueTypeSpec::Named,
                        Some("pkg.Missing"),
                    )],
                    functions: Vec::new(),
                },
            )
            .await,
            Err(CatalogueError::CataloguePredecessorInvalid)
        );
        let current = capture_tx(&state.connection).await.unwrap();
        assert_eq!(
            state
                .admit_catalogue_at(
                    lease,
                    &current,
                    CatalogueAdmission {
                        predecessor_capture: Some(first.capture),
                        types: vec![type_declaration(
                            "pkg.T",
                            8,
                            9,
                            CatalogueTypeSpec::Named,
                            Some("pkg.Missing"),
                        )],
                        functions: Vec::new(),
                    },
                )
                .await,
            Err(CatalogueError::CatalogueRenameSourceMissing)
        );
    }

    #[tokio::test]
    async fn incomplete_or_conflicting_batch_rolls_back_every_catalogue_row() {
        let (_directory, state) = state().await;
        let initial = admit_without_explicit_lease(
            &state,
            CatalogueAdmission {
                predecessor_capture: None,
                types: vec![
                    type_declaration("pkg.Result", 8, 9, CatalogueTypeSpec::Named, None),
                    type_declaration("pkg.Param", 10, 11, CatalogueTypeSpec::Named, None),
                ],
                functions: vec![function_declaration(
                    vec![CatalogueParameterDeclaration {
                        name: "x".into(),
                        position: 0,
                        type_name: "pkg.Param".into(),
                    }],
                    "pkg.Result",
                )],
            },
        )
        .await
        .unwrap();
        let initial_param_id = initial.types[1].object_id();
        let conflict = admit_without_explicit_lease(
            &state,
            CatalogueAdmission {
                predecessor_capture: None,
                types: vec![type_declaration(
                    "pkg.Result",
                    8,
                    9,
                    CatalogueTypeSpec::Value,
                    None,
                )],
                functions: Vec::new(),
            },
        )
        .await;
        assert_eq!(conflict, Err(CatalogueError::CatalogueTypeMismatch));
        let incomplete = admit_without_explicit_lease(
            &state,
            CatalogueAdmission {
                predecessor_capture: None,
                types: vec![
                    type_declaration("pkg.Result", 8, 9, CatalogueTypeSpec::Named, None),
                    type_declaration("pkg.Param", 10, 11, CatalogueTypeSpec::Named, None),
                ],
                functions: vec![function_declaration(Vec::new(), "pkg.Result")],
            },
        )
        .await;
        assert_eq!(incomplete, Err(CatalogueError::CatalogueRevisionConflict));
        let function = state.catalogue_function("pkg.f").await.unwrap().unwrap();
        assert_eq!(function.parameters.len(), 1);
        assert_eq!(
            state
                .catalogue_type("pkg.Param", CatalogueTypeForm::Named)
                .await
                .unwrap()
                .unwrap()
                .object_id(),
            initial_param_id
        );
    }

    #[tokio::test]
    async fn carried_forward_generation_preserves_identity_until_source_activation() {
        let (_directory, state) = state().await;
        let lease = state.acquire_lease([16; 16]).await.unwrap();
        let first = admit(
            &state,
            lease,
            CatalogueAdmission {
                predecessor_capture: None,
                types: vec![type_declaration(
                    "pkg.T",
                    14,
                    15,
                    CatalogueTypeSpec::Named,
                    None,
                )],
                functions: Vec::new(),
            },
        )
        .await
        .unwrap();
        let first_id = first.types[0].object_id();
        let second_capture = advance(&state, lease, 16).await;
        let second = state
            .catalogue_type("pkg.T", CatalogueTypeForm::Named)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_id, second.object_id());
        assert_eq!(second.snapshot(), capture_bytes(&second_capture).unwrap());

        let rejected_edit = admit(
            &state,
            lease,
            CatalogueAdmission {
                predecessor_capture: Some(first.capture.clone()),
                types: vec![type_declaration(
                    "pkg.T",
                    17,
                    18,
                    CatalogueTypeSpec::Named,
                    None,
                )],
                functions: Vec::new(),
            },
        )
        .await;
        assert_eq!(
            rejected_edit,
            Err(CatalogueError::CatalogueRevisionConflict)
        );

        let third_capture = advance(&state, lease, 19).await;
        let data_only = state
            .catalogue_type("pkg.T", CatalogueTypeForm::Named)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_id, data_only.object_id());
        assert_eq!(data_only.snapshot(), capture_bytes(&third_capture).unwrap());
    }

    #[tokio::test]
    async fn data_only_predecessor_capture_remains_available_for_replay() {
        let (_directory, state) = state().await;
        let lease = state.acquire_lease([25; 16]).await.unwrap();
        let first = admit(
            &state,
            lease,
            CatalogueAdmission {
                predecessor_capture: None,
                types: vec![
                    type_declaration("pkg.T", 26, 27, CatalogueTypeSpec::Named, None),
                    type_declaration("pkg.Result", 28, 29, CatalogueTypeSpec::Named, None),
                    type_declaration("pkg.Param", 30, 31, CatalogueTypeSpec::Named, None),
                ],
                functions: vec![function_declaration(
                    vec![CatalogueParameterDeclaration {
                        name: "value".into(),
                        position: 0,
                        type_name: "pkg.Param".into(),
                    }],
                    "pkg.Result",
                )],
            },
        )
        .await
        .unwrap();

        // The first generation is the source activation following the
        // admission. The second is a data-only activation with no catalogue
        // batch. The next catalogue replay must be able to use that exact
        // data-only capture as its immediate predecessor.
        let source_capture = advance(&state, lease, 28).await;
        let data_only_capture = advance(&state, lease, 29).await;
        let next_source_capture = advance(&state, lease, 30).await;
        for capture in [&source_capture, &data_only_capture, &next_source_capture] {
            let snapshot = capture_bytes(capture).unwrap();
            let mut rows = state
                .connection
                .query(
                    "SELECT database_id, runtime_id, generation, generation_digest
                     FROM runtime_catalogue_capture WHERE snapshot = ?1",
                    params![snapshot],
                )
                .await
                .unwrap();
            let row = rows.next().await.unwrap().expect("retained capture");
            let database_id: Vec<u8> = row.get(0).unwrap();
            let runtime_id: Vec<u8> = row.get(1).unwrap();
            let generation: i64 = row.get(2).unwrap();
            let generation_digest: Vec<u8> = row.get(3).unwrap();
            assert_eq!(database_id, capture.database_id().to_vec());
            assert_eq!(runtime_id, capture.runtime_id().to_vec());
            assert_eq!(
                generation,
                crate::bigint_to_i64(capture.generation()).unwrap()
            );
            assert_eq!(generation_digest, capture.generation_digest().to_vec());
        }

        for (table, expected) in [
            ("runtime_catalogue_revision", 4_i64),
            ("runtime_catalogue_type", 3_i64),
            ("runtime_catalogue_function", 1_i64),
            ("runtime_catalogue_parameter", 1_i64),
        ] {
            let mut rows = state
                .connection
                .query(
                    &format!("SELECT COUNT(*) FROM {table} WHERE snapshot = ?1"),
                    params![capture_bytes(&data_only_capture).unwrap()],
                )
                .await
                .unwrap();
            assert_eq!(
                rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
                expected,
                "data-only {table} projection"
            );
        }

        let second = admit(
            &state,
            lease,
            CatalogueAdmission {
                predecessor_capture: Some(data_only_capture.clone()),
                types: vec![
                    type_declaration("pkg.T", 26, 27, CatalogueTypeSpec::Named, None),
                    type_declaration("pkg.Result", 28, 29, CatalogueTypeSpec::Named, None),
                    type_declaration("pkg.Param", 30, 31, CatalogueTypeSpec::Named, None),
                ],
                functions: vec![function_declaration(
                    vec![CatalogueParameterDeclaration {
                        name: "value".into(),
                        position: 0,
                        type_name: "pkg.Param".into(),
                    }],
                    "pkg.Result",
                )],
            },
        )
        .await
        .unwrap();
        assert_eq!(first.types[0].object_id(), second.types[0].object_id());
        assert_eq!(
            first.functions[0].object.object_id,
            second.functions[0].object.object_id
        );
        assert_eq!(second.functions[0].parameters.len(), 1);
        assert_eq!(second.capture.generation(), &num_bigint::BigInt::from(3));
    }

    #[tokio::test]
    async fn retained_function_read_stays_pinned_and_rejects_invalid_captures() {
        let (directory, state) = state().await;
        let lease = state.acquire_lease([7; 16]).await.unwrap();
        let first = admit(
            &state,
            lease,
            CatalogueAdmission {
                predecessor_capture: None,
                types: vec![
                    type_declaration("pkg.T", 4, 5, CatalogueTypeSpec::Named, None),
                    type_declaration("pkg.Result", 6, 7, CatalogueTypeSpec::Named, None),
                    type_declaration("pkg.Param", 8, 9, CatalogueTypeSpec::Named, None),
                ],
                functions: vec![function_declaration(
                    vec![CatalogueParameterDeclaration {
                        name: "value".into(),
                        position: 0,
                        type_name: "pkg.Param".into(),
                    }],
                    "pkg.Result",
                )],
            },
        )
        .await
        .unwrap();
        let admitted = first.functions[0].clone();

        // The pinned read at the admitted generation is exact.
        let pinned = state
            .catalogue_function_at("pkg.f", &first.capture)
            .await
            .unwrap()
            .expect("retained function resolves at its admitted capture");
        assert_eq!(pinned, admitted);

        // Multiple later generations must not impose predecessor adjacency
        // on reads of an older pinned reference (ORNA-SYS-019).
        advance(&state, lease, 12).await;
        let later = advance(&state, lease, 14).await;
        assert_eq!(later.generation() - first.capture.generation(), 2.into());
        assert_eq!(
            state
                .catalogue_function_at("pkg.f", &first.capture)
                .await
                .unwrap(),
            Some(admitted.clone())
        );

        // Reopen: retained rows survive and stay pinned.
        drop(state);
        let reopened = RuntimeState::open_path(
            &directory.path().join("state.db"),
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            digest(3),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            reopened
                .catalogue_function_at("pkg.f", &first.capture)
                .await
                .unwrap()
                .expect("retained function survives reopen"),
            admitted
        );

        // Forged digest: same snapshot bytes, wrong generation digest.
        let forged = CwdCapture::new(first.capture.snapshot().clone(), [99; 32]).unwrap();
        assert_eq!(
            reopened
                .catalogue_function_at("pkg.f", &forged)
                .await
                .unwrap_err(),
            CatalogueError::CatalogueCaptureInvalid
        );

        // A different runtime must not resolve through the current catalogue.
        let foreign_snapshot = Snapshot::cwd(
            first.capture.database_id(),
            [9; 16],
            capture_tx(&reopened.connection)
                .await
                .unwrap()
                .generation()
                .clone(),
        )
        .unwrap();
        let foreign = CwdCapture::new(foreign_snapshot, later.generation_digest()).unwrap();
        assert_eq!(
            reopened
                .catalogue_function_at("pkg.f", &foreign)
                .await
                .unwrap_err(),
            CatalogueError::CatalogueCaptureInvalid
        );

        let never_retained = CwdCapture::new(
            Snapshot::cwd(
                first.capture.database_id(),
                first.capture.runtime_id(),
                later.generation() + 1,
            )
            .unwrap(),
            later.generation_digest(),
        )
        .unwrap();
        assert_eq!(
            reopened
                .catalogue_function_at("pkg.f", &never_retained)
                .await
                .unwrap_err(),
            CatalogueError::CatalogueCaptureInvalid
        );
        assert_eq!(
            reopened
                .catalogue_function_at("pkg.missing", &first.capture)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            capture_tx(&reopened.connection).await.unwrap(),
            later,
            "retained reads and rejected captures must not advance current state"
        );
    }

    #[tokio::test]
    async fn reopening_reads_the_authoritative_identity_row() {
        let (directory, state) = state().await;
        let admitted = admit_without_explicit_lease(
            &state,
            CatalogueAdmission {
                predecessor_capture: None,
                types: vec![type_declaration(
                    "pkg.Reopen",
                    23,
                    24,
                    CatalogueTypeSpec::Named,
                    None,
                )],
                functions: Vec::new(),
            },
        )
        .await
        .unwrap();
        let object_id = admitted.types[0].object_id();
        drop(state);
        let reopened = RuntimeState::open_path(
            &directory.path().join("state.db"),
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            digest(3),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            reopened
                .catalogue_type("pkg.Reopen", CatalogueTypeForm::Named)
                .await
                .unwrap()
                .unwrap()
                .object_id(),
            object_id
        );
    }
}

#[cfg(test)]
mod module_tests {
    use super::*;
    use crate::{NoFault, RuntimeIdentity, TableMutation};
    use orna_foundation_v1::CwdCapture;
    use tempfile::tempdir;

    fn digest(value: u8) -> [u8; 32] {
        [value; 32]
    }

    fn declaration(
        name: &str,
        revision: u8,
        semantic: u8,
        rename_from: Option<&str>,
    ) -> CatalogueModuleDeclaration {
        CatalogueModuleDeclaration {
            qualified_name: name.into(),
            revision_id: digest(revision),
            semantic_hash: digest(semantic),
            rename_from: rename_from.map(str::to_owned),
        }
    }

    async fn state() -> (tempfile::TempDir, RuntimeState) {
        let directory = tempdir().unwrap();
        let state = RuntimeState::open_path(
            &directory.path().join("state.db"),
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            digest(3),
            None,
        )
        .await
        .unwrap();
        (directory, state)
    }

    async fn admit(
        state: &RuntimeState,
        writer: crate::WriterLease,
        predecessor: Option<CwdCapture>,
        declaration: CatalogueModuleDeclaration,
    ) -> Result<CatalogueModule, CatalogueError> {
        let capture = capture_tx(&state.connection).await?;
        state
            .admit_catalogue_module_at(writer, &capture, predecessor, declaration)
            .await
    }

    async fn advance(state: &RuntimeState, lease: crate::WriterLease, value: u8) -> CwdCapture {
        let context = state.begin_activation().await.unwrap();
        let mutation = TableMutation::new(
            [value; 16],
            "module-catalogue-test",
            vec![value],
            Some(vec![value]),
        )
        .unwrap();
        state
            .commit_table_activation(lease, &context, &[mutation], digest(value), &NoFault)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn module_initial_admission_is_authoritative() {
        let (_directory, state) = state().await;
        let lease = state.acquire_lease([4; 16]).await.unwrap();
        let module = admit(&state, lease, None, declaration("pkg.main", 4, 5, None))
            .await
            .unwrap();
        assert_eq!(module.object.kind, CatalogueObjectKind::Module);
        assert_eq!(module.object.qualified_name, "pkg.main");
        assert_eq!(
            state.catalogue_module("pkg.main").await.unwrap(),
            Some(module)
        );
    }

    #[tokio::test]
    async fn module_same_capture_replay_is_idempotent() {
        let (_directory, state) = state().await;
        let lease = state.acquire_lease([5; 16]).await.unwrap();
        let first = admit(&state, lease, None, declaration("pkg.main", 6, 7, None))
            .await
            .unwrap();
        let replay = admit(&state, lease, None, declaration("pkg.main", 6, 7, None))
            .await
            .unwrap();
        assert_eq!(first, replay);
    }

    #[tokio::test]
    async fn module_same_capture_conflict_rolls_back_without_replacement() {
        let (_directory, state) = state().await;
        let lease = state.acquire_lease([6; 16]).await.unwrap();
        let first = admit(&state, lease, None, declaration("pkg.main", 8, 9, None))
            .await
            .unwrap();
        assert_eq!(
            admit(&state, lease, None, declaration("pkg.main", 10, 11, None)).await,
            Err(CatalogueError::CatalogueRevisionConflict)
        );
        let current = state.catalogue_module("pkg.main").await.unwrap().unwrap();
        assert_eq!(current, first);
        let mut rows = state
            .connection
            .query(
                "SELECT COUNT(*) FROM runtime_catalogue_revision
                 WHERE object_id = ?1",
                params![first.object_id().to_vec()],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn module_edit_supersedes_only_a_carried_forward_row() {
        let (_directory, state) = state().await;
        let lease = state.acquire_lease([7; 16]).await.unwrap();
        let first_capture = capture_tx(&state.connection).await.unwrap();
        let first = admit(&state, lease, None, declaration("pkg.main", 12, 13, None))
            .await
            .unwrap();
        let current_capture = advance(&state, lease, 14).await;
        let edited = admit(
            &state,
            lease,
            Some(first_capture.clone()),
            declaration("pkg.main", 15, 16, None),
        )
        .await
        .unwrap();
        assert_eq!(edited.object_id(), first.object_id());
        assert_eq!(edited.object.revision_id, digest(15));
        assert_eq!(edited.snapshot(), current_capture.snapshot());
        let mut rows = state
            .connection
            .query(
                "SELECT revision_id FROM runtime_catalogue_revision
                 WHERE object_id = ?1 AND snapshot = ?2",
                params![
                    first.object_id().to_vec(),
                    capture_bytes(&first_capture).unwrap()
                ],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<Vec<u8>>(0).unwrap(), digest(12).to_vec());
    }

    #[tokio::test]
    async fn module_explicit_rename_preserves_identity_and_predecessor() {
        let (_directory, state) = state().await;
        let lease = state.acquire_lease([8; 16]).await.unwrap();
        let first_capture = capture_tx(&state.connection).await.unwrap();
        let first = admit(&state, lease, None, declaration("pkg.old", 17, 18, None))
            .await
            .unwrap();
        advance(&state, lease, 19).await;
        let renamed = admit(
            &state,
            lease,
            Some(first_capture),
            declaration("pkg.new", 20, 21, Some("pkg.old")),
        )
        .await
        .unwrap();
        assert_eq!(renamed.object_id(), first.object_id());
        assert_eq!(state.catalogue_module("pkg.old").await.unwrap(), None);
        assert_eq!(
            state
                .catalogue_module("pkg.new")
                .await
                .unwrap()
                .unwrap()
                .object_id(),
            first.object_id()
        );
    }

    #[tokio::test]
    async fn module_reopen_reads_the_retained_identity() {
        let (directory, state) = state().await;
        let lease = state.acquire_lease([9; 16]).await.unwrap();
        let first = admit(&state, lease, None, declaration("pkg.reopen", 22, 23, None))
            .await
            .unwrap();
        let object_id = first.object_id();
        drop(state);
        let reopened = RuntimeState::open_path(
            &directory.path().join("state.db"),
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            digest(3),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            reopened
                .catalogue_module("pkg.reopen")
                .await
                .unwrap()
                .unwrap()
                .object_id(),
            object_id
        );
    }
}

impl std::fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::CatalogueCorrupt => "runtime catalogue is corrupt",
            Self::CatalogueKindMismatch => "runtime catalogue object kind mismatch",
            Self::CatalogueNameConflict => "runtime catalogue name conflict",
            Self::CatalogueRevisionConflict => "runtime catalogue revision conflict",
            Self::CatalogueRenameSourceMissing => "runtime catalogue rename source is missing",
            Self::CataloguePredecessorRequired => "runtime catalogue predecessor is required",
            Self::CataloguePredecessorInvalid => "runtime catalogue predecessor is invalid",
            Self::CatalogueCaptureInvalid => "runtime catalogue capture is invalid",
            Self::CatalogueTypeMismatch => "runtime catalogue type mismatch",
            Self::CatalogueTypeMissing => "runtime catalogue type is missing",
            Self::CatalogueFunctionMissing => "runtime catalogue function is missing",
            Self::CatalogueParameterConflict => "runtime catalogue parameter conflict",
            Self::CatalogueIdentityMissing => "runtime catalogue identity is missing",
            Self::CatalogueAllocationExhausted => "runtime catalogue identity allocation exhausted",
            Self::InvalidObservationReference => "runtime catalogue reference is invalid",
            Self::StorageUnavailable => "runtime catalogue storage is unavailable",
            Self::StaleCapture { .. } => "runtime catalogue capture is stale",
            Self::RuntimeFailure => "runtime catalogue operation failed",
        })
    }
}
impl std::error::Error for CatalogueError {}

// Keep the implementation's compact error spelling local to this module;
// shared runtime consumers continue to see the unchanged RuntimeError enum.
use self::CatalogueError as RuntimeError;

/// The catalogue object kinds currently admitted by the v1 runtime catalogue
/// authority. Other sys.Object kinds remain outside this bounded prerequisite.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogueObjectKind {
    Function,
    Type,
    Module,
}

impl CatalogueObjectKind {
    const fn code(self) -> i64 {
        match self {
            Self::Function => 1,
            Self::Type => 2,
            Self::Module => 3,
        }
    }

    const fn from_code(code: i64) -> Result<Self, RuntimeError> {
        match code {
            1 => Ok(Self::Function),
            2 => Ok(Self::Type),
            3 => Ok(Self::Module),
            _ => Err(RuntimeError::CatalogueCorrupt),
        }
    }
}

/// Exact type form stored by the catalogue.  A reference-to-T is not the same
/// type as T; it therefore requires its own admitted type object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogueTypeForm {
    Named,
    Value,
    Reference { target: [u8; 16] },
}

impl CatalogueTypeForm {
    fn encode(self) -> (&'static str, Option<Vec<u8>>) {
        match self {
            Self::Named => ("named", None),
            Self::Value => ("value", None),
            Self::Reference { target } => ("reference", Some(target.to_vec())),
        }
    }

    fn decode(form: String, target: Option<Vec<u8>>) -> Result<Self, RuntimeError> {
        match (form.as_str(), target) {
            ("named", None) => Ok(Self::Named),
            ("value", None) => Ok(Self::Value),
            ("reference", Some(target)) => Ok(Self::Reference {
                target: fixed(target)?,
            }),
            _ => Err(RuntimeError::CatalogueCorrupt),
        }
    }
}

/// A declaration admitted by the current source/catalogue resolver.
/// `rename_from` is an explicit semantic continuity signal; a plain changed
/// name consequently allocates a new identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueDeclaration {
    pub qualified_name: String,
    pub kind: CatalogueObjectKind,
    pub revision_id: [u8; 32],
    pub semantic_hash: [u8; 32],
    pub rename_from: Option<String>,
}

/// A source-level exact type expression. The runtime resolves `target` by
/// qualified name inside the same admission batch; callers never provide an
/// ObjectId as a substitute for catalogue authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogueTypeSpec {
    Named,
    Value,
    Reference { target: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueTypeDeclaration {
    pub declaration: CatalogueDeclaration,
    pub form: CatalogueTypeSpec,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueParameterDeclaration {
    pub name: String,
    pub position: u64,
    pub type_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueFunctionDeclaration {
    pub declaration: CatalogueDeclaration,
    pub parameters: Vec<CatalogueParameterDeclaration>,
    pub result_type_name: String,
}

/// One complete current-runtime source catalogue. Its predecessor is an
/// exact retained snapshot, not a global name search, and is validated before
/// any new current-snapshot rows are made visible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueAdmission {
    pub predecessor_capture: Option<orna_foundation_v1::CwdCapture>,
    pub types: Vec<CatalogueTypeDeclaration>,
    pub functions: Vec<CatalogueFunctionDeclaration>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueAdmissionResult {
    pub capture: orna_foundation_v1::CwdCapture,
    pub types: Vec<CatalogueTypeHandle>,
    pub functions: Vec<CatalogueFunction>,
}

/// The committed identity and immutable revision facts for one resolved
/// source module. The runtime allocates the ObjectId; callers may only provide
/// the resolved name, revision, semantic hash, and an explicit rename witness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueModuleDeclaration {
    pub qualified_name: String,
    pub revision_id: [u8; 32],
    pub semantic_hash: [u8; 32],
    pub rename_from: Option<String>,
}

impl CatalogueModuleDeclaration {
    fn as_catalogue_declaration(&self) -> CatalogueDeclaration {
        CatalogueDeclaration {
            qualified_name: self.qualified_name.clone(),
            kind: CatalogueObjectKind::Module,
            revision_id: self.revision_id,
            semantic_hash: self.semantic_hash,
            rename_from: self.rename_from.clone(),
        }
    }
}

/// A checked handle to an admitted type row. Its fields are public only as
/// read-only facts; the runtime rechecks the handle's row and snapshot before
/// accepting it into a function signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueTypeHandle {
    object_id: [u8; 16],
    snapshot: Vec<u8>,
    reference: TypeRef,
    form: CatalogueTypeForm,
}

impl CatalogueTypeHandle {
    pub fn object_id(&self) -> [u8; 16] {
        self.object_id
    }

    pub fn reference(&self) -> &TypeRef {
        &self.reference
    }

    pub fn form(&self) -> CatalogueTypeForm {
        self.form
    }

    pub fn snapshot(&self) -> &[u8] {
        &self.snapshot
    }
}

/// The durable result of declaration admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueObject {
    pub object_id: [u8; 16],
    pub kind: CatalogueObjectKind,
    pub qualified_name: String,
    pub revision_id: [u8; 32],
    pub semantic_hash: [u8; 32],
    pub reference: ObjectRef,
    pub function_reference: Option<FunctionRef>,
    pub type_reference: Option<TypeRef>,
}

/// A checked function row and its exact parameter/result type handles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueFunction {
    pub object: CatalogueObject,
    pub parameters: Vec<CatalogueParameterHandle>,
    pub result_type: CatalogueTypeHandle,
}

/// A checked handle to a committed module identity at one pinned capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueModule {
    pub object: CatalogueObject,
}

impl CatalogueModule {
    pub fn object_id(&self) -> [u8; 16] {
        self.object.object_id
    }

    pub fn reference(&self) -> &ObjectRef {
        &self.object.reference
    }

    pub fn snapshot(&self) -> &orna_foundation_v1::CanonicalSnapshot {
        &self.object.reference.as_row_ref().snapshot
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogueParameterHandle {
    pub name: String,
    pub position: u64,
    pub type_object: CatalogueTypeHandle,
}

/// Admits a complete candidate at the next, not-yet-visible generation and
/// records a real catalogue ledger event. This is not a table mutation or a
/// source revision producer: revision witnesses remain caller-supplied facts.
pub(crate) async fn admit_activation_catalogue_tx(
    transaction: &Transaction,
    predecessor: &orna_foundation_v1::CwdCapture,
    capture: &orna_foundation_v1::CwdCapture,
    admission: &CatalogueAdmission,
) -> Result<crate::Mutation, crate::RuntimeError> {
    use orna_foundation_v1::OvbRaw;
    use sha2::{Digest, Sha256};

    // The outer request boundary checks this before any candidate writes.
    if admission.predecessor_capture.as_ref() != Some(predecessor) {
        return Err(crate::RuntimeError::RecoveryInvalid);
    }
    persist_capture_tx(transaction, predecessor)
        .await
        .map_err(map_catalogue_runtime_error)?;
    admit_catalogue_batch_tx(transaction, capture, admission)
        .await
        .map_err(map_catalogue_runtime_error)?;

    fn declaration(value: &CatalogueDeclaration) -> OvbRaw {
        OvbRaw::Array(vec![
            OvbRaw::Text(value.qualified_name.clone()),
            OvbRaw::Bytes(value.revision_id.to_vec()),
            OvbRaw::Bytes(value.semantic_hash.to_vec()),
            value
                .rename_from
                .as_ref()
                .map_or(OvbRaw::Null, |name| OvbRaw::Text(name.clone())),
        ])
    }
    let types = admission
        .types
        .iter()
        .map(|value| {
            let form = match &value.form {
                CatalogueTypeSpec::Named => OvbRaw::Array(vec![OvbRaw::Text("named".into())]),
                CatalogueTypeSpec::Value => OvbRaw::Array(vec![OvbRaw::Text("value".into())]),
                CatalogueTypeSpec::Reference { target } => OvbRaw::Array(vec![
                    OvbRaw::Text("reference".into()),
                    OvbRaw::Text(target.clone()),
                ]),
            };
            OvbRaw::Array(vec![declaration(&value.declaration), form])
        })
        .collect();
    let functions = admission
        .functions
        .iter()
        .map(|value| {
            OvbRaw::Array(vec![
                declaration(&value.declaration),
                OvbRaw::Text(value.result_type_name.clone()),
                OvbRaw::Array(
                    value
                        .parameters
                        .iter()
                        .map(|parameter| {
                            OvbRaw::Array(vec![
                                OvbRaw::Text(parameter.name.clone()),
                                OvbRaw::Int(parameter.position.into()),
                                OvbRaw::Text(parameter.type_name.clone()),
                            ])
                        })
                        .collect(),
                ),
            ])
        })
        .collect();
    let batch = Value::new(OvbRaw::Array(vec![
        OvbRaw::Array(types),
        OvbRaw::Array(functions),
    ]))
    .and_then(|value| value.encode())
    .map_err(|_| crate::RuntimeError::RecoveryInvalid)?;
    let payload = Value::new(OvbRaw::Array(vec![
        OvbRaw::Text("orna.runtime.catalogue-change.v1".into()),
        predecessor.snapshot().raw(),
        OvbRaw::Bytes(predecessor.generation_digest().to_vec()),
        capture.snapshot().raw(),
        OvbRaw::Bytes(capture.generation_digest().to_vec()),
        OvbRaw::Bytes(Sha256::digest(&batch).to_vec()),
    ]))
    .and_then(|value| value.encode())
    .map_err(|_| crate::RuntimeError::RecoveryInvalid)?;
    Ok(crate::Mutation {
        id: *Uuid::new_v4().as_bytes(),
        digest: Sha256::digest(&payload).into(),
        payload,
    })
}

async fn admit_catalogue_batch_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    admission: &CatalogueAdmission,
) -> Result<CatalogueAdmissionResult, RuntimeError> {
    persist_capture_tx(transaction, capture).await?;
    let predecessor = if let Some(predecessor) = admission.predecessor_capture.as_ref() {
        validate_predecessor_capture_tx(transaction, predecessor, capture).await?;
        Some(predecessor)
    } else {
        if capture.generation() != &num_bigint::BigInt::from(0) {
            return Err(RuntimeError::CataloguePredecessorRequired);
        }
        None
    };
    reject_separate_module_rows_tx(transaction, capture).await?;
    let already_admitted = catalogue_admission_exists_tx(transaction, capture).await?;
    if already_admitted {
        return replay_catalogue_tx(transaction, capture, predecessor, admission).await;
    }
    clear_catalogue_snapshot_tx(transaction, capture).await?;

    let mut names = std::collections::BTreeMap::new();
    let mut type_ids = std::collections::BTreeMap::new();
    for declaration in &admission.types {
        if declaration.declaration.kind != CatalogueObjectKind::Type {
            return Err(RuntimeError::CatalogueKindMismatch);
        }
        validate_type_spec(&declaration.form)?;
        if names
            .insert(
                declaration.declaration.qualified_name.clone(),
                CatalogueObjectKind::Type,
            )
            .is_some()
        {
            return Err(RuntimeError::CatalogueNameConflict);
        }
        let object_id =
            admit_object_tx(transaction, capture, predecessor, &declaration.declaration).await?;
        type_ids.insert(declaration.declaration.qualified_name.clone(), object_id);
    }
    for declaration in &admission.types {
        let object_id = type_ids[&declaration.declaration.qualified_name];
        let form = resolve_type_spec(&declaration.form, &type_ids)?;
        insert_type_tx(transaction, capture, object_id, form).await?;
    }
    validate_type_references_tx(transaction, capture).await?;

    let mut function_ids = Vec::new();
    for declaration in &admission.functions {
        if declaration.declaration.kind != CatalogueObjectKind::Function {
            return Err(RuntimeError::CatalogueKindMismatch);
        }
        validate_function_shape(declaration)?;
        if names
            .insert(
                declaration.declaration.qualified_name.clone(),
                CatalogueObjectKind::Function,
            )
            .is_some()
        {
            return Err(RuntimeError::CatalogueNameConflict);
        }
        let object_id =
            admit_object_tx(transaction, capture, predecessor, &declaration.declaration).await?;
        let result_type = *type_ids
            .get(&declaration.result_type_name)
            .ok_or(RuntimeError::CatalogueTypeMissing)?;
        let parameter_types = declaration
            .parameters
            .iter()
            .map(|parameter| {
                type_ids
                    .get(&parameter.type_name)
                    .copied()
                    .ok_or(RuntimeError::CatalogueTypeMissing)
            })
            .collect::<Result<Vec<_>, _>>()?;
        insert_function_tx(
            transaction,
            capture,
            object_id,
            result_type,
            &declaration.parameters,
            &parameter_types,
        )
        .await?;
        function_ids.push(object_id);
    }

    validate_complete_batch_tx(transaction, capture, &names).await?;
    let inserted = transaction
        .execute(
            "INSERT INTO runtime_catalogue_admission (snapshot) VALUES (?1)",
            params![capture_bytes(capture)?],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    if inserted != 1 {
        return Err(RuntimeError::CatalogueCorrupt);
    }

    let mut types = Vec::with_capacity(admission.types.len());
    for declaration in &admission.types {
        types.push(
            load_type_handle_tx(
                transaction,
                capture,
                type_ids[&declaration.declaration.qualified_name],
            )
            .await?,
        );
    }
    let mut functions = Vec::with_capacity(function_ids.len());
    for object_id in function_ids {
        functions.push(load_function_tx(transaction, capture, object_id).await?);
    }
    Ok(CatalogueAdmissionResult {
        capture: capture.clone(),
        types,
        functions,
    })
}

impl RuntimeState {
    /// Returns the complete previously observed capture for use as a
    /// predecessor. Its generation digest is retained and checked by the
    /// admission transaction; adapters must not reduce it to snapshot bytes.
    pub fn catalogue_predecessor_capture(
        capture: &orna_foundation_v1::CwdCapture,
    ) -> orna_foundation_v1::CwdCapture {
        capture.clone()
    }

    /// Atomically admits the complete source catalogue at one pinned CWD
    /// capture. The caller must provide the capture used while resolving the
    /// source; the runtime rechecks it inside the write transaction.
    ///
    /// This is the only publication boundary for a source catalogue: callers
    /// must submit the complete resolved batch in one transaction. Runtime
    /// generations that contain no catalogue edits carry forward the complete
    /// catalogue and its admission marker. Replaying that catalogue is
    /// read-only; a source change must use the generation-advancing activation
    /// boundary rather than replacing an already observable capture.
    pub async fn admit_catalogue_at(
        &self,
        writer: crate::WriterLease,
        expected_capture: &orna_foundation_v1::CwdCapture,
        admission: CatalogueAdmission,
    ) -> Result<CatalogueAdmissionResult, RuntimeError> {
        let transaction = self.catalogue_transaction().await?;
        self.require_owner(&transaction, writer).await?;
        let capture = capture_tx(&transaction).await?;
        if &capture != expected_capture {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(capture),
            });
        }
        let result = admit_catalogue_batch_tx(&transaction, &capture, &admission).await?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(result)
    }

    /// Atomically admits or replays one resolved module identity at the
    /// caller's pinned CWD capture. A changed declaration must be associated
    /// with the immediately preceding retained capture; an explicit rename
    /// reuses the predecessor's identity only when that witness resolves to
    /// the same committed module object.
    ///
    /// This is deliberately only the runtime identity primitive. It does not
    /// claim to be the complete source-catalogue or activation bridge.
    pub async fn admit_catalogue_module_at(
        &self,
        writer: crate::WriterLease,
        expected_capture: &orna_foundation_v1::CwdCapture,
        predecessor_capture: Option<orna_foundation_v1::CwdCapture>,
        declaration: CatalogueModuleDeclaration,
    ) -> Result<CatalogueModule, RuntimeError> {
        let transaction = self.catalogue_transaction().await?;
        self.require_owner(&transaction, writer).await?;
        let capture = capture_tx(&transaction).await?;
        if &capture != expected_capture {
            return Err(RuntimeError::StaleCapture {
                current: Box::new(capture),
            });
        }
        persist_capture_tx(&transaction, &capture).await?;
        let predecessor = if let Some(predecessor) = predecessor_capture.as_ref() {
            validate_predecessor_capture_tx(&transaction, predecessor, &capture).await?;
            Some(predecessor)
        } else if capture.generation() == &num_bigint::BigInt::from(0) {
            None
        } else {
            return Err(RuntimeError::CataloguePredecessorRequired);
        };
        let object_id =
            admit_module_object_tx(&transaction, &capture, predecessor, &declaration).await?;
        mark_module_admission_tx(&transaction, &capture, object_id).await?;
        let module = load_module_tx(&transaction, &capture, object_id).await?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(module)
    }

    /// Looks up a current-runtime type by exact name and form.  The returned
    /// reference is built only after the persisted Object row and Type row are
    /// both found and validated.
    pub async fn catalogue_type(
        &self,
        qualified_name: &str,
        form: CatalogueTypeForm,
    ) -> Result<Option<CatalogueTypeHandle>, RuntimeError> {
        validate_observation_text(qualified_name)?;
        let transaction = self.catalogue_transaction_read().await?;
        let capture = capture_tx(&transaction).await?;
        let id = lookup_object_id(
            &transaction,
            &capture,
            qualified_name,
            CatalogueObjectKind::Type,
        )
        .await?;
        let Some(id) = id else {
            transaction
                .commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            return Ok(None);
        };
        let stored = load_type_form_tx(&transaction, &capture, id).await?;
        if stored != form {
            return Err(RuntimeError::CatalogueTypeMismatch);
        }
        let handle = load_type_handle_tx(&transaction, &capture, id).await?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(Some(handle))
    }

    /// Reads one current-runtime function row with all exact type witnesses.
    pub async fn catalogue_function(
        &self,
        qualified_name: &str,
    ) -> Result<Option<CatalogueFunction>, RuntimeError> {
        validate_observation_text(qualified_name)?;
        let transaction = self.catalogue_transaction_read().await?;
        let capture = capture_tx(&transaction).await?;
        let Some(id) = lookup_object_id(
            &transaction,
            &capture,
            qualified_name,
            CatalogueObjectKind::Function,
        )
        .await?
        else {
            transaction
                .commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            return Ok(None);
        };
        let object = load_object_tx(&transaction, &capture, id).await?;
        let function_ref = function_reference(object.reference.clone())
            .map_err(|_| RuntimeError::InvalidObservationReference)?;
        let result_id = load_result_type_id_tx(&transaction, &capture, id).await?;
        let result_type = load_type_handle_tx(&transaction, &capture, result_id).await?;
        let parameters = load_parameters_tx(&transaction, &capture, id).await?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(Some(CatalogueFunction {
            object: CatalogueObject {
                function_reference: Some(function_ref),
                ..object
            },
            parameters,
            result_type,
        }))
    }

    /// Reads the module identity retained by the current runtime capture.
    pub async fn catalogue_module(
        &self,
        qualified_name: &str,
    ) -> Result<Option<CatalogueModule>, RuntimeError> {
        validate_observation_text(qualified_name)?;
        let transaction = self.catalogue_transaction_read().await?;
        let capture = capture_tx(&transaction).await?;
        let Some(id) = lookup_object_id(
            &transaction,
            &capture,
            qualified_name,
            CatalogueObjectKind::Module,
        )
        .await?
        else {
            transaction
                .commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            return Ok(None);
        };
        let module = load_module_tx(&transaction, &capture, id).await?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(Some(module))
    }

    /// Reads one function row at an exact retained CWD capture.
    ///
    /// The capture must be the complete, already persisted same-runtime
    /// capture: database and runtime identity, generation and generation
    /// digest are all rechecked against `runtime_catalogue_capture`. A forged
    /// digest, foreign runtime/database, or never-retained capture is
    /// [`CatalogueError::CatalogueCaptureInvalid`] and never falls back to the
    /// current snapshot or writes rows (ORNA-SYS-019/020).
    pub async fn catalogue_function_at(
        &self,
        qualified_name: &str,
        capture: &orna_foundation_v1::CwdCapture,
    ) -> Result<Option<CatalogueFunction>, RuntimeError> {
        validate_observation_text(qualified_name)?;
        let transaction = self.catalogue_transaction_read().await?;
        let stored = retained_capture_row_tx(&transaction, capture).await?;
        let Some(id) = lookup_snapshot_object_id(&transaction, &stored, qualified_name).await?
        else {
            transaction
                .commit()
                .await
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            return Ok(None);
        };
        let function = load_function_tx(&transaction, capture, id).await?;
        transaction
            .commit()
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        Ok(Some(function))
    }
    async fn catalogue_transaction(&self) -> Result<Transaction, RuntimeError> {
        self.connection
            .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }

    async fn catalogue_transaction_read(&self) -> Result<Transaction, RuntimeError> {
        self.connection
            .transaction_with_behavior(libsql::TransactionBehavior::Deferred)
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)
    }
}

async fn replay_catalogue_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    predecessor: Option<&orna_foundation_v1::CwdCapture>,
    admission: &CatalogueAdmission,
) -> Result<CatalogueAdmissionResult, RuntimeError> {
    let mut names = std::collections::BTreeMap::new();
    let mut type_ids = std::collections::BTreeMap::new();
    for declaration in &admission.types {
        if declaration.declaration.kind != CatalogueObjectKind::Type {
            return Err(RuntimeError::CatalogueKindMismatch);
        }
        validate_type_spec(&declaration.form)?;
        if names
            .insert(
                declaration.declaration.qualified_name.clone(),
                CatalogueObjectKind::Type,
            )
            .is_some()
        {
            return Err(RuntimeError::CatalogueNameConflict);
        }
        let object_id = lookup_snapshot_object_id(
            transaction,
            &capture_bytes(capture)?,
            &declaration.declaration.qualified_name,
        )
        .await?
        .ok_or(RuntimeError::CatalogueRevisionConflict)?;
        if object_kind_tx(transaction, object_id).await? != CatalogueObjectKind::Type {
            return Err(RuntimeError::CatalogueKindMismatch);
        }
        validate_replay_rename_tx(
            transaction,
            predecessor,
            declaration.declaration.rename_from.as_deref(),
            object_id,
        )
        .await?;
        ensure_revision_row_tx(transaction, capture, object_id, &declaration.declaration).await?;
        type_ids.insert(declaration.declaration.qualified_name.clone(), object_id);
    }
    for declaration in &admission.types {
        let object_id = type_ids[&declaration.declaration.qualified_name];
        let expected = resolve_type_spec(&declaration.form, &type_ids)?;
        let actual = load_type_form_tx(transaction, capture, object_id).await?;
        if actual != expected {
            return Err(RuntimeError::CatalogueTypeMismatch);
        }
    }
    validate_type_references_tx(transaction, capture).await?;

    let mut function_ids = Vec::with_capacity(admission.functions.len());
    for declaration in &admission.functions {
        if declaration.declaration.kind != CatalogueObjectKind::Function {
            return Err(RuntimeError::CatalogueKindMismatch);
        }
        validate_function_shape(declaration)?;
        if names
            .insert(
                declaration.declaration.qualified_name.clone(),
                CatalogueObjectKind::Function,
            )
            .is_some()
        {
            return Err(RuntimeError::CatalogueNameConflict);
        }
        let object_id = lookup_snapshot_object_id(
            transaction,
            &capture_bytes(capture)?,
            &declaration.declaration.qualified_name,
        )
        .await?
        .ok_or(RuntimeError::CatalogueRevisionConflict)?;
        if object_kind_tx(transaction, object_id).await? != CatalogueObjectKind::Function {
            return Err(RuntimeError::CatalogueKindMismatch);
        }
        validate_replay_rename_tx(
            transaction,
            predecessor,
            declaration.declaration.rename_from.as_deref(),
            object_id,
        )
        .await?;
        ensure_revision_row_tx(transaction, capture, object_id, &declaration.declaration).await?;
        let result_type = *type_ids
            .get(&declaration.result_type_name)
            .ok_or(RuntimeError::CatalogueTypeMissing)?;
        if load_result_type_id_tx(transaction, capture, object_id).await? != result_type {
            return Err(RuntimeError::CatalogueRevisionConflict);
        }
        let parameter_types = declaration
            .parameters
            .iter()
            .map(|parameter| {
                type_ids
                    .get(&parameter.type_name)
                    .copied()
                    .ok_or(RuntimeError::CatalogueTypeMissing)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let actual = load_parameters_tx(transaction, capture, object_id).await?;
        if actual.len() != declaration.parameters.len()
            || actual
                .iter()
                .zip(&declaration.parameters)
                .zip(&parameter_types)
                .any(|((actual, expected), type_id)| {
                    actual.name != expected.name
                        || actual.position != expected.position
                        || actual.type_object.object_id != *type_id
                })
        {
            return Err(RuntimeError::CatalogueRevisionConflict);
        }
        function_ids.push(object_id);
    }

    validate_complete_batch_tx(transaction, capture, &names).await?;
    let mut types = Vec::with_capacity(admission.types.len());
    for declaration in &admission.types {
        types.push(
            load_type_handle_tx(
                transaction,
                capture,
                type_ids[&declaration.declaration.qualified_name],
            )
            .await?,
        );
    }
    let mut functions = Vec::with_capacity(function_ids.len());
    for object_id in function_ids {
        functions.push(load_function_tx(transaction, capture, object_id).await?);
    }
    Ok(CatalogueAdmissionResult {
        capture: capture.clone(),
        types,
        functions,
    })
}

async fn validate_replay_rename_tx(
    transaction: &Transaction,
    predecessor: Option<&orna_foundation_v1::CwdCapture>,
    rename_from: Option<&str>,
    object_id: [u8; 16],
) -> Result<(), RuntimeError> {
    let Some(rename_from) = rename_from else {
        return Ok(());
    };
    let Some(predecessor) = predecessor else {
        return Err(RuntimeError::CataloguePredecessorRequired);
    };
    let predecessor_snapshot = capture_bytes(predecessor)?;
    let source = lookup_snapshot_object_id(transaction, &predecessor_snapshot, rename_from)
        .await?
        .ok_or(RuntimeError::CatalogueRenameSourceMissing)?;
    if source != object_id {
        return Err(RuntimeError::CatalogueNameConflict);
    }
    Ok(())
}

async fn admit_object_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    predecessor: Option<&orna_foundation_v1::CwdCapture>,
    declaration: &CatalogueDeclaration,
) -> Result<[u8; 16], RuntimeError> {
    validate_observation_text(&declaration.qualified_name)?;
    if let Some(rename_from) = &declaration.rename_from {
        validate_observation_text(rename_from)?;
    }
    let snapshot = capture_bytes(capture)?;
    let existing_name = lookup_current_object_id(
        transaction,
        capture,
        &declaration.qualified_name,
        declaration.kind,
    )
    .await?;
    if let Some(object_id) = existing_name {
        if let Some(predecessor) = predecessor {
            let predecessor_snapshot = capture_bytes(predecessor)?;
            if let Some(rename_from) = &declaration.rename_from {
                let source =
                    lookup_snapshot_object_id(transaction, &predecessor_snapshot, rename_from)
                        .await?
                        .ok_or(RuntimeError::CatalogueRenameSourceMissing)?;
                if source != object_id {
                    return Err(RuntimeError::CatalogueNameConflict);
                }
            } else if let Some(previous) = lookup_snapshot_object_id(
                transaction,
                &predecessor_snapshot,
                &declaration.qualified_name,
            )
            .await?
                && previous != object_id
            {
                return Err(RuntimeError::CatalogueRevisionConflict);
            }
        } else if declaration.rename_from.is_some() {
            return Err(RuntimeError::CataloguePredecessorRequired);
        }
        ensure_revision_row_tx(transaction, capture, object_id, declaration).await?;
        return Ok(object_id);
    }
    let object_id = if let Some(rename_from) = &declaration.rename_from {
        let Some(predecessor) = predecessor else {
            return Err(RuntimeError::CataloguePredecessorRequired);
        };
        let predecessor_snapshot = capture_bytes(predecessor)?;
        let Some(object_id) =
            lookup_snapshot_object_id(transaction, &predecessor_snapshot, rename_from).await?
        else {
            return Err(RuntimeError::CatalogueRenameSourceMissing);
        };
        if object_kind_tx(transaction, object_id).await? != declaration.kind {
            return Err(RuntimeError::CatalogueKindMismatch);
        }
        object_id
    } else if let Some(predecessor) = predecessor {
        let predecessor_snapshot = capture_bytes(predecessor)?;
        if let Some(object_id) = lookup_snapshot_object_id(
            transaction,
            &predecessor_snapshot,
            &declaration.qualified_name,
        )
        .await?
        {
            if object_kind_tx(transaction, object_id).await? != declaration.kind {
                return Err(RuntimeError::CatalogueKindMismatch);
            }
            object_id
        } else {
            allocate_object_id_tx(transaction, declaration.kind).await?
        }
    } else {
        allocate_object_id_tx(transaction, declaration.kind).await?
    };
    if current_object_revision_exists(transaction, capture, object_id).await? {
        return Err(RuntimeError::CatalogueNameConflict);
    }
    if lookup_current_object_id(
        transaction,
        capture,
        &declaration.qualified_name,
        declaration.kind,
    )
    .await?
    .is_some()
    {
        return Err(RuntimeError::CatalogueNameConflict);
    }
    if object_kind_tx(transaction, object_id).await? != declaration.kind {
        return Err(RuntimeError::CatalogueKindMismatch);
    }
    validate_revision_identity_tx(transaction, object_id, declaration).await?;
    transaction
        .execute(
            "INSERT INTO runtime_catalogue_revision
             (object_id, snapshot, kind, qualified_name, revision_id, semantic_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                object_id.to_vec(),
                snapshot,
                declaration.kind.code(),
                declaration.qualified_name.clone(),
                declaration.revision_id.to_vec(),
                declaration.semantic_hash.to_vec()
            ],
        )
        .await
        .map_err(|_| RuntimeError::CatalogueNameConflict)?;
    Ok(object_id)
}

async fn validate_revision_identity_tx(
    transaction: &Transaction,
    object_id: [u8; 16],
    declaration: &CatalogueDeclaration,
) -> Result<(), RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT object_id, kind, semantic_hash
             FROM runtime_catalogue_revision WHERE revision_id = ?1",
            params![declaration.revision_id.to_vec()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let existing_object_id = fixed(row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?)?;
        let existing_kind = CatalogueObjectKind::from_code(
            row.get(1).map_err(|_| RuntimeError::CatalogueCorrupt)?,
        )?;
        let existing_hash = fixed(row.get(2).map_err(|_| RuntimeError::CatalogueCorrupt)?)?;
        if existing_object_id != object_id
            || existing_kind != declaration.kind
            || existing_hash != declaration.semantic_hash
        {
            return Err(RuntimeError::CatalogueRevisionConflict);
        }
    }
    Ok(())
}

pub(crate) async fn persist_capture_tx(
    transaction: &Connection,
    capture: &orna_foundation_v1::CwdCapture,
) -> Result<(), RuntimeError> {
    let snapshot = capture_bytes(capture)?;
    transaction
        .execute(
            "INSERT INTO runtime_catalogue_capture
             (snapshot, database_id, runtime_id, generation, generation_digest)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(snapshot) DO NOTHING",
            params![
                snapshot.clone(),
                capture.database_id().to_vec(),
                capture.runtime_id().to_vec(),
                crate::bigint_to_i64(capture.generation())?,
                capture.generation_digest().to_vec()
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut rows = transaction
        .query(
            "SELECT database_id, runtime_id, generation, generation_digest
             FROM runtime_catalogue_capture WHERE snapshot = ?1",
            params![snapshot],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::CatalogueCorrupt)?;
    if fixed(row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?)? != capture.database_id()
        || fixed(row.get(1).map_err(|_| RuntimeError::CatalogueCorrupt)?)? != capture.runtime_id()
        || row
            .get::<i64>(2)
            .map_err(|_| RuntimeError::CatalogueCorrupt)?
            != crate::bigint_to_i64(capture.generation())?
        || fixed(row.get(3).map_err(|_| RuntimeError::CatalogueCorrupt)?)?
            != capture.generation_digest()
    {
        return Err(RuntimeError::CataloguePredecessorInvalid);
    }
    Ok(())
}

/// Persists a generation capture from the shared runtime commit path.  That
/// path cannot expose the module-local catalogue error type, so preserve the
/// storage distinction and redact catalogue validation failures to the
/// existing runtime recovery error.
pub(crate) async fn persist_capture_for_runtime_tx(
    connection: &Connection,
    predecessor: &orna_foundation_v1::CwdCapture,
    capture: &orna_foundation_v1::CwdCapture,
) -> Result<(), crate::RuntimeError> {
    persist_capture_tx(connection, predecessor)
        .await
        .map_err(map_catalogue_runtime_error)?;
    persist_capture_tx(connection, capture)
        .await
        .map_err(map_catalogue_runtime_error)?;
    carry_forward_catalogue_tx(connection, predecessor, capture)
        .await
        .map_err(map_catalogue_runtime_error)
}

fn map_catalogue_runtime_error(error: CatalogueError) -> crate::RuntimeError {
    match error {
        CatalogueError::StorageUnavailable => crate::RuntimeError::StorageUnavailable,
        CatalogueError::StaleCapture { .. }
        | CatalogueError::CatalogueCorrupt
        | CatalogueError::CataloguePredecessorInvalid
        | CatalogueError::CatalogueKindMismatch
        | CatalogueError::CatalogueAllocationExhausted
        | CatalogueError::RuntimeFailure
        | CatalogueError::CatalogueCaptureInvalid
        | CatalogueError::CatalogueNameConflict
        | CatalogueError::CatalogueRevisionConflict
        | CatalogueError::CatalogueRenameSourceMissing
        | CatalogueError::CataloguePredecessorRequired
        | CatalogueError::CatalogueTypeMismatch
        | CatalogueError::CatalogueTypeMissing
        | CatalogueError::CatalogueFunctionMissing
        | CatalogueError::CatalogueParameterConflict
        | CatalogueError::CatalogueIdentityMissing
        | CatalogueError::InvalidObservationReference => crate::RuntimeError::RecoveryInvalid,
    }
}

async fn carry_forward_catalogue_tx(
    connection: &Connection,
    predecessor: &orna_foundation_v1::CwdCapture,
    capture: &orna_foundation_v1::CwdCapture,
) -> Result<(), RuntimeError> {
    let predecessor_snapshot = capture_bytes(predecessor)?;
    let snapshot = capture_bytes(capture)?;
    let mut admissions = connection
        .query(
            "SELECT 1 FROM runtime_catalogue_admission WHERE snapshot = ?1",
            params![predecessor_snapshot.clone()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let predecessor_was_admitted = admissions
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .is_some();
    let mut mismatches = connection
        .query(
            "SELECT 1
             FROM runtime_catalogue_revision AS revision
             LEFT JOIN runtime_catalogue_identity AS identity
               ON identity.object_id = revision.object_id
             WHERE revision.snapshot = ?1
               AND (identity.object_id IS NULL OR revision.kind <> identity.kind)
             LIMIT 1",
            params![predecessor_snapshot.clone()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    if mismatches
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .is_some()
    {
        return Err(RuntimeError::CatalogueKindMismatch);
    }
    copy_catalogue_relation_tx(
        connection,
        "runtime_catalogue_revision",
        &predecessor_snapshot,
        &snapshot,
    )
    .await?;
    copy_catalogue_relation_tx(
        connection,
        "runtime_catalogue_type",
        &predecessor_snapshot,
        &snapshot,
    )
    .await?;
    copy_catalogue_relation_tx(
        connection,
        "runtime_catalogue_function",
        &predecessor_snapshot,
        &snapshot,
    )
    .await?;
    copy_catalogue_relation_tx(
        connection,
        "runtime_catalogue_parameter",
        &predecessor_snapshot,
        &snapshot,
    )
    .await?;
    if predecessor_was_admitted {
        let inserted = connection
            .execute(
                "INSERT INTO runtime_catalogue_admission (snapshot) VALUES (?1)",
                params![snapshot],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if inserted != 1 {
            return Err(RuntimeError::CatalogueCorrupt);
        }
    }
    Ok(())
}

async fn copy_catalogue_relation_tx(
    connection: &Connection,
    table: &str,
    predecessor_snapshot: &[u8],
    snapshot: &[u8],
) -> Result<(), RuntimeError> {
    let (count_sql, insert_sql) = match table {
        "runtime_catalogue_revision" => (
            "SELECT COUNT(*) FROM runtime_catalogue_revision WHERE snapshot = ?1",
            "INSERT INTO runtime_catalogue_revision
             (object_id, snapshot, kind, qualified_name, revision_id, semantic_hash)
             SELECT object_id, ?2, kind, qualified_name, revision_id, semantic_hash
             FROM runtime_catalogue_revision WHERE snapshot = ?1",
        ),
        "runtime_catalogue_type" => (
            "SELECT COUNT(*) FROM runtime_catalogue_type WHERE snapshot = ?1",
            "INSERT INTO runtime_catalogue_type
             (object_id, snapshot, form, target_object_id)
             SELECT object_id, ?2, form, target_object_id
             FROM runtime_catalogue_type WHERE snapshot = ?1",
        ),
        "runtime_catalogue_function" => (
            "SELECT COUNT(*) FROM runtime_catalogue_function WHERE snapshot = ?1",
            "INSERT INTO runtime_catalogue_function
             (object_id, snapshot, result_type_object_id)
             SELECT object_id, ?2, result_type_object_id
             FROM runtime_catalogue_function WHERE snapshot = ?1",
        ),
        "runtime_catalogue_parameter" => (
            "SELECT COUNT(*) FROM runtime_catalogue_parameter WHERE snapshot = ?1",
            "INSERT INTO runtime_catalogue_parameter
             (object_id, snapshot, position, name, type_object_id)
             SELECT object_id, ?2, position, name, type_object_id
             FROM runtime_catalogue_parameter WHERE snapshot = ?1",
        ),
        _ => return Err(RuntimeError::CatalogueCorrupt),
    };
    let mut rows = connection
        .query(count_sql, params![predecessor_snapshot.to_vec()])
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let source_count: i64 = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::CatalogueCorrupt)?
        .get(0)
        .map_err(|_| RuntimeError::CatalogueCorrupt)?;
    let copied = connection
        .execute(
            insert_sql,
            params![predecessor_snapshot.to_vec(), snapshot.to_vec()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    if copied != u64::try_from(source_count).map_err(|_| RuntimeError::CatalogueCorrupt)? {
        return Err(RuntimeError::CatalogueCorrupt);
    }
    Ok(())
}

async fn catalogue_admission_exists_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
) -> Result<bool, RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT 1 FROM runtime_catalogue_admission WHERE snapshot = ?1",
            params![capture_bytes(capture)?],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .is_some())
}

async fn clear_catalogue_snapshot_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
) -> Result<(), RuntimeError> {
    let snapshot = capture_bytes(capture)?;
    for table in [
        "runtime_catalogue_parameter",
        "runtime_catalogue_function",
        "runtime_catalogue_type",
        "runtime_catalogue_revision",
    ] {
        transaction
            .execute(
                &format!("DELETE FROM {table} WHERE snapshot = ?1"),
                params![snapshot.clone()],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    Ok(())
}

async fn validate_complete_batch_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    expected: &std::collections::BTreeMap<String, CatalogueObjectKind>,
) -> Result<(), RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT qualified_name, kind FROM runtime_catalogue_revision
             WHERE snapshot = ?1",
            params![capture_bytes(capture)?],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut actual = std::collections::BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let name: String = row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?;
        let kind = CatalogueObjectKind::from_code(
            row.get(1).map_err(|_| RuntimeError::CatalogueCorrupt)?,
        )?;
        if actual.insert(name, kind).is_some() {
            return Err(RuntimeError::CatalogueCorrupt);
        }
    }
    if &actual != expected {
        return Err(RuntimeError::CatalogueRevisionConflict);
    }
    Ok(())
}

async fn validate_predecessor_capture_tx(
    transaction: &Transaction,
    predecessor: &orna_foundation_v1::CwdCapture,
    current: &orna_foundation_v1::CwdCapture,
) -> Result<(), RuntimeError> {
    let snapshot = capture_bytes(predecessor)?;
    validate_predecessor_snapshot(&snapshot, current)?;
    let mut rows = transaction
        .query(
            "SELECT database_id, runtime_id, generation, generation_digest
             FROM runtime_catalogue_capture WHERE snapshot = ?1",
            params![snapshot],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::CataloguePredecessorInvalid)?;
    if fixed(row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?)? != predecessor.database_id()
        || fixed(row.get(1).map_err(|_| RuntimeError::CatalogueCorrupt)?)?
            != predecessor.runtime_id()
        || row
            .get::<i64>(2)
            .map_err(|_| RuntimeError::CatalogueCorrupt)?
            != crate::bigint_to_i64(predecessor.generation())?
        || fixed(row.get(3).map_err(|_| RuntimeError::CatalogueCorrupt)?)?
            != predecessor.generation_digest()
    {
        return Err(RuntimeError::CataloguePredecessorInvalid);
    }
    Ok(())
}

/// Validates one exact retained same-runtime capture and returns its stored
/// snapshot bytes.
///
/// Retained reads use the full persisted identity set; unlike predecessor
/// validation there is no adjacency requirement, because a pinned reference
/// may point at any retained generation of this runtime, not only the
/// immediately preceding one (ORNA-SYS-019).
async fn retained_capture_row_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
) -> Result<Vec<u8>, RuntimeError> {
    let snapshot = capture_bytes(capture)?;
    let mut rows = transaction
        .query(
            "SELECT database_id, runtime_id, generation, generation_digest
             FROM runtime_catalogue_capture WHERE snapshot = ?1",
            params![snapshot.clone()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Err(RuntimeError::CatalogueCaptureInvalid);
    };
    if fixed(row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?)? != capture.database_id()
        || fixed(row.get(1).map_err(|_| RuntimeError::CatalogueCorrupt)?)? != capture.runtime_id()
        || row
            .get::<i64>(2)
            .map_err(|_| RuntimeError::CatalogueCorrupt)?
            != crate::bigint_to_i64(capture.generation())?
        || fixed(row.get(3).map_err(|_| RuntimeError::CatalogueCorrupt)?)?
            != capture.generation_digest()
    {
        return Err(RuntimeError::CatalogueCaptureInvalid);
    }
    Ok(snapshot)
}

async fn ensure_revision_row_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    object_id: [u8; 16],
    declaration: &CatalogueDeclaration,
) -> Result<(), RuntimeError> {
    let snapshot = capture_bytes(capture)?;
    let mut rows = transaction
        .query(
            "SELECT kind, revision_id, semantic_hash FROM runtime_catalogue_revision
             WHERE object_id = ?1 AND snapshot = ?2",
            params![object_id.to_vec(), snapshot],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Err(RuntimeError::CatalogueCorrupt);
    };
    let kind =
        CatalogueObjectKind::from_code(row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?)?;
    let revision_id: Vec<u8> = row.get(1).map_err(|_| RuntimeError::CatalogueCorrupt)?;
    let semantic_hash: Vec<u8> = row.get(2).map_err(|_| RuntimeError::CatalogueCorrupt)?;
    if kind != declaration.kind
        || revision_id.as_slice() != declaration.revision_id
        || semantic_hash.as_slice() != declaration.semantic_hash
    {
        return Err(RuntimeError::CatalogueRevisionConflict);
    }
    Ok(())
}

fn validate_type_spec(spec: &CatalogueTypeSpec) -> Result<(), RuntimeError> {
    if let CatalogueTypeSpec::Reference { target } = spec {
        validate_observation_text(target)?;
    }
    Ok(())
}

fn resolve_type_spec(
    spec: &CatalogueTypeSpec,
    type_ids: &std::collections::BTreeMap<String, [u8; 16]>,
) -> Result<CatalogueTypeForm, RuntimeError> {
    Ok(match spec {
        CatalogueTypeSpec::Named => CatalogueTypeForm::Named,
        CatalogueTypeSpec::Value => CatalogueTypeForm::Value,
        CatalogueTypeSpec::Reference { target } => CatalogueTypeForm::Reference {
            target: *type_ids
                .get(target)
                .ok_or(RuntimeError::CatalogueTypeMissing)?,
        },
    })
}

async fn insert_type_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    object_id: [u8; 16],
    form: CatalogueTypeForm,
) -> Result<(), RuntimeError> {
    if let CatalogueTypeForm::Reference { target } = form
        && object_kind_tx(transaction, target).await? != CatalogueObjectKind::Type
    {
        return Err(RuntimeError::CatalogueKindMismatch);
    }
    let (form_name, target) = form.encode();
    transaction
        .execute(
            "INSERT INTO runtime_catalogue_type (object_id, snapshot, form, target_object_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(object_id, snapshot) DO NOTHING",
            params![
                object_id.to_vec(),
                capture_bytes(capture)?,
                form_name,
                target
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    if load_type_form_tx(transaction, capture, object_id).await? != form {
        return Err(RuntimeError::CatalogueTypeMismatch);
    }
    Ok(())
}

async fn validate_type_references_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
) -> Result<(), RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT target_object_id FROM runtime_catalogue_type
             WHERE snapshot = ?1 AND form = 'reference'",
            params![capture_bytes(capture)?],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let target = fixed(row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?)?;
        let object = load_object_tx(transaction, capture, target).await?;
        if object.kind != CatalogueObjectKind::Type {
            return Err(RuntimeError::CatalogueKindMismatch);
        }
        load_type_form_tx(transaction, capture, target).await?;
    }
    Ok(())
}

async fn insert_function_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    object_id: [u8; 16],
    result_type: [u8; 16],
    parameters: &[CatalogueParameterDeclaration],
    parameter_types: &[[u8; 16]],
) -> Result<(), RuntimeError> {
    transaction
        .execute(
            "INSERT INTO runtime_catalogue_function (object_id, snapshot, result_type_object_id)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(object_id, snapshot) DO NOTHING",
            params![
                object_id.to_vec(),
                capture_bytes(capture)?,
                result_type.to_vec()
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let stored_result = load_result_type_id_tx(transaction, capture, object_id).await?;
    if stored_result != result_type {
        return Err(RuntimeError::CatalogueRevisionConflict);
    }
    for (parameter, type_id) in parameters.iter().zip(parameter_types) {
        transaction
            .execute(
                "INSERT INTO runtime_catalogue_parameter
                 (object_id, snapshot, position, name, type_object_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(object_id, snapshot, position) DO NOTHING",
                params![
                    object_id.to_vec(),
                    capture_bytes(capture)?,
                    i64::try_from(parameter.position)
                        .map_err(|_| RuntimeError::CatalogueCorrupt)?,
                    parameter.name.clone(),
                    type_id.to_vec()
                ],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
    }
    let actual = load_parameters_tx(transaction, capture, object_id).await?;
    if actual.len() != parameters.len() {
        return Err(RuntimeError::CatalogueRevisionConflict);
    }
    for ((actual, expected), type_id) in actual.iter().zip(parameters).zip(parameter_types) {
        if actual.name != expected.name
            || actual.position != expected.position
            || actual.type_object.object_id != *type_id
        {
            return Err(RuntimeError::CatalogueRevisionConflict);
        }
    }
    Ok(())
}

async fn load_function_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    id: [u8; 16],
) -> Result<CatalogueFunction, RuntimeError> {
    let object = load_object_tx(transaction, capture, id).await?;
    if object.kind != CatalogueObjectKind::Function {
        return Err(RuntimeError::CatalogueKindMismatch);
    }
    let function_reference = function_reference(object.reference.clone())
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    let result_id = load_result_type_id_tx(transaction, capture, id).await?;
    let result_type = load_type_handle_tx(transaction, capture, result_id).await?;
    let parameters = load_parameters_tx(transaction, capture, id).await?;
    Ok(CatalogueFunction {
        object: CatalogueObject {
            function_reference: Some(function_reference),
            ..object
        },
        parameters,
        result_type,
    })
}

async fn load_module_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    id: [u8; 16],
) -> Result<CatalogueModule, RuntimeError> {
    let object = load_object_tx(transaction, capture, id).await?;
    if object.kind != CatalogueObjectKind::Module {
        return Err(RuntimeError::CatalogueKindMismatch);
    }
    Ok(CatalogueModule { object })
}

async fn admit_module_object_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    predecessor: Option<&orna_foundation_v1::CwdCapture>,
    declaration: &CatalogueModuleDeclaration,
) -> Result<[u8; 16], RuntimeError> {
    let declaration = declaration.as_catalogue_declaration();
    validate_observation_text(&declaration.qualified_name)?;
    if let Some(rename_from) = declaration.rename_from.as_deref() {
        validate_observation_text(rename_from)?;
    }
    let snapshot = capture_bytes(capture)?;
    if let Some(current_id) =
        lookup_snapshot_object_id(transaction, &snapshot, &declaration.qualified_name).await?
    {
        if object_kind_tx(transaction, current_id).await? != CatalogueObjectKind::Module {
            return Err(RuntimeError::CatalogueKindMismatch);
        }
        validate_replay_rename_tx(
            transaction,
            predecessor,
            declaration.rename_from.as_deref(),
            current_id,
        )
        .await?;
        match ensure_revision_row_tx(transaction, capture, current_id, &declaration).await {
            Ok(()) => return Ok(current_id),
            Err(RuntimeError::CatalogueRevisionConflict)
                if !module_admission_exists_tx(transaction, capture, current_id).await?
                    && is_carried_forward_module_tx(
                        transaction,
                        capture,
                        predecessor,
                        current_id,
                        &declaration.qualified_name,
                    )
                    .await? =>
            {
                clear_module_revision_tx(transaction, capture, current_id).await?;
            }
            Err(error) => return Err(error),
        }
    }

    if let Some(rename_from) = declaration.rename_from.as_deref() {
        let Some(predecessor) = predecessor else {
            return Err(RuntimeError::CataloguePredecessorRequired);
        };
        let predecessor_snapshot = capture_bytes(predecessor)?;
        let old_id = lookup_snapshot_object_id(transaction, &predecessor_snapshot, rename_from)
            .await?
            .ok_or(RuntimeError::CatalogueRenameSourceMissing)?;
        if object_kind_tx(transaction, old_id).await? != CatalogueObjectKind::Module {
            return Err(RuntimeError::CatalogueKindMismatch);
        }
        if lookup_snapshot_object_id(transaction, &snapshot, rename_from)
            .await?
            .is_some()
        {
            if module_admission_exists_tx(transaction, capture, old_id).await?
                || !is_carried_forward_module_tx(
                    transaction,
                    capture,
                    Some(predecessor),
                    old_id,
                    rename_from,
                )
                .await?
            {
                return Err(RuntimeError::CatalogueRevisionConflict);
            }
            clear_module_revision_tx(transaction, capture, old_id).await?;
        }
    }
    admit_object_tx(transaction, capture, predecessor, &declaration).await
}

async fn module_admission_exists_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    object_id: [u8; 16],
) -> Result<bool, RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT 1 FROM runtime_catalogue_module_admission
             WHERE object_id = ?1 AND snapshot = ?2",
            params![object_id.to_vec(), capture_bytes(capture)?],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .is_some())
}

async fn mark_module_admission_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    object_id: [u8; 16],
) -> Result<(), RuntimeError> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO runtime_catalogue_module_admission
             (object_id, snapshot) VALUES (?1, ?2)",
            params![object_id.to_vec(), capture_bytes(capture)?],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(())
}

async fn is_carried_forward_module_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    predecessor: Option<&orna_foundation_v1::CwdCapture>,
    object_id: [u8; 16],
    qualified_name: &str,
) -> Result<bool, RuntimeError> {
    let Some(predecessor) = predecessor else {
        return Ok(false);
    };
    let predecessor_snapshot = capture_bytes(predecessor)?;
    if lookup_snapshot_object_id(transaction, &predecessor_snapshot, qualified_name).await?
        != Some(object_id)
    {
        return Ok(false);
    }
    Ok(
        module_revision_facts_tx(transaction, capture, object_id).await?
            == module_revision_facts_tx(transaction, predecessor, object_id).await?,
    )
}

async fn module_revision_facts_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    object_id: [u8; 16],
) -> Result<Option<([u8; 32], [u8; 32])>, RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT revision_id, semantic_hash FROM runtime_catalogue_revision
             WHERE object_id = ?1 AND snapshot = ?2 AND kind = ?3",
            params![
                object_id.to_vec(),
                capture_bytes(capture)?,
                CatalogueObjectKind::Module.code()
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok(None);
    };
    Ok(Some((
        fixed(row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?)?,
        fixed(row.get(1).map_err(|_| RuntimeError::CatalogueCorrupt)?)?,
    )))
}

async fn reject_separate_module_rows_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
) -> Result<(), RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT 1 FROM runtime_catalogue_revision
             WHERE snapshot = ?1 AND kind = ?2 LIMIT 1",
            params![capture_bytes(capture)?, CatalogueObjectKind::Module.code()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    if rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .is_some()
    {
        return Err(RuntimeError::CatalogueKindMismatch);
    }
    Ok(())
}

async fn clear_module_revision_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    object_id: [u8; 16],
) -> Result<(), RuntimeError> {
    transaction
        .execute(
            "DELETE FROM runtime_catalogue_revision
             WHERE object_id = ?1 AND snapshot = ?2 AND kind = ?3",
            params![
                object_id.to_vec(),
                capture_bytes(capture)?,
                CatalogueObjectKind::Module.code()
            ],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(())
}

async fn allocate_object_id_tx(
    transaction: &Transaction,
    kind: CatalogueObjectKind,
) -> Result<[u8; 16], RuntimeError> {
    for _ in 0..8 {
        let id = *Uuid::new_v4().as_bytes();
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_catalogue_identity (object_id, kind, created_ms)
                 VALUES (?1, ?2, ?3)",
                params![id.to_vec(), kind.code(), now_ms()?],
            )
            .await
            .map_err(|_| RuntimeError::StorageUnavailable)?;
        if inserted == 1 {
            return Ok(id);
        }
    }
    Err(RuntimeError::CatalogueAllocationExhausted)
}

async fn object_kind_tx(
    transaction: &Transaction,
    id: [u8; 16],
) -> Result<CatalogueObjectKind, RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT kind FROM runtime_catalogue_identity WHERE object_id = ?1",
            params![id.to_vec()],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::CatalogueIdentityMissing)?;
    CatalogueObjectKind::from_code(row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?)
}

async fn lookup_object_id(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    name: &str,
    kind: CatalogueObjectKind,
) -> Result<Option<[u8; 16]>, RuntimeError> {
    lookup_current_object_id(transaction, capture, name, kind).await
}

async fn lookup_current_object_id(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    name: &str,
    kind: CatalogueObjectKind,
) -> Result<Option<[u8; 16]>, RuntimeError> {
    let Some(id) = lookup_snapshot_object_id(transaction, &capture_bytes(capture)?, name).await?
    else {
        return Ok(None);
    };
    if object_kind_tx(transaction, id).await? != kind {
        return Err(RuntimeError::CatalogueKindMismatch);
    }
    Ok(Some(id))
}

async fn lookup_snapshot_object_id(
    transaction: &Transaction,
    snapshot: &[u8],
    name: &str,
) -> Result<Option<[u8; 16]>, RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT object_id FROM runtime_catalogue_revision
             WHERE snapshot = ?1 AND qualified_name = ?2",
            params![snapshot.to_vec(), name],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    else {
        return Ok(None);
    };
    Ok(Some(fixed(
        row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?,
    )?))
}

async fn current_object_revision_exists(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    id: [u8; 16],
) -> Result<bool, RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT 1 FROM runtime_catalogue_revision
             WHERE object_id = ?1 AND snapshot = ?2 LIMIT 1",
            params![id.to_vec(), capture_bytes(capture)?],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    Ok(rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .is_some())
}

fn validate_predecessor_snapshot(
    snapshot: &[u8],
    capture: &orna_foundation_v1::CwdCapture,
) -> Result<(), RuntimeError> {
    let value = Value::decode(snapshot).map_err(|_| RuntimeError::CataloguePredecessorInvalid)?;
    let snapshot =
        Snapshot::decode(value.raw()).map_err(|_| RuntimeError::CataloguePredecessorInvalid)?;
    let Snapshot::Cwd {
        database,
        runtime,
        generation,
        ..
    } = snapshot
    else {
        return Err(RuntimeError::CataloguePredecessorInvalid);
    };
    if database != capture.database_id() || runtime != capture.runtime_id() {
        return Err(RuntimeError::CataloguePredecessorInvalid);
    }
    if generation + 1 != *capture.generation() {
        return Err(RuntimeError::CataloguePredecessorInvalid);
    }
    Ok(())
}

async fn load_object_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    id: [u8; 16],
) -> Result<CatalogueObject, RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT revision.kind, identity.kind, revision.qualified_name, revision.revision_id, revision.semantic_hash
             FROM runtime_catalogue_revision AS revision
             JOIN runtime_catalogue_identity AS identity ON identity.object_id = revision.object_id
             WHERE revision.object_id = ?1 AND revision.snapshot = ?2",
            params![id.to_vec(), capture_bytes(capture)?],
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::CatalogueIdentityMissing)?;
    let kind =
        CatalogueObjectKind::from_code(row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?)?;
    let identity_kind =
        CatalogueObjectKind::from_code(row.get(1).map_err(|_| RuntimeError::CatalogueCorrupt)?)?;
    if kind != identity_kind {
        return Err(RuntimeError::CatalogueKindMismatch);
    }
    let reference = object_reference(capture.database_id(), id, capture.snapshot().clone())
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    Ok(CatalogueObject {
        object_id: id,
        kind,
        qualified_name: row.get(2).map_err(|_| RuntimeError::CatalogueCorrupt)?,
        revision_id: fixed(row.get(3).map_err(|_| RuntimeError::CatalogueCorrupt)?)?,
        semantic_hash: fixed(row.get(4).map_err(|_| RuntimeError::CatalogueCorrupt)?)?,
        reference,
        function_reference: None,
        type_reference: None,
    })
}

async fn load_type_form_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    id: [u8; 16],
) -> Result<CatalogueTypeForm, RuntimeError> {
    let mut rows = transaction
        .query("SELECT form, target_object_id FROM runtime_catalogue_type WHERE object_id = ?1 AND snapshot = ?2", params![id.to_vec(), capture_bytes(capture)?])
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::CatalogueTypeMissing)?;
    CatalogueTypeForm::decode(
        row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?,
        row.get(1).map_err(|_| RuntimeError::CatalogueCorrupt)?,
    )
}

async fn load_type_handle_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    id: [u8; 16],
) -> Result<CatalogueTypeHandle, RuntimeError> {
    let object = load_object_tx(transaction, capture, id).await?;
    if object.kind != CatalogueObjectKind::Type {
        return Err(RuntimeError::CatalogueKindMismatch);
    }
    let reference =
        type_reference(object.reference).map_err(|_| RuntimeError::InvalidObservationReference)?;
    validate_type_reference(reference.as_row_ref().clone(), capture)
        .map_err(|_| RuntimeError::InvalidObservationReference)?;
    let form = load_type_form_tx(transaction, capture, id).await?;
    Ok(CatalogueTypeHandle {
        object_id: id,
        snapshot: capture_bytes(capture)?,
        reference,
        form,
    })
}

async fn load_result_type_id_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    id: [u8; 16],
) -> Result<[u8; 16], RuntimeError> {
    let mut rows = transaction
        .query("SELECT result_type_object_id FROM runtime_catalogue_function WHERE object_id = ?1 AND snapshot = ?2", params![id.to_vec(), capture_bytes(capture)?])
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
        .ok_or(RuntimeError::CatalogueFunctionMissing)?;
    Ok(fixed(
        row.get(0).map_err(|_| RuntimeError::CatalogueCorrupt)?,
    )?)
}

async fn load_parameters_tx(
    transaction: &Transaction,
    capture: &orna_foundation_v1::CwdCapture,
    id: [u8; 16],
) -> Result<Vec<CatalogueParameterHandle>, RuntimeError> {
    let mut rows = transaction
        .query("SELECT position, name, type_object_id FROM runtime_catalogue_parameter WHERE object_id = ?1 AND snapshot = ?2 ORDER BY position", params![id.to_vec(), capture_bytes(capture)?])
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let mut parameters = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| RuntimeError::StorageUnavailable)?
    {
        let position = u64::try_from(
            row.get::<i64>(0)
                .map_err(|_| RuntimeError::CatalogueCorrupt)?,
        )
        .map_err(|_| RuntimeError::CatalogueCorrupt)?;
        let type_id = fixed(row.get(2).map_err(|_| RuntimeError::CatalogueCorrupt)?)?;
        parameters.push(CatalogueParameterHandle {
            name: row.get(1).map_err(|_| RuntimeError::CatalogueCorrupt)?,
            position,
            type_object: load_type_handle_tx(transaction, capture, type_id).await?,
        });
    }
    Ok(parameters)
}

fn validate_function_shape(declaration: &CatalogueFunctionDeclaration) -> Result<(), RuntimeError> {
    validate_observation_text(&declaration.result_type_name)?;
    let mut expected = 0;
    for parameter in &declaration.parameters {
        validate_observation_text(&parameter.name)?;
        validate_observation_text(&parameter.type_name)?;
        if parameter.position != expected {
            return Err(RuntimeError::CatalogueParameterConflict);
        }
        expected = expected
            .checked_add(1)
            .ok_or(RuntimeError::CatalogueCorrupt)?;
    }
    Ok(())
}

fn capture_bytes(capture: &orna_foundation_v1::CwdCapture) -> Result<Vec<u8>, RuntimeError> {
    Ok(crate::encode_capture(capture)?)
}

#[cfg(any())]
mod tests {
    use super::*;
    use crate::{NoFault, RuntimeIdentity, TableMutation};
    use orna_foundation_v1::CwdCapture;
    use tempfile::Builder;

    fn digest(value: u8) -> [u8; 32] {
        [value; 32]
    }

    fn declaration(
        name: &str,
        kind: CatalogueObjectKind,
        revision: u8,
        semantic: u8,
        predecessor_snapshot: Option<Vec<u8>>,
        rename_from: Option<&str>,
    ) -> CatalogueDeclaration {
        CatalogueDeclaration {
            qualified_name: name.to_owned(),
            kind,
            revision_id: digest(revision),
            semantic_hash: digest(semantic),
            predecessor_snapshot,
            rename_from: rename_from.map(str::to_owned),
        }
    }

    async fn state() -> (tempfile::TempDir, RuntimeState) {
        let directory = Builder::new()
            .prefix("orna-runtime-catalogue-")
            .tempdir_in("../../target")
            .unwrap();
        let state = RuntimeState::open_path(
            &directory.path().join("state.db"),
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            digest(3),
            None,
        )
        .await
        .unwrap();
        (directory, state)
    }

    async fn advance(state: &RuntimeState, lease: crate::WriterLease, value: u8) -> CwdCapture {
        let context = state.begin_activation().await.unwrap();
        let mutation = TableMutation::new(
            [value; 16],
            "catalogue-test",
            vec![value],
            Some(vec![value]),
        )
        .unwrap();
        state
            .commit_table_activation(lease, &context, &[mutation], digest(value), &NoFault)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn exact_type_form_is_persisted_and_reference_is_not_named_type() {
        let (_directory, state) = state().await;
        let named = state
            .admit_catalogue_type(
                declaration("pkg.T", CatalogueObjectKind::Type, 4, 5, None, None),
                CatalogueTypeForm::Named,
            )
            .await
            .unwrap();
        let reference = state
            .admit_catalogue_type(
                declaration("pkg.RefT", CatalogueObjectKind::Type, 6, 7, None, None),
                CatalogueTypeForm::Reference {
                    target: named.object_id(),
                },
            )
            .await
            .unwrap();
        assert_ne!(named.object_id(), reference.object_id());
        assert_eq!(named.form(), CatalogueTypeForm::Named);
        assert_eq!(
            reference.form(),
            CatalogueTypeForm::Reference {
                target: named.object_id()
            }
        );
        assert_eq!(
            state
                .catalogue_type(
                    "pkg.T",
                    CatalogueTypeForm::Reference {
                        target: named.object_id(),
                    }
                )
                .await,
            Err(RuntimeError::CatalogueTypeMismatch)
        );
    }

    #[tokio::test]
    async fn same_snapshot_conflicts_do_not_overwrite_or_leave_trailing_parameters() {
        let (_directory, state) = state().await;
        let result = state
            .admit_catalogue_type(
                declaration("pkg.Result", CatalogueObjectKind::Type, 8, 9, None, None),
                CatalogueTypeForm::Named,
            )
            .await
            .unwrap();
        let parameter_type = state
            .admit_catalogue_type(
                declaration("pkg.Param", CatalogueObjectKind::Type, 10, 11, None, None),
                CatalogueTypeForm::Named,
            )
            .await
            .unwrap();
        let function_declaration = |parameters| CatalogueFunctionDeclaration {
            declaration: declaration("pkg.f", CatalogueObjectKind::Function, 12, 13, None, None),
            parameters,
            result_type: result.clone(),
        };
        state
            .admit_catalogue_function(function_declaration(vec![CatalogueParameter {
                name: "x".into(),
                position: 0,
                type_object: parameter_type.clone(),
            }]))
            .await
            .unwrap();
        assert_eq!(
            state
                .admit_catalogue_type(
                    declaration("pkg.Result", CatalogueObjectKind::Type, 8, 9, None, None),
                    CatalogueTypeForm::Value,
                )
                .await,
            Err(RuntimeError::CatalogueTypeMismatch)
        );
        assert_eq!(
            state
                .admit_catalogue_function(function_declaration(Vec::new()))
                .await,
            Err(RuntimeError::CatalogueRevisionConflict)
        );
        let function = state.catalogue_function("pkg.f").await.unwrap().unwrap();
        assert_eq!(function.parameters.len(), 1);
        assert_eq!(function.parameters[0].name, "x");
    }

    #[tokio::test]
    async fn predecessor_snapshot_preserves_identity_across_edit_rename_and_data_only_generation() {
        let (_directory, state) = state().await;
        let lease = state.acquire_lease([16; 16]).await.unwrap();
        let first = state
            .admit_catalogue_type(
                declaration("pkg.T", CatalogueObjectKind::Type, 14, 15, None, None),
                CatalogueTypeForm::Named,
            )
            .await
            .unwrap();
        let first_snapshot = capture_bytes(&capture_tx(&state.connection).await.unwrap()).unwrap();

        let second_capture = advance(&state, lease, 16).await;
        let second = state
            .admit_catalogue_type(
                declaration(
                    "pkg.T",
                    CatalogueObjectKind::Type,
                    17,
                    18,
                    Some(first_snapshot.clone()),
                    None,
                ),
                CatalogueTypeForm::Named,
            )
            .await
            .unwrap();
        assert_eq!(first.object_id(), second.object_id());
        let second_snapshot = capture_bytes(&second_capture).unwrap();

        let third_capture = advance(&state, lease, 19).await;
        let renamed = state
            .admit_catalogue_type(
                declaration(
                    "pkg.Renamed",
                    CatalogueObjectKind::Type,
                    20,
                    21,
                    Some(second_snapshot.clone()),
                    Some("pkg.T"),
                ),
                CatalogueTypeForm::Named,
            )
            .await
            .unwrap();
        assert_eq!(second.object_id(), renamed.object_id());

        let fourth_capture = advance(&state, lease, 22).await;
        let data_only = state
            .admit_catalogue_type(
                declaration(
                    "pkg.Renamed",
                    CatalogueObjectKind::Type,
                    20,
                    21,
                    Some(capture_bytes(&third_capture).unwrap()),
                    None,
                ),
                CatalogueTypeForm::Named,
            )
            .await
            .unwrap();
        assert_eq!(renamed.object_id(), data_only.object_id());
        assert_eq!(
            data_only.snapshot(),
            capture_bytes(&fourth_capture).unwrap()
        );

        let mut rows = state
            .connection
            .query(
                "SELECT qualified_name, revision_id FROM runtime_catalogue_revision
                 WHERE object_id = ?1 AND snapshot = ?2",
                params![first.object_id().to_vec(), first_snapshot],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let old_name: String = row.get(0).unwrap();
        let old_revision: Vec<u8> = row.get(1).unwrap();
        assert_eq!(old_name, "pkg.T");
        assert_eq!(old_revision, digest(14).to_vec());
    }

    #[tokio::test]
    async fn reopening_reads_the_authoritative_identity_row() {
        let (directory, state) = state().await;
        let admitted = state
            .admit_catalogue_type(
                declaration("pkg.Reopen", CatalogueObjectKind::Type, 23, 24, None, None),
                CatalogueTypeForm::Named,
            )
            .await
            .unwrap();
        let object_id = admitted.object_id();
        drop(state);
        let reopened = RuntimeState::open_path(
            &directory.path().join("state.db"),
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            digest(3),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            reopened
                .catalogue_type("pkg.Reopen", CatalogueTypeForm::Named)
                .await
                .unwrap()
                .unwrap()
                .object_id(),
            object_id
        );
    }
}
