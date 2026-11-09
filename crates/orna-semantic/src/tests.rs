use super::*;

/// Builds the catalogue and checks the bodies of one synthetic pinned
/// standard bundle.
fn check_bundle(sources: Vec<(String, String)>) -> StandardSourceCheck {
    let profile = StandardDependencyProfile::from_sources("std-probe", sources.clone())
        .expect("probe profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("probe catalogue");
    check_standard_sources(&catalogue, sources, SystemProviders::DeclaredSurface)
}

#[test]
fn pinned_standard_body_with_a_decided_mismatch_is_reported() {
    let check = check_bundle(vec![(
        String::from("std/probe.orna"),
        String::from("\npub fn broken(): Int = \"text\";\n"),
    )]);
    let diagnostic = check
        .diagnostics
        .first()
        .expect("the broken pinned body is decided");
    assert_eq!(diagnostic.logical_path, "std/probe.orna");
    assert_eq!(diagnostic.line, 2);
    assert_eq!(diagnostic.code, DIAG_TYPE);
    assert!(
        diagnostic
            .notes
            .iter()
            .any(|note| note == "expected Int, found Text"),
        "{:?}",
        diagnostic.notes
    );
}

#[test]
fn pinned_standard_body_with_a_conditional_optional_is_admitted() {
    // The conditional below is valid Orna: both branches unify with the
    // declared `Int?` (ORNA-CFLOW-003, ORNA-TYPE-001). Once this slice
    // typed the second branch against the first branch's `null` and
    // reported a mismatch that described the checker rather than the
    // pinned source; the pinned bundle is admitted here because its body
    // really is well typed.
    let check = check_bundle(vec![(
        String::from("std/probe.orna"),
        String::from(
            "pub fn half(value: Int): Int? =\n    if value < 0 { null } else { Some(value) };\n",
        ),
    )]);
    assert!(check.is_ok(), "{:?}", check.diagnostics);
}

#[test]
fn pinned_standard_body_with_an_unresolved_name_stays_admitted() {
    let check = check_bundle(vec![(
        String::from("std/probe.orna"),
        String::from("pub fn broken(): Int = missing_name;\n"),
    )]);
    assert!(check.is_ok(), "{:?}", check.diagnostics);
}

#[test]
fn compiled_unit_analysis_retains_exact_source_basis_scale_and_provenance() {
    let analysis = checked(&[ModuleInput::new(
        "units/main.orna",
        "pub dim Information; pub unit B : Information base; pub unit MiB : Information = 1048576.B; pub dim Temperature; pub unit K : Temperature base; pub unit C : Temperature = K offset 273.15 affine;",
    )]);
    assert!(analysis.is_ok(), "diagnostics: {:?}", analysis.diagnostics);

    let mebibyte = &analysis.source_units[&SourceUnitIdentity {
        namespace: Namespace(vec!["units".into()]),
        name: "MiB".into(),
    }];
    assert_eq!(mebibyte.scale.numerator, "1048576");
    assert_eq!(mebibyte.scale.denominator, "1");
    assert_eq!(
        mebibyte.dimension_basis.keys().next(),
        Some(&SourceDimensionIdentity {
            namespace: Namespace(vec!["units".into()]),
            name: "Information".into(),
        })
    );
    assert_eq!(mebibyte.dimension_basis.len(), 1);
    assert_eq!(
        mebibyte
            .dimension_basis
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(mebibyte.provenance.logical_path, "units/main.orna");

    let celsius = &analysis.source_units[&SourceUnitIdentity {
        namespace: Namespace(vec!["units".into()]),
        name: "C".into(),
    }];
    assert_eq!(celsius.offset.numerator, "5463");
    assert_eq!(celsius.offset.denominator, "20");
    assert!(celsius.affine);
}

#[test]
fn sparse_checkpoint_seeding_crosses_relation_window_boundaries() {
    let paired = Type::Tuple(vec![
        contextual_snapshot_ref("selector:HEAD~left"),
        contextual_snapshot_ref("selector:HEAD~right"),
    ]);
    let pinned = Type::Relation(Box::new(Type::List(Box::new(Type::Record(
        BTreeMap::from([
            ("left_checkpoint".into(), paired.clone()),
            ("right_checkpoint".into(), paired),
            ("witness".into(), Type::Int),
        ]),
    )))));
    let omitted = Type::Relation(Box::new(Type::List(Box::new(Type::Record(
        BTreeMap::from([("witness".into(), Type::Int)]),
    )))));

    assert!(type_sequence_has_omitted_pinned_checkpoint_tuple(&[
        omitted.clone(),
        pinned.clone(),
    ]));

    let mut rows = [omitted, pinned];
    seed_omitted_pinned_checkpoint_tuple_paths(&mut rows);

    let Type::Relation(window) = &rows[0] else {
        panic!("window relation wrapper must survive sparse seeding");
    };
    let Type::List(row) = window.as_ref() else {
        panic!("window row-list wrapper must survive sparse seeding");
    };
    let Type::Record(fields) = row.as_ref() else {
        panic!("window row shape must be seeded as a record");
    };
    for name in ["left_checkpoint", "right_checkpoint"] {
        assert_eq!(
            fields.get(name),
            Some(&Type::Tuple(vec![Type::Bottom, Type::Bottom])),
            "the omitted {name} tuple keeps both fold slots neutral"
        );
    }
}

#[test]
fn suppressed_paired_rebind_keeps_same_named_tuple_pins_distinct() {
    let left_selector = "selector:binder:pair.orna@10..18:parameter:selected";
    let right_selector = "selector:binder:pair.orna@20..28:parameter:selected";
    let formal_pair = Type::Tuple(vec![
        contextual_snapshot_ref(left_selector),
        contextual_snapshot_ref(right_selector),
    ]);
    let result = Type::Record(BTreeMap::from([
        ("left".into(), contextual_snapshot_ref(left_selector)),
        ("right".into(), contextual_snapshot_ref(right_selector)),
        (
            "paired_map".into(),
            Type::Applied {
                base: "semantic.SnapshotContextMap".into(),
                arguments: vec![
                    Type::Named(left_selector.into()),
                    Type::Named(right_selector.into()),
                ],
            },
        ),
    ]));
    let call_span = SyntaxSpan::new(40, 72);
    let arguments = vec![orna_syntax::Argument {
        name: None,
        value: Expr::Name {
            text: "pins".into(),
            span: SyntaxSpan::new(48, 52),
        },
        span: SyntaxSpan::new(48, 52),
    }];

    let specialized = specialize_dynamic_parameter_snapshot_contexts(
        &result,
        DynamicParameters {
            formal_parameters: &[formal_pair, Type::Text],
            default_parameters: &BTreeSet::new(),
            parameter_names: None,
            arguments: &arguments,
            argument_types: &[Type::Tuple(vec![
                contextual_snapshot_ref("selector:HEAD~2"),
                contextual_snapshot_ref("selector:HEAD~1"),
            ])],
            local: &BTreeMap::new(),
            call_span: &call_span,
            historical_context: None,
        },
    );
    let Type::Record(fields) = specialized else {
        panic!("suppressed paired rebind must retain its record result");
    };
    let mut left = BTreeSet::new();
    collect_test_snapshot_contexts(&fields["left"], &mut left);
    let mut right = BTreeSet::new();
    collect_test_snapshot_contexts(&fields["right"], &mut right);
    assert_eq!(
        left.len(),
        1,
        "left pin slot must retain one identity: {left:?}"
    );
    assert_eq!(
        right.len(),
        1,
        "right pin slot must retain one identity: {right:?}"
    );
    assert!(
        left.iter()
            .next()
            .is_some_and(|selector| selector.contains("pair.orna@10..18")),
        "the left symbolic pin must keep its formal binder: {left:?}"
    );
    assert!(
        right
            .iter()
            .next()
            .is_some_and(|selector| selector.contains("pair.orna@20..28")),
        "the right symbolic pin must keep its formal binder: {right:?}"
    );
    let Type::Applied {
        base,
        arguments: paired_map,
    } = &fields["paired_map"]
    else {
        panic!("the suppressed paired checkpoint must retain a context map");
    };
    assert_eq!(base, "semantic.SnapshotContextMap");
    assert_eq!(
        paired_map.len(),
        2,
        "the paired symbolic map must retain its formal width: {paired_map:?}"
    );
    assert!(is_snapshot_context_map_shape(&fields["paired_map"]));
    assert_ne!(
        left, right,
        "suppression must keep same-named paired pin slots at distinct widths"
    );
}

#[test]
fn unknown_direct_pair_sibling_suppresses_valid_pin_promotion() {
    let left = contextual_snapshot_ref("selector:binder:pair.orna@10..18:parameter:left_pin");
    let right = contextual_snapshot_ref("selector:binder:pair.orna@20..28:parameter:right_pin");
    let result = Type::Record(BTreeMap::from([
        ("left".into(), left.clone()),
        ("right".into(), right.clone()),
    ]));
    let arguments = (0..2)
        .map(|index| orna_syntax::Argument {
            name: None,
            value: Expr::Name {
                text: format!("pair_pin_{index}"),
                span: SyntaxSpan::new(40 + index * 12, 48 + index * 12),
            },
            span: SyntaxSpan::new(40 + index * 12, 48 + index * 12),
        })
        .collect::<Vec<_>>();

    let specialized = specialize_dynamic_parameter_snapshot_contexts(
        &result,
        DynamicParameters {
            formal_parameters: &[left, right],
            default_parameters: &BTreeSet::new(),
            parameter_names: None,
            arguments: &arguments,
            argument_types: &[contextual_snapshot_ref("selector:HEAD~12"), Type::Error],
            local: &BTreeMap::new(),
            call_span: &SyntaxSpan::new(30, 68),
            historical_context: None,
        },
    );

    let mut contexts = BTreeSet::new();
    collect_test_snapshot_contexts(&specialized, &mut contexts);
    assert_eq!(
        contexts.len(),
        2,
        "both pins must remain represented: {contexts:?}"
    );
    assert!(
        contexts
            .iter()
            .all(|context| context.starts_with("selector:dynamic-call:")),
        "an unknown pair sibling must suppress every sibling promotion: {contexts:?}"
    );
    assert!(
        !contexts.iter().any(|context| context.contains("HEAD~12")),
        "the valid sibling must not be promoted independently: {contexts:?}"
    );
}

#[test]
fn unknown_nested_paired_width_suppresses_sibling_identity_promotion() {
    let callback_selected =
        contextual_snapshot_ref("selector:binder:callback.orna@10..18:parameter:selected");
    let callback_sibling =
        contextual_snapshot_ref("selector:binder:callback.orna@20..28:parameter:sibling");
    let paired_selected =
        contextual_snapshot_ref("selector:binder:pair.orna@30..38:parameter:selected");
    let paired_sibling =
        contextual_snapshot_ref("selector:binder:pair.orna@40..48:parameter:sibling");
    let formal_callback = Type::Function {
        parameters: vec![Type::Tuple(vec![
            callback_selected.clone(),
            callback_sibling.clone(),
        ])],
        parameter_names: None,
        default_parameters: BTreeSet::new(),
        result: Box::new(Type::Text),
    };
    let formal_pair = Type::Tuple(vec![paired_selected.clone(), paired_sibling.clone()]);
    let actual_callback = Type::Function {
        parameters: vec![Type::Tuple(vec![
            contextual_snapshot_ref("selector:HEAD~12"),
            Type::Error,
        ])],
        parameter_names: None,
        default_parameters: BTreeSet::new(),
        result: Box::new(Type::Text),
    };
    let arguments = (0..2)
        .map(|index| orna_syntax::Argument {
            name: None,
            value: Expr::Name {
                text: format!("argument_{index}"),
                span: SyntaxSpan::new(50 + index * 10, 55 + index * 10),
            },
            span: SyntaxSpan::new(50 + index * 10, 55 + index * 10),
        })
        .collect::<Vec<_>>();
    let result = Type::Record(BTreeMap::from([
        ("callback_selected".into(), callback_selected),
        ("callback_sibling".into(), callback_sibling),
        ("paired_selected".into(), paired_selected),
        ("paired_sibling".into(), paired_sibling),
    ]));

    let specialized = specialize_dynamic_parameter_snapshot_contexts(
        &result,
        DynamicParameters {
            formal_parameters: &[formal_callback, formal_pair],
            default_parameters: &BTreeSet::new(),
            parameter_names: None,
            arguments: &arguments,
            argument_types: &[
                actual_callback,
                Type::Tuple(vec![
                    contextual_snapshot_ref("selector:HEAD~10"),
                    contextual_snapshot_ref("selector:HEAD~9"),
                ]),
            ],
            local: &BTreeMap::new(),
            call_span: &SyntaxSpan::new(45, 75),
            historical_context: None,
        },
    );

    let mut contexts = BTreeSet::new();
    collect_test_snapshot_contexts(&specialized, &mut contexts);
    assert_eq!(
        contexts.len(),
        4,
        "all paired pins must remain represented after an unknown nested width: {contexts:?}"
    );
    assert!(
        contexts
            .iter()
            .all(|context| context.starts_with("selector:dynamic-call:")),
        "no valid callback or sibling pin may be promoted independently: {contexts:?}"
    );
    assert!(
        !contexts.iter().any(|context| context.contains("HEAD~")),
        "unknown paired width must suppress all concrete identities in this wave: {contexts:?}"
    );
}

#[test]
fn binder_context_collector_rejects_partial_tuple_widths() {
    let formal = Type::Tuple(vec![
        contextual_snapshot_ref("selector:binder:pair.orna@10..18:parameter:selected"),
        contextual_snapshot_ref("selector:binder:pair.orna@20..28:parameter:sibling"),
    ]);
    let actual = Type::Tuple(vec![contextual_snapshot_ref("selector:HEAD~12")]);
    let mut binder_contexts = BTreeMap::new();

    collect_snapshot_binder_contexts(&formal, &actual, &mut binder_contexts);

    assert!(
        binder_contexts.is_empty(),
        "an unpaired width must not collect even the valid tuple prefix: {binder_contexts:?}"
    );
}

#[test]
fn omitted_sibling_keeps_same_named_captured_and_live_slots_distinct() {
    let result = Type::Applied {
        base: "semantic.SnapshotContextMap".into(),
        arguments: vec![
            Type::Named("selector:capture:parameter:pin".into()),
            Type::Named("selector:parameter:pin".into()),
        ],
    };
    let arguments = vec![orna_syntax::Argument {
        name: None,
        value: Expr::Name {
            text: "supplied_pin".into(),
            span: SyntaxSpan::new(10, 22),
        },
        span: SyntaxSpan::new(10, 22),
    }];
    let specialized = specialize_dynamic_parameter_snapshot_contexts(
        &result,
        DynamicParameters {
            formal_parameters: &[
                Type::Named("sys.SnapshotRef".into()),
                Type::Tuple(vec![
                    Type::Named("sys.SnapshotRef".into()),
                    Type::Named("sys.SnapshotRef".into()),
                ]),
            ],
            default_parameters: &BTreeSet::new(),
            parameter_names: None,
            arguments: &arguments,
            argument_types: &[contextual_snapshot_ref("selector:HEAD~12")],
            local: &BTreeMap::new(),
            call_span: &SyntaxSpan::new(5, 30),
            historical_context: None,
        },
    );

    let Type::Applied { base, arguments } = &specialized else {
        panic!("omission must preserve the pair rather than collapse it: {specialized:?}");
    };
    assert_eq!(base, "semantic.SnapshotContextMap");
    assert!(is_snapshot_context_map_shape(&specialized));
    assert_eq!(
        arguments.len(),
        2,
        "both paired slots must remain distinct: {arguments:?}"
    );
    let Type::Named(left) = &arguments[0] else {
        panic!("the first suppressed slot must retain a selector identity: {arguments:?}");
    };
    let Type::Named(right) = &arguments[1] else {
        panic!("the second suppressed slot must retain a selector identity: {arguments:?}");
    };
    assert!(
        left.starts_with("selector:dynamic-call:") && right.starts_with("selector:dynamic-call:"),
        "the failed call must preserve symbolic identities: {arguments:?}"
    );
    assert_ne!(left, right, "captured and live slots must not collapse");
}

fn collect_test_snapshot_contexts(ty: &Type, into: &mut BTreeSet<String>) {
    if let Some(selector) = snapshot_ref_context_key(ty) {
        into.insert(selector.to_owned());
        return;
    }
    match ty {
        Type::Record(fields) => {
            for field in fields.values() {
                collect_test_snapshot_contexts(field, into);
            }
        }
        Type::Tuple(elements) => {
            for element in elements {
                collect_test_snapshot_contexts(element, into);
            }
        }
        Type::Applied { arguments, .. } => {
            for argument in arguments {
                collect_test_snapshot_contexts(argument, into);
            }
        }
        _ => {}
    }
}

#[test]
fn omitted_generic_argument_rollback_keeps_sibling_depth_identities() {
    let wrapped = |arguments| Type::Applied {
        base: "domain.CheckpointBundle".into(),
        arguments,
    };
    let omitted_pair = || Type::Tuple(vec![Type::Bottom, Type::Bottom]);
    let pin_pair = |left: &str, right: &str| {
        Type::Tuple(vec![
            contextual_snapshot_ref(left),
            contextual_snapshot_ref(right),
        ])
    };
    let anchor = wrapped(vec![omitted_pair(), omitted_pair()]);
    let accumulated = wrapped(vec![
        pin_pair("selector:HEAD~101", "selector:HEAD~102"),
        pin_pair("selector:HEAD~201", "selector:HEAD~202"),
    ]);
    let parent = wrapped(vec![
        pin_pair("selector:HEAD~102", "selector:HEAD~101"),
        pin_pair("selector:HEAD~301", "selector:HEAD~302"),
    ]);
    let mut rolled_back_paths = BTreeSet::new();
    let folded = merge_multi_parent_checkpoint_value_scoped(
        &accumulated,
        &parent,
        &anchor,
        &anchor,
        &mut Vec::new(),
        &mut rolled_back_paths,
    )
    .expect("same applied wrapper and tuple widths should fold transactionally");
    let Type::Applied { arguments, .. } = folded.merged else {
        panic!("the successful sibling argument must remain computed");
    };
    assert_eq!(
        arguments[0],
        omitted_pair(),
        "the crossed argument rolls back"
    );
    assert_eq!(
        arguments[1],
        Type::Tuple(vec![
            Type::Applied {
                base: "semantic.SnapshotContextMap".into(),
                arguments: vec![
                    Type::Named("selector:HEAD~201".into()),
                    Type::Named("selector:HEAD~301".into()),
                ],
            },
            Type::Applied {
                base: "semantic.SnapshotContextMap".into(),
                arguments: vec![
                    Type::Named("selector:HEAD~202".into()),
                    Type::Named("selector:HEAD~302".into()),
                ],
            },
        ]),
        "the unaffected applied argument retains real folded depth identities"
    );
    assert_eq!(
        folded.effective_parent,
        wrapped(vec![
            omitted_pair(),
            pin_pair("selector:HEAD~301", "selector:HEAD~302"),
        ]),
        "the rejected argument is removed from the effective parent before later folds"
    );
    assert!(
        folded.rejected_scope,
        "a rejected argument must be reported"
    );
    assert_eq!(
        rolled_back_paths,
        BTreeSet::from([vec![SnapshotTopologyBoundary::AppliedArgument {
            base: "domain.CheckpointBundle".into(),
            index: 0,
        }]]),
        "rollback is frozen at the smallest failing applied-argument boundary"
    );
}

#[test]
fn omitted_money_dimension_rollback_keeps_sibling_depth_identities() {
    let omitted_pair = || Type::Tuple(vec![Type::Bottom, Type::Bottom]);
    let pin_pair = |left: &str, right: &str| {
        Type::Tuple(vec![
            contextual_snapshot_ref(left),
            contextual_snapshot_ref(right),
        ])
    };
    let money = |currency, unit| Type::MoneyPerUnit {
        currency: Box::new(currency),
        unit: Box::new(unit),
    };
    let anchor = money(omitted_pair(), omitted_pair());
    let accumulated = money(
        pin_pair("selector:HEAD~401", "selector:HEAD~402"),
        pin_pair("selector:HEAD~501", "selector:HEAD~502"),
    );
    let parent = money(
        pin_pair("selector:HEAD~402", "selector:HEAD~401"),
        pin_pair("selector:HEAD~601", "selector:HEAD~602"),
    );
    let mut rolled_back_paths = BTreeSet::new();
    let folded = merge_multi_parent_checkpoint_value_scoped(
        &accumulated,
        &parent,
        &anchor,
        &anchor,
        &mut Vec::new(),
        &mut rolled_back_paths,
    )
    .expect("money dimensions should recover independently");

    let Type::MoneyPerUnit { currency, unit } = folded.merged else {
        panic!("the money result must retain its computed type");
    };
    assert_eq!(*currency, omitted_pair(), "the crossed currency rolls back");
    assert_eq!(
        *unit,
        Type::Tuple(vec![
            Type::Applied {
                base: "semantic.SnapshotContextMap".into(),
                arguments: vec![
                    Type::Named("selector:HEAD~501".into()),
                    Type::Named("selector:HEAD~601".into()),
                ],
            },
            Type::Applied {
                base: "semantic.SnapshotContextMap".into(),
                arguments: vec![
                    Type::Named("selector:HEAD~502".into()),
                    Type::Named("selector:HEAD~602".into()),
                ],
            },
        ]),
        "the unaffected unit keeps both computed depth identities"
    );
    assert_eq!(
        folded.effective_parent,
        money(
            omitted_pair(),
            pin_pair("selector:HEAD~601", "selector:HEAD~602")
        ),
        "the rejected currency is removed from the effective parent before later folds"
    );
    assert!(folded.rejected_scope);
    assert_eq!(
        rolled_back_paths,
        BTreeSet::from([vec![SnapshotTopologyBoundary::MoneyCurrency]]),
        "rollback is frozen at the smallest failing money dimension"
    );
}

#[test]
fn checkpoint_rebinds_require_canonical_snapshot_selector_maps() {
    let first = Type::Applied {
        base: "semantic.SnapshotContextMap".into(),
        arguments: vec![
            Type::Named("selector:HEAD~5".into()),
            Type::Named("selector:binder:first".into()),
        ],
    };
    let second = Type::Applied {
        base: "semantic.SnapshotContextMap".into(),
        arguments: vec![
            Type::Named("selector:HEAD~4".into()),
            Type::Named("selector:binder:second".into()),
        ],
    };
    let wider = Type::Applied {
        base: "semantic.SnapshotContextMap".into(),
        arguments: vec![
            Type::Named("selector:HEAD~4".into()),
            Type::Named("selector:HEAD~5".into()),
            Type::Named("selector:HEAD~6".into()),
        ],
    };
    let malformed = Type::Applied {
        base: "semantic.SnapshotContextMap".into(),
        arguments: vec![Type::Named("domain.Factory".into())],
    };
    let singleton = Type::Applied {
        base: "semantic.SnapshotContextMap".into(),
        arguments: vec![Type::Named("selector:HEAD~3".into())],
    };
    let unsorted = Type::Applied {
        base: "semantic.SnapshotContextMap".into(),
        arguments: vec![
            Type::Named("selector:binder:z".into()),
            Type::Named("selector:binder:a".into()),
        ],
    };
    let duplicate = Type::Applied {
        base: "semantic.SnapshotContextMap".into(),
        arguments: vec![
            Type::Named("selector:HEAD~3".into()),
            Type::Named("selector:HEAD~3".into()),
        ],
    };
    let nested_malformed = Type::Record(BTreeMap::from([
        ("pin".into(), contextual_snapshot_ref("selector:HEAD~3")),
        ("map".into(), malformed.clone()),
    ]));

    assert!(is_snapshot_context_map_shape(&first));
    assert!(types_match(&first, &first));
    assert!(types_match(&first, &Type::Bottom));
    assert!(pinned_snapshot_shape_matches(&first, &first));
    assert!(pinned_snapshot_shape_matches(&first, &second));
    assert!(!pinned_snapshot_shape_matches(&first, &wider));
    assert!(!pinned_snapshot_shape_matches(
        &contextual_snapshot_ref("selector:HEAD~3"),
        &first
    ));
    assert!(type_contains_pinned_snapshot_identity(&first));
    assert!(pinned_snapshot_reset_type(&first, &second).is_some());
    assert!(pinned_snapshot_reset_type(&first, &wider).is_none());
    let terminal_factory = Type::Function {
        parameters: vec![],
        parameter_names: Some(vec![]),
        default_parameters: BTreeSet::new(),
        result: Box::new(Type::Named("domain.Factory".into())),
    };
    let first_pinned_factory = historical_callable_type(&first, &terminal_factory);
    let second_pinned_factory = historical_callable_type(&second, &terminal_factory);
    let wider_pinned_factory = historical_callable_type(&wider, &terminal_factory);
    assert!(pinned_snapshot_shape_matches(
        &first_pinned_factory,
        &second_pinned_factory
    ));
    assert!(!pinned_snapshot_shape_matches(
        &first_pinned_factory,
        &wider_pinned_factory
    ));
    assert!(pinned_snapshot_reset_type(&first_pinned_factory, &second_pinned_factory).is_some());
    assert!(pinned_snapshot_reset_type(&first_pinned_factory, &wider_pinned_factory).is_none());
    assert!(!is_snapshot_context_map_shape(&malformed));
    assert!(!types_match(&malformed, &malformed));
    assert!(!types_match(&malformed, &Type::Bottom));
    assert!(!pinned_snapshot_shape_matches(&malformed, &malformed));
    assert!(!type_contains_pinned_snapshot_identity(&malformed));
    assert!(pinned_snapshot_reset_type(&malformed, &second).is_none());
    assert!(!is_snapshot_context_map_shape(&singleton));
    assert!(!is_snapshot_context_map_shape(&unsorted));
    assert!(!is_snapshot_context_map_shape(&duplicate));
    assert_eq!(
        merge_list_element_types(&first, &first),
        Some(first.clone())
    );
    assert!(merge_list_element_types(&malformed, &malformed).is_none());
    assert!(merge_checkpoint_field_map(&malformed, &malformed).is_none());
    assert!(merge_checkpoint_field_map(&malformed, &first).is_none());
    assert!(!checkpoint_snapshot_maps_are_valid(&nested_malformed));
    assert!(!types_match(&nested_malformed, &nested_malformed));
    assert!(!types_match(&nested_malformed, &Type::Bottom));
    assert!(!pinned_snapshot_shape_matches(
        &nested_malformed,
        &nested_malformed
    ));
    assert!(!type_contains_pinned_snapshot_identity(&nested_malformed));
    assert!(merge_list_element_types(&nested_malformed, &nested_malformed).is_none());
    assert!(merge_checkpoint_field_map(&nested_malformed, &nested_malformed).is_none());
    assert!(pinned_snapshot_reset_type(&nested_malformed, &nested_malformed).is_none());
}

#[test]
fn checkpoint_resets_preserve_nested_callable_input_contracts() {
    let callable = |parameter: Type, result_selector: &str| Type::Function {
        parameters: vec![parameter],
        parameter_names: Some(vec!["nested_pin".into()]),
        default_parameters: BTreeSet::new(),
        result: Box::new(contextual_snapshot_ref(result_selector)),
    };
    let concrete_input = callable(
        contextual_snapshot_ref("selector:input:checkpoint"),
        "selector:result:checkpoint",
    );
    let omitted_input = callable(Type::Bottom, "selector:result:omitted-input");

    assert!(pinned_snapshot_reset_type(&omitted_input, &concrete_input).is_some());
    assert!(
        pinned_snapshot_reset_type(&concrete_input, &omitted_input).is_none(),
        "restoring a nested checkpoint must not remove an input boundary that the local accepted"
    );
}

#[test]
fn tuple_checkpoint_rebind_preserves_full_selector_membership_signatures() {
    let context_map = |selectors: &[&str]| Type::Applied {
        base: "semantic.SnapshotContextMap".into(),
        arguments: selectors
            .iter()
            .map(|selector| Type::Named((*selector).into()))
            .collect(),
    };
    let saved = vec![
        context_map(&["selector:pin:a", "selector:pin:b"]),
        context_map(&["selector:pin:a", "selector:pin:c"]),
        context_map(&["selector:pin:b", "selector:pin:c"]),
    ];
    let relabeled = vec![
        context_map(&["selector:pin:d", "selector:pin:e"]),
        context_map(&["selector:pin:d", "selector:pin:f"]),
        context_map(&["selector:pin:d", "selector:pin:g"]),
    ];
    let same_topology = vec![
        context_map(&["selector:pin:u", "selector:pin:v"]),
        context_map(&["selector:pin:u", "selector:pin:w"]),
        context_map(&["selector:pin:v", "selector:pin:w"]),
    ];
    let pairwise_overlaps = |maps: &[Type]| {
        let selector_sets = maps
            .iter()
            .map(|map| {
                let Type::Applied { arguments, .. } = map else {
                    panic!("checkpoint selector map must be applied");
                };
                arguments
                    .iter()
                    .filter_map(|argument| match argument {
                        Type::Named(selector) => Some(selector.clone()),
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>()
            })
            .collect::<Vec<_>>();
        let mut overlaps = Vec::new();
        for index in 0..selector_sets.len() {
            for other in index + 1..selector_sets.len() {
                overlaps.push(
                    selector_sets[index]
                        .intersection(&selector_sets[other])
                        .count(),
                );
            }
        }
        overlaps
    };

    assert!(saved.iter().all(is_snapshot_context_map_shape));
    assert!(relabeled.iter().all(is_snapshot_context_map_shape));
    assert_eq!(pairwise_overlaps(&saved), vec![1, 1, 1]);
    assert_eq!(pairwise_overlaps(&relabeled), vec![1, 1, 1]);
    assert!(tuple_pin_identity_topology_matches(&saved, &same_topology));
    assert!(!tuple_pin_identity_topology_matches(&saved, &relabeled));
    assert!(!pinned_snapshot_shape_matches(
        &Type::Tuple(saved),
        &Type::Tuple(relabeled)
    ));
}

#[test]
fn record_checkpoint_compaction_rejects_cross_depth_selector_aliases() {
    let pin = |selector: &str| contextual_snapshot_ref(selector);
    let stacked = |root: &str, middle: &str, leaf: &str| {
        BTreeMap::from([
            ("root".into(), pin(root)),
            (
                "stack".into(),
                Type::Tuple(vec![
                    pin(middle),
                    Type::Record(BTreeMap::from([("leaf".into(), pin(leaf))])),
                ]),
            ),
        ])
    };
    let first = stacked(
        "selector:HEAD~501",
        "selector:HEAD~502",
        "selector:HEAD~503",
    );
    let cross_depth = stacked(
        "selector:HEAD~504",
        "selector:HEAD~501",
        "selector:HEAD~505",
    );
    let depth_stable = stacked(
        "selector:HEAD~601",
        "selector:HEAD~602",
        "selector:HEAD~603",
    );

    assert!(record_checkpoint_promotion_matches(&first, &depth_stable));
    assert!(!record_checkpoint_promotion_matches(&first, &cross_depth));
    assert!(merge_list_element_types(&Type::Record(first), &Type::Record(cross_depth)).is_none());
}

#[test]
fn paired_omission_folds_do_not_collapse_distinct_depth_labels() {
    let pin = |selector: &str| contextual_snapshot_ref(selector);
    let before = [Type::Tuple(vec![
        pin("selector:HEAD~depth-a"),
        pin("selector:HEAD~depth-b"),
    ])];
    let after = [Type::Tuple(vec![
        Type::Bottom,
        pin("selector:HEAD~depth-a"),
    ])];

    assert!(checkpoint_pin_map_widths_match(&before[0], &after[0]));
    assert!(
        !tuple_checkpoint_compaction_fold_preserves_pin_identity(&before, &after),
        "an omitted first slot must not hide the later fold merging depth-a with depth-b"
    );

    let stable_after = [Type::Tuple(vec![
        Type::Bottom,
        pin("selector:HEAD~depth-c"),
    ])];
    assert!(tuple_checkpoint_compaction_fold_preserves_pin_identity(
        &before,
        &stable_after
    ));
}

fn checked(inputs: &[ModuleInput]) -> Analysis {
    analyze(inputs)
}
/// Analysis as an evaluating host admits it: against production providers.
fn admitted(inputs: &[ModuleInput]) -> Analysis {
    analyze_with_providers(inputs, &Catalogue::empty(), SystemProviders::Production)
}
fn has(analysis: &Analysis, code: &str) -> bool {
    analysis.diagnostics.iter().any(|d| d.code() == code)
}
/// Every diagnostic as `code: message`. Diagnostic `Debug` output is
/// redacted, so tests compare these texts instead.
fn diagnostic_texts(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
        .collect()
}
/// The admission diagnostic for a portable member without a linked
/// production provider.
fn unlinked(member: &str) -> String {
    format!(
        "{DIAG_SYS_VERSION_INCOMPATIBLE}: {member} has no provider linked in this implementation"
    )
}
#[test]
fn boolean_case_is_exhaustive_only_with_unguarded_coverage() {
    let source = include_str!("../tests/fixtures/boolean_case_exhaustiveness.orna");
    let analysis = checked(&[ModuleInput::new("boolean-case.orna", source)]);

    assert!(
        !has(&analysis, DIAG_UNSUPPORTED),
        "Bool is a closed case scrutinee: {:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis.diagnostics.len(),
        1,
        "diagnostics: {:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis.diagnostics[0].message(),
        "case expression does not cover true and false"
    );
    let module = analysis
        .modules
        .values()
        .find(|module| module.symbols.contains_key("choose"))
        .expect("fixture module is analyzed");
    assert_eq!(
        module.symbols["choose"].ty,
        Type::Function {
            parameters: vec![Type::Bool],
            parameter_names: Some(vec!["flag".into()]),
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::Int),
        }
    );
    assert_eq!(
        module.symbols["capture"].ty,
        Type::Function {
            parameters: vec![Type::Bool],
            parameter_names: Some(vec!["flag".into()]),
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::Bool),
        }
    );
}

#[test]
fn case_guards_do_not_establish_exhaustive_coverage() {
    let source = include_str!("../tests/fixtures/guarded_case_exhaustiveness.orna");
    let analysis = checked(&[ModuleInput::new("guarded-case.orna", source)]);

    assert!(
        !has(&analysis, DIAG_UNSUPPORTED),
        "guards are supported and type-checked: {:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message()
                == "case expression must cover Some and null exactly once")
            .count(),
        1,
        "the guarded-only Some arm must not count as exhaustive: {:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message()
                == "case expression does not cover every enum variant")
            .count(),
        1,
        "guarded enum variants must not establish coverage: {:?}",
        analysis.diagnostics
    );
    assert_eq!(
        analysis.diagnostics.len(),
        2,
        "unguarded optional and enum fallbacks must remain exhaustive: {:?}",
        analysis.diagnostics
    );
    let module = analysis
        .modules
        .values()
        .find(|module| module.symbols.contains_key("positive_or_zero"))
        .expect("fixture module is analyzed");
    assert_eq!(
        module.symbols["positive_or_zero"].ty,
        Type::Function {
            parameters: vec![Type::Optional(Box::new(Type::Int))],
            parameter_names: Some(vec!["value".into()]),
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::Int),
        }
    );
    assert_eq!(
        module.symbols["classify"].ty,
        Type::Function {
            parameters: vec![Type::Named("State".into())],
            parameter_names: Some(vec!["value".into()]),
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::Int),
        }
    );
}

#[test]
fn maps_main_and_leaf_paths() {
    let a = checked(&[
        ModuleInput::new("sensors/greenhouse/main.orna", ""),
        ModuleInput::new("sensors/greenhouse/input.orna", ""),
    ]);
    assert!(
        a.modules
            .contains_key(&Namespace(vec!["sensors".into(), "greenhouse".into()]))
    );
    assert!(a.modules.contains_key(&Namespace(vec![
        "sensors".into(),
        "greenhouse".into(),
        "input".into()
    ])));
}
#[test]
fn rejects_duplicate_namespace_and_unicode_siblings() {
    let a = checked(&[
        ModuleInput::new("x.orna", ""),
        ModuleInput::new("x/main.orna", ""),
        ModuleInput::new("Café/a.orna", ""),
    ]);
    assert!(has(&a, DIAG_NAMESPACE));
    assert!(has(&a, DIAG_BAD_PATH));
}
#[test]
fn reserved_roots_cannot_be_source_modules() {
    let a = checked(&[
        ModuleInput::new("sys/main.orna", ""),
        ModuleInput::new("std/main.orna", ""),
    ]);
    assert!(has(&a, DIAG_RESERVED));
}

#[test]
fn reserved_sys_table_reaches_typechecking_without_shadowing_the_portable_root() {
    let analysis = checked(&[ModuleInput::new(
        "examples/invalid/reserved-sys.orna",
        "pub table sys { value: Int, } pub fn view() = sys.rt.id;",
    )]);

    assert!(
        !has(&analysis, DIAG_RESERVED),
        "table-name validation must not fail during header collection: {:?}",
        analysis.diagnostics
    );
    assert!(analysis.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_RESERVED_SYS && diagnostic.message() == "`sys` is reserved"
    }));
    assert!(
        !has(&analysis, DIAG_UNRESOLVED),
        "the invalid table must not hide the portable root: {:?}",
        analysis.diagnostics
    );
}

#[test]
fn standard_dependency_profile_requires_exact_pinned_source_bytes() {
    let profile = StandardDependencyProfile::from_sources(
        "std-snapshot-1",
        [("std/math.orna".into(), "fn increment() = 1;".into())],
    )
    .unwrap();

    assert_eq!(profile.snapshot(), "std-snapshot-1");
    assert_eq!(profile.module_digests().len(), 1);
    assert_eq!(
        profile.verify_source("std/math.orna", "fn increment() = 1;"),
        Ok(())
    );
    assert_eq!(
        profile.verify_source("std/math.orna", "fn increment() = 2;"),
        Err(StandardProfileError::DigestMismatch)
    );
    assert_eq!(
        profile.verify_source("std/text.orna", "fn trim() = 1;"),
        Err(StandardProfileError::MissingModule)
    );
}

#[test]
fn standard_dependency_profile_rejects_unsafe_or_duplicate_modules() {
    assert_eq!(
        StandardDependencyProfile::empty(""),
        Err(StandardProfileError::EmptySnapshot)
    );
    assert_eq!(
        StandardDependencyProfile::from_sources(
            "std-snapshot-1",
            [("project/math.orna".into(), "fn f() = 1;".into())],
        ),
        Err(StandardProfileError::InvalidModulePath)
    );
    assert_eq!(
        StandardDependencyProfile::from_sources(
            "std-snapshot-1",
            [("std/hidden.module.orna".into(), "fn f() = 1;".into())],
        ),
        Err(StandardProfileError::InvalidModulePath)
    );
    assert_eq!(
        StandardDependencyProfile::from_sources(
            "std-snapshot-1",
            [
                ("std/math.orna".into(), "fn f() = 1;".into()),
                ("std/math.orna".into(), "fn f() = 1;".into()),
            ],
        ),
        Err(StandardProfileError::DuplicateModule)
    );
}

#[test]
fn pinned_standard_sources_become_a_semantic_catalogue_without_execution() {
    let source = "pub fn increment(value: Int): Int = value + 1;";
    let profile = StandardDependencyProfile::from_sources(
        "std-snapshot-1",
        [("std/math.orna".into(), source.into())],
    )
    .unwrap();
    let catalogue =
        Catalogue::from_standard_sources(&profile, [("std/math.orna".into(), source.into())])
            .unwrap();
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "client.orna",
            "use std.math.{increment}; fn next(value: Int): Int = increment(value);",
        )],
        &catalogue,
    );
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
    assert_eq!(
        Catalogue::from_standard_sources(
            &profile,
            [("std/math.orna".into(), "pub fn broken( = 1;".into())],
        ),
        Err(StandardCatalogueError::Profile(
            StandardProfileError::DigestMismatch
        ))
    );
    assert_eq!(
        Catalogue::from_standard_sources(&profile, []),
        Err(StandardCatalogueError::MissingModule)
    );
}

#[test]
fn verified_standard_sources_replace_matching_core_modules() {
    let source = "pub fn increment(value: Int): Int = value + 1;";
    let profile = StandardDependencyProfile::from_sources(
        "std-snapshot-1",
        [("std/math.orna".into(), source.into())],
    )
    .unwrap();
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, [("std/math.orna".into(), source.into())])
        .unwrap();
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "client.orna",
            "use std.math.{increment}; fn next(value: Int): Int = increment(value);",
        )],
        &catalogue,
    );
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
    assert_eq!(
        Catalogue::authoritative_core().with_standard_sources(
            &profile,
            [("std/math.orna".into(), "pub fn broken( = 1;".into())],
        ),
        Err(StandardCatalogueError::Profile(
            StandardProfileError::DigestMismatch
        ))
    );
}

#[test]
fn pinned_standard_root_prelude_is_explicit_and_source_defined() {
    let source = "pub fn answer(): Int = 42;";
    let profile = StandardDependencyProfile::from_sources(
        "std-snapshot-1",
        [("std/main.orna".into(), source.into())],
    )
    .unwrap()
    .with_prelude_exports(["answer"]);
    let catalogue =
        Catalogue::from_standard_sources(&profile, [("std/main.orna".into(), source.into())])
            .unwrap();
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "client.orna",
            "use std as _; pub fn run(): Int = answer();",
        )],
        &catalogue,
    );
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);

    let invalid = StandardDependencyProfile::from_sources(
        "std-snapshot-1",
        [("std/main.orna".into(), source.into())],
    )
    .unwrap()
    .with_prelude_exports(["missing"]);
    assert_eq!(
        Catalogue::from_standard_sources(&invalid, [("std/main.orna".into(), source.into())]),
        Err(StandardCatalogueError::Profile(
            StandardProfileError::InvalidPreludeExport,
        ))
    );
}

#[test]
fn pinned_standard_module_prelude_uses_only_its_recorded_exports() {
    let root = "pub fn root_only(): Int = 7;";
    let prelude = "pub fn curated(): Int = 42; pub fn module_only(): Int = 9;";
    let sources = [
        ("std/main.orna".to_owned(), root.to_owned()),
        ("std/prelude.orna".to_owned(), prelude.to_owned()),
    ];
    let profile = StandardDependencyProfile::from_sources("std-snapshot-2", sources.clone())
        .unwrap()
        .with_module_prelude_exports("std/prelude.orna", ["curated"]);
    let catalogue = Catalogue::from_standard_sources(&profile, sources.clone()).unwrap();

    let curated = analyze_with_catalogue(
        &[ModuleInput::new(
            "client.orna",
            "use std.prelude as _; pub fn run(): Int = curated();",
        )],
        &catalogue,
    );
    assert!(curated.is_ok(), "{:#?}", curated.diagnostics);

    let not_curated = analyze_with_catalogue(
        &[ModuleInput::new(
            "client.orna",
            "use std.prelude as _; pub fn run(): Int = module_only();",
        )],
        &catalogue,
    );
    assert!(
        has(&not_curated, DIAG_UNRESOLVED),
        "{:#?}",
        not_curated.diagnostics
    );

    let invalid = StandardDependencyProfile::from_sources("std-snapshot-2", sources.clone())
        .unwrap()
        .with_module_prelude_exports("std/prelude.orna", ["missing"]);
    assert_eq!(
        Catalogue::from_standard_sources(&invalid, sources),
        Err(StandardCatalogueError::Profile(
            StandardProfileError::InvalidPreludeExport,
        ))
    );
}

#[test]
fn descriptor_admin_defaults_keep_version_required() {
    for (operation, status) in [("retry_failure", "open"), ("replay_failure", "skipped")] {
        let valid = checked(&[ModuleInput::new(
            "failure-cas.orna",
            format!(
                "fn action(failure: sys.Failure) = sys.admin.{operation}(failure.reference, expected_version: failure.version, expected_status: sys.FailureStatus.{status});"
            ),
        )]);
        assert!(valid.is_ok(), "{operation}: {:#?}", valid.diagnostics);
        // The call types, but no production provider is linked for the
        // failure administration callables (ORNA-SYS-008).
        let admitted_call = admitted(&[ModuleInput::new(
            "failure-cas.orna",
            format!(
                "fn action(failure: sys.Failure) = sys.admin.{operation}(failure.reference, expected_version: failure.version, expected_status: sys.FailureStatus.{status});"
            ),
        )]);
        assert_eq!(
            diagnostic_texts(&admitted_call.diagnostics),
            [unlinked(&format!(
                "portable system callable `sys.admin.{operation}`"
            ))],
        );

        let missing_version = checked(&[ModuleInput::new(
            "failure-cas.orna",
            format!(
                "fn action(failure: sys.Failure) = sys.admin.{operation}(failure.reference, expected_status: sys.FailureStatus.{status});"
            ),
        )]);
        assert!(
            has(&missing_version, DIAG_TYPE),
            "{operation}: {:#?}",
            missing_version.diagnostics
        );
    }
}

/// Portable `sys` members without a linked production provider are rejected with
/// `sys.version.incompatible` before evaluation can perform writes (ORNA-SYS-008).
#[test]
fn unlinked_sys_members_are_rejected_with_version_incompatible() {
    for (source, member) in [
        (
            "pub fn head() = sys.snapshot(\"HEAD~1\");",
            "portable system callable `sys.snapshot(Str)`",
        ),
        (
            "pub fn metadata() = sys.meta(1);",
            "portable system callable `sys.meta`",
        ),
        (
            "pub fn plan(function: sys.FunctionRef) = sys.explain(function);",
            "portable system callable `sys.explain(FunctionRef)`",
        ),
        (
            "pub fn plans() = sys.Plan;",
            "portable system relation `sys.Plan`",
        ),
        (
            "pub fn runs() = sys.Run;",
            "portable system relation `sys.Run`",
        ),
        (
            "pub fn objects() = sys.storage.objects;",
            "portable system relation `sys.storage.objects`",
        ),
        (
            "pub fn owner() = sys.rt.id;",
            "portable system singleton `sys.rt`",
        ),
        (
            "pub fn published() = sys.Storage.as_of(HEAD);",
            "historical view `sys.Storage.as_of`",
        ),
        (
            "pub fn old() = sys.database.as_of(HEAD);",
            "historical view `sys.database.as_of`",
        ),
    ] {
        let declared = checked(&[ModuleInput::new("main.orna", source)]);
        assert!(
            declared.is_ok(),
            "{source}: {:#?}",
            diagnostic_texts(&declared.diagnostics)
        );
        let analysis = admitted(&[ModuleInput::new("main.orna", source)]);
        assert_eq!(
            diagnostic_texts(&analysis.diagnostics),
            [unlinked(member)],
            "{source}"
        );
    }
}

/// Linked `sys` members are admitted. `sys.rt.info()` is one of them: ORNA-SYS-006
/// requires the runtime to expose the RuntimeInfo coordinates, which the production
/// host links (source/15-system.md:46).
#[test]
fn linked_sys_members_are_admitted() {
    let analysis = admitted(&[ModuleInput::new(
        "linked.orna",
        "pub fn label(value: Blob): Blob = sys.blob.annotate(value, \"text/plain\");
        pub fn capture(): Blob = sys.blob.capture_file(\"data\", \"input.bin\", 1024);
        pub fn pause(stream: sys.StreamRef): Bool = sys.admin.pause_stream(stream);
        pub fn resume(stream: sys.StreamRef): Bool = sys.admin.resume_stream(stream);
        pub fn storage() = sys.Storage;
        pub fn maintenance() = sys.MaintenanceJob;
        pub fn info() = sys.rt.info();
        pub fn plan() = sys.Storage | sys.explain();",
    )]);
    assert!(
        analysis.is_ok(),
        "{:#?}",
        diagnostic_texts(&analysis.diagnostics)
    );
}

#[test]
fn each_unlinked_sys_member_is_reported_once_per_analysis() {
    let analysis = admitted(&[
        ModuleInput::new(
            "a.orna",
            "pub fn first() = sys.snapshot(\"HEAD~1\"); pub fn second() = sys.snapshot(\"HEAD~1\");",
        ),
        ModuleInput::new("b.orna", "pub fn third() = sys.snapshot(\"HEAD~1\");"),
    ]);
    assert_eq!(
        diagnostic_texts(&analysis.diagnostics),
        [unlinked("portable system callable `sys.snapshot(Str)`")]
    );
}

#[test]
fn provider_linkage_leaves_type_checking_unchanged() {
    let source = [ModuleInput::new(
        "typed.orna",
        "pub fn head(): Int = sys.snapshot(\"HEAD~1\");",
    )];
    let declared = diagnostic_texts(&checked(&source).diagnostics);
    let mut production = diagnostic_texts(&admitted(&source).diagnostics);
    assert!(
        declared.iter().any(|text| text.starts_with(DIAG_TYPE)),
        "{declared:#?}"
    );
    let unlinked_member = unlinked("portable system callable `sys.snapshot(Str)`");
    assert!(production.contains(&unlinked_member), "{production:#?}");
    production.retain(|text| *text != unlinked_member);
    assert_eq!(production, declared);
}

#[test]
fn repl_admission_rejects_unlinked_sys_members() {
    let rejected = orna_syntax::parse_repl("sys.snapshot(\"HEAD~1\")");
    assert!(rejected.is_ok());
    let diagnostics = ReplContext::with_providers(SystemProviders::Production)
        .stage(&rejected.value)
        .expect_err("no production provider is linked for sys.snapshot(Str)");
    assert_eq!(
        diagnostic_texts(&diagnostics),
        [unlinked("portable system callable `sys.snapshot(Str)`")]
    );

    assert!(ReplContext::empty().stage(&rejected.value).is_ok());
    let project = admitted(&[ModuleInput::new("main.orna", "pub fn one(): Int = 1;")]);
    assert!(
        ReplContext::from_analysis(&project)
            .unwrap()
            .stage(&rejected.value)
            .is_err(),
        "a REPL built from a production analysis keeps its provider linkage"
    );

    let admitted_input = orna_syntax::parse_repl("sys.Storage");
    assert!(admitted_input.is_ok());
    assert!(
        ReplContext::with_providers(SystemProviders::Production)
            .stage(&admitted_input.value)
            .is_ok()
    );
}

#[test]
fn named_alias_glob_and_prelude_imports_resolve() {
    let source = "pub fn f(x: Int): Int = x; pub fn p(): Bool = true;";
    let a = checked(&[
        ModuleInput::new("lib.orna", source).with_prelude_exports(["p"]),
        ModuleInput::new(
            "client.orna",
            "use lib.{f}; use lib as l; use lib as _; fn g(x: Int) = f(x);",
        ),
    ]);
    assert!(!has(&a, DIAG_IMPORT), "{:?}", a.diagnostics);
    assert!(!has(&a, DIAG_UNRESOLVED));
}
#[test]
fn wildcard_ambiguity_is_stable() {
    let a = checked(&[
        ModuleInput::new("a.orna", "pub fn f(): Int = 1;"),
        ModuleInput::new("b.orna", "pub fn f(): Int = 2;"),
        ModuleInput::new("c.orna", "use a.*; use b.*; fn g() = f();"),
    ]);
    assert!(has(&a, DIAG_AMBIGUOUS));
    let codes = a.diagnostics.iter().map(|d| d.code()).collect::<Vec<_>>();
    assert_eq!(codes, {
        let mut x = codes.clone();
        x.sort();
        x
    });
}
#[test]
fn unresolved_name_and_omitted_parameter_annotation_are_diagnostics() {
    let a = checked(&[ModuleInput::new("m.orna", "fn f(x) = missing;")]);
    assert!(has(&a, DIAG_ANNOTATION));
    assert!(has(&a, DIAG_UNRESOLVED));
}

#[test]
fn source_bindings_shadow_the_now_intrinsic() {
    let function_analysis = checked(&[ModuleInput::new(
        "function.orna",
        "fn now(): Int = 7; fn use_now(): Int = now();",
    )]);
    assert!(
        function_analysis.is_ok(),
        "{:#?}",
        function_analysis.diagnostics
    );
    assert_eq!(
        function_analysis.modules.values().next().unwrap().symbols["use_now"].ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::Int),
        }
    );

    let value_analysis = checked(&[ModuleInput::new(
        "value.orna",
        "fn use_now(now: Int): Int = now;",
    )]);
    assert!(value_analysis.is_ok(), "{:#?}", value_analysis.diagnostics);
    assert_eq!(
        value_analysis.modules.values().next().unwrap().symbols["use_now"].ty,
        Type::Function {
            parameters: vec![Type::Int],
            parameter_names: Some(vec!["now".into()]),
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::Int),
        }
    );
}

#[test]
fn infers_primitive_record_list_and_function_without_any() {
    let a = checked(&[ModuleInput::new(
        "m.orna",
        "fn record() = { a: 1, }; fn list() = [1, 2]; fn f(x: Int) = x;",
    )]);
    let m = a.modules.values().next().unwrap();
    assert_eq!(
        m.symbols.get("record").expect("record is collected").ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::Record(BTreeMap::from([("a".into(), Type::Int)])))
        }
    );
    assert_eq!(
        m.symbols.get("list").expect("list is collected").ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::List(Box::new(Type::Int)))
        }
    );
    assert!(!has(&a, DIAG_TYPE));
}

#[test]
fn nested_optional_query_first_coalesces_with_integer_fallback() {
    let analysis = checked(&[ModuleInput::new(
        "query-coalesce.orna",
        include_str!("../tests/fixtures/nested_optional_query_coalesce.orna"),
    )]);

    assert!(analysis.is_ok(), "diagnostics: {:?}", analysis.diagnostics);
}

#[test]
fn empty_lambda_blocks_match_empty_record_literals() {
    let a = checked(&[ModuleInput::new(
        "m.orna",
        "pub fn makers() = [() => ({}), () => {},];",
    )]);
    assert!(
        !has(&a, DIAG_TYPE),
        "empty lambda records: {:?}",
        a.diagnostics
    );
    let module = a.modules.values().next().expect("module is present");
    let symbol = module.symbols.get("makers").expect("makers is collected");
    assert_eq!(
        symbol.ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::List(Box::new(Type::Function {
                parameters: vec![],
                parameter_names: Some(vec![]),
                default_parameters: BTreeSet::new(),
                result: Box::new(Type::Record(BTreeMap::new())),
            }))),
        }
    );
}

#[test]
fn duplicate_durable_consumers_are_rejected_before_execution() {
    let a = analyze_with_catalogue(
        &[ModuleInput::new(
            "m.orna",
            "pub fn main() = parallel([mail.google.sync, mail.google.sync]);",
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(a.diagnostics.iter().any(|diagnostic| {
        diagnostic.message()
            == "same durable source consumed twice without distinct consumer identity"
    }));
}

#[test]
fn durable_consumer_cannot_merge_multiple_source_roots() {
    let a = analyze_with_catalogue(
        &[ModuleInput::new(
            "m.orna",
            "pub fn main() = merge_streams([google.mail(), openbanking.transactions()]);",
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(a.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .starts_with("a durable consumer function may own only one checkpointed source root")
    }));
}

#[test]
fn fixture_provider_roots_reach_source_ownership_typechecking() {
    let a = analyze_with_catalogue(
        &[ModuleInput::new(
            "examples/invalid/two-durable-sources.orna",
            "pub fn main() = merge_streams([google.mail(), openbanking.transactions()]);",
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(
        !has(&a, DIAG_UNRESOLVED),
        "provider roots resolved: {:?}",
        a.diagnostics
    );
    assert!(a.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message()
            .starts_with("a durable consumer function may own only one checkpointed source root")
    }));
}

#[test]
fn multiple_checkpointed_roots_explain_the_required_refactoring() {
    let source = include_str!("../tests/fixtures/semantic_consumer_gap.orna");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "examples/invalid/semantic-consumer-gap.orna",
            source,
        )],
        &Catalogue::authoritative_fixture(),
    );

    assert!(
        !has(&analysis, DIAG_UNRESOLVED),
        "fixture provider roots must resolve before checking ownership: {:?}",
        analysis.diagnostics
    );
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("a durable consumer function may own only one checkpointed source root")
                && diagnostic
                    .message()
                    .contains("extract separate named consumer functions")
        }),
        "diagnostics: {:?}",
        analysis.diagnostics
    );
}

#[test]
fn assertion_plan_rejects_compile_time_boolean() {
    let a = checked(&[ModuleInput::new("m.orna", "assert true;")]);
    assert!(has(&a, DIAG_ASSERTION_SCOPE));
    assert_eq!(
        a.assertions.values().next().unwrap()[0].owner,
        AssertionOwner::Module
    );
}
#[test]
fn diagnostics_are_redacted_and_stable() {
    let a = checked(&[ModuleInput::new("secret.orna", "fn f() = missing;")]);
    let json = serde_json::to_string(&a.diagnostics[0]).unwrap();
    assert!(!json.contains("secret.orna"));
    assert!(!json.contains("missing"));
}

#[test]
fn shape_only_catalogue_does_not_invent_table_admission_rules() {
    let catalogue = Catalogue::authoritative_fixture();
    let order = &catalogue.attached_symbols["Order"];
    assert_eq!(order.ty, Type::Named("Order".into()));
    let schema = order.table_schema.as_ref().unwrap();
    assert!(schema.fields.contains_key("customer"));
    assert!(schema.admission.is_none());
    let contact = &catalogue.modules[&Namespace(vec!["contacts".into()])].exports["Contact"];
    assert_eq!(contact.ty, Type::Named("contacts.Contact".into()));
    let admission = contact
        .table_schema
        .as_ref()
        .unwrap()
        .admission
        .as_ref()
        .unwrap();
    assert_eq!(admission.required, BTreeSet::from(["name".into()]));
    assert_eq!(admission.computed, BTreeSet::from(["full_name".into()]));
    assert_eq!(admission.keys, vec![("id".into(), Type::Text)]);
    assert!(!admission.automatic_key);
}

#[test]
fn type_diagnostics_do_not_serialize_non_identifier_catalogue_names() {
    assert_eq!(
        diagnostic_type_name(&Type::Named("bad/type".into())),
        "<type>"
    );
    assert_eq!(
        diagnostic_type_name(&Type::Applied {
            base: "Float".into(),
            arguments: vec![Type::Named("bad/type".into())],
        }),
        "Float<<type>>"
    );
}
#[test]
fn ui_action_source_requires_explicit_provenance_and_input_type() {
    let catalogue = Catalogue::authoritative_core();
    let valid = analyze_with_catalogue(
        &[ModuleInput::new(
            "ui.orna",
            r#"fn render() = std.ui.action("save", as: Text, debug_kind: "button");"#,
        )],
        &catalogue,
    );
    assert!(valid.is_ok(), "{:#?}", valid.diagnostics);

    let missing_input = analyze_with_catalogue(
        &[ModuleInput::new(
            "ui.orna",
            r#"fn render() = std.ui.action("save");"#,
        )],
        &catalogue,
    );
    assert!(has(&missing_input, DIAG_TYPE));
}
#[test]
fn loop_break_requires_an_inferred_result_contract() {
    let analysis = checked(&[ModuleInput::new(
        "loop-break.orna",
        "fn invalid() { loop { break 1; } }",
    )]);
    assert!(analysis.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_UNSUPPORTED
            && diagnostic.message() == "break transfer requires a loop result contract"
    }));
}
/// `base[index]` has no type rule in the 1.1.0 grammar, and a bare `loop`
/// has no inferred result contract (ORNA-CFLOW-008 leaves its value to
/// `break`, which `validate_loop_transfers` rejects), so both are rejected
/// by name instead of falling through a catch-all arm.
#[test]
fn index_and_bare_loop_are_rejected_explicitly() {
    let analysis = checked(&[ModuleInput::new(
        "outside-slice.orna",
        "fn pick(rows: [Int]) = rows[0];\nfn spin() { loop { break; } }",
    )]);
    let outside = format!("{DIAG_UNSUPPORTED}: expression form is outside this semantic slice");
    assert_eq!(
        diagnostic_texts(&analysis.diagnostics)
            .iter()
            .filter(|text| *text == &outside)
            .count(),
        2,
        "{:?}",
        analysis.diagnostics
    );
}
/// A write through an attached database alias spans two independent
/// transaction logs, so it is rejected (ORNA-ATTACH-002); a read through
/// the same alias composes snapshots and stays admissible
/// (ORNA-ATTACH-001).
#[test]
fn writes_through_an_attached_database_are_rejected() {
    let catalogue = Catalogue::authoritative_fixture();
    let write = analyze_with_catalogue(
        &[ModuleInput::new(
            "cross-database-write.orna",
            "fn bad() = work.Contact.insert({ name: \"Alice\" });",
        )],
        &catalogue,
    );
    assert!(
        write.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_CROSS_DATABASE_WRITE
                && diagnostic.message() == "cross-database writes are not atomic/supported"
        }),
        "{:?}",
        write.diagnostics
    );
    let read = analyze_with_catalogue(
        &[ModuleInput::new(
            "cross-database-read.orna",
            "fn ok() = work.units.kWh;",
        )],
        &catalogue,
    );
    assert!(
        read.diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code() != DIAG_CROSS_DATABASE_WRITE),
        "{:?}",
        read.diagnostics
    );
}
#[test]
fn structured_parameter_patterns_bind_recursively() {
    let analysis = checked(&[ModuleInput::new(
        "patterns.orna",
        r#"
            type Point {
                pub x: Int,
                pub y: Int,
            }
            fn tuple((left, (right, _)): (Int, (Int, Int))): Int = left + right;
            fn list([head, tail]: [Int]): Int = head + tail;
            fn record({name, count: total}: {name: Str, count: Int}): Int = total;
            fn nominal({x, y: second}: Point): Int = x + second;
        "#,
    )]);
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
}

#[test]
fn structured_parameter_pattern_shape_errors_keep_type_diagnostic() {
    let analysis = checked(&[ModuleInput::new(
        "invalid-pattern.orna",
        "fn wrong((first, second): (Int,)): Int = first;",
    )]);
    assert!(has(&analysis, DIAG_TYPE));
}
