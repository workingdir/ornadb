//! `std.math` descriptors.
//!
//! The current `STANDARD_BINDING_MODULES` table registers `std.math.` with zero
//! operations, so these six arms are already unreachable. They are recorded as
//! `SourceOnly` because the pinned source bodies are authoritative; stage S5
//! deletes the arms rather than porting them.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const MATH: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::MathIncrement,
        module: Module::Math,
        name: "increment",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "value",
                ty: SigType::Int,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Int,
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::SourceOnly,
        justification: Justification::SourceExpressible,
        note: "pinned source implements exact successor; the unreachable Rust arm is deleted in S5",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::MathDecrement,
        module: Module::Math,
        name: "decrement",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "value",
                ty: SigType::Int,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Int,
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::SourceOnly,
        justification: Justification::SourceExpressible,
        note: "pinned source implements exact predecessor; the unreachable Rust arm is deleted in S5",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::MathIsZero,
        module: Module::Math,
        name: "is_zero",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "value",
                ty: SigType::Int,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Bool,
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::SourceOnly,
        justification: Justification::SourceExpressible,
        note: "pinned source reports exact zero; the unreachable Rust arm is deleted in S5",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::MathMin,
        module: Module::Math,
        name: "min",
        leaf: None,
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
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::SourceOnly,
        justification: Justification::SourceExpressible,
        note: "pinned source returns the lesser Int; the unreachable Rust arm widened the declared signature",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::MathMax,
        module: Module::Math,
        name: "max",
        leaf: None,
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
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::SourceOnly,
        justification: Justification::SourceExpressible,
        note: "pinned source returns the greater Int; the unreachable Rust arm widened the declared signature",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::MathClamp,
        module: Module::Math,
        name: "clamp",
        leaf: None,
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
                    name: "lower",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "upper",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Int,
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::SourceOnly,
        justification: Justification::SourceExpressible,
        note: "pinned source bounds the value and returns lower for reversed bounds",
        pins: PinRange::CURRENT,
    },
];
