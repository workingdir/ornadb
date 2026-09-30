use orna_foundation_v1::{CanonicalValue, OvbRaw, SchemaDescriptor};
use orna_repository_v1::{CompactManifest, Uuid};
use orna_storage_v1::{
    plan_compact_consolidation, plan_compact_scan, plan_storage_placement,
    plan_storage_rewrite, CompactKeyRange, CompactOvbProfile,
    EditableBaseRow, HybridBaseState, PhysicalPlacement, PlacementAction, PlacementCandidate,
    StoragePlacementError, StoragePreference, StorageProfile, StorageRewriteError,
    StorageRewriteTarget, PlacementReason, FrozenBatch, LooseMutation, LooseProjection, LooseRow,
    MutationId, AUTOMATIC_EDITABLE_MAX_PUBLICATION_BYTES,
};
use orna_syntax_v1::{Expr, LiteralKind, parse_row};

const EDITABLE_ROW: &str = include_str!("fixtures/storage-placement-row.orna");
const CASE_COLLISION_UPPER_ROW: &str = include_str!("fixtures/storage-placement-case-upper.orna");
const CASE_COLLISION_LOWER_ROW: &str = include_str!("fixtures/storage-placement-case-lower.orna");
const PATH_AT_LIMIT_ROW: &str = include_str!("fixtures/storage-placement-path-at-limit.orna");
const PATH_OVER_LIMIT_ROW: &str = include_str!("fixtures/storage-placement-path-over-limit.orna");
const PATH_ORDINARY_ROW: &str = include_str!("fixtures/storage-placement-path-ordinary.orna");
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

fn profile_for_str_key_arity(table: Uuid, arity: usize) -> CompactOvbProfile {
    let field_ids = (0..arity)
        .map(|index| Uuid::from_u128(100 + index as u128))
        .collect::<Vec<_>>();
    CompactOvbProfile::new(
        SchemaDescriptor::new(OvbRaw::Map(vec![
            (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
            (
                OvbRaw::Int(1.into()),
                OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(table.as_bytes().to_vec()))),
            ),
            (
                OvbRaw::Int(2.into()),
                OvbRaw::Array(
                    field_ids
                        .iter()
                        .map(|id| {
                            OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(id.as_bytes().to_vec())))
                        })
                        .collect(),
                ),
            ),
            (
                OvbRaw::Int(3.into()),
                OvbRaw::Array(
                    field_ids
                        .iter()
                        .enumerate()
                        .map(|(index, id)| {
                            OvbRaw::Array(vec![
                                OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(id.as_bytes().to_vec()))),
                                OvbRaw::Text(format!("key_{index}")),
                                OvbRaw::Array(vec![
                                    OvbRaw::Int(0.into()),
                                    OvbRaw::Text("Str".into()),
                                ]),
                                OvbRaw::Int(0.into()),
                                OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
                            ])
                        })
                        .collect(),
                ),
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

fn text_key_components_from_fixture(source: &str) -> Vec<String> {
    let parsed = parse_row(source);
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    let Expr::Record { fields, .. } = parsed.value else {
        panic!("path boundary fixture is a row record")
    };
    fields
        .into_iter()
        .enumerate()
        .map(|(index, field)| {
            assert_eq!(field.name, format!("key_{index}"));
            let Expr::Literal {
                text,
                kind: LiteralKind::String,
                ..
            } = field.value
            else {
                panic!("path boundary fixture key is a string")
            };
            text.strip_prefix('"')
                .unwrap()
                .strip_suffix('"')
                .unwrap()
                .to_owned()
        })
        .collect()
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

fn editable_path_components(
    keys: &[String],
) -> Result<orna_storage_v1::LoosePath, orna_storage_v1::Error> {
    orna_storage_v1::LoosePath::for_key("Placement", keys)
}

fn existing_numeric_paths(count: usize) -> Vec<orna_storage_v1::LoosePath> {
    (0..count)
        .map(|index| editable_path(&(20_000 + index).to_string()))
        .collect()
}

fn text_key(value: &str) -> Vec<u8> {
    CanonicalValue::new(OvbRaw::Text(value.to_owned()))
        .unwrap()
        .encode()
        .unwrap()
}

fn composite_text_key(values: &[String]) -> Vec<u8> {
    CanonicalValue::new(OvbRaw::Tag(
        60015,
        Box::new(OvbRaw::Array(
            values.iter().cloned().map(OvbRaw::Text).collect(),
        )),
    ))
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
        vec![editable_path("1"), editable_path("5")],
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
        existing_numeric_paths(10_000),
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
        Vec::new(),
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
            Vec::new(),
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
        existing_numeric_paths(9_999),
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
        Vec::new(),
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
fn automatic_placement_falls_back_for_case_only_aliases_and_keeps_existing_rows() {
    for fixture in [CASE_COLLISION_UPPER_ROW, CASE_COLLISION_LOWER_ROW] {
        let parsed = parse_row(fixture);
        assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    }

    let profile = profile_for_key_type(Uuid::from_u128(3), "Str");
    let upper_key = text_key("Alice");
    let lower_key = text_key("alice");
    let plan = plan_storage_placement(
        &profile,
        StoragePreference::Automatic,
        1,
        false,
        vec![editable_path("Alice")],
        [
            PlacementCandidate::update(
                upper_key.clone(),
                CASE_COLLISION_UPPER_ROW.len(),
                PhysicalPlacement::Editable,
                Some(editable_path("Alice")),
            ),
            PlacementCandidate::insert(
                lower_key.clone(),
                CASE_COLLISION_LOWER_ROW.len(),
                Some(editable_path("alice")),
            ),
        ],
    )
    .unwrap();

    assert_eq!(plan.new_row_placement(), PhysicalPlacement::Compact);
    assert_eq!(plan.reason(), PlacementReason::AutomaticUnrepresentablePath);
    let existing = plan
        .decisions()
        .iter()
        .find(|decision| decision.key().encoded() == upper_key)
        .unwrap();
    let inserted = plan
        .decisions()
        .iter()
        .find(|decision| decision.key().encoded() == lower_key)
        .unwrap();
    assert_eq!(existing.placement(), PhysicalPlacement::Editable);
    assert!(existing.editable_path().is_some());
    assert_eq!(inserted.placement(), PhysicalPlacement::Compact);
    assert!(inserted.editable_path().is_none());

    assert_eq!(
        plan_storage_placement(
            &profile,
            StoragePreference::Editable,
            0,
            false,
            Vec::new(),
            [
                PlacementCandidate::insert(
                    upper_key,
                    CASE_COLLISION_UPPER_ROW.len(),
                    Some(editable_path("Alice")),
                ),
                PlacementCandidate::insert(
                    lower_key,
                    CASE_COLLISION_LOWER_ROW.len(),
                    Some(editable_path("alice")),
                ),
            ],
        ),
        Err(StoragePlacementError::PathCollision)
    );
}

#[test]
fn automatic_placement_compacts_every_new_row_when_insert_paths_alias() {
    let upper = parse_row(CASE_COLLISION_UPPER_ROW);
    let lower = parse_row(CASE_COLLISION_LOWER_ROW);
    assert!(upper.diagnostics.is_empty(), "{:#?}", upper.diagnostics);
    assert!(lower.diagnostics.is_empty(), "{:#?}", lower.diagnostics);

    let profile = profile_for_key_type(Uuid::from_u128(4), "Str");
    let plan = plan_storage_placement(
        &profile,
        StoragePreference::Automatic,
        0,
        false,
        Vec::new(),
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
    )
    .unwrap();

    assert_eq!(plan.reason(), PlacementReason::AutomaticUnrepresentablePath);
    assert!(plan
        .decisions()
        .iter()
        .all(|decision| decision.placement() == PhysicalPlacement::Compact));
    assert!(plan
        .decisions()
        .iter()
        .all(|decision| decision.editable_path().is_none()));
}

#[test]
fn automatic_placement_falls_back_for_an_alias_with_an_untouched_editable_row() {
    let profile = profile_for_key_type(Uuid::from_u128(5), "Str");
    let plan = plan_storage_placement(
        &profile,
        StoragePreference::Automatic,
        1,
        false,
        vec![editable_path("Alice")],
        [PlacementCandidate::insert(
            text_key("alice"),
            CASE_COLLISION_LOWER_ROW.len(),
            Some(editable_path("alice")),
        )],
    )
    .unwrap();

    assert_eq!(plan.new_row_placement(), PhysicalPlacement::Compact);
    assert_eq!(plan.reason(), PlacementReason::AutomaticUnrepresentablePath);
    assert_eq!(plan.decisions()[0].placement(), PhysicalPlacement::Compact);
}

#[test]
fn portable_path_component_and_relative_path_limits_are_inclusive() {
    assert!(editable_path_components(&["x".repeat(200)]).is_ok());
    assert!(editable_path_components(&["x".repeat(201)]).is_err());

    let profile = profile_for_key_type(Uuid::from_u128(8), "Str");
    let body_bytes = PATH_AT_LIMIT_ROW.len();
    let at_component_limit = plan_storage_placement(
        &profile,
        StoragePreference::Automatic,
        0,
        false,
        Vec::new(),
        [PlacementCandidate::insert(
            text_key(&"x".repeat(200)),
            body_bytes,
            Some(editable_path_components(&["x".repeat(200)]).unwrap()),
        )],
    )
    .unwrap();
    assert_eq!(
        at_component_limit.new_row_placement(),
        PhysicalPlacement::Editable
    );

    // Five separators plus the final `.orna` suffix leave 1,014 bytes for
    // six encoded components at the exact table-relative path limit.
    let at_path_limit = text_key_components_from_fixture(PATH_AT_LIMIT_ROW);
    let over_path_limit = text_key_components_from_fixture(PATH_OVER_LIMIT_ROW);
    let ordinary_components = text_key_components_from_fixture(PATH_ORDINARY_ROW);
    assert_eq!(
        at_path_limit.iter().map(String::len).collect::<Vec<_>>(),
        [200, 200, 200, 200, 200, 14]
    );
    assert!(editable_path_components(&at_path_limit).is_ok());
    assert_eq!(
        over_path_limit.iter().map(String::len).collect::<Vec<_>>(),
        [200, 200, 200, 200, 200, 15]
    );
    assert!(editable_path_components(&over_path_limit).is_err());

    let composite_profile = profile_for_str_key_arity(Uuid::from_u128(9), 6);
    let exact_path = editable_path_components(&at_path_limit).unwrap();
    let exact_path_plan = plan_storage_placement(
        &composite_profile,
        StoragePreference::Automatic,
        0,
        false,
        Vec::new(),
        [PlacementCandidate::insert(
            composite_text_key(&at_path_limit),
            body_bytes,
            Some(exact_path.clone()),
        )],
    )
    .unwrap();
    assert_eq!(exact_path_plan.new_row_placement(), PhysicalPlacement::Editable);

    let over_limit_plan = plan_storage_placement(
        &composite_profile,
        StoragePreference::Automatic,
        1,
        false,
        vec![exact_path.clone()],
        [
            PlacementCandidate::update(
                composite_text_key(&at_path_limit),
                body_bytes,
                PhysicalPlacement::Editable,
                Some(exact_path.clone()),
            ),
            PlacementCandidate::insert(
                composite_text_key(&over_path_limit),
                PATH_OVER_LIMIT_ROW.len(),
                None,
            ),
            PlacementCandidate::insert(
                composite_text_key(&ordinary_components),
                PATH_ORDINARY_ROW.len(),
                Some(editable_path_components(&ordinary_components).unwrap()),
            ),
        ],
    )
    .unwrap();
    assert_eq!(over_limit_plan.new_row_placement(), PhysicalPlacement::Compact);
    let retained_boundary_row = over_limit_plan
        .decisions()
        .iter()
        .find(|decision| decision.key().encoded() == composite_text_key(&at_path_limit))
        .unwrap();
    assert_eq!(retained_boundary_row.action(), PlacementAction::Update);
    assert_eq!(retained_boundary_row.placement(), PhysicalPlacement::Editable);
    assert_eq!(retained_boundary_row.editable_path(), Some(&exact_path));
    assert!(over_limit_plan
        .decisions()
        .iter()
        .filter(|decision| decision.action() == PlacementAction::Insert)
        .all(|decision| decision.placement() == PhysicalPlacement::Compact));
}

#[test]
fn one_unrepresentable_automatic_insert_closes_the_new_row_batch() {
    let profile = profile_for_key_type(Uuid::from_u128(7), "Str");
    let existing_key = text_key("Alice");
    let too_long_key = text_key(&"x".repeat(201));
    let ordinary_key = text_key("safe");
    let existing_path = editable_path("Alice");
    let plan = plan_storage_placement(
        &profile,
        StoragePreference::Automatic,
        1,
        false,
        vec![existing_path.clone()],
        [
            PlacementCandidate::update(
                existing_key.clone(),
                CASE_COLLISION_UPPER_ROW.len(),
                PhysicalPlacement::Editable,
                Some(existing_path.clone()),
            ),
            PlacementCandidate::insert(
                too_long_key.clone(),
                CASE_COLLISION_LOWER_ROW.len(),
                None,
            ),
            PlacementCandidate::insert(
                ordinary_key.clone(),
                CASE_COLLISION_LOWER_ROW.len(),
                Some(editable_path("safe")),
            ),
        ],
    )
    .unwrap();

    assert_eq!(plan.new_row_placement(), PhysicalPlacement::Compact);
    assert_eq!(plan.reason(), PlacementReason::AutomaticUnrepresentablePath);
    let updated = plan
        .decisions()
        .iter()
        .find(|decision| decision.key().encoded() == existing_key)
        .unwrap();
    assert_eq!(updated.placement(), PhysicalPlacement::Editable);
    assert_eq!(updated.editable_path(), Some(&existing_path));
    for inserted_key in [&too_long_key, &ordinary_key] {
        let inserted = plan
            .decisions()
            .iter()
            .find(|decision| decision.key().encoded() == *inserted_key)
            .unwrap();
        assert_eq!(inserted.action(), PlacementAction::Insert);
        assert_eq!(inserted.placement(), PhysicalPlacement::Compact);
        assert!(inserted.editable_path().is_none());
    }

    assert_eq!(
        plan_storage_placement(
            &profile,
            StoragePreference::Editable,
            0,
            false,
            Vec::new(),
            [PlacementCandidate::insert(
                too_long_key.clone(),
                CASE_COLLISION_LOWER_ROW.len(),
                None,
            )],
        ),
        Err(StoragePlacementError::UnrepresentableEditableKey)
    );
    let explicit_compact = plan_storage_placement(
        &profile,
        StoragePreference::Compact,
        0,
        false,
        Vec::new(),
        [PlacementCandidate::insert(
            too_long_key,
            CASE_COLLISION_LOWER_ROW.len(),
            None,
        )],
    )
    .unwrap();
    assert_eq!(
        explicit_compact.new_row_placement(),
        PhysicalPlacement::Compact
    );

    // The row fixture is the preexisting editable value: fallback only
    // changes placement for the new rows, so its portable path stays valid.
    let parsed = parse_row(CASE_COLLISION_UPPER_ROW);
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    assert!(LooseRow::new(CASE_COLLISION_UPPER_ROW.as_bytes().to_vec()).is_ok());
}

#[test]
fn deleting_a_case_alias_source_frees_the_path_for_an_editable_reinsert() {
    let profile = profile_for_key_type(Uuid::from_u128(6), "Str");
    let upper_key = text_key("Alice");
    let lower_key = text_key("alice");
    let upper_path = editable_path("Alice");
    let lower_path = editable_path("alice");
    let plan = plan_storage_placement(
        &profile,
        StoragePreference::Automatic,
        1,
        false,
        vec![upper_path.clone()],
        [
            PlacementCandidate::delete(
                upper_key,
                PhysicalPlacement::Editable,
                Some(upper_path.clone()),
            ),
            PlacementCandidate::insert(
                lower_key,
                CASE_COLLISION_LOWER_ROW.len(),
                Some(lower_path.clone()),
            ),
        ],
    )
    .unwrap();

    assert_eq!(plan.new_row_placement(), PhysicalPlacement::Editable);
    assert_eq!(plan.decisions()[0].action(), PlacementAction::Delete);
    assert_eq!(plan.decisions()[0].placement(), PhysicalPlacement::Editable);
    assert_eq!(plan.decisions()[1].action(), PlacementAction::Insert);
    assert_eq!(plan.decisions()[1].placement(), PhysicalPlacement::Editable);

    let old_row = LooseRow::new(CASE_COLLISION_UPPER_ROW.as_bytes().to_vec()).unwrap();
    let mut initial = LooseProjection::default();
    initial
        .project(
            &FrozenBatch::new(
                MutationId::new("initial-case-row").unwrap(),
                vec![LooseMutation {
                    id: MutationId::new("initial-case-row-write").unwrap(),
                    path: upper_path.clone(),
                    expected: None,
                    next: Some(old_row.clone()),
                }],
                1,
            )
            .unwrap(),
        )
        .unwrap();
    let delete = LooseMutation {
        id: MutationId::new("delete-upper-case-row").unwrap(),
        path: upper_path.clone(),
        expected: Some(old_row.hash()),
        next: None,
    };
    let insert = LooseMutation {
        id: MutationId::new("insert-lower-case-row").unwrap(),
        path: lower_path.clone(),
        expected: None,
        next: Some(LooseRow::new(CASE_COLLISION_LOWER_ROW.as_bytes().to_vec()).unwrap()),
    };

    for mutations in [
        vec![delete.clone(), insert.clone()],
        vec![insert.clone(), delete.clone()],
    ] {
        let mut projection = initial.clone();
        projection
            .project(
                &FrozenBatch::new(
                    MutationId::new("case-row-replacement").unwrap(),
                    mutations,
                    2,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(projection.entries().count(), 1);
        assert!(projection.row(&lower_path).is_some());
        assert!(projection.row(&upper_path).is_none());
    }
}

#[test]
fn loose_projection_rejects_fixture_aliases_without_partial_application() {
    let upper = parse_row(CASE_COLLISION_UPPER_ROW);
    let lower = parse_row(CASE_COLLISION_LOWER_ROW);
    assert!(upper.diagnostics.is_empty(), "{:#?}", upper.diagnostics);
    assert!(lower.diagnostics.is_empty(), "{:#?}", lower.diagnostics);

    let mutation = |mutation_id: &str, key: &str, body: &str| LooseMutation {
        id: MutationId::new(mutation_id).unwrap(),
        path: orna_storage_v1::LoosePath::for_key("Placement", &[key.to_owned()]).unwrap(),
        expected: None,
        next: Some(LooseRow::new(body.as_bytes().to_vec()).unwrap()),
    };
    let batch = FrozenBatch::new(
        MutationId::new("case-alias-batch").unwrap(),
        vec![
            mutation("upper-row", "Alice", CASE_COLLISION_UPPER_ROW),
            mutation("lower-row", "alice", CASE_COLLISION_LOWER_ROW),
        ],
        1,
    )
    .unwrap();
    let mut projection = LooseProjection::default();

    assert_eq!(
        projection.project(&batch),
        Err(orna_storage_v1::Error::PathCollision)
    );
    assert_eq!(projection.entries().count(), 0);
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
