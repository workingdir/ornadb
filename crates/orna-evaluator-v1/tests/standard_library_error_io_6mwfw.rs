use std::path::Path;

use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::Raw;

fn sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            path == "std/error.orna"
                || path == "std/collection.orna"
                || path == "std/text.orna"
                || path == "std/io/main.orna"
                || path.starts_with("std/io/")
        })
        .collect()
}

fn pinned_session() -> (AdmittedReplSession, StandardDependencyProfile) {
    let sources = sources();
    let profile =
        StandardDependencyProfile::from_sources("orna.std/6mwfw-error-io", sources.clone())
            .expect("error and IO source bytes form a captured std snapshot");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("error and IO modules resolve against the captured std snapshot");
    let session = AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .unwrap_or_else(|error| {
            panic!(
                "selected error and IO modules failed to load: {}",
                error.code()
            )
        });
    (session, profile)
}

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected result is canonical")
}

fn rooted(source: &str, root: &Path) -> String {
    source.replace("ROOT_PATH", &root.to_string_lossy())
}

fn filesystem_bindings(root: &Path) -> SysHostBindingRegistry {
    let mut filesystem = FilesystemProvider::new();
    filesystem
        .allow_root(root)
        .expect("temporary root is authorized");
    SysHostBindingRegistry::new(EnvironmentProvider::default()).with_filesystem_provider(filesystem)
}

#[test]
fn error_code_traits_compute_stable_identities_and_ordered_cause_chains() {
    let (mut session, profile) = pinned_session();
    let captured_error = sources()
        .into_iter()
        .find(|(path, _)| path == "std/error.orna")
        .expect("the captured snapshot contains std.error");
    profile
        .verify_source(&captured_error.0, &captured_error.1)
        .expect("loaded error source bytes match the dependency snapshot");
    assert!(
        profile
            .verify_source(
                &captured_error.0,
                &format!("{}\n// drift", captured_error.1)
            )
            .is_err()
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-error-6mwfw.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-error-matches-code-6mwfw.orna"
        )),
        Ok(Some(canonical(Raw::Bool(true))))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-error-code-chain-6mwfw.orna")),
        Ok(Some(canonical(Raw::Array(vec![
            Raw::Text("outer.failure".into()),
            Raw::Text("storage.conflict".into()),
            Raw::Text("filesystem.denied".into()),
        ]))))
    );
}

#[test]
fn pinned_reader_and_writer_execute_real_utf8_file_operations() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("input.txt"), "north\n\nsouth\n").unwrap();
    let mut bindings = filesystem_bindings(root.path());
    let (mut session, _) = pinned_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-io-6mwfw.orna")),
        Ok(None)
    );

    let read_all = session
        .submit_with_sys_host_bindings(
            &rooted(
                include_str!("fixtures/stdlib-io-reader-read-all-6mwfw.orna"),
                root.path(),
            ),
            &mut bindings,
        )
        .unwrap_or_else(|error| panic!("reader read_all failed with {}", error.code()));
    assert_eq!(
        read_all,
        Some(canonical(Raw::Text("north\n\nsouth\n".into())))
    );
    let read_lines = session
        .submit_with_sys_host_bindings(
            &rooted(
                include_str!("fixtures/stdlib-io-reader-read-lines-6mwfw.orna"),
                root.path(),
            ),
            &mut bindings,
        )
        .unwrap_or_else(|error| panic!("reader read_lines failed with {}", error.code()));
    assert_eq!(
        read_lines,
        Some(canonical(Raw::Array(vec![
            Raw::Text("north".into()),
            Raw::Text(String::new()),
            Raw::Text("south".into()),
            Raw::Text(String::new()),
        ])))
    );
    let write_all = session
        .submit_with_sys_host_bindings(
            &rooted(
                include_str!("fixtures/stdlib-io-writer-write-6mwfw.orna"),
                root.path(),
            ),
            &mut bindings,
        )
        .unwrap_or_else(|error| panic!("writer write_all failed with {}", error.code()));
    assert_eq!(write_all, Some(canonical(Raw::Null)));
    let append = session
        .submit_with_sys_host_bindings(
            &rooted(
                include_str!("fixtures/stdlib-io-writer-append-6mwfw.orna"),
                root.path(),
            ),
            &mut bindings,
        )
        .unwrap_or_else(|error| panic!("writer append failed with {}", error.code()));
    assert_eq!(append, Some(canonical(Raw::Null)));
    assert_eq!(
        std::fs::read_to_string(root.path().join("output.txt")).unwrap(),
        "first\nsecond\nappended"
    );
}

#[test]
fn core_failures_work_without_std_and_error_import_is_not_filled_in() {
    let mut core = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        core.submit("2 + 3"),
        Ok(Some(canonical(Raw::Int(5.into()))))
    );
    assert_eq!(
        core.submit(include_str!(
            "fixtures/stdlib-error-without-snapshot-typdl.orna"
        ))
        .unwrap_err()
        .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        core.submit("4 + 1"),
        Ok(Some(canonical(Raw::Int(5.into()))))
    );
}
