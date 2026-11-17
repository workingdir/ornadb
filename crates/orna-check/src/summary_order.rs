//! Evaluation order for function-summary stabilisation.

use std::collections::BTreeMap;

use orna_syntax_ast::{Declaration, SyntaxTree};

use crate::Namespace;

// Module visits made while stabilising function summaries. A sweep visits
// every module once, so the count is the sweep count times the module count.
#[cfg(test)]
thread_local! {
    static SUMMARY_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn note_summary_visit() {
    SUMMARY_VISITS.with(|visits| visits.set(visits.get() + 1));
}

#[cfg(not(test))]
#[inline]
pub(crate) fn note_summary_visit() {}

#[cfg(test)]
fn summary_visits() -> usize {
    SUMMARY_VISITS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_summary_visits() {
    SUMMARY_VISITS.with(|visits| visits.set(0));
}

/// Orders summary evaluation so each module is visited after the modules it
/// imports. The sweep in `stabilize_function_summaries` still runs until no
/// summary changes, so the order only sets how many sweeps convergence takes:
/// an import chain settles in one sweep rather than one module per sweep.
/// A cycle keeps the order its first visit reached, and the same sweep loop
/// converges it.
pub(crate) fn summary_evaluation_order(parsed: &[(Namespace, SyntaxTree)]) -> Vec<usize> {
    let index_of = parsed
        .iter()
        .enumerate()
        .map(|(index, (namespace, _))| (namespace, index))
        .collect::<BTreeMap<_, _>>();
    let imports = parsed
        .iter()
        .map(|(_, tree)| {
            let mut targets = tree
                .items
                .iter()
                .filter_map(|item| match &item.declaration {
                    Declaration::Use { path, .. } => {
                        let target =
                            Namespace(path.iter().map(|segment| segment.name.clone()).collect());
                        index_of.get(&target).copied()
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            targets.sort_unstable();
            targets.dedup();
            targets
        })
        .collect::<Vec<_>>();
    let mut visited = vec![false; parsed.len()];
    let mut order = Vec::with_capacity(parsed.len());
    for root in 0..parsed.len() {
        if visited[root] {
            continue;
        }
        visited[root] = true;
        let mut stack = vec![(root, 0_usize)];
        while let Some(&(node, next)) = stack.last() {
            if let Some(&target) = imports[node].get(next) {
                stack.last_mut().expect("the frame was just read").1 += 1;
                if !visited[target] {
                    visited[target] = true;
                    stack.push((target, 0));
                }
            } else {
                stack.pop();
                order.push(node);
            }
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModuleInput, analyze};

    /// `links` modules in an import chain, listed importer-first: `main`
    /// imports `m0`, and each `m{i}` imports `m{i + 1}`. An importer that is
    /// evaluated before its import waits one sweep per link.
    fn import_chain(links: usize) -> Vec<ModuleInput> {
        let mut inputs = vec![ModuleInput::new(
            "main.orna",
            "use m0;\npub fn main(x: Int): Int = m0.f_0(x);\n",
        )];
        for index in 0..links {
            let source = if index + 1 < links {
                let next = index + 1;
                format!("use m{next};\npub fn f_{index}(x: Int): Int = m{next}.f_{next}(x) + 1;\n")
            } else {
                format!("pub fn f_{index}(x: Int): Int = x;\n")
            };
            inputs.push(ModuleInput::new(format!("m{index}.orna"), source));
        }
        inputs
    }

    #[test]
    fn import_chain_settles_in_a_constant_number_of_sweeps() {
        let inputs = import_chain(40);
        reset_summary_visits();
        let analysis = analyze(&inputs);
        assert!(
            analysis.diagnostics.is_empty(),
            "{:?}",
            analysis.diagnostics
        );
        // One sweep that visits each module after the modules it imports, and
        // one confirming sweep that changes nothing. Importer-first order needs
        // a sweep per link, so its visits grow with the square of the module
        // count.
        assert!(
            summary_visits() <= 3 * inputs.len(),
            "{} visits for {} modules",
            summary_visits(),
            inputs.len()
        );
    }
}
