//! Incremental ORP-1 maintenance (store-format-3 §3: "Writers may rebuild
//! incrementally but must match the canonical complete build").
//!
//! The partition is a left-to-right rule whose state resets at every page
//! boundary. A boundary that falls after every change ahead of it and before
//! an unchanged entry is therefore a boundary of the canonical build too, and
//! from there on the old pages are the canonical pages. Each level is
//! rewritten only around its changes: from the page holding the last
//! unchanged entry before a change (a boundary before the change itself may
//! move, because a page closes before an entry that would overflow it) forward
//! until the new partition closes a page exactly where an old page began. The
//! level above is then rewritten the same way around the child entries that
//! changed, up to the root. Untouched pages keep their object IDs and are
//! never read.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::{
    Partition, branch_entry_cost, branch_header_len, build_branch_levels, leaf_entry_cost,
    leaf_header_len,
};
use crate::{
    native_graph::{
        GraphError, NativeOid, NodeData, NodeKind, OrderedBranchEntry, OrderedLeafEntry,
    },
    row_store::TypedKey,
};

/// The final state of one row a maintenance pass touches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RowChange {
    /// The row is removed; the pinned map must hold it.
    Remove,
    /// The row's canonical stored value, inserted or replacing the old one.
    Put(Vec<u8>),
}

/// The touched rows of one pass by logical key, each with its canonical key
/// bytes and final state.
pub(crate) type RowChanges = BTreeMap<TypedKey, (Vec<u8>, RowChange)>;

/// The outcome of one incremental pass.
#[derive(Debug)]
pub(crate) struct Maintained {
    /// The new canonical root.
    pub(crate) root: NativeOid,
    /// Rows the new root holds.
    pub(crate) row_count: u64,
    /// Nodes this pass wrote that the new map names, leaves first.
    pub(crate) written: Vec<NativeOid>,
    /// Node data of every page this pass wrote, for payload accounting.
    pub(crate) written_leaves: Vec<NodeData>,
    /// Old nodes the new map names unchanged at the edge of a rewritten
    /// region: rewritten pages equal to their old page, and untouched
    /// children of rewritten branches.
    pub(crate) reused: BTreeSet<NativeOid>,
}

/// A contiguous run of one level's old node entries and the entries that
/// replace them, in key order. The run is never empty.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Splice {
    replaced: Vec<OrderedBranchEntry>,
    emitted: Vec<OrderedBranchEntry>,
}

impl Splice {
    /// Drops the unchanged entries at either end, keeping at least one
    /// replaced entry so the splice still has a position. `None` when nothing
    /// changed.
    fn trimmed(mut self) -> Option<Self> {
        if self.replaced == self.emitted {
            return None;
        }
        while self.replaced.len() > 1
            && self.emitted.len() > 1
            && self.replaced.first() == self.emitted.first()
        {
            self.replaced.remove(0);
            self.emitted.remove(0);
        }
        while self.replaced.len() > 1
            && self.emitted.len() > 1
            && self.replaced.last() == self.emitted.last()
        {
            self.replaced.pop();
            self.emitted.pop();
        }
        Some(self)
    }
}

/// One branch on a path from the root: its entries, the child taken and the
/// exclusive lower bound of its own key range.
struct Step {
    height: u8,
    entries: Vec<OrderedBranchEntry>,
    index: usize,
    lower: Option<TypedKey>,
}

/// A node at one height of the old map and the path that reaches it.
struct Cursor {
    path: Vec<Step>,
    oid: NativeOid,
    data: NodeData,
}

impl Cursor {
    fn has_next(&self) -> bool {
        self.path
            .iter()
            .any(|step| step.index + 1 < step.entries.len())
    }

    fn has_previous(&self) -> bool {
        self.path.iter().any(|step| step.index > 0)
    }

    fn leaf_entries(&self) -> Result<&[OrderedLeafEntry], GraphError> {
        match &self.data {
            NodeData::OrderedLeaf { entries, .. } => Ok(entries),
            other => Err(GraphError::WrongNodeKind {
                expected: NodeKind::OrderedLeaf,
                actual: other.kind(),
            }),
        }
    }

    fn branch_entries(&self) -> Result<&[OrderedBranchEntry], GraphError> {
        match &self.data {
            NodeData::OrderedBranch { entries, .. } => Ok(entries),
            other => Err(GraphError::WrongNodeKind {
                expected: NodeKind::OrderedBranch,
                actual: other.kind(),
            }),
        }
    }

    /// The entry naming this node in a parent, as the parent recorded it.
    fn entry(&self) -> Result<OrderedBranchEntry, GraphError> {
        let (inclusive_max_key, row_count) = match &self.data {
            NodeData::OrderedLeaf { entries, .. } => (
                entries
                    .last()
                    .map(|entry| entry.key.clone())
                    .unwrap_or_default(),
                entries.len() as u64,
            ),
            NodeData::OrderedBranch { entries, .. } => (
                entries
                    .last()
                    .map(|entry| entry.inclusive_max_key.clone())
                    .ok_or(GraphError::InvalidCount(0))?,
                branch_count(entries)?,
            ),
            other => {
                return Err(GraphError::WrongNodeKind {
                    expected: NodeKind::OrderedLeaf,
                    actual: other.kind(),
                });
            }
        };
        Ok(OrderedBranchEntry {
            inclusive_max_key,
            child: self.oid.clone(),
            row_count,
        })
    }
}

fn decode_key(bytes: &[u8]) -> Result<TypedKey, GraphError> {
    TypedKey::decode_canonical(bytes).map_err(|_| GraphError::NonCanonicalData)
}

fn branch_count(entries: &[OrderedBranchEntry]) -> Result<u64, GraphError> {
    entries.iter().try_fold(0u64, |sum, entry| {
        sum.checked_add(entry.row_count)
            .ok_or(GraphError::InvalidCount(usize::MAX))
    })
}

/// Rejects a node whose keys or fences are not strictly ascending: the pages
/// a pass rewrites must already be in canonical order (store-format-3 §2).
fn check_ascending(node: &NodeData) -> Result<(), GraphError> {
    let keys: Vec<&[u8]> = match node {
        NodeData::OrderedLeaf { entries, .. } => {
            entries.iter().map(|entry| entry.key.as_slice()).collect()
        }
        NodeData::OrderedBranch { entries, .. } => entries
            .iter()
            .map(|entry| entry.inclusive_max_key.as_slice())
            .collect(),
        other => {
            return Err(GraphError::WrongNodeKind {
                expected: NodeKind::OrderedLeaf,
                actual: other.kind(),
            });
        }
    };
    let mut previous: Option<TypedKey> = None;
    for key in keys {
        let key = decode_key(key)?;
        if previous.as_ref().is_some_and(|previous| *previous >= key) {
            return Err(GraphError::NonCanonicalData);
        }
        previous = Some(key);
    }
    Ok(())
}

/// Reads the child an entry of a branch at `parent_height` names, and checks
/// the claims the entry makes for it: domain, height, exact row count,
/// inclusive maximum key equal to the fence, a minimum above the previous
/// fence and strictly ascending entries (store-format-3 §2).
fn read_child(
    read: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    expected_domain: &[u8],
    entry: &OrderedBranchEntry,
    parent_height: u8,
    lower: Option<&TypedKey>,
) -> Result<NodeData, GraphError> {
    let data = read(&entry.child)?;
    let height = parent_height - 1;
    let (count, maximum, minimum) = match &data {
        NodeData::OrderedLeaf { domain, entries } if height == 0 && domain == expected_domain => (
            entries.len() as u64,
            entries.last().map(|entry| entry.key.as_slice()),
            entries.first().map(|entry| entry.key.as_slice()),
        ),
        NodeData::OrderedBranch {
            domain,
            height: actual,
            entries,
        } if *actual == height && domain == expected_domain => (
            branch_count(entries)?,
            entries
                .last()
                .map(|entry| entry.inclusive_max_key.as_slice()),
            entries
                .first()
                .map(|entry| entry.inclusive_max_key.as_slice()),
        ),
        _ => return Err(GraphError::InvalidDomain),
    };
    let (Some(maximum), Some(minimum)) = (maximum, minimum) else {
        return Err(GraphError::InvalidCount(0));
    };
    if count != entry.row_count || maximum != entry.inclusive_max_key.as_slice() {
        return Err(GraphError::InvalidCount(
            usize::try_from(count).unwrap_or(usize::MAX),
        ));
    }
    if let Some(lower) = lower
        && decode_key(minimum)? <= *lower
    {
        return Err(GraphError::NonCanonicalData);
    }
    check_ascending(&data)?;
    Ok(data)
}

/// The height a root node records: 0 for a leaf.
fn root_height(root_data: &NodeData) -> Result<u8, GraphError> {
    match root_data {
        NodeData::OrderedLeaf { .. } => Ok(0),
        NodeData::OrderedBranch { height, .. } => Ok(*height),
        other => Err(GraphError::WrongNodeKind {
            expected: NodeKind::OrderedLeaf,
            actual: other.kind(),
        }),
    }
}

/// The stored values of `keys`, ascending and distinct, in the pinned map
/// whose root node `root_data` the caller has validated. Each node on the
/// paths to the keys is read once and checked against its parent's claim,
/// so a batch costs one descent per page it touches, not one per key.
/// Absent keys are missing from the result.
pub(crate) fn lookup_ordered_entries(
    domain: &[u8],
    root_data: &NodeData,
    keys: &[TypedKey],
    read: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
) -> Result<BTreeMap<TypedKey, Vec<u8>>, GraphError> {
    check_ascending(root_data)?;
    let mut found = BTreeMap::new();
    lookup_in(
        read,
        domain,
        root_data,
        root_height(root_data)?,
        None,
        keys,
        &mut found,
    )?;
    Ok(found)
}

fn lookup_in(
    read: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    domain: &[u8],
    node: &NodeData,
    height: u8,
    lower: Option<&TypedKey>,
    keys: &[TypedKey],
    found: &mut BTreeMap<TypedKey, Vec<u8>>,
) -> Result<(), GraphError> {
    match node {
        NodeData::OrderedLeaf { entries, .. } => {
            let mut wanted = keys.iter().peekable();
            for entry in entries {
                if wanted.peek().is_none() {
                    break;
                }
                let key = decode_key(&entry.key)?;
                while wanted.next_if(|wanted| **wanted < key).is_some() {}
                if wanted.next_if(|wanted| **wanted == key).is_some() {
                    found.insert(key, entry.value.clone());
                }
            }
            Ok(())
        }
        NodeData::OrderedBranch { entries, .. } => {
            let fences = entries
                .iter()
                .map(|entry| decode_key(&entry.inclusive_max_key))
                .collect::<Result<Vec<_>, _>>()?;
            let mut rest = keys;
            for (index, entry) in entries.iter().enumerate() {
                if rest.is_empty() {
                    break;
                }
                // Keys above the last fence are above the map: absent.
                let split = rest.partition_point(|key| *key <= fences[index]);
                if split == 0 {
                    continue;
                }
                let child_lower = match index.checked_sub(1) {
                    Some(previous) => Some(&fences[previous]),
                    None => lower,
                };
                let child = read_child(read, domain, entry, height, child_lower)?;
                lookup_in(
                    read,
                    domain,
                    &child,
                    height - 1,
                    child_lower,
                    &rest[..split],
                    found,
                )?;
                rest = &rest[split..];
            }
            Ok(())
        }
        other => Err(GraphError::WrongNodeKind {
            expected: NodeKind::OrderedLeaf,
            actual: other.kind(),
        }),
    }
}

/// The pinned old map, read through `read` (which the caller caches).
struct OldMap<'a, R> {
    domain: &'a [u8],
    root: NativeOid,
    root_data: NodeData,
    root_height: u8,
    read: &'a mut R,
}

impl<R: FnMut(&NativeOid) -> Result<NodeData, GraphError>> OldMap<'_, R> {
    /// Reads and checks the child an entry of a branch at `parent_height`
    /// names (see [`read_child`]).
    fn child(
        &mut self,
        entry: &OrderedBranchEntry,
        parent_height: u8,
        lower: Option<&TypedKey>,
    ) -> Result<NodeData, GraphError> {
        read_child(&mut *self.read, self.domain, entry, parent_height, lower)
    }

    /// The node at `height` whose key range covers `key`: the first child
    /// whose fence is at or above the key at each level, or the last child
    /// for a key above every fence.
    fn seek(&mut self, height: u8, key: &TypedKey) -> Result<Cursor, GraphError> {
        let mut cursor = Cursor {
            path: Vec::new(),
            oid: self.root.clone(),
            data: self.root_data.clone(),
        };
        let mut node_height = self.root_height;
        let mut lower: Option<TypedKey> = None;
        while node_height > height {
            let entries = cursor.branch_entries()?.to_vec();
            let fences = entries
                .iter()
                .map(|entry| decode_key(&entry.inclusive_max_key))
                .collect::<Result<Vec<_>, _>>()?;
            let index = fences
                .partition_point(|fence| fence < key)
                .min(entries.len() - 1);
            let child_lower = match index.checked_sub(1) {
                Some(previous) => Some(fences[previous].clone()),
                None => lower.clone(),
            };
            let data = self.child(&entries[index], node_height, child_lower.as_ref())?;
            cursor.oid = entries[index].child.clone();
            cursor.path.push(Step {
                height: node_height,
                entries,
                index,
                lower,
            });
            cursor.data = data;
            lower = child_lower;
            node_height -= 1;
        }
        Ok(cursor)
    }

    /// Moves the cursor to the next node at its height. `false` at the end of
    /// the level.
    fn next(&mut self, cursor: &mut Cursor) -> Result<bool, GraphError> {
        self.step(cursor, true)
    }

    /// Moves the cursor to the previous node at its height. `false` at the
    /// start of the level.
    fn previous(&mut self, cursor: &mut Cursor) -> Result<bool, GraphError> {
        self.step(cursor, false)
    }

    fn step(&mut self, cursor: &mut Cursor, forward: bool) -> Result<bool, GraphError> {
        let Some(turn) = cursor.path.iter().rposition(|step| {
            if forward {
                step.index + 1 < step.entries.len()
            } else {
                step.index > 0
            }
        }) else {
            return Ok(false);
        };
        let target = cursor.path.len();
        cursor.path.truncate(turn + 1);
        let step = cursor.path.last_mut().ok_or(GraphError::InvalidCount(0))?;
        if forward {
            step.index += 1;
        } else {
            step.index -= 1;
        }
        loop {
            let step = cursor.path.last().ok_or(GraphError::InvalidCount(0))?;
            let lower = match step.index.checked_sub(1) {
                Some(previous) => Some(decode_key(&step.entries[previous].inclusive_max_key)?),
                None => step.lower.clone(),
            };
            let entry = step.entries[step.index].clone();
            let data = self.child(&entry, step.height, lower.as_ref())?;
            if cursor.path.len() == target {
                cursor.oid = entry.child;
                cursor.data = data;
                return Ok(true);
            }
            let NodeData::OrderedBranch {
                height, entries, ..
            } = data
            else {
                return Err(GraphError::InvalidDomain);
            };
            let index = if forward { 0 } else { entries.len() - 1 };
            cursor.path.push(Step {
                height,
                entries,
                index,
                lower,
            });
        }
    }

    /// Rewrites the leaves around `changes` and returns the splices of the
    /// level above, in key order.
    fn rewrite_leaves(
        &mut self,
        changes: &RowChanges,
        write: &mut Writer<'_, impl FnMut(&NodeData) -> Result<NativeOid, GraphError>>,
    ) -> Result<Vec<Splice>, GraphError> {
        let domain = self.domain;
        let mut splices = Vec::new();
        let mut pending = changes.iter().peekable();
        while let Some(&(anchor, _)) = pending.peek() {
            // Start at the leaf holding the last unchanged row before the
            // first change: the boundary in front of the change may move.
            let mut cursor = self.seek(0, anchor)?;
            let starts_at_change = match cursor.leaf_entries()?.first() {
                Some(first) => decode_key(&first.key)? >= *anchor,
                None => false,
            };
            if starts_at_change && cursor.has_previous() {
                self.previous(&mut cursor)?;
            }
            let mut partition = Partition::new(leaf_header_len(domain), 0);
            let mut pages: Vec<Vec<OrderedLeafEntry>> = Vec::new();
            let mut collect = |page: Vec<OrderedLeafEntry>| -> Result<(), GraphError> {
                pages.push(page);
                Ok(())
            };
            let mut replaced = Vec::new();
            let mut old_pages = Vec::new();
            loop {
                replaced.push(cursor.entry()?);
                let entries = cursor.leaf_entries()?.to_vec();
                old_pages.push(cursor.data.clone());
                for entry in entries {
                    let key = decode_key(&entry.key)?;
                    while let Some(&(change_key, (key_bytes, change))) = pending.peek() {
                        if *change_key >= key {
                            break;
                        }
                        let RowChange::Put(value) = change else {
                            return Err(GraphError::RowKeyAbsent);
                        };
                        let new_entry = OrderedLeafEntry {
                            key: key_bytes.clone(),
                            value: value.clone(),
                        };
                        let (size, refs) = leaf_entry_cost(domain, &new_entry)?;
                        partition.push(new_entry, size, refs, &mut collect)?;
                        pending.next();
                    }
                    let entry = match pending.peek() {
                        Some(&(change_key, (_, change))) if *change_key == key => {
                            pending.next();
                            match change {
                                RowChange::Remove => continue,
                                RowChange::Put(value) => OrderedLeafEntry {
                                    key: entry.key,
                                    value: value.clone(),
                                },
                            }
                        }
                        _ => entry,
                    };
                    let (size, refs) = leaf_entry_cost(domain, &entry)?;
                    partition.push(entry, size, refs, &mut collect)?;
                }
                if !cursor.has_next() {
                    // The rows left are above every old key.
                    for (_, (key_bytes, change)) in pending.by_ref() {
                        let RowChange::Put(value) = change else {
                            return Err(GraphError::RowKeyAbsent);
                        };
                        let entry = OrderedLeafEntry {
                            key: key_bytes.clone(),
                            value: value.clone(),
                        };
                        let (size, refs) = leaf_entry_cost(domain, &entry)?;
                        partition.push(entry, size, refs, &mut collect)?;
                    }
                    partition.finish(&mut collect)?;
                    break;
                }
                if partition.is_empty() && pending.peek().is_none() {
                    break;
                }
                self.next(&mut cursor)?;
                let first = cursor
                    .leaf_entries()?
                    .first()
                    .cloned()
                    .ok_or(GraphError::InvalidCount(0))?;
                let first_key = decode_key(&first.key)?;
                let changes_beyond = pending
                    .peek()
                    .is_none_or(|&(change_key, _)| *change_key > first_key);
                let (size, refs) = leaf_entry_cost(domain, &first)?;
                if changes_beyond && (partition.is_empty() || partition.closes_before(size, &refs))
                {
                    partition.finish(&mut collect)?;
                    break;
                }
            }
            let emitted = pages
                .into_iter()
                .enumerate()
                .map(|(index, page)| {
                    write.page(
                        NodeData::OrderedLeaf {
                            domain: domain.to_vec(),
                            entries: page,
                        },
                        old_pages.get(index).zip(replaced.get(index)),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(splice) = (Splice { replaced, emitted }).trimmed() {
                splices.push(splice);
            }
        }
        Ok(splices)
    }

    /// Rewrites the branches at `height` around `below`, the splices of the
    /// level under them, and returns the splices of the level above.
    fn rewrite_branches(
        &mut self,
        height: u8,
        below: Vec<Splice>,
        write: &mut Writer<'_, impl FnMut(&NodeData) -> Result<NativeOid, GraphError>>,
    ) -> Result<Vec<Splice>, GraphError> {
        let domain = self.domain;
        let mut splices = Vec::new();
        let mut pending = below.into_iter().peekable();
        while let Some(first) = pending.peek() {
            let anchor_entry = first.replaced[0].clone();
            let anchor = decode_key(&anchor_entry.inclusive_max_key)?;
            let mut cursor = self.seek(height, &anchor)?;
            let index = cursor
                .branch_entries()?
                .iter()
                .position(|entry| *entry == anchor_entry)
                .ok_or(GraphError::ContextMismatch)?;
            if index == 0 && cursor.has_previous() {
                self.previous(&mut cursor)?;
            }
            let mut partition = Partition::new(branch_header_len(domain, height), height);
            let mut pages: Vec<Vec<OrderedBranchEntry>> = Vec::new();
            let mut collect = |page: Vec<OrderedBranchEntry>| -> Result<(), GraphError> {
                pages.push(page);
                Ok(())
            };
            let mut replaced = Vec::new();
            let mut old_pages = Vec::new();
            // The splice being applied and how many of its old entries were
            // passed; its run may continue into the next node.
            let mut applying: Option<(Splice, usize)> = None;
            loop {
                replaced.push(cursor.entry()?);
                let entries = cursor.branch_entries()?.to_vec();
                old_pages.push(cursor.data.clone());
                for entry in entries {
                    if let Some((splice, passed)) = applying.as_mut() {
                        if splice.replaced.get(*passed) != Some(&entry) {
                            return Err(GraphError::ContextMismatch);
                        }
                        *passed += 1;
                        if *passed == splice.replaced.len() {
                            applying = None;
                        }
                        continue;
                    }
                    if pending
                        .peek()
                        .is_some_and(|splice| splice.replaced[0] == entry)
                    {
                        let splice = pending.next().ok_or(GraphError::ContextMismatch)?;
                        for new_entry in &splice.emitted {
                            let (size, refs) = branch_entry_cost(new_entry);
                            partition.push(new_entry.clone(), size, refs, &mut collect)?;
                        }
                        if splice.replaced.len() > 1 {
                            applying = Some((splice, 1));
                        }
                        continue;
                    }
                    let (size, refs) = branch_entry_cost(&entry);
                    partition.push(entry, size, refs, &mut collect)?;
                }
                if !cursor.has_next() {
                    if applying.is_some() || pending.peek().is_some() {
                        return Err(GraphError::ContextMismatch);
                    }
                    partition.finish(&mut collect)?;
                    break;
                }
                if applying.is_none() && partition.is_empty() && pending.peek().is_none() {
                    break;
                }
                self.next(&mut cursor)?;
                let first = cursor
                    .branch_entries()?
                    .first()
                    .cloned()
                    .ok_or(GraphError::InvalidCount(0))?;
                let first_fence = decode_key(&first.inclusive_max_key)?;
                let changes_beyond = applying.is_none()
                    && match pending.peek() {
                        None => true,
                        Some(splice) => {
                            decode_key(&splice.replaced[0].inclusive_max_key)? > first_fence
                        }
                    };
                let (size, refs) = branch_entry_cost(&first);
                if changes_beyond && (partition.is_empty() || partition.closes_before(size, &refs))
                {
                    partition.finish(&mut collect)?;
                    break;
                }
            }
            let emitted = pages
                .into_iter()
                .enumerate()
                .map(|(index, page)| {
                    write.page(
                        NodeData::OrderedBranch {
                            domain: domain.to_vec(),
                            height,
                            entries: page,
                        },
                        old_pages.get(index).zip(replaced.get(index)),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(splice) = (Splice { replaced, emitted }).trimmed() {
                splices.push(splice);
            }
        }
        Ok(splices)
    }
}

/// Writes rewritten pages, reusing an old node's object ID when a rewritten
/// page is exactly the old page at its position.
struct Writer<'a, W> {
    write: &'a mut W,
    written: Vec<NativeOid>,
    written_data: HashMap<NativeOid, NodeData>,
    reused: BTreeSet<NativeOid>,
}

impl<W: FnMut(&NodeData) -> Result<NativeOid, GraphError>> Writer<'_, W> {
    fn page(
        &mut self,
        node: NodeData,
        old: Option<(&NodeData, &OrderedBranchEntry)>,
    ) -> Result<OrderedBranchEntry, GraphError> {
        let (inclusive_max_key, row_count) = match &node {
            NodeData::OrderedLeaf { entries, .. } => (
                entries
                    .last()
                    .map(|entry| entry.key.clone())
                    .ok_or(GraphError::InvalidCount(0))?,
                entries.len() as u64,
            ),
            NodeData::OrderedBranch { entries, .. } => (
                entries
                    .last()
                    .map(|entry| entry.inclusive_max_key.clone())
                    .ok_or(GraphError::InvalidCount(0))?,
                branch_count(entries)?,
            ),
            other => {
                return Err(GraphError::WrongNodeKind {
                    expected: NodeKind::OrderedLeaf,
                    actual: other.kind(),
                });
            }
        };
        let child = match old {
            Some((old_data, old_entry)) if *old_data == node => {
                self.reused.insert(old_entry.child.clone());
                old_entry.child.clone()
            }
            _ => self.write_node(node)?,
        };
        Ok(OrderedBranchEntry {
            inclusive_max_key,
            child,
            row_count,
        })
    }

    fn write_node(&mut self, node: NodeData) -> Result<NativeOid, GraphError> {
        let oid = (self.write)(&node)?;
        if let NodeData::OrderedBranch { entries, .. } = &node {
            for entry in entries {
                if !self.written_data.contains_key(&entry.child) {
                    self.reused.insert(entry.child.clone());
                }
            }
        }
        self.written.push(oid.clone());
        self.written_data.insert(oid.clone(), node);
        Ok(oid)
    }
}

/// Applies `changes` to the pinned map at `root`, whose node data
/// `root_data` the caller has validated against the map's domain and row
/// count, and returns the canonical new root. Only the paths to the changes
/// and the pages around them are read; `read` should cache, since a pass may
/// revisit a node. A pass that changes no page keeps the old root, which it
/// reports as reused.
pub(crate) fn maintain_ordered_map(
    domain: &[u8],
    root: NativeOid,
    root_data: NodeData,
    changes: &RowChanges,
    read: &mut impl FnMut(&NativeOid) -> Result<NodeData, GraphError>,
    write_node: &mut impl FnMut(&NodeData) -> Result<NativeOid, GraphError>,
) -> Result<Maintained, GraphError> {
    let root_height = root_height(&root_data)?;
    check_ascending(&root_data)?;
    let old_count = match &root_data {
        NodeData::OrderedLeaf { entries, .. } => entries.len() as u64,
        NodeData::OrderedBranch { entries, .. } => branch_count(entries)?,
        _ => 0,
    };
    let mut writer = Writer {
        write: write_node,
        written: Vec::new(),
        written_data: HashMap::new(),
        reused: BTreeSet::new(),
    };
    let unchanged = |mut reused: BTreeSet<NativeOid>| {
        reused.insert(root.clone());
        Maintained {
            root: root.clone(),
            row_count: old_count,
            written: Vec::new(),
            written_leaves: Vec::new(),
            reused,
        }
    };
    if changes.is_empty() {
        return Ok(unchanged(BTreeSet::new()));
    }
    let mut old = OldMap {
        domain,
        root: root.clone(),
        root_data,
        root_height,
        read,
    };
    let mut splices = old.rewrite_leaves(changes, &mut writer)?;
    for height in 1..=root_height {
        if splices.is_empty() {
            return Ok(unchanged(writer.reused));
        }
        splices = old.rewrite_branches(height, splices, &mut writer)?;
    }
    // The root is the only node at its height, so the last level is one
    // splice that replaces it.
    let level = match splices.as_slice() {
        [] => return Ok(unchanged(writer.reused)),
        [splice] if splice.replaced.len() == 1 && splice.replaced[0].child == root => {
            splice.emitted.clone()
        }
        _ => return Err(GraphError::ContextMismatch),
    };
    let mut root = match level.len() {
        0 => writer.write_node(NodeData::OrderedLeaf {
            domain: domain.to_vec(),
            entries: Vec::new(),
        })?,
        1 => level[0].child.clone(),
        _ => {
            let mut write_level = |node: &NodeData| writer.write_node(node.clone());
            build_branch_levels(domain, root_height + 1, level, &mut write_level)?
        }
    };
    // A root with one child collapses: the canonical root is the lowest level
    // holding a single node (store-format-3 §2).
    loop {
        let data = match writer.written_data.get(&root) {
            Some(data) => data.clone(),
            None => (old.read)(&root)?,
        };
        match data {
            NodeData::OrderedBranch { entries, .. } if entries.len() == 1 => {
                writer.written.retain(|oid| *oid != root);
                root = entries[0].child.clone();
            }
            _ => break,
        }
    }
    let row_count = match writer.written_data.get(&root) {
        Some(NodeData::OrderedLeaf { entries, .. }) => entries.len() as u64,
        Some(NodeData::OrderedBranch { entries, .. }) => branch_count(entries)?,
        _ => match (old.read)(&root)? {
            NodeData::OrderedLeaf { entries, .. } => entries.len() as u64,
            NodeData::OrderedBranch { entries, .. } => branch_count(&entries)?,
            _ => return Err(GraphError::InvalidDomain),
        },
    };
    let written_leaves = writer
        .written
        .iter()
        .filter_map(|oid| match writer.written_data.get(oid) {
            Some(leaf @ NodeData::OrderedLeaf { .. }) => Some(leaf.clone()),
            _ => None,
        })
        .collect();
    Ok(Maintained {
        root,
        row_count,
        written: writer.written,
        written_leaves,
        reused: writer.reused,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        native_graph::{CborValue, GitHashAlgorithm, canonical_value_bytes},
        orp::build_ordered_map,
    };

    /// An in-memory node store; `reads` holds the nodes the last pass read.
    #[derive(Default)]
    struct Store {
        nodes: HashMap<NativeOid, NodeData>,
        reads: BTreeSet<NativeOid>,
    }

    impl Store {
        fn write(&mut self, node: &NodeData) -> Result<NativeOid, GraphError> {
            let digest = <sha1::Sha1 as sha1::Digest>::digest(node.encode_canonical()?);
            let oid = NativeOid::new(GitHashAlgorithm::Sha1, digest.as_slice())?;
            self.nodes.insert(oid.clone(), node.clone());
            Ok(oid)
        }
    }

    /// A small deterministic generator, so a failing case reproduces.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound.max(1)
        }
    }

    fn domain() -> Vec<u8> {
        canonical_value_bytes(&CborValue::Array(vec![
            CborValue::Text("rows".to_owned()),
            CborValue::Bytes(vec![0x11; 16]),
            CborValue::Bytes(vec![0x22; 32]),
        ]))
    }

    fn key(index: u64) -> TypedKey {
        TypedKey::UInt(index)
    }

    fn value(length: usize, salt: u64) -> Vec<u8> {
        canonical_value_bytes(&CborValue::Array(vec![
            CborValue::Unsigned(salt),
            CborValue::Text("v".repeat(length)),
        ]))
    }

    fn entry(index: u64, value: Vec<u8>) -> OrderedLeafEntry {
        OrderedLeafEntry {
            key: key(index).canonical_bytes().expect("canonical key"),
            value,
        }
    }

    fn build(store: &mut Store, rows: &BTreeMap<u64, Vec<u8>>) -> NativeOid {
        build_ordered_map(
            &domain(),
            rows.iter()
                .map(|(index, value)| entry(*index, value.clone())),
            &mut |node| store.write(node),
        )
        .expect("complete build")
    }

    fn maintain(
        store: &mut Store,
        root: &NativeOid,
        changes: &RowChanges,
    ) -> Result<Maintained, GraphError> {
        let root_data = store.nodes.get(root).cloned().expect("root node");
        let nodes = store.nodes.clone();
        let mut written = HashMap::new();
        let mut reads = BTreeSet::new();
        let result = maintain_ordered_map(
            &domain(),
            root.clone(),
            root_data,
            changes,
            &mut |oid| {
                reads.insert(oid.clone());
                nodes.get(oid).cloned().ok_or(GraphError::UnavailableObject)
            },
            &mut |node| {
                let digest = <sha1::Sha1 as sha1::Digest>::digest(node.encode_canonical()?);
                let oid = NativeOid::new(GitHashAlgorithm::Sha1, digest.as_slice())?;
                written.insert(oid.clone(), node.clone());
                Ok(oid)
            },
        );
        store.nodes.extend(written);
        store.reads = reads;
        result
    }

    /// store-format-3 section 3: "Writers may rebuild incrementally but must
    /// match the canonical complete build." Random maps of small, page-sized
    /// and near-inline rows take random removes, updates and inserts
    /// (including before the first and after the last key, whole pages
    /// deleted, and the table emptied); the incremental root must equal the
    /// complete build of the resulting rows every time (A07-storage-rows-04).
    #[test]
    fn incremental_maintenance_matches_the_complete_build() {
        let mut rng = Rng(0x5eed_0129_0bad_cafe);
        for case in 0..160u64 {
            let mut store = Store::default();
            let rows_before = [0, 1, 15, 17, 40, 300, 1200, 3000][(case % 8) as usize];
            let size_class = case / 8 % 4;
            let length = |rng: &mut Rng| match size_class {
                0 => rng.below(8) as usize,
                1 => 200 + rng.below(400) as usize,
                2 => 2_000 + rng.below(4_000) as usize,
                _ => 7_000 + rng.below(1_100) as usize,
            };
            let mut rows = BTreeMap::new();
            for index in 0..rows_before {
                let length = length(&mut rng);
                rows.insert(index * 4 + 2, value(length, index));
            }
            let root = build(&mut store, &rows);

            let mut changes = RowChanges::new();
            let edits = 1 + rng.below(if case % 5 == 0 { 400 } else { 12 });
            let empty_it = case % 37 == 0;
            for _ in 0..edits {
                let index = rng.below(rows_before * 4 + 8);
                let present = rows.contains_key(&index);
                let change = if present && (empty_it || rng.below(3) == 0) {
                    rows.remove(&index);
                    RowChange::Remove
                } else if empty_it {
                    continue;
                } else {
                    let new_value = value(length(&mut rng), rng.next());
                    rows.insert(index, new_value.clone());
                    RowChange::Put(new_value)
                };
                let previous = changes.insert(
                    key(index),
                    (key(index).canonical_bytes().expect("key"), change.clone()),
                );
                // A key touched twice keeps its final state: removing a row
                // inserted by this pass leaves no change at all.
                if let (Some((_, RowChange::Put(_))), RowChange::Remove) = (&previous, &change)
                    && !store_has(&store, &root, index)
                {
                    changes.remove(&key(index));
                }
            }
            if empty_it {
                for index in rows.keys().copied().collect::<Vec<_>>() {
                    rows.remove(&index);
                    changes.insert(
                        key(index),
                        (
                            key(index).canonical_bytes().expect("key"),
                            RowChange::Remove,
                        ),
                    );
                }
            }
            let maintained = maintain(&mut store, &root, &changes)
                .unwrap_or_else(|error| panic!("case {case}: {error:?}"));
            let mut expected_store = Store::default();
            let expected = build(&mut expected_store, &rows);
            assert_eq!(maintained.root, expected, "case {case}: canonical root");
            assert_eq!(maintained.row_count, rows.len() as u64, "case {case}: rows");
        }
    }

    /// Whether the map at `root` holds the row `index`, by a full scan of the
    /// in-memory store (test bookkeeping only).
    fn store_has(store: &Store, root: &NativeOid, index: u64) -> bool {
        let wanted = key(index).canonical_bytes().expect("key");
        let mut pending = vec![root.clone()];
        while let Some(oid) = pending.pop() {
            match store.nodes.get(&oid) {
                Some(NodeData::OrderedLeaf { entries, .. }) => {
                    if entries.iter().any(|entry| entry.key == wanted) {
                        return true;
                    }
                }
                Some(NodeData::OrderedBranch { entries, .. }) => {
                    pending.extend(entries.iter().map(|entry| entry.child.clone()));
                }
                _ => {}
            }
        }
        false
    }

    /// ORNA-ROW-006: copy-on-write maintenance reuses unchanged pages. One
    /// update in a 20,000-row map reads the path to the row and the pages
    /// beside it, never the table, and every untouched page keeps its object.
    #[test]
    fn one_change_reads_only_the_pages_around_it() {
        let mut store = Store::default();
        let rows: BTreeMap<u64, Vec<u8>> =
            (0..20_000).map(|index| (index, value(24, index))).collect();
        let root = build(&mut store, &rows);
        let nodes_before = store.nodes.len();
        let mut changes = RowChanges::new();
        changes.insert(
            key(12_345),
            (
                key(12_345).canonical_bytes().expect("key"),
                RowChange::Put(value(24, 1)),
            ),
        );
        let maintained = maintain(&mut store, &root, &changes).expect("one update");
        assert!(
            store.reads.len() <= 12,
            "one update read {} of {nodes_before} nodes",
            store.reads.len()
        );
        assert!(
            maintained.written.len() <= 6,
            "one update wrote {} nodes",
            maintained.written.len()
        );
        assert!(!maintained.reused.is_empty(), "untouched pages are reused");
        let mut expected_rows = rows;
        expected_rows.insert(12_345, value(24, 1));
        let mut expected_store = Store::default();
        assert_eq!(maintained.root, build(&mut expected_store, &expected_rows));
    }
}
