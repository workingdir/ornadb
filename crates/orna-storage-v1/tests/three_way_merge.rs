use orna_evolution_v1::{
    CanonicalValue, CheckpointGeneration, Field, FieldRole, FieldType, KeyedRow, ObjectId,
    RowKeyKind, Schema, Table,
};
use orna_foundation_v1::OvbRaw;
use orna_storage_v1::{
    BranchMergeBudget, BranchMergeConflict, BranchMergeError, BranchRowSource, KeyRange,
    MergeSide, MergedSegment, RowSegmentManifest, TableManifest, ThreeWaySnapshot,
    merge_three_way_snapshots,
};
use orna_syntax_v1::{Expr, LiteralKind, parse_row};
use std::collections::BTreeMap;

const BASE: &str = include_str!("fixtures/merge-contact-base.orna");
const LEFT: &str = include_str!("fixtures/merge-contact-left.orna");
const RIGHT: &str = include_str!("fixtures/merge-contact-right.orna");
const CONFLICT: &str = include_str!("fixtures/merge-contact-conflict.orna");

fn id(value: u8) -> ObjectId {
    ObjectId::new([value; 16])
}

fn string(value: &str) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Text(value.to_owned())).unwrap()
}

fn integer(value: i64) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Int(value.into())).unwrap()
}

fn schema(explicit_key: bool, name_type: FieldType) -> Schema {
    Schema {
        version: orna_evolution_v1::EvolutionVersion::V1_0,
        tables: vec![Table {
            id: id(1),
            name: "Contact".into(),
            explicit_key,
            fields: vec![
                Field { id: id(4), name: "id".into(), ty: FieldType::Int, role: FieldRole::Key, optional: false, introduction_fallback: None },
                Field { id: id(2), name: "name".into(), ty: name_type, role: FieldRole::Stored, optional: false, introduction_fallback: None },
                Field { id: id(3), name: "city".into(), ty: FieldType::Str, role: FieldRole::Stored, optional: false, introduction_fallback: None },
            ],
        }],
    }
}

fn parse_fixture(source: &str, kind: RowKeyKind) -> KeyedRow {
    let parsed = parse_row(source);
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    let Expr::Record { fields, .. } = parsed.value else { panic!("record fixture") };
    let mut key = None;
    let mut values = BTreeMap::new();
    for field in fields {
        let Expr::Literal { text, kind: literal_kind, .. } = field.value else { panic!("literal fixture") };
        match field.name.as_str() {
            "id" => {
                assert_eq!(literal_kind, LiteralKind::Integer);
                let number: i64 = text.parse().unwrap();
                key = Some(CanonicalValue::new(OvbRaw::Int(number.into())).unwrap());
            }
            "name" | "city" => {
                assert_eq!(literal_kind, LiteralKind::String);
                let value = text.strip_prefix('"').unwrap().strip_suffix('"').unwrap();
                let field_id = if field.name == "name" { id(2) } else { id(3) };
                values.insert(field_id, string(value));
            }
            other => panic!("unexpected fixture field {other}"),
        }
    }
    KeyedRow { table: id(1), key: key.unwrap(), key_kind: kind, fields: values }
}

fn manifest(digest: u8, segment_digest: u8, locator: &[u8]) -> TableManifest {
    TableManifest {
        digest: [digest; 32],
        segments: vec![RowSegmentManifest { locator: locator.to_vec(), range: KeyRange::all(), digest: [segment_digest; 32] }],
    }
}

fn snapshot(schema: Schema, manifest: TableManifest, checkpoint: Option<CheckpointGeneration>) -> ThreeWaySnapshot {
    let mut tables = BTreeMap::new();
    tables.insert(id(1), manifest);
    let mut checkpoints = BTreeMap::new();
    if let Some(checkpoint) = checkpoint { checkpoints.insert(b"consumer/source".to_vec(), checkpoint); }
    ThreeWaySnapshot { schema, tables, checkpoints }
}

#[derive(Default)]
struct FixtureRows {
    rows: BTreeMap<(MergeSide, Vec<u8>), Vec<KeyedRow>>,
    visited: Vec<(MergeSide, Vec<u8>)>,
}

impl FixtureRows {
    fn add(&mut self, side: MergeSide, locator: &[u8], rows: Vec<KeyedRow>) {
        self.rows.insert((side, locator.to_vec()), rows);
    }
}

impl BranchRowSource for FixtureRows {
    fn visit_rows(
        &mut self,
        side: MergeSide,
        table: ObjectId,
        segment: Option<&RowSegmentManifest>,
        range: &KeyRange,
        visitor: &mut dyn FnMut(KeyedRow) -> bool,
    ) -> Result<(), String> {
        let locator = segment.map(|segment| segment.locator.clone()).unwrap_or_default();
        self.visited.push((side, locator.clone()));
        for row in self.rows.get(&(side, locator)).into_iter().flatten() {
            assert_eq!(row.table, table);
            let key = row.key.encode().map_err(|error| error.to_string())?;
            if range.start.as_deref().is_none_or(|start| key.as_slice() >= start)
                && range.end.as_deref().is_none_or(|end| key.as_slice() < end)
                && !visitor(row.clone())
            {
                break;
            }
        }
        Ok(())
    }
}

fn budget() -> BranchMergeBudget {
    BranchMergeBudget { max_rows_examined: 100, max_conflicts: 20 }
}

fn row_checkpoint_conflict_inputs() -> (
    ThreeWaySnapshot,
    ThreeWaySnapshot,
    ThreeWaySnapshot,
    FixtureRows,
) {
    let base_checkpoint = CheckpointGeneration {
        generation: 4,
        position: Some(b"base-token".to_vec()),
    };
    let left_checkpoint = CheckpointGeneration {
        generation: 5,
        position: Some(b"left-token".to_vec()),
    };
    let right_checkpoint = CheckpointGeneration {
        generation: 6,
        position: Some(b"right-token".to_vec()),
    };
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(
        schema(true, FieldType::Str),
        manifest(1, 1, b"base"),
        Some(base_checkpoint),
    );
    let left = snapshot(
        schema(true, FieldType::Str),
        manifest(2, 2, b"left"),
        Some(left_checkpoint),
    );
    let right = snapshot(
        schema(true, FieldType::Str),
        manifest(3, 3, b"right"),
        Some(right_checkpoint),
    );
    (base, left, right, source)
}

fn schema_conflict_inputs() -> (
    ThreeWaySnapshot,
    ThreeWaySnapshot,
    ThreeWaySnapshot,
    FixtureRows,
) {
    let base_schema = schema(true, FieldType::Str);
    let mut left_schema = base_schema.clone();
    left_schema.tables[0].fields[1].ty = FieldType::Int;
    left_schema.tables[0].fields[2].ty = FieldType::Int;
    let mut right_schema = base_schema.clone();
    right_schema.tables[0].fields[1].ty = FieldType::Bool;
    right_schema.tables[0].fields[2].ty = FieldType::Bool;
    let base_checkpoint = CheckpointGeneration {
        generation: 4,
        position: Some(b"base-token".to_vec()),
    };
    let left_checkpoint = CheckpointGeneration {
        generation: 5,
        position: Some(b"left-token".to_vec()),
    };
    let right_checkpoint = CheckpointGeneration {
        generation: 6,
        position: Some(b"right-token".to_vec()),
    };
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(base_schema, manifest(1, 1, b"base"), Some(base_checkpoint));
    let left = snapshot(left_schema, manifest(2, 2, b"left"), Some(left_checkpoint));
    let right = snapshot(right_schema, manifest(3, 3, b"right"), Some(right_checkpoint));
    (base, left, right, source)
}

#[test]
fn independent_edits_to_one_keyed_row_merge_by_field_from_orna_fixtures() {
    let base_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let left_row = parse_fixture(LEFT, RowKeyKind::Explicit);
    let right_row = parse_fixture(RIGHT, RowKeyKind::Explicit);
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![base_row]);
    source.add(MergeSide::Left, b"left", vec![left_row]);
    source.add(MergeSide::Right, b"right", vec![right_row]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

    let plan = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap();
    let MergedSegment::Rows { rows, .. } = &plan.tables[&id(1)].segments[0] else { panic!("changed range is rebuilt") };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].fields[&id(2)], string("Grace"));
    assert_eq!(rows[0].fields[&id(3)], string("Paris"));
}

#[test]
fn same_field_conflict_is_reported_without_returning_a_partial_plan() {
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else { panic!("bounded semantic conflict") };
    assert!(matches!(conflicts.as_slice(), [BranchMergeConflict::Row { .. }]));
    assert_eq!(report.conflicts_lower_bound, 1);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
}

#[test]
fn equal_segment_digest_skips_unchanged_range_before_row_decode() {
    let row = parse_fixture(BASE, RowKeyKind::Explicit);
    let key = row.key.encode().unwrap();
    let low = RowSegmentManifest { locator: b"low".to_vec(), range: KeyRange::new(None, Some(key.clone())).unwrap(), digest: [7; 32] };
    let changed_base = RowSegmentManifest { locator: b"base-high".to_vec(), range: KeyRange::new(Some(key.clone()), None).unwrap(), digest: [1; 32] };
    let changed_left = RowSegmentManifest { locator: b"left-high".to_vec(), digest: [2; 32], ..changed_base.clone() };
    let changed_right = RowSegmentManifest { locator: b"right-high".to_vec(), digest: [3; 32], ..changed_base.clone() };
    let table_manifest = |digest, high| TableManifest { digest: [digest; 32], segments: vec![low.clone(), high] };
    let base = snapshot(schema(true, FieldType::Str), table_manifest(10, changed_base), None);
    let left = snapshot(schema(true, FieldType::Str), table_manifest(11, changed_left), None);
    let right = snapshot(schema(true, FieldType::Str), table_manifest(12, changed_right), None);
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base-high", vec![row.clone()]);
    source.add(MergeSide::Left, b"left-high", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right-high", vec![parse_fixture(RIGHT, RowKeyKind::Explicit)]);

    let plan = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap();
    assert_eq!(source.visited.len(), 3, "the equal low segment must not be decoded");
    assert!(source.visited.iter().all(|(_, locator)| locator.ends_with(b"high")));
    assert!(matches!(plan.tables[&id(1)].segments[0], MergedSegment::Reuse { from: MergeSide::Left, .. }));
}

#[test]
fn conflict_budget_stops_at_lower_bound_and_names_affected_range() {
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);
    let zero_conflicts = BranchMergeBudget { max_rows_examined: 10, max_conflicts: 0 };

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, zero_conflicts).unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else { panic!("must stop at conflict budget") };
    assert_eq!(report.conflicts_lower_bound, 1);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
}

#[test]
fn row_budget_stops_streaming_decode_and_reports_the_active_range() {
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(RIGHT, RowKeyKind::Explicit)]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);
    let no_rows = BranchMergeBudget { max_rows_examined: 0, max_conflicts: 20 };

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, no_rows).unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else { panic!("must stop at row budget") };
    assert_eq!(report.rows_examined, 1, "the reader is stopped at the first row over budget");
    assert_eq!(report.conflicts_lower_bound, 0);
    assert!(report.affected_tables.contains(&id(1)));
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
}

#[test]
fn optional_columns_added_on_separate_branches_merge_and_type_edits_conflict() {
    let mut base_schema = schema(true, FieldType::Str);
    base_schema.tables[0].fields.pop();
    let mut left_schema = base_schema.clone();
    left_schema.tables[0].fields.push(Field { id: id(5), name: "alias".into(), ty: FieldType::Str, role: FieldRole::Stored, optional: true, introduction_fallback: None });
    let mut right_schema = base_schema.clone();
    right_schema.tables[0].fields.push(Field { id: id(6), name: "region".into(), ty: FieldType::Str, role: FieldRole::Stored, optional: true, introduction_fallback: None });
    let base = snapshot(base_schema.clone(), manifest(1, 1, b"base"), None);
    let left = snapshot(left_schema, manifest(1, 1, b"base"), None);
    let right = snapshot(right_schema, manifest(1, 1, b"base"), None);
    let mut source = FixtureRows::default();
    let merged = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap();
    let fields = &merged.schema.tables[0].fields;
    assert!(fields.iter().any(|field| field.id == id(5)));
    assert!(fields.iter().any(|field| field.id == id(6)));

    let mut type_base = schema(true, FieldType::Str);
    type_base.tables[0].fields.pop();
    let mut type_left = type_base.clone();
    type_left.tables[0].fields[1].ty = FieldType::Int;
    let mut type_right = type_base.clone();
    type_right.tables[0].fields[1].ty = FieldType::Bool;
    let base = snapshot(type_base, manifest(1, 1, b"base"), None);
    let left = snapshot(type_left, manifest(1, 1, b"base"), None);
    let right = snapshot(type_right, manifest(1, 1, b"base"), None);
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap_err();
    assert!(matches!(error, BranchMergeError::Conflicts { conflicts, .. } if conflicts.iter().any(|conflict| matches!(conflict, BranchMergeConflict::Schema(_)))));
}

#[test]
fn schema_conflict_budget_boundary_is_exact_and_stops_later_phases() {
    for (max_conflicts, lower_bound) in [(0, 1), (1, 2)] {
        let (base, left, right, mut source) = schema_conflict_inputs();
        let limit = BranchMergeBudget { max_rows_examined: 100, max_conflicts };
        let error = merge_three_way_snapshots(&base, &left, &right, &mut source, limit)
            .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("schema conflicts beyond the configured budget stop planning")
        };
        assert_eq!(report.conflicts_lower_bound, lower_bound);
        assert!(report.affected_tables.contains(&id(1)));
        assert!(report.affected_ranges.is_empty());
        assert!(report.affected_checkpoints.is_empty());
        assert!(source.visited.is_empty());
    }

    let (base, left, right, mut source) = schema_conflict_inputs();
    let exact_limit = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, exact_limit)
        .unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else {
        panic!("schema conflicts exactly at the budget remain typed conflicts")
    };
    assert_eq!(conflicts.len(), 2);
    assert!(conflicts.iter().all(|conflict| matches!(conflict, BranchMergeConflict::Schema(_))));
    assert_eq!(report.conflicts_lower_bound, 2);
    assert!(report.affected_tables.contains(&id(1)));
    assert!(report.affected_ranges.is_empty());
    assert!(report.affected_checkpoints.is_empty());
    assert!(source.visited.is_empty());
}

#[test]
fn truncated_schema_conflicts_report_tables_beyond_the_materialized_prefix() {
    let mut base_schema = schema(true, FieldType::Str);
    let mut second_table = base_schema.tables[0].clone();
    second_table.id = id(5);
    second_table.name = "Company".into();
    base_schema.tables.push(second_table);
    let mut left_schema = base_schema.clone();
    left_schema.tables[0].fields[1].ty = FieldType::Int;
    left_schema.tables[1].fields[1].ty = FieldType::Int;
    let mut right_schema = base_schema.clone();
    right_schema.tables[0].fields[1].ty = FieldType::Bool;
    right_schema.tables[1].fields[1].ty = FieldType::Bool;

    let mut base_row_company = parse_fixture(BASE, RowKeyKind::Explicit);
    base_row_company.table = id(5);
    let mut left_row_company = parse_fixture(LEFT, RowKeyKind::Explicit);
    left_row_company.table = id(5);
    let mut right_row_company = parse_fixture(CONFLICT, RowKeyKind::Explicit);
    right_row_company.table = id(5);
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Base, b"base-company", vec![base_row_company]);
    source.add(MergeSide::Left, b"left-company", vec![left_row_company]);
    source.add(MergeSide::Right, b"right-company", vec![right_row_company]);

    let checkpoint = |generation, token: &[u8]| CheckpointGeneration {
        generation,
        position: Some(token.to_vec()),
    };
    let mut base = snapshot(
        base_schema,
        manifest(1, 1, b"base"),
        Some(checkpoint(4, b"base-token")),
    );
    let mut left = snapshot(
        left_schema,
        manifest(2, 2, b"left"),
        Some(checkpoint(5, b"left-token")),
    );
    let mut right = snapshot(
        right_schema,
        manifest(3, 3, b"right"),
        Some(checkpoint(6, b"right-token")),
    );
    base.tables.insert(id(5), manifest(10, 10, b"base-company"));
    left.tables.insert(id(5), manifest(20, 20, b"left-company"));
    right.tables.insert(id(5), manifest(30, 30, b"right-company"));

    let one_detail_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, one_detail_budget)
        .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the second table conflict exceeds the one-detail schema budget")
    };
    assert_eq!(report.conflicts_lower_bound, 2);
    assert!(report.affected_tables.contains(&id(1)));
    assert!(report.affected_tables.contains(&id(5)));
    assert!(report.affected_ranges.is_empty());
    assert!(report.affected_checkpoints.is_empty());
    assert!(source.visited.is_empty(), "schema conflicts stop before row and checkpoint phases");
}

#[test]
fn row_and_checkpoint_conflicts_accumulate_in_phase_order_without_a_partial_plan() {
    let base_checkpoint = CheckpointGeneration {
        generation: 4,
        position: Some(b"base-token".to_vec()),
    };
    let left_checkpoint = CheckpointGeneration {
        generation: 5,
        position: Some(b"left-token".to_vec()),
    };
    let right_checkpoint = CheckpointGeneration {
        generation: 6,
        position: Some(b"right-token".to_vec()),
    };
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(
        schema(true, FieldType::Str),
        manifest(1, 1, b"base"),
        Some(base_checkpoint.clone()),
    );
    let left = snapshot(
        schema(true, FieldType::Str),
        manifest(2, 2, b"left"),
        Some(left_checkpoint.clone()),
    );
    let right = snapshot(
        schema(true, FieldType::Str),
        manifest(3, 3, b"right"),
        Some(right_checkpoint.clone()),
    );

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget())
        .unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else {
        panic!("row and checkpoint conflicts must reject the complete plan")
    };
    assert_eq!(conflicts.len(), 2);
    assert!(matches!(&conflicts[0], BranchMergeConflict::Row { .. }));
    assert!(matches!(
        &conflicts[1],
        BranchMergeConflict::CheckpointConflict { id: checkpoint_id, conflict }
            if checkpoint_id.as_slice() == b"consumer/source"
                && conflict.base.as_ref() == Some(&base_checkpoint)
                && conflict.left.as_ref() == Some(&left_checkpoint)
                && conflict.right.as_ref() == Some(&right_checkpoint)
    ));
    assert_eq!(report.conflicts_lower_bound, 2);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_checkpoints.contains(b"consumer/source".as_slice()));
    assert_eq!(source.visited.len(), 3);
}

#[test]
fn conflict_budget_crossing_from_rows_into_checkpoints_has_a_precise_boundary() {
    let no_detail_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 0 };
    let (base, left, right, mut source) = row_checkpoint_conflict_inputs();
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, no_detail_budget)
        .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("first row conflict exceeds the zero-detail budget")
    };
    assert_eq!(report.conflicts_lower_bound, 1);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_checkpoints.is_empty());

    let one_detail_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 };
    let (base, left, right, mut source) = row_checkpoint_conflict_inputs();
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, one_detail_budget)
        .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("checkpoint conflict is the first conflict beyond the row detail")
    };
    assert_eq!(report.conflicts_lower_bound, 2);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_checkpoints.contains(b"consumer/source".as_slice()));

    let exact_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 };
    let (base, left, right, mut source) = row_checkpoint_conflict_inputs();
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, exact_budget)
        .unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else {
        panic!("conflicts at the exact budget remain a typed conflict result")
    };
    assert_eq!(conflicts.len(), 2);
    assert!(matches!(&conflicts[0], BranchMergeConflict::Row { .. }));
    assert!(matches!(&conflicts[1], BranchMergeConflict::CheckpointConflict { .. }));
    assert_eq!(report.conflicts_lower_bound, 2);
}

#[test]
fn conflict_budget_tail_reports_prior_table_ranges_and_checkpoint_impact() {
    let mut base_schema = schema(true, FieldType::Str);
    let mut second_table = base_schema.tables[0].clone();
    second_table.id = id(5);
    second_table.name = "Company".into();
    base_schema.tables.push(second_table);

    let mut base_row_company = parse_fixture(BASE, RowKeyKind::Explicit);
    base_row_company.table = id(5);
    let mut left_row_company = parse_fixture(LEFT, RowKeyKind::Explicit);
    left_row_company.table = id(5);
    let mut right_row_company = parse_fixture(CONFLICT, RowKeyKind::Explicit);
    right_row_company.table = id(5);
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Base, b"base-company", vec![base_row_company]);
    source.add(MergeSide::Left, b"left-company", vec![left_row_company]);
    source.add(MergeSide::Right, b"right-company", vec![right_row_company]);

    let checkpoint = |generation, token: &[u8]| CheckpointGeneration {
        generation,
        position: Some(token.to_vec()),
    };
    let mut base = snapshot(
        base_schema.clone(),
        manifest(1, 1, b"base"),
        Some(checkpoint(4, b"base-token")),
    );
    let mut left = snapshot(
        base_schema.clone(),
        manifest(2, 2, b"left"),
        Some(checkpoint(5, b"left-token")),
    );
    let mut right = snapshot(
        base_schema,
        manifest(3, 3, b"right"),
        Some(checkpoint(6, b"right-token")),
    );
    base.tables.insert(id(5), manifest(10, 10, b"base-company"));
    left.tables.insert(id(5), manifest(20, 20, b"left-company"));
    right.tables.insert(id(5), manifest(30, 30, b"right-company"));

    let exact_row_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 2 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, exact_row_budget)
        .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the checkpoint conflict is the first conflict beyond two row details")
    };
    assert_eq!(report.conflicts_lower_bound, 3);
    assert_eq!(report.rows_examined, 6);
    assert!(report.affected_tables.contains(&id(1)));
    assert!(report.affected_tables.contains(&id(5)));
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_ranges.contains(&(id(5), KeyRange::all())));
    assert!(report.affected_checkpoints.contains(b"consumer/source".as_slice()));
}

#[test]
fn checkpoint_budget_tail_reports_only_the_crossing_id_after_a_row_conflict() {
    let (mut base, mut left, mut right, mut source) = row_checkpoint_conflict_inputs();
    base.checkpoints.clear();
    left.checkpoints.clear();
    right.checkpoints.clear();

    // The ordered checkpoint walk stops at the first conflict beyond budget;
    // successful earlier entries and unexamined later IDs are not impacts.
    let base_clean = CheckpointGeneration { generation: 1, position: Some(b"clean-base".to_vec()) };
    let left_clean = CheckpointGeneration { generation: 2, position: Some(b"clean-left".to_vec()) };
    for (side, checkpoint) in [
        (&mut base, base_clean.clone()),
        (&mut left, left_clean.clone()),
        (&mut right, base_clean),
    ] {
        side.checkpoints.insert(b"consumer/a-clean".to_vec(), checkpoint);
    }
    for (snapshot, checkpoint) in [
        (&mut base, CheckpointGeneration { generation: 4, position: Some(b"m-base".to_vec()) }),
        (&mut left, CheckpointGeneration { generation: 5, position: Some(b"m-left".to_vec()) }),
        (&mut right, CheckpointGeneration { generation: 6, position: Some(b"m-right".to_vec()) }),
    ] {
        snapshot.checkpoints.insert(b"consumer/m-conflict".to_vec(), checkpoint);
    }
    for (snapshot, checkpoint) in [
        (&mut base, CheckpointGeneration { generation: 7, position: Some(b"z-base".to_vec()) }),
        (&mut left, CheckpointGeneration { generation: 8, position: Some(b"z-left".to_vec()) }),
        (&mut right, CheckpointGeneration { generation: 9, position: Some(b"z-right".to_vec()) }),
    ] {
        snapshot.checkpoints.insert(b"consumer/z-unvisited".to_vec(), checkpoint);
    }

    let one_detail_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 };
    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, one_detail_budget)
        .unwrap_err();
    let BranchMergeError::BudgetExceeded { report } = error else {
        panic!("the first conflicting checkpoint exceeds the row conflict detail")
    };
    assert_eq!(report.conflicts_lower_bound, 2);
    assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
    assert!(report.affected_checkpoints.contains(b"consumer/m-conflict".as_slice()));
    assert!(!report.affected_checkpoints.contains(b"consumer/a-clean".as_slice()));
    assert!(!report.affected_checkpoints.contains(b"consumer/z-unvisited".as_slice()));
    assert_eq!(source.visited.len(), 3);
}

#[test]
fn checkpoint_delete_update_budget_tail_reports_identity_for_either_deleted_side() {
    for delete_on_left in [true, false] {
        let (mut base, mut left, mut right, mut source) = row_checkpoint_conflict_inputs();
        base.checkpoints.clear();
        left.checkpoints.clear();
        right.checkpoints.clear();

        // Deletion-versus-update has an absent branch value, but impact stays
        // attached to the stable ID on either side of the conflict.
        let checkpoint_id = if delete_on_left {
            b"consumer/deleted-on-left".to_vec()
        } else {
            b"consumer/deleted-on-right".to_vec()
        };
        base.checkpoints.insert(
            checkpoint_id.clone(),
            CheckpointGeneration { generation: 4, position: Some(b"base-token".to_vec()) },
        );
        let edited_side = if delete_on_left { &mut right } else { &mut left };
        edited_side.checkpoints.insert(
            checkpoint_id.clone(),
            CheckpointGeneration { generation: 5, position: Some(b"edited-token".to_vec()) },
        );

        let one_detail_budget = BranchMergeBudget { max_rows_examined: 100, max_conflicts: 1 };
        let error = merge_three_way_snapshots(&base, &left, &right, &mut source, one_detail_budget)
            .unwrap_err();
        let BranchMergeError::BudgetExceeded { report } = error else {
            panic!("checkpoint delete/update is the first conflict beyond the row detail")
        };
        assert_eq!(report.conflicts_lower_bound, 2);
        assert!(report.affected_ranges.contains(&(id(1), KeyRange::all())));
        assert!(report.affected_checkpoints.contains(checkpoint_id.as_slice()));
        assert_eq!(source.visited.len(), 3);
    }
}

#[test]
fn schema_conflict_is_a_boundary_before_row_reads_and_checkpoint_resolution() {
    let base_checkpoint = CheckpointGeneration {
        generation: 4,
        position: Some(b"base-token".to_vec()),
    };
    let left_checkpoint = CheckpointGeneration {
        generation: 5,
        position: Some(b"left-token".to_vec()),
    };
    let right_checkpoint = CheckpointGeneration {
        generation: 6,
        position: Some(b"right-token".to_vec()),
    };
    let mut left_schema = schema(true, FieldType::Str);
    left_schema.tables[0].fields[1].ty = FieldType::Int;
    let mut right_schema = schema(true, FieldType::Str);
    right_schema.tables[0].fields[1].ty = FieldType::Bool;
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![parse_fixture(BASE, RowKeyKind::Explicit)]);
    source.add(MergeSide::Left, b"left", vec![parse_fixture(LEFT, RowKeyKind::Explicit)]);
    source.add(MergeSide::Right, b"right", vec![parse_fixture(CONFLICT, RowKeyKind::Explicit)]);
    let base = snapshot(
        schema(true, FieldType::Str),
        manifest(1, 1, b"base"),
        Some(base_checkpoint),
    );
    let left = snapshot(left_schema, manifest(2, 2, b"left"), Some(left_checkpoint));
    let right = snapshot(right_schema, manifest(3, 3, b"right"), Some(right_checkpoint));

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget())
        .unwrap_err();
    let BranchMergeError::Conflicts { conflicts, report } = error else {
        panic!("schema conflict must reject before producing a plan")
    };
    assert!(matches!(conflicts.as_slice(), [BranchMergeConflict::Schema(_)]));
    assert_eq!(report.conflicts_lower_bound, 1);
    assert!(report.affected_checkpoints.is_empty());
    assert!(source.visited.is_empty(), "unresolved schema prevents row decoding");
}

#[test]
fn divergent_checkpoint_generations_preserve_conflict_positions() {
    let base_position = CheckpointGeneration { generation: 4, position: Some(b"base-token".to_vec()) };
    let left_position = CheckpointGeneration { generation: 5, position: Some(b"left-token".to_vec()) };
    let right_position = CheckpointGeneration { generation: 6, position: Some(b"right-token".to_vec()) };
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), Some(base_position.clone()));
    let left = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), Some(left_position.clone()));
    let right = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), Some(right_position.clone()));
    let mut source = FixtureRows::default();

    let retained_base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), Some(base_position.clone()));
    let adopted = merge_three_way_snapshots(&base, &left, &retained_base, &mut source, budget()).unwrap();
    assert_eq!(adopted.checkpoints[b"consumer/source".as_slice()], left_position);

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap_err();
    assert!(matches!(error, BranchMergeError::Conflicts { conflicts, .. } if conflicts.iter().any(|item| matches!(item, BranchMergeConflict::CheckpointConflict { id, conflict } if item.diagnostic_code() == Some("sys.CheckpointConflict") && id.as_slice() == b"consumer/source" && conflict.base.as_ref() == Some(&base_position) && conflict.left.as_ref() == Some(&left_position) && conflict.right.as_ref() == Some(&right_position)))));
}

#[test]
fn generated_key_collision_is_a_row_conflict_and_never_renumbered() {
    let mut left_row = parse_fixture(LEFT, RowKeyKind::Automatic);
    let mut right_row = left_row.clone();
    let generated_key = CanonicalValue::new(OvbRaw::Int(92.into())).unwrap();
    left_row.key = generated_key.clone();
    right_row.key = generated_key;
    right_row.fields.insert(id(2), string("different payload"));
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", Vec::new());
    source.add(MergeSide::Left, b"left", vec![left_row]);
    source.add(MergeSide::Right, b"right", vec![right_row]);
    let base = snapshot(schema(false, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(false, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(false, FieldType::Str), manifest(3, 3, b"right"), None);

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap_err();
    assert!(matches!(error, BranchMergeError::Conflicts { conflicts, .. } if conflicts.iter().any(|item| matches!(item, BranchMergeConflict::Row { conflict: orna_evolution_v1::RowMergeConflict::AutomaticKeyCollision { key, .. }, .. } if key == &CanonicalValue::new(OvbRaw::Int(92.into())).unwrap()))));
}

#[test]
fn versioned_delete_survives_merge_as_a_tombstone_without_pruning_history() {
    let deleted_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let mut retained_row = deleted_row.clone();
    retained_row.key = integer(2);
    retained_row.fields.insert(id(2), string("old retained row"));
    let mut edited_retained_row = retained_row.clone();
    edited_retained_row.fields.insert(id(3), string("Paris"));

    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![deleted_row.clone(), retained_row.clone()]);
    source.add(MergeSide::Left, b"left", vec![retained_row.clone()]);
    source.add(MergeSide::Right, b"right", vec![deleted_row.clone(), edited_retained_row.clone()]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

    let plan = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap();
    let MergedSegment::Rows { rows, tombstones, .. } = &plan.tables[&id(1)].segments[0] else {
        panic!("all three versions changed and require a merged range")
    };
    assert_eq!(rows, &[edited_retained_row]);
    assert_eq!(tombstones, &[deleted_row.key.clone()]);
    assert!(source.rows[&(MergeSide::Base, b"base".to_vec())].contains(&deleted_row));
}

#[test]
fn versioned_delete_conflicts_with_a_concurrent_edit() {
    let base_row = parse_fixture(BASE, RowKeyKind::Explicit);
    let mut edited_row = base_row.clone();
    edited_row.fields.insert(id(2), string("concurrent edit"));
    let mut source = FixtureRows::default();
    source.add(MergeSide::Base, b"base", vec![base_row]);
    source.add(MergeSide::Left, b"left", Vec::new());
    source.add(MergeSide::Right, b"right", vec![edited_row]);
    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);

    let error = merge_three_way_snapshots(&base, &left, &right, &mut source, budget()).unwrap_err();
    assert!(matches!(
        error,
        BranchMergeError::Conflicts { conflicts, .. }
            if conflicts.iter().any(|conflict| matches!(
                conflict,
                BranchMergeConflict::Row {
                    conflict: orna_evolution_v1::RowMergeConflict::DeleteAndEdit { .. },
                    ..
                }
            ))
    ));
}

#[test]
fn unavailable_or_pruned_rows_fail_closed_instead_of_becoming_deletes() {
    struct UnavailableRows;
    impl BranchRowSource for UnavailableRows {
        fn visit_rows(
            &mut self,
            _side: MergeSide,
            _table: ObjectId,
            _segment: Option<&RowSegmentManifest>,
            _range: &KeyRange,
            _visitor: &mut dyn FnMut(KeyedRow) -> bool,
        ) -> Result<(), String> {
            Err("segment is pruned or unavailable".into())
        }
    }

    let base = snapshot(schema(true, FieldType::Str), manifest(1, 1, b"base"), None);
    let left = snapshot(schema(true, FieldType::Str), manifest(2, 2, b"left"), None);
    let right = snapshot(schema(true, FieldType::Str), manifest(3, 3, b"right"), None);
    let error = merge_three_way_snapshots(&base, &left, &right, &mut UnavailableRows, budget())
        .unwrap_err();
    assert_eq!(
        error,
        BranchMergeError::RowRead {
            message: "segment is pruned or unavailable".into(),
        }
    );
}
