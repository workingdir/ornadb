//! The `Some(p)` pattern: an option's present case. It is not a constructor of
//! a definition: a present option is its payload (decision D3), so the pattern
//! matches any non-null value and matches `p` against that same value.

use orna_syntax_ast::{NameSegment, Pattern as AstPattern, PatternField};

use crate::{ModuleId, Name, NameTarget, Pat, PatId, UseContext};

use super::BodyState;
use super::declare::Builder;
use super::pats::lower_pattern_with;

/// Whether a constructor pattern is the language's `Some(p)`: one unqualified
/// `Some` that no user definition names, with one positional sub-pattern.
pub(super) fn is_option_some(
    builder: &Builder,
    module: ModuleId,
    path: &[NameSegment],
    arguments: &[AstPattern],
    fields: &[PatternField],
) -> bool {
    let [segment] = path else {
        return false;
    };
    let [_] = arguments else {
        return false;
    };
    segment.text == "Some"
        && fields.is_empty()
        && matches!(
            builder.resolve_bare(module, &Name::new("Some")),
            NameTarget::Intrinsic
        )
}

/// Lowers `Some(argument)`, recording the `Some` spelling as the language
/// intrinsic it names.
pub(super) fn lower_option_some(
    builder: &mut Builder,
    state: &mut BodyState,
    segment: &NameSegment,
    argument: &AstPattern,
) -> PatId {
    let module = state.module;
    super::record_body_use(
        &mut builder.program,
        module,
        state.id,
        segment.span.clone(),
        Name::new(&segment.text),
        UseContext::Pattern,
        NameTarget::Intrinsic,
    );
    let inner = lower_pattern_with(builder, state, argument, None);
    state.push_pat(Pat::Some(inner))
}
