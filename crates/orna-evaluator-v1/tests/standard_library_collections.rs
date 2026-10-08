use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::{Raw, Value};

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn unit_value() -> CanonicalValue {
    CanonicalValue::new(Value::unit().raw().clone()).unwrap()
}

fn assert_reference_collection_true(session: &mut AdmittedReplSession, expression: &str) {
    let actual = session.submit(expression);
    assert_eq!(
        actual,
        Ok(Some(bool_value(true))),
        "{expression}: {}",
        actual
            .as_ref()
            .err()
            .map_or("success", |error| error.code())
    );
}

#[test]
fn pinned_core_collection_source_executes_the_documented_list_operations() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for expression in [
        "std.collection.filter([1, 2, 3, 4], value => value % 2 == 0) == [2, 4]",
        "std.collection.map([1, 2, 3], value => value * 2) == [2, 4, 6]",
        "std.collection.flat_map([2, 1], value => [value, value + 10]) == [2, 12, 1, 11]",
        "std.collection.sort_by([3, 2, 1, 4], value => value % 2) == [2, 4, 3, 1]",
        "std.collection.take([1, 2, 3], 2) == [1, 2]",
        "std.collection.drop([1, 2, 3], 2) == [3]",
        "std.collection.distinct([1, 2, 1, 3, 2]) == [1, 2, 3]",
        "std.collection.union([1, 2], [2, 3]) == [1, 2, 2, 3]",
        "std.collection.count([1, 2, 3]) == 3",
        "std.collection.first(std.collection.drop([4, 5], 2)) == null && std.collection.first([4, 5]) == Some(4)",
        "std.collection.one([4, 5], value => value == 5) == 5",
        "(std.collection.one([1, 2], value => value > 5) |? (failure => if failure.code == \"std.collection.one.none\" { 1 } else { 0 })) == 1",
        "(std.collection.one([1, 2], value => value > 0) |? (failure => if failure.code == \"std.collection.one.multiple\" { 1 } else { 0 })) == 1",
        "std.collection.sum([]) == 0 && std.collection.sum([9007199254740993, 1]) == 9007199254740994",
        "std.collection.min([]) == null && std.collection.max([]) == null",
        "std.collection.every(std.collection.take([1], 0), value => value > 0) && !std.collection.exists(std.collection.take([1], 0), value => value > 0)",
        "std.collection.every([0, 1], value => if value == 0 { false } else { 1 / (value - 1) > 0 }) == false",
        "std.collection.exists([1, 0], value => if value == 1 { true } else { 1 / value > 0 })",
    ] {
        assert_reference_collection_true(&mut session, expression);
    }

    for expression in [
        "std.collection.take([1, 2], -1)",
        "std.collection.drop([1, 2], -1)",
        "std.collection.filter([1], value => value)",
    ] {
        assert!(
            session.submit(expression).is_err(),
            "{expression} must fail"
        );
    }

    let failed_one = session
        .submit(
            "std.collection.one([1, 2, 3], value => if value < 3 { true } else { 1 / (value - 3) == 0 })",
        )
        .expect_err("one must propagate callback failures after multiple matches");
    assert_eq!(failed_one.code(), "ORNA-EVAL-DIVIDE-BY-ZERO");

    let failed_map = session
        .submit("std.collection.map([1, 0, 2], value => 1 / value)")
        .expect_err("map must propagate a callback failure");
    assert_eq!(failed_map.code(), "ORNA-EVAL-DIVIDE-BY-ZERO");
}

#[test]
fn pinned_list_map_and_set_modules_preserve_their_documented_order() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for import in [
        include_str!("fixtures/stdlib-use-list-i7bat.orna"),
        include_str!("fixtures/stdlib-use-map-i7bat.orna"),
        include_str!("fixtures/stdlib-use-set-i7bat.orna"),
    ] {
        let imported = session.submit(import);
        assert!(imported.is_ok(), "{}", imported.unwrap_err().code());
    }
    for declaration in [
        include_str!("fixtures/stdlib-list-contract-i7bat.orna"),
        include_str!("fixtures/stdlib-map-contract-i7bat.orna"),
        include_str!("fixtures/stdlib-set-contract-i7bat.orna"),
    ] {
        let parsed = orna_syntax_v1::parse_repl_with_file(declaration, "collection-contract.orna");
        assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
        let admitted = session.submit(declaration);
        assert!(
            admitted.is_ok(),
            "{declaration}: {:#?}",
            admitted.as_ref().err()
        );
    }
    for call in [
        include_str!("fixtures/stdlib-call-list-contract-i7bat.orna"),
        include_str!("fixtures/stdlib-call-map-contract-i7bat.orna"),
        include_str!("fixtures/stdlib-call-set-contract-i7bat.orna"),
    ] {
        let evaluated = session.submit(call);
        assert!(
            evaluated.is_ok(),
            "{call}: {}",
            evaluated.unwrap_err().code()
        );
        assert_eq!(evaluated.unwrap(), Some(bool_value(true)));
    }
}

#[test]
fn core_intrinsics_work_without_the_optional_collection_modules() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-core-without-collections-i7bat.orna"
        )),
        Ok(Some(bool_value(true)))
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-map-without-snapshot-i7bat.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-core-without-collections-i7bat.orna"
        )),
        Ok(Some(bool_value(true)))
    );
}

#[test]
fn pinned_list_exports_execute_without_unsupported_runtime_paths() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-list-i7bat.orna")),
        Ok(None)
    );
    for (index, behavior) in [
        "list.singleton(7) == [7]",
        "list.length([1, 2, 3]) == 3",
        "list.is_empty([1]) == false",
        "list.append([1, 2], 3) == [1, 2, 3]",
        "list.prepend([2, 3], 1) == [1, 2, 3]",
        "list.concat([1, 2], [3]) == [1, 2, 3]",
        "list.first([9, 8]) != null",
        "list.last([9, 8]) != null",
        "list.contains([1, 2, 3], 2)",
        "list.take([1, 2, 3], 2) == [1, 2]",
        "list.drop([1, 2, 3], 2) == [3]",
        "list.unique([1, 2, 1]) == [1, 2]",
        "list.reverse([1, 2, 3]) == [3, 2, 1]",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            session.submit(behavior),
            Ok(Some(bool_value(true))),
            "list behavior {index}: {behavior}"
        );
    }
}

#[test]
fn pinned_stream_iteration_surface_is_optional_and_does_not_replace_core() {
    let mut with_std = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        with_std.submit(include_str!("fixtures/stdlib-use-stream-n8phe.orna")),
        Ok(None)
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_std.submit(include_str!(
            "fixtures/stdlib-core-without-collections-i7bat.orna"
        )),
        Ok(Some(bool_value(true)))
    );
    assert_eq!(
        without_std
            .submit(include_str!(
                "fixtures/stdlib-stream-without-snapshot-n8phe.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        without_std.submit(include_str!(
            "fixtures/stdlib-core-without-collections-i7bat.orna"
        )),
        Ok(Some(bool_value(true)))
    );
}

#[test]
fn list_backed_stream_replays_values_and_returns_unit_after_each_callback() {
    let mut session = AdmittedReplSession::new(Limits::default());
    let actual = session
        .submit(include_str!(
            "fixtures/stdlib-stream-for-each-unit-6844.orna"
        ))
        .unwrap_or_else(|error| panic!("finite stream call failed: {}", error.code()));
    assert_eq!(actual, Some(unit_value()));
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-stream-for-each-unit-6844.orna"
        )),
        Ok(Some(unit_value())),
        "a list-backed source is replayable from its initial position"
    );
}

#[test]
fn set_edge_cases_hold_for_duplicates_absent_values_and_empty_operands() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for import in [include_str!("fixtures/stdlib-use-set-i7bat.orna")] {
        let imported = session.submit(import);
        assert!(imported.is_ok(), "{}", imported.unwrap_err().code());
    }
    let declared = session.submit(include_str!("fixtures/stdlib-set-edge-contract-ogcs1.orna"));
    assert!(declared.is_ok(), "{:#?}", declared.as_ref().err());
    let evaluated = session.submit(include_str!(
        "fixtures/stdlib-call-set-edge-contract-ogcs1.orna"
    ));
    assert_eq!(evaluated, Ok(Some(bool_value(true))));
}
