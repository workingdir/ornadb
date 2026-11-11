//! `orna-hir`: the typed, resolved intermediate representation for Orna 1.1.0.
//!
//! The crate sits between `orna-syntax` and every consumer of a program: the
//! checker, the executor, the language server and the REPL. Its job, per
//! `docs/design/hir.md`, is to resolve every name exactly once and to give the
//! later layers a program to walk rather than a syntax tree and a spelling.
//!
//! Stage H0 supplies the data model. Stage H1 adds lowering: the resolver for
//! names, imports, locals, slots and patterns (design section 2.2), the one
//! argument binder (section 2.7), `Callee` classification for every call
//! (section 2.6), the per-module [`NameUse`] index, and the item and body
//! lowering behind the same total [`lower`] entry point.
//!
//! Stage H2a adds the type pass: one interned [`Ty`], the first-order
//! [`InferCtxt`] local to a body, operator tables over types, expression
//! typing for fully annotated programs, call checking per [`Callee`] (a
//! `Pending` member call is settled from the receiver's type), and diagnostics
//! with spans under the ORNA-INFER codes. A construct outside the subset is a
//! [`NotYetReason`] rather than a program error. Effect inference arrives in
//! H3, and the `core`/`sys` native declarations needed to type `core` and `sys`
//! calls arrive with H2b/H5, so those calls stay "not yet on HIR" here.
//!
//! The crate depends on `orna-syntax` (input), `orna-value` (the portable
//! diagnostic the admission boundary converts to), `num-bigint` (literal
//! payloads) and `unicode-normalization` (identifier NFC). It must never depend
//! on `orna-check`, `orna-evaluator`, `orna-intrinsics`, `orna-sys` or a
//! storage crate; `scripts/check_architecture.py` enforces the layer table.

#![forbid(unsafe_code)]

mod body;
mod call;
mod def;
pub mod diag;
mod effect_pass;
mod effects;
mod embedded;
mod ids;
pub mod infer;
mod lower;
mod name_use;
pub mod ops;
mod pat;
pub mod print;
mod program;
mod resolve;
mod rules;
mod ty;
pub mod typeck;
mod units;
mod validate;

pub use body::{
    Arm, AssignOp, BinOp, Body, Capture, Expr, ExprKind, FieldRef, InterpPart, Lit, Local, LogicOp,
    Place, Stmt, UnOp,
};
pub use call::{Arg, ArgValue, Call, Callee, IntrinsicForm, RelOp, TableOp};
pub use def::{
    AssertionDef, Bound, Def, DefKind, DimensionDef, EnumDef, FieldDef, FieldInitializer, FnBody,
    FnDef, GenericParam, ImplDef, LetDef, NativeAbi, NativeBinding, NativeDecl, NativeKey,
    ParamDef, ProtocolDef, TableDef, TypeDef, UnitDef, UnitFactor, VariantDef, Visibility,
};
pub use diag::HirDiagnostic;
pub use effects::{EffectKind, EffectRow, EffectSet, EffectVar};
pub use embedded::ENGINE_NATIVE_ABI;
pub use ids::{
    BodyId, DefId, ExprId, Idx, IndexVec, LocalId, ModuleId, ModulePath, Name, PatId, SourceId,
};
pub use infer::{Conflict, InferCtxt};
pub use lower::{SourceUnit, lower};
pub use name_use::{NameTarget, NameUse, UseContext};
pub use pat::Pat;
pub use print::{print_signature, print_type};
pub use program::{Import, ImportTarget, Module, ModuleOrigin, Program};
pub use ty::{FnTy, ParamIdx, Ty, TyInterner, TyKind, TyVar};
pub use typeck::{BodyOutcome, NotYetReason, TypeCheckSummary, typeck};
pub use validate::ValidationError;
