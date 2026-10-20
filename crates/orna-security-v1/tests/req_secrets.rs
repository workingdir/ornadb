#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use orna_security_v1::{SecretRef, SecretResolver, SopsProvider};
use tempfile::TempDir;

/// Fake `sops` whose decrypt action snapshots `plaintext_temps` into `marker` while running.
/// It fails if the temp directory is non-empty or if the ciphertext input lies inside it.
fn snapshotting_sops(directory: &Path, plaintext_temps: &Path, marker: &Path) -> PathBuf {
    let body = format!(
        r##"#!/bin/sh
set -eu
action="$1"
shift
input=
for argument in "$@"; do input="$argument"; done
case "$action" in
  encrypt) printf 'FAKE:'; rev "$input" ;;
  decrypt)
    : >> '{marker}'
    ls -A '{temps}' >> '{marker}'
    case "$input" in '{temps}'/*) echo 'plaintext input inside temp dir' >&2; exit 3 ;; esac
    if [ -s '{marker}' ]; then echo 'plaintext temp present during decrypt' >&2; exit 3; fi
    tail -c +6 "$input" | rev ;;
  *) exit 2 ;;
esac
"##,
        marker = marker.display(),
        temps = plaintext_temps.display(),
    );
    let executable = directory.join("sops-fixture");
    fs::write(&executable, body).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    executable
}

/// ORNA-SOPS-001: An SOPS provider MUST decrypt in memory and SHOULD avoid writing plaintext temporary files.
#[test]
fn sops_reads_decrypt_in_memory_without_plaintext_temp_files_or_ciphertext_rewrites() {
    let root = TempDir::new().unwrap();
    let plaintext_temps = root.path().join("private-tmp");
    fs::create_dir(&plaintext_temps).unwrap();
    let marker = root.path().join("decrypt-snapshots");
    let executable = snapshotting_sops(root.path(), &plaintext_temps, &marker);
    let secrets = root.path().join("secrets");
    let provider = SopsProvider::new(secrets.clone())
        .with_executable(executable)
        .with_temp_dir(plaintext_temps.clone());
    let reference = SecretRef::new("messages.inbox").unwrap();
    provider.store_secret(&reference, b"in-memory-only").unwrap();
    let ciphertext_before = directory_contents(&secrets);
    assert_eq!(ciphertext_before.len(), 1);

    assert!(provider.metadata(&reference).unwrap().available());
    let mut returned = Vec::new();
    let mut operation = |secret: &[u8]| returned.extend_from_slice(secret);
    provider
        .with_secret(&reference, &mut operation)
        .expect("fixture decryption");
    assert_eq!(returned, b"in-memory-only");

    let snapshots = fs::read_to_string(&marker).unwrap();
    assert_eq!(snapshots, "", "plaintext temp listing during decrypt");
    assert_eq!(fs::read_dir(&plaintext_temps).unwrap().count(), 0);
    assert_eq!(directory_contents(&secrets), ciphertext_before);
}

fn directory_contents(directory: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    entries.sort();
    entries
}
