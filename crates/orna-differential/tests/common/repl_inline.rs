//! The repl-inline fixtures run through the new REPL session and through the old `ReplSession`.
//!
//! The ratchet test (`tests/repl_inline_differential.rs`) and the shadow report both read the fixtures, the
//! exclusion table and the expected-disagreement ledger through this module, so a disagreement is classified the same way.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use orna_evaluator::{Limits, ReplSession};
use orna_repl::{Session, submit};
use orna_value::Raw;

/// Directories, relative to the workspace `crates` directory, that hold repl-inline fixtures.
pub const FIXTURE_DIRS: [&str; 3] = [
    "orna-evaluator/src/fixtures",
    "orna-evaluator/tests/fixtures",
    "orna-project-repl/src/fixtures",
];

/// Fixtures that need a repository, a snapshot, a watch, persistence or the host: `<path> <reason>` per line.
pub const EXCLUSIONS: &str = include_str!("../repl_inline_exclusions.txt");

/// Runnable fixtures that disagree with the old REPL, listed in the `## expected` section.
pub const DIFFERING: &str = include_str!("../repl_inline_differing.txt");

/// The result of submitting one fixture to a REPL.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The submission was accepted, with a canonical value or nothing.
    Accepted(Option<Raw>),
    /// The submission was rejected with this error code.
    Rejected(String),
}

/// Submits `source` to the new REPL session.
#[must_use]
pub fn new_outcome(source: &str) -> Outcome {
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

/// Submits `source` to a fresh old `ReplSession`.
#[must_use]
pub fn old_outcome(source: &str) -> Outcome {
    let mut session = ReplSession::new(Limits::default());
    match session.submit(source) {
        Ok(None) => Outcome::Accepted(None),
        Ok(Some(value)) => Outcome::Accepted(Some(value.raw().clone())),
        Err(error) => Outcome::Rejected(error.code().to_owned()),
    }
}

/// Every repl-inline fixture, keyed by its path relative to the workspace `crates` directory.
#[must_use]
pub fn collect_fixtures() -> BTreeMap<String, String> {
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
#[must_use]
pub fn parse_exclusions() -> BTreeMap<String, String> {
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

/// The expected-disagreement table: the `## expected` section of `repl_inline_differing.txt`, one
/// `<fixture path> <reason>` per line. Blank lines and `#` comments are ignored.
#[must_use]
pub fn parse_expected() -> BTreeMap<String, String> {
    let section = DIFFERING
        .split_once("\n## expected\n")
        .expect("differing list has an expected section")
        .1;
    section
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (path, reason) = line
                .split_once(' ')
                .expect("expected line is path<SPACE>reason");
            (path.to_owned(), reason.to_owned())
        })
        .collect()
}
