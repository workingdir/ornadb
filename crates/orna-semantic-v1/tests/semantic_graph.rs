use std::collections::{BTreeMap, BTreeSet};
fn render_source_fixture(template: &str, replacements: &[(&str, &str)]) -> String {
    let mut source = template.to_owned();
    for (name, value) in replacements {
        source = source.replace(&format!("{{{name}}}"), value);
    }
    source
}

use orna_semantic_v1::{
    Catalogue, DIAG_AMBIGUOUS, DIAG_ANNOTATION, DIAG_ASSERTION, DIAG_ASSERTION_EFFECT,
    DIAG_ASSERTION_ONE_TABLE, DIAG_ASSERTION_SCOPE, DIAG_IMPORT, DIAG_LEGACY_RESULT,
    DIAG_LEGACY_SYS_RUNTIME, DIAG_LEGACY_TRYFROM, DIAG_RESERVED, DIAG_TYPE, DIAG_UNRESOLVED,
    DIAG_UNSUPPORTED, EffectSummary, ModuleHeader, ModuleInput, Namespace, StandardCatalogueError,
    StandardDependencyProfile, StandardProfileError, Symbol, SymbolKind, Type, analyze,
    analyze_with_catalogue,
};

fn has(result: &orna_semantic_v1::Analysis, code: &str) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code() == code)
}

#[test]
fn unresolved_legacy_result_plumbing_uses_the_targeted_diagnostic() {
    for source in [
        include_str!("fixtures/unresolved-result-type.orna"),
        include_str!("fixtures/unresolved-ok-call.orna"),
        include_str!("fixtures/unresolved-err-call.orna"),
    ] {
        let analysis = analyze(&[ModuleInput::new("legacy-result.orna", source)]);
        assert!(
            has(&analysis, DIAG_LEGACY_RESULT),
            "{source}: {:?}",
            analysis.diagnostics
        );
        assert!(
            !has(&analysis, DIAG_UNRESOLVED),
            "legacy control plumbing must have a targeted diagnostic: {source}: {:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn declared_result_and_ok_names_remain_ordinary_user_declarations() {
    for source in [
        include_str!("fixtures/ordinary-ok-function.orna"),
        include_str!("fixtures/ordinary-result-type.orna"),
    ] {
        let analysis = analyze(&[ModuleInput::new("ordinary-result.orna", source)]);
        assert!(analysis.is_ok(), "{source}: {:?}", analysis.diagnostics);
        assert!(
            !has(&analysis, DIAG_LEGACY_RESULT),
            "declared user names must not be treated as legacy plumbing: {source}: {:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn standard_dependency_revision_is_canonical_and_captures_provenance() {
    let first = (
        "std/a.orna".to_owned(),
        include_str!("fixtures/standard-dependency-a.orna").to_owned(),
    );
    let second = (
        "std/nested/b.orna".to_owned(),
        include_str!("fixtures/standard-dependency-b.orna").to_owned(),
    );
    let ordered = StandardDependencyProfile::from_sources(
        "orna.std/snapshot-1",
        [first.clone(), second.clone()],
    )
    .expect("ordered profile");
    let reversed = StandardDependencyProfile::from_sources(
        "orna.std/snapshot-1",
        [second.clone(), first.clone()],
    )
    .expect("reversed profile");

    assert_eq!(
        ordered.revision_digest(),
        reversed.revision_digest(),
        "caller source order must not change the captured revision"
    );
    assert_ne!(
        ordered.revision_digest(),
        StandardDependencyProfile::from_sources(
            "orna.std/snapshot-1",
            [
                ("std/renamed.orna".to_owned(), first.1.clone()),
                second.clone()
            ],
        )
        .expect("path-sensitive profile")
        .revision_digest(),
        "module paths are part of source provenance"
    );
    assert_ne!(
        ordered.revision_digest(),
        StandardDependencyProfile::from_sources(
            "orna.std/snapshot-1",
            [
                (
                    first.0.clone(),
                    include_str!("fixtures/standard-dependency-a-content-variant.orna").to_owned(),
                ),
                second.clone(),
            ],
        )
        .expect("content-sensitive profile")
        .revision_digest(),
        "source bytes are part of source provenance"
    );
    assert_ne!(
        ordered.revision_digest(),
        StandardDependencyProfile::from_sources("orna.std/snapshot-2", [first, second])
            .expect("snapshot-sensitive profile")
            .revision_digest(),
        "the pinned snapshot coordinate is part of source provenance"
    );
    assert_ne!(
        ordered.revision_digest(),
        ordered
            .clone()
            .with_prelude_exports(["a"])
            .revision_digest(),
        "prelude exports are part of catalogue admission provenance"
    );
}

#[test]
fn standard_catalogue_retains_verified_dependency_provenance() {
    let source = include_str!("fixtures/standard-dependency-a.orna");
    let mismatched_source = include_str!("fixtures/standard-dependency-a-digest-mismatch.orna");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/snapshot-1",
        [("std/a.orna".to_owned(), source.to_owned())],
    )
    .expect("profile");
    let catalogue =
        Catalogue::from_standard_sources(&profile, [("std/a.orna".to_owned(), source.to_owned())])
            .expect("catalogue");

    assert_eq!(catalogue.standard_dependency_profile(), Some(&profile));
    assert_eq!(
        catalogue.standard_dependency_revision(),
        Some(profile.revision_digest())
    );
    assert!(matches!(
        Catalogue::from_standard_sources(
            &profile,
            [("std/a.orna".to_owned(), mismatched_source.to_owned())],
        ),
        Err(StandardCatalogueError::Profile(
            StandardProfileError::DigestMismatch
        ))
    ));

    let attached = Catalogue::authoritative_core()
        .with_standard_sources(&profile, [("std/a.orna".to_owned(), source.to_owned())])
        .expect("attached catalogue");
    assert_eq!(attached.standard_dependency_profile(), Some(&profile));
    assert_eq!(
        attached.standard_dependency_revision(),
        Some(profile.revision_digest())
    );
}

fn collection_catalogue() -> Catalogue {
    let source = include_str!("fixtures/collection-catalogue.orna");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/v1-collection",
        [("std/collection.orna".into(), source.into())],
    )
    .expect("collection profile");
    Catalogue::authoritative_core()
        .with_standard_sources(&profile, [("std/collection.orna".into(), source.into())])
        .expect("verified collection catalogue")
}

fn float_collection_catalogue() -> Catalogue {
    let source = include_str!("fixtures/float-collection-catalogue.orna");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/v1-float-collection",
        [("std/collection.orna".into(), source.into())],
    )
    .expect("float collection profile");
    Catalogue::authoritative_core()
        .with_standard_sources(&profile, [("std/collection.orna".into(), source.into())])
        .expect("verified float collection catalogue")
}

#[test]
fn calendar_buckets_require_typed_zones_and_do_not_conflate_elapsed_durations() {
    for (body, expected) in [
        (
            include_str!("fixtures/inline-semantic_graph/8cb436bc3cc8.orna"),
            None,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/2c95da1afd29.orna"),
            None,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/e39c953df542.orna"),
            None,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/8205383d0b5a.orna"),
            None,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/863bb66e578a.orna"),
            None,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/ee1c82416217.orna"),
            Some("calendar bucketing of Instant requires a time zone"),
        ),
        (
            include_str!("fixtures/inline-semantic_graph/76a8137c1bd1.orna"),
            Some("bucket_by requires a named TimeZone argument"),
        ),
        (
            include_str!("fixtures/inline-semantic_graph/7fd0137e6f08.orna"),
            Some("bucket_by requires a named TimeZone argument"),
        ),
        (
            include_str!("fixtures/inline-semantic_graph/28932a517978.orna"),
            Some("bucket_by requires a named TimeZone argument"),
        ),
        (
            include_str!("fixtures/inline-semantic_graph/da83880cf7a8.orna"),
            Some("bucket_by requires an elapsed Duration or calendar-day period"),
        ),
        (
            include_str!("fixtures/inline-semantic_graph/42e881f8acdb.orna"),
            Some("bucket_by requires an elapsed Duration or calendar-day period"),
        ),
        (
            include_str!("fixtures/inline-semantic_graph/1361a1247fd2.orna"),
            Some("bucket_by requires an elapsed Duration or calendar-day period"),
        ),
        (
            include_str!("fixtures/inline-semantic_graph/6fd74fa645c1.orna"),
            Some("bucket_by requires Instant values or rows with an Instant time field"),
        ),
    ] {
        let source = render_source_fixture(
            include_str!("fixtures/inline-semantic_graph/7294729a7490.orna"),
            &[("body", &body)],
        );
        let result = analyze_with_catalogue(
            &[ModuleInput::new("buckets.orna", source)],
            &Catalogue::authoritative_fixture(),
        );
        if let Some(expected) = expected {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message() == expected),
                "{body}: {:?}",
                result.diagnostics
            );
        } else {
            assert!(result.is_ok(), "{body}: {:?}", result.diagnostics);
        }
    }
}

#[test]
fn table_selectors_are_callable_and_reject_rows_from_other_tables() {
    for (source, valid) in [
        (include_str!("fixtures/table-selector-call.orna"), true),
        (include_str!("fixtures/table-selector-pipe.orna"), true),
        (include_str!("fixtures/table-selector-map.orna"), true),
        (
            include_str!("fixtures/table-selector-local-shadow.orna"),
            true,
        ),
        (
            include_str!("fixtures/table-selector-higher-order.orna"),
            true,
        ),
        (
            include_str!("fixtures/table-selector-reject-other-table-call.orna"),
            false,
        ),
        (
            include_str!("fixtures/table-selector-reject-other-table-map.orna"),
            false,
        ),
        (
            include_str!("fixtures/table-selector-reject-struct-row.orna"),
            false,
        ),
    ] {
        let result = analyze(&[ModuleInput::new("selectors.orna", source)]);
        if valid {
            assert!(result.is_ok(), "{source}: {:?}", result.diagnostics);
        } else {
            assert!(
                has(&result, DIAG_TYPE),
                "{source}: {:?}",
                result.diagnostics
            );
        }
    }
    let catalogue = Catalogue::authoritative_fixture();
    for (source, valid) in [
        (
            include_str!("fixtures/table-selector-import-aliased.orna"),
            true,
        ),
        (
            include_str!("fixtures/table-selector-import-qualified-map.orna"),
            true,
        ),
        (
            include_str!("fixtures/table-selector-import-reject-unqualified-map.orna"),
            false,
        ),
        (
            include_str!("fixtures/table-selector-import-local-shadow.orna"),
            true,
        ),
    ] {
        let result =
            analyze_with_catalogue(&[ModuleInput::new("selectors.orna", source)], &catalogue);
        if valid {
            assert!(result.is_ok(), "{source}: {:?}", result.diagnostics);
        } else {
            assert!(
                has(&result, DIAG_TYPE),
                "{source}: {:?}",
                result.diagnostics
            );
        }
    }
}

#[test]
fn attached_table_results_preserve_nominal_identity_and_row_selectors() {
    let catalogue = Catalogue::authoritative_fixture();
    for source in [
        include_str!("fixtures/attached-table-create-order.orna"),
        include_str!("fixtures/attached-table-read-created.orna"),
        include_str!("fixtures/attached-table-read-contact.orna"),
        include_str!("fixtures/attached-table-read-contact-name.orna"),
        include_str!("fixtures/attached-table-insert-payment.orna"),
    ] {
        let result =
            analyze_with_catalogue(&[ModuleInput::new("consumer.orna", source)], &catalogue);
        assert!(result.is_ok(), "{source}: {:?}", result.diagnostics);
    }
    for source in [
        include_str!("fixtures/attached-table-reject-customer-payment.orna"),
        include_str!("fixtures/attached-table-reject-struct-payment.orna"),
        include_str!("fixtures/attached-table-reject-contact-order.orna"),
        include_str!("fixtures/attached-table-reject-nominal-contact.orna"),
    ] {
        let result =
            analyze_with_catalogue(&[ModuleInput::new("consumer.orna", source)], &catalogue);
        assert!(
            has(&result, DIAG_TYPE),
            "{source}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn stored_email_provider_is_typed_without_changing_connector_messages() {
    let catalogue = Catalogue::authoritative_fixture();
    for (source, expected) in [
        (
            include_str!("fixtures/stored-email-provider-valid.orna"),
            None,
        ),
        (
            include_str!("fixtures/stored-email-provider-wrong-type.orna"),
            Some(DIAG_TYPE),
        ),
        (
            include_str!("fixtures/stored-email-provider-unknown-field.orna"),
            Some(DIAG_TYPE),
        ),
        (
            include_str!("fixtures/stored-email-provider-connector-message.orna"),
            Some(DIAG_UNRESOLVED),
        ),
    ] {
        let result =
            analyze_with_catalogue(&[ModuleInput::new("mail-client.orna", source)], &catalogue);
        if let Some(expected) = expected {
            assert!(has(&result, expected), "{:?}", result.diagnostics);
        } else {
            assert!(result.is_ok(), "{:?}", result.diagnostics);
        }
    }
}

#[test]
fn table_mutations_validate_patch_fields_and_ordered_keys_across_imports() {
    let declaration = include_str!("fixtures/table-mutations-declaration.orna");
    for imported in [false, true] {
        let prefix = if imported {
            include_str!("fixtures/inline-semantic_graph/979f4931ff6e.orna")
        } else {
            ""
        };
        for (operation, expected) in [
            (
                include_str!("fixtures/inline-semantic_graph/d1f276be2092.orna"),
                None,
            ),
            (
                include_str!("fixtures/inline-semantic_graph/fa1821597fea.orna"),
                Some("table update cannot change a computed field"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/6715d5ed7a1d.orna"),
                Some("table update cannot change a primary key; use rekey"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/68af2cd6d752.orna"),
                Some("table write field has an incompatible type:"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/ddd48f78dfad.orna"),
                Some("table write contains an unknown field"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/d6d63f5e5d43.orna"),
                Some("table write requires a record"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/2c058ceddc37.orna"),
                Some("static types are incompatible"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/dbc54adaeb84.orna"),
                None,
            ),
            (
                include_str!("fixtures/inline-semantic_graph/bb48c72c75b8.orna"),
                Some("static types are incompatible"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/79f2bdf643e7.orna"),
                None,
            ),
            (
                include_str!("fixtures/inline-semantic_graph/bf485c7a51ae.orna"),
                Some("static types are incompatible"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/d79b1f5de6e4.orna"),
                Some("static types are incompatible"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/9467f9d43291.orna"),
                None,
            ),
            (
                include_str!("fixtures/inline-semantic_graph/a32861cfa835.orna"),
                Some("static types are incompatible"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/ff1377cc77b0.orna"),
                Some("static types are incompatible"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/4dcb70c4cfbc.orna"),
                None,
            ),
            (
                include_str!("fixtures/inline-semantic_graph/9dc20e5f0252.orna"),
                None,
            ),
            (
                include_str!("fixtures/inline-semantic_graph/891ba4a6674e.orna"),
                Some("table update cannot change a primary key; use rekey"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/2d519362b9ce.orna"),
                Some("static types are incompatible"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/0eb053853c25.orna"),
                Some(include_str!(
                    "fixtures/inline-semantic_graph/189790985a7e.orna"
                )),
            ),
        ] {
            let body = render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/01ea7c81fd2b.orna"),
                &[("prefix", &prefix), ("operation", &operation)],
            );
            let inputs = if imported {
                vec![
                    ModuleInput::new("data.orna", declaration),
                    ModuleInput::new(
                        "main.orna",
                        render_source_fixture(
                            include_str!("fixtures/inline-semantic_graph/df8535abdab0.orna"),
                            &[("body", &body)],
                        ),
                    ),
                ]
            } else {
                vec![ModuleInput::new(
                    "main.orna",
                    format!("{declaration} {body}"),
                )]
            };
            let result = analyze(&inputs);
            if let Some(expected) = expected {
                assert!(
                    result
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.message().starts_with(expected)),
                    "{imported} {operation}: {:?}",
                    result.diagnostics
                );
            } else {
                assert!(
                    result.is_ok(),
                    "{imported} {operation}: {:?}",
                    result.diagnostics
                );
            }
        }
    }
    for (source, expected) in [
        (
            include_str!("fixtures/table-mutation-patch-primary-key.orna"),
            "table update cannot change a primary key; use rekey",
        ),
        (
            include_str!("fixtures/table-mutation-patch-computed-field.orna"),
            "table update cannot change a computed field",
        ),
        (
            include_str!("fixtures/table-mutation-computed-field-tail.orna"),
            "table update cannot change a computed field",
        ),
    ] {
        let result = analyze(&[ModuleInput::new(
            "rows.orna",
            format!("{declaration} {source}"),
        )]);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message() == expected),
            "{:?}",
            result.diagnostics
        );
    }
}

#[test]
fn declared_table_admission_retains_required_default_and_computed_metadata_across_imports() {
    let declaration = include_str!("fixtures/inline-semantic_graph/f7380a748846.orna");
    for imported in [false, true] {
        for operation in [
            include_str!("fixtures/inline-semantic_graph/1e22560cee2c.orna"),
            include_str!("fixtures/inline-semantic_graph/4e3b92fce522.orna"),
        ] {
            for (row, expected) in [
                (
                    include_str!("fixtures/inline-semantic_graph/8d9a64dbd5fb.orna"),
                    None,
                ),
                (
                    include_str!("fixtures/inline-semantic_graph/384c0b9e3c47.orna"),
                    None,
                ),
                (
                    include_str!("fixtures/inline-semantic_graph/5a6527ef4b65.orna"),
                    if operation == include_str!("fixtures/inline-semantic_graph/4e3b92fce522.orna")
                    {
                        None
                    } else {
                        Some(include_str!(
                            "fixtures/inline-semantic_graph/43417f7b72f7.orna"
                        ))
                    },
                ),
                (
                    include_str!("fixtures/inline-semantic_graph/f967a2fb4d5f.orna"),
                    Some(include_str!(
                        "fixtures/inline-semantic_graph/6878397ee5b7.orna"
                    )),
                ),
                (
                    include_str!("fixtures/inline-semantic_graph/a27c9b2bc6c3.orna"),
                    Some("table write field has an incompatible type:"),
                ),
                (
                    include_str!("fixtures/inline-semantic_graph/d44d5d751af1.orna"),
                    Some("table write contains an unknown field"),
                ),
            ] {
                for indirect in [false, true] {
                    let receiver = if imported {
                        include_str!("fixtures/inline-semantic_graph/981417a67a3a.orna")
                    } else {
                        include_str!("fixtures/inline-semantic_graph/6007db63e18e.orna")
                    };
                    let body = if indirect {
                        render_source_fixture(
                            include_str!("fixtures/inline-semantic_graph/a3ba8311f81e.orna"),
                            &[
                                ("row", &row),
                                ("receiver", &receiver),
                                ("operation", &operation),
                            ],
                        )
                    } else {
                        render_source_fixture(
                            include_str!("fixtures/inline-semantic_graph/3f4b72b593e6.orna"),
                            &[
                                ("receiver", &receiver),
                                ("operation", &operation),
                                ("row", &row),
                            ],
                        )
                    };
                    let inputs = if imported {
                        vec![
                            ModuleInput::new("people.orna", declaration),
                            ModuleInput::new(
                                "main.orna",
                                render_source_fixture(
                                    include_str!(
                                        "fixtures/inline-semantic_graph/e4e34328e7ca.orna"
                                    ),
                                    &[("body", &body)],
                                ),
                            ),
                        ]
                    } else {
                        vec![ModuleInput::new(
                            "main.orna",
                            render_source_fixture(
                                include_str!("fixtures/inline-semantic_graph/252c5ea7ddea.orna"),
                                &[("declaration", &declaration), ("body", &body)],
                            ),
                        )]
                    };
                    let result = analyze(&inputs);
                    if let Some(expected) = expected {
                        assert!(
                            result
                                .diagnostics
                                .iter()
                                .any(|diagnostic| diagnostic.message().starts_with(expected)),
                            "{imported} {indirect} {operation} {row}: {:?}",
                            result.diagnostics
                        );
                    } else {
                        assert!(
                            result.is_ok(),
                            "{imported} {indirect} {operation} {row}: {:?}",
                            result.diagnostics
                        );
                    }
                }
            }
        }
    }
    let missing_key = analyze(&[ModuleInput::new(
        "rows.orna",
        include_str!("fixtures/inline-semantic_graph/0985426c4300.orna"),
    )]);
    assert!(
        missing_key
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message()
                == include_str!("fixtures/inline-semantic_graph/43417f7b72f7.orna"))
    );
    let distinct_schemas = analyze(&[
        ModuleInput::new("people.orna", declaration),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-semantic_graph/a64943434af8.orna"),
        ),
    ]);
    assert!(
        distinct_schemas.is_ok(),
        "{:?}",
        distinct_schemas.diagnostics
    );
}

#[test]
fn table_insert_and_upsert_validate_provided_fields() {
    let attached = analyze_with_catalogue(
        &[ModuleInput::new(
            "rows.orna",
            include_str!("fixtures/inline-semantic_graph/d5a95f21f1ad.orna"),
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(!attached.is_ok());
    assert!(
        attached
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.message()
                == "table write contains an unknown field `banana`"),
        "{:?}",
        attached.diagnostics
    );
    for operation in [
        include_str!("fixtures/inline-semantic_graph/1e22560cee2c.orna"),
        include_str!("fixtures/inline-semantic_graph/4e3b92fce522.orna"),
    ] {
        for (value, expected) in [
            (
                include_str!("fixtures/inline-semantic_graph/c04f3ed6f331.orna"),
                None,
            ),
            (
                include_str!("fixtures/inline-semantic_graph/15970aabe980.orna"),
                Some("table write contains an unknown field"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/c113b2a4ccf0.orna"),
                Some("table write field has an incompatible type"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/91f9f0bd5d11.orna"),
                Some("table write field has an incompatible type"),
            ),
            (
                include_str!("fixtures/inline-semantic_graph/73475cb40a56.orna"),
                Some("table write requires a record"),
            ),
        ] {
            let result = analyze(&[ModuleInput::new(
                "rows.orna",
                render_source_fixture(
                    include_str!("fixtures/inline-semantic_graph/808e0cc9149d.orna"),
                    &[("operation", &operation), ("value", &value)],
                ),
            )]);
            if let Some(expected) = expected {
                assert!(
                    result
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.message().starts_with(expected)),
                    "{operation} {value}: {:?}",
                    result.diagnostics
                );
            } else {
                assert!(
                    result.is_ok(),
                    "{operation} {value}: {:?}",
                    result.diagnostics
                );
            }
        }
    }
    for value in [
        include_str!("fixtures/inline-semantic_graph/15970aabe980.orna"),
        include_str!("fixtures/inline-semantic_graph/c113b2a4ccf0.orna"),
        include_str!("fixtures/inline-semantic_graph/6666ea1f2a5c.orna"),
    ] {
        let result = analyze(&[ModuleInput::new(
            "rows.orna",
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/22e14ae5d0e2.orna"),
                &[("value", &value)],
            ),
        )]);
        assert!(has(&result, DIAG_TYPE), "{:?}", result.diagnostics);
    }
}

#[test]
fn decimal_primary_key_upsert_admits_partial_rows_but_insert_requires_completeness() {
    let declaration = include_str!("fixtures/inline-semantic_graph/bddbd7a40d42.orna");

    let partial_upsert = analyze(&[ModuleInput::new(
        "decimal-upsert.orna",
        render_source_fixture(
            include_str!("fixtures/inline-semantic_graph/b9c1b567cc20.orna"),
            &[("declaration", &declaration)],
        ),
    )]);
    assert!(
        partial_upsert.is_ok(),
        "existing-row partial upsert should be admitted: {:?}",
        partial_upsert.diagnostics
    );

    let incomplete_insert = analyze(&[ModuleInput::new(
        "decimal-insert.orna",
        render_source_fixture(
            include_str!("fixtures/inline-semantic_graph/26ac9a852d79.orna"),
            &[("declaration", &declaration)],
        ),
    )]);
    assert!(
        incomplete_insert
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message()
                == include_str!("fixtures/inline-semantic_graph/43417f7b72f7.orna")),
        "absent-row insert must retain completeness admission: {:?}",
        incomplete_insert.diagnostics
    );

    let missing_key = analyze(&[ModuleInput::new(
        "decimal-missing-key.orna",
        render_source_fixture(
            include_str!("fixtures/inline-semantic_graph/c3066bc5ad9a.orna"),
            &[("declaration", &declaration)],
        ),
    )]);
    assert!(
        missing_key.diagnostics.iter().any(|diagnostic| {
            diagnostic.message() == include_str!("fixtures/inline-semantic_graph/a6ec790995c0.orna")
        }),
        "upsert must still require its Decimal primary key: {:?}",
        missing_key.diagnostics
    );

    let invalid_supplied_field = analyze(&[ModuleInput::new(
        "decimal-invalid-field.orna",
        render_source_fixture(
            include_str!("fixtures/inline-semantic_graph/d538eed4759d.orna"),
            &[("declaration", &declaration)],
        ),
    )]);
    assert!(
        invalid_supplied_field
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic
                .message()
                .starts_with("table write field has an incompatible type")),
        include_str!("fixtures/inline-semantic_graph/ccfc248d3c6c.orna"),
        invalid_supplied_field.diagnostics
    );
}

#[test]
fn attached_reading_write_diagnostics_retain_units_without_record_values() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "rows.orna",
            include_str!("fixtures/inline-semantic_graph/99b7836b02e4.orna"),
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message()
                == "table write field has an incompatible type: expected Float<mph>, found Str"),
        "{:?}",
        result.diagnostics
    );
    let encoded = serde_json::to_string(&result.diagnostics).unwrap();
    assert!(!encoded.contains("private-value"));
    assert!(!encoded.contains("rows.orna"));
}

#[test]
fn closure_lists_do_not_erase_incompatible_return_types() {
    let result = analyze(&[ModuleInput::new(
        "closures.orna",
        include_str!("fixtures/inline-semantic_graph/a57e5607a0fa.orna"),
    )]);
    assert!(has(&result, DIAG_TYPE), "{:?}", result.diagnostics);
    let compatible = analyze(&[ModuleInput::new(
        "closures.orna",
        include_str!("fixtures/inline-semantic_graph/2c09cc04f4aa.orna"),
    )]);
    assert!(compatible.is_ok(), "{:?}", compatible.diagnostics);
}

#[test]
fn relation_comparisons_require_an_explicit_comparison_operation() {
    for expression in [
        "Row == Row",
        "Row != Row",
        include_str!("fixtures/inline-semantic_graph/241acc31bd91.orna"),
    ] {
        let result = analyze(&[ModuleInput::new(
            "relations.orna",
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/1a494648dbd3.orna"),
                &[("expression", &expression)],
            ),
        )]);
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_TYPE
                && diagnostic.message() == "relation equality is ambiguous; choose sequence or row-set comparison explicitly"
        }), "{:?}", result.diagnostics);
    }
    let scalars = analyze(&[ModuleInput::new(
        "scalars.orna",
        include_str!("fixtures/inline-semantic_graph/101176875100.orna"),
    )]);
    assert!(scalars.is_ok(), "{:?}", scalars.diagnostics);
}

#[test]
fn ordered_comparisons_reject_mismatched_and_unsupported_evaluator_values() {
    for expression in [
        "1 < true",
        "[1] < [2]",
        include_str!("fixtures/inline-semantic_graph/804a80c4678f.orna"),
    ] {
        let result = analyze(&[ModuleInput::new(
            "ordered-comparison.orna",
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/d4d71edde22b.orna"),
                &[("expression", &expression)],
            ),
        )]);
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.code() == DIAG_TYPE
                    && diagnostic.message() == "static types are incompatible"
            }),
            "{expression}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn failure_skip_requires_a_typed_version_precondition() {
    for arguments in [
        include_str!("fixtures/inline-semantic_graph/47f32d8d23d9.orna"),
        include_str!("fixtures/inline-semantic_graph/56adeab5a94e.orna"),
    ] {
        let source = render_source_fixture(
            include_str!("fixtures/inline-semantic_graph/9638bea36fee.orna"),
            &[("arguments", &arguments)],
        );
        let result = analyze(&[ModuleInput::new("skip.orna", source)]);
        assert!(has(&result, DIAG_TYPE), "{:?}", result.diagnostics);
    }
    let valid = analyze(&[ModuleInput::new(
        "skip.orna",
        include_str!("fixtures/inline-semantic_graph/c2d71edc5480.orna"),
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);
    let symbol = &valid.modules[&orna_semantic_v1::Namespace(vec!["skip".into()])].exports["skip"];
    assert!(symbol.effects.effects.contains("admin"));
    assert!(symbol.effects.may_fail);
}

#[test]
fn failure_replay_requires_a_typed_version_precondition() {
    for arguments in [
        include_str!("fixtures/inline-semantic_graph/1a7027021b4e.orna"),
        include_str!("fixtures/inline-semantic_graph/95e04191ceb1.orna"),
        include_str!("fixtures/inline-semantic_graph/d52463678886.orna"),
    ] {
        let source = render_source_fixture(
            include_str!("fixtures/inline-semantic_graph/67c676dcea5a.orna"),
            &[("arguments", &arguments)],
        );
        let result = analyze(&[ModuleInput::new("replay.orna", source)]);
        assert!(has(&result, DIAG_TYPE), "{:?}", result.diagnostics);
    }
    let valid = analyze(&[ModuleInput::new(
        "replay.orna",
        include_str!("fixtures/inline-semantic_graph/a0a8de96808c.orna"),
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);
    let symbol =
        &valid.modules[&orna_semantic_v1::Namespace(vec!["replay".into()])].exports["replay"];
    assert!(symbol.effects.effects.contains("admin"));
    assert!(!symbol.effects.effects.contains("database write"));
    assert!(symbol.effects.may_fail);
}

#[test]
fn checkpoint_and_failure_cas_admission_requires_typed_versions() {
    let valid = analyze(&[ModuleInput::new(
        "cas.orna",
        include_str!("fixtures/inline-semantic_graph/1f0342ef7d4a.orna"),
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);
    for source in [
        include_str!("fixtures/inline-semantic_graph/157cc147476b.orna"),
        include_str!("fixtures/inline-semantic_graph/a67dd27256bb.orna"),
        include_str!("fixtures/inline-semantic_graph/781ba6dba695.orna"),
    ] {
        let invalid = analyze(&[ModuleInput::new("cas.orna", source)]);
        assert!(
            has(&invalid, DIAG_TYPE),
            "{source}: {:?}",
            invalid.diagnostics
        );
    }
}

#[test]
fn unicode_nfkc_casefold_sibling_collision_is_rejected() {
    let result = analyze(&[
        ModuleInput::new("ff/left.orna", ""),
        ModuleInput::new("ﬀ/right.orna", ""),
    ]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "ORNA-S002-NAMESPACE")
    );
}

#[test]
fn graph_resolution_keeps_explicit_imports_over_globs_and_rejects_module_assertion_execution() {
    let result = analyze(&[
        ModuleInput::new(
            "left.orna",
            include_str!("fixtures/semantic-graph/explicit-left.orna"),
        ),
        ModuleInput::new(
            "right.orna",
            include_str!("fixtures/semantic-graph/explicit-right.orna"),
        ),
        ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/semantic-graph/explicit-over-glob-consumer.orna"),
        ),
    ]);

    assert!(!has(&result, DIAG_AMBIGUOUS));
    assert!(has(&result, DIAG_ASSERTION_SCOPE));
}

#[test]
fn conflicting_module_imports_do_not_install_hidden_qualified_roots() {
    let catalogue = Catalogue::authoritative_fixture();
    let direct = analyze_with_catalogue(
        &[ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-semantic_graph/6ab8843dfd67.orna"),
        )],
        &catalogue,
    );
    assert!(direct.is_ok(), "{:?}", direct.diagnostics);

    for source in [
        include_str!("fixtures/inline-semantic_graph/a492c6ae666d.orna"),
        include_str!("fixtures/inline-semantic_graph/738ce3de578f.orna"),
    ] {
        let result = analyze_with_catalogue(&[ModuleInput::new("main.orna", source)], &catalogue);
        assert!(
            has(&result, DIAG_AMBIGUOUS),
            "{source}: {:?}",
            result.diagnostics
        );
        assert!(
            has(&result, DIAG_TYPE),
            "{source}: {:?}",
            result.diagnostics
        );
    }

    let ordered = analyze(&[
        ModuleInput::new(
            "contacts.orna",
            include_str!("fixtures/inline-semantic_graph/12669305ce1b.orna"),
        ),
        ModuleInput::new(
            "energy.orna",
            include_str!("fixtures/inline-semantic_graph/3641aa96ba18.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-semantic_graph/1b4a0137f06a.orna"),
        ),
    ]);
    assert!(has(&ordered, DIAG_AMBIGUOUS), "{:?}", ordered.diagnostics);
    assert!(!has(&ordered, DIAG_UNRESOLVED), "{:?}", ordered.diagnostics);
    assert!(!has(&ordered, DIAG_TYPE), "{:?}", ordered.diagnostics);
}

#[test]
fn qualified_module_member_calls_resolve_only_public_imported_exports() {
    let result = analyze(&[
        ModuleInput::new(
            "library.orna",
            include_str!("fixtures/inline-semantic_graph/a9bff76d17e0.orna"),
        ),
        ModuleInput::new(
            "warehouse.orna",
            include_str!("fixtures/inline-semantic_graph/063add3ce375.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-semantic_graph/fd00f14d8437.orna"),
        ),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let private = analyze(&[
        ModuleInput::new(
            "library.orna",
            include_str!("fixtures/inline-semantic_graph/68ea1c5f0590.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-semantic_graph/5a4e01c5aacf.orna"),
        ),
    ]);
    assert!(has(&private, DIAG_UNRESOLVED));

    let missing = analyze(&[
        ModuleInput::new(
            "library.orna",
            include_str!("fixtures/inline-semantic_graph/76d79a5238ee.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-semantic_graph/7b9e45b61b11.orna"),
        ),
    ]);
    assert!(has(&missing, DIAG_UNRESOLVED));
}

#[test]
fn system_checkpoint_history_selectors_resolve_from_intrinsic_surface() {
    let result = analyze(&[ModuleInput::new(
        "sys-checkpoint.orna",
        include_str!("fixtures/inline-semantic_graph/503e0a4410f3.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_run_history_sorting_resolves_system_row_fields() {
    let result = analyze(&[ModuleInput::new(
        "sys-run-history.orna",
        include_str!("fixtures/inline-semantic_graph/e5bca2b07069.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_storage_relation_filters_typed_status_fields() {
    let result = analyze(&[ModuleInput::new(
        "sys-storage.orna",
        include_str!("fixtures/inline-semantic_graph/66a96ad6378b.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_file_history_uses_the_file_reference_overload() {
    let result = analyze(&[ModuleInput::new(
        "sys-file-history.orna",
        include_str!("fixtures/inline-semantic_graph/f7252afe1bc5.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_catalogue_relations_support_typed_filter_and_map_queries() {
    let result = analyze(&[
        ModuleInput::new(
            "sys-definition-file.orna",
            include_str!("fixtures/inline-semantic_graph/b83a6f1b1f9f.orna"),
        ),
        ModuleInput::new(
            "sys-table-query.orna",
            include_str!("fixtures/inline-semantic_graph/7c3f9b7ff32d.orna"),
        ),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn table_projection_stages_type_computed_and_default_fields() {
    let source = include_str!("fixtures/inline-semantic_graph/f6ee1dddf583.orna");
    let filtered = include_str!("fixtures/inline-semantic_graph/4ab970735227.orna");
    let result = analyze(&[
        ModuleInput::new("computed-field.orna", source),
        ModuleInput::new("default-and-computed-fields.orna", filtered),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_dependency_queries_use_object_references() {
    let result = analyze(&[ModuleInput::new(
        "dependency-query.orna",
        include_str!("fixtures/inline-semantic_graph/e691a63bae25.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn system_snapshot_selectors_accept_revision_strings() {
    let result = analyze(&[ModuleInput::new(
        "historical-query.orna",
        include_str!("fixtures/inline-semantic_graph/fd8e15317cd4.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn qualified_table_operations_infer_rows_and_reach_block_expression_statements() {
    let result = analyze(&[
        ModuleInput::new(
            "library.orna",
            include_str!("fixtures/inline-semantic_graph/08682c91628a.orna"),
        ),
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-semantic_graph/612049a47d9a.orna"),
        ),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let wrong_arity = analyze(&[ModuleInput::new(
        "books.orna",
        include_str!("fixtures/inline-semantic_graph/3ae31ca0758e.orna"),
    )]);
    assert!(has(&wrong_arity, "ORNA-S021-TYPE"));

    let non_table = analyze(&[ModuleInput::new(
        "books.orna",
        include_str!("fixtures/inline-semantic_graph/b43716a788c7.orna"),
    )]);
    assert!(has(&non_table, "ORNA-S021-TYPE"));
}

#[test]
fn table_assertion_rejects_authoritative_std_net_effect() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/inline-semantic_graph/2861d1a34793.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(has(&result, DIAG_ASSERTION_EFFECT));
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "declaration assertion uses forbidden network effect"
    }));
}

#[test]
fn imported_std_net_callables_preserve_effects_and_imports_remain_analysis_only() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/inline-semantic_graph/23626790f7ed.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code() == DIAG_ASSERTION_EFFECT)
            .count(),
        2,
        "only the assertions evaluate the imported callable: {:?}",
        result.diagnostics
    );
    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "consumer")
        .expect("consumer module");
    for name in ["aliased", "named"] {
        let symbol = module.symbols.get(name).expect("imported callable summary");
        assert!(symbol.effects.effects.contains("network"));
        assert!(symbol.effects.may_fail);
    }
}

#[test]
fn unused_std_net_import_is_admitted_without_execution() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/inline-semantic_graph/0cd09c296b82.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let consumer = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "consumer")
        .expect("consumer module");
    assert!(consumer.symbols.contains_key("untouched"));
    assert!(!consumer.symbols.contains_key("http"));
}

#[test]
fn table_assertion_rejects_standard_filesystem_effect_before_admission() {
    let result = analyze(&[ModuleInput::new(
        "consumer.orna",
        include_str!("fixtures/inline-semantic_graph/c26c115a2799.orna"),
    )]);

    assert!(has(&result, DIAG_ASSERTION_EFFECT));
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_ASSERTION_EFFECT
            && diagnostic.message() == "declaration assertion uses forbidden filesystem effect"
    }));
    assert!(
        !serde_json::to_string(&result.diagnostics)
            .expect("diagnostics encode")
            .contains("private-input")
    );
}

#[test]
fn table_assertion_rejects_owner_type_mismatch() {
    let result = analyze(&[ModuleInput::new(
        "books.orna",
        include_str!("fixtures/inline-semantic_graph/7bb60161edba.orna"),
    )]);

    assert!(has(&result, DIAG_ASSERTION));
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_ASSERTION
            && diagnostic.message()
                == include_str!("fixtures/inline-semantic_graph/0b8396270d94.orna")
    }));
}

#[test]
fn legacy_table_assertion_owner_pipes_keep_published_diagnostics() {
    let owner = analyze(&[ModuleInput::new(
        "owner-pipe.orna",
        include_str!("fixtures/inline-semantic_graph/469ca68ac080.orna"),
    )]);
    assert!(!has(&owner, DIAG_UNRESOLVED));
    assert!(owner.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == include_str!("fixtures/inline-semantic_graph/1496e412be87.orna")
    }));

    let self_pipe = analyze(&[ModuleInput::new(
        "self-pipe.orna",
        include_str!("fixtures/inline-semantic_graph/c5d195bfaae9.orna"),
    )]);
    assert!(!has(&self_pipe, DIAG_UNRESOLVED));
    assert!(self_pipe.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == include_str!("fixtures/inline-semantic_graph/9f0ffbf867da.orna")
    }));
}

#[test]
fn table_assertion_elaborates_reference_relation_predicates_without_an_evaluator() {
    let result = analyze(&[ModuleInput::new(
        "library.orna",
        include_str!("fixtures/inline-semantic_graph/00c79142baf5.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn module_assertion_elaborates_the_reference_projects_nested_relation_predicate() {
    let result = analyze(&[ModuleInput::new(
        "library.orna",
        include_str!("fixtures/inline-semantic_graph/4de58047bfa1.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn authoritative_core_catalogue_resolves_prelude_types_and_common_functions() {
    let profile = Catalogue::authoritative_core();
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/inline-semantic_graph/2684ea6469f3.orna"),
        )],
        &profile,
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn authoritative_core_resolves_text_key_helpers() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "keys.orna",
            include_str!("fixtures/inline-semantic_graph/65c1641efc90.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn authoritative_core_resolves_nested_operations_through_an_imported_root() {
    let profile = Catalogue::authoritative_core();
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/inline-semantic_graph/d6e1d06e0bbb.orna"),
        )],
        &profile,
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let missing = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/inline-semantic_graph/15e66ec6bc0c.orna"),
        )],
        &profile,
    );
    assert!(has(&missing, DIAG_UNRESOLVED));
}

#[test]
fn qualified_collection_requires_an_admitted_optional_module() {
    let rejected = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/inline-semantic_graph/906d103234a6.orna"),
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(
        has(&rejected, DIAG_UNRESOLVED),
        "{:?}",
        rejected.diagnostics
    );

    let import_rejected = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/inline-semantic_graph/1f27027f871f.orna"),
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(has(&import_rejected, DIAG_IMPORT));

    let bare_core = analyze(&[ModuleInput::new(
        "consumer.orna",
        include_str!("fixtures/inline-semantic_graph/472675e85e4c.orna"),
    )]);
    assert!(bare_core.is_ok(), "{:?}", bare_core.diagnostics);
}

#[test]
fn refined_aliases_check_owner_assertions_and_expose_static_constructors() {
    let result = analyze(&[ModuleInput::new(
        "ports.orna",
        include_str!("fixtures/inline-semantic_graph/ab133cfae7c2.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("module header");
    assert_eq!(
        module.symbols.get("default_port").expect("function").ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            result: Box::new(Type::Named("Port".into())),
            default_parameters: Default::default(),
        }
    );
    assert_eq!(
        result.assertions.values().next().expect("plans"),
        &vec![
            orna_semantic_v1::AssertionPlan {
                owner: orna_semantic_v1::AssertionOwner::RefinedType("Port".into()),
                dependencies: Default::default(),
                effects: Default::default(),
            },
            orna_semantic_v1::AssertionPlan {
                owner: orna_semantic_v1::AssertionOwner::RefinedType("Port".into()),
                dependencies: Default::default(),
                effects: Default::default(),
            },
        ]
    );
}

#[test]
fn catalogue_is_closed_world_and_diagnostics_remain_redacted_and_stable() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "secret.orna",
            include_str!("fixtures/inline-semantic_graph/08476bf4d88a.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(has(&result, DIAG_UNRESOLVED));
    let json = serde_json::to_string(&result.diagnostics).unwrap();
    assert!(!json.contains("secret.orna"));
    assert!(!json.contains("definitely_not_in_the_catalogue"));
    assert_eq!(
        result
            .diagnostics
            .iter()
            .map(|d| d.code())
            .collect::<Vec<_>>(),
        vec![DIAG_UNRESOLVED]
    );
}

#[test]
fn catalogue_does_not_relax_reserved_source_roots() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new("std/main.orna", "")],
        &Catalogue::authoritative_core(),
    );

    assert!(has(&result, DIAG_RESERVED));
}

#[test]
fn reserved_table_names_keep_typecheck_diagnostics() {
    let std_table = analyze(&[ModuleInput::new(
        "reserved-std.orna",
        include_str!("fixtures/inline-semantic_graph/0304b2011b9f.orna"),
    )]);
    assert!(
        std_table
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "`std` is reserved" })
    );

    let sys_table = analyze(&[ModuleInput::new(
        "reserved-sys.orna",
        include_str!("fixtures/inline-semantic_graph/8c1b51ec5b67.orna"),
    )]);
    assert!(
        sys_table
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "`sys` is reserved" })
    );
}

#[test]
fn authoritative_ui_catalogue_checks_page_builder_contextually() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "page.orna",
            include_str!("fixtures/page-builder-contextual.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("page module");
    assert_eq!(
        module.symbols.get("values_page").expect("page function").ty,
        Type::Function {
            parameters: vec![Type::List(Box::new(Type::Text))],
            parameter_names: Some(vec!["values".into()]),
            result: Box::new(Type::Named("std.UI".into())),
            default_parameters: Default::default(),
        }
    );
}

#[test]
fn generic_ordering_pipeline_keeps_element_and_optional_types() {
    let result = analyze(&[ModuleInput::new(
        "generic.orna",
        include_str!("fixtures/inline-semantic_graph/f74bf9bbd200.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("generic module");
    assert_eq!(
        module.symbols.get("maximum").expect("maximum function").ty,
        Type::Function {
            parameters: vec![Type::List(Box::new(Type::Named("T".into())))],
            parameter_names: Some(vec!["values".into()]),
            result: Box::new(Type::Optional(Box::new(Type::Named("T".into())))),
            default_parameters: Default::default(),
        }
    );
}

#[test]
fn imported_generic_calls_preserve_declared_effects_and_failure_metadata() {
    let result = analyze(&[
        ModuleInput::new(
            "library.orna",
            include_str!("fixtures/inline-semantic_graph/34cf576507eb.orna"),
        ),
        ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/inline-semantic_graph/c47e90557a30.orna"),
        ),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let consumer = result
        .modules
        .get(&Namespace(vec!["consumer".into()]))
        .expect("consumer module");
    let read = consumer.exports.get("read").expect("read export");
    assert!(matches!(
        &read.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.ValueMetadata".into(),
                arguments: vec![Type::Int],
            }
    ));
    assert_eq!(
        read.effects.effects,
        std::collections::BTreeSet::from(["database read".into()])
    );
    assert!(read.effects.may_fail);
}

#[test]
fn imported_generic_calls_reject_invalid_explicit_type_arguments() {
    for (name, body) in [
        (
            "too_many",
            include_str!("fixtures/inline-semantic_graph/e0ff416fc0e3.orna"),
        ),
        (
            "unknown",
            include_str!("fixtures/inline-semantic_graph/c35831ebb0e3.orna"),
        ),
    ] {
        let result = analyze(&[
            ModuleInput::new(
                "library.orna",
                include_str!("fixtures/inline-semantic_graph/34cf576507eb.orna"),
            ),
            ModuleInput::new(
                "consumer.orna",
                render_source_fixture(
                    include_str!("fixtures/inline-semantic_graph/1ba59ac17631.orna"),
                    &[("body", &body)],
                ),
            ),
        ]);
        assert!(has(&result, DIAG_TYPE), "{name}: {:?}", result.diagnostics);
        let consumer = result
            .modules
            .get(&Namespace(vec!["consumer".into()]))
            .expect("consumer module");
        assert!(
            !consumer.exports.contains_key(name),
            "{name} must not be admitted as an exported callable: {:?}",
            consumer.exports
        );
    }
}

#[test]
fn imported_generic_calls_reject_unsupported_protocol_bound_substitutions() {
    let result = analyze(&[
        ModuleInput::new(
            "library.orna",
            include_str!("fixtures/inline-semantic_graph/18f56ee1bba3.orna"),
        ),
        ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/inline-semantic_graph/df7bf35dfc24.orna"),
        ),
    ]);

    assert!(
        has(&result, DIAG_TYPE) || has(&result, DIAG_UNSUPPORTED),
        "{:?}",
        result.diagnostics
    );
    let consumer = result
        .modules
        .get(&Namespace(vec!["consumer".into()]))
        .expect("consumer module");
    assert!(
        !consumer.exports.contains_key("invalid"),
        "unsupported bound substitution must not be admitted as a callable: {:?}",
        consumer.exports
    );
}

#[test]
fn relation_pairs_preserve_element_type_in_overlapping_tuples() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "pairs.orna",
            include_str!("fixtures/inline-semantic_graph/4ffdd171862b.orna"),
        )],
        &Catalogue::authoritative_fixture(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .get(&orna_semantic_v1::Namespace(vec!["pairs".into()]))
        .expect("pairs module");
    assert_eq!(
        module.exports.get("paired").expect("paired function").ty,
        Type::Function {
            parameters: Vec::new(),
            parameter_names: Some(Vec::new()),
            result: Box::new(Type::Relation(Box::new(Type::Tuple(vec![
                Type::Named("sys.Failure".into()),
                Type::Named("sys.Failure".into()),
            ])))),
            default_parameters: Default::default(),
        }
    );
}

#[test]
fn relation_extrema_preserve_element_type_as_optional_values() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "extrema.orna",
            include_str!("fixtures/inline-semantic_graph/fd55146cc39c.orna"),
        )],
        &Catalogue::authoritative_fixture(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .get(&orna_semantic_v1::Namespace(vec!["extrema".into()]))
        .expect("extrema module");
    for name in ["smallest", "largest"] {
        assert_eq!(
            module.exports.get(name).expect("extrema function").ty,
            Type::Function {
                parameters: Vec::new(),
                parameter_names: Some(Vec::new()),
                result: Box::new(Type::Optional(Box::new(Type::Named("sys.Failure".into(),)))),
                default_parameters: Default::default(),
            }
        );
    }
}

#[test]
fn omitted_numeric_function_parameters_are_inferred_without_dynamic_fallback() {
    let result = analyze(&[ModuleInput::new(
        "inferred.orna",
        include_str!("fixtures/omitted-numeric-inference.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("inferred module");
    assert_eq!(
        module.symbols.get("square").expect("square function").ty,
        Type::Function {
            parameters: vec![Type::Int],
            parameter_names: Some(vec!["value".into()]),
            result: Box::new(Type::Int),
            default_parameters: Default::default(),
        }
    );
}

#[test]
fn table_keys_reject_float_and_affine_temperatures_reject_addition() {
    let key = analyze(&[ModuleInput::new(
        "key.orna",
        include_str!("fixtures/inline-semantic_graph/4bae95d858a3.orna"),
    )]);
    assert!(has(&key, DIAG_TYPE));

    let temperature = analyze(&[ModuleInput::new(
        "temperature.orna",
        include_str!("fixtures/inline-semantic_graph/e4d502137c61.orna"),
    )]);
    assert!(has(&temperature, DIAG_TYPE));
}

#[test]
fn table_keys_reject_ranges_with_the_published_primary_key_rule() {
    let result = analyze(&[ModuleInput::new(
        "range-key.orna",
        include_str!("fixtures/inline-semantic_graph/77477ebfa193.orna"),
    )]);

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == include_str!("fixtures/inline-semantic_graph/a6178701fcd8.orna")
    }));
}

#[test]
fn table_keys_reject_transparent_range_aliases_with_the_published_primary_key_rule() {
    let result = analyze(&[ModuleInput::new(
        "range-alias-key.orna",
        include_str!("fixtures/inline-semantic_graph/9018248ac127.orna"),
    )]);

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == include_str!("fixtures/inline-semantic_graph/a6178701fcd8.orna")
    }));
}

#[test]
fn automatic_key_tables_reject_explicit_rekey_operations() {
    let result = analyze(&[ModuleInput::new(
        "rekey.orna",
        include_str!("fixtures/inline-semantic_graph/a336e892e93f.orna"),
    )]);

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == include_str!("fixtures/inline-semantic_graph/189790985a7e.orna")
    }));
}

#[test]
fn display_implementations_reject_database_writes() {
    let result = analyze(&[ModuleInput::new(
        "display.orna",
        include_str!("fixtures/inline-semantic_graph/312b7839ab8f.orna"),
    )]);

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "Display and Present implementations must be read-only"
    }));
}

#[test]
fn nominal_targets_reject_overlapping_protocol_implementations() {
    let distinct = analyze(&[ModuleInput::new(
        "distinct-conversions.orna",
        include_str!("fixtures/inline-semantic_graph/ee4e105090e1.orna"),
    )]);
    assert!(distinct.is_ok(), "{:#?}", distinct.diagnostics);

    let overlapping = analyze(&[ModuleInput::new(
        "overlapping-conversions.orna",
        include_str!("fixtures/inline-semantic_graph/bba09a2efb55.orna"),
    )]);
    assert!(overlapping.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message()
                == include_str!("fixtures/inline-semantic_graph/529e5f255207.orna")
    }));
}

#[test]
fn frozen_nominal_nested_impl_uses_authoritative_fixture_surface() {
    let source = include_str!("fixtures/inline-semantic_graph/af7c08049c11.orna");
    let valid = analyze_with_catalogue(
        &[ModuleInput::new("nominal-type-nested-impl.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(valid.is_ok(), "{:#?}", valid.diagnostics);

    let unresolved_helper = analyze_with_catalogue(
        &[ModuleInput::new(
            "nominal-type-nested-impl-missing-helper.orna",
            source.replace("valid_email", "missing_email"),
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(has(&unresolved_helper, DIAG_UNRESOLVED));

    let invalid_source = analyze_with_catalogue(
        &[ModuleInput::new(
            "nominal-type-nested-impl-missing-source.orna",
            source.replace("EmailHeader", "MissingHeader"),
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(has(&invalid_source, DIAG_TYPE));
    assert!(!has(&invalid_source, DIAG_UNRESOLVED));
    let nonexact_source = render_source_fixture(
        include_str!("fixtures/inline-semantic_graph/787d3b77e398.orna"),
        &[("source", &source)],
    );
    let nonexact = analyze_with_catalogue(
        &[ModuleInput::new(
            "nominal-type-nested-impl-nonexact-source.orna",
            nonexact_source,
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(has(&nonexact, DIAG_TYPE));
}

#[test]
fn table_rows_reject_overlapping_protocol_implementations() {
    let distinct = analyze(&[ModuleInput::new(
        "distinct-table-conversions.orna",
        include_str!("fixtures/inline-semantic_graph/ddc1070ae1fd.orna"),
    )]);
    assert!(distinct.is_ok(), "{:#?}", distinct.diagnostics);

    let overlapping = analyze(&[ModuleInput::new(
        "overlapping-table-conversions.orna",
        include_str!("fixtures/inline-semantic_graph/6f9f39aac506.orna"),
    )]);
    assert!(overlapping.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message()
                == include_str!("fixtures/inline-semantic_graph/529e5f255207.orna")
    }));
}

#[test]
fn secret_values_reject_display_after_authoritative_open() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "secret.orna",
            include_str!("fixtures/inline-semantic_graph/bf3665cb2f18.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "secret values cannot be displayed" })
    );
}

#[test]
fn computed_fields_reject_effectful_initializers() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "contact.orna",
            include_str!("fixtures/inline-semantic_graph/281a25bf6870.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "computed field must be deterministic and row-local"
    }));
}

#[test]
fn computed_fields_reject_database_mutation_effects() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "contact.orna",
            include_str!("fixtures/table-computed-mutation-effect.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "computed field must be deterministic and row-local"
    }));
}

#[test]
fn system_commit_rows_reject_mutation() {
    let result = analyze(&[ModuleInput::new(
        "commit.orna",
        include_str!("fixtures/inline-semantic_graph/4ce1add26a29.orna"),
    )]);

    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "sys.Commit is read-only" })
    );
}

#[test]
fn money_rate_unit_resolves_before_float_exactness_check() {
    let analysis = analyze(&[ModuleInput::new(
        "rate.orna",
        include_str!("fixtures/inline-semantic_graph/459784eb93e7.orna"),
    )]);
    assert!(
        !has(&analysis, DIAG_UNRESOLVED),
        "{:?}",
        analysis.diagnostics
    );
    assert!(has(&analysis, DIAG_TYPE), "{:?}", analysis.diagnostics);

    let unknown = analyze(&[ModuleInput::new(
        "unknown.orna",
        include_str!("fixtures/inline-semantic_graph/e2e3ba7cadca.orna"),
    )]);
    assert!(has(&unknown, DIAG_UNRESOLVED));
}

#[test]
fn published_money_and_affine_diagnostics_are_preserved() {
    let affine_sum = analyze(&[ModuleInput::new(
        "sum.orna",
        include_str!("fixtures/inline-semantic_graph/35dcb5e92b6c.orna"),
    )]);
    assert!(
        affine_sum
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "cannot sum absolute affine quantities" })
    );

    let currency_symbol = analyze(&[ModuleInput::new(
        "currency.orna",
        include_str!("fixtures/inline-semantic_graph/3ef85d1c9f2f.orna"),
    )]);
    assert!(currency_symbol.diagnostics.iter().any(|diagnostic| {
        diagnostic.message()
            == "currency symbols belong to locale-aware formatting, not Currency identity"
    }));

    let float_money = analyze(&[ModuleInput::new(
        "money.orna",
        include_str!("fixtures/inline-semantic_graph/e6cc03beb7e7.orna"),
    )]);
    assert!(float_money.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "binary Float cannot enter an exact Money calculation implicitly"
    }));

    let float_constructor = analyze(&[ModuleInput::new(
        "constructor.orna",
        include_str!("fixtures/inline-semantic_graph/7012225696c5.orna"),
    )]);
    assert!(float_constructor.diagnostics.iter().any(|diagnostic| {
        diagnostic.message()
            == "Money cannot be constructed from an inexact Float without explicit rounding"
    }));

    let exact_constructor = analyze(&[ModuleInput::new(
        "constructor.orna",
        include_str!("fixtures/inline-semantic_graph/210723bc8b9b.orna"),
    )]);
    assert!(
        exact_constructor.is_ok(),
        "{:?}",
        exact_constructor.diagnostics
    );
    let good = &exact_constructor.modules.values().next().unwrap().exports["good"];
    assert!(matches!(
        &good.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "Money".into(),
                arguments: vec![Type::Named("GBP".into())],
            }
    ));
}

#[test]
fn legacy_system_admin_methods_are_rejected_with_published_messages() {
    let result = analyze(&[ModuleInput::new(
        "legacy.orna",
        include_str!("fixtures/inline-semantic_graph/bb00e0a88ed6.orna"),
    )]);
    let messages = result
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message())
        .collect::<Vec<_>>();
    assert!(messages.contains(
        &"system rows are read-only; use `sys.admin.reset_checkpoint` with compare-and-set arguments"
    ));
    assert!(messages.contains(&include_str!(
        "fixtures/inline-semantic_graph/88a7c3586e5d.orna"
    )));
    assert!(messages.contains(&include_str!(
        "fixtures/inline-semantic_graph/781e42671598.orna"
    )));
    assert!(messages.contains(
        &"system rows are read-only; use `sys.admin.retry_failure` on a `sys.FailureRef`"
    ));
    assert!(messages.contains(
        &"system rows are read-only; use `sys.admin.skip_failure` on a `sys.FailureRef`"
    ));
}

#[test]
fn distinct_nominal_types_require_named_conversions() {
    let result = analyze(&[ModuleInput::new(
        "conversion.orna",
        include_str!("fixtures/inline-semantic_graph/bd8b70b891a8.orna"),
    )]);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message()
            == "implicit conversion chains are not searched; name each conversion explicitly"
    }));
}

#[test]
fn module_assertion_scope_distinguishes_zero_and_one_table_invariants() {
    let one_table = analyze(&[ModuleInput::new(
        "one.orna",
        include_str!("fixtures/inline-semantic_graph/a15d9f6d89df.orna"),
    )]);
    assert!(has(&one_table, DIAG_ASSERTION_ONE_TABLE));
    assert!(one_table.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == include_str!("fixtures/inline-semantic_graph/35d3c54e9537.orna")
    }));

    let zero_table = analyze(&[ModuleInput::new("zero.orna", "assert 1 + 1 == 2;")]);
    assert_eq!(
        zero_table.diagnostics.len(),
        1,
        "{:?}",
        zero_table.diagnostics
    );
    assert_eq!(zero_table.diagnostics[0].code(), DIAG_ASSERTION_SCOPE);
    assert_eq!(
        zero_table.diagnostics[0].message(),
        "module assertions must depend on at least two distinct tables"
    );
    let zero_plan = zero_table
        .assertions
        .values()
        .next()
        .and_then(|plans| plans.first())
        .expect("table-free module assertion plan");
    assert!(zero_plan.dependencies.is_empty());
    assert_eq!(zero_plan.effects, EffectSummary::default());
}

#[test]
fn module_assertion_invoking_pure_helper_retains_transitive_table_dependencies() {
    let result = analyze(&[ModuleInput::new(
        "transitive-helper.orna",
        include_str!("fixtures/inline-semantic_graph/85af910a7e57.orna"),
    )]);

    assert!(
        !has(&result, DIAG_ASSERTION_SCOPE),
        include_str!("fixtures/inline-semantic_graph/6cf914ae92c4.orna"),
        result.diagnostics
    );
    assert!(
        has(&result, DIAG_ASSERTION_EFFECT),
        "the helper's existing database-read/failure summary must remain visible: {:?}",
        result.diagnostics
    );
    let module = result.modules.values().next().expect("semantic module");
    let helper = module.exports.get("related").expect("pure helper export");
    assert_eq!(
        helper.effects.effects,
        std::collections::BTreeSet::from(["database read".into()])
    );
    assert!(helper.effects.may_fail);

    let plan = result
        .assertions
        .values()
        .next()
        .and_then(|plans| plans.first())
        .expect("module assertion plan");
    assert_eq!(
        plan.dependencies,
        std::collections::BTreeSet::from(["Account".into(), "User".into()])
    );
    assert_eq!(plan.effects, helper.effects);
}

#[test]
fn module_assertion_dependencies_count_resolved_tables_not_uppercase_values() {
    let result = analyze(&[ModuleInput::new(
        "one-table-optional.orna",
        include_str!("fixtures/inline-semantic_graph/7a3741813616.orna"),
    )]);

    assert!(has(&result, DIAG_ASSERTION_ONE_TABLE));
    let plans = result
        .assertions
        .values()
        .next()
        .expect("module assertion plan");
    assert_eq!(
        plans[0].dependencies,
        std::collections::BTreeSet::from(["User".into()])
    );
}

#[test]
fn module_assertion_dependencies_follow_resolved_lowercase_table_names() {
    let result = analyze(&[ModuleInput::new(
        "lowercase-tables.orna",
        include_str!("fixtures/inline-semantic_graph/ad9dcd04f4c7.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let plans = result
        .assertions
        .values()
        .next()
        .expect("module assertion plan");
    assert_eq!(
        plans[0].dependencies,
        std::collections::BTreeSet::from(["accounts".into(), "users".into()])
    );
}

#[test]
fn legacy_system_and_result_forms_keep_phase_specific_diagnostics() {
    let runtime = analyze(&[ModuleInput::new(
        "runtime.orna",
        include_str!("fixtures/inline-semantic_graph/a4160353f082.orna"),
    )]);
    assert!(has(&runtime, DIAG_LEGACY_SYS_RUNTIME));
    assert!(
        runtime
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "`sys.runtime` was renamed to `sys.rt`" })
    );

    let storage = analyze(&[ModuleInput::new(
        "storage.orna",
        include_str!("fixtures/inline-semantic_graph/d04fa7ab174f.orna"),
    )]);
    assert!(storage.diagnostics.iter().any(|diagnostic| {
        diagnostic.message()
            == "`sys.storage` is a grouping namespace; use `sys.Storage` or `sys.admin` storage functions"
    }));

    let result = analyze(&[ModuleInput::new(
        "result.orna",
        include_str!("fixtures/inline-semantic_graph/a1e7aae36a3a.orna"),
    )]);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == include_str!("fixtures/inline-semantic_graph/13f0339f1c09.orna")
    }));

    let try_from = analyze(&[ModuleInput::new(
        "try-from.orna",
        include_str!("fixtures/inline-semantic_graph/6b8dac4b53a1.orna"),
    )]);
    assert!(has(&try_from, DIAG_LEGACY_TRYFROM));
    assert!(try_from.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == include_str!("fixtures/inline-semantic_graph/c19a6841280f.orna")
    }));
}

#[test]
fn closed_literal_addition_diagnostics_preserve_published_meaning() {
    let currencies = analyze(&[ModuleInput::new(
        "currency.orna",
        include_str!("fixtures/inline-semantic_graph/1b8649578d3c.orna"),
    )]);
    assert!(currencies.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "cannot add different currencies without conversion"
    }));

    let dimensions = analyze(&[ModuleInput::new(
        "dimensions.orna",
        include_str!("fixtures/inline-semantic_graph/14ce14a24d45.orna"),
    )]);
    assert!(
        dimensions
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message() == "cannot add Time and Energy")
    );
}

#[test]
fn contextual_numeric_and_exact_money_unit_postfixes_remain_closed() {
    let result = analyze(&[ModuleInput::new(
        "literals.orna",
        include_str!("fixtures/inline-semantic_graph/7f039374396b.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let non_currency = analyze(&[ModuleInput::new(
        "literals.orna",
        include_str!("fixtures/inline-semantic_graph/4f50dc5cff02.orna"),
    )]);
    assert!(has(&non_currency, DIAG_TYPE));

    let unsupported = analyze(&[ModuleInput::new(
        "rates.orna",
        include_str!("fixtures/inline-semantic_graph/66566070ac1a.orna"),
    )]);
    assert!(has(&unsupported, DIAG_UNSUPPORTED));
}

#[test]
fn numeric_methods_and_relation_count_use_closed_intrinsic_shapes() {
    let result = analyze(&[ModuleInput::new(
        "intrinsics.orna",
        include_str!("fixtures/numeric-methods-relation-count.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn relation_windows_admit_the_standard_size_and_step_contract() {
    let valid = analyze(&[ModuleInput::new(
        "windows.orna",
        include_str!("fixtures/inline-semantic_graph/4946fa75883a.orna"),
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);
    let module = valid
        .modules
        .values()
        .find(|module| module.namespace.display() == "windows")
        .unwrap();
    for name in ["default_step", "named_step", "direct", "dynamic"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. }
                if result.as_ref()
                    == &Type::Relation(Box::new(Type::List(Box::new(Type::Named("Note".into())))))
        ));
    }

    for (body, expected) in [
        (
            "Note | window()",
            include_str!("fixtures/inline-semantic_graph/fe5fd540d62e.orna"),
        ),
        (
            "Note | window(2, 3, 4)",
            include_str!("fixtures/inline-semantic_graph/fe5fd540d62e.orna"),
        ),
        ("Note | window(\"two\")", "window size must be an Int"),
        ("Note | window(2, \"three\")", "window step must be an Int"),
        ("Note | window(0)", "window size must be positive"),
        ("Note | window(-1)", "window size must be positive"),
        (
            include_str!("fixtures/inline-semantic_graph/7c59678f61e8.orna"),
            "window step must be positive",
        ),
    ] {
        let result = analyze(&[ModuleInput::new(
            "invalid-window.orna",
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/8f6114f1e1fb.orna"),
                &[("body", &body)],
            ),
        )]);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message() == expected),
            "{body}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn authoritative_core_exposes_implicit_encoding_and_duration_members() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "standard.orna",
            include_str!("fixtures/inline-semantic_graph/aeffa575d43a.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn authoritative_core_types_locale_aware_money_pipeline() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "receipt.orna",
            include_str!("fixtures/inline-semantic_graph/e9055e4effff.orna"),
        )],
        &Catalogue::authoritative_core(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let wrong_input = analyze_with_catalogue(
        &[ModuleInput::new(
            "receipt.orna",
            include_str!("fixtures/inline-semantic_graph/97fa4e575786.orna"),
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(has(&wrong_input, DIAG_TYPE));
}

#[test]
fn parallel_callbacks_share_result_type_and_return_ordered_stream() {
    let source = include_str!("fixtures/parallel-callback-results.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("parallel-callback-results.orna", source)],
        &Catalogue::authoritative_core(),
    );
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.message() == "parallel callbacks must have compatible result types"
        }),
        "incompatible callback result must be diagnosed: {:?}",
        result.diagnostics
    );
    let module = result
        .modules
        .values()
        .find(|module| module.symbols.contains_key("ordered"))
        .expect("fixture module is analyzed");
    assert_eq!(
        module.symbols["ordered"].ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            default_parameters: Default::default(),
            result: Box::new(Type::Stream(Box::new(Type::Int))),
        }
    );
}

#[test]
fn race_callbacks_share_result_type_and_return_that_type() {
    let source = include_str!("fixtures/race-callback-results.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("race-callback-results.orna", source)],
        &Catalogue::authoritative_core(),
    );
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.message() == "race callbacks must have compatible result types"
        }),
        "incompatible callback result must be diagnosed: {:?}",
        result.diagnostics
    );
    let module = result
        .modules
        .values()
        .find(|module| module.symbols.contains_key("winner"))
        .expect("fixture module is analyzed");
    assert_eq!(
        module.symbols["winner"].ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            default_parameters: Default::default(),
            result: Box::new(Type::Int),
        }
    );
}

#[test]
fn timeout_callback_and_duration_are_admitted_and_checked() {
    let source = include_str!("fixtures/timeout-callback-results.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("timeout-callback-results.orna", source)],
        &Catalogue::authoritative_core(),
    );
    for expected in [
        "timeout callback must be a function",
        "timeout callback must not take parameters",
        "timeout duration must be a Duration",
        "timeout expects a callback and a duration",
    ] {
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message() == expected),
            "missing `{expected}` diagnostic: {:?}",
            result.diagnostics
        );
    }
    let module = result
        .modules
        .values()
        .find(|module| module.symbols.contains_key("completed"))
        .expect("fixture module is analyzed");
    assert_eq!(
        module.symbols["completed"].ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            default_parameters: Default::default(),
            result: Box::new(Type::Int),
        }
    );
}

#[test]
fn authoritative_fixture_resolves_attached_tables_connectors_and_modules() {
    let sources = [
        (
            "attached tables",
            ModuleInput::new(
                "attached_tables.orna",
                include_str!("fixtures/inline-semantic_graph/f62473b875f3.orna"),
            ),
        ),
        (
            "attached connectors",
            ModuleInput::new(
                "attached_connectors.orna",
                include_str!("fixtures/inline-semantic_graph/9ecf374d8798.orna"),
            ),
        ),
        (
            "named attached row",
            ModuleInput::new(
                "named_row.orna",
                include_str!("fixtures/inline-semantic_graph/46ea37039746.orna"),
            ),
        ),
        (
            "snapshot attached table",
            ModuleInput::new(
                "snapshot_table.orna",
                include_str!("fixtures/inline-semantic_graph/d3644b797367.orna"),
            ),
        ),
        (
            "system observations",
            ModuleInput::new(
                "authoritative_system_observations.orna",
                include_str!("fixtures/authoritative-system-observations.orna"),
            ),
        ),
        (
            "system runtime presentation",
            ModuleInput::new(
                "system_runtime_presentation.orna",
                include_str!("fixtures/inline-semantic_graph/0bbb6da0dccf.orna"),
            ),
        ),
        (
            "inferred relation parameter",
            ModuleInput::new(
                "inferred_relation.orna",
                include_str!("fixtures/inline-semantic_graph/ecf247280c89.orna"),
            ),
        ),
        (
            "recovery lambda",
            ModuleInput::new(
                "recovery.orna",
                include_str!("fixtures/inline-semantic_graph/71e0355c51eb.orna"),
            ),
        ),
    ];
    for (name, source) in sources {
        let result = analyze_with_catalogue(&[source], &Catalogue::authoritative_fixture());
        assert!(result.is_ok(), "{name}: {:?}", result.diagnostics);
    }
}

#[test]
fn authoritative_fixture_resolves_attached_csv_without_inventing_a_schema() {
    let source = include_str!("fixtures/attached_csv.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("attached_csv.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn frozen_historical_program_resolves_through_authoritative_projection() {
    let source = include_str!("fixtures/historical-program.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("historical-program.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn historical_callable_context_survives_namespace_decomposition() {
    let source = include_str!("fixtures/historical-closure-context-decomposed.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("historical-closure-context.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(
        result.is_ok(),
        "{:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code())
            .collect::<Vec<_>>()
    );
}

#[test]
fn distinct_historical_callable_contexts_cannot_be_mixed() {
    let source = include_str!("fixtures/historical-closure-context-mixed.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("historical-closure-context-mixed.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(
        !result.is_ok(),
        "{:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code())
            .collect::<Vec<_>>()
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "{:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code())
            .collect::<Vec<_>>()
    );
}

#[test]
fn equivalent_head_selectors_share_historical_callable_context() {
    let source = include_str!("fixtures/historical-selector-equivalent-head.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("historical-selector-equivalent-head.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn reassigned_dynamic_selector_does_not_reuse_a_historical_pin() {
    let source = include_str!("fixtures/historical-dynamic-pin-reassignment.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("historical-dynamic-pin-reassignment.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    let codes = result
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code())
        .collect::<Vec<_>>();
    let messages = result
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&DIAG_TYPE),
        "unexpected diagnostics: {codes:?} {messages:?}"
    );
}

#[test]
fn reused_dynamic_snapshot_ref_preserves_its_historical_pin() {
    let source = include_str!("fixtures/historical-dynamic-pin-reused.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("historical-dynamic-pin-reused.orna", source)],
        &Catalogue::authoritative_fixture(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn dynamic_selector_occurrences_from_distinct_modules_do_not_alias() {
    let module_source = include_str!("fixtures/historical-dynamic-module-pin.orna");
    let result = analyze_with_catalogue(
        &[
            ModuleInput::new("left.orna", module_source),
            ModuleInput::new("right.orna", module_source),
        ],
        &Catalogue::authoritative_fixture(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let left = &result.modules[&Namespace(vec!["left".into()])].symbols["pin"].ty;
    let right = &result.modules[&Namespace(vec!["right".into()])].symbols["pin"].ty;
    assert_ne!(left, right, "source-local dynamic pins must remain distinct");
}

fn historical_nested_callable_catalogue() -> Catalogue {
    historical_nested_callable_catalogue_with_other(false)
}

fn historical_nested_callable_catalogue_with_other(include_other: bool) -> Catalogue {
    let continuation = Type::Function {
        parameters: Vec::new(),
        parameter_names: Some(Vec::new()),
        default_parameters: BTreeSet::new(),
        result: Box::new(Type::Int),
    };
    let reader = Type::Function {
        parameters: Vec::new(),
        parameter_names: Some(Vec::new()),
        default_parameters: BTreeSet::new(),
        result: Box::new(continuation),
    };
    let factory = Symbol {
        kind: SymbolKind::Function,
        ty: Type::Function {
            parameters: Vec::new(),
            parameter_names: Some(Vec::new()),
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::Record(BTreeMap::from([("read".into(), reader)]))),
        },
        public: true,
        effects: EffectSummary::default(),
        generic_parameters: Vec::new(),
        enum_variants: BTreeSet::new(),
        table_schema: None,
    };
    let mut symbols = BTreeMap::from([("factory".to_owned(), factory)]);
    if include_other {
        symbols.insert(
            "other".into(),
            Symbol {
                kind: SymbolKind::Function,
                ty: Type::Function {
                    parameters: Vec::new(),
                    parameter_names: Some(Vec::new()),
                    default_parameters: BTreeSet::new(),
                    result: Box::new(Type::Text),
                },
                public: true,
                effects: EffectSummary::default(),
                generic_parameters: Vec::new(),
                enum_variants: BTreeSet::new(),
                table_schema: None,
            },
        );
    }
    Catalogue::authoritative_fixture().with_historical_modules([ModuleHeader {
        namespace: Namespace(vec!["energy".into()]),
        exports: symbols.clone(),
        symbols,
        generic_functions: BTreeMap::new(),
        prelude_exports: BTreeSet::new(),
        implicit: true,
    }])
}

#[test]
fn nested_historical_callable_context_survives_decomposition() {
    let source = include_str!("fixtures/historical-nested-closure-context.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new("historical-nested-closure-context.orna", source)],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn nested_callables_from_distinct_historical_contexts_cannot_mix() {
    let source = include_str!("fixtures/historical-nested-closure-context-mixed.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-nested-closure-context-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn dynamic_nested_callable_context_survives_decomposition() {
    let source = include_str!("fixtures/historical-dynamic-nested-closure-context.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-dynamic-nested-closure-context.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn nested_callables_from_distinct_dynamic_contexts_cannot_mix() {
    let source = include_str!("fixtures/historical-dynamic-nested-closure-context-mixed.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-dynamic-nested-closure-context-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn calls_to_dynamic_pin_helpers_keep_call_site_identity() {
    let source = include_str!("fixtures/historical-dynamic-helper-closure-context-mixed.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-dynamic-helper-closure-context-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn reassigned_dynamic_selector_keeps_helper_closure_pins_distinct() {
    let source = include_str!("fixtures/historical-dynamic-helper-reassignment.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-dynamic-helper-reassignment.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn dynamic_helper_literal_head_context_matches_symbolic_head() {
    let source = include_str!("fixtures/historical-dynamic-helper-head-context.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-dynamic-helper-head-context.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn resolved_snapshot_identity_survives_specialized_pinned_closure_chains() {
    let source = include_str!("fixtures/historical-pinned-closure-chain-same.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-same.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn different_resolved_snapshots_stay_separate_across_pinned_closure_chains() {
    let source = include_str!("fixtures/historical-pinned-closure-chain-distinct.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-distinct.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "distinct pins should not compose: {:?}",
        result.diagnostics
    );
}

#[test]
fn rebinding_snapshot_pin_preserves_old_and_specializes_new_closure_chains() {
    let source = include_str!("fixtures/historical-pinned-closure-chain-rebinding.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-rebinding.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn rebound_snapshot_closures_from_distinct_pins_cannot_mix() {
    let source = include_str!("fixtures/historical-pinned-closure-chain-rebinding-mixed.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-rebinding-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "closures from different sides of a pin rebind should not compose: {:?}",
        result.diagnostics
    );
}

#[test]
fn pinned_closure_identities_survive_parameter_rebinding_chains() {
    let source = include_str!("fixtures/historical-pinned-closure-chain-parameter-rebinding.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-parameter-rebinding.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "{:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn each_call_specializes_all_stages_of_a_pinned_rebinding_chain() {
    let source = include_str!("fixtures/historical-pinned-closure-chain-parameter-rebinding.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-parameter-rebinding.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "{:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn closure_contexts_from_parameter_rebinding_stages_stay_distinct() {
    let source = include_str!(
        "fixtures/historical-pinned-closure-chain-parameter-rebinding-mixed.orna"
    );
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-parameter-rebinding-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "closures from separate parameter rebinding stages should not compose: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn closure_contexts_from_distinct_rebinding_chain_calls_do_not_merge() {
    let source = include_str!(
        "fixtures/historical-pinned-closure-chain-parameter-rebinding-mixed.orna"
    );
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-parameter-rebinding-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "closures from distinct rebinding-chain calls should not compose: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn pinned_closure_identity_survives_rebinding_chain_storms() {
    let source = include_str!("fixtures/historical-pinned-closure-chain-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "{:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn pinned_closure_chain_storms_reject_cross_stage_and_cross_call_mixing() {
    let source = include_str!("fixtures/historical-pinned-closure-chain-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "closures from separate storm stages and call sites should not compose: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn closure_rebinding_retains_identity_through_nested_callables() {
    let source = include_str!("fixtures/historical-pinned-closure-chain-closure-rebind.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-closure-rebind.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "{:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn closure_rebinding_does_not_merge_old_and_new_pin_contexts() {
    let source = include_str!(
        "fixtures/historical-pinned-closure-chain-closure-rebind-mixed.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-closure-rebind-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "closures rebound across distinct pins should not compose: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn closure_rebinding_rejects_a_changed_callable_shape() {
    let source = include_str!(
        "fixtures/historical-pinned-closure-chain-closure-rebind-shape-mismatch.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-closure-rebind-shape-mismatch.orna",
            source,
        )],
        &historical_nested_callable_catalogue_with_other(true),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "closures with different callable contracts should not rebind: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn closure_identity_returns_with_its_pin_after_rebinding_round_trips() {
    // The reference requires exact SnapshotRef pinning but does not specify
    // whether repeated local closure rebinds create a new identity. Identity
    // follows the pinned snapshot value, so returning to a prior pin rejoins it.
    let source = include_str!(
        "fixtures/historical-pinned-closure-chain-rebind-round-trip.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-rebind-round-trip.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "{:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn closure_identity_round_trips_do_not_merge_intermediate_pins() {
    let source = include_str!(
        "fixtures/historical-pinned-closure-chain-rebind-round-trip-mixed.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chain-rebind-round-trip-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "round-tripping to an older pin must not merge an intermediate closure: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn closure_bundle_rebinding_retains_component_pin_identities() {
    let source = include_str!("fixtures/historical-pinned-closure-bundle-rebind.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-bundle-rebind.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "{:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn closure_bundle_round_trips_keep_intermediate_pins_distinct() {
    let source = include_str!(
        "fixtures/historical-pinned-closure-bundle-rebind-mixed.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-bundle-rebind-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    let type_diagnostics = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == DIAG_TYPE)
        .collect::<Vec<_>>();
    assert_eq!(
        type_diagnostics.len(),
        1,
        "bundle rebinds should succeed while distinct pin values stay incompatible: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn structural_closure_rebind_storm_preserves_every_stage_pin() {
    let source = include_str!("fixtures/historical-pinned-closure-bundle-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-bundle-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "{:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn structural_closure_rebind_storm_keeps_nonadjacent_pins_distinct() {
    let source = include_str!(
        "fixtures/historical-pinned-closure-bundle-storm-mixed.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-bundle-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    let type_diagnostics = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == DIAG_TYPE)
        .collect::<Vec<_>>();
    assert_eq!(
        type_diagnostics.len(),
        1,
        "eight rebind stages must remain distinct after returning to the first pin: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn nested_pins_keep_identity_across_rebind_chain_storms() {
    let source = include_str!("fixtures/historical-pinned-closure-nested-chain-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-nested-chain-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "nested captured pins and round-trip aliases should match their own stages: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn nested_pins_do_not_merge_intermediate_chain_stages_after_round_trip() {
    let source = include_str!("fixtures/historical-pinned-closure-nested-chain-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-nested-chain-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "nested closures from distinct intermediate pins must remain incompatible: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn nested_selector_pins_are_specialized_at_the_inner_call_across_rebind_storms() {
    let source = include_str!("fixtures/historical-pinned-closure-nested-selector-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-nested-selector-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "each nested call must specialize its own pin while retaining the anchor pin: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn nested_selector_pins_at_one_call_site_remain_distinct() {
    let source =
        include_str!("fixtures/historical-pinned-closure-nested-selector-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-nested-selector-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "distinct pins passed to a nested callable at one source call site must not alias: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn structural_rebind_storms_preserve_each_nested_component_pin() {
    let source = include_str!("fixtures/historical-pinned-closure-structural-pin-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-structural-pin-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "all structural rebind stages and the round trip should retain component pins: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn historical_projection_rejects_unknown_members_and_snapshot_context_mixing() {
    let catalogue = Catalogue::authoritative_fixture();
    for source in [
        include_str!("fixtures/inline-semantic_graph/24210d89392f.orna"),
        include_str!("fixtures/inline-semantic_graph/15f9fae7a062.orna"),
        include_str!("fixtures/inline-semantic_graph/ff7746686e31.orna"),
        include_str!("fixtures/inline-semantic_graph/c0ee105f91e4.orna"),
    ] {
        let result = analyze_with_catalogue(
            &[ModuleInput::new("historical-negative.orna", source)],
            &catalogue,
        );
        assert!(!result.is_ok(), "{source}: {:?}", result.diagnostics);
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.code() == DIAG_TYPE || diagnostic.code() == DIAG_UNRESOLVED
            }),
            "{source}: {:?}",
            result.diagnostics
        );
    }
}

fn historical_effect_catalogue(effect: &str) -> Catalogue {
    let symbol = Symbol {
        kind: SymbolKind::Function,
        ty: Type::Function {
            parameters: Vec::new(),
            parameter_names: Some(Vec::new()),
            default_parameters: std::collections::BTreeSet::new(),
            result: Box::new(Type::Null),
        },
        public: true,
        effects: EffectSummary {
            effects: std::collections::BTreeSet::from([effect.to_owned()]),
            may_fail: true,
        },
        generic_parameters: Vec::new(),
        enum_variants: BTreeSet::new(),
        table_schema: None,
    };
    let symbols = std::collections::BTreeMap::from([("run".to_owned(), symbol)]);
    Catalogue::authoritative_core().with_historical_modules([ModuleHeader {
        namespace: Namespace(vec!["unsafe".into()]),
        exports: symbols.clone(),
        symbols,
        generic_functions: std::collections::BTreeMap::new(),
        prelude_exports: std::collections::BTreeSet::new(),
        implicit: true,
    }])
}

#[test]
fn historical_callables_are_read_effect_only() {
    for effect in [
        "database write",
        "checkpoint",
        "secret",
        "network",
        "external",
    ] {
        let result = analyze_with_catalogue(
            &[ModuleInput::new(
                "historical-effect.orna",
                include_str!("fixtures/inline-semantic_graph/4085c519acf1.orna"),
            )],
            &historical_effect_catalogue(effect),
        );
        assert!(!result.is_ok(), "{effect}: {:?}", result.diagnostics);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code() == DIAG_UNSUPPORTED),
            "{effect}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn qualified_kwh_units_share_the_closed_cross_database_identity() {
    let result = analyze(&[ModuleInput::new(
        "units.orna",
        include_str!("fixtures/inline-semantic_graph/4d774ab8f9ad.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let incompatible = analyze(&[ModuleInput::new(
        "units.orna",
        include_str!("fixtures/inline-semantic_graph/fa031c0cc369.orna"),
    )]);
    assert!(has(&incompatible, DIAG_TYPE));
}

#[test]
fn closed_enum_case_blocks_accept_the_core_log_intrinsic() {
    let result = analyze(&[ModuleInput::new(
        "inspection.orna",
        include_str!("fixtures/inline-semantic_graph/9d39d0050a90.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}

#[test]
fn parenthesized_pipeline_lambdas_receive_the_input_type_context() {
    let result = analyze(&[ModuleInput::new(
        "contacts.orna",
        include_str!("fixtures/inline-semantic_graph/cb8a5814f124.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
}
#[test]
fn authoritative_lambda_fixture_infers_comparison_shapes_and_connector_effects() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "lambda.orna",
            include_str!("fixtures/inline-semantic_graph/561e2f1487ab.orna"),
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "lambda")
        .expect("lambda module");
    assert!(matches!(
        &module.exports["over"].ty,
        Type::Function { result, .. }
            if matches!(
                result.as_ref(),
                Type::Function { parameters, result, .. }
                    if parameters == &[Type::Record(BTreeMap::from([(
                        "amount".into(),
                        Type::Applied {
                            base: "Money".into(),
                            arguments: vec![Type::Named("GBP".into())],
                        },
                    )]))]
                        && result.as_ref() == &Type::Bool
            )
    ));
    assert!(matches!(
        &module.exports["direct"].ty,
        Type::Function { result, .. }
            if matches!(
                result.as_ref(),
                Type::Function { parameters, result, .. }
                    if parameters == &[Type::Applied {
                        base: "Money".into(),
                        arguments: vec![Type::Named("GBP".into())],
                    }]
                        && result.as_ref() == &Type::Bool
            )
    ));
    assert!(matches!(
        &module.exports["between"].ty,
        Type::Function { result, .. }
            if matches!(
                result.as_ref(),
                Type::Function { parameters, result, .. }
                    if parameters == &[Type::Int] && result.as_ref() == &Type::Bool
            )
    ));
    let sync = &module.exports["sync_selected"];
    assert!(sync.effects.effects.contains("network"));
    assert!(sync.effects.may_fail);

    let invalid = analyze(&[ModuleInput::new(
        "invalid-lambda.orna",
        include_str!("fixtures/inline-semantic_graph/5b8f2ab000e7.orna"),
    )]);
    assert!(
        invalid
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_ANNOTATION),
        "{:?}",
        invalid.diagnostics
    );
    let invalid_equality = analyze_with_catalogue(
        &[ModuleInput::new(
            "invalid-lambda-equality.orna",
            include_str!("fixtures/inline-semantic_graph/f3b1bc905733.orna"),
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(
        invalid_equality
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "{:?}",
        invalid_equality.diagnostics
    );
}

#[test]
fn recovery_pipeline_unifies_success_type_before_later_stages() {
    let matching = analyze(&[ModuleInput::new(
        "recovery-matching.orna",
        include_str!("fixtures/inline-semantic_graph/2d1ced4c3962.orna"),
    )]);
    assert!(matching.is_ok(), "{:?}", matching.diagnostics);
    let module = matching
        .modules
        .values()
        .find(|module| module.namespace.display() == "recovery-matching")
        .expect("matching recovery module");
    assert!(matches!(
        &module.symbols["recovered"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));

    let mismatched = analyze(&[ModuleInput::new(
        "recovery-mismatched.orna",
        include_str!("fixtures/inline-semantic_graph/7d0527db3744.orna"),
    )]);
    assert!(has(&mismatched, DIAG_TYPE), "{:?}", mismatched.diagnostics);
    assert!(mismatched.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE && diagnostic.message() == "static types are incompatible"
    }));
    let module = mismatched
        .modules
        .values()
        .find(|module| module.namespace.display() == "recovery-mismatched")
        .expect("mismatched recovery module");
    assert!(matches!(
        &module.symbols["recovered"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Error
    ));
}

#[test]
fn recovery_pipeline_replaces_handled_failure_effect() {
    let result = analyze(&[ModuleInput::new(
        "recovery-effects.orna",
        include_str!("fixtures/recovery-effects-type-family.orna"),
    )]);
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "recovery-effects")
        .expect("recovery effects module");
    assert!(
        module.symbols["recovered"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(!module.symbols["recovered"].effects.may_fail);
    assert!(
        module.symbols["still_fails"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(module.symbols["still_fails"].effects.may_fail);
}

#[test]
fn fail_has_bottom_success_type_and_accepts_error_values() {
    let result = analyze(&[ModuleInput::new(
        "fail-bottom.orna",
        include_str!("fixtures/inline-semantic_graph/f4aaa7dfceb3.orna"),
    )]);
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "fail-bottom")
        .expect("fail bottom module");
    assert!(matches!(
        &module.symbols["abort"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Bottom
    ));
    assert!(module.symbols["abort"].effects.may_fail);
}

#[test]
fn fail_bottom_is_compatible_with_expected_results_and_recovery() {
    let result = analyze(&[ModuleInput::new(
        "fail-recovery.orna",
        include_str!("fixtures/inline-semantic_graph/138fe96d3c44.orna"),
    )]);
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "fail-recovery")
        .expect("fail recovery module");
    assert!(matches!(
        &module.symbols["abort"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
    assert!(matches!(
        &module.symbols["reemit"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
    assert!(module.symbols["reemit"].effects.may_fail);
}

#[test]
fn fail_rejects_wrong_argument_shape_and_type() {
    for source in [
        include_str!("fixtures/inline-semantic_graph/f34af47e38d3.orna"),
        include_str!("fixtures/inline-semantic_graph/02f859c12394.orna"),
        include_str!("fixtures/inline-semantic_graph/0d8de78d56e2.orna"),
    ] {
        let result = analyze(&[ModuleInput::new("fail-invalid.orna", source)]);
        assert!(
            has(&result, DIAG_TYPE),
            "{source}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn user_defined_fail_shadows_the_intrinsic() {
    let result = analyze(&[ModuleInput::new(
        "fail-shadow.orna",
        include_str!("fixtures/inline-semantic_graph/110f48d1a998.orna"),
    )]);
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "fail-shadow")
        .expect("fail shadow module");
    assert!(matches!(
        &module.symbols["call"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
    assert!(!module.symbols["call"].effects.may_fail);
}

#[test]
fn error_constructor_returns_an_inspectable_error_value() {
    let result = analyze(&[ModuleInput::new(
        "error-constructor.orna",
        include_str!("fixtures/inline-semantic_graph/1c2d70c6957f.orna"),
    )]);
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "error-constructor")
        .expect("error constructor module");
    for name in ["construct", "fresh", "replacing"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Named("Error".into())
        ));
    }
    assert!(module.symbols["replacing"].effects.may_fail);
}

#[test]
fn error_constructor_rejects_invalid_named_arguments() {
    for source in [
        include_str!("fixtures/inline-semantic_graph/de6f16134bff.orna"),
        include_str!("fixtures/inline-semantic_graph/00d31c7651f8.orna"),
        include_str!("fixtures/inline-semantic_graph/7f1b2afa50c6.orna"),
        include_str!("fixtures/inline-semantic_graph/0d82800b2ffc.orna"),
        include_str!("fixtures/inline-semantic_graph/2d687762cf62.orna"),
        include_str!("fixtures/inline-semantic_graph/6f93b55919a3.orna"),
    ] {
        let result = analyze(&[ModuleInput::new("error-invalid.orna", source)]);
        assert!(
            has(&result, DIAG_TYPE),
            "{source}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn user_defined_error_shadows_the_intrinsic() {
    let result = analyze(&[ModuleInput::new(
        "error-shadow.orna",
        include_str!("fixtures/inline-semantic_graph/74b36c5d4d1e.orna"),
    )]);
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "error-shadow")
        .expect("error shadow module");
    assert!(matches!(
        &module.symbols["call"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
    assert!(!module.symbols["call"].effects.may_fail);
}

#[test]
fn root_relation_and_stream_intrinsics_cover_reference_pipelines_without_execution() {
    let source = include_str!("fixtures/inline-semantic_graph/c7ae9b4c6a6b.orna");
    let result = analyze(&[ModuleInput::new("sensors.orna", source)]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "sensors")
        .unwrap();
    let ingest = module.symbols.get("ingest").unwrap();
    assert!(ingest.effects.effects.contains("database write"));
    assert!(ingest.effects.may_fail);
}

#[test]
fn authoritative_named_pipeline_fixtures_insert_the_input_before_explicit_arguments() {
    let pipe_first_argument = include_str!("fixtures/inline-semantic_graph/25ccfd0cd0b6.orna");
    let pipeline_precedence = include_str!("fixtures/inline-semantic_graph/c716531ac3a9.orna");
    let precedence_source = pipeline_precedence.to_owned();

    let result = analyze(&[
        ModuleInput::new("pipe-first-argument.orna", pipe_first_argument),
        ModuleInput::new("pipeline-precedence.orna", precedence_source),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let precedence = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "pipeline-precedence")
        .unwrap();
    assert!(matches!(
        &precedence.symbols["square_sum"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
    assert!(matches!(
        &precedence.symbols["increment_count"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
}

#[test]
fn declared_defaults_are_checked_and_shared_by_direct_and_piped_calls() {
    for expression in [
        "add(1)",
        "1 | add",
        "1 | add()",
        include_str!("fixtures/inline-semantic_graph/c2eaa9bcfae4.orna"),
        include_str!("fixtures/inline-semantic_graph/2c3958f04dc0.orna"),
        include_str!("fixtures/inline-semantic_graph/18c951f80a0b.orna"),
    ] {
        let result = analyze(&[ModuleInput::new(
            "defaults.orna",
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/3ba4b5f57e5a.orna"),
                &[("expression", &expression)],
            ),
        )]);
        assert!(result.is_ok(), "{expression}: {:?}", result.diagnostics);
    }
    for source in [
        include_str!("fixtures/inline-semantic_graph/cadceb63b07d.orna"),
        include_str!("fixtures/inline-semantic_graph/c41702fa98b9.orna"),
        include_str!("fixtures/inline-semantic_graph/fa566d98ec5d.orna"),
        include_str!("fixtures/inline-semantic_graph/336dda3a0db0.orna"),
        include_str!("fixtures/inline-semantic_graph/05c95ac02a1f.orna"),
    ] {
        let result = analyze(&[ModuleInput::new("invalid-defaults.orna", source)]);
        assert!(
            has(&result, DIAG_TYPE),
            "{source}: {:?}",
            result.diagnostics
        );
    }
    let unresolved = analyze(&[ModuleInput::new(
        "unresolved-default.orna",
        include_str!("fixtures/inline-semantic_graph/e3a8b996af16.orna"),
    )]);
    assert!(
        has(&unresolved, DIAG_UNRESOLVED),
        "{:?}",
        unresolved.diagnostics
    );
}

#[test]
fn runtime_root_and_info_function_use_the_current_sys_names() {
    let current = analyze(&[ModuleInput::new(
        "runtime.orna",
        include_str!("fixtures/inline-semantic_graph/4640934a702f.orna"),
    )]);
    assert!(
        current.is_ok(),
        "current sys runtime names: {:?}",
        current.diagnostics
    );

    let legacy = analyze(&[ModuleInput::new(
        "runtime.orna",
        include_str!("fixtures/inline-semantic_graph/a137b23101a1.orna"),
    )]);
    assert!(
        legacy
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "`sys.runtime` was renamed to `sys.rt`" })
    );
}

#[test]
fn json_described_read_only_sys_views_resolve_without_fabricating_other_members() {
    for source in [
        include_str!("fixtures/inline-semantic_graph/368d75dcc0d1.orna"),
        include_str!("fixtures/inline-semantic_graph/4385f38d8244.orna"),
        include_str!("fixtures/inline-semantic_graph/d49fdcf883d2.orna"),
        include_str!("fixtures/inline-semantic_graph/20ea57e74fe3.orna"),
        include_str!("fixtures/inline-semantic_graph/0222038936eb.orna"),
        include_str!("fixtures/inline-semantic_graph/e9eb1c9aff16.orna"),
    ] {
        let supported = analyze(&[ModuleInput::new("read-only-sys.orna", source)]);
        assert!(supported.is_ok(), "{source}: {:#?}", supported.diagnostics);
    }

    let shadowed = analyze(&[ModuleInput::new(
        "shadowed-sys.orna",
        include_str!("fixtures/inline-semantic_graph/b73074596af8.orna"),
    )]);
    assert!(has(&shadowed, DIAG_RESERVED), "{:#?}", shadowed.diagnostics);

    for source in [
        include_str!("fixtures/inline-semantic_graph/8c85c43ad3e8.orna"),
        include_str!("fixtures/inline-semantic_graph/afe66c207bd7.orna"),
        include_str!("fixtures/inline-semantic_graph/495e086d6e66.orna"),
        include_str!("fixtures/inline-semantic_graph/84e7b585048e.orna"),
        include_str!("fixtures/inline-semantic_graph/75c03044e2a8.orna"),
    ] {
        let rejected = analyze(&[ModuleInput::new("reserved-sys.orna", source)]);
        assert!(
            has(&rejected, DIAG_RESERVED),
            "{source}: {:#?}",
            rejected.diagnostics
        );
    }

    let reserved_table = analyze(&[ModuleInput::new(
        "reserved-sys.orna",
        include_str!("fixtures/inline-semantic_graph/6b942f43b3cf.orna"),
    )]);
    assert!(
        reserved_table.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_TYPE && diagnostic.message() == "`sys` is reserved"
        }),
        "{:#?}",
        reserved_table.diagnostics
    );
    assert!(
        !has(&reserved_table, DIAG_RESERVED),
        "table-name validation belongs to typechecking: {:#?}",
        reserved_table.diagnostics
    );

    let alias = analyze(&[
        ModuleInput::new(
            "helper.orna",
            include_str!("fixtures/inline-semantic_graph/136f28c1e416.orna"),
        ),
        ModuleInput::new(
            "alias-sys.orna",
            include_str!("fixtures/inline-semantic_graph/d46b48e72e37.orna"),
        ),
    ]);
    assert!(has(&alias, DIAG_RESERVED), "{:#?}", alias.diagnostics);
    assert!(
        !has(&alias, DIAG_UNRESOLVED),
        "the built-in root must remain available: {:#?}",
        alias.diagnostics
    );

    let metadata = analyze(&[ModuleInput::new(
        "unsupported-sys.orna",
        include_str!("fixtures/inline-semantic_graph/14bfc3dca48f.orna"),
    )]);
    assert!(metadata.is_ok(), "{:#?}", metadata.diagnostics);
    let metadata = &metadata.modules.values().next().unwrap().symbols["metadata"];
    assert!(matches!(
        &metadata.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.ValueMetadata".into(),
                arguments: vec![Type::Int],
            }
    ));
    assert!(metadata.effects.effects.contains("database read"));
    assert!(metadata.effects.may_fail);

    let named_metadata = analyze(&[ModuleInput::new(
        "named-metadata.orna",
        include_str!("fixtures/inline-semantic_graph/e0c00864b5d1.orna"),
    )]);
    assert!(named_metadata.is_ok(), "{:#?}", named_metadata.diagnostics);
    let named_metadata = &named_metadata.modules.values().next().unwrap().symbols["metadata"];
    assert!(matches!(
        &named_metadata.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.ValueMetadata".into(),
                arguments: vec![Type::Int],
            }
    ));

    let explicit_metadata = analyze(&[ModuleInput::new(
        "explicit-metadata.orna",
        include_str!("fixtures/inline-semantic_graph/da5d20668255.orna"),
    )]);
    assert!(
        explicit_metadata.is_ok(),
        "{:#?}",
        explicit_metadata.diagnostics
    );
    let explicit_metadata = explicit_metadata.modules.values().next().unwrap();
    for (name, value_type) in [
        ("integer", Type::Int),
        ("piped", Type::Int),
        ("text", Type::Text),
    ] {
        let symbol = &explicit_metadata.exports[name];
        assert!(matches!(
            &symbol.ty,
            Type::Function { result, .. }
                if result.as_ref() == &Type::Applied {
                    base: "sys.ValueMetadata".into(),
                    arguments: vec![value_type.clone()],
                }
        ));
        assert!(symbol.effects.effects.contains("database read"));
        assert!(symbol.effects.may_fail);
    }
    let generic = &explicit_metadata.exports["generic"];
    assert!(matches!(
        &generic.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.ValueMetadata".into(),
                arguments: vec![Type::Named("T".into())],
            }
    ));
    let money = &explicit_metadata.exports["money"];
    assert!(matches!(
        &money.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "Money".into(),
                arguments: vec![Type::Named("GBP".into())],
            }
    ));

    for source in [
        include_str!("fixtures/inline-semantic_graph/8a231b7f0234.orna"),
        include_str!("fixtures/inline-semantic_graph/a56f3231162f.orna"),
        include_str!("fixtures/inline-semantic_graph/b6e52055a4d6.orna"),
        include_str!("fixtures/inline-semantic_graph/bff06d6a5b7c.orna"),
    ] {
        let rejected = analyze(&[ModuleInput::new("invalid-explicit-metadata.orna", source)]);
        assert!(
            has(&rejected, DIAG_TYPE),
            "{source}: {:#?}",
            rejected.diagnostics
        );
    }

    let invalid_metadata = analyze(&[ModuleInput::new(
        "unsupported-sys.orna",
        include_str!("fixtures/inline-semantic_graph/671e04bbded9.orna"),
    )]);
    assert!(
        has(&invalid_metadata, DIAG_TYPE),
        "{:#?}",
        invalid_metadata.diagnostics
    );

    let invalid_label = analyze(&[ModuleInput::new(
        "invalid-metadata-label.orna",
        include_str!("fixtures/inline-semantic_graph/3d892d7310cd.orna"),
    )]);
    assert!(
        has(&invalid_label, DIAG_TYPE),
        "{:#?}",
        invalid_label.diagnostics
    );

    let invalid_value = analyze(&[ModuleInput::new(
        "invalid-metadata-value.orna",
        include_str!("fixtures/inline-semantic_graph/8fcd56f0b5d4.orna"),
    )]);
    assert!(
        has(&invalid_value, DIAG_UNRESOLVED),
        "{:#?}",
        invalid_value.diagnostics
    );
    let invalid_value = &invalid_value.modules.values().next().unwrap().symbols["metadata"];
    assert!(matches!(
        &invalid_value.ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Error
    ));

    for source in [
        include_str!("fixtures/inline-semantic_graph/3cc09ee1d5c1.orna"),
        include_str!("fixtures/inline-semantic_graph/f67da7274e96.orna"),
    ] {
        let rejected = analyze(&[ModuleInput::new("unsupported-sys.orna", source)]);
        assert!(
            has(&rejected, DIAG_UNSUPPORTED),
            "{source}: {:#?}",
            rejected.diagnostics
        );
    }
}

#[test]
fn sys_await_substitutes_the_invocation_handle_result_type_and_rejects_bad_calls() {
    let valid = analyze(&[ModuleInput::new(
        "await-generic.orna",
        include_str!("fixtures/inline-semantic_graph/b7b19990d4e5.orna"),
    )]);
    assert!(valid.is_ok(), "{:#?}", valid.diagnostics);
    let awaited = &valid.modules.values().next().unwrap().exports["awaited"];
    assert!(matches!(
        &awaited.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.InvocationResult".into(),
                arguments: vec![Type::Named("sys.Value".into())],
            }
    ));
    assert!(awaited.effects.effects.contains("admin"));
    assert!(awaited.effects.effects.contains("invoke"));
    assert!(awaited.effects.may_fail);

    let typed = analyze(&[ModuleInput::new(
        "await-generic-typed.orna",
        include_str!("fixtures/inline-semantic_graph/ef89978ed4a6.orna"),
    )]);
    assert!(typed.is_ok(), "{:#?}", typed.diagnostics);
    let typed = typed.modules.values().next().unwrap();
    for name in [
        "omitted",
        "null_timeout",
        "duration_timeout",
        "explicit",
        "explicit_named",
    ] {
        let typed = &typed.exports[name];
        assert!(matches!(
            &typed.ty,
            Type::Function { result, .. }
                if result.as_ref() == &Type::Applied {
                    base: "sys.InvocationResult".into(),
                    arguments: vec![Type::Int],
                }
        ));
        assert_eq!(
            typed.effects.effects,
            std::collections::BTreeSet::from(["invoke".into()])
        );
        assert!(typed.effects.may_fail);
    }

    let local_generic = &typed.exports["local_generic"];
    assert!(matches!(
        &local_generic.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.InvocationResult".into(),
                arguments: vec![Type::Named("T".into())],
            }
    ));
    assert_eq!(
        local_generic.effects.effects,
        std::collections::BTreeSet::from(["invoke".into()])
    );
    assert!(local_generic.effects.may_fail);

    for source in [
        include_str!("fixtures/inline-semantic_graph/5a322d215d12.orna"),
        include_str!("fixtures/inline-semantic_graph/c6eb946e8d56.orna"),
        include_str!("fixtures/inline-semantic_graph/aa60b21ff645.orna"),
        include_str!("fixtures/inline-semantic_graph/7d5ac0848574.orna"),
        include_str!("fixtures/inline-semantic_graph/1f027a728b4b.orna"),
        include_str!("fixtures/inline-semantic_graph/1a2ead28189c.orna"),
        include_str!("fixtures/inline-semantic_graph/4a56f6b09b69.orna"),
        include_str!("fixtures/inline-semantic_graph/8b9ef94631ca.orna"),
    ] {
        let rejected = analyze(&[ModuleInput::new("await-generic-invalid.orna", source)]);
        assert!(
            has(&rejected, DIAG_TYPE),
            "{source}: {:#?}",
            rejected.diagnostics
        );
    }

    let empty_generic = analyze(&[ModuleInput::new(
        "await-generic-invalid.orna",
        include_str!("fixtures/inline-semantic_graph/0c4df1af2baa.orna"),
    )]);
    assert!(
        has(&empty_generic, "ORNA-S000-PARSE"),
        "{:#?}",
        empty_generic.diagnostics
    );

    let unknown_generic = analyze(&[ModuleInput::new(
        "await-generic-invalid.orna",
        include_str!("fixtures/inline-semantic_graph/5e577c07bdbf.orna"),
    )]);
    assert!(
        has(&unknown_generic, DIAG_TYPE),
        "{:#?}",
        unknown_generic.diagnostics
    );

    let malformed_cancel = analyze(&[ModuleInput::new(
        "cancel-generic-invalid.orna",
        include_str!("fixtures/inline-semantic_graph/e0385a574e3a.orna"),
    )]);
    assert!(
        has(&malformed_cancel, DIAG_UNRESOLVED),
        "{:#?}",
        malformed_cancel.diagnostics
    );
    let malformed = &malformed_cancel.modules.values().next().unwrap().symbols["unsupported"];
    assert!(matches!(
        &malformed.ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Error
    ));

    let mismatched_generic = analyze(&[ModuleInput::new(
        "await-generic-unsupported.orna",
        include_str!("fixtures/inline-semantic_graph/a78dbf203c9b.orna"),
    )]);
    assert!(
        has(&mismatched_generic, DIAG_TYPE),
        "{:#?}",
        mismatched_generic.diagnostics
    );

    let already_error = analyze(&[ModuleInput::new(
        "await-generic-error.orna",
        include_str!("fixtures/inline-semantic_graph/18fbe5fbb6e1.orna"),
    )]);
    assert!(
        has(&already_error, DIAG_UNRESOLVED),
        "{:#?}",
        already_error.diagnostics
    );
    let invalid = &already_error.modules.values().next().unwrap().symbols["invalid"];
    assert!(matches!(
        &invalid.ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Error
    ));
}

#[test]
fn sys_cancel_infers_and_validates_the_invocation_handle_result_type() {
    let valid = analyze(&[ModuleInput::new(
        "cancel-generic.orna",
        include_str!("fixtures/inline-semantic_graph/131a63b72fbd.orna"),
    )]);
    assert!(valid.is_ok(), "{:#?}", valid.diagnostics);
    let module = valid.modules.values().next().unwrap();
    for name in [
        "inferred",
        "positional",
        "positional_null",
        "positional_explicit",
        "explicit",
        "named",
        "local_generic",
        "piped",
        "piped_positional",
        "piped_positional_null",
        "piped_positional_explicit",
        "piped_explicit",
    ] {
        let symbol = &module.exports[name];
        assert!(matches!(
            &symbol.ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Bool
        ));
        assert_eq!(
            symbol.effects.effects,
            std::collections::BTreeSet::from(["invoke".into()])
        );
        assert!(symbol.effects.may_fail);
    }

    for (name, source) in [
        (
            "missing",
            include_str!("fixtures/inline-semantic_graph/9ba8c6cf8261.orna"),
        ),
        (
            "wrong_handle",
            include_str!("fixtures/inline-semantic_graph/7c420bb274eb.orna"),
        ),
        (
            "wrong_reason",
            include_str!("fixtures/inline-semantic_graph/8851fd64f79c.orna"),
        ),
        (
            "mismatched_generic",
            include_str!("fixtures/inline-semantic_graph/cd68f64316ae.orna"),
        ),
        (
            "unknown_generic",
            include_str!("fixtures/inline-semantic_graph/1d938f9387fd.orna"),
        ),
        (
            "extra_generic",
            include_str!("fixtures/inline-semantic_graph/1b3048164a64.orna"),
        ),
        (
            "duplicate",
            include_str!("fixtures/inline-semantic_graph/516ec625cceb.orna"),
        ),
        (
            "unknown_label",
            include_str!("fixtures/inline-semantic_graph/efd83b5f37e7.orna"),
        ),
        (
            "positional_after_named",
            include_str!("fixtures/inline-semantic_graph/10633c1b7837.orna"),
        ),
        (
            "pipeline_positional_after_named",
            include_str!("fixtures/inline-semantic_graph/bf067a7cd5eb.orna"),
        ),
    ] {
        let rejected = analyze(&[ModuleInput::new("cancel-generic-invalid.orna", source)]);
        assert!(
            has(&rejected, DIAG_TYPE),
            "{name}: {:#?}",
            rejected.diagnostics
        );
        let symbol = &rejected.modules.values().next().unwrap().symbols[name];
        assert!(matches!(
            &symbol.ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Error
        ));
    }
}

#[test]
fn sys_invoke_typed_admission_substitutes_the_explicit_result_witness() {
    // ORNA-SYS-077/078/079: semantic admission keeps reflection behind the
    // typed descriptor boundary; runtime binding owns the pinned identity and
    // canonical argument envelope after this check.
    // ORNA-SYS-132/140: `as: T` is explicit, exact, and never inferred as
    // `sys.Value`. ORNA-SYS-039: the invoke effect remains conservative.
    let valid = analyze(&[ModuleInput::new(
        "invoke-generic.orna",
        include_str!("fixtures/inline-semantic_graph/5dc672934689.orna"),
    )]);
    assert!(valid.is_ok(), "{:#?}", valid.diagnostics);
    let module = valid.modules.values().next().unwrap();
    for name in ["positional", "named", "piped"] {
        let symbol = &module.exports[name];
        assert!(matches!(
            &symbol.ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Int
        ));
        assert_eq!(
            symbol.effects.effects,
            std::collections::BTreeSet::from(["invoke".into()])
        );
        assert!(symbol.effects.may_fail);
    }
    let local_generic = &module.exports["local_generic"];
    assert!(matches!(
        &local_generic.ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Named("T".into())
    ));
    assert_eq!(
        local_generic.effects.effects,
        std::collections::BTreeSet::from(["invoke".into()])
    );
    assert!(local_generic.effects.may_fail);
    let erased = &module.exports["erased"];
    assert!(matches!(
        &erased.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Named("sys.Value".into())
    ));
    assert_eq!(
        erased.effects.effects,
        std::collections::BTreeSet::from(["invoke".into()])
    );
    assert!(erased.effects.may_fail);

    for (name, source, message) in [
        (
            "missing",
            include_str!("fixtures/inline-semantic_graph/c2cef1f532cf.orna"),
            include_str!("fixtures/inline-semantic_graph/bfc6f8ead90d.orna"),
        ),
        (
            "malformed",
            include_str!("fixtures/inline-semantic_graph/7a38ea6a06cc.orna"),
            "sys.invoke as: witness must name a known static type",
        ),
        (
            "mismatched",
            include_str!("fixtures/inline-semantic_graph/e908908f288a.orna"),
            include_str!("fixtures/inline-semantic_graph/42284b7f3426.orna"),
        ),
        (
            "extra_generic",
            include_str!("fixtures/inline-semantic_graph/34d3dea337fc.orna"),
            include_str!("fixtures/inline-semantic_graph/f1d092cfeeee.orna"),
        ),
        (
            "unknown_generic",
            include_str!("fixtures/inline-semantic_graph/6304cc42595c.orna"),
            include_str!("fixtures/inline-semantic_graph/5f46c9c50cc9.orna"),
        ),
        (
            "wrong_target",
            include_str!("fixtures/inline-semantic_graph/a3eda0e0b196.orna"),
            include_str!("fixtures/inline-semantic_graph/6a35e14725fb.orna"),
        ),
        (
            "wrong_arguments",
            include_str!("fixtures/inline-semantic_graph/d32a30130743.orna"),
            include_str!("fixtures/inline-semantic_graph/6a35e14725fb.orna"),
        ),
        (
            "duplicate",
            include_str!("fixtures/inline-semantic_graph/6b4a9b8170c1.orna"),
            include_str!("fixtures/inline-semantic_graph/3c3cd31bcb5b.orna"),
        ),
        (
            "unknown_label",
            include_str!("fixtures/inline-semantic_graph/7c0e96f8aa53.orna"),
            include_str!("fixtures/inline-semantic_graph/6a35e14725fb.orna"),
        ),
        (
            "positional_witness",
            include_str!("fixtures/inline-semantic_graph/76f434b1a121.orna"),
            include_str!("fixtures/inline-semantic_graph/bfc6f8ead90d.orna"),
        ),
        (
            "positional_after_named",
            include_str!("fixtures/inline-semantic_graph/bdf4713fbd9e.orna"),
            include_str!("fixtures/inline-semantic_graph/6a35e14725fb.orna"),
        ),
    ] {
        let rejected = analyze(&[ModuleInput::new("invoke-generic-invalid.orna", source)]);
        assert!(
            has(&rejected, DIAG_TYPE),
            "{name}: {:#?}",
            rejected.diagnostics
        );
        assert!(
            rejected
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message() == message),
            "{name}: expected {message:?}, got {:#?}",
            rejected.diagnostics
        );
        let symbol = &rejected.modules.values().next().unwrap().symbols[name];
        assert!(matches!(
            &symbol.ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Error
        ));
    }
}

#[test]
fn sys_start_typed_admission_requires_a_matching_result_witness_and_exact_shape() {
    let valid = analyze(&[ModuleInput::new(
        "start-generic.orna",
        include_str!("fixtures/inline-semantic_graph/4a6d95bfd780.orna"),
    )]);
    assert!(valid.is_ok(), "{:#?}", valid.diagnostics);
    let module = valid.modules.values().next().unwrap();
    for name in ["positional", "named"] {
        let symbol = &module.exports[name];
        assert!(matches!(
            &symbol.ty,
            Type::Function { result, .. }
                if result.as_ref() == &Type::Applied {
                    base: "sys.InvocationHandle".into(),
                    arguments: vec![Type::Int],
                }
        ));
        assert_eq!(
            symbol.effects.effects,
            std::collections::BTreeSet::from(["invoke".into()])
        );
        assert!(symbol.effects.may_fail);
    }
    let local_generic = &module.exports["local_generic"];
    assert!(matches!(
        &local_generic.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.InvocationHandle".into(),
                arguments: vec![Type::Named("T".into())],
            }
    ));
    assert_eq!(
        local_generic.effects.effects,
        std::collections::BTreeSet::from(["invoke".into()])
    );
    assert!(local_generic.effects.may_fail);

    for (name, source, message) in [
        (
            "missing",
            include_str!("fixtures/inline-semantic_graph/e99b32149776.orna"),
            include_str!("fixtures/inline-semantic_graph/d06e822b49bc.orna"),
        ),
        (
            "malformed",
            include_str!("fixtures/inline-semantic_graph/7c9b3341e7b2.orna"),
            "sys.start as: witness must name a known static type",
        ),
        (
            "mismatched",
            include_str!("fixtures/inline-semantic_graph/a38ee8c93ed0.orna"),
            include_str!("fixtures/inline-semantic_graph/2479d5f18ad6.orna"),
        ),
        (
            "extra_generic",
            include_str!("fixtures/inline-semantic_graph/44cd4c172bb3.orna"),
            include_str!("fixtures/inline-semantic_graph/42115eb1cd4c.orna"),
        ),
        (
            "unknown_generic",
            include_str!("fixtures/inline-semantic_graph/2460b60192c9.orna"),
            include_str!("fixtures/inline-semantic_graph/a83f09501fe0.orna"),
        ),
        (
            "wrong_target",
            include_str!("fixtures/inline-semantic_graph/ce432c541d31.orna"),
            include_str!("fixtures/inline-semantic_graph/4b037957cbf6.orna"),
        ),
        (
            "wrong_arguments",
            include_str!("fixtures/inline-semantic_graph/b1a39516923e.orna"),
            include_str!("fixtures/inline-semantic_graph/4b037957cbf6.orna"),
        ),
        (
            "duplicate",
            include_str!("fixtures/inline-semantic_graph/28db0b9bd6de.orna"),
            include_str!("fixtures/inline-semantic_graph/f05087c7aa49.orna"),
        ),
        (
            "unknown_label",
            include_str!("fixtures/inline-semantic_graph/e031f9dff073.orna"),
            include_str!("fixtures/inline-semantic_graph/4b037957cbf6.orna"),
        ),
        (
            "positional_witness",
            include_str!("fixtures/inline-semantic_graph/3f6ec0078410.orna"),
            include_str!("fixtures/inline-semantic_graph/d06e822b49bc.orna"),
        ),
        (
            "positional_after_named",
            include_str!("fixtures/inline-semantic_graph/43b21a3b6599.orna"),
            include_str!("fixtures/inline-semantic_graph/4b037957cbf6.orna"),
        ),
    ] {
        let rejected = analyze(&[ModuleInput::new("start-generic-invalid.orna", source)]);
        assert!(
            has(&rejected, DIAG_TYPE),
            "{source}: {:#?}",
            rejected.diagnostics
        );
        assert!(
            rejected
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message() == message),
            "{name}: expected {message:?}, got {:#?}",
            rejected.diagnostics
        );
        let symbol = &rejected.modules.values().next().unwrap().symbols[name];
        assert!(matches!(
            &symbol.ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Error
        ));
    }
}

#[test]
fn sys_start_erased_admission_returns_a_value_handle_without_a_witness() {
    let valid = analyze(&[ModuleInput::new(
        "start-erased.orna",
        include_str!("fixtures/inline-semantic_graph/57c6974dffb2.orna"),
    )]);
    assert!(valid.is_ok(), "{:#?}", valid.diagnostics);
    let module = valid.modules.values().next().unwrap();
    for name in ["positional", "named"] {
        let symbol = &module.exports[name];
        assert!(matches!(
            &symbol.ty,
            Type::Function { result, .. }
                if result.as_ref() == &Type::Applied {
                    base: "sys.InvocationHandle".into(),
                    arguments: vec![Type::Named("sys.Value".into())],
                }
        ));
        assert_eq!(
            symbol.effects.effects,
            std::collections::BTreeSet::from(["invoke".into()])
        );
        assert!(symbol.effects.may_fail);
    }

    let invalid = analyze(&[ModuleInput::new(
        "start-erased-invalid.orna",
        include_str!("fixtures/inline-semantic_graph/2eea1d98d28a.orna"),
    )]);
    assert!(has(&invalid, DIAG_TYPE), "{:#?}", invalid.diagnostics);
    let symbol = &invalid.modules.values().next().unwrap().symbols["positional_after_named"];
    assert!(matches!(
        &symbol.ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Error
    ));
}

#[test]
fn sys_start_generic_pipeline_preserves_handle_type_and_rejects_bad_shape() {
    let valid = analyze(&[ModuleInput::new(
        "start-generic-pipeline.orna",
        include_str!("fixtures/inline-semantic_graph/cf3e62b9414b.orna"),
    )]);
    assert!(valid.is_ok(), "{:#?}", valid.diagnostics);
    let piped = &valid.modules.values().next().unwrap().exports["piped"];
    assert!(matches!(
        &piped.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.InvocationHandle".into(),
                arguments: vec![Type::Int],
            }
    ));
    assert_eq!(
        piped.effects.effects,
        std::collections::BTreeSet::from(["invoke".into()])
    );
    assert!(piped.effects.may_fail);

    for (name, source, message) in [
        (
            "malformed",
            include_str!("fixtures/inline-semantic_graph/1a1a7ae41918.orna"),
            "sys.start as: witness must name a known static type",
        ),
        (
            "mismatched",
            include_str!("fixtures/inline-semantic_graph/785be4b8742f.orna"),
            include_str!("fixtures/inline-semantic_graph/2479d5f18ad6.orna"),
        ),
        (
            "positional_after_named",
            include_str!("fixtures/inline-semantic_graph/6bdb582bcf93.orna"),
            include_str!("fixtures/inline-semantic_graph/4b037957cbf6.orna"),
        ),
    ] {
        let rejected = analyze(&[ModuleInput::new(
            "start-generic-pipeline-invalid.orna",
            source,
        )]);
        assert!(
            has(&rejected, DIAG_TYPE),
            "{source}: {:#?}",
            rejected.diagnostics
        );
        assert!(
            rejected
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message() == message),
            "{name}: expected {message:?}, got {:#?}",
            rejected.diagnostics
        );
        let symbol = &rejected.modules.values().next().unwrap().symbols[name];
        assert!(matches!(
            &symbol.ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Error
        ));
    }
}

#[test]
fn replay_status_default_does_not_waive_the_version_precondition() {
    for expression in [
        include_str!("fixtures/inline-semantic_graph/ac556f9fcdb2.orna"),
        include_str!("fixtures/inline-semantic_graph/255e48187e5d.orna"),
    ] {
        let result = analyze(&[ModuleInput::new(
            "replay-default.orna",
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/8522f29bf6a0.orna"),
                &[("expression", &expression)],
            ),
        )]);
        assert!(result.is_ok(), "{expression}: {:?}", result.diagnostics);
        let symbol = &result.modules.values().next().unwrap().exports["replay"];
        assert!(matches!(&symbol.ty, Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "sys.InvocationHandle".into(),
                arguments: vec![Type::Named("sys.Value".into())],
            }
        ));
    }
    let invalid = analyze(&[ModuleInput::new(
        "replay-default.orna",
        include_str!("fixtures/inline-semantic_graph/b14783de0409.orna"),
    )]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);
}

#[test]
fn callable_list_defaults_require_every_member_to_supply_a_default() {
    for (second, expected_defaults) in [
        (
            include_str!("fixtures/inline-semantic_graph/372391944cb0.orna"),
            std::collections::BTreeSet::new(),
        ),
        (
            include_str!("fixtures/inline-semantic_graph/b8e818390e20.orna"),
            std::collections::BTreeSet::from([0]),
        ),
    ] {
        let result = analyze(&[ModuleInput::new(
            "callable-defaults.orna",
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/28b3f043e08d.orna"),
                &[("second", &second)],
            ),
        )]);
        assert!(result.is_ok(), "{:?}", result.diagnostics);
        let Type::Function {
            result: returned, ..
        } = &result.modules.values().next().unwrap().exports["callbacks"].ty
        else {
            panic!("function expected")
        };
        let Type::List(element) = returned.as_ref() else {
            panic!("list expected")
        };
        let Type::Function {
            default_parameters, ..
        } = element.as_ref()
        else {
            panic!("callable element expected")
        };
        assert_eq!(default_parameters, &expected_defaults);
    }
}

#[test]
fn default_expression_effects_remain_visible_on_the_callable() {
    for annotation in [
        "",
        include_str!("fixtures/inline-semantic_graph/f9a2e5617e71.orna"),
    ] {
        let result = analyze(&[ModuleInput::new(
            "effectful-default.orna",
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/fe8f91f175b2.orna"),
                &[("annotation", &annotation)],
            ),
        )]);
        assert!(result.is_ok(), "{:?}", result.diagnostics);
        let symbol = &result.modules.values().next().unwrap().exports["pinned"];
        assert!(symbol.effects.effects.contains("database read"));
        assert!(symbol.effects.may_fail);
    }
}

#[test]
fn direct_and_piped_calls_share_argument_validation() {
    for (arguments, valid) in [
        (
            include_str!("fixtures/inline-semantic_graph/9e6b37f1c1b4.orna"),
            true,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/dddc1ad01257.orna"),
            true,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/2afb1cc7bbac.orna"),
            true,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/fd3ce0985d13.orna"),
            true,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/30232b402f61.orna"),
            false,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/b3f9678bc727.orna"),
            false,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/18d2a02fe748.orna"),
            false,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/f0e86db793a2.orna"),
            false,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/5525d0931bf7.orna"),
            false,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/097b535a5a20.orna"),
            false,
        ),
        (
            include_str!("fixtures/inline-semantic_graph/1d4b3b2cbe9a.orna"),
            false,
        ),
    ] {
        for expression in [
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/6c9762998de2.orna"),
                &[("arguments", &arguments)],
            ),
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/3a5ee8f3394a.orna"),
                &[("arguments", &arguments)],
            ),
        ] {
            let result = analyze(&[ModuleInput::new(
                "arguments.orna",
                render_source_fixture(
                    include_str!("fixtures/inline-semantic_graph/b562c026bfb5.orna"),
                    &[("expression", &expression)],
                ),
            )]);
            assert_eq!(
                result.is_ok(),
                valid,
                "{expression}: {:?}",
                result.diagnostics
            );
            if !valid {
                assert!(
                    has(&result, DIAG_TYPE),
                    "{expression}: {:?}",
                    result.diagnostics
                );
            }
        }
    }
}

#[test]
fn logical_operators_require_boolean_operands_without_losing_effects() {
    for expression in [
        "true && false",
        "!false",
        include_str!("fixtures/inline-semantic_graph/a46c1b44c0d3.orna"),
    ] {
        let result = analyze(&[ModuleInput::new(
            "logical-operators.orna",
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/857baac217a5.orna"),
                &[("expression", &expression)],
            ),
        )]);
        assert!(result.is_ok(), "{expression}: {:?}", result.diagnostics);
        if expression.contains("snapshot") {
            let symbol = &result.modules.values().next().expect("module").symbols["logical"];
            assert!(symbol.effects.effects.contains("database read"));
            assert!(symbol.effects.may_fail);
        }
    }

    for expression in ["1 && true", "!1"] {
        let result = analyze(&[ModuleInput::new(
            "logical-operators.orna",
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/857baac217a5.orna"),
                &[("expression", &expression)],
            ),
        )]);
        assert!(
            has(&result, DIAG_TYPE),
            "{expression}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn generic_and_table_pipeline_stages_remain_fail_closed() {
    let result = analyze(&[ModuleInput::new(
        "unsupported.orna",
        include_str!("fixtures/inline-semantic_graph/4956b70d7435.orna"),
    )]);

    assert!(has(&result, DIAG_UNSUPPORTED));
}

#[test]
fn stream_from_list_requires_the_closed_named_identity_argument() {
    let result = analyze(&[ModuleInput::new(
        "invalid.orna",
        include_str!("fixtures/inline-semantic_graph/398c459ce5c0.orna"),
    )]);

    assert!(has(&result, DIAG_TYPE));
}

#[test]
fn authoritative_ranges_fixture_accepts_numeric_membership_and_integer_list_slices() {
    let result = analyze(&[ModuleInput::new(
        "ranges.orna",
        include_str!("fixtures/inline-semantic_graph/046edb1d0919.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().unwrap();
    assert!(matches!(
        &module.symbols["inside"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Bool
    ));
    assert!(matches!(
        &module.symbols["first_ten"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::List(Box::new(Type::Int))
    ));
    assert!(matches!(
        &module.symbols["after_ten"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::List(Box::new(Type::Int))
    ));

    let invalid = analyze(&[
        ModuleInput::new(
            "text-range.orna",
            include_str!("fixtures/inline-semantic_graph/2e56c757bf25.orna"),
        ),
        ModuleInput::new(
            "table-take.orna",
            include_str!("fixtures/inline-semantic_graph/4dcd6c2c94b3.orna"),
        ),
        ModuleInput::new(
            "range-take.orna",
            include_str!("fixtures/inline-semantic_graph/2cb7cb735e6e.orna"),
        ),
        ModuleInput::new(
            "negative-take.orna",
            include_str!("fixtures/inline-semantic_graph/2eb13241faf0.orna"),
        ),
        ModuleInput::new(
            "range-drop.orna",
            include_str!("fixtures/inline-semantic_graph/0d4905b468e6.orna"),
        ),
        ModuleInput::new(
            "negative-drop.orna",
            include_str!("fixtures/inline-semantic_graph/ba795c386ab9.orna"),
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE));
    assert!(has(&invalid, DIAG_UNSUPPORTED));
}

#[test]
fn authoritative_ranges_fixture_accepts_transparent_integer_range_slices() {
    let result = analyze(&[ModuleInput::new(
        "ranges.orna",
        include_str!("fixtures/inline-semantic_graph/2bd46722e949.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().unwrap();
    assert!(matches!(
        &module.symbols["first_ten"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::List(Box::new(Type::Int))
    ));

    let invalid = analyze(&[ModuleInput::new(
        "invalid-range-slice.orna",
        include_str!("fixtures/inline-semantic_graph/a1d54ab7aa04.orna"),
    )]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);
}

#[test]
fn finite_list_distinct_and_union_preserve_types_and_reject_invalid_inputs() {
    let valid = analyze(&[ModuleInput::new(
        "collections.orna",
        include_str!("fixtures/inline-semantic_graph/a4e284b50fb1.orna"),
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);
    let module = valid.modules.values().next().unwrap();
    assert!(matches!(
        &module.symbols["dedupe"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::List(Box::new(Type::Int))
    ));
    assert!(matches!(
        &module.symbols["combine"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::List(Box::new(Type::Int))
    ));
    assert!(matches!(
        &module.symbols["combine_named"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::List(Box::new(Type::Int))
    ));

    let invalid = analyze(&[
        ModuleInput::new(
            "float.orna",
            include_str!("fixtures/inline-semantic_graph/f609efc9c706.orna"),
        ),
        ModuleInput::new(
            "mismatched.orna",
            include_str!("fixtures/inline-semantic_graph/f4d2f53ab3ed.orna"),
        ),
        ModuleInput::new(
            "scalar.orna",
            include_str!("fixtures/inline-semantic_graph/f14c6f9540df.orna"),
        ),
        ModuleInput::new(
            "relation.orna",
            include_str!("fixtures/inline-semantic_graph/12425433bd49.orna"),
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);
    assert!(
        invalid
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message().contains("default Float equality")),
        "{:?}",
        invalid.diagnostics
    );
}

#[test]
fn relation_distinct_preserves_relation_type_and_rejects_non_relation_fallbacks() {
    let valid = analyze(&[ModuleInput::new(
        "relation-distinct.orna",
        include_str!("fixtures/inline-semantic_graph/d541b52cb1a8.orna"),
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);
    let module = valid
        .modules
        .values()
        .next()
        .expect("relation distinct module");
    let readings = &module.symbols["readings"];
    assert!(matches!(
        &readings.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Relation(Box::new(Type::Named("Reading".into())))
    ));
    assert!(readings.effects.effects.contains("database read"));
    assert!(readings.effects.may_fail);
    let direct = &module.symbols["direct"];
    assert!(matches!(
        &direct.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Relation(Box::new(Type::Named("Reading".into())))
    ));
    assert!(direct.effects.effects.contains("database read"));
    assert!(direct.effects.may_fail);

    let catalogue_valid = analyze_with_catalogue(
        &[ModuleInput::new(
            "catalogue-relation-distinct.orna",
            include_str!("fixtures/inline-semantic_graph/672b2330903b.orna"),
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(catalogue_valid.is_ok(), "{:?}", catalogue_valid.diagnostics);
    let module = catalogue_valid
        .modules
        .values()
        .find(|module| module.symbols.contains_key("readings"))
        .expect("catalogue relation distinct module");
    let readings = &module.symbols["readings"];
    assert!(
        matches!(
            &readings.ty,
            Type::Function { result, .. }
                if result.as_ref() == &Type::Relation(Box::new(Type::Named("energy.Reading".into())))
        ),
        "{:?}",
        readings.ty
    );
    assert!(readings.effects.effects.contains("database read"));
    assert!(readings.effects.may_fail);

    let relation_argument = analyze(&[ModuleInput::new(
        "relation-distinct-argument.orna",
        include_str!("fixtures/inline-semantic_graph/4a6beb03bcb8.orna"),
    )]);
    assert!(
        has(&relation_argument, DIAG_UNSUPPORTED),
        "{:?}",
        relation_argument.diagnostics
    );

    let scalar = analyze(&[ModuleInput::new(
        "scalar-distinct.orna",
        include_str!("fixtures/inline-semantic_graph/b3b9826e3d10.orna"),
    )]);
    assert!(has(&scalar, DIAG_UNSUPPORTED), "{:?}", scalar.diagnostics);

    let float_list = analyze(&[ModuleInput::new(
        "float-list-distinct.orna",
        include_str!("fixtures/inline-semantic_graph/2cc0ac814808.orna"),
    )]);
    assert!(has(&float_list, DIAG_TYPE), "{:?}", float_list.diagnostics);
    assert!(!float_list.is_ok(), "{:?}", float_list.diagnostics);
}

#[test]
fn finite_list_filter_types_predicate_and_preserves_effects() {
    let valid = analyze(&[ModuleInput::new(
        "filter.orna",
        include_str!("fixtures/inline-semantic_graph/ebaa746fc8f6.orna"),
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);
    let module = valid.modules.values().next().expect("filter module");
    for name in ["positive", "positive_named"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. }
                if result.as_ref() == &Type::List(Box::new(Type::Int))
        ));
    }
    assert!(
        module.symbols["reads"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(module.symbols["reads"].effects.may_fail);
}

#[test]
fn finite_list_filter_rejects_wrong_callback_shape_without_affecting_relations() {
    let invalid = analyze(&[
        ModuleInput::new(
            "arity.orna",
            include_str!("fixtures/inline-semantic_graph/8ac44df17930.orna"),
        ),
        ModuleInput::new(
            "too-many.orna",
            include_str!("fixtures/inline-semantic_graph/88ab58ea5e4a.orna"),
        ),
        ModuleInput::new(
            "wrong-result.orna",
            include_str!("fixtures/inline-semantic_graph/da7a7f4b5dc9.orna"),
        ),
        ModuleInput::new(
            "wrong-name.orna",
            include_str!("fixtures/inline-semantic_graph/0e552f81e3ee.orna"),
        ),
        ModuleInput::new(
            "wrong-parameter.orna",
            include_str!("fixtures/inline-semantic_graph/24e714ddd56c.orna"),
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);
    assert!(!invalid.diagnostics.is_empty(), "{:?}", invalid.diagnostics);

    let relation = analyze(&[ModuleInput::new(
        "relation.orna",
        include_str!("fixtures/inline-semantic_graph/ebabcc0f91fe.orna"),
    )]);
    assert!(relation.is_ok(), "{:?}", relation.diagnostics);
    let module = relation.modules.values().next().expect("relation module");
    assert!(matches!(
        &module.symbols["recent"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Relation(Box::new(Type::Named("Reading".into())))
    ));
}

#[test]
fn finite_list_map_and_flat_map_preserve_list_types_and_callback_effects() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "map.orna",
            include_str!("fixtures/inline-semantic_graph/167a254e0171.orna"),
        )],
        &collection_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("map module");
    for name in ["direct", "pipeline", "named", "reads"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. }
                if result.as_ref() == &Type::List(Box::new(Type::Int))
        ));
    }
    assert!(matches!(
        &module.symbols["flattened"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::List(Box::new(Type::Int))
    ));
    assert!(
        module.symbols["reads"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(module.symbols["reads"].effects.may_fail);
}

#[test]
fn finite_list_map_and_flat_map_reject_invalid_callbacks_and_collections() {
    let invalid = analyze(&[
        ModuleInput::new(
            "wrong-result.orna",
            include_str!("fixtures/inline-semantic_graph/9f0b159601ca.orna"),
        ),
        ModuleInput::new(
            "wrong-arity.orna",
            include_str!("fixtures/inline-semantic_graph/ed6057c5f866.orna"),
        ),
        ModuleInput::new(
            "wrong-name.orna",
            include_str!("fixtures/inline-semantic_graph/637075f9d858.orna"),
        ),
        ModuleInput::new(
            "wrong-collection.orna",
            include_str!("fixtures/inline-semantic_graph/c5f3196baa1f.orna"),
        ),
        ModuleInput::new(
            "pipeline-shape.orna",
            include_str!("fixtures/inline-semantic_graph/5acf0d21e70d.orna"),
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);
    assert!(!invalid.diagnostics.is_empty(), "{:?}", invalid.diagnostics);

    let relation = analyze(&[ModuleInput::new(
        "relation.orna",
        include_str!("fixtures/inline-semantic_graph/030cd0bb5ee7.orna"),
    )]);
    assert!(relation.is_ok(), "{:?}", relation.diagnostics);
    let module = relation.modules.values().next().expect("relation module");
    assert!(matches!(
        &module.symbols["mapped"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Relation(Box::new(Type::Int))
    ));
}

#[test]
fn finite_list_sort_by_admits_direct_pipeline_and_named_keys() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "sort-by.orna",
            include_str!("fixtures/inline-semantic_graph/ad544da997bc.orna"),
        )],
        &collection_catalogue(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("sort_by module");
    for name in ["direct", "pipeline", "named", "qualified", "reads"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. }
                if result.as_ref() == &Type::List(Box::new(Type::Int))
        ));
    }
    assert!(
        module.symbols["reads"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(module.symbols["reads"].effects.may_fail);
}

#[test]
fn finite_list_sort_by_rejects_bad_callbacks_keys_and_boundaries() {
    let invalid = analyze_with_catalogue(
        &[
            ModuleInput::new(
                "missing.orna",
                include_str!("fixtures/inline-semantic_graph/f3cc30dbe47c.orna"),
            ),
            ModuleInput::new(
                "wrong-arity.orna",
                include_str!("fixtures/inline-semantic_graph/f3b573c70fd0.orna"),
            ),
            ModuleInput::new(
                "wrong-name.orna",
                include_str!("fixtures/inline-semantic_graph/f60c136a881e.orna"),
            ),
            ModuleInput::new(
                "wrong-parameter.orna",
                include_str!("fixtures/inline-semantic_graph/e5a285cdc8cb.orna"),
            ),
            ModuleInput::new(
                "wrong-key.orna",
                include_str!("fixtures/inline-semantic_graph/64921356cc6f.orna"),
            ),
            ModuleInput::new(
                "wrong-annotation.orna",
                include_str!("fixtures/inline-semantic_graph/d7cb57c731c9.orna"),
            ),
            ModuleInput::new(
                "relation.orna",
                include_str!("fixtures/inline-semantic_graph/6911644978f0.orna"),
            ),
        ],
        &collection_catalogue(),
    );
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);
    assert!(!invalid.is_ok(), "{:?}", invalid.diagnostics);

    let unprofiled = analyze_with_catalogue(
        &[ModuleInput::new(
            "unprofiled.orna",
            include_str!("fixtures/inline-semantic_graph/d58035e5f3f6.orna"),
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(
        has(&unprofiled, DIAG_UNRESOLVED),
        "{:?}",
        unprofiled.diagnostics
    );
}

#[test]
fn finite_list_first_returns_an_optional_element_for_all_supported_call_forms() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "first.orna",
            include_str!("fixtures/inline-semantic_graph/d3e9b7a21e1f.orna"),
        )],
        &collection_catalogue(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("first module");
    for name in [
        "direct",
        "pipeline",
        "named",
        "qualified",
        "qualified_pipeline",
        "qualified_named",
    ] {
        assert_eq!(
            module.symbols[name].ty,
            Type::Function {
                parameters: vec![Type::List(Box::new(Type::Int))],
                parameter_names: Some(vec!["rows".into()]),
                result: Box::new(Type::Optional(Box::new(Type::Int))),
                default_parameters: Default::default(),
            },
            "{name}"
        );
    }
}

#[test]
fn finite_list_first_rejects_invalid_arity_names_and_types_without_changing_relation_members() {
    let invalid = analyze(&[
        ModuleInput::new(
            "arity.orna",
            include_str!("fixtures/inline-semantic_graph/b63bd4a335bc.orna"),
        ),
        ModuleInput::new(
            "extra.orna",
            include_str!("fixtures/inline-semantic_graph/44011e7d1fba.orna"),
        ),
        ModuleInput::new(
            "unknown-name.orna",
            include_str!("fixtures/inline-semantic_graph/bf90750f6163.orna"),
        ),
        ModuleInput::new(
            "wrong-type.orna",
            include_str!("fixtures/inline-semantic_graph/1aa5439d22c8.orna"),
        ),
        ModuleInput::new(
            "pipeline-argument.orna",
            include_str!("fixtures/inline-semantic_graph/232074d43014.orna"),
        ),
        ModuleInput::new(
            "pipeline-row-name.orna",
            include_str!("fixtures/inline-semantic_graph/1f43f9b2b0eb.orna"),
        ),
        ModuleInput::new(
            "qualified-name.orna",
            include_str!("fixtures/inline-semantic_graph/4704221c1646.orna"),
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);

    let relation = analyze(&[ModuleInput::new(
        "relation-first.orna",
        include_str!("fixtures/inline-semantic_graph/720dee153345.orna"),
    )]);
    assert!(relation.is_ok(), "{:?}", relation.diagnostics);
    let module = relation.modules.values().next().expect("relation module");
    assert_eq!(
        module.symbols["first_reading"].ty,
        Type::Function {
            parameters: Vec::new(),
            parameter_names: Some(Vec::new()),
            result: Box::new(Type::Optional(Box::new(Type::Named("Reading".into())))),
            default_parameters: Default::default(),
        }
    );
    assert_eq!(
        module.symbols["first_pipeline"].ty,
        Type::Function {
            parameters: Vec::new(),
            parameter_names: Some(Vec::new()),
            result: Box::new(Type::Optional(Box::new(Type::Named("Reading".into())))),
            default_parameters: Default::default(),
        }
    );

    let invalid_relation = analyze(&[
        ModuleInput::new(
            "relation-first-arity.orna",
            include_str!("fixtures/inline-semantic_graph/e9996c4ee0d4.orna"),
        ),
        ModuleInput::new(
            "relation-first-name.orna",
            include_str!("fixtures/inline-semantic_graph/d3aae47fa6dc.orna"),
        ),
        ModuleInput::new(
            "relation-first-unknown.orna",
            include_str!("fixtures/inline-semantic_graph/176d56995eda.orna"),
        ),
    ]);
    assert!(
        has(&invalid_relation, DIAG_TYPE),
        "{:?}",
        invalid_relation.diagnostics
    );
}

#[test]
fn finite_list_one_returns_exactly_the_element_type_and_preserves_callback_effects() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "one.orna",
            include_str!("fixtures/inline-semantic_graph/35635da1fb4a.orna"),
        )],
        &collection_catalogue(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("one module");
    for name in [
        "direct",
        "direct_named",
        "direct_predicate",
        "unused_predicate",
        "qualified",
        "pipeline",
        "pipeline_predicate",
        "qualified_pipeline",
    ] {
        assert!(
            matches!(
                &module.symbols[name].ty,
                Type::Function { result, .. } if result.as_ref() == &Type::Int
            ),
            "{name}: {:?}",
            module.symbols[name].ty
        );
    }
    for name in ["reads", "named_reads"] {
        assert!(
            matches!(
                &module.symbols[name].ty,
                Type::Function { result, .. } if result.as_ref() == &Type::Int
            ),
            "{name}: {:?}",
            module.symbols[name].ty
        );
        assert!(
            module.symbols[name]
                .effects
                .effects
                .contains("database read"),
            "{name}: {:?}",
            module.symbols[name].effects
        );
        assert!(module.symbols[name].effects.may_fail);
    }
}

#[test]
fn finite_list_one_rejects_invalid_signatures_and_keeps_relation_one_behavior() {
    let invalid = analyze(&[
        ModuleInput::new(
            "arity.orna",
            include_str!("fixtures/inline-semantic_graph/52ad419d8ed0.orna"),
        ),
        ModuleInput::new(
            "extra.orna",
            include_str!("fixtures/inline-semantic_graph/fa0d33beb90b.orna"),
        ),
        ModuleInput::new(
            "unknown-row-name.orna",
            include_str!("fixtures/inline-semantic_graph/c8870ff4e121.orna"),
        ),
        ModuleInput::new(
            "unknown-predicate-name.orna",
            include_str!("fixtures/inline-semantic_graph/2e4362a8e06b.orna"),
        ),
        ModuleInput::new(
            "wrong-collection.orna",
            include_str!("fixtures/inline-semantic_graph/b3d23c35edee.orna"),
        ),
        ModuleInput::new(
            "wrong-result.orna",
            include_str!("fixtures/inline-semantic_graph/5bc07006fca7.orna"),
        ),
        ModuleInput::new(
            "wrong-parameter-count.orna",
            include_str!("fixtures/inline-semantic_graph/47a06fd4f598.orna"),
        ),
        ModuleInput::new(
            "wrong-parameter-type.orna",
            include_str!("fixtures/inline-semantic_graph/ed99cd38996d.orna"),
        ),
        ModuleInput::new(
            "non-callback.orna",
            include_str!("fixtures/inline-semantic_graph/50004bb89c76.orna"),
        ),
        ModuleInput::new(
            "pipeline-row-name.orna",
            include_str!("fixtures/inline-semantic_graph/711273c0ab98.orna"),
        ),
        ModuleInput::new(
            "pipeline-extra.orna",
            include_str!("fixtures/inline-semantic_graph/a162d17f61a6.orna"),
        ),
        ModuleInput::new(
            "qualified-name.orna",
            include_str!("fixtures/inline-semantic_graph/42b463000a95.orna"),
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);

    let relation = analyze(&[ModuleInput::new(
        "relation-one.orna",
        include_str!("fixtures/inline-semantic_graph/8dc5593a6a47.orna"),
    )]);
    assert!(relation.is_ok(), "{:?}", relation.diagnostics);
    let module = relation
        .modules
        .values()
        .next()
        .expect("relation one module");
    for name in ["member", "pipeline", "predicate"] {
        assert!(
            matches!(
                &module.symbols[name].ty,
                Type::Function { result, .. } if result.as_ref() == &Type::Named("Reading".into())
            ),
            "{name}: {:?}",
            module.symbols[name].ty
        );
    }
}

#[test]
fn finite_list_every_and_exists_return_bool_for_all_supported_call_forms_and_preserve_effects() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "every-exists.orna",
            include_str!("fixtures/inline-semantic_graph/34799fd41331.orna"),
        )],
        &collection_catalogue(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result.modules.values().next().expect("every/exists module");
    for name in [
        "direct",
        "direct_lambda",
        "named",
        "pipeline",
        "pipeline_named",
        "qualified",
        "qualified_pipeline",
        "reads",
    ] {
        assert!(
            matches!(
                &module.symbols[name].ty,
                Type::Function { result, .. } if result.as_ref() == &Type::Bool
            ),
            "{name}: {:?}",
            module.symbols[name].ty
        );
    }
    assert!(
        module.symbols["reads"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(module.symbols["reads"].effects.may_fail);
    assert!(matches!(
        &module.symbols["relation_exists"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Bool
    ));
}

#[test]
fn finite_list_every_and_exists_reject_invalid_signatures_without_changing_assertions() {
    let invalid = analyze(&[
        ModuleInput::new(
            "missing-predicate.orna",
            include_str!("fixtures/inline-semantic_graph/ffd51d1440e3.orna"),
        ),
        ModuleInput::new(
            "extra.orna",
            include_str!("fixtures/inline-semantic_graph/5c296e0000fd.orna"),
        ),
        ModuleInput::new(
            "wrong-row-name.orna",
            include_str!("fixtures/inline-semantic_graph/2d61d3f5aa4b.orna"),
        ),
        ModuleInput::new(
            "wrong-predicate-name.orna",
            include_str!("fixtures/inline-semantic_graph/080243e7126b.orna"),
        ),
        ModuleInput::new(
            "wrong-collection.orna",
            include_str!("fixtures/inline-semantic_graph/26df8196b02e.orna"),
        ),
        ModuleInput::new(
            "wrong-result.orna",
            include_str!("fixtures/inline-semantic_graph/8f052baa3c13.orna"),
        ),
        ModuleInput::new(
            "wrong-parameter-count.orna",
            include_str!("fixtures/inline-semantic_graph/f9848322e43d.orna"),
        ),
        ModuleInput::new(
            "wrong-parameter-type.orna",
            include_str!("fixtures/inline-semantic_graph/b7fcb818f349.orna"),
        ),
        ModuleInput::new(
            "pipeline-row-name.orna",
            include_str!("fixtures/inline-semantic_graph/95136702a0c9.orna"),
        ),
        ModuleInput::new(
            "pipeline-extra.orna",
            include_str!("fixtures/inline-semantic_graph/9a240bc8be28.orna"),
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);
    assert!(!invalid.diagnostics.is_empty(), "{:?}", invalid.diagnostics);

    let qualified = analyze_with_catalogue(
        &[ModuleInput::new(
            "unprofiled.orna",
            include_str!("fixtures/inline-semantic_graph/ccadb756aac0.orna"),
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(
        has(&qualified, DIAG_UNRESOLVED),
        "{:?}",
        qualified.diagnostics
    );

    let assertions = analyze(&[ModuleInput::new(
        "assertions.orna",
        include_str!("fixtures/inline-semantic_graph/56097f8cf399.orna"),
    )]);
    assert!(assertions.is_ok(), "{:?}", assertions.diagnostics);
}

#[test]
fn finite_list_sum_returns_exact_int_for_supported_forms_and_preserves_effects() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "sum.orna",
            include_str!("fixtures/inline-semantic_graph/3416adec0c46.orna"),
        )],
        &collection_catalogue(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.symbols.contains_key("direct"))
        .expect("sum module");
    for name in [
        "direct",
        "direct_literal",
        "direct_empty",
        "named",
        "pipeline",
        "pipeline_empty",
        "qualified",
        "qualified_pipeline",
        "reads",
    ] {
        assert!(
            matches!(
                &module.symbols[name].ty,
                Type::Function { result, .. } if result.as_ref() == &Type::Int
            ),
            "{name}: {:?}",
            module.symbols[name].ty
        );
    }
    assert!(
        module.symbols["reads"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(module.symbols["reads"].effects.may_fail);
}

#[test]
fn finite_list_sum_rejects_unsupported_kinds_arguments_and_unprofiled_qualified_names() {
    let invalid = analyze(&[
        ModuleInput::new(
            "decimal.orna",
            include_str!("fixtures/inline-semantic_graph/d8643613e10f.orna"),
        ),
        ModuleInput::new(
            "float.orna",
            include_str!("fixtures/inline-semantic_graph/8ce3e1a56c8b.orna"),
        ),
        ModuleInput::new(
            "money.orna",
            include_str!("fixtures/inline-semantic_graph/0964864e7523.orna"),
        ),
        ModuleInput::new(
            "affine.orna",
            include_str!("fixtures/inline-semantic_graph/c2bdc967d0b1.orna"),
        ),
        ModuleInput::new(
            "wrong-collection.orna",
            include_str!("fixtures/inline-semantic_graph/9ca7dcc01524.orna"),
        ),
        ModuleInput::new(
            "missing-rows.orna",
            include_str!("fixtures/inline-semantic_graph/53c474bb0ffa.orna"),
        ),
        ModuleInput::new(
            "unknown-name.orna",
            include_str!("fixtures/inline-semantic_graph/548d586af4ef.orna"),
        ),
        ModuleInput::new(
            "pipeline-argument.orna",
            include_str!("fixtures/inline-semantic_graph/5184f4549245.orna"),
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);
    assert!(!invalid.diagnostics.is_empty(), "{:?}", invalid.diagnostics);

    let qualified = analyze_with_catalogue(
        &[ModuleInput::new(
            "unprofiled.orna",
            include_str!("fixtures/inline-semantic_graph/8fac65264f2b.orna"),
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(
        has(&qualified, DIAG_UNRESOLVED),
        "{:?}",
        qualified.diagnostics
    );
}

#[test]
fn finite_list_integer_min_max_return_optional_int_for_all_call_forms() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "min-max.orna",
            include_str!("fixtures/inline-semantic_graph/b448d65fb856.orna"),
        )],
        &collection_catalogue(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.symbols.contains_key("direct_min"))
        .expect("min/max module");
    for name in [
        "direct_min",
        "direct_max",
        "named_min",
        "named_max",
        "bare_pipeline_min",
        "bare_pipeline_max",
        "call_pipeline_min",
        "call_pipeline_max",
        "qualified_min",
        "qualified_max",
        "empty_min",
        "empty_max",
    ] {
        assert!(
            matches!(
                &module.symbols[name].ty,
                Type::Function { result, .. }
                    if result.as_ref() == &Type::Optional(Box::new(Type::Int))
            ),
            "{name}: {:?}",
            module.symbols[name].ty
        );
    }
    assert!(
        module.symbols["reads"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(module.symbols["reads"].effects.may_fail);
}

#[test]
fn finite_list_integer_min_max_reject_unsupported_shapes_and_preserve_relation_behavior() {
    let invalid = analyze(&[
        ModuleInput::new(
            "decimal.orna",
            include_str!("fixtures/inline-semantic_graph/544555dc4039.orna"),
        ),
        ModuleInput::new(
            "float.orna",
            include_str!("fixtures/inline-semantic_graph/324759d97bd2.orna"),
        ),
        ModuleInput::new(
            "money.orna",
            include_str!("fixtures/inline-semantic_graph/0e3d00f2bc80.orna"),
        ),
        ModuleInput::new(
            "affine.orna",
            include_str!("fixtures/inline-semantic_graph/4249a94f6114.orna"),
        ),
        ModuleInput::new(
            "wrong-collection.orna",
            include_str!("fixtures/inline-semantic_graph/93ddf69e4c59.orna"),
        ),
        ModuleInput::new(
            "missing-rows.orna",
            include_str!("fixtures/inline-semantic_graph/09ab00e83ab4.orna"),
        ),
        ModuleInput::new(
            "unknown-name.orna",
            include_str!("fixtures/inline-semantic_graph/60ee2007e200.orna"),
        ),
        ModuleInput::new(
            "pipeline-argument.orna",
            include_str!("fixtures/inline-semantic_graph/838ce8cf0c6c.orna"),
        ),
        ModuleInput::new(
            "pipeline-extra.orna",
            include_str!("fixtures/inline-semantic_graph/34a586b7e0b2.orna"),
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);

    let qualified = analyze_with_catalogue(
        &[ModuleInput::new(
            "unprofiled.orna",
            include_str!("fixtures/inline-semantic_graph/a161fc681171.orna"),
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(
        has(&qualified, DIAG_UNRESOLVED),
        "{:?}",
        qualified.diagnostics
    );

    let relation = analyze(&[ModuleInput::new(
        "relation-min-max.orna",
        include_str!("fixtures/inline-semantic_graph/127592a42d9e.orna"),
    )]);
    assert!(relation.is_ok(), "{:?}", relation.diagnostics);
    let module = relation.modules.values().next().expect("relation module");
    for name in ["minimum", "maximum"] {
        assert!(
            matches!(
                &module.symbols[name].ty,
                Type::Function { result, .. }
                    if result.as_ref() == &Type::Optional(Box::new(Type::Named("Reading".into())))
            ),
            "{name}: {:?}",
            module.symbols[name].ty
        );
    }
}

#[test]
fn root_relation_aggregates_respect_shadowed_bindings() {
    for (operation, terminal) in [
        (
            include_str!("fixtures/inline-semantic_graph/60be9861750f.orna"),
            include_str!("fixtures/inline-semantic_graph/09f5ffef2830.orna"),
        ),
        (
            include_str!("fixtures/inline-semantic_graph/1f6fa6f69d18.orna"),
            include_str!("fixtures/inline-semantic_graph/949b743dfb54.orna"),
        ),
        (
            include_str!("fixtures/inline-semantic_graph/9baf3a40312f.orna"),
            include_str!("fixtures/inline-semantic_graph/d0a75dd6ed1e.orna"),
        ),
        (
            include_str!("fixtures/inline-semantic_graph/09f5ffef2830.orna"),
            include_str!("fixtures/inline-semantic_graph/09f5ffef2830.orna"),
        ),
    ] {
        let result = analyze(&[ModuleInput::new(
            format!("shadowed-{operation}.orna"),
            render_source_fixture(
                include_str!("fixtures/inline-semantic_graph/11875dc9243f.orna"),
                &[("operation", &operation), ("terminal", &terminal)],
            ),
        )]);
        assert!(
            has(&result, DIAG_TYPE),
            "{operation}: {:?}",
            result.diagnostics
        );
        assert!(
            !has(&result, DIAG_ANNOTATION),
            "{operation}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn finite_list_float_aggregates_return_exact_types_for_all_call_forms_and_effects() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "float-aggregates.orna",
            include_str!("fixtures/inline-semantic_graph/1f57c9931545.orna"),
        )],
        &float_collection_catalogue(),
    );

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.symbols.contains_key("direct_sum"))
        .expect("float aggregate module");
    for name in [
        "direct_sum",
        "named_sum",
        "pipeline_sum",
        "empty_sum",
        "qualified_sum",
    ] {
        assert!(
            matches!(
                &module.symbols[name].ty,
                Type::Function { result, .. } if result.as_ref() == &Type::Float
            ),
            "{name}: {:?}",
            module.symbols[name].ty
        );
    }
    for name in [
        "direct_min",
        "direct_max",
        "named_min",
        "named_max",
        "pipeline_min",
        "pipeline_max",
        "call_pipeline_min",
        "empty_min",
        "empty_max",
        "qualified_min",
        "qualified_max",
    ] {
        assert!(
            matches!(
                &module.symbols[name].ty,
                Type::Function { result, .. }
                    if result.as_ref() == &Type::Optional(Box::new(Type::Float))
            ),
            "{name}: {:?}",
            module.symbols[name].ty
        );
    }
    assert!(
        module.symbols["reads"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(module.symbols["reads"].effects.may_fail);
}

#[test]
fn finite_list_float_aggregates_reject_mixed_inputs_and_unprofiled_qualified_names() {
    let invalid = analyze(&[
        ModuleInput::new(
            "mixed-sum.orna",
            include_str!("fixtures/inline-semantic_graph/43b2485ad0be.orna"),
        ),
        ModuleInput::new(
            "mixed-min.orna",
            include_str!("fixtures/inline-semantic_graph/7405493b91fc.orna"),
        ),
        ModuleInput::new(
            "mixed-max.orna",
            include_str!("fixtures/inline-semantic_graph/e4a9fea69b8d.orna"),
        ),
        ModuleInput::new(
            "decimal.orna",
            include_str!("fixtures/inline-semantic_graph/d8643613e10f.orna"),
        ),
        ModuleInput::new(
            "int-min.orna",
            include_str!("fixtures/inline-semantic_graph/a29cf5eb2e3f.orna"),
        ),
        ModuleInput::new(
            "wrong-name.orna",
            include_str!("fixtures/inline-semantic_graph/8c006f368079.orna"),
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);

    let qualified = analyze_with_catalogue(
        &[ModuleInput::new(
            "unprofiled.orna",
            include_str!("fixtures/inline-semantic_graph/45acf201d157.orna"),
        )],
        &Catalogue::authoritative_core(),
    );
    assert!(
        has(&qualified, DIAG_UNRESOLVED),
        "{:?}",
        qualified.diagnostics
    );

    let incompatible_source = include_str!("fixtures/inline-semantic_graph/e0b8483cc8bc.orna");
    let incompatible_profile = StandardDependencyProfile::from_sources(
        "orna.std/v1-incompatible-float-collection",
        [("std/collection.orna".into(), incompatible_source.into())],
    )
    .expect("incompatible float collection profile");
    let incompatible_catalogue = Catalogue::authoritative_core()
        .with_standard_sources(
            &incompatible_profile,
            [("std/collection.orna".into(), incompatible_source.into())],
        )
        .expect("verified incompatible float collection catalogue");
    let incompatible = analyze_with_catalogue(
        &[ModuleInput::new(
            "incompatible.orna",
            include_str!("fixtures/inline-semantic_graph/45acf201d157.orna"),
        )],
        &incompatible_catalogue,
    );
    assert!(
        has(&incompatible, DIAG_TYPE),
        "{:?}",
        incompatible.diagnostics
    );
}

#[test]
fn qualified_integer_aggregate_admission_requires_compatible_pinned_exports() {
    let source = include_str!("fixtures/inline-semantic_graph/4440fcd92f93.orna");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/incompatible-collection",
        [("std/collection.orna".into(), source.into())],
    )
    .expect("incompatible collection profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, [("std/collection.orna".into(), source.into())])
        .expect("verified incompatible collection catalogue");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "consumer.orna",
            include_str!("fixtures/inline-semantic_graph/8be4f4900cfc.orna"),
        )],
        &catalogue,
    );

    assert!(has(&result, DIAG_TYPE), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.symbols.contains_key("minimum"))
        .expect("consumer module");
    assert!(matches!(
        &module.symbols["minimum"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Optional(Box::new(Type::Decimal))
    ));
    assert!(matches!(
        &module.symbols["maximum"].ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Optional(Box::new(Type::Float))
    ));
}

#[test]
fn relation_flat_map_preserves_relation_output_and_accepts_finite_inner_collections() {
    let valid = analyze(&[ModuleInput::new(
        "relation-flat-map.orna",
        include_str!("fixtures/inline-semantic_graph/7d0fdeec7b50.orna"),
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);
    let module = valid
        .modules
        .values()
        .next()
        .expect("relation flat_map module");
    for name in ["list_inner", "relation_inner"] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. }
                if result.as_ref() == &Type::Relation(Box::new(Type::Int))
        ));
    }
    assert!(
        module.symbols["relation_inner"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(module.symbols["relation_inner"].effects.may_fail);
}

#[test]
fn relation_flat_map_rejects_noncollections_and_keeps_existing_callback_rules() {
    let invalid = analyze(&[
        ModuleInput::new(
            "scalar-inner.orna",
            include_str!("fixtures/inline-semantic_graph/945025842cd0.orna"),
        ),
        ModuleInput::new(
            "stream-inner.orna",
            include_str!("fixtures/inline-semantic_graph/cf66ecf95db7.orna"),
        ),
        ModuleInput::new(
            "wrong-name.orna",
            include_str!("fixtures/inline-semantic_graph/a98fd90b3f73.orna"),
        ),
        ModuleInput::new(
            "wrong-callback.orna",
            include_str!("fixtures/inline-semantic_graph/49f6db21da79.orna"),
        ),
        ModuleInput::new(
            "unrelated-callback.orna",
            include_str!("fixtures/inline-semantic_graph/bdcc49a08507.orna"),
        ),
    ]);
    assert!(has(&invalid, DIAG_TYPE), "{:?}", invalid.diagnostics);
    assert!(!invalid.diagnostics.is_empty(), "{:?}", invalid.diagnostics);
}

#[test]
fn optional_numeric_ranges_infer_from_endpoints_or_expected_range_context() {
    let result = analyze(&[ModuleInput::new(
        "optional-ranges.orna",
        include_str!("fixtures/inline-semantic_graph/8749bee1f13e.orna"),
    )]);
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let invalid = analyze(&[ModuleInput::new(
        "untyped-range.orna",
        include_str!("fixtures/inline-semantic_graph/4c43c7187d95.orna"),
    )]);
    assert!(has(&invalid, DIAG_TYPE));
}

#[test]
fn typed_date_ranges_are_ordered_values_with_contextual_unbounded_endpoints() {
    let valid = analyze(&[ModuleInput::new(
        "date-ranges.orna",
        include_str!("fixtures/inline-semantic_graph/d147c8995e75.orna"),
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);

    let module = valid.modules.values().next().expect("date-range module");
    for name in [
        "half_open",
        "closed",
        "lower_unbounded",
        "upper_unbounded",
        "equal_half_open",
        "equal_closed",
        "reversed",
    ] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Range(Box::new(Type::Date))
        ));
    }

    for (source, expected) in [
        (
            include_str!("fixtures/inline-semantic_graph/f289f294e1b0.orna"),
            "range bounds must have the same ordered type",
        ),
        (
            include_str!("fixtures/inline-semantic_graph/98e718f92120.orna"),
            "range bounds must have the same ordered type",
        ),
    ] {
        let invalid = analyze(&[ModuleInput::new("mixed-date-range.orna", source)]);
        let diagnostic = invalid
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code() == DIAG_TYPE)
            .expect("mixed range endpoints must be rejected");
        assert_eq!(
            diagnostic.message(),
            expected,
            "{source}: {:?}",
            invalid.diagnostics
        );
    }
}

#[test]
fn typed_instant_ranges_are_ordered_values_with_contextual_unbounded_endpoints() {
    let valid = analyze(&[ModuleInput::new(
        "instant-ranges.orna",
        include_str!("fixtures/inline-semantic_graph/f90c2be94218.orna"),
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);

    let module = valid.modules.values().next().expect("instant-range module");
    for name in [
        "half_open",
        "closed",
        "lower_unbounded",
        "upper_unbounded",
        "equal_half_open",
        "equal_closed",
        "reversed",
    ] {
        assert!(matches!(
            &module.symbols[name].ty,
            Type::Function { result, .. } if result.as_ref() == &Type::Range(Box::new(Type::Instant))
        ));
    }

    for source in [
        include_str!("fixtures/inline-semantic_graph/0bf4d1c4eb22.orna"),
        include_str!("fixtures/inline-semantic_graph/fa401ab79408.orna"),
    ] {
        let invalid = analyze(&[ModuleInput::new("mixed-instant-range.orna", source)]);
        let diagnostic = invalid
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code() == DIAG_TYPE)
            .expect("mixed range endpoints must be rejected");
        assert_eq!(
            diagnostic.message(),
            "range bounds must have the same ordered type",
            "{source}: {:?}",
            invalid.diagnostics
        );
    }
}

#[test]
fn contextual_empty_instant_ranges_are_rejected() {
    let result = analyze(&[ModuleInput::new(
        "empty-instant-range.orna",
        include_str!("fixtures/inline-semantic_graph/6eeaeb515bc4.orna"),
    )]);

    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message()
                == "an untyped range needs at least one endpoint or a range context"
    }));
}

#[test]
fn affine_collection_aggregates_keep_mean_and_admit_extrema() {
    let valid = analyze(&[ModuleInput::new(
        "average.orna",
        include_str!("fixtures/inline-semantic_graph/3d83cc57ba3a.orna"),
    )]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);

    let extrema = analyze(&[
        ModuleInput::new(
            "maximum.orna",
            include_str!("fixtures/inline-semantic_graph/da1404c4638d.orna"),
        ),
        ModuleInput::new(
            "minimum.orna",
            include_str!("fixtures/inline-semantic_graph/a95ed1c30da6.orna"),
        ),
    ]);
    assert!(extrema.is_ok(), "{:?}", extrema.diagnostics);

    let shadowed = analyze(&[ModuleInput::new(
        "shadowed-max.orna",
        include_str!("fixtures/inline-semantic_graph/584bf3a8693a.orna"),
    )]);
    assert!(has(&shadowed, DIAG_TYPE));

    let invalid = analyze(&[ModuleInput::new(
        "sum.orna",
        include_str!("fixtures/inline-semantic_graph/35dcb5e92b6c.orna"),
    )]);
    assert!(has(&invalid, DIAG_TYPE));
    assert!(!has(&invalid, DIAG_UNSUPPORTED));
}

#[test]
fn relation_sum_preserves_numeric_exactness_and_rejects_invalid_elements() {
    let exact = analyze(&[ModuleInput::new(
        "readings.orna",
        include_str!("fixtures/inline-semantic_graph/2a9f6781afdb.orna"),
    )]);
    assert!(exact.is_ok(), "{:?}", exact.diagnostics);

    let absolute = analyze(&[ModuleInput::new(
        "temperatures.orna",
        include_str!("fixtures/inline-semantic_graph/c64bb707dc52.orna"),
    )]);
    assert!(
        absolute
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message() == "cannot sum absolute affine quantities")
    );

    let non_numeric = analyze(&[ModuleInput::new(
        "labels.orna",
        include_str!("fixtures/inline-semantic_graph/227167ccdf09.orna"),
    )]);
    assert!(
        non_numeric
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message() == "sum requires a numeric element type")
    );
}

#[test]
fn inferred_function_summaries_propagate_through_project_calls_independent_of_input_order() {
    let result = analyze(&[
        ModuleInput::new(
            "main.orna",
            include_str!("fixtures/inline-semantic_graph/e7d69b2dc2a2.orna"),
        ),
        ModuleInput::new(
            "sensors.orna",
            include_str!("fixtures/inline-semantic_graph/d9e8cb815bd8.orna"),
        ),
    ]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let sensors = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "sensors")
        .unwrap();
    assert!(matches!(
        &sensors.symbols["input"].ty,
        Type::Function { result, .. } if matches!(result.as_ref(), Type::Stream(_))
    ));
    assert!(
        sensors.symbols["ingest"]
            .effects
            .effects
            .contains("database write")
    );
    assert!(sensors.symbols["ingest"].effects.may_fail);

    let main = result
        .modules
        .values()
        .find(|module| module.namespace.display().is_empty())
        .unwrap();
    assert!(
        main.symbols["run"]
            .effects
            .effects
            .contains("database write")
    );
    assert!(main.symbols["run"].effects.may_fail);
}

#[test]
fn numeric_nested_lambdas_infer_omitted_parameters_without_dynamic_fallback() {
    let result = analyze(&[ModuleInput::new(
        "lambda.orna",
        include_str!("fixtures/inline-semantic_graph/28ead27d36dc.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert_eq!(
        result
            .modules
            .values()
            .next()
            .expect("module")
            .symbols
            .get("curried_add")
            .expect("function")
            .ty,
        Type::Function {
            parameters: vec![],
            parameter_names: Some(vec![]),
            default_parameters: Default::default(),
            result: Box::new(Type::Function {
                parameters: vec![Type::Int],
                parameter_names: Some(vec!["x".into()]),
                default_parameters: Default::default(),
                result: Box::new(Type::Function {
                    parameters: vec![Type::Int],
                    parameter_names: Some(vec!["y".into()]),
                    default_parameters: Default::default(),
                    result: Box::new(Type::Int),
                }),
            }),
        }
    );

    let underconstrained = analyze(&[ModuleInput::new(
        "lambda.orna",
        include_str!("fixtures/inline-semantic_graph/0a3f0c2a9637.orna"),
    )]);
    assert!(has(&underconstrained, "ORNA-S020-ANNOTATION"));
}

#[test]
fn reference_values_module_infers_closed_enum_optional_and_interpolation_cases() {
    let result = analyze(&[ModuleInput::new(
        "values.orna",
        include_str!("fixtures/reference-values-type-family.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let values = result.modules.values().next().unwrap();
    assert!(matches!(
        &values.symbols["describe"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Text
    ));
    assert!(matches!(
        &values.symbols["optional_name"].ty,
        Type::Function { parameters, result, .. }
            if parameters == &[Type::Optional(Box::new(Type::Text))]
                && result.as_ref() == &Type::Text
    ));
}

#[test]
fn case_inference_rejects_non_exhaustive_and_malformed_reference_patterns() {
    let non_exhaustive = analyze(&[ModuleInput::new(
        "values.orna",
        include_str!("fixtures/inline-semantic_graph/9f3f7de9d269.orna"),
    )]);
    assert!(has(&non_exhaustive, DIAG_TYPE));

    let malformed_enum = analyze(&[ModuleInput::new(
        "values.orna",
        include_str!("fixtures/inline-semantic_graph/45734a07b8f8.orna"),
    )]);
    assert!(has(&malformed_enum, DIAG_UNSUPPORTED));

    let malformed_optional = analyze(&[ModuleInput::new(
        "values.orna",
        include_str!("fixtures/inline-semantic_graph/36bc75af5558.orna"),
    )]);
    assert!(has(&malformed_optional, DIAG_TYPE));

    let non_text_interpolation = analyze(&[ModuleInput::new(
        "values.orna",
        include_str!("fixtures/inline-semantic_graph/1d47e5a8882e.orna"),
    )]);
    assert!(has(&non_text_interpolation, DIAG_TYPE));
}

#[test]
fn case_arms_preserve_call_effects_and_named_calls_use_declared_parameter_names() {
    let result = analyze(&[ModuleInput::new(
        "values.orna",
        include_str!("fixtures/inline-semantic_graph/8aa881052835.orna"),
    )]);

    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let values = result.modules.values().next().unwrap();
    assert!(
        values.symbols["describe"]
            .effects
            .effects
            .contains("database read")
    );
    assert!(values.symbols["describe"].effects.may_fail);

    let malformed = analyze(&[ModuleInput::new(
        "values.orna",
        include_str!("fixtures/inline-semantic_graph/335086c676b1.orna"),
    )]);
    assert!(has(&malformed, DIAG_TYPE));
}

#[test]
fn control_flow_infers_list_for_and_local_assignment_while_other_shapes_fail_closed() {
    let fixture = analyze(&[ModuleInput::new(
        "control-flow.orna",
        include_str!("fixtures/inline-semantic_graph/7761cedbc2bc.orna"),
    )]);
    let module = fixture.modules.values().next().unwrap();
    assert!(matches!(
        &module.symbols["describe"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Text
    ));
    assert!(fixture.is_ok(), "{:?}", fixture.diagnostics);

    let mismatched_branches = analyze(&[ModuleInput::new(
        "mismatched-if.orna",
        include_str!("fixtures/inline-semantic_graph/b27ae74efe72.orna"),
    )]);
    assert!(has(&mismatched_branches, DIAG_TYPE));

    let missing_else = analyze(&[ModuleInput::new(
        "missing-else.orna",
        include_str!("fixtures/inline-semantic_graph/9cb8dc333a32.orna"),
    )]);
    assert!(has(&missing_else, DIAG_UNSUPPORTED));

    let compound_assignment = analyze(&[ModuleInput::new(
        "compound-assignment.orna",
        include_str!("fixtures/inline-semantic_graph/55dcfbed2380.orna"),
    )]);
    assert!(
        compound_assignment.is_ok(),
        "{:?}",
        compound_assignment.diagnostics
    );

    let mismatched_compound = analyze(&[ModuleInput::new(
        "mismatched-compound-assignment.orna",
        include_str!("fixtures/inline-semantic_graph/7f30f77a4e31.orna"),
    )]);
    assert!(has(&mismatched_compound, DIAG_TYPE));

    let field_assignment = analyze(&[ModuleInput::new(
        "field-assignment.orna",
        include_str!("fixtures/inline-semantic_graph/e593b788dcd4.orna"),
    )]);
    assert!(has(&field_assignment, DIAG_UNSUPPORTED));

    let non_list_for = analyze(&[ModuleInput::new(
        "non-list-for.orna",
        include_str!("fixtures/inline-semantic_graph/8604bcd1d5e1.orna"),
    )]);
    assert!(has(&non_list_for, DIAG_UNSUPPORTED));
}

#[test]
fn for_over_canonical_integer_ranges_binds_int_and_rejects_other_iterables() {
    let valid = analyze(&[ModuleInput::new(
        "integer-range-for.orna",
        include_str!("fixtures/inline-semantic_graph/ffef8f5d45ae.orna"),
    )]);
    let module = valid.modules.values().next().unwrap();
    assert!(matches!(
        &module.symbols["sum_to"].ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
    let populate = &module.symbols["populate"];
    assert!(populate.effects.effects.contains("database write"));
    assert!(populate.effects.may_fail);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);

    let typed_break = analyze(&[ModuleInput::new(
        "integer-range-break.orna",
        include_str!("fixtures/inline-semantic_graph/00506306aa89.orna"),
    )]);
    assert!(has(&typed_break, DIAG_TYPE));
    assert!(!has(&typed_break, DIAG_UNSUPPORTED));

    let decimal_range = analyze(&[ModuleInput::new(
        "decimal-range-for.orna",
        include_str!("fixtures/inline-semantic_graph/912d8d2b3ffa.orna"),
    )]);
    assert!(has(&decimal_range, DIAG_UNSUPPORTED));

    let generic_range = analyze(&[ModuleInput::new(
        "generic-range-for.orna",
        include_str!("fixtures/inline-semantic_graph/88f95aff5ce0.orna"),
    )]);
    assert!(has(&generic_range, DIAG_UNSUPPORTED));
}

#[test]
fn while_requires_a_boolean_condition_preserves_body_effects_and_validates_transfers() {
    let valid = analyze(&[ModuleInput::new(
        "while.orna",
        include_str!("fixtures/inline-semantic_graph/808c80b1c76a.orna"),
    )]);
    let module = valid
        .modules
        .values()
        .find(|module| module.namespace.display() == "while")
        .unwrap();
    let poll = &module.symbols["poll"];
    assert!(matches!(
        &poll.ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Null
    ));
    assert!(poll.effects.effects.contains("database write"));
    assert!(poll.effects.may_fail);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);

    let non_boolean = analyze(&[ModuleInput::new(
        "while-non-boolean.orna",
        include_str!("fixtures/inline-semantic_graph/9864616820ba.orna"),
    )]);
    assert!(has(&non_boolean, DIAG_TYPE));

    let nearest_loop = analyze(&[ModuleInput::new(
        "nearest-loop.orna",
        include_str!("fixtures/inline-semantic_graph/ff658761b05b.orna"),
    )]);
    assert!(nearest_loop.is_ok(), "{:?}", nearest_loop.diagnostics);

    let outside_loop = analyze(&[ModuleInput::new(
        "outside-loop.orna",
        include_str!("fixtures/inline-semantic_graph/3a902689de0f.orna"),
    )]);
    assert!(has(&outside_loop, DIAG_UNSUPPORTED));

    let mismatched_break_value = analyze(&[ModuleInput::new(
        "mismatched-break-value.orna",
        include_str!("fixtures/inline-semantic_graph/a27e1e4bb24c.orna"),
    )]);
    assert!(has(&mismatched_break_value, DIAG_TYPE));
    assert!(!has(&mismatched_break_value, DIAG_UNSUPPORTED));

    let lambda_boundary = analyze(&[ModuleInput::new(
        "lambda-boundary.orna",
        include_str!("fixtures/inline-semantic_graph/de6174121729.orna"),
    )]);
    assert!(has(&lambda_boundary, DIAG_UNSUPPORTED));
}

#[test]
fn coalesce_types_optional_values_with_precedence_and_grouping() {
    let valid = analyze(&[
        ModuleInput::new(
            "coalesce-precedence.orna",
            include_str!("fixtures/inline-semantic_graph/3b08fdd554d2.orna"),
        ),
        ModuleInput::new(
            "grouped-coalesce.orna",
            include_str!("fixtures/inline-semantic_graph/7a2126efc26a.orna"),
        ),
    ]);
    assert!(valid.is_ok(), "{:?}", valid.diagnostics);

    let incompatible = analyze(&[ModuleInput::new(
        "incompatible-coalesce.orna",
        include_str!("fixtures/inline-semantic_graph/61979de51695.orna"),
    )]);
    assert!(has(&incompatible, DIAG_TYPE));
}
#[test]
fn uuid7_intrinsic_infers_std_uuid_type() {
    let result = analyze(&[ModuleInput::new(
        "uuid7.orna",
        include_str!("fixtures/inline-semantic_graph/3a7503af7186.orna"),
    )]);
    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "uuid7")
        .expect("uuid7 module");
    assert!(matches!(
        &module.exports["generated"].ty,
        Type::Function { parameters, result, .. }
            if parameters.is_empty() && result.as_ref() == &Type::Named("std.UUID".into())
    ));
}

#[test]
fn uuid7_intrinsic_rejects_wrong_arity_and_argument_type() {
    for source in [
        include_str!("fixtures/inline-semantic_graph/684c8c574f93.orna"),
        include_str!("fixtures/inline-semantic_graph/3042c8be3167.orna"),
        include_str!("fixtures/inline-semantic_graph/c4bc27f7eca5.orna"),
    ] {
        let result = analyze(&[ModuleInput::new("uuid7-invalid.orna", source)]);
        assert!(
            has(&result, DIAG_TYPE),
            "{source}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn user_defined_uuid7_shadows_the_root_intrinsic() {
    let result = analyze(&[ModuleInput::new(
        "uuid7-shadow.orna",
        include_str!("fixtures/inline-semantic_graph/5a6cc7aecba9.orna"),
    )]);
    assert!(result.is_ok(), "{:?}", result.diagnostics);

    let module = result
        .modules
        .values()
        .find(|module| module.namespace.display() == "uuid7-shadow")
        .expect("uuid7 shadow module");
    assert!(matches!(
        &module.exports["generated"].ty,
        Type::Function { parameters, result, .. }
            if parameters.is_empty() && result.as_ref() == &Type::Text
    ));
}
