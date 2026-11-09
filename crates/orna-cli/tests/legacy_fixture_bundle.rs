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
//! - `BUNDLE` is not a byte-for-byte 1.0.0 golden: it comes from the in-repo
//!   test encoder. `GOLDEN_BUNDLE` is the 1.0-era writer artifact described in
//!   `legacy-format1-1.0.0-golden.provenance.txt`; the key-bound claims below
//!   are made against it only.
//! - The `Blob` table proves the decoder reads an unannotated `BYTE_ARRAY`
//!   (1.0.0 `23-storage.md:184`) as payload bytes only. The ORNA-UPGRADE-004
//!   octet-stream default is applied by the migration layer
//!   (`MigrationAnnotationDefaults`), not by this reader, so no annotated value
//!   identity is asserted from a legacy read.
//! - `state.db` and persistent consumers are not covered (runtime-only; a
//!   persistent consumer with a Blob argument is not modelled).
//! - ORNA-UPGRADE-008 fail-closed behaviour is NOT asserted by this test.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use orna_foundation::{CanonicalValue, OvbRaw, compare_primary_keys};
use orna_repository::{Repository, RepositoryError, Uuid};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BUNDLE: &str = "tests/fixtures/legacy-format1.bundle";
/// SHA-256 of `BUNDLE`. Regenerate with the `build_legacy_fixture` example
/// (`<empty-root> <bundle-path> tests/fixtures`) and
/// `python3 scripts/check_fixture_manifest.py --update` when the builder changes.
const BUNDLE_SHA256: &str = "34396705a4854d6152fd20f39ce8e622b6f5f7a91f37f13ed6df3929c9da579e";
/// A format-1 compact store written by the 1.0-era production compact
/// publication boundary at `adb70ba2f68a8822fbdce0ad9911933b6e3e255e`.
///
/// Unlike `BUNDLE`, this artifact is not rebuilt by the in-repo fixture writer:
/// it is the output of `Repository::prepare_compact_publication` and
/// `publish_compact_repository_boundary` at that commit, with its Parquet row
/// payloads written by the same `test-encoder-v1` encoding the 1.0-era tests
/// used (that commit range has no production Parquet writer). See
/// `legacy-format1-1.0.0-golden.provenance.txt` in this fixture directory.
const GOLDEN_BUNDLE: &str = "tests/fixtures/legacy-format1-1.0.0-golden.bundle";
/// SHA-256 of `GOLDEN_BUNDLE`. The golden is captured, not regenerated: its
/// database identity is a random UUID minted once by the 1.0-era initializer.
const GOLDEN_BUNDLE_SHA256: &str =
    "cfbc158205c0f5702785621fe47a9d2cecfe286f0057f1258a738cfb2c664bc3";
/// The three tables in the golden, in the order the generator published them.
const GOLDEN_INT_TABLE: &str = "018f0000-0000-7300-0000-000000000001";
const GOLDEN_STR_TABLE: &str = "018f0000-0000-7300-0000-000000000002";
const GOLDEN_DATE_TABLE: &str = "018f0000-0000-7300-0000-000000000003";
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

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

/// The canonical OVB-1 bytes of one key, which is what the compact profile
/// stores in `min_key`/`max_key` (hex-authored) and in leaf entries.
fn canonical(raw: OvbRaw) -> Vec<u8> {
    CanonicalValue::new(raw)
        .expect("canonical key")
        .encode()
        .expect("canonical key encoding")
}

fn integer(value: i64) -> OvbRaw {
    OvbRaw::Int(value.into())
}

fn date(text: &str) -> OvbRaw {
    OvbRaw::Tag(60001, Box::new(OvbRaw::Text(text.to_owned())))
}

/// Clones `bundle` and rewrites exactly one committed file, then commits the
/// change so the reader sees a different snapshot. Used to prove the key-bound
/// checks reject a tampered store.
fn clone_with_replacement(
    bundle: &Path,
    parent: &TempDir,
    name: &str,
    path: &str,
    from: &str,
    to: &str,
) -> PathBuf {
    let destination = parent.path().join(name);
    clone_bundle(bundle, &destination);
    let original = String::from_utf8(git_bytes(&destination, &["show", &format!("HEAD:{path}")]))
        .expect("committed fixture text is UTF-8");
    assert_eq!(
        original.matches(from).count(),
        1,
        "tamper target must occur exactly once in {path}"
    );
    std::fs::write(destination.join(path), original.replacen(from, to, 1)).unwrap();
    git(&destination, &["add", path]);
    git(
        &destination,
        &[
            "-c",
            "user.name=orna-fixture",
            "-c",
            "user.email=orna-fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "tamper key bounds",
        ],
    );
    destination
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
            "2",
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

/// The committed 1.0.0-writer golden decodes under the old key rules, including
/// the key bounds.
///
/// The fixture is the output of the 1.0-era production compact publication
/// boundary (`adb70ba2f`), so the store's commit tree, manifest, shards and
/// segment digests are the writer's, not this repository's test encoder's. The
/// manifest and shard `min_key`/`max_key` fields store the hex of the canonical
/// OVB-1 key bytes; this test decodes those bytes as key values of the table's
/// key schema, and pins the writer's encoding against the committed text.
///
/// Negative `Int` keys make the two orders differ: canonical CBOR bytes sort
/// `-1` (`20`) before `-2` (`21`), while the logical key order is `-2` first.
/// The writer's shard minimum is `-2`, so a byte-order implementation could not
/// have produced this store. `Str` keys do the same with `beta` (`64…`) before
/// `alpha` (`65…`), where the writer's minimum is `alpha`.
///
/// Mutation: the same test rewrites one key bound in the shard and one in the
/// manifest of a throwaway clone and requires the reader to refuse the snapshot.
#[allow(clippy::too_many_lines)]
#[test]
fn committed_1_0_0_writer_golden_decodes_min_max_key_bounds() {
    let parent = TempDir::new().unwrap();
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_BUNDLE);
    let bytes = std::fs::read(&bundle).unwrap();
    assert_eq!(
        hex(Sha256::digest(&bytes).as_slice()),
        GOLDEN_BUNDLE_SHA256,
        "committed 1.0.0 golden digest changed"
    );

    let source = parent.path().join("golden");
    clone_bundle(&bundle, &source);
    assert_eq!(
        git_bytes(&source, &["show", "HEAD:.orna/format.orna"]),
        FORMAT_1_MARKER.as_bytes(),
        "1.0.0 golden does not carry the recorded format-1 marker"
    );
    let tracked = git(&source, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(
        !tracked.lines().any(|path| path.starts_with(".orna/store")),
        "format-1 golden must not carry a format-3 store"
    );

    let repository = Repository::discover(&source).unwrap();
    let head = repository.head().unwrap().expect("committed legacy HEAD");

    // The old rows decode under their original rules.
    let tables = repository.read_legacy_authoritative_tables(&head).unwrap();
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
    let int_keys =
        encoded_keys(&[-2, -1, 0, 1].map(|value| CanonicalValue::new(integer(value)).unwrap()));
    let str_keys = encoded_keys(
        &["alpha", "beta", "gamma"]
            .map(|value| CanonicalValue::new(OvbRaw::Text(value.to_owned())).unwrap()),
    );
    let date_keys = encoded_keys(
        &["1969-12-31", "1970-01-01", "1970-01-02"]
            .map(|text| CanonicalValue::new(date(text)).unwrap()),
    );
    assert!(key_sets.contains(&int_keys), "Int keys -2..=1 not found");
    assert!(
        key_sets.contains(&str_keys),
        "Str keys alpha/beta/gamma not found"
    );
    assert!(
        key_sets.contains(&date_keys),
        "Date keys -1, 0, 1 not found"
    );

    // The Int table has two segments. Their recorded bounds are the canonical
    // key bytes, which the reader decodes as Int key values.
    let int_manifest = repository
        .read_compact_manifest(&head, Uuid::parse_str(GOLDEN_INT_TABLE).unwrap())
        .unwrap()
        .expect("committed Int manifest");
    assert_eq!(int_manifest.entries().len(), 2);
    let mut int_bounds = int_manifest
        .entries()
        .iter()
        .map(|entry| (entry.min_key().to_vec(), entry.max_key().to_vec()))
        .collect::<Vec<_>>();
    int_bounds.sort();
    assert_eq!(
        int_bounds,
        vec![
            (canonical(integer(0)), canonical(integer(1))),
            (canonical(integer(-2)), canonical(integer(-1))),
        ]
    );
    for entry in int_manifest.entries() {
        let min = CanonicalValue::decode(entry.min_key()).unwrap();
        let max = CanonicalValue::decode(entry.max_key()).unwrap();
        assert!(
            matches!(min.raw(), OvbRaw::Int(_)),
            "Int key bound is not Int"
        );
        assert!(
            matches!(max.raw(), OvbRaw::Int(_)),
            "Int key bound is not Int"
        );
        assert_ne!(
            compare_primary_keys(&min, &max).unwrap(),
            Ordering::Greater,
            "a segment's min_key is greater than its max_key"
        );
    }

    // The writer recorded bounds as the hex of the canonical key bytes, not as
    // typed record literals. The shard bound is the logical extremum over its
    // entries: -2 wins even though its bytes sort after -1's.
    let neg_two = hex(&canonical(integer(-2)));
    let neg_one = hex(&canonical(integer(-1)));
    let zero = hex(&canonical(integer(0)));
    let one = hex(&canonical(integer(1)));
    assert!(
        neg_one < neg_two,
        "canonical CBOR byte order puts -1 before -2"
    );
    let int_manifest_text = String::from_utf8(git_bytes(
        &source,
        &[
            "show",
            &format!("HEAD:.orna/storage/{GOLDEN_INT_TABLE}/manifest.orna"),
        ],
    ))
    .unwrap();
    assert!(
        int_manifest_text.contains(&format!("min_key: \"{neg_two}\"")),
        "manifest shard minimum is not the logical minimum -2"
    );
    assert!(int_manifest_text.contains(&format!("max_key: \"{one}\"")));
    let int_shard_text = String::from_utf8(git_bytes(
        &source,
        &[
            "show",
            &format!("HEAD:.orna/storage/{GOLDEN_INT_TABLE}/shards/00000000.orna"),
        ],
    ))
    .unwrap();
    assert_eq!(
        int_shard_text
            .matches(&format!("min_key: \"{neg_two}\""))
            .count(),
        1
    );
    assert_eq!(
        int_shard_text
            .matches(&format!("max_key: \"{neg_one}\""))
            .count(),
        1
    );
    assert_eq!(
        int_shard_text
            .matches(&format!("min_key: \"{zero}\""))
            .count(),
        1
    );
    assert_eq!(
        int_shard_text
            .matches(&format!("max_key: \"{one}\""))
            .count(),
        1
    );

    // Str bounds are decoded Str key values; alpha is the logical minimum even
    // though beta's canonical bytes sort first.
    let str_manifest = repository
        .read_compact_manifest(&head, Uuid::parse_str(GOLDEN_STR_TABLE).unwrap())
        .unwrap()
        .expect("committed Str manifest");
    assert_eq!(str_manifest.entries().len(), 1);
    let str_entry = &str_manifest.entries()[0];
    let alpha = canonical(OvbRaw::Text("alpha".to_owned()));
    let beta = canonical(OvbRaw::Text("beta".to_owned()));
    let gamma = canonical(OvbRaw::Text("gamma".to_owned()));
    assert_eq!(str_entry.min_key(), alpha);
    assert_eq!(str_entry.max_key(), gamma);
    assert!(
        beta < alpha,
        "canonical CBOR byte order puts beta before alpha"
    );
    let str_min = CanonicalValue::decode(str_entry.min_key()).unwrap();
    assert!(
        matches!(str_min.raw(), OvbRaw::Text(value) if value == "alpha"),
        "Str key bound did not decode as the Str key value"
    );
    let str_manifest_text = String::from_utf8(git_bytes(
        &source,
        &[
            "show",
            &format!("HEAD:.orna/storage/{GOLDEN_STR_TABLE}/manifest.orna"),
        ],
    ))
    .unwrap();
    assert!(str_manifest_text.contains(&format!("min_key: \"{}\"", hex(&alpha))));
    assert!(str_manifest_text.contains(&format!("max_key: \"{}\"", hex(&gamma))));

    // Date bounds decode as Date key values (tag 60001 date text) for days -1,
    // 0 and 1.
    let date_manifest = repository
        .read_compact_manifest(&head, Uuid::parse_str(GOLDEN_DATE_TABLE).unwrap())
        .unwrap()
        .expect("committed Date manifest");
    assert_eq!(date_manifest.entries().len(), 1);
    let date_entry = &date_manifest.entries()[0];
    let earliest = canonical(date("1969-12-31"));
    let latest = canonical(date("1970-01-02"));
    assert_eq!(date_entry.min_key(), earliest);
    assert_eq!(date_entry.max_key(), latest);
    let date_min = CanonicalValue::decode(date_entry.min_key()).unwrap();
    assert!(
        matches!(date_min.raw(), OvbRaw::Tag(60001, _)),
        "Date key bound did not decode as a Date key value"
    );
    let date_manifest_text = String::from_utf8(git_bytes(
        &source,
        &[
            "show",
            &format!("HEAD:.orna/storage/{GOLDEN_DATE_TABLE}/manifest.orna"),
        ],
    ))
    .unwrap();
    assert!(date_manifest_text.contains(&format!("min_key: \"{}\"", hex(&earliest))));
    assert!(date_manifest_text.contains(&format!("max_key: \"{}\"", hex(&latest))));

    // Bounds are not decorative: a bound that disagrees with its entries (in
    // the shard) or with the shard extrema (in the manifest) is corruption.
    let shard_path = format!(".orna/storage/{GOLDEN_INT_TABLE}/shards/00000000.orna");
    let tampered = clone_with_replacement(
        &bundle,
        &parent,
        "tampered-shard",
        &shard_path,
        &format!("min_key: \"{neg_two}\""),
        &format!("min_key: \"{neg_one}\""),
    );
    let tampered_repository = Repository::discover(&tampered).unwrap();
    let tampered_head = tampered_repository.head().unwrap().unwrap();
    assert!(
        matches!(
            tampered_repository.read_legacy_authoritative_tables(&tampered_head),
            Err(RepositoryError::InvalidCompactManifest)
        ),
        "a shard entry min_key that is not its logical minimum must be refused"
    );

    let manifest_path = format!(".orna/storage/{GOLDEN_INT_TABLE}/manifest.orna");
    let tampered = clone_with_replacement(
        &bundle,
        &parent,
        "tampered-manifest",
        &manifest_path,
        &format!("min_key: \"{neg_two}\""),
        &format!("min_key: \"{neg_one}\""),
    );
    let tampered_repository = Repository::discover(&tampered).unwrap();
    let tampered_head = tampered_repository.head().unwrap().unwrap();
    assert!(
        matches!(
            tampered_repository.read_legacy_authoritative_tables(&tampered_head),
            Err(RepositoryError::InvalidCompactManifest)
        ),
        "a manifest shard min_key that is not its entries' logical minimum must be refused"
    );
}
