//! Checks that the `formerly` clauses of `sys.orna` agree with the `removed_names` table derived from them.

use std::collections::BTreeSet;

use orna_syntax::{Declaration, parse_interface};
use orna_sys_interface::{SYS_ORNA, native_label, native_raw_document};

/// Every `formerly` clause names a retired name whose `removed_names` entry carries the same replacement label and
/// diagnostic. Every `removed_names` entry with a replacement has a declaration carrying the matching clause.
#[test]
fn formerly_clauses_agree_with_removed_names() {
    let removed_names = native_raw_document()
        .expect("sys.orna decodes")
        .removed_names;
    let mut differences = Vec::new();
    let mut declared_formerly: BTreeSet<String> = BTreeSet::new();
    for item in &parse_interface(SYS_ORNA).value.items {
        let Some(clause) = &item.clauses.formerly else {
            continue;
        };
        let name = match &item.declaration {
            Declaration::Native { signature, .. } => signature.name.as_str(),
            Declaration::NativeStatic { name, .. } => name.as_str(),
            _ => {
                differences.push(format!(
                    "`formerly {}` is not on a native callable or native static",
                    clause.name.text
                ));
                continue;
            }
        };
        let label = native_label(item, name);
        declared_formerly.insert(clause.name.text.clone());
        let entry = removed_names.get(&clause.name.text);
        let replacement = entry.and_then(|entry| entry.replacement.as_deref());
        let diagnostic = entry.map(|entry| entry.diagnostic.as_str());
        let declared = clause
            .diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.text.as_str());
        if replacement != Some(label.as_str()) || diagnostic != declared {
            differences.push(format!(
                "{label}: `formerly {}` declares diagnostic {declared:?}, but removed_names has replacement \
                 {replacement:?} and diagnostic {diagnostic:?}",
                clause.name.text
            ));
        }
    }
    for (old, entry) in &removed_names {
        let Some(replacement) = entry.replacement.as_deref() else {
            continue;
        };
        if !declared_formerly.contains(old) {
            differences.push(format!(
                "`{old}` maps to `{replacement}` in removed_names with no declaring `formerly` clause"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} formerly clause(s) disagree with removed_names:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
