//! The link plan over real lowered programs and real library files.

use std::fs;
use std::path::{Path, PathBuf};

use orna_hir::{
    DefId, DefKind, FnBody, ModulePath, Name, NativeBinding, NativeKey, Program, SourceId,
    SourceUnit, lower,
};
use orna_link::{
    Binding, EngineRegistry, LinkError, LinkErrorCode, LinkPlan, LinkProfile, ProjectManifest, plan,
};
use orna_syntax::parse_module_with_file;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const TARGET: &str = "x86_64-unknown-linux-gnu";
const FILE: &str = "native/x86_64-unknown-linux-gnu/libslug.so";
const BYTES: &[u8] = b"slug library bytes";
const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const CLOCK: &str = "pub native fn clock(): Int effect none;\n";

fn lowered(source: &str, module: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let path = ModulePath::new(module.split('.').map(Name::new));
    lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path,
    }])
}

fn engine_keys(program: &Program) -> Vec<NativeKey> {
    program
        .defs
        .iter()
        .filter_map(|def| match &def.kind {
            DefKind::Fn(function) => match &function.body {
                FnBody::Native(decl) if matches!(decl.binding, NativeBinding::Engine { .. }) => {
                    Some(decl.key.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A registry that implements every engine native the program declares.
fn full_registry(program: &Program) -> EngineRegistry {
    EngineRegistry::new(engine_keys(program))
}

fn native_def(program: &Program, module: &str, name: &str) -> DefId {
    program
        .defs
        .iter_enumerated()
        .find_map(|(id, def)| {
            let DefKind::Fn(function) = &def.kind else {
                return None;
            };
            let FnBody::Native(decl) = &function.body else {
                return None;
            };
            (decl.key.module.to_string() == module && decl.key.name.to_string() == name)
                .then_some(id)
        })
        .unwrap_or_else(|| panic!("no native declaration `{module}.{name}`"))
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn digest_of(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn library_block(name: &str, modules: &str, abi: u32, path: &str, digest: &str) -> String {
    format!(
        "[[native_library]]\nname = \"{name}\"\nmodules = [{modules}]\nabi = {abi}\n\n\
         [native_library.target.{TARGET}]\npath = \"{path}\"\nsha256 = \"{digest}\"\n"
    )
}

/// A valid block for `slug` over the file written by `place_library`.
fn good_block(abi: u32) -> String {
    library_block("slug", "\"text\"", abi, FILE, &hex_of(&digest_of(BYTES)))
}

fn place_library(root: &Path) {
    let path = root.join(FILE);
    fs::create_dir_all(path.parent().expect("library directory"))
        .expect("create library directory");
    fs::write(path, BYTES).expect("write library file");
}

fn link(
    program: &Program,
    manifest: &ProjectManifest,
    root: &Path,
    profile: LinkProfile,
) -> Result<LinkPlan, Vec<LinkError>> {
    plan(
        program,
        manifest,
        root,
        TARGET,
        profile,
        &full_registry(program),
    )
}

fn codes(errors: &[LinkError]) -> Vec<LinkErrorCode> {
    errors.iter().map(|error| error.code).collect()
}

#[test]
fn engine_natives_bind_to_the_registry() {
    let program = lowered("fn run(): Int = 1;\n", "app");
    let plan = link(
        &program,
        &ProjectManifest::default(),
        Path::new("."),
        LinkProfile::Native,
    )
    .expect("every engine native is in the registry");
    let now = native_def(&program, "core", "now");
    assert_eq!(
        plan.bindings.get(&now),
        Some(&Binding::Engine {
            key: NativeKey {
                module: ModulePath::new([Name::new("core")]),
                name: Name::new("now"),
            },
        })
    );
    assert_eq!(plan.bindings.len(), engine_keys(&program).len());
    assert!(plan.libraries.is_empty());
}

#[test]
fn engine_native_missing_from_registry_is_unbound() {
    let program = lowered("fn run(): Int = 1;\n", "app");
    let errors = plan(
        &program,
        &ProjectManifest::default(),
        Path::new("."),
        TARGET,
        LinkProfile::Native,
        &EngineRegistry::default(),
    )
    .expect_err("an empty registry binds nothing");
    let now = errors
        .iter()
        .find(|error| {
            error
                .declarations
                .iter()
                .any(|site| site.module == "core" && site.name == "now")
        })
        .expect("an error names core.now");
    assert_eq!(now.code, LinkErrorCode::Unbound);
}

#[test]
fn browser_profile_without_registry_entry_is_profile() {
    let program = lowered("fn run(): Int = 1;\n", "app");
    let errors = plan(
        &program,
        &ProjectManifest::default(),
        Path::new("."),
        TARGET,
        LinkProfile::BrowserWasm,
        &EngineRegistry::default(),
    )
    .expect_err("the browser profile needs registry entries");
    assert!(
        codes(&errors)
            .iter()
            .all(|code| *code == LinkErrorCode::Profile),
        "{errors:?}"
    );
}

#[test]
fn browser_profile_links_when_the_registry_covers_every_native() {
    let program = lowered("fn run(): Int = 1;\n", "app");
    let plan = link(
        &program,
        &ProjectManifest::default(),
        Path::new("."),
        LinkProfile::BrowserWasm,
    )
    .expect("the full registry links for the browser profile");
    assert!(plan.libraries.is_empty());
}

#[test]
fn library_native_binds_to_its_verified_file() {
    let project = TempDir::new().expect("temp project");
    place_library(project.path());
    let manifest = ProjectManifest::parse(&good_block(1)).expect("valid manifest");
    let program = lowered(CLOCK, "text");
    let plan =
        link(&program, &manifest, project.path(), LinkProfile::Native).expect("library links");
    let clock = native_def(&program, "text", "clock");
    match plan.bindings.get(&clock) {
        Some(Binding::Library { library, symbol }) => {
            assert_eq!(library, "slug");
            assert!(symbol.contains("clock"), "{symbol}");
        }
        other => panic!("expected a library binding, found {other:?}"),
    }
    let planned = &plan.libraries["slug"];
    assert_eq!(planned.file, project.path().join(FILE));
    assert_eq!(planned.sha256, digest_of(BYTES));
}

#[test]
fn digest_mismatch_names_expected_and_actual() {
    let project = TempDir::new().expect("temp project");
    place_library(project.path());
    let manifest = ProjectManifest::parse(&library_block("slug", "\"text\"", 1, FILE, ZERO_DIGEST))
        .expect("valid manifest");
    let errors = link(
        &lowered(CLOCK, "text"),
        &manifest,
        project.path(),
        LinkProfile::Native,
    )
    .expect_err("the file does not match the manifest");
    assert_eq!(codes(&errors), [LinkErrorCode::Digest]);
    let error = &errors[0];
    assert_eq!(error.library.as_deref(), Some("slug"));
    assert_eq!(error.declarations[0].module, "text");
    assert!(error.detail.contains(ZERO_DIGEST), "{}", error.detail);
    assert!(
        error.detail.contains(&hex_of(&digest_of(BYTES))),
        "{}",
        error.detail
    );
}

#[test]
fn missing_target_entry_is_a_library_error() {
    let project = TempDir::new().expect("temp project");
    place_library(project.path());
    let text = library_block("slug", "\"text\"", 1, FILE, ZERO_DIGEST)
        .replace(TARGET, "aarch64-apple-darwin");
    let manifest = ProjectManifest::parse(&text).expect("valid manifest");
    let errors = link(
        &lowered(CLOCK, "text"),
        &manifest,
        project.path(),
        LinkProfile::Native,
    )
    .expect_err("the manifest has no entry for this target");
    assert_eq!(codes(&errors), [LinkErrorCode::Library]);
    assert_eq!(errors[0].library.as_deref(), Some("slug"));
}

#[test]
fn missing_file_is_a_library_error() {
    let project = TempDir::new().expect("temp project");
    let manifest = ProjectManifest::parse(&good_block(1)).expect("valid manifest");
    let errors = link(
        &lowered(CLOCK, "text"),
        &manifest,
        project.path(),
        LinkProfile::Native,
    )
    .expect_err("the file is absent");
    assert_eq!(codes(&errors), [LinkErrorCode::Library]);
}

#[cfg(unix)]
#[test]
fn symbolic_link_is_a_library_error() {
    let project = TempDir::new().expect("temp project");
    let outside = TempDir::new().expect("outside directory");
    place_library(outside.path());
    let link_path: PathBuf = project.path().join(FILE);
    fs::create_dir_all(link_path.parent().expect("library directory"))
        .expect("create library directory");
    std::os::unix::fs::symlink(outside.path().join(FILE), &link_path).expect("create symlink");
    let manifest = ProjectManifest::parse(&good_block(1)).expect("valid manifest");
    let errors = link(
        &lowered(CLOCK, "text"),
        &manifest,
        project.path(),
        LinkProfile::Native,
    )
    .expect_err("a symbolic link is refused");
    assert_eq!(codes(&errors), [LinkErrorCode::Library]);
    assert!(
        errors[0].detail.contains("symbolic link"),
        "{}",
        errors[0].detail
    );
}

#[test]
fn library_abi_other_than_one_is_an_abi_error() {
    let project = TempDir::new().expect("temp project");
    place_library(project.path());
    let manifest = ProjectManifest::parse(&good_block(2)).expect("valid manifest");
    let errors = link(
        &lowered(CLOCK, "text"),
        &manifest,
        project.path(),
        LinkProfile::Native,
    )
    .expect_err("the library ABI differs");
    assert_eq!(codes(&errors), [LinkErrorCode::Abi]);
    assert_eq!(errors[0].library.as_deref(), Some("slug"));
}

#[test]
fn native_module_without_a_library_is_unbound() {
    let project = TempDir::new().expect("temp project");
    let program = lowered(CLOCK, "text");
    let errors = link(
        &program,
        &ProjectManifest::default(),
        project.path(),
        LinkProfile::Native,
    )
    .expect_err("no library serves text");
    assert_eq!(codes(&errors), [LinkErrorCode::Unbound]);
    assert_eq!(errors[0].declarations[0].module, "text");
    assert_eq!(errors[0].declarations[0].name, "clock");
}

#[test]
fn two_libraries_serving_one_module_are_duplicate() {
    let project = TempDir::new().expect("temp project");
    place_library(project.path());
    let text = format!(
        "{}\n{}",
        good_block(1),
        library_block("other", "\"text\"", 1, FILE, &hex_of(&digest_of(BYTES)))
    );
    let manifest = ProjectManifest::parse(&text).expect("valid manifest");
    let errors = link(
        &lowered(CLOCK, "text"),
        &manifest,
        project.path(),
        LinkProfile::Native,
    )
    .expect_err("two libraries list text");
    assert_eq!(codes(&errors), [LinkErrorCode::Duplicate]);
    assert_eq!(errors[0].library, None);
    assert!(
        errors[0].detail.contains("`slug`") && errors[0].detail.contains("`other`"),
        "{}",
        errors[0].detail
    );
}

#[test]
fn library_natives_are_refused_under_the_browser_profile() {
    let project = TempDir::new().expect("temp project");
    place_library(project.path());
    let manifest = ProjectManifest::parse(&good_block(1)).expect("valid manifest");
    let errors = link(
        &lowered(CLOCK, "text"),
        &manifest,
        project.path(),
        LinkProfile::BrowserWasm,
    )
    .expect_err("the browser profile cannot load a library");
    assert_eq!(codes(&errors), [LinkErrorCode::Profile]);
    assert_eq!(errors[0].library.as_deref(), Some("slug"));
    assert_eq!(errors[0].declarations[0].name, "clock");
}
