use serde_json::Value;
use syn::{
    Expr, ForeignItem, ImplItem, ItemFn, Lit, TraitItem,
    visit::{self, Visit},
};

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
    if !signature.starts_with("fn ") || !signature.contains("(") || !signature.contains("): ") {
        return Err("function metadata `signature` must be a function signature".to_owned());
    }
    let effect = object["effect"].as_str().expect("required effect validated");
    if !matches!(effect, "read" | "invoke" | "admin") {
        return Err(format!("unknown system API effect `{effect}`"));
    }
    Ok(())
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
    for function in functions {
        validate_function_metadata(function)?;
        let name = function["name"].as_str().expect("validated name");
        if !names.insert(name) {
            return Err(format!("duplicate system API function `{name}`"));
        }
    }
    Ok(())
}
