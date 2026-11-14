use std::os::unix::fs::PermissionsExt as _;

use super::{
    Errno, VFS_EDITOR_DRAFT_DIRECTORY, VFS_RETAINED_DRAFT_DIRECTORY, create_editor_draft,
    denied_attribute_change, refused_mutation_errno,
};

#[test]
fn editor_draft_is_private_exclusive_and_unpredictable() {
    let runtime = tempfile::tempdir().unwrap();
    let (first, first_path) = create_editor_draft(runtime.path()).unwrap();
    let (_second, second_path) = create_editor_draft(runtime.path()).unwrap();
    let directory = runtime
        .path()
        .join(VFS_RETAINED_DRAFT_DIRECTORY)
        .join(VFS_EDITOR_DRAFT_DIRECTORY);
    assert_eq!(first_path.parent(), Some(directory.as_path()));
    assert_eq!(second_path.parent(), Some(directory.as_path()));
    assert_ne!(first_path, second_path);
    assert_eq!(
        std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&first_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(first);
}

#[test]
fn ownership_mode_and_flag_changes_are_denied_with_eperm() {
    assert_eq!(denied_attribute_change(None, None, None, None), None);
    assert_eq!(
        denied_attribute_change(Some(0o600), None, None, None),
        Some(Errno::EPERM)
    );
    assert_eq!(
        denied_attribute_change(None, Some(0), None, None),
        Some(Errno::EPERM)
    );
    assert_eq!(
        denied_attribute_change(None, None, Some(0), None),
        Some(Errno::EPERM)
    );
}

#[test]
fn editor_draft_directory_pre_created_symlink_is_refused() {
    let runtime = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let retained = runtime.path().join(VFS_RETAINED_DRAFT_DIRECTORY);
    std::fs::create_dir(&retained).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), retained.join(VFS_EDITOR_DRAFT_DIRECTORY))
        .unwrap();
    let error = create_editor_draft(runtime.path()).unwrap_err();
    assert!(error.contains("not a real directory"), "{error}");
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn editor_draft_directory_pre_created_world_writable_is_tightened() {
    let runtime = tempfile::tempdir().unwrap();
    let directory = runtime
        .path()
        .join(VFS_RETAINED_DRAFT_DIRECTORY)
        .join(VFS_EDITOR_DRAFT_DIRECTORY);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777)).unwrap();
    let (_draft, _path) = create_editor_draft(runtime.path()).unwrap();
    assert_eq!(
        std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn editor_draft_creation_refuses_when_leftover_store_is_full() {
    let runtime = tempfile::tempdir().unwrap();
    let directory = runtime
        .path()
        .join(VFS_RETAINED_DRAFT_DIRECTORY)
        .join(VFS_EDITOR_DRAFT_DIRECTORY);
    std::fs::create_dir_all(&directory).unwrap();
    for index in 0..super::VFS_RETAINED_DRAFT_LIMIT {
        std::fs::write(directory.join(format!("orna-edit-{index}.draft")), b"").unwrap();
    }
    let error = create_editor_draft(runtime.path()).unwrap_err();
    assert!(error.contains("editor draft store is full"), "{error}");
}

#[test]
fn owner_draft_read_refuses_a_symlink_and_reads_a_regular_draft() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("elsewhere");
    std::fs::write(&target, b"outside the draft store").unwrap();
    let draft = directory.path().join("orna-edit-1.draft");
    std::os::unix::fs::symlink(&target, &draft).unwrap();
    assert!(super::read_regular_draft(&draft).is_err());
    std::fs::remove_file(&draft).unwrap();
    std::fs::write(&draft, b"candidate").unwrap();
    assert_eq!(super::read_regular_draft(&draft).unwrap(), b"candidate");
}

#[test]
fn accepted_edit_reports_success_even_when_its_draft_cannot_be_removed() {
    let directory = tempfile::tempdir().unwrap();
    // A directory at the draft path makes remove_file fail after a durable commit.
    let undeletable = directory.path().join("orna-edit-1.draft");
    std::fs::create_dir(&undeletable).unwrap();
    assert!(super::admit_edit_result(Ok(()), &undeletable).is_ok());
    assert!(
        undeletable.is_dir(),
        "the undeletable draft is left in place"
    );
    // A draft that is already gone is still an accepted edit.
    let gone = directory.path().join("orna-edit-2.draft");
    assert!(super::admit_edit_result(Ok(()), &gone).is_ok());
}

#[test]
fn accepted_edit_removes_its_draft_and_refusals_retain_it() {
    let directory = tempfile::tempdir().unwrap();
    let draft = directory.path().join("orna-edit-3.draft");
    std::fs::write(&draft, b"candidate").unwrap();
    assert!(super::admit_edit_result(Err(super::DocumentCommitError::Stale), &draft).is_err());
    assert!(draft.exists(), "a refused edit retains its draft");
    assert!(super::admit_edit_result(Ok(()), &draft).is_ok());
    assert!(!draft.exists(), "an accepted edit removes its draft");
}

#[test]
fn release_names_an_unrefused_candidate_unsynced_and_keeps_a_refusal_cause() {
    assert_eq!(super::release_retention_cause(None), "unsynced");
    let refusal = super::DraftRefusal {
        path: String::new(),
        cause: "sys.vfs.stale_edit",
        handle: None,
        candidate: None,
        draft: None,
        unretained: Vec::new(),
    };
    assert_eq!(
        super::release_retention_cause(Some(&refusal)),
        "sys.vfs.stale_edit"
    );
}

/// A 64 MiB candidate is retained without a whole copy: every candidate read
/// is at most one chunk, and the record holds all of its bytes.
#[test]
fn retained_candidate_streams_in_bounded_chunks() {
    const LEN: u64 = 64 * 1024 * 1024;
    let runtime = tempfile::tempdir().unwrap();
    let quota = std::sync::Arc::new(orna_runtime::vfs::DraftQuota::new(1 << 30));
    let store = super::VfsRetainedDraftStore::open(runtime.path(), quota).unwrap();
    let largest_read = std::cell::Cell::new(0_usize);
    let total_read = std::cell::Cell::new(0_u64);
    let mut read_chunk = |offset: u64, length: usize| -> Result<Vec<u8>, super::RetentionError> {
        largest_read.set(largest_read.get().max(length));
        total_read.set(total_read.get() + length as u64);
        Ok((offset..offset + length as u64)
            .map(|index| (index % 251) as u8)
            .collect())
    };
    let mut candidate = orna_runtime::vfs::QuotaCharge::empty(&store.quota);
    let retained = store
        .retain(
            "/music/big.orna",
            "refused",
            LEN,
            &mut candidate,
            &mut read_chunk,
        )
        .unwrap();
    assert!(largest_read.get() > 0);
    assert!(largest_read.get() <= super::CANDIDATE_CHUNK_BYTES);
    // The digest pass and the write pass each read the whole candidate once.
    assert_eq!(total_read.get(), 2 * LEN);
    let record_bytes = std::fs::read(&retained.record).unwrap();
    let marker = format!("bytes {LEN}\n\n");
    let start = record_bytes
        .windows(marker.len())
        .position(|window| window == marker.as_bytes())
        .expect("record header carries the candidate length")
        + marker.len();
    let body = &record_bytes[start..];
    assert_eq!(body.len() as u64, LEN);
    assert!(
        body.iter()
            .enumerate()
            .all(|(index, byte)| *byte == (index as u64 % 251) as u8)
    );
}

#[test]
fn live_mount_refusal_is_eperm() {
    assert_eq!(refused_mutation_errno(), Errno::EPERM);
}

#[test]
fn scratch_record_round_trips_its_creation_schema_identity() {
    use super::{
        TableSchemaIdentity, TemporaryScratchIdentity, VFS_SCRATCH_RECORD_MAGIC, VfsScratchStore,
    };
    use std::io::Write as _;
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("record");
    let identity = TemporaryScratchIdentity {
        logical_path: "table/row/field.txt".into(),
        relation_id: [7; 16],
        key: b"key".to_vec(),
        name: "field.txt".into(),
        blob_field_name: None,
    };
    let schema = TableSchemaIdentity::new([9; 32]);
    let mut file = std::fs::File::create(&path).expect("record file");
    file.write_all(VFS_SCRATCH_RECORD_MAGIC).expect("magic");
    VfsScratchStore::write_identity(&mut file, &identity).expect("identity");
    file.write_all(schema.bytes()).expect("schema identity");
    VfsScratchStore::write_optional(&mut file, None).expect("baseline");
    file.write_all(&0_u64.to_le_bytes())
        .expect("empty candidate");
    drop(file);

    let record = VfsScratchStore::read_record(&path).expect("ORNAVFS2 record");
    assert_eq!(
        record
            .schema_identity
            .as_ref()
            .map(TableSchemaIdentity::bytes),
        Some(schema.bytes())
    );
    assert_eq!(record.identity.logical_path, "table/row/field.txt");
    assert!(record.baseline.is_none());
}

#[test]
fn legacy_scratch_record_is_read_without_schema_identity() {
    use super::{TemporaryScratchIdentity, VFS_SCRATCH_RECORD_MAGIC_V1, VfsScratchStore};
    use std::io::Write as _;
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("record");
    let identity = TemporaryScratchIdentity {
        logical_path: "table/row/field.txt".into(),
        relation_id: [7; 16],
        key: b"key".to_vec(),
        name: "field.txt".into(),
        blob_field_name: None,
    };
    let mut file = std::fs::File::create(&path).expect("record file");
    file.write_all(VFS_SCRATCH_RECORD_MAGIC_V1).expect("magic");
    VfsScratchStore::write_identity(&mut file, &identity).expect("identity");
    VfsScratchStore::write_optional(&mut file, None).expect("baseline");
    file.write_all(&0_u64.to_le_bytes())
        .expect("empty candidate");
    drop(file);

    let record = VfsScratchStore::read_record(&path).expect("ORNAVFS1 record is read");
    assert!(
        record.schema_identity.is_none(),
        "a legacy record has no creation identity"
    );
    assert_eq!(record.identity.logical_path, "table/row/field.txt");
}

#[test]
fn corrupt_scratch_record_is_reported_and_left_on_disk() {
    use super::VfsScratchStore;
    let root = tempfile::tempdir().expect("temporary directory");
    let store = VfsScratchStore::open(root.path()).expect("scratch store");
    let corrupt = store.directory.join("0000.scratch");
    std::fs::write(&corrupt, b"not a record").expect("corrupt record");

    let loaded = store.load().expect("a bad record does not fail the load");
    assert!(loaded.records.is_empty());
    assert_eq!(loaded.unreadable.len(), 1);
    assert_eq!(loaded.unreadable[0].path, corrupt);
    assert!(corrupt.exists(), "an unreadable record is left on disk");
}
