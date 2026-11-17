//! Prefix scans over module namespaces.
//!
//! `Namespace` orders lexicographically by its segments, so the namespaces
//! that start with a prefix form one contiguous run beginning at the prefix
//! itself. A scan from the prefix that stops at the first namespace past the
//! run visits the matches in the order a full filtered scan would, and never
//! touches the rest of the map.

use std::collections::BTreeMap;
use std::ops::Bound;

use crate::Namespace;

/// Yields the entries whose namespace starts with `prefix`, in ascending
/// namespace order.
pub(super) fn entries_under_prefix<'a, V>(
    modules: &'a BTreeMap<Namespace, V>,
    prefix: Namespace,
) -> impl Iterator<Item = (&'a Namespace, &'a V)> + 'a {
    modules
        .range((Bound::Included(&prefix), Bound::Unbounded))
        .take_while(move |(namespace, _)| namespace.0.starts_with(&prefix.0))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::entries_under_prefix;
    use crate::Namespace;

    fn namespace(segments: &[&str]) -> Namespace {
        Namespace(
            segments
                .iter()
                .map(|segment| (*segment).to_owned())
                .collect(),
        )
    }

    fn filtered_scan(
        modules: &BTreeMap<Namespace, u8>,
        prefix: &Namespace,
    ) -> Vec<(Namespace, u8)> {
        modules
            .iter()
            .filter(|(candidate, _)| {
                candidate.0.len() >= prefix.0.len() && candidate.0[..prefix.0.len()] == prefix.0[..]
            })
            .map(|(candidate, value)| (candidate.clone(), *value))
            .collect()
    }

    #[test]
    fn prefix_scan_matches_a_filtered_scan_in_namespace_order() {
        let modules = BTreeMap::from([
            (namespace(&[]), 0),
            (namespace(&["a"]), 1),
            (namespace(&["a", "b"]), 2),
            (namespace(&["a", "b", "c"]), 3),
            (namespace(&["a0"]), 4),
            (namespace(&["aa"]), 5),
            (namespace(&["b"]), 6),
            (namespace(&["b", "a"]), 7),
        ]);
        let prefixes = [
            namespace(&[]),
            namespace(&["a"]),
            namespace(&["a", "b"]),
            namespace(&["a", "b", "c", "d"]),
            namespace(&["a", "c"]),
            namespace(&["b"]),
            namespace(&["c"]),
        ];
        for prefix in &prefixes {
            let scanned: Vec<(Namespace, u8)> = entries_under_prefix(&modules, prefix.clone())
                .map(|(candidate, value)| (candidate.clone(), *value))
                .collect();
            assert_eq!(
                scanned,
                filtered_scan(&modules, prefix),
                "prefix {prefix:?}"
            );
        }
    }

    #[test]
    fn prefix_matches_whole_segments_not_string_prefixes() {
        let modules = BTreeMap::from([
            (namespace(&["a"]), 1),
            (namespace(&["a", "b"]), 2),
            (namespace(&["a0"]), 3),
            (namespace(&["aa"]), 4),
        ]);
        let under_a: Vec<Namespace> = entries_under_prefix(&modules, namespace(&["a"]))
            .map(|(candidate, _)| candidate.clone())
            .collect();
        assert_eq!(under_a, vec![namespace(&["a"]), namespace(&["a", "b"])]);
        assert_eq!(
            entries_under_prefix(&modules, namespace(&[])).count(),
            modules.len()
        );
    }
}
