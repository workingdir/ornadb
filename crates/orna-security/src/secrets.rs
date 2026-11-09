//! Secret references, non-sensitive metadata, and the adapter-owned resolver boundary.

use serde::Serialize;
use std::fmt;

/// A stable, non-secret name for one externally managed secret.
///
/// The reference is descriptive data, not a resolution capability. The SOPS
/// provider maps names to opaque hashed filenames; references never select a
/// path, decryption identity, or external authority.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SecretRef(String);

impl SecretRef {
    /// Creates a reference from its stable name.
    ///
    /// Names are intentionally not assigned a provider-specific grammar. Empty
    /// and control-containing names are rejected so a canonical textual
    /// reference cannot contain framing or diagnostic separators.
    ///
    /// # Errors
    ///
    /// Returns [`SecretBoundaryError::InvalidReference`] for an empty or
    /// control-containing name.
    pub fn new(value: impl Into<String>) -> Result<Self, SecretBoundaryError> {
        let value = value.into();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(SecretBoundaryError::InvalidReference);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for SecretRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Non-sensitive metadata exposed for a secret reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SecretMetadata {
    reference: SecretRef,
    provider: String,
    available: bool,
}

impl SecretMetadata {
    #[must_use]
    pub fn new(reference: SecretRef, provider: impl Into<String>, available: bool) -> Self {
        Self {
            reference,
            provider: provider.into(),
            available,
        }
    }

    #[must_use]
    pub fn reference(&self) -> &SecretRef {
        &self.reference
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    #[must_use]
    pub const fn available(&self) -> bool {
        self.available
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretBoundaryError {
    InvalidReference,
    Denied,
    Unavailable,
}

impl fmt::Display for SecretBoundaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidReference => "invalid secret reference",
            Self::Denied => "secret resolution denied",
            Self::Unavailable => "secret unavailable",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SecretBoundaryError {}

/// Adapter-owned secret boundary.
///
/// `metadata` returns only stable, non-secret facts. Providers may probe
/// availability by decrypting into memory and immediately discarding the
/// result. `with_secret` is the explicit privileged resolution operation;
/// implementations must decrypt in memory and return
/// [`SecretBoundaryError::Unavailable`] when the external identity is absent.
/// The callback is the only byte-bearing surface exposed by this crate, so
/// resolved material has no public `Debug`, `Display`, or serialization API.
pub trait SecretResolver {
    /// Returns non-sensitive metadata without disclosing resolved secret bytes.
    ///
    /// # Errors
    ///
    /// Returns [`SecretBoundaryError::Denied`] when metadata access is denied.
    /// Providers report ordinary unavailability with
    /// [`SecretMetadata::available`] set to `false`.
    fn metadata(&self, reference: &SecretRef) -> Result<SecretMetadata, SecretBoundaryError>;

    /// Resolves the secret for one explicitly privileged callback.
    ///
    /// # Errors
    ///
    /// Returns a provider-owned boundary error when disclosure is denied or
    /// the external decryption identity is unavailable.
    fn with_secret<T>(
        &self,
        reference: &SecretRef,
        operation: &mut dyn FnMut(&[u8]) -> T,
    ) -> Result<T, SecretBoundaryError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Resolver;

    impl SecretResolver for Resolver {
        fn metadata(&self, reference: &SecretRef) -> Result<SecretMetadata, SecretBoundaryError> {
            Ok(SecretMetadata::new(reference.clone(), "sops", true))
        }

        fn with_secret<T>(
            &self,
            _: &SecretRef,
            operation: &mut dyn FnMut(&[u8]) -> T,
        ) -> Result<T, SecretBoundaryError> {
            Ok(operation(b"not-for-diagnostics"))
        }
    }

    #[test]
    fn secret_reference_and_metadata_never_contain_secret_bytes() {
        let reference = SecretRef::new("messages.inbox").unwrap();
        assert_eq!(reference.as_str(), "messages.inbox");
        assert_eq!(reference.to_string(), "messages.inbox");
        assert!(SecretRef::new("").is_err());
        assert!(SecretRef::new("messages\ninbox").is_err());

        let resolver = Resolver;
        let metadata = resolver.metadata(&reference).unwrap();
        let debug = format!("{metadata:?}");
        assert!(metadata.available());
        assert_eq!(metadata.provider(), "sops");
        assert_eq!(metadata.reference(), &reference);
        assert!(!debug.contains("not-for-diagnostics"));

        let mut callback = |secret: &[u8]| secret.len();
        assert_eq!(resolver.with_secret(&reference, &mut callback), Ok(19));
    }
}
