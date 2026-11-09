//! `std.bits` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const BITS: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::BitsBitOr,
        module: Module::Bits,
        name: "bit_or",
        leaf: Some("__bit_or"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "left",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "right",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Int,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::RepresentationAccess,
        note: "bitwise OR over the unbounded signed two's-complement Int model",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::BitsBitAnd,
        module: Module::Bits,
        name: "bit_and",
        leaf: Some("__bit_and"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "left",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "right",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Int,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::RepresentationAccess,
        note: "bitwise AND over the unbounded signed two's-complement Int model",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::BitsBitXor,
        module: Module::Bits,
        name: "bit_xor",
        leaf: Some("__bit_xor"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "left",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "right",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Int,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::RepresentationAccess,
        note: "bitwise XOR over the unbounded signed two's-complement Int model",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::BitsBitNot,
        module: Module::Bits,
        name: "bit_not",
        leaf: Some("__bit_not"),
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "value",
                ty: SigType::Int,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Int,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::RepresentationAccess,
        note: "bit_not(x) = -x - 1 over the unbounded signed two's-complement model",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::BitsShiftLeft,
        module: Module::Bits,
        name: "shift_left",
        leaf: Some("__shift_left"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "value",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "count",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Int,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::RepresentationAccess,
        note: "exact left shift with a nonnegative count and resource bounds",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::BitsShiftRight,
        module: Module::Bits,
        name: "shift_right",
        leaf: Some("__shift_right"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "value",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "count",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Int,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::RepresentationAccess,
        note: "arithmetic right shift with a nonnegative count",
        pins: PinRange::CURRENT,
    },
];
