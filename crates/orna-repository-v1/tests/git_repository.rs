#[path = "../src/test_support.rs"]
mod test_support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use fs2::FileExt;
use orna_foundation_v1::{GitHash, OvbRaw, SchemaDescriptor};
use orna_repository_v1::{
    CheckoutTarget, CommittedTreeEntryKind, CompactManifest,
    GitDeclaredObjectSetState, GitObjectKind, GitObjectState, GitRepositoryMode, IndexGeneration,
    LegacyAnnotationCandidate, ManagedFileChange, ManagedPath, MigrationAnnotationDefaults,
    MigrationContinuityRecord, MountError, MountView, NativeObjectId, OrnaInternalRef,
    PublicationJournal, PublicationJournalEntry, PublicationJournalStage, RemoteContinuity,
    Repository, RepositoryError, RequiredInternalRef, RuntimeGeneration,
    StreamCheckpointIdentityPredecessor, WorktreeState,
};

use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use uuid::Uuid;

/// Accepts every checkout target: these repository-level tests stage no assertion.
struct AcceptCheckout;

impl orna_repository_v1::CheckoutAssertionGate for AcceptCheckout {
    fn validate(
        &self, _: &orna_repository_v1::Repository, _: &orna_repository_v1::CheckoutPreflight,
    ) -> Result<(), orna_repository_v1::CandidateAssertionRefusal> {
        Ok(())
    }
}

/// Runs `check` over the locked checkout preflight, standing in for the
/// production checkout gate (ORNA-ASSERT-044).
struct CheckoutGate<'a>(
    &'a dyn Fn(
        &orna_repository_v1::Repository,
        &orna_repository_v1::CheckoutPreflight,
    ) -> Result<(), orna_repository_v1::CandidateAssertionRefusal>,
);

impl orna_repository_v1::CheckoutAssertionGate for CheckoutGate<'_> {
    fn validate(
        &self,
        repository: &orna_repository_v1::Repository,
        plan: &orna_repository_v1::CheckoutPreflight,
    ) -> Result<(), orna_repository_v1::CandidateAssertionRefusal> {
        (self.0)(repository, plan)
    }
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn decode_oid(object_id: &str) -> Vec<u8> {
    object_id
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).unwrap();
            let low = (pair[1] as char).to_digit(16).unwrap();
            ((high << 4) | low) as u8
        })
        .collect()
}

fn repository_with_object_format(format: &str) -> Option<TempDir> {
    let temp = TempDir::new().unwrap();
    let output = Command::new("git")
        .current_dir(temp.path())
        .args(["init", "-b", "main"])
        .arg(format!("--object-format={format}"))
        .output()
        .unwrap();
    if !output.status.success() {
        return None;
    }
    test_support::configure_fixture_git_identity(temp.path());
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(
        temp.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna"),
    )
    .unwrap();
    git(temp.path(), &["add", "main.orna"]);
    git(temp.path(), &["commit", "-m", "initial"]);
    Some(temp)
}

fn repository() -> TempDir {
    let temp = TempDir::new().unwrap();
    git(temp.path(), &["init", "-b", "main"]);
    test_support::configure_fixture_git_identity(temp.path());
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(
        temp.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna"),
    )
    .unwrap();
    fs::write(temp.path().join("ordinary.txt"), "base\n").unwrap();
    fs::create_dir_all(temp.path().join(".orna")).unwrap();
    fs::write(temp.path().join(".orna/format.orna"), "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n").unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-m", "initial"]);
    temp
}

fn repository_with_legacy_format(format: &str) -> TempDir {
    let temp = TempDir::new().unwrap();
    git(temp.path(), &["init", "-b", "main"]);
    test_support::configure_fixture_git_identity(temp.path());
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(
        temp.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna"),
    )
    .unwrap();
    fs::write(temp.path().join("ordinary.txt"), "base\n").unwrap();
    fs::create_dir_all(temp.path().join(".orna")).unwrap();
    fs::write(temp.path().join(".orna/format.orna"), format).unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-m", "legacy format fixture"]);
    temp
}

fn repository_with_v1_module_fixture() -> TempDir {
    let temp = TempDir::new().unwrap();
    git(temp.path(), &["init", "-b", "main"]);
    test_support::configure_fixture_git_identity(temp.path());
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(
        temp.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna").as_bytes(),
    )
    .unwrap();
    fs::write(temp.path().join("ordinary.txt"), "base\n").unwrap();
    fs::create_dir_all(temp.path().join(".orna")).unwrap();
    fs::write(temp.path().join(".orna/format.orna"), "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n").unwrap();
    git(temp.path(), &["add", "."]);
    git(
        temp.path(),
        &["commit", "-m", "initial with checked-in Orna fixture"],
    );
    temp
}

fn configure_hostile_git_environment(command: &mut Command, hostile: &Path) {
    command
        .env("GIT_DIR", hostile.join(".git"))
        .env("GIT_WORK_TREE", hostile)
        .env("GIT_INDEX_FILE", hostile.join(".git/index"))
        .env("GIT_COMMON_DIR", hostile.join(".git"))
        .env("GIT_OBJECT_DIRECTORY", hostile.join(".git/objects"))
        .env(
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            hostile.join(".git/objects"),
        )
        .env("GIT_CEILING_DIRECTORIES", hostile)
        .env("GIT_DISCOVERY_ACROSS_FILESYSTEM", "0")
        .env("GIT_CONFIG_SYSTEM", hostile.join("system.gitconfig"))
        .env("GIT_CONFIG_GLOBAL", hostile.join("global.gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "0")
        .env("GIT_CONFIG", hostile.join("hostile.gitconfig"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.worktree")
        .env("GIT_CONFIG_VALUE_0", hostile)
        .env("GIT_IMPLICIT_WORK_TREE", "0")
        .env("GIT_PREFIX", "hostile")
        .env("GIT_NAMESPACE", "hostile")
        .env("GIT_REPLACE_REF_BASE", "refs/replace-hostile")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_GRAFT_FILE", hostile.join("grafts"))
        .env("GIT_SHALLOW_FILE", hostile.join("shallow"));
}

fn git_state(
    repository: &Repository,
    root: &Path,
) -> (
    Option<orna_repository_v1::GitCommitRef>,
    IndexGeneration,
    WorktreeState,
    String,
    String,
) {
    (
        repository.head().unwrap(),
        repository.index_generation().unwrap(),
        repository.worktree_state().unwrap(),
        git(root, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        git(root, &["config", "--local", "--null", "--list"]),
    )
}

fn with_remote(root: &Path) {
    git(
        root,
        &[
            "remote",
            "add",
            "origin",
            "https://account:secret@example.invalid/private.git",
        ],
    );
    let head = git(root, &["rev-parse", "HEAD"]);
    git(root, &["update-ref", "refs/remotes/origin/main", &head]);
}

fn continuity_remote(root: &Path) -> TempDir {
    let remote = TempDir::new().unwrap();
    git(remote.path(), &["init", "--bare", "."]);
    let remote_path = remote.path().to_str().unwrap();
    git(root, &["remote", "add", "origin", remote_path]);
    git(root, &["push", "origin", "HEAD:refs/heads/main"]);
    remote
}

fn required_internal_ref(object_id: &str) -> RequiredInternalRef {
    RequiredInternalRef::new(
        OrnaInternalRef::new("refs/orna/ids/0123456789abcdef").unwrap(),
        NativeObjectId::new(object_id).unwrap(),
    )
}

fn with_partial_clone(root: &Path) {
    with_remote(root);
    git(root, &["config", "extensions.partialClone", "origin"]);
    git(root, &["config", "remote.origin.promisor", "true"]);
    git(
        root,
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    );
}

fn compact_field_id() -> Uuid {
    Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001)
}

fn compact_schema(table: Uuid) -> SchemaDescriptor {
    compact_schema_for_type(table, compact_field_id(), "Int")
}

fn compact_schema_for_type(table: Uuid, field: Uuid, type_name: &str) -> SchemaDescriptor {
    SchemaDescriptor::new(OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (
            OvbRaw::Int(1.into()),
            OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(table.as_bytes().to_vec()))),
        ),
        (
            OvbRaw::Int(2.into()),
            OvbRaw::Array(vec![OvbRaw::Tag(
                37,
                Box::new(OvbRaw::Bytes(field.as_bytes().to_vec())),
            )]),
        ),
        (
            OvbRaw::Int(3.into()),
            OvbRaw::Array(vec![OvbRaw::Array(vec![
                OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(field.as_bytes().to_vec()))),
                OvbRaw::Text(format!("f_{}", field.simple())),
                OvbRaw::Array(vec![
                    OvbRaw::Int(0.into()),
                    OvbRaw::Text(type_name.to_owned()),
                ]),
                OvbRaw::Int(0.into()),
                OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
            ])]),
        ),
        (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
    ]))
    .unwrap()
}

fn compact_schema_fingerprint(table: Uuid) -> [u8; 32] {
    schema_fingerprint(&compact_schema(table))
}

fn schema_fingerprint(schema: &SchemaDescriptor) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"orna.schema.v1\0");
    digest.update(schema.encode().unwrap());
    digest.finalize().into()
}

/// Builds a real filtered clone when the installed Git supports file-protocol
/// filtering. `None` means the platform cannot provide the fixture; callers
/// must then assert the conservative unavailable boundary rather than claim a
/// promised object from configuration alone.
fn filtered_clone() -> Option<(TempDir, PathBuf, String)> {
    let fixture = TempDir::new().ok()?;
    let origin = fixture.path().join("origin.git");
    let seed = fixture.path().join("seed");
    let clone = fixture.path().join("partial");
    git(fixture.path(), &["init", "--bare", "origin.git"]);
    git(fixture.path(), &["clone", "origin.git", "seed"]);
    test_support::configure_fixture_git_identity(&seed);
    git(&seed, &["config", "commit.gpgsign", "false"]);
    fs::write(seed.join("visible.txt"), "visible\n").ok()?;
    fs::write(seed.join("promised.txt"), "promised\n").ok()?;
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "initial"]);
    git(&seed, &["push", "origin", "HEAD"]);
    git(&origin, &["config", "uploadpack.allowFilter", "true"]);
    let origin_url = format!("file://{}", origin.display());
    let output = Command::new("git")
        .current_dir(fixture.path())
        .args([
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            &origin_url,
            "partial",
        ])
        .output()
        .ok()?;
    if !output.status.success()
        || git(
            &clone,
            &["config", "--local", "--get", "remote.origin.promisor"],
        ) != "true"
    {
        return None;
    }
    let promised = git(&seed, &["rev-parse", "HEAD:promised.txt"]);
    let missing = Command::new("git")
        .current_dir(&clone)
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(["cat-file", "-e", &promised])
        .output()
        .ok()?;
    if missing.status.success() {
        return None;
    }
    Some((fixture, clone, promised))
}

#[test]
fn discovers_head_index_worktree_and_per_worktree_runtime_area() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    assert_eq!(repo.worktree(), root.path());
    assert!(repo.head().unwrap().is_some());
    assert!(repo.index_generation().unwrap().tree().is_some());
    assert!(repo.worktree_state().unwrap().is_clean());
    assert_eq!(
        repo.runtime_paths().root(),
        root.path()
            .join(git(root.path(), &["rev-parse", "--git-path", "orna"]))
    );
    assert!(
        !repo
            .runtime_paths()
            .root()
            .starts_with(root.path().join(".orna"))
    );
    repo.runtime_paths().ensure_exists().unwrap();
    assert!(
        repo.runtime_paths()
            .state_db()
            .starts_with(repo.runtime_paths().root())
    );
    let cwd = repo.cwd_generation(RuntimeGeneration::new(9)).unwrap();
    assert_eq!(cwd.runtime().get(), 9);
}

#[test]
fn hostile_git_routing_child() {
    let Some(root) = std::env::var_os("ORNA_TEST_GIT_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let repo = Repository::discover(&root).unwrap();
    let expected_head = std::env::var("ORNA_TEST_EXPECTED_HEAD").unwrap();
    let expected_index_tree = std::env::var("ORNA_TEST_EXPECTED_INDEX_TREE").unwrap();
    let expected_runtime = PathBuf::from(std::env::var_os("ORNA_TEST_EXPECTED_RUNTIME").unwrap());

    assert_eq!(
        repo.head().unwrap().as_ref().map(|head| head.as_str()),
        Some(expected_head.as_str())
    );
    assert_eq!(
        repo.index_generation()
            .unwrap()
            .tree()
            .map(|tree| tree.as_str()),
        Some(expected_index_tree.as_str())
    );
    assert!(!repo.worktree_state().unwrap().is_clean());
    let cwd = repo.cwd_generation(RuntimeGeneration::new(41)).unwrap();
    assert_eq!(
        cwd.head().map(|head| head.as_str()),
        Some(expected_head.as_str())
    );
    assert_eq!(cwd.branch(), Some("main"));
    assert!(!cwd.worktree().is_clean());
    assert_eq!(repo.runtime_paths().root(), expected_runtime);
}

#[test]
fn hostile_git_routing_cannot_redirect_repository_observations() {
    let root = repository();
    let hostile = repository();
    fs::write(hostile.path().join("hostile-only.txt"), "hostile\n").unwrap();
    git(hostile.path(), &["add", "hostile-only.txt"]);
    git(hostile.path(), &["commit", "-m", "hostile"]);

    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "root staged\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "root unstaged\n").unwrap();
    let expected_head = repo.head().unwrap();
    let expected_index = repo.index_generation().unwrap();
    let expected_runtime = repo.runtime_paths().root().to_owned();

    let expected_head = expected_head.unwrap();
    let expected_index_tree = expected_index.tree().unwrap().clone();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", "hostile_git_routing_child", "--nocapture"]);
    child
        .env("ORNA_TEST_GIT_ROOT", root.path())
        .env("ORNA_TEST_EXPECTED_HEAD", expected_head.as_str())
        .env(
            "ORNA_TEST_EXPECTED_INDEX_TREE",
            expected_index_tree.as_str(),
        )
        .env("ORNA_TEST_EXPECTED_RUNTIME", &expected_runtime);
    configure_hostile_git_environment(&mut child, hostile.path());
    assert!(child.status().unwrap().success());
}

#[test]
fn linked_worktree_gets_its_own_git_resolved_runtime_area() {
    let root = repository();
    let linked = TempDir::new().unwrap();
    let linked_path = linked.path().join("linked");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            linked_path.to_str().unwrap(),
        ],
    );
    let main = Repository::discover(root.path()).unwrap();
    let other = Repository::discover(&linked_path).unwrap();
    assert_ne!(main.runtime_paths().root(), other.runtime_paths().root());
    assert_eq!(
        other.runtime_paths().root(),
        Path::new(&git(&linked_path, &["rev-parse", "--git-path", "orna"]))
    );
}

#[test]
fn managed_staging_preserves_unrelated_index_and_worktree_changes() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join(".orna/format.orna"), "{repository_format: 2, row_profile: \"ORR-2\", blob_profile: \"OGB-1\", semantic_profile: \"SOV-2\", compact_profile: \"compact-storage-v2\"}\n").unwrap();
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    let before = repo.index_generation().unwrap();
    let after = repo
        .stage_managed(&before, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap();
    assert_ne!(before, after);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
    let stale = repo
        .stage_managed(&before, &[ManagedPath::new("main.orna").unwrap()])
        .unwrap_err();
    assert!(matches!(
        stale,
        orna_repository_v1::RepositoryError::StaleIndex { .. }
    ));
    assert!(ManagedPath::new("../ordinary.txt").is_err());
    assert!(ManagedPath::new(".git/config").is_err());
}

#[test]
fn managed_paths_refuse_git_metadata_at_any_depth_and_in_any_case() {
    for hostile in [
        "sub/.git/config",
        "sub/.git/hooks/pre-commit",
        "a/b/.git",
        "SUB/.GIT/config",
        ".GIT/config",
        "a/.Git/x",
        "nested/.gIt",
        "sub/.git./config",
        "sub/.git /config",
        "sub/git~1/config",
    ] {
        assert!(
            matches!(
                ManagedPath::new(hostile),
                Err(orna_repository_v1::RepositoryError::UnsafeManagedPath)
            ),
            "{hostile} must be refused as git metadata"
        );
    }
    assert!(ManagedPath::new("sub/.github/workflows/ci.yml").is_ok());
    assert!(ManagedPath::new("docs/git-notes.md").is_ok());
    assert!(ManagedPath::new("sub/.gitignore").is_ok());
    assert!(ManagedPath::new("sub/foo.git/config").is_ok());
}

#[test]
fn managed_paths_do_not_cap_length_because_git_stores_any_length() {
    let long_component = "a".repeat(300);
    assert!(ManagedPath::new(&long_component).is_ok());
    assert!(ManagedPath::new(format!("docs/{long_component}.md")).is_ok());
}

#[test]
fn managed_paths_refuse_traversal_absolute_nul_backslash_and_empty_components() {
    for hostile in [
        "..",
        "../x",
        "a/../b",
        "/etc/passwd",
        "..\\x",
        "a\\..\\b",
        "a\\b",
        "a\0b",
        "a//b",
        "a/",
        "sub/./",
    ] {
        assert!(
            matches!(
                ManagedPath::new(hostile),
                Err(orna_repository_v1::RepositoryError::UnsafeManagedPath)
            ),
            "{hostile:?} must be refused"
        );
    }
    assert!(ManagedPath::new("./a/b").is_ok());
}

#[test]
fn untracked_git_named_directory_changes_the_status_digest() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::create_dir_all(root.path().join("Sub/.GIT")).unwrap();
    fs::write(root.path().join("Sub/.GIT/notes"), "first\n").unwrap();
    let before = repo.worktree_state().unwrap();
    fs::write(root.path().join("Sub/.GIT/notes"), "second\n").unwrap();
    let after = repo.worktree_state().unwrap();
    assert_ne!(before, after);
}

/// Runs Git with raw bytes on stdin, for objects Git's own tree writers refuse.
fn git_raw(directory: &Path, arguments: &[&str], input: &[u8]) -> String {
    use std::io::Write as _;
    let mut child = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Encodes one raw tree object from `(mode, name, object id)` entries.
fn raw_tree(entries: &[(&str, &str, &str)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (mode, name, object_id) in entries {
        bytes.extend_from_slice(format!("{mode} {name}\0").as_bytes());
        bytes.extend(decode_oid(object_id));
    }
    bytes
}

/// Writes one raw tree object from `(mode, name, object id)` entries.
fn raw_tree_object(root: &Path, entries: &[(&str, &str, &str)]) -> String {
    git_raw(
        root,
        &["hash-object", "-t", "tree", "--literally", "-w", "--stdin"],
        &raw_tree(entries),
    )
}

/// Publishes a commit whose root tree is `tree` on `refs/heads/<branch>`.
fn publish_raw_commit(root: &Path, tree: &str, branch: &str) {
    let commit_body = format!(
        "tree {tree}\nauthor Orna <orna@example.invalid> 1 +0000\n\
         committer Orna <orna@example.invalid> 1 +0000\n\nraw\n"
    );
    let commit_id = git_raw(
        root,
        &["hash-object", "-t", "commit", "--literally", "-w", "--stdin"],
        commit_body.as_bytes(),
    );
    git(root, &["update-ref", &format!("refs/heads/{branch}"), &commit_id]);
}

/// Builds a root tree holding `sub/.git/config`, which Git's own writers refuse.
fn hostile_nested_tree(root: &Path) -> String {
    let blob = git_raw(root, &["hash-object", "-w", "--stdin"], b"[core]\n");
    let gitdir = raw_tree_object(root, &[("100644", "config", &blob)]);
    let sub = raw_tree_object(root, &[("40000", ".git", &gitdir)]);
    raw_tree_object(root, &[("40000", "sub", &sub)])
}

#[test]
fn committed_git_metadata_path_is_refused_on_load() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    publish_raw_commit(root.path(), &hostile_nested_tree(root.path()), "hostile");
    let commit = repo.resolve_snapshot("hostile").unwrap();

    assert!(matches!(
        repo.list_committed_source_tree(&commit),
        Err(orna_repository_v1::RepositoryError::UnsafeManagedPath)
    ));
}

#[test]
fn committed_git_metadata_path_is_refused_on_ls_tree_listing() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    publish_raw_commit(root.path(), &hostile_nested_tree(root.path()), "hostile");
    let commit = repo.resolve_snapshot("hostile").unwrap();

    assert!(matches!(
        repo.list_committed_tree(&commit, 100),
        Err(orna_repository_v1::RepositoryError::UnsafeManagedPath)
    ));
}

#[test]
fn committed_overlong_entries_list_without_a_path_limit() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let blob = git_raw(root.path(), &["hash-object", "-w", "--stdin"], b"long\n");
    // A 303-byte name exceeds NAME_MAX. Sixteen 255-byte components under
    // `deep/` make a 4108-byte path, past the old 4096-byte total cap, while
    // each name stays within the object reader's per-entry bound. The 4108-byte
    // record exceeds `list_tree_entries`' separate 4 KiB record bound, so the
    // ls-tree listing is asserted on a tree that holds only the long name.
    let long_component = format!("{}.md", "a".repeat(300));
    let deep_component = "c".repeat(255);
    let mut deep = raw_tree_object(root.path(), &[("100644", "leaf.txt", &blob)]);
    for _ in 0..16 {
        deep = raw_tree_object(root.path(), &[("40000", &deep_component, &deep)]);
    }
    let docs = raw_tree_object(root.path(), &[("100644", &long_component, &blob)]);
    let tree = raw_tree_object(
        root.path(),
        &[("40000", "docs", &docs), ("40000", "deep", &deep)],
    );
    publish_raw_commit(root.path(), &tree, "overlong");
    let commit = repo.resolve_snapshot("overlong").unwrap();

    let listed = repo.list_committed_source_tree(&commit).unwrap();
    let paths = listed
        .iter()
        .map(|entry| entry.path().as_path().to_path_buf())
        .collect::<Vec<_>>();
    let mut expected_deep = PathBuf::from("deep");
    for _ in 0..16 {
        expected_deep.push(&deep_component);
    }
    expected_deep.push("leaf.txt");
    assert!(paths.contains(&expected_deep));
    assert!(paths.contains(&PathBuf::from("docs").join(&long_component)));
    let name_only = raw_tree_object(root.path(), &[("40000", "docs", &docs)]);
    publish_raw_commit(root.path(), &name_only, "overlong-name");
    let commit = repo.resolve_snapshot("overlong-name").unwrap();
    let listed = repo.list_committed_tree(&commit, 100).unwrap();
    let long_path = PathBuf::from("docs").join(&long_component);
    assert!(
        listed
            .iter()
            .any(|entry| entry.path().as_path() == long_path.as_path()),
        "ls-tree lists a committed name longer than NAME_MAX"
    );
}

#[test]
fn checkout_plan_accepts_user_directories_named_like_git_metadata() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    fs::create_dir_all(root.path().join("Sub/.GIT")).unwrap();
    fs::write(root.path().join("Sub/.GIT/notes"), "user data\n").unwrap();
    fs::create_dir_all(root.path().join(".GIT")).unwrap();
    fs::write(root.path().join(".GIT/a"), "user data\n").unwrap();

    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(25))
        .unwrap();
    assert!(plan.git().conflicting_paths().is_empty());
    let affected = plan
        .git()
        .affected_paths()
        .iter()
        .map(|path| path.as_path().to_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(affected, [".GIT/a", "Sub/.GIT/notes"]);
}

#[test]
fn force_checkout_keeps_user_directories_named_like_git_metadata() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged discard\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::create_dir_all(root.path().join("Sub/.GIT")).unwrap();
    fs::write(root.path().join("Sub/.GIT/notes"), "user data\n").unwrap();
    fs::create_dir_all(root.path().join(".GIT")).unwrap();
    fs::write(root.path().join(".GIT/a"), "user data\n").unwrap();
    // Status observes the user directories as ordinary untracked data.
    repo.worktree_state().unwrap();

    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(43))
        .unwrap();
    // Only the committed target path conflicts, so the leniently listed user
    // names never reach `git restore`.
    let ordinary = ManagedPath::new("ordinary.txt").unwrap();
    assert_eq!(
        plan.git().discardable_paths(),
        std::slice::from_ref(&ordinary)
    );
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();
    repo.execute_validated_force_checkout(&discard).unwrap();

    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "target");
    for user_file in ["Sub/.GIT/notes", ".GIT/a"] {
        assert_eq!(
            fs::read_to_string(root.path().join(user_file)).unwrap(),
            "user data\n",
            "{user_file} stays as the user wrote it"
        );
    }
}

#[test]
fn managed_unstage_and_coordination_lock_preserve_unrelated_staging() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "ordinary staged\n").unwrap();
    fs::write(root.path().join(".orna/format.orna"), "format staged\n").unwrap();
    git(root.path(), &["add", "ordinary.txt", ".orna/format.orna"]);
    let staged = repo.index_generation().unwrap();
    let after = repo
        .unstage_managed(&staged, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap();
    assert_ne!(staged, after);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "ordinary staged"
    );
    assert_eq!(
        git(root.path(), &["show", ":.orna/format.orna"]),
        "{repository_format: 1, storage_profile: \"compact-storage-v1\"}"
    );

    repo.runtime_paths().ensure_exists().unwrap();
    fs::create_dir_all(repo.runtime_paths().locks()).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(repo.runtime_paths().locks().join("coordination.lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    let busy = repo
        .stage_managed(&after, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap_err();
    assert!(matches!(
        busy,
        orna_repository_v1::RepositoryError::RepositoryBusy
    ));
    lock.unlock().unwrap();
    // Advisory locks are held by the owner FD rather than the directory entry:
    // after owner death/close, a new repository operation recovers directly.
    repo.stage_managed(&after, &[ManagedPath::new(".orna/format.orna").unwrap()])
        .unwrap();

    // A normal Git writer owns this filename while it updates `index`; Orna
    // refuses rather than replacing an index under that writer.
    let index = root
        .path()
        .join(git(root.path(), &["rev-parse", "--git-path", "index"]));
    let generation = repo.index_generation().unwrap();
    let git_lock = index.with_extension("lock");
    fs::write(&git_lock, "ordinary Git writer").unwrap();
    assert!(matches!(
        repo.stage_managed(
            &generation,
            &[ManagedPath::new(".orna/format.orna").unwrap()]
        ),
        Err(orna_repository_v1::RepositoryError::GitIndexLockPresent)
    ));
    fs::remove_file(git_lock).unwrap();
}

#[test]
fn snapshot_resolution_accepts_commits_and_rejects_non_commits_and_malformed_ids() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["hash-object", "-w", "ordinary.txt"]);
    let blob = git(root.path(), &["hash-object", "ordinary.txt"]);
    let tree = git(root.path(), &["write-tree"]);
    assert!(repo.resolve_snapshot(&blob).is_err());
    assert!(repo.resolve_snapshot(&tree).is_err());
    assert!(repo.resolve_snapshot("abc").is_err());
    let dangling = git(
        root.path(),
        &["commit-tree", "HEAD^{tree}", "-m", "dangling"],
    );
    assert!(matches!(
        repo.resolve_snapshot(&dangling),
        Err(orna_repository_v1::RepositoryError::SnapshotNotReachable)
    ));
    git(root.path(), &["switch", "--detach", &dangling]);
    assert_eq!(repo.resolve_snapshot("HEAD").unwrap().as_str(), dangling);
}

#[test]
fn exact_committed_oid_resolution_reuses_commit_and_reachability_guards() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let head_bytes = decode_oid(head.as_str());
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.resolve_committed_oid(GitHash::Sha1, &head_bytes)
            .unwrap()
            .as_str(),
        head.as_str()
    );
    assert_eq!(git_state(&repo, root.path()), before);

    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha1, &[0; 20]),
        Err(orna_repository_v1::RepositoryError::SnapshotNotFound)
    ));
    let blob = git(root.path(), &["hash-object", "ordinary.txt"]);
    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha1, &decode_oid(&blob)),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));

    let unreachable = git(
        root.path(),
        &["commit-tree", "HEAD^{tree}", "-m", "unreachable"],
    );
    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha1, &decode_oid(&unreachable)),
        Err(orna_repository_v1::RepositoryError::SnapshotNotReachable)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn exact_committed_oid_resolution_rejects_malformed_and_foreign_formats() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();

    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha1, &[0; 19]),
        Err(orna_repository_v1::RepositoryError::InvalidObjectId)
    ));
    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha256, &[0; 32]),
        Err(orna_repository_v1::RepositoryError::InvalidObjectId)
    ));
}

#[test]
fn exact_committed_oid_resolution_supports_sha256_repositories() {
    let Some(root) = repository_with_object_format("sha256") else {
        return;
    };
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let head_bytes = decode_oid(head.as_str());

    assert_eq!(head_bytes.len(), 32);
    assert_eq!(
        repo.resolve_committed_oid(GitHash::Sha256, &head_bytes)
            .unwrap()
            .as_str(),
        head.as_str()
    );
    assert!(matches!(
        repo.resolve_committed_oid(GitHash::Sha1, &[0; 20]),
        Err(orna_repository_v1::RepositoryError::InvalidObjectId)
    ));
}

#[test]
fn committed_file_reads_are_bounded_and_do_not_mutate_repository_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.read_committed_file(&head, Path::new("ordinary.txt"), 64)
            .unwrap(),
        b"base\n"
    );
    assert_eq!(git_state(&repo, root.path()), before);
    assert!(matches!(
        repo.read_committed_file(&head, Path::new("ordinary.txt"), 4),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    assert!(matches!(
        repo.read_committed_file(&head, Path::new(".orna"), 64),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    assert!(matches!(
        repo.read_committed_file(&head, Path::new("../ordinary.txt"), 64),
        Err(orna_repository_v1::RepositoryError::UnsafeManagedPath)
    ));
    assert!(matches!(
        repo.read_committed_file(&head, Path::new("missing.orna"), 64),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn committed_tree_listing_is_bounded_and_does_not_mutate_repository_state() {
    let root = repository();
    fs::create_dir_all(root.path().join("nested")).unwrap();
    fs::write(
        root.path().join("nested/tool.orna"),
        include_str!("fixtures/git-repository-nested-tool.orna"),
    )
    .unwrap();
    git(root.path(), &["add", "nested/tool.orna"]);
    git(root.path(), &["commit", "-m", "add nested source"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());

    let entries = repo.list_committed_tree(&head, 8).unwrap();
    assert_eq!(git_state(&repo, root.path()), before);
    assert!(entries.iter().any(|entry| {
        entry.path().as_path() == Path::new("main.orna")
            && entry.kind() == CommittedTreeEntryKind::File { executable: false }
    }));
    assert!(entries.iter().any(|entry| {
        entry.path().as_path() == Path::new("nested/tool.orna")
            && entry.kind() == CommittedTreeEntryKind::File { executable: false }
    }));
    assert!(matches!(
        repo.list_committed_tree(&head, entries.len().saturating_sub(1)),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[cfg(unix)]
#[test]
fn committed_tree_listing_classifies_symlinks_without_following_them() {
    let root = repository();
    std::os::unix::fs::symlink("main.orna", root.path().join("linked.orna")).unwrap();
    git(root.path(), &["add", "linked.orna"]);
    git(root.path(), &["commit", "-m", "add source symlink"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());

    let entries = repo.list_committed_tree(&head, 8).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.path().as_path() == Path::new("linked.orna")
            && entry.kind() == CommittedTreeEntryKind::Symlink
    }));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn named_branch_snapshot_resolution_stays_pinned_after_branch_moves() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "published"]);

    // ORNA-SYS-106/139: resolving a named selector returns the immutable
    // commit it named at resolution time, rather than a live branch handle.
    let first = repo.resolve_snapshot("published").unwrap();
    let initial = git(root.path(), &["rev-parse", "published"]);
    assert_eq!(first.as_str(), initial);

    git(root.path(), &["switch", "published"]);
    fs::write(root.path().join("ordinary.txt"), "published advance\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "advance published"]);
    let advanced = git(root.path(), &["rev-parse", "published"]);

    // The first snapshot stays pinned; a later selector resolution observes
    // the branch's new immutable commit.
    assert_eq!(first.as_str(), initial);
    let second = repo.resolve_snapshot("published").unwrap();
    assert_eq!(second.as_str(), advanced);
    assert_ne!(first, second);
}

#[test]
fn checkout_preflight_classifies_a_local_branch_without_mutating_cwd() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(17)).unwrap();

    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(17))
        .unwrap();

    assert_eq!(plan.cwd(), &before);
    assert_eq!(plan.expected_head(), before.head());
    assert_eq!(plan.target().branch_name(), Some("experiment"));
    assert_eq!(plan.target().commit(), before.head().unwrap());
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(17)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
}

#[test]
fn checkout_preflight_classifies_a_commit_as_detached_and_rejects_ambiguity() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let before = repo.cwd_generation(RuntimeGeneration::new(18)).unwrap();

    let plan = repo
        .plan_checkout(&head, RuntimeGeneration::new(18))
        .unwrap();
    assert!(matches!(plan.target(), CheckoutTarget::Detached { .. }));
    assert_eq!(plan.target().commit().as_str(), head);
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(18)).unwrap(),
        before
    );

    git(root.path(), &["tag", "main"]);
    let before_ambiguous = repo.cwd_generation(RuntimeGeneration::new(18)).unwrap();
    assert!(matches!(
        repo.plan_checkout("main", RuntimeGeneration::new(18)),
        Err(orna_repository_v1::RepositoryError::InvalidSelector)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(18)).unwrap(),
        before_ambiguous
    );
}

#[test]
fn checkout_preflight_rejects_local_branch_ambiguous_with_remote_tracking_ref() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "origin/topic"]);
    git(
        root.path(),
        &["update-ref", "refs/remotes/origin/topic", "HEAD"],
    );
    let runtime = RuntimeGeneration::new(182);
    let before = git_state(&repo, root.path());
    let before_cwd = repo.cwd_generation(runtime).unwrap();

    assert!(matches!(
        repo.plan_checkout("origin/topic", runtime),
        Err(orna_repository_v1::RepositoryError::InvalidSelector)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(repo.cwd_generation(runtime).unwrap(), before_cwd);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
}

#[test]
fn checkout_rejects_a_full_commit_id_branch_name_without_mutating_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    git(root.path(), &["branch", &head]);
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("local.txt"), "unstaged local\n").unwrap();
    let runtime = RuntimeGeneration::new(181);
    let before = git_state(&repo, root.path());
    let before_cwd = repo.cwd_generation(runtime).unwrap();

    assert!(matches!(
        repo.plan_checkout(&head, runtime),
        Err(orna_repository_v1::RepositoryError::InvalidSelector)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(repo.cwd_generation(runtime).unwrap(), before_cwd);
}

#[test]
/// ORNA-CHECKOUT-001: invalid/stale checkout plans leave accepted state and drafts unchanged.
fn checkout_preflight_invalid_selector_is_strictly_non_mutating() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(19)).unwrap();
    assert!(matches!(
        repo.plan_checkout("-force", RuntimeGeneration::new(19)),
        Err(orna_repository_v1::RepositoryError::InvalidSelector)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(19)).unwrap(),
        before
    );
}

#[test]
fn checkout_preflight_revalidation_accepts_unchanged_state_and_rejects_worktree_drift() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(20))
        .unwrap();
    assert!(repo.verify_checkout_preflight(&plan).is_ok());

    fs::write(root.path().join("main.orna"), "changed after planning\n").unwrap();
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
}
#[cfg(unix)]
#[test]
fn checkout_force_revalidation_rejects_executable_bit_drift() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target mode"]);
    git(root.path(), &["switch", "main"]);
    git(root.path(), &["config", "core.filemode", "false"]);

    fs::write(root.path().join("ordinary.txt"), "local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(20))
        .unwrap();
    let token = plan.force_token();

    let mut permissions = fs::metadata(root.path().join("ordinary.txt"))
        .unwrap()
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(root.path().join("ordinary.txt"), permissions).unwrap();
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.authorize_checkout_force(&plan, true, Some(&token)),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn checkout_preflight_revalidation_rejects_index_head_and_branch_tip_drift() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(21))
        .unwrap();

    fs::write(root.path().join("ordinary.txt"), "index drift\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));

    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(22))
        .unwrap();
    git(
        root.path(),
        &["commit", "--allow-empty", "-m", "head drift"],
    );
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));

    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(23))
        .unwrap();
    let next = git(
        root.path(),
        &[
            "commit-tree",
            "HEAD^{tree}",
            "-p",
            "HEAD",
            "-m",
            "tip drift",
        ],
    );
    git(
        root.path(),
        &[
            "update-ref",
            "refs/heads/experiment",
            &next,
            &git(root.path(), &["rev-parse", "experiment"]),
        ],
    );
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
}

#[test]
fn checkout_preflight_rejects_same_commit_detached_attachment_drift_without_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(24))
        .unwrap();

    let target = git(root.path(), &["rev-parse", "HEAD"]);
    git(root.path(), &["switch", "--detach", &target]);
    let before_verification = repo.cwd_generation(RuntimeGeneration::new(24)).unwrap();

    // CHECKOUT-1 steps 1 and 6 / ORNA-BRANCH-002: attachment is part of
    // the state-bound preflight even where the selected commit is unchanged.
    assert!(matches!(
        repo.verify_checkout_preflight(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));

    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(24)).unwrap(),
        before_verification
    );
    assert!(git(root.path(), &["branch", "--show-current"]).is_empty());
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]), target);
}

#[test]
fn checkout_force_authorization_is_canonical_and_stale_state_is_rejected() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    let branch = repo
        .plan_checkout("experiment", RuntimeGeneration::new(24))
        .unwrap();
    let unchanged = repo
        .plan_checkout("experiment", RuntimeGeneration::new(24))
        .unwrap();
    assert_eq!(branch.force_token(), unchanged.force_token());
    assert!(
        repo.authorize_checkout_force(&branch, true, Some(&branch.force_token()))
            .is_ok()
    );
    assert!(matches!(
        repo.authorize_checkout_force(&branch, false, Some(&branch.force_token())),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert!(matches!(
        repo.authorize_checkout_force(&branch, true, None),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));

    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let detached = repo
        .plan_checkout(&head, RuntimeGeneration::new(24))
        .unwrap();
    assert_ne!(branch.force_token(), detached.force_token());

    fs::write(root.path().join("main.orna"), "changed after planning\n").unwrap();
    assert!(matches!(
        repo.authorize_checkout_force(&branch, true, Some(&branch.force_token())),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
}

#[test]
fn checkout_force_authorization_rejects_changed_bytes_for_an_ignored_path() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join(".gitignore"), "local-cache/\n").unwrap();
    git(root.path(), &["add", ".gitignore"]);
    git(root.path(), &["commit", "-m", "ignore local cache"]);
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    let ignored_path = root.path().join("local-cache").join("keep.bin");
    fs::create_dir_all(ignored_path.parent().unwrap()).unwrap();
    fs::write(&ignored_path, b"ignored before\0\xff\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(42))
        .unwrap();
    let token = plan.force_token();
    let status_before = git(root.path(), &["status", "--porcelain=v2"]);

    // Ignored bytes are absent from ordinary status and checkout path sets,
    // but remain part of the dirty worktree that a force plan must fence.
    fs::write(&ignored_path, b"ignored after\0\xfe\n").unwrap();

    assert_eq!(
        git(root.path(), &["status", "--porcelain=v2"]),
        status_before
    );
    assert!(matches!(
        repo.authorize_checkout_force(&plan, true, Some(&token)),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
}

#[test]
/// ORNA-BRANCH-002: same-commit branch selection preserves the worktree and staged index.
fn same_commit_checkout_switches_attachment_without_discarding_local_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(26))
        .unwrap();

    repo.execute_same_commit_checkout(&plan).unwrap();

    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    let detached = repo
        .plan_checkout(
            &git(root.path(), &["rev-parse", "HEAD"]),
            RuntimeGeneration::new(26),
        )
        .unwrap();
    repo.execute_same_commit_checkout(&detached).unwrap();
    assert!(git(root.path(), &["branch", "--show-current"]).is_empty());
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn same_commit_checkout_rejects_a_divergent_target_without_mutating_cwd() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);
    let before = repo.cwd_generation(RuntimeGeneration::new(27)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(27))
        .unwrap();

    assert!(matches!(
        repo.execute_same_commit_checkout(&plan),
        Err(orna_repository_v1::RepositoryError::CheckoutExecutionUnsafe)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(27)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
}


fn create_format3_checkout_target(root: &Path, with_store_root: bool) {
    git(root, &["switch", "-c", "format3-target"]);
    fs::remove_file(root.join(".orna/format.orna")).unwrap();
    fs::write(
        root.join(".orna/database.orna"),
        "{\n    repository_format: 3,\n    database_id: \"00000000-0000-4000-8000-000000000001\",\n}\n",
    )
    .unwrap();
    if with_store_root {
        fs::create_dir_all(root.join(".orna/store")).unwrap();
        fs::write(root.join(".orna/store/root"), b"format-3 store root\n").unwrap();
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", "format-3 checkout target"]);
    git(root, &["switch", "main"]);
}

#[test]
fn divergent_checkout_rejects_format3_target_without_a_store_root() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    create_format3_checkout_target(root.path(), false);
    let runtime = RuntimeGeneration::new(91);
    let before = repo.cwd_generation(runtime).unwrap();
    let plan = repo.plan_checkout("format3-target", runtime).unwrap();

    assert!(matches!(
        repo.execute_nonconflicting_git_checkout(&plan, &AcceptCheckout),
        Err(orna_repository_v1::RepositoryError::CheckoutTargetInvalid)
    ));
    assert_eq!(repo.cwd_generation(runtime).unwrap(), before);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
}

#[test]
fn divergent_checkout_accepts_a_format3_target_with_source_and_store_roots() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    create_format3_checkout_target(root.path(), true);
    let runtime = RuntimeGeneration::new(92);
    let plan = repo.plan_checkout("format3-target", runtime).unwrap();

    repo.execute_nonconflicting_git_checkout(&plan, &AcceptCheckout).unwrap();

    assert_eq!(repo.head().unwrap().as_ref(), Some(plan.target().commit()));
    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "format3-target"
    );
    assert!(root.path().join(".orna/database.orna").is_file());
    assert!(root.path().join(".orna/store/root").is_file());
}

#[test]
fn divergent_checkout_logical_validation_rejection_fences_git_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("target.orna"), "target source\n").unwrap();
    git(root.path(), &["add", "target.orna"]);
    git(root.path(), &["commit", "-m", "target source"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(31)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(31))
        .unwrap();

    assert!(matches!(
        repo.execute_nonconflicting_git_checkout(&plan, &CheckoutGate(&|repository, observed| {
            assert_eq!(repository.head().unwrap(), observed.expected_head().cloned());
            assert_eq!(observed.target().branch_name(), Some("experiment"));
            Err(orna_repository_v1::CandidateAssertionRefusal::AssertionFalse(None))
        })),
        Err(orna_repository_v1::RepositoryError::CandidateAssertionRefused(
            orna_repository_v1::CandidateAssertionRefusal::AssertionFalse(None)
        ))
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(31)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged source\n"
    );
    assert!(!root.path().join("target.orna").exists());
}

#[test]
fn divergent_checkout_rechecks_git_state_after_candidate_validation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("target.orna"), "target source\n").unwrap();
    git(root.path(), &["add", "target.orna"]);
    git(root.path(), &["commit", "-m", "target source"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(42))
        .unwrap();
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.execute_nonconflicting_git_checkout(&plan, &CheckoutGate(&|_, _| {
            fs::write(root.path().join("main.orna"), "changed during validation\n").unwrap();
            Ok(())
        })),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert_eq!(git_state(&repo, root.path()).0, before.0);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged ordinary"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "changed during validation\n"
    );
    assert!(!root.path().join("target.orna").exists());
}

#[test]
fn divergent_checkout_refuses_to_overwrite_an_untracked_target_path() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("new.orna"), "target source\n").unwrap();
    git(root.path(), &["add", "new.orna"]);
    git(root.path(), &["commit", "-m", "target source"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("new.orna"), "untracked local\n").unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(33)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(33))
        .unwrap();

    assert_eq!(
        plan.git().conflicting_paths(),
        &[ManagedPath::new("new.orna").unwrap()]
    );
    assert!(matches!(
        repo.execute_nonconflicting_git_checkout(&plan, &AcceptCheckout),
        Err(orna_repository_v1::RepositoryError::CheckoutExecutionUnsafe)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(33)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        fs::read_to_string(root.path().join("new.orna")).unwrap(),
        "untracked local\n"
    );
}

#[test]
fn divergent_checkout_refuses_to_overwrite_an_ignored_target_path() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join(".gitignore"), "new.orna\n").unwrap();
    git(root.path(), &["add", ".gitignore"]);
    git(root.path(), &["commit", "-m", "ignore local source"]);
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("new.orna"), "target source\n").unwrap();
    git(root.path(), &["add", "-f", "new.orna"]);
    git(root.path(), &["commit", "-m", "target source"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("new.orna"), "ignored local\n").unwrap();
    let before = repo.cwd_generation(RuntimeGeneration::new(38)).unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(38))
        .unwrap();

    assert_eq!(
        plan.git().conflicting_paths(),
        &[ManagedPath::new("new.orna").unwrap()]
    );
    assert!(matches!(
        repo.execute_nonconflicting_git_checkout(&plan, &AcceptCheckout),
        Err(orna_repository_v1::RepositoryError::CheckoutExecutionUnsafe)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(38)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(
        fs::read_to_string(root.path().join("new.orna")).unwrap(),
        "ignored local\n"
    );
}

#[test]
fn checkout_subplan_classifies_target_and_local_path_sets() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(25))
        .unwrap();
    let ordinary = ManagedPath::new("ordinary.txt").unwrap();
    assert_eq!(plan.git().affected_paths(), std::slice::from_ref(&ordinary));
    assert_eq!(
        plan.git().conflicting_paths(),
        std::slice::from_ref(&ordinary)
    );
    assert_eq!(
        plan.git().discardable_paths(),
        std::slice::from_ref(&ordinary)
    );

    fs::write(root.path().join("main.orna"), "carried\n").unwrap();
    let carried = repo
        .plan_checkout("experiment", RuntimeGeneration::new(25))
        .unwrap();
    assert_eq!(
        carried.git().affected_paths(),
        &[
            ManagedPath::new("main.orna").unwrap(),
            ManagedPath::new("ordinary.txt").unwrap()
        ]
    );
    assert_eq!(
        carried.git().conflicting_paths(),
        plan.git().conflicting_paths()
    );
    assert_ne!(carried.force_token(), plan.force_token());
}

#[test]
fn checkout_discard_set_requires_the_canonical_force_witness_and_exact_paths() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged local\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(28))
        .unwrap();
    let token = plan.force_token();
    let before = repo.cwd_generation(RuntimeGeneration::new(28)).unwrap();

    let _validated = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(28)).unwrap(),
        before
    );

    assert!(matches!(
        repo.validate_checkout_discard_set(&plan, true, Some(&token), &[], &AcceptCheckout),
        Err(orna_repository_v1::RepositoryError::CheckoutDiscardSetMismatch)
    ));
    assert!(matches!(
        repo.validate_checkout_discard_set(
            &plan,
            false,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        ),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(28)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged local\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn checkout_discard_set_rejects_a_force_plan_after_target_branch_drift() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(34))
        .unwrap();
    let token = plan.force_token();
    let target = git(root.path(), &["rev-parse", "experiment"]);
    let successor = git(
        root.path(),
        &[
            "commit-tree",
            "experiment^{tree}",
            "-p",
            "experiment",
            "-m",
            "target branch moved after planning",
        ],
    );
    git(
        root.path(),
        &["update-ref", "refs/heads/experiment", &successor, &target],
    );
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        ),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
}

#[test]
fn admitted_force_discard_is_fenced_against_later_target_drift() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(35))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();

    let target = git(root.path(), &["rev-parse", "experiment"]);
    let successor = git(
        root.path(),
        &[
            "commit-tree",
            "experiment^{tree}",
            "-p",
            "experiment",
            "-m",
            "target branch moved after admission",
        ],
    );
    git(
        root.path(),
        &["update-ref", "refs/heads/experiment", &successor, &target],
    );
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.verify_validated_checkout_discard(&discard),
        Err(orna_repository_v1::RepositoryError::CheckoutPlanStale)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
}

#[test]
fn pre_execution_force_checkout_recovery_retains_drifted_intent_without_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(36))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();
    assert!(repo.has_pending_pre_execution_checkout().unwrap());

    let restarted = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("main.orna"), "interleaved source\n").unwrap();
    let before = git_state(&restarted, root.path());
    assert!(matches!(
        restarted.recover_pre_execution_checkout(),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert_eq!(git_state(&restarted, root.path()), before);
    assert!(restarted.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");

    fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
    restarted.recover_pre_execution_checkout().unwrap();
    assert!(!restarted.has_pending_pre_execution_checkout().unwrap());
}

#[test]
fn pending_force_checkout_recovery_fences_other_git_visible_checkout_executors() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);
    git(root.path(), &["branch", "same-commit"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let force_plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(37))
        .unwrap();
    let force_token = force_plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &force_plan,
            true,
            Some(&force_token),
            force_plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let safe_plan = repo
        .plan_checkout("same-commit", RuntimeGeneration::new(37))
        .unwrap();
    let before = git_state(&repo, root.path());
    assert!(matches!(
        repo.execute_same_commit_checkout(&safe_plan),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
    assert!(repo.has_pending_pre_execution_checkout().unwrap());
}

#[test]
fn restarted_force_discard_recovery_requires_the_recorded_canonical_witness() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);
    git(root.path(), &["branch", "same-commit"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(38))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let restarted = Repository::discover(root.path()).unwrap();
    let unrelated = restarted
        .plan_checkout("same-commit", RuntimeGeneration::new(38))
        .unwrap();
    let before_unrelated = git_state(&restarted, root.path());
    assert!(matches!(
        restarted.execute_same_commit_checkout(&unrelated),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert_eq!(git_state(&restarted, root.path()), before_unrelated);
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");

    fs::write(root.path().join("ordinary.txt"), "stale local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let stale = restarted
        .plan_checkout("experiment", RuntimeGeneration::new(38))
        .unwrap();
    assert_ne!(stale.force_token(), token);
    let stale_discard = restarted
        .validate_checkout_discard_set(
            &stale,
            true,
            Some(&stale.force_token()),
            stale.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    let before_stale = git_state(&restarted, root.path());
    assert!(matches!(
        restarted.persist_validated_checkout_discard(&stale_discard),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(matches!(
        restarted.recover_pre_execution_checkout(),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert_eq!(git_state(&restarted, root.path()), before_stale);
    assert_eq!(
        fs::read_to_string(root.path().join("ordinary.txt")).unwrap(),
        "stale local\n"
    );
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "stale local");
    assert!(restarted.has_pending_pre_execution_checkout().unwrap());

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    restarted.recover_pre_execution_checkout().unwrap();
    assert!(!restarted.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
}

#[test]
fn force_checkout_discards_only_the_consented_paths_and_carries_unrelated_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged discard\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "staged preserve\n").unwrap();
    git(root.path(), &["add", "main.orna"]);
    fs::write(root.path().join("main.orna"), "unstaged preserve\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked preserve\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(39))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();
    repo.execute_validated_force_checkout(&discard).unwrap();
    assert!(!repo.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "target");
    assert_eq!(git(root.path(), &["show", ":main.orna"]), "staged preserve");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged preserve\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked preserve\n"
    );
}

#[test]
fn interrupted_force_checkout_after_discard_journal_recovers_recorded_target() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged discard\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "staged preserve\n").unwrap();
    git(root.path(), &["add", "main.orna"]);
    fs::write(root.path().join("main.orna"), "unstaged preserve\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked preserve\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(41))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let mut interrupt = || Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired);
    assert!(matches!(
        repo.execute_validated_force_checkout_after_discard_with_test_hook(
            &discard,
            &mut interrupt
        ),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(repo.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");

    let restarted = Repository::discover(root.path()).unwrap();
    restarted.recover_pre_execution_checkout().unwrap();
    assert!(!restarted.has_pending_pre_execution_checkout().unwrap());
    assert!(
        !restarted
            .runtime_paths()
            .root()
            .join("checkout-journal.bin")
            .exists()
    );
    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "target");
    assert_eq!(git(root.path(), &["show", ":main.orna"]), "staged preserve");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged preserve\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked preserve\n"
    );
}


#[test]
fn interrupted_force_checkout_after_discard_journal_recovers_detached_target() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    let target = git(root.path(), &["rev-parse", "HEAD"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged discard\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "staged preserve\n").unwrap();
    git(root.path(), &["add", "main.orna"]);
    fs::write(root.path().join("main.orna"), "unstaged preserve\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked preserve\n").unwrap();
    let plan = repo
        .plan_checkout(&target, RuntimeGeneration::new(42))
        .unwrap();
    assert!(matches!(
        plan.target(),
        CheckoutTarget::Detached { commit } if commit.as_str() == target
    ));
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let mut interrupt = || Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired);
    assert!(matches!(
        repo.execute_validated_force_checkout_after_discard_with_test_hook(
            &discard,
            &mut interrupt
        ),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(repo.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");

    let restarted = Repository::discover(root.path()).unwrap();
    restarted.recover_pre_execution_checkout().unwrap();
    assert!(!restarted.has_pending_pre_execution_checkout().unwrap());
    assert!(
        !restarted
            .runtime_paths()
            .root()
            .join("checkout-journal.bin")
            .exists()
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "");
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]), target);
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "target");
    assert_eq!(
        fs::read_to_string(root.path().join("ordinary.txt")).unwrap(),
        "target\n"
    );
    assert_eq!(git(root.path(), &["show", ":main.orna"]), "staged preserve");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged preserve\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked preserve\n"
    );
}

#[test]
fn interrupted_force_checkout_after_git_switch_recovers_selected_target() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join(".gitignore"), "local-cache/\n").unwrap();
    git(root.path(), &["add", ".gitignore"]);
    git(root.path(), &["commit", "-m", "ignore local cache"]);
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged discard\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "staged preserve\n").unwrap();
    git(root.path(), &["add", "main.orna"]);
    fs::write(root.path().join("main.orna"), "unstaged preserve\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked preserve\n").unwrap();
    let ignored_path = root.path().join("local-cache").join("keep.bin");
    let ignored_bytes = b"ignored local bytes\0\xff\n".to_vec();
    fs::create_dir_all(ignored_path.parent().unwrap()).unwrap();
    fs::write(&ignored_path, &ignored_bytes).unwrap();
    git(root.path(), &["check-ignore", "-q", "local-cache/keep.bin"]);
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(40))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let mut interrupt = || Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired);
    assert!(matches!(
        repo.execute_validated_force_checkout_with_test_hook(&discard, &mut interrupt),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(repo.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );

    let restarted = Repository::discover(root.path()).unwrap();
    restarted.recover_pre_execution_checkout().unwrap();
    assert!(!restarted.has_pending_pre_execution_checkout().unwrap());
    assert_eq!(
        git(root.path(), &["branch", "--show-current"]),
        "experiment"
    );
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "target");
    assert_eq!(git(root.path(), &["show", ":main.orna"]), "staged preserve");
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged preserve\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked preserve\n"
    );
    assert_eq!(fs::read(ignored_path).unwrap(), ignored_bytes);
}

#[test]
fn malformed_discarded_checkout_journal_attachment_fails_before_recovery_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "beta"]);
    git(root.path(), &["switch", "beta"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged discard\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let plan = repo
        .plan_checkout("beta", RuntimeGeneration::new(41))
        .unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();

    let mut interrupt = || Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired);
    assert!(matches!(
        repo.execute_validated_force_checkout_with_test_hook(&discard, &mut interrupt),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));

    let journal_path = repo.runtime_paths().root().join("checkout-journal.bin");
    let mut journal = fs::read(&journal_path).unwrap();
    let discarded_branch = journal
        .windows(b"main".len())
        .rposition(|window| window == b"main")
        .expect("discarded branch is journalled");
    journal[discarded_branch..discarded_branch + b"main".len()].copy_from_slice(b"beta");
    fs::write(&journal_path, &journal).unwrap();

    let restarted = Repository::discover(root.path()).unwrap();
    let before = git_state(&restarted, root.path());
    assert!(matches!(
        restarted.recover_pre_execution_checkout(),
        Err(orna_repository_v1::RepositoryError::InvalidCheckoutJournal)
    ));
    assert_eq!(git_state(&restarted, root.path()), before);
    assert_eq!(fs::read(&journal_path).unwrap(), journal);
}

#[test]
fn checkout_discard_set_logical_validation_rejection_fences_force_admission() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);

    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("untracked.txt"), "untracked\n").unwrap();
    let plan = repo
        .plan_checkout("experiment", RuntimeGeneration::new(32))
        .unwrap();
    let token = plan.force_token();
    let before = repo.cwd_generation(RuntimeGeneration::new(32)).unwrap();

    assert!(matches!(
        repo.validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &CheckoutGate(&|repository, observed| {
                assert_eq!(repository.head().unwrap(), observed.expected_head().cloned());
                assert_eq!(observed.target().branch_name(), Some("experiment"));
                Err(orna_repository_v1::CandidateAssertionRefusal::AssertionFalse(None))
            }),
        ),
        Err(orna_repository_v1::RepositoryError::CandidateAssertionRefused(
            orna_repository_v1::CandidateAssertionRefusal::AssertionFalse(None)
        ))
    ));
    assert_eq!(
        repo.cwd_generation(RuntimeGeneration::new(32)).unwrap(),
        before
    );
    assert_eq!(git(root.path(), &["branch", "--show-current"]), "main");
    assert_eq!(git(root.path(), &["show", ":ordinary.txt"]), "staged local");
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn verify_cwd_rejects_a_worktree_only_interleaving() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let observed = repo.cwd_generation(RuntimeGeneration::new(7)).unwrap();
    fs::write(root.path().join("main.orna"), "changed only in worktree\n").unwrap();
    assert!(matches!(
        repo.verify_cwd(&observed),
        Err(orna_repository_v1::RepositoryError::StaleCwd)
    ));
}

#[test]
fn ordinary_git_add_interleavings_make_stage_and_unstage_stale() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new(".orna/format.orna").unwrap();
    fs::write(root.path().join(".orna/format.orna"), "candidate stage\n").unwrap();
    let expected_stage = repo.index_generation().unwrap();
    let mut stage_race = || {
        fs::write(root.path().join("ordinary.txt"), "ordinary race one\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
    };
    assert!(matches!(
        repo.stage_managed_with_test_hook(
            &expected_stage,
            std::slice::from_ref(&managed),
            &mut stage_race
        ),
        Err(orna_repository_v1::RepositoryError::StaleIndex { .. })
    ));
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "ordinary race one"
    );
    assert_eq!(
        git(root.path(), &["show", ":.orna/format.orna"]),
        "{repository_format: 1, storage_profile: \"compact-storage-v1\"}"
    );

    let expected_unstage = repo.index_generation().unwrap();
    let mut unstage_race = || {
        fs::write(root.path().join("ordinary.txt"), "ordinary race two\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
    };
    assert!(matches!(
        repo.unstage_managed_with_test_hook(
            &expected_unstage,
            std::slice::from_ref(&managed),
            &mut unstage_race
        ),
        Err(orna_repository_v1::RepositoryError::StaleIndex { .. })
    ));
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "ordinary race two"
    );
}

#[test]
fn conflicted_index_fails_closed_and_errors_redact_local_paths() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["switch", "-c", "other"]);
    fs::write(root.path().join("ordinary.txt"), "other\n").unwrap();
    git(root.path(), &["commit", "-am", "other"]);
    git(root.path(), &["switch", "main"]);
    fs::write(root.path().join("ordinary.txt"), "main\n").unwrap();
    git(root.path(), &["commit", "-am", "main"]);
    let output = Command::new("git")
        .current_dir(root.path())
        .args(["merge", "other"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(matches!(
        repo.index_generation(),
        Err(orna_repository_v1::RepositoryError::GitOperationFailed)
    ));
    let error = Repository::discover(root.path().join("missing").join("file")).unwrap_err();
    let rendered = format!("{error:?} {error}");
    assert!(!rendered.contains(&root.path().display().to_string()));
}


#[test]
fn create_branch_at_unborn_head_fails_without_mutating_repository_state() {
    let root = TempDir::new().unwrap();
    git(root.path(), &["init", "-b", "main"]);

    fs::create_dir_all(root.path().join(".orna")).unwrap();
    fs::write(root.path().join(".orna/format.orna"), "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n").unwrap();
    fs::write(root.path().join("ordinary.txt"), "untracked\n").unwrap();
    git(root.path(), &["add", ".orna/format.orna"]);

    let repo = Repository::discover(root.path()).unwrap();
    let head_before = git(root.path(), &["symbolic-ref", "HEAD"]);
    let refs_before = git(
        root.path(),
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let index_before = fs::read(root.path().join(".git/index")).unwrap();
    let status_before = git(
        root.path(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let staged_source_before = fs::read(root.path().join(".orna/format.orna")).unwrap();
    let untracked_file_before = fs::read(root.path().join("ordinary.txt")).unwrap();

    assert!(matches!(
        repo.create_branch_at_head("experiment"),
        Err(orna_repository_v1::RepositoryError::UnbornHead)
    ));

    assert_eq!(git(root.path(), &["symbolic-ref", "HEAD"]), head_before);
    assert_eq!(
        git(
            root.path(),
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        refs_before
    );
    assert_eq!(
        fs::read(root.path().join(".git/index")).unwrap(),
        index_before
    );
    assert_eq!(
        git(
            root.path(),
            &["status", "--porcelain=v1", "--untracked-files=all"],
        ),
        status_before
    );
    assert_eq!(
        fs::read(root.path().join(".orna/format.orna")).unwrap(),
        staged_source_before
    );
    assert_eq!(
        fs::read(root.path().join("ordinary.txt")).unwrap(),
        untracked_file_before
    );
}

#[test]
fn observes_an_ordinary_repository_and_materialized_head_without_mutation() {
    let root = repository();
    with_remote(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    let capabilities = repo.observe_git_capabilities().unwrap();
    assert_eq!(capabilities.mode(), GitRepositoryMode::Ordinary);
    assert!(!capabilities.sparse_checkout());
    assert!(!capabilities.partial_clone());
    assert!(capabilities.promisor_remotes().is_empty());
    assert!(format!("{capabilities:?}").contains("Ordinary"));
    assert!(!format!("{capabilities:?}").contains("example.invalid"));
    assert!(!format!("{capabilities:?}").contains("account"));
    assert!(!format!("{capabilities:?}").contains("secret"));
    assert!(!format!("{capabilities:?}").contains(&root.path().display().to_string()));

    let head = git(root.path(), &["rev-parse", "HEAD"]);
    assert!(matches!(
        repo.observe_git_object(&head).unwrap(),
        GitObjectState::Materialized {
            kind: GitObjectKind::Commit,
            size: 1..
        }
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn remote_continuity_reports_missing_internal_refs_without_mutation() {
    let root = repository();
    let _remote = continuity_remote(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_remote_continuity("origin", &[required_internal_ref(&head)]),
        RemoteContinuity::Missing
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn local_internal_and_tracking_refs_cannot_substitute_remote_continuity() {
    let root = repository();
    let _remote = continuity_remote(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let required = required_internal_ref(&head);

    // These locally materialized names can be stale after a plain-Git transfer.
    // They are not evidence that the configured remote retained Orna continuity.
    git(
        root.path(),
        &["update-ref", "refs/orna/ids/0123456789abcdef", &head],
    );
    git(
        root.path(),
        &[
            "update-ref",
            "refs/remotes/origin/orna/ids/0123456789abcdef",
            &head,
        ],
    );
    let before = git_state(&repo, root.path());

    let continuity = repo.observe_remote_continuity("origin", std::slice::from_ref(&required));

    assert_eq!(continuity, RemoteContinuity::Missing);
    assert!(!continuity.permits_continuity_claim());
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn remote_continuity_reports_matching_and_stale_internal_refs_without_mutation() {
    let root = repository();
    let remote = continuity_remote(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let expected = head.to_ascii_uppercase();
    let required = required_internal_ref(&expected);
    git(
        remote.path(),
        &["update-ref", "refs/orna/ids/0123456789abcdef", &head],
    );
    git(
        remote.path(),
        &["update-ref", "refs/orna/ids/unrelated", &head],
    );
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_remote_continuity("origin", std::slice::from_ref(&required)),
        RemoteContinuity::Continuous
    );
    assert_eq!(git_state(&repo, root.path()), before);
    let tree = git(root.path(), &["rev-parse", "HEAD^{tree}"]);
    let stale = git(
        root.path(),
        &["commit-tree", &tree, "-p", &head, "-m", "stale continuity"],
    );
    let refspec = format!("{stale}:refs/heads/stale-continuity");
    git(root.path(), &["push", "origin", &refspec]);
    git(
        remote.path(),
        &["update-ref", "refs/orna/ids/0123456789abcdef", &stale],
    );
    let stale_before = git_state(&repo, root.path());
    assert_eq!(
        repo.observe_remote_continuity("origin", std::slice::from_ref(&required)),
        RemoteContinuity::Stale
    );
    assert_eq!(git_state(&repo, root.path()), stale_before);
}

#[test]
fn remote_continuity_fails_closed_for_invalid_or_ambiguous_input_and_remote_failure() {
    let root = repository();
    let remote = continuity_remote(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let required = required_internal_ref(&head);
    let before = git_state(&repo, root.path());

    assert!(OrnaInternalRef::new("refs/heads/main").is_err());
    assert!(OrnaInternalRef::new("refs/orna/ids/contains\x7fdel").is_err());
    assert!(NativeObjectId::new("not-a-native-object-id").is_err());
    assert_eq!(
        repo.observe_remote_continuity("origin", &[]),
        RemoteContinuity::InvalidEvidence
    );
    assert_eq!(
        repo.observe_remote_continuity("origin", &[required.clone(), required.clone()]),
        RemoteContinuity::InvalidEvidence
    );
    let wrong_native_format = RequiredInternalRef::new(
        OrnaInternalRef::new("refs/orna/ids/0123456789abcdef").unwrap(),
        NativeObjectId::new("0123456789012345678901234567890123456789012345678901234567890123")
            .unwrap(),
    );
    assert_eq!(
        repo.observe_remote_continuity("origin", &[wrong_native_format]),
        RemoteContinuity::InvalidEvidence
    );
    assert_eq!(
        repo.observe_remote_continuity("missing", std::slice::from_ref(&required)),
        RemoteContinuity::Unverifiable
    );
    git(
        remote.path(),
        &["update-ref", "refs/orna/unexpected", &head],
    );
    assert_eq!(
        repo.observe_remote_continuity("origin", std::slice::from_ref(&required)),
        RemoteContinuity::Missing
    );
    assert_eq!(git_state(&repo, root.path()), before);

    let unreachable = repository();
    with_remote(unreachable.path());
    let unreachable_repo = Repository::discover(unreachable.path()).unwrap();
    let unreachable_head = git(unreachable.path(), &["rev-parse", "HEAD"]);
    let unreachable_before = git_state(&unreachable_repo, unreachable.path());
    assert_eq!(
        unreachable_repo
            .observe_remote_continuity("origin", &[required_internal_ref(&unreachable_head)]),
        RemoteContinuity::Unverifiable
    );
    assert_eq!(
        git_state(&unreachable_repo, unreachable.path()),
        unreachable_before
    );
}

#[test]
fn distinguishes_sparse_checkout_from_partial_clone() {
    let root = repository();
    git(root.path(), &["sparse-checkout", "init", "--no-cone"]);
    git(
        root.path(),
        &["sparse-checkout", "set", "main.orna", "ordinary.txt"],
    );
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());
    let capabilities = repo.observe_git_capabilities().unwrap();

    assert_eq!(capabilities.mode(), GitRepositoryMode::SparseCheckout);
    assert!(capabilities.sparse_checkout());
    assert!(!capabilities.partial_clone());
    assert!(capabilities.promisor_remotes().is_empty());
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn sparse_checkout_omits_worktree_paths_without_promising_their_objects() {
    let root = repository();
    let blob = git(root.path(), &["rev-parse", "HEAD:ordinary.txt"]);
    git(root.path(), &["sparse-checkout", "init", "--no-cone"]);
    git(root.path(), &["sparse-checkout", "set", "main.orna"]);
    assert!(!root.path().join("ordinary.txt").exists());

    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());
    assert_eq!(
        repo.observe_git_capabilities().unwrap().mode(),
        GitRepositoryMode::SparseCheckout
    );
    assert!(matches!(
        repo.observe_git_object(&blob).unwrap(),
        GitObjectState::Materialized {
            kind: GitObjectKind::Blob,
            size: 1..
        }
    ));
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn observes_a_partial_clone_and_a_promised_object_without_hydrating() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        // A configuration-only repository never authorizes a Promised result.
        let root = repository();
        with_partial_clone(root.path());
        let repo = Repository::discover(root.path()).unwrap();
        assert_eq!(
            repo.observe_git_object("0000000000000000000000000000000000000000")
                .unwrap(),
            GitObjectState::Unavailable
        );
        return;
    };
    let repo = Repository::discover(&clone).unwrap();
    let before = git_state(&repo, &clone);
    let capabilities = repo.observe_git_capabilities().unwrap();

    assert_eq!(capabilities.mode(), GitRepositoryMode::PartialClone);
    assert!(!capabilities.sparse_checkout());
    assert!(capabilities.partial_clone());
    assert_eq!(capabilities.promisor_remotes(), &["origin".to_owned()]);
    assert_eq!(
        repo.observe_git_object(&promised).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(git_state(&repo, &clone), before);
    drop(fixture);
}

#[test]
fn filtered_clone_keeps_promised_unavailable_and_materialized_objects_distinct_without_mutation() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        // The fixture requires file-protocol filtering support.  A
        // configuration-only repository is covered separately and must never
        // manufacture a promised result.
        return;
    };
    let repo = Repository::discover(&clone).unwrap();
    let before = git_state(&repo, &clone);
    let origin = fixture.path().join("origin.git");
    let origin_refs_before = git(
        &origin,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let origin_objects_before = git(&origin, &["count-objects", "-v"]);
    let materialized = git(&clone, &["rev-parse", "HEAD"]);
    let unavailable = "0".repeat(materialized.len());

    assert!(matches!(
        repo.observe_git_object(&materialized).unwrap(),
        GitObjectState::Materialized {
            kind: GitObjectKind::Commit,
            size: 1..
        }
    ));
    assert_eq!(
        repo.observe_git_object(&promised).unwrap(),
        GitObjectState::Promised
    );
    assert_eq!(
        repo.observe_git_object(&unavailable).unwrap(),
        GitObjectState::Unavailable
    );

    assert_eq!(git_state(&repo, &clone), before);
    assert_eq!(
        git(
            &origin,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        origin_refs_before
    );
    assert_eq!(
        git(&origin, &["count-objects", "-v"]),
        origin_objects_before
    );
    drop(fixture);
}

#[test]
fn declared_object_set_requires_every_object_to_be_materialized_or_proven_promised() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        // The fixture requires file-protocol filtering support. A
        // configuration-only repository cannot prove a missing object is
        // promised, so it is not evidence for declared-set completeness.
        return;
    };
    let repo = Repository::discover(&clone).unwrap();
    let before = git_state(&repo, &clone);
    let origin = fixture.path().join("origin.git");
    let origin_refs_before = git(
        &origin,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let origin_objects_before = git(&origin, &["count-objects", "-v"]);
    let materialized = git(&clone, &["rev-parse", "HEAD"]);
    let unavailable = "0".repeat(materialized.len());

    assert_eq!(
        repo.observe_declared_git_object_set(&[&materialized, &promised])
            .unwrap(),
        GitDeclaredObjectSetState::Complete
    );
    assert_eq!(
        repo.observe_declared_git_object_set(&[&materialized, &unavailable])
            .unwrap(),
        GitDeclaredObjectSetState::Incomplete
    );
    assert_eq!(
        repo.observe_declared_git_object_set(&["not-a-native-object-id"])
            .unwrap(),
        GitDeclaredObjectSetState::Malformed
    );

    assert_eq!(git_state(&repo, &clone), before);
    assert_eq!(
        git(
            &origin,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        origin_refs_before
    );
    assert_eq!(
        git(&origin, &["count-objects", "-v"]),
        origin_objects_before
    );
    drop(fixture);
}

#[test]
fn declared_segment_blob_set_rejects_non_blobs_without_hydrating_promises() {
    let Some((fixture, clone, promised)) = filtered_clone() else {
        // The fixture requires file-protocol filtering support. A
        // configuration-only repository cannot prove a missing blob is
        // promised, so it is not evidence for segment completeness.
        return;
    };
    let repo = Repository::discover(&clone).unwrap();
    let before = git_state(&repo, &clone);
    let origin = fixture.path().join("origin.git");
    let origin_refs_before = git(
        &origin,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let origin_objects_before = git(&origin, &["count-objects", "-v"]);
    let blob = git(&clone, &["rev-parse", "HEAD:visible.txt"]);
    let tree = git(&clone, &["rev-parse", "HEAD^{tree}"]);
    let unavailable = "0".repeat(blob.len());

    assert_eq!(
        repo.observe_declared_segment_blob_set(&[&blob, &promised])
            .unwrap(),
        GitDeclaredObjectSetState::Complete
    );
    assert_eq!(
        repo.observe_declared_segment_blob_set(&[&tree]).unwrap(),
        GitDeclaredObjectSetState::Malformed
    );
    assert_eq!(
        repo.observe_declared_segment_blob_set(&[&unavailable])
            .unwrap(),
        GitDeclaredObjectSetState::Incomplete
    );

    assert_eq!(git_state(&repo, &clone), before);
    assert_eq!(
        git(
            &origin,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        ),
        origin_refs_before
    );
    assert_eq!(
        git(&origin, &["count-objects", "-v"]),
        origin_objects_before
    );
    drop(fixture);
}

#[test]
fn compact_manifest_observation_fails_closed_without_manifest_metadata() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_compact_manifest_segment_blobs(&head, Uuid::nil())
            .unwrap(),
        GitDeclaredObjectSetState::Incomplete
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn empty_compact_manifest_is_a_valid_committed_snapshot() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let table = Uuid::new_v4();
    let schema = compact_schema_fingerprint(table);
    let manifest_path = ManagedPath::new(format!(".orna/storage/{table}/manifest.orna")).unwrap();
    let manifest = format!(
        "{{profile: \"compact-storage-v1\", table: \"{table}\", schema: \"{}\", next_generation: 1, shards: []}}\n",
        hex_digest(&schema),
    )
    .into_bytes();
    let head = repo.head().unwrap().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                manifest_path,
                Some(manifest),
            )],
            "test: commit empty compact manifest",
        )
        .unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    assert_eq!(
        repo.read_compact_manifest(&candidate.commit().clone(), table)
            .unwrap(),
        Some(CompactManifest::empty(table, schema))
    );
}

#[test]
fn compact_manifest_inventory_ignores_policy_only_table_roots() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let initial = repo.head().unwrap().unwrap();
    assert!(
        repo.plan_compact_manifest_inventory_hydration(&initial)
            .unwrap()
            .is_empty()
    );
    let table = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0606);
    let head = repo.head().unwrap().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                ManagedPath::new(format!(".orna/storage/{table}/policy.orna")).unwrap(),
                Some(b"{placement: \"automatic\"}\n".to_vec()),
            )],
            "test: commit policy-only compact root",
        )
        .unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    assert!(
        repo.read_compact_manifest_inventory(candidate.commit())
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.plan_compact_manifest_inventory_hydration(candidate.commit())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn observes_combined_sparse_and_partial_capabilities() {
    let root = repository();
    with_partial_clone(root.path());
    git(root.path(), &["config", "core.sparseCheckout", "true"]);
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    let capabilities = repo.observe_git_capabilities().unwrap();
    assert_eq!(capabilities.mode(), GitRepositoryMode::Combined);
    assert!(capabilities.sparse_checkout());
    assert!(capabilities.partial_clone());
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn reports_malformed_configuration_and_object_ids_without_leaking_state() {
    let root = repository();
    with_remote(root.path());
    git(
        root.path(),
        &["config", "extensions.partialClone", "missing-remote"],
    );
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_git_capabilities().unwrap().mode(),
        GitRepositoryMode::Malformed
    );
    assert_eq!(
        repo.observe_git_object("not-a-native-object-id").unwrap(),
        GitObjectState::Malformed
    );
    assert_eq!(
        repo.observe_git_object("0000000000000000000000000000000000000000")
            .unwrap(),
        GitObjectState::Malformed
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn reports_an_unavailable_object_distinct_from_a_promised_object() {
    let root = repository();
    with_partial_clone(root.path());
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    // Valid promisor configuration alone is not proof that an arbitrary
    // object is promised by a reachable local commit or tree.
    assert_eq!(
        repo.observe_git_object("0000000000000000000000000000000000000000")
            .unwrap(),
        GitObjectState::Unavailable
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn observes_materialized_tree_blob_and_tag_without_mutation() {
    let root = repository();
    git(root.path(), &["tag", "-a", "release", "-m", "release"]);
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());
    let tree = git(root.path(), &["rev-parse", "HEAD^{tree}"]);
    let blob = git(root.path(), &["rev-parse", "HEAD:ordinary.txt"]);
    let tag = git(root.path(), &["rev-parse", "release^{tag}"]);

    for (object, kind) in [
        (tree, GitObjectKind::Tree),
        (blob, GitObjectKind::Blob),
        (tag, GitObjectKind::Tag),
    ] {
        assert!(matches!(
            repo.observe_git_object(&object).unwrap(),
            GitObjectState::Materialized { kind: observed, .. } if observed == kind
        ));
    }
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn observes_native_objects_with_case_insensitive_hex_spelling() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    // This is the fixed SHA-1 for the `main.orna` bytes written by
    // `repository()`. Keeping the object ID explicit makes the case change
    // deterministic instead of depending on a particular HEAD hash.
    let object = "8bffabe30647ae6f01aec31fbcc2ad23dde3dcbd";
    assert_eq!(git(root.path(), &["rev-parse", "HEAD:main.orna"]), object);
    let uppercase = object.to_ascii_uppercase();
    assert_ne!(object, uppercase);

    assert_eq!(
        repo.observe_git_object(&uppercase).unwrap(),
        repo.observe_git_object(object).unwrap()
    );
}

#[test]
fn reports_a_corrupt_local_object_as_malformed_without_mutation() {
    let root = repository();
    let object = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let object_directory = root.path().join(".git").join("objects").join("aa");
    fs::create_dir_all(&object_directory).unwrap();
    fs::write(object_directory.join(&object[2..]), b"not a Git object").unwrap();
    let repo = Repository::discover(root.path()).unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.observe_git_object(object).unwrap(),
        GitObjectState::Malformed
    );
    assert_eq!(git_state(&repo, root.path()), before);
}

#[test]
fn managed_materialization_is_atomic_and_conflict_fenced() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let path = ManagedPath::new("generated/books/one.orna").unwrap();

    repo.materialize_managed_file(&path, None, Some(b"first"))
        .unwrap();
    assert_eq!(
        repo.managed_file_bytes(&path).unwrap(),
        Some(b"first".to_vec())
    );
    assert_eq!(
        fs::read(root.path().join(path.as_path())).unwrap(),
        b"first"
    );

    fs::write(root.path().join(path.as_path()), b"editor change").unwrap();
    assert!(matches!(
        repo.materialize_managed_file(&path, Some(b"first"), Some(b"second")),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(
        fs::read(root.path().join(path.as_path())).unwrap(),
        b"editor change"
    );
    repo.materialize_managed_file(&path, Some(b"editor change"), None)
        .unwrap();
    assert!(!root.path().join(path.as_path()).exists());
}

#[cfg(unix)]
#[test]
fn managed_materialization_rejects_symlinked_parents() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("generated")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("generated/linked")).unwrap();
    let path = ManagedPath::new("generated/linked/row.orna").unwrap();

    assert!(matches!(
        repo.materialize_managed_file(&path, None, Some(b"row")),
        Err(orna_repository_v1::RepositoryError::UnsafeManagedPath)
    ));
    assert!(!outside.path().join("row.orna").exists());
}

#[test]
fn private_candidate_uses_head_and_preserves_ordinary_cwd_state() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let worktree_before = repo.worktree_state().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                ManagedPath::new("generated/row.orna").unwrap(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();

    assert_ne!(candidate.commit(), &head);
    assert_eq!(repo.head().unwrap().unwrap(), head);
    assert_eq!(repo.index_generation().unwrap(), index_before);
    assert_eq!(repo.worktree_state().unwrap(), worktree_before);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        git(
            root.path(),
            &[
                "show",
                &format!("{}:generated/row.orna", candidate.commit())
            ]
        ),
        "candidate row"
    );
    assert_eq!(
        git(
            root.path(),
            &["show", &format!("{}:ordinary.txt", candidate.commit())]
        ),
        "base"
    );
    assert_eq!(
        git(
            root.path(),
            &["rev-parse", &format!("{}^", candidate.commit())]
        ),
        head.as_str()
    );
}

#[test]
fn capture_rejects_a_tracked_case_folded_loose_sibling_without_mutation() {
    let root = repository();
    fs::create_dir_all(root.path().join("Contact")).unwrap();
    fs::write(root.path().join("Contact/Alice.orna"), b"committed row\n").unwrap();
    git(root.path(), &["add", "Contact/Alice.orna"]);
    git(root.path(), &["commit", "-m", "add managed row"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let proposed = ManagedPath::new("Contact/alice.orna").unwrap();
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.capture_managed_publication_state(&head, &index, std::slice::from_ref(&proposed)),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(
        fs::read(root.path().join("Contact/Alice.orna")).unwrap(),
        b"committed row\n"
    );
    assert!(!root.path().join(proposed.as_path()).exists());
}

#[test]
fn capture_rejects_a_case_folded_table_root_without_mutation() {
    let root = repository();
    fs::create_dir_all(root.path().join("Contact")).unwrap();
    fs::write(root.path().join("Contact/Alice.orna"), b"committed row\n").unwrap();
    git(root.path(), &["add", "Contact/Alice.orna"]);
    git(root.path(), &["commit", "-m", "add managed row"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let proposed = ManagedPath::new("contact/Alice.orna").unwrap();
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.capture_managed_publication_state(&head, &index, std::slice::from_ref(&proposed)),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(
        fs::read(root.path().join("Contact/Alice.orna")).unwrap(),
        b"committed row\n"
    );
    assert!(!root.path().join(proposed.as_path()).exists());
}

#[test]
fn capture_rejects_case_folded_planned_loose_siblings_without_mutation() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let upper = ManagedPath::new("Contact/Alice.orna").unwrap();
    let lower = ManagedPath::new("Contact/alice.orna").unwrap();
    let before = git_state(&repo, root.path());

    assert!(matches!(
        repo.capture_managed_publication_state(&head, &index, &[upper, lower]),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert!(!root.path().join("Contact/Alice.orna").exists());
    assert!(!root.path().join("Contact/alice.orna").exists());
}

#[test]
fn private_candidate_accepts_a_case_only_rekey_after_deleting_the_old_path() {
    let root = repository();
    fs::create_dir_all(root.path().join("Contact")).unwrap();
    fs::write(root.path().join("Contact/Alice.orna"), b"old row\n").unwrap();
    git(root.path(), &["add", "Contact/Alice.orna"]);
    git(root.path(), &["commit", "-m", "add managed row"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let old_path = ManagedPath::new("Contact/Alice.orna").unwrap();
    let new_path = ManagedPath::new("Contact/alice.orna").unwrap();
    let before = git_state(&repo, root.path());

    assert_eq!(
        repo.capture_managed_publication_state(
            &head,
            &index,
            &[old_path.clone(), new_path.clone()]
        )
        .unwrap(),
        vec![Some(b"old row\n".to_vec()), None]
    );
    let candidate = repo
        .build_private_commit(
            &head,
            &[
                orna_repository_v1::ManagedFileChange::new(old_path, None),
                orna_repository_v1::ManagedFileChange::new(
                    new_path,
                    Some(b"rekeyed row\n".to_vec()),
                ),
            ],
            "orna: case-only re-key",
        )
        .unwrap();

    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(
        git(
            root.path(),
            &[
                "ls-tree",
                "-r",
                "--name-only",
                candidate.commit().as_str(),
                "--",
                "Contact/alice.orna",
            ],
        ),
        "Contact/alice.orna"
    );
    assert_eq!(
        git(
            root.path(),
            &[
                "ls-tree",
                "-r",
                "--name-only",
                candidate.commit().as_str(),
                "--",
                "Contact/Alice.orna",
            ],
        ),
        ""
    );
}

#[test]
fn capture_allows_an_exact_tracked_loose_path_update() {
    let root = repository();
    fs::create_dir_all(root.path().join("Contact")).unwrap();
    fs::write(root.path().join("Contact/Alice.orna"), b"committed row\n").unwrap();
    git(root.path(), &["add", "Contact/Alice.orna"]);
    git(root.path(), &["commit", "-m", "add managed row"]);
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let path = ManagedPath::new("Contact/Alice.orna").unwrap();

    assert_eq!(
        repo.capture_managed_publication_state(&head, &index, &[path.clone(), path])
            .unwrap(),
        vec![
            Some(b"committed row\n".to_vec()),
            Some(b"committed row\n".to_vec())
        ]
    );
}

#[test]
fn private_candidate_advances_current_branch_with_compare_and_set() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                ManagedPath::new("generated/row.orna").unwrap(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();

    repo.advance_current_ref(&head, &candidate).unwrap();
    assert_eq!(repo.head().unwrap().unwrap(), *candidate.commit());
    let index_after = repo.index_generation().unwrap();
    assert_eq!(index_after.tree(), index_before.tree());
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged human edit\n"
    );
    let status = git(root.path(), &["status", "--short"]);
    assert!(status.contains("ordinary.txt"));
    assert!(status.contains("main.orna"));
    assert!(status.contains("generated/row.orna"));
    assert!(matches!(
        repo.advance_current_ref(&head, &candidate),
        Err(orna_repository_v1::RepositoryError::StaleHead)
    ));
}

#[test]
fn published_candidate_reconciles_only_managed_index_entries() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    let reconciled = repo
        .reconcile_published_index(&index_before, &candidate, &[managed])
        .unwrap();
    assert_eq!(reconciled.head(), Some(candidate.commit()));
    assert_eq!(
        git(root.path(), &["show", ":generated/row.orna"]),
        "candidate row"
    );
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged human edit\n"
    );
}

#[test]
fn publication_journal_round_trips_atomically_and_advances_monotonically() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new(
        head.clone(),
        candidate.commit().clone(),
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.write_publication_journal(&journal).unwrap();
    assert_eq!(
        repo.read_publication_journal().unwrap(),
        Some(journal.clone())
    );
    let mut resumed = repo.read_publication_journal().unwrap().unwrap();
    resumed
        .advance(orna_repository_v1::PublicationJournalStage::RefAdvanced)
        .unwrap();
    assert!(matches!(
        resumed.advance(orna_repository_v1::PublicationJournalStage::Complete),
        Err(orna_repository_v1::RepositoryError::InvalidPublicationJournal)
    ));
    repo.write_publication_journal(&resumed).unwrap();
    assert_eq!(
        repo.read_publication_journal().unwrap().unwrap().stage(),
        orna_repository_v1::PublicationJournalStage::RefAdvanced
    );
    repo.clear_publication_journal().unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn published_branch_tip_keeps_the_configured_identity() {
    let root = repository();
    git(root.path(), &["config", "user.name", "Identity X"]);
    git(
        root.path(),
        &["config", "user.email", "identity-x@example.invalid"],
    );
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let bytes = b"identity row\n".to_vec();
    let head_before = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head_before,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(bytes.clone()),
            )],
            "orna: publish identity row",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head_before.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [5; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(bytes),
        )],
    )
    .unwrap();
    repo.publish_candidate(&index_before, &candidate, &mut journal)
        .unwrap();
    repo.clear_publication_journal().unwrap();

    let tip = git(root.path(), &["log", "-1", "--format=%an <%ae>"]);
    assert_eq!(tip.trim(), "Identity X <identity-x@example.invalid>");
}

#[test]
fn republish_after_edit_bumps_the_revision_and_serves_the_edited_bytes() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let first = b"first publication\n".to_vec();
    let edited: &[u8] = include_bytes!("fixtures/row-edit-v2.orna");

    let head_before = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head_before,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(first.clone()),
            )],
            "orna: publish row",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head_before.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [4; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(first.clone()),
        )],
    )
    .unwrap();
    repo.publish_candidate(&index_before, &candidate, &mut journal)
        .unwrap();
    repo.clear_publication_journal().unwrap();
    let revision_one = repo.head().unwrap().unwrap();
    assert_ne!(revision_one, head_before);

    let index_one = repo.index_generation().unwrap();
    let edit = repo
        .build_private_commit(
            &revision_one,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(edited.to_vec()),
            )],
            "orna: republish edited row",
        )
        .unwrap();
    let mut edit_journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        revision_one.clone(),
        edit.commit().clone(),
        index_one.tree().unwrap().clone(),
        [5; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            Some(first),
            Some(edited.to_vec()),
        )],
    )
    .unwrap();
    repo.publish_candidate(&index_one, &edit, &mut edit_journal)
        .unwrap();

    let revision_two = repo.head().unwrap().unwrap();
    assert_ne!(
        revision_two, revision_one,
        "republish must bump the revision"
    );
    assert_eq!(
        git(
            root.path(),
            &["show", &format!("HEAD:{}", managed.as_path().display())]
        ),
        String::from_utf8(edited.to_vec()).unwrap().trim_end(),
    );
}

#[test]
fn publish_candidate_completes_ref_index_and_worktree_boundaries() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();

    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [1; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.publish_candidate(&index_before, &candidate, &mut journal)
        .unwrap();
    assert_eq!(repo.head().unwrap().unwrap(), *candidate.commit());
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"candidate row\n"
    );
    assert_eq!(
        git(root.path(), &["show", ":generated/row.orna"]),
        "candidate row"
    );
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged human edit"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("main.orna")).unwrap(),
        "unstaged human edit\n"
    );
    repo.mark_runtime_complete([1; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn publication_refuses_to_replace_a_retained_journal() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let retained_candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"retained row\n".to_vec()),
            )],
            "orna: retained publication",
        )
        .unwrap();
    let retained = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        retained_candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [71; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"retained row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&retained).unwrap();

    let replacement_candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"replacement row\n".to_vec()),
            )],
            "orna: replacement publication",
        )
        .unwrap();
    let mut replacement = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        replacement_candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [72; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"replacement row\n".to_vec()),
        )],
    )
    .unwrap();

    assert!(matches!(
        repo.publish_candidate(&index_before, &replacement_candidate, &mut replacement),
        Err(orna_repository_v1::RepositoryError::PublicationPending)
    ));
    assert_eq!(repo.head().unwrap(), Some(head));
    assert_eq!(repo.index_generation().unwrap(), index_before);
    assert_eq!(repo.managed_file_bytes(&managed).unwrap(), None);
    assert_eq!(repo.read_publication_journal().unwrap(), Some(retained));
    assert_eq!(
        replacement.stage(),
        orna_repository_v1::PublicationJournalStage::Prepared
    );
}

#[test]
fn retained_journal_with_moved_head_is_stale_and_keeps_batch() {
    // ORNA-PUB-009: a HEAD that moved past this publisher's expected head is a
    // lost CAS and reports StaleHead, checked before the retained-journal
    // pending outcome. ORNA-PUB-006: the retained batch is not released.
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let retained_candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"retained row\n".to_vec()),
            )],
            "orna: retained publication",
        )
        .unwrap();
    let retained = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        retained_candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [71; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"retained row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&retained).unwrap();

    let replacement_candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"replacement row\n".to_vec()),
            )],
            "orna: replacement publication",
        )
        .unwrap();
    let mut replacement = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        replacement_candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [72; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"replacement row\n".to_vec()),
        )],
    )
    .unwrap();

    git(root.path(), &["commit", "--allow-empty", "-m", "moved head"]);
    let moved_head = repo.head().unwrap();
    assert_ne!(moved_head, Some(head));

    assert!(matches!(
        repo.publish_candidate(&index_before, &replacement_candidate, &mut replacement),
        Err(orna_repository_v1::RepositoryError::StaleHead)
    ));
    assert_eq!(repo.head().unwrap(), moved_head);
    assert_eq!(repo.read_publication_journal().unwrap(), Some(retained));
}

#[test]
fn crashed_publisher_after_ref_advance_recovers_despite_stale_retry() {
    // ORNA-PUB-006: a publisher that dies after RefAdvanced leaves its journal
    // retained and the ref moved. A later publisher prepared from the old head
    // is refused with StaleHead, which must not mask recovery of that journal.
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let crashed_candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"crashed row\n".to_vec()),
            )],
            "orna: crashed publication",
        )
        .unwrap();
    let mut crashed = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        crashed_candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [73; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"crashed row\n".to_vec()),
        )],
    )
    .unwrap();
    let stale_candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"stale row\n".to_vec()),
            )],
            "orna: stale publication",
        )
        .unwrap();
    let mut stale = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        stale_candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [74; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"stale row\n".to_vec()),
        )],
    )
    .unwrap();

    // Simulated crash: unwinding right after the ref is durably advanced.
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        repo.publish_candidate_with_stage_hook(
            &index_before,
            &crashed_candidate,
            &mut crashed,
            |stage| {
                if stage == orna_repository_v1::PublicationJournalStage::RefAdvanced {
                    panic!("simulated publisher crash after ref advance");
                }
            },
        )
    }));
    assert!(crash.is_err());
    let advanced = repo.head().unwrap();
    assert_eq!(advanced, Some(crashed_candidate.commit().clone()));
    let retained = repo.read_publication_journal().unwrap();
    assert_eq!(
        retained.as_ref().map(|journal| journal.stage()),
        Some(orna_repository_v1::PublicationJournalStage::RefAdvanced)
    );

    assert!(matches!(
        repo.publish_candidate(&index_before, &stale_candidate, &mut stale),
        Err(orna_repository_v1::RepositoryError::StaleHead)
    ));
    assert_eq!(repo.head().unwrap(), advanced);
    assert_eq!(repo.read_publication_journal().unwrap(), retained);

    // Recovery reconciles the ref-advanced journal and then stops at the
    // runtime-completion boundary, which only the runtime owner may clear.
    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired)
    ));
    assert_eq!(repo.head().unwrap(), advanced);
    assert_eq!(
        repo.read_publication_journal()
            .unwrap()
            .map(|journal| journal.stage()),
        Some(orna_repository_v1::PublicationJournalStage::WorktreeReconciled)
    );
}

#[test]
fn cross_protocol_recovery_journals_are_mutually_exclusive() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    git(root.path(), &["branch", "experiment"]);
    git(root.path(), &["switch", "experiment"]);
    fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(root.path(), &["commit", "-m", "target change"]);
    git(root.path(), &["switch", "main"]);
    fs::write(root.path().join("ordinary.txt"), "staged local\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);

    let runtime = RuntimeGeneration::new(877);
    let plan = repo.plan_checkout("experiment", runtime).unwrap();
    let token = plan.force_token();
    let discard = repo
        .validate_checkout_discard_set(
            &plan,
            true,
            Some(&token),
            plan.git().discardable_paths(),
            &AcceptCheckout,
        )
        .unwrap();
    repo.persist_validated_checkout_discard(&discard).unwrap();
    let checkout_path = repo.runtime_paths().root().join("checkout-journal.bin");
    let checkout_bytes = fs::read(&checkout_path).unwrap();
    repo.recover_pre_execution_checkout().unwrap();
    assert!(!checkout_path.exists());

    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: cross-protocol publication",
        )
        .unwrap();
    let mut publication = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [87; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&publication).unwrap();
    let publication_path = repo.runtime_paths().root().join("publication-journal.bin");
    let publication_bytes = fs::read(&publication_path).unwrap();
    fs::write(&checkout_path, &checkout_bytes).unwrap();

    let before = git_state(&repo, root.path());
    let index_file_before = fs::read(root.path().join(".git/index")).unwrap();
    assert!(matches!(
        repo.publish_candidate(&index_before, &candidate, &mut publication),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(matches!(
        repo.clear_publication_journal(),
        Err(orna_repository_v1::RepositoryError::CheckoutRecoveryRequired)
    ));
    assert!(matches!(
        repo.persist_validated_checkout_discard(&discard),
        Err(orna_repository_v1::RepositoryError::PublicationPending)
    ));
    assert!(matches!(
        repo.execute_validated_force_checkout(&discard),
        Err(orna_repository_v1::RepositoryError::PublicationPending)
    ));
    assert!(matches!(
        repo.recover_pre_execution_checkout(),
        Err(orna_repository_v1::RepositoryError::PublicationPending)
    ));
    assert_eq!(git_state(&repo, root.path()), before);
    assert_eq!(
        fs::read(root.path().join(".git/index")).unwrap(),
        index_file_before
    );
    assert_eq!(fs::read(&publication_path).unwrap(), publication_bytes);
    assert_eq!(fs::read(&checkout_path).unwrap(), checkout_bytes);
}

#[test]
fn publication_pauses_for_an_existing_git_index_lock_before_ref_change() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [9; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    fs::write(root.path().join(".git/index.lock"), b"ordinary writer\n").unwrap();
    assert!(matches!(
        repo.publish_candidate(&index_before, &candidate, &mut journal),
        Err(orna_repository_v1::RepositoryError::GitIndexLockPresent)
    ));
    assert_eq!(repo.head().unwrap(), Some(head));
    assert_eq!(
        journal.stage(),
        orna_repository_v1::PublicationJournalStage::Prepared
    );
}

#[test]
fn publication_pauses_during_unfinished_merge_and_rebase() {
    for operation in ["merge", "rebase"] {
        let root = repository_with_v1_module_fixture();
        let repo = Repository::discover(root.path())
            .unwrap()
            .with_coordination_lock_wait(std::time::Duration::from_millis(50));
        let head = repo.head().unwrap().unwrap();
        let index_before = repo.index_generation().unwrap();
        let managed = ManagedPath::new("generated/row.orna").unwrap();
        let candidate = repo
            .build_private_commit(
                &head,
                &[orna_repository_v1::ManagedFileChange::new(
                    managed.clone(),
                    Some(b"candidate row\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
            head,
            candidate.commit().clone(),
            index_before.tree().unwrap().clone(),
            [11; 16],
            vec![orna_repository_v1::PublicationJournalEntry::new(
                managed.clone(),
                None,
                Some(b"candidate row\n".to_vec()),
            )],
        )
        .unwrap();

        git(root.path(), &["switch", "-c", "topic"]);
        fs::write(root.path().join("main.orna"), b"topic change\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "topic change"]);
        git(root.path(), &["switch", "main"]);

        let marker_name = if operation == "merge" {
            git(root.path(), &["merge", "--no-commit", "--no-ff", "topic"]);
            "MERGE_HEAD"
        } else {
            fs::write(root.path().join("main.orna"), b"main change\n").unwrap();
            git(root.path(), &["add", "main.orna"]);
            git(root.path(), &["commit", "-m", "main change"]);
            let rebase = Command::new("git")
                .current_dir(root.path())
                .args(["rebase", "main", "topic"])
                .output()
                .unwrap();
            assert!(
                !rebase.status.success(),
                "expected topic rebase to stop on a content conflict"
            );
            fs::write(root.path().join("main.orna"), b"resolved rebase conflict\n").unwrap();
            git(root.path(), &["add", "main.orna"]);
            let rebase_merge = PathBuf::from(git(
                root.path(),
                &["rev-parse", "--git-path", "rebase-merge"],
            ));
            if rebase_merge.is_absolute() {
                if rebase_merge.exists() {
                    "rebase-merge"
                } else {
                    "rebase-apply"
                }
            } else if root.path().join(&rebase_merge).exists() {
                "rebase-merge"
            } else {
                "rebase-apply"
            }
        };

        let marker_path =
            PathBuf::from(git(root.path(), &["rev-parse", "--git-path", marker_name]));
        let marker_path = if marker_path.is_absolute() {
            marker_path
        } else {
            root.path().join(marker_path)
        };
        assert!(marker_path.exists(), "{operation} marker missing");
        assert_eq!(
            git(root.path(), &["diff", "--name-only", "--diff-filter=U"]),
            "",
            "{operation} index must be resolved and usable"
        );

        let capture_head = repo.head().unwrap().unwrap();
        let capture_index = repo.index_generation().unwrap();
        let before = git_state(&repo, root.path());
        let index_bytes = fs::read(root.path().join(".git/index")).unwrap();
        let worktree_bytes = fs::read(root.path().join("main.orna")).unwrap();

        assert!(matches!(
            repo.capture_managed_publication_state(
                &capture_head,
                &capture_index,
                std::slice::from_ref(&managed)
            ),
            Err(orna_repository_v1::RepositoryError::RepositoryBusy)
        ));
        assert!(matches!(
            repo.publish_candidate(&index_before, &candidate, &mut journal),
            Err(orna_repository_v1::RepositoryError::RepositoryBusy)
        ));
        assert_eq!(git_state(&repo, root.path()), before);
        assert_eq!(
            fs::read(root.path().join(".git/index")).unwrap(),
            index_bytes
        );
        assert_eq!(
            fs::read(root.path().join("main.orna")).unwrap(),
            worktree_bytes
        );
        assert!(marker_path.exists(), "{operation} marker was removed");
        assert_eq!(
            journal.stage(),
            orna_repository_v1::PublicationJournalStage::Prepared
        );
        assert_eq!(repo.read_publication_journal().unwrap(), None);
    }
}

#[test]
fn publication_uses_worktree_scoped_git_operation_markers() {
    let root = repository_with_v1_module_fixture();
    let sibling_repo = Repository::discover(root.path()).unwrap();
    let linked_container = TempDir::new().unwrap();
    let linked_root = linked_container.path().join("linked");
    let linked_root_text = linked_root.to_str().unwrap().to_owned();
    git(
        root.path(),
        &["worktree", "add", "-b", "linked", &linked_root_text],
    );
    git(&linked_root, &["switch", "-c", "topic"]);
    fs::write(linked_root.join("main.orna"), b"topic change\n").unwrap();
    git(&linked_root, &["add", "main.orna"]);
    git(&linked_root, &["commit", "-m", "topic change"]);
    git(&linked_root, &["switch", "linked"]);
    git(&linked_root, &["merge", "--no-commit", "--no-ff", "topic"]);

    let linked_repo = Repository::discover(&linked_root).unwrap();
    let linked_head = linked_repo.head().unwrap().unwrap();
    let linked_index = linked_repo.index_generation().unwrap();
    let managed = ManagedPath::new("main.orna").unwrap();
    let git_admin_path = |directory: &Path, name: &str| {
        let path = PathBuf::from(git(directory, &["rev-parse", "--git-path", name]));
        if path.is_absolute() {
            path
        } else {
            directory.join(path)
        }
    };
    let linked_marker = git_admin_path(&linked_root, "MERGE_HEAD");
    let sibling_marker = git_admin_path(root.path(), "MERGE_HEAD");
    let linked_index_path = git_admin_path(&linked_root, "index");
    let sibling_index_path = git_admin_path(root.path(), "index");
    assert!(linked_marker.exists());
    assert!(!sibling_marker.exists());
    assert_eq!(
        git(&linked_root, &["diff", "--name-only", "--diff-filter=U"]),
        ""
    );

    let sibling_before = git_state(&sibling_repo, root.path());
    let sibling_index_before = fs::read(&sibling_index_path).unwrap();
    let sibling_source_before = fs::read(root.path().join("main.orna")).unwrap();
    let linked_before = git_state(&linked_repo, &linked_root);
    let linked_index_before = fs::read(&linked_index_path).unwrap();
    let linked_source_before = fs::read(linked_root.join("main.orna")).unwrap();
    let sibling_head = sibling_repo.head().unwrap().unwrap();
    let sibling_index = sibling_repo.index_generation().unwrap();
    let sibling_managed = ManagedPath::new("main.orna").unwrap();
    assert_eq!(
        sibling_repo
            .capture_managed_publication_state(
                &sibling_head,
                &sibling_index,
                std::slice::from_ref(&sibling_managed)
            )
            .unwrap(),
        vec![Some(sibling_source_before.clone())]
    );

    assert!(matches!(
        linked_repo.capture_managed_publication_state(
            &linked_head,
            &linked_index,
            std::slice::from_ref(&managed)
        ),
        Err(orna_repository_v1::RepositoryError::RepositoryBusy)
    ));

    assert_eq!(git_state(&sibling_repo, root.path()), sibling_before);
    assert_eq!(fs::read(&sibling_index_path).unwrap(), sibling_index_before);
    assert_eq!(
        fs::read(root.path().join("main.orna")).unwrap(),
        sibling_source_before
    );
    assert_eq!(git_state(&linked_repo, &linked_root), linked_before);
    assert_eq!(fs::read(&linked_index_path).unwrap(), linked_index_before);
    assert_eq!(
        fs::read(linked_root.join("main.orna")).unwrap(),
        linked_source_before
    );
    assert!(linked_marker.exists());
    assert!(!sibling_marker.exists());
    assert!(!git_admin_path(&linked_root, "index.lock").exists());
    assert!(!git_admin_path(root.path(), "index.lock").exists());
    assert_eq!(git(root.path(), &["status", "--porcelain"]), "");
}

#[test]
fn publication_rejects_a_known_managed_edit_before_ref_advance() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [10; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    fs::create_dir_all(root.path().join("generated")).unwrap();
    fs::write(root.path().join(managed.as_path()), b"editor row\n").unwrap();

    assert!(matches!(
        repo.publish_candidate(&index_before, &candidate, &mut journal),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(repo.head().unwrap(), Some(head));
    assert_eq!(repo.index_generation().unwrap(), index_before);
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"editor row\n"
    );
    assert_eq!(
        journal.stage(),
        orna_repository_v1::PublicationJournalStage::Prepared
    );
    // The refusal precedes the Prepared journal, so nothing fences a retry
    // once the editor's bytes are resolved (ORNA-PUB-006).
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn recovery_resumes_after_ref_and_index_boundaries() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head,
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [2; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&journal.old_head().clone(), &candidate)
        .unwrap();
    repo.reconcile_published_index(&index_before, &candidate, std::slice::from_ref(&managed))
        .unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired)
    ));
    assert_eq!(repo.head().unwrap().unwrap(), *candidate.commit());
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"candidate row\n"
    );
    let mut journal = repo.read_publication_journal().unwrap().unwrap();
    repo.mark_runtime_complete([2; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn recovery_after_ref_advance_preserves_unrelated_partial_staging() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    fs::write(root.path().join("ordinary.txt"), "staged ordinary\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    fs::write(
        root.path().join("ordinary.txt"),
        "staged ordinary\nunstaged ordinary\n",
    )
    .unwrap();
    fs::write(root.path().join("main.orna"), "unstaged source\n").unwrap();
    fs::write(root.path().join("untracked.txt"), "untracked ordinary\n").unwrap();
    let staged_before = git(root.path(), &["diff", "--cached", "--binary"]);
    let unstaged_before = git(root.path(), &["diff", "--binary"]);

    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [29; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired)
    ));
    assert_eq!(
        git(root.path(), &["diff", "--cached", "--binary"]),
        staged_before
    );
    assert_eq!(git(root.path(), &["diff", "--binary"]), unstaged_before);
    assert_eq!(
        fs::read_to_string(root.path().join("untracked.txt")).unwrap(),
        "untracked ordinary\n"
    );
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"candidate row\n"
    );
    assert_eq!(
        git(root.path(), &["show", ":generated/row.orna"]),
        "candidate row"
    );

    let mut journal = repo.read_publication_journal().unwrap().unwrap();
    repo.mark_runtime_complete([29; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

#[test]
fn runtime_completion_finalizes_a_descendant_head_without_rewriting_it() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let mut journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head,
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [28; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();

    repo.publish_candidate(&index_before, &candidate, &mut journal)
        .unwrap();
    fs::write(root.path().join("ordinary.txt"), "ordinary commit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    git(
        root.path(),
        &["commit", "-m", "ordinary commit after publication"],
    );

    // ORNA-PUB-009 recovery row "Ref no longer H or N": the ordinary commit
    // descends from the candidate, so the batch is present. Completion clears
    // the journal and leaves HEAD, index and worktree untouched.
    let follow_up = repo.head().unwrap().unwrap();
    repo.mark_runtime_complete([28; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
    assert_eq!(repo.head().unwrap().unwrap(), follow_up);
    assert_eq!(
        fs::read_to_string(root.path().join("ordinary.txt")).unwrap(),
        "ordinary commit\n"
    );
}

/// Builds a one-row legacy candidate on `head` and its Prepared journal.
fn row_publication(
    repo: &Repository,
    head: &orna_repository_v1::GitCommitRef,
    index: &IndexGeneration,
    row: &[u8],
    intent: [u8; 16],
) -> (
    orna_repository_v1::PrivateCommit,
    orna_repository_v1::PublicationJournal,
) {
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let candidate = repo
        .build_private_commit(
            head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(row.to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index.tree().unwrap().clone(),
        intent,
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed,
            None,
            Some(row.to_vec()),
        )],
    )
    .unwrap();
    (candidate, journal)
}

/// Recovery row "Ref still at H" (ORNA-PUB-006, ORNA-PUB-012): a Prepared
/// journal whose candidate never reached HEAD is discarded even though the
/// unreferenced candidate object survives and the user staged unrelated work
/// since. The staged work is preserved and the batch publishes on retry.
#[test]
fn recovery_discards_a_pre_ref_journal_and_keeps_staged_work() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let (candidate, journal) =
        row_publication(&repo, &head, &index_before, b"candidate row\n", [8; 16]);
    repo.write_publication_journal(&journal).unwrap();
    fs::write(
        root.path().join("ordinary.txt"),
        "staged after interruption\n",
    )
    .unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let staged = repo.index_generation().unwrap();
    assert_ne!(staged, index_before);
    assert_eq!(
        git(
            root.path(),
            &["cat-file", "-t", candidate.commit().as_str()]
        ),
        "commit"
    );

    assert_eq!(repo.recover_publication().unwrap(), Some(staged.clone()));
    assert_eq!(repo.read_publication_journal().unwrap(), None);
    assert_eq!(repo.head().unwrap(), Some(head.clone()));
    assert_eq!(repo.index_generation().unwrap(), staged);
    assert_eq!(repo.managed_file_bytes(&managed).unwrap(), None);

    let (retry, mut retry_journal) =
        row_publication(&repo, &head, &staged, b"retried row\n", [9; 16]);
    repo.publish_candidate(&staged, &retry, &mut retry_journal)
        .unwrap();
    repo.mark_runtime_complete([9; 16], &mut retry_journal)
        .unwrap();
    assert_eq!(repo.head().unwrap().as_ref(), Some(retry.commit()));
    assert_eq!(repo.read_publication_journal().unwrap(), None);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged after interruption"
    );
}

/// A detached HEAD refuses publication before the Prepared journal exists,
/// so attaching the branch again lets the next publication succeed.
#[test]
fn publication_on_a_detached_head_leaves_no_journal_and_succeeds_once_attached() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    git(root.path(), &["switch", "--quiet", "--detach"]);
    let index = repo.index_generation().unwrap();
    let (candidate, mut journal) =
        row_publication(&repo, &head, &index, b"detached row\n", [12; 16]);

    assert!(matches!(
        repo.publish_candidate(&index, &candidate, &mut journal),
        Err(orna_repository_v1::RepositoryError::DetachedHead)
    ));
    assert_eq!(repo.read_publication_journal().unwrap(), None);
    assert_eq!(repo.head().unwrap(), Some(head.clone()));
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/heads/main"]),
        head.as_str()
    );
    assert_eq!(repo.index_generation().unwrap(), index);

    git(root.path(), &["switch", "--quiet", "main"]);
    assert_eq!(repo.recover_publication().unwrap(), None);
    let index = repo.index_generation().unwrap();
    let (retry, mut retry_journal) =
        row_publication(&repo, &head, &index, b"attached row\n", [13; 16]);
    repo.publish_candidate(&index, &retry, &mut retry_journal)
        .unwrap();
    repo.mark_runtime_complete([13; 16], &mut retry_journal)
        .unwrap();
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/heads/main"]),
        retry.commit().as_str()
    );
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

/// A ref CAS that loses to a concurrent ref writer discards the journal its
/// attempt wrote, so the pending batch is not fenced behind it.
#[test]
fn publication_that_loses_its_ref_cas_leaves_no_blocking_journal() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let (candidate, mut journal) =
        row_publication(&repo, &head, &index, b"candidate row\n", [14; 16]);
    assert!(root.path().join(".git/refs/heads/main").is_file());
    let ref_lock = root.path().join(".git/refs/heads/main.lock");
    fs::write(&ref_lock, b"concurrent ref writer\n").unwrap();

    assert!(matches!(
        repo.publish_candidate(&index, &candidate, &mut journal),
        Err(orna_repository_v1::RepositoryError::StaleHead)
    ));
    assert_eq!(repo.read_publication_journal().unwrap(), None);
    assert_eq!(repo.head().unwrap(), Some(head.clone()));
    assert_eq!(repo.index_generation().unwrap(), index);
    assert_eq!(repo.managed_file_bytes(&managed).unwrap(), None);
    assert_eq!(fs::read(&ref_lock).unwrap(), b"concurrent ref writer\n");

    fs::remove_file(&ref_lock).unwrap();
    assert_eq!(repo.recover_publication().unwrap(), None);
    repo.publish_candidate(&index, &candidate, &mut journal)
        .unwrap();
    repo.mark_runtime_complete([14; 16], &mut journal).unwrap();
    assert_eq!(repo.head().unwrap().as_ref(), Some(candidate.commit()));
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

/// PUB-3 step 7: an index write that lands after the base was captured is
/// refused before the ref CAS, so publication never installs an index that
/// stages the inverse of its batch.
#[test]
fn publication_refuses_an_index_write_that_lands_before_the_ref_cas() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let (candidate, mut journal) =
        row_publication(&repo, &head, &index, b"candidate row\n", [18; 16]);
    let index_path = root.path().join(".git/index");
    let captured = fs::read(&index_path).unwrap();
    fs::write(root.path().join("ordinary.txt"), "raced staging\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let raced = fs::read(&index_path).unwrap();
    fs::write(&index_path, &captured).unwrap();

    let published =
        repo.publish_candidate_with_stage_hook(&index, &candidate, &mut journal, |stage| {
            if stage == PublicationJournalStage::Prepared {
                // A writer outside Git's lock protocol lands its staged change.
                fs::write(&index_path, &raced).unwrap();
            }
        });

    assert!(matches!(published, Err(RepositoryError::StaleIndex { .. })));
    assert_eq!(repo.head().unwrap(), Some(head));
    assert_eq!(repo.read_publication_journal().unwrap(), None);
    assert_eq!(fs::read(&index_path).unwrap(), raced);
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "raced staging"
    );
}

/// ORNA-PUB-005: every object the candidate adds must be durable before the
/// ref names it. An object that cannot be flushed refuses publication before
/// the CAS and leaves no journal behind.
#[test]
fn publication_flushes_candidate_objects_before_the_ref_names_them() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let (candidate, mut journal) =
        row_publication(&repo, &head, &index, b"candidate row\n", [19; 16]);
    let blob = git(
        root.path(),
        &[
            "rev-parse",
            &format!("{}:generated/row.orna", candidate.commit().as_str()),
        ],
    );
    let loose = root
        .path()
        .join(".git/objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    let aside = root.path().join(".git/orna-test-row-object");
    fs::rename(&loose, &aside).unwrap();
    // The object path no longer names a file that can be flushed.
    fs::create_dir(&loose).unwrap();

    assert!(matches!(
        repo.publish_candidate(&index, &candidate, &mut journal),
        Err(RepositoryError::Graph(
            orna_repository_v1::GraphError::DurabilityFailed
        ))
    ));
    assert_eq!(repo.head().unwrap(), Some(head));
    assert_eq!(repo.read_publication_journal().unwrap(), None);

    fs::remove_dir(&loose).unwrap();
    fs::rename(&aside, &loose).unwrap();
    repo.publish_candidate(&index, &candidate, &mut journal)
        .unwrap();
    repo.mark_runtime_complete([19; 16], &mut journal).unwrap();
    assert_eq!(repo.head().unwrap().as_ref(), Some(candidate.commit()));
}

/// A Prepared journal is discarded once HEAD moves to a commit that lacks the
/// candidate, and kept as a recovery conflict once HEAD descends from it.
#[test]
fn recovery_discards_a_prepared_journal_only_while_head_lacks_the_candidate() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let (_, journal) = row_publication(&repo, &head, &index, b"candidate row\n", [16; 16]);
    repo.write_publication_journal(&journal).unwrap();
    git(
        root.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "human commit"],
    );
    let human = repo.head().unwrap().unwrap();

    assert_eq!(
        repo.recover_publication().unwrap(),
        Some(repo.index_generation().unwrap())
    );
    assert_eq!(repo.read_publication_journal().unwrap(), None);
    assert_eq!(repo.head().unwrap(), Some(human));

    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let (candidate, journal) = row_publication(&repo, &head, &index, b"candidate row\n", [17; 16]);
    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();
    git(
        root.path(),
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "human commit on the candidate",
        ],
    );

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::StaleHead)
    ));
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

/// Recovery row "Ref no longer H or N": a CAS that landed before the
/// publisher died leaves the batch published on its branch. After the user
/// switches to another branch, recovery must keep the journal as a conflict
/// rather than discard it and publish the batch a second time (ORNA-PUB-008).
#[test]
fn recovery_keeps_a_prepared_journal_whose_candidate_another_branch_holds() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let head = repo.head().unwrap().unwrap();
    let index = repo.index_generation().unwrap();
    let (candidate, journal) = row_publication(&repo, &head, &index, b"candidate row\n", [20; 16]);
    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();
    git(
        root.path(),
        &["switch", "--quiet", "-c", "other", head.as_str()],
    );

    assert!(matches!(
        repo.recover_publication(),
        Err(RepositoryError::StaleHead)
    ));
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
    assert_eq!(repo.head().unwrap(), Some(head));
    assert_eq!(
        git(root.path(), &["rev-parse", "refs/heads/main"]),
        candidate.commit().as_str()
    );
}

#[test]
fn recovery_rejects_a_post_ref_journal_whose_bytes_differ_from_the_candidate() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [19; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"journal row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::InvalidPublicationJournal)
    ));
    assert_eq!(repo.head().unwrap(), Some(candidate.commit().clone()));
    assert_eq!(repo.index_generation().unwrap().tree(), index_before.tree());
    assert_eq!(repo.managed_file_bytes(&managed).unwrap(), None);
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

#[test]
fn recovery_rejects_a_journal_candidate_outside_the_recorded_head_lineage() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let old_head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let unrelated = git(
        root.path(),
        &[
            "commit-tree",
            "HEAD^{tree}",
            "-m",
            "unrelated recovery candidate",
        ],
    );
    git(root.path(), &["switch", "--detach", &unrelated]);
    let unrelated_head = repo.head().unwrap().unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        old_head,
        unrelated_head,
        index_before.tree().unwrap().clone(),
        [18; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&journal).unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::InvalidPublicationJournal)
    ));
    assert_eq!(repo.head().unwrap().unwrap(), *journal.new_head());
    assert_eq!(repo.index_generation().unwrap().tree(), index_before.tree());
    assert_eq!(repo.managed_file_bytes(&managed).unwrap(), None);
    assert_eq!(repo.read_publication_journal().unwrap(), Some(journal));
}

#[test]
fn recovery_preserves_post_ref_external_conflict_and_can_resume() {
    let root = repository();
    let repo = Repository::discover(root.path()).unwrap();
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let head = repo.head().unwrap().unwrap();
    let index_before = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &head,
            &[orna_repository_v1::ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = orna_repository_v1::PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().unwrap().clone(),
        [3; 16],
        vec![orna_repository_v1::PublicationJournalEntry::new(
            managed.clone(),
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    repo.write_publication_journal(&journal).unwrap();
    repo.advance_current_ref(&head, &candidate).unwrap();
    fs::create_dir_all(root.path().join("generated")).unwrap();
    fs::write(root.path().join(managed.as_path()), b"editor").unwrap();

    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::ManagedContentConflict)
    ));
    assert_eq!(
        fs::read(root.path().join(managed.as_path())).unwrap(),
        b"editor"
    );
    assert_eq!(
        repo.read_publication_journal().unwrap().unwrap().stage(),
        orna_repository_v1::PublicationJournalStage::IndexReconciled
    );

    fs::write(root.path().join(managed.as_path()), b"candidate row\n").unwrap();
    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired)
    ));
    let mut journal = repo.read_publication_journal().unwrap().unwrap();
    repo.mark_runtime_complete([3; 16], &mut journal).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
}

fn hex_digest(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn legacy_format_migration_uses_pub3_journal_and_preserves_user_state() {
    for format in [
        include_str!("fixtures/format-context/format-1-ornadb-1.0.orna"),
        include_str!("fixtures/format-context/format-2-draft-1.1.0.orna"),
    ] {
        let root = repository_with_legacy_format(format);
        let dependency_source = TempDir::new().unwrap();
        git(dependency_source.path(), &["init", "-b", "main"]);
        test_support::configure_fixture_git_identity(dependency_source.path());
        git(
            dependency_source.path(),
            &["config", "commit.gpgsign", "false"],
        );
        fs::write(
            dependency_source.path().join("dependency.txt"),
            "base dependency\n",
        )
        .unwrap();
        git(dependency_source.path(), &["add", "."]);
        git(
            dependency_source.path(),
            &["commit", "-m", "dependency fixture"],
        );
        git(
            root.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                dependency_source.path().to_str().unwrap(),
                "deps/sample",
            ],
        );
        git(root.path(), &["commit", "-m", "add dependency fixture"]);
        let repo = Repository::discover(root.path()).unwrap();
        let old_head = repo.head().unwrap().unwrap();

        fs::write(
            root.path().join("deps/sample/dependency.txt"),
            "dirty dependency worktree\n",
        )
        .unwrap();
        fs::write(
            root.path().join("main.orna"),
            include_str!("fixtures/git-repository-nested-tool.orna"),
        )
        .unwrap();
        fs::write(root.path().join("ordinary.txt"), "staged user edit\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);

        let expected_index = repo.index_generation().unwrap();
        let database = include_str!("fixtures/format-context/database-final.orna");
        let candidate = repo
            .build_private_commit(
                &old_head,
                &[
                    ManagedFileChange::new(
                        ManagedPath::new(".orna/database.orna").unwrap(),
                        Some(database.as_bytes().to_vec()),
                    ),
                    ManagedFileChange::new(ManagedPath::new(".orna/format.orna").unwrap(), None),
                    ManagedFileChange::new(
                        ManagedPath::new(".orna/store/root").unwrap(),
                        Some(b"verified candidate store root".to_vec()),
                    ),
                ],
                "explicit format 3 migration",
            )
            .unwrap();
        let intent = [71; 16];
        let journal_entries = vec![
            PublicationJournalEntry::new(
                ManagedPath::new(".orna/database.orna").unwrap(),
                None,
                Some(database.as_bytes().to_vec()),
            ),
            PublicationJournalEntry::new(
                ManagedPath::new(".orna/format.orna").unwrap(),
                Some(format.as_bytes().to_vec()),
                None,
            ),
            PublicationJournalEntry::new(
                ManagedPath::new(".orna/store/root").unwrap(),
                None,
                Some(b"verified candidate store root".to_vec()),
            ),
        ];
        let missing_continuity = PublicationJournal::new_with_runtime_intent(
            old_head.clone(),
            candidate.commit().clone(),
            expected_index.tree().unwrap().clone(),
            intent,
            journal_entries.clone(),
        )
        .unwrap();
        let mut missing_continuity = missing_continuity;
        assert!(matches!(
            repo.publish_legacy_format_migration(
                &expected_index,
                &candidate,
                &mut missing_continuity,
            ),
            Err(RepositoryError::InvalidFormatMigration)
        ));
        assert_eq!(repo.head().unwrap(), Some(old_head.clone()));
        assert!(repo.read_publication_journal().unwrap().is_none());

        let continuity = MigrationContinuityRecord::new(vec![
            StreamCheckpointIdentityPredecessor::new(
                [31; 32],
                b"legacy-source".to_vec(),
                None,
                [32; 32],
                b"format3-source".to_vec(),
                None,
            )
            .unwrap(),
        ])
        .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            old_head.clone(),
            candidate.commit().clone(),
            expected_index.tree().unwrap().clone(),
            intent,
            journal_entries,
        )
        .unwrap()
        .with_migration_continuity(continuity.clone())
        .unwrap()
        // The fixture has no legacy Blob rows, so no row's annotated value
        // identity changes; the coordinate is still required and reported.
        .with_migration_annotation_defaults(MigrationAnnotationDefaults::new(0).unwrap())
        .unwrap();

        repo.publish_legacy_format_migration(&expected_index, &candidate, &mut journal)
            .unwrap();

        assert_eq!(
            repo.open_format_context()
                .unwrap()
                .repository_format_number(),
            3
        );
        assert_eq!(journal.stage(), PublicationJournalStage::WorktreeReconciled);
        let persisted = repo.read_publication_journal().unwrap().unwrap();
        assert_eq!(persisted.runtime_intent_id(), Some(intent));
        assert_eq!(persisted.migration_continuity(), Some(&continuity));
        assert_eq!(
            persisted.stage(),
            PublicationJournalStage::WorktreeReconciled
        );
        assert_eq!(
            repo.read_committed_file(&old_head, ".orna/format.orna", format.len())
                .unwrap(),
            format.as_bytes()
        );
        assert!(!root.path().join(".orna/format.orna").exists());
        assert_eq!(
            fs::read(root.path().join(".orna/database.orna")).unwrap(),
            database.as_bytes()
        );
        let status = git(root.path(), &["status", "--short", "--untracked-files=all"]);
        assert!(
            status.lines().any(|line| line.ends_with("main.orna")),
            "{status}"
        );
        assert!(
            status.lines().any(|line| line.ends_with("ordinary.txt")),
            "{status}"
        );
        assert!(
            status.lines().any(|line| line.ends_with("deps/sample")),
            "{status}"
        );
        assert_eq!(
            fs::read(root.path().join("deps/sample/dependency.txt")).unwrap(),
            b"dirty dependency worktree\n"
        );
    }
}

#[test]
fn legacy_format_migration_preserves_protected_pin_refs_across_publication() {
    let format = include_str!("fixtures/format-context/format-1-ornadb-1.0.orna");
    let root = repository_with_legacy_format(format);
    let repo = Repository::discover(root.path()).unwrap();
    let old_head = repo.head().unwrap().unwrap();

    // A protected pin on the legacy head's own Blob bytes, created before the
    // migration. The migration must not drop or retarget it.
    let pinned_blob = git(
        root.path(),
        &["rev-parse", &format!("{}:main.orna", old_head.as_str())],
    );
    let pin_ref = "refs/orna/pins/legacy-fixture/pending/main";
    git(root.path(), &["update-ref", pin_ref, &pinned_blob]);

    let expected_index = repo.index_generation().unwrap();
    let database = include_str!("fixtures/format-context/database-final.orna");
    let candidate = repo
        .build_private_commit(
            &old_head,
            &[
                ManagedFileChange::new(
                    ManagedPath::new(".orna/database.orna").unwrap(),
                    Some(database.as_bytes().to_vec()),
                ),
                ManagedFileChange::new(ManagedPath::new(".orna/format.orna").unwrap(), None),
                ManagedFileChange::new(
                    ManagedPath::new(".orna/store/root").unwrap(),
                    Some(b"verified candidate store root".to_vec()),
                ),
            ],
            "explicit format 3 migration",
        )
        .unwrap();
    let intent = [72; 16];
    let journal_entries = vec![
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/database.orna").unwrap(),
            None,
            Some(database.as_bytes().to_vec()),
        ),
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/format.orna").unwrap(),
            Some(format.as_bytes().to_vec()),
            None,
        ),
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/store/root").unwrap(),
            None,
            Some(b"verified candidate store root".to_vec()),
        ),
    ];
    let continuity = MigrationContinuityRecord::new(vec![
        StreamCheckpointIdentityPredecessor::new(
            [41; 32],
            b"legacy-source".to_vec(),
            None,
            [42; 32],
            b"format3-source".to_vec(),
            None,
        )
        .unwrap(),
    ])
    .unwrap();
    let mut journal = PublicationJournal::new_with_runtime_intent(
        old_head.clone(),
        candidate.commit().clone(),
        expected_index.tree().unwrap().clone(),
        intent,
        journal_entries,
    )
    .unwrap()
    .with_migration_continuity(continuity)
    .unwrap()
    .with_migration_annotation_defaults(MigrationAnnotationDefaults::new(0).unwrap())
    .unwrap();

    repo.publish_legacy_format_migration(&expected_index, &candidate, &mut journal)
        .unwrap();

    assert_eq!(
        repo.open_format_context()
            .unwrap()
            .repository_format_number(),
        3
    );
    assert_eq!(
        git(root.path(), &["rev-parse", pin_ref]),
        pinned_blob,
        "migration must keep the protected pin pointing at the same Blob"
    );
    assert_eq!(git(root.path(), &["cat-file", "-t", &pinned_blob]), "blob");
    assert_eq!(
        git(
            root.path(),
            &["show", &format!("{}:main.orna", old_head.as_str())]
        ),
        git(root.path(), &["show", &format!("{pinned_blob}")])
    );
}

#[test]
fn recovery_refuses_to_finish_an_unstarted_legacy_migration_and_keeps_its_pin() {
    let format = include_str!("fixtures/format-context/format-1-ornadb-1.0.orna");
    let root = repository_with_legacy_format(format);
    let repo = Repository::discover(root.path()).unwrap();
    let old_head = repo.head().unwrap().unwrap();

    let pinned_blob = git(
        root.path(),
        &["rev-parse", &format!("{}:main.orna", old_head.as_str())],
    );
    let pin_ref = "refs/orna/pins/legacy-fixture/pending/main";
    git(root.path(), &["update-ref", pin_ref, &pinned_blob]);

    let expected_index = repo.index_generation().unwrap();
    let database = include_str!("fixtures/format-context/database-final.orna");
    let candidate = repo
        .build_private_commit(
            &old_head,
            &[
                ManagedFileChange::new(
                    ManagedPath::new(".orna/database.orna").unwrap(),
                    Some(database.as_bytes().to_vec()),
                ),
                ManagedFileChange::new(ManagedPath::new(".orna/format.orna").unwrap(), None),
                ManagedFileChange::new(
                    ManagedPath::new(".orna/store/root").unwrap(),
                    Some(b"verified candidate store root".to_vec()),
                ),
            ],
            "explicit format 3 migration",
        )
        .unwrap();
    let journal_entries = vec![
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/database.orna").unwrap(),
            None,
            Some(database.as_bytes().to_vec()),
        ),
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/format.orna").unwrap(),
            Some(format.as_bytes().to_vec()),
            None,
        ),
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/store/root").unwrap(),
            None,
            Some(b"verified candidate store root".to_vec()),
        ),
    ];
    let continuity = MigrationContinuityRecord::new(vec![
        StreamCheckpointIdentityPredecessor::new(
            [41; 32],
            b"legacy-source".to_vec(),
            None,
            [42; 32],
            b"format3-source".to_vec(),
            None,
        )
        .unwrap(),
    ])
    .unwrap();
    let journal = PublicationJournal::new_with_runtime_intent(
        old_head.clone(),
        candidate.commit().clone(),
        expected_index.tree().unwrap().clone(),
        [72; 16],
        journal_entries,
    )
    .unwrap()
    .with_migration_continuity(continuity)
    .unwrap()
    .with_migration_annotation_defaults(MigrationAnnotationDefaults::new(0).unwrap())
    .unwrap();

    // Simulate a crash after the journal is durable and before publication.
    repo.write_publication_journal(&journal).unwrap();

    // A prepared journal whose head has not moved is still owned by its
    // publisher, so recovery must refuse to finish it and leave both the head
    // and the protected pin exactly as they were.
    assert!(matches!(
        repo.recover_publication(),
        Err(orna_repository_v1::RepositoryError::PublicationPending)
    ));
    assert_eq!(
        git(root.path(), &["rev-parse", "HEAD"]),
        old_head.as_str(),
        "recovery must not advance the head of an unpublished migration"
    );
    assert_eq!(
        git(root.path(), &["rev-parse", pin_ref]),
        pinned_blob,
        "recovery must keep the protected pin pointing at the same Blob"
    );
}

#[test]
fn legacy_format_migration_rejects_implicit_relabel_without_publication() {
    let format = include_str!("fixtures/format-context/format-1-ornadb-1.0.orna");
    let root = repository_with_legacy_format(format);
    let repo = Repository::discover(root.path()).unwrap();
    let old_head = repo.head().unwrap().unwrap();
    let expected_index = repo.index_generation().unwrap();
    let candidate = repo
        .build_private_commit(
            &old_head,
            &[ManagedFileChange::new(
                ManagedPath::new("main.orna").unwrap(),
                Some(
                    include_str!("fixtures/git-repository-nested-tool.orna")
                        .as_bytes()
                        .to_vec(),
                ),
            )],
            "candidate that keeps legacy metadata",
        )
        .unwrap();
    let mut journal = PublicationJournal::new_with_runtime_intent(
        old_head.clone(),
        candidate.commit().clone(),
        expected_index.tree().unwrap().clone(),
        [72; 16],
        vec![PublicationJournalEntry::new(
            ManagedPath::new("main.orna").unwrap(),
            Some(
                include_str!("fixtures/git-repository-main.orna")
                    .as_bytes()
                    .to_vec(),
            ),
            Some(
                include_str!("fixtures/git-repository-nested-tool.orna")
                    .as_bytes()
                    .to_vec(),
            ),
        )],
    )
    .unwrap();

    assert!(matches!(
        repo.publish_legacy_format_migration(&expected_index, &candidate, &mut journal),
        Err(RepositoryError::InvalidFormatMigration)
    ));
    assert_eq!(repo.head().unwrap(), Some(old_head));
    assert!(repo.read_publication_journal().unwrap().is_none());
}

#[test]
fn legacy_format_migration_requires_and_reports_the_annotation_default_coordinate() {
    for format in [
        include_str!("fixtures/format-context/format-1-ornadb-1.0.orna"),
        include_str!("fixtures/format-context/format-2-draft-1.1.0.orna"),
    ] {
        let root = repository_with_legacy_format(format);
        let repo = Repository::discover(root.path()).unwrap();
        let old_head = repo.head().unwrap().unwrap();

        // The decoded legacy row metadata: one row already carries a
        // representable annotation, one is recorded without any. Payload bytes
        // are never read, so this inventory is exactly what the migration knows.
        let rows = vec![
            LegacyAnnotationCandidate::new(
                b"row-labelled".to_vec(),
                Some("audio/mpeg".to_owned()),
                Some("mp2".to_owned()),
            ),
            LegacyAnnotationCandidate::new(b"row-unlabelled".to_vec(), None, None),
        ];
        let defaults = MigrationAnnotationDefaults::from_legacy_inventory(&rows).unwrap();
        assert_eq!(defaults.entry_count(), 1);
        assert!(!defaults.payload_inspected());

        // A legacy annotation that format 3 cannot represent fails the
        // migration rather than being downcast to bytes.
        let unrepresentable = vec![LegacyAnnotationCandidate::new(
            b"row-unrepresentable".to_vec(),
            Some("audio/*".to_owned()),
            Some("mp3".to_owned()),
        )];
        assert!(matches!(
            MigrationAnnotationDefaults::from_legacy_inventory(&unrepresentable),
            Err(RepositoryError::InvalidFormatMigration)
        ));

        let expected_index = repo.index_generation().unwrap();
        let database = include_str!("fixtures/format-context/database-final.orna");
        let store_root = b"verified candidate store root";
        let candidate = repo
            .build_private_commit(
                &old_head,
                &[
                    ManagedFileChange::new(
                        ManagedPath::new(".orna/database.orna").unwrap(),
                        Some(database.as_bytes().to_vec()),
                    ),
                    ManagedFileChange::new(ManagedPath::new(".orna/format.orna").unwrap(), None),
                    ManagedFileChange::new(
                        ManagedPath::new(".orna/store/root").unwrap(),
                        Some(store_root.to_vec()),
                    ),
                ],
                "explicit format 3 migration",
            )
            .unwrap();
        let intent = [72; 16];
        let journal_entries = vec![
            PublicationJournalEntry::new(
                ManagedPath::new(".orna/database.orna").unwrap(),
                None,
                Some(database.as_bytes().to_vec()),
            ),
            PublicationJournalEntry::new(
                ManagedPath::new(".orna/format.orna").unwrap(),
                Some(format.as_bytes().to_vec()),
                None,
            ),
            PublicationJournalEntry::new(
                ManagedPath::new(".orna/store/root").unwrap(),
                None,
                Some(store_root.to_vec()),
            ),
        ];
        let continuity = MigrationContinuityRecord::new(vec![
            StreamCheckpointIdentityPredecessor::new(
                [51; 32],
                b"legacy-source".to_vec(),
                None,
                [52; 32],
                b"format3-source".to_vec(),
                None,
            )
            .unwrap(),
        ])
        .unwrap();
        let base = PublicationJournal::new_with_runtime_intent(
            old_head.clone(),
            candidate.commit().clone(),
            expected_index.tree().unwrap().clone(),
            intent,
            journal_entries,
        )
        .unwrap();

        // Byte identity of the old commit is untouched: the migration is
        // refused before any ref, index, or worktree change.
        let mut without_annotation_defaults = base
            .clone()
            .with_migration_continuity(continuity.clone())
            .unwrap();
        assert!(matches!(
            repo.publish_legacy_format_migration(
                &expected_index,
                &candidate,
                &mut without_annotation_defaults,
            ),
            Err(RepositoryError::InvalidFormatMigration)
        ));
        assert_eq!(repo.head().unwrap(), Some(old_head.clone()));
        assert!(repo.read_publication_journal().unwrap().is_none());
        assert_eq!(
            repo.read_committed_file(&old_head, ".orna/format.orna", format.len())
                .unwrap(),
            format.as_bytes()
        );

        // The reported coordinate survives the journal round trip that a
        // process restart performs before the publication is resumed.
        let mut journal = base
            .with_migration_continuity(continuity.clone())
            .unwrap()
            .with_migration_annotation_defaults(defaults)
            .unwrap();
        assert_eq!(journal.migration_annotation_defaults(), Some(defaults));

        // Resume: a publisher that reopens the prepared journal must report the
        // same durable coordinate, and cannot substitute another one.
        assert_eq!(
            journal
                .clone()
                .resume_migration_annotation_defaults(defaults)
                .unwrap(),
            defaults
        );
        assert!(matches!(
            journal
                .clone()
                .resume_migration_annotation_defaults(MigrationAnnotationDefaults::new(2).unwrap()),
            Err(RepositoryError::InvalidFormatMigration)
        ));
        // A journal without the recorded coordinate is a pre-v7 migration and
        // has nothing to resume.
        assert!(matches!(
            without_annotation_defaults
                .clone()
                .resume_migration_annotation_defaults(defaults),
            Err(RepositoryError::InvalidFormatMigration)
        ));

        repo.publish_legacy_format_migration(&expected_index, &candidate, &mut journal)
            .unwrap();

        let persisted = repo.read_publication_journal().unwrap().unwrap();
        assert_eq!(persisted.migration_continuity(), Some(&continuity));
        assert_eq!(persisted.migration_annotation_defaults(), Some(defaults));
        assert_eq!(
            persisted.stage(),
            PublicationJournalStage::WorktreeReconciled
        );
        assert_eq!(
            repo.open_format_context()
                .unwrap()
                .repository_format_number(),
            3
        );
        assert_eq!(
            repo.read_committed_file(&old_head, ".orna/format.orna", format.len())
                .unwrap(),
            format.as_bytes()
        );
        assert_eq!(
            fs::read(root.path().join(".orna/store/root")).unwrap(),
            store_root
        );
    }
}

#[test]
fn resumed_legacy_migration_reports_the_recorded_successor_identity_without_resetting_it() {
    let format = include_str!("fixtures/format-context/format-1-ornadb-1.0.orna");
    let root = repository_with_legacy_format(format);
    let repo = Repository::discover(root.path()).unwrap();
    let old_head = repo.head().unwrap().unwrap();

    // The legacy stream's committed consumer, source and partition coordinates.
    let legacy_consumer = [61; 32];
    let legacy_source = b"events".to_vec();
    let legacy_partition = Some(b"shard-0".to_vec());
    let format3_consumer = [62; 32];
    let format3_source = b"stream-events".to_vec();
    let format3_partition = Some(b"partition-0".to_vec());

    let expected_index = repo.index_generation().unwrap();
    let database = include_str!("fixtures/format-context/database-final.orna");
    let store_root = b"verified candidate store root";
    let candidate = repo
        .build_private_commit(
            &old_head,
            &[
                ManagedFileChange::new(
                    ManagedPath::new(".orna/database.orna").unwrap(),
                    Some(database.as_bytes().to_vec()),
                ),
                ManagedFileChange::new(ManagedPath::new(".orna/format.orna").unwrap(), None),
                ManagedFileChange::new(
                    ManagedPath::new(".orna/store/root").unwrap(),
                    Some(store_root.to_vec()),
                ),
            ],
            "explicit format 3 migration",
        )
        .unwrap();
    let journal_entries = vec![
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/database.orna").unwrap(),
            None,
            Some(database.as_bytes().to_vec()),
        ),
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/format.orna").unwrap(),
            Some(format.as_bytes().to_vec()),
            None,
        ),
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/store/root").unwrap(),
            None,
            Some(store_root.to_vec()),
        ),
    ];
    let continuity = MigrationContinuityRecord::new(vec![
        StreamCheckpointIdentityPredecessor::new(
            legacy_consumer,
            legacy_source.clone(),
            legacy_partition.clone(),
            format3_consumer,
            format3_source.clone(),
            format3_partition.clone(),
        )
        .unwrap(),
        // A second stream whose successor deliberately has no partition.
        StreamCheckpointIdentityPredecessor::new(
            [63; 32],
            b"metrics".to_vec(),
            Some(b"shard-1".to_vec()),
            [64; 32],
            b"stream-metrics".to_vec(),
            None,
        )
        .unwrap(),
    ])
    .unwrap();
    let defaults = MigrationAnnotationDefaults::new(0).unwrap();
    let journal = PublicationJournal::new_with_runtime_intent(
        old_head.clone(),
        candidate.commit().clone(),
        expected_index.tree().unwrap().clone(),
        [73; 16],
        journal_entries,
    )
    .unwrap()
    .with_migration_continuity(continuity.clone())
    .unwrap()
    .with_migration_annotation_defaults(defaults)
    .unwrap();

    // A process restart persists the prepared journal, then reopens it. The
    // successor coordinate must be the migration's recorded one, not a
    // re-derived identity: re-deriving is the reset this record prevents.
    repo.write_publication_journal(&journal).unwrap();
    let reopened = repo.read_publication_journal().unwrap().unwrap();
    let resumed = reopened.migration_continuity().unwrap();
    assert_eq!(
        resumed.successor_for(legacy_consumer, &legacy_source, legacy_partition.as_deref()),
        Some((
            format3_consumer,
            format3_source.as_slice(),
            format3_partition.as_deref()
        ))
    );

    // `None` partition stays distinct from an encoded empty partition.
    assert_eq!(
        resumed.successor_for([63; 32], b"metrics", Some(b"shard-1")),
        Some(([64; 32], b"stream-metrics".as_slice(), None))
    );
    assert_eq!(
        resumed.successor_for([63; 32], b"metrics", Some(b"")),
        None,
        "an empty partition is not the absent partition"
    );

    // A legacy identity the migration never carried has no successor, and a
    // legacy key cannot be answered with another key's successor.
    assert_eq!(
        resumed.successor_for([99; 32], &legacy_source, legacy_partition.as_deref()),
        None
    );
    assert_eq!(
        resumed.successor_for(
            legacy_consumer,
            b"other-source",
            legacy_partition.as_deref()
        ),
        None
    );

    // The journal resumes and publishes with the recorded coordinate intact:
    // the resumed publisher still reports exactly what it recorded before.
    let mut resumed_journal = reopened.clone();
    assert_eq!(
        resumed_journal
            .resume_migration_annotation_defaults(defaults)
            .unwrap(),
        defaults
    );
    // The stream/checkpoint inventory resumes the same way: the journal already
    // carries it, so the resumed publisher re-admits what is recorded rather
    // than re-deriving an identity. The returned record is the durable one.
    assert_eq!(
        resumed_journal
            .resume_migration_continuity(continuity.clone())
            .unwrap(),
        continuity
    );
    // A substituted inventory cannot replace the recorded one: a publisher that
    // re-keys the migrated stream is refused instead of resetting its identity.
    let rekeyed = MigrationContinuityRecord::new(vec![
        StreamCheckpointIdentityPredecessor::new(
            legacy_consumer,
            legacy_source.clone(),
            legacy_partition.clone(),
            [77; 32],
            b"stream-rekeyed".to_vec(),
            format3_partition.clone(),
        )
        .unwrap(),
    ])
    .unwrap();
    assert!(matches!(
        resumed_journal
            .resume_migration_continuity(rekeyed)
            .unwrap_err(),
        RepositoryError::InvalidFormatMigration
    ));
    // The re-admitted inventory answers the migrated stream's successor, and it
    // is still the one the resumed publisher captured before publication.
    let resumed_continuity = resumed_journal.migration_continuity().unwrap();
    assert_eq!(
        resumed_continuity
            .successor_for(legacy_consumer, &legacy_source, legacy_partition.as_deref())
            .map(|(consumer, source, partition)| (
                consumer,
                source.to_vec(),
                partition.map(<[u8]>::to_vec)
            )),
        Some((
            format3_consumer,
            format3_source.clone(),
            format3_partition.clone()
        ))
    );
    let source_identity = resumed
        .successor_for(legacy_consumer, &legacy_source, legacy_partition.as_deref())
        .expect("the resumed stream keeps its successor identity");
    assert_eq!(source_identity.0, format3_consumer);
    repo.publish_legacy_format_migration(&expected_index, &candidate, &mut resumed_journal)
        .unwrap();
    let persisted = repo.read_publication_journal().unwrap().unwrap();
    assert_eq!(
        persisted.migration_continuity().unwrap().successor_for(
            legacy_consumer,
            &legacy_source,
            legacy_partition.as_deref()
        ),
        Some((
            format3_consumer,
            format3_source.as_slice(),
            format3_partition.as_deref()
        )),
        "publication must not change the recorded successor identity"
    );
    assert_eq!(
        repo.open_format_context()
            .unwrap()
            .repository_format_number(),
        3
    );
}

#[test]
fn interrupted_legacy_migration_recovers_without_resetting_the_recorded_consumer_identity() {
    // Gate F resume: a migrated stream must survive a process interruption with
    // its consumer/checkpoint identity intact. The journal crosses the failure
    // at a durable stage, and recovery completes the migration from the record
    // instead of re-keying the streams it already carried.
    let format = include_str!("fixtures/format-context/format-1-ornadb-1.0.orna");
    let root = repository_with_legacy_format(format);
    let repo = Repository::discover(root.path()).unwrap();
    let old_head = repo.head().unwrap().unwrap();
    let expected_index = repo.index_generation().unwrap();
    let database = include_str!("fixtures/format-context/database-final.orna");
    let store_root = b"verified candidate store root";
    let candidate = repo
        .build_private_commit(
            &old_head,
            &[
                ManagedFileChange::new(
                    ManagedPath::new(".orna/database.orna").unwrap(),
                    Some(database.as_bytes().to_vec()),
                ),
                ManagedFileChange::new(ManagedPath::new(".orna/format.orna").unwrap(), None),
                ManagedFileChange::new(
                    ManagedPath::new(".orna/store/root").unwrap(),
                    Some(store_root.to_vec()),
                ),
            ],
            "interrupted format 3 migration",
        )
        .unwrap();
    let journal_entries = vec![
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/database.orna").unwrap(),
            None,
            Some(database.as_bytes().to_vec()),
        ),
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/format.orna").unwrap(),
            Some(format.as_bytes().to_vec()),
            None,
        ),
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/store/root").unwrap(),
            None,
            Some(store_root.to_vec()),
        ),
    ];
    let legacy_consumer = [81; 32];
    let legacy_source = b"ledger".to_vec();
    let format3_consumer = [82; 32];
    let format3_source = b"stream-ledger".to_vec();
    let continuity = MigrationContinuityRecord::new(vec![
        StreamCheckpointIdentityPredecessor::new(
            legacy_consumer,
            legacy_source.clone(),
            Some(b"shard-0".to_vec()),
            format3_consumer,
            format3_source.clone(),
            Some(b"partition-0".to_vec()),
        )
        .unwrap(),
    ])
    .unwrap();
    let intent = [91; 16];
    let mut journal = PublicationJournal::new_with_runtime_intent(
        old_head.clone(),
        candidate.commit().clone(),
        expected_index.tree().unwrap().clone(),
        intent,
        journal_entries,
    )
    .unwrap()
    .with_migration_continuity(continuity.clone())
    .unwrap()
    .with_migration_annotation_defaults(MigrationAnnotationDefaults::new(0).unwrap())
    .unwrap();

    // The migration reaches its durable stage, then the process dies before the
    // embedder records runtime completion.
    repo.publish_legacy_format_migration(&expected_index, &candidate, &mut journal)
        .unwrap();
    assert_eq!(journal.stage(), PublicationJournalStage::WorktreeReconciled);
    assert_eq!(
        repo.open_format_context()
            .unwrap()
            .repository_format_number(),
        3
    );

    // A fresh process reopens the durable journal. It recovers the recorded
    // stream identity rather than reconstructing it from the migrated rows.
    let mut resumed = repo.read_publication_journal().unwrap().unwrap();
    assert_eq!(resumed.runtime_intent_id(), Some(intent));
    assert_eq!(resumed.migration_continuity(), Some(&continuity));
    // Resume at the durable publication boundary, not only while Prepared.
    let defaults = MigrationAnnotationDefaults::new(0).unwrap();
    assert_eq!(
        resumed
            .resume_migration_annotation_defaults(defaults)
            .unwrap(),
        defaults
    );
    assert_eq!(
        resumed
            .resume_migration_continuity(continuity.clone())
            .unwrap(),
        continuity
    );
    assert_eq!(
        resumed.migration_continuity().unwrap().successor_for(
            legacy_consumer,
            &legacy_source,
            Some(b"shard-0")
        ),
        Some((
            format3_consumer,
            format3_source.as_slice(),
            Some(b"partition-0".as_slice())
        )),
        "recovery must not re-derive the successor identity"
    );

    // Completion still requires the runtime intent, which the resumed embedder
    // holds; recovery refuses to finish it independently.
    assert!(matches!(
        repo.recover_publication().unwrap_err(),
        RepositoryError::RuntimeCompletionRequired
    ));
    assert_eq!(
        repo.read_publication_journal().unwrap().unwrap(),
        resumed,
        "a refused recovery leaves the durable record untouched"
    );

    // The resumed embedder completes the migration under its recorded identity.
    repo.mark_runtime_complete(intent, &mut resumed).unwrap();
    assert_eq!(repo.read_publication_journal().unwrap(), None);
    assert_eq!(repo.head().unwrap().unwrap(), candidate.commit().clone());
    assert_eq!(
        fs::read(root.path().join(".orna/database.orna")).unwrap(),
        database.as_bytes()
    );
    assert!(!root.path().join(".orna/format.orna").exists());
}

#[test]
fn interrupted_legacy_migration_recovers_new_snapshot_and_preserves_checkout_edits() {
    let format = include_str!("fixtures/format-context/format-1-ornadb-1.0.orna");
    let root = repository_with_legacy_format(format);
    let repo = Repository::discover(root.path()).unwrap();
    let old_head = repo.head().unwrap().unwrap();

    // A staged user edit is outside the migration journal and must survive the
    // ref transition and recovery reconciliation unchanged.
    fs::write(root.path().join("ordinary.txt"), "staged user edit\n").unwrap();
    git(root.path(), &["add", "ordinary.txt"]);
    let expected_index = repo.index_generation().unwrap();
    let database = include_str!("fixtures/format-context/database-final.orna");
    let store_root = b"verified candidate store root";
    let candidate = repo
        .build_private_commit(
            &old_head,
            &[
                ManagedFileChange::new(
                    ManagedPath::new(".orna/database.orna").unwrap(),
                    Some(database.as_bytes().to_vec()),
                ),
                ManagedFileChange::new(ManagedPath::new(".orna/format.orna").unwrap(), None),
                ManagedFileChange::new(
                    ManagedPath::new(".orna/store/root").unwrap(),
                    Some(store_root.to_vec()),
                ),
            ],
            "interrupted explicit format 3 migration",
        )
        .unwrap();
    let entries = vec![
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/database.orna").unwrap(),
            None,
            Some(database.as_bytes().to_vec()),
        ),
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/format.orna").unwrap(),
            Some(format.as_bytes().to_vec()),
            None,
        ),
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/store/root").unwrap(),
            None,
            Some(store_root.to_vec()),
        ),
    ];
    let continuity =
        MigrationContinuityRecord::new(vec![StreamCheckpointIdentityPredecessor::new(
            [91; 32],
            b"legacy-source".to_vec(),
            Some(b"legacy-partition".to_vec()),
            [92; 32],
            b"format3-source".to_vec(),
            Some(b"format3-partition".to_vec()),
        )
        .unwrap()])
        .unwrap();
    let intent = [93; 16];
    let journal = PublicationJournal::new_with_runtime_intent(
        old_head.clone(),
        candidate.commit().clone(),
        expected_index.tree().unwrap().clone(),
        intent,
        entries,
    )
    .unwrap()
    .with_migration_continuity(continuity)
    .unwrap()
    .with_migration_annotation_defaults(MigrationAnnotationDefaults::new(0).unwrap())
    .unwrap();

    // Crash after the branch ref moves but before the journal records that
    // boundary. The checkout remains on its original branch; recovery must
    // finish the prepared transition rather than expose a mixed snapshot.
    repo.write_publication_journal(&journal).unwrap();
    assert_eq!(
        git(root.path(), &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(
        git(root.path(), &["rev-parse", "--show-toplevel"]),
        root.path().to_str().unwrap()
    );
    assert_eq!(repo.head().unwrap(), Some(old_head.clone()));
    assert_eq!(
        repo.open_format_context()
            .unwrap()
            .repository_format_number(),
        1
    );
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged user edit"
    );
    git(
        root.path(),
        &[
            "update-ref",
            "refs/heads/main",
            candidate.commit().as_str(),
            old_head.as_str(),
        ],
    );

    assert!(matches!(
        repo.recover_publication(),
        Err(RepositoryError::RuntimeCompletionRequired)
    ));
    assert_eq!(repo.head().unwrap(), Some(candidate.commit().clone()));
    assert_eq!(
        git(root.path(), &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(
        git(root.path(), &["rev-parse", "--show-toplevel"]),
        root.path().to_str().unwrap()
    );
    assert_eq!(
        repo.open_format_context()
            .unwrap()
            .repository_format_number(),
        3
    );
    assert_eq!(
        repo.read_committed_file(&old_head, ".orna/format.orna", 64)
            .unwrap(),
        format.as_bytes(),
        "the legacy snapshot remains readable under its original format"
    );
    assert_eq!(
        fs::read(root.path().join(".orna/database.orna")).unwrap(),
        database.as_bytes()
    );
    assert_eq!(
        fs::read(root.path().join(".orna/store/root")).unwrap(),
        store_root
    );
    assert!(!root.path().join(".orna/format.orna").exists());
    assert_eq!(
        git(root.path(), &["show", ":ordinary.txt"]),
        "staged user edit",
        "recovery must preserve the user's staged blob"
    );
    assert!(git(root.path(), &["diff", "--cached", "--name-only"])
        .lines()
        .any(|path| path == "ordinary.txt"));
    assert_eq!(
        fs::read(root.path().join("ordinary.txt")).unwrap(),
        b"staged user edit\n",
        "recovery must preserve the checked-out user file"
    );

    let mut resumed = repo.read_publication_journal().unwrap().unwrap();
    assert_eq!(resumed.stage(), PublicationJournalStage::WorktreeReconciled);
    repo.mark_runtime_complete(intent, &mut resumed).unwrap();
    assert!(repo.read_publication_journal().unwrap().is_none());
    assert_eq!(repo.head().unwrap(), Some(candidate.commit().clone()));
    assert!(git(root.path(), &["diff", "--cached", "--name-only"])
        .lines()
        .any(|path| path == "ordinary.txt"));
}

#[test]
fn disposable_migration_copy_preserves_legacy_bytes_and_reports_its_coordinates() {
    for (format, format_number) in [
        (include_str!("fixtures/format-context/format-1-ornadb-1.0.orna"), 1u8),
        (include_str!("fixtures/format-context/format-2-draft-1.1.0.orna"), 2u8),
    ] {
        let source = repository_with_legacy_format(format);
        let module_source = TempDir::new().unwrap();
        git(module_source.path(), &["init", "-b", "main"]);
        test_support::configure_fixture_git_identity(module_source.path());
        git(module_source.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(
            module_source.path().join("main.orna"),
            include_str!("fixtures/git-repository-main.orna"),
        )
        .unwrap();
        fs::write(module_source.path().join("module.txt"), "module base\n").unwrap();
        git(module_source.path(), &["add", "."]);
        git(module_source.path(), &["commit", "-m", "module base"]);
        git(
            source.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                module_source.path().to_str().unwrap(),
                "deps/lib",
            ],
        );
        git(source.path(), &["commit", "-m", "add module"]);
        fs::write(source.path().join("removed.txt"), "tracked before removal\n").unwrap();
        git(source.path(), &["add", "removed.txt"]);
        git(source.path(), &["commit", "-m", "add tracked file"]);
        let repo = Repository::discover(source.path()).unwrap();
        let source_head = repo.head().unwrap().unwrap();
        // Preserve an unrelated staged version separately from its later
        // unstaged edit, plus untracked and ignored user data.
        fs::write(source.path().join("ordinary.txt"), "staged\n").unwrap();
        git(source.path(), &["add", "ordinary.txt"]);
        fs::remove_file(source.path().join("removed.txt")).unwrap();
        git(source.path(), &["add", "-u", "--", "removed.txt"]);
        let source_index = repo.index_generation().unwrap();
        fs::write(source.path().join("ordinary.txt"), "dirty\n").unwrap();
        fs::write(source.path().join("untracked.txt"), "untracked\n").unwrap();
        fs::write(source.path().join(".gitignore"), "ignored/\n").unwrap();
        fs::create_dir_all(source.path().join("ignored")).unwrap();
        fs::write(
            source.path().join("ignored/payload.bin"),
            b"ignored payload",
        )
        .unwrap();
        let module_worktree = source.path().join("deps/lib");
        fs::write(module_worktree.join("module.txt"), "module staged\n").unwrap();
        git(&module_worktree, &["add", "module.txt"]);
        fs::write(module_worktree.join("module.txt"), "module dirty\n").unwrap();
        let directory = TempDir::new().unwrap();
        let continuity = MigrationContinuityRecord::new(vec![
            StreamCheckpointIdentityPredecessor::new(
                [61; 32],
                b"legacy-stream-source".to_vec(),
                Some(b"legacy-partition".to_vec()),
                [62; 32],
                b"format3-stream-source".to_vec(),
                Some(b"format3-partition".to_vec()),
            )
            .unwrap(),
        ])
        .unwrap();
        let defaults = MigrationAnnotationDefaults::new(7).unwrap();
        let destination = directory.path().join("migrated");

        let copy = orna_repository_v1::migration_copy::prepare_format3_migration(
            &repo,
            &destination,
            continuity.clone(),
            defaults,
        )
        .unwrap();

        // The source is untouched: same head, same recorded format, same
        // working tree, including the dirty and ignored paths.
        assert_eq!(repo.head().unwrap(), Some(source_head.clone()));
        assert_eq!(repo.index_generation().unwrap(), source_index);
        assert_eq!(
            repo.open_format_context()
                .unwrap()
                .repository_format_number(),
            format_number
        );
        assert_eq!(
            fs::read_to_string(source.path().join("ordinary.txt")).unwrap(),
            "dirty\n"
        );
        assert_eq!(git(source.path(), &["show", ":ordinary.txt"]), "staged");
        assert!(!source.path().join("removed.txt").exists());
        assert_eq!(
            git(&module_worktree, &["show", ":module.txt"]),
            "module staged"
        );
        assert_eq!(
            git(&module_worktree, &["diff", "--cached", "--name-only"]),
            "module.txt"
        );
        assert_eq!(
            fs::read(source.path().join("ignored/payload.bin")).unwrap(),
            b"ignored payload"
        );

        // The copy carries the source's commits under their original identity
        // and its own content, including the paths Git ignores.
        let migrated = Repository::discover(&copy.destination()).unwrap();
        assert_eq!(copy.source_commit(), &source_head);
        assert_eq!(migrated.head().unwrap(), Some(source_head.clone()));
        assert_eq!(
            git(copy.destination(), &["show", ":ordinary.txt"]),
            "staged"
        );
        assert_eq!(
            git(copy.destination(), &["diff", "--cached", "--name-only"]),
            "ordinary.txt\nremoved.txt"
        );
        assert_eq!(
            git(copy.destination(), &["diff", "--name-only"]),
            "deps/lib\nordinary.txt"
        );
        assert!(!copy.destination().join("removed.txt").exists());
        let migrated_module = copy.destination().join("deps/lib");
        assert_eq!(
            git(&migrated_module, &["rev-parse", "--show-toplevel"]),
            migrated_module.to_string_lossy()
        );
        assert_eq!(
            git(&migrated_module, &["show", ":module.txt"]),
            "module staged"
        );
        assert_eq!(
            git(&migrated_module, &["diff", "--cached", "--name-only"]),
            "module.txt"
        );
        assert_eq!(
            git(&migrated_module, &["diff", "--name-only"]),
            "module.txt"
        );
        assert_eq!(
            fs::read_to_string(migrated_module.join("module.txt")).unwrap(),
            "module dirty\n"
        );
        // The journalled migration commit is the one that establishes the
        // migrated representation, and it names its own coordinate as a Git
        // trailer so a history walk can report the transition instead of
        // leaving the reader to infer it from the message prose.
        let candidate_message = git(
            copy.destination(),
            &[
                "log",
                "-1",
                "--format=%B",
                copy.candidate_journal().new_head().as_str(),
            ],
        );
        assert_eq!(
            candidate_message,
            format!(
                "explicit format {format_number} to 3 migration: 7 rows take the MIME-1 default \
                 annotation, 1 stream checkpoint identities carried\n\n\
                 Orna-Migration: format-{format_number}-to-3"
            ),
            "the candidate states the migration coordinate as a trailer"
        );
        // Git only recognizes a trailer after a blank line; a marker written
        // into the summary would not be one.
        assert_eq!(
            git(
                copy.destination(),
                &[
                    "log",
                    "-1",
                    "--format=%(trailers:key=Orna-Migration,valueonly)",
                    copy.candidate_journal().new_head().as_str(),
                ]
            ),
            format!("format-{format_number}-to-3")
        );
        assert_eq!(
            fs::read_to_string(copy.destination().join("ordinary.txt")).unwrap(),
            "dirty\n"
        );
        assert_eq!(
            fs::read_to_string(copy.destination().join("untracked.txt")).unwrap(),
            "untracked\n"
        );
        assert_eq!(
            fs::read(copy.destination().join("ignored/payload.bin")).unwrap(),
            b"ignored payload"
        );
        assert_eq!(
            fs::read_to_string(copy.destination().join("main.orna")).unwrap(),
            include_str!("fixtures/git-repository-main.orna")
        );
        // The copy retired nothing yet: preparation preserves the source's own
        // `.orna` bytes, and the format-3 records live in the candidate, which
        // is exactly what keeps the migration unpacked-but-unpublished.
        assert_eq!(
            fs::read_to_string(copy.destination().join(".orna/format.orna")).unwrap(),
            format
        );
        // Neither the source nor the copy admits canonical metadata: a legacy
        // workspace carries the format record and no database record, which the
        // reader reports as incomplete metadata rather than a usable identity.
        // The copy deliberately stays in that state — its new identity lives in
        // the candidate, where `copy.database_id()` names it.
        assert!(matches!(
            orna_repository_v1::inspect_metadata(&repo),
            Err(orna_repository_v1::RepositoryInitError::MetadataIncomplete)
        ));
        assert!(matches!(
            orna_repository_v1::inspect_metadata(&migrated),
            Err(orna_repository_v1::RepositoryInitError::MetadataIncomplete)
        ));
        assert_eq!(copy.source_format_number(), format_number);

        // The coordinates the migration must report are the ones it was
        // prepared with, not values re-derived from anything.
        assert_eq!(copy.annotation_defaults(), defaults);
        assert!(!copy.annotation_defaults().payload_inspected());
        assert_eq!(copy.continuity().predecessors(), continuity.predecessors());
        let legacy_consumer = [61; 32];
        assert_eq!(
            copy.continuity().successor_for(
                legacy_consumer,
                b"legacy-stream-source",
                Some(b"legacy-partition")
            ),
            Some((
                [62; 32],
                b"format3-stream-source".as_slice(),
                Some(b"format3-partition".as_slice())
            ))
        );
        assert_eq!(copy.dependency_entries(), 1);
        let tree_identity = copy.source_tree_identity();
        assert!(tree_identity.iter().any(|byte| *byte != 0));
        assert!(copy.source_index_generation().tree().is_some());
        let working_tree_identity = copy.working_tree_identity();
        assert!(working_tree_identity.iter().any(|byte| *byte != 0));
        // The two identities answer different questions: the source identity is
        // derived from the legacy commit, the copy's from the settled tree the
        // migration published, so a copy that silently reused one for the other
        // is detectable here.
        assert_ne!(tree_identity, working_tree_identity);
        // The whole copied tree includes untracked and ignored paths, not only
        // entries already present in the clone's Git index.
        assert!(copy.working_tree_entries() > 0);
        fs::write(
            source.path().join("ignored/payload.bin"),
            b"changed ignored payload",
        )
        .unwrap();
        let changed_copy = orna_repository_v1::migration_copy::prepare_format3_migration(
            &repo,
            &directory.path().join("migrated-after-ignored-change"),
            continuity.clone(),
            defaults,
        )
        .unwrap();
        assert_ne!(
            copy.working_tree_identity(),
            changed_copy.working_tree_identity(),
            "ignored user data participates in the copied-tree identity"
        );
        assert_eq!(
            fs::read(source.path().join("ignored/payload.bin")).unwrap(),
            b"changed ignored payload",
            "preparing a second copy leaves source ignored work untouched"
        );

        // The prepared journal binds the copy's legacy head to the format-3
        // candidate and carries both migration coordinates, so the migration is
        // resumable before the head moves.
        let journal = copy.candidate_journal();
        assert_eq!(journal.old_head(), &source_head);
        assert!(journal.new_head().as_str() != source_head.as_str());
        assert_eq!(journal.stage(), PublicationJournalStage::Prepared);
        assert_eq!(journal.migration_annotation_defaults(), Some(defaults));
        assert_eq!(
            journal.migration_continuity().unwrap().predecessors(),
            continuity.predecessors()
        );
        assert!(journal.runtime_intent_id().is_some());
        // The journal names every path the migration changes, so a resume
        // replays the transition rather than re-deciding it.
        assert_eq!(journal.entries().len(), 3);
        assert_eq!(
            journal
                .entries()
                .iter()
                .map(PublicationJournalEntry::path)
                .map(|path| path.as_path().to_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            vec![
                ".orna/database.orna".to_owned(),
                ".orna/format.orna".to_owned(),
                ".orna/migration.json".to_owned()
            ]
        );
        // The candidate installs the canonical database record and retires the
        // legacy format record, which the profile reader spells as "no format
        // file beside a database record" rather than a literal `format 3` line.
        // It does not yet carry a converted `.orna/store` root, so publication
        // still refuses it: that store conversion is a separate step.
        let candidate_commit = journal.new_head();
        assert_ne!(candidate_commit.as_str(), source_head.as_str());
        assert_eq!(
            git(
                copy.destination(),
                &[
                    "show",
                    &format!("{}:.orna/database.orna", candidate_commit.as_str())
                ]
            ),
            format!(
                "{{\n    repository_format: 3,\n    database_id: \"{}\",\n}}",
                copy.database_id()
            )
        );
        assert!(
            git(
                copy.destination(),
                &[
                    "ls-tree",
                    "--name-only",
                    candidate_commit.as_str(),
                    ".orna/"
                ]
            )
            .lines()
            .all(|line| line != ".orna/format.orna")
        );
        // The copy's head has not moved: the legacy commit stays checked out,
        // so an old reader still dispatches to the legacy decoder there even
        // though the working tree already carries format-3 metadata.
        assert_eq!(migrated.head().unwrap(), Some(source_head.clone()));
        assert_eq!(
            migrated
                .open_format_context()
                .unwrap()
                .repository_format_number(),
            format_number
        );

        // A destination that already exists is refused rather than merged into.
        assert!(matches!(
            orna_repository_v1::migration_copy::prepare_format3_migration(
                &repo,
                &destination,
                MigrationContinuityRecord::new(Vec::new()).unwrap(),
                MigrationAnnotationDefaults::new(0).unwrap(),
            ),
            Err(RepositoryError::PublicationRecoveryConflict)
        ));
    }
}
fn only_mount_record(repository: &Repository) -> PathBuf {
    fs::read_dir(repository.runtime_paths().root().join("mounts"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "orna"))
        .unwrap()
}

fn mount_record_field<'a>(record: &'a str, name: &str) -> &'a str {
    let prefix = format!("{name} ");
    record
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap()
}

fn mount_pin_reference(repository: &Repository, record: &str) -> String {
    let registry_owner_id = fs::read_to_string(
        repository
            .runtime_paths()
            .root()
            .join("mount-registry-owner-id-v1"),
    )
    .unwrap();
    format!(
        "refs/orna/pins/{}/mounts/{}/{}",
        registry_owner_id,
        mount_record_field(record, "owner"),
        mount_record_field(record, "view"),
    )
}

#[test]
fn mount_attach_rejects_a_second_view_and_reopens_by_view_identity() {
    let root = repository();
    let repository = Repository::discover(root.path()).unwrap();
    let mountpoint = TempDir::new().unwrap();
    let _original = MountView::attach(&repository, mountpoint.path(), "HEAD").unwrap();
    let status = repository.mount_status().unwrap().pop().unwrap();

    assert_eq!(
        MountView::attach(&repository, mountpoint.path(), "HEAD").unwrap_err(),
        MountError::MountpointAlreadyAttached,
    );
    let reopened = repository
        .open_mount_view_by_id(&status.view_hex())
        .unwrap();
    assert_eq!(reopened.verify_record().unwrap(), status);
    reopened.detach().unwrap();
    assert!(repository.mount_status().unwrap().is_empty());
}

#[test]
fn mount_registry_reconciles_interrupted_pin_and_record_updates() {
    let root = repository();
    let repository = Repository::discover(root.path()).unwrap();
    let mountpoint = TempDir::new().unwrap();
    let _first = MountView::attach(&repository, mountpoint.path(), "HEAD").unwrap();

    let first_record_path = only_mount_record(&repository);
    let first_record = fs::read_to_string(&first_record_path).unwrap();
    let first_pin = mount_pin_reference(&repository, &first_record);
    fs::remove_file(&first_record_path).unwrap();
    assert!(repository.mount_status().unwrap().is_empty());
    assert!(!Command::new("git")
        .current_dir(root.path())
        .args(["show-ref", "--verify", "--quiet", &first_pin])
        .status()
        .unwrap()
        .success());

    let _second = MountView::attach(&repository, mountpoint.path(), "HEAD").unwrap();
    let second_record_path = only_mount_record(&repository);
    let second_record = fs::read_to_string(&second_record_path).unwrap();
    let second_pin = mount_pin_reference(&repository, &second_record);
    let pin_oid = mount_record_field(&second_record, "pin");
    git(
        root.path(),
        &["update-ref", "-d", second_pin.as_str(), pin_oid],
    );
    assert!(repository.mount_status().unwrap().is_empty());
    assert!(!second_record_path.exists());
}

#[test]
fn mount_registry_reconciliation_preserves_other_worktree_pins() {
    let root = repository();
    let linked_worktree_path = root.path().join("linked-worktree");
    let linked_worktree_argument = linked_worktree_path.to_str().unwrap();
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            linked_worktree_argument,
            "HEAD",
        ],
    );
    let repository = Repository::discover(root.path()).unwrap();
    let linked_repository = Repository::discover(&linked_worktree_path).unwrap();
    assert_ne!(
        repository.runtime_paths().root(),
        linked_repository.runtime_paths().root()
    );

    let mountpoint = TempDir::new().unwrap();
    let linked_mountpoint = TempDir::new().unwrap();
    let first_view = MountView::attach(&repository, mountpoint.path(), "HEAD").unwrap();
    let linked_view =
        MountView::attach(&linked_repository, linked_mountpoint.path(), "HEAD").unwrap();
    let owner_id_path = |repository: &Repository| {
        repository
            .runtime_paths()
            .root()
            .join("mount-registry-owner-id-v1")
    };
    let first_registry_id = fs::read_to_string(owner_id_path(&repository)).unwrap();
    let linked_registry_id = fs::read_to_string(owner_id_path(&linked_repository)).unwrap();
    assert_ne!(first_registry_id, linked_registry_id);
    let rediscovered = Repository::discover(root.path()).unwrap();
    assert_eq!(
        fs::read_to_string(owner_id_path(&rediscovered)).unwrap(),
        first_registry_id,
    );

    let linked_record_path = only_mount_record(&linked_repository);
    let linked_record = fs::read_to_string(&linked_record_path).unwrap();
    let linked_pin = mount_pin_reference(&linked_repository, &linked_record);
    fs::remove_file(&linked_record_path).unwrap();

    assert_eq!(repository.mount_status().unwrap().len(), 1);
    assert!(Command::new("git")
        .current_dir(root.path())
        .args(["show-ref", "--verify", "--quiet", &linked_pin])
        .status()
        .unwrap()
        .success());
    assert!(linked_repository.mount_status().unwrap().is_empty());
    assert!(!Command::new("git")
        .current_dir(root.path())
        .args(["show-ref", "--verify", "--quiet", &linked_pin])
        .status()
        .unwrap()
        .success());
    linked_view.detach().unwrap();
    first_view.detach().unwrap();
}

#[test]
fn mount_detach_propagates_non_not_found_registry_read_errors() {
    let root = repository();
    let repository = Repository::discover(root.path()).unwrap();
    let mountpoint = TempDir::new().unwrap();
    let view = MountView::attach(&repository, mountpoint.path(), "HEAD").unwrap();
    let record_path = only_mount_record(&repository);
    let record = fs::read_to_string(&record_path).unwrap();
    let pin = mount_pin_reference(&repository, &record);

    fs::remove_file(&record_path).unwrap();
    fs::create_dir(&record_path).unwrap();
    assert_eq!(view.detach().unwrap_err(), MountError::HostFilesystem);
    assert!(Command::new("git")
        .current_dir(root.path())
        .args(["show-ref", "--verify", "--quiet", &pin])
        .status()
        .unwrap()
        .success());
}
