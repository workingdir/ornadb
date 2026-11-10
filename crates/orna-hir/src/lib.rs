//! `orna-hir`: the typed, resolved intermediate representation for Orna 1.1.0.
//!
//! The crate sits between `orna-syntax` and every consumer of a program: the
//! checker, the executor, the language server and the REPL. Its job, per
//! `docs/design/hir.md`, is to resolve every name exactly once and to give the
//! later layers a program to walk rather than a syntax tree and a spelling.
//!
//! Stage H0 supplies the data model and nothing else: the ids and tables
//! (design section 2.2), the definition, body, pattern and call shapes
//! (sections 2.4, 2.6, 2.7), one interned type and the effect vocabulary
//! (sections 2.3, 2.5), the total [`lower`] entry point, and
//! [`Program::validate`], the structural invariant check. Lowering has no
//! passes yet, so it returns an empty program; the resolver, the inference
//! pass and the executor arrive in H1 to H4 behind the same types.
//!
//! The crate depends only on `orna-syntax` (input), `orna-value` (the
//! diagnostic type) and `unicode-normalization` (identifier NFC). It must never
//! depend on `orna-check`, `orna-evaluator`, `orna-intrinsics`, `orna-sys` or a
//! storage crate; `scripts/check_architecture.py` enforces the layer table.

#![forbid(unsafe_code)]

mod body;
mod call;
mod def;
mod effects;
mod ids;
mod lower;
mod pat;
mod program;
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
pub use effects::{EffectKind, EffectRow, EffectSet, EffectVar};
pub use ids::{
    BodyId, DefId, ExprId, Idx, IndexVec, LocalId, ModuleId, ModulePath, Name, PatId, SourceId,
};
pub use lower::{SourceUnit, lower};
pub use pat::Pat;
pub use program::{Import, ImportTarget, Module, ModuleOrigin, Program};
pub use ty::{FnTy, ParamIdx, Ty, TyInterner, TyKind, TyVar};
pub use validate::ValidationError;
