#[path = "support/pinned_io_std.rs"]
mod pinned_io_std;

use orna_evaluator_v1::SysHostBindingRegistry;
use orna_foundation_v1::CanonicalValue;
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::Raw;

fn verify_io_snapshot() {
    let (profile, sources) = pinned_io_std::captured_profile();
    let (filesystem_path, filesystem_source) = sources
        .iter()
        .find(|(path, _)| path == "std/io/fs.orna")
        .expect("the captured sources include std.io.fs");
    profile
        .verify_source(filesystem_path, filesystem_source)
        .expect("filesystem module bytes match the captured dependency snapshot");
    let mut changed_filesystem_source = filesystem_source.clone();
    changed_filesystem_source.push_str("\n// post-capture edit\n");
    assert!(
        profile
            .verify_source(filesystem_path, &changed_filesystem_source)
            .is_err()
    );
}

#[test]
fn pinned_read_lines_distinguishes_empty_files_and_preserves_terminal_lines() {
    verify_io_snapshot();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("empty.txt"), "").unwrap();
    std::fs::write(root.path().join("terminal-breaks.txt"), "alpha\n\nomega\n").unwrap();

    let mut filesystem = FilesystemProvider::new();
    filesystem.allow_root(root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem);
    let mut session = pinned_io_std::session();
    session
        .submit(include_str!("fixtures/stdlib-use-io-fs-mpk0d.orna"))
        .unwrap();

    let source = include_str!("fixtures/stdlib-io-fs-lines-uil5e.orna")
        .replace("ROOT_PATH", &root.path().to_string_lossy());
    assert_eq!(
        session.submit_with_sys_host_bindings(&source, &mut bindings),
        Ok(Some(
            CanonicalValue::new(Raw::Tag(
                60015,
                Box::new(Raw::Array(vec![
                    Raw::Array(vec![]),
                    Raw::Array(vec![
                        Raw::Text("alpha".into()),
                        Raw::Text(String::new()),
                        Raw::Text("omega".into()),
                        Raw::Text(String::new()),
                    ]),
                ])),
            ))
            .unwrap()
        ))
    );
}
