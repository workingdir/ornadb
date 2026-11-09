//! `std.text` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const TEXT: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::TextTrim,
        module: Module::Text,
        name: "trim",
        leaf: Some("__trim"),
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "value",
                ty: SigType::Str,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Str,
        },
        effect: Effect::PinnedData,
        dispatch: Dispatch::Native,
        justification: Justification::PinnedUnicodeData,
        note: "trims the pinned Unicode 16.0.0 White_Space set; the Rust toolchain's tables are not a substitute",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TextSplit,
        module: Module::Text,
        name: "split",
        leaf: Some("__split"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "value",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "separator",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::List(&SigType::Str),
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::RepresentationAccess,
        note: "splits on Unicode scalar boundaries and preserves empty fields at either edge",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TextJoin,
        module: Module::Text,
        name: "join",
        leaf: Some("__join"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "values",
                    ty: SigType::List(&SigType::Str),
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "separator",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Str,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::RepresentationAccess,
        note: "recomposes Str without exposing its UTF-8 representation to source",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TextStartsWith,
        module: Module::Text,
        name: "starts_with",
        leaf: Some("__starts_with"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "value",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "prefix",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Bool,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::MeasuredWorse,
        note: "the source form must materialise a scalar list; the measured threshold keeps this native",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TextEndsWith,
        module: Module::Text,
        name: "ends_with",
        leaf: Some("__ends_with"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "value",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "suffix",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Bool,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::MeasuredWorse,
        note: "the source form must materialise a scalar list; the measured threshold keeps this native",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TextContains,
        module: Module::Text,
        name: "contains",
        leaf: Some("__contains"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "value",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "needle",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Bool,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::MeasuredWorse,
        note: "the source form must materialise a scalar list; the measured threshold keeps this native",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TextReplace,
        module: Module::Text,
        name: "replace",
        leaf: Some("__replace"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "value",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "from",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "to",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Str,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::MeasuredWorse,
        note: "replace is join(split(value, from), to); the measured threshold keeps this native",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TextNormalise,
        module: Module::Text,
        name: "normalise",
        leaf: Some("__normalise"),
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "value",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "form",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Str,
        },
        effect: Effect::PinnedData,
        dispatch: Dispatch::Native,
        justification: Justification::PinnedUnicodeData,
        note: "canonical NFC/NFD normalisation from the pinned Unicode 16.0.0 tables",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TextLower,
        module: Module::Text,
        name: "lower",
        leaf: Some("__lower"),
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "value",
                ty: SigType::Str,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Str,
        },
        effect: Effect::PinnedData,
        dispatch: Dispatch::Native,
        justification: Justification::PinnedUnicodeData,
        note: "full Unicode 16.0.0 lowercase from the pinned tables",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::TextUpper,
        module: Module::Text,
        name: "upper",
        leaf: Some("__upper"),
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "value",
                ty: SigType::Str,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Str,
        },
        effect: Effect::PinnedData,
        dispatch: Dispatch::Native,
        justification: Justification::PinnedUnicodeData,
        note: "full Unicode 16.0.0 uppercase including Final_Sigma-adjacent expansions",
        pins: PinRange::CURRENT,
    },
];
