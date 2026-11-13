//! Native declarations: the C symbol a native key exports and the effects its clause declares.

use orna_syntax_ast::NativeEffectSet;

/// The C symbol a shared library exports for a native key (library document
/// section 4): `orna_` followed by the module path segments and the name joined
/// by `__`. The flag is false when a part is outside `[A-Za-z0-9_]`.
pub(super) fn native_symbol(module: &crate::ModulePath, name: &crate::Name) -> (String, bool) {
    let parts: Vec<&str> = module
        .segments()
        .iter()
        .chain(std::iter::once(name))
        .map(crate::Name::as_str)
        .collect();
    let ascii = parts.iter().all(|part| {
        part.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    });
    (format!("orna_{}", parts.join("__")), ascii)
}

/// The effect kinds a native declaration's `effect` clause names (ORNA-NATIVE-009).
/// A recovered parse with no clause contributes nothing; a class spelling outside
/// the vocabulary fails closed as `Opaque`.
pub(super) fn declared_effects(clause: Option<&NativeEffectSet>) -> crate::EffectSet {
    match clause {
        Some(NativeEffectSet::Classes { classes, .. }) => {
            classes
                .iter()
                .fold(crate::EffectSet::empty(), |set, class| {
                    set.join(match crate::EffectKind::from_class(&class.text) {
                        Some(kind) => crate::EffectSet::single(kind),
                        None => crate::EffectSet::opaque(),
                    })
                })
        }
        Some(NativeEffectSet::None { .. }) | None => crate::EffectSet::empty(),
    }
}
