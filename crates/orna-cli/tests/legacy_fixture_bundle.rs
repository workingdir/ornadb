//! Legacy format-1 compact repository, committed as a git bundle.
//!
//! The bundle is produced by `examples/build_legacy_fixture.rs` in
//! `orna-repository` (feature `legacy-fixture-writer`). The builder writes the
//! 1.0.0 layout with plain files and `git commit`, not through the
//! production publication path, but it shares the canonical OVB encoder, base64
//! helper and Parquet page-checksum helper with production (see the builder's
//! module docs). The bundle bytes are pinned below by SHA-256 so the committed
//! artifact cannot drift silently, and
//! `builder_regenerates_the_committed_bundle_byte_for_byte` rebuilds the
//! repository from the builder and compares it with the committed bundle object
//! by object, so a builder change needs a regenerated bundle. It does not
//! compare the `git bundle` file bytes: those depend on the git release and on
//! the host's defaults, so file-byte equality would test the runner.
//!
//! This test holds the bundle closed, checks the pinned digest, checks the
//! committed repository is format 1 (marker bytes, record kinds, no
//! `.orna/store`), reads the committed legacy authoritative tables, reads the
//! committed loose rows the legacy read path needs, verifies the source
//! repository is unchanged by `orna migrate copy` (UPGRADE-006,
//! `reference/Orna-1.1.0/source/40-upgrade-contract.md` line 48), and admits
//! the snapshot through that copy path.
//!
//! Non-claims (partial slice of bead 5il1.394; see docs/migration-1.0.0.md):
//! - Not a byte-for-byte 1.0.0 golden: the bundle comes from the test encoder.
//! - The `Blob` table proves the decoder reads an unannotated `BYTE_ARRAY`
//!   (1.0.0 `23-storage.md:184`) as payload bytes only. The ORNA-UPGRADE-004
//!   octet-stream default is applied by the migration layer
//!   (`MigrationAnnotationDefaults`), not by this reader, so no annotated value
//!   identity is asserted from a legacy read.
//! - `state.db` and persistent consumers are not covered (runtime-only; a
//!   persistent consumer with a Blob argument is not modelled).
//! - Key-bound encoding (hex of canonical key bytes in `min_key`/`max_key`) is
//!   unverified against the 1.0.0 key-schema text.
//! - ORNA-UPGRADE-008 fail-closed behaviour is NOT asserted by this test.

use std::fmt::Write as _;
use std::{path::Path, process::Command};

use orna_foundation::{CanonicalValue, OvbRaw};
use orna_repository::Repository;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BUNDLE: &str = "tests/fixtures/legacy-format1.bundle";
/// SHA-256 of `BUNDLE`. Regenerate with the `build_legacy_fixture` example
/// (`<empty-root> <bundle-path> tests/fixtures`) and
/// `python3 scripts/check_fixture_manifest.py --update` when the builder changes.
const BUNDLE_SHA256: &str = "921a32f9689450d91208e01e262b3270ecda5e9cff76d0e56d5ebe6e03469118";
/// The 1.0.0 format marker, including its trailing LF. 1.0.0 `19-repository.md`
/// keeps it a legacy-format input, and it is the record the relabel guard reads,
/// so its exact bytes are part of the fixture contract.
const FORMAT_1_MARKER: &str = "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n";

fn git(directory: &Path, arguments: &[&str]) -> String {
    String::from_utf8(git_bytes(directory, arguments))
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

/// Runs git and returns its raw stdout, so object content can be compared byte
/// for byte without a trimming round trip masking a difference.
fn git_bytes(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn clone_bundle(bundle: &Path, destination: &Path) {
    let clone = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("clone")
        .arg("--quiet")
        .arg(bundle)
        .arg(destination)
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "git clone bundle: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
}

fn encoded_keys(keys: &[CanonicalValue]) -> Vec<Vec<u8>> {
    let mut encoded = keys
        .iter()
        .map(|key| key.encode().unwrap())
        .collect::<Vec<_>>();
    encoded.sort();
    encoded
}

#[allow(clippy::too_many_lines)]
#[test]
fn committed_legacy_format1_bundle_reads_and_admits_through_migrate_copy() {
    let parent = TempDir::new().unwrap();
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLE);
    let bytes = std::fs::read(&bundle).unwrap();
    let digest = Sha256::digest(&bytes);
    let mut digest_hex = String::with_capacity(64);
    for byte in &digest {
        write!(digest_hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    assert_eq!(
        digest_hex, BUNDLE_SHA256,
        "committed legacy bundle digest changed"
    );

    let source = parent.path().join("source");
    clone_bundle(&bundle, &source);

    // The committed repository really is format 1. The marker is the record the
    // format context and the relabel guard read; the record kinds around it are
    // the 1.0.0 layout (1.0.0 `23-storage.md:88-106`). Compared as raw bytes, so
    // the record's trailing LF is part of the contract.
    assert_eq!(
        git_bytes(&source, &["show", "HEAD:.orna/format.orna"]),
        FORMAT_1_MARKER.as_bytes(),
        "committed repository is not the 1.0.0 format marker"
    );
    let tracked = git(&source, &["ls-tree", "-r", "--name-only", "HEAD"]);
    let tracked_paths = tracked.lines().collect::<Vec<_>>();
    assert!(
        !tracked_paths
            .iter()
            .any(|path| path.starts_with(".orna/store")),
        "format-1 fixture must not carry a format-3 store"
    );
    for path in [
        ".orna/database.orna",
        "main.orna",
        "ordinary.txt",
        "vendor/stdlib",
        "contacts/main.orna",
        "contacts/Contact/alice-smith.orna",
        "contacts/Contact/bob-jones.orna",
    ] {
        assert!(
            tracked_paths.contains(&path),
            "committed repository is missing {path}"
        );
    }
    // Loose rows in the 1.0.0 layout are row units of non-key fields (ORNA-ROW-001)
    // addressed by the key component reconstructed from the path (ORNA-ROW-002,
    // key-path-v1 `07-tables.md:295-397`). The legacy read path for query,
    // history, mount and export needs both representations in one commit.
    assert_eq!(
        git_bytes(&source, &["show", "HEAD:contacts/Contact/alice-smith.orna"]),
        include_str!("fixtures/legacy-loose-contact-alice-smith.orna").as_bytes(),
        "loose row unit bytes changed"
    );

    let repository = Repository::discover(&source).unwrap();
    let head = repository.head().unwrap().expect("committed legacy HEAD");
    let tables = repository.read_legacy_authoritative_tables(&head).unwrap();

    // Table 1 is Int (two segments, four rows); table 2 is Str (three rows);
    // table 3 is Date (three rows, including the negative day -1); table 5 is
    // the Int-keyed `Blob` table (two rows).
    let mut row_counts = tables
        .iter()
        .map(|table| table.rows().len())
        .collect::<Vec<_>>();
    row_counts.sort_unstable();
    assert_eq!(row_counts, vec![2, 3, 3, 4]);

    let key_sets = tables
        .iter()
        .map(|table| {
            let keys = table
                .rows()
                .iter()
                .map(|row| row.key().clone())
                .collect::<Vec<_>>();
            encoded_keys(&keys)
        })
        .collect::<Vec<_>>();
    let int_keys = encoded_keys(
        &(1..=4)
            .map(|value: i64| CanonicalValue::new(OvbRaw::Int(value.into())).unwrap())
            .collect::<Vec<_>>(),
    );
    let blob_table_keys = encoded_keys(
        &[1, 2].map(|value: i64| CanonicalValue::new(OvbRaw::Int(value.into())).unwrap()),
    );
    let str_keys = encoded_keys(
        &["alpha", "beta", "gamma"]
            .map(|value| CanonicalValue::new(OvbRaw::Text(value.to_owned())).unwrap()),
    );
    assert!(key_sets.contains(&int_keys), "Int keys 1..=4 not found");
    assert!(
        key_sets.contains(&blob_table_keys),
        "Blob table Int keys 1..=2 not found"
    );
    assert!(
        key_sets.contains(&str_keys),
        "Str keys alpha/beta/gamma not found"
    );
    for keys in &key_sets {
        let mut distinct = keys.clone();
        distinct.dedup();
        assert_eq!(&distinct, keys, "each table key set is distinct");
    }

    // Exact key shapes. The bundle bytes come from the in-repo test encoder
    // (`examples/build_legacy_fixture.rs` in orna-repository), not a 1.0.0
    // golden, so these checks pin what the reader decodes from that encoder's
    // output. They are not spec evidence.
    let date_key = |text: &str| {
        let tagged = OvbRaw::Tag(60001, Box::new(OvbRaw::Text(text.to_owned())));
        CanonicalValue::new(tagged).unwrap()
    };
    let date_keys = encoded_keys(&["1969-12-31", "1970-01-01", "1970-01-02"].map(date_key));
    assert!(
        key_sets.contains(&date_keys),
        "Date keys -1, 0, 1 not found"
    );
    for table in &tables {
        for row in table.rows() {
            let is_known_key = matches!(
                row.key().raw(),
                OvbRaw::Int(_) | OvbRaw::Text(_) | OvbRaw::Tag(60001, _)
            );
            assert!(is_known_key, "key is not an Int, Str or Date key");
        }
    }

    // Each decoded row value is the 60009 record the reader builds,
    // `[null, [[field, value], ...]]`. Every field is named by a 16-byte UUID
    // tagged 37; the key field repeats the row key. Three tables store only that
    // key column. The `Blob` table also stores one value column: 1.0.0
    // `23-storage.md:184` maps `Blob` to an unannotated BYTE_ARRAY, so the field
    // holds the payload bytes alone. No media type or suffix appears, because
    // ORNA-BLOB-009 keeps the legacy bytes-only value and the ORNA-UPGRADE-004
    // octet-stream default belongs to the migration layer.
    let expected_blobs: [&[u8]; 2] = [
        b"\x00\xff\x80orna",
        // The second payload is empty: a zero-length Blob is still a present value.
        b"",
    ];
    let mut blob_tables = 0usize;
    for table in &tables {
        let mut field_names = std::collections::BTreeSet::new();
        let mut blobs = Vec::new();
        for row in table.rows() {
            let OvbRaw::Tag(60009, record) = row.value().raw() else {
                panic!("row value is not a 60009 record");
            };
            let OvbRaw::Array(parts) = &**record else {
                panic!("60009 record is not an array");
            };
            let [OvbRaw::Null, OvbRaw::Array(fields)] = parts.as_slice() else {
                panic!("60009 record is not [null, fields]");
            };
            let mut key_field = None;
            for field in fields {
                let OvbRaw::Array(field) = field else {
                    panic!("row field is not an array");
                };
                let [OvbRaw::Tag(37, field_name), stored] = field.as_slice() else {
                    panic!("row field is not a tagged name and a value");
                };
                let OvbRaw::Bytes(name_bytes) = &**field_name else {
                    panic!("row field name is not bytes");
                };
                assert_eq!(name_bytes.len(), 16, "field name is a 16-byte UUID");
                if stored == row.key().raw() {
                    assert!(key_field.is_none(), "one key field per row");
                    key_field = Some(name_bytes.clone());
                } else {
                    let OvbRaw::Bytes(payload) = stored else {
                        panic!("stored value is not a Blob payload");
                    };
                    blobs.push(payload.clone());
                }
            }
            let key_field = key_field.expect("each row carries its key field");
            field_names.insert(key_field);
        }
        assert_eq!(field_names.len(), 1, "each table names one key field");
        if !blobs.is_empty() {
            blob_tables += 1;
            assert_eq!(
                blobs,
                expected_blobs
                    .iter()
                    .map(|payload| payload.to_vec())
                    .collect::<Vec<_>>(),
                "legacy Blob payloads must survive decoding byte for byte"
            );
        }
    }
    assert_eq!(blob_tables, 1, "exactly one table stores a Blob column");

    let source_head_before = git(&source, &["rev-parse", "HEAD"]);
    let source_status_before = git(&source, &["status", "--porcelain"]);

    let destination = parent.path().join("migrated");
    let copy = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            source.to_str().unwrap(),
            "migrate",
            "copy",
            destination.to_str().unwrap(),
            "--annotation-defaults",
            "0",
            "--no-persistent-consumers",
        ])
        .output()
        .unwrap();
    assert!(
        copy.status.success(),
        "migrate copy: {}",
        String::from_utf8_lossy(&copy.stderr)
    );

    assert_eq!(
        git(&source, &["rev-parse", "HEAD"]),
        source_head_before,
        "migrate copy moved the source HEAD"
    );
    assert_eq!(
        git(&source, &["status", "--porcelain"]),
        source_status_before,
        "migrate copy changed the source worktree"
    );

    let migrated = Repository::discover(&destination).unwrap();
    assert!(migrated.holds_prepared_migration());
}

/// The committed bundle must be exactly what the builder produces now.
///
/// The pinned SHA-256 above only proves the committed file did not change; it
/// cannot notice a builder that stopped producing that file. This test closes
/// that gap: it writes a fresh repository with
/// `orna_repository::write_legacy_format1_golden_fixture` (pointed at this
/// crate's own fixture tree, which owns the editable-row sources) and compares
/// it with a clone of the committed bundle. A builder change therefore needs a
/// regenerated bundle in the same change.
///
/// Compared, all environment-independent:
/// - `HEAD` and every `refs/heads`/`refs/tags` name and target;
/// - the ordered `rev-list --objects --all` listing, which fixes the whole
///   commit/tree/blob graph by object id and path;
/// - every object's content byte for byte, through `git cat-file -p`, so a
///   rewritten commit message, author/committer date, tree mode, entry name or
///   blob byte differs here and fails.
///
/// NOT compared: the `git bundle` file bytes. `git bundle create` output
/// depends on the git release (pack version, delta heuristics, zlib), on the
/// host's default object format and default branch, so file-byte equality
/// tests the runner rather than the builder: it fails on GitHub's git while the
/// repository it produced is identical. What that gives up is only the packfile
/// encoding and the bundle's advertised-ref header; the committed artifact is
/// still frozen by `BUNDLE_SHA256`, and a bundle re-created by another git is
/// accepted here exactly when it carries the same graph and the same object
/// bytes. No builder drift is hidden by this: every object and ref the builder
/// writes is compared.
///
/// Bundle bytes are still reproducible here for a given git release because the
/// builder pins author/committer dates, the object format and the line-ending
/// conversion; that is what keeps the checked-in artifact stable, not what this
/// test asserts.
#[test]
fn builder_regenerates_the_committed_bundle_byte_for_byte() {
    let parent = TempDir::new().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLE);

    let root = parent.path().join("repository");
    std::fs::create_dir(&root).unwrap();
    orna_repository::write_legacy_format1_golden_fixture(&root, &fixtures);

    let committed = parent.path().join("committed");
    clone_bundle(&bundle, &committed);

    assert_eq!(
        git(&root, &["rev-parse", "HEAD"]),
        git(&committed, &["rev-parse", "HEAD"]),
        "rebuilt repository HEAD differs from the committed bundle HEAD"
    );
    assert_eq!(
        git(
            &root,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads",
                "refs/tags",
            ],
        ),
        git(
            &committed,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads",
                "refs/tags",
            ],
        ),
        "rebuilt repository refs differ from the committed bundle"
    );

    // The ordered listing fixes every commit, tree and blob by id and path.
    let rebuilt_graph = git_bytes(&root, &["rev-list", "--objects", "--all"]);
    let committed_graph = git_bytes(&committed, &["rev-list", "--objects", "--all"]);
    assert_eq!(
        String::from_utf8(rebuilt_graph.clone()).unwrap(),
        String::from_utf8(committed_graph.clone()).unwrap(),
        "rebuilt repository object graph differs from the committed bundle: \
         regenerate it with `cargo run -p orna-repository --example \
         build_legacy_fixture --features legacy-fixture-writer -- <empty-root> {} \
         tests/fixtures` and repin BUNDLE_SHA256 plus scripts/fixture-manifest.sha256",
        bundle.display()
    );

    // Object ids already commit to their content; comparing the content too
    // names the first object whose bytes differ and covers a hash algorithm the
    // ids alone would hide behind a differing format.
    for line in String::from_utf8(rebuilt_graph).unwrap().lines() {
        let oid = line.split(' ').next().unwrap();
        assert_eq!(
            git_bytes(&root, &["cat-file", "-p", oid]),
            git_bytes(&committed, &["cat-file", "-p", oid]),
            "object {line} differs between the rebuild and the committed bundle"
        );
    }
}
