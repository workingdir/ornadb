//! Bare sibling field names inside a table field's initializer body.
//!
//! A field initializer is written in the row it belongs to: `full_name: Str => "{first} {last}"` names the
//! sibling fields `first` and `last` directly. The body's `self` slot is that row, so a bare sibling name
//! lowers to a field selection on it, exactly as `self.first` would.

use orna_syntax::SyntaxSpan;

use crate::{
    DefId, DefKind, ExprId, ExprKind, FieldRef, Idx, LocalId, Name, NameTarget, Ty, UseContext,
};

use super::BodyState;
use super::declare::{Builder, PendingKind};
use super::exprs::push_typed;

/// The row a field initializer body reads: its `self` slot and the table that owns the field.
#[derive(Clone, Copy)]
pub(crate) struct RowScope {
    /// The `self` slot, which holds the row.
    pub(crate) slot: LocalId,
    /// The table whose fields are in scope by bare name.
    pub(crate) table: DefId,
}

/// The sibling field of `table` named `name` and its declared type, when there is one.
fn sibling_field(builder: &Builder<'_>, table: DefId, name: &Name) -> Option<(DefId, Ty)> {
    let PendingKind::Built(DefKind::Table(table)) = &builder.pending[table.index()].kind else {
        return None;
    };
    let field = table
        .fields
        .iter()
        .copied()
        .find(|field| builder.pending[field.index()].name == *name)?;
    match &builder.pending[field.index()].kind {
        PendingKind::Built(DefKind::Field(definition)) => Some((field, definition.ty)),
        _ => None,
    }
}

/// Lowers `name` as a selection on the row when the owning table declares a field of that name. Returns
/// `None` when the body has no row scope or no sibling field matches, so the caller resolves the name
/// as usual.
pub(crate) fn lower_sibling_field(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    name: &Name,
    span: &SyntaxSpan,
) -> Option<ExprId> {
    let row = state.row?;
    let (field, ty) = sibling_field(builder, row.table, name)?;
    let row_ty = state.locals[row.slot].ty;
    let base = push_typed(
        builder,
        state,
        ExprKind::Local(row.slot),
        span.clone(),
        row_ty,
    );
    super::record_body_use(
        &mut builder.program,
        state.module,
        state.id,
        span.clone(),
        name.clone(),
        UseContext::Field,
        NameTarget::Def(field),
    );
    Some(push_typed(
        builder,
        state,
        ExprKind::Field {
            base,
            field: FieldRef::Def(field),
        },
        span.clone(),
        ty,
    ))
}
