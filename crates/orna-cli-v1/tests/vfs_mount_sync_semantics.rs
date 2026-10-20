#![cfg(target_os = "linux")]

//! Gate E: acknowledged versus draft state on the writable mount for
//! fsync/fdatasync, repeated flush, dup/fork, temporary sync and rename,
//! writes after sync, and VFS-007/008/009 draft acknowledgement (EDIT-1).

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    time::Duration,
};

#[allow(dead_code)]
#[path = "support/format3.rs"]
mod format3;
#[allow(dead_code)]
#[path = "support/media_commit.rs"]
mod media_commit;

#[path = "support/editor_mount.rs"]
mod editor_mount;
use editor_mount::{EditorMount, require_fuse_or_skip, start_editor_mount};

const EINVAL: i32 = 22;
const EBUSY: i32 = 16;

fn row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: \"Kieran\", }}\n")
}

/// Title that the schema rejects: `artist` must be Str, so the draft fails at sync.
fn invalid_row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: 123, }}\n")
}

fn open_rw(fixture: &EditorMount) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fixture.data_path)
        .expect("open data.orna read-write")
}

fn replace_draft(file: &mut File, document: &str) {
    file.set_len(0).expect("truncate private candidate");
    file.seek(SeekFrom::Start(0)).expect("seek to start");
    file.write_all(document.as_bytes())
        .expect("write private candidate");
}

fn read_own_draft(file: &mut File) -> String {
    let mut text = String::new();
    file.seek(SeekFrom::Start(0)).expect("seek to start");
    file.read_to_string(&mut text).expect("read own draft");
    text
}

fn fresh_view(fixture: &EditorMount) -> String {
    std::fs::read_to_string(&fixture.data_path).expect("read accepted state through a new open")
}

fn assert_errno(result: std::io::Result<()>, expected: i32, context: &str) {
    let error = result.expect_err(context);
    assert_eq!(error.raw_os_error(), Some(expected), "{context}: {error}");
}

fn mount_root(fixture: &EditorMount) -> PathBuf {
    fixture._mount_parent.path().join("library")
}

/// Draft guard value the mount publishes on its root: `1` while any draft is
/// unresolved, `0` when none is.
fn draft_guard(fixture: &EditorMount) -> u8 {
    let root = mount_root(fixture);
    let mut value = [0_u8; 1];
    let length = rustix::fs::getxattr(root.as_path(), "user.orna.unresolved-drafts", &mut value)
        .expect("read the mount draft guard");
    assert_eq!(length, 1, "the draft guard is one byte");
    value[0]
}

/// Waits up to five seconds for the draft guard to reach `expected`. The FUSE
/// release handler can run after close(2) returns, so a guard read right after
/// a drop polls instead of reading once.
async fn await_draft_guard(fixture: &EditorMount, expected: u8, context: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let value = draft_guard(fixture);
        if value == expected {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("{context}: guard is {value}, expected {expected} after 5s");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Sets the mount root's unmount-quiesce attribute, as `orna-cli-v1` does
/// before unmount.
fn set_quiesce(fixture: &EditorMount, value: &[u8]) -> rustix::io::Result<()> {
    let root = mount_root(fixture);
    rustix::fs::setxattr(
        root.as_path(),
        "user.orna.unmount-quiesce",
        value,
        rustix::fs::XattrFlags::empty(),
    )
}

/// ORNA-VFS-008/EDIT-1: fsync acknowledges a valid draft; a rejected draft
/// returns EINVAL, stays visible to its own handle, and is never admitted.
#[tokio::test]
async fn fsync_acknowledges_valid_draft_and_rejects_invalid_draft() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);

    replace_draft(&mut file, &row("Fsync accepted"));
    file.sync_all().expect("valid draft fsync is acknowledged");
    assert!(fresh_view(&fixture).contains("Fsync accepted"));

    replace_draft(&mut file, &invalid_row("Bad fsync draft"));
    assert_errno(
        file.sync_all(),
        EINVAL,
        "invalid draft fsync must fail with EINVAL",
    );
    assert!(
        read_own_draft(&mut file).contains("Bad fsync draft"),
        "the rejected draft stays visible to its own handle",
    );
    assert!(!fresh_view(&fixture).contains("Bad fsync draft"));
    assert!(
        fresh_view(&fixture).contains("Fsync accepted"),
        "a new open reads accepted state, never the rejected draft",
    );
}

/// ORNA-VFS-008: fdatasync acknowledges and rejects with the same contract as fsync.
#[tokio::test]
async fn fdatasync_acknowledges_valid_draft_and_rejects_invalid_draft() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);

    replace_draft(&mut file, &row("Datasync accepted"));
    file.sync_data()
        .expect("valid draft fdatasync is acknowledged");
    assert!(fresh_view(&fixture).contains("Datasync accepted"));

    replace_draft(&mut file, &invalid_row("Bad datasync draft"));
    assert_errno(
        file.sync_data(),
        EINVAL,
        "invalid draft fdatasync must fail with EINVAL",
    );
    assert!(!fresh_view(&fixture).contains("Bad datasync draft"));
    assert!(fresh_view(&fixture).contains("Datasync accepted"));
}

/// ORNA-VFS-008: repeated fsync/fdatasync/flush of one accepted candidate
/// return the recorded outcome and never create a second acceptance. A handle
/// opened after the first acceptance must still sync afterwards: a second
/// acceptance would advance the baseline and make its write stale.
#[tokio::test]
async fn repeated_sync_and_flush_do_not_accept_the_candidate_twice() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Repeated once"));
    file.sync_all().expect("first fsync accepts the candidate");
    let after_first = fresh_view(&fixture);
    assert!(after_first.contains("Repeated once"));
    let mut later = open_rw(&fixture);

    file.sync_all()
        .expect("repeated fsync returns the recorded outcome");
    file.sync_data()
        .expect("repeated fdatasync returns the recorded outcome");
    for _ in 0..3 {
        // Dropping a dup'd descriptor issues a FUSE flush for the shared handle.
        drop(file.try_clone().expect("dup for a flush cycle"));
    }
    file.sync_all()
        .expect("fsync after flush cycles is still idempotent");
    assert_eq!(
        fresh_view(&fixture),
        after_first,
        "repeated sync and flush must not admit the same candidate again",
    );

    replace_draft(&mut later, &row("Later after repeats"));
    later
        .sync_all()
        .expect("a handle opened after the first acceptance still syncs");
    assert!(fresh_view(&fixture).contains("Later after repeats"));
}

/// ORNA-VFS-008: a dup shares one handle; syncing through either descriptor
/// acknowledges the same single candidate. A handle opened after that single
/// acceptance must still sync, so a second acceptance would be detectable.
#[tokio::test]
async fn dup_descriptors_share_one_draft_and_one_acceptance() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    let dup = file.try_clone().expect("dup the handle");
    replace_draft(&mut file, &row("Dup shared"));
    dup.sync_all()
        .expect("sync through the dup acknowledges the shared draft");
    assert!(fresh_view(&fixture).contains("Dup shared"));
    let mut later = open_rw(&fixture);
    file.sync_all()
        .expect("sync through the original is idempotent");
    drop(dup);
    assert!(fresh_view(&fixture).contains("Dup shared"));
    replace_draft(&mut later, &row("Dup later"));
    later
        .sync_all()
        .expect("a handle opened after the single acceptance still syncs");
    assert!(fresh_view(&fixture).contains("Dup later"));
}

/// ORNA-VFS-007/008: a close flush of an unsynced draft is not acceptance; the
/// close succeeds, a new open sees only accepted state, and the close leaves no
/// draft guard set.
#[tokio::test]
async fn close_flush_of_unsynced_draft_is_not_acceptance_and_close_succeeds() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Never synced"));
    let dup = file.try_clone().expect("dup the handle");
    drop(dup);
    drop(file);

    assert!(
        fresh_view(&fixture).contains("Original"),
        "a new open after close sees only accepted state",
    );
    assert!(!fresh_view(&fixture).contains("Never synced"));
    await_draft_guard(
        &fixture,
        b'0',
        "closing an unsynced draft must not leave the draft guard set",
    )
    .await;
}

/// ORNA-VFS-008: a rejected draft that is closed without a successful sync
/// leaves the durable row unchanged. This test does not observe close(2) errors.
#[tokio::test]
async fn close_after_rejected_sync_leaves_the_durable_row_unchanged() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &invalid_row("Bad close draft"));
    assert_errno(
        file.sync_all(),
        EINVAL,
        "invalid draft fsync must fail with EINVAL",
    );
    let before_close = fresh_view(&fixture);
    drop(file);
    assert_eq!(
        fresh_view(&fixture),
        before_close,
        "closing after a rejected sync must not change the durable row",
    );
    assert!(fresh_view(&fixture).contains("Original"));
}

/// ORNA-VFS-008: a child process that inherits the handle's open file
/// description (the shared-description effect of dup/fork) writes through it;
/// the parent's sync acknowledges that write, and a later unsynced parent write
/// is not acknowledged. A literal `fork(2)` cannot be called here because the
/// crate forbids `unsafe_code`; this gap is recorded on the Gate E bead.
#[tokio::test]
async fn inherited_handle_child_write_is_acknowledged_and_parent_unsynced_write_is_not() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    file.set_len(0).expect("truncate private candidate");
    file.seek(SeekFrom::Start(0)).expect("seek to start");

    let shared = file.try_clone().expect("dup for the child's stdout");
    let child_document = row("Inherited child");
    let status = std::process::Command::new("/bin/sh")
        .args(["-c", "printf '%s' \"$ORNA_CHILD_DOCUMENT\""])
        .env("ORNA_CHILD_DOCUMENT", &child_document)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(shared))
        .status()
        .expect("run child sharing the open file description");
    assert!(
        status.success(),
        "child write through the inherited handle must succeed"
    );
    file.sync_all()
        .expect("parent sync acknowledges the child's write");
    assert!(fresh_view(&fixture).contains("Inherited child"));

    replace_draft(&mut file, &row("Parent unsynced"));
    drop(file);
    let accepted = fresh_view(&fixture);
    assert!(accepted.contains("Inherited child"));
    assert!(
        !accepted.contains("Parent unsynced"),
        "the parent's unsynced write after the shared handle was used must not be acknowledged",
    );
}

/// ORNA-VFS-008: writes after a successful sync form a new candidate; only the
/// next sync acknowledges them.
#[tokio::test]
async fn writes_after_sync_start_a_new_candidate_until_the_next_sync() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("After first"));
    file.sync_all().expect("first sync acknowledges");

    replace_draft(&mut file, &row("After second"));
    assert!(
        read_own_draft(&mut file).contains("After second"),
        "the writing handle sees its own new candidate",
    );
    let accepted = fresh_view(&fixture);
    assert!(accepted.contains("After first"));
    assert!(
        !accepted.contains("After second"),
        "an unsynced write after a sync is not acknowledged",
    );
    assert!(fresh_view(&fixture).contains("After first"));

    file.sync_all()
        .expect("second sync acknowledges the new candidate");
    let accepted = fresh_view(&fixture);
    assert!(accepted.contains("After second"));
    assert!(!accepted.contains("After first"));
}

/// ORNA-VFS-008/EDIT-1: fsync and fdatasync on a clean handle with no pending
/// write do not invent a candidate and must succeed without changing the row.
#[tokio::test]
async fn clean_handle_sync_succeeds_without_changing_the_row() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let before = fresh_view(&fixture);
    let file = open_rw(&fixture);
    file.sync_all()
        .expect("fsync of a clean handle is not a failure");
    file.sync_data()
        .expect("fdatasync of a clean handle is not a failure");
    drop(file);
    assert_eq!(fresh_view(&fixture), before);
}

/// ORNA-VFS-009/EDIT-1: `fsync(temp)` preserves scratch bytes only; the row
/// changes when the temporary is renamed onto data.orna, not before.
#[tokio::test]
async fn temporary_sync_is_not_acceptance_and_rename_applies_it() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let temporary = fixture.row_directory.join(".data.orna.sync-semantics-tmp");
    let mut scratch = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .expect("create temporary sibling");
    scratch
        .write_all(row("Temp synced").as_bytes())
        .expect("write temporary");
    scratch.sync_all().expect("fsync(temp) succeeds");
    scratch.sync_data().expect("fdatasync(temp) succeeds");
    assert!(
        !fresh_view(&fixture).contains("Temp synced"),
        "fsync(temp) must not be row acceptance",
    );
    assert!(fresh_view(&fixture).contains("Original"));

    drop(scratch);
    std::fs::rename(&temporary, &fixture.data_path)
        .expect("rename applies the validated temporary");
    assert!(fresh_view(&fixture).contains("Temp synced"));
}

/// ORNA-VFS-009: a synced temporary that is abandoned instead of renamed never
/// reaches the row, and unlinking it clears the draft guard.
#[tokio::test]
async fn abandoned_synced_temporary_never_reaches_the_row() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let temporary = fixture.row_directory.join(".data.orna.abandoned-tmp");
    let mut scratch = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .expect("create temporary sibling");
    scratch
        .write_all(row("Abandoned temp").as_bytes())
        .expect("write temporary");
    scratch.sync_all().expect("fsync(temp) succeeds");
    drop(scratch);
    await_draft_guard(
        &fixture,
        b'1',
        "a synced scratch that is not yet renamed holds the draft guard",
    )
    .await;
    std::fs::remove_file(&temporary).expect("unlink abandoned temporary");
    assert!(
        !temporary.exists(),
        "unlink removes the abandoned temporary"
    );
    await_draft_guard(
        &fixture,
        b'0',
        "unlinking the abandoned temporary clears the draft guard",
    )
    .await;
    assert!(fresh_view(&fixture).contains("Original"));
    assert!(!fresh_view(&fixture).contains("Abandoned temp"));
}

/// ORNA-VFS-009: unlinking a clean scratch that is still open removes it from
/// the namespace; a later write through the open handle must not recreate draft
/// state for the missing inode, so the guard stays clear and unmount proceeds.
#[tokio::test]
async fn open_unlink_then_write_leaves_no_ghost_draft() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let temporary = fixture.row_directory.join(".data.orna.open-unlink-tmp");
    let mut scratch = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&temporary)
        .expect("create temporary sibling");
    std::fs::remove_file(&temporary).expect("unlink of a clean open scratch succeeds");
    assert!(!temporary.exists());
    // The handle outlived its name. The write and fsync outcomes are not the
    // subject here; the draft guard and unmount quiesce are.
    let _ = scratch.write_all(row("Ghost").as_bytes());
    let _ = scratch.sync_all();
    drop(scratch);
    await_draft_guard(
        &fixture,
        b'0',
        "a write through an unlinked handle must not leave draft state",
    )
    .await;
    set_quiesce(&fixture, b"1").expect("unmount quiesce is not blocked by a ghost draft");
    set_quiesce(&fixture, b"0").expect("cancel unmount quiesce");
}

/// ORNA-VFS-009: a temporary whose rename onto data.orna is rejected keeps the
/// old row and the scratch; the scratch cannot be unlinked away, and the guard
/// stays set until a corrected rename is accepted.
#[tokio::test]
async fn rejected_rename_keeps_scratch_and_guard_until_a_corrected_rename() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let temporary = fixture.row_directory.join(".data.orna.rejected-tmp");
    let mut scratch = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .expect("create temporary sibling");
    scratch
        .write_all(invalid_row("Rejected rename").as_bytes())
        .expect("write invalid temporary");
    scratch
        .sync_all()
        .expect("fsync(temp) preserves the bytes and is not acceptance");
    drop(scratch);

    assert_errno(
        std::fs::rename(&temporary, &fixture.data_path),
        EINVAL,
        "rename of an invalid temporary must fail with EINVAL",
    );
    assert!(temporary.exists(), "a failed rename keeps the scratch");
    assert!(
        fresh_view(&fixture).contains("Original"),
        "a failed rename leaves the old row",
    );
    await_draft_guard(&fixture, b'1', "the rejected scratch holds the draft guard").await;
    let unlink =
        std::fs::remove_file(&temporary).expect_err("unlink must not discard a rejected scratch");
    assert_eq!(unlink.raw_os_error(), Some(EBUSY), "{unlink}");
    assert!(temporary.exists());
    await_draft_guard(&fixture, b'1', "a rejected unlink leaves the guard set").await;

    let mut corrected = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&temporary)
        .expect("reopen the rejected scratch");
    replace_draft(&mut corrected, &row("Rejected corrected"));
    corrected
        .sync_all()
        .expect("fsync(temp) of the corrected bytes");
    drop(corrected);
    std::fs::rename(&temporary, &fixture.data_path).expect("corrected rename is accepted");
    assert!(fresh_view(&fixture).contains("Rejected corrected"));
    await_draft_guard(
        &fixture,
        b'0',
        "an accepted rename resolves the rejected scratch",
    )
    .await;
}

/// ORNA-VFS-008: a clean handle opened before another writer's accepted save
/// submitted nothing; its fsync and fdatasync succeed and leave no draft state.
#[tokio::test]
async fn stale_clean_handle_sync_succeeds_without_draft_state() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let stale = open_rw(&fixture);
    let mut writer = open_rw(&fixture);
    replace_draft(&mut writer, &row("Peer accepted"));
    writer.sync_all().expect("peer fsync accepts its candidate");
    stale
        .sync_all()
        .expect("fsync of a clean stale handle is not a failure");
    stale
        .sync_data()
        .expect("fdatasync of a clean stale handle is not a failure");
    assert_eq!(
        draft_guard(&fixture),
        b'0',
        "a clean stale handle holds no draft state",
    );
    drop(stale);
    drop(writer);
    await_draft_guard(
        &fixture,
        b'0',
        "dropping clean handles leaves no draft state",
    )
    .await;
    assert!(fresh_view(&fixture).contains("Peer accepted"));
}

/// ORNA-VFS-008: a same-length ftruncate on an open handle is pending draft
/// state until fsync; the accepted fsync clears the guard while the handle is
/// still open.
#[tokio::test]
async fn same_length_truncate_clears_guard_on_accepted_fsync_while_open() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Truncate same"));
    file.sync_all().expect("first fsync accepts the candidate");
    let accepted_len = fresh_view(&fixture).len() as u64;
    file.set_len(accepted_len)
        .expect("same-length ftruncate on the open handle");
    assert_eq!(
        draft_guard(&fixture),
        b'1',
        "a truncate on an open handle is pending draft state",
    );
    file.sync_all()
        .expect("fsync of the same-length candidate is acknowledged");
    assert_eq!(
        draft_guard(&fixture),
        b'0',
        "the accepted fsync clears the guard while the handle stays open",
    );
    drop(file);
    await_draft_guard(&fixture, b'0', "closing the handle leaves no draft state").await;
    assert!(fresh_view(&fixture).contains("Truncate same"));
}
