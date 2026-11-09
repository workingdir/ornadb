use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Deserialize, Serialize)]
pub struct AbiVersion {
    pub major: u16,
    pub minor: u16,
}

impl AbiVersion {
    /// The version the registry publishes; read by the parity tests and the
    /// generated registry contract.
    pub const V1_0: Self = Self { major: 1, minor: 0 };

    /// Used by provider negotiation in the `orna-sys` library.
    pub const fn compatible_provider(self, provider: Self) -> bool {
        self.major == provider.major && provider.minor >= self.minor
    }
}
