//! `std.ui` descriptors.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const UI: &[Intrinsic] = &[Intrinsic {
    id: IntrinsicId::UiNode,
    module: Module::Ui,
    name: "node",
    leaf: Some("__node"),
    signature: Signature {
        type_params: &["P", "A"],
        params: &[
            Param {
                name: "kind",
                ty: SigType::Str,
                has_default: false,
                pipeline: false,
            },
            Param {
                name: "properties",
                ty: SigType::Param(0),
                has_default: false,
                pipeline: false,
            },
            Param {
                name: "children",
                ty: SigType::List(&SigType::Ui),
                has_default: false,
                pipeline: false,
            },
            Param {
                name: "actions",
                ty: SigType::List(&SigType::Tuple(&[SigType::Str, SigType::Param(1)])),
                has_default: false,
                pipeline: false,
            },
            Param {
                name: "property_types",
                ty: SigType::List(&SigType::Tuple(&[SigType::Str, SigType::Str])),
                has_default: false,
                pipeline: false,
            },
        ],
        result: SigType::Ui,
    },
    effect: Effect::Pure,
    dispatch: Dispatch::Native,
    justification: Justification::RepresentationAccess,
    note: "constructs the core presentation tree without executing an action descriptor",
    pins: PinRange::CURRENT,
}];
