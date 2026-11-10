//! Call classification and the one argument binder (design sections 2.6, 2.7).
//!
//! Every path callee is classified once, here, from the resolved definition or
//! from the language-defined form its spelling names. Later layers read
//! [`crate::Callee`] and never the spelling again.
//!
//! Arguments are bound to parameter slots once, during lowering. The binder
//! applies positional-then-named rules, puts a piped input in parameter 0, and
//! emits one [`crate::Arg`] per parameter in parameter order, with
//! [`crate::ArgValue::Default`] where the declaration supplies a default.

use std::collections::BTreeMap;

use orna_syntax::SyntaxSpan;

use crate::{
    Arg, ArgValue, Callee, DefId, DefKind, ExprId, Idx, IntrinsicForm, ModuleId, ModuleOrigin,
    Name, ParamIdx, RelOp, TableOp,
};

use super::declare::{Builder, PendingKind};

/// The language-defined form a root spelling names, if any.
///
/// The four declarable forms (`Some`, `now`, `uuid7`, `error`) become
/// [`Callee::Native`] when `core.orna` lands with the `native fn` production
/// (design section 4.2); until then the classification is
/// [`Callee::Intrinsic`], whose map to the old evaluator's root-intrinsic route
/// is the same.
pub(crate) fn intrinsic_form(name: &str) -> Option<IntrinsicForm> {
    Some(match name {
        "fail" => IntrinsicForm::Fail,
        "parallel" => IntrinsicForm::Parallel,
        "race" => IntrinsicForm::Race,
        "timeout" => IntrinsicForm::Timeout,
        "merge_streams" => IntrinsicForm::MergeStreams,
        "Some" => IntrinsicForm::OptionSome,
        "now" => IntrinsicForm::Now,
        "uuid7" => IntrinsicForm::Uuid7,
        "error" => IntrinsicForm::Error,
        _ => return None,
    })
}

/// The table operation a member name denotes.
pub(crate) fn table_op(name: &str) -> Option<TableOp> {
    Some(match name {
        "insert" => TableOp::Insert,
        "upsert" => TableOp::Upsert,
        "update" => TableOp::Update,
        "delete" => TableOp::Delete,
        "rekey" => TableOp::Rekey,
        "count" => TableOp::Count,
        "first" => TableOp::First,
        "one" => TableOp::One,
        "as_of" => TableOp::AsOf,
        _ => return None,
    })
}

/// The relation operator a pipeline-stage name denotes.
pub(crate) fn rel_op(name: &str) -> Option<RelOp> {
    Some(match name {
        "filter" => RelOp::Filter,
        "map" => RelOp::Map,
        "sort_by" => RelOp::SortBy,
        "bucket_by" => RelOp::BucketBy,
        "distinct" => RelOp::Distinct,
        "pairs" => RelOp::Pairs,
        "window" => RelOp::Window,
        "take" => RelOp::Take,
        "drop" => RelOp::Drop,
        "flat_map" => RelOp::FlatMap,
        "project" => RelOp::Project,
        "unique" => RelOp::Unique,
        "union" => RelOp::Union,
        "zip" => RelOp::Zip,
        "zip_exact" => RelOp::ZipExact,
        "asof_join" => RelOp::AsOfJoin,
        "count" => RelOp::Count,
        "first" => RelOp::First,
        "last" => RelOp::Last,
        "one" => RelOp::One,
        "every" => RelOp::Every,
        "exists" => RelOp::Exists,
        "all_unique" => RelOp::AllUnique,
        "sum" => RelOp::Sum,
        "min" => RelOp::Min,
        "max" => RelOp::Max,
        _ => return None,
    })
}

/// One parameter as the binder needs it.
#[derive(Clone, Debug)]
pub(crate) struct ParamInfo {
    /// The declared name, used by named arguments.
    pub(crate) name: Name,
    /// Whether the declaration supplies a default.
    pub(crate) has_default: bool,
}

/// The classification of one written callee.
pub(crate) enum Classified {
    /// A definition with a known parameter list, or a constructor's fields.
    Declared {
        /// The classified callee.
        callee: Callee,
        /// Its parameters, in order.
        params: Vec<ParamInfo>,
    },
    /// A form with no single declaration: a first-class value, an intrinsic,
    /// a relation operator, a pending member, a table operation, or a
    /// constructor whose fields are not a declaration's parameters.
    Open {
        /// The classified callee.
        callee: Callee,
    },
    /// The definition exists but cannot be called.
    NotCallable,
}

impl Builder<'_> {
    /// Classifies one resolved definition.
    pub(crate) fn classify_def(&self, def: DefId) -> Classified {
        let module = self.pending[def.index()].module;
        let origin = self.program.modules[module].origin;
        match &self.pending[def.index()].kind {
            PendingKind::Function(function) => Classified::Declared {
                callee: match (origin, function.native.is_some()) {
                    (ModuleOrigin::Core, true) => Callee::Native {
                        def,
                        targs: Box::new([]),
                    },
                    (ModuleOrigin::Sys, true) => Callee::Sys {
                        def,
                        targs: Box::new([]),
                    },
                    _ => Callee::Source {
                        def,
                        targs: Box::new([]),
                    },
                },
                params: function
                    .params
                    .iter()
                    .map(|param| ParamInfo {
                        name: param.name.clone(),
                        has_default: param.default.is_some(),
                    })
                    .collect(),
            },
            PendingKind::Built(DefKind::Fn(function)) => Classified::Declared {
                callee: match &function.body {
                    crate::FnBody::Native(_) => Callee::Native {
                        def,
                        targs: Box::new([]),
                    },
                    _ => Callee::Source {
                        def,
                        targs: Box::new([]),
                    },
                },
                params: function
                    .params
                    .iter()
                    .map(|param| ParamInfo {
                        name: param.name.clone(),
                        has_default: param.default.is_some(),
                    })
                    .collect(),
            },
            PendingKind::Built(DefKind::Variant(variant)) => Classified::Declared {
                callee: Callee::Ctor { def },
                params: variant
                    .fields
                    .iter()
                    .map(|field| ParamInfo {
                        name: self.def_name(*field).clone(),
                        has_default: false,
                    })
                    .collect(),
            },
            PendingKind::Built(DefKind::Type(_) | DefKind::Enum(_) | DefKind::Table(_)) => {
                Classified::Open {
                    callee: Callee::Ctor { def },
                }
            }
            PendingKind::Built(DefKind::Unit(_) | DefKind::Dimension(_)) => Classified::NotCallable,
            PendingKind::Built(_) | PendingKind::Unbuilt(_) => Classified::NotCallable,
        }
    }

    /// Binds already-lowered arguments to parameter slots.
    ///
    /// Returns one [`Arg`] per parameter in parameter order and whether the
    /// input was piped into slot 0. `placeholder` is the expression a missing
    /// required argument binds, after the omission is diagnosed.
    pub(crate) fn bind_arguments(
        &mut self,
        module: ModuleId,
        params: Option<&[ParamInfo]>,
        arguments: &[(Option<Name>, ExprId, SyntaxSpan)],
        input: Option<ExprId>,
        placeholder: ExprId,
        span: &SyntaxSpan,
    ) -> (Vec<Arg>, bool) {
        let mut filled: BTreeMap<usize, ArgValue> = BTreeMap::new();
        let mut piped = false;
        let mut next_positional = 0usize;
        let mut named_seen = false;
        if let Some(input) = input {
            filled.insert(0, ArgValue::Expr(input));
            next_positional = 1;
            piped = true;
        }
        for (name, value, argument_span) in arguments {
            let slot = match name {
                Some(name) => {
                    named_seen = true;
                    match params
                        .and_then(|params| params.iter().position(|param| param.name == *name))
                    {
                        Some(slot) => slot,
                        None => {
                            self.diagnose(
                                module,
                                crate::diag::ARGUMENT,
                                format!("no parameter is named `{name}`"),
                                argument_span.clone(),
                            );
                            continue;
                        }
                    }
                }
                None => {
                    if named_seen {
                        self.diagnose(
                            module,
                            crate::diag::ARGUMENT,
                            "a positional argument follows a named argument",
                            argument_span.clone(),
                        );
                    }
                    let limit = params.map_or(usize::MAX, <[ParamInfo]>::len);
                    if next_positional >= limit {
                        self.diagnose(
                            module,
                            crate::diag::ARGUMENT,
                            "the call supplies more positional arguments than parameters",
                            argument_span.clone(),
                        );
                        continue;
                    }
                    let slot = next_positional;
                    next_positional += 1;
                    slot
                }
            };
            if filled.contains_key(&slot) {
                self.diagnose(
                    module,
                    crate::diag::ARGUMENT,
                    "the same parameter is supplied twice",
                    argument_span.clone(),
                );
                continue;
            }
            filled.insert(slot, ArgValue::Expr(*value));
        }
        let count = params.map_or_else(
            || filled.keys().next_back().map_or(0, |slot| slot + 1),
            <[ParamInfo]>::len,
        );
        let mut bound = Vec::with_capacity(count);
        for slot in 0..count {
            let value = match filled.remove(&slot) {
                Some(value) => value,
                None => match params {
                    Some(params) if params[slot].has_default => ArgValue::Default,
                    Some(_) => {
                        self.diagnose(
                            module,
                            crate::diag::ARGUMENT,
                            "the call omits a required parameter",
                            span.clone(),
                        );
                        ArgValue::Expr(placeholder)
                    }
                    None => ArgValue::Default,
                },
            };
            bound.push(Arg {
                slot: ParamIdx::new(u32::try_from(slot).expect("slot fits u32")),
                value,
            });
        }
        (bound, piped)
    }
}
