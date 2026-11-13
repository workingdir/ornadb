//! Static checks of table writes: `insert`, `upsert`, `update` and `rekey`.
//!
//! The rules are the ones the old checker reports on a write's row before any
//! store is reached, and they keep its codes: a required field left out of an
//! insert (E2002), a row field the table does not declare (E2003), a computed
//! field written directly (E3010 on insert, E3011 on update), a primary key
//! changed by an update (E3002), and an explicit rekey of an automatic key
//! (E3013).

use orna_syntax_ast::SourceSpan as SyntaxSpan;

use super::{BodyCheck, NotYetReason};
use crate::{
    ArgValue, Body, Call, DefId, DefKind, ExprKind, FieldInitializer, HirDiagnostic, Name, TableOp,
    Ty,
};

/// A required field is missing from the row of an insert, or a key field from an upsert.
pub(crate) const MISSING_FIELD: &str = "E2002";
/// A row names a field the table does not declare.
pub(crate) const UNKNOWN_FIELD: &str = "E2003";
/// An update supplies a primary key; keys change only through `rekey`.
pub(crate) const KEY_IMMUTABLE: &str = "E3002";
/// An insert or upsert supplies a computed field.
pub(crate) const COMPUTED_INSERT: &str = "E3010";
/// An update supplies a computed field.
pub(crate) const COMPUTED_UPDATE: &str = "E3011";
/// A table with an automatic key is explicitly re-keyed.
pub(crate) const AUTOMATIC_KEY_REKEY: &str = "E3013";

/// One declared field of a table, as the write rules read it.
struct RowField {
    def: DefId,
    name: Name,
    initializer: Option<FieldInitializer>,
    key: bool,
}

impl BodyCheck<'_> {
    /// Checks the row argument at `slot` of an `insert`, `upsert` or `update`
    /// on `table`, whose row type is `row`.
    ///
    /// A row written as a record literal is checked field by field against
    /// the table's declared fields, so each rule names the fields it concerns.
    /// Any other row is typed as one value of the row type.
    pub(super) fn check_table_row(
        &mut self,
        body: &mut Body,
        call: &Call,
        slot: usize,
        table: DefId,
        row: Ty,
        op: TableOp,
    ) -> Result<(), NotYetReason> {
        let Some(argument) = call.args.get(slot) else {
            return Ok(());
        };
        let ArgValue::Expr(id) = argument.value else {
            return Ok(());
        };
        let ExprKind::Record(written) = body.exprs[id].kind.clone() else {
            self.infer_expr(body, id, Some(row))?;
            return Ok(());
        };
        let span = body.exprs[id].span.clone();
        let declared = self.table_row_fields(table);
        let mut supplied = Vec::with_capacity(written.len());
        for (name, value) in written.iter() {
            match declared.iter().find(|field| field.name == *name) {
                Some(field) => {
                    let ty = self.field_definition_ty(field.def)?;
                    self.infer_expr(body, *value, Some(ty))?;
                    supplied.push(field.def);
                }
                None => {
                    self.infer_expr(body, *value, None)?;
                    self.table_write_error(
                        UNKNOWN_FIELD,
                        format!("table write contains an unknown field `{}`", name.as_str()),
                        span.clone(),
                    );
                }
            }
        }
        let is_supplied = |def: DefId| supplied.contains(&def);
        match op {
            TableOp::Insert => {
                for field in &declared {
                    if field.initializer.is_none() && !is_supplied(field.def) {
                        self.missing_field_error(
                            field,
                            "table insertion omits a required field",
                            &span,
                        );
                    }
                }
            }
            TableOp::Upsert => {
                for field in declared.iter().filter(|field| field.key) {
                    if !is_supplied(field.def) {
                        self.missing_field_error(
                            field,
                            "table upsert omits a required primary key field",
                            &span,
                        );
                    }
                }
            }
            _ => {}
        }
        for field in &declared {
            let computed = matches!(field.initializer, Some(FieldInitializer::Computed(_)));
            if !computed || !is_supplied(field.def) {
                continue;
            }
            if op == TableOp::Update {
                self.table_write_error(
                    COMPUTED_UPDATE,
                    format!(
                        "table update cannot change the computed field `{}`",
                        field.name.as_str()
                    ),
                    span.clone(),
                );
            } else {
                self.table_write_error(
                    COMPUTED_INSERT,
                    format!(
                        "table insertion cannot supply the computed field `{}`",
                        field.name.as_str()
                    ),
                    span.clone(),
                );
            }
        }
        if op == TableOp::Update
            && declared
                .iter()
                .any(|field| field.key && is_supplied(field.def))
        {
            self.table_write_error(
                KEY_IMMUTABLE,
                "table update cannot change a primary key; use rekey",
                span.clone(),
            );
        }
        body.exprs[id].ty = row;
        Ok(())
    }

    /// Checks `rekey` on `table`. A table with an automatic key cannot be
    /// re-keyed (E3013); the rest of its rekey typing is not on HIR yet.
    pub(super) fn check_rekey(
        &mut self,
        body: &mut Body,
        call: &Call,
        table: DefId,
        span: SyntaxSpan,
    ) -> Result<(), NotYetReason> {
        if !self.table_is_automatic(table) {
            return Err(NotYetReason::Unsupported(
                "`rekey` is not on HIR yet".into(),
            ));
        }
        for argument in call.args.iter() {
            if let ArgValue::Expr(id) = argument.value {
                self.infer_expr(body, id, None)?;
            }
        }
        self.table_write_error(
            AUTOMATIC_KEY_REKEY,
            "an automatic-key table cannot be explicitly re-keyed",
            span,
        );
        Ok(())
    }

    fn missing_field_error(&mut self, field: &RowField, message: &str, span: &SyntaxSpan) {
        self.table_write_error(
            MISSING_FIELD,
            format!("{message} `{}`", field.name.as_str()),
            span.clone(),
        );
    }

    /// Reports one table-write rule violation. The same rule at the same span
    /// is reported once.
    fn table_write_error(
        &mut self,
        code: &'static str,
        message: impl Into<Box<str>>,
        span: SyntaxSpan,
    ) {
        let diagnostic = HirDiagnostic::new(code, message, span);
        if self.diagnostics.contains(&diagnostic) {
            return;
        }
        self.conflicts += 1;
        self.diagnostics.push(diagnostic);
    }

    /// The declared fields of `table`, in declaration order.
    fn table_row_fields(&self, table: DefId) -> Vec<RowField> {
        let Some(DefKind::Table(definition)) = self.program.defs.get(table).map(|def| &def.kind)
        else {
            return Vec::new();
        };
        definition
            .fields
            .iter()
            .filter_map(|&field| {
                let def = self.program.defs.get(field)?;
                let DefKind::Field(declared) = &def.kind else {
                    return None;
                };
                Some(RowField {
                    def: field,
                    name: def.name.clone(),
                    initializer: declared.initializer,
                    key: definition.key.contains(&field),
                })
            })
            .collect()
    }

    /// Whether `table` has no declared key, so the store allocates it.
    fn table_is_automatic(&self, table: DefId) -> bool {
        matches!(
            self.program.defs.get(table).map(|def| &def.kind),
            Some(DefKind::Table(definition)) if definition.key.is_empty()
        )
    }
}
