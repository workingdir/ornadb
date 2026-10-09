//! After a Blob replace edit is published, `orna history` lists the edit as
//! its own revision and `orna query --at <prior commit>` still reports the
//! descriptor that commit named.
//!
//! The two verbs run as the real CLI binary in the fixture worktree, so this
//! pins the user-visible contract: a `--at` selector is resolved once to one
//! commit, and the row map, the listing and the revision walk all come from
//! that commit's own objects. A later edit therefore cannot retarget an
//! earlier answer, and a pinned listing never reads a media payload.

use std::path::{Path, PathBuf};

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::*;

#[path = "support/media_commit.rs"]
mod media_commit;
use media_commit::commit_capture;

#[path = "support/orna_cli_run.rs"]
mod orna_cli_run;
use orna_cli_run::run_in;

const MEDIA_FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");
/// The capture fixture, retargeted at one payload file and one media root.
const CAPTURE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/media/replace-image-capture.orna"
);
const MEDIA_ROOT_PLACEHOLDER: &str = "__MEDIA_ROOT__";
const PAYLOAD_PLACEHOLDER: &str = "__PAYLOAD__";

/// The first payload: the shared 1x1 RGBA PNG.
const ORIGINAL_PIXEL: &[u8] = include_bytes!("fixtures/media/pixel.png");
/// The replacement payload: the shared 2x2 RGBA PNG, same media type and
/// suffix, different bytes, so only the content identity can move.
const REPLACEMENT_PIXEL: &[u8] = include_bytes!("fixtures/media/pixel2.png");

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

/// The capture expression that reads `payload` out of the media root.
fn capture_expression(root: &Path, payload: &str) -> String {
    let root = format!("{:?}", root.to_string_lossy().as_ref());
    std::fs::read_to_string(CAPTURE_FIXTURE)
        .unwrap()
        .trim_end()
        .replace(PAYLOAD_PLACEHOLDER, payload)
        .replace(MEDIA_ROOT_PLACEHOLDER, &root)
}

/// The published commit the fixture repository currently points at.
fn published_head(worktree: &Path) -> String {
    let output = std::process::Command::new("git")
        .args(["-C"])
        .arg(worktree)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse runs");
    assert!(output.status.success(), "git rev-parse succeeds");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Runs one `orna` verb in the fixture worktree and parses its JSON report.
fn verb_json(worktree: &Path, arguments: &[&str]) -> Json {
    let output = run_in(worktree, arguments);
    assert!(
        output.status.success(),
        "orna {arguments:?} succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("the verb prints a JSON report")
}

/// The single listing of a one-row query report.
fn only_listing(report: &Json) -> &Json {
    let listings = report["listings"].as_array().expect("listings is an array");
    assert_eq!(listings.len(), 1, "the relation holds exactly one row");
    &listings[0]
}

/// Opens a committed media row through the admitted request path, publishes
/// it, and returns the commit it landed in.
async fn publish_capture(
    repository: &Repository,
    state: &RuntimeState,
    bindings: &mut SysHostBindingRegistry,
    expression: &str,
    key: &str,
    ordinal: u8,
    insert_only: bool,
) -> String {
    let writer = state.acquire_lease([0x63; 16]).await.unwrap();
    commit_capture(
        repository,
        state,
        writer,
        bindings,
        expression,
        key,
        ordinal,
        insert_only,
    )
    .await;
    published_head(repository.worktree())
}

#[tokio::test]
async fn a_pinned_query_reports_the_descriptor_that_commit_named() {
    let (_directory, repository, relation) = empty_format3_repository();
    let source = TempDir::new().unwrap();
    for payload in ["pixel.png", "pixel2.png"] {
        std::fs::copy(
            Path::new(MEDIA_FIXTURES).join(payload),
            source.path().join(payload),
        )
        .unwrap();
    }

    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [0, 0, 0, 0, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 0, 1],
            repository_id: [0x61; 16],
        },
        [0x62; 32],
    )
    .await
    .unwrap();
    let capability = repository.capture_capability(relation).unwrap();
    let mut filesystem = FilesystemProvider::with_limits(1 << 20, 16).unwrap();
    filesystem.allow_root(source.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem)
        .with_repository_capture_capability(capability);

    let first_commit = publish_capture(
        &repository,
        &state,
        &mut bindings,
        &capture_expression(source.path(), "pixel.png"),
        "image",
        0xb0,
        true,
    )
    .await;

    let relation = hex(&relation);
    let worktree = repository.worktree().to_path_buf();
    let at_first = verb_json(
        &worktree,
        &[
            "query",
            &relation,
            "--key",
            "image",
            "--at",
            &first_commit,
            "--format",
            "json",
        ],
    );
    let first_listing = only_listing(&at_first);
    assert_eq!(first_listing["media_type"], "image/png");
    assert_eq!(
        first_listing["sha256"],
        hex(&Sha256::digest(ORIGINAL_PIXEL)),
        "the pinned row names the payload that commit captured"
    );
    let first_descriptor = first_listing["descriptor"]
        .as_str()
        .expect("a stored reference names its descriptor")
        .to_owned();
    assert_eq!(
        at_first["media_payload_bytes_read"], 0,
        "a pinned listing resolves the descriptor without reading the payload"
    );

    // Replace the payload and publish again: the row's own reference moves.
    let second_commit = publish_capture(
        &repository,
        &state,
        &mut bindings,
        &capture_expression(source.path(), "pixel2.png"),
        "image",
        0xc0,
        false,
    )
    .await;
    assert_ne!(
        second_commit, first_commit,
        "a published edit is its own commit"
    );

    // The edit is listed as a revision of its own, beside the capture.
    let history = verb_json(&worktree, &[&relation, "image", "--format", "json"]);
    let revisions: Vec<&str> = history
        .as_array()
        .expect("history prints an array")
        .iter()
        .map(|revision| revision["commit"].as_str().expect("commit is hex"))
        .collect();
    assert!(
        revisions.contains(&first_commit.as_str()) && revisions.contains(&second_commit.as_str()),
        "the listing carries the capture and the edit: {revisions:?}"
    );

    // The same query answers per snapshot: the older pin keeps the older
    // descriptor, the newer pin and the unpinned read report the replacement.
    let at_second = verb_json(
        &worktree,
        &[
            "query",
            &relation,
            "--key",
            "image",
            "--at",
            &second_commit,
            "--format",
            "json",
        ],
    );
    let head = verb_json(
        &worktree,
        &[&relation, "--key", "image", "--format", "json"],
    );
    let second_listing = only_listing(&at_second);
    let head_listing = only_listing(&head);

    assert_eq!(
        second_listing["sha256"],
        hex(&Sha256::digest(REPLACEMENT_PIXEL)),
        "the newer pin names the replacement payload"
    );
    assert_eq!(
        head_listing["sha256"],
        hex(&Sha256::digest(REPLACEMENT_PIXEL)),
        "an unpinned read still follows the published head"
    );
    let second_descriptor = second_listing["descriptor"].as_str().unwrap();
    assert_ne!(
        second_descriptor, first_descriptor,
        "the replacement payload mints a different descriptor"
    );
    assert_eq!(
        head_listing["descriptor"], second_descriptor,
        "the unpinned read reports the current descriptor"
    );
    assert_eq!(
        second_listing["media_type"], first_listing["media_type"],
        "replacing a same-type payload leaves the annotation alone"
    );

    drop(worktree);
}

/// `orna query --at` refuses a selector that names more than one branch, tag
/// or remote branch, exactly as `orna history --at` does.
///
/// Git resolves such a name by ref precedence and reports success with only a
/// warning, so a pinned query would silently answer from whichever ref it
/// preferred and nothing in the listing could report that choice.
#[test]
fn a_query_pin_refuses_an_ambiguous_selector() {
    let directory = TempDir::new().unwrap();
    let root = directory.path();
    let git = |arguments: &[&str]| {
        let output = std::process::Command::new("git")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(arguments)
            .current_dir(root)
            .output()
            .expect("Git process");
        assert!(output.status.success(), "git {arguments:?}");
    };
    git(&["init", "--quiet"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("main.orna"), b"pub table Music(id: Str) {}\n").unwrap();
    git(&["add", "--all"]);
    git(&[
        "-c",
        "user.email=kieran@drewett.dev",
        "-c",
        "user.name=kierandrewett",
        "commit",
        "--quiet",
        "-m",
        "one",
    ]);
    git(&["branch", "collide"]);
    git(&["tag", "collide"]);

    // The relation exists in no repository: the ambiguity gate runs before any
    // row is read, so this case never reaches one.
    let relation = "0".repeat(32);
    let output = run_in(
        root,
        &["query", &relation, "--at", "collide", "--format", "json"],
    );

    assert_eq!(
        output.status.code(),
        Some(1),
        "an ambiguous pin fails closed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "nothing may be listed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ambiguous"),
        "the refusal names the ambiguity rather than an unrelated failure: {stderr}"
    );
}
