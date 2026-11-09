//! `std.concurrent` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const CONCURRENT: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::ConcurrentParallel,
        module: Module::Concurrent,
        name: "parallel",
        leaf: None,
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "callbacks",
                ty: SigType::List(&SigType::Fn(&[], &SigType::Param(0))),
                has_default: false,
                pipeline: false,
            }],
            result: SigType::List(&SigType::Param(0)),
            arities: &[],
        },
        effect: Effect::Host,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "runs one owned child activation per callback and joins results in input order",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::ConcurrentRace,
        module: Module::Concurrent,
        name: "race",
        leaf: None,
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "callbacks",
                ty: SigType::List(&SigType::Fn(&[], &SigType::Param(0))),
                has_default: false,
                pipeline: false,
            }],
            result: SigType::Param(0),
            arities: &[],
        },
        effect: Effect::Host,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "returns the first observed success and cancels every losing child",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::ConcurrentTimeout,
        module: Module::Concurrent,
        name: "timeout",
        leaf: None,
        signature: Signature {
            type_params: &["T"],
            params: &[
                Param {
                    name: "callback",
                    ty: SigType::Fn(&[], &SigType::Param(0)),
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "duration",
                    ty: SigType::Duration,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Param(0),
            arities: &[],
        },
        effect: Effect::Host,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "runs one owned child under an elapsed-time deadline and joins it on expiry",
        pins: PinRange::CURRENT,
    },
];
