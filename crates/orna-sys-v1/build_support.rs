use serde_json::Value;
use syn::{
    Expr, ForeignItem, ImplItem, ItemFn, Lit, TraitItem,
    visit::{self, Visit},
};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct Function {
    pub method: String,
    pub metadata: Value,
}

#[derive(Default)]
pub struct Collector {
    pub functions: Vec<Function>,
    pub errors: Vec<String>,
}

impl Collector {
    pub fn collect_source(&mut self, source_name: &str, source: &str) {
        match syn::parse_file(source) {
            Ok(syntax) => self.visit_file(&syntax),
            Err(error) => self
                .errors
                .push(format!("{source_name}: invalid Rust source: {error}")),
        }
    }

    fn collect_method(&mut self, method: &syn::Signature, attrs: &[syn::Attribute]) {
        let annotations = attrs
            .iter()
            .filter(|attribute| is_ornasys(attribute))
            .collect::<Vec<_>>();
        if annotations.len() > 1 {
            self.errors.push(format!(
                "{} has multiple #[ornasys] attributes; a method declares one portable API function",
                method.ident
            ));
            return;
        }
        if let Some(attribute) = annotations.first() {
            match parse_function_attribute(attribute) {
                Ok(metadata) => self.functions.push(Function {
                    method: method.ident.to_string(),
                    metadata,
                }),
                Err(error) => self.errors.push(format!("{}: {error}", method.ident)),
            }
        }
    }

    fn reject_non_method(&mut self, name: &str, attrs: &[syn::Attribute]) {
        if attrs.iter().any(is_ornasys) {
            self.errors.push(format!(
                "{name} is annotated with #[ornasys] but is not a method; annotate a trait or impl method"
            ));
        }
    }
}

impl<'ast> Visit<'ast> for Collector {
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        for member in &item.items {
            match member {
                ImplItem::Fn(method) => self.collect_method(&method.sig, &method.attrs),
                ImplItem::Const(item) => self.reject_non_method("associated const", &item.attrs),
                ImplItem::Type(item) => self.reject_non_method("associated type", &item.attrs),
                _ => {}
            }
        }
        visit::visit_item_impl(self, item);
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        for member in &item.items {
            match member {
                TraitItem::Fn(method) => self.collect_method(&method.sig, &method.attrs),
                TraitItem::Const(item) => self.reject_non_method("trait associated const", &item.attrs),
                TraitItem::Type(item) => self.reject_non_method("trait associated type", &item.attrs),
                _ => {}
            }
        }
        visit::visit_item_trait(self, item);
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.reject_non_method("free function", &item.attrs);
        visit::visit_item_fn(self, item);
    }

    fn visit_foreign_item(&mut self, item: &'ast ForeignItem) {
        match item {
            ForeignItem::Fn(item) => self.reject_non_method("foreign function", &item.attrs),
            ForeignItem::Static(item) => self.reject_non_method("foreign static", &item.attrs),
            ForeignItem::Type(item) => self.reject_non_method("foreign type", &item.attrs),
            _ => {}
        }
        visit::visit_foreign_item(self, item);
    }
}

pub fn is_ornasys(attribute: &syn::Attribute) -> bool {
    attribute
        .path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "ornasys")
}

pub fn parse_function_attribute(attribute: &syn::Attribute) -> Result<Value, String> {
    let mut function_json = None;
    attribute
        .parse_nested_meta(|meta| {
            if !meta.path.is_ident("function") {
                return Err(meta.error("expected `function = \"<JSON>\"`"));
            }
            let value = meta.value()?;
            let expression: Expr = value.parse()?;
            let Expr::Lit(expression) = expression else {
                return Err(meta.error("function metadata must be a string literal"));
            };
            let Lit::Str(value) = expression.lit else {
                return Err(meta.error("function metadata must be a string literal"));
            };
            if function_json.replace(value.value()).is_some() {
                return Err(meta.error("only one function metadata value is allowed"));
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    let source = function_json.ok_or_else(|| "missing function JSON metadata".to_owned())?;
    let metadata: Value = serde_json::from_str(&source)
        .map_err(|error| format!("invalid function JSON metadata: {error}"))?;
    validate_function_metadata(&metadata)?;
    Ok(metadata)
}

pub fn validate_function_metadata(metadata: &Value) -> Result<(), String> {
    let object = metadata
        .as_object()
        .ok_or_else(|| "function metadata must be a JSON object".to_owned())?;
    // These are the fields consumed by semantic SystemApi. Rejecting unknown
    // keys catches annotation typos that serde would otherwise silently drop.
    const REQUIRED: [&str; 4] = ["name", "effect", "signature", "purpose"];
    const OPTIONAL: [&str; 4] = ["contract", "preconditions", "ownership", "snapshot_rule"];

    for field in REQUIRED {
        let Some(value) = object.get(field).and_then(Value::as_str) else {
            return Err(format!("function metadata requires string field `{field}`"));
        };
        if value.trim().is_empty() {
            return Err(format!("function metadata field `{field}` must not be blank"));
        }
    }
    for field in OPTIONAL {
        if let Some(value) = object.get(field) {
            let Some(value) = value.as_str() else {
                return Err(format!("function metadata field `{field}` must be a string"));
            };
            if value.trim().is_empty() {
                return Err(format!("function metadata field `{field}` must not be blank"));
            }
        }
    }
    if let Some(field) = object
        .keys()
        .find(|field| !REQUIRED.contains(&field.as_str()) && !OPTIONAL.contains(&field.as_str()))
    {
        return Err(format!("unknown function metadata field `{field}`"));
    }

    let name = object["name"].as_str().expect("required name validated");
    if !name.starts_with("sys.") || name.chars().any(char::is_whitespace) {
        return Err("function metadata `name` must be a whitespace-free sys API label".to_owned());
    }
    let signature = object["signature"]
        .as_str()
        .expect("required signature validated");
    let signature_identity = parse_signature_identity(signature)?;
    if !valid_function_label(name, &signature_identity) {
        return Err(format!(
            "function label `{name}` does not match callable shape `{}`",
            signature_identity.name,
        ));
    }
    let effect = object["effect"].as_str().expect("required effect validated");
    if !matches!(effect, "read" | "invoke" | "admin") {
        return Err(format!("unknown system API effect `{effect}`"));
    }

    let requires_contract = name.starts_with("sys.admin.");
    let requires_preconditions = FUNCTIONS_WITH_PRECONDITIONS.contains(&name);
    let requires_ownership = matches!(name, "sys.start(Value)" | "sys.start<T>");
    let requires_snapshot_rule = matches!(
        name,
        "sys.invoke(Value)" | "sys.invoke<T>" | "sys.start(Value)" | "sys.start<T>"
    );
    for (field, required) in [
        ("contract", requires_contract),
        ("preconditions", requires_preconditions),
        ("ownership", requires_ownership),
        ("snapshot_rule", requires_snapshot_rule),
    ] {
        if object.contains_key(field) != required {
            return Err(format!(
                "function `{name}` requires metadata field `{field}`={required}"
            ));
        }
    }

    // Freeze the published 1.0 effect partition at generation time. The
    // semantic loader applies the same surface rule before calls are admitted.
    let expected_effect = if name.starts_with("sys.admin.plan_checkout") {
        "read"
    } else if name.starts_with("sys.admin.") {
        "admin"
    } else if matches!(
        name,
        "sys.invoke(Value)" | "sys.invoke<T>" | "sys.start(Value)" | "sys.start<T>"
            | "sys.await" | "sys.cancel"
    ) {
        "invoke"
    } else {
        "read"
    };
    if effect != expected_effect {
        return Err(format!(
            "function `{name}` requires effect `{expected_effect}`, found `{effect}`"
        ));
    }
    Ok(())
}

// These are the 1.0 callable labels whose checkout/recovery contract requires
// explicit optimistic preconditions. Keep this list aligned with the semantic
// SystemApi loader; labels are exact so overload metadata cannot bleed across.
const FUNCTIONS_WITH_PRECONDITIONS: &[&str] = &[
    "sys.admin.checkout(SnapshotRef)",
    "sys.admin.checkout(CommitRef)",
    "sys.admin.checkout(BranchRef)",
    "sys.admin.checkout(TagRef)",
    "sys.admin.checkout(GitOid)",
    "sys.admin.checkout(Str)",
    "sys.admin.create_branch(SnapshotRef)",
    "sys.admin.create_branch(CommitRef)",
    "sys.admin.create_branch(BranchRef)",
    "sys.admin.create_branch(TagRef)",
    "sys.admin.create_branch(GitOid)",
    "sys.admin.create_branch(Str)",
    "sys.admin.retry_failure",
    "sys.admin.skip_failure",
    "sys.admin.replay_failure",
    "sys.admin.resolve_failure",
];

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SignatureIdentity {
    name: String,
    type_parameters: BTreeSet<String>,
    parameters: Vec<(String, bool)>,
    result: String,
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CallableIdentity {
    name: String,
    type_parameters: BTreeSet<String>,
    parameters: Vec<(String, bool)>,
}

impl SignatureIdentity {
    fn callable_identity(&self) -> CallableIdentity {
        CallableIdentity {
            name: self.name.clone(),
            type_parameters: self.type_parameters.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

fn parse_signature_identity(signature: &str) -> Result<SignatureIdentity, String> {
    let signature = signature
        .strip_prefix("fn ")
        .ok_or_else(|| "function metadata `signature` must start with `fn `".to_owned())?;
    let open = signature
        .find('(')
        .ok_or_else(|| "function signature is missing `(`".to_owned())?;
    let close = matching_delimiter(signature, open, '(', ')')
        .ok_or_else(|| "function signature has unbalanced parameters".to_owned())?;
    let header = &signature[..open];
    let (name, type_parameters) = match header.split_once('<') {
        Some((name, params)) if params.ends_with('>') => {
            let params = params[..params.len() - 1].split(',').map(str::trim);
            let mut type_parameters = BTreeSet::new();
            for parameter in params {
                if !valid_identifier(parameter) || !type_parameters.insert(parameter.to_owned()) {
                    return Err("function signature has invalid or duplicate type parameters".to_owned());
                }
            }
            (name, type_parameters)
        }
        Some(_) => return Err("function signature has malformed type parameters".to_owned()),
        None => (header, BTreeSet::new()),
    };
    if !valid_sys_path(name) {
        return Err("function signature must name a valid sys function path".to_owned());
    }
    let result = signature[close + 1..]
        .strip_prefix(": ")
        .ok_or_else(|| "function signature is missing a result type".to_owned())?;
    if result.trim().is_empty() || result.contains('\n') {
        return Err("function signature result type must be single-line nonblank text".to_owned());
    }

    let mut parameters = Vec::new();
    let mut parameter_names = BTreeSet::new();
    for parameter in split_top_level(&signature[open + 1..close])? {
        let parameter = parameter.trim();
        if parameter.is_empty() {
            continue;
        }
        let (declaration, has_default) = match parameter.split_once(" = ") {
            Some((declaration, _)) => (declaration, true),
            None => (parameter, false),
        };
        let (parameter_name, parameter_type) = declaration
            .split_once(": ")
            .ok_or_else(|| "function parameter must use `name: Type` notation".to_owned())?;
        if !valid_identifier(parameter_name)
            || parameter_type.trim().is_empty()
            || !parameter_names.insert(parameter_name.to_owned())
        {
            return Err("function signature has an invalid or duplicate parameter".to_owned());
        }
        parameters.push((parameter_type.trim().to_owned(), has_default));
    }
    Ok(SignatureIdentity {
        name: name.to_owned(),
        type_parameters,
        parameters,
        result: result.trim().to_owned(),
    })
}

fn split_top_level(input: &str) -> Result<Vec<&str>, String> {
    let mut parts = Vec::new();
    let (mut angle, mut square, mut paren) = (0_i32, 0_i32, 0_i32);
    let mut start = 0;
    for (index, character) in input.char_indices() {
        match character {
            '<' => angle += 1,
            '>' => angle -= 1,
            '[' => square += 1,
            ']' => square -= 1,
            '(' => paren += 1,
            ')' => paren -= 1,
            ',' if angle == 0 && square == 0 && paren == 0 => {
                parts.push(&input[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
        if angle < 0 || square < 0 || paren < 0 {
            return Err("function signature has unbalanced parameter types".to_owned());
        }
    }
    if angle != 0 || square != 0 || paren != 0 {
        return Err("function signature has unbalanced parameter types".to_owned());
    }
    if !input[start..].trim().is_empty() {
        parts.push(&input[start..]);
    }
    Ok(parts)
}

fn matching_delimiter(input: &str, open: usize, left: char, right: char) -> Option<usize> {
    let mut depth = 0;
    for (index, character) in input.char_indices().skip_while(|(index, _)| *index < open) {
        if character == left {
            depth += 1;
        } else if character == right {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn valid_identifier(identifier: &str) -> bool {
    let mut chars = identifier.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn valid_sys_path(path: &str) -> bool {
    let mut segments = path.split('.');
    matches!(segments.next(), Some("sys"))
        && segments.clone().count() >= 1
        && segments.all(|segment| {
            let mut chars = segment.chars();
            chars
                .next()
                .is_some_and(|first| first.is_ascii_lowercase())
                && chars.all(|character| {
                    character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || character == '_'
                })
        })
}

fn validate_object_fields<'a>(
    value: &'a Value,
    context: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{context} must be an object"))?;
    if let Some(field) = required.iter().find(|field| !object.contains_key(**field)) {
        return Err(format!("{context} is missing required field `{field}`"));
    }
    if let Some(field) = object
        .keys()
        .find(|field| !required.contains(&field.as_str()) && !optional.contains(&field.as_str()))
    {
        return Err(format!("{context} has unknown field `{field}`"));
    }
    Ok(object)
}

fn validate_nonblank_string(value: &Value, context: &str) -> Result<(), String> {
    if value.as_str().is_none_or(|text| text.trim().is_empty()) {
        Err(format!("{context} must be a nonblank string"))
    } else {
        Ok(())
    }
}

fn validate_unique_string_array(value: &Value, context: &str) -> Result<(), String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{context} must be an array of strings"))?;
    let mut seen = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        validate_nonblank_string(value, &format!("{context}[{index}]"))?;
        let text = value.as_str().expect("string validated");
        if !seen.insert(text) {
            return Err(format!("{context} contains duplicate `{text}`"));
        }
    }
    Ok(())
}

fn validate_named_rows(
    value: &Value,
    inventory: &str,
    required: &[&str],
    optional: &[&str],
    string_fields: &[&str],
    boolean_fields: &[&str],
    string_array_fields: &[&str],
    nested_name_type_fields: &[&str],
) -> Result<(), String> {
    let rows = value
        .as_array()
        .ok_or_else(|| format!("system API `{inventory}` must be an array"))?;
    let mut names = BTreeSet::new();
    for (index, row) in rows.iter().enumerate() {
        let context = format!("{inventory}[{index}]");
        let object = validate_object_fields(row, &context, required, optional)?;
        for field in string_fields {
            validate_nonblank_string(
                object.get(*field).expect("required field was checked"),
                &format!("{context}.{field}"),
            )?;
        }
        for field in boolean_fields {
            if !object.get(*field).is_some_and(Value::is_boolean) {
                return Err(format!("{context}.{field} must be a boolean"));
            }
        }
        for field in string_array_fields {
            if let Some(value) = object.get(*field) {
                validate_unique_string_array(value, &format!("{context}.{field}"))?;
            }
        }
        for field in nested_name_type_fields {
            let nested = object
                .get(*field)
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{context}.{field} must be an array of named types"))?;
            let mut nested_names = BTreeSet::new();
            for (nested_index, item) in nested.iter().enumerate() {
                let item_context = format!("{context}.{field}[{nested_index}]");
                let item = validate_object_fields(
                    item,
                    &item_context,
                    &["name", "type"],
                    &[],
                )?;
                for item_field in ["name", "type"] {
                    validate_nonblank_string(
                        item.get(item_field).expect("nested field was checked"),
                        &format!("{item_context}.{item_field}"),
                    )?;
                }
                let name = item["name"].as_str().expect("nested name validated");
                if !nested_names.insert(name) {
                    return Err(format!("{item_context} duplicates field `{name}`"));
                }
            }
        }
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .expect("named inventory requires a validated name string");
        if !names.insert(name) {
            return Err(format!("system API `{inventory}` duplicates name `{name}`"));
        }
    }
    Ok(())
}

fn validate_enum_inventory(value: &Value) -> Result<(), String> {
    let enums = value
        .as_object()
        .ok_or_else(|| "system API `enums` must be an object".to_owned())?;
    for (name, variants) in enums {
        validate_nonblank_string(&Value::String(name.clone()), "system API enum name")?;
        validate_unique_string_array(variants, &format!("system API enum `{name}` variants"))?;
    }
    Ok(())
}

fn validate_removed_names(value: &Value) -> Result<(), String> {
    let names = value
        .as_object()
        .ok_or_else(|| "system API `removed_names` must be an object".to_owned())?;
    for (name, descriptor) in names {
        let context = format!("system API removed name `{name}`");
        let descriptor = validate_object_fields(
            descriptor,
            &context,
            &["replacement", "diagnostic"],
            &[],
        )?;
        for field in ["replacement", "diagnostic"] {
            validate_nonblank_string(
                descriptor.get(field).expect("required field was checked"),
                &format!("{context}.{field}"),
            )?;
        }
    }
    Ok(())
}

const BUILTIN_TYPES: &[&str] = &[
    "Blob",
    "Bool",
    "Decimal",
    "Digest",
    "Duration",
    "Instant",
    "Int",
    "Locale",
    "Path",
    "PresentContext",
    "PresentTree",
    "Str",
    "TimeZone",
];

#[derive(Default)]
struct ApiTypeNames {
    concrete: BTreeSet<String>,
    generic_arity: std::collections::BTreeMap<String, usize>,
    relation_fields: std::collections::BTreeMap<String, Value>,
    value_fields: std::collections::BTreeMap<String, Value>,
    alias_targets: std::collections::BTreeMap<String, String>,
}

impl ApiTypeNames {
    fn from_api(api: &Value) -> Self {
        let mut names = Self::default();
        for name in api["opaque_identifiers"].as_array().into_iter().flatten() {
            if let Some(name) = name.as_str() {
                names.concrete.insert(name.to_owned());
            }
        }
        for (name, _) in api["enums"].as_object().into_iter().flatten() {
            names.concrete.insert(name.clone());
        }
        for relation in api["relations"].as_array().into_iter().flatten() {
            if let Some(name) = relation["name"].as_str() {
                names.concrete.insert(name.to_owned());
                names
                    .relation_fields
                    .insert(name.to_owned(), relation["fields"].clone());
            }
        }
        for value_type in api["value_types"].as_array().into_iter().flatten() {
            let Some(declaration) = value_type["name"].as_str() else {
                continue;
            };
            let (name, arity) = generic_declaration(declaration);
            names.concrete.insert(name.to_owned());
            if arity > 0 {
                names.generic_arity.insert(name.to_owned(), arity);
            }
            names
                .value_fields
                .insert(name.to_owned(), value_type["fields"].clone());
        }
        for alias in api["reference_aliases"].as_array().into_iter().flatten() {
            if let (Some(name), Some(target)) =
                (alias["name"].as_str(), alias["target"].as_str())
            {
                names.concrete.insert(name.to_owned());
                names.alias_targets.insert(name.to_owned(), target.to_owned());
            }
        }
        names
    }

    fn validate_type(&self, source: &str, parameters: &BTreeSet<String>) -> Result<(), String> {
        let mut parser = TypeExpressionParser {
            source,
            offset: 0,
            names: self,
            parameters,
        };
        parser.parse_type()?;
        parser.skip_space();
        if parser.offset != source.len() {
            return Err(format!("unexpected text in type `{source}`"));
        }
        Ok(())
    }

    fn fields_for(&self, type_name: &str) -> Option<&Value> {
        let type_name = type_name.trim_end_matches('?');
        let base = generic_declaration(type_name).0;
        self.relation_fields
            .get(base)
            .or_else(|| self.value_fields.get(base))
            .or_else(|| {
                self.alias_targets
                    .get(base)
                    .and_then(|target| self.relation_fields.get(target))
            })
    }
}

fn generic_declaration(declaration: &str) -> (&str, usize) {
    let Some(open) = declaration.find('<') else {
        return (declaration, 0);
    };
    let Some(parameters) = declaration.strip_suffix('>').map(|_| &declaration[open + 1..declaration.len() - 1]) else {
        return (declaration, 0);
    };
    let arity = parameters.split(',').filter(|parameter| !parameter.trim().is_empty()).count();
    (&declaration[..open], arity)
}

fn generic_parameters(declaration: &str) -> Option<Vec<&str>> {
    let open = declaration.find('<')?;
    let parameters = declaration.strip_suffix('>')?;
    let parameters = parameters.get(open + 1..)?;
    Some(parameters.split(',').map(str::trim).collect())
}

fn validate_cross_inventory_names(api: &Value) -> Result<(), String> {
    let mut type_names = std::collections::BTreeMap::<String, &'static str>::new();
    let mut add_type = |name: &str, inventory: &'static str| -> Result<(), String> {
        if let Some(previous) = type_names.insert(name.to_owned(), inventory) {
            return Err(format!(
                "system API type `{name}` is declared by both `{previous}` and `{inventory}`"
            ));
        }
        Ok(())
    };
    // Singleton `type` entries refer to supporting value types; they do not
    // define a second type with the same name.
    for name in api["opaque_identifiers"].as_array().into_iter().flatten() {
        add_type(name.as_str().expect("validated opaque identifier"), "opaque_identifiers")?;
    }
    for (name, _) in api["enums"].as_object().into_iter().flatten() {
        add_type(name, "enums")?;
    }
    for relation in api["relations"].as_array().into_iter().flatten() {
        add_type(relation["name"].as_str().expect("validated relation name"), "relations")?;
    }
    for value_type in api["value_types"].as_array().into_iter().flatten() {
        let declaration = value_type["name"].as_str().expect("validated value type name");
        add_type(generic_declaration(declaration).0, "value_types")?;
    }
    for alias in api["reference_aliases"].as_array().into_iter().flatten() {
        add_type(alias["name"].as_str().expect("validated alias name"), "reference_aliases")?;
    }

    let mut public_values = BTreeSet::new();
    for singleton in api["singletons"].as_array().into_iter().flatten() {
        public_values.insert(singleton["name"].as_str().expect("validated singleton name"));
    }
    for function in api["functions"].as_array().into_iter().flatten() {
        let name = function["name"].as_str().expect("validated function name");
        if !public_values.insert(name) {
            return Err(format!("system API public value `{name}` is declared more than once"));
        }
    }
    for (removed, descriptor) in api["removed_names"].as_object().into_iter().flatten() {
        if public_values.contains(removed.as_str()) {
            return Err(format!("removed system API name `{removed}` is still publicly declared"));
        }
        let replacement = descriptor["replacement"].as_str().expect("validated replacement");
        if !public_values.contains(replacement) {
            return Err(format!(
                "removed system API name `{removed}` has unresolved replacement `{replacement}`"
            ));
        }
    }
    Ok(())
}

struct TypeExpressionParser<'a, 'names, 'params> {
    source: &'a str,
    offset: usize,
    names: &'names ApiTypeNames,
    parameters: &'params BTreeSet<String>,
}

impl TypeExpressionParser<'_, '_, '_> {
    fn skip_space(&mut self) {
        while self
            .source
            .as_bytes()
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
    }

    fn parse_type(&mut self) -> Result<(), String> {
        self.skip_space();
        if self.source.as_bytes().get(self.offset) == Some(&b'[') {
            self.offset += 1;
            self.parse_type()?;
            self.skip_space();
            if self.source.as_bytes().get(self.offset) != Some(&b']') {
                return Err(format!("unclosed array type in `{}`", self.source));
            }
            self.offset += 1;
        } else {
            let start = self.offset;
            while self.source.as_bytes().get(self.offset).is_some_and(|byte| {
                byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'.'
            }) {
                self.offset += 1;
            }
            if self.offset == start {
                return Err(format!("expected a type name in `{}`", self.source));
            }
            let name = &self.source[start..self.offset];
            self.skip_space();
            let has_arguments = self.source.as_bytes().get(self.offset) == Some(&b'<');
            if has_arguments {
                self.offset += 1;
                let mut arity = 0;
                loop {
                    self.skip_space();
                    self.parse_type()?;
                    arity += 1;
                    self.skip_space();
                    match self.source.as_bytes().get(self.offset) {
                        Some(b',') => self.offset += 1,
                        Some(b'>') => {
                            self.offset += 1;
                            break;
                        }
                        _ => return Err(format!("unclosed generic type in `{}`", self.source)),
                    }
                }
                let expected = match name {
                    "Relation" | "Query" => Some(1),
                    name => self.names.generic_arity.get(name).copied(),
                };
                if expected != Some(arity) {
                    return Err(format!(
                        "type `{name}` expects {} generic argument(s), found {arity}",
                        expected.map_or_else(|| "no declared".to_owned(), |count| count.to_string())
                    ));
                }
                if !matches!(name, "Relation" | "Query") && !self.names.concrete.contains(name) {
                    return Err(format!("unresolved system API type `{name}`"));
                }
            } else if self.parameters.contains(name) {
                // Generic witnesses are introduced by the containing declaration.
            } else if BUILTIN_TYPES.contains(&name) {
                // The builtin list is intentionally explicit so misspelled core types fail closed.
            } else if name == "Relation" || name == "Query" {
                return Err(format!("type constructor `{name}` requires one type argument"));
            } else if self.names.generic_arity.contains_key(name) {
                return Err(format!("generic system API type `{name}` requires arguments"));
            } else if !self.names.concrete.contains(name) {
                return Err(format!("unresolved system API type `{name}`"));
            }
        }
        self.skip_space();
        if self.source.as_bytes().get(self.offset) == Some(&b'?') {
            self.offset += 1;
        }
        Ok(())
    }
}

fn validate_api_type_graph(api: &Value, names: &ApiTypeNames) -> Result<(), String> {
    validate_cross_inventory_names(api)?;
    for singleton in api["singletons"].as_array().into_iter().flatten() {
        names
            .validate_type(singleton["type"].as_str().expect("validated singleton type"), &BTreeSet::new())
            .map_err(|error| format!("singletons type: {error}"))?;
    }
    for alias in api["reference_aliases"].as_array().into_iter().flatten() {
        let name = alias["name"].as_str().expect("validated alias name");
        let target = alias["target"].as_str().expect("validated alias target");
        let definition = alias["definition"].as_str().expect("validated alias definition");
        if !names.relation_fields.contains_key(target) {
            return Err(format!("reference alias `{name}` targets unknown relation `{target}`"));
        }
        if definition != format!("sys.RowRef<{target}>") {
            return Err(format!(
                "reference alias `{name}` definition does not reference its target `{target}`"
            ));
        }
        names
            .validate_type(definition, &BTreeSet::new())
            .map_err(|error| format!("reference alias `{name}`: {error}"))?;
    }
    for value_type in api["value_types"].as_array().into_iter().flatten() {
        let declaration = value_type["name"].as_str().expect("validated value type name");
        if declaration.contains('<') && generic_parameters(declaration).is_none() {
            return Err(format!("value type declaration `{declaration}` is malformed"));
        }
        let (name, arity) = generic_declaration(declaration);
        let parameters = value_type["type_parameters"]
            .as_array()
            .expect("validated type parameter array")
            .iter()
            .map(|parameter| parameter.as_str().expect("validated type parameter").to_owned())
            .collect::<BTreeSet<_>>();
        let declared_parameters = generic_parameters(declaration).unwrap_or_default();
        if arity != parameters.len()
            || declared_parameters.len() != parameters.len()
            || declared_parameters
                .iter()
                .any(|parameter| !parameters.contains(*parameter))
        {
            return Err(format!(
                "value type `{name}` declaration and type_parameters disagree"
            ));
        }
        if parameters.iter().any(|parameter| !valid_identifier(parameter)) {
            return Err(format!("value type `{name}` has an invalid type parameter"));
        }
        for field in value_type["fields"].as_array().into_iter().flatten() {
            let field_name = field["name"].as_str().expect("validated field name");
            let field_type = field["type"].as_str().expect("validated field type");
            names
                .validate_type(field_type, &parameters)
                .map_err(|error| format!("value type `{name}` field `{field_name}`: {error}"))?;
        }
    }
    for relation in api["relations"].as_array().into_iter().flatten() {
        let name = relation["name"].as_str().expect("validated relation name");
        let reference_type = relation["reference_type"]
            .as_str()
            .expect("validated reference type");
        if names.alias_targets.get(reference_type).map(String::as_str) != Some(name) {
            return Err(format!(
                "relation `{name}` reference_type `{reference_type}` must be its matching reference alias"
            ));
        }
        let key_fields = relation["key_fields"]
            .as_array()
            .expect("validated key fields")
            .iter()
            .map(|field| field.as_str().expect("validated key field"))
            .collect::<Vec<_>>();
        let key = relation["key"].as_str().expect("validated relation key");
        let expected_key = key_fields.join(" + ");
        if key_fields.is_empty() || key != expected_key {
            return Err(format!(
                "relation `{name}` key `{key}` does not match key_fields `{expected_key}`"
            ));
        }
        for field in relation["fields"].as_array().into_iter().flatten() {
            let field_name = field["name"].as_str().expect("validated field name");
            let field_type = field["type"].as_str().expect("validated field type");
            names
                .validate_type(field_type, &BTreeSet::new())
                .map_err(|error| format!("relation `{name}` field `{field_name}`: {error}"))?;
        }
        for (index, key_field) in relation["key_fields"]
            .as_array()
            .expect("validated key fields")
            .iter()
            .enumerate()
        {
            // Key paths use dotted names for fields of embedded records (for
            // example DiffEntry.change.area); resolve each hop through the
            // published relation/value-type graph so schema edits cannot leave
            // a key pointing at a removed nested field.
            let key_field = key_field.as_str().expect("validated key field");
            let mut fields = &relation["fields"];
            let mut resolved_type = None;
            let segments = key_field.split('.').collect::<Vec<_>>();
            for (segment_index, segment) in segments.iter().enumerate() {
                let Some(field) = fields
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|field| field["name"] == *segment)
                else {
                    return Err(format!(
                        "relation `{name}` key_fields[{index}] cannot resolve field path `{key_field}`"
                    ));
                };
                resolved_type = field["type"].as_str();
                if segment_index + 1 < segments.len() {
                    fields = resolved_type
                        .and_then(|field_type| names.fields_for(field_type))
                        .ok_or_else(|| {
                            format!(
                                "relation `{name}` key_fields[{index}] cannot traverse field path `{key_field}`"
                            )
                        })?;
                }
            }
            if resolved_type.is_none() {
                return Err(format!(
                    "relation `{name}` key_fields[{index}] has an invalid field path `{key_field}`"
                ));
            }
        }
    }
    for function in api["functions"].as_array().into_iter().flatten() {
        let name = function["name"].as_str().expect("validated function name");
        let signature = function["signature"].as_str().expect("validated signature");
        let parsed = parse_signature_identity(signature)?;
        let mut saw_default = false;
        for (index, (_, has_default)) in parsed.parameters.iter().enumerate() {
            if !has_default && saw_default {
                return Err(format!(
                    "function `{name}` parameter {index} is required after a defaulted parameter"
                ));
            }
            saw_default |= has_default;
        }
        let open = signature.find('(').expect("validated function signature");
        let close = matching_delimiter(signature, open, '(', ')')
            .expect("validated function signature delimiters");
        for parameter in split_top_level(&signature[open + 1..close])? {
            let Some((declaration, default)) = parameter.split_once(" = ") else {
                continue;
            };
            let (parameter_name, parameter_type) = declaration
                .split_once(": ")
                .expect("validated parameter declaration");
            validate_function_default(
                api,
                names,
                name,
                parameter_name,
                parameter_type.trim(),
                default,
            )?;
        }
        for (index, (parameter, _)) in parsed.parameters.iter().enumerate() {
            names
                .validate_type(parameter, &parsed.type_parameters)
                .map_err(|error| format!("function `{name}` parameter {index}: {error}"))?;
        }
        names
            .validate_type(&parsed.result, &parsed.type_parameters)
            .map_err(|error| format!("function `{name}` result: {error}"))?;
    }
    Ok(())
}

fn validate_function_default(
    api: &Value,
    names: &ApiTypeNames,
    function: &str,
    parameter: &str,
    parameter_type: &str,
    default: &str,
) -> Result<(), String> {
    if default == "null" {
        return if parameter_type.ends_with('?') {
            Ok(())
        } else {
            Err(format!(
                "function `{function}` default for `{parameter}` uses `null` with non-optional type `{parameter_type}`"
            ))
        };
    }

    let actual_type = match default {
        "true" | "false" => "Bool".to_owned(),
        _ => {
            if let Some((enum_name, variant)) = default.rsplit_once('.')
                && let Some(variants) = api["enums"].get(enum_name).and_then(Value::as_array)
            {
                if !variants
                    .iter()
                    .any(|candidate| candidate.as_str() == Some(variant))
                {
                    return Err(format!(
                        "function `{function}` default for `{parameter}` references unknown `{enum_name}.{variant}`"
                    ));
                }
                enum_name.to_owned()
            } else {
                resolve_singleton_default_type(api, names, default).map_err(|error| {
                    format!("function `{function}` default for `{parameter}` {error}")
                })?
            }
        }
    };

    if actual_type != parameter_type {
        return Err(format!(
            "function `{function}` default `{default}` has type `{actual_type}`, not parameter type `{parameter_type}`"
        ));
    }
    Ok(())
}

fn resolve_singleton_default_type(
    api: &Value,
    names: &ApiTypeNames,
    path: &str,
) -> Result<String, String> {
    let singleton = api["singletons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|singleton| {
            let name = singleton["name"].as_str()?;
            (path == name || path.strip_prefix(name).is_some_and(|suffix| suffix.starts_with('.')))
                .then_some((name, singleton))
        })
        .max_by_key(|(name, _)| name.len());
    let Some((singleton_name, singleton)) = singleton else {
        return Err(format!("does not resolve to an enum variant or singleton path `{path}`"));
    };

    let mut actual_type = singleton["type"]
        .as_str()
        .expect("validated singleton type")
        .to_owned();
    let suffix = path.strip_prefix(singleton_name).expect("singleton prefix matched");
    if !suffix.is_empty() {
        let fields = suffix
            .strip_prefix('.')
            .expect("singleton suffix has a field separator");
        for field_name in fields.split('.') {
            if actual_type.ends_with('?') {
                return Err(format!("cannot resolve singleton field path `{path}`"));
            }
            let field = names
                .value_fields
                .get(generic_declaration(&actual_type).0)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|field| field["name"] == field_name)
                .ok_or_else(|| format!("cannot resolve singleton field path `{path}`"))?;
            actual_type = field["type"]
                .as_str()
                .expect("validated field type")
                .to_owned();
        }
    }
    Ok(actual_type)
}

fn valid_function_label(label: &str, signature: &SignatureIdentity) -> bool {
    let Some(suffix) = label.strip_prefix(&signature.name) else {
        return false;
    };
    if suffix.is_empty() {
        return true;
    }
    if let Some(parameters) = suffix
        .strip_prefix('<')
        .and_then(|parameters| parameters.strip_suffix('>'))
    {
        let parameters = parameters
            .split(',')
            .map(str::trim)
            .collect::<BTreeSet<_>>();
        return !parameters.is_empty()
            && parameters.len() == signature.type_parameters.len()
            && parameters.iter().all(|parameter| {
                valid_identifier(parameter) && signature.type_parameters.contains(*parameter)
            });
    }
    let Some(labelled_type) = suffix
        .strip_prefix('(')
        .and_then(|parameters| parameters.strip_suffix(')'))
        .map(str::trim)
    else {
        return false;
    };
    if labelled_type.is_empty() || labelled_type.contains(',') {
        return false;
    }
    let is_erased_generic_input = matches!(signature.name.as_str(), "sys.invoke" | "sys.start")
        && labelled_type == "Value";
    is_erased_generic_input
        || signature
            .parameters
            .iter()
            .any(|(parameter_type, _)| type_label(parameter_type) == labelled_type)
}

fn type_label(ty: &str) -> &str {
    let ty = ty.trim().strip_suffix('?').unwrap_or(ty.trim());
    if ty.starts_with('[') && ty.ends_with(']') {
        return type_label(&ty[1..ty.len() - 1]);
    }
    ty.split('<')
        .next()
        .unwrap_or(ty)
        .rsplit('.')
        .next()
        .unwrap_or(ty)
}

/// The erased invoke/start labels are a published pairing with the generic
/// overload. Validate that relationship as a collection invariant: the erased
/// overload removes exactly the generic type witness and substitutes that
/// witness into the result. This keeps the build collector aligned with the
/// semantic SystemApi loader without requiring a second copy of its type table.
fn validate_erased_generic_pairs(signatures: &[(String, SignatureIdentity)]) -> Result<(), String> {
    for (label, erased) in signatures {
        let Some((base, erased_type)) = [
            ("sys.invoke(Value)", "sys.invoke"),
            ("sys.start(Value)", "sys.start"),
        ]
        .into_iter()
        .find(|(candidate, _)| label == candidate)
        else {
            continue;
        };
        let direct_input = erased
            .parameters
            .iter()
            .any(|(parameter_type, _)| type_label(parameter_type) == "Value");
        if direct_input {
            continue;
        }

        let valid_sibling = erased.type_parameters.is_empty()
            && signatures.iter().any(|(_, generic)| {
                if generic.name != erased_type || generic.type_parameters.len() != 1 {
                    return false;
                }
                let witness = generic
                    .type_parameters
                    .iter()
                    .next()
                    .expect("one generic type parameter");
                let witness_indices = generic
                    .parameters
                    .iter()
                    .enumerate()
                    .filter_map(|(index, (parameter_type, _))| {
                        (parameter_type.trim() == witness).then_some(index)
                    })
                    .collect::<Vec<_>>();
                if witness_indices.len() != 1 {
                    return false;
                }
                let witness_index = witness_indices[0];
                let remaining = generic
                    .parameters
                    .iter()
                    .enumerate()
                    .filter_map(|(index, parameter)| (index != witness_index).then_some(parameter))
                    .cloned()
                    .collect::<Vec<_>>();
                remaining == erased.parameters
                    && type_parameter_substitution(&generic.result, witness, &erased.result)
                        .is_some()
            });
        if !valid_sibling {
            return Err(format!(
                "erased overload `{base}` requires a matching generic sibling with the same remaining inputs and result"
            ));
        }
    }
    Ok(())
}

fn type_parameter_substitution(template: &str, parameter: &str, concrete: &str) -> Option<String> {
    let spans = type_parameter_spans(template, parameter);
    let (first_start, first_end) = *spans.first()?;
    let prefix = &template[..first_start];
    let after_prefix = concrete.strip_prefix(prefix)?;
    let fixed_suffix = spans
        .get(1)
        .map(|(next_start, _)| &template[first_end..*next_start])
        .unwrap_or(&template[first_end..]);
    let replacement = if fixed_suffix.is_empty() {
        after_prefix
    } else if spans.len() == 1 {
        after_prefix.strip_suffix(fixed_suffix)?
    } else {
        let suffix_index = after_prefix.find(fixed_suffix)?;
        &after_prefix[..suffix_index]
    };
    let substituted = substitute_type_parameter(template, parameter, replacement);
    (substituted == concrete).then(|| replacement.to_owned())
}

fn type_parameter_spans(source: &str, parameter: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut characters = source.char_indices().peekable();
    while let Some((start, character)) = characters.next() {
        if character != '_' && !character.is_ascii_alphabetic() {
            continue;
        }
        let mut end = start + character.len_utf8();
        while let Some((index, next)) = characters.peek().copied() {
            if next == '_' || next.is_ascii_alphanumeric() {
                characters.next();
                end = index + next.len_utf8();
            } else {
                break;
            }
        }
        if &source[start..end] == parameter
            && !source[..start].trim_end().ends_with('.')
        {
            spans.push((start, end));
        }
    }
    spans
}

fn substitute_type_parameter(source: &str, parameter: &str, replacement: &str) -> String {
    let spans = type_parameter_spans(source, parameter);
    let mut result = String::with_capacity(source.len() + replacement.len() * spans.len());
    let mut cursor = 0;
    for (start, end) in spans {
        result.push_str(&source[cursor..start]);
        result.push_str(replacement);
        cursor = end;
    }
    result.push_str(&source[cursor..]);
    result
}

/// Validate cross-annotation invariants before deriving constants or emitting JSON.
pub fn validate_collection(functions: &[Function]) -> Result<(), String> {
    let mut names = BTreeSet::new();
    let mut constants = BTreeSet::new();
    let mut signatures = BTreeSet::new();
    let mut parsed_signatures = Vec::new();
    for function in functions {
        validate_function_metadata(&function.metadata)?;
        let name = function.metadata["name"].as_str().expect("validated name");
        if !names.insert(name.to_owned()) {
            return Err(format!("duplicate #[ornasys] name {name}"));
        }
        let constant = format!("{}_DESCRIPTOR", function.method.to_ascii_uppercase());
        if !constants.insert(constant.clone()) {
            return Err(format!("duplicate #[ornasys] descriptor constant {constant}"));
        }
        let signature = function.metadata["signature"]
            .as_str()
            .expect("validated signature");
        let parsed = parse_signature_identity(signature)?;
        if !signatures.insert(parsed.callable_identity()) {
            return Err(format!(
                "duplicate #[ornasys] callable signature for function `{name}`"
            ));
        }
        parsed_signatures.push((name.to_owned(), parsed));
    }
    validate_erased_generic_pairs(&parsed_signatures)
}

/// The normative system API has fixed top-level collection counts. Checking them
/// before serialization catches stale base inventories as well as bad function
/// collection, instead of publishing a self-consistent but truncated document.
pub fn validate_api_document(api: &Value) -> Result<(), String> {
    let object = api
        .as_object()
        .ok_or_else(|| "system API document must be a JSON object".to_owned())?;
    const TOP_LEVEL_FIELDS: [&str; 15] = [
        "title",
        "language_version",
        "sys_version",
        "status",
        "source_of_truth",
        "removed_names",
        "singletons",
        "opaque_identifiers",
        "reference_aliases",
        "value_types",
        "enums",
        "relations",
        "functions",
        "failure_codes",
        "counts",
    ];
    if object.len() != TOP_LEVEL_FIELDS.len()
        || object
            .keys()
            .any(|field| !TOP_LEVEL_FIELDS.contains(&field.as_str()))
    {
        return Err("system API document must contain exactly the published top-level fields".to_owned());
    }
    for field in ["title", "language_version", "sys_version", "status", "source_of_truth"] {
        if object
            .get(field)
            .and_then(Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(format!("system API `{field}` must be a nonblank string"));
        }
    }
    let counts = object
        .get("counts")
        .and_then(Value::as_object)
        .ok_or_else(|| "system API `counts` must be an object".to_owned())?;
    let inventories = [
        ("singletons", "singletons", false),
        ("opaque_identifiers", "opaque_identifiers", false),
        ("reference_aliases", "reference_aliases", false),
        ("value_types", "value_types", false),
        ("enums", "enums", true),
        ("relations", "relations", false),
        ("functions", "functions", false),
        ("failure_codes", "failure_codes", false),
    ];
    if counts.len() != inventories.len() {
        return Err("system API `counts` must declare each normative inventory exactly once".to_owned());
    }
    for (count_name, inventory_name, is_object) in inventories {
        let inventory = object
            .get(inventory_name)
            .ok_or_else(|| format!("system API `{inventory_name}` inventory is missing"))?;
        let actual = if is_object {
            inventory
                .as_object()
                .map(serde_json::Map::len)
                .ok_or_else(|| format!("system API `{inventory_name}` must be an object"))?
        } else {
            inventory
                .as_array()
                .map(Vec::len)
                .ok_or_else(|| format!("system API `{inventory_name}` must be an array"))?
        };
        let declared = counts
            .get(count_name)
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| format!("system API count `{count_name}` must be a natural number"))?;
        if declared != actual {
            return Err(format!(
                "system API count `{count_name}` declares {declared}, inventory contains {actual}"
            ));
        }
    }

    validate_removed_names(&object["removed_names"])?;
    validate_named_rows(
        &object["singletons"],
        "singletons",
        &["name", "type", "availability"],
        &[],
        &["name", "type", "availability"],
        &[],
        &[],
        &[],
    )?;
    validate_unique_string_array(&object["opaque_identifiers"], "opaque_identifiers")?;
    validate_named_rows(
        &object["reference_aliases"],
        "reference_aliases",
        &["name", "target", "definition"],
        &[],
        &["name", "target", "definition"],
        &[],
        &[],
        &[],
    )?;
    validate_named_rows(
        &object["value_types"],
        "value_types",
        &["name", "kind", "purpose", "type_parameters", "fields", "invariants"],
        &[],
        &["name", "kind", "purpose"],
        &[],
        &["type_parameters", "invariants"],
        &["fields"],
    )?;
    validate_enum_inventory(&object["enums"])?;
    validate_named_rows(
        &object["relations"],
        "relations",
        &[
            "name",
            "kind",
            "availability",
            "grouped_handle",
            "writable",
            "key",
            "key_fields",
            "purpose",
            "reference_type",
            "fields",
        ],
        &["invariants"],
        &[
            "name",
            "kind",
            "availability",
            "grouped_handle",
            "key",
            "purpose",
            "reference_type",
        ],
        &["writable"],
        &["key_fields", "invariants"],
        &["fields"],
    )?;
    validate_unique_string_array(&object["failure_codes"], "failure_codes")?;

    let functions = object["functions"]
        .as_array()
        .expect("function inventory validated");
    let mut names = std::collections::BTreeSet::new();
    let mut signatures = BTreeSet::new();
    let mut parsed_signatures = Vec::new();
    for function in functions {
        validate_function_metadata(function)?;
        let name = function["name"].as_str().expect("validated name");
        if !names.insert(name) {
            return Err(format!("duplicate system API function `{name}`"));
        }
        let signature = function["signature"]
            .as_str()
            .expect("validated signature");
        let parsed = parse_signature_identity(signature)?;
        if !signatures.insert(parsed.callable_identity()) {
            return Err(format!("duplicate callable signature for system API function `{name}`"));
        }
        parsed_signatures.push((name.to_owned(), parsed));
    }
    validate_erased_generic_pairs(&parsed_signatures)?;
    let type_names = ApiTypeNames::from_api(api);
    validate_api_type_graph(api, &type_names)
}
