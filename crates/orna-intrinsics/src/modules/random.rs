//! `std.random` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const RANDOM: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::RandomBytes,
        module: Module::Random,
        name: "bytes",
        leaf: Some("__bytes"),
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "count",
                ty: SigType::Int,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Blob,
        },
        effect: Effect::Entropy,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "returns exactly count bytes from the host cryptographically secure random source",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::RandomInteger,
        module: Module::Random,
        name: "integer",
        leaf: Some("__integer"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "lower_inclusive",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "upper_exclusive",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Int,
        },
        effect: Effect::Entropy,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "uniform rejection sampling from a half-open interval without modulo bias",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::RandomChoose,
        module: Module::Random,
        name: "choose",
        leaf: Some("__choose"),
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "values",
                ty: SigType::List(&SigType::Param(0)),
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Option(&SigType::Param(0)),
        },
        effect: Effect::Entropy,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "selects one list position uniformly and returns null for an empty list",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::RandomShuffle,
        module: Module::Random,
        name: "shuffle",
        leaf: Some("__shuffle"),
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "values",
                ty: SigType::List(&SigType::Param(0)),
                has_default: false,
                pipeline: true,
            }],
            result: SigType::List(&SigType::Param(0)),
        },
        effect: Effect::Entropy,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "unbiased Fisher-Yates permutation retaining every input position exactly once",
        pins: PinRange::CURRENT,
    },
];
