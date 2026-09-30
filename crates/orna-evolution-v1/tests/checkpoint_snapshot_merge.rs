use orna_evolution_v1::{
    CanonicalValue, CheckpointConflictReason, CheckpointIdentity, CheckpointPosition,
    CheckpointPositionError, CheckpointSnapshot, CheckpointSnapshotError,
    CheckpointSnapshotRefs, merge_checkpoint_snapshots, merge_checkpoint_snapshots_bounded,
};
use orna_foundation_v1::OvbRaw;

const BASE: &str = include_str!("fixtures/checkpoint-base.orna");
const LEFT: &str = include_str!("fixtures/checkpoint-left.orna");
const RIGHT: &str = include_str!("fixtures/checkpoint-right.orna");
const MIGRATED: &str = include_str!("fixtures/checkpoint-migrated.orna");

fn identity(source: &str, partition: Option<&str>) -> CheckpointIdentity {
    CheckpointIdentity::new(
        b"consumer:database/function/arguments".to_vec(),
        source,
        partition.map(str::to_owned),
    )
}

fn position(format: &str, version: u32, fixture: &str) -> CheckpointPosition {
    let canonical = CanonicalValue::new(OvbRaw::Bytes(fixture.as_bytes().to_vec()))
        .unwrap()
        .encode()
        .unwrap();
    CheckpointPosition::new(format, version, canonical).unwrap()
}

fn snapshot(
    entries: impl IntoIterator<Item = (CheckpointIdentity, CheckpointPosition)>,
) -> CheckpointSnapshot {
    CheckpointSnapshot::new(entries).unwrap()
}

fn refs() -> CheckpointSnapshotRefs {
    CheckpointSnapshotRefs {
        base: b"base-snapshot".to_vec(),
        left: b"left-snapshot".to_vec(),
        right: b"right-snapshot".to_vec(),
    }
}

#[test]
fn snapshot_merge_combines_disjoint_progress_and_preserves_natural_identity() {
    let unpartitioned = identity("orders", None);
    let empty_partition = identity("orders", Some(""));
    let east_partition = identity("orders", Some("east"));
    let left_only = identity("orders", Some("left-only"));
    let right_only = identity("inventory", None);
    let base_unpartitioned = position("cursor", 1, BASE);
    let base_empty = position("cursor", 1, RIGHT);
    let base_east = position("cursor", 1, LEFT);
    let left_progress = position("cursor", 1, LEFT);
    let right_progress = position("cursor", 1, RIGHT);
    let left = snapshot([
        (unpartitioned.clone(), left_progress.clone()),
        (empty_partition.clone(), base_empty.clone()),
        (east_partition.clone(), base_east.clone()),
        (left_only.clone(), position("cursor", 1, BASE)),
    ]);
    let right = snapshot([
        (unpartitioned.clone(), base_unpartitioned.clone()),
        (empty_partition.clone(), right_progress.clone()),
        (east_partition.clone(), base_east.clone()),
        (right_only.clone(), position("cursor", 1, BASE)),
    ]);
    let base = snapshot([
        (unpartitioned.clone(), base_unpartitioned),
        (empty_partition.clone(), base_empty),
        (east_partition.clone(), base_east),
    ]);

    let merged = merge_checkpoint_snapshots(&base, &left, &right, &refs()).unwrap();
    assert_eq!(merged.checkpoints().len(), 5);
    assert_eq!(merged.get(&unpartitioned), Some(&left_progress));
    assert_eq!(merged.get(&empty_partition), Some(&right_progress));
    assert!(merged.get(&identity("orders", None)).is_some());
    assert!(merged.get(&identity("orders", Some(""))).is_some());
    assert!(merged.get(&left_only).is_some());
    assert!(merged.get(&right_only).is_some());
}

#[test]
fn equal_opaque_positions_merge_and_unilateral_deletes_are_retained() {
    let equal_id = identity("orders", Some("east"));
    let deleted_id = identity("orders", Some("west"));
    let base = snapshot([
        (equal_id.clone(), position("cursor", 1, BASE)),
        (deleted_id.clone(), position("cursor", 1, BASE)),
    ]);
    let shared = position("cursor", 1, LEFT);
    let left = snapshot([(equal_id.clone(), shared.clone())]);
    let right = snapshot([(equal_id.clone(), shared.clone()), (deleted_id.clone(), position("cursor", 1, BASE))]);

    let merged = merge_checkpoint_snapshots(&base, &left, &right, &refs()).unwrap();
    assert_eq!(merged.get(&equal_id), Some(&shared));
    assert!(merged.get(&deleted_id).is_none());
}

#[test]
fn divergent_positions_formats_and_delete_updates_keep_typed_conflict_evidence() {
    let id = identity("orders", Some("east"));
    let base_position = position("cursor", 1, BASE);
    let left_position = position("cursor", 1, LEFT);
    let right_position = position("cursor", 1, RIGHT);
    let base = snapshot([(id.clone(), base_position.clone())]);

    let divergent = merge_checkpoint_snapshots(
        &base,
        &snapshot([(id.clone(), left_position.clone())]),
        &snapshot([(id.clone(), right_position.clone())]),
        &refs(),
    )
    .unwrap_err();
    assert_eq!(divergent.len(), 1);
    assert_eq!(divergent[0].reason, CheckpointConflictReason::DivergentPosition);
    assert_eq!(divergent[0].reason.as_str(), "divergent_position");
    assert_eq!(divergent[0].base, Some(base_position.clone()));
    assert_eq!(divergent[0].left, Some(left_position));
    assert_eq!(divergent[0].right, Some(right_position));
    assert_eq!(divergent[0].left_snapshot, b"left-snapshot");
    assert_eq!(divergent[0].right_snapshot, b"right-snapshot");

    let incompatible = merge_checkpoint_snapshots(
        &base,
        &snapshot([(id.clone(), position("cursor-v2", 2, LEFT))]),
        &snapshot([(id.clone(), position("cursor-v3", 3, RIGHT))]),
        &refs(),
    )
    .unwrap_err();
    assert_eq!(incompatible[0].reason, CheckpointConflictReason::IncompatibleFormat);
    assert_eq!(incompatible[0].reason.as_str(), "incompatible_format");

    let deletion_update = merge_checkpoint_snapshots(
        &base,
        &snapshot([]),
        &snapshot([(id, position("cursor", 1, RIGHT))]),
        &refs(),
    )
    .unwrap_err();
    assert_eq!(deletion_update[0].reason, CheckpointConflictReason::DeleteUpdate);
    assert_eq!(deletion_update[0].reason.as_str(), "delete_update");
}

#[test]
fn conflict_budget_returns_only_a_bounded_prefix_and_position_requires_portable_bytes() {
    let first = identity("orders", Some("east"));
    let second = identity("orders", Some("west"));
    let base_position = position("cursor", 1, BASE);
    let base = snapshot([
        (first.clone(), base_position.clone()),
        (second.clone(), base_position.clone()),
    ]);
    let left_position = position("cursor", 1, LEFT);
    let right_position = position("cursor", 1, RIGHT);
    let left = snapshot([
        (first.clone(), left_position.clone()),
        (second.clone(), left_position),
    ]);
    let right = snapshot([
        (first, right_position.clone()),
        (second, right_position),
    ]);
    let failure = merge_checkpoint_snapshots_bounded(&base, &left, &right, &refs(), 1)
        .unwrap_err();
    assert_eq!(failure.conflicts.len(), 1);
    assert_eq!(failure.conflicts_lower_bound, 2);

    assert_eq!(
        CheckpointPosition::new("cursor", 0, position("cursor", 1, BASE).canonical_payload().to_vec()),
        Err(CheckpointPositionError::InvalidFormat)
    );
    assert_eq!(
        CheckpointPosition::new("cursor", 1, vec![0xff]),
        Err(CheckpointPositionError::InvalidCanonicalPayload)
    );
    let duplicate_id = identity("orders", None);
    let one = position("cursor", 1, BASE);
    assert_eq!(
        CheckpointSnapshot::new([(duplicate_id.clone(), one.clone()), (duplicate_id, one)]),
        Err(CheckpointSnapshotError::DuplicateIdentity)
    );

    let _fixture_is_a_real_position = position("cursor", 2, MIGRATED);
}

#[test]
fn conflict_order_and_delete_update_reason_are_stable_under_snapshot_order() {
    let east = identity("orders", Some("east"));
    let west = identity("orders", Some("west"));
    let base_position = position("cursor", 1, BASE);
    let left_position = position("cursor", 1, LEFT);
    let right_position = position("cursor", 1, RIGHT);

    let base = snapshot([
        (west.clone(), base_position.clone()),
        (east.clone(), base_position.clone()),
    ]);
    let left = snapshot([
        (west.clone(), left_position.clone()),
        (east.clone(), left_position.clone()),
    ]);
    let right = snapshot([
        (east.clone(), right_position.clone()),
        (west.clone(), right_position.clone()),
    ]);
    let conflicts = merge_checkpoint_snapshots(&base, &left, &right, &refs()).unwrap_err();
    assert_eq!(conflicts.len(), 2);
    assert_eq!(conflicts[0].identity, east);
    assert_eq!(conflicts[1].identity, west);

    // Reversing insertion order in all portable snapshots preserves the
    // bounded, identity-sorted diagnostic sequence.
    let reordered_base = snapshot([
        (east.clone(), base_position.clone()),
        (west.clone(), base_position.clone()),
    ]);
    let reordered_left = snapshot([
        (east.clone(), left_position.clone()),
        (west.clone(), left_position.clone()),
    ]);
    let reordered_right = snapshot([
        (west.clone(), right_position.clone()),
        (east.clone(), right_position.clone()),
    ]);
    let reordered = merge_checkpoint_snapshots(
        &reordered_base,
        &reordered_left,
        &reordered_right,
        &refs(),
    )
    .unwrap_err();
    assert_eq!(reordered, conflicts);

    // When a deleted checkpoint competes with a changed-format cursor, report
    // the higher-level delete/update conflict whichever branch carries it.
    let changed_format = position("cursor-v2", 2, RIGHT);
    let deletion = snapshot([]);
    let edited = snapshot([(east.clone(), changed_format.clone())]);
    let base_one = snapshot([(east.clone(), base_position)]);
    let delete_left = merge_checkpoint_snapshots(&base_one, &deletion, &edited, &refs())
        .unwrap_err();
    let delete_right = merge_checkpoint_snapshots(&base_one, &edited, &deletion, &refs())
        .unwrap_err();
    assert_eq!(delete_left[0].reason, CheckpointConflictReason::DeleteUpdate);
    assert_eq!(delete_right[0].reason, CheckpointConflictReason::DeleteUpdate);
    assert_eq!(delete_left[0].left, None);
    assert_eq!(delete_left[0].right, Some(changed_format.clone()));
    assert_eq!(delete_right[0].left, Some(changed_format));
    assert_eq!(delete_right[0].right, None);
}
