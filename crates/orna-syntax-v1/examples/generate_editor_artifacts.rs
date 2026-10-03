use std::{env, fs, path::Path, process::ExitCode};

use orna_syntax_v1::generated_editor_artifacts;

fn main() -> ExitCode {
    let check_only = match env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => {
            eprintln!("usage: generate_editor_artifacts [--check]");
            return ExitCode::from(2);
        }
    };
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-syntax-v1 is under crates/");
    let mut failed = false;
    for artifact in generated_editor_artifacts() {
        let path = workspace_root.join(artifact.path);
        if check_only {
            match fs::read_to_string(&path) {
                Ok(actual) if actual == artifact.contents => {
                    println!("verified {}", artifact.path);
                }
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
            continue;
        }
        if let Some(parent) = path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            eprintln!("cannot create {}: {error}", parent.display());
            failed = true;
            continue;
        }
        match fs::write(&path, artifact.contents) {
            Ok(()) => println!("generated {}", artifact.path),
            Err(error) => {
                eprintln!("cannot write {}: {error}", path.display());
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
