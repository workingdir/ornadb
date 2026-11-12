//! The session's accumulated source: its `use` declarations, function definitions and module-level `let`s, as
//! text. Every stage lowers this source over the standard library, which is lowered once and reused.

/// One definition of the session module: the name it defines and the source text of the declaration.
#[derive(Clone, Debug)]
pub(crate) struct Definition {
    pub(crate) name: String,
    pub(crate) text: String,
}

/// The session's source, in module order: imports, then function definitions, then module-level `let`s.
#[derive(Clone, Debug, Default)]
pub(crate) struct Base {
    imports: Vec<String>,
    definitions: Vec<Definition>,
    globals: Vec<Definition>,
}

impl Base {
    /// Whether the session already accepts this `use` declaration.
    pub(crate) fn has_import(&self, text: &str) -> bool {
        self.imports.iter().any(|import| import == text)
    }

    #[must_use]
    pub(crate) fn with_import(mut self, text: String) -> Self {
        self.imports.push(text);
        self
    }

    #[must_use]
    pub(crate) fn with_definition(mut self, definition: Definition) -> Self {
        self.definitions.push(definition);
        self
    }

    #[must_use]
    pub(crate) fn with_global(mut self, global: Definition) -> Self {
        self.globals.push(global);
        self
    }

    /// The base without the function definitions and module-level `let`s named `name`.
    #[must_use]
    pub(crate) fn without(&self, name: &str) -> Self {
        let mut base = self.clone();
        base.definitions
            .retain(|definition| definition.name != name);
        base.globals.retain(|global| global.name != name);
        base
    }

    /// The module source of the imports and the function definitions alone, which a function is checked against.
    pub(crate) fn definitions_source(&self) -> String {
        lines(
            self.imports.iter().map(String::as_str).chain(
                self.definitions
                    .iter()
                    .map(|definition| definition.text.as_str()),
            ),
        )
    }

    /// The module source of the whole base, then `entry` when there is one.
    pub(crate) fn source(&self, entry: Option<&str>) -> String {
        let mut source = lines(
            self.imports
                .iter()
                .map(String::as_str)
                .chain(
                    self.definitions
                        .iter()
                        .map(|definition| definition.text.as_str()),
                )
                .chain(self.globals.iter().map(|global| global.text.as_str())),
        );
        if let Some(entry) = entry {
            source.push_str(entry);
            source.push('\n');
        }
        source
    }
}

/// The given declarations, one per line.
fn lines<'a>(declarations: impl IntoIterator<Item = &'a str>) -> String {
    let mut source = String::new();
    for declaration in declarations {
        source.push_str(declaration);
        source.push('\n');
    }
    source
}

#[cfg(test)]
mod tests {
    use super::{Base, Definition};

    fn definition(name: &str, text: &str) -> Definition {
        Definition {
            name: name.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn source_lists_imports_then_definitions_then_globals_then_entry() {
        let base = Base::default()
            .with_import("use std.math;".to_owned())
            .with_definition(definition("f", "fn f() = 1;"))
            .with_global(definition("x", "let x = 2;"));
        assert_eq!(
            base.source(Some("fn e() = 3;")),
            "use std.math;\nfn f() = 1;\nlet x = 2;\nfn e() = 3;\n"
        );
        assert_eq!(base.definitions_source(), "use std.math;\nfn f() = 1;\n");
    }

    #[test]
    fn without_removes_definitions_and_globals_of_the_name_only() {
        let base = Base::default()
            .with_definition(definition("f", "fn f() = 1;"))
            .with_definition(definition("g", "fn g() = 2;"))
            .with_global(definition("f", "let f = 3;"));
        assert_eq!(base.without("f").source(None), "fn g() = 2;\n");
        assert_eq!(
            base.source(None),
            "fn f() = 1;\nfn g() = 2;\nlet f = 3;\n",
            "without leaves the original base unchanged"
        );
    }
}
