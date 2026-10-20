#![cfg(target_os = "linux")]

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    process::Command,
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
use editor_mount::{
    EditorMount, mountinfo_contains, repository_with_music_row, require_fuse_or_skip,
    run_orna_unmount, spawn_editor_mount, spawn_mount_process, start_editor_mount, write_and_sync,
};

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

fn save_with_vim_in_place(path: &Path) {
    let output = Command::new("vi")
        .args([
            "-Es",
            "-u",
            "NONE",
            "-n",
            "-i",
            "NONE",
            "-c",
            "set backupcopy=yes nobackup nowritebackup noswapfile fsync",
            "-c",
            "%s/Original/Vim in-place/",
            "-c",
            "write",
            "-c",
            "quit",
        ])
        .arg(path)
        .output()
        .expect("start the real Vim editor");
    assert!(
        output.status.success(),
        "Vim in-place save failed ({}): stdout={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn save_with_emacs_temp_rename(path: &Path) {
    let output = Command::new("emacs")
        .args([
            "--batch",
            "-Q",
            "--eval",
            "(setq file-precious-flag t write-region-inhibit-fsync nil make-backup-files nil auto-save-default nil create-lockfiles nil)",
            "--visit",
        ])
        .arg(path)
        .args([
            "--eval",
            "(progn (goto-char (point-min)) (search-forward \"Vim in-place\") (replace-match \"Emacs temp rename\") (save-buffer))",
        ])
        .output()
        .expect("start the real Emacs editor");
    assert!(
        output.status.success(),
        "Emacs temp/rename save failed ({}): stdout={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[tokio::test]
async fn real_editors_save_through_the_mounted_linux_vfs() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    assert!(mountinfo_contains(fixture._mounted.mountpoint()));

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&fixture.repository);
    let engine_state = RuntimeState::open(&fixture.repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let pending_before = engine_state.pending_count().await.unwrap();
    save_with_vim_in_place(&fixture.data_path);
    assert!(
        std::fs::read_to_string(&fixture.data_path)
            .unwrap()
            .contains("Vim in-place"),
        "Vim's in-place save must be visible through the mounted view",
    );
    let vim_pending = engine_state.pending_count().await.unwrap();
    assert_eq!(
        vim_pending,
        pending_before + 1,
        "Vim's accepted save must add one durable engine mutation",
    );

    save_with_emacs_temp_rename(&fixture.data_path);
    assert!(
        std::fs::read_to_string(&fixture.data_path)
            .unwrap()
            .contains("Emacs temp rename"),
        "Emacs's temp/rename save must be visible through the mounted view",
    );
    let emacs_pending = engine_state.pending_count().await.unwrap();
    assert_eq!(
        emacs_pending,
        vim_pending + 1,
        "Emacs's accepted save must add one durable engine mutation",
    );
}

#[tokio::test]
async fn fsynced_temporary_scratch_survives_mount_restart() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let head_before_restart = fixture.repository.head().unwrap().unwrap();
    let repository = fixture.repository.clone();
    let relation_id = fixture.relation_id;
    assert!(mountinfo_contains(fixture._mounted.mountpoint()));
    let scratch_path = fixture.row_directory.join(".data.orna.persisted-scratch");
    let expected = b"durable editor scratch, not an accepted row";
    let mut scratch = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch_path)
        .expect("create temporary scratch through FUSE");
    scratch.write_all(expected).unwrap();
    scratch
        .sync_all()
        .expect("fsync persists the private scratch");
    drop(scratch);

    let EditorMount {
        _mounted: previous_mount,
        _mount_parent: mount_parent,
        _repository_directory: repository_directory,
        ..
    } = fixture;
    drop(previous_mount);
    assert_eq!(
        repository.head().unwrap().unwrap().as_str(),
        head_before_restart.as_str(),
        "fsync of temporary scratch must not advance workspace HEAD",
    );
    let context = repository.open_format_context_at_selector("HEAD").unwrap();
    let row_map = context.load_row_map(relation_id).unwrap();
    assert_eq!(
        row_map.version().row_count(),
        Some(1),
        "fsync of temporary scratch must leave the committed row present",
    );
    let restarted_mountpoint = mount_parent.path().join("restarted");
    let _restarted_mount = spawn_editor_mount(repository_directory.path(), &restarted_mountpoint);
    assert!(mountinfo_contains(&restarted_mountpoint));
    let restarted_row = restarted_mountpoint.join("Music/london");
    assert_eq!(
        std::fs::read(restarted_row.join(".data.orna.persisted-scratch")).unwrap(),
        expected,
        "synced scratch survives the mount process restart",
    );
    assert!(
        std::fs::read_to_string(restarted_row.join("data.orna"))
            .unwrap()
            .contains("Original"),
        "fsync of temporary scratch must not accept the row",
    );
}

/// Must match the scratch directory name used by the mount.
const SCRATCH_DIRECTORY: &str = "vfs-editor-scratch-v1";

#[tokio::test]
async fn unlinked_fsynced_temporary_scratch_is_gone_after_mount_restart() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let scratch_path = fixture.row_directory.join(".data.orna.unlinked-scratch");
    let mut scratch = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch_path)
        .expect("create temporary scratch through FUSE");
    scratch.write_all(b"unlinked editor scratch").unwrap();
    scratch
        .sync_all()
        .expect("fsync persists the private scratch");
    drop(scratch);
    std::fs::remove_file(&scratch_path).expect("unlink persisted scratch");

    let EditorMount {
        _mounted: previous_mount,
        _mount_parent: mount_parent,
        _repository_directory: repository_directory,
        ..
    } = fixture;
    drop(previous_mount);
    let restarted_mountpoint = mount_parent.path().join("restarted");
    let _restarted_mount = spawn_editor_mount(repository_directory.path(), &restarted_mountpoint);
    assert!(
        !restarted_mountpoint
            .join("Music/london/.data.orna.unlinked-scratch")
            .exists(),
        "an unlinked temporary scratch must not reappear after mount restart",
    );
    run_orna_unmount(repository_directory.path(), &restarted_mountpoint);
}

#[tokio::test]
async fn stale_persisted_temporary_scratch_is_skipped_at_mount_start() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let scratch_path = fixture.row_directory.join(".data.orna.stale-scratch");
    let mut scratch = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch_path)
        .expect("create temporary scratch through FUSE");
    scratch.write_all(b"stale editor scratch").unwrap();
    scratch
        .sync_all()
        .expect("fsync persists the private scratch");
    drop(scratch);
    let source_store = fixture
        .repository
        .runtime_paths()
        .root()
        .join(SCRATCH_DIRECTORY);

    let EditorMount {
        _mounted: previous_mount,
        _mount_parent: mount_parent,
        _repository_directory: source_directory,
        ..
    } = fixture;
    drop(previous_mount);

    // The target repository has no `london` row, so the copied record cannot resolve.
    let initial = "{ title: \"Other\", artist: \"Kieran\", }\n";
    let (target_directory, target_repository, _) =
        repository_with_music_row("paris", 0x93, initial, false).await;
    let target_store = target_repository
        .runtime_paths()
        .root()
        .join(SCRATCH_DIRECTORY);
    std::fs::create_dir_all(&target_store).unwrap();
    for entry in std::fs::read_dir(&source_store).expect("read persisted scratch records") {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), target_store.join(entry.file_name())).unwrap();
    }
    drop(source_directory);

    let mountpoint = mount_parent.path().join("stale");
    let mounted = spawn_mount_process(target_directory.path(), &mountpoint);
    let mut stderr = mounted.stderr_text();
    for _ in 0..500 {
        if stderr.contains("skipped unresolved persisted temporary scratch") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        stderr = mounted.stderr_text();
    }
    assert!(
        stderr.contains("skipped unresolved persisted temporary scratch")
            && stderr.contains("persisted temporary scratch target"),
        "mount must report the skipped stale scratch; stderr={stderr:?}",
    );
    assert!(
        std::fs::read_to_string(mountpoint.join("Music/paris/data.orna"))
            .unwrap()
            .contains("Other"),
        "the mount must still serve the remaining rows",
    );
    assert!(
        !mountpoint
            .join("Music/paris/.data.orna.stale-scratch")
            .exists(),
        "an unresolved temporary scratch must not be restored",
    );
    run_orna_unmount(target_directory.path(), &mountpoint);
}
