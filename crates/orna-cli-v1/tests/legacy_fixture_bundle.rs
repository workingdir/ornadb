//! Legacy format-1 compact repository, committed as a git bundle.
//!
//! The bundle is produced by `examples/build_legacy_fixture.rs` in
//! `orna-repository-v1` (feature `legacy-fixture-writer`). The builder writes the
//! 1.0.0 layout with plain files and `git commit`, not through the
//! production publication path, but it shares the canonical OVB encoder, base64
//! helper and Parquet page-checksum helper with production (see the builder's
//! module docs). The bundle bytes are pinned below by SHA-256 so the committed
//! artifact cannot drift silently, and
//! `builder_regenerates_the_committed_bundle_byte_for_byte` rebuilds the
//! repository from the builder and requires the rebuilt bundle to equal the
//! committed one byte for byte, so a builder change needs a regenerated bundle.
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
//! - No Blob-bearing row: the format-1 compact decoder admits no `Blob`/`blob`
//!   column (`PhysicalValueKind` in `compact.rs`) although 1.0.0
//!   `23-storage.md:184` defines one, so ORNA-UPGRADE-004's annotation default
//!   still has no fixture value to apply.
//! - `state.db` and persistent consumers are not covered (runtime-only; a
//!   persistent consumer with a Blob argument is not modelled).
//! - Key-bound encoding (hex of canonical key bytes in `min_key`/`max_key`) is
//!   unverified against the 1.0.0 key-schema text.
//! - ORNA-UPGRADE-008 fail-closed behaviour is NOT asserted by this test.

use std::fmt::Write as _;
use std::{path::Path, process::Command};

use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_repository_v1::Repository;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BUNDLE: &str = "tests/fixtures/legacy-format1.bundle";
/// SHA-256 of `BUNDLE`. Regenerate with the `build_legacy_fixture` example and
/// `python3 scripts/check_fixture_manifest.py --update` when the builder changes.
const BUNDLE_SHA256: &str = "19f36c5aa732ba69ad95cb8d24c0771946cd6326ebed8eb2a9123a6d4a9b1dc1";
/// The 1.0.0 format marker, including its trailing LF. 1.0.0 `19-repository.md`
/// keeps it a legacy-format input, and it is the record the relabel guard reads,
/// so its exact bytes are part of the fixture contract.
const FORMAT_1_MARKER: &str =
    "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n";

fn git(directory: &Path, arguments: &[&str]) -> String {
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
    String::from_utf8(output.stdout).unwrap()
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
    // the 1.0.0 layout (1.0.0 `23-storage.md:88-106`).
    assert_eq!(
        git(&source, &["show", "HEAD:.orna/format.orna"]),
        FORMAT_1_MARKER,
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
        git(&source, &["show", "HEAD:contacts/Contact/alice-smith.orna"]),
        include_str!(
            "../../orna-repository-v1/tests/fixtures/legacy-loose-contact-alice-smith.orna"
        ),
        "loose row unit bytes changed"
    );

    let repository = Repository::discover(&source).unwrap();
    let head = repository.head().unwrap().expect("committed legacy HEAD");
    let tables = repository.read_legacy_authoritative_tables(&head).unwrap();

    // Table 1 is Int (two segments, four rows); table 2 is Str (three rows);
    // table 3 is Date (three rows, including the negative day -1).
    let mut row_counts = tables
        .iter()
        .map(|table| table.rows().len())
        .collect::<Vec<_>>();
    row_counts.sort_unstable();
    assert_eq!(row_counts, vec![3, 3, 4]);

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
    let str_keys = encoded_keys(
        &["alpha", "beta", "gamma"]
            .map(|value| CanonicalValue::new(OvbRaw::Text(value.to_owned())).unwrap()),
    );
    assert!(key_sets.contains(&int_keys), "Int keys 1..=4 not found");
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
    // (`examples/build_legacy_fixture.rs` in orna-repository-v1), not a 1.0.0
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

    // Key-only live rows: each decoded value is the 60009 record the reader builds
    // for a key-only segment, `[null, [[field, key]]]`. The single field is named
    // by a 16-byte UUID tagged 37 and repeats the row key. The fixture stores no
    // other column, so no stored value is asserted here.
    for table in &tables {
        let mut field_names = std::collections::BTreeSet::new();
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
            let [OvbRaw::Array(field)] = fields.as_slice() else {
                panic!("key-only row must carry exactly one field");
            };
            let [OvbRaw::Tag(37, field_name), stored] = field.as_slice() else {
                panic!("key-only field is not a tagged name and a value");
            };
            let OvbRaw::Bytes(name_bytes) = &**field_name else {
                panic!("key-only field name is not bytes");
            };
            assert_eq!(name_bytes.len(), 16, "field name is a 16-byte UUID");
            assert!(
                stored == row.key().raw(),
                "key-only field repeats the row key"
            );
            field_names.insert(name_bytes.clone());
        }
        assert_eq!(field_names.len(), 1, "each table names one key field");
    }

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
/// `orna_repository_v1::write_legacy_format1_fixture`, bundles it the same way
/// `examples/build_legacy_fixture.rs` does, and requires the rebuilt bundle to
/// equal the committed one byte for byte. A builder change therefore needs a
/// regenerated bundle in the same change.
///
/// Bundle bytes are reproducible for a given git release because the builder
/// pins author and committer dates; a git release that changes pack encoding
/// fails this test rather than silently redefining the golden bytes.
#[test]
fn builder_regenerates_the_committed_bundle_byte_for_byte() {
    let parent = TempDir::new().unwrap();
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLE);

    let root = parent.path().join("repository");
    std::fs::create_dir(&root).unwrap();
    orna_repository_v1::write_legacy_format1_fixture(&root);

    // Compare the rebuilt history with the committed one before the bytes, so a
    // mismatch reports which of the two drifted and where.
    let committed = parent.path().join("committed");
    clone_bundle(&bundle, &committed);
    assert_eq!(
        git(&root, &["rev-parse", "HEAD"]),
        git(&committed, &["rev-parse", "HEAD"]),
        "rebuilt repository HEAD differs from the committed bundle HEAD"
    );
    assert_eq!(
        git(&root, &["rev-list", "--objects", "--all"]),
        git(&committed, &["rev-list", "--objects", "--all"]),
        "rebuilt repository object graph differs from the committed bundle"
    );

    let rebuilt_bundle = parent.path().join("rebuilt.bundle");
    let status = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(&root)
        .args(["bundle", "create"])
        .arg(&rebuilt_bundle)
        .arg("--all")
        .status()
        .expect("spawn git bundle create");
    assert!(status.success(), "git bundle create failed: {status}");

    let rebuilt = std::fs::read(&rebuilt_bundle).unwrap();
    let committed_bytes = std::fs::read(&bundle).unwrap();
    assert_eq!(
        rebuilt.len(),
        committed_bytes.len(),
        "the committed bundle is stale: regenerate it with \
         `cargo run -p orna-repository-v1 --example build_legacy_fixture --features \
         legacy-fixture-writer -- <empty-root> {}` and repin BUNDLE_SHA256 plus \
         scripts/fixture-manifest.sha256",
        bundle.display()
    );
    assert!(
        rebuilt == committed_bytes,
        "the committed bundle is stale: rebuild it from the builder and repin \
         BUNDLE_SHA256 plus scripts/fixture-manifest.sha256"
    );
}
