//! Effect classes for the intrinsic catalogue.

/// The effect an intrinsic has on its environment.
///
/// The checker reads this instead of a separate per-operation table
/// (`spec 09:44`, ORNA-LIB-004). The classes are the design's closed set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    /// Deterministic over its arguments; safe in assertions and watches.
    Pure,
    /// Reads pinned data tables (Unicode, time-zone, locale). Deterministic for
    /// a captured snapshot, so also assertion-safe.
    PinnedData,
    /// Reads the host clock.
    Clock,
    /// Reads host entropy.
    Entropy,
    /// Crosses a host effect boundary (fs, process, net).
    Host,
    /// Consumes or advances a stream / checkpoint.
    Stream,
    /// Reads table state through a query plan.
    Query,
}

impl Effect {
    /// The stable export name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Effect::Pure => "Pure",
            Effect::PinnedData => "PinnedData",
            Effect::Clock => "Clock",
            Effect::Entropy => "Entropy",
            Effect::Host => "Host",
            Effect::Stream => "Stream",
            Effect::Query => "Query",
        }
    }

    /// True when the effect is reproducible from a captured snapshot alone.
    pub const fn is_snapshot_deterministic(self) -> bool {
        matches!(self, Effect::Pure | Effect::PinnedData)
    }
}
