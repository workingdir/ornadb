//! `orna commit`, `orna push` and `orna fetch` keep Orna's private refs local,
//! and a reviewed commit is created only from a configured identity after its
//! candidate validates.
//!
//! - ORNA-GIT-008 and ORNA-CAP-003: only allocator and consumer continuity
//!   refs travel with an ordinary push or fetch. Capture pins and reviewed
//!   commit state never leave or enter a repository, so they can neither block
//!   a push nor become witnesses a peer must hold.
//! - 16-administration, `commit`: the staged candidate is validated before its
//!   commit is created, and a missing Git identity fails before any commit
//!   object exists.
//!
//! Every verb is the real `orna` binary against real Git repositories.
//! The format-3 table comes from the shared CLI test harness: a repository
//! made by `orna init` has no store root yet, so it cannot pass reviewed
//! commit validation.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;

const CONTACT_SOURCE: &str = "pub table Contact(id: Str) {\n    name: Str,\n}\n";

const MEDIA_SOURCE: &str = r#"pub table media(id: Str) {
    title: Str,
    content: Blob,
}

pub fn add(id: Str, title: Str, content: Blob) =
    media.insert({ id: id, title: title, content: content });
"#;

/// Inherited variables that would let the caller's environment, rather than
/// the repository under test, route Git or supply an identity.
const INHERITED_GIT_VARIABLES: [&str; 10] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "EMAIL",
];

/// Runs the CLI with a private, empty home directory, so the only Git
/// identity it can find is the one the repository under test configures.
fn orna(directory: &Path, home: &Path, arguments: &[&str], environment: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    command
        .current_dir(directory)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for variable in INHERITED_GIT_VARIABLES {
        command.env_remove(variable);
    }
    command
        .envs(environment.iter().copied())
        .args(arguments)
        .output()
        .expect("CLI process starts")
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(arguments)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Every ref name under `prefix`, sorted.
fn refs(directory: &Path, prefix: &str) -> Vec<String> {
    git(directory, &["for-each-ref", "--format=%(refname)", prefix])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Every ref with the object it names, so a test can prove nothing moved.
fn ref_state(directory: &Path) -> String {
    git(directory, &["for-each-ref", "--format=%(objectname) %(refname)"])
}

/// Every commit object in the repository, reachable or not.
fn commit_objects(directory: &Path) -> BTreeSet<String> {
    git(
        directory,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objecttype) %(objectname)",
        ],
    )
    .lines()
    .filter_map(|line| line.strip_prefix("commit "))
    .map(str::to_owned)
    .collect()
}

fn current_branch(directory: &Path) -> String {
    git(directory, &["symbolic-ref", "--short", "HEAD"])
}

/// Appends one line to `main.orna` and stages it, giving `orna commit` a
/// real source change to review.
fn stage_source_change(directory: &Path, line: &str) {
    let path = directory.join("main.orna");
    let mut source = fs::read_to_string(&path).unwrap();
    source.push_str(line);
    source.push('\n');
    fs::write(&path, source).unwrap();
    git(directory, &["add", "main.orna"]);
}

/// Captures one file through the trusted REPL and inserts it as a Blob row,
/// the product path that leaves an owner-scoped pending pin under
/// `refs/orna/pins/` (CAPTURE-3).
fn capture_blob_row(root: &Path, home: &Path, key: &str) {
    fs::write(root.join("recording.mp3"), format!("synthetic audio for {key}")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    command
        .args(["--db", root.to_str().expect("UTF-8 path"), "repl"])
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for variable in INHERITED_GIT_VARIABLES {
        command.env_remove(variable);
    }
    let mut child = command.spawn().expect("Orna REPL process");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(
            format!(
                "let raw = sys.blob.capture_file({root:?}, \"recording.mp3\", 64);\nlet audio = sys.blob.annotate(raw, \"audio/mpeg\");\nmain.add({key:?}, \"Recording\", audio).id\n:quit\n",
                root = root.to_string_lossy().as_ref(),
            )
            .as_bytes(),
        )
        .expect("write trusted REPL submission");
    let output = child.wait_with_output().expect("wait for Orna REPL");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}\n{}", stderr(&output));
    assert!(
        !stdout.contains("ORNA-"),
        "the trusted REPL must capture and insert the row: {stdout}"
    );
}

#[test]
fn reviewed_commits_push_repeatedly_and_keep_private_refs_local() {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(CONTACT_SOURCE, "Contact");
    let root = directory.path();
    let home = TempDir::new().unwrap();
    let remote_root = TempDir::new().unwrap();
    let remote = remote_root.path().join("remote.git");
    git(remote_root.path(), &["init", "--bare", "--quiet", remote.to_str().unwrap()]);
    git(root, &["remote", "add", "origin", remote.to_str().unwrap()]);
    let branch = current_branch(root);
    let remote_branch = format!("refs/heads/{branch}");

    stage_source_change(root, "// first reviewed change");
    let committed = orna(root, home.path(), &["commit", "-m", "first reviewed change"], &[]);
    assert_eq!(committed.status.code(), Some(0), "first commit: {}", stderr(&committed));
    let pushed = orna(root, home.path(), &["push"], &[]);
    assert_eq!(pushed.status.code(), Some(0), "first push: {}", stderr(&pushed));
    assert_eq!(
        git(&remote, &["rev-parse", &remote_branch]),
        git(root, &["rev-parse", "HEAD"])
    );

    stage_source_change(root, "// second reviewed change");
    let committed = orna(root, home.path(), &["commit", "-m", "second reviewed change"], &[]);
    assert_eq!(committed.status.code(), Some(0), "second commit: {}", stderr(&committed));
    let pushed = orna(root, home.path(), &["push"], &[]);
    assert_eq!(
        pushed.status.code(),
        Some(0),
        "a push after a reviewed commit must not report a remote conflict: {}",
        stderr(&pushed)
    );
    assert_eq!(
        git(&remote, &["rev-parse", &remote_branch]),
        git(root, &["rev-parse", "HEAD"]),
        "the second push publishes the second reviewed commit"
    );

    assert_eq!(
        refs(root, "refs/orna/"),
        Vec::<String>::new(),
        "a completed reviewed commit leaves no private candidate ref behind"
    );
    assert_eq!(
        refs(&remote, "refs/orna/"),
        Vec::<String>::new(),
        "an ordinary push exposes no private Orna ref"
    );
}

#[test]
fn fetch_with_capture_pins_succeeds_without_importing_a_peers_pins() {
    let home = TempDir::new().unwrap();
    let (peer_directory, _peer_repository, _relation) = format3::format3_repository(MEDIA_SOURCE);
    let peer = peer_directory.path();
    capture_blob_row(peer, home.path(), "peer");
    let peer_pins = refs(peer, "refs/orna/pins/");
    assert_eq!(peer_pins.len(), 1, "the peer holds one capture pin");

    let local_root = TempDir::new().unwrap();
    let local = local_root.path().join("local");
    git(
        local_root.path(),
        &["clone", "--quiet", peer.to_str().unwrap(), local.to_str().unwrap()],
    );
    capture_blob_row(&local, home.path(), "local");
    let local_pins = refs(&local, "refs/orna/pins/");
    assert_eq!(local_pins.len(), 1, "the local repository holds its own capture pin");
    assert!(
        local_pins.iter().all(|pin| !peer_pins.contains(pin)),
        "each repository's capture runtime owns its own pins"
    );

    git(peer, &["commit", "--quiet", "--allow-empty", "-m", "peer advances"]);
    let branch = current_branch(peer);
    let fetched = orna(&local, home.path(), &["fetch", "origin", &branch], &[]);

    assert_eq!(
        (fetched.status.code(), refs(&local, "refs/orna/")),
        (Some(0), local_pins),
        "a local capture pin is not a continuity witness the peer must hold, and \
         fetch imports no peer pin: {}",
        stderr(&fetched)
    );
    assert_eq!(
        git(&local, &["rev-parse", &format!("refs/remotes/origin/{branch}")]),
        git(peer, &["rev-parse", "HEAD"]),
        "the requested branch is fetched"
    );
}

#[test]
fn commit_without_a_configured_identity_creates_no_commit_or_ref() {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(CONTACT_SOURCE, "Contact");
    let root = directory.path();
    let home = TempDir::new().unwrap();
    git(root, &["config", "--unset", "user.name"]);
    git(root, &["config", "--unset", "user.email"]);
    stage_source_change(root, "// unattributed change");
    let commits_before = commit_objects(root);
    let refs_before = ref_state(root);

    let refused = orna(
        root,
        home.path(),
        &["commit", "-m", "unattributed change"],
        &[("EMAIL", "guessed@example.invalid")],
    );

    assert_eq!(
        commit_objects(root),
        commits_before,
        "no commit object is created, so no identity is guessed from EMAIL: {}",
        stderr(&refused)
    );
    assert_eq!(ref_state(root), refs_before, "no ref is created or moved");
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("E2301"),
        "a missing identity has its own diagnostic: {}",
        stderr(&refused)
    );

    git(root, &["config", "user.name", "Orna Tester"]);
    git(root, &["config", "user.email", "tester@example.invalid"]);
    let committed = orna(
        root,
        home.path(),
        &["commit", "-m", "attributed change"],
        &[("EMAIL", "guessed@example.invalid")],
    );
    assert_eq!(committed.status.code(), Some(0), "{}", stderr(&committed));
    assert_eq!(
        git(root, &["log", "-1", "--format=%an <%ae>|%cn <%ce>"]),
        "Orna Tester <tester@example.invalid>|Orna Tester <tester@example.invalid>",
        "author and committer come from the configured identity"
    );
}

#[test]
fn invalid_candidate_is_refused_before_any_commit_object_exists() {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(CONTACT_SOURCE, "Contact");
    let root = directory.path();
    let home = TempDir::new().unwrap();
    git(root, &["rm", "-r", "--cached", "--quiet", ".orna/store"]);
    let commits_before = commit_objects(root);
    let refs_before = ref_state(root);

    let refused = orna(root, home.path(), &["commit", "-m", "drop the store root"], &[]);

    assert_eq!(
        commit_objects(root),
        commits_before,
        "validation runs before the candidate commit is created: {}",
        stderr(&refused)
    );
    assert_eq!(ref_state(root), refs_before, "no ref is created or moved");
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("E2302"),
        "an invalid candidate has its own diagnostic: {}",
        stderr(&refused)
    );
}
