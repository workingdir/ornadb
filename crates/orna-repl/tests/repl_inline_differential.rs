//! Differential check of the repl-inline fixtures against the old evaluator's REPL.
//!
//! The fixtures carry no expected-stdout files; their expectations are asserts in `orna-evaluator` and
//! `orna-project-repl`. Each fixture that needs only a standalone session is run through the new session and
//! through a fresh old `ReplSession`, and the canonical result or the error code must match. Fixtures that need
//! a repository, a snapshot, a watch or persistence are listed in `repl_inline_exclusions.txt` with a reason.
//! `AGREED_FLOOR` is a ratchet: the agreed count must not drop, and it is raised when it rises.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use orna_evaluator::{Limits, ReplSession};
use orna_repl::{Session, submit};
use orna_value::Raw;

const FIXTURE_DIRS: [&str; 3] = [
    "orna-evaluator/src/fixtures",
    "orna-evaluator/tests/fixtures",
    "orna-project-repl/src/fixtures",
];
const EXCLUSIONS: &str = include_str!("repl_inline_exclusions.txt");
const AGREED_FLOOR: usize = 7;

#[derive(Debug, PartialEq)]
enum Outcome {
    Accepted(Option<Raw>),
    Rejected(String),
}

fn new_outcome(source: &str) -> Outcome {
    let mut session = Session::new();
    match submit(&mut session, source) {
        Ok(None) => Outcome::Accepted(None),
        Ok(Some(value)) => match value.to_raw() {
            Ok(raw) => Outcome::Accepted(Some(raw)),
            Err(code) => Outcome::Rejected(code.to_owned()),
        },
        Err(error) => Outcome::Rejected(error.code().to_owned()),
    }
}

fn old_outcome(source: &str) -> Outcome {
    let mut session = ReplSession::new(Limits::default());
    match session.submit(source) {
        Ok(None) => Outcome::Accepted(None),
        Ok(Some(value)) => Outcome::Accepted(Some(value.raw().clone())),
        Err(error) => Outcome::Rejected(error.code().to_owned()),
    }
}

/// Every repl-inline fixture, keyed by its path relative to the workspace `crates` directory.
fn collect_fixtures() -> BTreeMap<String, String> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut fixtures = BTreeMap::new();
    for dir in FIXTURE_DIRS {
        for entry in fs::read_dir(crates.join(dir)).expect("fixture directory") {
            let path = entry.expect("fixture entry").path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("fixture name");
            if name.starts_with("repl-inline-") && name.ends_with(".orna") {
                let source = fs::read_to_string(&path).expect("fixture source");
                fixtures.insert(format!("{dir}/{name}"), source);
            }
        }
    }
    fixtures
}

/// The exclusion table: `<fixture path> <reason>` per line; blank lines and `#` comments are ignored.
fn parse_exclusions() -> BTreeMap<String, String> {
    EXCLUSIONS
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (path, reason) = line
                .split_once(' ')
                .expect("exclusion line is path<SPACE>reason");
            (path.to_owned(), reason.to_owned())
        })
        .collect()
}

#[test]
fn repl_inline_fixtures_agree_with_the_old_repl() {
    let fixtures = collect_fixtures();
    let exclusions = parse_exclusions();
    for path in exclusions.keys() {
        assert!(fixtures.contains_key(path), "stale exclusion: {path}");
    }

    let mut agreed = 0;
    let mut disagreed = Vec::new();
    for (path, source) in &fixtures {
        if exclusions.contains_key(path) {
            continue;
        }
        let (new, old) = (new_outcome(source), old_outcome(source));
        if new == old {
            agreed += 1;
        } else {
            eprintln!("disagree: {path}\n  new={new:?}\n  old={old:?}");
            disagreed.push(path.clone());
        }
    }
    let runnable = fixtures.len() - exclusions.len();
    eprintln!(
        "repl-inline differential: agreed {agreed} of {runnable} runnable, {} total",
        fixtures.len()
    );
    eprintln!("disagreements: {}", disagreed.len());
    assert!(
        agreed >= AGREED_FLOOR,
        "agreed {agreed} dropped below the ratchet floor {AGREED_FLOOR}"
    );
}
