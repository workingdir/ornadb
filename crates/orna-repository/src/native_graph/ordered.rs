//! Walkers and validators for ORP-1 ordered indexes.
//!
//! An ordered index is a tree of leaves and branches whose domain is either
//! the relation map or one row map (store-format-3 section 2). Every walker
//! here reads nodes through a caller-supplied closure and checks each node
//! against what its parent claims for it, so a walk never trusts the tree it
//! is walking. The domain encoding and the per-level key-order rules live here
//! with the walkers, because a walker that does not check them cannot tell a
//! valid index from a plausible one.

use super::*;

/// Which map an ordered node belongs to, decoded from its domain bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum OrderedDomain {
    Relations([u8; 16]),
    Rows([u8; 16], [u8; 32]),
}

pub(super) fn parse_ordered_domain(domain: &[u8]) -> Result<OrderedDomain, GraphError> {
    if domain.is_empty() || domain.len() > 256 {
        return Err(GraphError::InvalidDomain);
    }
    let CborValue::Array(fields) = decode_canonical_cbor(domain)? else {
        return Err(GraphError::InvalidDomain);
    };
    let Some(CborValue::Text(name)) = fields.first() else {
        return Err(GraphError::InvalidDomain);
    };
    match name.as_str() {
        "relations" if fields.len() == 2 => {
            let Some(CborValue::Bytes(database_id)) = fields.get(1) else {
                return Err(GraphError::InvalidDomain);
            };
            Ok(OrderedDomain::Relations(
                database_id
                    .as_slice()
                    .try_into()
                    .map_err(|_| GraphError::InvalidDomain)?,
            ))
        }
        "rows" if fields.len() == 3 => {
            let (Some(CborValue::Bytes(relation_id)), Some(CborValue::Bytes(schema_digest))) =
                (fields.get(1), fields.get(2))
            else {
                return Err(GraphError::InvalidDomain);
            };
            Ok(OrderedDomain::Rows(
                relation_id
                    .as_slice()
                    .try_into()
                    .map_err(|_| GraphError::InvalidDomain)?,
                schema_digest
                    .as_slice()
                    .try_into()
                    .map_err(|_| GraphError::InvalidDomain)?,
            ))
        }
        _ => Err(GraphError::InvalidDomain),
    }
}

pub(super) fn check_domain(domain: &[u8]) -> Result<(), GraphError> {
    parse_ordered_domain(domain).map(|_| ())
}

pub(super) fn check_leaf_key_order(
    domain: &[u8],
    entries: &[OrderedLeafEntry],
) -> Result<(), GraphError> {
    let domain = parse_ordered_domain(domain)?;
    let mut previous: Option<crate::row_store::TypedKey> = None;
    for entry in entries {
        let key = crate::row_store::TypedKey::decode_canonical(&entry.key)
            .map_err(|_| GraphError::NonCanonicalData)?;
        if matches!(domain, OrderedDomain::Relations(_))
            && !matches!(&key, crate::row_store::TypedKey::Bytes(bytes) if bytes.len() == 16)
        {
            return Err(GraphError::InvalidDomain);
        }
        if previous.as_ref().is_some_and(|previous| previous >= &key) {
            return Err(GraphError::NonCanonicalData);
        }
        previous = Some(key);
    }
    Ok(())
}

pub(super) fn check_branch_fence_order(
    domain: &[u8],
    entries: &[OrderedBranchEntry],
) -> Result<(), GraphError> {
    let domain = parse_ordered_domain(domain)?;
    let mut previous: Option<crate::row_store::TypedKey> = None;
    for entry in entries {
        let fence = crate::row_store::TypedKey::decode_canonical(&entry.inclusive_max_key)
            .map_err(|_| GraphError::NonCanonicalData)?;
        if matches!(domain, OrderedDomain::Relations(_))
            && !matches!(&fence, crate::row_store::TypedKey::Bytes(bytes) if bytes.len() == 16)
        {
            return Err(GraphError::InvalidDomain);
        }
        if previous.as_ref().is_some_and(|previous| previous >= &fence) {
            return Err(GraphError::OverlappingChildRange);
        }
        previous = Some(fence);
    }
    Ok(())
}

pub(super) fn find_ordered_entry_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    node: NodeData,
    expected_domain: &[u8],
    key: &crate::row_store::TypedKey,
) -> Result<Option<OrderedLeafEntry>, GraphError> {
    // ORP-1 child ranges are nonoverlapping and exhaustive in fence order
    // (store-format-3 section 2), so the first child whose inclusive maximum
    // fence is at or above the key is the only child that can hold it. A
    // point lookup therefore reads exactly one node per level, and a miss
    // returns after that single path instead of probing later siblings.
    //
    // Every node on the path is checked against what its parent's entry
    // claims for it: domain, height, exact row count, an inclusive maximum
    // equal to the fence, and a minimum above the previous fence. Those checks
    // use only the node itself, so they add no reads. A malformed range on
    // the path is rejected; overlap between children off the path is left to
    // full admission and to range reads, which read those children anyway.
    let mut node = node;
    // The root's height is whatever it records, and it has no lower bound.
    let mut expected_height: Option<u8> = None;
    let mut lower_exclusive: Option<crate::row_store::TypedKey> = None;
    // The parent's claim for `node`: its exact row count and inclusive
    // maximum key. The root has no parent entry and so no claim.
    let mut claim: Option<(u64, crate::row_store::TypedKey)> = None;
    loop {
        match node {
            NodeData::OrderedLeaf { domain, entries } => {
                if domain != expected_domain || expected_height.is_some_and(|height| height != 0) {
                    return Err(GraphError::InvalidDomain);
                }
                let mut decoded: Vec<(crate::row_store::TypedKey, &OrderedLeafEntry)> =
                    Vec::with_capacity(entries.len());
                for entry in &entries {
                    let entry_key = crate::row_store::TypedKey::decode_canonical(&entry.key)
                        .map_err(|_| GraphError::NonCanonicalData)?;
                    if decoded.last().is_some_and(|(last, _)| last >= &entry_key) {
                        return Err(GraphError::NonCanonicalData);
                    }
                    decoded.push((entry_key, entry));
                }
                let breaks_claim = claim.as_ref().is_some_and(|(row_count, fence)| {
                    entries.len() as u64 != *row_count
                        || decoded.last().is_none_or(|(maximum, _)| maximum != fence)
                });
                let breaks_lower = lower_exclusive.as_ref().is_some_and(|lower| {
                    decoded.first().is_some_and(|(minimum, _)| minimum <= lower)
                });
                if breaks_claim || breaks_lower {
                    return Err(GraphError::InvalidCount(entries.len()));
                }
                return Ok(decoded
                    .binary_search_by(|(candidate, _)| candidate.cmp(key))
                    .ok()
                    .map(|index| decoded[index].1.clone()));
            }
            NodeData::OrderedBranch {
                domain,
                height,
                entries,
            } => {
                if domain != expected_domain
                    || height == 0
                    || expected_height.is_some_and(|expected| expected != height)
                    || entries.is_empty()
                {
                    return Err(GraphError::InvalidDomain);
                }
                let total_count = entries.iter().try_fold(0u64, |sum, entry| {
                    sum.checked_add(entry.row_count)
                        .filter(|count| *count <= MAX_SIGNED_LENGTH)
                        .ok_or(GraphError::InvalidCount(usize::MAX))
                })?;
                let mut fences: Vec<crate::row_store::TypedKey> = Vec::with_capacity(entries.len());
                for entry in &entries {
                    let fence =
                        crate::row_store::TypedKey::decode_canonical(&entry.inclusive_max_key)
                            .map_err(|_| GraphError::NonCanonicalData)?;
                    if fences.last().is_some_and(|previous| previous >= &fence) {
                        return Err(GraphError::NonCanonicalData);
                    }
                    fences.push(fence);
                }
                // This branch's maximum is its last child's fence and its
                // minimum is no greater than its first child's fence, so both
                // halves of the parent's claim are checked without reading
                // any child other than the one the key selects.
                let breaks_claim = claim.as_ref().is_some_and(|(row_count, maximum)| {
                    total_count != *row_count || fences.last() != Some(maximum)
                });
                let breaks_lower = lower_exclusive
                    .as_ref()
                    .is_some_and(|lower| fences.first().is_some_and(|first| first <= lower));
                if breaks_claim || breaks_lower {
                    return Err(GraphError::InvalidCount(
                        usize::try_from(total_count).unwrap_or(usize::MAX),
                    ));
                }
                let index = fences.partition_point(|fence| fence < key);
                let Some(entry) = entries.get(index) else {
                    // The key sorts above this node's maximum: no row holds it.
                    return Ok(None);
                };
                // The first child inherits this node's own lower bound; any
                // later child must sort above its predecessor's fence.
                if let Some(index) = index.checked_sub(1) {
                    lower_exclusive = Some(fences[index].clone());
                }
                let child = read_node(&entry.child)?;
                expected_height = Some(height - 1);
                claim = Some((entry.row_count, fences.swap_remove(index)));
                node = child;
            }
            other => {
                return Err(GraphError::WrongNodeKind {
                    expected: NodeKind::OrderedLeaf,
                    actual: other.kind(),
                });
            }
        }
    }
}

/// The parameters describe one record; each caller passes them straight through.
#[allow(clippy::too_many_arguments)]
pub(super) fn replace_ordered_entry_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    write_node: &mut impl FnMut(&NodeData) -> Result<NativeOid, GraphError>,
    node: NodeData,
    expected_domain: &[u8],
    expected_height: Option<u8>,
    lower_exclusive: Option<&crate::row_store::TypedKey>,
    key: &crate::row_store::TypedKey,
    value: Vec<u8>,
) -> Result<NativeOid, GraphError> {
    match node {
        NodeData::OrderedLeaf {
            domain,
            mut entries,
        } => {
            if domain != expected_domain || expected_height.is_some_and(|height| height != 0) {
                return Err(GraphError::InvalidDomain);
            }
            check_leaf_key_order(&domain, &entries)?;
            if lower_exclusive.is_some_and(|lower| {
                entries
                    .first()
                    .and_then(|entry| crate::row_store::TypedKey::decode_canonical(&entry.key).ok())
                    .is_some_and(|minimum| minimum <= *lower)
            }) {
                return Err(GraphError::NonCanonicalData);
            }
            let index = entries
                .binary_search_by(|entry| {
                    crate::row_store::TypedKey::decode_canonical(&entry.key)
                        .map(|candidate| candidate.cmp(key))
                        .unwrap_or(std::cmp::Ordering::Less)
                })
                .map_err(|_| GraphError::ContextMismatch)?;
            entries[index].value = value;
            write_node(&NodeData::OrderedLeaf { domain, entries })
        }
        NodeData::OrderedBranch {
            domain,
            height,
            mut entries,
        } => {
            if domain != expected_domain
                || height == 0
                || expected_height.is_some_and(|expected| expected != height)
                || entries.is_empty()
            {
                return Err(GraphError::InvalidDomain);
            }
            check_branch_fence_order(&domain, &entries)?;
            let mut previous_fence: Option<crate::row_store::TypedKey> = None;
            let mut selected = None;
            for (index, entry) in entries.iter().enumerate() {
                let fence = crate::row_store::TypedKey::decode_canonical(&entry.inclusive_max_key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
                if key <= &fence {
                    let child = read_node(&entry.child)?;
                    let bounds = ordered_node_bounds_with(
                        read_node,
                        child.clone(),
                        expected_domain,
                        Some(height - 1),
                    )?;
                    if bounds.0 != entry.row_count
                        || bounds.2 != fence
                        || previous_fence
                            .as_ref()
                            .is_some_and(|previous| bounds.1 <= *previous)
                    {
                        return Err(GraphError::InvalidCount(
                            usize::try_from(bounds.0).unwrap_or(usize::MAX),
                        ));
                    }
                    selected = Some((index, child, previous_fence.as_ref().cloned()));
                    break;
                }
                previous_fence = Some(fence);
            }
            let Some((index, child, lower_bound)) = selected else {
                return Err(GraphError::ContextMismatch);
            };
            entries[index].child = replace_ordered_entry_with(
                read_node,
                write_node,
                child,
                expected_domain,
                Some(height - 1),
                lower_bound.as_ref(),
                key,
                value,
            )?;
            write_node(&NodeData::OrderedBranch {
                domain,
                height,
                entries,
            })
        }
        other => Err(GraphError::WrongNodeKind {
            expected: NodeKind::OrderedLeaf,
            actual: other.kind(),
        }),
    }
}

/// The parameters describe one record; each caller passes them straight through.
#[allow(clippy::too_many_arguments)]
/// Collects every node object of one ordered index (root, branches and leaves)
/// plus every stored-value payload object its entries reference, validating
/// each node against its parent's claims on the way down.
pub(super) fn collect_ordered_index_objects_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    node: &NodeData,
    node_oid: &NativeOid,
    expected_domain: &[u8],
    expected_height: Option<u8>,
    nodes: &mut BTreeSet<NativeOid>,
    payload: &mut BTreeSet<NativeOid>,
    // One walk has a single fixed domain; include height so a shared OID
    // reached at another level is validated again under that parent context.
    validated_contexts: &mut BTreeSet<(NativeOid, Option<u8>)>,
) -> Result<(), GraphError> {
    let context = (node_oid.clone(), expected_height);
    match node {
        NodeData::OrderedLeaf { domain, entries } => {
            if domain != expected_domain || expected_height.is_some_and(|height| height != 0) {
                return Err(GraphError::InvalidDomain);
            }
            check_leaf_key_order(domain, entries)?;
            if !validated_contexts.insert(context) {
                return Ok(());
            }
            for entry in entries {
                for dependency in leaf_value_dependencies(domain, &entry.value)? {
                    payload.insert(dependency.oid);
                }
            }
            Ok(())
        }
        NodeData::OrderedBranch {
            domain,
            height,
            entries,
        } => {
            if domain != expected_domain
                || *height == 0
                || expected_height.is_some_and(|expected| expected != *height)
                || entries.is_empty()
            {
                return Err(GraphError::InvalidDomain);
            }
            check_branch_fence_order(domain, entries)?;
            if !validated_contexts.insert(context) {
                return Ok(());
            }
            let mut previous_fence: Option<crate::row_store::TypedKey> = None;
            for entry in entries {
                let fence = crate::row_store::TypedKey::decode_canonical(&entry.inclusive_max_key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
                let child = read_node(&entry.child)?;
                let bounds = ordered_node_bounds_with(
                    read_node,
                    child.clone(),
                    expected_domain,
                    Some(*height - 1),
                )?;
                if bounds.0 != entry.row_count
                    || bounds.2 != fence
                    || previous_fence
                        .as_ref()
                        .is_some_and(|previous| bounds.1 <= *previous)
                {
                    return Err(GraphError::InvalidCount(
                        usize::try_from(bounds.0).unwrap_or(usize::MAX),
                    ));
                }
                let child_oid = entry.child.clone();
                nodes.insert(child_oid.clone());
                collect_ordered_index_objects_with(
                    read_node,
                    &child,
                    &child_oid,
                    expected_domain,
                    Some(*height - 1),
                    nodes,
                    payload,
                    validated_contexts,
                )?;
                previous_fence = Some(fence);
            }
            Ok(())
        }
        other => Err(GraphError::WrongNodeKind {
            expected: NodeKind::OrderedLeaf,
            actual: other.kind(),
        }),
    }
}

fn ordered_node_bounds_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    node: NodeData,
    expected_domain: &[u8],
    expected_height: Option<u8>,
) -> Result<(u64, crate::row_store::TypedKey, crate::row_store::TypedKey), GraphError> {
    match node {
        NodeData::OrderedLeaf { domain, entries } => {
            if domain != expected_domain || expected_height.is_some_and(|height| height != 0) {
                return Err(GraphError::InvalidDomain);
            }
            let first = entries.first().ok_or(GraphError::InvalidCount(0))?;
            let last = entries.last().ok_or(GraphError::InvalidCount(0))?;
            let minimum = crate::row_store::TypedKey::decode_canonical(&first.key)
                .map_err(|_| GraphError::NonCanonicalData)?;
            let maximum = crate::row_store::TypedKey::decode_canonical(&last.key)
                .map_err(|_| GraphError::NonCanonicalData)?;
            Ok((entries.len() as u64, minimum, maximum))
        }
        NodeData::OrderedBranch {
            domain,
            height,
            entries,
        } => {
            if domain != expected_domain
                || height == 0
                || expected_height.is_some_and(|expected| expected != height)
                || entries.is_empty()
            {
                return Err(GraphError::InvalidDomain);
            }
            let total_count = entries.iter().try_fold(0u64, |sum, entry| {
                sum.checked_add(entry.row_count)
                    .filter(|count| *count <= MAX_SIGNED_LENGTH)
                    .ok_or(GraphError::InvalidCount(usize::MAX))
            })?;
            let first = entries.first().ok_or(GraphError::InvalidCount(0))?;
            let last = entries.last().ok_or(GraphError::InvalidCount(0))?;
            let first_fence =
                crate::row_store::TypedKey::decode_canonical(&first.inclusive_max_key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
            let last_fence = crate::row_store::TypedKey::decode_canonical(&last.inclusive_max_key)
                .map_err(|_| GraphError::NonCanonicalData)?;
            let first_node = read_node(&first.child)?;
            let first_bounds =
                ordered_node_bounds_with(read_node, first_node, expected_domain, Some(height - 1))?;
            if first_bounds.0 != first.row_count || first_bounds.2 != first_fence {
                return Err(GraphError::InvalidCount(
                    usize::try_from(first_bounds.0).unwrap_or(usize::MAX),
                ));
            }
            let (minimum, maximum) = if std::ptr::eq(first, last) {
                (first_bounds.1, first_bounds.2)
            } else {
                let last_node = read_node(&last.child)?;
                let last_bounds = ordered_node_bounds_with(
                    read_node,
                    last_node,
                    expected_domain,
                    Some(height - 1),
                )?;
                if last_bounds.0 != last.row_count || last_bounds.2 != last_fence {
                    return Err(GraphError::InvalidCount(
                        usize::try_from(last_bounds.0).unwrap_or(usize::MAX),
                    ));
                }
                if last_bounds.1 <= first_bounds.2 {
                    return Err(GraphError::NonCanonicalData);
                }
                (first_bounds.1, last_bounds.2)
            };
            Ok((total_count, minimum, maximum))
        }
        other => Err(GraphError::WrongNodeKind {
            expected: NodeKind::OrderedLeaf,
            actual: other.kind(),
        }),
    }
}

pub(super) fn validate_ordered_root_node(
    node: &NodeData,
    expected_domain: &[u8],
    expected_count: u64,
) -> Result<(), GraphError> {
    match node {
        NodeData::OrderedLeaf { domain, entries } => {
            if domain != expected_domain || entries.len() as u64 != expected_count {
                return Err(GraphError::InvalidCount(entries.len()));
            }
        }
        NodeData::OrderedBranch {
            domain, entries, ..
        } => {
            if domain != expected_domain || entries.len() < 2 {
                return Err(GraphError::InvalidDomain);
            }
            check_branch_fence_order(domain, entries)?;
            let count = entries.iter().try_fold(0u64, |sum, entry| {
                sum.checked_add(entry.row_count)
                    .filter(|count| *count <= MAX_SIGNED_LENGTH)
                    .ok_or(GraphError::InvalidCount(usize::MAX))
            })?;
            if count != expected_count {
                return Err(GraphError::InvalidCount(
                    usize::try_from(count).unwrap_or(usize::MAX),
                ));
            }
        }
        actual => {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::OrderedLeaf,
                actual: actual.kind(),
            });
        }
    }
    Ok(())
}

/// The parameters describe one record; each caller passes them straight through.
#[allow(clippy::too_many_arguments)]
pub(super) fn collect_ordered_range_with(
    read_node: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    node: NodeData,
    expected_domain: &[u8],
    expected_height: Option<u8>,
    lower_exclusive: Option<&crate::row_store::TypedKey>,
    lower_inclusive: Option<&crate::row_store::TypedKey>,
    resume_after: Option<&crate::row_store::TypedKey>,
    upper_exclusive: Option<&crate::row_store::TypedKey>,
    limit: usize,
    output: &mut Vec<OrderedLeafEntry>,
) -> Result<(), GraphError> {
    if output.len() >= limit {
        return Ok(());
    }
    match node {
        NodeData::OrderedLeaf { domain, entries } => {
            if domain != expected_domain || expected_height.is_some_and(|height| height != 0) {
                return Err(GraphError::InvalidDomain);
            }
            let mut previous: Option<crate::row_store::TypedKey> = None;
            for entry in entries {
                let key = crate::row_store::TypedKey::decode_canonical(&entry.key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
                if previous.as_ref().is_some_and(|last| last >= &key)
                    || lower_exclusive.is_some_and(|lower| key <= *lower)
                {
                    return Err(GraphError::NonCanonicalData);
                }
                previous = Some(key.clone());
                if resume_after.is_some_and(|after| key <= *after) {
                    continue;
                }
                if lower_inclusive.is_some_and(|lower| key < *lower) {
                    continue;
                }
                if upper_exclusive.is_some_and(|upper| key >= *upper) {
                    break;
                }
                output.push(entry);
                if output.len() >= limit {
                    break;
                }
            }
            Ok(())
        }
        NodeData::OrderedBranch {
            domain,
            height,
            entries,
        } => {
            if domain != expected_domain
                || height == 0
                || expected_height.is_some_and(|expected| expected != height)
                || entries.is_empty()
            {
                return Err(GraphError::InvalidDomain);
            }
            let mut total_count = 0u64;
            let mut previous_fence: Option<crate::row_store::TypedKey> = None;
            for entry in entries {
                total_count = total_count
                    .checked_add(entry.row_count)
                    .filter(|count| *count <= MAX_SIGNED_LENGTH)
                    .ok_or(GraphError::InvalidCount(usize::MAX))?;
                let fence = crate::row_store::TypedKey::decode_canonical(&entry.inclusive_max_key)
                    .map_err(|_| GraphError::NonCanonicalData)?;
                if previous_fence
                    .as_ref()
                    .is_some_and(|previous| previous >= &fence)
                {
                    return Err(GraphError::NonCanonicalData);
                }
                // A fence is the child's own inclusive maximum key, so a child
                // whose fence sorts below the lower bound spans no key in the
                // interval and is left unread. Children are ordered by fence,
                // so the first child whose own minimum reaches the exclusive
                // upper bound ends the range: it and every later child lie at
                // or above it. That child is still read and validated, so the
                // upper bound keeps its exact meaning, and every child the
                // range does descend into is fully validated below. A bounded
                // range therefore reads the pages that overlap its interval
                // rather than the whole index.
                // A resume cursor skips a child whose fence is at or below it,
                // for the same reason a lower bound does: no key it holds can
                // order after the cursor.
                let within_lower = lower_inclusive.is_none_or(|lower| fence >= *lower)
                    && resume_after.is_none_or(|after| fence > *after);
                let mut past_upper = false;
                if within_lower {
                    let child = read_node(&entry.child)?;
                    let child_bounds = ordered_node_bounds_with(
                        read_node,
                        child.clone(),
                        expected_domain,
                        Some(height - 1),
                    )?;
                    if child_bounds.0 != entry.row_count
                        || child_bounds.2 != fence
                        || previous_fence
                            .as_ref()
                            .is_some_and(|previous| child_bounds.1 <= *previous)
                    {
                        return Err(GraphError::InvalidCount(
                            usize::try_from(child_bounds.0).unwrap_or(usize::MAX),
                        ));
                    }
                    if upper_exclusive.is_some_and(|upper| child_bounds.1 >= *upper) {
                        past_upper = true;
                    } else {
                        collect_ordered_range_with(
                            read_node,
                            child,
                            expected_domain,
                            Some(height - 1),
                            previous_fence.as_ref(),
                            lower_inclusive,
                            resume_after,
                            upper_exclusive,
                            limit,
                            output,
                        )?;
                    }
                }
                previous_fence = Some(fence);
                if past_upper || output.len() >= limit {
                    break;
                }
            }
            if total_count > MAX_SIGNED_LENGTH {
                return Err(GraphError::InvalidCount(usize::MAX));
            }
            Ok(())
        }
        other => Err(GraphError::WrongNodeKind {
            expected: NodeKind::OrderedLeaf,
            actual: other.kind(),
        }),
    }
}
