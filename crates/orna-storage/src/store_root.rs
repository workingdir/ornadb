use std::{borrow::Cow, collections::BTreeMap};

use orna_repository::{
    GitCommitRef, HistoricalFormatContext, IndexMaintenance, NativeGraphContext, NativeOid,
    PublicationLogicalRowRekey, Repository, RepositoryFormat, RowIndexMutation, RowMapSnapshot,
};
use orna_runtime::TableMutation;
use orna_value::Format3Context;

use crate::Error;
use crate::projection::{
    CommittedProjectionSchema, ProjectedRuntimeMutation, committed_projection_schema,
    project_runtime_mutation,
};
use crate::publication::PublicationChurn;
use crate::{graph_operation_error, repository_format_error};

/// One relation's committed state at the expected head: its row map, the
/// graph over it, the schema its rows are projected against and the value
/// context they decode in.
struct RelationBase {
    rows: RowMapSnapshot,
    graph: NativeGraphContext,
    schema: CommittedProjectionSchema<'static>,
    value_context: Format3Context,
}

impl RelationBase {
    fn load(format: &HistoricalFormatContext, relation_id: [u8; 16]) -> Result<Self, Error> {
        let rows = format
            .load_row_map(relation_id)
            .map_err(repository_format_error)?;
        let graph = format
            .open_native_graph(&rows)
            .map_err(repository_format_error)?;
        let scope = graph.open_read_scope().map_err(graph_operation_error)?;
        let descriptor = graph
            .read_schema_descriptor(&scope)
            .map_err(graph_operation_error)?;
        let schema = committed_projection_schema(Cow::Owned(descriptor))?;
        let value_context = format
            .open_value_context(relation_id)
            .map_err(repository_format_error)?;
        Ok(Self {
            rows,
            graph,
            schema,
            value_context,
        })
    }
}

/// Whether a row key exists in the committed snapshot a frozen prefix is
/// applied to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommittedPresence {
    Present,
    Absent,
}

/// The net effect of a frozen prefix on one row key: whether the key is in
/// the committed snapshot, and the stored value, if any, the prefix leaves.
#[derive(Debug, Eq, PartialEq)]
struct NetRow {
    committed: CommittedPresence,
    next: Option<Vec<u8>>,
}

impl NetRow {
    /// The row-index mutations that turn the committed row into the net row.
    /// A replacement is a delete followed by an insert; a row the prefix
    /// inserts and removes again needs none.
    fn index_mutations(self, key: &[u8]) -> Result<Vec<RowIndexMutation>, Error> {
        let delete =
            || RowIndexMutation::delete_canonical(key).map_err(Error::repository_unavailable);
        let insert = |value: &[u8]| {
            RowIndexMutation::insert_canonical(key, value).map_err(Error::repository_unavailable)
        };
        match (self.committed, self.next) {
            (CommittedPresence::Absent, None) => Ok(Vec::new()),
            (CommittedPresence::Absent, Some(value)) => Ok(vec![insert(&value)?]),
            (CommittedPresence::Present, None) => Ok(vec![delete()?]),
            (CommittedPresence::Present, Some(value)) => Ok(vec![delete()?, insert(&value)?]),
        }
    }
}

/// A frozen prefix folded into one net change per row key, so a row inserted
/// and deleted inside the prefix publishes nothing and a row replaced twice
/// publishes its last value.
#[derive(Default)]
struct NetRows {
    rows: BTreeMap<([u8; 16], Vec<u8>), NetRow>,
}

impl NetRows {
    /// Folds one projected mutation in. `creates_key` says the mutation
    /// inserts its key, so the key was absent from the committed snapshot
    /// when the prefix first touched it; a re-key target is absent for the
    /// same reason.
    fn fold(
        &mut self,
        relation_id: [u8; 16],
        projected: ProjectedRuntimeMutation,
        creates_key: bool,
    ) {
        let committed = if creates_key {
            CommittedPresence::Absent
        } else {
            CommittedPresence::Present
        };
        let source = self
            .rows
            .entry((relation_id, projected.key))
            .or_insert(NetRow {
                committed,
                next: None,
            });
        match projected.rekey_to {
            Some(target) => {
                // The old key ends absent; its row now lives at the target.
                source.next = None;
                self.rows
                    .entry((relation_id, target))
                    .or_insert(NetRow {
                        committed: CommittedPresence::Absent,
                        next: None,
                    })
                    .next = projected.value;
            }
            None => source.next = projected.value,
        }
    }

    /// The row-index mutations of every relation with a net change, keys in
    /// ascending canonical order.
    fn into_index_mutations(self) -> Result<BTreeMap<[u8; 16], Vec<RowIndexMutation>>, Error> {
        let mut by_relation = BTreeMap::<[u8; 16], Vec<RowIndexMutation>>::new();
        for ((relation_id, key), row) in self.rows {
            let mutations = row.index_mutations(&key)?;
            if !mutations.is_empty() {
                by_relation
                    .entry(relation_id)
                    .or_default()
                    .extend(mutations);
            }
        }
        Ok(by_relation)
    }
}

/// A relation's committed state with the row-index maintenance the prefix
/// performs on it, `None` when the prefix leaves its rows unchanged.
struct RelationCandidate {
    rows: RowMapSnapshot,
    graph: NativeGraphContext,
    maintenance: Option<IndexMaintenance>,
}

/// The store root a frozen prefix publishes, with what the caller journals and
/// measures.
pub(crate) struct FrozenStoreRoot {
    pub(crate) graph: NativeGraphContext,
    pub(crate) store_root: NativeOid,
    pub(crate) logical_rekeys: Vec<PublicationLogicalRowRekey>,
    pub(crate) churn: PublicationChurn,
}

pub(crate) fn build_frozen_publication_store_root(
    repository: &Repository,
    expected_head: &GitCommitRef,
    mutations: &[TableMutation],
) -> Result<FrozenStoreRoot, Error> {
    let format = repository
        .open_format_context_at_selector(expected_head.as_str())
        .map_err(Error::repository_unavailable)?;
    if format.repository_format() != RepositoryFormat::Format3 {
        return Err(Error::repository_unavailable_reason(
            "publication snapshot is not format 3",
        ));
    }

    let mut bases = BTreeMap::<[u8; 16], RelationBase>::new();
    let mut net_rows = NetRows::default();
    let mut logical_rekeys = Vec::new();
    for mutation in mutations {
        let relation_id = mutation
            .table_object_id()
            .ok_or_else(|| {
                Error::repository_unavailable_reason("runtime mutation has no table object id")
            })?
            .bytes();
        let base = match bases.entry(relation_id) {
            std::collections::btree_map::Entry::Occupied(occupied) => occupied.into_mut(),
            std::collections::btree_map::Entry::Vacant(vacant) => {
                vacant.insert(RelationBase::load(&format, relation_id)?)
            }
        };
        let projected =
            project_runtime_mutation(&base.schema, relation_id, mutation, &base.value_context)?;
        if let Some(new_key) = projected.rekey_to.as_ref() {
            logical_rekeys.push(
                PublicationLogicalRowRekey::new(
                    relation_id,
                    projected.key.clone(),
                    new_key.clone(),
                )
                .map_err(Error::repository_unavailable)?,
            );
        }
        net_rows.fold(relation_id, projected, mutation.is_insert());
    }

    // Relations are maintained in ascending id order, and the first one's
    // graph carries the publication.
    let mut index_mutations = net_rows.into_index_mutations()?;
    let mut candidates = Vec::with_capacity(bases.len());
    for (relation_id, base) in bases {
        let mutations = index_mutations.remove(&relation_id).unwrap_or_default();
        let maintenance = if mutations.is_empty() {
            None
        } else {
            let scope = base
                .graph
                .open_read_scope()
                .map_err(graph_operation_error)?;
            Some(
                base.graph
                    .maintain_row_index(&mutations, &scope)
                    .map_err(graph_operation_error)?,
            )
        };
        candidates.push(RelationCandidate {
            rows: base.rows,
            graph: base.graph,
            maintenance,
        });
    }
    let mut candidates = candidates.into_iter();
    let Some(first) = candidates.next() else {
        return Err(Error::IncompleteStaging);
    };
    let others: Vec<RelationCandidate> = candidates.collect();
    let all = || std::iter::once(&first).chain(&others);

    // A publication that changes rows also records the allocator watermark
    // known when it is made (ORNA-AUTOID-005); a no-op prefix stays a no-op.
    let rows_changed = all().any(|candidate| {
        candidate.maintenance.as_ref().is_some_and(|maintenance| {
            maintenance.primary_root() != candidate.rows.version().primary_root()
        })
    });
    let allocator_summary = if rows_changed {
        repository
            .allocator_summary_maintenance(expected_head)
            .map_err(Error::repository_unavailable)?
    } else {
        None
    };
    let scope = first
        .graph
        .open_read_scope()
        .map_err(graph_operation_error)?;
    let maintained = all()
        .filter_map(|candidate| {
            candidate
                .maintenance
                .as_ref()
                .map(|maintenance| (candidate.rows.version(), maintenance))
        })
        .chain(
            allocator_summary
                .iter()
                .map(|(rows, maintenance)| (rows.version(), maintenance)),
        )
        .collect::<Vec<_>>();
    let store_root = first
        .graph
        .maintain_relations_rows(&maintained, &scope)
        .map_err(graph_operation_error)?;
    let mut churn = PublicationChurn::default();
    for (_, maintenance) in &maintained {
        churn.add(maintenance);
    }
    let store_root = repository
        .append_rekey_log(&format, &store_root, &logical_rekeys)
        .map_err(Error::repository_unavailable)?;
    Ok(FrozenStoreRoot {
        graph: first.graph,
        store_root,
        logical_rekeys,
        churn,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_repository::TypedKey;

    fn text_key(text: &str) -> Vec<u8> {
        TypedKey::Text(text.to_owned()).canonical_bytes().unwrap()
    }

    /// A stored-field tuple the row index accepts: the one-element array `[n]`.
    fn stored(n: u8) -> Vec<u8> {
        vec![0x81, n]
    }

    fn projected(
        key: &[u8],
        rekey_to: Option<&[u8]>,
        value: Option<Vec<u8>>,
    ) -> ProjectedRuntimeMutation {
        ProjectedRuntimeMutation {
            key: key.to_vec(),
            rekey_to: rekey_to.map(<[u8]>::to_vec),
            value,
        }
    }

    fn delete_of(key: &[u8]) -> RowIndexMutation {
        RowIndexMutation::delete_canonical(key).unwrap()
    }

    fn insert_of(key: &[u8], n: u8) -> RowIndexMutation {
        RowIndexMutation::insert_canonical(key, &stored(n)).unwrap()
    }

    #[test]
    fn a_row_inserted_and_deleted_inside_one_prefix_publishes_nothing() {
        let relation = [1; 16];
        let key = text_key("a");
        let mut net = NetRows::default();
        net.fold(relation, projected(&key, None, Some(stored(1))), true);
        net.fold(relation, projected(&key, None, None), false);
        assert!(net.into_index_mutations().unwrap().is_empty());
    }

    #[test]
    fn a_committed_row_updated_twice_is_replaced_by_its_last_value() {
        let relation = [1; 16];
        let key = text_key("a");
        let mut net = NetRows::default();
        net.fold(relation, projected(&key, None, Some(stored(1))), false);
        net.fold(relation, projected(&key, None, Some(stored(2))), false);
        assert_eq!(
            net.into_index_mutations().unwrap(),
            BTreeMap::from([(relation, vec![delete_of(&key), insert_of(&key, 2)])])
        );
    }

    #[test]
    fn deleting_then_reinserting_a_committed_key_is_a_replacement() {
        let relation = [1; 16];
        let key = text_key("a");
        let mut net = NetRows::default();
        net.fold(relation, projected(&key, None, None), false);
        // The re-insert creates the key in the runtime's view, but the key was
        // already in the committed snapshot when the prefix first touched it.
        net.fold(relation, projected(&key, None, Some(stored(3))), true);
        assert_eq!(
            net.into_index_mutations().unwrap(),
            BTreeMap::from([(relation, vec![delete_of(&key), insert_of(&key, 3)])])
        );
    }

    #[test]
    fn deleting_a_committed_row_only_deletes_it() {
        let relation = [1; 16];
        let key = text_key("a");
        let mut net = NetRows::default();
        net.fold(relation, projected(&key, None, None), false);
        assert_eq!(
            net.into_index_mutations().unwrap(),
            BTreeMap::from([(relation, vec![delete_of(&key)])])
        );
    }

    #[test]
    fn a_rekey_chain_publishes_only_its_ends() {
        let relation = [1; 16];
        let (a, b, c) = (text_key("a"), text_key("b"), text_key("c"));
        let mut net = NetRows::default();
        net.fold(relation, projected(&a, Some(&b), Some(stored(4))), false);
        assert_eq!(
            net.rows.get(&(relation, b.clone())),
            Some(&NetRow {
                committed: CommittedPresence::Absent,
                next: Some(stored(4)),
            })
        );
        // `b` is the source of the second re-key but was never committed.
        net.fold(relation, projected(&b, Some(&c), Some(stored(4))), false);
        assert_eq!(
            net.into_index_mutations().unwrap(),
            BTreeMap::from([(relation, vec![delete_of(&a), insert_of(&c, 4)])])
        );
    }

    #[test]
    fn index_mutations_group_by_relation_in_canonical_key_order() {
        let (first, second) = ([1; 16], [2; 16]);
        let (a, b) = (text_key("a"), text_key("b"));
        let mut net = NetRows::default();
        net.fold(second, projected(&a, None, Some(stored(1))), true);
        net.fold(first, projected(&b, None, Some(stored(2))), true);
        net.fold(first, projected(&a, None, Some(stored(3))), true);
        let grouped = net.into_index_mutations().unwrap();
        assert_eq!(
            grouped.into_iter().collect::<Vec<_>>(),
            vec![
                (first, vec![insert_of(&a, 3), insert_of(&b, 2)]),
                (second, vec![insert_of(&a, 1)]),
            ]
        );
    }
}
