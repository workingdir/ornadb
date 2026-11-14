//! The shadow report (`docs/design/hir-cutover.md`, section 4.2): the whole corpus run through
//! the old and the HIR pipelines, every disagreement classified against its expected ledger.
//!
//! The three pipelines are classified by the same functions as their ratchet tests, so a
//! difference is unexplained in the report exactly when the matching test fails on it:
//! diagnostics through [`diagnostics_parity`], the executor through [`ExecAllowances`] and the REPL
//! through [`repl_inline`]. A report is clean when nothing is unexplained and no ledger entry is stale.

use std::{collections::BTreeMap, collections::BTreeSet, fmt::Write as _};

use orna_differential::{
    case::Corpus,
    diagnostics_parity,
    exec_allowance::{ExecAllowances, Kind},
    observe_exec::{self, ExecOutcome},
    observe_old,
};

use super::repl_inline;

/// One ledger row: the expected disagreements of one class, with how many cases they cover.
#[derive(Debug, PartialEq, Eq)]
pub struct LedgerRow {
    /// The class the disagreements are grouped under.
    pub class: String,
    /// The recorded reason the class is expected to disagree.
    pub reason: String,
    /// The number of cases the class covers.
    pub count: usize,
}

/// The diagnostic-code parity of every module and expression case.
#[derive(Debug, Default)]
pub struct DiagnosticsSection {
    /// Cases both the checker and HIR analysed.
    pub compared: usize,
    /// Cases one of the two pipelines did not analyse.
    pub skipped: usize,
    /// Cases whose code sets differ, expected or not.
    pub differing: usize,
    /// Expected differing cases, by the code they differ on.
    pub ledger: Vec<LedgerRow>,
    /// Cases whose differences are listed in the ledger.
    pub expected: usize,
    /// Differences not in the ledger, and differing codes without a recorded reason.
    pub unexplained: Vec<String>,
    /// Ledger lines whose case no longer differs.
    pub stale: Vec<String>,
}

/// The executor's agreement with the old pipeline over the evaluable cases.
#[derive(Debug, Default)]
pub struct ExecutorSection {
    /// Cases the corpus marks as evaluated.
    pub evaluable: usize,
    /// Cases the executor ran to a value or a language error.
    pub run: usize,
    /// Cases the executor cannot run yet.
    pub not_yet: usize,
    /// Run cases whose outcome matches the old pipeline's.
    pub agreed: usize,
    /// Run cases whose step count matches the old pipeline's exactly.
    pub steps_exact: usize,
    /// Allowed differences, by the class of allowance that covers them.
    pub ledger: Vec<LedgerRow>,
    /// Differences not covered by an allowance, and failed old-pipeline observations.
    pub unexplained: Vec<String>,
    /// Allowances whose case no longer differs.
    pub stale: Vec<String>,
}

/// The REPL-inline fixtures run through the new and the old REPL.
#[derive(Debug, Default)]
pub struct ReplSection {
    /// Fixtures found in the fixture directories.
    pub fixtures: usize,
    /// Fixtures excluded because they need a repository, a snapshot, a watch, persistence or the host.
    pub excluded: usize,
    /// Fixtures compared: the fixtures less the exclusions.
    pub runnable: usize,
    /// Compared fixtures whose outcomes match.
    pub agreed: usize,
    /// Compared fixtures whose disagreement is listed in the ledger, by its reason.
    pub ledger: Vec<LedgerRow>,
    /// Disagreements not in the ledger.
    pub unexplained: Vec<String>,
    /// Ledger, exclusion and fixture entries that no longer match a comparison.
    pub stale: Vec<String>,
}

/// The shadow report over the whole corpus.
#[derive(Debug, Default)]
pub struct Report {
    /// Every case in the corpus.
    pub cases: usize,
    /// The diagnostic-code parity.
    pub diagnostics: DiagnosticsSection,
    /// The executor's agreement with the old pipeline.
    pub executor: ExecutorSection,
    /// The REPL-inline agreement with the old REPL.
    pub repl: ReplSection,
}

impl Report {
    /// Whether every difference is explained and every ledger entry is current.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.unexplained_count() == 0 && self.stale_count() == 0
    }

    /// The number of unexplained differences across the three pipelines.
    #[must_use]
    pub fn unexplained_count(&self) -> usize {
        self.diagnostics.unexplained.len()
            + self.executor.unexplained.len()
            + self.repl.unexplained.len()
    }

    /// The number of stale ledger entries across the three pipelines.
    #[must_use]
    pub fn stale_count(&self) -> usize {
        self.diagnostics.stale.len() + self.executor.stale.len() + self.repl.stale.len()
    }

    /// Renders the report as markdown: a summary table, then one section per pipeline.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let d = &self.diagnostics;
        let e = &self.executor;
        let r = &self.repl;
        writeln!(out, "# HIR shadow report\n").expect("writing to a String is infallible");
        writeln!(
            out,
            "Corpus: {} cases. Clean: {}. Unexplained differences: {}. Stale ledger entries: {}.\n",
            self.cases,
            yes_no(self.is_clean()),
            self.unexplained_count(),
            self.stale_count()
        )
        .expect("writing to a String is infallible");
        writeln!(out, "## Summary\n").expect("writing to a String is infallible");
        writeln!(
            out,
            "| Pipeline | Compared | Agreeing | Expected differences | Unexplained | Stale |"
        )
        .expect("writing to a String is infallible");
        writeln!(out, "|---|---:|---:|---:|---:|---:|").expect("writing to a String is infallible");
        writeln!(
            out,
            "| Diagnostic codes | {} | {} | {} | {} | {} |",
            d.compared,
            d.compared - d.differing,
            d.expected,
            d.unexplained.len(),
            d.stale.len()
        )
        .expect("writing to a String is infallible");
        writeln!(
            out,
            "| Executor | {} | {} | {} | {} | {} |",
            e.run,
            e.agreed,
            allowed(&e.ledger),
            e.unexplained.len(),
            e.stale.len()
        )
        .expect("writing to a String is infallible");
        writeln!(
            out,
            "| REPL inline | {} | {} | {} | {} | {} |\n",
            r.runnable,
            r.agreed,
            allowed(&r.ledger),
            r.unexplained.len(),
            r.stale.len()
        )
        .expect("writing to a String is infallible");

        writeln!(out, "## Diagnostic codes\n").expect("writing to a String is infallible");
        writeln!(
            out,
            "{} cases compared, {} skipped (one pipeline did not analyse them).\n",
            d.compared, d.skipped
        )
        .expect("writing to a String is infallible");
        render_ledger(&mut out, "Ledger by code", &d.ledger, true);
        render_list(&mut out, "Unexplained differences", &d.unexplained);
        render_list(&mut out, "Stale ledger entries", &d.stale);

        writeln!(out, "## Executor\n").expect("writing to a String is infallible");
        writeln!(
            out,
            "{} cases evaluated, {} ran, {} not yet runnable. {} run cases agree; {} of them charge exactly the old pipeline's steps.\n",
            e.evaluable, e.run, e.not_yet, e.agreed, e.steps_exact
        )
        .expect("writing to a String is infallible");
        render_ledger(&mut out, "Allowances by class", &e.ledger, true);
        render_list(&mut out, "Unexplained differences", &e.unexplained);
        render_list(&mut out, "Stale allowances", &e.stale);

        writeln!(out, "## REPL inline\n").expect("writing to a String is infallible");
        writeln!(
            out,
            "{} fixtures found, {} excluded, {} compared, {} agree.\n",
            r.fixtures, r.excluded, r.runnable, r.agreed
        )
        .expect("writing to a String is infallible");
        render_ledger(
            &mut out,
            "Expected disagreements by reason",
            &r.ledger,
            false,
        );
        render_list(&mut out, "Unexplained disagreements", &r.unexplained);
        render_list(&mut out, "Stale ledger entries", &r.stale);
        out
    }
}

/// Runs the whole corpus through the old and HIR pipelines and classifies every disagreement.
///
/// # Errors
///
/// Returns an error when the executor allowance tables do not parse.
pub fn run(corpus: &Corpus) -> Result<Report, String> {
    Ok(Report {
        cases: corpus.cases.len(),
        diagnostics: diagnostics(corpus),
        executor: executor(corpus)?,
        repl: repl(),
    })
}

fn diagnostics(corpus: &Corpus) -> DiagnosticsSection {
    let measured = diagnostics_parity::measure(corpus);
    let expected = diagnostics_parity::expected_differences(diagnostics_parity::DIFFERING);
    let mut ledger: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut unexplained = Vec::new();
    let mut differing_lines = BTreeSet::new();
    let mut matched = 0;
    for (case, (old, new)) in &measured.differing {
        let line = diagnostics_parity::difference_line(case, old, new);
        if expected.contains(&line) {
            matched += 1;
            for code in old.symmetric_difference(new) {
                let reason = diagnostics_parity::recorded_reason(code).unwrap_or("UNRECORDED");
                *ledger.entry((code.clone(), reason.to_owned())).or_default() += 1;
            }
        } else {
            unexplained.push(line.clone());
        }
        differing_lines.insert(line);
    }
    for (code, row) in &measured.table {
        if row[1] + row[2] > 0 && diagnostics_parity::recorded_reason(code).is_none() {
            unexplained.push(format!("{code}: differing code without a recorded reason"));
        }
    }
    DiagnosticsSection {
        compared: measured.compared,
        skipped: measured.skipped,
        differing: measured.differing.len(),
        ledger: rows(ledger),
        expected: matched,
        unexplained,
        stale: expected.difference(&differing_lines).cloned().collect(),
    }
}

fn executor(corpus: &Corpus) -> Result<ExecutorSection, String> {
    let mut allowances = ExecAllowances::load()?;
    let mut section = ExecutorSection {
        evaluable: corpus.cases.iter().filter(|case| case.evaluate).count(),
        ..ExecutorSection::default()
    };
    let mut ledger: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut step_differences: Vec<(String, u64, u64)> = Vec::new();
    for case in &corpus.cases {
        match observe_exec::run_case(case) {
            ExecOutcome::NotYet(_) => section.not_yet += 1,
            ExecOutcome::Ran { outcome, steps } => {
                section.run += 1;
                let observation = match observe_old::observe(case) {
                    Ok(observation) => observation,
                    Err(error) => {
                        section
                            .unexplained
                            .push(format!("{}: the old pipeline failed: {error}", case.id));
                        continue;
                    }
                };
                if observation.outcome != outcome {
                    let allowed = outcome.as_ref().and_then(|executor| {
                        allowances.outcome_allowed(case, executor, observation.outcome.as_ref())
                    });
                    match allowed {
                        Some(kind) => count(&mut ledger, kind),
                        None => section.unexplained.push(format!(
                            "{}: executor {outcome:?}, old {:?}",
                            case.id, observation.outcome
                        )),
                    }
                    continue;
                }
                section.agreed += 1;
                match observation.steps {
                    Some(old) if old == steps => section.steps_exact += 1,
                    Some(old) => step_differences.push((case.id.clone(), old, steps)),
                    None => {}
                }
            }
        }
    }
    for (id, old, executor) in step_differences {
        if allowances.step_allowed(&id, old, executor) {
            count(&mut ledger, Kind::Step);
        } else {
            section.unexplained.push(format!(
                "{id}: executor charges {executor} steps, old pipeline {old}"
            ));
        }
    }
    section.ledger = rows(ledger);
    section.stale = allowances.stale();
    Ok(section)
}

fn repl() -> ReplSection {
    let fixtures = repl_inline::collect_fixtures();
    let exclusions = repl_inline::parse_exclusions();
    let expected = repl_inline::parse_expected();
    let mut section = ReplSection {
        fixtures: fixtures.len(),
        excluded: exclusions.len(),
        runnable: fixtures.len() - exclusions.len(),
        ..ReplSection::default()
    };
    for path in exclusions.keys() {
        if !fixtures.contains_key(path) {
            section.stale.push(format!("stale exclusion: {path}"));
        }
    }
    let mut disagreed = BTreeSet::new();
    for (path, source) in &fixtures {
        if exclusions.contains_key(path) {
            continue;
        }
        let (new, old) = (
            repl_inline::new_outcome(source),
            repl_inline::old_outcome(source),
        );
        if new == old {
            section.agreed += 1;
        } else {
            disagreed.insert(path.clone());
            match expected.get(path) {
                Some(reason) => {
                    let row = (String::from("repl"), reason.clone());
                    *ledger_entry(&mut section.ledger, row) += 1;
                }
                None => section
                    .unexplained
                    .push(format!("{path}: new={new:?} old={old:?}")),
            }
        }
    }
    for path in expected.keys() {
        if !fixtures.contains_key(path) {
            section.stale.push(format!("stale expectation: {path}"));
        } else if !disagreed.contains(path) {
            section.stale.push(format!(
                "stale expectation, now agrees with the old REPL: {path}"
            ));
        }
    }
    section
}

fn ledger_entry(rows: &mut Vec<LedgerRow>, key: (String, String)) -> &mut usize {
    let index = match rows
        .iter()
        .position(|row| row.class == key.0 && row.reason == key.1)
    {
        Some(index) => index,
        None => {
            rows.push(LedgerRow {
                class: key.0,
                reason: key.1,
                count: 0,
            });
            rows.len() - 1
        }
    };
    &mut rows[index].count
}

fn count(ledger: &mut BTreeMap<(String, String), usize>, kind: Kind) {
    *ledger
        .entry((kind.name().to_owned(), executor_reason(kind).to_owned()))
        .or_default() += 1;
}

fn executor_reason(kind: Kind) -> &'static str {
    match kind {
        Kind::Harness => {
            "the H0 observer has no table store, so the old pipeline cannot reach the table"
        }
        Kind::Representation => {
            "the old pipeline answers ORNA-EVAL-VALUE for a result the executor returns as a raw blob"
        }
        Kind::ExecutorGap => "a listed executor error and a listed old error, for one case",
        Kind::Step => "the executor charges a different step count for an agreeing run",
        Kind::Currency => {
            "the old pipeline cannot evaluate currency values; HIR evaluates Money per spec"
        }
    }
}

fn rows(ledger: BTreeMap<(String, String), usize>) -> Vec<LedgerRow> {
    ledger
        .into_iter()
        .map(|((class, reason), count)| LedgerRow {
            class,
            reason,
            count,
        })
        .collect()
}

fn allowed(ledger: &[LedgerRow]) -> usize {
    ledger.iter().map(|row| row.count).sum()
}

fn yes_no(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

fn render_ledger(out: &mut String, title: &str, ledger: &[LedgerRow], has_class: bool) {
    writeln!(out, "### {title}\n").expect("writing to a String is infallible");
    if ledger.is_empty() {
        writeln!(out, "None.\n").expect("writing to a String is infallible");
        return;
    }
    if has_class {
        writeln!(out, "| Class | Cases | Reason |").expect("writing to a String is infallible");
        writeln!(out, "|---|---:|---|").expect("writing to a String is infallible");
        for row in ledger {
            writeln!(
                out,
                "| {} | {} | {} |",
                cell(&row.class),
                row.count,
                cell(&row.reason)
            )
            .expect("writing to a String is infallible");
        }
    } else {
        writeln!(out, "| Reason | Cases |").expect("writing to a String is infallible");
        writeln!(out, "|---|---:|").expect("writing to a String is infallible");
        for row in ledger {
            writeln!(out, "| {} | {} |", cell(&row.reason), row.count)
                .expect("writing to a String is infallible");
        }
    }
    writeln!(out).expect("writing to a String is infallible");
}

fn render_list(out: &mut String, title: &str, items: &[String]) {
    writeln!(out, "### {title}\n").expect("writing to a String is infallible");
    if items.is_empty() {
        writeln!(out, "None.\n").expect("writing to a String is infallible");
        return;
    }
    for item in items {
        writeln!(out, "- {}", cell(item)).expect("writing to a String is infallible");
    }
    writeln!(out).expect("writing to a String is infallible");
}
