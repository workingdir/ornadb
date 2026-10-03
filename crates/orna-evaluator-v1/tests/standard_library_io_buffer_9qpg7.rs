#[path = "support/pinned_io_std.rs"]
mod pinned_io_std;

use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_foundation_v1::CanonicalValue;
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::Raw;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected value is canonical")
}

fn rooted(source: &str, root: &std::path::Path) -> String {
    source.replace("ROOT_PATH", &root.to_string_lossy())
}

fn bindings(root: &std::path::Path) -> SysHostBindingRegistry {
    let mut filesystem = FilesystemProvider::new();
    filesystem
        .allow_root(root)
        .expect("temporary directory is allowed");
    SysHostBindingRegistry::new(EnvironmentProvider::default()).with_filesystem_provider(filesystem)
}

#[test]
fn pinned_io_buffer_batches_real_lines_into_a_replayable_stream() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("input.txt"), "alpha\n\nomega\n").unwrap();
    let mut host = bindings(root.path());
    let mut session = pinned_io_std::session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-io-buffer-9qpg7.orna")),
        Ok(None)
    );

    let actual = session
        .submit_with_sys_host_bindings(
            &rooted(
                include_str!("fixtures/stdlib-io-buffer-read-batches-9qpg7.orna"),
                root.path(),
            ),
            &mut host,
        )
        .unwrap_or_else(|error| panic!("reading line batches failed with {}", error.code()));

    assert_eq!(
        actual,
        Some(canonical(Raw::Array(vec![
            Raw::Array(vec![Raw::Text("alpha".into()), Raw::Text(String::new())]),
            Raw::Array(vec![Raw::Text("omega".into()), Raw::Text(String::new())]),
        ])))
    );
}

#[test]
fn pinned_io_buffer_flattens_batches_and_writes_the_exact_line_bytes() {
    let root = tempfile::tempdir().unwrap();
    let mut host = bindings(root.path());
    let mut session = pinned_io_std::session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-io-buffer-9qpg7.orna")),
        Ok(None)
    );

    let actual = session
        .submit_with_sys_host_bindings(
            &rooted(
                include_str!("fixtures/stdlib-io-buffer-write-batches-9qpg7.orna"),
                root.path(),
            ),
            &mut host,
        )
        .unwrap_or_else(|error| panic!("writing line batches failed with {}", error.code()));

    assert_eq!(actual, Some(canonical(Raw::Null)));
    assert_eq!(
        std::fs::read(root.path().join("output.txt")).unwrap(),
        b"red\n\ngreen\nblue\n"
    );
}

#[test]
fn core_still_works_without_optional_io_buffer_modules() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit("1 + 2"),
        Ok(Some(canonical(Raw::Int(3.into()))))
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-io-buffer-without-snapshot-9qpg7.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        session.submit("1 + 2"),
        Ok(Some(canonical(Raw::Int(3.into()))))
    );
}
