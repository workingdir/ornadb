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
use editor_mount::{
    EditorMount, clear_retained_draft_store_filler, fill_retained_draft_store,
    require_fuse_or_skip, retained_drafts, start_editor_mount,
};

const EINVAL: i32 = 22;
const EDQUOT: i32 = 122;

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

fn assert_view_contains(fixture: &EditorMount, needle: &str) {
    let view = fresh_view(fixture);
    assert!(view.contains(needle), "accepted view lacks {needle:?}: {view:?}");
}

fn assert_view_lacks(fixture: &EditorMount, needle: &str) {
    let view = fresh_view(fixture);
    assert!(!view.contains(needle), "accepted view contains {needle:?}: {view:?}");
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

/// Sets the mount root's unmount-quiesce attribute, as `orna` does
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

/// Asks the mount to retain every unresolved draft, as `orna` does before an
/// unmount. An error means at least one candidate could not be kept.
fn retain_drafts(fixture: &EditorMount) -> rustix::io::Result<()> {
    let root = mount_root(fixture);
    rustix::fs::setxattr(
        root.as_path(),
        "user.orna.retain-drafts",
        b"1",
        rustix::fs::XattrFlags::empty(),
    )
}

/// Puts a regular file where the recovery store directory belongs, so every
/// retain attempt fails. The caller removes the file to restore the store.
fn block_retained_store(fixture: &EditorMount) -> PathBuf {
    let blocker = fixture
        .repository
        .runtime_paths()
        .root()
        .join("vfs-retained-drafts-v1");
    std::fs::write(&blocker, b"not a directory").expect("block the recovery store");
    blocker
}

/// The mount's unresolved-draft report: one `path<TAB>state` line per draft.
fn unresolved_draft_list(fixture: &EditorMount) -> String {
    let mut value = vec![0_u8; 16 * 1024];
    let length = rustix::fs::getxattr(
        mount_root(fixture).as_path(),
        "user.orna.unresolved-draft-list",
        &mut value[..],
    )
    .expect("read the unresolved draft list");
    String::from_utf8_lossy(&value[..length]).into_owned()
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
    assert_view_contains(&fixture, "Fsync accepted");

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
    assert_view_lacks(&fixture, "Bad fsync draft");
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
    assert_view_contains(&fixture, "Datasync accepted");

    replace_draft(&mut file, &invalid_row("Bad datasync draft"));
    assert_errno(
        file.sync_data(),
        EINVAL,
        "invalid draft fdatasync must fail with EINVAL",
    );
    assert_view_lacks(&fixture, "Bad datasync draft");
    assert_view_contains(&fixture, "Datasync accepted");
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
    assert_view_contains(&fixture, "Later after repeats");
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
    assert_view_contains(&fixture, "Dup shared");
    let mut later = open_rw(&fixture);
    file.sync_all()
        .expect("sync through the original is idempotent");
    drop(dup);
    assert_view_contains(&fixture, "Dup shared");
    replace_draft(&mut later, &row("Dup later"));
    later
        .sync_all()
        .expect("a handle opened after the single acceptance still syncs");
    assert_view_contains(&fixture, "Dup later");
}

/// ORNA-VFS-007/008, EDIT-1 step 5: a close flush of a dirty draft attempts the
/// same acceptance as fsync, and the close succeeds. The accepted candidate is
/// visible to a new open, leaves no draft guard, and retains no draft.
#[tokio::test]
async fn close_flush_accepts_dirty_draft_once() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Closed dirty"));
    let dup = file.try_clone().expect("dup the handle");
    drop(dup);
    assert!(
        fresh_view(&fixture).contains("Closed dirty"),
        "the close flush of a dirty draft accepts it",
    );
    drop(file);
    assert_view_contains(&fixture, "Closed dirty");
    await_draft_guard(&fixture, b'0', "an accepted close leaves no draft guard").await;
    assert!(
        retained_drafts(&fixture.repository).is_empty(),
        "an accepted close retains no draft",
    );
    let mut later = open_rw(&fixture);
    replace_draft(&mut later, &row("After close"));
    later
        .sync_all()
        .expect("a handle opened after the close acceptance still syncs");
    assert_view_contains(&fixture, "After close");
}

/// ORNA-VFS-007/009: a close flush whose acceptance is refused has its error
/// ignored by the application. The durable row stays unchanged and the refused
/// candidate is retained as a recoverable draft that holds the draft guard.
#[tokio::test]
async fn ignored_close_error_leaves_row_unchanged_and_draft_retained() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &invalid_row("Refused on close"));
    drop(file);
    assert!(
        fresh_view(&fixture).contains("Original"),
        "a refused close flush leaves the accepted row unchanged",
    );
    assert_view_lacks(&fixture, "Refused on close");
    assert!(
        retained_drafts(&fixture.repository)
            .iter()
            .any(|draft| draft.contains("Refused on close")),
        "the refused candidate is retained in recovery storage",
    );
    await_draft_guard(&fixture, b'1', "the retained refusal holds the draft guard").await;
}

/// ORNA-VFS-008/009: a dirty handle whose baseline a peer has since accepted is
/// stale. Its close flush is refused, the peer's accepted row is kept, and the
/// stale candidate is retained. This test does not observe the close(2) errno.
#[tokio::test]
async fn stale_close_flush_keeps_peer_row_and_retains_candidate() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut stale = open_rw(&fixture);
    replace_draft(&mut stale, &row("Stale close draft"));
    let mut winner = open_rw(&fixture);
    replace_draft(&mut winner, &row("Winner accepted"));
    winner
        .sync_all()
        .expect("the peer's fsync accepts its candidate");
    drop(winner);
    drop(stale);
    assert!(
        fresh_view(&fixture).contains("Winner accepted"),
        "the peer's accepted row is kept",
    );
    assert_view_lacks(&fixture, "Stale close draft");
    assert!(
        retained_drafts(&fixture.repository)
            .iter()
            .any(|draft| draft.contains("Stale close draft")),
        "the stale candidate is retained in recovery storage",
    );
    await_draft_guard(
        &fixture,
        b'1',
        "the retained stale candidate holds the draft guard",
    )
    .await;
}

/// ORNA-VFS-008/009: a stale fsync refusal keeps the handle open. A peer's later
/// accepted save from a fresh baseline resolves that refusal, so dropping the
/// still-open stale handle must not re-insert the refusal and leave the draft
/// guard set.
#[tokio::test]
async fn stale_refusal_resolved_by_later_peer_accept_leaves_guard_clear_on_drop() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut stale = open_rw(&fixture);
    replace_draft(&mut stale, &row("Stale refused"));
    let mut winner = open_rw(&fixture);
    replace_draft(&mut winner, &row("Winner accepted"));
    if let Err(error) = winner.sync_all() {
        panic!(
            "peer fsync of the winner candidate failed (operation: fsync, errno {:?}): {error:?}",
            error.raw_os_error()
        );
    }
    drop(winner);
    let stale_sync = stale.sync_all();
    assert!(stale_sync.is_err(), "a stale fsync is refused: {stale_sync:?}");
    await_draft_guard(
        &fixture,
        b'1',
        "the refused stale candidate holds the draft guard",
    )
    .await;
    let mut fresh = open_rw(&fixture);
    replace_draft(&mut fresh, &row("Fresh after refusal"));
    fresh
        .sync_all()
        .expect("a fresh baseline accepts its candidate");
    assert_view_contains(&fixture, "Fresh after refusal");
    drop(fresh);
    drop(stale);
    await_draft_guard(
        &fixture,
        b'0',
        "the stale handle leaves no draft state once a peer resolved it",
    )
    .await;
}

/// ORNA-VFS-009, EDIT-1 step 5: a dirty valid draft that unmount retention keeps
/// is recovery data only. The accepted view is read before retention, because
/// retention quiesces the mount and later opens are refused. Closing the
/// retained handle leaves the candidate in recovery storage: retention cleared
/// the unresolved handle, so the close path has no candidate to commit.
#[tokio::test]
async fn retained_dirty_draft_is_not_accepted_by_close() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Retained unsynced"));
    // A close-time FLUSH (EDIT-1 step 5) may accept the dirty candidate at any
    // time, including the close a child spawned by another test in this process
    // performs at exec (ornadb-rw8g). Before retention the view is therefore one
    // complete candidate and nothing else.
    let before_retention = fresh_view(&fixture);
    assert!(
        before_retention.contains("Original") || before_retention.contains("Retained unsynced"),
        "the accepted view is one complete candidate before retention: {before_retention:?}",
    );
    retain_drafts(&fixture).expect("a valid dirty draft is retained");
    assert!(
        retained_drafts(&fixture.repository)
            .iter()
            .any(|draft| draft.contains("Retained unsynced")),
        "the dirty candidate is kept in recovery storage",
    );
    drop(file);
    assert!(
        retained_drafts(&fixture.repository)
            .iter()
            .any(|draft| draft.contains("Retained unsynced")),
        "closing a retained handle leaves the candidate in recovery storage",
    );
}

/// ORNA-VFS-009: when recovery storage refuses a dirty valid draft, retention
/// fails with EIO and the candidate stays in memory as an unresolved draft. The
/// draft guard stays set and quiesce is refused until a retry keeps the bytes.
#[tokio::test]
async fn refused_retention_keeps_candidate_until_a_retry_succeeds() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Retained valid"));
    let blocker = block_retained_store(&fixture);
    assert_eq!(
        retain_drafts(&fixture).expect_err("a refused store fails the retention"),
        rustix::io::Errno::IO,
        "a candidate recovery storage refuses makes retention fail with EIO",
    );
    await_draft_guard(&fixture, b'1', "a refused candidate holds the draft guard").await;
    assert_eq!(
        set_quiesce(&fixture, b"1").expect_err("quiesce is refused while a draft is unresolved"),
        rustix::io::Errno::BUSY,
        "an unretained candidate blocks quiesce",
    );
    std::fs::remove_file(&blocker).expect("unblock the recovery store");
    retain_drafts(&fixture).expect("a retry keeps the candidate once storage works");
    assert!(
        retained_drafts(&fixture.repository)
            .iter()
            .any(|draft| draft.contains("Retained valid")),
        "the retried candidate is kept in recovery storage",
    );
    drop(file);
}

/// ORNA-VFS-009, profiles/vfs-1 errno table: when recovery storage is at its
/// record limit, a refused fsync replies EDQUOT rather than EINVAL. The draft
/// stays unresolved and listed as not retained until storage has room.
#[tokio::test]
async fn full_recovery_store_replies_quota_to_refused_fsync_and_keeps_draft_listed() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &invalid_row("Full store draft"));
    fill_retained_draft_store(&fixture.repository);
    assert_errno(
        file.sync_all(),
        EDQUOT,
        "a refused draft that a full recovery store cannot keep fails with EDQUOT",
    );
    await_draft_guard(&fixture, b'1', "the unkept draft holds the draft guard").await;
    let list = unresolved_draft_list(&fixture);
    assert!(
        list.contains("Music/london/data.orna\tnot accepted (")
            && list.contains("candidate not retained"),
        "the unkept draft is listed as not retained: {list:?}",
    );
    assert!(
        !retained_drafts(&fixture.repository)
            .iter()
            .any(|draft| draft.contains("Full store draft")),
        "a full recovery store keeps none of the refused draft",
    );
    clear_retained_draft_store_filler(&fixture.repository);
    retain_drafts(&fixture).expect("a retry keeps the draft once storage has room");
    assert!(
        retained_drafts(&fixture.repository)
            .iter()
            .any(|draft| draft.contains("Full store draft")),
        "the retried draft is kept in recovery storage",
    );
    drop(file);
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
    assert_view_contains(&fixture, "Original");
}

/// ORNA-VFS-008: a child process that inherits the handle's open file
/// description (the shared-description effect of dup/fork) writes through it;
/// the parent's sync acknowledges that write. A later unsynced parent write is
/// not acknowledged by any fsync, but the close flush attempts it as dirty state
/// (EDIT-1 step 5). A literal `fork(2)` cannot be called here because the
/// crate forbids `unsafe_code`; this gap is recorded on the Gate E bead.
#[tokio::test]
async fn inherited_handle_child_write_is_acknowledged_and_parent_unsynced_write_is_flushed_on_close() {
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
    assert_view_contains(&fixture, "Inherited child");

    replace_draft(&mut file, &row("Parent unsynced"));
    drop(file);
    let accepted = fresh_view(&fixture);
    assert!(
        accepted.contains("Parent unsynced") && !accepted.contains("Inherited child"),
        "the close flush accepts the parent's dirty candidate once, superseding the child's row",
    );
}

/// EDIT-1 step 5 with the PR owner rule: the opening writer's own close commits
/// its dirty candidate when no child has closed an inherited copy first.
#[tokio::test]
async fn writer_close_commits_dirty_candidate_without_a_prior_child_close() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Solo writer"));
    drop(file);
    assert_view_contains(&fixture, "Solo writer");
}

/// PR owner rule (not EDIT-1 step 5 text): an exec'd child closes its CLOEXEC
/// copy of the parent's dirty candidate. That FLUSH comes from the child, not the
/// opener, so it must not accept; the parent's own close then commits.
#[tokio::test]
async fn child_exec_close_does_not_accept_parent_dirty_candidate_and_parent_close_commits() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let before = fresh_view(&fixture);
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Parent dirty"));
    let shared = file.try_clone().expect("dup for the exec'd child");
    let status = std::process::Command::new("/bin/true")
        .status()
        .expect("run child that execs and closes the inherited descriptor");
    assert!(status.success(), "exec'd child must succeed");
    assert_eq!(
        fresh_view(&fixture),
        before,
        "a child's exec-close must not accept the parent's dirty candidate"
    );
    drop(file);
    assert_view_contains(&fixture, "Parent dirty");
    drop(shared);
}

/// PR owner rule: a child that writes through an inherited descriptor and closes
/// it does not become the owner. Its close acknowledges without accepting; the
/// parent's own close (here its dup, then the original) commits the candidate.
#[tokio::test]
async fn child_write_through_inherited_handle_does_not_take_close_ownership() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let before = fresh_view(&fixture);
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Writer one"));
    file.seek(SeekFrom::Start(0)).expect("seek to start");
    let shared = file.try_clone().expect("dup for the child's stdout");
    let child_document = row("Writer two");
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", "printf '%s' \"$ORNA_CHILD_DOCUMENT\""])
        .env("ORNA_CHILD_DOCUMENT", &child_document)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(shared));
    let status = command.status().expect("run child writing through the inherited handle");
    assert!(status.success(), "child write through the inherited handle must succeed");
    assert_eq!(
        fresh_view(&fixture),
        before,
        "the child's close must not accept the candidate it wrote"
    );
    // The Command still owns the dup, so dropping it is the parent's own close.
    drop(command);
    drop(file);
    assert_view_contains(&fixture, "Writer two");
}

/// PR owner rule for truncate-only candidates: an O_TRUNC open is owned by its
/// opener, so an exec'd child's close of the inherited copy must not accept it.
#[tokio::test]
async fn child_exec_close_does_not_accept_parent_o_trunc_candidate() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let before = fresh_view(&fixture);
    let file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&fixture.data_path)
        .expect("open with O_TRUNC");
    let shared = file.try_clone().expect("dup for the exec'd child");
    let status = std::process::Command::new("/bin/true")
        .status()
        .expect("run child that execs and closes the inherited descriptor");
    assert!(status.success(), "exec'd child must succeed");
    assert_eq!(
        fresh_view(&fixture),
        before,
        "a child's exec-close must not accept the parent's O_TRUNC candidate"
    );
    drop(shared);
    drop(file);
}

/// PR owner rule for truncate-only candidates: a size change through a handle
/// (ftruncate) is owned by the handle's opener, so an exec'd child's close must
/// not accept it.
#[tokio::test]
async fn child_exec_close_does_not_accept_parent_ftruncate_candidate() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let before = fresh_view(&fixture);
    let file = open_rw(&fixture);
    file.set_len(0).expect("ftruncate candidate");
    let shared = file.try_clone().expect("dup for the exec'd child");
    let status = std::process::Command::new("/bin/true")
        .status()
        .expect("run child that execs and closes the inherited descriptor");
    assert!(status.success(), "exec'd child must succeed");
    assert_eq!(
        fresh_view(&fixture),
        before,
        "a child's exec-close must not accept the parent's ftruncate candidate"
    );
    drop(shared);
    drop(file);
}

/// ORNA-VFS-008: writes after a successful sync form a new candidate; the next
/// sync acknowledges it. A close-time flush may accept a dirty candidate at any
/// time (EDIT-1 step 5). The kernel sends it for every close of the open file
/// description, including the close a child spawned by another test in this
/// process performs when it execs, so before the next sync the accepted view is
/// either candidate and nothing else.
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
    let before_sync = fresh_view(&fixture);
    assert!(
        before_sync.contains("After first") || before_sync.contains("After second"),
        "the accepted view is one complete candidate: {before_sync:?}",
    );

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
    assert_view_contains(&fixture, "Original");

    drop(scratch);
    std::fs::rename(&temporary, &fixture.data_path)
        .expect("rename applies the validated temporary");
    assert_view_contains(&fixture, "Temp synced");
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
    assert_view_contains(&fixture, "Original");
    assert_view_lacks(&fixture, "Abandoned temp");
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
/// old row and the scratch, and the guard stays set until a corrected rename is
/// accepted. Removing a rejected scratch instead first retains its candidate in
/// recovery storage, so the name goes away without discarding the draft.
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
    assert_view_contains(&fixture, "Rejected corrected");
    await_draft_guard(
        &fixture,
        b'0',
        "an accepted rename resolves the rejected scratch",
    )
    .await;

    let removed = fixture.row_directory.join(".data.orna.rejected-removed");
    let mut scratch = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&removed)
        .expect("create a second temporary sibling");
    scratch
        .write_all(invalid_row("Rejected then removed").as_bytes())
        .expect("write invalid temporary");
    scratch
        .sync_all()
        .expect("fsync(temp) preserves the bytes and is not acceptance");
    drop(scratch);
    assert_errno(
        std::fs::rename(&removed, &fixture.data_path),
        EINVAL,
        "rename of an invalid temporary must fail with EINVAL",
    );
    await_draft_guard(&fixture, b'1', "the rejected scratch holds the draft guard").await;
    std::fs::remove_file(&removed)
        .expect("a rejected scratch can be removed once its candidate is retained");
    assert!(!removed.exists());
    await_draft_guard(&fixture, b'0', "removing the rejected scratch resolves it").await;
    assert!(
        retained_drafts(&fixture.repository)
            .iter()
            .any(|draft| draft.contains("Rejected then removed")),
        "the removed scratch candidate is retained in recovery storage"
    );
    assert_view_contains(&fixture, "Rejected corrected");
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
    assert_view_contains(&fixture, "Peer accepted");
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
    assert_view_contains(&fixture, "Truncate same");
}

/// ORNA-VFS-008, EDIT-1 step 2 (spec-silent order, flagged for review): a path
/// truncate while a writer holds a dirty candidate applies to that candidate, so
/// the fsync that accepts it accepts the truncated bytes, never the cut tail.
#[tokio::test]
async fn path_truncate_while_writer_open_cuts_the_accepted_candidate() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    let kept = row("Truncated before accept");
    replace_draft(&mut file, &format!("{kept}{}", row("discarded tail")));
    assert_eq!(draft_guard(&fixture), b'1', "a write is pending draft state");
    let truncated = std::process::Command::new("python3")
        .args(["-c", "import os, sys; os.truncate(sys.argv[1], int(sys.argv[2]))"])
        .arg(&fixture.data_path)
        .arg(kept.len().to_string())
        .output()
        .expect("run python3 os.truncate");
    assert!(
        truncated.status.success(),
        "path truncate of the open writer's candidate failed: {}",
        String::from_utf8_lossy(&truncated.stderr)
    );
    file.sync_all()
        .expect("fsync accepts the truncated candidate");
    assert_view_contains(&fixture, "Truncated before accept");
    assert_view_lacks(&fixture, "discarded tail");
    assert_eq!(
        draft_guard(&fixture),
        b'0',
        "the accepted fsync clears the dirty handle",
    );
    drop(file);
}

/// EDIT-1 step 2: a write followed by fsync of a valid candidate is acknowledged
/// only after the candidate is accepted, and the accepted fsync clears the
/// handle's dirty state.
#[tokio::test]
async fn write_then_fsync_accepts_candidate_and_clears_dirty_state() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Write then sync"));
    assert_eq!(draft_guard(&fixture), b'1', "a write is pending draft state");
    file.sync_all().expect("fsync of a written candidate is acknowledged");
    assert_view_contains(&fixture, "Write then sync");
    assert_eq!(
        draft_guard(&fixture),
        b'0',
        "the accepted fsync clears the dirty handle",
    );
}

/// EDIT-1 step 2: rewriting the accepted bytes produces a candidate equal to
/// the baseline; fsync of that candidate is acknowledged and leaves the handle
/// clean.
#[tokio::test]
async fn identical_bytes_write_then_fsync_is_ok_and_clean() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Identical bytes"));
    file.sync_all().expect("first fsync accepts the candidate");
    let accepted = fresh_view(&fixture);
    replace_draft(&mut file, &accepted);
    file.sync_all()
        .expect("fsync of a rewrite of the accepted bytes is acknowledged");
    assert_eq!(
        draft_guard(&fixture),
        b'0',
        "an identical-bytes rewrite leaves the handle clean",
    );
    assert_eq!(fresh_view(&fixture), accepted, "accepted bytes are unchanged");
}

/// EDIT-1 step 2: O_TRUNC with no write submits an empty candidate. That
/// candidate is validated like any other: the row schema rejects an empty
/// document, so fsync returns EINVAL (not the dirty-handle EIO), the accepted
/// view is unchanged, and a new open still reads the accepted row.
#[tokio::test]
async fn open_truncate_without_write_then_fsync_is_validated_not_eio() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut seed = open_rw(&fixture);
    replace_draft(&mut seed, &row("Truncate target"));
    seed.sync_all().expect("seed fsync accepts the candidate");
    drop(seed);
    let file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&fixture.data_path)
        .expect("open data.orna with O_TRUNC");
    assert_errno(
        file.sync_all(),
        EINVAL,
        "fsync of an O_TRUNC-only empty candidate is rejected by validation",
    );
    drop(file);
    assert_view_contains(&fixture, "Truncate target");
}

/// EDIT-1 step 2: a failed write (offset beyond the draft size limit) leaves no
/// candidate, so it must not mark the handle dirty; the following fsync is
/// acknowledged and the accepted view is unchanged.
#[tokio::test]
async fn failed_write_then_fsync_is_ok_and_not_dirty() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let mut file = open_rw(&fixture);
    replace_draft(&mut file, &row("Before failed write"));
    file.sync_all().expect("first fsync accepts the candidate");
    let accepted = fresh_view(&fixture);
    file.seek(SeekFrom::Start(1 << 40))
        .expect("seek beyond the draft size limit");
    assert!(
        file.write_all(b"x").is_err(),
        "a write beyond the draft size limit is refused",
    );
    assert_eq!(
        draft_guard(&fixture),
        b'0',
        "a refused write does not leave the handle dirty",
    );
    file.sync_all()
        .expect("fsync after a refused write is acknowledged");
    assert_eq!(fresh_view(&fixture), accepted, "accepted state is unchanged");
}
