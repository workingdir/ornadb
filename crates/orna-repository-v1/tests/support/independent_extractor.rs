//! Independent extractor for one complete-copy archive.
//!
//! Gate F requires the reconstruction to be checked by an extractor that is
//! independent of the code under test. This module therefore shares nothing
//! with `orna_repository_v1::complete_copy`:
//!
//! - it parses `manifest.tsv` itself,
//! - it moves objects with the `git` binary alone,
//! - it recomputes every blob's object ID from the raw bytes it read, with the
//!   `sha1` hash crate rather than the crate under test, and
//! - it compares that against the object ID the manifest recorded.
//!
//! Included by test binaries with `#[path = "support/independent_extractor.rs"]
//! mod independent_extractor;`.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use sha1::{Digest, Sha1};

/// One object the manifest records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedObject {
    pub kind: String,
    pub oid: String,
    pub size: u64,
}

/// One snapshot the manifest records, with that member's own closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedMember {
    pub commit: String,
    pub bundle: String,
    pub objects: Vec<RecordedObject>,
}

/// Everything one archive records, read without `orna-repository-v1`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedArchive {
    pub snapshot: String,
    pub members: Vec<RecordedMember>,
    pub dependencies: Vec<(String, String)>,
}

/// Reads `manifest.tsv` directly.
pub fn read_manifest(archive: &Path) -> RecordedArchive {
    let document = fs::read_to_string(archive.join("manifest.tsv")).expect("manifest is readable");
    let mut lines = document.lines();
    let header = lines.next().expect("manifest is not empty");
    let legacy = header == "orna-complete-copy 1";
    assert!(
        legacy || header == "orna-complete-copy 2",
        "unexpected manifest header {header:?}"
    );
    let mut snapshot = None;
    let mut members: Vec<RecordedMember> = Vec::new();
    let mut dependencies = Vec::new();
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["snapshot", commit] => snapshot = Some((*commit).to_owned()),
            ["archive", commit, bundle] => members.push(RecordedMember {
                commit: (*commit).to_owned(),
                bundle: (*bundle).to_owned(),
                objects: Vec::new(),
            }),
            ["object", kind, oid, size] => {
                let object = RecordedObject {
                    kind: (*kind).to_owned(),
                    oid: (*oid).to_owned(),
                    size: size.parse().expect("object size is a number"),
                };
                if let Some(member) = members.last_mut() {
                    member.objects.push(object);
                } else {
                    // A header-1 archive lists one closure without a member
                    // line; it belongs to the single snapshot of that archive.
                    assert!(legacy, "object line before any archive line");
                    members.push(RecordedMember {
                        commit: snapshot.clone().expect("snapshot line precedes objects"),
                        bundle: "superproject.bundle".to_owned(),
                        objects: vec![object],
                    });
                }
            }
            ["dependency", path, commit, ..] => {
                dependencies.push(((*path).to_owned(), (*commit).to_owned()));
            }
            other => panic!("unrecognised manifest line: {other:?}"),
        }
    }
    RecordedArchive {
        snapshot: snapshot.expect("manifest records a snapshot"),
        members,
        dependencies,
    }
}

/// Recomputes the object ID of `bytes` as Git does: `sha1("<kind> <len>\0" || bytes)`.
pub fn object_id(kind: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("{kind} {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

fn git(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(arguments)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Moves one archive member into `destination` using the bundle alone.
///
/// No other archive file is read and no remote is configured, so a member that
/// only survives because the source repository still exists is not extractable
/// here and the test fails.
pub fn extract_member(archive: &Path, member: &RecordedMember, destination: &Path) {
    fs::create_dir_all(destination).expect("create extraction directory");
    git(destination, &["init", "--quiet", "--template=", "."]);
    git(
        destination,
        &[
            "-c",
            "fetch.recurseSubmodules=false",
            "fetch",
            "--no-tags",
            archive.join(&member.bundle).to_str().expect("UTF-8 path"),
            &format!("+refs/heads/orna-complete-copy:refs/heads/extracted"),
        ],
    );
    let resolved = String::from_utf8(git(
        destination,
        &["rev-parse", "refs/heads/extracted"],
    ))
    .expect("git output is UTF-8");
    assert_eq!(
        resolved.trim(),
        member.commit,
        "the bundle carries the recorded member commit"
    );
}

/// Reads every recorded blob of `member` back from `repository` and checks its
/// raw bytes against the manifest with this module's own object-ID hashing.
///
/// Returns the number of verified blobs.
pub fn verify_blobs(repository: &Path, member: &RecordedMember) -> usize {
    let mut verified = 0;
    for object in &member.objects {
        let bytes = git(repository, &["cat-file", "blob", &object.oid]);
        assert_eq!(
            bytes.len() as u64,
            object.size,
            "blob {} length matches the manifest",
            object.oid
        );
        assert_eq!(
            object_id("blob", &bytes),
            object.oid,
            "blob {} raw bytes hash back to the recorded object ID",
            object.oid
        );
        verified += 1;
    }
    verified
}

/// Every blob both members record, keyed by object ID, for cross-checking that
/// two snapshots in one archive are genuinely distinct closures.
pub fn blobs_by_oid(member: &RecordedMember) -> BTreeMap<String, u64> {
    member
        .objects
        .iter()
        .filter(|object| object.kind == "blob")
        .map(|object| (object.oid.clone(), object.size))
        .collect()
}

/// The worktree-relative path a blob is reachable at inside `commit`.
pub fn tree_paths(repository: &Path, commit: &str) -> Vec<(String, String)> {
    let listing = String::from_utf8(git(
        repository,
        &["ls-tree", "-r", "-t", commit],
    ))
    .expect("git output is UTF-8");
    listing
        .lines()
        .filter_map(|line| {
            let (header, path) = line.split_once('\t')?;
            let mut fields = header.split_ascii_whitespace();
            let (kind, oid) = (fields.next()?, fields.next()?);
            Some((path.to_owned(), format!("{kind} {oid}")))
        })
        .collect()
}

/// Reads one path's committed bytes at `commit`.
pub fn read_path(repository: &Path, commit: &str, path: &str) -> Vec<u8> {
    git(repository, &["cat-file", "blob", &format!("{commit}:{path}")])
}

/// Where a helper materialises an extraction.
pub fn scratch(prefix: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("orna-extractor-{prefix}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    base
}
