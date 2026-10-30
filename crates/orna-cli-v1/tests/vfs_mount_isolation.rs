#![cfg(target_os = "linux")]

//! Gate E isolation for the writable VFS: private drafts stay invisible to
//! other opens and remounts, read handles pin their generation, kernel IO
//! modes do not leak drafts, and temp-and-rename saves are compared against
//! their captured baseline (including a baseline moved by `orna edit`). REPL
//! writes while the mount is live are refused: a live mount holds the writer
//! lease, so an owner REPL write is refused. The other concurrency baseline cases
//! are open under ornadb-5il1.368; the stat freshness gap is ornadb-5il1.468.
//!
//! Spec (Orna-1.1.0, source/41-vfs.md): EDIT-1 ("A writer sees its own draft;
//! other opens see accepted state"; "Read handles ... pin coherent
//! generations"; step 5 close-time flush by the writer), the Temporary-save
//! replacement paragraph ("This does not prove when the editor originally read
//! the row"), ORNA-VFS-009, ORNA-VFS-011, ORNA-VFS-022, ORNA-VFS-024, and
//! ACCEPTANCE Gate E.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    time::Duration,
};

use tempfile::TempDir;

#[allow(dead_code)]
#[path = "support/format3.rs"]
mod format3;

#[allow(dead_code)]
#[path = "support/media_commit.rs"]
mod media_commit;

#[path = "support/editor_mount.rs"]
mod editor_mount;
use editor_mount::{
    EditorMount, assert_errno, await_draft_guard, fresh_view, open_rw, require_fuse_or_skip,
    start_editor_mount, start_mount_process, wait_for_mount,
};

fn music_row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: \"Kieran\", }}\n")
}

fn signal(process: u32, name: &str) {
    let status = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(process.to_string())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{name} {process} failed");
}

/// Reads a handle from its start, so each call observes the handle's own
/// generation rather than the position left by the previous read.
fn read_from_start(file: &mut File) -> String {
    file.seek(SeekFrom::Start(0)).expect("seek to start");
    let mut text = String::new();
    file.read_to_string(&mut text).expect("read handle");
    text
}

/// Creates a temporary sibling of `data.orna` the way an editor's temp-and-rename
/// save does: create, fill and fsync the scratch, then close it. Creation
/// captures the row's current baseline (Temporary-save replacement).
fn write_temporary_sibling(fixture: &EditorMount, name: &str, document: &str) -> PathBuf {
    let path = fixture.row_directory.join(name);
    let mut scratch = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("create temporary sibling");
    scratch
        .write_all(document.as_bytes())
        .expect("write temporary sibling");
    scratch.sync_all().expect("fsync temporary sibling");
    path
}

/// Commits `title` through the owner-routed `orna edit` while the mount is live,
/// the same path `replace_row_through_owner` uses in `vfs_mount_integration`.
/// The commit advances the row past any baseline captured before it.
fn owner_routed_edit(fixture: &EditorMount, title: &str) {
    let scripts = TempDir::new().unwrap();
    let editor = scripts.path().join("owner-routed-edit");
    std::fs::write(
        &editor,
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}' > \"$1\"\n",
            music_row(title).trim_end()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--db")
        .arg(fixture._repository_directory.path())
        .args(["edit", "Music/london/data.orna"])
        .env("EDITOR", &editor)
        .output()
        .expect("run orna edit through the mount owner");
    assert!(
        output.status.success(),
        "owner-routed orna edit failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// EDIT-1: "A writer sees its own draft; other opens see accepted state." An
/// unsynced private draft stays invisible to a second open even after a child
/// process has been spawned while the writer's descriptor is open. exec(2)
/// closes the inherited CLOEXEC copy of that descriptor in the child, and the
/// kernel then issues a FLUSH under the child's lock owner. That FLUSH is not
/// the writer's close (EDIT-1 step 5), so it must not accept the draft.
/// SIGKILL plus a remount does not turn the draft into accepted state
/// (ORNA-VFS-024).
#[tokio::test]
async fn unsynced_draft_is_invisible_to_second_open_and_after_child_exec_and_sigkill_remount() {
    if !require_fuse_or_skip() {
        return;
    }
    let mut fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mountpoint = fixture._mount_parent.path().join("library");

    let mut draft = open_rw(&fixture);
    draft.set_len(0).unwrap();
    draft
        .write_all(music_row("Unsynced draft").as_bytes())
        .unwrap();
    assert!(
        read_from_start(&mut draft).contains("Unsynced draft"),
        "the writer sees its own draft"
    );

    // Spawn a child while the draft descriptor is open: its exec closes the
    // inherited copy, which is the FLUSH path this test pins down.
    let child = Command::new("true")
        .output()
        .expect("spawn a child process");
    assert!(child.status.success(), "the child process exits 0");

    let mut second = File::open(&fixture.data_path).unwrap();
    assert_eq!(
        read_from_start(&mut second),
        original,
        "a second handle sees the accepted row, never the unsynced draft"
    );
    assert_eq!(
        fresh_view(&fixture),
        original,
        "a new open sees the accepted row, never the unsynced draft"
    );

    // Kill with the writer's descriptor still open. Once the daemon is gone the
    // connection is aborted, so closing the descriptor afterwards cannot flush
    // the draft into acceptance.
    signal(fixture._mounted.pid(), "KILL");
    fixture
        ._mounted
        .wait_for_exit(Duration::from_secs(10))
        .expect("SIGKILL stops the mount");
    fixture._mounted.release_mountpoint();
    drop(draft);
    drop(second);

    fixture._mounted = wait_for_mount(start_mount_process(
        fixture._repository_directory.path(),
        &mountpoint,
    ));
    fixture._mounted.await_path(&fixture.data_path);
    assert_eq!(
        fresh_view(&fixture),
        original,
        "a remount after SIGKILL serves only the accepted row"
    );
}

/// EDIT-1 "read handles ... pin coherent generations"; ACCEPTANCE Gate E: "Open
/// reads remain on old content after replacement." A handle opened before a
/// temp rename-over keeps its captured bytes and size, while a new open sees the
/// replacement.
#[tokio::test]
async fn read_handle_opened_before_temp_rename_keeps_old_bytes() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mut old_handle = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut old_handle), original);

    let temp = write_temporary_sibling(
        &fixture,
        ".data.orna.replace-temp",
        &music_row("Temp replacement"),
    );
    std::fs::rename(&temp, &fixture.data_path).expect("rename admits the temp replacement");

    assert!(
        fresh_view(&fixture).contains("Temp replacement"),
        "a new open sees the admitted replacement"
    );
    assert_eq!(
        read_from_start(&mut old_handle),
        original,
        "a handle opened before the replacement reads its captured bytes"
    );
    assert_eq!(
        old_handle.metadata().unwrap().len(),
        original.len() as u64,
        "fstat on the handle reports the captured generation's size"
    );
    drop(old_handle);
}

/// F2 finding (ornadb-5il1.466): a read handle opened before an owner-routed
/// `orna edit`, then a temp created after that edit. 41-vfs.md:67-68: the temp's
/// creation captures the row's current baseline, so its rename must succeed. The
/// read handle opened before the owner edit keeps its generation (EDIT-1).
#[tokio::test]
async fn read_handle_across_owner_edit_temp_created_after_it_renames_f2() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mut pinned = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut pinned), original);

    owner_routed_edit(&fixture, "Owner change");
    let temp = write_temporary_sibling(
        &fixture,
        ".data.orna.after-owner",
        &music_row("After owner"),
    );
    std::fs::rename(&temp, &fixture.data_path)
        .expect("a temp created after the owner edit renames over the row");
    let view = fresh_view(&fixture);
    assert!(
        view.contains("After owner") && !view.contains("Owner change"),
        "the rename applies the post-edit temp, got {view}"
    );
    assert_eq!(
        read_from_start(&mut pinned),
        original,
        "the read handle opened before the owner edit keeps its generation"
    );
    drop(pinned);
}

/// ORNA-VFS-011 CAS against the captured temp-create baseline; ORNA-VFS-009
/// (stale candidate preserves the accepted row and retains the draft bytes).
/// A temp created before another writer's commit captures the older baseline,
/// so its rename is refused with EAGAIN. The owner's row stays, and the scratch
/// keeps its bytes and holds the draft guard until it is removed.
#[tokio::test]
async fn temp_created_before_owner_change_is_refused_with_eagain_and_keeps_scratch() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let temp = write_temporary_sibling(&fixture, ".data.orna.stale-temp", &music_row("Stale temp"));
    owner_routed_edit(&fixture, "Owner change");

    assert_errno(
        std::fs::rename(&temp, &fixture.data_path),
        &[libc::EAGAIN],
        "a temp created before the owner change must be refused",
    );
    let view = fresh_view(&fixture);
    assert!(
        view.contains("Owner change") && !view.contains("Stale temp"),
        "the refused temp leaves the owner's row in place, got {view}"
    );
    assert_eq!(
        std::fs::read_to_string(&temp).expect("the refused scratch is kept"),
        music_row("Stale temp"),
    );
    await_draft_guard(
        &fixture,
        b'1',
        "the refused scratch is an unresolved draft",
    )
    .await;

    std::fs::remove_file(&temp).expect("remove the refused scratch");
    await_draft_guard(
        &fixture,
        b'0',
        "removing the refused scratch clears the guard",
    )
    .await;
}

/// Two temporaries for one row each capture the baseline current at their own
/// creation: one created before an owner commit is refused with EAGAIN, and one
/// created after that commit renames over the owner's row. The refused scratch
/// does not block the later temporary.
#[tokio::test]
async fn two_temporaries_for_one_row_capture_their_own_baselines() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let stale = write_temporary_sibling(
        &fixture,
        ".data.orna.stale-temp",
        &music_row("Stale temp"),
    );
    owner_routed_edit(&fixture, "Owner change");
    assert_errno(
        std::fs::rename(&stale, &fixture.data_path),
        &[libc::EAGAIN],
        "a temp created before the owner change must be refused",
    );

    let fresh = write_temporary_sibling(
        &fixture,
        ".data.orna.fresh-temp",
        &music_row("Fresh temp"),
    );
    std::fs::rename(&fresh, &fixture.data_path)
        .expect("a temp created after the owner change renames over the owner's row");
    let view = fresh_view(&fixture);
    assert!(
        view.contains("Fresh temp") && !view.contains("Owner change"),
        "the later temp replaces the owner's row, got {view}"
    );
    std::fs::remove_file(&stale).expect("remove the refused scratch");
}

/// ORNA-VFS-022: "supported caching/IO modes MUST preserve per-open generation
/// semantics despite non-VFS database writers"; ACCEPTANCE Gate E: "Kernel
/// caching must not leak private drafts into another handle/generation."
/// Repeated observer opens and a read handle opened before the write never show
/// the private draft. After acceptance, the earlier handle keeps its generation
/// and a new open sees the accepted bytes.
#[tokio::test]
async fn repeated_observer_reads_never_show_private_draft_and_read_handles_pin_generation() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mut pinned = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut pinned), original);

    let mut writer = open_rw(&fixture);
    writer.set_len(0).unwrap();
    writer
        .write_all(music_row("Observer draft").as_bytes())
        .unwrap();
    assert!(
        read_from_start(&mut writer).contains("Observer draft"),
        "the writer sees its own draft"
    );

    for _ in 0..5 {
        assert_eq!(
            fresh_view(&fixture),
            original,
            "a repeated observer open never reads the private draft"
        );
        assert_eq!(
            read_from_start(&mut pinned),
            original,
            "the read handle keeps its generation while the draft is private"
        );
    }

    writer.sync_all().expect("fsync accepts the private draft");
    assert!(
        fresh_view(&fixture).contains("Observer draft"),
        "a new open sees the accepted draft"
    );
    assert_eq!(
        read_from_start(&mut pinned),
        original,
        "a read handle opened before acceptance keeps its captured generation"
    );
    drop(writer);
    drop(pinned);
}

/// Writes a `#!/bin/sh` editor that replaces its draft argument with `body`.
fn write_editor_script(path: &std::path::Path, body: &str) {
    std::fs::write(
        path,
        format!("#!/bin/sh\nprintf '%s\\n' '{}' > \"$1\"\n", body.trim_end()),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// ORNA-VFS-011 and 41-vfs.md:75-77: `orna edit PATH` captures a read-to-save
/// baseline and runs `$EDITOR` against an explicit draft ("providing stronger
/// CAS protection"). Another writer's commit that lands while that editor
/// session is open advances the row past the read baseline, so the save is
/// refused (E2000, the stale-edit cause), the draft is retained, and the other
/// writer's row stays in place.
/// The `E2000` assertion relies on `ESTALE` on the wire: `EAGAIN` surfaces as EOPNOTSUPP on
/// this kernel (ornadb-eufo.1.1; tests/vfs_setxattr_errno_kernel_limit.rs).
#[tokio::test]
async fn orna_edit_refuses_save_when_row_changed_during_editor_session() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let scripts = TempDir::new().unwrap();
    let other_editor = scripts.path().join("other-writer-editor");
    write_editor_script(&other_editor, &music_row("Other writer"));
    let outer_editor = scripts.path().join("outer-editor");
    std::fs::write(
        &outer_editor,
        format!(
            "#!/bin/sh\nEDITOR='{other}' '{bin}' --db '{db}' edit Music/london/data.orna || exit 1\nprintf '%s\\n' '{mine}' > \"$1\"\n",
            other = other_editor.display(),
            bin = env!("CARGO_BIN_EXE_orna"),
            db = fixture._repository_directory.path().display(),
            mine = music_row("Mine").trim_end(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&outer_editor, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--debug")
        .arg("--db")
        .arg(fixture._repository_directory.path())
        .args(["edit", "Music/london/data.orna"])
        .env("EDITOR", &outer_editor)
        .output()
        .expect("run orna edit with a concurrent writer");
    let diagnostics = format!(
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(!output.status.success(), "stale orna edit must fail: {diagnostics}");
    assert!(
        diagnostics.contains("E2000"),
        "the refusal is the stale-edit cause E2000: {diagnostics}"
    );
    let view = fresh_view(&fixture);
    assert!(
        view.contains("Other writer") && !view.contains("Mine"),
        "the concurrent writer's row stays, got {view}"
    );
    let retained = diagnostics
        .split("draft retained at ")
        .nth(1)
        .and_then(|rest| rest.split(['\n', ',', '"']).next())
        .map(|path| PathBuf::from(path.trim()))
        .unwrap_or_else(|| panic!("the E2000 refusal names the retained draft path: {diagnostics}"));
    let draft = std::fs::read_to_string(&retained)
        .unwrap_or_else(|error| panic!("retained draft at {}: {error}", retained.display()));
    assert!(
        draft.contains("Mine"),
        "the refused edit's draft is retained with its content, got {draft}"
    );
}

/// ORNA-VFS-022 and 41-vfs.md:39-40 (EDIT-1 "other opens see accepted state"):
/// a row written through the owner while the mount is live, with the kernel
/// caching disabled by direct_io on editable opens, is visible to every new open.
/// A read handle opened before that write keeps its generation, which is the
/// pinned-generation rule, not a stale cache. The fresh `stat` size after this
/// write is asserted by `stat_after_owner_write_reports_fresh_size`.
#[tokio::test]
async fn owner_write_is_visible_through_mount_to_every_new_open() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mut pinned = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut pinned), original);

    owner_routed_edit(&fixture, "Api write");
    let accepted = fresh_view(&fixture);
    assert!(accepted.contains("Api write"), "a new open sees the owner write, got {accepted}");
    for _ in 0..3 {
        let mut reopened = File::open(&fixture.data_path).unwrap();
        assert_eq!(read_from_start(&mut reopened), accepted);
    }
    assert_eq!(
        read_from_start(&mut pinned),
        original,
        "the handle opened before the owner write keeps its generation"
    );
    drop(pinned);
}

/// ORNA-VFS-019 and profiles/vfs-1.md:133 ("stat reports descriptor size without
/// media access"): after an owner-routed API write, `stat` on the document reports
/// the accepted row's size. The path stat reads the live accepted image; open
/// handles keep their captured generation (see the tests below).
#[tokio::test]
async fn stat_after_owner_write_reports_fresh_size() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    owner_routed_edit(&fixture, "Api write with a longer title");
    let accepted = fresh_view(&fixture);
    let size = std::fs::metadata(&fixture.data_path).unwrap().len();
    assert_eq!(size, accepted.len() as u64, "stat reports the accepted row size");
}

/// ORNA-VFS-019 and 41-vfs.md:67-68: a writer opened before an owner-routed write
/// captured the baseline current at its open. Its save is refused with EAGAIN and
/// the owner's row stays. A path stat seen by other opens is asserted by
/// `other_open_stat_after_refused_writer_reports_accepted_size` (ornadb-5il1.470).
#[tokio::test]
async fn writer_opened_before_owner_write_is_refused_with_eagain() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut writer = open_rw(&fixture);
    writer.set_len(0).unwrap();
    writer
        .write_all(music_row("Stale writer").as_bytes())
        .unwrap();
    owner_routed_edit(&fixture, "Owner change");

    assert_errno(
        writer.sync_all(),
        &[libc::EAGAIN],
        "a writer opened before the owner change must be refused",
    );
    let view = fresh_view(&fixture);
    assert!(
        view.contains("Owner change") && !view.contains("Stale writer"),
        "the refused writer leaves the owner's row in place, got {view}"
    );
    drop(writer);
}

/// ORNA-VFS-019 and 41-vfs.md:38-39: a writer's stat reports its own draft size
/// while the draft is open, before any owner write.
#[tokio::test]
async fn writer_stat_reports_its_own_draft_size() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut writer = open_rw(&fixture);
    writer.set_len(0).unwrap();
    writer.write_all(music_row("Stale writer").as_bytes()).unwrap();
    assert_eq!(
        std::fs::metadata(&fixture.data_path).unwrap().len(),
        music_row("Stale writer").len() as u64,
        "the writer's stat reports its own draft size"
    );
    drop(writer);
}

/// ORNA-VFS-019 and 41-vfs.md:38-39: after a refused writer, other opens see the
/// accepted state. A path stat by another open reports the accepted size, not the
/// retained draft.
#[tokio::test]
async fn other_open_stat_after_refused_writer_reports_accepted_size() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut writer = open_rw(&fixture);
    writer.set_len(0).unwrap();
    writer.write_all(music_row("Stale writer").as_bytes()).unwrap();
    owner_routed_edit(&fixture, "Owner change");
    assert_errno(
        writer.sync_all(),
        &[libc::EAGAIN],
        "a writer opened before the owner change must be refused",
    );
    drop(writer);
    let accepted = fresh_view(&fixture);
    // The refused draft can still be reported for a moment after the writer is dropped.
    // Poll for a bounded time: a lag clears, while a stale accepted size still fails.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::fs::metadata(&fixture.data_path).unwrap().len() != accepted.len() as u64
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::metadata(&fixture.data_path).unwrap().len(),
        accepted.len() as u64,
        "a stat by another open reports the accepted size"
    );
}

/// ORNA-VFS-022, 41-vfs.md:39-40 and profiles/vfs-1.md:128: a clean handle opened
/// before an owner-routed write keeps its generation, so its fstat reports the
/// size of the generation it pinned. FUSE passes no file handle to fstat, so the
/// nodeid alone selects the generation the server reports.
#[tokio::test]
async fn reader_opened_before_owner_write_keeps_fstat_size() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let reader = File::open(&fixture.data_path).unwrap();
    owner_routed_edit(&fixture, "Api write with a longer title");
    assert_ne!(fresh_view(&fixture).len(), original.len(), "the owner write changes the size");
    assert_eq!(
        reader.metadata().unwrap().len(),
        original.len() as u64,
        "fstat on a handle opened before the owner write reports its pinned size"
    );
    drop(reader);
}

/// ornadb-5il1.470: a temp rename over `data.orna` after an owner-routed write. The
/// handle opened before the owner write keeps the bytes and size it pinned.
#[tokio::test]
async fn reader_opened_before_owner_write_survives_temp_rename_over_row() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mut reader = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut reader), original);
    owner_routed_edit(&fixture, "Api write with a longer title");
    let temp = write_temporary_sibling(
        &fixture,
        ".data.orna.replace-temp",
        &music_row("Temp replacement"),
    );
    std::fs::rename(&temp, &fixture.data_path).expect("rename admits the temp replacement");
    assert!(
        fresh_view(&fixture).contains("Temp replacement"),
        "a new open sees the admitted replacement"
    );
    assert_eq!(
        read_from_start(&mut reader),
        original,
        "a handle opened before the owner write reads its pinned bytes after the rename"
    );
    assert_eq!(
        reader.metadata().unwrap().len(),
        original.len() as u64,
        "fstat on that handle reports the pinned size after the rename"
    );
    drop(reader);
}

/// ornadb-5il1.470.3 and 41-vfs.md:38-40: a handle opened on the live `data.orna`, with no
/// read or owner write before the rename, keeps the bytes and size it pinned when a temp file
/// renames over the row. A new open shows the admitted replacement (profiles/vfs-1.md:126-133).
/// No read happens before the rename, so the kernel page cache cannot serve the old bytes. The
/// sleep lets the attributes cached at open expire (fs/fuse/dir.c compares i_time with jiffies
/// strictly), so `fstat` reaches the server, which must report the pinned node.
#[tokio::test]
async fn reader_opened_on_live_row_survives_temp_rename_over_row() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mut reader = File::open(&fixture.data_path).unwrap();
    let temp = write_temporary_sibling(
        &fixture,
        ".data.orna.replace-temp",
        &music_row("Temp replacement, substantially longer than the original row"),
    );
    std::fs::rename(&temp, &fixture.data_path).expect("rename admits the temp replacement");
    assert!(
        fresh_view(&fixture).contains("Temp replacement"),
        "a new open sees the admitted replacement"
    );
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        reader.metadata().unwrap().len(),
        original.len() as u64,
        "fstat on a handle pinned to the live row reports its pinned size after the rename"
    );
    assert_eq!(
        read_from_start(&mut reader),
        original,
        "a handle pinned to the live row reads its pinned bytes after the rename"
    );
    drop(reader);
}

/// ornadb-5il1.470.4: a path `stat` taken right after each owner-routed write reports the
/// accepted size, even though a `stat` just before the write cached the dentry and attributes.
/// ENTRY_TTL is zero, so the kernel revalidates on use (fs/fuse/dir.c: fuse_time_to_jiffies
/// returns 0 for a zero timeout and fuse_dentry_revalidate compares that against jiffies).
/// The row grows every round, so a stale size would differ from the accepted size.
#[tokio::test]
async fn path_stat_after_owner_write_reports_accepted_size_in_loop() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    for round in 0..24 {
        std::fs::metadata(&fixture.data_path).unwrap();
        let title = "y".repeat(round + 1);
        owner_routed_edit(&fixture, &title);
        let stat = std::fs::metadata(&fixture.data_path).unwrap().len();
        let accepted = fresh_view(&fixture);
        assert!(
            accepted.contains(&title),
            "round {round}: the owner write is accepted, got {accepted:?}"
        );
        assert_eq!(
            stat,
            accepted.len() as u64,
            "round {round}: a path stat right after the owner write reports the accepted size"
        );
    }
}

/// ORNA-VFS-019 and 41-vfs.md:39-40: a handle opened before an owner write that
/// shrinks the row keeps its captured bytes. One owner write from the original row.
#[tokio::test]
async fn reader_opened_before_shrinking_owner_write_keeps_old_bytes() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let pinned = fresh_view(&fixture);
    let mut reader = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut reader), pinned);
    owner_routed_edit(&fixture, "x");
    let accepted = fresh_view(&fixture);
    assert!(accepted.len() < pinned.len(), "the owner write shrinks the row, got {accepted}");
    assert_eq!(
        read_from_start(&mut reader),
        pinned,
        "a handle opened before the shrinking write keeps its captured bytes"
    );
    drop(reader);
}

/// ORNA-VFS-011 and 41-vfs.md:75-77: an `orna edit` that starts after an owner-routed
/// write captures its draft and read-to-save baseline from the accepted row, not from
/// a stale in-process copy of the row. The editor sees the owner's row and its save
/// succeeds. A stale baseline would make this save fail (E2000) or overwrite the draft.
#[tokio::test]
async fn orna_edit_started_after_owner_write_starts_from_fresh_row() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    owner_routed_edit(&fixture, "Owner change");
    let scripts = TempDir::new().unwrap();
    let editor = scripts.path().join("fresh-editor");
    std::fs::write(
        &editor,
        format!(
            "#!/bin/sh\ngrep -q 'Owner change' \"$1\" || exit 3\nprintf '%s\\n' '{}' > \"$1\"\n",
            music_row("Mine").trim_end()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--db")
        .arg(fixture._repository_directory.path())
        .args(["edit", "Music/london/data.orna"])
        .env("EDITOR", &editor)
        .output()
        .expect("run orna edit after an owner write");
    let diagnostics = format!(
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        output.status.success(),
        "an edit started after the owner write saves from the fresh row: {diagnostics}"
    );
    let view = fresh_view(&fixture);
    assert!(
        view.contains("Mine") && !view.contains("Owner change"),
        "the edit's save replaces the fresh row, got {view}"
    );
}

/// ORNA-VFS-019 and profiles/vfs-1.md:133: a handle opened after an owner-routed
/// write reads the accepted bytes and reports the accepted size through both
/// `fstat` and path `stat`.
#[tokio::test]
async fn reader_opened_after_owner_write_sees_fresh_bytes_and_size() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    owner_routed_edit(&fixture, "Api write with a longer title");
    let accepted = fresh_view(&fixture);

    let mut reader = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut reader), accepted);
    assert_eq!(
        reader.metadata().unwrap().len(),
        accepted.len() as u64,
        "fstat on a handle opened after the owner write reports the accepted size"
    );
    assert_eq!(
        std::fs::metadata(&fixture.data_path).unwrap().len(),
        accepted.len() as u64,
        "path stat reports the accepted size"
    );
    drop(reader);
}

/// ORNA-VFS-019 (open handles do not retarget): a handle opened before an
/// owner-routed write keeps the bytes of its generation, while a path stat
/// reports the accepted size. fstat on that handle is not asserted here; see
/// `reader_opened_before_owner_write_keeps_fstat_size` (ornadb-5il1.470).
#[tokio::test]
async fn reader_opened_before_owner_write_keeps_old_bytes() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mut old_reader = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut old_reader), original);

    owner_routed_edit(&fixture, "Api write with a longer title");
    let accepted = fresh_view(&fixture);
    assert_ne!(accepted.len(), original.len(), "the owner write changes the size");

    assert_eq!(
        read_from_start(&mut old_reader),
        original,
        "a handle opened before the owner write keeps its captured bytes"
    );
    assert_eq!(
        std::fs::metadata(&fixture.data_path).unwrap().len(),
        accepted.len() as u64,
        "path stat reports the accepted size"
    );
    drop(old_reader);
}

/// Demonstrated limitation (profiles/vfs-1.md:118-119, 41-vfs.md:65-77): "Blind
/// replacement after a late temp-create has no original-read witness." An
/// editor that read the row, then saw another writer commit, then created its
/// temporary and renamed it over the row, succeeds and silently replaces the
/// other writer's change. The same race through `orna edit` (strong read
/// baseline) is refused with E2000 (the stale-edit cause), which is the supported protection.
#[tokio::test]
async fn blind_replacement_after_late_temp_overwrites_owner_change_while_orna_edit_refuses() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let original = fresh_view(&fixture);
    let mut editor_read = File::open(&fixture.data_path).unwrap();
    assert_eq!(read_from_start(&mut editor_read), original);

    owner_routed_edit(&fixture, "Owner change");
    let temp = write_temporary_sibling(
        &fixture,
        ".data.orna.blind-temp",
        &music_row("Blind replacement"),
    );
    std::fs::rename(&temp, &fixture.data_path)
        .expect("the late temp has no read witness, so the blind rename is admitted");
    let view = fresh_view(&fixture);
    assert!(
        view.contains("Blind replacement") && !view.contains("Owner change"),
        "DEMONSTRATED LIMITATION: the blind replacement overwrote the owner change, got {view}"
    );
    assert_eq!(
        read_from_start(&mut editor_read),
        original,
        "the handle opened before the replacement keeps its captured bytes"
    );
    drop(editor_read);

    // Contrast: the same read-then-commit race through `orna edit` is refused.
    let scripts = TempDir::new().unwrap();
    let other_editor = scripts.path().join("other-writer-editor");
    write_editor_script(&other_editor, &music_row("Second owner change"));
    let outer_editor = scripts.path().join("outer-editor");
    std::fs::write(
        &outer_editor,
        format!(
            "#!/bin/sh\nEDITOR='{other}' '{bin}' --db '{db}' edit Music/london/data.orna || exit 1\nprintf '%s\\n' '{mine}' > \"$1\"\n",
            other = other_editor.display(),
            bin = env!("CARGO_BIN_EXE_orna"),
            db = fixture._repository_directory.path().display(),
            mine = music_row("Strong edit").trim_end(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&outer_editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--db")
        .arg(fixture._repository_directory.path())
        .args(["edit", "Music/london/data.orna"])
        .env("EDITOR", &outer_editor)
        .output()
        .expect("run orna edit against a concurrent writer");
    let diagnostics = format!(
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !output.status.success(),
        "orna edit's strong read baseline refuses the save: {diagnostics}"
    );
    // The owner refuses the explicit edit with ESTALE (the xattr commit's Stale arm), and the CLI
    // reports that reply as the stale detail with E2000 (cli_mount.rs:436-440). [INFERENCE] from
    // reading fuse_mount.rs:3893: that refusal returns before the mount records a draft refusal,
    // so the spec cause sys.vfs.stale_edit (fuse_mount.rs:1587, Orna-1.1.0.md:8674) is not in
    // the mount's unresolved-draft fields. This test does not assert that absence.
    // E2000 relies on ESTALE on the wire: EAGAIN surfaces as EOPNOTSUPP on this kernel
    // (ornadb-eufo.1.1; tests/vfs_setxattr_errno_kernel_limit.rs).
    assert!(
        diagnostics.contains("E2000"),
        "the refusal carries the product code E2000: {diagnostics}"
    );
    assert!(
        diagnostics.contains("Edit is stale against the mounted baseline"),
        "the refusal names the stale baseline: {diagnostics}"
    );
    assert!(
        diagnostics.contains("reapply the retained draft"),
        "the refusal points at the retained draft: {diagnostics}"
    );
    let view = fresh_view(&fixture);
    assert!(
        view.contains("Second owner change") && !view.contains("Strong edit"),
        "the refused orna edit leaves the other writer's row and does not admit Strong edit, got {view}"
    );
}

/// A REPL write while a live editor mount holds the writer lease is refused, not
/// applied through a second writable handle (ORNA-TURSO-005: other local writers
/// coordinate with the owner or fail). The refusal carries ORNA-REPL-EFFECT together
/// with its cause (the repository's runtime owner lock is held by another live process,
/// so this REPL cannot take the writer lease) and the remedy (stop the mount for REPL
/// database writes; file writes while mounted go through the owner-routed `orna edit`,
/// 41-vfs.md:76; profiles/vfs-1.md:119). The same command succeeds once only the mount
/// is released (control below).
#[tokio::test]
async fn repl_write_is_refused_while_live_mount_holds_writer_lease() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let before = fresh_view(&fixture);
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--db")
        .arg(fixture._repository_directory.path())
        .args([
            "repl",
            "main.Music.insert({ id: \"paris\", title: \"Repl\", artist: \"Kieran\" })",
        ])
        .output()
        .expect("run orna repl while mounted");
    let diagnostics = format!(
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !output.status.success(),
        "a REPL write under a live writer lease must fail: {diagnostics}"
    );
    assert!(
        diagnostics.contains("ORNA-REPL-EFFECT"),
        "the refusal is ORNA-REPL-EFFECT: {diagnostics}"
    );
    assert!(
        diagnostics.contains("the runtime owner lock is held by another live process")
            && diagnostics.contains("writer lease"),
        "the refusal names the writer-lease cause for this path: {diagnostics}"
    );
    assert!(
        !diagnostics.contains("another writer owns the repository writer lease"),
        "this REPL does not hold the runtime owner lock, so the other lease-cause text must not appear: {diagnostics}"
    );
    assert!(
        diagnostics.contains("REPL database writes need the mount stopped")
            && diagnostics.contains("orna unmount")
            && diagnostics.contains("orna edit"),
        "the refusal names the remedy: stop the mount for REPL writes, file writes go through orna edit: {diagnostics}"
    );
    assert_eq!(
        fresh_view(&fixture),
        before,
        "the refused REPL write leaves the mounted row unchanged"
    );

    // Control: dropping only the mount releases the writer lease and keeps the
    // repository directory, so the same command must now succeed.
    drop(fixture._mounted);
    let released = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--db")
        .arg(fixture._repository_directory.path())
        .args([
            "repl",
            "main.Music.insert({ id: \"paris\", title: \"Repl\", artist: \"Kieran\" })",
        ])
        .output()
        .expect("run orna repl after the mount is released");
    assert!(
        released.status.success(),
        "the same REPL write succeeds once the mount releases the lease: stderr={}",
        String::from_utf8_lossy(&released.stderr),
    );
}