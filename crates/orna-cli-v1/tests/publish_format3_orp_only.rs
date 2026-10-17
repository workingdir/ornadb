//! Format-3 publication stores ORP rows under the store root, independently of
//! legacy loose-row path limits; CLI history, query, and offline copies read them.

use std::{path::Path, process::{Command, Output}};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::RuntimeState;
use orna_storage_v1::LoosePath;
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::commit_capture_pending;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song.orna"
);

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
    command.args(arguments).output().expect("CLI process starts")
}

fn import_expression(root: &Path) -> String {
    let source = std::fs::read_to_string(SONG_IMPORT_FIXTURE).unwrap();
    let root = format!("{:?}", root.to_string_lossy().as_ref());
    source.trim_end().replace("__MEDIA_ROOT__", &root)
}

fn relation_hex(relation_id: [u8; 16]) -> String {
    relation_id.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test]
async fn publish_format3_orp_only() {
    let (directory, repository, relation_id) = empty_format3_repository();
    let root = directory.path();
    let media = TempDir::new().unwrap();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("tone.wav"),
        media.path().join("tone.wav"),
    )
    .unwrap();
    let key = "k".repeat(512);
    assert!(LoosePath::for_key("media", &[key.clone()]).is_err());

    let (identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .unwrap();
    let writer = state.acquire_lease(identity.repository_id).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(media.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);
    commit_capture_pending(
        &repository,
        &state,
        writer,
        &mut bindings,
        &import_expression(media.path()),
        &key,
        0x71,
        true,
        relation_id,
    )
    .await;
    drop(bindings);
    drop(state);

    let published = run(root, &["publish"]);
    assert_eq!(published.status.code(), Some(0), "publish failed: {}", stderr(&published));
    let head = String::from_utf8(git_output(root, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();
    let tree = String::from_utf8(git_output(root, &["ls-tree", "-r", "--name-only", &head], None))
        .unwrap();
    assert!(tree.lines().any(|path| path.starts_with(".orna/store/")), "store root is committed: {tree}");
    assert!(
        tree.lines().all(|path| !path.starts_with("media/")),
        "format-3 rows have no loose row files in the commit: {tree}"
    );

    let relation = relation_hex(relation_id);
    let query = run(root, &["query", &relation, "--format", "json"]);
    assert_eq!(query.status.code(), Some(0), "query failed: {}", stderr(&query));
    let report: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
    assert_eq!(report["listings"].as_array().unwrap().len(), 1, "{report}");
    assert_eq!(report["listings"][0]["key"], key);

    let history = run(root, &["history", &relation, &key, "--format", "json"]);
    assert_eq!(history.status.code(), Some(0), "history failed: {}", stderr(&history));
    let revisions: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    assert!(!revisions.as_array().unwrap().is_empty(), "{revisions}");
    assert_eq!(revisions[0]["commit"], head);
    assert_eq!(revisions[0]["present"], true);

    let outside = TempDir::new().unwrap();
    let archive = outside.path().join("format3.orna");
    let archive_arg = archive.to_str().unwrap();
    let exported = run(root, &["export", archive_arg, "--at", "HEAD"]);
    assert_eq!(exported.status.code(), Some(0), "export failed: {}", stderr(&exported));
    let restored = outside.path().join("restored");
    let restored_arg = restored.to_str().unwrap();
    let restore = run(
        outside.path(),
        &["export", archive_arg, "--restore", restored_arg, "--worktree"],
    );
    assert_eq!(restore.status.code(), Some(0), "restore failed: {}", stderr(&restore));

    let copied_query = run(&restored, &["query", &relation, "--format", "json"]);
    assert_eq!(copied_query.status.code(), Some(0), "copied query failed: {}", stderr(&copied_query));
    let copied_report: serde_json::Value = serde_json::from_slice(&copied_query.stdout).unwrap();
    assert_eq!(copied_report["listings"][0]["key"], key);
    let copied_history = run(&restored, &["history", &relation, &key, "--format", "json"]);
    assert_eq!(copied_history.status.code(), Some(0), "copied history failed: {}", stderr(&copied_history));
    let copied_revisions: serde_json::Value = serde_json::from_slice(&copied_history.stdout).unwrap();
    assert_eq!(copied_revisions[0]["commit"], head);
}
