use std::{
    env, fs,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use syn::{
    Expr, ImplItem, Lit,
    visit::{self, Visit},
};

#[derive(Clone)]
struct Function {
    method: String,
    metadata: Value,
}

#[derive(Default)]
struct Collector {
    functions: Vec<Function>,
    errors: Vec<String>,
}

impl<'ast> Visit<'ast> for Collector {
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        for member in &item.items {
            if let ImplItem::Fn(method) = member {
                for attribute in &method.attrs {
                    if attribute.path().segments.last().is_some_and(|segment| {
                        segment.ident == "ornasys"
                    }) {
                        match parse_function_attribute(attribute) {
                            Ok(metadata) => self.functions.push(Function {
                                method: method.sig.ident.to_string(),
                                metadata,
                            }),
                            Err(error) => self
                                .errors
                                .push(format!("{}: {error}", method.sig.ident)),
                        }
                    }
                }
            }
        }
        visit::visit_item_impl(self, item);
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if item.attrs.iter().any(|attribute| {
            attribute
                .path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "ornasys")
        }) {
            self.errors.push(format!(
                "{} is annotated with #[ornasys] outside an impl; annotate an implementation method",
                item.sig.ident
            ));
        }
        visit::visit_item_fn(self, item);
    }
}

fn parse_function_attribute(attribute: &syn::Attribute) -> Result<Value, String> {
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
    let object = metadata
        .as_object()
        .ok_or_else(|| "function metadata must be a JSON object".to_owned())?;
    for field in ["name", "effect", "signature", "purpose"] {
        if !object.get(field).is_some_and(Value::is_string) {
            return Err(format!("function metadata requires string field `{field}`"));
        }
    }
    let effect = metadata["effect"].as_str().expect("validated string");
    if !matches!(effect, "read" | "invoke" | "admin") {
        return Err(format!("unknown system API effect `{effect}`"));
    }
    Ok(metadata)
}

fn rust_sources(root: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, output)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
    Ok(())
}

fn descriptor_constant(method: &str) -> String {
    format!("{}_DESCRIPTOR", method.to_ascii_uppercase())
}

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let source_root = manifest.join("src");
    let base_path = manifest.join("src/system_api_base.json");
    println!("cargo:rerun-if-changed={}", base_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    // `syn` scans written Rust source and cannot see items emitted later by
    // `macro_rules!` or procedural-macro expansion. System API bindings must
    // therefore be explicit annotated impl methods; generated methods are not
    // collected unless a future collector adds macro expansion support.
    let mut sources = Vec::new();
    rust_sources(&source_root, &mut sources).expect("walk sys crate Rust source");
    let mut collector = Collector::default();
    for source in &sources {
        println!("cargo:rerun-if-changed={}", source.display());
        let text = fs::read_to_string(source)
            .unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
        let syntax = syn::parse_file(&text)
            .unwrap_or_else(|error| panic!("parse {}: {error}", source.display()));
        collector.visit_file(&syntax);
    }
    assert!(collector.errors.is_empty(), "{}", collector.errors.join("\n"));
    assert!(
        !collector.functions.is_empty(),
        "no #[ornasys] implementation methods were collected"
    );

    let mut names = std::collections::BTreeSet::new();
    let mut constants = std::collections::BTreeSet::new();
    for function in &collector.functions {
        let name = function.metadata["name"].as_str().expect("validated name");
        assert!(names.insert(name.to_owned()), "duplicate #[ornasys] name {name}");
        let constant = descriptor_constant(&function.method);
        assert!(
            constants.insert(constant.clone()),
            "duplicate #[ornasys] descriptor constant {constant}"
        );
    }

    let base_text = fs::read_to_string(&base_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", base_path.display()));
    let mut api: Value = serde_json::from_str(&base_text).expect("valid system API base JSON");
    let functions = collector
        .functions
        .iter()
        .map(|function| function.metadata.clone())
        .collect::<Vec<_>>();
    api["functions"] = Value::Array(functions);
    api["counts"]["functions"] = json!(collector.functions.len());
    api["source_of_truth"] = json!(
        "crates/orna-sys-v1/src/system_api.rs #[ornasys] descriptor methods; normative semantics in source chapters and generated system reference"
    );

    let mut generated_json = serde_json::to_string_pretty(&api).expect("serialize system API");
    generated_json.push('\n');

    let mut generated_rust = String::new();
    for function in &collector.functions {
        let metadata = &function.metadata;
        let constant = descriptor_constant(&function.method);
        let effect = match metadata["effect"].as_str().expect("validated effect") {
            "read" => "Read",
            "invoke" => "Invoke",
            "admin" => "Admin",
            _ => unreachable!("effect was validated"),
        };
        generated_rust.push_str(&format!(
            "pub const {constant}: SystemFunctionDescriptor = SystemFunctionDescriptor {{\n\
             name: {name:?}, effect: SystemEffect::{effect}, signature: {signature:?}, purpose: {purpose:?},\n\
             }};\n",
            name = metadata["name"].as_str().expect("validated name"),
            signature = metadata["signature"].as_str().expect("validated signature"),
            purpose = metadata["purpose"].as_str().expect("validated purpose"),
        ));
    }
    generated_rust.push_str("\npub static SYSTEM_FUNCTION_DESCRIPTORS: &[SystemFunctionDescriptor] = &[\n");
    for function in &collector.functions {
        generated_rust.push_str(&format!("    {},\n", descriptor_constant(&function.method)));
    }
    generated_rust.push_str("];\n");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    fs::write(out_dir.join("api_sys.json"), generated_json).expect("write generated api/sys.json");
    fs::write(out_dir.join("system_api_catalog.rs"), generated_rust)
        .expect("write generated Rust descriptor catalog");
}
