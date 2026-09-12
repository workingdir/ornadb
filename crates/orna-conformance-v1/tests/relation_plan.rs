use orna_conformance_v1::{SourceUnit, StageOutcome, TransactionalEvaluator};
use orna_evaluator_v1::Limits;

fn relation_source(source: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: "relation-plan".into(),
        source_id: "relation-plan.orna".into(),
        parse_as: "module_unit".into(),
        source: source.into(),
    }
}

fn assert_relation_one_error(insertions: &str, expression: &str, expected: &str) {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(&format!(
        r#"
            pub table Note(id: Int) {{ value: Int, }}
            fn selected() = {expression};
            fn parent() {{
                {insertions}
                selected();
            }}
        "#,
    ));

    match runtime.execute_source(&unit) {
        StageOutcome::Failed(diagnostic) => assert_eq!(diagnostic.code(), expected),
        outcome => panic!("expected relation one failure, got {outcome:?}"),
    }
}

#[test]
fn take_before_sort_bounds_source_and_post_sort_callbacks() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(
        r#"
            pub table Note(id: Int) { value: Int, }
            fn parent() {
                Note.insert({ id: 1, value: 10 });
                Note.insert({ id: 2, value: 20 });
                assert ((Note | take(0) | sort_by(note => note.id)
                    | map(note => note.id % (note.id - note.id)) | first()) ?? -1) == -1;
                assert ((Note | sort_by(note => -note.id)
                    | map(note => note.id % (note.id - 1)) | first()) ?? -1) == 0;
            }
        "#,
    );

    let outcome = runtime.execute_source(&unit);
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn bound_relation_variables_support_direct_terminals() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(
        r#"
            pub table Note(id: Int) { value: Int, }
            fn parent() {
                Note.insert({ id: 1, value: 1 });
                Note.insert({ id: 2, value: 2 });
                let relation = Note | filter(note => note.value > 1);
                let sorted = sort_by(rows: relation, key: note => -note.id);
                assert (first(rows: sorted) ?? Note.insert({ id: 99, value: 0 })).id == 2;
                let named_relation = filter(rows: Note, predicate: note => note.value > 1);
                let named_sorted = sort_by(rows: named_relation, key: note => -note.id);
                assert (first(rows: named_sorted) ?? Note.insert({ id: 98, value: 0 })).id == 2;
            }
        "#,
    );

    let outcome = runtime.execute_source(&unit);
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn lexical_filter_shadow_is_not_hijacked_by_relation_intrinsic() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(
        r#"
            pub table Note(id: Int) { value: Int, }
            fn apply(filter: Int) = Note | filter(note => true) | count;
            fn parent() {
                Note.insert({ id: 1, value: 1 });
                apply(1);
            }
        "#,
    );

    assert!(matches!(
        runtime.execute_source(&unit),
        StageOutcome::Failed(_)
    ));
    assert_eq!(
        runtime.committed_row("Note", &orna_foundation_v1::Value::int(1.into())),
        None
    );
}

#[test]
fn declared_filter_executes_instead_of_the_relation_intrinsic() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(
        r#"
            pub table Note(id: Int) { value: Int, }
            fn filter(rows: Relation<Note>): Int = 99;
            fn parent() {
                assert filter(Note) == 99;
                assert (Note | filter) == 99;
            }
        "#,
    );
    let outcome = runtime.execute_source(&unit);
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn declared_relation_helpers_shadow_all_specialized_lowering_paths() {
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let unit = relation_source(
        r#"
            pub table Note(id: Int) { value: Int, }
            fn one(rows: Relation<Note>): Int = 11;
            fn count(rows: Relation<Note>): Int = 12;
            fn window(rows: Relation<Note>, size: Int): Int = 13;
            fn parent() {
                assert (Note | filter(note => note.id == 1) | one()) == 11;
                assert (Note | filter(note => note.value == 1) | one()) == 11;
                assert (Note | filter(note => note.value == 1) | count) == 12;
                assert (Note | count) == 12;
                assert window(Note, 2) == 13;
                assert (Note | window(2)) == 13;
            }
        "#,
    );
    let outcome = runtime.execute_source(&unit);
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn relation_one_preserves_exact_cardinality_errors_before_and_after_sort() {
    const ZERO: &str = "ORNA-EVAL-RELATION-ONE-ZERO";
    const MULTIPLE: &str = "ORNA-EVAL-RELATION-ONE-MULTIPLE";

    for (insertions, expression, expected) in [
        ("", "Note | one()", ZERO),
        (
            "Note.insert({ id: 1, value: 1 }); Note.insert({ id: 2, value: 2 });",
            "Note | one()",
            MULTIPLE,
        ),
        (
            "Note.insert({ id: 1, value: 1 }); Note.insert({ id: 2, value: 2 });",
            "Note | one(note => note.id == 99)",
            ZERO,
        ),
        (
            "Note.insert({ id: 1, value: 1 }); Note.insert({ id: 2, value: 2 });",
            "Note | one(note => note.id > 0)",
            MULTIPLE,
        ),
        ("", "Note | sort_by(note => -note.id) | one()", ZERO),
        (
            "Note.insert({ id: 1, value: 1 }); Note.insert({ id: 2, value: 2 });",
            "Note | sort_by(note => -note.id) | one()",
            MULTIPLE,
        ),
        (
            "Note.insert({ id: 1, value: 1 }); Note.insert({ id: 2, value: 2 });",
            "Note | sort_by(note => -note.id) | one(note => note.id == 99)",
            ZERO,
        ),
        (
            "Note.insert({ id: 1, value: 1 }); Note.insert({ id: 2, value: 2 });",
            "Note | sort_by(note => -note.id) | one(note => note.id > 0)",
            MULTIPLE,
        ),
    ] {
        assert_relation_one_error(insertions, expression, expected);
    }
}
