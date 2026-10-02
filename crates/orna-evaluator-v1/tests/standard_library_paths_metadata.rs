use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_path_and_metadata_helpers_have_portable_behavior() {
    let contract = include_str!("fixtures/stdlib-io-path-metadata-contract-l80o5.orna");
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-io-t7auz.orna"))
        .unwrap_or_else(|error| panic!("std.io import failed: {}", error.code()));
    for (index, expression) in contract.split(" &&\n").enumerate() {
        assert_eq!(
            session.submit(expression).unwrap_or_else(|error| {
                panic!(
                    "path/metadata contract clause {index} failed: {}: {}",
                    error.code(),
                    error.diagnostic().message()
                )
            }),
            Some(bool_value(true)),
            "path/metadata contract clause {index} returned false"
        );
    }
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-io-path-root-escape-l80o5.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-io-path-absolute-l80o5.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-io-path-backslash-l80o5.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-ERROR"
    );
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("assets")).unwrap();
    std::fs::write(root.path().join("assets/file.txt"), "proof").unwrap();
    let mut filesystem = FilesystemProvider::new();
    filesystem.allow_root(root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem);
    let source = include_str!("fixtures/stdlib-io-fs-metadata-host-call-l80o5.orna")
        .replace("authorized-root", &root.path().to_string_lossy());
    let metadata = session
        .submit_with_sys_host_bindings(&source, &mut bindings)
        .unwrap()
        .unwrap();
    assert!(matches!(
        metadata.raw(),
        OvbRaw::Array(fields)
            if fields.len() == 4
                && fields[0] == OvbRaw::Text("file".into())
                && fields[1] == OvbRaw::Tag(60013, Box::new(OvbRaw::Array(vec![
                    OvbRaw::Int(1.into()), OvbRaw::Int(5.into())
                ])))
    ));
}

#[test]
fn core_arithmetic_remains_available_without_optional_filesystem_modules() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-core-without-io-path-metadata-l80o5.orna"
            ))
            .unwrap_or_else(|error| panic!("core source failed without std: {}", error.code())),
        Some(bool_value(true))
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-io-path-without-snapshot-l80o5.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-core-without-io-path-metadata-l80o5.orna"
            ))
            .unwrap_or_else(|error| panic!("core source failed without std: {}", error.code())),
        Some(bool_value(true))
    );
}
