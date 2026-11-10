//! Calls, classified once (design sections 2.6 and 2.7).
//!
//! Lowering turns every call into one [`Callee`]. After lowering, no layer
//! looks at the callee's spelling again: a call is a source body, a native, a
//! `sys` native, a table operation, a relational operator, an intrinsic form,
//! a constructor or a first-class value. Arguments are bound to parameters
//! once, during lowering: [`Arg::slot`] is the parameter position and
//! [`ArgValue::Default`] stands for a default that runs at the call.

use crate::{DefId, ExprId, ParamIdx, Ty};

/// One call.
#[derive(Clone, Debug)]
pub struct Call {
    /// What the call invokes.
    pub callee: Callee,
    /// One entry per parameter, in parameter order.
    pub args: Box<[Arg]>,
    /// Whether argument 0 is the piped value.
    pub piped: bool,
}

/// What a call invokes.
#[derive(Clone, Debug)]
pub enum Callee {
    /// A function with an Orna body.
    Source {
        /// The function.
        def: DefId,
        /// Its generic arguments.
        targs: Box<[Ty]>,
    },
    /// A `native fn` in `core` or the pinned `std`, implemented by the host.
    Native {
        /// The declaration.
        def: DefId,
        /// Its generic arguments.
        targs: Box<[Ty]>,
    },
    /// A `native fn` in `sys.orna`. The declaration and link mechanism are the
    /// same as [`Callee::Native`]; the variant exists because the host
    /// capability and the `sys.EffectKind` gating differ.
    Sys {
        /// The declaration.
        def: DefId,
        /// Its generic arguments.
        targs: Box<[Ty]>,
    },
    /// A table operation.
    TableOp {
        /// The table.
        table: DefId,
        /// The operation.
        op: TableOp,
    },
    /// A relation pipeline stage or relation constructor.
    Relational {
        /// The operator.
        op: RelOp,
    },
    /// A form that cannot be a function because it has a non-call evaluation
    /// rule.
    Intrinsic(IntrinsicForm),
    /// An enum variant, nominal or `Money<C>` constructor.
    Ctor {
        /// The constructed definition.
        def: DefId,
    },
    /// A call of a first-class value: a closure, or a function-typed
    /// parameter or local. The callee is an expression of type `Ty::Fn`.
    Value {
        /// The callee expression.
        callee: ExprId,
    },
}

/// One bound argument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Arg {
    /// The parameter position it binds.
    pub slot: ParamIdx,
    /// The value it supplies.
    pub value: ArgValue,
}

/// The value one argument supplies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgValue {
    /// An expression written at the call site.
    Expr(ExprId),
    /// The parameter's default body.
    Default,
}

/// A table operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableOp {
    /// `insert`.
    Insert,
    /// `upsert`.
    Upsert,
    /// `update`.
    Update,
    /// `delete`.
    Delete,
    /// `rekey`.
    Rekey,
    /// `count`.
    Count,
    /// `first`.
    First,
    /// `one`.
    One,
    /// `as_of`.
    AsOf,
}

/// A relation pipeline stage or relation constructor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelOp {
    /// A source scan.
    SourceScan,
    /// `filter`.
    Filter,
    /// `map`.
    Map,
    /// `sort_by`.
    SortBy,
    /// `bucket_by`.
    BucketBy,
    /// `distinct`.
    Distinct,
    /// `pairs`.
    Pairs,
    /// `window`.
    Window,
    /// `take`.
    Take,
    /// `drop`.
    Drop,
    /// `flat_map`.
    FlatMap,
}

/// A form with a non-call evaluation rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntrinsicForm {
    /// `fail`, which is abrupt.
    Fail,
    /// `parallel`.
    Parallel,
    /// `race`.
    Race,
    /// `timeout`.
    Timeout,
    /// `merge_streams`.
    MergeStreams,
}
