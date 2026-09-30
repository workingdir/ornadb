//! SOPS-backed secret files. Plaintext staging is bounded to one private
//! temporary file during writes; decryption output stays in a zeroizing
//! memory buffer.

use std::{
    ffi::OsString,
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};
use tempfile::{Builder, NamedTempFile};
use zeroize::{Zeroize, Zeroizing};

use crate::{SecretBoundaryError, SecretMetadata, SecretRef, SecretResolver};

const PROVIDER_NAME: &str = "sops";
const UNAVAILABLE: SecretBoundaryError = SecretBoundaryError::Unavailable;

/// Stores one SOPS-encrypted binary document per secret reference.
///
/// SOPS owns encryption and decryption, including age, PGP and KMS key
/// selection. References are SHA-256 mapped to opaque filenames so arbitrary
/// stable names cannot become filesystem paths. The configured directory is
/// expected to be the application's versioned encrypted-secret directory.
pub struct SopsProvider {
    root: PathBuf,
    executable: OsString,
    temp_dir: Option<PathBuf>,
}

impl SopsProvider {
    /// Creates a provider rooted at an encrypted-secret directory.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            executable: OsString::from("sops"),
            temp_dir: None,
        }
    }

    /// Uses a specific SOPS executable, primarily for controlled host setup
    /// and adapter tests.
    #[must_use]
    pub fn with_executable(mut self, executable: impl Into<OsString>) -> Self {
        self.executable = executable.into();
        self
    }

    /// Places temporary plaintext in a caller-selected directory.
    ///
    /// The directory should be private or memory-backed. Files created there
    /// use the restrictive permissions provided by `tempfile` and are wiped
    /// best-effort before unlinking. By default, the operating system temp
    /// directory is used.
    #[must_use]
    pub fn with_temp_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.temp_dir = Some(directory.into());
        self
    }

    /// Encrypts and atomically stores a secret document.
    ///
    /// The plaintext is placed in a mode-restricted temporary file because
    /// SOPS' file-based encrypt command needs a filename for repository
    /// configuration selection. On every exit path the temporary is
    /// best-effort overwritten and removed. Only the encrypted output is
    /// renamed into the provider directory.
    ///
    /// # Errors
    ///
    /// Returns [`SecretBoundaryError::Unavailable`] if SOPS, its encryption
    /// identity, or the filesystem cannot complete the write. Provider details
    /// are intentionally not included in the error.
    pub fn store_secret(
        &self,
        reference: &SecretRef,
        secret: &[u8],
    ) -> Result<(), SecretBoundaryError> {
        fs::create_dir_all(&self.root).map_err(|_| UNAVAILABLE)?;
        let encrypted_path = self.encrypted_path(reference);
        let mut plaintext =
            PlaintextTemp::new(self.temp_dir.as_deref()).map_err(|_| UNAVAILABLE)?;
        plaintext
            .file_mut()
            .write_all(secret)
            .map_err(|_| UNAVAILABLE)?;
        plaintext
            .file()
            .as_file()
            .sync_all()
            .map_err(|_| UNAVAILABLE)?;

        let mut ciphertext = self.encrypt(plaintext.path(), &encrypted_path)?;
        plaintext.cleanup().map_err(|_| UNAVAILABLE)?;

        let mut staged = Builder::new()
            .prefix(".orna-sops-encrypted-")
            .tempfile_in(&self.root)
            .map_err(|_| UNAVAILABLE)?;
        staged
            .write_all(ciphertext.as_slice())
            .map_err(|_| UNAVAILABLE)?;
        staged.as_file().sync_all().map_err(|_| UNAVAILABLE)?;
        ciphertext.zeroize();
        staged
            .persist(&encrypted_path)
            .map_err(|_| UNAVAILABLE)?;
        Ok(())
    }

    fn encrypted_path(&self, reference: &SecretRef) -> PathBuf {
        let digest = Sha256::digest(reference.as_str().as_bytes());
        let mut name = String::with_capacity(64 + ".sops.yaml".len());
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(name, "{byte:02x}");
        }
        name.push_str(".sops.yaml");
        self.root.join(name)
    }

    fn encrypt(
        &self,
        plaintext: &Path,
        encrypted_path: &Path,
    ) -> Result<Zeroizing<Vec<u8>>, SecretBoundaryError> {
        self.run_sops("encrypt", plaintext, encrypted_path)
    }

    fn decrypt(
        &self,
        encrypted_path: &Path,
    ) -> Result<Zeroizing<Vec<u8>>, SecretBoundaryError> {
        let file_type = fs::symlink_metadata(encrypted_path)
            .map_err(|_| UNAVAILABLE)?
            .file_type();
        if !file_type.is_file() {
            return Err(UNAVAILABLE);
        }
        // Missing identities, malformed documents, and command failures do
        // not have a stable safe distinction in the SOPS CLI boundary. Keep
        // all of them unavailable so a clone without keys is not treated as
        // corrupt database state, and never forward provider diagnostics.
        self.run_sops("decrypt", encrypted_path, encrypted_path)
    }

    fn run_sops(
        &self,
        action: &str,
        input: &Path,
        filename_override: &Path,
    ) -> Result<Zeroizing<Vec<u8>>, SecretBoundaryError> {
        let mut output = Command::new(&self.executable)
            .arg(action)
            .arg("--input-type")
            .arg("binary")
            .arg("--output-type")
            .arg("binary")
            .arg("--filename-override")
            .arg(filename_override)
            .arg(input)
            .output()
            .map_err(|_| UNAVAILABLE)?;

        output.stderr.zeroize();
        if !output.status.success() {
            output.stdout.zeroize();
            return Err(UNAVAILABLE);
        }
        Ok(Zeroizing::new(output.stdout))
    }
}

impl SecretResolver for SopsProvider {
    fn metadata(
        &self,
        reference: &SecretRef,
    ) -> Result<SecretMetadata, SecretBoundaryError> {
        // Availability includes whether the local identity can decrypt this
        // file, so metadata probes through SOPS. That can contact a configured
        // KMS on each lookup, but SOPS emits plaintext only to this short-lived
        // zeroizing memory buffer; no bytes are returned through metadata.
        let available = self.decrypt(&self.encrypted_path(reference)).is_ok();
        Ok(SecretMetadata::new(reference.clone(), PROVIDER_NAME, available))
    }

    fn with_secret<T>(
        &self,
        reference: &SecretRef,
        operation: &mut dyn FnMut(&[u8]) -> T,
    ) -> Result<T, SecretBoundaryError> {
        let secret = self.decrypt(&self.encrypted_path(reference))?;
        Ok(operation(secret.as_slice()))
    }
}

struct PlaintextTemp(Option<NamedTempFile>);

impl PlaintextTemp {
    fn new(directory: Option<&Path>) -> std::io::Result<Self> {
        let mut builder = Builder::new();
        builder.prefix(".orna-sops-plaintext-").suffix(".tmp");
        let file = match directory {
            Some(directory) => builder.tempfile_in(directory)?,
            None => builder.tempfile()?,
        };
        Ok(Self(Some(file)))
    }

    fn file(&self) -> &NamedTempFile {
        self.0.as_ref().expect("plaintext temp is live")
    }

    fn file_mut(&mut self) -> &mut NamedTempFile {
        self.0.as_mut().expect("plaintext temp is live")
    }

    fn path(&self) -> &Path {
        self.file().path()
    }

    fn cleanup(mut self) -> std::io::Result<()> {
        let wipe_result = wipe_plaintext(self.file_mut());
        let file = self.0.take().expect("plaintext temp is live");
        let close_result = file.close();
        wipe_result.and(close_result)
    }
}

impl Drop for PlaintextTemp {
    fn drop(&mut self) {
        if let Some(file) = &mut self.0 {
            let _ = wipe_plaintext(file);
        }
    }
}

fn wipe_plaintext(file: &mut NamedTempFile) -> std::io::Result<()> {
    let length = file.as_file().metadata()?.len();
    let file = file.as_file_mut();
    file.seek(SeekFrom::Start(0))?;

    let zeros = [0_u8; 4096];
    let mut remaining = length;
    while remaining > 0 {
        let count = remaining.min(zeros.len() as u64) as usize;
        file.write_all(&zeros[..count])?;
        remaining -= count as u64;
    }
    file.set_len(0)?;
    file.sync_all()
}
