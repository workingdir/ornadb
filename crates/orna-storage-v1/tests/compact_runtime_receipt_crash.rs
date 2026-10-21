//! PUB-1 crash proof for the boundary after the runtime consumes a compact
//! frozen prefix but before the repository accepts its signed receipt.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_repository_v1::{
    CompactManifest, CompactSegment, CompactSegmentRole, ManagedPath, Repository, Uuid,
};
use orna_runtime_v1::{
    Checkpoint, PublicationTableIdentity, RuntimeIdentity, RuntimeState, TableMutation,
};
use orna_storage_v1::{CompactOvbProfile, CompactTailWindow, RuntimePublicationCoordinator};
use parquet::file::{
    metadata::{FileMetaData, KeyValue, ParquetMetaData, ParquetMetaDataWriter},
    reader::{FileReader, SerializedFileReader},
};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

const TABLE: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

fn git(directory: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> (TempDir, Repository) {
    let temp = TempDir::new().unwrap();
    git(temp.path(), &["init", "-b", "main"]);
    git(temp.path(), &["config", "user.email", "kieran@drewett.dev"]);
    git(temp.path(), &["config", "user.name", "kierandrewett"]);
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(
        temp.path().join("main.orna"),
        include_str!("fixtures/compact-runtime-receipt-main.orna"),
    )
    .unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-m", "initial"]);
    let repository = Repository::discover(temp.path()).unwrap();
    (temp, repository)
}

fn uuid_raw(value: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(value.to_vec())))
}

fn profile() -> SchemaDescriptor {
    let field = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);
    SchemaDescriptor::new(OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (OvbRaw::Int(1.into()), uuid_raw(TABLE)),
        (
            OvbRaw::Int(2.into()),
            OvbRaw::Array(vec![uuid_raw(*field.as_bytes())]),
        ),
        (
            OvbRaw::Int(3.into()),
            OvbRaw::Array(vec![OvbRaw::Array(vec![
                uuid_raw(*field.as_bytes()),
                OvbRaw::Text(format!("f_{}", field.simple())),
                OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Int".into())]),
                OvbRaw::Int(0.into()),
                OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
            ])]),
        ),
        (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
    ]))
    .unwrap()
}

fn compact_columns() -> Vec<u8> {
    let field = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);
    CanonicalValue::new(OvbRaw::Array(vec![OvbRaw::Array(vec![
        OvbRaw::Array(vec![uuid_raw(*field.as_bytes())]),
        OvbRaw::Array(vec![OvbRaw::Text(format!("f_{}", field.simple()))]),
        OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Int".into())]),
        OvbRaw::Text("int64".into()),
        OvbRaw::Array(Vec::new()),
    ])]))
    .unwrap()
    .encode()
    .unwrap()
}

fn schema_fingerprint(schema: &SchemaDescriptor) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"orna.schema.v1\0");
    digest.update(schema.encode().unwrap());
    digest.finalize().into()
}

fn publication_identity() -> PublicationTableIdentity {
    PublicationTableIdentity {
        table: "Contact".to_owned(),
        table_id: TABLE,
        schema_fingerprint: schema_fingerprint(&profile()),
        candidate_generation: 1,
    }
}

fn decode_base64(value: &str) -> Vec<u8> {
    BASE64.decode(value).unwrap()
}

fn parquet_fixture() -> (Vec<u8>, [u8; 32]) {
    const PARQUET: &str = "UEFSMRUGFRAVEBWRgLnnCkwVAhUAFQIVABUAFQASAAABAAAAAAAAABkSAhkYCAEAAAAAAAAAGRgIAQAAAAAAAAAVAhkWAAAZHBYIFTwWAAAAFQIZLEgGc2NoZW1hFQIAFQQlABgiZl8wMThmMDAwMDAwMDA3MDAwODAwMDAwMDAwMDAwMDAwMQAWAhkcGRwmABwVBBklAAYZGCJmXzAxOGYwMDAwMDAwMDcwMDA4MDAwMDAwMDAwMDAwMDAxFQwWAhY8FkgmCDw2ACgIAQAAAAAAAAAYCAEAAAAAAAAAEREAABaCARUUFkQVPgAWPBYCJggWSBQAABl8GAxvcm5hLnByb2ZpbGUYEmNvbXBhY3Qtc3RvcmFnZS12MQAYCm9ybmEudGFibGUYJDAwMDAwMDAwLTAwMDAtMDAwMC0wMDAwLTAwMDAwMDAwMDAwMQAYEm9ybmEuc2NoZW1hLnNoYTI1NhhANThhMDg5NzBhZGMyMzE4N2U5Nzk0NDVlOTNiODQzN2ExNjI3ZWUxMDk0ZjI2NTg5YTJlZmVmZWE4NTFiMmExZQAYD29ybmEuc2NoZW1hLm92YhiYAXBRQUJBZGdsVUFBQUFBQUFBQUFBQUFBQUFBQUFBQUVDZ2RnbFVBR1BBQUFBQUhBQWdBQUFBQUFBQUFFRGdZWFlKVkFCandBQUFBQndBSUFBQUFBQUFBQUJlQ0ptWHpBeE9HWXdNREF3TURBd01EY3dNREE0TURBd01EQXdNREF3TURBd01EQXhnZ0JqU1c1MEFJRUFCSUE9ABgQb3JuYS5jb2x1bW5zLm92YhhgZ1lXQjJDVlFBWThBQUFBQWNBQ0FBQUFBQUFBQUFZRjRJbVpmTURFNFpqQXdNREF3TURBd056QXdNRGd3TURBd01EQXdNREF3TURBd01ER0NBR05KYm5SbGFXNTBOalNBABgMb3JuYS5lbmNvZGVyGA90ZXN0LWVuY29kZXItdjEAGBFvcm5hLnRlc3QucGF5bG9hZBgOY29tcGFjdCBvYmplY3QAGBlwYXJxdWV0LXJzIHZlcnNpb24gNTkuMy4wGRwcAAAA3AIAAFBBUjE=";
    let original = decode_base64(PARQUET);
    let reader = SerializedFileReader::new(Bytes::from(original.clone())).unwrap();
    let file = reader.metadata().file_metadata();
    let schema = profile();
    let fingerprint = schema_fingerprint(&schema);
    let hex = fingerprint
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let metadata = FileMetaData::new(
        1,
        file.num_rows(),
        file.created_by().map(str::to_owned),
        Some(vec![
            KeyValue::new("orna.profile".into(), Some("compact-storage-v1".into())),
            KeyValue::new("orna.table".into(), Some(Uuid::from_u128(1).to_string())),
            KeyValue::new("orna.schema.sha256".into(), Some(hex)),
            KeyValue::new(
                "orna.schema.ovb".into(),
                Some(BASE64.encode(schema.encode().unwrap())),
            ),
            KeyValue::new(
                "orna.columns.ovb".into(),
                Some(BASE64.encode(compact_columns())),
            ),
            KeyValue::new("orna.encoder".into(), Some("test-encoder-v1".into())),
        ]),
        file.schema_descr_ptr(),
        file.column_orders().cloned(),
    );
    let metadata = ParquetMetaData::new(metadata, reader.metadata().row_groups().to_vec());
    let mut footer = Vec::new();
    ParquetMetaDataWriter::new(&mut footer, &metadata)
        .finish()
        .unwrap();
    let footer_start = original.len()
        - 8
        - u32::from_le_bytes(
            original[original.len() - 8..original.len() - 4]
                .try_into()
                .unwrap(),
        ) as usize;
    let mut bytes = original[..footer_start].to_vec();
    bytes.extend(footer);
    (bytes, fingerprint)
}

fn plan(
    repository: &Repository,
    freeze: &orna_runtime_v1::PublicationFreeze,
) -> orna_repository_v1::CompactPublicationPlan {
    let (parquet, schema) = parquet_fixture();
    let table = Uuid::from_u128(1);
    let id = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);
    let segment = CompactSegment::new(
        id,
        CompactSegmentRole::Data,
        schema,
        "test-encoder-v1",
        ManagedPath::new(format!(
            ".orna/storage/{table}/data/{}/{id}.parquet",
            &id.to_string()[..2]
        ))
        .unwrap(),
        parquet,
        CanonicalValue::int(1.into()).encode().unwrap(),
        CanonicalValue::int(1.into()).encode().unwrap(),
        1,
        compact_columns(),
        true,
        false,
    )
    .unwrap();
    let head = repository.head().unwrap().unwrap();
    repository
        .prepare_compact_publication(
            &head,
            repository.index_generation().unwrap(),
            CompactManifest::empty(table, schema),
            freeze.intent_id,
            freeze.checkpoint.digest,
            &[segment],
            "orna: publish compact runtime data",
        )
        .unwrap()
}

#[tokio::test]
async fn recovery_uses_persisted_receipt_after_runtime_consumes_prefix() {
    let (_temp, repository) = repository();
    let runtime = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [71; 16],
            repository_id: [72; 16],
        },
        [73; 32],
    )
    .await
    .unwrap();
    let lease = runtime.acquire_lease([74; 16]).await.unwrap();
    let context = runtime.begin_activation().await.unwrap();
    runtime
        .commit_table_activation(
            lease,
            &context,
            &[TableMutation::new(
                [75; 16],
                "Contact",
                b"compact row".to_vec(),
                Some(b"compact row".to_vec()),
            )
            .unwrap()],
            [76; 32],
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();
    let freeze = runtime
        .freeze(
            [77; 16],
            &Checkpoint {
                generation: 1,
                digest: [76; 32],
                mutation_sequence: 1,
            },
        )
        .await
        .unwrap();
    let tail = runtime.begin_activation().await.unwrap();
    runtime
        .commit_table_activation(
            lease,
            &tail,
            &[TableMutation::new(
                [78; 16],
                "Contact",
                b"tail row".to_vec(),
                Some(b"tail row".to_vec()),
            )
            .unwrap()],
            [79; 32],
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();

    repository
        .publish_compact_repository_boundary(plan(&repository, &freeze))
        .unwrap();
    let pending = match repository
        .recover_compact_publication_boundary()
        .unwrap()
        .unwrap()
    {
        orna_repository_v1::CompactPublicationRecovery::PendingRuntimeReceipt(pending) => pending,
        orna_repository_v1::CompactPublicationRecovery::ReconciliationRequired(_) => {
            panic!("freshly published candidate needs no reconciliation")
        }
    };
    let journal = repository.read_publication_journal().unwrap().unwrap();
    let source = journal.old_head().clone();
    assert_eq!(
        orna_storage_v1::trusted_compact_schema_descriptor(
            &repository,
            &source,
            Uuid::from_u128(1),
            schema_fingerprint(&profile()),
            &journal,
            pending.commit(),
        )
        .unwrap(),
        profile()
    );
    let mut mismatched_schema = schema_fingerprint(&profile());
    mismatched_schema[0] ^= 1;
    assert!(
        orna_storage_v1::trusted_compact_schema_descriptor(
            &repository,
            &source,
            Uuid::from_u128(1),
            mismatched_schema,
            &journal,
            pending.commit(),
        )
        .is_err()
    );
    assert!(
        orna_storage_v1::trusted_compact_schema_descriptor(
            &repository,
            pending.commit(),
            Uuid::from_u128(1),
            schema_fingerprint(&profile()),
            &journal,
            pending.commit(),
        )
        .is_err()
    );
    runtime
        .complete_compact_publication(&pending, &freeze)
        .await
        .unwrap();
    assert!(runtime.pending_through(&freeze).await.unwrap().is_empty());
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    drop(runtime);
    let runtime = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [71; 16],
            repository_id: [72; 16],
        },
        [73; 32],
    )
    .await
    .unwrap();

    // Simulate process loss after the runtime atomically persists the signed
    // receipt and consumes the prefix but before repository journal finalization.
    assert!(repository.read_publication_journal().unwrap().is_some());
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    let mut mismatched_identity = publication_identity();
    mismatched_identity.table_id = [80; 16];
    mismatched_identity.schema_fingerprint[0] ^= 1;
    mismatched_identity.candidate_generation += 1;
    // A persisted receipt no longer needs source schema identity to finalize.
    assert!(
        RuntimePublicationCoordinator::recover_compact_validated(
            &repository,
            &runtime,
            &CompactOvbProfile::new(profile()).unwrap(),
            &mismatched_identity,
        )
        .await
        .unwrap()
        .is_some()
    );
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    assert!(repository.read_publication_journal().unwrap().is_none());
}

#[tokio::test]
async fn recovery_preserves_rows_when_compact_freeze_identity_is_missing() {
    let (_temp, repository) = repository();
    let identity = RuntimeIdentity {
        database_id: [91; 16],
        repository_id: [92; 16],
    };
    let runtime = RuntimeState::open(&repository, identity, [93; 32])
        .await
        .unwrap();
    let lease = runtime.acquire_lease([94; 16]).await.unwrap();
    let context = runtime.begin_activation().await.unwrap();
    runtime
        .commit_table_activation(
            lease,
            &context,
            &[TableMutation::new(
                [95; 16],
                "Contact",
                b"compact row".to_vec(),
                Some(b"compact row".to_vec()),
            )
            .unwrap()],
            [96; 32],
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();
    let freeze = runtime
        .freeze(
            [97; 16],
            &Checkpoint {
                generation: 1,
                digest: [96; 32],
                mutation_sequence: 1,
            },
        )
        .await
        .unwrap();
    let tail = runtime.begin_activation().await.unwrap();
    runtime
        .commit_table_activation(
            lease,
            &tail,
            &[TableMutation::new(
                [98; 16],
                "Contact",
                b"tail row".to_vec(),
                Some(b"tail row".to_vec()),
            )
            .unwrap()],
            [99; 32],
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();

    let old_head = repository.head().unwrap().unwrap();
    repository
        .publish_compact_repository_boundary(plan(&repository, &freeze))
        .unwrap();
    let journal = repository.read_publication_journal().unwrap().unwrap();
    assert_ne!(journal.old_head(), journal.new_head());
    assert_eq!(journal.old_head(), &old_head);
    assert_eq!(
        repository.head().unwrap().as_ref(),
        Some(journal.new_head())
    );
    assert_eq!(runtime.pending_through(&freeze).await.unwrap().len(), 1);
    assert_eq!(runtime.pending().await.unwrap().len(), 2);
    assert!(
        orna_storage_v1::trusted_compact_schema_descriptor(
            &repository,
            journal.old_head(),
            Uuid::from_u128(1),
            schema_fingerprint(&profile()),
            &journal,
            journal.old_head(),
        )
        .is_err()
    );
    assert!(
        orna_storage_v1::trusted_compact_schema_descriptor(
            &repository,
            journal.new_head(),
            Uuid::from_u128(1),
            schema_fingerprint(&profile()),
            &journal,
            journal.new_head(),
        )
        .is_err()
    );
    assert_eq!(
        repository.head().unwrap().as_ref(),
        Some(journal.new_head())
    );
    assert_eq!(
        repository.read_publication_journal().unwrap(),
        Some(journal.clone())
    );
    assert_eq!(runtime.pending_through(&freeze).await.unwrap().len(), 1);
    assert_eq!(runtime.pending().await.unwrap().len(), 2);
    assert_eq!(
        RuntimePublicationCoordinator::compact_reader_visibility(&repository, &runtime)
            .await
            .unwrap()
            .tail(),
        CompactTailWindow::AllPending,
    );

    // Process loss at this point leaves the runtime receipt absent while the
    // repository ref, index, worktree, and publication journal have advanced.
    drop(runtime);
    let runtime = RuntimeState::open(&repository, identity, [93; 32])
        .await
        .unwrap();
    assert_eq!(
        RuntimePublicationCoordinator::recover_compact_validated(
            &repository,
            &runtime,
            &CompactOvbProfile::new(profile()).unwrap(),
            &publication_identity(),
        )
        .await,
        Err(orna_storage_v1::Error::InvalidTransition)
    );
    assert!(repository.read_publication_journal().unwrap().is_some());
    let pending = runtime.pending().await.unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(runtime.pending_through(&freeze).await.unwrap().len(), 1);
    assert_eq!(
        repository.head().unwrap().as_ref(),
        Some(journal.new_head())
    );
    assert!(repository.worktree_state().unwrap().is_clean());
}

#[tokio::test]
async fn stale_candidate_before_ref_advance_preserves_frozen_rows_and_new_head() {
    let (temp, repository) = repository();
    let runtime = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [81; 16],
            repository_id: [82; 16],
        },
        [83; 32],
    )
    .await
    .unwrap();
    let lease = runtime.acquire_lease([84; 16]).await.unwrap();
    let context = runtime.begin_activation().await.unwrap();
    runtime
        .commit_table_activation(
            lease,
            &context,
            &[TableMutation::new(
                [85; 16],
                "Contact",
                b"compact row".to_vec(),
                Some(b"compact row".to_vec()),
            )
            .unwrap()],
            [86; 32],
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();
    let freeze = runtime
        .freeze(
            [87; 16],
            &Checkpoint {
                generation: 1,
                digest: [86; 32],
                mutation_sequence: 1,
            },
        )
        .await
        .unwrap();
    let stale_plan = plan(&repository, &freeze);
    let base_head = repository.head().unwrap().unwrap();

    fs::write(
        temp.path().join("ordinary.txt"),
        "concurrent native commit\n",
    )
    .unwrap();
    git(temp.path(), &["add", "ordinary.txt"]);
    git(
        temp.path(),
        &["commit", "-m", "native writer advances HEAD"],
    );
    let native_head = repository.head().unwrap().unwrap();
    assert_ne!(native_head, base_head);

    assert!(
        repository
            .publish_compact_repository_boundary(stale_plan)
            .is_err()
    );

    assert_eq!(repository.head().unwrap().as_ref(), Some(&native_head));
    assert!(repository.read_publication_journal().unwrap().is_none());
    assert_eq!(runtime.pending_through(&freeze).await.unwrap().len(), 1);
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
}

#[tokio::test]
async fn compact_ref_advance_after_runtime_commit_preserves_branch_update() {
    let (temp, repository) = repository();
    let identity = RuntimeIdentity {
        database_id: [101; 16],
        repository_id: [102; 16],
    };
    let runtime = RuntimeState::open(&repository, identity, [103; 32])
        .await
        .unwrap();
    let lease = runtime.acquire_lease([104; 16]).await.unwrap();
    let context = runtime.begin_activation().await.unwrap();
    runtime
        .commit_table_activation(
            lease,
            &context,
            &[TableMutation::new(
                [105; 16],
                "Contact",
                b"compact row".to_vec(),
                Some(b"compact row".to_vec()),
            )
            .unwrap()],
            [106; 32],
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();
    let freeze = runtime
        .freeze(
            [107; 16],
            &Checkpoint {
                generation: 1,
                digest: [106; 32],
                mutation_sequence: 1,
            },
        )
        .await
        .unwrap();
    let tail = runtime.begin_activation().await.unwrap();
    runtime
        .commit_table_activation(
            lease,
            &tail,
            &[TableMutation::new(
                [108; 16],
                "Contact",
                b"tail row".to_vec(),
                Some(b"tail row".to_vec()),
            )
            .unwrap()],
            [109; 32],
            &orna_runtime_v1::NoFault,
        )
        .await
        .unwrap();
    repository
        .publish_compact_repository_boundary(plan(&repository, &freeze))
        .unwrap();
    let pending = match repository
        .recover_compact_publication_boundary()
        .unwrap()
        .unwrap()
    {
        orna_repository_v1::CompactPublicationRecovery::PendingRuntimeReceipt(pending) => pending,
        orna_repository_v1::CompactPublicationRecovery::ReconciliationRequired(_) => {
            panic!("newly published compact candidate needs no reconciliation")
        }
    };
    let receipt = runtime
        .complete_compact_publication(&pending, &freeze)
        .await
        .unwrap();
    assert_eq!(receipt.commit(), pending.commit());
    assert_eq!(
        runtime
            .persisted_compact_publication_receipt(&pending)
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    runtime.validate_recovery().await.unwrap();

    fs::write(
        temp.path().join("ordinary.txt"),
        "branch update after runtime commit\n",
    )
    .unwrap();
    git(temp.path(), &["add", "ordinary.txt"]);
    git(
        temp.path(),
        &["commit", "-m", "branch update after receipt"],
    );
    let branch_head = repository.head().unwrap().unwrap();
    assert_ne!(branch_head, *pending.commit());
    let branch_index = repository.index_generation().unwrap();
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    // ORNA-PUB-009: an advanced branch requires reconciliation; ancestor acceptance is rejected.
    assert_eq!(
        RuntimePublicationCoordinator::recover_compact_validated(
            &repository,
            &runtime,
            &CompactOvbProfile::new(profile()).unwrap(),
            &publication_identity(),
        )
        .await,
        Err(orna_storage_v1::Error::RefConflict)
    );
    assert_eq!(repository.head().unwrap(), Some(branch_head));
    assert_eq!(repository.index_generation().unwrap(), branch_index);
    assert!(repository.read_publication_journal().unwrap().is_some());
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    assert_eq!(
        runtime
            .persisted_compact_publication_receipt(&pending)
            .await
            .unwrap(),
        Some(receipt)
    );
    runtime.validate_recovery().await.unwrap();
}
