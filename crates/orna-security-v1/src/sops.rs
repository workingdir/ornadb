//! SOPS-backed encrypted files for Orna's secret boundary.
//!
//! SOPS receives plaintext on stdin and returns ciphertext on stdout. The
//! provider writes only that ciphertext to a private, same-directory staging
//! file before atomically replacing the destination. Decryption output stays
//! in memory and is cleared after the privileged callback returns.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use tempfile::NamedTempFile;
use zeroize::Zeroize;

use crate::{SecretBoundaryError, SecretMetadata, SecretRef, SecretResolver};

/// A redacted error from writing a SOPS-encrypted secret.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SopsFileError {
    InvalidReference,
    Unavailable,
    Io,
}

impl fmt::Display for SopsFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidReference => "invalid secret reference",
            Self::Unavailable => "SOPS provider unavailable",
            Self::Io => "secret storage operation failed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SopsFileError {}

/// A file-backed SOPS provider with one encrypted binary value per reference.
///
/// SOPS configuration supplies the recipient policy (`age`, PGP, or KMS) as
/// usual. References are stored as `<stable-name>.sops.json` in `root`. The
/// frozen contract does not prescribe a file layout, so this adapter uses a
/// single readable filename per name and rejects path separators to keep a
/// reference from escaping the configured directory.
#[derive(Clone, Debug)]
pub struct SopsFileProvider {
    root: PathBuf,
    executable: OsString,
}

impl SopsFileProvider {
    /// Creates a provider that invokes `sops` from `PATH`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            executable: OsString::from("sops"),
        }
    }

    /// Selects an explicit SOPS executable, mainly for managed installations.
    #[must_use]
    pub fn with_executable(mut self, executable: impl Into<OsString>) -> Self {
        self.executable = executable.into();
        self
    }

    /// Encrypts and atomically stores one secret value.
    ///
    /// On Unix, plaintext is piped directly to SOPS and never written to a
    /// temporary file. Platforms where SOPS requires a filename use a private
    /// temporary plaintext file that is overwritten and removed on drop. Only
    /// encrypted output is staged on disk for the atomic destination rename.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the reference is unsafe, SOPS cannot encrypt
    /// the input, or the encrypted document cannot be persisted.
    pub fn put(&self, reference: &SecretRef, plaintext: &[u8]) -> Result<(), SopsFileError> {
        Self::validate_reference(reference)?;
        let root = self.canonical_root_for_write()?;
        let destination = Self::path_for(&root, reference)?;
        let encrypted = self
            .encrypt(&destination, plaintext)
            .map_err(|()| SopsFileError::Unavailable)?;
        if encrypted.0.is_empty() {
            return Err(SopsFileError::Unavailable);
        }

        let mut staging = NamedTempFile::new_in(&root).map_err(|_| SopsFileError::Io)?;
        set_private_permissions(staging.as_file()).map_err(|_| SopsFileError::Io)?;
        staging
            .write_all(&encrypted.0)
            .and_then(|()| staging.as_file().sync_all())
            .map_err(|_| SopsFileError::Io)?;
        staging
            .persist(&destination)
            .map_err(|_| SopsFileError::Io)?;
        sync_directory(&root);
        Ok(())
    }

    fn canonical_root_for_write(&self) -> Result<PathBuf, SopsFileError> {
        fs::create_dir_all(&self.root).map_err(|_| SopsFileError::Io)?;
        fs::canonicalize(&self.root).map_err(|_| SopsFileError::Io)
    }

    fn canonical_root(&self) -> Result<PathBuf, SecretBoundaryError> {
        fs::canonicalize(&self.root).map_err(|_| SecretBoundaryError::Unavailable)
    }

    fn path_for(root: &Path, reference: &SecretRef) -> Result<PathBuf, SopsFileError> {
        Self::validate_reference(reference)?;
        Ok(root.join(format!("{}.sops.json", reference.as_str())))
    }

    fn validate_reference(reference: &SecretRef) -> Result<(), SopsFileError> {
        let name = reference.as_str();
        if name == "."
            || name == ".."
            || name.contains(['/', '\\'])
            || name.chars().any(char::is_control)
        {
            return Err(SopsFileError::InvalidReference);
        }
        Ok(())
    }

    fn secret_path(&self, reference: &SecretRef) -> Result<PathBuf, SecretBoundaryError> {
        Self::validate_reference(reference)
            .map_err(|_| SecretBoundaryError::InvalidReference)?;
        let root = self.canonical_root()?;
        Ok(root.join(format!("{}.sops.json", reference.as_str())))
    }

    fn encrypt(&self, destination: &Path, plaintext: &[u8]) -> Result<SecretBytes, ()> {
        // SOPS documents stdin through /dev/stdin on Unix. `filename-override`
        // keeps normal .sops.yaml discovery and recipient rules for the target.
        #[cfg(not(unix))]
        {
            let parent = destination.parent().ok_or(())?;
            let mut staged_plaintext = PlaintextTemp(NamedTempFile::new_in(parent).map_err(|_| ())?);
            set_private_permissions(staged_plaintext.0.as_file()).map_err(|_| ())?;
            staged_plaintext.0.write_all(plaintext).map_err(|_| ())?;
            staged_plaintext.0.as_file().sync_all().map_err(|_| ())?;
            let output = self
                .run(
                    [
                        OsString::from("--encrypt"),
                        OsString::from("--input-type"),
                        OsString::from("binary"),
                        OsString::from("--output-type"),
                        OsString::from("json"),
                        OsString::from("--filename-override"),
                        destination.as_os_str().to_owned(),
                        staged_plaintext.0.path().as_os_str().to_owned(),
                    ],
                    None,
                )
                .map_err(|_| ())?;
            successful_stdout(output)
        }

        #[cfg(unix)]
        {
            let output = self
                .run(
                    [
                        OsString::from("--encrypt"),
                        OsString::from("--input-type"),
                        OsString::from("binary"),
                        OsString::from("--output-type"),
                        OsString::from("json"),
                        OsString::from("--filename-override"),
                        destination.as_os_str().to_owned(),
                        OsString::from("/dev/stdin"),
                    ],
                    Some(plaintext),
                )
                .map_err(|_| ())?;
            successful_stdout(output)
        }
    }

    fn decrypt(&self, path: &Path) -> Result<SecretBytes, ()> {
        let output = self
            .run(
                [
                    OsString::from("--decrypt"),
                    OsString::from("--input-type"),
                    OsString::from("json"),
                    OsString::from("--output-type"),
                    OsString::from("binary"),
                    path.as_os_str().to_owned(),
                ],
                None,
            )
            .map_err(|_| ())?;
        successful_stdout(output)
    }

    fn can_decrypt(&self, path: &Path) -> bool {
        let Ok(mut child) = Command::new(&self.executable)
            .args([
                OsString::from("--decrypt"),
                OsString::from("--input-type"),
                OsString::from("json"),
                OsString::from("--output-type"),
                OsString::from("binary"),
                path.as_os_str().to_owned(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        child.wait().is_ok_and(|status| status.success())
    }

    fn run(
        &self,
        args: impl IntoIterator<Item = OsString>,
        input: Option<&[u8]>,
    ) -> io::Result<SopsOutput> {
        let mut child = Command::new(&self.executable)
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        if let Some(input) = input {
            let write_result = child.stdin.take().map_or_else(
                || Err(io::Error::other("SOPS stdin unavailable")),
                |mut stdin| stdin.write_all(input),
            );
            if let Err(error) = write_result {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }

        let mut stdout = SecretBytes(Vec::new());
        let mut child_stdout = child.stdout.take().ok_or_else(|| {
            io::Error::other("SOPS stdout unavailable")
        })?;
        if let Err(error) = child_stdout.read_to_end(&mut stdout.0) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let status = child.wait()?;
        Ok(SopsOutput { status, stdout })
    }
}

impl SecretResolver for SopsFileProvider {
    fn metadata(&self, reference: &SecretRef) -> Result<SecretMetadata, SecretBoundaryError> {
        let available = match self.secret_path(reference) {
            Ok(path) => fs::symlink_metadata(&path).is_ok_and(|metadata| {
                metadata.file_type().is_file() && self.can_decrypt(&path)
            }),
            // Metadata remains readable when the provider has no local store
            // or decryption identity; the availability bit carries that fact.
            Err(SecretBoundaryError::Unavailable) => false,
            Err(error) => return Err(error),
        };
        Ok(SecretMetadata::new(reference.clone(), "sops", available))
    }

    fn with_secret<T>(
        &self,
        reference: &SecretRef,
        operation: &mut dyn FnMut(&[u8]) -> T,
    ) -> Result<T, SecretBoundaryError> {
        let path = self.secret_path(reference)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| SecretBoundaryError::Unavailable)?;
        if !metadata.file_type().is_file() {
            return Err(SecretBoundaryError::Unavailable);
        }
        let plaintext = self.decrypt(&path).map_err(|()| SecretBoundaryError::Unavailable)?;
        Ok(operation(&plaintext.0))
    }
}

struct SecretBytes(Vec<u8>);

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

struct SopsOutput {
    status: ExitStatus,
    stdout: SecretBytes,
}

fn successful_stdout(output: SopsOutput) -> Result<SecretBytes, ()> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(())
    }
}

fn set_private_permissions(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(false);
        file.set_permissions(permissions)
    }
}

#[cfg(not(unix))]
struct PlaintextTemp(NamedTempFile);

#[cfg(not(unix))]
impl Drop for PlaintextTemp {
    fn drop(&mut self) {
        let file = self.0.as_file_mut();
        let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
            return;
        };
        if wipe_file_contents(file, length).is_err() {
            return;
        }
        let _ = file.set_len(0);
        let _ = file.sync_all();
    }
}

#[cfg(not(unix))]
fn wipe_file_contents(file: &mut File, mut remaining: u64) -> io::Result<()> {
    use std::io::{Seek, SeekFrom};

    file.seek(SeekFrom::Start(0))?;
    let zeros = [0_u8; 8192];
    while remaining != 0 {
        let count = usize::try_from(remaining.min(zeros.len() as u64)).unwrap_or(zeros.len());
        file.write_all(&zeros[..count])?;
        remaining -= count as u64;
    }
    Ok(())
}

fn sync_directory(path: &Path) {
    if let Ok(directory) = OpenOptions::new().read(true).open(path) {
        let _ = directory.sync_all();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};

    use tempfile::TempDir;

    use crate::{SecretBoundaryError, SecretRef, SecretResolver};

    use super::{SopsFileError, SopsFileProvider};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn fake_sops(directory: &TempDir) -> std::path::PathBuf {
        let executable = directory.path().join(format!(
            "fake-sops-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(
            &executable,
            "#!/bin/sh\n\
             if [ \"$1\" = \"--encrypt\" ]; then\n\
               value=$(cat)\n\
               case \"$value\" in reject-this-value) exit 23;; esac\n\
               printf 'ORNA-ENCRYPTED:'\n\
               printf '%s' \"$value\" | tr 'A-Za-z' 'N-ZA-Mn-za-m'\n\
               exit 0\n\
             fi\n\
             if [ \"$1\" = \"--decrypt\" ]; then\n\
               file=\"\"\n\
               for argument in \"$@\"; do file=\"$argument\"; done\n\
               sed 's/^ORNA-ENCRYPTED://' \"$file\" | tr 'N-ZA-Mn-za-m' 'A-Za-z'\n\
               exit 0\n\
             fi\n\
             exit 24\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        executable
    }

    #[test]
    fn writes_only_ciphertext_atomically_and_resolves_in_memory() {
        let directory = TempDir::new().unwrap();
        let root = directory.path().join("secrets");
        let provider = SopsFileProvider::new(&root).with_executable(fake_sops(&directory));
        let reference = SecretRef::new("messages.inbox").unwrap();
        let plaintext = b"fixture-private-value";

        provider.put(&reference, plaintext).unwrap();

        let encrypted_file = root.join("messages.inbox.sops.json");
        let encrypted = fs::read(&encrypted_file).unwrap();
        assert!(encrypted.starts_with(b"ORNA-ENCRYPTED:"));
        assert!(!encrypted
            .windows(plaintext.len())
            .any(|window| window == plaintext));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        assert_eq!(
            fs::metadata(&encrypted_file).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let metadata = provider.metadata(&reference).unwrap();
        assert_eq!(metadata.provider(), "sops");
        assert!(metadata.available());
        assert!(!format!("{metadata:?}").contains("fixture-private-value"));
        let mut callback = |bytes: &[u8]| bytes.to_vec();
        assert_eq!(provider.with_secret(&reference, &mut callback), Ok(plaintext.to_vec()));

        let previous = fs::read(&encrypted_file).unwrap();
        assert_eq!(
            provider.put(&reference, b"reject-this-value"),
            Err(SopsFileError::Unavailable)
        );
        assert_eq!(fs::read(&encrypted_file).unwrap(), previous);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn missing_identity_is_unavailable_and_references_cannot_escape_root() {
        let directory = TempDir::new().unwrap();
        let root = directory.path().join("secrets");
        let reference = SecretRef::new("messages.inbox").unwrap();
        let writer = SopsFileProvider::new(&root).with_executable(fake_sops(&directory));
        writer.put(&reference, b"fixture-private-value").unwrap();

        let no_key = directory.path().join("sops-no-key");
        fs::write(&no_key, "#!/bin/sh\nexit 25\n").unwrap();
        fs::set_permissions(&no_key, fs::Permissions::from_mode(0o700)).unwrap();
        let reader = SopsFileProvider::new(&root).with_executable(no_key);
        let metadata = reader.metadata(&reference).unwrap();
        assert!(!metadata.available());
        let callback_calls = std::cell::Cell::new(0);
        let mut callback = |_: &[u8]| callback_calls.set(callback_calls.get() + 1);
        assert_eq!(
            reader.with_secret(&reference, &mut callback),
            Err(SecretBoundaryError::Unavailable)
        );
        assert_eq!(callback_calls.get(), 0);

        let traversal = SecretRef::new("../outside").unwrap();
        assert_eq!(writer.put(&traversal, b"nope"), Err(SopsFileError::InvalidReference));
        assert_eq!(
            writer.metadata(&traversal),
            Err(SecretBoundaryError::InvalidReference)
        );
        assert_eq!(
            writer.with_secret(&traversal, &mut callback),
            Err(SecretBoundaryError::InvalidReference)
        );
        assert!(!directory.path().join("outside.sops.json").exists());

        let missing_store = directory.path().join("not-created");
        let no_store_provider =
            SopsFileProvider::new(&missing_store).with_executable(fake_sops(&directory));
        assert!(!no_store_provider.metadata(&reference).unwrap().available());
        assert!(!missing_store.exists());
    }

    #[test]
    fn provider_does_not_follow_ciphertext_symlinks() {
        let directory = TempDir::new().unwrap();
        let root = directory.path().join("secrets");
        fs::create_dir(&root).unwrap();
        let outside = directory.path().join("outside.sops.json");
        fs::write(&outside, b"ORNA-ENCRYPTED:fixture-private-value").unwrap();
        symlink(&outside, root.join("messages.inbox.sops.json")).unwrap();
        let provider = SopsFileProvider::new(&root).with_executable(fake_sops(&directory));
        let reference = SecretRef::new("messages.inbox").unwrap();

        assert!(!provider.metadata(&reference).unwrap().available());
        let mut callback = |_: &[u8]| ();
        assert_eq!(
            provider.with_secret(&reference, &mut callback),
            Err(SecretBoundaryError::Unavailable)
        );
    }
}
