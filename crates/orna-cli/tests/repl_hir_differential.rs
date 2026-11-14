//! Differential: repl-inline fixtures through `orna repl` on the old path and under `ORNA_REPL_HIR=1`.
//!
//! Each fixture that needs only a standalone session runs as stdin in a directory outside any Git
//! worktree, once per path. Stdout and the exit code must agree, except for the `## expected` rows
//! of `repl_hir_differing.txt`. That ledger is ratcheted exactly: a new disagreement fails, and a
//! listed fixture that now agrees is stale. The exclusion table is shared with the orna-differential
//! repl-inline ratchet, so both suites skip the same fixtures.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

const EXCLUSIONS: &str = include_str!("../../orna-differential/tests/repl_inline_exclusions.txt");
const DIFFERING: &str = include_str!("repl_hir_differing.txt");
const FIXTURE_DIRS: [&str; 3] = [
    "orna-evaluator/src/fixtures",
    "orna-evaluator/tests/fixtures",
    "orna-project-repl/src/fixtures",
];

struct Run {
    code: Option<i32>,
    stdout: Vec<u8>,
}

/// Runs `script` through `orna repl` outside any worktree, on the HIR driver or the old path.
fn run(script: &str, hir: bool) -> Run {
    let directory = tempfile::tempdir().expect("REPL working directory");
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    command
        .arg("repl")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if hir {
        command.env("ORNA_REPL_HIR", "1");
    } else {
        command.env_remove("ORNA_REPL_HIR");
    }
    let mut child = command.spawn().expect("Orna REPL process");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(script.as_bytes())
        .expect("write REPL script");
    let output = child.wait_with_output().expect("Orna REPL output");
    Run {
        code: output.status.code(),
        stdout: output.stdout,
    }
}

/// Every repl-inline fixture, keyed by its path relative to the workspace `crates` directory.
fn fixtures() -> BTreeMap<String, String> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut fixtures = BTreeMap::new();
    for dir in FIXTURE_DIRS {
        for entry in std::fs::read_dir(crates.join(dir)).expect("fixture directory") {
            let path = entry.expect("fixture entry").path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("fixture name");
            if name.starts_with("repl-inline-")
                && Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("orna"))
            {
                let source = std::fs::read_to_string(&path).expect("fixture source");
                fixtures.insert(format!("{dir}/{name}"), source);
            }
        }
    }
    fixtures
}

/// The fixture paths the shared exclusion table skips.
fn excluded() -> BTreeSet<&'static str> {
    EXCLUSIONS
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once(' ').map(|(path, _)| path))
        .collect()
}

/// The `## expected` rows: `<fixture path> <reason>` per line, keyed by path.
fn expected() -> BTreeMap<&'static str, &'static str> {
    let section = DIFFERING
        .split_once("\n## expected\n")
        .expect("differing list has an expected section")
        .1;
    section
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.split_once(' ')
                .expect("differing row is path<SPACE>reason")
        })
        .collect()
}

#[test]
fn repl_inline_fixtures_match_the_hir_driver_outside_a_worktree() {
    let excluded = excluded();
    let expected = expected();
    let mut differing = BTreeMap::new();
    for (path, source) in fixtures() {
        if excluded.contains(path.as_str()) {
            continue;
        }
        let script = format!("{}\n:quit\n", source.trim_end());
        let old = run(&script, false);
        let new = run(&script, true);
        if old.code != new.code || old.stdout != new.stdout {
            differing.insert(path, (old, new));
        }
    }
    let unexpected: Vec<String> = differing
        .iter()
        .filter(|(path, _)| !expected.contains_key(path.as_str()))
        .map(|(path, (old, new))| {
            format!(
                "{path}\n  old exit {:?}: {}\n  hir exit {:?}: {}",
                old.code,
                String::from_utf8_lossy(&old.stdout).escape_debug(),
                new.code,
                String::from_utf8_lossy(&new.stdout).escape_debug()
            )
        })
        .collect();
    let stale: Vec<&str> = expected
        .keys()
        .copied()
        .filter(|path| !differing.contains_key(*path))
        .collect();
    assert!(
        unexpected.is_empty(),
        "REPL outputs differ from the old path and are not in repl_hir_differing.txt:\n{}",
        unexpected.join("\n")
    );
    assert!(
        stale.is_empty(),
        "repl_hir_differing.txt lists fixtures that now agree: {stale:?}"
    );
}
