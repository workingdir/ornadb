use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Deserialize, Serialize)]
pub struct AbiVersion {
    pub major: u16,
    pub minor: u16,
}

impl AbiVersion {
    /// The version the registry publishes; read by the parity tests rather than
    /// by this file's own code.
    #[allow(dead_code)]
    pub const V1_0: Self = Self { major: 1, minor: 0 };

    /// Used by the provider negotiation in `provider.rs`, which shares this
    /// source file between the library and the build-script target; the
    /// build-script copy cannot see that caller.
    #[allow(dead_code)]
    pub const fn compatible_provider(self, provider: Self) -> bool {
        self.major == provider.major && provider.minor >= self.minor
    }
}
