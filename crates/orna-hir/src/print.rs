//! Printing a [`Ty`] in the specification's type spelling (design section 2.3).
//!
//! The stage H2 acceptance compares the printed type of every public signature
//! with `orna-check`'s. The spelling is the one the specification uses
//! (`Str`, not `Text`; `T?`; `[T]`; `Map<K, V>`), and a nominal type prints as
//! its declared name because the [`DefId`] is its identity: two declarations
//! that share a spelling in different modules remain distinct types while
//! printing the same way, exactly as `TyKind::Adt` compares.
//!
//! A type variable prints as `?n` and an unresolved diagnostic type as
//! `Error`. Neither appears in a signature the type pass accepted.

use crate::{DefId, DefKind, Name, Program, Ty, TyKind};

/// Prints one type.
#[must_use]
pub fn print_type(program: &Program, ty: Ty) -> String {
    print_type_with(program, ty, &[])
}

/// Prints one function's signature as `name(params) -> result`.
///
/// Returns `None` when the definition is not a function.
#[must_use]
pub fn print_signature(program: &Program, def: DefId) -> Option<String> {
    let definition = program.defs.get(def)?;
    let DefKind::Fn(function) = &definition.kind else {
        return None;
    };
    let generics = function
        .generics
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>();
    let parameters = function
        .params
        .iter()
        .map(|parameter| print_type_with(program, parameter.ty, &generics))
        .collect::<Vec<_>>()
        .join(", ");
    let result = print_type_with(program, function.ret, &generics);
    Some(format!(
        "{}({parameters}) -> {result}",
        definition.name.as_str()
    ))
}

/// Prints one type with the enclosing definition's generic parameter names.
fn print_type_with(program: &Program, ty: Ty, generics: &[Name]) -> String {
    let Some(kind) = program.tys.kind(ty) else {
        return "?".to_owned();
    };
    match kind {
        TyKind::Bool => "Bool".to_owned(),
        TyKind::Int => "Int".to_owned(),
        TyKind::Decimal => "Decimal".to_owned(),
        TyKind::Float => "Float".to_owned(),
        TyKind::Str => "Str".to_owned(),
        TyKind::Blob => "Blob".to_owned(),
        TyKind::Uuid => "Uuid".to_owned(),
        TyKind::Date => "Date".to_owned(),
        TyKind::Instant => "Instant".to_owned(),
        TyKind::Duration => "Duration".to_owned(),
        TyKind::Period => "Period".to_owned(),
        TyKind::Digest => "Digest".to_owned(),
        TyKind::Unit => "Unit".to_owned(),
        TyKind::Never => "Never".to_owned(),
        TyKind::Error => "Error".to_owned(),
        TyKind::Option(inner) => format!("{}?", print_type_with(program, *inner, generics)),
        TyKind::List(inner) => format!("[{}]", print_type_with(program, *inner, generics)),
        TyKind::Set(inner) => format!("Set<{}>", print_type_with(program, *inner, generics)),
        TyKind::Map(key, value) => format!(
            "Map<{}, {}>",
            print_type_with(program, *key, generics),
            print_type_with(program, *value, generics)
        ),
        TyKind::Tuple(elements) => format!(
            "({})",
            elements
                .iter()
                .map(|element| print_type_with(program, *element, generics))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TyKind::Record(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(name, field)| {
                    format!(
                        "{}: {}",
                        name.as_str(),
                        print_type_with(program, *field, generics)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TyKind::Range(inner) => format!("Range<{}>", print_type_with(program, *inner, generics)),
        TyKind::Relation(inner) => {
            format!("Relation<{}>", print_type_with(program, *inner, generics))
        }
        TyKind::Stream(inner) => {
            format!("Stream<{}>", print_type_with(program, *inner, generics))
        }
        TyKind::Adt(def, arguments) => {
            let name = program
                .defs
                .get(*def)
                .map_or_else(|| "?".to_owned(), definitions_name);
            if arguments.is_empty() {
                name
            } else {
                format!(
                    "{name}<{}>",
                    arguments
                        .iter()
                        .map(|argument| print_type_with(program, *argument, generics))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        TyKind::Fn(function) => format!(
            "({}) -> {}",
            function
                .params
                .iter()
                .map(|parameter| print_type_with(program, *parameter, generics))
                .collect::<Vec<_>>()
                .join(", "),
            print_type_with(program, function.ret, generics)
        ),
        TyKind::Param(index) => generics.get(index.as_u32() as usize).map_or_else(
            || format!("T{}", index.as_u32()),
            |name| name.as_str().to_owned(),
        ),
        TyKind::Infer(var) => format!("?{}", var.as_u32()),
    }
}

/// The declared name of a nominal definition.
fn definitions_name(definition: &crate::Def) -> String {
    definition.name.as_str().to_owned()
}

#[cfg(test)]
mod tests {
    use super::print_type;
    use crate::{Program, TyKind};

    #[test]
    fn prints_the_specification_spelling() {
        let mut program = Program::empty();
        let int = program.tys.intern(TyKind::Int);
        let str_ty = program.tys.intern(TyKind::Str);
        let option = program.tys.intern(TyKind::Option(str_ty));
        let list = program.tys.intern(TyKind::List(int));
        let pair = program.tys.intern(TyKind::Tuple(Box::new([option, list])));
        assert_eq!(print_type(&program, str_ty), "Str");
        assert_eq!(print_type(&program, option), "Str?");
        assert_eq!(print_type(&program, list), "[Int]");
        assert_eq!(print_type(&program, pair), "(Str?, [Int])");
    }
}
