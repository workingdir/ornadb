//! The H0 acceptance proof (`docs/design/hir.md` sections 6.2 and 6.3).
//!
//! * [`corpus_accounts_for_every_program_the_repository_owns`] proves the loader
//!   account: every `.orna` program is a case or is inside an exclusion scope
//!   with a recorded count.
//! * [`the_old_pipeline_agrees_with_itself_on_every_case`] proves the harness:
//!   old against old over the whole corpus, with zero differences, zero flaky
//!   cases, and every difference either absent or recorded in
//!   `tests/differential/expected-differences.toml`.

use std::collections::BTreeMap;
use std::time::Instant;

use orna_differential::case::{Corpus, Family};
use orna_differential::{Observation, compare, observe_old};

/// Cases per family on the accepted corpus. Growing a family is a deliberate
/// edit here: the constants are the corpus ratchet of section 6.2's risk note.
const REFERENCE_CASES: usize = 169;
const EXPRESSION_CASES: usize = 42;
const REPL_CASES: usize = 14;
const STANDARD_CASES: usize = 1;

/// Programs outside the corpus, summed over the exclusion scopes.
const EXCLUDED_PROGRAMS: usize = 5399;

/// Reference-corpus programs the published manifest calls valid that the old
/// pipeline rejects.
///
/// This is the harness's fidelity measure: a valid fixture is rejected because
/// the case carries no environment for it, not because the fixture is wrong.
/// Measured at H0: 15 `ORNA-EVAL-NAME` (a relation source whose rows the case
/// does not carry), 11 `sys.version.incompatible` (the binary attaches a
/// `SysHostContext` per activation, and a deterministic one has not been
/// defined yet), and 11 others (four `ORNA-EVAL-UNSUPPORTED`, three
/// `ORNA-S021-TYPE`, one each of `E2002`, `ORNA-EVAL-TABLE-SNAPSHOT`,
/// `ORNA-EVAL-TYPE` and `ORNA-EVAL-VALUE`). The number is a ratchet: follow-up
/// PRs carry the environments and lower it deliberately.
const REFERENCE_VALID_REJECTED: usize = 37;

/// The old pipeline's outcome tally, per family and per published expectation.
type Tally = BTreeMap<(String, &'static str, String), usize>;

#[test]
fn corpus_accounts_for_every_program_the_repository_owns() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let excluded = excluded_programs(&corpus);
    println!(
        "corpus: {} cases ({} reference, {} expression, {} repl, {} standard); \
         {} excluded programs in {} scopes",
        corpus.cases.len(),
        counts_in(&corpus, Family::Reference),
        counts_in(&corpus, Family::Expression),
        counts_in(&corpus, Family::Repl),
        counts_in(&corpus, Family::Standard),
        excluded,
        corpus.excluded.len(),
    );
    assert_eq!(
        counts_in(&corpus, Family::Reference),
        REFERENCE_CASES,
        "the reference family changed; update the constant in the same PR"
    );
    assert_eq!(
        counts_in(&corpus, Family::Expression),
        EXPRESSION_CASES,
        "the expression family changed; update the constant in the same PR"
    );
    assert_eq!(
        counts_in(&corpus, Family::Repl),
        REPL_CASES,
        "the REPL family changed; update the constant in the same PR"
    );
    assert_eq!(
        counts_in(&corpus, Family::Standard),
        STANDARD_CASES,
        "the standard family changed; update the constant in the same PR"
    );
    assert_eq!(
        excluded, EXCLUDED_PROGRAMS,
        "the excluded-program total changed; update the constant and the reason in the same PR"
    );
}

fn counts_in(corpus: &Corpus, family: Family) -> usize {
    corpus
        .cases
        .iter()
        .filter(|case| case.family == family)
        .count()
}

fn excluded_programs(corpus: &Corpus) -> usize {
    corpus.excluded.iter().map(|scope| scope.files).sum()
}

#[test]
fn the_old_pipeline_agrees_with_itself_on_every_case() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let expected = compare::load_expected_differences().expect("the ratchet file parses");
    let started = Instant::now();
    let mut differences = Vec::new();
    let mut errors = Vec::new();
    let mut measured_steps = 0usize;
    let mut tally = Tally::new();
    for case in &corpus.cases {
        let first = match observe_old::observe(case) {
            Ok(observation) => observation,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let second = match observe_old::observe(case) {
            Ok(observation) => observation,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        measured_steps += usize::from(first.steps.is_some());
        tally_case(&mut tally, case, &first);
        differences.extend(compare::compare(&case.id, &first, &second));
    }
    let elapsed = started.elapsed();
    println!(
        "self-differential: {} cases observed twice in {:.1}s; {} with a step count; \
         {} differences (all flakiness by construction), {} errors",
        corpus.cases.len(),
        elapsed.as_secs_f64(),
        measured_steps,
        differences.len(),
        errors.len(),
    );
    for ((family, published, outcome), count) in &tally {
        println!("  {family:>10} {published:>8} {outcome:>32}: {count:>3}");
    }
    for difference in &differences {
        println!("{}", difference.describe());
    }
    assert!(
        errors.is_empty(),
        "the observer failed on cases it must drive: {errors:?}"
    );
    let rejected = tally
        .iter()
        .filter(|((family, published, outcome), _)| {
            family == "reference" && *published == "valid" && outcome.starts_with("error:")
        })
        .map(|(_, count)| count)
        .sum::<usize>();
    assert_eq!(
        rejected, REFERENCE_VALID_REJECTED,
        "the number of valid reference fixtures the harness cannot drive to acceptance changed; \
         update the constant (and the exclusion reason) in the same PR"
    );
    let failures = compare::ratchet(&expected, &differences);
    assert!(
        failures.is_empty(),
        "old against old must report no differences:\n{}",
        failures.join("\n")
    );
}

/// Tallies one observed case by family, published expectation and outcome.
fn tally_case(tally: &mut Tally, case: &orna_differential::case::Case, observation: &Observation) {
    let published = match case
        .expectation
        .as_ref()
        .map(|expectation| expectation.accept)
    {
        Some(true) => "valid",
        Some(false) => "invalid",
        None => "no expectation",
    };
    let outcome = match &observation.outcome {
        None => "not evaluated".to_owned(),
        Some(Ok(_)) => "value".to_owned(),
        Some(Err(code)) => format!("error:{code}"),
    };
    *tally
        .entry((case.family.id().to_owned(), published, outcome))
        .or_insert(0) += 1;
}
