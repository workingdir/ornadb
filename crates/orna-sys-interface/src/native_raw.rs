//! The sys descriptor read from the hand-written `sys.orna` declarations. `native_raw_document` derives the same
//! `RawSystemApi` that `raw_document` decodes from the catalogue JSON, so both sources meet one validation seam.
//! Each field below names the `sys.orna` source it is read from; a field with no source is a constant and is
//! named as such where it is defined.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax::{
    Declaration, Expr, GenericParameter, Item, LiteralKind, NativeEffectSet, Pattern, TypeExpr,
    TypeMember, TypeRepresentation, parse_interface,
};

use crate::{
    InvalidSysInterface, RawCounts, RawField, RawFunction, RawReferenceAlias, RawRelation,
    RawRemovedName, RawSingleton, RawSystemApi, RawValueType,
};

pub const SYS_ORNA: &str = include_str!("../../orna-hir/interface/sys.orna");

/// The namespace modules that hold relation handles. Each file's handles are `sys.<module>.<name>`.
pub const NAMESPACE_FILES: [(&str, &str); 5] = [
    (
        "catalog",
        include_str!("../../orna-hir/interface/sys/catalog.orna"),
    ),
    ("git", include_str!("../../orna-hir/interface/sys/git.orna")),
    (
        "storage",
        include_str!("../../orna-hir/interface/sys/storage.orna"),
    ),
    (
        "history",
        include_str!("../../orna-hir/interface/sys/history.orna"),
    ),
    (
        "build",
        include_str!("../../orna-hir/interface/sys/build.orna"),
    ),
];

/// Type names the retired `scripts/gen_interface.py` left bare in its JSON (its `BUILTINS`). These are declared in
/// `sys.orna` and the renderer must not qualify them.
const GENERATOR_BUILTINS: [&str; 2] = ["Unit", "Query"];

/// `declared` with the generator builtins added back for type positions outside signatures: the JSON qualifies them
/// in reference aliases, relations and value-type fields (`sys.RowRef<sys.Unit>`, `Relation<sys.Query>`), while
/// signatures leave them bare (`query: Query<T>`, `: Unit`).
fn declared_for_types(declared: &BTreeSet<String>) -> BTreeSet<String> {
    declared
        .iter()
        .cloned()
        .chain(GENERATOR_BUILTINS.iter().map(|name| (*name).to_owned()))
        .collect()
}

/// The `diagnostic` of a removed callable that carries no `formerly` clause. The `removed` clause has no diagnostic
/// word, so the catalogue's version-incompatibility code is the one every such callable reports.
const REMOVED_DIAGNOSTIC: &str = "sys.version.incompatible";

/// Names declared at the top level of `sys.orna` that the JSON spells with a `sys.` prefix.
pub fn declared_names(items: &[Item]) -> BTreeSet<String> {
    items
        .iter()
        .filter_map(|item| match &item.declaration {
            Declaration::Enum { name, .. }
            | Declaration::Type { name, .. }
            | Declaration::NativeType { name, .. }
            | Declaration::Protocol { name, .. }
            | Declaration::Dimension { name, .. }
            | Declaration::Unit { name, .. } => Some(name.clone()),
            _ => None,
        })
        .filter(|name| !GENERATOR_BUILTINS.contains(&name.as_str()))
        .collect()
}

pub fn render_generics(generics: &[GenericParameter]) -> String {
    if generics.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = generics
        .iter()
        .map(|generic| generic.name.as_str())
        .collect();
    format!("<{}>", names.join(", "))
}

pub fn render_type(ty: &TypeExpr, declared: &BTreeSet<String>) -> String {
    match ty {
        TypeExpr::Name {
            path, arguments, ..
        } => {
            let head = path.join(".");
            let qualified = if declared.contains(&head) {
                format!("sys.{head}")
            } else {
                head
            };
            if arguments.is_empty() {
                qualified
            } else {
                let rendered: Vec<String> = arguments
                    .iter()
                    .map(|arg| render_type(arg, declared))
                    .collect();
                format!("{qualified}<{}>", rendered.join(", "))
            }
        }
        TypeExpr::Optional { inner, .. } => format!("{}?", render_type(inner, declared)),
        TypeExpr::List { inner, .. } => format!("[{}]", render_type(inner, declared)),
        _ => "<unrendered-type>".to_owned(),
    }
}

/// The dotted path of a name or field chain, e.g. `current.snapshot`.
fn render_path(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{name}", render_path(base)?)),
        _ => None,
    }
}

fn render_default(expr: &Expr) -> String {
    match expr {
        Expr::Name { text, .. } if text == "null" || text == "true" || text == "false" => {
            text.clone()
        }
        Expr::Literal { text, kind, .. } => match kind {
            LiteralKind::Null | LiteralKind::Boolean | LiteralKind::Integer => text.clone(),
            _ => "<unrendered-default>".to_owned(),
        },
        other => render_path(other).map_or_else(
            || "<unrendered-default>".to_owned(),
            |path| format!("sys.{path}"),
        ),
    }
}

/// The JSON head of a label without its overload or generic part: `sys.snapshot(BranchRef)` -> `sys.snapshot`.
fn label_head(label: &str) -> &str {
    label
        .find(['(', '<'])
        .map_or(label, |index| &label[..index])
}

/// Renders a `pub native fn` into the JSON `signature` grammar under the JSON head of its label.
pub fn render_native(
    label: &str,
    signature: &orna_syntax::FunctionSignature,
    declared: &BTreeSet<String>,
) -> String {
    let parameters: Vec<String> = signature
        .parameters
        .iter()
        .map(|parameter| {
            let name = match &parameter.pattern {
                Pattern::Name(name, _) => name.as_str(),
                _ => "<unrendered-pattern>",
            };
            let annotation = parameter
                .annotation
                .as_ref()
                .map_or_else(|| "<missing>".to_owned(), |ty| render_type(ty, declared));
            match &parameter.default {
                Some(default) => format!("{name}: {annotation} = {}", render_default(default)),
                None => format!("{name}: {annotation}"),
            }
        })
        .collect();
    let result = signature
        .result
        .as_ref()
        .map_or_else(|| "()".to_owned(), |ty| render_type(ty, declared));
    format!(
        "fn {}{}({}): {result}",
        label_head(label),
        render_generics(&signature.generics),
        parameters.join(", ")
    )
}

/// The catalogue class a native effect set stands for: `table_read` is `read`, `storage_admin` is `admin`,
/// `runtime_admin` is `invoke`. This is the only mapping the retired `gen_interface.py` used.
pub fn catalogue_class(effects: Option<&NativeEffectSet>) -> String {
    let Some(NativeEffectSet::Classes { classes, .. }) = effects else {
        return "<unrendered-effect>".to_owned();
    };
    classes
        .iter()
        .map(|class| match class.text.as_str() {
            "table_read" => "read",
            "storage_admin" => "admin",
            "runtime_admin" => "invoke",
            _ => "<unknown-effect>",
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// The label of a `pub native fn`: its `spelling` clause, or `sys.<name>` without one.
pub fn native_label(item: &Item, name: &str) -> String {
    item.clauses
        .spelling
        .as_ref()
        .map_or_else(|| format!("sys.{name}"), |spelling| spelling.text.clone())
}

/// Splits a declaration's doc lines into its prose lines and the `- CODE` items of its `Failure codes:` section. The
/// section runs from its heading to the first doc line that is not a list item.
pub(super) fn split_failure_codes(docs: &[String]) -> (Vec<String>, Vec<String>) {
    let mut prose = Vec::new();
    let mut codes = Vec::new();
    let mut in_codes = false;
    for line in docs {
        let trimmed = line.trim();
        match trimmed.strip_prefix("- ") {
            Some(code) if in_codes => codes.push(code.trim().to_owned()),
            _ if trimmed == "Failure codes:" => in_codes = true,
            _ => {
                in_codes = false;
                prose.push(line.clone());
            }
        }
    }
    (prose, codes)
}

pub(super) fn flatten(parts: &[String]) -> String {
    parts
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The labelled prose lines that open a doc block: `Kind:`, `Contract:`, and so on.
const LABELS: [&str; 7] = [
    "Kind",
    "Invariants",
    "Contract",
    "Preconditions",
    "Ownership",
    "Snapshot rule",
    "Availability",
];

/// The label a doc line opens, if the line opens one.
fn opened_label(line: &str) -> Option<&'static str> {
    let trimmed = line.trim();
    LABELS.iter().copied().find(|label| {
        trimmed
            .strip_prefix(label)
            .is_some_and(|rest| rest.starts_with(':'))
    })
}

/// Whether a doc line opens a new section: a labelled line or the `Failure codes:` marker that ends prose.
fn opens_section(line: &str) -> bool {
    opened_label(line).is_some() || line.trim() == "Failure codes:"
}

/// The purpose of a declaration: the doc prose before its first labelled line, flattened.
fn purpose(docs: &[String]) -> String {
    let prose: Vec<String> = docs
        .iter()
        .take_while(|line| !opens_section(line))
        .cloned()
        .collect();
    flatten(&prose)
}

/// The flattened text of a labelled doc block: the text after `Label:` and every following line up to the next blank
/// line or labelled line. `None` when the declaration carries no such label.
fn labelled(docs: &[String], label: &str) -> Option<String> {
    let start = docs
        .iter()
        .position(|line| opened_label(line) == Some(label))?;
    let mut parts = Vec::new();
    for (index, line) in docs[start..].iter().enumerate() {
        let trimmed = line.trim();
        if index > 0 && (trimmed.is_empty() || opens_section(line) || trimmed.starts_with("- ")) {
            break;
        }
        if index == 0 {
            parts.push(trimmed[label.len() + 1..].to_owned());
        } else {
            parts.push(trimmed.to_owned());
        }
    }
    Some(flatten(&parts))
}

/// The `- item` lines of a labelled doc block, in order.
fn labelled_items(docs: &[String], label: &str) -> Vec<String> {
    let Some(start) = docs
        .iter()
        .position(|line| opened_label(line) == Some(label))
    else {
        return Vec::new();
    };
    docs[start + 1..]
        .iter()
        .map(|line| line.trim())
        .take_while(|line| line.starts_with("- "))
        .map(|line| line["- ".len()..].trim().to_owned())
        .collect()
}

/// Whether a type declaration is a relation row type: the only ones carrying a `key` or `availability` clause.
fn is_relation(item: &Item) -> bool {
    !item.clauses.key.is_empty() || item.clauses.availability.is_some()
}

/// The raw descriptor document derived from `sys.orna` and its namespace modules.
pub fn native_raw_document() -> Result<RawSystemApi, InvalidSysInterface> {
    let parsed = parse_interface(SYS_ORNA);
    if !parsed.diagnostics.is_empty() {
        return Err(InvalidSysInterface);
    }
    let items = parsed.value.items;
    let declared = declared_names(&items);
    let handles = grouped_handles(&items, &declared)?;
    let mut collected = Collected::default();
    for item in &items {
        collected
            .failure_codes
            .extend(split_failure_codes(&item.docs).1);
        collect_removed(item, &mut collected.removed_names);
        collected.collect(item, &declared, &handles);
    }
    Ok(collected.finish())
}

/// The declarations of `sys.orna`, gathered by kind in source order.
#[derive(Default)]
struct Collected {
    singletons: Vec<RawSingleton>,
    opaque_identifiers: Vec<String>,
    reference_aliases: Vec<RawReferenceAlias>,
    value_types: Vec<RawValueType>,
    enums: BTreeMap<String, Vec<String>>,
    relations: Vec<RawRelation>,
    functions: Vec<RawFunction>,
    failure_codes: Vec<String>,
    removed_names: BTreeMap<String, RawRemovedName>,
}

impl Collected {
    fn collect(
        &mut self,
        item: &Item,
        declared: &BTreeSet<String>,
        handles: &BTreeMap<String, String>,
    ) {
        match &item.declaration {
            Declaration::Enum { name, variants, .. }
                if item.clauses.removed.is_none()
                    && matches!(item.visibility, orna_syntax::Visibility::Public { .. }) =>
            {
                let variants = variants
                    .iter()
                    .map(|variant| variant.name.clone())
                    .collect();
                self.enums.insert(format!("sys.{name}"), variants);
            }
            Declaration::Native { signature, effects } if item.clauses.removed.is_none() => {
                let label = native_label(item, &signature.name);
                self.functions.push(RawFunction {
                    signature: render_native(&label, signature, declared),
                    effect: catalogue_class(effects.as_ref()),
                    purpose: purpose(&item.docs),
                    since: item.clauses.since.as_ref().map(|since| since.text.clone()),
                    contract: labelled(&item.docs, "Contract"),
                    preconditions: labelled(&item.docs, "Preconditions"),
                    ownership: labelled(&item.docs, "Ownership"),
                    snapshot_rule: labelled(&item.docs, "Snapshot rule"),
                    name: label,
                });
            }
            Declaration::NativeStatic {
                name,
                annotation: Some(annotation),
                ..
            } if !render_type(annotation, declared).starts_with("Relation<") => {
                self.singletons.push(RawSingleton {
                    name: native_label(item, name),
                    ty: render_type(annotation, declared),
                    availability: labelled(&item.docs, "Availability").unwrap_or_default(),
                });
            }
            Declaration::Type {
                name,
                generics,
                representation,
            } => self.collect_type(item, name, generics, representation, declared, handles),
            _ => {}
        }
    }

    fn collect_type(
        &mut self,
        item: &Item,
        name: &str,
        generics: &[GenericParameter],
        representation: &TypeRepresentation,
        declared: &BTreeSet<String>,
        handles: &BTreeMap<String, String>,
    ) {
        let qualified = format!("sys.{name}");
        let type_declared = declared_for_types(declared);
        if is_relation(item) {
            self.relations.push(relation(
                item,
                name,
                representation,
                &type_declared,
                handles,
            ));
            return;
        }
        if let TypeRepresentation::Alias { ty, .. } = representation {
            let definition = render_type(ty, &type_declared);
            if let Some(target) = definition
                .strip_prefix("sys.RowRef<")
                .and_then(|rest| rest.strip_suffix('>'))
            {
                self.reference_aliases.push(RawReferenceAlias {
                    name: qualified,
                    target: target.to_owned(),
                    definition,
                });
            }
            return;
        }
        if labelled(&item.docs, "Kind").is_none() {
            self.opaque_identifiers.push(qualified);
            return;
        }
        self.value_types.push(RawValueType {
            name: format!("{qualified}{}", render_generics(generics)),
            kind: labelled(&item.docs, "Kind").unwrap_or_default(),
            purpose: purpose(&item.docs),
            type_parameters: generics
                .iter()
                .map(|generic| generic.name.clone())
                .collect(),
            fields: fields(representation, &type_declared),
            invariants: labelled_items(&item.docs, "Invariants"),
        });
    }

    fn finish(self) -> RawSystemApi {
        let counts = RawCounts {
            singletons: self.singletons.len(),
            opaque_identifiers: self.opaque_identifiers.len(),
            reference_aliases: self.reference_aliases.len(),
            value_types: self.value_types.len(),
            enums: self.enums.len(),
            relations: self.relations.len(),
            functions: self.functions.len(),
            failure_codes: self.failure_codes.len(),
        };
        RawSystemApi {
            counts,
            singletons: self.singletons,
            opaque_identifiers: self.opaque_identifiers,
            reference_aliases: self.reference_aliases,
            value_types: self.value_types,
            enums: self.enums,
            relations: self.relations,
            functions: self.functions,
            failure_codes: self.failure_codes,
            removed_names: self.removed_names,
        }
    }
}

/// The fields of a nominal record, in source order. An alias declares no fields.
fn fields(representation: &TypeRepresentation, declared: &BTreeSet<String>) -> Vec<RawField> {
    let TypeRepresentation::Nominal { members } = representation else {
        return Vec::new();
    };
    members
        .iter()
        .filter_map(|member| match member {
            TypeMember::Field { name, ty, .. } => Some(RawField {
                name: name.clone(),
                ty: render_type(ty, declared),
            }),
            _ => None,
        })
        .collect()
}

fn relation(
    item: &Item,
    name: &str,
    representation: &TypeRepresentation,
    declared: &BTreeSet<String>,
    handles: &BTreeMap<String, String>,
) -> RawRelation {
    let qualified = format!("sys.{name}");
    let key_fields: Vec<String> = item
        .clauses
        .key
        .iter()
        .map(|key| key.text.clone())
        .collect();
    RawRelation {
        grouped_handle: handles.get(&qualified).cloned().unwrap_or_default(),
        kind: "relation".to_owned(),
        availability: item
            .clauses
            .availability
            .as_ref()
            .map(|availability| availability.text.clone())
            .unwrap_or_default(),
        key: key_fields.join(" + "),
        purpose: purpose(&item.docs),
        key_fields,
        invariants: labelled_items(&item.docs, "Invariants"),
        fields: fields(representation, declared),
        // Every relation is read-only in `sys.orna`: no declaration carries a writable source. The guard test
        // compares this constant with the catalogue, so a relation that becomes writable must add its source.
        writable: false,
        reference_type: format!("sys.{name}Ref"),
        name: qualified,
    }
}

/// Each relation row type's grouped handle, keyed by the qualified row type. A namespace module's `Relation<T>`
/// static names `sys.<module>.<name>`; the `Relation` fields of `RuntimeView` name `sys.rt.<field>`.
fn grouped_handles(
    items: &[Item],
    declared: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, InvalidSysInterface> {
    let types = declared_for_types(declared);
    let mut handles = BTreeMap::new();
    for (module, text) in NAMESPACE_FILES {
        let parsed = parse_interface(text);
        if !parsed.diagnostics.is_empty() {
            return Err(InvalidSysInterface);
        }
        for item in &parsed.value.items {
            if let Declaration::NativeStatic {
                name,
                annotation: Some(annotation),
                ..
            } = &item.declaration
            {
                insert_relation_handle(
                    &mut handles,
                    &render_type(annotation, &types),
                    format!("sys.{module}.{name}"),
                );
            }
        }
    }
    for item in items {
        if let Declaration::Type {
            name,
            representation,
            ..
        } = &item.declaration
            && name == "RuntimeView"
            && let TypeRepresentation::Nominal { members } = representation
        {
            for member in members {
                if let TypeMember::Field {
                    name: field, ty, ..
                } = member
                {
                    insert_relation_handle(
                        &mut handles,
                        &render_type(ty, &types),
                        format!("sys.rt.{field}"),
                    );
                }
            }
        }
    }
    Ok(handles)
}

/// Records `handle` under the row type of a `Relation<T>` rendering. Other renderings name no relation.
fn insert_relation_handle(handles: &mut BTreeMap<String, String>, rendered: &str, handle: String) {
    if let Some(row) = rendered
        .strip_prefix("Relation<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        handles.insert(row.to_owned(), handle);
    }
}

/// `formerly` and `removed` callables, keyed by the name they were known under. A `formerly` clause names the
/// replacement and its diagnostic; a removed callable carries only the default diagnostic. Removed types are not
/// catalogue entries and are skipped.
fn collect_removed(item: &Item, removed: &mut BTreeMap<String, RawRemovedName>) {
    let label = match &item.declaration {
        Declaration::Native { signature, .. } => native_label(item, &signature.name),
        Declaration::NativeStatic { name, .. } => native_label(item, name),
        _ => return,
    };
    if let Some(formerly) = &item.clauses.formerly {
        removed.insert(
            formerly.name.text.clone(),
            RawRemovedName {
                replacement: Some(label.clone()),
                diagnostic: formerly.diagnostic.as_ref().map_or_else(
                    || REMOVED_DIAGNOSTIC.to_owned(),
                    |diagnostic| diagnostic.text.clone(),
                ),
            },
        );
    }
    if let Some(retired) = &item.clauses.removed
        && matches!(item.declaration, Declaration::Native { .. })
    {
        removed.insert(
            label,
            RawRemovedName {
                replacement: retired
                    .replacement
                    .as_ref()
                    .map(|replacement| replacement.text.clone()),
                diagnostic: REMOVED_DIAGNOSTIC.to_owned(),
            },
        );
    }
}
