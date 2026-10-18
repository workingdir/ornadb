//! Typed, authority-free OVB payloads returned by the browser Wasm profile.

use orna_foundation_v1::{CanonicalValue, ValueError};

/// One validated result emitted by the shared evaluator's browser profile.
///
/// OVB values are data, not callable server handles or closures. Decoding this
/// wrapper does not grant authority to query storage or invoke server functions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserProfileValue(CanonicalValue);

impl BrowserProfileValue {
    /// Decode and validate one canonical OVB-1 value returned by the Wasm profile.
    pub fn decode(bytes: &[u8]) -> Result<Self, ValueError> {
        CanonicalValue::decode(bytes).map(Self)
    }

    /// Borrow the validated model value.
    #[must_use]
    pub const fn value(&self) -> &CanonicalValue {
        &self.0
    }
}
