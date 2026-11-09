//! `std.encoding.json` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const JSON: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::JsonEncode,
        module: Module::Json,
        name: "encode",
        leaf: Some("__encode"),
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "value",
                ty: SigType::Param(0),
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Str,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CanonicalFormat,
        note: "schema-directed encoding that fails on unsupported values instead of dropping fields",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::JsonDecode,
        module: Module::Json,
        name: "decode",
        leaf: Some("__decode"),
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "input",
                ty: SigType::Str,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Param(0),
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CanonicalFormat,
        note: "schema-directed decoding that rejects duplicate keys and distinguishes missing from null",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::JsonDecodeWithOptions,
        module: Module::Json,
        name: "decode_with_options",
        leaf: Some("__decode_with_options"),
        signature: Signature {
            type_params: &["T"],
            params: &[
                Param {
                    name: "input",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "ignore_unknown_fields",
                    ty: SigType::Bool,
                    has_default: true,
                    pipeline: false,
                },
            ],
            result: SigType::Param(0),
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CanonicalFormat,
        note: "unknown fields are ignored only when the caller opts in explicitly",
        pins: PinRange::CURRENT,
    },
];
