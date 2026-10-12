//! `orna export` end to end: freeze a pinned snapshot into an archive, read the
//! archive back with no repository, and reconstruct it into a fresh directory
//! that has no object of the source.
//!
//! The repository is a real format-3 fixture built by `support/format3.rs`; the
//! archive is written and read by the real `orna-cli-v1` binary, so this proves
//! the three export modes are wired to the shared complete-copy path rather
//! than to a private one.

use std::{
    io::Read,
    path::Path,
    process::{Command, Output},
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

#[path = "support/media_commit.rs"]
mod media_commit;

/// Runs `orna-cli-v1 <arguments>` in `directory` with Git routing isolated, so
/// the developer's own Git configuration cannot decide the outcome.
fn run(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ] {
        command.env_remove(name);
    }
    command
        .args(arguments)
        .output()
        .expect("CLI process starts")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn export_check_and_restore_carry_one_snapshot_offline() {
    let (root, _repository, _relation_id) = empty_format3_repository();
    let project: &Path = root.path();
    let snapshot = String::from_utf8(git_output(project, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();

    // Freeze the pinned snapshot: exit 0 and the primary commit on stdout.
    let archive = project.join("archive");
    let exported = run(project, &["export", "archive", "--at", "HEAD"]);
    assert_eq!(
        exported.status.code(),
        Some(0),
        "export failed: {}",
        stderr(&exported)
    );
    assert!(
        stdout(&exported).contains(&snapshot),
        "export names the pinned commit: {}",
        stdout(&exported)
    );
    assert!(archive.join("manifest.tsv").is_file());

    // Read the archive back: the check mode needs no repository at all, so it
    // runs from an unrelated directory.
    let unrelated = TempDir::new().unwrap();
    let archive_path = archive.to_str().unwrap();
    let checked = run(unrelated.path(), &["export", archive_path, "--check"]);
    assert_eq!(
        checked.status.code(),
        Some(0),
        "check failed: {}",
        stderr(&checked)
    );
    assert!(
        stdout(&checked).contains(&snapshot),
        "check reports the recorded snapshot: {}",
        stdout(&checked)
    );

    // Reconstruct into a fresh directory: no remote is configured and the copy
    // names the pinned commit as its HEAD. `--worktree` also materialises the
    // pinned snapshot, which is what makes the copy usable and lets the source
    // below be read from its own worktree.
    let restored = project.join("restored");
    let restored_path = restored.to_str().unwrap();
    let reconstructed = run(
        project,
        &[
            "export",
            archive_path,
            "--restore",
            restored_path,
            "--worktree",
        ],
    );
    assert_eq!(
        reconstructed.status.code(),
        Some(0),
        "restore failed: {}",
        stderr(&reconstructed)
    );
    let head = String::from_utf8(git_output(&restored, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(head, snapshot, "the copy's HEAD is the pinned commit");
    let remotes = String::from_utf8(git_output(&restored, &["remote"], None)).unwrap();
    assert_eq!(remotes.trim(), "", "the copy configures no remote");

    // The checked-out code is the snapshot's own source, read from the copy.
    let source = std::fs::read_to_string(restored.join("main.orna")).unwrap();
    assert_eq!(source, "capture test schema");
}

#[test]
fn export_refuses_a_second_snapshot_into_one_local_repository() {
    let (root, _repository, _relation_id) = empty_format3_repository();
    let project: &Path = root.path();

    // An absent selector and an absent archive are both failures, and neither
    // writes a claimed-complete archive.
    let unresolved = run(project, &["export", "archive", "--at", "no-such-ref"]);
    assert_eq!(unresolved.status.code(), Some(1), "{}", stderr(&unresolved));
    assert!(!project.join("archive").join("manifest.tsv").exists());

    let absent = run(project, &["export", "absent-archive", "--check"]);
    assert_eq!(absent.status.code(), Some(1), "{}", stderr(&absent));

    // The export verb belongs to a repository: a directory that is not one
    // fails instead of writing an empty archive.
    let unrelated = TempDir::new().unwrap();
    let outside = run(unrelated.path(), &["export", "archive", "--at", "HEAD"]);
    assert_eq!(outside.status.code(), Some(1), "{}", stderr(&outside));
    assert!(!unrelated.path().join("archive").exists());
}

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-image.orna"
);
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";

/// The identity `orna publish` derives for a repository: the metadata's
/// database id with a rotated-and-salted repository id, and the concatenation
/// of both as the initial digest. This is `cli_publish`'s own derivation (see
/// `cli_publish.rs::runtime_identity`), so the harness must open the runtime
/// under exactly this identity or `orna publish` reports a runtime for a
/// repository it cannot see.
fn cli_runtime_identity(
    repository: &orna_repository_v1::Repository,
) -> (orna_runtime_v1::RuntimeIdentity, [u8; 32]) {
    let metadata = orna_repository_v1::inspect_metadata(repository)
        .unwrap()
        .expect("the fixture records repository metadata");
    let database_id = *metadata.database_id().as_bytes();
    let mut repository_id = database_id;
    for (index, byte) in repository_id.iter_mut().enumerate() {
        let rotation = u32::try_from(index % 7 + 1).unwrap();
        let salt = u8::try_from(index).unwrap();
        *byte = byte.rotate_left(rotation) ^ (0x5a_u8.wrapping_add(salt));
    }
    if repository_id == [0; 16] {
        repository_id[0] = 1;
    }
    let mut initial_digest = [0; 32];
    initial_digest[..16].copy_from_slice(&database_id);
    initial_digest[16..].copy_from_slice(&repository_id);
    (
        orna_runtime_v1::RuntimeIdentity {
            database_id,
            repository_id,
        },
        initial_digest,
    )
}

/// Reads every committed row through the ordinary repository reader, exactly
/// as a query does.
fn committed_rows(
    repository: &orna_repository_v1::Repository,
    relation_id: [u8; 16],
) -> Vec<orna_repository_v1::AdmittedRow> {
    let format = repository
        .open_format_context()
        .expect("open the format context");
    let row_map = format.load_row_map(relation_id).expect("load the row map");
    let graph = format
        .open_native_graph(&row_map)
        .expect("open the native graph");
    let scope = graph.open_read_scope().expect("open a read scope");
    graph
        .range_rows(
            &orna_repository_v1::KeyRange::new(None, None, 16).expect("key range"),
            &scope,
        )
        .expect("read the committed rows")
}

/// `orna publish` of a database holding an imported media row produces a
/// complete copy whose history lists the publication commit and whose
/// reconstructed copy reads the same row with no media fetch.
#[tokio::test]
async fn publish_then_history_then_offline_copy_carries_the_publication_commit() {
    let (root, repository, relation_id) = empty_format3_repository();
    let project: &Path = root.path();
    let source_repository_path = project.to_path_buf();
    git(
        project,
        &[
            "remote",
            "add",
            "origin",
            "file:///deliberately-unavailable/original.git",
        ],
    );
    let source = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        source.path().join("pixel.png"),
    )
    .unwrap();

    // Commit one imported media row under the identity `orna publish` will
    // derive, so the CLI verb sees the range this harness leaves pending.
    let (runtime_identity, initial_digest) = cli_runtime_identity(&repository);
    let state = orna_runtime_v1::RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = orna_sys_v1::FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings =
        orna_evaluator_v1::SysHostBindingRegistry::new(orna_sys_v1::EnvironmentProvider::default())
            .with_filesystem_provider(filesystem)
            .with_repository_capture_capability(capability);
    let fixture = std::fs::read_to_string(IMPORT_FIXTURE).unwrap();
    let media_root = format!("{:?}", source.path().to_string_lossy().as_ref());
    let expression = fixture
        .trim_end()
        .replace(MEDIA_ROOT_PLACEHOLDER, &media_root);
    media_commit::commit_capture(
        &repository,
        &state,
        writer,
        &mut bindings,
        &expression,
        "image",
        0xb0,
        true,
    )
    .await;
    drop(bindings);
    drop(state);

    // Next: reader inputs — the relation id, a key spelling that names the
    // imported row, and the committed bytes the copy must reproduce.
    let relation = hex(&relation_id);
    let bytes_on_disk = std::fs::metadata(Path::new(MEDIA_FIXTURES).join("pixel.png"))
        .unwrap()
        .len();

    let parent = String::from_utf8(git_output(project, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    let published = run(project, &["publish"]);
    assert_eq!(
        published.status.code(),
        Some(0),
        "publish failed: {}",
        stderr(&published)
    );
    let published_repository =
        orna_repository_v1::Repository::discover(project).expect("discover published repository");
    let source_rows = committed_rows(&published_repository, relation_id);
    assert_eq!(source_rows.len(), 1, "publish commits the imported row");
    let publication = stdout(&published)
        .split_whitespace()
        .find(|word| word.len() == 40 && word.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            panic!(
                "publish names the publication commit: {}",
                stdout(&published)
            )
        });
    let head = String::from_utf8(git_output(project, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();

    // Step 2: the history of the imported row lists the publication commit.
    let history = run(
        project,
        &["history", &relation, "image", "--format", "json"],
    );
    assert_eq!(
        history.status.code(),
        Some(0),
        "history failed: {}",
        stderr(&history)
    );
    let revisions: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    let commits: Vec<String> = revisions
        .as_array()
        .expect("history --format json prints a list")
        .iter()
        .filter_map(|revision| revision["commit"].as_str().map(str::to_owned))
        .collect();
    assert!(
        commits.iter().any(|commit| commit == &publication),
        "the publication commit {publication} is listed: {commits:?}"
    );
    let listed_head = commits
        .first()
        .expect("the walk lists the newest commit first");
    assert!(
        head.starts_with(listed_head.as_str()),
        "the newest listed revision is HEAD {head}"
    );

    // Step 3: freeze that publication and reconstruct it somewhere else.
    let outside = TempDir::new().unwrap();
    let archive = outside.path().join("publication-archive");
    let archive_path = archive.to_str().unwrap();
    let exported = run(
        project,
        &["export", archive_path, "--at", "HEAD", "--at", &parent],
    );
    assert_eq!(
        exported.status.code(),
        Some(0),
        "export failed: {}",
        stderr(&exported)
    );
    drop(repository);
    drop(root);
    drop(source);
    assert!(
        !source_repository_path.exists(),
        "the source repository is gone"
    );
    let restored = outside.path().join("restored");
    let restored_path = restored.to_str().unwrap();
    let reconstructed = run(
        outside.path(),
        &[
            "export",
            archive_path,
            "--restore",
            restored_path,
            "--worktree",
        ],
    );
    assert_eq!(
        reconstructed.status.code(),
        Some(0),
        "restore failed: {}",
        stderr(&reconstructed)
    );

    // Step 4: the copy opens offline, recovers the committed key, and reads its
    // raw Blob bytes. A missing payload or descriptor closure fails the read.
    assert_eq!(
        String::from_utf8(git_output(&restored, &["remote"], None))
            .unwrap()
            .trim(),
        "",
        "the restored repository has no configured remotes"
    );
    for commit in [&parent, &publication] {
        assert_eq!(
            String::from_utf8(git_output(
                &restored,
                &["show", &format!("{commit}:main.orna")],
                None,
            ))
            .unwrap(),
            "capture test schema",
            "each snapshot independently recovers its module source"
        );
        let database = String::from_utf8(git_output(
            &restored,
            &["show", &format!("{commit}:.orna/database.orna")],
            None,
        ))
        .unwrap();
        assert!(
            database.contains("repository_format: 3"),
            "each snapshot independently recovers its format-3 database schema"
        );
        git_output(&restored, &["checkout", "--force", commit], None);
        let member_copy = orna_repository_v1::Repository::discover(&restored)
            .expect("discover the selected copied snapshot");
        let checked_out =
            String::from_utf8(git_output(&restored, &["rev-parse", "HEAD"], None)).unwrap();
        assert_eq!(
            checked_out.trim(),
            commit,
            "the selected snapshot is checked out"
        );
        let member_rows = committed_rows(&member_copy, relation_id);
        if commit == &publication {
            assert_eq!(
                member_rows, source_rows,
                "the published member recovers its row"
            );
            assert!(
                member_rows.iter().any(|row| {
                    row.key() == &orna_repository_v1::TypedKey::Bytes(b"image".to_vec())
                        || row.key() == &orna_repository_v1::TypedKey::Text("image".to_owned())
                }),
                "the copied publication retains the concrete image key"
            );
        } else {
            assert!(
                member_rows.is_empty(),
                "the pre-publication snapshot has no row"
            );
        }
    }
    let copy = orna_repository_v1::Repository::discover(&restored).expect("open the copy");
    assert_eq!(
        String::from_utf8(git_output(&restored, &["remote"], None))
            .unwrap()
            .trim(),
        "",
        "the copy configures no remote"
    );
    let copy_rows = committed_rows(&copy, relation_id);
    assert_eq!(
        copy_rows, source_rows,
        "the copy reads the identical committed row"
    );
    let row = copy_rows.first().expect("the copy carries the row");
    let drawn = row.blob_fields().expect("the row decodes its Blob fields");
    assert_eq!(drawn.len(), 1, "the row carries exactly one Blob field");
    let raw_payload = std::fs::read(Path::new(MEDIA_FIXTURES).join("pixel.png")).unwrap();
    let expected_sha256: [u8; 32] = Sha256::digest(&raw_payload).into();
    let format = copy
        .open_format_context()
        .expect("open the restored format context");
    let row_map = format
        .load_row_map(relation_id)
        .expect("load the restored committed row map");
    let graph = format
        .open_native_graph(&row_map)
        .expect("open the restored native graph");
    let scope = graph
        .open_read_scope()
        .expect("open the restored read scope");
    let copied_row = graph
        .lookup_row(row.key(), &scope)
        .expect("look up the copied image row")
        .expect("the image key remains present");
    let descriptor = copied_row
        .value()
        .dependencies()
        .into_iter()
        .find(|dependency| dependency.kind() == orna_repository_v1::NativeObjectKind::Tree)
        .expect("the copied row depends on its Blob descriptor")
        .oid()
        .clone();
    let reference = graph
        .admit_blob_reference(&copied_row, &descriptor, &scope)
        .expect("admit the copied descriptor from its row");
    let copied_payload = graph
        .read_blob_range(&reference, 0..drawn[0].1.length(), &scope)
        .expect("read raw Blob bytes from the restored snapshot")
        .bytes()
        .to_vec();
    assert_eq!(
        copied_payload, raw_payload,
        "the restored Blob bytes match the fixture"
    );
    let copied_sha256: [u8; 32] = Sha256::digest(&copied_payload).into();
    assert_eq!(
        copied_sha256, expected_sha256,
        "the restored raw Blob hash matches"
    );
    assert_eq!(
        drawn[0].1.sha256(),
        expected_sha256,
        "the restored Blob row preserves its raw content hash"
    );
    assert_eq!(
        drawn[0].1.length(),
        bytes_on_disk,
        "the row still carries the committed payload length"
    );
}

/// A repository is only discoverable from a directory inside the worktree.
#[test]
fn export_discovers_the_repository_from_a_subdirectory() {
    let (root, _repository, _relation_id) = empty_format3_repository();
    let project: &Path = root.path();
    let nested = project.join(".orna");
    let archive = project.join("nested-archive");

    let exported = run(
        &nested,
        &["export", archive.to_str().unwrap(), "--at", "HEAD"],
    );
    assert_eq!(
        exported.status.code(),
        Some(0),
        "export from a subdirectory failed: {}",
        stderr(&exported)
    );
    assert!(archive.join("manifest.tsv").is_file());
}

/// A promised object that exists only at `origin` must not be fetched to make
/// an otherwise incomplete local snapshot exportable.
#[test]
fn export_fails_locally_for_a_missing_promisor_object() {
    let (root, _repository, _) = empty_format3_repository();
    let source = root.path();
    std::fs::write(source.join("remote-only.txt"), b"promisor-only payload").unwrap();
    git(source, &["add", "remote-only.txt"]);
    git(source, &["commit", "--quiet", "-m", "promisor source"]);
    let oid = String::from_utf8(git_output(
        source,
        &["rev-parse", "HEAD:remote-only.txt"],
        None,
    ))
    .unwrap()
    .trim()
    .to_owned();
    let source_object = source.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
    assert!(source_object.is_file(), "origin has the promised object");

    let outside = TempDir::new().unwrap();
    let partial = outside.path().join("partial");
    git_output(
        outside.path(),
        &[
            "clone",
            "--local",
            source.to_str().unwrap(),
            partial.to_str().unwrap(),
        ],
        None,
    );
    git(&partial, &["config", "extensions.partialClone", "origin"]);
    git(&partial, &["config", "remote.origin.promisor", "true"]);
    git(
        &partial,
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    );
    let partial_object = partial.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
    assert!(
        partial_object.is_file(),
        "the clone initially has the object"
    );
    std::fs::remove_file(&partial_object).unwrap();

    let has_local_object = || {
        Command::new("git")
            .current_dir(&partial)
            .args(["cat-file", "-e", &oid])
            .env("GIT_NO_LAZY_FETCH", "1")
            .status()
            .expect("check the local object database")
            .success()
    };
    assert!(
        !has_local_object(),
        "the object is missing locally before export"
    );
    assert!(
        source_object.is_file(),
        "the origin can still serve the object"
    );

    let archive = outside.path().join("incomplete-copy");
    std::fs::create_dir(&archive).unwrap();
    let exported = run(
        &partial,
        &["export", archive.to_str().unwrap(), "--at", "HEAD"],
    );
    assert_ne!(
        exported.status.code(),
        Some(0),
        "export refuses a locally incomplete snapshot: {} {}",
        stdout(&exported),
        stderr(&exported)
    );
    assert!(
        stderr(&exported).contains(&oid),
        "diagnostic names missing object {oid}: {}",
        stderr(&exported)
    );
    assert!(
        std::fs::read_dir(&archive).unwrap().next().is_none(),
        "a failed export preserves its existing empty destination"
    );
    assert!(
        !has_local_object(),
        "export does not fetch the promised object from origin"
    );

    // An actual offline source: the promisor URL no longer resolves, but the
    // export still reports the missing object rather than a host-file fallback.
    let disabled_origin = outside.path().join("disabled-origin");
    git(
        &partial,
        &[
            "remote",
            "set-url",
            "origin",
            disabled_origin.to_str().unwrap(),
        ],
    );
    let offline_archive = outside.path().join("offline-copy");
    let offline_export = run(
        &partial,
        &["export", offline_archive.to_str().unwrap(), "--at", "HEAD"],
    );
    assert_ne!(
        offline_export.status.code(),
        Some(0),
        "offline export must fail"
    );
    assert!(
        stderr(&offline_export).contains(&oid),
        "offline diagnostic names missing object {oid}: {}",
        stderr(&offline_export)
    );
    assert!(
        !offline_archive.exists(),
        "offline failure creates no destination"
    );
    assert!(
        !has_local_object(),
        "offline export keeps the object absent locally"
    );
}

/// A streamed blob remains byte-identical after the source and its configured
/// origin are unavailable; restore publishes only the verified local copy.
#[test]
fn restore_streams_large_blob_after_source_and_origin_are_gone() {
    let (root, repository, _) = empty_format3_repository();
    let source = root.path();
    let outside = TempDir::new().unwrap();
    let unavailable_origin = outside.path().join("unavailable-origin");
    git(
        source,
        &[
            "remote",
            "add",
            "origin",
            unavailable_origin.to_str().unwrap(),
        ],
    );

    let payload_length = 8 * 1024 * 1024;
    let payload: Vec<u8> = (0..payload_length)
        .map(|index| ((index * 31 + (index >> 8)) & 0xff) as u8)
        .collect();
    let expected_sha256: [u8; 32] = Sha256::digest(&payload).into();
    std::fs::write(source.join("large-payload.bin"), &payload).unwrap();
    git(source, &["add", "large-payload.bin"]);
    git(
        source,
        &["commit", "--quiet", "-m", "large offline payload"],
    );
    let source_oid = String::from_utf8(git_output(
        source,
        &["rev-parse", "HEAD:large-payload.bin"],
        None,
    ))
    .unwrap()
    .trim()
    .to_owned();

    let archive = outside.path().join("archive");
    let exported = run(
        source,
        &["export", archive.to_str().unwrap(), "--at", "HEAD"],
    );
    assert_eq!(
        exported.status.code(),
        Some(0),
        "offline-local export failed: {}",
        stderr(&exported)
    );
    assert_eq!(
        String::from_utf8(git_output(source, &["remote", "get-url", "origin"], None))
            .unwrap()
            .trim(),
        unavailable_origin.to_str().unwrap(),
        "the source remote is explicitly unavailable"
    );

    drop(repository);
    std::fs::remove_dir_all(source).unwrap();
    assert!(
        !source.exists(),
        "the source repository is gone before restore"
    );
    assert!(
        !unavailable_origin.exists(),
        "the configured origin is unavailable"
    );

    let restored = outside.path().join("restored");
    std::fs::create_dir(&restored).unwrap();
    let restored_output = run(
        outside.path(),
        &[
            "export",
            archive.to_str().unwrap(),
            "--restore",
            restored.to_str().unwrap(),
            "--worktree",
        ],
    );
    assert_eq!(
        restored_output.status.code(),
        Some(0),
        "offline restore failed: {}",
        stderr(&restored_output)
    );
    assert_eq!(
        String::from_utf8(git_output(&restored, &["remote"], None))
            .unwrap()
            .trim(),
        "",
        "the reconstructed repository has no remote"
    );
    let restored_oid = String::from_utf8(git_output(
        &restored,
        &["rev-parse", "HEAD:large-payload.bin"],
        None,
    ))
    .unwrap()
    .trim()
    .to_owned();
    assert_eq!(
        restored_oid, source_oid,
        "the pinned blob object is preserved"
    );

    let mut payload_file = std::fs::File::open(restored.join("large-payload.bin")).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut restored_length = 0_u64;
    loop {
        let count = payload_file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        restored_length += count as u64;
        hasher.update(&buffer[..count]);
    }
    assert_eq!(restored_length, payload_length as u64);
    let restored_sha256: [u8; 32] = hasher.finalize().into();
    assert_eq!(
        restored_sha256, expected_sha256,
        "streamed raw payload hash"
    );
}
