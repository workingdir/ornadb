#![cfg(target_os = "linux")]

//! Gate E: zero truncation, ftruncate shrink/extend, sparse holes, quota
//! exhaustion, and path-based truncate refusal (VFS-007/008, EDIT-1,
//! profiles/vfs-1 errno table: EINVAL for malformed candidates, ENOSPC/EDQUOT
//! for capacity/quota).

use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
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
    EDQUOT, ENOSPC, EditorMount, assert_errno, await_draft_guard, draft_guard, fresh_view, open_rw,
    require_fuse_or_skip, run_orna_unmount, spawn_editor_mount, start_editor_mount,
};

/// profiles/vfs-1 errno table: EINVAL for malformed candidates.
const EINVAL: i32 = 22;

/// Compact-profile per-file bound (COMPACT_FILE_BOUND_BYTES); the mounted draft
/// quota must enforce it.
const FILE_QUOTA_BYTES: u64 = 64 * 1024 * 1024;

fn row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: \"Kieran\", }}\n")
}

fn replace_draft(file: &mut std::fs::File, document: &str) {
    file.set_len(0).expect("truncate private candidate");
    file.seek(SeekFrom::Start(0)).expect("seek to start");
    file.write_all(document.as_bytes())
        .expect("write private candidate");
}

/// Reads the handle's own candidate, the only place a rejected or unsynced
/// draft is visible.
fn read_own_draft(file: &mut std::fs::File) -> Vec<u8> {
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0)).expect("seek to start");
    file.read_to_end(&mut bytes).expect("read own draft");
    bytes
}

/// ORNA-VFS-008/EDIT-1: `O_TRUNC` on open empties only the private candidate.
/// Until a valid write is synced, the accepted row is unchanged; the synced
/// replacement is the only content a new open sees.
#[tokio::test]
async fn o_trunc_open_is_a_private_zero_length_candidate_until_synced() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture: EditorMount = start_editor_mount(false).await;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(true)
        .open(&fixture.data_path)
        .expect("open data.orna with O_TRUNC");
    assert!(
        read_own_draft(&mut file).is_empty(),
        "O_TRUNC leaves the writer's candidate empty",
    );
    assert!(
        fresh_view(&fixture).contains("Original"),
        "O_TRUNC is not acceptance: a new open still sees the accepted row",
    );
    assert_eq!(
        draft_guard(&fixture),
        b'1',
        "an O_TRUNC candidate is unresolved draft state",
    );

    replace_draft(&mut file, &row("Zero truncated"));
    file.sync_all()
        .expect("the valid replacement after O_TRUNC is acknowledged");
    let accepted = fresh_view(&fixture);
    assert!(accepted.contains("Zero truncated"));
    assert!(
        !accepted.contains("Original"),
        "no byte of the pre-truncate row survives the accepted replacement",
    );
    await_draft_guard(&fixture, b'0', "the accepted O_TRUNC draft is resolved").await;
}

/// ORNA-VFS-008: a zero-length row candidate is malformed for `data.orna`
/// (EINVAL) and is rejected at fsync. The durable row is not emptied.
#[tokio::test]
async fn zero_length_row_candidate_is_rejected_at_fsync() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let accepted = fresh_view(&fixture);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(true)
        .open(&fixture.data_path)
        .expect("open data.orna with O_TRUNC");

    assert_errno(
        file.sync_all(),
        &[EINVAL],
        "an empty row candidate must fail fsync with EINVAL",
    );
    assert!(read_own_draft(&mut file).is_empty());
    assert_eq!(
        fresh_view(&fixture),
        accepted,
        "a rejected zero-length candidate never empties the durable row",
    );
}

/// ORNA-VFS-008: ftruncate shrink is candidate state. A shrunk row is malformed,
/// so fsync rejects it whole; the accepted row stays intact and a corrected
/// replacement is then accepted.
#[tokio::test]
async fn shrink_truncate_is_rejected_whole_and_recovers_on_a_corrected_write() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let accepted = fresh_view(&fixture);
    let mut file = open_rw(&fixture);
    let half = accepted.len() as u64 / 2;
    file.set_len(half)
        .expect("ftruncate shrink on the open handle");
    assert_eq!(
        read_own_draft(&mut file).len() as u64,
        half,
        "the shrink is visible to its own handle",
    );
    assert_eq!(
        fresh_view(&fixture),
        accepted,
        "a shrink is not visible to a new open before fsync",
    );

    assert_errno(
        file.sync_all(),
        &[EINVAL],
        "a truncated row must fail fsync with EINVAL",
    );
    assert_eq!(
        fresh_view(&fixture),
        accepted,
        "the rejected shrink admits no partial row",
    );

    replace_draft(&mut file, &row("Shrink recovered"));
    file.sync_all()
        .expect("a corrected replacement after a rejected shrink is acknowledged");
    assert!(fresh_view(&fixture).contains("Shrink recovered"));
    await_draft_guard(&fixture, b'0', "the corrected shrink draft is resolved").await;
}

/// ORNA-VFS-008: ftruncate extend zero-fills the candidate. Zero bytes are not
/// valid row syntax, so fsync rejects the extended candidate whole (EINVAL) and
/// the accepted row is unchanged.
#[tokio::test]
async fn extend_truncate_zero_fills_candidate_and_zero_bytes_are_rejected() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let accepted = fresh_view(&fixture);
    let accepted_len = accepted.len() as u64;
    let mut file = open_rw(&fixture);
    file.set_len(accepted_len + 8)
        .expect("ftruncate extend on the open handle");

    let own = read_own_draft(&mut file);
    assert_eq!(own.len() as u64, accepted_len + 8);
    assert_eq!(
        &own[accepted.len()..],
        &[0_u8; 8],
        "extension reads back as defined zero bytes",
    );
    assert_errno(
        file.sync_all(),
        &[EINVAL],
        "an extended row with zero bytes must fail fsync with EINVAL",
    );
    assert_eq!(
        fresh_view(&fixture),
        accepted,
        "a rejected extension never reaches the accepted row",
    );
}

/// ORNA-VFS-008 sparse holes: a write past the end of the candidate creates a
/// hole that reads back as zero bytes. A row carrying those zero bytes is
/// malformed; fsync rejects it (EINVAL) and the accepted row is unchanged.
#[tokio::test]
async fn sparse_hole_past_end_reads_as_zeros_and_is_rejected_whole() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let accepted = fresh_view(&fixture);
    let accepted_len = accepted.len() as u64;
    let mut file = open_rw(&fixture);
    file.seek(SeekFrom::Start(accepted_len + 4))
        .expect("seek past the end of the candidate");
    file.write_all(b"x")
        .expect("write past the end creates a hole");

    let own = read_own_draft(&mut file);
    assert_eq!(own.len() as u64, accepted_len + 5);
    assert_eq!(
        &own[accepted.len()..accepted.len() + 4],
        &[0_u8; 4],
        "the sparse hole reads back as defined zero bytes",
    );
    assert_eq!(own[accepted.len() + 4], b'x');
    assert_errno(
        file.sync_all(),
        &[EINVAL],
        "a row with a sparse hole must fail fsync with EINVAL",
    );
    assert_eq!(
        fresh_view(&fixture),
        accepted,
        "a rejected sparse candidate admits no partial row",
    );
}

/// ORNA-VFS-008 quota: a write whose end passes the per-file bound fails with
/// ENOSPC or EDQUOT, leaves the candidate as it was, and is never admitted.
/// Dropping the handle without a sync leaves the accepted row unchanged.
#[tokio::test]
async fn write_past_file_quota_fails_with_quota_errno_and_is_not_admitted() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let accepted = fresh_view(&fixture);
    let mut file = open_rw(&fixture);
    let before = read_own_draft(&mut file);

    assert_errno(
        file.write_at(b"x", FILE_QUOTA_BYTES).map(|_| ()),
        &[ENOSPC, EDQUOT],
        "a write past the per-file quota must fail with ENOSPC or EDQUOT",
    );
    assert_eq!(
        read_own_draft(&mut file),
        before,
        "a quota-rejected write leaves no partial bytes in the candidate",
    );

    drop(file);
    assert_eq!(
        fresh_view(&fixture),
        accepted,
        "an unsynced quota-rejected candidate never changes the accepted row",
    );
    await_draft_guard(&fixture, b'0', "a quota-rejected write holds no draft").await;
}

/// Gate E unmount/restart: a quota-rejected write was never acknowledged, so
/// after unmount and remount of the same repository only the accepted row is
/// readable.
#[tokio::test]
async fn quota_rejected_write_is_not_retained_across_unmount_restart() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let accepted = fresh_view(&fixture);
    let mut file = open_rw(&fixture);
    assert_errno(
        file.write_at(b"x", FILE_QUOTA_BYTES).map(|_| ()),
        &[ENOSPC, EDQUOT],
        "a write past the per-file quota must fail with ENOSPC or EDQUOT",
    );
    drop(file);

    let EditorMount {
        _mounted: previous_mount,
        _mount_parent: mount_parent,
        _repository_directory: repository_directory,
        ..
    } = fixture;
    drop(previous_mount);
    let restarted_mountpoint = mount_parent.path().join("restarted");
    let _restarted_mount = spawn_editor_mount(repository_directory.path(), &restarted_mountpoint);
    assert_eq!(
        std::fs::read_to_string(restarted_mountpoint.join("Music/london/data.orna")).unwrap(),
        accepted,
        "after restart only the acknowledged row is readable",
    );
    run_orna_unmount(repository_directory.path(), &restarted_mountpoint);
}

/// ORNA-VFS-008 quota: ftruncate past the per-file bound fails with ENOSPC or
/// EDQUOT and the candidate length is unchanged.
#[tokio::test]
async fn truncate_past_file_quota_fails_with_quota_errno() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let accepted = fresh_view(&fixture);
    let mut file = open_rw(&fixture);

    assert_errno(
        file.set_len(FILE_QUOTA_BYTES + 1),
        &[ENOSPC, EDQUOT],
        "ftruncate past the per-file quota must fail with ENOSPC or EDQUOT",
    );
    assert_eq!(
        read_own_draft(&mut file).len(),
        accepted.len(),
        "a quota-rejected truncate leaves the candidate length unchanged",
    );
    file.sync_all()
        .expect("fsync of the unchanged candidate is not a rejection");
    assert_eq!(fresh_view(&fixture), accepted);
}

/// ORNA-VFS-008 quota: path-based truncate(2) past the per-file bound has no
/// open handle, so it is refused with EDQUOT before any pending size is
/// recorded. The accepted row is unchanged, and a later open starts from the
/// accepted row rather than a pending oversize truncate.
#[tokio::test]
async fn path_truncate_past_file_quota_is_refused_and_records_no_pending_truncate() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount(false).await;
    let accepted = fresh_view(&fixture);
    // The crate forbids `unsafe`, and rustix 0.38 has no path-based truncate, so
    // truncate(2) on the path (no open fd, hence the setattr route) is issued by
    // python3 `os.truncate`, which reports the errno text on stderr.
    let output = std::process::Command::new("python3")
        .args([
            "-c",
            "import os, sys; os.truncate(sys.argv[1], 100 * 1024 * 1024)",
        ])
        .arg(&fixture.data_path)
        .env("LC_ALL", "C")
        .output()
        .expect("run python3 os.truncate on the mounted data.orna");
    assert!(
        !output.status.success(),
        "path truncate past the per-file quota must fail",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Disk quota exceeded"),
        "path truncate past the per-file quota must fail with EDQUOT, got: {stderr}",
    );
    assert_eq!(
        fresh_view(&fixture),
        accepted,
        "a refused path truncate never changes the accepted row",
    );
    let mut file = open_rw(&fixture);
    assert_eq!(
        read_own_draft(&mut file),
        accepted.as_bytes(),
        "a later open starts from the accepted row, not a pending oversize truncate",
    );
    drop(file);
    await_draft_guard(&fixture, b'0', "a refused path truncate leaves no draft").await;
}
