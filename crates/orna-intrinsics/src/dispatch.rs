//! Dispatch and justification classes for the intrinsic catalogue.

/// Where an intrinsic's implementation lives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dispatch {
    /// Bound to a declaration in the pinned source; the Rust arm is authoritative.
    Native,
    /// Bound to a host operation in the `orna-sys` registry.
    Host { operation: &'static str },
    /// The Rust arm was deleted; the pinned source body is authoritative.
    SourceOnly,
}

impl Dispatch {
    /// The stable export name.
    pub const fn kind(self) -> &'static str {
        match self {
            Dispatch::Native => "Native",
            Dispatch::Host { .. } => "Host",
            Dispatch::SourceOnly => "SourceOnly",
        }
    }

    /// The `orna-sys` operation name for a host dispatch.
    pub const fn host_operation(self) -> Option<&'static str> {
        match self {
            Dispatch::Host { operation } => Some(operation),
            Dispatch::Native | Dispatch::SourceOnly => None,
        }
    }
}

/// Why the runtime keeps a Rust implementation for this operation.
///
/// The design's closed set is extended with [`Justification::SourceExpressible`]
/// and [`Justification::HistoricalPin`] so that the catalogue can classify the
/// inventory's `SRC` and `legacy` rows honestly. `MeasuredWorse` remains the
/// class for `SRC?` rows, whose Orna form was measured and rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Justification {
    /// Part of the intrinsic environment required by spec 9:5-7.
    CoreIntrinsic,
    HostResource,
    CanonicalFormat,
    PinnedUnicodeData,
    RepresentationAccess,
    PlanOrStreamMachinery,
    SuperlinearCanonicalKey,
    /// Expressible in Orna, but the Orna form was measured worse; see `note`.
    MeasuredWorse,
    /// Expressible in Orna; the Rust arm is scheduled for the std repo.
    SourceExpressible,
    /// Registered only so historical pins keep their captured behaviour.
    HistoricalPin,
}

impl Justification {
    /// The stable export name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Justification::CoreIntrinsic => "CoreIntrinsic",
            Justification::HostResource => "HostResource",
            Justification::CanonicalFormat => "CanonicalFormat",
            Justification::PinnedUnicodeData => "PinnedUnicodeData",
            Justification::RepresentationAccess => "RepresentationAccess",
            Justification::PlanOrStreamMachinery => "PlanOrStreamMachinery",
            Justification::SuperlinearCanonicalKey => "SuperlinearCanonicalKey",
            Justification::MeasuredWorse => "MeasuredWorse",
            Justification::SourceExpressible => "SourceExpressible",
            Justification::HistoricalPin => "HistoricalPin",
        }
    }
}
