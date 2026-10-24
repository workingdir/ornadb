//! ORNA-APP-022; `store-format-3.md` §§2 and 5 (row maps and ROV-3 Blob descriptors).
//! The browser profile's media row and Blob are written through the app path
//! (trusted REPL submission -> admitted REPL transaction -> runtime commit),
//! then published by the CLI and read back from the committed format-3 graph.

use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

use orna_repository_v1::TypedKey;
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

const BROWSER_SOURCE: &str = r#"pub table media(id: Str) {
    content: Blob,
}

pub fn store_profile(id: Str, content: Blob) =
    media.insert({ id: id, content: content });
"#;
const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const BROWSER_PROFILE_BLOB: &[u8] = include_bytes!("fixtures/media/pixel.png");

fn run_repl(directory: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Orna REPL process");
    use std::io::Write as _;
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write trusted REPL submission");
    child.wait_with_output().expect("wait for Orna REPL")
}

fn run_publish(directory: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .current_dir(directory)
        .arg("publish")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run orna publish")
}

fn git_head(directory: &Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("read Git HEAD");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("HEAD text")
        .trim()
        .to_owned()
}

#[test]
fn browser_profile_stores_and_retrieves_row_and_blob_from_format3_store() {
    let (directory, repository, relation_id) = format3_repository(BROWSER_SOURCE);
    let root = directory.path();
    std::fs::copy(
        Path::new(MEDIA_FIXTURES).join("pixel.png"),
        root.join("profile.png"),
    )
    .expect("copy browser profile image");
    let head_before = git_head(root);

    // The app path: the admitted REPL stages the effectful invocation, the
    // runtime commits its table mutation and Blob capture as one request.
    let input = format!(
        "let raw = sys.blob.capture_file({root:?}, \"profile.png\", 65536);\nlet image = sys.blob.annotate(raw, \"image/png\");\nmain.store_profile(\"browser-profile\", image).id",
        root = root.to_string_lossy().as_ref(),
    );
    let output = run_repl(root, &input);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{stdout}\n{:?}",
        output.stderr
    );
    assert!(
        !stdout.contains("ORNA-REPL-EFFECT") && !stdout.contains("ORNA-EVAL-VALUE"),
        "trusted REPL rejected the browser profile invocation: {stdout}"
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert_eq!(
        git_head(root),
        head_before,
        "REPL must not publish a Git commit"
    );

    // Publication makes the committed row durable in the format-3 graph.
    let published = run_publish(root);
    assert!(
        published.status.success(),
        "publish failed: {}",
        String::from_utf8_lossy(&published.stderr)
    );

    // Read the row independently from the published relation graph.
    let rows = committed_store_rows(&repository, relation_id);
    assert_eq!(rows.len(), 1);
    let row = rows
        .iter()
        .find(|row| row.key() == &TypedKey::Text("browser-profile".to_owned()))
        .expect("published browser-profile row is present");
    let blobs = row.blob_fields().expect("the row stores an annotated Blob");
    assert_eq!(blobs.len(), 1);

    // Hydrate the payload only after resolving its stored descriptor from the
    // committed row, using the public native graph range reader.
    let format = repository.open_format_context().unwrap();
    let row_map = format.load_row_map(relation_id).unwrap();
    let graph = format.open_native_graph(&row_map).unwrap();
    let scope = graph.open_read_scope().unwrap();
    let stored_row = graph.lookup_row(row.key(), &scope).unwrap().unwrap();
    let stored_blob = stored_row
        .value()
        .dependencies()
        .iter()
        .find_map(|dependency| {
            if dependency.kind() != orna_repository_v1::NativeObjectKind::Tree {
                return None;
            }
            graph
                .read_stored_blob_value(&stored_row, dependency.oid(), &scope)
                .ok()
        })
        .expect("the published row resolves its Blob descriptor");
    assert_eq!(stored_blob.annotation().media_type(), "image/png");
    assert_eq!(
        stored_blob.reference().content_identity().length(),
        BROWSER_PROFILE_BLOB.len() as u64
    );
    let payload = graph
        .read_blob_range(
            stored_blob.reference(),
            0..BROWSER_PROFILE_BLOB.len() as u64,
            &scope,
        )
        .expect("the committed Blob is retrievable")
        .bytes()
        .to_vec();
    assert_eq!(payload, BROWSER_PROFILE_BLOB);
}
