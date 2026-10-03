use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_sys_v1::{EnvironmentProvider, FilesystemProvider};
use orna_value_v1::Raw;

fn pinned_io_session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            path == "std/collection.orna"
                || path == "std/text.orna"
                || path == "std/io.orna"
                || path.starts_with("std/io/")
        })
        .collect::<Vec<_>>();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/v1-reference-library#uil5e-io-fs",
        sources.clone(),
    )
    .expect("the selected IO and filesystem source units form a captured profile");
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
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("selected IO and filesystem sources resolve against the pinned catalogue");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .expect("IO and filesystem source bodies load without unrelated std modules")
}

#[test]
fn pinned_read_lines_distinguishes_empty_files_and_preserves_terminal_lines() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("empty.txt"), "").unwrap();
    std::fs::write(root.path().join("terminal-breaks.txt"), "alpha\n\nomega\n").unwrap();

    let mut filesystem = FilesystemProvider::new();
    filesystem.allow_root(root.path()).unwrap();
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem);
    let mut session = pinned_io_session();
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
