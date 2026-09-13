use orna_semantic_v1::{
    Catalogue, DIAG_TYPE, DIAG_UNRESOLVED, ModuleInput, Type, analyze, analyze_with_catalogue,
};

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

#[test]
fn piped_relation_flat_map_rejects_wrong_argument_names_and_respects_shadowing() {
    let invalid = analyze(&[ModuleInput::new(
        "piped-flat-map-shape.orna",
        r#"
            pub table Note(id: Int) { value: Int, }
            pub fn invalid(rows: Relation<Note>) = rows | flat_map(predicate: note => [note.value]);
        "#,
    )]);
    assert!(
        has_message(
            &invalid,
            "relation flat_map argument name does not match its static signature"
        ),
        "{:#?}",
        invalid.diagnostics
    );
    let module = invalid
        .modules
        .values()
        .next()
        .expect("invalid flat_map module");
    assert!(matches!(
        &module.symbols["invalid"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Error
    ));

    let shadowed = analyze(&[ModuleInput::new(
        "piped-flat-map-shadowing.orna",
        r#"
            pub table Note(id: Int) { value: Int, }
            fn flat_map(rows: Relation<Note>): Relation<Note> = rows;
            pub fn shadowed(rows: Relation<Note>) = rows | flat_map;
        "#,
    )]);
    assert!(shadowed.is_ok(), "{:#?}", shadowed.diagnostics);
}

#[test]
fn relational_callbacks_preserve_effects_and_failure_before_planning() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "relation-callback-effects.orna",
            r#"
                pub table Note(id: Int) { value: Int, }
                pub fn pure(rows: Relation<Note>) = rows | filter(note => note.value > 0);
                pub fn database(rows: Relation<Note>) = rows | filter(note => Note.count() > 0);
                pub fn direct_one(rows: Relation<Note>) = one(rows);
                pub fn predicate_one(rows: Relation<Note>) = one(rows, note => note.value > 0);
                pub fn failed(rows: Relation<Note>) = one(rows, note => Note.one().value > 0);
                pub fn direct_flat_map(rows: Relation<Note>) = flat_map(rows, note => [note.value]);
                pub fn piped_flat_map(rows: Relation<Note>) = rows | flat_map(note => [note.value]);
            "#,
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .next()
        .expect("relation callback module");

    assert!(module.symbols["pure"].effects.effects.is_empty());
    assert!(!module.symbols["pure"].effects.may_fail);
    assert!(
        module.symbols["database"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(module.symbols["database"].effects.may_fail);
    assert!(module.symbols["direct_one"].effects.may_fail);
    assert!(module.symbols["predicate_one"].effects.may_fail);
    assert!(module.symbols["failed"].effects.may_fail);
    for name in ["direct_flat_map", "piped_flat_map"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. }
                if result.as_ref() == &Type::Relation(Box::new(Type::Int))
        ));
    }
    for name in ["direct_one", "predicate_one"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Named("Note".into())
        ));
    }
}

#[test]
fn relational_callbacks_reject_mutations_and_external_effects() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "relation-callback-effect-rejection.orna",
            r#"
                pub table Note(id: Int) { value: Int, }
                pub fn mutating(rows: Relation<Note>) = rows | filter(note => {
                    Note.insert({ id: note.id, value: note.value });
                    true
                });
                pub fn external(rows: Relation<Note>) = rows | filter(note => std.net.http.get("https://example.com") == "ok");
            "#,
        )],
        &Catalogue::authoritative_core(),
    );
    let rejections = result
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code() == DIAG_TYPE
                && diagnostic.message() == "relation query callback must be read-only"
        })
        .count();
    assert_eq!(rejections, 2, "{:#?}", result.diagnostics);
    assert!(
        result
            .diagnostics
            .iter()
            .all(
                |diagnostic| diagnostic.message() != "relation query callback must be read-only"
                    || diagnostic.code() == DIAG_TYPE,
            ),
        "relation callback effect diagnostic: {:#?}",
        result.diagnostics
    );
}

#[test]
fn relation_flat_map_rejects_mutations_and_external_effects_in_both_call_forms() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "relation-flat-map-effect-rejection.orna",
            r#"
                pub table Note(id: Int) { value: Int, }
                pub fn direct_mutating(rows: Relation<Note>) = flat_map(rows, note => {
                    Note.insert({ id: note.id, value: note.value });
                    [note.value]
                });
                pub fn piped_external(rows: Relation<Note>) = rows | flat_map(note => [std.net.http.get("https://example.com")]);
            "#,
        )],
        &Catalogue::authoritative_core(),
    );
    let rejections = result
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code() == DIAG_TYPE
                && diagnostic.message() == "relation query callback must be read-only"
        })
        .count();
    assert_eq!(rejections, 2, "{:#?}", result.diagnostics);
}

#[test]
fn relation_every_exists_pipeline_callbacks_are_read_only_in_all_call_forms() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "relation-every-exists-effect-rejection.orna",
            r#"
                pub table Note(id: Int) { value: Int, }
                pub fn pure_every(rows: Relation<Note>) = rows | every(note => note.value > 0);
                pub fn pure_exists(rows: Relation<Note>) = rows | exists(predicate: note => note.value > 0);
                pub fn piped_every(rows: Relation<Note>) = rows | every(note => {
                    Note.insert({ id: note.id, value: note.value });
                    true
                });
                pub fn piped_exists(rows: Relation<Note>) = rows | exists(predicate: note => std.net.http.get("https://example.com") == "ok");
                pub fn direct_every(rows: Relation<Note>) = every(rows: rows, predicate: note => {
                    Note.insert({ id: note.id, value: note.value });
                    true
                });
                pub fn direct_exists(rows: Relation<Note>) = exists(predicate: note => std.net.http.get("https://example.com") == "ok", rows: rows);
            "#,
        )],
        &Catalogue::authoritative_core(),
    );
    let read_only = result
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code() == DIAG_TYPE
                && diagnostic.message() == "relation query callback must be read-only"
        })
        .count();
    assert_eq!(read_only, 4, "{:#?}", result.diagnostics);
    assert!(
        !result.is_ok(),
        "effectful relation callbacks were accepted"
    );

    let module = result
        .modules
        .values()
        .next()
        .expect("relation every/exists module");
    for name in ["pure_every", "pure_exists"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Bool
        ));
    }
}

#[test]
fn relational_callbacks_keep_existing_shape_diagnostics() {
    let result = analyze(&[ModuleInput::new(
        "relation-callback-shape.orna",
        r#"
            pub table Note(id: Int) { value: Int, }
            pub fn invalid(rows: Relation<Note>) = rows | filter(note => note.value);
        "#,
    )]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "{:#?}",
        result.diagnostics
    );
}

#[test]
fn malformed_direct_relation_shape_preserves_later_argument_diagnostic() {
    let result = analyze(&[ModuleInput::new(
        "relation-callback-order.orna",
        r#"
            pub table Note(id: Int) { value: Int, }
            pub fn invalid(rows: Relation<Note>) = every(rows, unexpected: missing);
        "#,
    )]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNRESOLVED),
        "later argument unresolved diagnostic: {:#?}",
        result.diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message() == "relation every requires rows and predicate"),
        "relation shape diagnostic: {:#?}",
        result.diagnostics
    );
}

#[test]
fn relation_callback_and_later_argument_diagnostics_are_both_preserved() {
    let result = analyze(&[ModuleInput::new(
        "relation-callback-order.orna",
        r#"
            pub table Note(id: Int) { value: Int, }
            pub fn invalid(rows: Relation<Note>) = every(rows, value => value, unexpected: later_missing);
        "#,
    )]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "callback type diagnostic: {:#?}",
        result.diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNRESOLVED),
        "later argument unresolved diagnostic: {:#?}",
        result.diagnostics
    );
}

#[test]
fn malformed_direct_one_calls_report_the_static_signature_diagnostic() {
    for body in [
        "one(rows, unexpected: 1)",
        "one(rows, 1, 2)",
        "one(rows: rows, rows: rows)",
    ] {
        let result = analyze(&[ModuleInput::new(
            "malformed-direct-one.orna",
            format!(
                "pub table Note(id: Int) {{ value: Int, }} pub fn invalid(rows: Relation<Note>) = {body};"
            ),
        )]);
        assert!(
            has_message(
                &result,
                "relation one arguments do not match its static signature"
            ),
            "{body}: {:#?}",
            result.diagnostics
        );
    }
}
