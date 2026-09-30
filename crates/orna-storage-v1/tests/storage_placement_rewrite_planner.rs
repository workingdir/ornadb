use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_repository_v1::{CompactManifest, Uuid};
use orna_storage_v1::{
    plan_compact_consolidation, plan_compact_scan, plan_storage_placement,
    plan_storage_rewrite, CompactKeyRange, CompactOvbProfile,
    EditableBaseRow, HybridBaseState, PhysicalPlacement, PlacementAction, PlacementCandidate,
    StoragePlacementError, StoragePreference, StorageProfile, StorageRewriteError,
    StorageRewriteTarget, AUTOMATIC_EDITABLE_MAX_PUBLICATION_BYTES,
};
use orna_syntax_v1::{Expr, LiteralKind, parse_row};

const EDITABLE_ROW: &str = include_str!("fixtures/storage-placement-row.orna");
const CASE_COLLISION_UPPER_ROW: &str = include_str!("fixtures/storage-placement-case-upper.orna");
const CASE_COLLISION_LOWER_ROW: &str = include_str!("fixtures/storage-placement-case-lower.orna");
const REWRITE_TAIL_FIRST: &str = include_str!("fixtures/storage-rewrite-tail-first.orna");
const REWRITE_TAIL_LAST: &str = include_str!("fixtures/storage-rewrite-tail-last.orna");
const KEY_FIELD: Uuid = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);

fn profile() -> CompactOvbProfile {
    profile_for_table(Uuid::from_u128(1))
}

fn profile_for_table(table: Uuid) -> CompactOvbProfile {
    profile_for_key_type(table, "Int")
}

fn profile_for_key_type(table: Uuid, key_type: &str) -> CompactOvbProfile {
    CompactOvbProfile::new(
        SchemaDescriptor::new(OvbRaw::Map(vec![
            (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
            (
                OvbRaw::Int(1.into()),
                OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(table.as_bytes().to_vec()))),
            ),
            (
                OvbRaw::Int(2.into()),
                OvbRaw::Array(vec![OvbRaw::Tag(
                    37,
                    Box::new(OvbRaw::Bytes(KEY_FIELD.as_bytes().to_vec())),
                )]),
            ),
            (
                OvbRaw::Int(3.into()),
                OvbRaw::Array(vec![OvbRaw::Array(vec![
                    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(KEY_FIELD.as_bytes().to_vec()))),
                    OvbRaw::Text(format!("f_{}", KEY_FIELD.simple())),
                    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text(key_type.into())]),
                    OvbRaw::Int(0.into()),
                    OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
                ])]),
            ),
            (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
        ]))
        .unwrap(),
    )
    .unwrap()
}

fn key(value: i64) -> Vec<u8> {
    CanonicalValue::new(OvbRaw::Int(value.into()))
        .unwrap()
        .encode()
        .unwrap()
}

fn rewrite_tail_fixture_key(source: &str) -> i64 {
    let parsed = parse_row(source);
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    let Expr::Record { fields, .. } = parsed.value else {
        panic!("rewrite tail fixture is a row record")
    };
    let [field] = fields.as_slice() else {
        panic!("rewrite tail fixture has one key field")
    };
    assert_eq!(field.name, "id");
    let Expr::Literal {
        text,
        kind: LiteralKind::Integer,
        ..
    } = &field.value
    else {
        panic!("rewrite tail fixture key is an integer")
    };
    text.parse().unwrap()
}

fn row(value: i64) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Tag(
        60009,
        Box::new(OvbRaw::Array(vec![
            OvbRaw::Null,
            OvbRaw::Array(vec![OvbRaw::Array(vec![
                OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(KEY_FIELD.as_bytes().to_vec()))),
                OvbRaw::Int(value.into()),
            ])]),
        ])),
    ))
    .unwrap()
}

fn editable_path(key: &str) -> orna_storage_v1::LoosePath {
    orna_storage_v1::LoosePath::for_key("Placement", &[key.to_owned()]).unwrap()
}

fn text_key(value: &str) -> Vec<u8> {
    CanonicalValue::new(OvbRaw::Text(value.to_owned()))
        .unwrap()
        .encode()
        .unwrap()
}

#[test]
fn placement_preserves_existing_rows_and_applies_automatic_policy_to_inserts() {
    let profile = profile();
    let body = EDITABLE_ROW.len();
    let plan = plan_storage_placement(
        &profile,
        StoragePreference::Automatic,
        4,
        true,
        [
            PlacementCandidate::update(
                key(1),
                body,
                PhysicalPlacement::Editable,
                Some(editable_path("1")),
            ),
            PlacementCandidate::update(
                key(2),
                body,
                PhysicalPlacement::Compact,
                None,
            ),
            PlacementCandidate::insert(key(3), body, Some(editable_path("3"))),
            PlacementCandidate::delete(key(4), PhysicalPlacement::Compact, None),
            PlacementCandidate::delete(
                key(5),
                PhysicalPlacement::Editable,
                Some(editable_path("5")),
            ),
        ],
    )
    .unwrap();

    assert_eq!(plan.resulting_row_count(), 3);
    assert_eq!(plan.new_row_placement(), PhysicalPlacement::Compact);
    assert_eq!(plan.decisions()[0].action(), PlacementAction::Update);
    assert_eq!(plan.decisions()[0].placement(), PhysicalPlacement::Editable);
    assert_eq!(plan.decisions()[1].placement(), PhysicalPlacement::Compact);
    assert_eq!(plan.decisions()[2].placement(), PhysicalPlacement::Compact);
    assert_eq!(plan.decisions()[3].action(), PlacementAction::Delete);
    assert_eq!(plan.decisions()[3].placement(), PhysicalPlacement::Compact);
    assert_eq!(plan.decisions()[4].action(), PlacementAction::Delete);
    assert_eq!(plan.decisions()[4].placement(), PhysicalPlacement::Editable);
}

#[test]
fn automatic_thresholds_and_explicit_editable_path_errors_are_checked_before_mutation() {
    let profile = profile();
    let body = EDITABLE_ROW.len();
    let threshold = plan_storage_placement(
        &profile,
        StoragePreference::Automatic,
        10_000,
        false,
        [PlacementCandidate::insert(
            key(1),
            body.clone(),
            Some(editable_path("1")),
        )],
    )
    .unwrap();
    assert_eq!(threshold.new_row_placement(), PhysicalPlacement::Compact);

    let too_large = plan_storage_placement(
        &profile,
        StoragePreference::Automatic,
        0,
        false,
        [PlacementCandidate::insert(
            key(2),
            AUTOMATIC_EDITABLE_MAX_PUBLICATION_BYTES + 1,
            Some(editable_path("2")),
        )],
    )
    .unwrap();
    assert_eq!(too_large.new_row_placement(), PhysicalPlacement::Compact);

    assert_eq!(
        plan_storage_placement(
            &profile,
            StoragePreference::Editable,
            0,
            false,
            [PlacementCandidate::insert(key(3), body, None)],
        ),
        Err(StoragePlacementError::UnrepresentableEditableKey)
    );
}

#[test]
fn automatic_placement_keeps_inclusive_row_and_byte_limits() {
    let profile = profile();
    let at_row_limit = plan_storage_placement(
        &profile,
        StoragePreference::Automatic,
        9_999,
        false,
        [PlacementCandidate::insert(
            key(11),
            EDITABLE_ROW.len(),
            Some(editable_path("11")),
        )],
    )
    .unwrap();
    assert_eq!(at_row_limit.resulting_row_count(), 10_000);
    assert_eq!(at_row_limit.new_row_placement(), PhysicalPlacement::Editable);

    let at_byte_limit = plan_storage_placement(
        &profile,
        StoragePreference::Automatic,
        0,
        false,
        [PlacementCandidate::insert(
            key(12),
            AUTOMATIC_EDITABLE_MAX_PUBLICATION_BYTES,
            Some(editable_path("12")),
        )],
    )
    .unwrap();
    assert_eq!(at_byte_limit.new_row_placement(), PhysicalPlacement::Editable);
}

#[test]
fn placement_rejects_case_only_portable_path_aliases_from_row_fixtures() {
    for fixture in [CASE_COLLISION_UPPER_ROW, CASE_COLLISION_LOWER_ROW] {
        let parsed = parse_row(fixture);
        assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    }

    let profile = profile_for_key_type(Uuid::from_u128(3), "Str");
    assert_eq!(
        plan_storage_placement(
            &profile,
            StoragePreference::Automatic,
            0,
            false,
            [
                PlacementCandidate::insert(
                    text_key("Alice"),
                    CASE_COLLISION_UPPER_ROW.len(),
                    Some(editable_path("Alice")),
                ),
                PlacementCandidate::insert(
                    text_key("alice"),
                    CASE_COLLISION_LOWER_ROW.len(),
                    Some(editable_path("alice")),
                ),
            ],
        ),
        Err(StoragePlacementError::PathCollision)
    );
}

#[test]
fn rewrite_preview_is_generation_bound_and_proves_an_empty_logical_diff() {
    let profile = profile();
    let compact = orna_storage_v1::fold_compact_committed_base(&profile, [], 1).unwrap();
    let hybrid = HybridBaseState::new(
        &profile,
        compact,
        [EditableBaseRow {
            key: CanonicalValue::decode(&key(17)).unwrap(),
            value: row(17),
        }],
    )
    .unwrap();
    let plan = plan_storage_rewrite(
        &profile,
        &hybrid,
        StorageRewriteTarget::Compact,
        |_| panic!("compact target does not need loose paths"),
        |_, _| panic!("compact target does not need loose row encoding"),
        |_| panic!("compact target does not need loose row decoding"),
    )
    .unwrap();

    assert_eq!(plan.from(), StorageProfile::Editable);
    assert_eq!(plan.generation(), 1);
    assert_eq!(plan.previous_generation(), 0);
    assert_eq!(plan.rows().len(), 1);
    assert_eq!(plan.rows()[0].source(), PhysicalPlacement::Editable);
    assert_eq!(plan.rows()[0].destination(), PhysicalPlacement::Compact);
    let candidate = plan
        .rows()
        .iter()
        .map(|row| (row.key().to_vec(), row.canonical_value().to_vec()));
    let verified = plan.verify_candidate(&profile, candidate).unwrap();
    assert_eq!(verified.rows(), 1);
    assert_eq!(verified.semantic_diff_entries(), 0);
    assert_eq!(verified.logical_digest(), plan.logical_digest());
    assert_eq!(
        plan.verify_candidate(&profile, [(key(17), row(18).encode().unwrap())]),
        Err(StorageRewriteError::CandidateMismatch)
    );
}

#[test]
fn rewrite_source_fence_preserves_the_full_editable_tail_at_repeated_generations() {
    let profile = profile();
    let compact = orna_storage_v1::fold_compact_committed_base(&profile, [], 1).unwrap();
    let hybrid = HybridBaseState::new(
        &profile,
        compact,
        [REWRITE_TAIL_FIRST, REWRITE_TAIL_LAST].map(|source| {
            let value = rewrite_tail_fixture_key(source);
            EditableBaseRow {
                key: CanonicalValue::decode(&key(value)).unwrap(),
                value: row(value),
            }
        }),
    )
    .unwrap();
    let plan = plan_storage_rewrite(
        &profile,
        &hybrid,
        StorageRewriteTarget::Compact,
        |_| panic!("compact target does not need loose paths"),
        |_, _| panic!("compact target does not need loose row encoding"),
        |_| panic!("compact target does not need loose row decoding"),
    )
    .unwrap();
    let source_rows: Vec<_> = plan
        .rows()
        .iter()
        .map(|rewrite_row| {
            (
                rewrite_row.key().to_vec(),
                rewrite_row.canonical_value().to_vec(),
            )
        })
        .collect();
    let fixture_rows = [REWRITE_TAIL_FIRST, REWRITE_TAIL_LAST]
        .map(|source| rewrite_tail_fixture_key(source))
        .map(|value| (key(value), row(value).encode().unwrap()));

    assert_eq!(plan.previous_generation(), 0);
    assert_eq!(plan.generation(), 1);
    assert_eq!(source_rows.len(), 2);
    assert_eq!(source_rows, fixture_rows);
    assert_eq!(
        plan.verify_source_snapshot(&profile, 0, source_rows.clone()),
        Ok(())
    );
    assert_eq!(
        plan.verify_source_snapshot(&profile, 0, [source_rows[1].clone()]),
        Err(StorageRewriteError::StaleInput)
    );
    assert_eq!(
        plan.verify_source_snapshot(&profile, 1, source_rows.clone()),
        Err(StorageRewriteError::StaleInput)
    );
    assert_eq!(
        plan.verify_candidate(
            &profile_for_table(Uuid::from_u128(2)),
            source_rows,
        ),
        Err(StorageRewriteError::WrongProfile)
    );
}

#[test]
fn compact_scan_hook_adds_key_projection_and_rejects_invalid_ranges() {
    let profile = profile();
    let manifest = CompactManifest::empty(
        Uuid::from_bytes(profile.table_id()),
        profile.schema_fingerprint(),
    );
    let range = CompactKeyRange::new(&profile, Some((key(2), true)), Some((key(8), false)))
        .unwrap();
    let plan = plan_compact_scan(&profile, &manifest, range, [[0x55; 16]]).unwrap();
    assert!(plan.selected_segments().is_empty());
    assert_eq!(plan.pruned_segments().len(), 0);
    assert!(plan.projected_field_ids().contains(&[0x55; 16]));
    assert!(plan.projected_field_ids().contains(KEY_FIELD.as_bytes()));
    assert_eq!(plan.next_generation(), 1);
    assert!(!plan.may_match_primary_key_scope(&profile, Some(&key(0)), Some(&key(1))));
    assert!(plan.may_match_primary_key_scope(&profile, Some(&key(2)), Some(&key(3))));
    assert!(plan.may_match_primary_key_scope(&profile, None, Some(&key(3))));
    assert!(plan.may_match_primary_key_scope(&profile, Some(&[0xff]), Some(&key(3))));

    assert!(CompactKeyRange::new(&profile, Some((key(9), true)), Some((key(1), true))).is_err());
}

#[test]
fn consolidation_refuses_a_routine_data_only_rewrite() {
    let profile = profile();
    let manifest = CompactManifest::empty(
        Uuid::from_bytes(profile.table_id()),
        profile.schema_fingerprint(),
    );
    let base = orna_storage_v1::fold_compact_committed_base(&profile, [], 1).unwrap();
    assert_eq!(
        plan_compact_consolidation(&profile, &manifest, &base, 0),
        Err(orna_storage_v1::CompactConsolidationError::NotMateriallyOverlaid)
    );
    assert!(
        plan_compact_consolidation(&profile, &manifest, &base, 100)
            .unwrap_err()
            == orna_storage_v1::CompactConsolidationError::NotMateriallyOverlaid
    );
}
