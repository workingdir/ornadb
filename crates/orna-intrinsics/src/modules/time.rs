//! `std.time` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const TIME: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::TimeOffsetAt,
        module: Module::Time,
        name: "offset_at",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "instant",
                    ty: SigType::Instant,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "zone",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Int,
            arities: &[],
        },
        effect: Effect::PinnedData,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "returns signed seconds east of UTC from the pinned IANA edition, including at a transition",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TimeResolveLocal,
        module: Module::Time,
        name: "resolve_local",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "local",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "zone",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "ambiguous",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "gap",
                    ty: SigType::Option(&SigType::Str),
                    has_default: true,
                    pipeline: false,
                },
            ],
            result: SigType::Instant,
            arities: &[3, 4],
        },
        effect: Effect::PinnedData,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "resolves a local timestamp against the pinned zone edition with explicit ambiguous and gap policies",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TimeDurationCompactFormat,
        module: Module::Time,
        name: "duration.compact.format",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "duration",
                    ty: SigType::Duration,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "context",
                    ty: SigType::Record(&[("locale", SigType::Str), ("time_zone", SigType::Str)]),
                    has_default: true,
                    pipeline: false,
                },
            ],
            result: SigType::Str,
            arities: &[1, 2],
        },
        effect: Effect::PinnedData,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "compact elapsed form from the pinned English profile with an immutable validated context",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TimeDurationClockFormat,
        module: Module::Time,
        name: "duration.clock.format",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "duration",
                    ty: SigType::Duration,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "context",
                    ty: SigType::Record(&[("locale", SigType::Str), ("time_zone", SigType::Str)]),
                    has_default: true,
                    pipeline: false,
                },
            ],
            result: SigType::Str,
            arities: &[1, 2],
        },
        effect: Effect::PinnedData,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "clock elapsed form from the pinned English profile with an immutable validated context",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TimeDurationWordsFormat,
        module: Module::Time,
        name: "duration.words.format",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "duration",
                    ty: SigType::Duration,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "context",
                    ty: SigType::Record(&[("locale", SigType::Str), ("time_zone", SigType::Str)]),
                    has_default: true,
                    pipeline: false,
                },
            ],
            result: SigType::Str,
            arities: &[1, 2],
        },
        effect: Effect::PinnedData,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "words elapsed form from the pinned English profile with an immutable validated context",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TimeDurationIsoFormat,
        module: Module::Time,
        name: "duration.iso.format",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "duration",
                    ty: SigType::Duration,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "context",
                    ty: SigType::Record(&[("locale", SigType::Str), ("time_zone", SigType::Str)]),
                    has_default: true,
                    pipeline: false,
                },
            ],
            result: SigType::Str,
            arities: &[1, 2],
        },
        effect: Effect::PinnedData,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "ISO elapsed form from the pinned profile; calendar periods are not converted to fixed seconds",
        pins: PinRange::CURRENT,
    },
];
