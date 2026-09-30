use orna_evolution_v1::{
    CanonicalValue, KeyedRow, ObjectId, RowKeyKind, RowMergeConflict, RowMergeOperation,
    RowSnapshotMergeError, RowSnapshotSide, RowSnapshotState, merge_keyed_row,
    merge_keyed_row_states,
};
use orna_foundation_v1::OvbRaw;
use orna_syntax_v1::{Expr, LiteralKind, parse_row};
use std::collections::BTreeMap;

const BASE: &str = include_str!("fixtures/merge-row-base.orna");
const LEFT: &str = include_str!("fixtures/merge-row-left.orna");
const RIGHT: &str = include_str!("fixtures/merge-row-right.orna");
const SAME_EDIT: &str = include_str!("fixtures/merge-row-same-edit.orna");
const CONFLICT: &str = include_str!("fixtures/merge-row-conflict.orna");
const DELETE_CITY: &str = include_str!("fixtures/merge-row-delete-city.orna");
const MULTI_CONFLICT_LEFT: &str = include_str!("fixtures/merge-row-multi-conflict-left.orna");
const MULTI_CONFLICT_RIGHT: &str = include_str!("fixtures/merge-row-multi-conflict-right.orna");
const OTHER_KEY: &str = include_str!("fixtures/merge-row-other-key.orna");

fn id(value: u8) -> ObjectId {
    ObjectId::new([value; 16])
}

fn parse_fixture(source: &str) -> KeyedRow {
    let parsed = parse_row(source);
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    let Expr::Record { fields, .. } = parsed.value else { panic!("row fixture must be a record") };
    let mut key = None;
    let mut values = BTreeMap::new();
    for field in fields {
        let Expr::Literal { text, kind, .. } = field.value else { panic!("row fields must be literals") };
        match field.name.as_str() {
            "id" => {
                assert_eq!(kind, LiteralKind::Integer);
                let number: i64 = text.parse().unwrap();
                key = Some(CanonicalValue::new(OvbRaw::Int(number.into())).unwrap());
            }
            "name" | "city" => {
                assert_eq!(kind, LiteralKind::String);
                let value = text.strip_prefix('"').unwrap().strip_suffix('"').unwrap();
                let field_id = if field.name == "name" { id(2) } else { id(3) };
                values.insert(field_id, CanonicalValue::new(OvbRaw::Text(value.into())).unwrap());
            }
            other => panic!("unexpected fixture field {other}"),
        }
    }
    KeyedRow {
        table: id(1),
        key: key.unwrap(),
        key_kind: RowKeyKind::Explicit,
        fields: values,
    }
}

fn tombstone(row: &KeyedRow) -> RowSnapshotState<'_> {
    RowSnapshotState::Tombstone { table: row.table, key: &row.key }
}

#[test]
fn disjoint_edits_convergent_edits_and_unchanged_sides_reconcile() {
    let base = parse_fixture(BASE);
    let left = parse_fixture(LEFT);
    let right = parse_fixture(RIGHT);
    let mut expected = left.clone();
    expected.fields.insert(
        id(3),
        CanonicalValue::new(OvbRaw::Text("Paris".into())).unwrap(),
    );
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Present(&left),
            RowSnapshotState::Present(&right),
        ),
        Ok(RowMergeOperation::Upsert(expected)),
    );

    let same_edit = parse_fixture(SAME_EDIT);
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Present(&same_edit),
            RowSnapshotState::Present(&same_edit),
        ),
        Ok(RowMergeOperation::Upsert(same_edit.clone())),
    );
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Present(&left),
            RowSnapshotState::Present(&base),
        ),
        Ok(RowMergeOperation::Upsert(left.clone())),
    );

    let mut expected_with_deleted_city = parse_fixture(LEFT);
    expected_with_deleted_city.fields.remove(&id(3));
    let delete_city = parse_fixture(DELETE_CITY);
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Present(&delete_city),
            RowSnapshotState::Present(&left),
        ),
        Ok(RowMergeOperation::Upsert(expected_with_deleted_city)),
    );

    let conflict = parse_fixture(CONFLICT);
    assert!(matches!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Present(&left),
            RowSnapshotState::Present(&conflict),
        ),
        Err(RowSnapshotMergeError::Conflict(RowMergeConflict::Fields { fields, .. }))
            if fields == vec![id(2)]
    ));
    assert!(matches!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Present(&delete_city),
            RowSnapshotState::Present(&right),
        ),
        Err(RowSnapshotMergeError::Conflict(RowMergeConflict::Fields { fields, .. }))
            if fields == vec![id(3)]
    ));
}

#[test]
fn known_deletions_emit_tombstones_but_never_created_rows_remain_absent() {
    let base = parse_fixture(BASE);
    let tombstone = RowMergeOperation::Tombstone { table: base.table, key: base.key.clone() };
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Absent,
            RowSnapshotState::Present(&base),
        ),
        Ok(tombstone.clone()),
    );
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Absent,
            RowSnapshotState::Absent,
        ),
        Ok(tombstone),
    );

    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Absent,
            RowSnapshotState::Absent,
            RowSnapshotState::Absent,
        ),
        Ok(RowMergeOperation::Absent),
    );
    let inserted = parse_fixture(LEFT);
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Absent,
            RowSnapshotState::Present(&inserted),
            RowSnapshotState::Absent,
        ),
        Ok(RowMergeOperation::Upsert(inserted)),
    );
}

#[test]
fn retained_tombstones_are_logical_absence_and_can_be_folded_from_complete_snapshots() {
    let live = parse_fixture(BASE);
    let deleted = RowMergeOperation::Tombstone {
        table: live.table,
        key: live.key.clone(),
    };
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&live),
            tombstone(&live),
            RowSnapshotState::Present(&live),
        ),
        Ok(deleted),
    );
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&live),
            tombstone(&live),
            tombstone(&live),
        ),
        Ok(RowMergeOperation::Tombstone {
            table: live.table,
            key: live.key.clone(),
        }),
    );
    let edited = parse_fixture(CONFLICT);
    assert!(matches!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&live),
            tombstone(&live),
            RowSnapshotState::Present(&edited),
        ),
        Err(RowSnapshotMergeError::Conflict(RowMergeConflict::DeleteAndEdit { .. }))
    ));

    // A complete base already records absence. Once its tombstone effect is
    // folded into a replacement snapshot, the marker can be omitted there.
    assert_eq!(
        merge_keyed_row_states(
            tombstone(&live),
            tombstone(&live),
            RowSnapshotState::Absent,
        ),
        Ok(RowMergeOperation::Absent),
    );

    // If another branch inserts at an absent base key, an old marker is not
    // a competing value and must not erase the new live row.
    let inserted = parse_fixture(LEFT);
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Absent,
            tombstone(&inserted),
            RowSnapshotState::Present(&inserted),
        ),
        Ok(RowMergeOperation::Upsert(inserted)),
    );

    let reinserted = parse_fixture(LEFT);
    assert_eq!(
        merge_keyed_row_states(
            tombstone(&reinserted),
            RowSnapshotState::Present(&reinserted),
            tombstone(&reinserted),
        ),
        Ok(RowMergeOperation::Upsert(reinserted)),
    );
}

#[test]
fn pruned_history_is_unavailable_and_delete_update_stays_a_conflict() {
    let base = parse_fixture(BASE);
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Pruned,
            RowSnapshotState::Present(&base),
        ),
        Err(RowSnapshotMergeError::PrunedInput {
            sides: vec![RowSnapshotSide::Left],
        }),
    );
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Pruned,
            RowSnapshotState::Absent,
            RowSnapshotState::Absent,
        ),
        Err(RowSnapshotMergeError::PrunedInput {
            sides: vec![RowSnapshotSide::Base],
        }),
    );

    let edited = parse_fixture(LEFT);
    assert!(matches!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Absent,
            RowSnapshotState::Present(&edited),
        ),
        Err(RowSnapshotMergeError::Conflict(RowMergeConflict::DeleteAndEdit { .. }))
    ));
}

#[test]
fn pruned_sides_are_reported_deterministically_in_merge_argument_order() {
    let base = parse_fixture(BASE);
    let expected = Err(RowSnapshotMergeError::PrunedInput {
        sides: vec![RowSnapshotSide::Base, RowSnapshotSide::Right],
    });
    for _ in 0..3 {
        assert_eq!(
            merge_keyed_row_states(
                RowSnapshotState::Pruned,
                RowSnapshotState::Present(&base),
                RowSnapshotState::Pruned,
            ),
            expected,
        );
    }
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Pruned,
            RowSnapshotState::Pruned,
            RowSnapshotState::Pruned,
        ),
        Err(RowSnapshotMergeError::PrunedInput {
            sides: vec![RowSnapshotSide::Base, RowSnapshotSide::Left, RowSnapshotSide::Right],
        }),
    );
}

#[test]
fn multi_field_conflicts_are_stable_under_branch_and_fixture_field_order() {
    let base = parse_fixture(BASE);
    let left = parse_fixture(MULTI_CONFLICT_LEFT);
    let right = parse_fixture(MULTI_CONFLICT_RIGHT);
    let expected = Err(RowSnapshotMergeError::Conflict(RowMergeConflict::Fields {
        table: id(1),
        key: base.key.clone(),
        fields: vec![id(2), id(3)],
    }));
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Present(&left),
            RowSnapshotState::Present(&right),
        ),
        expected,
    );
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            RowSnapshotState::Present(&right),
            RowSnapshotState::Present(&left),
        ),
        expected,
    );
}

#[test]
fn identity_conflicts_have_a_stable_anchor_and_tombstones_cannot_change_keys() {
    let base = parse_fixture(BASE);
    let other = parse_fixture(OTHER_KEY);
    let expected = RowMergeConflict::Identity {
        table: base.table,
        key: base.key.clone(),
    };

    // With no common base, the encoded canonical key order is the diagnostic
    // tie-breaker. Integer key 1 precedes key 2 in OVB, independent of sides.
    assert_eq!(merge_keyed_row(None, Some(&base), Some(&other)), Err(expected.clone()));
    assert_eq!(merge_keyed_row(None, Some(&other), Some(&base)), Err(expected.clone()));
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Absent,
            RowSnapshotState::Present(&other),
            RowSnapshotState::Present(&base),
        ),
        Err(RowSnapshotMergeError::Conflict(expected.clone())),
    );

    // A tombstone is scoped to its own table/key and cannot be silently
    // interpreted as a deletion for whichever row happens to be in the base.
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            tombstone(&other),
            RowSnapshotState::Present(&base),
        ),
        Err(RowSnapshotMergeError::Conflict(expected.clone())),
    );
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Absent,
            tombstone(&other),
            RowSnapshotState::Present(&base),
        ),
        Err(RowSnapshotMergeError::Conflict(expected)),
    );
}

#[test]
fn pruned_history_takes_precedence_over_tombstone_identity_checks() {
    let base = parse_fixture(BASE);
    let other = parse_fixture(OTHER_KEY);
    assert_eq!(
        merge_keyed_row_states(
            RowSnapshotState::Present(&base),
            tombstone(&other),
            RowSnapshotState::Pruned,
        ),
        Err(RowSnapshotMergeError::PrunedInput {
            sides: vec![RowSnapshotSide::Right],
        }),
    );
}
