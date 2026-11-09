//! `std.encoding.base64` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const BASE64: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::Base64Encode,
        module: Module::Base64,
        name: "encode",
        leaf: Some("__encode"),
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "input",
                ty: SigType::Blob,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Str,
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CanonicalFormat,
        note: "RFC 4648 standard alphabet with required padding and no inserted whitespace",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::Base64Decode,
        module: Module::Base64,
        name: "decode",
        leaf: Some("__decode"),
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "input",
                ty: SigType::Str,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Blob,
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CanonicalFormat,
        note: "rejects invalid characters, noncanonical padding and nonzero unused trailing bits",
        pins: PinRange::CURRENT,
    },
];
