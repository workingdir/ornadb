use orna_semantic_v1::{ModuleInput, analyze};

fn has_message(result: &orna_semantic_v1::Analysis, expected: &str) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message() == expected)
}

#[test]
fn direct_relation_bounds_reject_nonpositive_constants() {
    for (body, expected) in [
        ("take(Note, -1)", "relation take count must be nonnegative"),
        (
            "take(rows: Note, count: -1)",
            "relation take count must be nonnegative",
        ),
        ("drop(Note, -1)", "relation drop count must be nonnegative"),
        (
            "drop(rows: Note, count: -1)",
            "relation drop count must be nonnegative",
        ),
        ("window(Note, 0)", "window size must be positive"),
        (
            "window(rows: Note, size: 0)",
            "window size must be positive",
        ),
        ("window(Note, -1)", "window size must be positive"),
        ("window(Note, 2, step: 0)", "window step must be positive"),
        (
            "window(rows: Note, size: 2, step: -1)",
            "window step must be positive",
        ),
    ] {
        let result = analyze(&[ModuleInput::new(
            "direct-relation-bounds.orna",
            format!("pub table Note(id: Int) {{ value: Int, }} fn invalid() = {body};"),
        )]);
        assert!(
            has_message(&result, expected),
            "{body}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn declared_relation_helper_shadows_the_core_filter_name() {
    let result = analyze(&[ModuleInput::new(
        "declared-filter.orna",
        r#"
            pub table Note(id: Int) { value: Int, }
            fn filter(rows: Relation<Note>): Int = 99;
            fn direct() = filter(Note) == 99;
            fn piped() = (Note | filter) == 99;
        "#,
    )]);
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
}
