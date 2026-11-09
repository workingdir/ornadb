//! Historical-pin-only leaves.
//!
//! These six private names are registered only so the
//! `module-upgrade-std-collection-v4`/`-v5` fixtures keep the behaviour they
//! captured. Nothing in the current pin references them; stage S5 moves the
//! live bindings here and deletes the current arms.

use crate::dispatch::{Dispatch, Justification};
use crate::effect::Effect;
use crate::id::IntrinsicId;
use crate::modules::{Intrinsic, Module};
use crate::pin::PinRange;
use crate::signature::{Param, SigType, Signature};

pub(crate) const RETIRED: &[Intrinsic] = &[
    Intrinsic {
        id: IntrinsicId::CollectionListLength,
        module: Module::Collection,
        name: "__list_length",
        leaf: None,
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "values",
                ty: SigType::List(&SigType::Param(0)),
                has_default: false,
                pipeline: false,
            }],
            result: SigType::Int,
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::HistoricalPin,
        note: "historical v4/v5 list length helper; no current-pin declaration",
        pins: PinRange::HISTORICAL_V4_V5,
    },
    Intrinsic {
        id: IntrinsicId::CollectionListConcat,
        module: Module::Collection,
        name: "__list_concat",
        leaf: None,
        signature: Signature {
            type_params: &["T"],
            params: &[
                Param {
                    name: "left",
                    ty: SigType::List(&SigType::Param(0)),
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "right",
                    ty: SigType::List(&SigType::Param(0)),
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::List(&SigType::Param(0)),
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::HistoricalPin,
        note: "historical v4/v5 list concatenation helper; no current-pin declaration",
        pins: PinRange::HISTORICAL_V4_V5,
    },
    Intrinsic {
        id: IntrinsicId::CollectionNumericSum,
        module: Module::Collection,
        name: "__numeric_sum",
        leaf: None,
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "values",
                ty: SigType::List(&SigType::Param(0)),
                has_default: false,
                pipeline: false,
            }],
            result: SigType::Param(0),
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::HistoricalPin,
        note: "historical v4/v5 numeric sum helper; no current-pin declaration",
        pins: PinRange::HISTORICAL_V4_V5,
    },
    Intrinsic {
        id: IntrinsicId::CollectionStableSort,
        module: Module::Collection,
        name: "__stable_sort",
        leaf: None,
        signature: Signature {
            type_params: &["T", "K"],
            params: &[
                Param {
                    name: "values",
                    ty: SigType::List(&SigType::Param(0)),
                    has_default: false,
                    pipeline: false,
                },
                Param {
                    name: "key",
                    ty: SigType::Fn(&[SigType::Param(0)], &SigType::Param(1)),
                    has_default: false,
                    pipeline: false,
                },
            ],
            result: SigType::List(&SigType::Param(0)),
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::HistoricalPin,
        note: "historical v4/v5 stable sort helper; no current-pin declaration",
        pins: PinRange::HISTORICAL_V4_V5,
    },
    Intrinsic {
        id: IntrinsicId::CollectionMinimum,
        module: Module::Collection,
        name: "__minimum",
        leaf: None,
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "values",
                ty: SigType::List(&SigType::Param(0)),
                has_default: false,
                pipeline: false,
            }],
            result: SigType::Option(&SigType::Param(0)),
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::HistoricalPin,
        note: "historical v4/v5 minimum helper; no current-pin declaration",
        pins: PinRange::HISTORICAL_V4_V5,
    },
    Intrinsic {
        id: IntrinsicId::CollectionMaximum,
        module: Module::Collection,
        name: "__maximum",
        leaf: None,
        signature: Signature {
            type_params: &["T"],
            params: &[Param {
                name: "values",
                ty: SigType::List(&SigType::Param(0)),
                has_default: false,
                pipeline: false,
            }],
            result: SigType::Option(&SigType::Param(0)),
            arities: &[],
        },
        effect: Effect::Pure,
        dispatch: Dispatch::Native,
        justification: Justification::HistoricalPin,
        note: "historical v4/v5 maximum helper; no current-pin declaration",
        pins: PinRange::HISTORICAL_V4_V5,
    },
];
