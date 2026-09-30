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
    validate_erased_generic_pairs(&parsed_signatures)
}
