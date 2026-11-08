#![cfg(target_os = "linux")]

//! Gate E: the mount-level aggregate draft budget (profiles/vfs-1). Every open
//! candidate in one mounted editor draws on a single quota. Needs the
//! `test-seams` feature: only that build lets the mount read its budget from
//! `ORNA_DRAFT_QUOTA_BYTES`.

use std::{
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
};

#[path = "support/format3.rs"]
mod format3;
#[path = "support/media_commit.rs"]
mod media_commit;

#[path = "support/editor_mount.rs"]
mod editor_mount;
use editor_mount::{
    EDQUOT, assert_errno, repository_with_music_rows, require_fuse_or_skip,
    spawn_mount_process_with_env,
};
use tempfile::TempDir;

fn row(title: &str) -> String {
    format!("{{ title: \"{title}\", artist: \"Kieran\", }}\n")
}

fn replace_draft(file: &mut std::fs::File, document: &str) {
    file.set_len(0).expect("truncate private candidate");
    file.seek(SeekFrom::Start(0)).expect("seek to start");
    file.write_all(document.as_bytes())
        .expect("write private candidate");
}

/// Mount-level aggregate draft budget (profiles/vfs-1): every open candidate in
/// one mounted editor draws on a single quota, not one quota per document. The
/// mount runs with a 1 MiB budget through `ORNA_DRAFT_QUOTA_BYTES`. The first
/// candidate grows to exactly that budget and stays open, so the second
/// document's one-byte candidate is refused EDQUOT and its accepted row is
/// unchanged. Truncating the held first candidate returns its charge, and the
/// same candidate refills the whole budget. A valid replacement of the first
/// candidate is then synced and accepted, after which the second candidate
/// fills the whole budget exactly. The replacement truncates before it syncs,
/// so the accept step does not isolate its own release from the truncate.
#[tokio::test]
async fn open_candidates_share_one_draft_quota_across_documents() {
    const BUDGET_BYTES: u64 = 1 << 20;
    if !require_fuse_or_skip() {
        return;
    }
    let first_row = row("first");
    let second_row = row("second");
    let (repository_directory, _repository, _relation_id) = repository_with_music_rows(&[
        ("row-00", first_row.as_str()),
        ("row-01", second_row.as_str()),
    ])
    .await;
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    let budget = BUDGET_BYTES.to_string();
    let mut mounted = spawn_mount_process_with_env(
        repository_directory.path(),
        &mountpoint,
        &[("ORNA_DRAFT_QUOTA_BYTES", budget.as_str())],
    );
    let document = |key: &str| mountpoint.join(format!("Music/{key}/data.orna"));
    mounted.await_path(&document("row-00"));
    let second_accepted =
        std::fs::read_to_string(document("row-01")).expect("read the accepted second row");

    // The first candidate stays open, so its charge is held while the second opens.
    let mut first = OpenOptions::new()
        .read(true)
        .write(true)
        .open(document("row-00"))
        .expect("open the first candidate read-write");
    first
        .write_at(b"X", BUDGET_BYTES - 1)
        .expect("the first candidate fills the shared budget exactly");

    let second = OpenOptions::new()
        .read(true)
        .write(true)
        .open(document("row-01"))
        .expect("open the second candidate read-write");
    assert_errno(
        second.write_at(b"x", 0).map(|_| ()),
        &[EDQUOT],
        "a one-byte candidate is refused once an open candidate holds the shared budget",
    );
    drop(second);
    assert_eq!(
        std::fs::read_to_string(document("row-01")).expect("read the second row"),
        second_accepted,
        "the refused candidate leaves its accepted row unchanged",
    );

    // Release on truncate: truncating the held first candidate returns its
    // charge, so the same candidate refills the whole budget.
    first
        .set_len(0)
        .expect("truncating the first candidate releases its charge");
    first
        .write_at(b"X", BUDGET_BYTES - 1)
        .expect("the released budget refills the first candidate");

    // Release on accept: a valid replacement of the first candidate is synced
    // and accepted, after which the second candidate fills the whole budget
    // exactly. The replacement's own truncate also returns the charge, so this
    // step does not isolate accept from truncate. A dirty draft released
    // unsynced keeps its charge until resolved (ORNA-VFS-009), so no step
    // relies on a bare close to return budget.
    replace_draft(&mut first, &first_row);
    first
        .sync_all()
        .expect("sync the valid first candidate, which is accepted");
    drop(first);
    assert_eq!(
        std::fs::read_to_string(document("row-00")).expect("read the accepted first row"),
        first_row,
        "the synced replacement is the accepted first row",
    );
    let mut second = OpenOptions::new()
        .read(true)
        .write(true)
        .open(document("row-01"))
        .expect("reopen the second candidate read-write");
    second
        .write_at(b"X", BUDGET_BYTES - 1)
        .expect("accepting the first candidate returns the shared budget");
    replace_draft(&mut second, &second_accepted);
    second
        .sync_all()
        .expect("sync the valid second candidate, which is accepted");
    drop(second);
    assert_eq!(
        std::fs::read_to_string(document("row-01")).expect("read the accepted second row"),
        second_accepted,
        "the synced replacement is the accepted second row",
    );
}
