//! Independent extractor for one complete-copy archive.
//!
//! Gate F requires the reconstruction to be checked by an extractor that is
//! independent of the code under test. This module therefore shares nothing
//! with `orna_repository::complete_copy`:
//!
//! - it parses `manifest.tsv` itself,
//! - it moves objects with the `git` binary alone,
//! - it recomputes every blob's object ID from the raw bytes it read, with the
//!   `sha1` hash crate rather than the crate under test, and
//! - it compares that against the object ID the manifest recorded.
//!
//! Included by test binaries with `#[path = "support/independent_extractor.rs"]
//! mod independent_extractor;`.

// Included by path from more than one test target.
#![allow(dead_code)]

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
    /// The member's recorded repository format as the manifest spells it
    /// (`legacy-1`, `legacy-2` or `format-3`), or `None` in an archive written
    /// before the per-member format column existed.
    pub format: Option<String>,
    pub objects: Vec<RecordedObject>,
}

/// Everything one archive records, read without `orna-repository`.
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
        legacy || header == "orna-complete-copy 2" || header == "orna-complete-copy 3",
        "unexpected manifest header {header:?}"
    );
    // Only the newest header states each member's recorded format, and this
    // module reads the columns the header it found says are there.
    let per_member_format = header == "orna-complete-copy 3";
    let mut snapshot = None;
    let mut members: Vec<RecordedMember> = Vec::new();
    let mut dependencies = Vec::new();
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["snapshot", commit] => snapshot = Some((*commit).to_owned()),
            ["archive", commit, bundle] => {
                assert!(
                    !per_member_format,
                    "a header-3 archive names each member's format: {line:?}"
                );
                members.push(RecordedMember {
                    commit: (*commit).to_owned(),
                    bundle: (*bundle).to_owned(),
                    format: None,
                    objects: Vec::new(),
                });
            }
            ["archive", commit, bundle, format] => {
                assert!(
                    per_member_format,
                    "a header-2 archive names no member format: {line:?}"
                );
                assert!(
                    matches!(*format, "legacy-1" | "legacy-2" | "format-3"),
                    "the member format names a known reader: {line:?}"
                );
                members.push(RecordedMember {
                    commit: (*commit).to_owned(),
                    bundle: (*bundle).to_owned(),
                    format: Some((*format).to_owned()),
                    objects: Vec::new(),
                });
            }
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
                        format: None,
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

/// The format one member's own committed metadata records, read with Git alone.
///
/// A legacy member commits `.orna/format.orna`, whose `repository_format`
/// field is the numeric coordinate (1.0.0 `19-repository.md`); a format-3 member
/// commits no such record and carries `.orna/store` instead. This is checked
/// here so the manifest's claim is verified against the member's tree rather
/// than against the crate under test.
pub fn committed_format(repository: &Path, commit: &str) -> &'static str {
    let record = format!("{commit}:.orna/format.orna");
    if git_succeeds(repository, &["cat-file", "-e", &record]) {
        let document =
            String::from_utf8(git(repository, &["show", &record])).expect("git output is UTF-8");
        if document.contains("repository_format: 2") {
            return "legacy-2";
        }
        assert!(
            document.contains("repository_format: 1"),
            "the committed format record names a known coordinate: {document:?}"
        );
        return "legacy-1";
    }
    assert!(
        git_succeeds(
            repository,
            &["cat-file", "-e", &format!("{commit}:.orna/store")]
        ),
        "a member with no format record commits a format-3 store root"
    );
    "format-3"
}

/// Whether one Git command succeeds without asserting on its output.
fn git_succeeds(directory: &Path, arguments: &[&str]) -> bool {
    Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(arguments)
        .output()
        .expect("git runs")
        .status
        .success()
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
            "+refs/heads/orna-complete-copy:refs/heads/extracted",
        ],
    );
    let resolved = String::from_utf8(git(destination, &["rev-parse", "refs/heads/extracted"]))
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
/// Only blob objects are read: a commit or tree in the closure is verified by
/// Git's own transfer and named by the manifest, but it has no raw payload to
/// re-hash. Returns the number of verified blobs.
pub fn verify_blobs(repository: &Path, member: &RecordedMember) -> usize {
    let mut verified = 0;
    for object in member.objects.iter().filter(|o| o.kind == "blob") {
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
    let listing = String::from_utf8(git(repository, &["ls-tree", "-r", "-t", commit]))
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
    git(
        repository,
        &["cat-file", "blob", &format!("{commit}:{path}")],
    )
}

/// Where a helper materialises an extraction.
pub fn scratch(prefix: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("orna-extractor-{prefix}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    base
}
