//! Gate G (ORNA-ASSERT-042, ORNA-ASSERT-046, ORNA-ASSERT-048, ORNA-ROW-011): one
//! invalid mutation stays invalid from every writer entrypoint.
//!
//! The same violating change (a `Music` row whose title is `Forbidden`, which
//! the table assertion rejects) is submitted through the trusted REPL, through
//! `orna edit --row`, and through a mounted editor. Each refuses the change and
//! leaves the stored row unchanged. The REPL leg names the refusal `ORNA-A091-009`,
//! the spec diagnostic. The edit leg names the refusal class it reports today,
//! `TableAssertionFalse`; it does not yet print `ORNA-A091-009` (defect bead filed
//! with this PR). The mount reports the refusal only as `EINVAL` on sync, so that leg
//! proves the errno and the unchanged row, not a diagnostic code.

#[path = "support/editor_mount.rs"]
mod editor_mount;
#[path = "support/format3.rs"]
mod format3;
#[path = "support/media_commit.rs"]
mod media_commit;
#[path = "support/product.rs"]
mod product;

use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use editor_mount::{open_rw, require_fuse_or_skip, start_editor_mount_with_source};
use orna_evaluator::SysHostBindingRegistry;
use orna_runtime::{RuntimeState, RuntimeTableIdentity, TableObjectId};
use orna_sys::EnvironmentProvider;
use product::{assert_success, commit_source, git, initialized_database, orna_repl, transcript};
use tempfile::TempDir;

const SOURCE: &str = "pub table Music(id: Str) {\n    title: Str,\n    artist: Str,\n    \
assert every(row => row.title != \"Forbidden\");\n}\n";
const ORIGINAL: &str = "{ title: \"Original\", artist: \"Kieran\", }\n";
const VIOLATING: &str = "{ title: \"Forbidden\", artist: \"Kieran\", }\n";
const ROW_PATH: &str = "Music/london/data.orna";
const REFUSAL: &str = "ORNA-A091-009";
/// The class the edit path reports today; the spec code is not yet printed (defect).
const EDIT_REFUSAL: &str = "TableAssertionFalse";

#[tokio::test]
async fn invalid_table_assertion_is_refused_identically_from_repl_edit_and_mount() {
    repl_leg();
    edit_leg().await;
    mount_leg().await;
}

/// The REPL leg: the update is refused, and neither HEAD nor the worktree moves.
fn repl_leg() {
    let directory = TempDir::new().expect("temporary directory");
    let root = initialized_database(&directory, SOURCE);
    commit_source(&root, "invalid mutation parity fixture");
    assert_success(
        &orna_repl(
            &root,
            "main.Music.insert({ id: \"london\", title: \"Original\", artist: \"Kieran\" })",
        ),
        "admit valid row",
    );
    let head = git(&root, &["rev-parse", "HEAD"]);
    let status = git(&root, &["status", "--porcelain"]);

    let output = orna_repl(
        &root,
        "main.Music.update(\"london\", { title: \"Forbidden\" })",
    );
    let text = transcript(&output);
    assert!(text.contains(REFUSAL), "REPL refusal: {text}");
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head, "REPL: {text}");
    assert_eq!(
        git(&root, &["status", "--porcelain"]),
        status,
        "REPL worktree: {text}"
    );
}

/// The edit leg: `orna edit --row` refuses the candidate and the stored row stays put.
async fn edit_leg() {
    let (repository_directory, repository, relation_id, format_context) =
        format3::format3_repository_with_context(SOURCE, "Music");
    let (identity, digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, digest)
        .await
        .expect("runtime state opens");
    let writer = state
        .acquire_lease(identity.repository_id)
        .await
        .expect("writer lease");
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
            source_row: ORIGINAL,
            ordinal: 0x90,
            insert_only: true,
        }],
    )
    .await;
    drop(state);
    let before = current_runtime_row(&repository, relation_id, "Music", b"london")
        .await
        .expect("the admitted row is readable before the edit");

    let scripts = TempDir::new().expect("temporary directory");
    let editor = editor_script(&scripts, VIOLATING);
    let output = run_row_edit(&repository_directory, &editor);
    let text = output_text(&output);
    assert!(!output.status.success(), "edit leg accepted: {text}");
    assert!(text.contains(EDIT_REFUSAL), "edit refusal: {text}");
    assert_eq!(
        current_runtime_row(&repository, relation_id, "Music", b"london").await,
        Some(before),
        "edit leg changed the stored row: {text}"
    );
}

/// The mount leg: the mounted draft's save fails and the stored row bytes stay put.
async fn mount_leg() {
    if !require_fuse_or_skip() {
        return;
    }
    let fixture = start_editor_mount_with_source(SOURCE, ORIGINAL, "Original", false).await;
    let before = current_runtime_row(&fixture.repository, fixture.relation_id, "Music", b"london")
        .await
        .expect("the admitted row is readable before the mount write");

    let mut draft = open_rw(&fixture);
    draft.set_len(0).expect("truncate the draft");
    draft
        .write_all(VIOLATING.as_bytes())
        .expect("write the violating draft");
    assert_eq!(
        draft.sync_all().unwrap_err().raw_os_error(),
        Some(libc::EINVAL),
        "mount leg: the violating draft must be refused on save"
    );
    assert_eq!(
        current_runtime_row(&fixture.repository, fixture.relation_id, "Music", b"london").await,
        Some(before),
        "mount leg changed the stored row"
    );
}

async fn current_runtime_row(
    repository: &orna_repository::Repository,
    relation_id: [u8; 16],
    table: &str,
    key: &[u8],
) -> Option<Vec<u8>> {
    let (identity, digest) = format3::cli_runtime_identity(repository);
    let state = RuntimeState::open(repository, identity, digest)
        .await
        .expect("runtime state opens");
    let table_identity =
        RuntimeTableIdentity::new(table, TableObjectId::new(relation_id)).expect("table identity");
    let snapshot = state
        .begin_admitted_table_activation(std::slice::from_ref(&table_identity))
        .await
        .expect("admitted table activation");
    snapshot
        .query_exact(table_identity.table(), key)
        .expect("exact row query")
        .map(<[u8]>::to_vec)
}

fn editor_script(directory: &TempDir, document: &str) -> PathBuf {
    let path = directory.path().join("editor.sh");
    std::fs::write(
        &path,
        format!("#!/bin/sh\ncat > \"$1\" <<'ORNADB_TEST'\n{document}ORNADB_TEST\n"),
    )
    .expect("write the editor script");
    let mut permissions = std::fs::metadata(&path)
        .expect("editor script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("make the editor executable");
    path
}

fn run_row_edit(repository_directory: &TempDir, editor: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .current_dir(repository_directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("EDITOR", editor)
        .args(["--debug", "edit", "--row", ROW_PATH])
        .output()
        .expect("production orna edit --row entrypoint runs")
}

fn output_text(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
