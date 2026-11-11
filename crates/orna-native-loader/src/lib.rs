//! Loads a native library whose bytes match the digest its manifest pins
//! (docs/design/native-libraries.md, sections 8 and 10).
//!
//! The order is fixed: read the file, check its SHA-256, stage a private copy,
//! load that copy, check `orna_library_abi`, then resolve symbols on demand. A
//! digest mismatch loads nothing. The caller passes the expected digest and the
//! expected library ABI number in; this crate does not read a manifest.
//!
//! All `unsafe` code is in the `ffi` module, and each block states its invariant.

#![deny(unsafe_code)]

mod error;
mod ffi;
mod stage;

use std::marker::PhantomData;
use std::path::Path;

use sha2::{Digest, Sha256};

pub use error::LoadError;

/// A library that passed the digest and ABI checks and is loaded into the process.
///
/// Dropping it unloads the library. Symbols borrowed from it cannot outlive it.
pub struct NativeLibrary {
    loaded: ffi::Loaded,
    // Keeps the staged copy alive for the library's lifetime.
    _staged: stage::Staged,
}

/// The address of an exported symbol, valid only while its library is loaded.
pub struct NativeSymbol<'library> {
    address: *const std::ffi::c_void,
    _library: PhantomData<&'library NativeLibrary>,
}

impl NativeSymbol<'_> {
    /// The symbol's address in the loaded library.
    #[must_use]
    pub fn address(&self) -> *const std::ffi::c_void {
        self.address
    }
}

/// Verifies `path` against `expected_sha256`, stages a private copy, loads it, and
/// checks that its `orna_library_abi` export returns `expected_abi`.
///
/// # Errors
///
/// Returns [`LoadError::Library`] when the file cannot be read, staged or loaded,
/// [`LoadError::Digest`] when its SHA-256 differs (nothing is loaded), and
/// [`LoadError::Abi`] when the export is missing or returns another number.
pub fn load_verified(
    path: &Path,
    expected_sha256: &[u8; 32],
    expected_abi: u32,
) -> Result<NativeLibrary, LoadError> {
    let bytes = std::fs::read(path).map_err(|error| LoadError::Library {
        path: path.to_owned(),
        reason: error.to_string(),
    })?;
    let actual: [u8; 32] = Sha256::digest(&bytes).into();
    if &actual != expected_sha256 {
        return Err(LoadError::Digest {
            path: path.to_owned(),
            expected: error::hex(expected_sha256),
            actual: error::hex(&actual),
        });
    }
    let staged = stage::Staged::new(&bytes).map_err(|error| LoadError::Library {
        path: path.to_owned(),
        reason: format!("cannot be staged ({error})"),
    })?;
    let loaded = ffi::Loaded::open(staged.path()).map_err(|reason| LoadError::Library {
        path: path.to_owned(),
        reason,
    })?;
    let found = loaded.abi();
    if found != Some(expected_abi) {
        return Err(LoadError::Abi {
            expected: expected_abi,
            found,
        });
    }
    Ok(NativeLibrary {
        loaded,
        _staged: staged,
    })
}

impl NativeLibrary {
    /// Resolves the exported symbol `name`.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::Symbol`] when the library does not export `name`.
    pub fn symbol(&self, name: &str) -> Result<NativeSymbol<'_>, LoadError> {
        let address = self
            .loaded
            .symbol_address(name)
            .ok_or_else(|| LoadError::Symbol {
                name: name.to_owned(),
            })?;
        Ok(NativeSymbol {
            address,
            _library: PhantomData,
        })
    }
}
