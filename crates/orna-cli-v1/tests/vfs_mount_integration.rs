//! Gate E.1: a historical format-3 view is a real Linux FUSE mount backed by the
//! published row graph, and Blob reads are served as verified byte ranges.

#![cfg(target_os = "linux")]

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
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
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
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
/// ORNA-VFS-002, ORNA-VFS-003, ORNA-VFS-005, ORNA-VFS-018, and ORNA-VFS-023: a pinned view projects shared rows and Blob bytes, survives backing HEAD movement, and hides repository administration paths.
async fn mount_at_head_parent_pins_store_row_and_media_after_head_advances() {
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
        "{{\n    title: \"Live in London\",\n    artist: \"Kieran\",\n    content: sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 4194304), \"audio/mpeg\"),\n    cover: null,\n}}\n"
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
    let capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 4194304), \"audio/mpeg\")"
    );
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
    let historical_commit = current.clone();
    let revised_row = format!(
        "{{\n    title: \"Recorded in London\",\n    artist: \"Kieran\",\n    content: sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 4194304), \"audio/mpeg\"),\n    cover: null,\n}}\n"
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
        repository.head().unwrap().unwrap(),
        historical_commit
    );
    let historical_commit_selector = "HEAD~1";
    let (selected_commit, historical_format) = repository
        .open_pinned_format_context_with_commit(historical_commit_selector)
        .expect("resolve the same prior commit through the repository format context");
    assert_eq!(selected_commit.as_str(), historical_commit.as_str());
    let row_map = historical_format
        .load_row_map(relation_id)
        .expect("load the prior commit relation row map");
    let graph = historical_format
        .open_native_graph(&row_map)
        .expect("open the prior commit native store graph");
    let scope = graph.open_read_scope().expect("open a historical graph read scope");
    let stored_row = graph
        .lookup_row(
            &orna_repository_v1::TypedKey::Text("london".to_owned()),
            &scope,
        )
        .expect("read london from the historical graph")
        .expect("the historical graph contains the mounted row");
    assert!(
        !stored_row.value().dependencies().is_empty(),
        "the historical store row references the captured media objects"
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
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--at",
            historical_commit_selector,
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
    let root_entries = directory_entries(&mountpoint);
    assert!(
        !root_entries
            .split_whitespace()
            .any(|entry| matches!(entry, ".git" | ".orna")),
        "the view must not expose repository administration paths: {root_entries}"
    );
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
/// ORNA-VFS-005: workspace editing stays attached to the selected worktree snapshot after HEAD advances.
async fn cwd_mount_edit_row_uses_pinned_snapshot_after_head_advances() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
            "Music/london.orna",
            initial,
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest).await.unwrap();
    let writer = state.acquire_lease(runtime_identity.repository_id).await.unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x92,
            insert_only: true,
        }],
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
        .unwrap()
        .query_exact("Music", b"london")
        .unwrap()
        .unwrap()
        .to_vec();
    drop(state);

    let pinned_commit = repository.head().unwrap().unwrap();
    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    let row_path = mountpoint.join("Music/london");
    let document_path = row_path.join("data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !document_path.exists() {
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
        assert!(Instant::now() < deadline, "FUSE mount did not expose the row document");
        thread::sleep(Duration::from_millis(20));
    }
    std::fs::write(
        repository_directory.path().join("main.orna"),
        "invalid @@@ source\n",
    )
    .unwrap();
    format3::git(repository_directory.path(), &["add", "main.orna"]);
    format3::git(
        repository_directory.path(),
        &[
            "commit",
            "--quiet",
            "-m",
            "advance HEAD while workspace mount stays pinned",
        ],
    );
    let head_after_move = repository.head().unwrap().unwrap();
    assert_ne!(head_after_move, pinned_commit);
    let scripts = TempDir::new().unwrap();
    let editor = scripts.path().join("editor");
    std::fs::write(&editor, "#!/bin/sh\nprintf '%s\n' '{ title: \"after\", }' > \"$1\"\n").unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "edit",
            "--row",
        ])
        .arg(&row_path)
        .env("EDITOR", &editor)
        .output()
        .expect("run edit --row against mounted absolute row directory");
    assert!(
        output.status.success(),
        "edit --row failed; stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    assert!(status.success(), "mount exits cleanly after edit and unmount");
    let state = RuntimeState::open(&repository, runtime_identity, digest).await.unwrap();
    let after = state
        .begin_admitted_table_activation(&tables)
        .await
        .unwrap()
        .query_exact("Music", b"london")
        .unwrap()
        .unwrap()
        .to_vec();
    assert_eq!(
        orna_value_v1::Value::decode(&before)
            .expect("decode the original admitted row")
            .raw(),
        &orna_value_v1::Raw::Array(vec![orna_value_v1::Raw::Text("before".to_owned())]),
    );
    assert_eq!(
        orna_value_v1::Value::decode(&after)
            .expect("decode the edited admitted row")
            .raw(),
        &orna_value_v1::Raw::Array(vec![orna_value_v1::Raw::Text("after".to_owned())]),
    );
    assert_eq!(repository.head().unwrap().unwrap(), head_after_move);
}

/// A live `mount --cwd` over the committed `Music/london` row. The handles it
/// keeps let a test act as another writer and read what the store holds.
struct MountedMusicRow {
    mounted: MountedProcess,
    repository_directory: TempDir,
    repository: orna_repository_v1::Repository,
    runtime_identity: orna_runtime_v1::RuntimeIdentity,
    digest: [u8; 32],
    relation_id: [u8; 16],
    row_path: std::path::PathBuf,
    scripts: TempDir,
    _mount_parent: TempDir,
}

/// Seeds `Music/london` with `initial`, then mounts the cwd. Returns `None`
/// when FUSE is opted out or its prerequisites are optional and absent.
async fn mount_music_row(initial: &str) -> Option<MountedMusicRow> {
    if !fuse_test_available() {
        return None;
    }
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
            "Music/london.orna",
            initial,
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x92,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    let row_path = mountpoint.join("Music/london");
    let document_path = row_path.join("data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !document_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return None;
            }
            panic!("mount exited {status}: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not expose the row document"
        );
        thread::sleep(Duration::from_millis(20));
    }
    Some(MountedMusicRow {
        mounted,
        repository_directory,
        repository,
        runtime_identity,
        digest,
        relation_id,
        row_path,
        scripts: TempDir::new().unwrap(),
        _mount_parent: mount_parent,
    })
}

impl MountedMusicRow {
    /// Saves `body` through `edit --row` on the mounted absolute row directory.
    fn save(&self, name: &str, body: &str) -> std::process::Output {
        let editor = self.scripts.path().join(name);
        std::fs::write(
            &editor,
            format!("#!/bin/sh\nprintf '%s\\n' '{body}' > \"$1\"\n"),
        )
        .unwrap();
        std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
        Command::new(env!("CARGO_BIN_EXE_orna"))
            .args([
                "--db",
                self.repository_directory.path().to_str().unwrap(),
                "edit",
                "--row",
            ])
            .arg(&self.row_path)
            .env("EDITOR", &editor)
            .output()
            .expect("run edit --row against mounted absolute row directory")
    }

    /// Commits `source_row` over `Music/london` as a writer outside this mount.
    async fn replace_row_as_other_writer(&self, source_row: &str) {
        let state = RuntimeState::open(&self.repository, self.runtime_identity, self.digest)
            .await
            .unwrap();
        let writer = state
            .acquire_lease(self.runtime_identity.repository_id)
            .await
            .unwrap();
        let head = self.repository.head().unwrap().unwrap();
        let format_context =
            format3::format3_context_for_snapshot(&self.repository, head.as_str());
        let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
        media_commit::commit_source_captures_for_table(
            &self.repository,
            &state,
            writer,
            &mut bindings,
            self.relation_id,
            &format_context,
            "Music",
            &[media_commit::SourceCaptureRow {
                key: "london",
                blob_fields: &[],
                source_row: Some(source_row),
                ordinal: 0x9a,
                insert_only: false,
            }],
        )
        .await;
    }

    /// The `Music/london` row bytes the store holds now. Call after `unmount`.
    async fn stored_row(&self) -> Vec<u8> {
        let state = RuntimeState::open(&self.repository, self.runtime_identity, self.digest)
            .await
            .unwrap();
        let table = orna_runtime_v1::RuntimeTableIdentity::new(
            "Music",
            orna_runtime_v1::TableObjectId::new(self.relation_id),
        )
        .unwrap();
        let tables = [table];
        state
            .begin_admitted_table_activation(&tables)
            .await
            .unwrap()
            .query_exact("Music", b"london")
            .unwrap()
            .unwrap()
            .to_vec()
    }

    fn unmount(&mut self) {
        run_unmount(
            self.repository_directory.path(),
            &self.mounted.mountpoint,
        );
        let status = self.mounted.child.wait().unwrap();
        assert!(status.success(), "mount exits cleanly after unmount");
    }
}

fn assert_saved(output: &std::process::Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed; stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn assert_title(row: &[u8], expected: &str) {
    assert_eq!(
        orna_value_v1::Value::decode(row)
            .expect("decode the admitted row")
            .raw(),
        &orna_value_v1::Raw::Array(vec![orna_value_v1::Raw::Text(expected.to_owned())]),
    );
}

#[tokio::test]
/// ORNA-VFS-005: consecutive saves of one row in one mount both commit; the
/// second compares against the row the first save accepted.
async fn cwd_mount_consecutive_saves_of_one_row_both_commit() {
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    assert_saved(&row.save("first", "{ title: \"after-one\", }"), "first save");
    assert_saved(&row.save("second", "{ title: \"after-two\", }"), "second save");
    row.unmount();
    assert_title(&row.stored_row().await, "after-two");
}

#[tokio::test]
/// ORNA-VFS-005: a save after another writer changed the row and advanced HEAD
/// is refused as stale, and the other writer's row is kept.
async fn cwd_mount_save_after_another_writer_advances_head_is_stale() {
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    assert_saved(&row.save("first", "{ title: \"after-one\", }"), "first save");
    row.replace_row_as_other_writer("{ title: \"other\", }\n").await;
    let repository = row.repository_directory.path().to_path_buf();
    std::fs::write(repository.join("notes.txt"), "advance HEAD\n").unwrap();
    format3::git(&repository, &["add", "notes.txt"]);
    format3::git(
        &repository,
        &[
            "commit",
            "--quiet",
            "-m",
            "advance HEAD while another writer owns the row",
        ],
    );
    let second = row.save("second", "{ title: \"after-two\", }");
    let stdout = String::from_utf8_lossy(&second.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&second.stderr).into_owned();
    assert!(
        !second.status.success(),
        "a save against a row another writer changed must be refused; stdout={stdout}; stderr={stderr}"
    );
    assert!(
        format!("{stdout}{stderr}").contains("stale"),
        "the refusal names the stale baseline; stdout={stdout}; stderr={stderr}"
    );
    row.unmount();
    assert_title(&row.stored_row().await, "other");
}

#[tokio::test]
/// ORNA-VFS-005: a HEAD rename of a mounted table keeps saves on the pinned
/// declaration name the mounted row mutations address.
async fn cwd_mount_save_after_head_renames_table_keeps_pinned_declaration() {
    let Some(mut row) = mount_music_row("{ title: \"before\", }\n").await else {
        return;
    };
    let repository = row.repository_directory.path().to_path_buf();
    let rename = Command::new(env!("CARGO_BIN_EXE_orna"))
        .current_dir(&repository)
        .args(["mv", "Music", "Archive"])
        .output()
        .expect("run mv at HEAD");
    assert!(
        rename.status.success(),
        "mv failed; stderr={}",
        String::from_utf8_lossy(&rename.stderr)
    );
    format3::git(&repository, &["add", "--all"]);
    format3::git(
        &repository,
        &["commit", "--quiet", "-m", "rename Music to Archive at HEAD"],
    );
    let head_source = std::fs::read_to_string(repository.join("main.orna")).unwrap();
    assert!(
        head_source.contains("Archive"),
        "HEAD declares the renamed table: {head_source}"
    );
    assert_saved(&row.save("save", "{ title: \"after\", }"), "save through pinned name");
    row.unmount();
    assert_title(&row.stored_row().await, "after");
}

#[tokio::test]
/// ORNA-VFS-005: a historical snapshot mount cannot be retargeted or edited.
async fn edit_row_refuses_read_only_historical_mount_without_mutation() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
            "Music/london.orna",
            initial,
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest).await.unwrap();
    let writer = state.acquire_lease(runtime_identity.repository_id).await.unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x93,
            insert_only: true,
        }],
    )
    .await;
    let head_before = repository.head().unwrap().unwrap();
    let rows_before = format3::committed_store_rows(&repository, relation_id);
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("history");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "mount",
            mountpoint.to_str().unwrap(),
            "--at",
            "HEAD",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --at HEAD");
    let mut mounted = MountedProcess {
        child,
        mountpoint: mountpoint.clone(),
    };
    let row_path = mountpoint.join("Music/london");
    let document_path = row_path.join("data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !document_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return;
            }
            panic!("historical mount exited {status}: {stderr}");
        }
        assert!(Instant::now() < deadline, "historical mount did not expose the row document");
        thread::sleep(Duration::from_millis(20));
    }

    let scripts = TempDir::new().unwrap();
    let editor = scripts.path().join("editor");
    let marker = scripts.path().join("editor-was-run");
    std::fs::write(&editor, "#!/bin/sh\nprintf invoked > \"$ORNA_EDIT_MARKER\"\n").unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "edit",
            "--row",
        ])
        .arg(&row_path)
        .env("EDITOR", &editor)
        .env("ORNA_EDIT_MARKER", &marker)
        .output()
        .expect("run edit --row on read-only historical mount");
    assert!(
        !output.status.success(),
        "edit --row must refuse a read-only --at mount"
    );
    assert!(!marker.exists(), "read-only refusal must happen before launching the editor");
    assert_eq!(repository.head().unwrap().unwrap(), head_before);
    assert_eq!(format3::committed_store_rows(&repository, relation_id), rows_before);

    run_unmount(repository_directory.path(), &mountpoint);
    let status = mounted.child.wait().unwrap();
    assert!(status.success(), "historical mount exits cleanly after unmount");
}

#[tokio::test]
/// ORNA-VFS-011: replacing content through a scratch rename compares the captured baseline and preserves row identity.
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
    let row_source = format!(
        "{{\n    content: sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 65536), \"audio/mpeg\"),\n}}\n"
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
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 65536), \"audio/mpeg\")"
    );
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
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
/// ORNA-VFS-010, ORNA-VFS-018: metadata-only saves do not fetch corrupt sibling content; reading that content reports I/O failure.
async fn cwd_mount_metadata_save_avoids_corrupt_sibling_payload() {
    if !fuse_test_available() {
        return;
    }

    let media_root = TempDir::new().unwrap();
    let expected = b"corrupt-chunk-read-test-content";
    let source_file = media_root.path().join("tone.mp3");
    std::fs::write(&source_file, expected).unwrap();
    let source = "pub table Music(id: Str) { content: Blob, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let row_source = format!(
        "{{\n    content: sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 65536), \"audio/mpeg\"),\n}}\n"
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
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"tone.mp3\", 65536), \"audio/mpeg\")"
    );
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

     let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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


    let metadata = Command::new("cat")
        .arg(mountpoint.join("Music/london/data.orna"))
        .output()
        .expect("read row metadata after corrupting the media chunk");
    assert!(
        metadata.status.success(),
        "metadata read must not fetch corrupt media payload"
    );
    let metadata = String::from_utf8(metadata.stdout).unwrap();
    assert!(metadata.contains(
        "content: { path: \"./content.mp3\", media_type: \"audio/mpeg\" }"
    ));

    let data_path = mountpoint.join("Music/london/data.orna");
    let replacement_document = "{\n    content: { path: \"./content.mp3\", media_type: \"audio/mpeg\" },\n}\n";
    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .expect("open projected row document for metadata-only save");
    document.write_all(replacement_document.as_bytes()).unwrap();
    document
        .sync_all()
        .expect("metadata-only save must not fetch corrupt sibling payload");
    drop(document);

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
/// ORNA-VFS-016: an invalid duplicate-Blob row does not hide unrelated valid keys.
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
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut pipe) = mounted.child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        panic!("mount process exits cleanly after SIGTERM ({status}): {stderr}");
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    while mountinfo_contains(&mountpoint) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !mountinfo_contains(&mountpoint),
        "SIGTERM left the FUSE mountpoint in /proc/self/mountinfo"
    );
    let status = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    run_unmount_from(
        repository_directory.path(),
        mount_parent.path(),
        Path::new("library"),
    );
    assert_eq!(
        dead_mount_error.map(|error| error.kind()),
        Some(std::io::ErrorKind::NotConnected),
        "a directory request to the dead FUSE mount should fail with ENOTCONN"
    );
}
#[tokio::test]
/// ORNA-VFS-024: unmount refuses an unresolved draft and succeeds after fsync accepts it.
async fn cwd_mount_refuses_unmount_with_dirty_draft_and_allows_clean_unmount() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
            "Music/london.orna",
            initial,
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x92,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    let data_path = mountpoint.join("Music/london/data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            panic!("mount exited before data.orna appeared ({status})");
        }
        assert!(Instant::now() < deadline, "data.orna did not appear");
        thread::sleep(Duration::from_millis(20));
    }

    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .unwrap();
    document.write_all(b"{ title: \"after\", }\n").unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "unmount",
        ])
        .arg(&mountpoint)
        .output()
        .expect("run dirty-state unmount");
    assert!(!refused.status.success(), "dirty mount unmount was accepted");
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        diagnostic.contains("unresolved workspace drafts remain"),
        "unmount refusal did not explain draft recovery: {diagnostic}"
    );
    assert!(mountinfo_contains(&mountpoint));
    assert_eq!(repository.mount_status().unwrap().len(), 1);

    document.sync_all().unwrap();
    drop(document);
    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
}
#[tokio::test]
/// ORNA-VFS-024: released drafts are discarded; path-based truncates stay pending until fsync.
async fn cwd_mount_tracks_released_drafts_and_path_truncates() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
            "Music/london.orna",
            initial,
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x93,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let start_mount = || {
        Command::new(env!("CARGO_BIN_EXE_orna"))
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
            .expect("start mount --cwd")
    };
    let data_path = mountpoint.join("Music/london/data.orna");
    let mut mounted = MountedProcess {
        child: start_mount(),
        mountpoint: mountpoint.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            panic!("mount exited before data.orna appeared ({status})");
        }
        assert!(Instant::now() < deadline, "data.orna did not appear");
        thread::sleep(Duration::from_millis(20));
    }

    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .unwrap();
    document.write_all(b"{ title: \"unretained\", }\n").unwrap();
    drop(document);
    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
    drop(mounted);

    let mut mounted = MountedProcess {
        child: start_mount(),
        mountpoint: mountpoint.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            panic!("remount exited before data.orna appeared ({status})");
        }
        assert!(Instant::now() < deadline, "data.orna did not reappear");
        thread::sleep(Duration::from_millis(20));
    }

    let truncated = Command::new("python3")
        .args(["-c", "import os, sys; os.truncate(sys.argv[1], 0)"])
        .arg(&data_path)
        .output()
        .expect("run path-based truncate");
    assert!(
        truncated.status.success(),
        "path-based truncate failed: {}",
        String::from_utf8_lossy(&truncated.stderr)
    );
    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "unmount",
        ])
        .arg(&mountpoint)
        .output()
        .expect("run pending path-truncate unmount");
    assert!(!refused.status.success(), "pending path truncate was discarded");
    assert!(mountinfo_contains(&mountpoint));

    let mut document = OpenOptions::new().write(true).open(&data_path).unwrap();
    document.write_all(b"{ title: \"accepted\", }\n").unwrap();
    document.sync_all().unwrap();
    drop(document);
    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
}
#[tokio::test]
/// ORNA-VFS-024: a rejected Document draft survives close and still blocks unmount until accepted.
async fn cwd_mount_refuses_unmount_after_rejected_document_draft_is_released() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
            "Music/london.orna",
            initial,
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x94,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    let data_path = mountpoint.join("Music/london/data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            panic!("mount exited before data.orna appeared ({status})");
        }
        assert!(Instant::now() < deadline, "data.orna did not appear");
        thread::sleep(Duration::from_millis(20));
    }

    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .unwrap();
    document.write_all(b"{ title: \"rejected\",\n").unwrap();
    assert!(
        document.sync_all().is_err(),
        "malformed Document candidate must be rejected by fsync"
    );
    drop(document);

    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository_directory.path().to_str().unwrap(),
            "unmount",
        ])
        .arg(&mountpoint)
        .output()
        .expect("run rejected-draft unmount");
    assert!(
        !refused.status.success(),
        "unmount discarded a rejected Document draft after its handle was released"
    );
    assert!(mountinfo_contains(&mountpoint));
    assert_eq!(repository.mount_status().unwrap().len(), 1);

    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .unwrap();
    document.write_all(b"{ title: \"accepted\", }\n").unwrap();
    document.sync_all().unwrap();
    drop(document);
    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
    drop(mounted);
}

#[tokio::test]
/// ORNA-VFS-024: an accepted atomic replacement supersedes the rejected Document draft, so unmount succeeds.
async fn cwd_mount_atomic_replacement_clears_rejected_draft_for_unmount() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
            "Music/london.orna",
            initial,
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x95,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    let data_path = mountpoint.join("Music/london/data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            panic!("mount exited before data.orna appeared ({status})");
        }
        assert!(Instant::now() < deadline, "data.orna did not appear");
        thread::sleep(Duration::from_millis(20));
    }

    let mut document = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&data_path)
        .unwrap();
    document.write_all(b"{ title: \"rejected\",\n").unwrap();
    assert!(
        document.sync_all().is_err(),
        "malformed Document candidate must be rejected by fsync"
    );
    drop(document);

    let scratch = mountpoint.join("Music/london/.data.orna.replacement-tmp");
    let mut replacement = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&scratch)
        .expect("create replacement scratch beside data.orna");
    replacement.write_all(b"{ title: \"replaced\", }\n").unwrap();
    replacement.sync_all().expect("accept replacement scratch");
    drop(replacement);
    std::fs::rename(&scratch, &data_path).expect("atomically replace data.orna");
    assert!(
        std::fs::read_to_string(&data_path)
            .unwrap()
            .contains("replaced")
    );

    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
    drop(mounted);
}

#[tokio::test]
/// ORNA-VFS-024: fsync on a Document handle that submitted no candidate does not block unmount.
async fn cwd_mount_unmodified_document_fsync_does_not_block_unmount() {
    if !fuse_test_available() {
        return;
    }
    let initial = "{ title: \"before\", }\n";
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_for_table_with_source_row(
            "pub table Music(id: Str) { title: Str, }\n",
            "Music",
            "Music/london.orna",
            initial,
        );
    let (runtime_identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, digest)
        .await
        .unwrap();
    let writer = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default());
    media_commit::commit_source_captures_for_table(
        &repository,
        &state,
        writer,
        &mut bindings,
        relation_id,
        &format_context,
        "Music",
        &[media_commit::SourceCaptureRow {
            key: "london",
            blob_fields: &[],
            source_row: Some(initial),
            ordinal: 0x96,
            insert_only: true,
        }],
    )
    .await;
    drop(state);

    let mount_parent = TempDir::new().unwrap();
    let mountpoint = mount_parent.path().join("library");
    std::fs::create_dir(&mountpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
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
    let data_path = mountpoint.join("Music/london/data.orna");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !data_path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            panic!("mount exited before data.orna appeared ({status})");
        }
        assert!(Instant::now() < deadline, "data.orna did not appear");
        thread::sleep(Duration::from_millis(20));
    }

    let document = OpenOptions::new()
        .write(true)
        .open(&data_path)
        .unwrap();
    // The handle submitted no candidate, so the fsync result is not a rejection and is not asserted.
    let _ = document.sync_all();
    drop(document);

    run_unmount_from(repository_directory.path(), Path::new("."), &mountpoint);
    assert!(!mountinfo_contains(&mountpoint));
    assert!(repository.mount_status().unwrap().is_empty());
    drop(mounted);
}

/// Deterministic bytes for Blob fixtures; distinct seeds make a wrong generation fail the comparison.
fn lcg_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut entropy = seed;
    (0..len)
        .map(|_| {
            entropy = entropy
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (entropy >> 32) as u8
        })
        .collect()
}

/// Canonical 44-byte RIFF/WAVE header over 16-bit mono PCM `samples` at `sample_rate`.
fn pcm_wav(sample_rate: u32, samples: &[u8]) -> Vec<u8> {
    let data_len = u32::try_from(samples.len()).unwrap();
    let block_align: u16 = 2;
    let byte_rate = sample_rate * u32::from(block_align);
    let mut wav = Vec::with_capacity(44 + samples.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(samples);
    wav
}

/// A live `mount --cwd` over `Music/london`, whose `content` Blob was captured from the source bytes.
struct MountedBlobRow {
    mounted: MountedProcess,
    repository_directory: TempDir,
    _repository: orna_repository_v1::Repository,
    _media_root: TempDir,
    _mount_parent: TempDir,
}

fn spawn_cwd_mount(repository: &Path, mountpoint: &Path) -> MountedProcess {
    let child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args([
            "--db",
            repository.to_str().expect("repository path is UTF-8"),
            "mount",
        ])
        .arg(mountpoint)
        .arg("--cwd")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start mount --cwd");
    MountedProcess {
        child,
        mountpoint: mountpoint.to_path_buf(),
    }
}

/// Waits for `path` in the live mount. Returns `false` when FUSE setup is optionally unavailable.
fn wait_for_mount_path(mounted: &mut MountedProcess, path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if let Some(status) = mounted.child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = mounted.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if is_optional_fuse_setup_failure(&stderr) {
                eprintln!("SKIP: FUSE mount prerequisites failed: {stderr}");
                return false;
            }
            panic!("mount exited {status}: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "FUSE mount did not expose {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
    true
}

/// Unmounts through `orna unmount` and requires the mount process to exit cleanly.
fn unmount_and_reap(repository: &Path, mounted: &mut MountedProcess) {
    run_unmount(repository, &mounted.mountpoint);
    let status = mounted.child.wait().unwrap();
    let mut stderr = String::new();
    if let Some(mut pipe) = mounted.child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        status.success(),
        "mount exits cleanly after unmount; status={status:?}; stderr={stderr}"
    );
}

/// Captures `original` as `Music/london/content.mp3` (`audio/mpeg`) and mounts the cwd.
/// Returns `None` when FUSE is opted out or its prerequisites are optional and absent.
async fn mount_captured_blob(original: &[u8]) -> Option<MountedBlobRow> {
    mount_captured_media(original, "tone.mp3", "audio/mpeg", "content.mp3").await
}

/// Captures `original` from `file_name` as a `media_type` Blob in `Music/london` and mounts the cwd.
/// The exported filename is `mounted_name`.
/// Returns `None` when FUSE is opted out or its prerequisites are optional and absent.
async fn mount_captured_media(
    original: &[u8],
    file_name: &str,
    media_type: &str,
    mounted_name: &str,
) -> Option<MountedBlobRow> {
    if !fuse_test_available() {
        return None;
    }
    // The host cap bounds the source; it must admit the whole fixture, so it matches the provider limit.
    let capture_limit: usize = 4 << 20;
    let media_root = TempDir::new().unwrap();
    std::fs::write(media_root.path().join(file_name), original).unwrap();
    let source = "pub table Music(id: Str) { content: Blob, }\n";
    let root = format!("{:?}", media_root.path().to_string_lossy().as_ref());
    let capture = format!(
        "sys.blob.annotate(sys.blob.capture_file({root}, \"{file_name}\", {capture_limit}), \"{media_type}\")"
    );
    let row_source = format!("{{\n    content: {capture},\n}}\n");
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
    let mut filesystem = FilesystemProvider::with_limits(capture_limit, 16).unwrap();
    filesystem.allow_root(media_root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    let blob_captures = [("content", capture.as_str())];
    let capture_rows = [media_commit::SourceCaptureRow {
        key: "london",
        blob_fields: &blob_captures,
        source_row: None,
        ordinal: 0x93,
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
    let mut mounted = spawn_cwd_mount(repository_directory.path(), &mountpoint);
    if !wait_for_mount_path(
        &mut mounted,
        &mountpoint.join("Music/london").join(mounted_name),
    ) {
        return None;
    }
    Some(MountedBlobRow {
        mounted,
        repository_directory,
        _repository: repository,
        _media_root: media_root,
        _mount_parent: mount_parent,
    })
}

#[tokio::test]
/// profiles/vfs-1.md Blob fields and ACCEPTANCE Gate E: a rename-over replacement gives new opens the new generation while a handle opened earlier keeps the captured bytes.
async fn cwd_mount_open_blob_handle_keeps_captured_generation_across_replacement() {
    let original = lcg_bytes(0x0123_4567_89ab_cdef, 1_048_576 + 12_345);
    let replacement = lcg_bytes(0xfedc_ba98_7654_3210, 2_097_152 + 777);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");
    let window = 64 * 1024;

    let mut old_handle = File::open(&blob_path).unwrap();
    let mut head = vec![0u8; window];
    old_handle.read_exact(&mut head).unwrap();
    assert_eq!(head.as_slice(), &original[..window]);

    let scratch_path = blob_path.with_file_name(".content.mp3.replace");
    std::fs::write(&scratch_path, &replacement).unwrap();
    std::fs::rename(&scratch_path, &blob_path).unwrap();
    assert_eq!(std::fs::read(&blob_path).unwrap(), replacement);

    assert_eq!(
        old_handle.metadata().unwrap().len(),
        original.len() as u64,
        "an open handle reports the size of the generation it captured"
    );
    let tail_start = original.len() - window;
    old_handle.seek(SeekFrom::Start(tail_start as u64)).unwrap();
    let mut tail = vec![0u8; window];
    old_handle.read_exact(&mut tail).unwrap();
    assert_eq!(tail.as_slice(), &original[tail_start..]);
    drop(old_handle);

    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}

#[tokio::test]
#[ignore = "ornadb-5il1.293 open gap: remount after replacement; run with --ignored"]
/// profiles/vfs-1.md Blob fields and ACCEPTANCE Gate E: the acknowledged replacement survives a restart of the mount process.
async fn cwd_mount_rename_replacement_survives_remount() {
    let original = lcg_bytes(0x0123_4567_89ab_cdef, 1_048_576 + 12_345);
    let replacement = lcg_bytes(0xfedc_ba98_7654_3210, 2_097_152 + 777);
    let Some(mut row) = mount_captured_blob(&original).await else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.mp3");

    let scratch_path = blob_path.with_file_name(".content.mp3.replace");
    std::fs::write(&scratch_path, &replacement).unwrap();
    std::fs::rename(&scratch_path, &blob_path).unwrap();
    assert_eq!(std::fs::read(&blob_path).unwrap(), replacement);

    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);

    let remount_parent = TempDir::new().unwrap();
    let remount_point = remount_parent.path().join("library");
    std::fs::create_dir(&remount_point).unwrap();
    let mut remounted = spawn_cwd_mount(row.repository_directory.path(), &remount_point);
    let remounted_blob = remount_point.join("Music/london/content.mp3");
    assert!(
        wait_for_mount_path(&mut remounted, &remounted_blob),
        "FUSE remount must expose the committed Blob after a successful mount"
    );
    let restarted = std::fs::read(&remounted_blob).unwrap();
    unmount_and_reap(row.repository_directory.path(), &mut remounted);
    assert_eq!(
        restarted, replacement,
        "a restarted mount serves the acknowledged replacement"
    );
}

#[tokio::test]
/// profiles/vfs-1.md Blob fields and ACCEPTANCE Gate E: a WAV Blob row exports as `content.wav`, serves the seek and range reads a media decoder issues, and exports byte-for-byte.
async fn cwd_mount_wav_blob_row_plays_through_seek_reads_and_exports_byte_for_byte() {
    let samples = lcg_bytes(0x0016_0000_5eed, 640_000);
    let original = pcm_wav(16_000, &samples);
    let Some(mut row) =
        mount_captured_media(&original, "tone.wav", "audio/wav", "content.wav").await
    else {
        return;
    };
    let blob_path = row.mounted.mountpoint.join("Music/london/content.wav");
    let mut handle = File::open(&blob_path).unwrap();
    assert_eq!(handle.metadata().unwrap().len(), original.len() as u64);

    // Format probe: the canonical header is served before any sample is decoded.
    let mut header = [0u8; 44];
    handle.read_exact(&mut header).unwrap();
    assert_eq!(header.as_slice(), &original[..44]);
    assert_eq!(&header[0..4], b"RIFF");
    assert_eq!(&header[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes([header[20], header[21]]), 1, "PCM");
    assert_eq!(u16::from_le_bytes([header[22], header[23]]), 1, "mono");
    assert_eq!(
        u32::from_le_bytes(header[24..28].try_into().unwrap()),
        16_000
    );
    assert_eq!(
        u16::from_le_bytes([header[34], header[35]]),
        16,
        "16-bit samples"
    );
    assert_eq!(&header[36..40], b"data");
    assert_eq!(
        u32::from_le_bytes(header[40..44].try_into().unwrap()),
        640_000
    );

    // Sequential 64 KiB reads cover the whole Blob, then report EOF.
    let window = 64 * 1024;
    handle.seek(SeekFrom::Start(0)).unwrap();
    let mut streamed = Vec::with_capacity(original.len());
    let mut chunk = vec![0u8; window];
    loop {
        let read = handle.read(&mut chunk).unwrap();
        if read == 0 {
            break;
        }
        streamed.extend_from_slice(&chunk[..read]);
        assert!(streamed.len() <= original.len(), "read past the Blob end");
    }
    assert_eq!(streamed, original);
    assert_eq!(
        handle.read(&mut chunk).unwrap(),
        0,
        "EOF stays at the Blob end"
    );

    // Container tail probe from the end of the file.
    let tail_len = 4096;
    handle.seek(SeekFrom::End(-(tail_len as i64))).unwrap();
    let mut tail = vec![0u8; tail_len];
    handle.read_exact(&mut tail).unwrap();
    assert_eq!(tail.as_slice(), &original[original.len() - tail_len..]);

    // Seeking decoder: deterministic random offsets, each range compared with the row bytes.
    let mut entropy = 0x5eed_5eed_5eed_5eed_u64;
    for _ in 0..64 {
        entropy = entropy
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let len = 1 + ((entropy >> 48) as usize % 8_192);
        let offset = (entropy as usize) % (original.len() - len);
        handle.seek(SeekFrom::Start(offset as u64)).unwrap();
        let mut range = vec![0u8; len];
        handle.read_exact(&mut range).unwrap();
        assert_eq!(
            range.as_slice(),
            &original[offset..offset + len],
            "range at {offset} length {len}"
        );
    }

    // Real media player: mpv opens and seeks the mounted WAV; skipped only when mpv is absent.
    let mpv_installed = Command::new("mpv")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if mpv_installed {
        let output = Command::new("mpv")
            .args([
                "--no-config",
                "--no-video",
                "--ao=null",
                "--no-terminal",
                "--msg-level=all=error",
                "--start=10",
                "--end=11",
            ])
            .arg(&blob_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "mpv plays the mounted WAV; status={:?}; stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    } else {
        eprintln!("SKIP: mpv is not installed; the seek and range reads are the decoder leg");
    }

    // Binary export: a copy through the mount is byte-identical to the row.
    let export_dir = TempDir::new().unwrap();
    let export_path = export_dir.path().join("content.wav");
    std::fs::copy(&blob_path, &export_path).unwrap();
    assert_eq!(std::fs::read(&export_path).unwrap(), original);
    drop(handle);

    unmount_and_reap(row.repository_directory.path(), &mut row.mounted);
}
