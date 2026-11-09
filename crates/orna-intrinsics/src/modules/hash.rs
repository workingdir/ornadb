//! `std.hash` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const HASH: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::HashSha256,
        module: Module::Hash,
        name: "sha256",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "input",
                ty: SigType::Blob,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Digest,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CanonicalFormat,
        note: "SHA-256 over the complete Blob returning the 32-byte digest value",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::HashSha256Text,
        module: Module::Hash,
        name: "sha256_text",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "input",
                ty: SigType::Str,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Digest,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CanonicalFormat,
        note: "hashes exact UTF-8 bytes without normalisation, case folding or locale conversion",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::HashDomainSha256,
        module: Module::Hash,
        name: "domain_sha256",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[
                Param {
                    name: "domain",
                    ty: SigType::Str,
                    has_default: false,
                    pipeline: true,
                },
                Param {
                    name: "payload",
                    ty: SigType::Blob,
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::Digest,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::CanonicalFormat,
        note: "computes SHA256(ASCII(domain) || 00 || payload) with a nonempty ASCII domain",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::HashToHex,
        module: Module::Hash,
        name: "to_hex",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "digest",
                ty: SigType::Digest,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Str,
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::RepresentationAccess,
        note: "renders a Digest as exactly 64 lowercase hexadecimal characters",
        pins: PinRange::CURRENT,
    },
    Intrinsic {
        id: IntrinsicId::HashFromHex,
        module: Module::Hash,
        name: "from_hex",
        leaf: None,
        signature: Signature {
            type_params: &[],
            params: &[Param {
                name: "value",
                ty: SigType::Str,
                has_default: false,
                pipeline: true,
            }],
            result: SigType::Option(&SigType::Digest),
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::RepresentationAccess,
        note: "parses exactly 64 lowercase hexadecimal characters and returns null otherwise",
        pins: PinRange::CURRENT,
    },
];
