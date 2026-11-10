//! The H0 environment ratchet (`docs/design/hir.md` section 6.3, stage H0).
//!
//! The reference corpus names fixture tables, connectors, hosts and currencies
//! that the HIR does not declare. Each use lowers to a "not in scope" or "not a
//! declared type" diagnostic. This test counts those uses by category and
//! ratchets the counts against the `[[environment]]` tables in
//! `tests/differential/expected-differences.toml`. It prints the per-name
//! breakdown, so the classes stay visible.

use std::collections::BTreeMap;

use orna_differential::case::Corpus;
use orna_differential::environment::{self, CATEGORIES};
use orna_differential::observe_new;

#[test]
fn environment_names_only_shrink_as_declarations_arrive() {
    let corpus = Corpus::load().expect("the corpus loads and reconciles");
    let recorded =
        environment::load_environment_categories().expect("the environment ratchet parses");
    let mut observed: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    for case in &corpus.cases {
        let Ok(mut program) = observe_new::lower_case(case) else {
            continue;
        };
        let observation = observe_new::type_observation(&mut program);
        for diagnostic in &observation.diagnostics {
            let Some(category) = environment::classify(diagnostic) else {
                continue;
            };
            *observed.entry(category).or_insert(0) += 1;
            if let Some(start) = diagnostic.find('`')
                && let Some(length) = diagnostic[start + 1..].find('`')
            {
                let name = &diagnostic[start + 1..start + 1 + length];
                *names.entry(format!("{category}: {name}")).or_insert(0) += 1;
            }
        }
    }
    for category in CATEGORIES {
        println!(
            "environment {category}: {}",
            observed.get(category).copied().unwrap_or(0)
        );
    }
    for (name, count) in &names {
        println!("  {count:>4} {name}");
    }
    let failures = environment::ratchet(&recorded, &observed);
    assert!(
        failures.is_empty(),
        "the environment ratchet failed:\n{}",
        failures.join("\n")
    );
}
