use std::{fs, path::Path, process::Command};

use num_bigint::BigInt;
use orna_foundation_v1::{
    CanonicalSnapshot, CwdCapture, GitHash, RowRef, SystemReferenceError, snapshot_reference,
    validate_snapshot_reference,
};
use orna_repository_v1::Repository;
use orna_syntax_v1::parse_module;
use tempfile::TempDir;

const SNAPSHOT_SOURCE: &str = include_str!("fixtures/snapshot-round-trip.orna");

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> (TempDir, Repository) {
    let directory = TempDir::new().expect("create snapshot fixture repository");
    git(
        directory.path(),
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git(directory.path(), &["config", "user.name", "kierandrewett"]);
    git(
        directory.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    git(directory.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(directory.path().join("main.orna"), SNAPSHOT_SOURCE)
        .expect("write in-crate snapshot fixture");
    git(directory.path(), &["add", "main.orna"]);
    git(directory.path(), &["commit", "--quiet", "-m", "snapshot fixture"]);
    let repository = Repository::discover(directory.path()).expect("discover fixture repository");
    (directory, repository)
}

fn hex_bytes(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(digits, 16).unwrap()
        })
        .collect()
}

#[test]
fn snapshot_and_cwd_reference_round_trip_keep_the_captured_generation() {
    let parsed_fixture = parse_module(SNAPSHOT_SOURCE);
    assert!(
        parsed_fixture.diagnostics.is_empty(),
        "the in-crate Orna fixture must remain syntactically valid: {:?}",
        parsed_fixture.diagnostics
    );
    let (_directory, repository) = repository();
    let head = repository.head().unwrap().expect("fixture commit");
    let database = [1; 16];

    let committed = CanonicalSnapshot::Commit {
        database,
        algorithm: GitHash::Sha1,
        oid: hex_bytes(head.as_str()),
    };
    assert_eq!(
        CanonicalSnapshot::decode_bytes(&committed.encode().unwrap()).unwrap(),
        committed,
        "a source-backed Git snapshot must survive its storage codec"
    );

    let runtime = [2; 16];
    // Snapshot identity includes the complete arbitrary-precision generation,
    // rather than truncating it to a machine word.
    let generation = (BigInt::from(1_u8) << 130_usize) + BigInt::from(42_u8);
    let captured = CanonicalSnapshot::cwd(database, runtime, generation.clone()).unwrap();
    let encoded_snapshot = captured.encode().expect("encode CWD snapshot");
    let decoded_snapshot =
        CanonicalSnapshot::decode_bytes(&encoded_snapshot).expect("decode CWD snapshot");
    assert_eq!(decoded_snapshot, captured);
    assert_eq!(decoded_snapshot.encode().unwrap(), encoded_snapshot);

    let next_generation = CanonicalSnapshot::cwd(database, runtime, &generation + 1).unwrap();
    assert_ne!(
        decoded_snapshot, next_generation,
        "successive generations must have distinct snapshot identities"
    );

    let capture = CwdCapture::new(captured.clone(), [3; 32]).unwrap();
    let reference = snapshot_reference(database, captured.clone())
        .unwrap()
        .into_row_ref();
    let encoded_reference = reference.encode().expect("encode CWD reference");
    let decoded_reference = RowRef::decode(&encoded_reference).expect("decode CWD reference");
    assert_eq!(decoded_reference, reference);
    assert_eq!(decoded_reference.snapshot, captured);
    assert_eq!(
        validate_snapshot_reference(decoded_reference.clone(), &capture).unwrap(),
        snapshot_reference(database, decoded_snapshot).unwrap()
    );

    let later_capture = CwdCapture::new(next_generation, [4; 32]).unwrap();
    assert_eq!(
        validate_snapshot_reference(decoded_reference, &later_capture),
        Err(SystemReferenceError::SnapshotMismatch),
        "decoding an old CWD reference must not rebind it to the later capture"
    );
}

#[test]
fn snapshot_encoders_reject_forged_generation_identities() {
    let mut forged = CanonicalSnapshot::cwd([1; 16], [2; 16], BigInt::from(42_u8)).unwrap();
    let CanonicalSnapshot::Cwd { id, .. } = &mut forged else {
        unreachable!();
    };
    *id = [0xa5; 32];

    assert!(forged.encode().is_err());
    assert!(RowRef::new([1; 16], [4; 16], orna_foundation_v1::OvbRaw::Null, forged).is_err());
}
