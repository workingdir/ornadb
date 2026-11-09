//! Pin ranges for the intrinsic catalogue.

/// The commit of the pinned standard library the current catalogue targets.
///
/// This is the `stdlib/std` gitlink on `origin/main` when the design was
/// written; `scripts/check_fixture_manifest.py` keeps the submodule pin honest.
pub const CURRENT_STD_PIN: &str = "b3ce954c38483e2e1f9edbd9cc2a9906b94680df";

/// The pins in which one binding is active (`spec 09:11`, ORNA-LIB-001).
///
/// Pin identities are opaque snapshot labels, not semver ranges: a historical
/// snapshot is identified by the module text it captured. `contains` therefore
/// compares identity, and the two endpoints are the only identities recorded
/// here. A later stage may widen this to an ordered range once the project
/// exposes a total order over pins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinRange {
    /// First pinned std commit that declares the binding.
    pub since: &'static str,
    /// Last pinned std commit that declares it, or `None` for the current pin.
    pub until: Option<&'static str>,
}

impl PinRange {
    /// Active in the current pinned snapshot only.
    pub const CURRENT: PinRange = PinRange {
        since: CURRENT_STD_PIN,
        until: None,
    };

    /// Active only in the historical `module-upgrade-std-collection` v4/v5 pins.
    pub const HISTORICAL_V4_V5: PinRange = PinRange {
        since: "module-upgrade-std-collection-v4",
        until: Some("module-upgrade-std-collection-v5"),
    };

    /// Reports whether `pin` is one of the identities this range records.
    pub fn contains(&self, pin: &str) -> bool {
        pin == self.since || self.until == Some(pin)
    }

    /// True when this range targets the current pin.
    pub const fn is_current(&self) -> bool {
        self.until.is_none()
    }
}
