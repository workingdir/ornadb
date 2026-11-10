//! The differential harness for the HIR strangler migration (stage H0).
//!
//! `docs/design/hir.md` section 6.2 defines this crate: one loader that turns
//! every program the repository already owns into a [`case::Case`], one
//! observer per pipeline that runs a case and records an [`Observation`], and
//! a comparator that decides whether two observations agree.
//!
//! # What H0 proves
//!
//! No new pipeline exists yet, so the harness is run old against old: every
//! case is observed twice and the two observations must be identical. That is
//! the acceptance proof of section 6.3 H0 — "zero differences, zero flaky
//! cases" — and it proves the loader, the observation format and the
//! comparator before any new code can hide behind them.
//!
//! # The ratchet
//!
//! Two checked-in files under `tests/differential/` are ratchets, in the
//! manner of `scripts/known-test-failures.toml`:
//!
//! * `expected-differences.toml` holds one entry per accepted difference
//!   between two pipelines. A run fails on a difference with no entry and on an
//!   entry that no longer differs, so the list can only shrink.
//! * `excluded.toml` names every `.orna` program in the repository the loader
//!   does not turn into a case, with a reason and the number of files its scope
//!   holds. A run fails when a program is neither loaded nor excluded, when a
//!   scope's count drifts, or when a scope no longer holds an excluded file.
//!
//! [`compare`] owns both ratchets; [`case`] owns the corpus account.
//!
//! # The new pipeline
//!
//! There is no `observe_new` yet. Stage H1 adds it next to
//! [`observe_old`](observe_old), and the comparator already takes two
//! observations, so the only missing piece is a second driver. The recorded
//! `calls` list is empty in H0 for the same reason: the evaluator's
//! `route-trace` hook exists, but the classification it is compared against
//! arrives with H1.

#![deny(missing_docs)]

pub mod case;
pub mod compare;
pub mod observe_old;
mod toml_lite;

use std::path::{Path, PathBuf};

/// What one pipeline did with one case.
///
/// The fields are those of `docs/design/hir.md` section 6.2, with one
/// addition: [`Observation::inputs`], because a REPL case submits an ordered
/// list of inputs and a single `outcome` cannot describe the session's state
/// evolution.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Observation {
    /// Diagnostic codes, sorted and deduplicated. Spans and messages are not
    /// compared: they are new information the second pipeline may carry.
    pub diagnostics: Vec<String>,
    /// The canonical value bytes of the case's result, or the error code that
    /// ended it. `None` when the case stopped before evaluation.
    pub outcome: Option<Result<Vec<u8>, String>>,
    /// The smallest step budget that reproduces [`Observation::outcome`], where
    /// the pipeline exposes a step budget or a monotone budget predicate.
    pub steps: Option<u64>,
    /// One record per executed call, in execution order. Empty until stage H1
    /// records the old evaluator's routing (`route-trace`).
    pub calls: Vec<CallRecord>,
    /// Table writes in staging order: table, canonical key, and the row bytes
    /// the write installs (`None` deletes).
    pub writes: Vec<Write>,
    /// One entry per submitted input of a REPL case, in submission order.
    /// Empty for module and expression cases.
    pub inputs: Vec<InputOutcome>,
}

impl Observation {
    /// Returns the error code of a failed outcome, if the case failed.
    #[must_use]
    pub fn outcome_code(&self) -> Option<&str> {
        match &self.outcome {
            Some(Err(code)) => Some(code.as_str()),
            _ => None,
        }
    }
}

/// One executed call: where it is, and which branch of the old call cascade
/// served it (`docs/design/hir.md` section 1.3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallRecord {
    /// Logical path of the module the call is written in.
    pub logical_path: String,
    /// Byte range of the callee expression in that module's source.
    pub start: usize,
    /// End of the callee expression.
    pub end: usize,
    /// The cascade branch that served the call.
    pub route: Route,
}

/// The nine branches of `Evaluator::call`, in decision order.
///
/// Section 1.3: the order is the semantics, so a classification that disagrees
/// with this order is a behaviour difference, not a cosmetic one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Route {
    /// Decimal `.divide` on an unresolved callee.
    DecimalDivide,
    /// A `std.encoding.*` codec decode.
    CodecDecode,
    /// `std.ui.action`.
    UiAction,
    /// A root intrinsic: `Some`, `now`, `uuid7`, `error`, `fail`.
    RootIntrinsic,
    /// A relation source.
    RelationSource,
    /// A relation stage.
    RelationStage,
    /// A `sys` host call.
    SysHost,
    /// A native function.
    Native,
    /// A source function or closure.
    SourceFunction,
}

/// One staged table write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Write {
    /// Table name as the admitted schema spells it.
    pub table: String,
    /// Canonical key bytes.
    pub key: Vec<u8>,
    /// Canonical row bytes, or `None` for a delete.
    pub value: Option<Vec<u8>>,
}

/// The result of one submitted REPL input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputOutcome {
    /// Zero-based index into the case's input list.
    pub input: usize,
    /// The value the input produced, an error code, or `None` when the input
    /// produced no value (for example a declaration).
    pub result: Option<Result<Vec<u8>, String>>,
}

/// Returns the repository root this crate belongs to.
///
/// The corpus, the reference manifest and both ratchet files are read from the
/// checkout the crate is compiled in, so a case id is a repository-relative
/// path and a failure names a file the reader can open.
#[must_use]
pub fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/orna-differential is two levels below the repository root")
        .to_path_buf()
}

/// Returns the directory holding `expected-differences.toml` and `excluded.toml`.
#[must_use]
pub fn differential_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/differential")
}

/// Reads one UTF-8 file and reports the repository-relative path on failure.
pub(crate) fn read_file(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}
