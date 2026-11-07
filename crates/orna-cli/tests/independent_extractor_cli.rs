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

use orna_evaluator::SysHostBindingRegistry;
use orna_runtime::RuntimeState;
use orna_sys::{EnvironmentProvider, FilesystemProvider};
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
    command
        .args(arguments)
        .output()
        .expect("CLI process starts")
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
    use std::fmt::Write as _;
    let mut out = String::with_capacity(32);
    for byte in relation_id {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

/// The extracted row whose key is `key`. An integer key matches its decimal
/// text, the form the real reader's listings print.
fn extracted_row<'a>(rows: &'a [Value], key: &str) -> &'a Value {
    rows.iter()
        .find(|row| match &row["key"] {
            Value::String(text) => text == key,
            Value::Number(number) => key.parse::<u64>().ok() == number.as_u64(),
            _ => false,
        })
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
        .map_or_else(
            || panic!("the extracted row holds field index {index}: {row}"),
            |field| &field["value"],
        )
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn extractor_recovers_modules_schema_rows_and_blob_bytes_from_an_export() {
    let (directory, repository, relation_id) = empty_format3_repository();
    // Transitional (bead ornadb-5il1.413): the format-3 fixture commits the
    // legacy .orna/object-identities.cbor sidecar, and the product's publish
    // carries it into HEAD. ORNA-META-001 forbids that tracked entry, and this
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
        assert_eq!(
            blob["descriptor"], listing["descriptor"],
            "{key}: descriptor"
        );
        assert_eq!(blob["length"], listing["length"], "{key}: length");
        assert_eq!(
            blob["media_type"], listing["media_type"],
            "{key}: media type"
        );
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
        assert_eq!(
            &recovered, bytes,
            "{key}: the recovered payload equals the source"
        );
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

/// The golden legacy format-1 bundle the migration tests clone.
const GOLDEN_BUNDLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/legacy-format1.bundle"
);
/// The gitlink the golden snapshot pins: a legacy orna snapshot with no origin.
const LEGACY_DEPENDENCY: &str = "vendor/stdlib";
/// The golden copy's relations, as the 32-digit hex the CLI and extractor print.
const GOLDEN_RELATIONS: [&str; 5] = [
    "018f0000000071000000000000000001",
    "018f0000000071000000000000000002",
    "018f0000000071000000000000000003",
    "018f0000000071000000000000000005",
    "2a3cc85f989e8a79a242eef69e41174b",
];
/// The golden copy lists two Blob values, both in relation ...05.
const GOLDEN_BLOB_LISTINGS: usize = 2;

/// Clones the golden bundle into `parent/source`.
fn clone_golden_bundle(parent: &Path) -> PathBuf {
    let source = parent.join("source");
    let clone = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("clone")
        .arg("--quiet")
        .arg(GOLDEN_BUNDLE)
        .arg(&source)
        .output()
        .expect("git starts");
    assert!(
        clone.status.success(),
        "git clone bundle: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    source
}

/// Migrates the golden source into `parent/migrated` and activates it in
/// place. The source is only read by the migration.
fn activated_golden_copy(parent: &Path) -> PathBuf {
    let source = clone_golden_bundle(parent);
    let destination = parent.join("migrated");
    let source_arg = source.to_str().unwrap();
    let destination_arg = destination.to_str().unwrap();
    let prepared = run(
        parent,
        &[
            "--db",
            source_arg,
            "migrate",
            "copy",
            destination_arg,
            "--no-persistent-consumers",
        ],
    );
    assert_eq!(
        prepared.status.code(),
        Some(0),
        "migrate copy: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let activated = run(
        parent,
        &[
            "--db",
            source_arg,
            "upgrade",
            "--to-format",
            "3",
            destination_arg,
            "--yes-activate",
        ],
    );
    assert_eq!(
        activated.status.code(),
        Some(0),
        "upgrade --yes-activate: {}",
        String::from_utf8_lossy(&activated.stderr)
    );
    destination
}

/// Drops the golden snapshot's legacy gitlink with a plain commit. The
/// complete-copy export pins no legacy dependency only after this step; the
/// shipped golden snapshot fails export with E2000 (no recorded origin).
fn drop_legacy_dependency(copy: &Path) {
    git(copy, &["rm", "--quiet", "--", LEGACY_DEPENDENCY]);
    git(
        copy,
        &["commit", "--quiet", "-m", "drop legacy dependency gitlink"],
    );
}

/// A migrated golden copy, exported with the real `orna export`, restored,
/// and removed from the disk: the extractor's rows, schema, modules and Blob
/// records must match what the real reader reports for the same copy, and
/// each Blob's recovered bytes must hash to the real reader's length and
/// SHA-256.
#[allow(clippy::too_many_lines)]
#[test]
fn extractor_matches_the_real_reader_on_a_migrated_golden_copy() {
    let parent = TempDir::new().unwrap();
    let copy = activated_golden_copy(parent.path());
    drop_legacy_dependency(&copy);
    let snapshot = String::from_utf8(git_output(&copy, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_owned();

    // The real reader's answer for every golden relation, read while the copy
    // still exists.
    let source_reports: Vec<(&str, Value)> = GOLDEN_RELATIONS
        .iter()
        .map(|relation| {
            let query = run(&copy, &["query", relation, "--format", "json"]);
            assert_eq!(
                query.status.code(),
                Some(0),
                "{relation}: source query failed: {}",
                String::from_utf8_lossy(&query.stderr)
            );
            (*relation, serde_json::from_slice(&query.stdout).unwrap())
        })
        .collect();

    let outside = TempDir::new().unwrap();
    let archive = outside.path().join("archive");
    let archive_path = archive.to_str().unwrap();
    let exported = run(&copy, &["export", archive_path, "--at", "HEAD"]);
    assert_eq!(
        exported.status.code(),
        Some(0),
        "export: {}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let restored: PathBuf = outside.path().join("restored");
    let restored_path = restored.to_str().unwrap().to_owned();
    let reconstructed = run(
        &copy,
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
        "restore: {}",
        String::from_utf8_lossy(&reconstructed.stderr)
    );

    // The migrated copy and its source are gone before the extractor runs.
    drop(parent);
    assert!(!copy.exists(), "the migrated copy is removed");

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

    let relations = report["snapshot"]["relations"].as_array().unwrap();
    let mut extracted_ids: Vec<&str> = relations
        .iter()
        .map(|entry| entry["relation_id"].as_str().unwrap())
        .collect();
    extracted_ids.sort_unstable();
    let mut expected_ids = GOLDEN_RELATIONS.to_vec();
    expected_ids.sort_unstable();
    assert_eq!(
        extracted_ids, expected_ids,
        "the extractor recovers exactly the golden relations"
    );

    let blobs = report["blobs"].as_array().unwrap();
    let mut listed = 0;
    for (relation, source) in &source_reports {
        let entry = relations
            .iter()
            .find(|entry| entry["relation_id"] == *relation)
            .unwrap_or_else(|| panic!("the extractor recovers relation {relation}"));
        let rows = entry["rows"].as_array().unwrap();
        let source_rows = source["rows"].as_u64().unwrap();
        assert_eq!(
            rows.len() as u64,
            source_rows,
            "{relation}: the extractor recovers every row the real reader counts"
        );
        assert_eq!(entry["row_count"].as_u64(), Some(source_rows));
        for listing in source["listings"].as_array().unwrap() {
            listed += 1;
            let key = listing["key"].as_str().unwrap();
            let field = listing["field"].as_u64().unwrap();
            let row = extracted_row(rows, key);
            let blob = &extracted_field(row, field)["blob"];
            for name in ["descriptor", "length", "media_type", "suffix", "sha256"] {
                assert_eq!(
                    blob[name], listing[name],
                    "{relation} {key}: {name} matches the real reader"
                );
            }
            let path = blob["path"].as_str().unwrap();
            let recovered = std::fs::read(output.join(path)).unwrap();
            assert_eq!(
                recovered.len() as u64,
                listing["length"].as_u64().unwrap(),
                "{key}: the recovered length"
            );
            assert_eq!(
                hex(Sha256::digest(&recovered).as_slice()),
                listing["sha256"].as_str().unwrap(),
                "{key}: the recovered bytes hash to the real reader's SHA-256"
            );
            assert!(
                blobs.iter().any(|record| record["path"] == path),
                "{key}: the Blob is listed in the report's blob table"
            );
        }
    }
    assert_eq!(
        listed, GOLDEN_BLOB_LISTINGS,
        "the real reader lists both golden Blobs"
    );

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

/// The extractor reads format 3 only. A migrated copy that still pins the
/// legacy dependency is refused with exit 1, not read partially.
#[test]
fn extractor_refuses_a_migrated_copy_that_pins_an_unreadable_legacy_dependency() {
    let parent = TempDir::new().unwrap();
    let copy = activated_golden_copy(parent.path());
    let output = parent.path().join("extracted");
    let extracted = extract(&copy, &output);
    let stderr = String::from_utf8_lossy(&extracted.stderr);
    assert_eq!(extracted.status.code(), Some(1), "{stderr}");
    assert!(
        extracted.stdout.is_empty(),
        "a refused extraction prints no partial report"
    );
    assert!(
        stderr.contains("vendor/stdlib") && stderr.contains("is unavailable"),
        "the refusal names the unreadable dependency: {stderr}"
    );
}

/// The origin the legacy stdlib submodule records. Export reads it from
/// `.gitmodules` and never fetches it.
const LEGACY_DEPENDENCY_ORIGIN: &str = "https://github.com/workingdir/ornadb-std.git";

/// Records the golden gitlink's origin in `.gitmodules` with a plain commit,
/// keeping the pin. The shipped snapshot records no origin.
fn record_legacy_dependency_origin(copy: &Path) {
    let path_key = format!("submodule.{LEGACY_DEPENDENCY}.path");
    let url_key = format!("submodule.{LEGACY_DEPENDENCY}.url");
    git(
        copy,
        &["config", "-f", ".gitmodules", &path_key, LEGACY_DEPENDENCY],
    );
    git(
        copy,
        &[
            "config",
            "-f",
            ".gitmodules",
            &url_key,
            LEGACY_DEPENDENCY_ORIGIN,
        ],
    );
    git(copy, &["add", "--", ".gitmodules"]);
    git(
        copy,
        &["commit", "--quiet", "-m", "record legacy dependency origin"],
    );
}

/// The golden copy as shipped, gitlink kept. Export refuses the missing origin
/// (ORNA-GIT-001). With the origin recorded and the pinned dependency supplied,
/// export and restore carry the dependency at its exact pin (ORNA-GIT-003,
/// ORNA-GIT-007). The independent extractor still refuses the restored copy,
/// because the pinned dependency is a legacy format-1/2 snapshot it does not
/// read; the refusal is defined and names the dependency.
#[test]
fn migrated_golden_copy_exports_and_restores_its_pinned_legacy_dependency() {
    let parent = TempDir::new().unwrap();
    let copy = activated_golden_copy(parent.path());
    let dependency = parent.path().join("source");
    let probe = parent.path().join("shipped-archive");
    let refused = run(&copy, &["export", probe.to_str().unwrap(), "--at", "HEAD"]);
    assert_eq!(
        refused.status.code(),
        Some(1),
        "the shipped golden refuses export: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("has no recorded origin"),
        "the shipped refusal names the missing origin: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!probe.exists(), "no archive is written on refusal");

    record_legacy_dependency_origin(&copy);
    let pin = String::from_utf8(git_output(
        &copy,
        &["ls-tree", "HEAD", "--", LEGACY_DEPENDENCY],
        None,
    ))
    .unwrap();
    let pinned = pin
        .split_whitespace()
        .nth(2)
        .expect("the golden snapshot pins the dependency")
        .to_owned();

    let outside = TempDir::new().unwrap();
    let archive = outside.path().join("archive");
    let archive_path = archive.to_str().unwrap();
    let unavailable = run(&copy, &["export", archive_path, "--at", "HEAD"]);
    assert_eq!(
        unavailable.status.code(),
        Some(1),
        "a recorded origin without its repository is refused: {}",
        String::from_utf8_lossy(&unavailable.stderr)
    );
    assert!(
        String::from_utf8_lossy(&unavailable.stderr).contains("A pinned dependency is unavailable"),
        "the refusal names the unavailable dependency: {}",
        String::from_utf8_lossy(&unavailable.stderr)
    );
    assert!(!archive.exists(), "no archive is written on refusal");

    let dependency_arg = format!("{LEGACY_DEPENDENCY}={}", dependency.to_str().unwrap());
    let exported = run(
        &copy,
        &[
            "export",
            archive_path,
            "--at",
            "HEAD",
            "--dependency",
            &dependency_arg,
        ],
    );
    assert_eq!(
        exported.status.code(),
        Some(0),
        "export: {}",
        String::from_utf8_lossy(&exported.stderr)
    );

    let restored = outside.path().join("restored");
    let restored_arg = restored.to_str().unwrap();
    let reconstructed = run(
        &copy,
        &[
            "export",
            archive_path,
            "--restore",
            restored_arg,
            "--worktree",
        ],
    );
    assert_eq!(
        reconstructed.status.code(),
        Some(0),
        "restore: {}",
        String::from_utf8_lossy(&reconstructed.stderr)
    );
    let restored_dependency = restored.join(LEGACY_DEPENDENCY);
    assert_eq!(
        String::from_utf8(git_output(
            &restored_dependency,
            &["rev-parse", "HEAD"],
            None
        ))
        .unwrap()
        .trim(),
        pinned,
        "the restored dependency is checked out at its exact pin"
    );

    // The copy and the dependency source are gone before the extractor runs.
    drop(parent);
    let output = outside.path().join("extracted");
    let extracted = extract(&restored, &output);
    let stderr = String::from_utf8_lossy(&extracted.stderr);
    assert_eq!(extracted.status.code(), Some(1), "{stderr}");
    assert!(
        extracted.stdout.is_empty(),
        "a refused extraction prints no partial report"
    );
    assert!(
        stderr.contains("vendor/stdlib") && stderr.contains("legacy format-1/2 snapshot"),
        "the refusal names the legacy dependency: {stderr}"
    );
}
