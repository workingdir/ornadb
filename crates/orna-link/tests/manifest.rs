//! The `orna.toml` native-library manifest: what parses, and what is refused.

use std::fs;

use orna_link::{LinkErrorCode, ProjectManifest};
use tempfile::TempDir;

const DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn one_library(extra: &str, target_path: &str, digest: &str) -> String {
    format!(
        "[[native_library]]\nname = \"slug\"\nmodules = [\"text\", \"text.more\"]\nabi = 1\n{extra}\n\
         [native_library.target.x86_64-unknown-linux-gnu]\npath = \"{target_path}\"\nsha256 = \"{digest}\"\n"
    )
}

fn errors_of(text: &str) -> Vec<(LinkErrorCode, String)> {
    ProjectManifest::parse(text)
        .expect_err("the manifest must be refused")
        .into_iter()
        .map(|error| (error.code, error.detail))
        .collect()
}

#[test]
fn valid_manifest_parses_every_field() {
    let text = one_library("", "./native/libslug.so", DIGEST);
    let manifest = ProjectManifest::parse(&text).expect("valid manifest");
    assert_eq!(manifest.libraries.len(), 1);
    let library = &manifest.libraries[0];
    assert_eq!(library.name, "slug");
    assert_eq!(library.modules, ["text", "text.more"]);
    assert_eq!(library.abi, 1);
    let target = &library.targets["x86_64-unknown-linux-gnu"];
    assert_eq!(target.path, std::path::PathBuf::from("native/libslug.so"));
    assert_eq!(target.sha256, [0; 32]);
}

#[test]
fn empty_text_is_an_empty_manifest() {
    let manifest = ProjectManifest::parse("").expect("an empty file names no library");
    assert!(manifest.libraries.is_empty());
}

#[test]
fn missing_file_is_an_empty_manifest() {
    let project = TempDir::new().expect("temp project");
    let manifest = ProjectManifest::load(project.path()).expect("no orna.toml means no libraries");
    assert!(manifest.libraries.is_empty());
}

#[test]
fn load_reads_orna_toml_from_the_root() {
    let project = TempDir::new().expect("temp project");
    fs::write(
        project.path().join("orna.toml"),
        one_library("", "native/libslug.so", DIGEST),
    )
    .expect("write manifest");
    let manifest = ProjectManifest::load(project.path()).expect("valid manifest");
    assert_eq!(manifest.libraries[0].name, "slug");
}

#[test]
fn invalid_toml_is_a_manifest_error_with_a_location() {
    let errors = errors_of("[[native_library\nname = \"slug\"\n");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, LinkErrorCode::Manifest);
    assert!(errors[0].1.contains("orna.toml:"), "{}", errors[0].1);
}

#[test]
fn unknown_top_level_key_is_refused() {
    let text = format!(
        "{}\n[other]\nx = 1\n",
        one_library("", "native/libslug.so", DIGEST)
    );
    let errors = errors_of(&text);
    assert!(
        errors
            .iter()
            .any(|(code, detail)| *code == LinkErrorCode::Manifest
                && detail.contains("unknown key `other`")),
        "{errors:?}"
    );
}

#[test]
fn unknown_library_key_is_refused() {
    let errors = errors_of(&one_library("version = 2\n", "native/libslug.so", DIGEST));
    assert!(
        errors
            .iter()
            .any(|(_, detail)| detail.contains("unknown key `version`")),
        "{errors:?}"
    );
}

#[test]
fn unknown_target_key_is_refused() {
    let text = format!(
        "[[native_library]]\nname = \"slug\"\nmodules = [\"text\"]\nabi = 1\n\
         [native_library.target.x86_64-unknown-linux-gnu]\npath = \"native/libslug.so\"\nsha256 = \"{DIGEST}\"\nsize = 1\n"
    );
    let errors = errors_of(&text);
    assert!(
        errors
            .iter()
            .any(|(_, detail)| detail.contains("unknown key `size`")),
        "{errors:?}"
    );
}

#[test]
fn engine_module_cannot_be_listed() {
    let text = format!(
        "[[native_library]]\nname = \"slug\"\nmodules = [\"core.math\"]\nabi = 1\n\
         [native_library.target.x86_64-unknown-linux-gnu]\npath = \"native/libslug.so\"\nsha256 = \"{DIGEST}\"\n"
    );
    let errors = errors_of(&text);
    assert!(
        errors
            .iter()
            .any(|(_, detail)| detail.contains("engine-embedded")),
        "{errors:?}"
    );
}

#[test]
fn malformed_module_path_is_refused() {
    let text = format!(
        "[[native_library]]\nname = \"slug\"\nmodules = [\"text-x\"]\nabi = 1\n\
         [native_library.target.x86_64-unknown-linux-gnu]\npath = \"native/libslug.so\"\nsha256 = \"{DIGEST}\"\n"
    );
    let errors = errors_of(&text);
    assert!(
        errors
            .iter()
            .any(|(_, detail)| detail.contains("dotted path")),
        "{errors:?}"
    );
}

#[test]
fn duplicate_module_in_one_library_is_refused() {
    let text = format!(
        "[[native_library]]\nname = \"slug\"\nmodules = [\"text\", \"text\"]\nabi = 1\n\
         [native_library.target.x86_64-unknown-linux-gnu]\npath = \"native/libslug.so\"\nsha256 = \"{DIGEST}\"\n"
    );
    let errors = errors_of(&text);
    assert!(
        errors
            .iter()
            .any(|(_, detail)| detail.contains("listed twice")),
        "{errors:?}"
    );
}

#[test]
fn duplicate_library_name_is_refused() {
    let text = format!(
        "{}\n{}",
        one_library("", "native/a.so", DIGEST),
        one_library("", "native/b.so", DIGEST)
    );
    let errors = errors_of(&text);
    assert!(
        errors
            .iter()
            .any(|(_, detail)| detail.contains("listed twice")),
        "{errors:?}"
    );
}

#[test]
fn library_name_outside_the_allowed_characters_is_refused() {
    let text = format!(
        "[[native_library]]\nname = \"sl ug\"\nmodules = [\"text\"]\nabi = 1\n\
         [native_library.target.x86_64-unknown-linux-gnu]\npath = \"native/libslug.so\"\nsha256 = \"{DIGEST}\"\n"
    );
    let errors = errors_of(&text);
    assert!(
        errors
            .iter()
            .any(|(_, detail)| detail.contains("must be non-empty ASCII")),
        "{errors:?}"
    );
}

#[test]
fn zero_abi_is_refused() {
    let errors =
        errors_of(&one_library("", "native/libslug.so", DIGEST).replace("abi = 1", "abi = 0"));
    assert!(
        errors
            .iter()
            .any(|(_, detail)| detail.contains("positive integer")),
        "{errors:?}"
    );
}

#[test]
fn missing_modules_key_is_refused() {
    let text = format!(
        "[[native_library]]\nname = \"slug\"\nabi = 1\n\
         [native_library.target.x86_64-unknown-linux-gnu]\npath = \"native/libslug.so\"\nsha256 = \"{DIGEST}\"\n"
    );
    let errors = errors_of(&text);
    assert!(
        errors
            .iter()
            .any(|(_, detail)| detail.contains("missing required key `modules`")),
        "{errors:?}"
    );
}

#[test]
fn escaping_path_is_a_library_error() {
    let errors = errors_of(&one_library("", "../libslug.so", DIGEST));
    assert!(
        errors
            .iter()
            .any(|(code, detail)| *code == LinkErrorCode::Library && detail.contains("`..`")),
        "{errors:?}"
    );
}

#[test]
fn absolute_path_is_a_library_error() {
    let errors = errors_of(&one_library("", "/usr/lib/libslug.so", DIGEST));
    assert!(
        errors.iter().any(|(code, detail)| *code == LinkErrorCode::Library && detail.contains("is absolute")),
        "{errors:?}"
    );
}

#[test]
fn short_digest_is_refused() {
    let errors = errors_of(&one_library("", "native/libslug.so", "abcd"));
    assert!(
        errors
            .iter()
            .any(|(code, detail)| *code == LinkErrorCode::Manifest && detail.contains("64")),
        "{errors:?}"
    );
}

#[test]
fn native_library_must_be_an_array_of_tables() {
    let errors = errors_of("native_library = 1\n");
    assert!(
        errors
            .iter()
            .any(|(_, detail)| detail.contains("array of tables")),
        "{errors:?}"
    );
}
