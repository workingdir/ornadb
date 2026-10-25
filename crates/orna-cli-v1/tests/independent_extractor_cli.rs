//! Gate F: the independent extractor recovers a complete copy without the
//! engine's own readers.
//!
//! The walkthrough repository holds a song row and an image row. It is frozen
//! with `orna export --at HEAD`, reconstructed into a restored copy, and the
//! source is deleted. `scripts/orna_extract.py` then reads only the copy's Git
//! objects through `git cat-file --batch`, and must report the same rows that
//! `orna query --format json` reported from the source before the source was
//! removed. Each Blob's bytes come back from the extractor's `blobs/` output and
//! must hash to the recorded (length, SHA-256) and equal the source payload.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_runtime_v1::RuntimeState;
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::commit_capture;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
const SONG_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-song.orna"
);
const IMAGE_IMPORT_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/import-image.orna"
);
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";
const EXTRACTOR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/orna_extract.py");

/// Runs `orna <arguments>` in `directory` with Git routing isolated, the same
/// way the other CLI tests do.
fn run(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command, directory);
    command.args(arguments).output().expect("CLI process starts")
}

/// Runs the standard-library extractor over `repository` at HEAD, writing Blob
/// bytes under `output`.
fn extract(repository: &Path, output: &Path) -> Output {
    let mut command = Command::new("python3");
    isolate_git(&mut command, repository);
    command
        .arg(EXTRACTOR)
        .arg(repository)
        .arg("HEAD")
        .arg("--output")
        .arg(output)
        .output()
        .expect("python3 starts the extractor")
}

fn isolate_git(command: &mut Command, directory: &Path) {
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
}

/// An import expression whose media root points at `root`.
fn import_expression(fixture: &str, root: &Path) -> String {
    let fixture = std::fs::read_to_string(fixture).unwrap();
    let root = format!("{:?}", root.to_string_lossy().as_ref());
    fixture.trim_end().replace(MEDIA_ROOT_PLACEHOLDER, &root)
}

/// The 16-byte relation id as the 32-digit hex the CLI and extractor print.
fn relation_hex(relation_id: [u8; 16]) -> String {
    relation_id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The extracted row whose key is the text `key`.
fn extracted_row<'a>(rows: &'a [Value], key: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["key"] == key)
        .unwrap_or_else(|| panic!("the extractor recovers the {key} row"))
}

/// The stored field at row index `index` of an extracted row. The source
/// listing names a field by its index, which the extractor also records.
fn extracted_field(row: &Value, index: u64) -> &Value {
    row["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["index"].as_u64() == Some(index))
        .map(|field| &field["value"])
        .unwrap_or_else(|| panic!("the extracted row holds field index {index}: {row}"))
}

#[tokio::test]
async fn extractor_recovers_modules_schema_rows_and_blob_bytes_from_an_export() {
    let (directory, repository, relation_id) = empty_format3_repository();
    // Transitional (bead ornadb-9cvu): the format-3 test fixture
    // (tests/support/format3.rs:451 writes, :530 commits) puts the legacy
    // .orna/object-identities.cbor sidecar into HEAD; `orna publish` adds no
    // commit for it. ORNA-META-001 forbids that tracked entry, and this
    // extractor rejects it. Drop the tracked sidecar here, before any runtime
    // or publish step reads the repository, so the snapshot this test exports
    // satisfies the format-3 rule.
    git(
        directory.path(),
        &["rm", "--quiet", "--", ".orna/object-identities.cbor"],
    );
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "drop legacy identity sidecar"],
    );
    let source = TempDir::new().unwrap();
    for name in ["tone.wav", "pixel.png"] {
        std::fs::copy(
            Path::new(MEDIA_FIXTURES).join(name),
            source.path().join(name),
        )
        .unwrap();
    }

    let (runtime_identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .unwrap();
    let writer = state.acquire_lease([0x64; 16]).await.unwrap();
    let capability = repository.capture_capability(relation_id).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    for (key, fixture, ordinal) in [
        ("song", SONG_IMPORT_FIXTURE, 0x90_u8),
        ("image", IMAGE_IMPORT_FIXTURE, 0xa0_u8),
    ] {
        let expression = import_expression(fixture, source.path());
        commit_capture(
            &repository,
            &state,
            writer,
            &mut bindings,
            &expression,
            key,
            ordinal,
            true,
            relation_id,
        )
        .await;
    }
    drop(bindings);
    drop(state);
    let project = directory.path().to_path_buf();
    let publication = run(&project, &["publish"]);
    assert_eq!(
        publication.status.code(),
        Some(0),
        "publish failed: {}",
        String::from_utf8_lossy(&publication.stderr)
    );

    // The payload each row must still serve, read from the source before it is
    // frozen, so the extractor is compared against something real.
    let expected = [("song", "tone.wav"), ("image", "pixel.png")].map(|(key, file)| {
        (
            key,
            std::fs::read(Path::new(MEDIA_FIXTURES).join(file)).unwrap(),
        )
    });

    let relation = relation_hex(relation_id);
    let source_query = run(&project, &["query", &relation, "--format", "json"]);
    assert_eq!(
        source_query.status.code(),
        Some(0),
        "source query failed: {}",
        String::from_utf8_lossy(&source_query.stderr)
    );
    let source_report: Value = serde_json::from_slice(&source_query.stdout).unwrap();
    let listings = source_report["listings"].as_array().unwrap();
    assert_eq!(listings.len(), 2, "both rows are listed by the source");
    let snapshot = String::from_utf8(git_output(&project, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();

    // Freeze the snapshot and reconstruct it into a copy that is not the source.
    let outside = TempDir::new().unwrap();
    let archive = outside.path().join("archive");
    let archive_path = archive.to_str().unwrap();
    let exported = run(&project, &["export", archive_path, "--at", "HEAD"]);
    assert_eq!(
        exported.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let restored: PathBuf = outside.path().join("restored");
    let restored_path = restored.to_str().unwrap().to_owned();
    let reconstructed = run(
        &project,
        &[
            "export",
            archive_path,
            "--restore",
            restored_path.as_str(),
            "--worktree",
        ],
    );
    assert_eq!(
        reconstructed.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&reconstructed.stderr)
    );

    // The source repository and its worktree are gone before the extractor runs.
    drop(repository);
    drop(directory);
    drop(source);
    assert!(!project.exists(), "the source worktree is removed");

    // The extractor reads only the copy's Git objects.
    let output = outside.path().join("extracted");
    let extracted = extract(&restored, &output);
    assert_eq!(
        extracted.status.code(),
        Some(0),
        "extractor failed: {}",
        String::from_utf8_lossy(&extracted.stderr)
    );
    let report: Value = serde_json::from_slice(&extracted.stdout).unwrap();
    assert_eq!(
        report["snapshot"]["commit"],
        snapshot.as_str(),
        "the extracted snapshot"
    );

    // Rows and schema: the relation the source queried, with exactly the rows
    // the source listed.
    let relations = report["snapshot"]["relations"].as_array().unwrap();
    let relation_report = relations
        .iter()
        .find(|entry| entry["relation_id"] == relation.as_str())
        .unwrap_or_else(|| panic!("the extractor recovers relation {relation}"));
    let rows = relation_report["rows"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        2,
        "the extractor recovers every committed row of the relation"
    );
    assert_eq!(
        relation_report["row_count"].as_u64(),
        Some(rows.len() as u64)
    );
    assert!(
        !relation_report["schema"]["definitions"].is_null(),
        "the relation's schema definitions are recovered"
    );

    // Each source listing is matched by key and field: the extracted descriptor,
    // length, media type, suffix and SHA-256 equal what the source reported.
    for listing in listings {
        let key = listing["key"].as_str().unwrap();
        let row = extracted_row(rows, key);
        let field = listing["field"].as_u64().unwrap();
        let blob = &extracted_field(row, field)["blob"];
        assert_eq!(blob["descriptor"], listing["descriptor"], "{key}: descriptor");
        assert_eq!(blob["length"], listing["length"], "{key}: length");
        assert_eq!(blob["media_type"], listing["media_type"], "{key}: media type");
        assert_eq!(blob["suffix"], listing["suffix"], "{key}: suffix");
        assert_eq!(blob["sha256"], listing["sha256"], "{key}: SHA-256");
    }

    // Blob bytes: the extracted file for each Blob hashes to the recorded
    // (length, SHA-256) and equals the payload the source held.
    let blobs = report["blobs"].as_array().unwrap();
    for listing in listings {
        let key = listing["key"].as_str().unwrap();
        let field = listing["field"].as_u64().unwrap();
        let (_, bytes) = expected
            .iter()
            .find(|(name, _)| *name == key)
            .unwrap_or_else(|| panic!("the source payload for {key} is known"));
        let row = extracted_row(rows, key);
        let blob = &extracted_field(row, field)["blob"];
        let path = blob["path"].as_str().unwrap();
        let recovered = std::fs::read(output.join(path)).unwrap();
        assert_eq!(recovered.len() as u64, bytes.len() as u64, "{key}: length");
        assert_eq!(
            hex(Sha256::digest(&recovered).as_slice()),
            blob["sha256"].as_str().unwrap(),
            "{key}: the recovered bytes hash to the recorded SHA-256"
        );
        assert_eq!(&recovered, bytes, "{key}: the recovered payload equals the source");
        assert!(
            blobs.iter().any(|record| record["path"] == path),
            "{key}: the Blob is listed in the report's blob table"
        );
    }

    // Modules: every module the extractor reports is the blob the restored
    // copy's Git tree holds at that path.
    for module in report["snapshot"]["modules"].as_array().unwrap() {
        let path = module["path"].as_str().unwrap();
        let content = git_output(&restored, &["show", &format!("{snapshot}:{path}")], None);
        assert_eq!(
            module["size"].as_u64(),
            Some(content.len() as u64),
            "{path}: the module size"
        );
        assert_eq!(
            module["text"].as_str().unwrap().as_bytes(),
            content.as_slice(),
            "{path}: the module text matches the tree"
        );
    }
}
