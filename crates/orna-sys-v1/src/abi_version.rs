use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Deserialize, Serialize)]
pub struct AbiVersion {
    pub major: u16,
    pub minor: u16,
}

impl AbiVersion {
    pub const V1_0: Self = Self { major: 1, minor: 0 };

    pub const fn compatible_provider(self, provider: Self) -> bool {
        self.major == provider.major && provider.minor >= self.minor
    }
}
