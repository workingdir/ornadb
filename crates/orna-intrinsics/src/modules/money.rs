//! `std.money` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const MONEY: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::MoneyQuantize,
        module: Module::Money,
        name: "quantize",
        leaf: Some("__quantize"),
        signature: Signature {
            type_params: &["T"],
            params: &[
                Param {
                    name: "amount",
                    ty: SigType::Param(0),
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "minor_digits",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "rounding",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Param(0),
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::MeasuredWorse,
        note: "exact minor-unit quantisation with an explicit rounding rule; the source form is measured worse",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::MoneyAllocate,
        module: Module::Money,
        name: "allocate",
        leaf: Some("__allocate"),
        signature: Signature {
            type_params: &["T"],
            params: &[
                Param {
                    name: "amount",
                    ty: SigType::Param(0),
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "weights",
                    ty: SigType::List(&SigType::Int),
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "minor_digits",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "rounding",
                    ty: SigType::Option(&SigType::Str),
                    has_default: true,
                    pipeline: false,
                },
            ],
            result: SigType::List(&SigType::Param(0)),
            arities: &[3, 4],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::MeasuredWorse,
        note: "allocates exact minor units by descending fractional remainder with input-index ties",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::MoneyFormat,
        module: Module::Money,
        name: "format",
        leaf: Some("__format"),
        signature: Signature {
            type_params: &["T"],
            params: &[
                Param {
                    name: "amount",
                    ty: SigType::Param(0),
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "currency_code",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "minor_digits",
                    ty: SigType::Int,
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "rounding",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "locale",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Str,
            arities: &[],
        },
        effect: Effect::PinnedData,
        dispatch: Dispatch::Native,
        justification: Justification::HostResource,
        note: "symbols, grouping and decimal marks come from the captured locale profile",
        pins: PinRange::CURRENT,
    },
];
