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
//! lowering behind the same total [`lower`] entry point. There is still no
//! type inference or effect inference: those arrive in H2 and H3.
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
mod diag;
mod effects;
mod ids;
mod lower;
mod name_use;
mod pat;
mod program;
mod resolve;
mod ty;
mod validate;

pub use body::{
    Arm, AssignOp, BinOp, Body, Capture, Expr, ExprKind, FieldRef, InterpPart, Lit, Local, LogicOp,
    Place, Stmt, UnOp,
};
pub use call::{Arg, ArgValue, Call, Callee, IntrinsicForm, RelOp, TableOp};
pub use def::{
    AssertionDef, Def, DefKind, DimensionDef, EnumDef, FieldDef, FnBody, FnDef, GenericParam,
    ImplDef, LetDef, NativeAbi, NativeDecl, NativeKey, ParamDef, ProtocolDef, TableDef, TypeDef,
    UnitDef, VariantDef, Visibility,
};
pub use diag::HirDiagnostic;
pub use effects::{EffectKind, EffectRow, EffectSet, EffectVar};
pub use ids::{
    BodyId, DefId, ExprId, Idx, IndexVec, LocalId, ModuleId, ModulePath, Name, PatId, SourceId,
};
pub use lower::{SourceUnit, lower};
pub use name_use::{NameTarget, NameUse};
pub use pat::Pat;
pub use program::{Import, ImportTarget, Module, ModuleOrigin, Program};
pub use ty::{FnTy, ParamIdx, Ty, TyInterner, TyKind, TyVar};
pub use validate::ValidationError;
