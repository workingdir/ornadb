//! PUB-1 crash proof for the boundary after the runtime consumes a compact
//! frozen prefix but before the repository accepts its signed receipt.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_repository_v1::{
    CompactManifest, CompactSegment, CompactSegmentRole, ManagedPath, Repository, Uuid,
};
use orna_runtime_v1::{Checkpoint, RuntimeIdentity, RuntimeState, TableMutation};
use orna_storage_v1::{CompactTailWindow, RuntimePublicationCoordinator};
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
async fn recovery_finishes_receipt_boundary_after_runtime_consumes_only_frozen_prefix() {
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

    let pending = repository
        .publish_compact_repository_boundary(plan(&repository, &freeze))
        .unwrap();
    drop(
        runtime
            .complete_compact_publication(&pending, &freeze)
            .await
            .unwrap(),
    );
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

    // Simulate process loss after Turso commits the prefix receipt and before
    // the repository journal accepts it. Recovery must verify that same
    // receipt, finish the journal, and leave post-freeze mutations pending.
    assert!(repository.read_publication_journal().unwrap().is_some());
    assert!(runtime.pending_through(&freeze).await.unwrap().is_empty());
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    assert_eq!(
        RuntimePublicationCoordinator::compact_reader_visibility(&repository, &runtime)
            .await
            .unwrap()
            .tail(),
        CompactTailWindow::AfterWatermark(freeze.checkpoint.mutation_sequence),
    );

    RuntimePublicationCoordinator::recover(&repository, &runtime)
        .await
        .unwrap();

    assert!(repository.read_publication_journal().unwrap().is_none());
    assert_eq!(runtime.pending().await.unwrap().len(), 1);
    assert_eq!(runtime.pending().await.unwrap()[0].id, [78; 16]);
}

#[tokio::test]
async fn recovery_after_ref_advance_before_runtime_receipt() {
    let (temp, repository) = repository();
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
    assert_eq!(repository.head().unwrap().as_ref(), Some(journal.new_head()));
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
    RuntimePublicationCoordinator::recover(&repository, &runtime)
        .await
        .unwrap();

    assert!(repository.read_publication_journal().unwrap().is_none());
    let pending = runtime.pending().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, [98; 16]);
    assert!(runtime.pending_through(&freeze).await.unwrap().is_empty());
    let visibility = RuntimePublicationCoordinator::compact_reader_visibility(&repository, &runtime)
        .await
        .unwrap();
    assert_eq!(visibility.snapshot(), &repository.head().unwrap().unwrap());
    assert_eq!(visibility.tail(), CompactTailWindow::AllPending);
    let manifest = repository
        .read_compact_manifest(visibility.snapshot(), Uuid::from_u128(1))
        .unwrap()
        .unwrap();
    assert_eq!(manifest.entries().len(), 1);
    assert_eq!(manifest.entries()[0].row_count(), 1);

    let index = repository.index_generation().unwrap();
    assert_eq!(index.head(), Some(visibility.snapshot()));
    let head_tree = Command::new("git")
        .current_dir(temp.path())
        .args(["rev-parse", "HEAD^{tree}"])
        .output()
        .unwrap();
    assert!(head_tree.status.success());
    assert_eq!(
        index.tree().unwrap().as_str(),
        String::from_utf8_lossy(&head_tree.stdout).trim(),
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
