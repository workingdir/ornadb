//! `orna history <relation-hex> --diff <from> <to>` compares one relation
//! between two pinned snapshots over real imported media rows. Both endpoints
//! are named commits, so the diff is reproducible from the repository alone:
//! the row added by the later import shows as `added`, the row present on both
//! sides shows as no change, and the comparison reads no media payload.

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
use media_commit::commit_capture;

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

/// The full revision once more: the song row exists on both sides, so a diff
/// between an older and a newer commit reports exactly the row the newer
/// commit added and leaves the older row untouched.
#[tokio::test]
async fn diff_between_two_pinned_snapshots_reports_the_added_row_without_payloads() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(
            Path::new(MEDIA_FIXTURES).join(name),
            source.path().join(name),
        )
        .unwrap();
    }

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
    let capture = |name: &str| format!("sys.blob.capture_file({root}, {name:?}, 65536)");

    // The song alone is the earlier snapshot.
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &capture("tone.wav"),
        "song",
        0x80,
        true,
    )
    .await;
    let before = String::from_utf8(git_output(directory.path(), &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();

    // The image import is the later snapshot.
    commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &capture("pixel.png"),
        "image",
        0x90,
        true,
    )
    .await;
    let after = String::from_utf8(git_output(directory.path(), &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    assert_ne!(before, after, "the image import advanced HEAD");
    drop(bindings);
    drop(state);

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

    // The image row is the difference; the song row is unchanged on both sides.
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
    assert_eq!(changes[0]["key"], serde_json::json!("image"));

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
    assert!(stdout.contains("added   image"), "human listing: {stdout}");
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
    let orna_tree = git_tree(
        directory,
        &format!("100644 blob {database_oid}\tdatabase.orna\n040000 tree {store_oid}\tstore\n"),
    );
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
    let anchor = format!("refs/orna-diff-fixture/{}", message.replace(' ', "-"));
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
    std::fs::write(root.join("main.orna"), b"module main;\n").unwrap();
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
