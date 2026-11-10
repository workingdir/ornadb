//! Patterns to slots (design section 2.4).
//!
//! A pattern binds names to local slots of the body it appears in and resolves
//! constructors to definitions. Every binding in the pattern gets a slot before
//! the pattern is built, so a body can refer to a name a pattern introduced
//! without a second pass.
//!
//! Lowering records the type a binding was written with when it can: `let`
//! annotations and annotated parameters reach [`crate::Local::ty`], so the type
//! pass of stage H2 has an expected type for the initializer without keeping a
//! second, syntax-side annotation table.

use orna_syntax::{Pattern as AstPattern, SyntaxSpan};

use crate::Idx;
use crate::resolve::PathKind;
use crate::{DefId, DefKind, Name, NameTarget, Pat, PatId, Ty, TyKind};

use super::BodyState;
use super::declare::{Builder, PendingKind};
use super::literals::lower_literal;

/// Lowers one pattern with no written type.
pub(crate) fn lower_pattern(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    pattern: &AstPattern,
) -> PatId {
    lower_pattern_with(builder, state, pattern, None)
}

/// Lowers one pattern whose value is written to have type `expected`.
///
/// The expected type is pushed into binding slots and, where the pattern
/// destructures a tuple, list, record or constructor, into the sub-patterns.
pub(crate) fn lower_pattern_with(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    pattern: &AstPattern,
    expected: Option<Ty>,
) -> PatId {
    let module = state.module;
    let error_ty = builder.program.tys.error();
    match pattern {
        AstPattern::Name(text, span) => {
            let name = Name::new(text);
            let ty = expected.unwrap_or(error_ty);
            let local = state.add_local(name.clone(), ty, span.clone());
            super::record_binding_use(
                &mut builder.program,
                module,
                state.id,
                span.clone(),
                name,
                local,
                false,
            );
            state.push_pat(Pat::Bind { local })
        }
        AstPattern::Wildcard(_) => state.push_pat(Pat::Wild),
        AstPattern::Literal { text, kind, span } => match lower_literal(text, *kind) {
            Some(literal) => state.push_pat(Pat::Lit(literal)),
            None => {
                builder.diagnose(
                    module,
                    crate::diag::UNSUPPORTED,
                    "pattern literal has no canonical payload",
                    span.clone(),
                );
                state.push_pat(Pat::Wild)
            }
        },
        AstPattern::Tuple { elements, .. } => {
            let element_types = match expected.and_then(|ty| builder.program.tys.kind(ty).cloned())
            {
                Some(TyKind::Tuple(types)) if types.len() == elements.len() => {
                    types.iter().copied().map(Some).collect::<Vec<_>>()
                }
                _ => vec![None; elements.len()],
            };
            let lowered = elements
                .iter()
                .zip(element_types)
                .map(|(element, ty)| lower_pattern_with(builder, state, element, ty))
                .collect::<Vec<_>>();
            state.push_pat(Pat::Tuple(lowered.into_boxed_slice()))
        }
        AstPattern::List { elements, .. } => {
            let element_ty = match expected.and_then(|ty| builder.program.tys.kind(ty).cloned()) {
                Some(TyKind::List(element)) => Some(element),
                _ => None,
            };
            let prefix = elements
                .iter()
                .map(|element| lower_pattern_with(builder, state, element, element_ty))
                .collect::<Vec<_>>();
            state.push_pat(Pat::List {
                prefix: prefix.into_boxed_slice(),
                rest: None,
                suffix: Box::new([]),
            })
        }
        AstPattern::Record { fields, .. } => {
            let mut lowered = Vec::with_capacity(fields.len());
            for (name, field_pattern, span) in fields {
                let field_ty = expected.and_then(|ty| field_type(builder, ty, name));
                super::record_body_use(
                    &mut builder.program,
                    module,
                    state.id,
                    span.clone(),
                    Name::new(name),
                    crate::UseContext::Field,
                    NameTarget::Unresolved,
                );
                let sub = match field_pattern {
                    Some(field_pattern) => {
                        lower_pattern_with(builder, state, field_pattern, field_ty)
                    }
                    None => {
                        let local = state.add_local(
                            Name::new(name),
                            field_ty.unwrap_or(error_ty),
                            span.clone(),
                        );
                        super::record_binding_use(
                            &mut builder.program,
                            module,
                            state.id,
                            span.clone(),
                            Name::new(name),
                            local,
                            true,
                        );
                        state.push_pat(Pat::Bind { local })
                    }
                };
                lowered.push((Name::new(name), sub));
            }
            lowered.sort_by(|left, right| left.0.cmp(&right.0));
            state.push_pat(Pat::Record(lowered.into_boxed_slice()))
        }
        AstPattern::Constructor {
            path,
            arguments,
            fields,
            span,
        } => lower_constructor(builder, state, path, arguments, fields, span),
    }
}

/// The declared type of a record-pattern field, when the expected type names
/// one.
fn field_type(builder: &Builder<'_>, expected: Ty, name: &str) -> Option<Ty> {
    match builder.program.tys.kind(expected).cloned() {
        Some(TyKind::Record(fields)) => fields
            .iter()
            .find(|(field, _)| field.as_str() == name)
            .map(|(_, ty)| *ty),
        Some(TyKind::Adt(def, _)) => field_definition(builder, def, name),
        _ => None,
    }
}

/// The type of a field definition named `name` on `owner`.
fn field_definition(builder: &Builder<'_>, owner: DefId, name: &str) -> Option<Ty> {
    let field = match &builder.pending[owner.index()].kind {
        PendingKind::Built(DefKind::Table(table)) => table
            .fields
            .iter()
            .copied()
            .find(|field| builder.def_name(*field).as_str() == name),
        PendingKind::Built(DefKind::Type(definition)) => definition
            .members
            .iter()
            .copied()
            .find(|member| builder.def_name(*member).as_str() == name),
        PendingKind::Built(DefKind::Variant(variant)) => variant
            .fields
            .iter()
            .copied()
            .find(|field| builder.def_name(*field).as_str() == name),
        _ => None,
    }?;
    match &builder.pending[field.index()].kind {
        PendingKind::Built(DefKind::Field(definition)) => Some(definition.ty),
        _ => None,
    }
}

/// Lowers a constructor pattern against its resolved definition.
fn lower_constructor(
    builder: &mut Builder<'_>,
    state: &mut BodyState,
    path: &[orna_syntax::NameSegment],
    arguments: &[AstPattern],
    fields: &[orna_syntax::PatternField],
    span: &SyntaxSpan,
) -> PatId {
    let module = state.module;
    let error_ty = builder.program.tys.error();
    let segments = path
        .iter()
        .map(|segment| Name::new(&segment.text))
        .collect::<Vec<_>>();
    let last_span = path
        .last()
        .map_or_else(|| span.clone(), |segment| segment.span.clone());
    let path_segments = path
        .iter()
        .zip(&segments)
        .map(|(segment, name)| (name.clone(), segment.span.clone()))
        .collect::<Vec<_>>();
    super::exprs::record_prefix_uses(
        builder,
        module,
        Some(state.id),
        &path_segments,
        crate::UseContext::Pattern,
    );
    let last_name = segments.last().cloned().unwrap_or_else(|| Name::new(""));
    let resolved = match builder.resolve_path(module, &segments) {
        PathKind::Member { base, .. } => builder
            .member_def(base, &last_name)
            .map_or(PathKind::Unresolved, PathKind::Def),
        other => other,
    };
    let def = match resolved {
        PathKind::Def(def) => Some(def),
        _ => None,
    };
    super::record_body_use(
        &mut builder.program,
        module,
        state.id,
        last_span.clone(),
        last_name.clone(),
        crate::UseContext::Pattern,
        def.map_or(NameTarget::Unresolved, NameTarget::Def),
    );
    if def.is_none() {
        builder.diagnose(
            module,
            crate::diag::UNRESOLVED,
            "pattern constructor does not resolve",
            span.clone(),
        );
    }
    let mut lowered_fields = Vec::new();
    for field in fields {
        let name_span = SyntaxSpan::new(field.span.start, field.span.start + field.name.len());
        let field_def = def.and_then(|def| find_field(builder, def, &field.name));
        super::record_body_use(
            &mut builder.program,
            module,
            state.id,
            name_span.clone(),
            Name::new(&field.name),
            crate::UseContext::Field,
            field_def.map_or(NameTarget::Unresolved, NameTarget::Def),
        );
        let field_ty = field_def.and_then(|field| field_definition_of(builder, field));
        let sub = match &field.pattern {
            Some(pattern) => lower_pattern_with(builder, state, pattern, field_ty),
            None => {
                let local = state.add_local(
                    Name::new(&field.name),
                    field_ty.unwrap_or(error_ty),
                    field.span.clone(),
                );
                super::record_binding_use(
                    &mut builder.program,
                    module,
                    state.id,
                    field.span.clone(),
                    Name::new(&field.name),
                    local,
                    true,
                );
                state.push_pat(Pat::Bind { local })
            }
        };
        if let Some(field_def) = field_def {
            lowered_fields.push((field_def, sub));
        }
    }
    for (index, argument) in arguments.iter().enumerate() {
        let field_def = def.and_then(|def| field_at(builder, def, index));
        let field_ty = field_def.and_then(|field| field_definition_of(builder, field));
        let sub = lower_pattern_with(builder, state, argument, field_ty);
        if let Some(field_def) = field_def {
            lowered_fields.push((field_def, sub));
        }
    }
    match def {
        Some(def) => state.push_pat(Pat::Ctor {
            def,
            fields: lowered_fields.into_boxed_slice(),
        }),
        None => state.push_pat(Pat::Wild),
    }
}

/// The declared type of one field definition.
fn field_definition_of(builder: &Builder<'_>, field: DefId) -> Option<Ty> {
    match &builder.pending[field.index()].kind {
        PendingKind::Built(DefKind::Field(definition)) => Some(definition.ty),
        _ => None,
    }
}

/// The definition of the field named `name` on `owner`.
fn find_field(builder: &Builder<'_>, owner: DefId, name: &str) -> Option<DefId> {
    payload(builder, owner)
        .into_iter()
        .find(|field| builder.def_name(*field).as_str() == name)
}

/// The definition of the field at `index` on `owner`.
fn field_at(builder: &Builder<'_>, owner: DefId, index: usize) -> Option<DefId> {
    payload(builder, owner).get(index).copied()
}

/// The field definitions a constructor owns, in declaration order.
pub(crate) fn payload(builder: &Builder<'_>, owner: DefId) -> Vec<DefId> {
    use crate::DefKind;
    match &builder.pending[owner.index()].kind {
        PendingKind::Built(DefKind::Variant(variant)) => variant.fields.clone(),
        PendingKind::Built(DefKind::Type(definition)) => definition.members.clone(),
        PendingKind::Built(DefKind::Table(table)) => table.fields.clone(),
        _ => Vec::new(),
    }
}
