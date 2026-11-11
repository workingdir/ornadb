//! The loader's failure channel, with the codes of docs/design/native-libraries.md
//! section 11.

use std::fmt::{self, Write};
use std::path::PathBuf;

/// Why a library could not be loaded or a symbol could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// The file could not be read, staged or loaded.
    Library {
        /// The library file the caller named.
        path: PathBuf,
        /// What failed.
        reason: String,
    },
    /// The file's SHA-256 differs from the pinned digest. Nothing was loaded.
    Digest {
        /// The library file the caller named.
        path: PathBuf,
        /// The pinned digest, in lowercase hexadecimal.
        expected: String,
        /// The digest of the bytes read, in lowercase hexadecimal.
        actual: String,
    },
    /// `orna_library_abi` is missing or returns a different number.
    Abi {
        /// The ABI number the caller required.
        expected: u32,
        /// The number the library returned, or `None` when it does not export one.
        found: Option<u32>,
    },
    /// The library does not export the requested symbol.
    Symbol {
        /// The symbol name that was requested.
        name: String,
    },
}

impl LoadError {
    /// The error code of section 11, for example `ORNA-STD-NATIVE-DIGEST`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Library { .. } => "ORNA-STD-NATIVE-LIBRARY",
            Self::Digest { .. } => "ORNA-STD-NATIVE-DIGEST",
            Self::Abi { .. } => "ORNA-STD-NATIVE-ABI",
            Self::Symbol { .. } => "ORNA-STD-NATIVE-SYMBOL",
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.code();
        match self {
            Self::Library { path, reason } => {
                write!(
                    formatter,
                    "{code}: native library {} {reason}",
                    path.display()
                )
            }
            Self::Digest {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "{code}: native library {} does not match its digest: expected {expected}, found {actual}",
                path.display()
            ),
            Self::Abi { expected, found } => match found {
                Some(found) => write!(
                    formatter,
                    "{code}: library ABI is {found}, expected {expected}"
                ),
                None => write!(
                    formatter,
                    "{code}: library does not export orna_library_abi, expected {expected}"
                ),
            },
            Self::Symbol { name } => write!(formatter, "{code}: symbol {name} is not exported"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Lowercase hexadecimal of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}
