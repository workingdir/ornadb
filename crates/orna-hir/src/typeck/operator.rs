//! Operator protocols (D16 P1-A): an operator whose left operand is a nominal type
//! and whose kernel scalar row does not apply selects an instance of its `core.ops`
//! protocol. The instance's right protocol argument must equal the right operand's
//! type. The selected implementation member is the call the operator becomes.

use orna_syntax::SyntaxSpan;

use crate::diag::GENERIC_OVERLAP;
use crate::{
    Arg, ArgValue, BinOp, Body, Call, Callee, DefId, DefKind, ExprId, ExprKind, HirDiagnostic,
    ModulePath, Name, ParamIdx, Ty, TyKind, UnOp,
};

use super::BodyCheck;

/// The `core.ops` protocol and member that serve one operator.
struct OperatorProtocol {
    symbol: &'static str,
    protocol: &'static str,
    member: &'static str,
}

fn binary_protocol(op: BinOp) -> Option<OperatorProtocol> {
    let (symbol, protocol, member) = match op {
        BinOp::Add => ("+", "Add", "add"),
        BinOp::Sub => ("-", "Sub", "sub"),
        BinOp::Mul => ("*", "Mul", "mul"),
        BinOp::Div => ("/", "Div", "div"),
        BinOp::Rem => ("%", "Rem", "rem"),
        BinOp::Pow => ("^", "Pow", "pow"),
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => return None,
    };
    Some(OperatorProtocol {
        symbol,
        protocol,
        member,
    })
}

/// One instance that can serve an operator: its member and that member's signature.
struct Implementation {
    member: DefId,
    params: Vec<Ty>,
    output: Ty,
    /// The protocol's last type argument, which the member's result must equal.
    protocol_output: Option<Ty>,
}

impl BodyCheck<'_> {
    /// Types `left OP right` through the operator protocol of a nominal left operand.
    ///
    /// `None` means no protocol applies, so the caller keeps the scalar rules.
    /// `Some` is the operator's type, including the error type after a reported
    /// missing or ambiguous instance.
    pub(super) fn binary_operator_ty(
        &mut self,
        body: &mut Body,
        id: ExprId,
        op: BinOp,
        operands: (ExprId, ExprId),
        types: (Ty, Ty),
        span: SyntaxSpan,
    ) -> Option<Ty> {
        let operator = binary_protocol(op)?;
        let (left_expr, right_expr) = operands;
        let (left, right) = types;
        if matches!(self.kind_of(right), Some(TyKind::Infer(_))) {
            return None;
        }
        let owner = self.nominal_owner(left)?;
        let protocol = self.core_ops_protocol(operator.protocol)?;
        let found = self.operator_implementations(owner, protocol, operator.member, Some(right));
        let Some(implementation) =
            self.settle_operator(found, &operator, (left, Some(right)), &span)
        else {
            return Some(self.error());
        };
        self.unify_operands(&[left, right], &implementation.params, &span);
        body.exprs[id].kind = operator_call(implementation.member, &[left_expr, right_expr], span);
        Some(implementation.output)
    }

    /// Types `-operand` through the `Neg` instance of a nominal operand.
    pub(super) fn unary_operator_ty(
        &mut self,
        body: &mut Body,
        id: ExprId,
        op: UnOp,
        operand: ExprId,
        operand_ty: Ty,
        span: SyntaxSpan,
    ) -> Option<Ty> {
        if !matches!(op, UnOp::Neg) {
            return None;
        }
        let owner = self.nominal_owner(operand_ty)?;
        let operator = OperatorProtocol {
            symbol: "-",
            protocol: "Neg",
            member: "neg",
        };
        let protocol = self.core_ops_protocol(operator.protocol)?;
        let found = self.operator_implementations(owner, protocol, operator.member, None);
        let Some(implementation) =
            self.settle_operator(found, &operator, (operand_ty, None), &span)
        else {
            return Some(self.error());
        };
        self.unify_operands(&[operand_ty], &implementation.params, &span);
        body.exprs[id].kind = operator_call(implementation.member, &[operand], span);
        Some(implementation.output)
    }

    /// The nominal owner of a type, when it is a nominal type.
    fn nominal_owner(&self, ty: Ty) -> Option<DefId> {
        match self.kind_of(ty) {
            Some(TyKind::Adt(owner, _)) => Some(owner),
            _ => None,
        }
    }

    /// The `core.ops` protocol called `name`.
    fn core_ops_protocol(&self, name: &str) -> Option<DefId> {
        let path = ModulePath::new([Name::new("core"), Name::new("ops")]);
        let module = self
            .program
            .modules
            .iter()
            .find(|module| module.path == path)?;
        module.exports.get(&Name::new(name)).copied()
    }

    /// The implementations of `protocol` on `owner` that serve the operator.
    ///
    /// `right` filters on the protocol's first type argument, the right operand's type.
    fn operator_implementations(
        &self,
        owner: DefId,
        protocol: DefId,
        member_name: &str,
        right: Option<Ty>,
    ) -> Vec<Implementation> {
        let member_name = Name::new(member_name);
        let implementations = self
            .program
            .instances
            .get(&owner)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut found = Vec::new();
        for implementation in implementations {
            let DefKind::Impl(declared) = &self.program.defs[*implementation].kind else {
                continue;
            };
            if declared.protocol != Some(protocol) {
                continue;
            }
            if let Some(right) = right
                && declared.protocol_args.first() != Some(&right)
            {
                continue;
            }
            let Some(member) = declared
                .members
                .iter()
                .copied()
                .find(|member| self.program.defs[*member].name == member_name)
            else {
                continue;
            };
            let DefKind::Fn(function) = &self.program.defs[member].kind else {
                continue;
            };
            found.push(Implementation {
                member,
                params: function.params.iter().map(|param| param.ty).collect(),
                output: function.ret,
                protocol_output: declared.protocol_args.last().copied(),
            });
        }
        found
    }

    /// Picks the single implementation an operator uses, reporting a missing or ambiguous one.
    fn settle_operator(
        &mut self,
        mut found: Vec<Implementation>,
        operator: &OperatorProtocol,
        operands: (Ty, Option<Ty>),
        span: &SyntaxSpan,
    ) -> Option<Implementation> {
        let (left, right) = operands;
        match found.len() {
            0 => {
                let shown = match right {
                    Some(right) => format!(
                        "{} and {}",
                        crate::print::print_type(self.program, left),
                        crate::print::print_type(self.program, right),
                    ),
                    None => crate::print::print_type(self.program, left),
                };
                self.conflict(
                    format!(
                        "operator `{}` is not defined for {shown}: no `{}` instance takes that operand",
                        operator.symbol, operator.protocol,
                    ),
                    span.clone(),
                );
                None
            }
            1 => {
                let implementation = found.remove(0);
                if let Some(expected) = implementation.protocol_output {
                    self.unify(implementation.output, expected, span.clone());
                }
                Some(implementation)
            }
            _ => {
                self.conflicts += 1;
                self.diagnostics.push(HirDiagnostic::new(
                    GENERIC_OVERLAP,
                    format!(
                        "two `{}` instances supply the operator `{}` on {}",
                        operator.protocol,
                        operator.symbol,
                        crate::print::print_type(self.program, left),
                    ),
                    span.clone(),
                ));
                None
            }
        }
    }

    /// Unifies each operand with the parameter it binds.
    fn unify_operands(&mut self, operands: &[Ty], params: &[Ty], span: &SyntaxSpan) {
        for (operand, param) in operands.iter().zip(params) {
            self.unify(*operand, *param, span.clone());
        }
    }
}

/// The source call an operator becomes once its implementation is selected.
fn operator_call(member: DefId, operands: &[ExprId], span: SyntaxSpan) -> ExprKind {
    let args = operands
        .iter()
        .enumerate()
        .map(|(slot, operand)| Arg {
            slot: ParamIdx::new(slot as u32),
            value: ArgValue::Expr(*operand),
        })
        .collect();
    ExprKind::Call(Box::new(Call {
        callee: Callee::Source {
            def: member,
            targs: Box::new([]),
        },
        callee_span: span,
        args,
        piped: false,
    }))
}
