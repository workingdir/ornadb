use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use orna_syntax_v1::editor;

fn main() -> ExitCode {
    let check = match env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => {
            eprintln!("usage: generate_editor_artifacts [--check]");
            return ExitCode::from(2);
        }
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-syntax-v1 is under crates/");
    let artifacts = editor::generated_artifacts();
    let mut failed = false;
    for artifact in &artifacts {
        let path = root.join(artifact.path);
        if check {
            match fs::read_to_string(&path) {
                Ok(actual) if actual == artifact.contents => println!("verified {}", artifact.path),
                Ok(_) => {
                    eprintln!(
                        "stale editor artifact {}; regenerate with `just editor-artifacts`",
                        artifact.path
                    );
                    failed = true;
                }
                Err(error) => {
                    eprintln!("cannot read {}: {error}", path.display());
                    failed = true;
                }
            }
        } else {
            if let Some(parent) = path.parent()
                && let Err(error) = fs::create_dir_all(parent)
            {
                eprintln!("cannot create {}: {error}", parent.display());
                failed = true;
                continue;
            }
            match fs::write(&path, &artifact.contents) {
                Ok(()) => println!("generated {}", artifact.path),
                Err(error) => {
                    eprintln!("cannot write {}: {error}", path.display());
                    failed = true;
                }
            }
        }
    }
    let expected = artifacts
        .iter()
        .filter(|artifact| artifact.path.starts_with("editors/"))
        .map(|artifact| artifact.path.to_owned())
        .collect::<BTreeSet<_>>();
    match editor_files(&root.join("editors"), &root) {
        Ok(actual) if actual == expected => {
            println!(
                "verified complete editor artifact set ({} files)",
                actual.len()
            );
        }
        Ok(actual) => {
            for extra in actual.difference(&expected) {
                eprintln!(
                    "unmanaged editor file {}; add it to the generator or remove it",
                    extra
                );
            }
            for missing in expected.difference(&actual) {
                eprintln!("missing generated editor file {missing}");
            }
            failed = true;
        }
        Err(error) => {
            eprintln!("cannot enumerate generated editor files: {error}");
            failed = true;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn editor_files(directory: &Path, root: &Path) -> std::io::Result<BTreeSet<String>> {
    let mut files = BTreeSet::new();
    collect_editor_files(directory, root, &mut files)?;
    Ok(files)
}

fn collect_editor_files(
    directory: &Path,
    root: &Path,
    files: &mut BTreeSet<String>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path: PathBuf = entry.path();
        if path.is_dir() {
            collect_editor_files(&path, root, files)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("editor files stay under the repository root")
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(relative);
        }
    }
    Ok(())
}
