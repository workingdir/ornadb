//! `std.encoding.ovb` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const OVB_CODEC: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::OvbCodecEncode,
        module: Module::OvbCodec,
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
            result: SigType::Blob,
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CanonicalFormat,
        note: "OVB-1 uses shortest heads, definite lengths and deterministic map-key order",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::OvbCodecDecode,
        module: Module::OvbCodec,
        name: "decode",
        leaf: Some("__decode"),
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "input",
                ty: SigType::Blob,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Param(0),
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CanonicalFormat,
        note: "strict OVB-1 decoding rejects duplicate keys, indefinite items and noncanonical aliases",
        pins: PinRange::CURRENT,
    },
];
