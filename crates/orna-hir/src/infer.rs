//! Inference variables and first-order unification (design section 2.3).
//!
//! [`InferCtxt`] is the union-find table of [`TyVar`] a body's type pass uses.
//! A variable is created for a construct whose type source does not write
//! (the empty list, `null`, an unannotated lambda parameter) and solved by
//! unifying the types the surrounding expressions impose on it.
//!
//! The solver is deliberately first-order and local to one body: decision D1
//! says full Hindley-Milner generalisation is not part of the language, and
//! section 2.3 says a variable that is still unsolved after a body is checked
//! is an ambiguity diagnostic rather than a silent `Any`. There is therefore no
//! generalisation level to track: every variable created while checking one
//! body lives at the same level.
//!
//! A `TyKind::Infer` never survives a successful body check; [`InferCtxt::zonk`]
//! replaces each solved variable with its solution and the type pass reports and
//! rejects every variable that is still unbound.

use orna_syntax::SyntaxSpan;

use crate::{Ty, TyInterner, TyKind, TyVar};

/// Two types that cannot be made equal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conflict {
    /// The left type, as the solver saw it.
    pub left: Ty,
    /// The right type, as the solver saw it.
    pub right: Ty,
}

/// One inference variable's state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VarState {
    /// Not solved yet.
    Unbound,
    /// Solved to a type.
    Bound(Ty),
}

/// The inference table for one body.
#[derive(Clone, Debug, Default)]
pub struct InferCtxt {
    vars: Vec<VarState>,
    spans: Vec<SyntaxSpan>,
}

impl InferCtxt {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates one unsolved variable and records where it came from.
    ///
    /// The span is the expression that introduced the variable; the ambiguity
    /// diagnostic cites it (ORNA-INFER-007, ORNA-INFER-011).
    pub fn new_var(&mut self, types: &mut TyInterner, span: SyntaxSpan) -> Ty {
        let var = TyVar::new(u32::try_from(self.vars.len()).expect("inference table fits u32"));
        self.vars.push(VarState::Unbound);
        self.spans.push(span);
        types.intern(TyKind::Infer(var))
    }

    /// The span that introduced `var`.
    #[must_use]
    pub fn span(&self, var: TyVar) -> Option<&SyntaxSpan> {
        self.spans.get(var.as_u32() as usize)
    }

    /// Follows solved variables one step at a time until a variable or a
    /// non-variable type is reached.
    #[must_use]
    pub fn resolve(&self, types: &TyInterner, ty: Ty) -> Ty {
        let mut current = ty;
        loop {
            match types.kind(current) {
                Some(TyKind::Infer(var)) => match self.vars.get(var.as_u32() as usize) {
                    Some(VarState::Bound(next)) => current = *next,
                    _ => return current,
                },
                _ => return current,
            }
        }
    }

    /// Unifies `left` with `right`.
    ///
    /// `TyKind::Error` and `TyKind::Never` unify with every type: the first
    /// records that a diagnostic was already emitted, the second is the bottom
    /// type of an expression that cannot complete. Neither binding hides a
    /// diagnosis the caller did not already report.
    pub fn unify(&mut self, types: &mut TyInterner, left: Ty, right: Ty) -> Result<(), Conflict> {
        let left = self.resolve(types, left);
        let right = self.resolve(types, right);
        if left == right {
            return Ok(());
        }
        match (types.kind(left).cloned(), types.kind(right).cloned()) {
            (Some(TyKind::Error), _) | (_, Some(TyKind::Error)) => Ok(()),
            (Some(TyKind::Never), _) | (_, Some(TyKind::Never)) => Ok(()),
            (Some(TyKind::Infer(var)), _) => self.bind(types, var, right),
            (_, Some(TyKind::Infer(var))) => self.bind(types, var, left),
            (Some(TyKind::Option(left_inner)), Some(TyKind::Option(right_inner))) => {
                self.unify(types, left_inner, right_inner)
            }
            (Some(TyKind::List(left_inner)), Some(TyKind::List(right_inner)))
            | (Some(TyKind::Set(left_inner)), Some(TyKind::Set(right_inner)))
            | (Some(TyKind::Range(left_inner)), Some(TyKind::Range(right_inner)))
            | (Some(TyKind::Relation(left_inner)), Some(TyKind::Relation(right_inner)))
            | (Some(TyKind::Stream(left_inner)), Some(TyKind::Stream(right_inner))) => {
                self.unify(types, left_inner, right_inner)
            }
            (
                Some(TyKind::Map(left_key, left_value)),
                Some(TyKind::Map(right_key, right_value)),
            ) => {
                self.unify(types, left_key, right_key)?;
                self.unify(types, left_value, right_value)
            }
            (Some(TyKind::Tuple(left_elements)), Some(TyKind::Tuple(right_elements))) => {
                if left_elements.len() != right_elements.len() {
                    return Err(Conflict { left, right });
                }
                for (left_element, right_element) in left_elements.iter().zip(right_elements.iter())
                {
                    self.unify(types, *left_element, *right_element)?;
                }
                Ok(())
            }
            (Some(TyKind::Record(left_fields)), Some(TyKind::Record(right_fields))) => {
                if left_fields.len() != right_fields.len() {
                    return Err(Conflict { left, right });
                }
                for ((left_name, left_ty), (right_name, right_ty)) in
                    left_fields.iter().zip(right_fields.iter())
                {
                    if left_name != right_name {
                        return Err(Conflict { left, right });
                    }
                    self.unify(types, *left_ty, *right_ty)?;
                }
                Ok(())
            }
            (Some(TyKind::Adt(left_def, left_args)), Some(TyKind::Adt(right_def, right_args))) => {
                if left_def != right_def || left_args.len() != right_args.len() {
                    return Err(Conflict { left, right });
                }
                for (left_arg, right_arg) in left_args.iter().zip(right_args.iter()) {
                    self.unify(types, *left_arg, *right_arg)?;
                }
                Ok(())
            }
            (Some(TyKind::Fn(left_fn)), Some(TyKind::Fn(right_fn))) => {
                if left_fn.params.len() != right_fn.params.len() {
                    return Err(Conflict { left, right });
                }
                for (left_param, right_param) in left_fn.params.iter().zip(right_fn.params.iter()) {
                    self.unify(types, *left_param, *right_param)?;
                }
                self.unify(types, left_fn.ret, right_fn.ret)
            }
            _ => Err(Conflict { left, right }),
        }
    }

    /// Binds `var` to `ty`, rejecting a cyclic solution.
    fn bind(&mut self, types: &mut TyInterner, var: TyVar, ty: Ty) -> Result<(), Conflict> {
        if self.occurs(types, var, ty) {
            return Err(Conflict {
                left: types.intern(TyKind::Infer(var)),
                right: ty,
            });
        }
        let index = var.as_u32() as usize;
        if let Some(state) = self.vars.get_mut(index) {
            *state = VarState::Bound(ty);
        }
        Ok(())
    }

    /// Whether `var` occurs in `ty`.
    fn occurs(&self, types: &TyInterner, var: TyVar, ty: Ty) -> bool {
        let ty = self.resolve(types, ty);
        match types.kind(ty) {
            Some(TyKind::Infer(other)) => *other == var,
            Some(TyKind::Option(inner))
            | Some(TyKind::List(inner))
            | Some(TyKind::Set(inner))
            | Some(TyKind::Range(inner))
            | Some(TyKind::Relation(inner))
            | Some(TyKind::Stream(inner)) => self.occurs(types, var, *inner),
            Some(TyKind::Map(key, value)) => {
                self.occurs(types, var, *key) || self.occurs(types, var, *value)
            }
            Some(TyKind::Tuple(elements)) => elements
                .iter()
                .any(|element| self.occurs(types, var, *element)),
            Some(TyKind::Record(fields)) => fields
                .iter()
                .any(|(_, field)| self.occurs(types, var, *field)),
            Some(TyKind::Adt(_, arguments)) => arguments
                .iter()
                .any(|argument| self.occurs(types, var, *argument)),
            Some(TyKind::Fn(function)) => {
                function
                    .params
                    .iter()
                    .any(|parameter| self.occurs(types, var, *parameter))
                    || self.occurs(types, var, function.ret)
            }
            _ => false,
        }
    }

    /// Replaces every solved variable in `ty` with its solution, deeply.
    ///
    /// An unsolved variable stays an [`TyKind::Infer`]; the caller reports it
    /// as an ambiguity and rejects the body, so no finished body holds one.
    #[must_use]
    pub fn zonk(&self, types: &mut TyInterner, ty: Ty) -> Ty {
        let ty = self.resolve(types, ty);
        let Some(kind) = types.kind(ty).cloned() else {
            return ty;
        };
        let rebuilt = match kind {
            TyKind::Infer(_)
            | TyKind::Bool
            | TyKind::Int
            | TyKind::Decimal
            | TyKind::Float
            | TyKind::Str
            | TyKind::Blob
            | TyKind::Uuid
            | TyKind::Date
            | TyKind::Instant
            | TyKind::Duration
            | TyKind::Period
            | TyKind::Digest
            | TyKind::Path
            | TyKind::TimeZone
            | TyKind::Locale
            | TyKind::PresentContext
            | TyKind::PresentTree
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Error
            | TyKind::Param(_) => return ty,
            TyKind::Option(inner) => TyKind::Option(self.zonk(types, inner)),
            TyKind::List(inner) => TyKind::List(self.zonk(types, inner)),
            TyKind::Set(inner) => TyKind::Set(self.zonk(types, inner)),
            TyKind::Range(inner) => TyKind::Range(self.zonk(types, inner)),
            TyKind::Relation(inner) => TyKind::Relation(self.zonk(types, inner)),
            TyKind::Stream(inner) => TyKind::Stream(self.zonk(types, inner)),
            TyKind::Map(key, value) => TyKind::Map(self.zonk(types, key), self.zonk(types, value)),
            TyKind::Tuple(elements) => TyKind::Tuple(
                elements
                    .iter()
                    .map(|element| self.zonk(types, *element))
                    .collect(),
            ),
            TyKind::Record(fields) => TyKind::Record(
                fields
                    .iter()
                    .map(|(name, field)| (name.clone(), self.zonk(types, *field)))
                    .collect(),
            ),
            TyKind::Adt(def, arguments) => TyKind::Adt(
                def,
                arguments
                    .iter()
                    .map(|argument| self.zonk(types, *argument))
                    .collect(),
            ),
            TyKind::Fn(function) => TyKind::Fn(crate::FnTy {
                params: function
                    .params
                    .iter()
                    .map(|parameter| self.zonk(types, *parameter))
                    .collect(),
                ret: self.zonk(types, function.ret),
                effects: function.effects,
            }),
            TyKind::Quantity(number, key) => TyKind::Quantity(self.zonk(types, number), key),
        };
        types.intern(rebuilt)
    }

    /// Every variable that is still unsolved, with the span that introduced it.
    #[must_use]
    pub fn unsolved(&self) -> Vec<(TyVar, SyntaxSpan)> {
        self.vars
            .iter()
            .enumerate()
            .filter(|(_, state)| matches!(state, VarState::Unbound))
            .filter_map(|(index, _)| {
                let var = TyVar::new(u32::try_from(index).ok()?);
                Some((var, self.spans.get(index).cloned()?))
            })
            .collect()
    }

    /// Whether every variable is solved.
    #[must_use]
    pub fn is_solved(&self) -> bool {
        self.vars
            .iter()
            .all(|state| matches!(state, VarState::Bound(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::{Conflict, InferCtxt};
    use crate::{ParamIdx, TyInterner, TyKind};
    use orna_syntax::SyntaxSpan;

    fn span() -> SyntaxSpan {
        SyntaxSpan::new(0, 1)
    }

    #[test]
    fn a_variable_takes_the_type_it_is_unified_with() {
        let mut types = TyInterner::new();
        let mut infer = InferCtxt::new();
        let var = infer.new_var(&mut types, span());
        let int = types.intern(TyKind::Int);
        infer.unify(&mut types, var, int).expect("a variable binds");
        assert_eq!(infer.resolve(&types, var), int);
        assert_eq!(infer.zonk(&mut types, var), int);
        assert!(infer.is_solved());
    }

    #[test]
    fn structural_types_unify_through_variables() {
        let mut types = TyInterner::new();
        let mut infer = InferCtxt::new();
        let var = infer.new_var(&mut types, span());
        let int = types.intern(TyKind::Int);
        let str_ty = types.intern(TyKind::Str);
        let list_var = types.intern(TyKind::List(var));
        let list_int = types.intern(TyKind::List(int));
        infer
            .unify(&mut types, list_var, list_int)
            .expect("lists unify by element");
        assert_eq!(infer.resolve(&types, var), int);
        let list_str = types.intern(TyKind::List(str_ty));
        assert_eq!(
            infer.unify(&mut types, list_var, list_str),
            Err(Conflict {
                left: int,
                right: str_ty
            })
        );
    }

    #[test]
    fn a_cyclic_solution_is_rejected() {
        let mut types = TyInterner::new();
        let mut infer = InferCtxt::new();
        let var = infer.new_var(&mut types, span());
        let list = types.intern(TyKind::List(var));
        assert!(infer.unify(&mut types, var, list).is_err());
    }

    #[test]
    fn error_and_never_unify_with_everything() {
        let mut types = TyInterner::new();
        let mut infer = InferCtxt::new();
        let error = types.error();
        let never = types.intern(TyKind::Never);
        let str_ty = types.intern(TyKind::Str);
        assert!(infer.unify(&mut types, error, str_ty).is_ok());
        assert!(infer.unify(&mut types, never, str_ty).is_ok());
        assert!(infer.unify(&mut types, str_ty, never).is_ok());
    }

    #[test]
    fn generic_parameters_unify_only_with_the_same_position() {
        let mut types = TyInterner::new();
        let mut infer = InferCtxt::new();
        let first = types.intern(TyKind::Param(ParamIdx::new(0)));
        let second = types.intern(TyKind::Param(ParamIdx::new(1)));
        assert!(infer.unify(&mut types, first, first).is_ok());
        assert!(infer.unify(&mut types, first, second).is_err());
    }

    #[test]
    fn an_unbound_variable_is_unsolved_and_keeps_its_span() {
        let mut types = TyInterner::new();
        let mut infer = InferCtxt::new();
        let var = infer.new_var(&mut types, SyntaxSpan::new(4, 9));
        assert!(!infer.is_solved());
        let unsolved = infer.unsolved();
        assert_eq!(unsolved.len(), 1);
        assert_eq!(types.kind(var), Some(&TyKind::Infer(unsolved[0].0)));
        assert_eq!(unsolved[0].1, SyntaxSpan::new(4, 9));
    }
}
