#![cfg(unix)]

use std::{
    cell::Cell,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use orna_security_v1::{SecretBoundaryError, SecretRef, SecretResolver, SopsProvider};
use tempfile::TempDir;

fn fake_sops(directory: &Path, body: &str) -> PathBuf {
    let executable = directory.join("sops-fixture");
    fs::write(&executable, body).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    executable
}

const REVERSING_SOPS: &str = r##"#!/bin/sh
set -eu
action="$1"
shift
input=
for argument in "$@"; do input="$argument"; done
case "$action" in
  encrypt) printf 'FAKE:'; rev "$input" ;;
  decrypt) tail -c +6 "$input" | rev ;;
  *) exit 2 ;;
esac
"##;

#[test]
fn write_encrypts_before_persisting_and_removes_plaintext_temp() {
    let root = TempDir::new().unwrap();
    let executable = fake_sops(root.path(), REVERSING_SOPS);
    let secrets = root.path().join("secrets");
    let plaintext_temps = root.path().join("private-tmp");
    fs::create_dir(&plaintext_temps).unwrap();
    let provider = SopsProvider::new(secrets.clone())
        .with_executable(executable)
        .with_temp_dir(plaintext_temps.clone());
    let reference = SecretRef::new("messages.inbox").unwrap();
    let payload = b"plaintext-must-not-survive";

    provider.store_secret(&reference, payload).unwrap();

    assert_eq!(fs::read_dir(&plaintext_temps).unwrap().count(), 0);
    let encrypted_file = fs::read_dir(&secrets)
        .unwrap()
        .next()
        .expect("stored secret document")
        .unwrap()
        .path();
    let encrypted = fs::read(&encrypted_file).unwrap();
    assert!(encrypted.starts_with(b"FAKE:"));
    assert!(!encrypted
        .windows(payload.len())
        .any(|window| window == payload));

    provider.store_secret(&reference, b"replacement-secret").unwrap();
    assert_eq!(fs::read_dir(&secrets).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&plaintext_temps).unwrap().count(), 0);
    let replacement_ciphertext = fs::read(&encrypted_file).unwrap();
    assert!(!replacement_ciphertext
        .windows(b"replacement-secret".len())
        .any(|window| window == b"replacement-secret"));

    let mut returned = Vec::new();
    let mut operation = |secret: &[u8]| returned.extend_from_slice(secret);
    provider
        .with_secret(&reference, &mut operation)
        .expect("fixture decryption");
    assert_eq!(returned, b"replacement-secret");

    let metadata = provider.metadata(&reference).unwrap();
    assert!(metadata.available());
    assert_eq!(metadata.reference(), &reference);
    assert_eq!(metadata.provider(), "sops");
    let serialized_reference = serde_json::to_string(&reference).unwrap();
    assert_eq!(serialized_reference, r#""messages.inbox""#);
    let serialized_metadata = serde_json::to_string(&metadata).unwrap();
    assert!(serialized_metadata.contains("messages.inbox"));
    assert!(serialized_metadata.contains("sops"));
    assert!(!serialized_metadata.contains("plaintext-must-not-survive"));
    assert!(!serialized_metadata.contains("replacement-secret"));
}

#[test]
fn absent_identity_and_missing_ciphertext_are_unavailable_without_callback() {
    let root = TempDir::new().unwrap();
    let executable = fake_sops(
        root.path(),
        "#!/bin/sh\nprintf '%s\\n' 'fixture identity unavailable: private-value' >&2\nexit 1\n",
    );
    let secrets = root.path().join("secrets");
    fs::create_dir(&secrets).unwrap();
    let plaintext_temps = root.path().join("private-tmp");
    fs::create_dir(&plaintext_temps).unwrap();
    let provider = SopsProvider::new(&secrets)
        .with_executable(executable)
        .with_temp_dir(plaintext_temps.clone());
    let reference = SecretRef::new("private.fixture").unwrap();
    let callback_calls = Cell::new(0);
    let mut callback = |_: &[u8]| callback_calls.set(callback_calls.get() + 1);

    assert!(!provider.metadata(&reference).unwrap().available());
    assert_eq!(
        provider.with_secret(&reference, &mut callback),
        Err(SecretBoundaryError::Unavailable)
    );
    assert_eq!(callback_calls.get(), 0);
    assert_eq!(
        provider.store_secret(&reference, b"never-committed-in-plaintext"),
        Err(SecretBoundaryError::Unavailable)
    );
    assert_eq!(fs::read_dir(&plaintext_temps).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&secrets).unwrap().count(), 0);
    assert_eq!(
        SecretBoundaryError::Unavailable.to_string(),
        "secret unavailable"
    );

    let no_file_provider = SopsProvider::new(root.path().join("no-files"));
    assert!(!no_file_provider.metadata(&reference).unwrap().available());
    assert_eq!(
        no_file_provider.with_secret(&reference, &mut callback),
        Err(SecretBoundaryError::Unavailable)
    );
    assert_eq!(callback_calls.get(), 0);
}

#[test]
fn invalid_reference_has_the_stable_boundary_error() {
    assert_eq!(
        SecretRef::new("bad\nreference").unwrap_err(),
        SecretBoundaryError::InvalidReference
    );
    assert_eq!(
        SecretBoundaryError::InvalidReference.to_string(),
        "invalid secret reference"
    );
}
