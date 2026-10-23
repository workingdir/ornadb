//! Call and type hierarchies, call-count code lenses and monikers, built from
//! resolved names.
//!
//! A call is an edge only when its callee resolves to one function or method,
//! and a type implements a protocol only when the implementation's protocol
//! path resolves to one protocol. Ambiguous and unresolved names create no
//! edges.

use std::collections::{BTreeMap, HashMap};

use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, CodeLens, Command,
    Location, Moniker, MonikerKind, Range, SymbolKind, TypeHierarchyItem, UniquenessLevel,
};
use orna_syntax_v1::{
    Declaration, Implementation, SyntaxSpan, TableMember, TypeExpr, TypeMember, TypeRepresentation,
};

use crate::analysis::{self, EditorSymbolKind};
use crate::documents::PositionMapper;
use crate::names::{DeclaredKind, Role};
use crate::workspace::{Resolved, Target, Workspace};

const MONIKER_SCHEME: &str = "orna-syntax-v1";

/// Line indexes of the modules one request touches, each built once.
struct Mappers<'a> {
    workspace: &'a Workspace,
    built: HashMap<usize, PositionMapper<'a>>,
}

impl<'a> Mappers<'a> {
    fn new(workspace: &'a Workspace) -> Self {
        Self {
            workspace,
            built: HashMap::new(),
        }
    }

    fn get(&mut self, module: usize) -> &PositionMapper<'a> {
        let workspace = self.workspace;
        self.built
            .entry(module)
            .or_insert_with(|| PositionMapper::new(workspace.file(module).text()))
    }

    fn location(&mut self, module: usize, span: &SyntaxSpan) -> Location {
        Location {
            uri: self.workspace.file(module).uri().clone(),
            range: self.get(module).range(span),
        }
    }
}

fn is_function(kind: DeclaredKind) -> bool {
    matches!(kind, DeclaredKind::Function | DeclaredKind::Method)
}

/// The declaration in `module` whose name an item's selection range covers.
fn declared_at_range(workspace: &Workspace, module: usize, range: &Range) -> Option<usize> {
    let file = workspace.file(module);
    let mapper = PositionMapper::new(file.text());
    let span = SyntaxSpan::new(mapper.byte_offset(range.start), mapper.byte_offset(range.end));
    file.names.declared_at(&span)
}

/// The function or method declared at an item's selection range.
fn function_of_item(workspace: &Workspace, module: usize, item: &CallHierarchyItem) -> Option<usize> {
    let index = declared_at_range(workspace, module, &item.selection_range)?;
    let declared = workspace.declaration(module, index);
    (is_function(declared.kind) && declared.name == item.name).then_some(index)
}

fn call_item(
    workspace: &Workspace,
    mappers: &mut Mappers<'_>,
    module: usize,
    index: usize,
) -> CallHierarchyItem {
    let file = workspace.file(module);
    let declared = workspace.declaration(module, index);
    let mapper = mappers.get(module);
    CallHierarchyItem {
        name: declared.name.clone(),
        kind: if declared.kind == DeclaredKind::Method {
            SymbolKind::METHOD
        } else {
            SymbolKind::FUNCTION
        },
        tags: None,
        detail: analysis::declared_symbol(&file.symbols, &declared.selection)
            .and_then(|symbol| symbol.detail.clone()),
        uri: file.uri().clone(),
        range: mapper.range(&declared.full),
        selection_range: mapper.range(&declared.selection),
        data: None,
    }
}

/// Orders declarations as editors list them: by document, then position.
fn sort_declarations(workspace: &Workspace, declarations: &mut [(usize, usize)]) {
    declarations.sort_by(|left, right| {
        workspace
            .file(left.0)
            .uri()
            .as_str()
            .cmp(workspace.file(right.0).uri().as_str())
            .then_with(|| {
                let left = &workspace.declaration(left.0, left.1).selection;
                let right = &workspace.declaration(right.0, right.1).selection;
                (left.start, left.end).cmp(&(right.start, right.end))
            })
    });
}

/// Calls that resolve to `target`.
fn calls_of(workspace: &Workspace, target: &Target) -> Vec<(usize, SyntaxSpan)> {
    workspace
        .search(target)
        .found
        .into_iter()
        .filter(|(module, span)| {
            workspace
                .file(*module)
                .names
                .occurrence(span)
                .is_some_and(|occurrence| occurrence.callee)
        })
        .collect()
}

/// The innermost function or method in `module` whose body contains `span`.
fn enclosing_function(workspace: &Workspace, module: usize, span: &SyntaxSpan) -> Option<usize> {
    workspace
        .file(module)
        .names
        .declarations()
        .iter()
        .enumerate()
        .filter(|(_, declared)| {
            is_function(declared.kind)
                && declared.full.start <= span.start
                && span.end <= declared.full.end
        })
        .min_by_key(|(_, declared)| declared.full.end - declared.full.start)
        .map(|(index, _)| index)
}

/// The function or method named at `byte`, at its declaration or at any
/// reference that resolves to it.
pub(crate) fn prepare_call_hierarchy(
    workspace: &Workspace,
    module: usize,
    byte: usize,
) -> Option<Vec<CallHierarchyItem>> {
    let (_, resolved) = workspace.resolve_at(module, byte)?;
    let Target::Declared {
        module: declared_module,
        index,
    } = resolved.target()?
    else {
        return None;
    };
    if !is_function(workspace.declaration(declared_module, index).kind) {
        return None;
    }
    let mut mappers = Mappers::new(workspace);
    Some(vec![call_item(workspace, &mut mappers, declared_module, index)])
}

/// The functions and methods that call the one `item` names, with the
/// ranges of their calls.
pub(crate) fn incoming_calls(
    workspace: &Workspace,
    module: usize,
    item: &CallHierarchyItem,
) -> Vec<CallHierarchyIncomingCall> {
    let Some(index) = function_of_item(workspace, module, item) else {
        return Vec::new();
    };
    let mut callers = BTreeMap::<(usize, usize), Vec<SyntaxSpan>>::new();
    for (caller_module, span) in calls_of(workspace, &Target::Declared { module, index }) {
        if let Some(caller) = enclosing_function(workspace, caller_module, &span) {
            callers.entry((caller_module, caller)).or_default().push(span);
        }
    }
    let mut order = callers.keys().copied().collect::<Vec<_>>();
    sort_declarations(workspace, &mut order);
    let mut mappers = Mappers::new(workspace);
    order
        .into_iter()
        .map(|(caller_module, caller)| {
            let from = call_item(workspace, &mut mappers, caller_module, caller);
            let mapper = mappers.get(caller_module);
            CallHierarchyIncomingCall {
                from,
                from_ranges: callers[&(caller_module, caller)]
                    .iter()
                    .map(|span| mapper.range(span))
                    .collect(),
            }
        })
        .collect()
}

/// The functions and methods that the one `item` names calls, with the
/// ranges of the calls in its body.
pub(crate) fn outgoing_calls(
    workspace: &Workspace,
    module: usize,
    item: &CallHierarchyItem,
) -> Vec<CallHierarchyOutgoingCall> {
    let Some(source) = function_of_item(workspace, module, item) else {
        return Vec::new();
    };
    let full = &workspace.declaration(module, source).full;
    let mut callees = BTreeMap::<(usize, usize), Vec<SyntaxSpan>>::new();
    for occurrence in workspace.file(module).names.occurrences() {
        if !occurrence.callee
            || occurrence.span.start < full.start
            || full.end < occurrence.span.end
            || enclosing_function(workspace, module, &occurrence.span) != Some(source)
        {
            continue;
        }
        if let Resolved::Target(Target::Declared {
            module: callee_module,
            index,
        }) = workspace.resolve(module, occurrence)
            && is_function(workspace.declaration(callee_module, index).kind)
        {
            callees
                .entry((callee_module, index))
                .or_default()
                .push(occurrence.span.clone());
        }
    }
    let mut order = callees.keys().copied().collect::<Vec<_>>();
    sort_declarations(workspace, &mut order);
    let mut mappers = Mappers::new(workspace);
    order
        .into_iter()
        .map(|(callee_module, callee)| {
            let to = call_item(workspace, &mut mappers, callee_module, callee);
            let mapper = mappers.get(module);
            CallHierarchyOutgoingCall {
                to,
                from_ranges: callees[&(callee_module, callee)]
                    .iter()
                    .map(|span| mapper.range(span))
                    .collect(),
            }
        })
        .collect()
}

/// One lens per function and method declared in `module`, counting the calls
/// that resolve to it and listing them for `editor.action.showReferences`.
pub(crate) fn code_lenses(workspace: &Workspace, module: usize) -> Vec<CodeLens> {
    let file = workspace.file(module);
    let mut mappers = Mappers::new(workspace);
    let mut lenses = Vec::new();
    for (index, declared) in file.names.declarations().iter().enumerate() {
        if !is_function(declared.kind) {
            continue;
        }
        let mut calls = calls_of(workspace, &Target::Declared { module, index })
            .into_iter()
            .map(|(caller_module, span)| mappers.location(caller_module, &span))
            .collect::<Vec<_>>();
        calls.sort_by(|left, right| {
            left.uri.as_str().cmp(right.uri.as_str()).then_with(|| {
                (left.range.start.line, left.range.start.character)
                    .cmp(&(right.range.start.line, right.range.start.character))
            })
        });
        let mapper = mappers.get(module);
        lenses.push(CodeLens {
            range: mapper.range(&declared.selection),
            command: Some(Command {
                title: format!("{} incoming calls", calls.len()),
                command: "editor.action.showReferences".to_owned(),
                arguments: Some(vec![
                    serde_json::json!(file.uri()),
                    serde_json::json!(mapper.position(declared.selection.start)),
                    serde_json::json!(calls),
                ]),
            }),
            data: None,
        });
    }
    lenses
}

fn hierarchy_kind(kind: DeclaredKind) -> Option<SymbolKind> {
    match kind {
        DeclaredKind::Type => Some(SymbolKind::STRUCT),
        DeclaredKind::Table => Some(SymbolKind::CLASS),
        DeclaredKind::Protocol => Some(SymbolKind::INTERFACE),
        _ => None,
    }
}

fn type_item(
    workspace: &Workspace,
    mappers: &mut Mappers<'_>,
    module: usize,
    index: usize,
) -> Option<TypeHierarchyItem> {
    let file = workspace.file(module);
    let declared = workspace.declaration(module, index);
    if declared.owner.is_some() {
        return None;
    }
    let kind = hierarchy_kind(declared.kind)?;
    let mapper = mappers.get(module);
    Some(TypeHierarchyItem {
        name: declared.name.clone(),
        kind,
        tags: None,
        detail: analysis::declared_symbol(&file.symbols, &declared.selection)
            .and_then(|symbol| symbol.detail.clone()),
        uri: file.uri().clone(),
        range: mapper.range(&declared.full),
        selection_range: mapper.range(&declared.selection),
        data: Some(serde_json::json!({
            "scheme": MONIKER_SCHEME,
            "uri": file.uri().as_str(),
            "name": declared.key,
            "start": declared.selection.start,
            "end": declared.selection.end,
        })),
    })
}

/// The table, type or protocol declared at an item's selection range.
fn type_of_item(workspace: &Workspace, module: usize, item: &TypeHierarchyItem) -> Option<usize> {
    let index = declared_at_range(workspace, module, &item.selection_range)?;
    let declared = workspace.declaration(module, index);
    (declared.owner.is_none()
        && declared.name == item.name
        && hierarchy_kind(declared.kind) == Some(item.kind))
    .then_some(index)
}

/// The table, type or protocol named at `byte`, at its declaration or at any
/// reference that resolves to it.
pub(crate) fn prepare_type_hierarchy(
    workspace: &Workspace,
    module: usize,
    byte: usize,
) -> Option<Vec<TypeHierarchyItem>> {
    let (_, resolved) = workspace.resolve_at(module, byte)?;
    let Target::Declared {
        module: declared_module,
        index,
    } = resolved.target()?
    else {
        return None;
    };
    let mut mappers = Mappers::new(workspace);
    Some(vec![type_item(workspace, &mut mappers, declared_module, index)?])
}

fn implementations(declaration: &Declaration) -> Vec<&Implementation> {
    match declaration {
        Declaration::Table { members, .. } => members
            .iter()
            .filter_map(|member| match member {
                TableMember::Implementation { implementation, .. } => Some(implementation),
                _ => None,
            })
            .collect(),
        Declaration::Type {
            representation:
                TypeRepresentation::Alias { refinements: members, .. }
                | TypeRepresentation::Nominal { members },
            ..
        } => members
            .iter()
            .filter_map(|member| match member {
                TypeMember::Implementation { implementation, .. } => Some(implementation),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The protocols the table or type `index` implements, each through a
/// protocol path that resolves to exactly one protocol declaration.
fn implemented_protocols(workspace: &Workspace, module: usize, index: usize) -> Vec<(usize, usize)> {
    let file = workspace.file(module);
    let full = &workspace.declaration(module, index).full;
    let Some(item) = file
        .parse
        .value
        .items
        .iter()
        .find(|item| item.span.start == full.start && item.span.end == full.end)
    else {
        return Vec::new();
    };
    let mut protocols = Vec::new();
    for implementation in implementations(&item.declaration) {
        let TypeExpr::Name { path, span, .. } = &implementation.protocol else {
            continue;
        };
        // The protocol is named by the last segment of its path.
        let Some(occurrence) = file.names.occurrences().iter().find(|occurrence| {
            span.start <= occurrence.span.start
                && occurrence.span.end <= span.end
                && matches!(
                    &occurrence.role,
                    Role::Path { path: keys, index } if keys.len() == path.len() && index + 1 == keys.len()
                )
        }) else {
            continue;
        };
        if let Resolved::Target(Target::Declared {
            module: protocol_module,
            index: protocol,
        }) = workspace.resolve(module, occurrence)
            && workspace.declaration(protocol_module, protocol).kind == DeclaredKind::Protocol
        {
            protocols.push((protocol_module, protocol));
        }
    }
    protocols.sort_unstable();
    protocols.dedup();
    protocols
}

/// The protocols the table or type `item` names implements.
pub(crate) fn supertypes(
    workspace: &Workspace,
    module: usize,
    item: &TypeHierarchyItem,
) -> Vec<TypeHierarchyItem> {
    let Some(index) = type_of_item(workspace, module, item) else {
        return Vec::new();
    };
    let mut protocols = implemented_protocols(workspace, module, index);
    sort_declarations(workspace, &mut protocols);
    let mut mappers = Mappers::new(workspace);
    protocols
        .into_iter()
        .filter_map(|(protocol_module, protocol)| {
            type_item(workspace, &mut mappers, protocol_module, protocol)
        })
        .collect()
}

/// The tables and types that implement the protocol `item` names.
pub(crate) fn subtypes(
    workspace: &Workspace,
    module: usize,
    item: &TypeHierarchyItem,
) -> Vec<TypeHierarchyItem> {
    let Some(index) = type_of_item(workspace, module, item) else {
        return Vec::new();
    };
    if workspace.declaration(module, index).kind != DeclaredKind::Protocol {
        return Vec::new();
    }
    let mut implementors = Vec::new();
    for (candidate_module, entry) in workspace.modules.iter().enumerate() {
        for (candidate, declared) in entry.file.names.declarations().iter().enumerate() {
            if declared.owner.is_none()
                && matches!(declared.kind, DeclaredKind::Table | DeclaredKind::Type)
                && implemented_protocols(workspace, candidate_module, candidate)
                    .contains(&(module, index))
            {
                implementors.push((candidate_module, candidate));
            }
        }
    }
    sort_declarations(workspace, &mut implementors);
    let mut mappers = Mappers::new(workspace);
    implementors
        .into_iter()
        .filter_map(|(implementor_module, implementor)| {
            type_item(workspace, &mut mappers, implementor_module, implementor)
        })
        .collect()
}

fn moniker_kind(kind: Option<EditorSymbolKind>) -> &'static str {
    match kind {
        Some(EditorSymbolKind::Function) => "function",
        Some(EditorSymbolKind::Type) => "type",
        Some(EditorSymbolKind::Enum) => "enum",
        Some(EditorSymbolKind::EnumVariant) => "enum-member",
        Some(EditorSymbolKind::Table) => "table",
        Some(EditorSymbolKind::Protocol) => "protocol",
        Some(EditorSymbolKind::Method) => "method",
        Some(EditorSymbolKind::Field) => "field",
        Some(EditorSymbolKind::Constant) => "constant",
        Some(EditorSymbolKind::Dimension) => "dimension",
        Some(EditorSymbolKind::Unit) => "unit",
        None => "symbol",
    }
}

/// The moniker of the name at `byte`: a document-unique one for a local
/// binding, a project-unique one for a declaration. A public declaration is
/// exported where it is declared and imported from other modules.
pub(crate) fn moniker(workspace: &Workspace, module: usize, byte: usize) -> Option<Vec<Moniker>> {
    let (span, resolved) = workspace.resolve_at(module, byte)?;
    let moniker = match resolved.target()? {
        Target::Local {
            module: binding_module,
            binding,
        } => {
            let binding = workspace.file(binding_module).names.binding(binding);
            Moniker {
                scheme: MONIKER_SCHEME.to_owned(),
                identifier: format!(
                    "{}#local:{}:{}",
                    workspace.file(binding_module).uri().as_str(),
                    binding.selection.start,
                    binding.key
                ),
                unique: UniquenessLevel::Document,
                kind: Some(MonikerKind::Local),
            }
        }
        Target::Declared {
            module: declared_module,
            index,
        } => {
            let file = workspace.file(declared_module);
            let declared = workspace.declaration(declared_module, index);
            let at_declaration = declared_module == module
                && declared.selection.start == span.start
                && declared.selection.end == span.end;
            let kind = if !declared.public {
                MonikerKind::Local
            } else if at_declaration {
                MonikerKind::Export
            } else if declared_module != module {
                MonikerKind::Import
            } else {
                MonikerKind::Local
            };
            let name = match declared.owner {
                Some(owner) => format!(
                    "{}.{}",
                    workspace.declaration(declared_module, owner).key,
                    declared.key
                ),
                None => declared.key.clone(),
            };
            let symbol = analysis::declared_symbol(&file.symbols, &declared.selection);
            Moniker {
                scheme: MONIKER_SCHEME.to_owned(),
                identifier: format!(
                    "{}#{}:{}",
                    file.uri().as_str(),
                    moniker_kind(symbol.map(|symbol| symbol.kind)),
                    name
                ),
                unique: UniquenessLevel::Project,
                kind: Some(kind),
            }
        }
        Target::Module(_) | Target::Standard { .. } => return None,
    };
    Some(vec![moniker])
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::documents::Document;
    use crate::standard::StandardLibrary;
    use crate::workspace::{FileAnalysis, Module};

    fn file(uri: &str, text: &str) -> Arc<FileAnalysis> {
        let parse = orna_syntax_v1::parse_module(text);
        assert!(parse.diagnostics.is_empty(), "{:#?}", parse.diagnostics);
        Arc::new(FileAnalysis::new(Document::new(
            uri.parse().unwrap(),
            text.to_owned(),
            1,
        )))
    }

    fn title(lenses: &[CodeLens], text: &str, name: &str) -> String {
        let start = text.find(&format!("fn {name}(")).unwrap() + "fn ".len();
        let line = text[..start].matches('\n').count() as u32;
        lenses
            .iter()
            .find(|lens| lens.range.start.line == line)
            .and_then(|lens| lens.command.as_ref())
            .map(|command| command.title.clone())
            .unwrap_or_else(|| panic!("no lens for {name}"))
    }

    /// U03-12: the parser keeps a copy of a `for` body among the loop's
    /// arms; a call inside the loop is still one call.
    #[test]
    fn a_call_inside_a_for_loop_counts_once() {
        const TEXT: &str = "pub fn step(value: Int): Int = value + 1;\n\
            pub fn total(values: [Int]): Int {\n    let sum = 0;\n    \
            for value in values {\n        sum = step(value);\n    }\n    sum\n}\n";
        let workspace = Workspace::detached(vec![file("file:///w/loop.orna", TEXT)]);
        let lenses = code_lenses(&workspace, 0);
        assert_eq!(title(&lenses, TEXT, "step"), "1 incoming calls");
        assert_eq!(title(&lenses, TEXT, "total"), "0 incoming calls");

        let total = prepare_call_hierarchy(&workspace, 0, TEXT.find("total").unwrap()).unwrap();
        let outgoing = outgoing_calls(&workspace, 0, &total[0]);
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].to.name, "step");
        assert_eq!(outgoing[0].from_ranges.len(), 1);

        let step = prepare_call_hierarchy(&workspace, 0, TEXT.rfind("step").unwrap()).unwrap();
        let incoming = incoming_calls(&workspace, 0, &step[0]);
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].from.name, "total");
        assert_eq!(incoming[0].from_ranges.len(), 1);
    }

    /// A protocol imported from another module is a supertype, and an
    /// imported function's calls are counted in its own module.
    #[test]
    fn hierarchies_follow_imports_across_modules() {
        const SHAPES: &str = "pub protocol Renderable {\n    fn render(self): Str;\n}\n\
            pub fn label(): Str = \"label\";\n";
        const MAIN: &str = "use shapes.{Renderable, label};\n\
            pub type Page {\n    pub title: Str,\n    impl Renderable {\n        \
            fn render(self): Str = label();\n    }\n}\n";
        let workspace = Workspace::project(
            vec![
                Module::project(file("file:///p/main.orna", MAIN), "main.orna"),
                Module::project(file("file:///p/shapes.orna", SHAPES), "shapes.orna"),
            ],
            StandardLibrary::none(),
            None,
        );
        let page = prepare_type_hierarchy(&workspace, 0, MAIN.find("Page").unwrap()).unwrap();
        let supertypes = supertypes(&workspace, 0, &page[0]);
        assert_eq!(supertypes.len(), 1);
        assert_eq!(supertypes[0].name, "Renderable");
        assert_eq!(supertypes[0].uri.as_str(), "file:///p/shapes.orna");

        let renderable = prepare_type_hierarchy(&workspace, 1, SHAPES.find("Renderable").unwrap())
            .unwrap();
        let subtypes = subtypes(&workspace, 1, &renderable[0]);
        assert_eq!(subtypes.len(), 1);
        assert_eq!(subtypes[0].name, "Page");

        let lenses = code_lenses(&workspace, 1);
        assert_eq!(title(&lenses, SHAPES, "label"), "1 incoming calls");
        let label = prepare_call_hierarchy(&workspace, 1, SHAPES.find("label").unwrap()).unwrap();
        let incoming = incoming_calls(&workspace, 1, &label[0]);
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].from.name, "render");
        assert_eq!(incoming[0].from.kind, SymbolKind::METHOD);
    }
}
