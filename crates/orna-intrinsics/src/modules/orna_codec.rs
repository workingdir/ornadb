//! `std.encoding.orna` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const ORNA_CODEC: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::OrnaCodecEncode,
        module: Module::OrnaCodec,
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
        note: "canonical Orna text is schema directed and never executes declarations",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::OrnaCodecDecode,
        module: Module::OrnaCodec,
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
        note: "accepts only the data subset for the supplied type and revalidates refinements",
        pins: PinRange::CURRENT,
    },
];
