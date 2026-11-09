//! The root intrinsic environment (`spec 09:5-7`).

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const CORE: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::CoreSome,
        module: Module::Core,
        name: "Some",
        leaf: None,
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "value",
                ty: SigType::Param(0),
                has_default: false,
                pipeline: false,
            }],
            result: SigType::Option(&SigType::Param(0)),
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CoreIntrinsic,
        note: "constructs the core optional value; available without an import",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::CoreNow,
        module: Module::Core,
        name: "now",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[],
            result: SigType::Instant,
        },
        effect: Effect::Clock,
        dispatch: Dispatch::Native,
        justification: Justification::CoreIntrinsic,
        note: "reads the activation-scoped host clock through the effect boundary",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::CoreUuid7,
        module: Module::Core,
        name: "uuid7",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[],
            result: SigType::Uuid,
        },
        effect: Effect::Entropy,
        dispatch: Dispatch::Native,
        justification: Justification::CoreIntrinsic,
        note: "reads the activation-scoped clock and entropy for a time-ordered UUID",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::CoreError,
        module: Module::Core,
        name: "error",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "code",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "message",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Error,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CoreIntrinsic,
        note: "constructs the core Error value from a code and message",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::CoreFail,
        module: Module::Core,
        name: "fail",
        leaf: None,
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "error",
                ty: SigType::Error,
                has_default: false,
                pipeline: false,
            }],
            result: SigType::Param(0),
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CoreIntrinsic,
        note: "raises the core failure channel with a declared result type",
        pins: PinRange::CURRENT,
    },
];
