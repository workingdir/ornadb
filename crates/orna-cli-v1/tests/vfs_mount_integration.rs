//! Gate E.1: a historical format-3 view is a real Linux FUSE mount backed by the
//! published row graph, and Blob reads are served as verified byte ranges.

#![cfg(target_os = "linux")]

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::RuntimeState;
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
#[path = "support/media_commit.rs"]
mod media_commit;

struct MountedProcess {
    child: Child,
    mountpoint: std::path::PathBuf,
}

impl Drop for MountedProcess {
    fn drop(&mut self) {
        if mountinfo_contains(&self.mountpoint) {
            if let Some(program) = std::env::var_os("FUSERMOUNT_PATH") {
                let _ = Command::new(program)
                    .args(["-u", "--"])
                    .arg(&self.mountpoint)
                    .status();
            } else {
                for program in ["fusermount3", "fusermount"] {
                    if Command::new(program)
                        .args(["-u", "--"])
                        .arg(&self.mountpoint)
                        .status()
                        .is_ok_and(|status| status.success())
                    {
                        break;
                    }
                }
            }
        }
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn run_unmount(repository: &Path, mountpoint: &Path) {
    run_unmount_from(repository, Path::new("."), mountpoint);
}

fn run_unmount_from(repository: &Path, working_directory: &Path, mountpoint: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .current_dir(working_directory)
        .args([
            "--db",
            repository.to_str().expect("repository path is UTF-8"),
            "unmount",
        ])
        .arg(mountpoint)
        .output()
        .expect("run `orna unmount`");
    assert!(
        output.status.success(),
        "orna unmount failed; stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn git_output(repository: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repository)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

fn mount_options(mountpoint: &Path) -> String {
    let mountpoint = mountpoint.to_str().expect("mountpoint is UTF-8");
    std::fs::read_to_string("/proc/self/mountinfo")
        .expect("read Linux mount table")
        .lines()
        .find_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            (fields.get(4).copied() == Some(mountpoint)).then(|| fields[5].to_owned())
        })
        .expect("FUSE mount is present in the kernel mount table")
}

fn directory_entries(path: &Path) -> String {
    match std::fs::read_dir(path) {
        Ok(entries) => entries
            .map(|entry| {
                entry
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .unwrap_or_else(|error| format!("<entry error: {error}>"))
            })
            .collect::<Vec<_>>()
            .join(", "),
        Err(error) => format!("<directory error: {error}>"),
    }
}
fn fuse_test_required() -> bool {
    std::env::var_os("ORNA_REQUIRE_FUSE").as_deref() == Some(std::ffi::OsStr::new("1"))
}

fn fuse_test_available() -> bool {
    let required = fuse_test_required();
    let explicitly_skipped =
        std::env::var_os("ORNA_SKIP_FUSE_TEST").as_deref() == Some(std::ffi::OsStr::new("1"));
    assert!(
        !required || !explicitly_skipped,
        "ORNA_REQUIRE_FUSE=1 forbids ORNA_SKIP_FUSE_TEST=1"
    );
    if explicitly_skipped {
        eprintln!("SKIP: FUSE integration tests explicitly opted out with ORNA_SKIP_FUSE_TEST=1");
        return false;
    }

    let prerequisites = (|| {
        let _device = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fuse")
            .map_err(|error| format!("/dev/fuse is unavailable: {error}"))?;
        if let Some(helper) = std::env::var_os("FUSERMOUNT_PATH") {
            Command::new(helper)
                .arg("-h")
                .output()
                .map_err(|error| format!("FUSERMOUNT_PATH is not executable: {error}"))?;
        } else if !["fusermount3", "fusermount"]
            .into_iter()
            .any(|helper| Command::new(helper).arg("-h").output().is_ok())
        {
            return Err("neither fusermount3 nor fusermount is executable".to_owned());
        }
        Ok::<(), String>(())
    })();
    match prerequisites {
        Ok(()) => true,
        Err(reason) if required => {
            panic!("required FUSE integration prerequisites failed: {reason}")
        }
        Err(reason) => {
            eprintln!("SKIP: FUSE integration prerequisites are unavailable: {reason}");
            false
        }
    }
}

fn is_optional_fuse_setup_failure(stderr: &str) -> bool {
    if fuse_test_required() {
        return false;
    }
    let stderr = stderr.to_ascii_lowercase();
    (stderr.contains("fuse") || stderr.contains("fusermount") || stderr.contains("/dev/fuse"))
        && [
            "permission denied",
            "operation not permitted",
            "no such file or directory",
            "not found",
        ]
        .iter()
        .any(|reason| stderr.contains(reason))
}

fn mountinfo_contains(mountpoint: &Path) -> bool {
    let mountpoint = mountpoint.to_str().expect("mountpoint is UTF-8");
    std::fs::read_to_string("/proc/self/mountinfo")
        .expect("read Linux mount table")
        .lines()
        .any(|line| line.split_whitespace().nth(4) == Some(mountpoint))
}

#[tokio::test]
async fn mount_at_prior_commit_reads_path_changed_afterward() {
    if !fuse_test_available() {
        return;
    }
    let media_root = TempDir::new().unwrap();
    let payload_len = 3 * 1_048_576 + 17;
    let mut entropy = 0x0123_4567_89ab_cdef_u64;
    let expected = (0..payload_len)
        .map(|_| {
            entropy = entropy
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (entropy >> 32) as u8
        })
        .collect::<Vec<_>>();
    std::fs::write(media_root.path().join("tone.mp3"), &expected).unwrap();
    let source =
        "pub table Music(id: Str) { title: Str, artist: Str, content: Blob, cover: Blob?, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let row_source = format!(
        "{{\n    title: \"Live in London\",\n    artist: \"Kieran\",\n    content: sys.blob.capture_file({root}, \"tone.mp3\", 4194304),\n    cover: null,\n}}\n"
    );
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            source,
            "Music",
            "Music/london.orna",
            &row_source,
        );

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(4 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let capture = format!("sys.blob.capture_file({root}, \"tone.mp3\", 4194304)");
    let blob_captures = [("content", capture.as_str())];
    let capture_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &blob_captures,
        source_row: None,
        ordinal: 0x80,
        insert_only: true,
    }];
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &capture_rows,
    )
    .await;
    drop(state);

    assert_eq!(
        format3::committed_store_rows(&repository, relation_id).len(),
        1
    );
    let current = repository.head().unwrap().unwrap();
    let historical_commit = current.to_string();
    let revised_row = format!(
        "{{\n    title: \"Recorded in London\",\n    artist: \"Kieran\",\n    content: sys.blob.capture_file({root}, \"tone.mp3\", 4194304),\n    cover: null,\n}}\n"
    );
    std::fs::write(
        repository_directory.path().join("Music/london.orna"),
        &revised_row,
    )
    .unwrap();
    format3::git(repository_directory.path(), &["add", "Music/london.orna"]);
    format3::git(
        repository_directory.path(),
        &[
            "commit",
            "--quiet",
            "-m",
            "update source row after historical commit",
        ],
    );
    assert_ne!(
        repository.head().unwrap().unwrap().to_string(),
        historical_commit
    );
    let schema = orna_project_v1::ProjectLoader::default()
        .resolve_committed_source_table_schema(
            &repository,
            &current,
            orna_runtime_v1::TableObjectId::new(relation_id),
        )
        .expect("format-3 row data is not source text");
    assert_eq!(schema.declaration_name(), "Music");

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--at",
            historical_commit.as_str(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --at historical commit");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };

    let data_path = mountpoint.join("Music/london/data.orna");
    let blob_path = mountpoint.join("Music/london/content.mp3");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() || !blob_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return;
            }
            panic!("mount exited {status}: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not expose the row files; root=[{}], Music=[{}], london=[{}]",
            directory_entries(&mountpoint),
            directory_entries(&mountpoint.join("Music")),
            directory_entries(&mountpoint.join("Music/london")),
        );
        thread::sleep(Duration::from_millis(20));
    }

    let output = Command::new("cat")
        .arg(&data_path)
        .output()
        .expect("read data.orna with cat");
    assert!(output.status.success());
    let document = String::from_utf8(output.stdout).unwrap();
    let identities = orna_project_v1::ObjectIdentityManifest::read_committed(&repository, &current)
        .expect("read the committed field identities");
    let mut expected_fields = vec![
        ("title", "\"Live in London\""),
        (
            "content",
            "{ path: \"./content.mp3\", media_type: \"audio/mpeg\" }",
        ),
        ("artist", "\"Kieran\""),
        ("cover", "null"),
    ];
    expected_fields.sort_by_key(|(name, _)| {
        let qualified_name = format!("Music.{name}");
        *identities
            .id(orna_project_v1::ObjectKind::Field, &qualified_name)
            .expect("stored field identity exists")
    });
    let expected_fields = expected_fields
        .into_iter()
        .map(|(name, value)| format!("    {name}: {value},\n"))
        .collect::<String>();
    let expected_document = format!("{{\n{expected_fields}}}\n");
    assert_eq!(document, expected_document);

    let options = mount_options(&mountpoint);
    for required in ["ro", "nodev", "nosuid", "noexec"] {
        assert!(
            options.split(',').any(|option| option == required),
            "missing {required} in mount options {options}"
        );
    }

    let write_open_error = OpenOptions::new()
        .write(true)
        .open(&blob_path)
        .expect_err("opening a projected Blob for writing must fail");
    assert_eq!(
        write_open_error.raw_os_error(),
        Some(30),
        "write-open must fail with Linux EROFS",
    );
    let create_dir_error = std::fs::create_dir(mountpoint.join("Music/london/new-directory"))
        .expect_err("creating a directory in the mount must fail");
    assert_eq!(
        create_dir_error.raw_os_error(),
        Some(30),
        "mkdir must fail with Linux EROFS",
    );
    let remove_file_error =
        std::fs::remove_file(&data_path).expect_err("removing a projected file must fail");
    assert_eq!(
        remove_file_error.raw_os_error(),
        Some(30),
        "unlink must fail with Linux EROFS",
    );
    let chmod_error = std::fs::set_permissions(&blob_path, std::fs::Permissions::from_mode(0o600))
        .expect_err("changing permissions in the mount must fail");
    assert_eq!(
        chmod_error.raw_os_error(),
        Some(30),
        "chmod must fail with Linux EROFS",
    );

    let current_commit = current.to_string();
    let parent_ref = format!("{current_commit}^");
    let parent = git_output(
        repository_directory.path(),
        &["rev-parse", parent_ref.as_str()],
    );
    let parent_tree_ref = format!("{parent}^{{tree}}");
    let parent_tree = git_output(
        repository_directory.path(),
        &["rev-parse", parent_tree_ref.as_str()],
    );
    let moved_head = git_output(
        repository_directory.path(),
        &[
            "commit-tree",
            parent_tree.as_str(),
            "-m",
            "move FUSE test HEAD",
        ],
    );
    git_output(
        repository_directory.path(),
        &["update-ref", "HEAD", moved_head.as_str()],
    );
    assert_ne!(moved_head, current_commit);
    assert_eq!(
        git_output(repository_directory.path(), &["rev-parse", "HEAD"]),
        moved_head,
        "the worktree HEAD moved away from the mounted commit",
    );
    git_output(repository_directory.path(), &["gc", "--prune=now"]);
    git_output(repository_directory.path(), &["repack", "-ad"]);
    assert_eq!(
        std::fs::read_to_string(&data_path).unwrap(),
        expected_document,
        "the mounted snapshot remains visible after HEAD moves and GC",
    );

    let mut blob = File::open(&blob_path).expect("open projected Blob");
    assert_eq!(
        blob.metadata().unwrap().len() as usize,
        expected.len(),
        "the projected Blob size comes from its stored descriptor",
    );
    let boundary_range_len = 1_048_576 + 1;
    let mut actual = vec![0u8; boundary_range_len];
    blob.seek(SeekFrom::Start(0)).unwrap();
    blob.read_exact(&mut actual).unwrap();
    assert_eq!(actual.as_slice(), &expected[..boundary_range_len]);

    drop(blob);
    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    let mut stderr = String::new();
    if let Some(mut pipe) = mounted.child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "mount process exits cleanly after unmount; status={status:?}; stderr={stderr}"
    );
}

#[tokio::test]
async fn cwd_mount_replace_content_updates_blob_descriptor_and_preserves_row_identity() {
    if !fuse_test_available() {
        return;
    }

    let media_root = TempDir::new().unwrap();
    let original = b"original-mounted-content";
    let replacement = b"replacement-mounted-content-with-a-new-hash";
    let source_file = media_root.path().join("tone.mp3");
    std::fs::write(&source_file, original).unwrap();
    let source = "pub table Music(id: Str) { content: Blob, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let row_source =
        format!("{{\n    content: sys.blob.capture_file({root}, \"tone.mp3\", 65536),\n}}\n");
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            source,
            "Music",
            "Music/london.orna",
            &row_source,
        );

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let capture = format!("sys.blob.capture_file({root}, \"tone.mp3\", 65536)");
    let blob_captures = [("content", capture.as_str())];
    let capture_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &blob_captures,
        source_row: None,
        ordinal: 0x91,
        insert_only: true,
    }];
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &capture_rows,
    )
    .await;

    let table = orna_runtime_v1::RuntimeTableIdentity::new(
        "Music",
        orna_runtime_v1::TableObjectId::new(relation_id),
    )
    .unwrap();
    let tables = [table];
    let before = state
        .begin_admitted_table_activation(&tables)
        .await
        .unwrap();
    let old_row = before
        .query_exact("Music", b"london")
        .unwrap()
        .unwrap()
        .to_vec();
    drop(before);
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let blob_path = mountpoint.join("Music/london/content.mp3");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !blob_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return;
            }
            panic!("mount exited {status}: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not expose the captured Blob"
        );
        thread::sleep(Duration::from_millis(20));
    }

    let scratch_path = mountpoint.join("Music/london/.content.mp3.replace");
    std::fs::write(&scratch_path, replacement).unwrap();
    if let Err(error) = std::fs::rename(&scratch_path, &blob_path) {
        run_unmount(repository_directory.path(), &mountpoint);
        let _ = mounted.child.wait();
        let mut stderr = String::new();
        if let Some(mut pipe) = mounted.child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        panic!("mounted Blob replacement rename failed: {error}; stderr={stderr}");
    }
    assert_eq!(std::fs::read(&blob_path).unwrap(), replacement);

    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    let mut stderr = String::new();
    if let Some(mut pipe) = mounted.child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "mount process exits cleanly after replacement and unmount; status={status:?}; stderr={stderr}"
    );

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let after = state
        .begin_admitted_table_activation(&tables)
        .await
        .unwrap();
    let new_row = after.query_exact("Music", b"london").unwrap().unwrap();
    assert_ne!(
        old_row.as_slice(),
        new_row,
        "the same row key must now carry the replacement Blob descriptor/hash"
    );
}
#[tokio::test]
async fn cwd_mount_returns_eio_when_a_chunk_object_is_corrupt() {
    if !fuse_test_available() {
        return;
    }

    let media_root = TempDir::new().unwrap();
    let expected = b"corrupt-chunk-read-test-content";
    let source_file = media_root.path().join("tone.mp3");
    std::fs::write(&source_file, expected).unwrap();
    let source = "pub table Music(id: Str) { content: Blob, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let row_source =
        format!("{{\n    content: sys.blob.capture_file({root}, \"tone.mp3\", 65536),\n}}\n");
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            source,
            "Music",
            "Music/london.orna",
            &row_source,
        );

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let capture = format!("sys.blob.capture_file({root}, \"tone.mp3\", 65536)");
    let blob_captures = [("content", capture.as_str())];
    let capture_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &blob_captures,
        source_row: None,
        ordinal: 0x91,
        insert_only: true,
    }];
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &capture_rows,
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let blob_path = mountpoint.join("Music/london/content.mp3");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !blob_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return;
            }
            panic!("mount exited {status}: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not expose the Blob; root=[{}], Music=[{}], london=[{}]",
            directory_entries(&mountpoint),
            directory_entries(&mountpoint.join("Music")),
            directory_entries(&mountpoint.join("Music/london")),
        );
        thread::sleep(Duration::from_millis(20));
    }

    let chunk_oid = git_output(
        repository_directory.path(),
        &["hash-object", source_file.to_str().unwrap()],
    );
    let objects = repository_directory.path().join(git_output(
        repository_directory.path(),
        &["rev-parse", "--git-path", "objects"],
    ));
    let chunk_path = objects.join(&chunk_oid[..2]).join(&chunk_oid[2..]);
    assert!(
        chunk_path.is_file(),
        "captured chunk is stored as a loose object"
    );
    std::fs::set_permissions(&chunk_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&chunk_path, b"corrupt Git loose object bytes").unwrap();

    let mut blob = File::open(&blob_path).expect("open projected Blob");
    let mut byte = [0u8; 1];
    let error = blob
        .read_exact(&mut byte)
        .expect_err("a corrupt chunk object must not return data");
    assert_eq!(
        error.raw_os_error(),
        Some(5),
        "a corrupt chunk object must surface as Linux EIO",
    );

    drop(blob);
    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    let mut stderr = String::new();
    if let Some(mut pipe) = mounted.child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "mount process exits cleanly after unmount; status={status:?}; stderr={stderr}"
    );
}

#[tokio::test]
async fn mount_keeps_good_rows_available_when_one_row_has_duplicate_blob_values() {
    if !fuse_test_available() {
        return;
    }

    let media_root = TempDir::new().unwrap();
    let expected = b"row-local-error-test-content";
    std::fs::write(media_root.path().join("tone.mp3"), expected).unwrap();
    let source = "pub table Music(id: Str) { title: Str, content: Blob, duplicate: Blob?, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let capture = format!("sys.blob.capture_file({root}, \"tone.mp3\", 65536)");
    let bad_row = format!(
        "{{\n    title: \"bad\",\n    content: {capture},\n    duplicate: {capture},\n}}\n",
    );
    let good_row =
        format!("{{\n    title: \"good\",\n    content: {capture},\n    duplicate: null,\n}}\n",);
    let source_rows = [
        ("Music/bad.orna", bad_row.as_str()),
        ("Music/good.orna", good_row.as_str()),
    ];
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_rows(source, "Music", &source_rows);

    let (runtime_identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let duplicate_blob_fields = [
        ("content", capture.as_str()),
        ("duplicate", capture.as_str()),
    ];
    let single_blob_fields = [("content", capture.as_str())];
    let capture_rows = [
        media_commit::SourceCaptureRow {
            key: "bad",
            blob_fields: &duplicate_blob_fields,
            source_row: None,
            ordinal: 0x81,
            insert_only: true,
        },
        media_commit::SourceCaptureRow {
            key: "good",
            blob_fields: &single_blob_fields,
            source_row: None,
            ordinal: 0x82,
            insert_only: true,
        },
    ];
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &capture_rows,
    )
    .await;

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let good_data_path = mountpoint.join("Music/good/data.orna");
    let bad_data_path = mountpoint.join("Music/bad/data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !good_data_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return;
            }
            panic!("mount exited {status}: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not expose good row; root=[{}], Music=[{}], good=[{}]",
            directory_entries(&mountpoint),
            directory_entries(&mountpoint.join("Music")),
            directory_entries(&mountpoint.join("Music/good")),
        );
        thread::sleep(Duration::from_millis(20));
    }

    let rows = std::fs::read_dir(mountpoint.join("Music"))
        .expect("indexed table listing succeeds despite a malformed row")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(rows.iter().any(|name| name == "bad"));
    assert!(rows.iter().any(|name| name == "good"));
    let bad_row_error = std::fs::read(&bad_data_path).expect_err("malformed row is isolated");
    assert_eq!(
        bad_row_error.raw_os_error(),
        Some(5),
        "a malformed row returns Linux EIO without failing the mount",
    );
    let good_document = std::fs::read_to_string(&good_data_path).unwrap();
    assert!(good_document.contains("title: \"good\""));
    assert!(mounted.child.try_wait().unwrap().is_none());

    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    let mut stderr = String::new();
    if let Some(mut pipe) = mounted.child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "mount process exits cleanly after unmount; status={status:?}; stderr={stderr}"
    );
}

#[test]
fn cwd_mount_sigterm_unmounts_and_detaches_the_view() {
    if !fuse_test_available() {
        return;
    }
    let source = "pub table Music(id: Str) { title: Str, }\n";
    let (repository_directory, _repository, _relation_id) =
        format3::format3_repository_for_table(source, "Music");
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !mountinfo_contains(&mountpoint) {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return;
            }
            panic!("mount exited before appearing in mountinfo ({status}): {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not appear in /proc/self/mountinfo"
        );
        thread::sleep(Duration::from_millis(20));
    }

    let signal = Command::new("kill")
        .args(["-TERM", &mounted.child.id().to_string()])
        .status()
        .expect("send SIGTERM to mount child");
    assert!(signal.success(), "kill -TERM failed");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "mount did not stop after SIGTERM"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        status.success(),
        "mount process exits cleanly after SIGTERM"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while mountinfo_contains(&mountpoint) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !mountinfo_contains(&mountpoint),
        "SIGTERM left the FUSE mountpoint in /proc/self/mountinfo"
    );
    let status = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            "status",
            "--json",
        ])
        .output()
        .expect("read mount status after SIGTERM");
    assert!(
        status.status.success(),
        "mount status failed: {:?}",
        status.stderr
    );
    assert_eq!(status.stdout, b"{\"mounts\":[]}\n");
}

#[test]
fn unmount_releases_a_dead_unregistered_fuse_mount() {
    if !fuse_test_available() {
        return;
    }
    let source = "pub table Music(id: Str) { title: Str, }\n";
    let (repository_directory, repository, _relation_id) =
        format3::format3_repository_for_table(source, "Music");
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--cwd",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !mountinfo_contains(&mountpoint) {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return;
            }
            panic!("mount exited before appearing in mountinfo ({status}): {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not appear in /proc/self/mountinfo"
        );
        thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(repository.mount_status().unwrap().len(), 1);
    repository.unmount_view(&mountpoint).unwrap();
    assert!(repository.mount_status().unwrap().is_empty());

    let signal = Command::new("kill")
        .args(["-KILL", &mounted.child.id().to_string()])
        .status()
        .expect("send SIGKILL to mount child");
    assert!(signal.success(), "kill -KILL failed");
    let deadline = Instant::now() + Duration::from_secs(10);
    let child_status = loop {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "mount did not exit after SIGKILL"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        !child_status.success(),
        "SIGKILL should terminate the mount child"
    );
    let dead_mount_error = std::fs::read_dir(&mountpoint)
        .and_then(|entries| entries.collect::<std::io::Result<Vec<_>>>().map(|_| ()))
        .err();
    run_unmount_from(
        repository_directory.path(),
        mount_parent.path(),
        Path::new("library"),
    );

    assert!(
        !mountinfo_contains(&mountpoint),
        "orna unmount left the orphaned FUSE mount in /proc/self/mountinfo"
    );
    assert_eq!(repository.mount_status().unwrap().len(), 0);
    assert_eq!(
        dead_mount_error.map(|error| error.kind()),
        Some(std::io::ErrorKind::NotConnected),
        "a directory request to the dead FUSE mount should fail with ENOTCONN"
    );
}
