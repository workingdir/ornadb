//! The comparator and the expected-difference ratchet.
//!
//! Two observations of one case agree when every compared field agrees
//! (`docs/design/hir.md` section 6.2):
//!
//! * `diagnostics`: as sorted code sets; spans and messages are new information
//!   and not a difference;
//! * `outcome`: byte for byte;
//! * `steps`: exactly (the per-case allowance mechanism arrives with the step
//!   parity work in H4; H0 records counts only);
//! * `calls`: in order, by span and route (the mapping of section 6.3 arrives in
//!   H1, which is when the field starts being populated);
//! * `writes`: in staging order;
//! * `inputs`: in submission order, one per submitted REPL input.
//!
//! A run fails on a difference with no entry in `expected-differences.toml` and
//! on an entry that no longer differs, so the recorded list only shrinks.

use std::collections::{BTreeMap, BTreeSet};

use crate::observe_new::ClassifiedCall;
use crate::{
    CallRecord, InputOutcome, Observation, ResolutionObservation, Route, Write, differential_dir,
    read_file, toml_lite,
};

/// One disagreement between two observations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Difference {
    /// The case the disagreement belongs to.
    pub case: String,
    /// The field that disagrees.
    pub field: &'static str,
    /// The first pipeline's value, rendered.
    pub left: String,
    /// The second pipeline's value, rendered.
    pub right: String,
}

impl Difference {
    /// Renders the difference for a failure message.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{}: {} differs\n  first:  {}\n  second: {}",
            self.case, self.field, self.left, self.right
        )
    }
}

/// Compares two observations of one case field by field.
#[must_use]
pub fn compare(case_id: &str, first: &Observation, second: &Observation) -> Vec<Difference> {
    let mut differences = Vec::new();
    if first.diagnostics != second.diagnostics {
        differences.push(difference(
            case_id,
            "diagnostics",
            first.diagnostics.join(","),
            second.diagnostics.join(","),
        ));
    }
    if first.outcome != second.outcome {
        differences.push(difference(
            case_id,
            "outcome",
            render_outcome(&first.outcome),
            render_outcome(&second.outcome),
        ));
    }
    if first.steps != second.steps {
        differences.push(difference(
            case_id,
            "steps",
            render_steps(first.steps),
            render_steps(second.steps),
        ));
    }
    if first.calls != second.calls {
        differences.push(difference(
            case_id,
            "calls",
            render_calls(first),
            render_calls(second),
        ));
    }
    if first.writes != second.writes {
        differences.push(difference(
            case_id,
            "writes",
            render_writes(&first.writes),
            render_writes(&second.writes),
        ));
    }
    if first.inputs != second.inputs {
        differences.push(difference(
            case_id,
            "inputs",
            render_inputs(&first.inputs),
            render_inputs(&second.inputs),
        ));
    }
    differences
}

fn difference(case: &str, field: &'static str, left: String, right: String) -> Difference {
    Difference {
        case: case.to_owned(),
        field,
        left,
        right,
    }
}

/// Renders an observation's outcome for a message or a ratchet entry.
#[must_use]
pub fn render_outcome(outcome: &Option<Result<Vec<u8>, String>>) -> String {
    match outcome {
        None => "none".to_owned(),
        Some(Ok(bytes)) => format!("value:{}", encode(bytes)),
        Some(Err(code)) => format!("error:{code}"),
    }
}

fn render_steps(steps: Option<u64>) -> String {
    steps.map_or_else(|| "unmeasured".to_owned(), |steps| steps.to_string())
}

fn render_calls(observation: &Observation) -> String {
    observation
        .calls
        .iter()
        .map(|call| {
            format!(
                "{}@{}..{}={:?}",
                call.logical_path, call.start, call.end, call.route
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_writes(writes: &[Write]) -> String {
    writes
        .iter()
        .map(|write| {
            format!(
                "{}={}={}",
                write.table,
                encode(&write.key),
                write
                    .value
                    .as_deref()
                    .map_or_else(|| "-".to_owned(), encode)
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_inputs(inputs: &[InputOutcome]) -> String {
    inputs
        .iter()
        .map(|input| {
            format!(
                "{}:{}",
                input.input,
                match &input.result {
                    None => "none".to_owned(),
                    Some(Ok(bytes)) => format!("value:{}", encode(bytes)),
                    Some(Err(code)) => format!("error:{code}"),
                }
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compares the old evaluator's route trace with the HIR classification.
///
/// Design H1 compares a call *at its span*: for every call the old evaluator
/// executed, the HIR `Callee` at the callee's span must map to the recorded
/// route. Execution order, repetition and HIR calls that never run are not
/// differences, so the comparison keys by `(module, start, end)`.
#[must_use]
pub fn compare_calls(
    case_id: &str,
    old: &[CallRecord],
    new: &[ClassifiedCall],
) -> (Vec<Difference>, usize) {
    let mut classified: BTreeMap<(String, usize, usize), BTreeSet<Route>> = BTreeMap::new();
    let mut unobservable: BTreeSet<(String, usize, usize)> = BTreeSet::new();
    for record in new {
        let key = (record.logical_path.clone(), record.start, record.end);
        match record.route {
            Some(route) => {
                classified.entry(key).or_default().insert(route);
            }
            None => {
                unobservable.insert(key);
            }
        }
    }
    let mut differences = Vec::new();
    let mut skipped = 0usize;
    let mut seen: BTreeSet<(String, String, String)> = BTreeSet::new();
    for record in old {
        let key = (record.logical_path.clone(), record.start, record.end);
        let left = format!(
            "{}@{}..{}={:?}",
            record.logical_path, record.start, record.end, record.route
        );
        let rendered = match classified.get(&key) {
            Some(routes) if routes.contains(&record.route) => continue,
            Some(routes) => (
                left,
                routes
                    .iter()
                    .map(|route| format!("{route:?}"))
                    .collect::<Vec<_>>()
                    .join("|"),
            ),
            None if unobservable.contains(&key) => {
                skipped += 1;
                continue;
            }
            None => (left, "no HIR call".to_owned()),
        };
        if seen.insert((rendered.0.clone(), rendered.1.clone(), case_id.to_owned())) {
            differences.push(difference(
                case_id,
                "call-classification",
                rendered.0,
                rendered.1,
            ));
        }
    }
    (differences, skipped)
}

/// Compares the old checker's name resolution with the HIR resolver's.
///
/// The comparison is by name because the checker's diagnostics carry no span:
/// the two pipelines agree when every spelling is unresolved in both, with the
/// same occurrence count, and when they report the same number of ambiguities.
#[must_use]
pub fn compare_resolution(
    case_id: &str,
    old: &ResolutionObservation,
    new: &ResolutionObservation,
) -> Vec<Difference> {
    let mut differences = Vec::new();
    let names = old
        .unresolved
        .keys()
        .chain(new.unresolved.keys())
        .collect::<BTreeSet<_>>();
    for name in names {
        let left = old.unresolved.get(name).copied().unwrap_or(0);
        let right = new.unresolved.get(name).copied().unwrap_or(0);
        if left != right {
            differences.push(difference(
                case_id,
                "resolution-unresolved",
                format!("{name}:{left}"),
                format!("{name}:{right}"),
            ));
        }
    }
    if old.ambiguous != new.ambiguous {
        differences.push(difference(
            case_id,
            "resolution-ambiguous",
            old.ambiguous.to_string(),
            new.ambiguous.to_string(),
        ));
    }
    differences
}

/// Lowercase hex, the canonical rendering both ratchets and messages use.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One accepted difference, with the reason it is accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedDifference {
    /// The case.
    pub case: String,
    /// The field.
    pub field: String,
    /// The first pipeline's value.
    pub left: String,
    /// The second pipeline's value.
    pub right: String,
    /// Why the difference is accepted.
    pub reason: String,
    /// The bead that closes it.
    pub bead: String,
}

impl ExpectedDifference {
    fn matches(&self, difference: &Difference) -> bool {
        self.case == difference.case
            && self.field == difference.field
            && self.left == difference.left
            && self.right == difference.right
    }
}

/// Reads `tests/differential/expected-differences.toml`.
pub fn load_expected_differences() -> Result<Vec<ExpectedDifference>, String> {
    let path = differential_dir().join("expected-differences.toml");
    parse_expected_differences(&read_file(&path)?)
}

/// Parses the ratchet file's text.
///
/// The checked-in file is empty at H0, so the entry schema is verified by the
/// unit tests below: when stage H1 adds the first accepted difference, a typo
/// in a key is a load error and not a silently unmatched entry.
pub fn parse_expected_differences(source: &str) -> Result<Vec<ExpectedDifference>, String> {
    let document = toml_lite::parse(source)?;
    let mut entries = Vec::new();
    for table in document.tables_named("difference") {
        let entry = ExpectedDifference {
            case: table.string("case")?.to_owned(),
            field: table.string("field")?.to_owned(),
            left: table.string("first")?.to_owned(),
            right: table.string("second")?.to_owned(),
            reason: table.string("reason")?.to_owned(),
            bead: table.string("bead")?.to_owned(),
        };
        if entry.reason.trim().is_empty() {
            return Err(format!(
                "{}: an accepted difference needs a reason",
                entry.case
            ));
        }
        if entry.bead.trim().is_empty() {
            return Err(format!(
                "{}: an accepted difference needs a bead",
                entry.case
            ));
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Applies the ratchet: every observed difference needs an entry, and every
/// entry needs an observed difference.
///
/// Returns the failure messages; an empty vector is a clean run.
#[must_use]
pub fn ratchet(expected: &[ExpectedDifference], observed: &[Difference]) -> Vec<String> {
    let mut matched = vec![false; expected.len()];
    let mut failures = Vec::new();
    for difference in observed {
        match expected.iter().position(|entry| entry.matches(difference)) {
            Some(index) => matched[index] = true,
            None => failures.push(format!(
                "new difference with no entry in expected-differences.toml\n{}",
                difference.describe()
            )),
        }
    }
    for (index, entry) in expected.iter().enumerate() {
        if !matched[index] {
            failures.push(format!(
                "stale expected difference: {} / {} ({} -> {}) no longer differs; delete the \
                 entry in the commit that removed the difference (bead {})",
                entry.case, entry.field, entry.left, entry.right, entry.bead
            ));
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::{Difference, ExpectedDifference, compare, ratchet, render_outcome};
    use crate::{InputOutcome, Observation};

    fn differing_observations() -> (Observation, Observation) {
        let first = Observation {
            diagnostics: vec!["ORNA-S020-ANNOTATION".to_owned()],
            outcome: Some(Err("ORNA-APP-SEMANTIC".to_owned())),
            steps: Some(7),
            ..Observation::default()
        };
        let second = Observation {
            outcome: Some(Ok(vec![1, 2])),
            ..Observation::default()
        };
        (first, second)
    }

    #[test]
    fn reports_one_difference_per_field() {
        let (first, second) = differing_observations();
        let differences = compare("case", &first, &second);
        let fields = differences
            .iter()
            .map(|difference| difference.field)
            .collect::<Vec<_>>();
        assert_eq!(fields, ["diagnostics", "outcome", "steps"]);
        assert_eq!(render_outcome(&first.outcome), "error:ORNA-APP-SEMANTIC");
        assert_eq!(render_outcome(&second.outcome), "value:0102");
    }

    #[test]
    fn identical_observations_do_not_differ() {
        let (first, second) = differing_observations();
        assert!(compare("case", &first, &first).is_empty());
        assert_ne!(first, second);
    }

    #[test]
    fn ratchet_accepts_a_matching_entry() {
        let (first, second) = differing_observations();
        let differences = compare("case", &first, &second);
        let expected = differences
            .iter()
            .map(|difference| ExpectedDifference {
                case: difference.case.clone(),
                field: difference.field.to_owned(),
                left: difference.left.clone(),
                right: difference.right.clone(),
                reason: "test".to_owned(),
                bead: "ornadb-csenl.15".to_owned(),
            })
            .collect::<Vec<_>>();
        assert_eq!(ratchet(&expected, &differences), Vec::<String>::new());
    }

    #[test]
    fn ratchet_fails_on_a_new_difference_and_on_a_stale_entry() {
        let observed = vec![Difference {
            case: "case".to_owned(),
            field: "outcome",
            left: "value:00".to_owned(),
            right: "value:01".to_owned(),
        }];
        let stale = ExpectedDifference {
            case: "other".to_owned(),
            field: "outcome".to_owned(),
            left: "value:00".to_owned(),
            right: "value:01".to_owned(),
            reason: "test".to_owned(),
            bead: "ornadb-csenl.15".to_owned(),
        };
        let failures = ratchet(&[stale], &observed);
        assert_eq!(failures.len(), 2, "{failures:?}");
        assert!(failures[0].contains("new difference"));
        assert!(failures[1].contains("stale expected difference"));
    }

    #[test]
    fn a_populated_ratchet_file_parses() {
        let entries = super::parse_expected_differences(
            "[[difference]]\n\
             case = \"reference/valid/affine-max.orna\"\n\
             field = \"outcome\"\n\
             first = \"value:00\"\n\
             second = \"value:01\"\n\
             reason = \"HIR folds the group step into the child\"\n\
             bead = \"ornadb-csenl.16\"\n",
        )
        .expect("a complete entry parses");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].case, "reference/valid/affine-max.orna");
        assert_eq!(entries[0].field, "outcome");
        assert_eq!(entries[0].left, "value:00");
        assert_eq!(entries[0].right, "value:01");
        assert_eq!(entries[0].bead, "ornadb-csenl.16");
    }

    #[test]
    fn an_entry_without_a_reason_or_bead_is_a_load_error() {
        for missing in ["reason", "bead"] {
            let mut entry = String::from(
                "[[difference]]\ncase = \"c\"\nfield = \"steps\"\nfirst = \"1\"\nsecond = \"2\"\nreason = \"r\"\nbead = \"b\"\n",
            );
            if missing == "reason" {
                entry = entry.replace("reason = \"r\"\n", "reason = \" \"\n");
            } else {
                entry = entry.replace("bead = \"b\"\n", "bead = \"\"\n");
            }
            let error = super::parse_expected_differences(&entry)
                .expect_err("an incomplete entry must not parse");
            assert!(error.contains(missing), "{missing}: {error}");
        }
    }

    #[test]
    fn calls_and_writes_are_compared_in_order() {
        let call = |start: usize, route: crate::Route| crate::CallRecord {
            logical_path: "main.orna".to_owned(),
            start,
            end: start + 3,
            route,
        };
        let write = |key: u8| crate::Write {
            table: "Note".to_owned(),
            key: vec![key],
            value: Some(vec![key]),
        };
        let first = Observation {
            calls: vec![
                call(0, crate::Route::Native),
                call(9, crate::Route::SourceFunction),
            ],
            writes: vec![write(1), write(2)],
            ..Observation::default()
        };
        assert!(compare("calls", &first, &first).is_empty());

        let reordered_calls = Observation {
            calls: vec![call(0, crate::Route::Native), call(9, crate::Route::Native)],
            ..first.clone()
        };
        assert_eq!(
            compare("calls", &first, &reordered_calls)
                .iter()
                .map(|difference| difference.field)
                .collect::<Vec<_>>(),
            ["calls"]
        );

        let reordered_writes = Observation {
            writes: vec![write(2), write(1)],
            ..first.clone()
        };
        assert_eq!(
            compare("writes", &first, &reordered_writes)
                .iter()
                .map(|difference| difference.field)
                .collect::<Vec<_>>(),
            ["writes"]
        );
    }

    #[test]
    fn repl_inputs_are_compared_in_order() {
        let first = Observation {
            inputs: vec![
                InputOutcome {
                    input: 0,
                    result: Some(Ok(vec![1])),
                },
                InputOutcome {
                    input: 1,
                    result: None,
                },
            ],
            ..Observation::default()
        };
        let second = Observation {
            inputs: vec![
                InputOutcome {
                    input: 0,
                    result: Some(Ok(vec![1])),
                },
                InputOutcome {
                    input: 1,
                    result: Some(Err("ORNA-EVAL-LIMIT".to_owned())),
                },
            ],
            ..Observation::default()
        };
        let differences = compare("repl", &first, &second);
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].field, "inputs");
    }
}
