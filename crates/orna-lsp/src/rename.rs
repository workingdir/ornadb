//! Rename planning from name resolution.
//!
//! A rename edits exactly the occurrences that resolve to the renamed
//! declaration or binding. It is refused, with a reason, when an occurrence
//! of the same name might refer to it but cannot be resolved, when the new
//! name is not a valid identifier, or when the edit would change what any
//! other name in the workspace refers to.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use orna_syntax_v1::SyntaxSpan;

use crate::documents::Document;
use crate::names::{DeclaredKind, Role, key_of};
use crate::workspace::{FileAnalysis, Resolved, Target, Workspace};

/// Why a rename cannot be made. The message completes "cannot rename: ".
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refusal(pub String);

impl Refusal {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The name under the cursor that a rename would change.
pub(crate) struct Renamable {
    pub span: SyntaxSpan,
    pub placeholder: String,
    pub target: Target,
}

/// Edits per module: byte spans in the module's current text and their
/// replacement.
pub(crate) type ModuleEdits = BTreeMap<usize, Vec<(SyntaxSpan, String)>>;

/// The renamable name at `byte`, or why there is none.
pub(crate) fn prepare(workspace: &Workspace, module: usize, byte: usize) -> Result<Renamable, Refusal> {
    let file = workspace.file(module);
    if let Some(keyword) = file.keyword_at(byte) {
        return Err(Refusal::new(format!(
            "`{}` is a reserved word",
            keyword.spelling()
        )));
    }
    if let Some((broken, diagnostic)) = workspace.modules.iter().enumerate().find_map(|(index, entry)| {
        entry
            .file
            .parse
            .diagnostics
            .first()
            .map(|diagnostic| (index, diagnostic))
    }) {
        return Err(Refusal::new(format!(
            "{} has a syntax error ({}); references cannot be found until it is fixed",
            workspace.location_label(broken, diagnostic.span.start),
            diagnostic.message
        )));
    }
    let Some((span, resolved)) = workspace.resolve_at(module, byte) else {
        return Err(Refusal::new("there is no name to rename here"));
    };
    let placeholder = file.text()[span.start..span.end].to_owned();
    let target = match resolved {
        Resolved::Target(target) => target,
        Resolved::Ambiguous => {
            return Err(Refusal::new(format!(
                "`{placeholder}` matches more than one declaration, so its references cannot be found safely"
            )));
        }
        Resolved::Unknown => {
            return Err(Refusal::new(format!(
                "`{placeholder}` does not resolve to a declaration"
            )));
        }
    };
    match &target {
        Target::Module(_) => Err(Refusal::new(format!(
            "`{placeholder}` names a module; rename its source file instead"
        ))),
        Target::Standard { module, .. } => Err(Refusal::new(format!(
            "`{placeholder}` is declared in the read-only standard library module `{module}`"
        ))),
        Target::Local { module, binding }
            if workspace.file(*module).names.binding(*binding).key == "self" =>
        {
            Err(Refusal::new("`self` cannot be renamed"))
        }
        _ => Ok(Renamable {
            span,
            placeholder,
            target,
        }),
    }
}

/// The edits that rename the name at `byte` to `new_name`.
pub(crate) fn plan(
    workspace: &Workspace,
    module: usize,
    byte: usize,
    new_name: &str,
) -> Result<(Renamable, ModuleEdits), Refusal> {
    let renamable = prepare(workspace, module, byte)?;
    let old_name = renamable.placeholder.clone();
    if !valid_identifier(new_name) {
        return Err(Refusal::new(format!(
            "`{new_name}` is not a valid name: use an identifier that is not a reserved word"
        )));
    }
    if let Some(reason) = &workspace.incomplete {
        return Err(Refusal::new(format!(
            "the project could not be loaded ({reason}), so every reference to `{old_name}` cannot be found"
        )));
    }
    let search = workspace.search(&renamable.target);
    if let Some((uncertain_module, span)) = search.uncertain.first() {
        return Err(Refusal::new(format!(
            "`{old_name}` at {} may refer to this {}, but what it refers to cannot be resolved",
            workspace.location_label(*uncertain_module, span.start),
            target_noun(workspace, &renamable.target),
        )));
    }
    for (found_module, span) in &search.found {
        let names = &workspace.file(*found_module).names;
        let shorthand = names.binding_at(span).is_some()
            && names
                .occurrence(span)
                .is_some_and(|occurrence| matches!(occurrence.role, Role::PatternField { .. }));
        if shorthand {
            return Err(Refusal::new(format!(
                "the pattern at {} binds `{old_name}` by its field name; write `{old_name}: {old_name}` there first",
                workspace.location_label(*found_module, span.start)
            )));
        }
    }
    let mut edits = ModuleEdits::new();
    for (found_module, span) in search.found {
        edits
            .entry(found_module)
            .or_default()
            .push((span, new_name.to_owned()));
    }
    verify_resolution(workspace, &edits)?;
    Ok((renamable, edits))
}

/// A rename target must parse as a declaration name, so a reserved word
/// (ORNA-LEX-007) is never produced.
pub(crate) fn valid_identifier(name: &str) -> bool {
    let source = format!("fn {name}() = 0;");
    let parse = orna_syntax_v1::parse_module(&source);
    parse.diagnostics.is_empty()
        && matches!(
            parse.value.items.as_slice(),
            [item] if matches!(
                &item.declaration,
                orna_syntax_v1::Declaration::Function { signature, .. }
                    if signature.name == name && key_of(&signature.name) == key_of(name)
            )
        )
}

fn target_noun(workspace: &Workspace, target: &Target) -> &'static str {
    match target {
        Target::Local { .. } => "local name",
        Target::Declared { module, index } => match workspace.declaration(*module, *index).kind {
            DeclaredKind::Field | DeclaredKind::KeyField | DeclaredKind::PayloadField => "field",
            DeclaredKind::Variant => "variant",
            DeclaredKind::Method => "function",
            DeclaredKind::Static => "static property",
            _ => "declaration",
        },
        Target::Module(_) | Target::Standard { .. } => "declaration",
    }
}

/// Applies `edits` to one text.
pub(crate) fn apply(text: &str, edits: &[(SyntaxSpan, String)]) -> String {
    let mut sorted = edits.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
    let mut updated = text.to_owned();
    for (span, replacement) in sorted {
        updated.replace_range(span.start..span.end, replacement);
    }
    updated
}

/// Maps byte offsets of a text to the text after one module's edits.
struct Shift<'a> {
    edits: Vec<&'a (SyntaxSpan, String)>,
}

impl<'a> Shift<'a> {
    fn new(edits: Option<&'a Vec<(SyntaxSpan, String)>>) -> Self {
        let mut edits = edits.map_or_else(Vec::new, |edits| edits.iter().collect::<Vec<_>>());
        edits.sort_by_key(|(span, _)| span.start);
        Self { edits }
    }

    fn offset(&self, byte: usize) -> usize {
        let mut shifted = byte;
        for (span, replacement) in &self.edits {
            if span.end <= byte {
                shifted = shifted + replacement.len() - (span.end - span.start);
            }
        }
        shifted
    }

    fn span(&self, span: &SyntaxSpan) -> SyntaxSpan {
        let start = self.offset(span.start);
        match self
            .edits
            .iter()
            .find(|(edited, _)| edited.start == span.start && edited.end == span.end)
        {
            Some((_, replacement)) => SyntaxSpan::new(start, start + replacement.len()),
            None => SyntaxSpan::new(start, self.offset(span.end)),
        }
    }
}

/// A resolution result in the coordinates of the edited workspace.
#[derive(Debug, PartialEq, Eq)]
enum Meaning {
    Local(usize, usize),
    Declared(usize, usize),
    Module(Vec<String>),
    Standard(String, String),
    Ambiguous,
    Unknown,
}

fn meaning(workspace: &Workspace, resolved: Resolved, shifts: &[Shift<'_>]) -> Meaning {
    match resolved {
        Resolved::Target(Target::Local { module, binding }) => {
            let selection = &workspace.file(module).names.binding(binding).selection;
            Meaning::Local(module, shifts[module].offset(selection.start))
        }
        Resolved::Target(Target::Declared { module, index }) => {
            let selection = &workspace.declaration(module, index).selection;
            Meaning::Declared(module, shifts[module].offset(selection.start))
        }
        Resolved::Target(Target::Module(path)) => Meaning::Module(path),
        Resolved::Target(Target::Standard { module, name }) => Meaning::Standard(module, name),
        Resolved::Ambiguous => Meaning::Ambiguous,
        Resolved::Unknown => Meaning::Unknown,
    }
}

/// Refuses the edits unless every name in every module still resolves to
/// what it resolved to before, the edited text still parses as it did, and
/// no new names appear.
fn verify_resolution(workspace: &Workspace, edits: &ModuleEdits) -> Result<(), Refusal> {
    let replaced = edits
        .iter()
        .map(|(module, module_edits)| {
            let file = workspace.file(*module);
            let document = Document::new(
                file.uri().clone(),
                apply(file.text(), module_edits),
                file.document.version,
            );
            (*module, Arc::new(FileAnalysis::new(document)))
        })
        .collect::<HashMap<_, _>>();
    for (module, file) in &replaced {
        if file.parse.diagnostics.len() > workspace.file(*module).parse.diagnostics.len() {
            return Err(Refusal::new(format!(
                "the new name does not parse in {}",
                workspace.location_label(*module, 0)
            )));
        }
    }
    let edited = workspace.with_files(replaced);
    let shifts = (0..workspace.modules.len())
        .map(|module| Shift::new(edits.get(&module)))
        .collect::<Vec<_>>();
    let after_shifts = (0..workspace.modules.len())
        .map(|_| Shift::new(None))
        .collect::<Vec<_>>();
    for module in 0..workspace.modules.len() {
        let before = workspace.file(module).names.occurrences();
        let after = edited.file(module).names.occurrences();
        if before.len() != after.len() {
            return Err(Refusal::new(format!(
                "the edit would change the names in {}",
                workspace.location_label(module, 0)
            )));
        }
        for occurrence in before {
            let span = shifts[module].span(&occurrence.span);
            let Some(moved) = edited.file(module).names.occurrence(&span) else {
                return Err(Refusal::new(format!(
                    "the edit would change the name at {}",
                    workspace.location_label(module, occurrence.span.start)
                )));
            };
            let old = meaning(workspace, workspace.resolve(module, occurrence), &shifts);
            let new = meaning(&edited, edited.resolve(module, moved), &after_shifts);
            if old != new {
                let text = &workspace.file(module).text()[occurrence.span.start..occurrence.span.end];
                return Err(Refusal::new(format!(
                    "`{text}` at {} would then refer to something else",
                    workspace.location_label(module, occurrence.span.start)
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Module;

    fn workspace(files: &[(&str, &str)]) -> Workspace {
        Workspace::detached(
            files
                .iter()
                .map(|(uri, text)| {
                    Arc::new(FileAnalysis::new(Document::new(
                        uri.parse().unwrap(),
                        (*text).to_owned(),
                        1,
                    )))
                })
                .collect(),
        )
    }

    fn renamed(workspace: &Workspace, module: usize, edits: &ModuleEdits) -> String {
        apply(workspace.file(module).text(), &edits[&module])
    }

    const FIXTURE: &str = "pub table Contact(id: Str) {\n    title: Str,\n}\n\
        pub fn title(contact: Contact): Str = contact.title;\n\
        pub fn show(contact: Contact): Str = title(contact);\n\
        pub fn shout(value: Str): Str = value;\n\
        pub type Badge {\n    pub text: Str,\n    impl Display {\n        \
        fn display(self, context: DisplayContext): Str = shout(self.text);\n    }\n}\n";

    #[test]
    fn renaming_a_function_leaves_the_same_named_field_alone() {
        let workspace = workspace(&[("file:///w/main.orna", FIXTURE)]);
        let byte = FIXTURE.find("fn title").unwrap() + 3;
        let (_, edits) = plan(&workspace, 0, byte, "heading").unwrap();
        let text = renamed(&workspace, 0, &edits);
        assert!(text.contains("pub fn heading(contact: Contact): Str = contact.title;"), "{text}");
        assert!(text.contains("= heading(contact);"), "{text}");
        assert!(text.contains("    title: Str,"), "{text}");
    }

    #[test]
    fn renaming_a_field_edits_its_declaration_and_typed_accesses_only() {
        let workspace = workspace(&[("file:///w/main.orna", FIXTURE)]);
        let byte = FIXTURE.find("title: Str").unwrap();
        let (_, edits) = plan(&workspace, 0, byte, "heading").unwrap();
        let text = renamed(&workspace, 0, &edits);
        assert!(text.contains("    heading: Str,"), "{text}");
        assert!(text.contains("pub fn title(contact: Contact): Str = contact.heading;"), "{text}");
        assert!(text.contains("= title(contact);"), "{text}");
    }

    #[test]
    fn calls_inside_implementations_are_renamed() {
        let workspace = workspace(&[("file:///w/main.orna", FIXTURE)]);
        let byte = FIXTURE.find("fn shout").unwrap() + 3;
        let (_, edits) = plan(&workspace, 0, byte, "yell").unwrap();
        let text = renamed(&workspace, 0, &edits);
        assert!(text.contains("pub fn yell(value: Str)"), "{text}");
        assert!(text.contains("= yell(self.text);"), "{text}");
        assert!(!text.contains("shout"), "{text}");
    }

    #[test]
    fn unresolvable_member_accesses_refuse_a_field_rename() {
        let source = "pub table Contact(id: Str) {\n    title: Str,\n}\n\
            pub fn guess(value): Str = value.title;\n";
        let workspace = workspace(&[("file:///w/main.orna", source)]);
        let byte = source.find("title: Str").unwrap();
        let Err(Refusal(reason)) = plan(&workspace, 0, byte, "heading") else {
            panic!("an untyped `value.title` must block the rename");
        };
        assert!(reason.contains("main.orna:4:"), "{reason}");
    }

    #[test]
    fn renames_that_would_capture_or_shadow_a_name_are_refused() {
        let source = "pub fn total(value: Int): Int = value;\n\
            pub fn sum(value: Int): Int = value + 1;\n\
            pub fn caller(value: Int): Int = total(value) + sum(value);\n";
        let workspace = workspace(&[("file:///w/main.orna", source)]);
        let byte = source.find("fn total").unwrap() + 3;
        let Err(Refusal(reason)) = plan(&workspace, 0, byte, "sum") else {
            panic!("renaming onto an existing declaration must be refused");
        };
        assert!(reason.contains("would then refer to something else"), "{reason}");

        let local = "pub fn shadow(value: Int): Int { let other = 1; value + other }\n";
        let workspace = super::tests::workspace(&[("file:///w/main.orna", local)]);
        let byte = local.find("other = 1").unwrap();
        assert!(plan(&workspace, 0, byte, "value").is_err());
    }

    #[test]
    fn reserved_words_and_standard_names_are_refused_with_reasons() {
        let source = "use std.math.{abs};\npub fn magnitude(value: Int): Int = abs(value);\n";
        let workspace = workspace(&[("file:///w/main.orna", source)]);
        let byte = source.find("abs(value)").unwrap();
        let Err(Refusal(reason)) = prepare(&workspace, 0, byte) else {
            panic!("standard library names are read-only");
        };
        assert!(reason.contains("std.math"), "{reason}");
        let keyword = source.find("pub fn").unwrap() + 4;
        assert!(prepare(&workspace, 0, keyword).is_err());
        let byte = source.find("magnitude").unwrap();
        assert!(plan(&workspace, 0, byte, "let").is_err());
    }

    #[test]
    fn project_rename_updates_imports_and_qualified_uses() {
        let main = "use util;\nuse util.{helper};\npub fn total(): Int = util.helper() + helper();\n";
        let util = "pub fn helper(): Int = 1;\n";
        let workspace = Workspace::project(
            vec![
                Module::project(
                    Arc::new(FileAnalysis::new(Document::new(
                        "file:///p/main.orna".parse().unwrap(),
                        main.to_owned(),
                        1,
                    ))),
                    "main.orna",
                ),
                Module::project(
                    Arc::new(FileAnalysis::new(Document::new(
                        "file:///p/util.orna".parse().unwrap(),
                        util.to_owned(),
                        1,
                    ))),
                    "util.orna",
                ),
            ],
            crate::standard::StandardLibrary::none(),
            None,
        );
        let (_, edits) = plan(&workspace, 1, 8, "assist").unwrap();
        assert_eq!(
            renamed(&workspace, 0, &edits),
            "use util;\nuse util.{assist};\npub fn total(): Int = util.assist() + assist();\n"
        );
        assert_eq!(renamed(&workspace, 1, &edits), "pub fn assist(): Int = 1;\n");
    }
}
