#![cfg(target_os = "linux")]

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
};

use orna_runtime_v1::RuntimeState;

#[allow(dead_code)]
#[path = "support/format3.rs"]
mod format3;

#[allow(dead_code)]
#[path = "support/media_commit.rs"]
mod media_commit;

#[path = "support/editor_mount.rs"]
mod editor_mount;
use editor_mount::{require_fuse_or_skip, start_editor_mount, write_and_sync};

#[tokio::test]
async fn editor_saves_use_the_validated_vfs_row_path() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let repository = &fixture.repository;
    let relation_id = fixture.relation_id;
    let row_directory = &fixture.row_directory;
    let data_path = &fixture.data_path;
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Original"));
    let mut private_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&data_path)
        .unwrap();
    let mut observer = File::open(&data_path).unwrap();
    private_writer
        .write_all(b"{ title: \"Private draft\", artist: \"Kieran\", }\n")
        .unwrap();
    private_writer.seek(SeekFrom::Start(0)).unwrap();
    let mut writer_view = String::new();
    private_writer.read_to_string(&mut writer_view).unwrap();
    assert!(writer_view.contains("Private draft"));
    let mut observer_view = String::new();
    observer.read_to_string(&mut observer_view).unwrap();
    assert!(observer_view.contains("Original"));
    drop(private_writer);
    drop(observer);
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Original"));
    write_and_sync(
        &data_path,
        "{ title: \"In place\", artist: \"Kieran\", }\n",
    )
    .expect("in-place editor save must validate and sync");
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("In place"));


    let arbitrary = row_directory.join("unrelated");
    assert!(
        File::create(&arbitrary).is_err(),
        "an arbitrary sibling create must not become a row-save candidate",
    );

    let stale_temporary = row_directory.join(".data.orna.stale-tmp");
    let mut stale_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stale_temporary)
        .expect("editor creates a replacement scratch");
    stale_file
        .write_all(b"{ title: \"Stale candidate\", artist: \"Kieran\", }\n")
        .unwrap();
    stale_file.sync_all().unwrap();
    drop(stale_file);
    write_and_sync(
        &data_path,
        "{ title: \"New baseline\", artist: \"Kieran\", }\n",
    )
    .expect("later row save advances the baseline");
    assert!(
        std::fs::rename(&stale_temporary, &data_path).is_err(),
        "rename must reject a scratch whose captured row baseline is stale",
    );
    assert!(
        std::fs::read_to_string(&data_path).unwrap().contains("New baseline"),
        "stale rename must preserve the newer accepted row",
    );
    let temporary = row_directory.join(".data.orna.editor-tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .expect("editor creates temporary sibling");
    file.write_all(b"{ title: \"Renamed\", artist: \"Kieran\", }\n")
        .unwrap();
    file.sync_all().expect("editor syncs temporary file");
    drop(file);
    std::fs::rename(&temporary, &data_path).expect("editor atomically replaces data.orna");
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Renamed"));

    let durable_before_rejection = format3::committed_store_rows(&repository, relation_id);
    let mut invalid = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&data_path)
        .expect("open row document for invalid edit");
    invalid.set_len(0).unwrap();
    invalid
        .write_all(b"{ title: 123, artist: \"Kieran\", }\n")
        .unwrap();
    assert!(invalid.sync_all().is_err(), "invalid row must be rejected at sync");
    invalid.seek(SeekFrom::Start(0)).unwrap();
    let mut rejected_draft = String::new();
    invalid.read_to_string(&mut rejected_draft).unwrap();
    assert!(rejected_draft.contains("title: 123"));
    drop(invalid);
    assert!(std::fs::read_to_string(&data_path).unwrap().contains("Renamed"));
    let durable_after_rejection = format3::committed_store_rows(&repository, relation_id);
    assert_eq!(
        durable_after_rejection, durable_before_rejection,
        "rejected editor draft must leave the durable row unchanged",
    );
}

/// ORNA-VFS-006: a writer without the selected worktree lease cannot submit an edit.
#[tokio::test]
async fn editor_saves_reject_a_foreign_writer_lease() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(true).await;
    let durable_before = format3::committed_store_rows(&fixture.repository, fixture.relation_id);

    let error = write_and_sync(
        &fixture.data_path,
        "{ title: \"Rejected\", artist: \"Kieran\", }\n",
    )
    .expect_err("a foreign writer lease must reject an editor save");
    assert_eq!(error.raw_os_error(), Some(5));
    assert!(
        std::fs::read_to_string(&fixture.data_path)
            .unwrap()
            .contains("Original")
    );
    let durable_after = format3::committed_store_rows(&fixture.repository, fixture.relation_id);
    assert_eq!(
        durable_after,
        durable_before,
        "rejected save must preserve the durable row",
    );

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&fixture.repository);
    let state = RuntimeState::open(&fixture.repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    assert!(matches!(
        state.acquire_lease(runtime_identity.repository_id).await,
        Err(orna_runtime_v1::RuntimeError::LeaseHeld),
    ));
}
