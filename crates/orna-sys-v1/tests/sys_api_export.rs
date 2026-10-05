#![cfg(feature = "dev-sys-export")]

use std::{collections::BTreeMap, fs, path::Path, process::Command};

use orna_sys_v1::{
    SystemProviderAbi, system_api_json, system_api_schema_json, system_binding_modules_json,
    system_binding_stubs, system_dispatch_table, system_host_operation_registry_json,
    system_host_operation_registry_schema_json, system_provider_abi_json,
    system_provider_abi_schema_json,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[path = "../build_host.rs"]
#[allow(dead_code)]
mod build_host;
#[path = "../build_support.rs"]
#[allow(dead_code)]
mod build_support;

const SYS_API_V1_SHA256: &str = "06ea44ae524baa1310c0b84b63f58c5ed2a90d7cc5b67d248c8cbbf9538f04fb";

fn export(schema: bool, output_path: Option<&std::path::Path>) -> Vec<u8> {
    export_mode(schema.then_some("--schema"), output_path)
}

fn export_mode(option: Option<&str>, output_path: Option<&std::path::Path>) -> Vec<u8> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sys-api-export"));
    if let Some(option) = option {
        command.arg(option);
    }
    if let Some(output_path) = output_path {
        command.arg(output_path);
    }
    let output = command
        .output()
        .expect("run dev-only sys artifact exporter");
    assert!(
        output.status.success(),
        "exporter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    match output_path {
        Some(path) => fs::read(path).expect("read exported sys artifact"),
        None => output.stdout,
    }
}

fn read_exported_tree(root: &Path) -> std::io::Result<BTreeMap<String, Vec<u8>>> {
    fn visit(
        root: &Path,
        base: &Path,
        files: &mut BTreeMap<String, Vec<u8>>,
    ) -> std::io::Result<()> {
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            if path.is_dir() {
                visit(&path, base, files)?;
            } else {
                let relative = path
                    .strip_prefix(base)
                    .expect("export path remains beneath its output directory")
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(relative, fs::read(path)?);
            }
        }
        Ok(())
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

#[test]
fn dev_exports_are_byte_stable_and_api_export_matches_the_embedded_schema() {
    let output_dir =
        std::env::temp_dir().join(format!("orna-sys-artifacts-{}", std::process::id()));
    let _ = fs::remove_dir_all(&output_dir);
    fs::create_dir_all(&output_dir).expect("create temporary export directory");
    let api_path_one = output_dir.join("first/api.json");
    let api_path_two = output_dir.join("second/api.json");
    let exported = export(false, Some(&api_path_one));
    let second_export = export(false, Some(&api_path_two));
    assert_eq!(
        exported, second_export,
        "separate exporter runs are byte-stable"
    );
    assert_eq!(exported, system_api_json().as_bytes());
    assert_eq!(
        format!("{:x}", Sha256::digest(&exported)),
        SYS_API_V1_SHA256,
        "the export preserves the frozen final 1.1 contract bytes"
    );

    let api: Value = serde_json::from_slice(&exported).expect("exported API JSON");
    let schema: Value =
        serde_json::from_str(system_api_schema_json()).expect("embedded generated schema");
    let canonical_schema = build_support::canonical_pretty_json(&schema)
        .expect("canonical embedded schema JSON")
        + "\n";
    assert_eq!(
        system_api_schema_json(),
        canonical_schema,
        "the embedded schema uses deterministic canonical serialization"
    );
    build_support::validate_published_schema_shape(&api, &schema)
        .expect("exported contract matches the embedded schema");
    build_support::validate_api_document(&api)
        .expect("exported contract satisfies the final 1.1 type graph and schema invariants");

    assert_eq!(
        export(false, None),
        exported,
        "first stdout export matches the file"
    );
    assert_eq!(
        export(false, None),
        exported,
        "second stdout export is byte-stable"
    );

    let schema_path_one = output_dir.join("first/schema.json");
    let schema_path_two = output_dir.join("second/schema.json");
    let exported_schema = export(true, Some(&schema_path_one));
    assert_eq!(exported_schema, system_api_schema_json().as_bytes());
    assert_eq!(
        exported_schema,
        export(true, Some(&schema_path_two)),
        "separate schema exporter runs are byte-stable"
    );
    assert_eq!(export(true, None), exported_schema);
    assert_eq!(export(true, None), exported_schema);

    fs::remove_dir_all(&output_dir).expect("remove temporary export directory");
}

#[test]
fn dev_provider_registry_export_is_byte_stable_schema_valid_and_runtime_identical() {
    let output_dir = std::env::temp_dir().join(format!(
        "orna-sys-provider-artifacts-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&output_dir);
    fs::create_dir_all(&output_dir).expect("create temporary provider export directory");
    let first_path = output_dir.join("first/dispatch.json");
    let second_path = output_dir.join("second/dispatch.json");
    let exported = export_mode(Some("--provider-abi"), Some(&first_path));
    assert_eq!(
        exported,
        export_mode(Some("--provider-abi"), Some(&second_path)),
        "provider registry exports from separate processes are byte-stable"
    );
    assert_eq!(exported, system_provider_abi_json().as_bytes());
    let registry: Value = serde_json::from_slice(&exported).expect("exported provider registry");
    assert_eq!(
        build_support::canonical_pretty_json(&registry).expect("canonical provider registry")
            + "\n",
        std::str::from_utf8(&exported).expect("provider export is UTF-8"),
        "provider registry export uses deterministic canonical serialization"
    );

    let first_schema_path = output_dir.join("first/dispatch.schema.json");
    let second_schema_path = output_dir.join("second/dispatch.schema.json");
    let exported_schema = export_mode(Some("--provider-abi-schema"), Some(&first_schema_path));
    assert_eq!(
        exported_schema,
        export_mode(Some("--provider-abi-schema"), Some(&second_schema_path)),
        "provider schema exports from separate processes are byte-stable"
    );
    assert_eq!(
        exported_schema,
        system_provider_abi_schema_json().as_bytes()
    );

    let registry_json = std::str::from_utf8(&exported).expect("provider export is UTF-8");
    let schema_json = std::str::from_utf8(&exported_schema).expect("provider schema is UTF-8");
    build_host::validate_json_against_schema(registry_json, schema_json)
        .expect("conformance export matches its embedded provider schema");
    let exported_table = SystemProviderAbi::from_json(registry_json)
        .expect("conformance export deserializes into the typed provider registry");
    assert_eq!(
        &exported_table,
        system_dispatch_table(),
        "conformance export resolves to the exact runtime dispatch table"
    );
    let schema: Value = serde_json::from_str(schema_json).expect("exported provider schema JSON");
    assert_eq!(
        build_support::canonical_pretty_json(&schema).expect("canonical provider schema") + "\n",
        schema_json,
        "provider schema export is canonically serialized"
    );
    assert_eq!(export_mode(Some("--provider-abi"), None), exported);
    assert_eq!(
        export_mode(Some("--provider-abi-schema"), None),
        exported_schema
    );

    fs::remove_dir_all(&output_dir).expect("remove temporary provider export directory");
}

#[test]
fn dev_exports_cover_embedded_host_registry_schema_and_binding_bundle() {
    let output_dir =
        std::env::temp_dir().join(format!("orna-sys-extra-artifacts-{}", std::process::id()));
    let _ = fs::remove_dir_all(&output_dir);
    fs::create_dir_all(&output_dir).expect("create temporary sys artifact directory");

    for (label, option, expected) in [
        (
            "host operations",
            "--host-operations",
            system_host_operation_registry_json(),
        ),
        (
            "host operation schema",
            "--host-operations-schema",
            system_host_operation_registry_schema_json(),
        ),
        ("binding bundle", "--bindings", system_binding_stubs()),
        (
            "binding modules manifest",
            "--binding-modules",
            system_binding_modules_json(),
        ),
    ] {
        let first_path = output_dir.join(format!("first/{label}.out"));
        let second_path = output_dir.join(format!("second/{label}.out"));
        let first = export_mode(Some(option), Some(&first_path));
        assert_eq!(
            first,
            export_mode(Some(option), Some(&second_path)),
            "separate exporter processes produce stable {label} bytes"
        );
        assert_eq!(
            first,
            expected.as_bytes(),
            "{label} export matches its embedded build artifact"
        );
        assert_eq!(
            export_mode(Some(option), None),
            expected.as_bytes(),
            "{label} stdout matches its embedded build artifact"
        );
    }

    fs::remove_dir_all(&output_dir).expect("remove temporary sys artifact directory");
}

#[test]
fn dev_all_export_reconstructs_the_complete_embedded_artifact_tree() {
    let root = std::env::temp_dir().join(format!("orna-sys-all-artifacts-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let first_root = root.join("first");
    let second_root = root.join("second");

    for output_root in [&first_root, &second_root] {
        let output = Command::new(env!("CARGO_BIN_EXE_sys-api-export"))
            .arg("--all")
            .arg(output_root)
            .output()
            .expect("run complete dev artifact export");
        assert!(
            output.status.success(),
            "complete exporter failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }

    let modules: BTreeMap<String, String> =
        serde_json::from_str(system_binding_modules_json()).expect("embedded module manifest");
    let mut expected = BTreeMap::from([
        ("api_sys.json".to_owned(), system_api_json().into_bytes()),
        (
            "system_api_schema.json".to_owned(),
            system_api_schema_json().as_bytes().to_vec(),
        ),
        (
            "system_provider_abi.json".to_owned(),
            system_provider_abi_json().as_bytes().to_vec(),
        ),
        (
            "system_provider_abi.schema.json".to_owned(),
            system_provider_abi_schema_json().as_bytes().to_vec(),
        ),
        (
            "system_host_operations.json".to_owned(),
            system_host_operation_registry_json().as_bytes().to_vec(),
        ),
        (
            "system_host_operations.schema.json".to_owned(),
            system_host_operation_registry_schema_json()
                .as_bytes()
                .to_vec(),
        ),
        (
            "system_bindings.orna".to_owned(),
            system_binding_stubs().as_bytes().to_vec(),
        ),
        (
            "system_binding_modules.json".to_owned(),
            system_binding_modules_json().as_bytes().to_vec(),
        ),
    ]);
    for (relative_path, source) in modules {
        expected.insert(
            format!("system_bindings/{relative_path}"),
            source.into_bytes(),
        );
    }

    let first = read_exported_tree(&first_root).expect("read first complete export tree");
    let second = read_exported_tree(&second_root).expect("read second complete export tree");
    assert_eq!(
        first, expected,
        "all exported bytes match embedded artifacts"
    );
    assert_eq!(second, expected, "independent all exports are byte-stable");
    fs::remove_dir_all(&root).expect("remove temporary complete export directory");
}

#[test]
fn dev_export_rejects_unknown_modes_and_extra_output_paths() {
    let unknown = Command::new(env!("CARGO_BIN_EXE_sys-api-export"))
        .arg("--not-a-mode")
        .output()
        .expect("run exporter with an unknown option");
    assert!(!unknown.status.success());
    assert!(unknown.stdout.is_empty());
    let unknown_stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(unknown_stderr.contains("unknown option `--not-a-mode`"));
    assert!(unknown_stderr.contains("--host-operations"));
    assert!(unknown_stderr.contains("--bindings"));
    assert!(unknown_stderr.contains("--binding-modules"));
    assert!(unknown_stderr.contains("--all output-directory"));

    let missing_all_path = Command::new(env!("CARGO_BIN_EXE_sys-api-export"))
        .arg("--all")
        .output()
        .expect("run exporter with no directory for complete export");
    assert!(!missing_all_path.status.success());
    assert!(missing_all_path.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&missing_all_path.stderr)
            .contains("--all requires an output directory")
    );

    let nonempty_dir =
        std::env::temp_dir().join(format!("orna-sys-nonempty-export-{}", std::process::id()));
    let _ = fs::remove_dir_all(&nonempty_dir);
    fs::create_dir_all(&nonempty_dir).expect("create nonempty export directory");
    let sentinel = nonempty_dir.join("keep.txt");
    fs::write(&sentinel, "preserve").expect("write sentinel file");
    let nonempty_output = Command::new(env!("CARGO_BIN_EXE_sys-api-export"))
        .arg("--all")
        .arg(&nonempty_dir)
        .output()
        .expect("run complete exporter with a nonempty directory");
    assert!(!nonempty_output.status.success());
    assert!(nonempty_output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&nonempty_output.stderr)
            .contains("requires an empty output directory")
    );
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "preserve");
    fs::remove_dir_all(&nonempty_dir).expect("remove nonempty export directory");

    let extra_path = Command::new(env!("CARGO_BIN_EXE_sys-api-export"))
        .arg("first.json")
        .arg("second.json")
        .output()
        .expect("run exporter with extra positional arguments");
    assert!(!extra_path.status.success());
    assert!(extra_path.stdout.is_empty());
    assert!(String::from_utf8_lossy(&extra_path.stderr).contains("usage: sys-api-export"));
}
