//! `orna history <relation-hex> --diff <from> <to>` compares one source-owned
//! Object relation between two pinned snapshots. The fixture records the real
//! table identity and field schema at each endpoint; the diff reads no payload.

use std::path::Path;
use std::process::{Command, Output};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::{commit_source_captures, SourceCaptureRow};

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

/// Runs `orna history` with `arguments` inside `directory` and returns its
/// output, whatever the exit status.
fn run_history(directory: &Path, arguments: &[&str]) -> Output {
    let mut words = vec!["history"];
    words.extend_from_slice(arguments);
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(&words)
        .current_dir(directory)
        .output()
        .unwrap()
}

/// The 32-digit hex relation id the `orna history` CLI takes.
fn relation_hex(relation_id: [u8; 16]) -> String {
    let mut output = String::with_capacity(32);
    for byte in relation_id {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

/// The song row is added after `before`, then its optional cover changes from
/// None to Some without reading either snapshot's payloads.
#[tokio::test]
async fn diff_between_two_pinned_snapshots_reports_a_nullable_blob_field_change_without_payloads() {
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(
            Path::new(MEDIA_FIXTURES).join(name),
            source.path().join(name),
        )
        .unwrap();
    }
    let (directory, repository, relation_id, format_context) =
        object_format3_repository(source.path());
    let before = String::from_utf8(git_output(directory.path(), &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();

    let runtime_identity = RuntimeIdentity {
        database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
        repository_id: [0x81; 16],
    };
    let state = RuntimeState::open(&repository, runtime_identity, [0x82; 32])
        .await
        .unwrap();
    let writer = state.acquire_lease([0x83; 16]).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let capture = format!("sys.blob.capture_file({root}, \"tone.wav\", 65536)");
    let initial_fields = [("payload", capture.as_str())];
    let initial_row = [SourceCaptureRow {
        key: "song",
        blob_fields: &initial_fields,
        source_row: None,
        ordinal: 0x90,
        insert_only: true,
    }];
    commit_source_captures(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        &initial_row,
    )
    .await;
    let after = commit_source_row_to_native_graph(
        directory.path(),
        &repository,
        &before,
        relation_id,
        &format_context,
        "song",
    );
    assert_ne!(before, after, "publishing the Object row advanced HEAD");

    let relation = relation_hex(relation_id);
    let output = run_history(
        directory.path(),
        &[&relation, "--diff", &before, &after, "--format", "json"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "history --diff failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let diff: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    // The source fixture contains several candidate Object rows, but only the
    // admitted song row is published in either native store endpoint.
    assert_eq!(
        diff["added"],
        serde_json::json!(1),
        "one row was added: {diff}"
    );
    assert_eq!(diff["changed"], serde_json::json!(0), "no row changed");
    assert_eq!(diff["removed"], serde_json::json!(0), "no row was removed");
    let changes = diff["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["change"], serde_json::json!("added"));
    assert_eq!(changes[0]["key"], serde_json::json!("song"));

    // The comparison is a local read: neither side fetched a media payload.
    assert_eq!(
        diff["media_payload_bytes_read"],
        serde_json::json!(0),
        "the diff must not fetch media payloads"
    );

    // Comparing a snapshot with itself reports no change, so the rows genuinely
    // come from the named commits rather than from the working tree.
    let identical = run_history(
        directory.path(),
        &[&relation, "--diff", &after, &after, "--format", "json"],
    );
    assert_eq!(identical.status.code(), Some(0));
    let identical: serde_json::Value = serde_json::from_slice(&identical.stdout).unwrap();
    assert_eq!(identical["added"], serde_json::json!(0));
    assert_eq!(identical["changed"], serde_json::json!(0));
    assert_eq!(identical["removed"], serde_json::json!(0));

    // Reversing the endpoints reports the same row as removed, so the sides are
    // not interchangeable.
    let reversed = run_history(
        directory.path(),
        &[&relation, "--diff", &after, &before, "--format", "json"],
    );
    assert_eq!(reversed.status.code(), Some(0));
    let reversed: serde_json::Value = serde_json::from_slice(&reversed.stdout).unwrap();
    assert_eq!(reversed["added"], serde_json::json!(0));
    assert_eq!(reversed["removed"], serde_json::json!(1));

    // The human listing names the change and the payload-free cost.
    let human = run_history(directory.path(), &[&relation, "--diff", &before, &after]);
    assert_eq!(human.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(stdout.contains("added   song"), "human listing: {stdout}");
    assert!(
        stdout.contains("media payload bytes read: 0"),
        "human listing reports the cost: {stdout}"
    );

    // An unresolvable endpoint is refused rather than silently reading HEAD.
    let missing = run_history(
        directory.path(),
        &[&relation, "--diff", &before, "no-such-ref"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("Snapshot could not be resolved"),
        "unresolvable endpoint refused: {}",
        String::from_utf8_lossy(&missing.stderr)
    );

    drop(bindings);
    drop(state);

    let updated_state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
            repository_id: [0x81; 16],
        },
        [0x82; 32],
    )
    .await
    .unwrap();
    let writer = updated_state.acquire_lease([0x83; 16]).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let updated_commit = repository.head().unwrap().unwrap();
    let committed_source = repository
        .read_committed_file(&updated_commit, "Object/song.orna", 1 << 20)
        .unwrap();
    let updated_format_context =
        format3_context_for_snapshot(&repository, updated_commit.as_str());
    let updated_source = std::str::from_utf8(&committed_source)
        .unwrap()
        .replace("    cover: null,\n", "");
    assert!(
        !updated_source.contains("cover"),
        "cover is omitted from the updated source row: {updated_source}"
    );
    let cover_capture = format!(r#"sys.blob.capture_file({root}, "pixel.png", 65536)"#);
    let updated_fields = [("payload", capture.as_str()), ("cover", cover_capture.as_str())];
    let updated_row = [SourceCaptureRow {
        key: "song",
        blob_fields: &updated_fields,
        source_row: Some(&updated_source),
        ordinal: 0xA0,
        insert_only: false,
    }];
    commit_source_captures(
        &repository,
        &updated_state,
        writer,
        &mut bindings,
        relation_id,
        &updated_format_context,
        &updated_row,
    )
    .await;
    let with_cover = commit_source_row_to_native_graph(
        directory.path(),
        &repository,
        &after,
        relation_id,
        &format_context,
        "song",
    );
    assert_ne!(after, with_cover, "publishing the cover update advanced HEAD");
    drop(bindings);
    drop(updated_state);

    let cover_diff = run_history(
        directory.path(),
        &[&relation, "--diff", &after, &with_cover, "--format", "json"],
    );
    assert_eq!(
        cover_diff.status.code(),
        Some(0),
        "history --diff for None to Some cover failed: {}",
        String::from_utf8_lossy(&cover_diff.stderr)
    );
    let cover_diff: serde_json::Value = serde_json::from_slice(&cover_diff.stdout).unwrap();
    assert_eq!(cover_diff["added"], serde_json::json!(0));
    assert_eq!(cover_diff["changed"], serde_json::json!(1));
    assert_eq!(cover_diff["removed"], serde_json::json!(0));
    assert_eq!(cover_diff["changes"][0]["change"], "changed");
    assert_eq!(cover_diff["changes"][0]["key"], "song");
    assert_eq!(cover_diff["media_payload_bytes_read"], serde_json::json!(0));

    drop(directory);
}

/// A repository records its database identity in the tracked metadata of every
/// snapshot. A reinitialized repository rewrites that identity while its older
/// commits stay reachable, so one repository can hold snapshots of two
/// databases. The format-3 row map is keyed by the recorded identity, so a
/// diff across two identities would join unrelated rows and report them as
/// changes. The endpoints must be refused instead.
///
/// The second identity is written only into the commit, never the checkout, so
/// the refusal also proves the identity is read from each pinned snapshot:
/// reading the worktree's metadata would report the same identity for both
/// endpoints and compare them instead of refusing.
#[test]
fn diff_refuses_endpoints_that_record_different_repositories() {
    let (directory, _repository, relation_id) = empty_format3_repository();
    let root = directory.path();
    let before = revision(root, "HEAD");

    // Same tree, same identity, one commit later: still one repository, so a
    // diff of it must proceed rather than refuse.
    let same_database = std::fs::read_to_string(root.join(".orna/database.orna")).unwrap();
    let later = commit_metadata(root, &before, &same_database, "same identity");

    // A different database identity, exactly as reinitialization records it.
    let other_database = "{\n    repository_format: 3,\n    database_id: \
                          \"00000000-0000-4000-8000-000000000002\",\n}\n";
    let other = commit_metadata(root, &before, other_database, "other identity");

    let relation = relation_hex(relation_id);
    let refused = run_history(
        root,
        &[&relation, "--diff", &before, &other, "--format", "json"],
    );
    assert_eq!(
        refused.status.code(),
        Some(1),
        "a diff across two database identities must be refused: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("Snapshots belong to different repositories"),
        "the refusal names the identity mismatch: {}",
        String::from_utf8_lossy(&refused.stderr)
    );

    // The guard is about identity, not about naming two different commits.
    let allowed = run_history(
        root,
        &[&relation, "--diff", &before, &later, "--format", "json"],
    );
    assert_eq!(
        allowed.status.code(),
        Some(0),
        "two commits of one database still compare: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    let allowed: serde_json::Value = serde_json::from_slice(&allowed.stdout).unwrap();
    assert_eq!(allowed["added"], serde_json::json!(0));
    assert_eq!(allowed["removed"], serde_json::json!(0));

    drop(directory);
}

/// One abbreviated git query as trimmed text.
fn revision(directory: &Path, spec: &str) -> String {
    String::from_utf8(git_output(directory, &["rev-parse", spec], None))
        .unwrap()
        .trim()
        .to_owned()
}

/// Commits `parent`'s tree with `.orna/database.orna` replaced by `database`,
/// the shape a reinitialized repository commits. Only the commit changes; the
/// checkout keeps its previous metadata.
///
/// The new commit is also pinned by `refs/checks/<message>` and checked out, so
/// it is a reachable snapshot rather than a dangling object. A repository
/// resolves a snapshot only when the commit is HEAD or reachable from a ref
/// (`Repository::resolve_snapshot`); a commit nothing points at is refused, so
/// the endpoint here must be named by a ref like any real snapshot. Pinning it
/// also survives the next call, which advances HEAD to a different commit.
fn commit_metadata(directory: &Path, parent: &str, database: &str, message: &str) -> String {
    let database_oid = git_object(directory, "blob", database.as_bytes());
    let store_oid = revision(directory, &format!("{parent}:.orna/store"));
    let source_oid = revision(directory, &format!("{parent}:main.orna"));
    let old_orna_tree = revision(directory, &format!("{parent}:.orna"));
    let old_orna_entries =
        String::from_utf8(git_output(directory, &["ls-tree", &old_orna_tree], None)).unwrap();
    let mut orna_entries =
        format!("100644 blob {database_oid}\tdatabase.orna\n040000 tree {store_oid}\tstore\n");
    for entry in old_orna_entries
        .lines()
        .filter(|entry| !entry.ends_with("\tdatabase.orna") && !entry.ends_with("\tstore"))
    {
        orna_entries.push_str(entry);
        orna_entries.push('\n');
    }
    let orna_tree = git_tree(directory, &orna_entries);
    let root_tree = git_tree(
        directory,
        &format!("040000 tree {orna_tree}\t.orna\n100644 blob {source_oid}\tmain.orna\n"),
    );
    let commit = String::from_utf8(git_output(
        directory,
        &["commit-tree", &root_tree, "-p", parent, "-m", message],
        None,
    ))
    .unwrap()
    .trim()
    .to_owned();
    let head_ref = String::from_utf8(git_output(directory, &["symbolic-ref", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    git(directory, &["update-ref", &head_ref, &commit]);
    // A diff endpoint is resolved inside the repository, and a commit no ref
    // reaches is not readable there. Both callers build a commit and then move
    // HEAD to the next one, so each commit needs a stable, valid ref of its own.
    let anchor = format!("refs/orna-diff-fixture/{commit}");
    git(directory, &["update-ref", &anchor, &commit]);
    commit
}

/// A name carried by more than one ref at once names no single snapshot. Git
/// resolves it by ref precedence and reports success with only a warning, so a
/// diff pinned with an ambiguous endpoint would silently compare against
/// whichever ref Git preferred. The endpoint guard is the same one `--at`
/// uses, so the ambiguity is refused here too.
#[test]
fn diff_refuses_an_endpoint_carried_by_more_than_one_ref() {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    for words in [
        &["init", "--quiet"][..],
        &["config", "user.email", "kieran@drewett.dev"][..],
        &["config", "user.name", "kierandrewett"][..],
        &["config", "commit.gpgsign", "false"][..],
    ] {
        git_output(root, words, None);
    }
    std::fs::write(
        root.join("main.orna"),
        include_bytes!("fixtures/run/main.orna"),
    )
    .unwrap();
    git_output(root, &["add", "--all"], None);
    git_output(root, &["commit", "--quiet", "-m", "one commit"], None);
    // One short name, two refs: a branch and a tag.
    git_output(root, &["branch", "twin"], None);
    git_output(root, &["tag", "twin"], None);

    // A relation id that exists in no repository: the guard must run before
    // any row is read, so the refusal is about the name alone.
    let relation = "00000000000000000000000000000001";
    let refused = run_history(
        root,
        &[
            relation,
            "--diff",
            "twin",
            "refs/heads/twin",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        refused.status.code(),
        Some(1),
        "an ambiguous diff endpoint must be refused"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("Snapshot name is ambiguous"),
        "the refusal names the ambiguity: {stderr}"
    );

    // A full ref name identifies one object: the guard must not refuse it. The
    // read still fails on the unknown relation, so the check is the reason.
    let full = run_history(
        root,
        &[
            relation,
            "--diff",
            "refs/heads/twin",
            "refs/heads/twin",
            "--format",
            "json",
        ],
    );
    assert!(
        !String::from_utf8_lossy(&full.stderr).contains("Snapshot name is ambiguous"),
        "a full ref name is not ambiguous: {}",
        String::from_utf8_lossy(&full.stderr)
    );

    drop(directory);
}
