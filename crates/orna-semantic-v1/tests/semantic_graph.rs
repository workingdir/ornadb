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
fn structural_rebind_round_trip_does_not_merge_intermediate_nested_pins() {
    let source = include_str!("fixtures/historical-pinned-closure-structural-pin-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-structural-pin-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "an intermediate continuation pin must remain distinct after the round trip: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn nested_callable_rebind_cascades_preserve_each_pin_owner() {
    let source = include_str!("fixtures/historical-pinned-closure-rebind-cascade.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-rebind-cascade.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "curried closure rebinds must preserve the root, bridge, and leaf pins independently: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn nested_callable_cascade_keeps_intermediate_bridge_pins_distinct() {
    let source = include_str!("fixtures/historical-pinned-closure-rebind-cascade-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-rebind-cascade-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "a bridge closure from an intermediate cascade stage must not merge with the returned stage: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn structural_storms_preserve_curried_cascade_pins_at_every_depth() {
    let source = include_str!("fixtures/historical-pinned-closure-cascade-structural-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-cascade-structural-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "structurally rebound cascades at every record depth must retain their pin owners: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn structural_storm_round_trip_keeps_nested_cascade_pins_distinct() {
    let source = include_str!("fixtures/historical-pinned-closure-cascade-structural-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-cascade-structural-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "a nested cascade from an intermediate structural stage must stay distinct after round trip: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn structural_storms_preserve_identity_through_chained_rebind_cascades() {
    let source = include_str!("fixtures/historical-pinned-closure-chained-cascade-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chained-cascade-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "nested closure identities must survive structural storms and successive outer/inner rebinds: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn chained_rebind_round_trips_keep_intermediate_structural_pins_distinct() {
    let source = include_str!("fixtures/historical-pinned-closure-chained-cascade-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-chained-cascade-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "an intermediate bridge pin must not merge with the returned structure after chained rebinds: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn shared_ancestor_pin_survives_sibling_cascade_rebind_storms() {
    let source = include_str!("fixtures/historical-pinned-closure-sibling-cascade-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-sibling-cascade-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "sibling cascades must share the same ancestor pin while retaining their own child pins: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn sibling_bridge_pins_do_not_merge_after_structural_round_trip() {
    let source = include_str!("fixtures/historical-pinned-closure-sibling-cascade-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-sibling-cascade-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "sibling cascades with distinct bridge pins must remain incompatible after structural round trip: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_structural_storms_preserve_chained_pin_identity() {
    let source = include_str!("fixtures/historical-pinned-closure-tuple-chain-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-tuple-chain-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "tuple-nested root, bridge, and leaf pins must survive structural storm rebinding at saved stages: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_structural_round_trip_keeps_chain_pins_from_distinct_stages() {
    let source = include_str!("fixtures/historical-pinned-closure-tuple-chain-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-closure-tuple-chain-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "tuple-nested chain closures from distinct storm stages must retain distinct pins after round trip: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_pin_slots_keep_identity_through_structural_storm_rebinds() {
    let source = include_str!("fixtures/historical-tuple-pins-structural-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-pins-structural-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "nested tuple pin slots must keep their selector identities across structural storm rebinds: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_pin_rebind_stages_remain_distinct_after_round_trip() {
    let source = include_str!("fixtures/historical-tuple-pins-structural-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-pins-structural-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "pins from the intermediate and returned tuple stages must remain distinct: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_pin_cascades_keep_identity_through_storm_rebinds() {
    let source = include_str!("fixtures/historical-tuple-pin-cascade-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-pin-cascade-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "tuple-held root, bridge, and leaf pins must survive chained storm and closure rebinds: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_pin_cascade_stages_do_not_merge_after_round_trip() {
    let source = include_str!("fixtures/historical-tuple-pin-cascade-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-pin-cascade-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "closure cascades built from intermediate and returned tuple pin stages must stay distinct: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_pin_cascade_depths_stay_isolated_through_chained_storm_rebinds() {
    let source = include_str!("fixtures/historical-tuple-pin-cascade-depth-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-pin-cascade-depth-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "nested tuple pin cascades must preserve each depth and storm stage independently: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_pin_cascade_depths_reject_cross_depth_mixing_after_storm_rebinds() {
    let source = include_str!("fixtures/historical-tuple-pin-cascade-depth-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-pin-cascade-depth-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "cascade results rooted at separate tuple depths must remain type-distinct: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_pin_cascade_paired_depths_preserve_each_side_through_storm_rebinds() {
    let source = include_str!("fixtures/historical-tuple-pin-cascade-paired-depth-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-pin-cascade-paired-depth-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired tuple cascades must preserve left and right pins at every depth and saved storm stage: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn concurrent_tuple_pin_callbacks_keep_paired_depth_identities_after_rebind() {
    let source = include_str!("fixtures/historical-concurrent-paired-tuple-pin-rebind.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-concurrent-paired-tuple-pin-rebind.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "same-lane callbacks must retain their tuple pin through paired-depth rebind and restore: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );

    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("concurrent_paired_tuple_pins_survive_rebind")
        })
        .expect("concurrent paired tuple-pin fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["concurrent_paired_tuple_pins_survive_rebind"]
        .ty
    else {
        panic!("paired callback proof must be a function");
    };
    let Type::Record(streams) = result.as_ref() else {
        panic!("paired callback proof must expose its concurrent checkpoints");
    };
    let pin_type = |name: &str| {
        let Type::Stream(element) = streams.get(name).expect("parallel result field") else {
            panic!("{name} must be a parallel stream");
        };
        assert!(
            matches!(element.as_ref(), Type::Applied { base, .. } if base == "sys.HistoricalCallable"),
            "{name} must carry the captured historical callable identity: {element:?}"
        );
        element.as_ref()
    };

    // The reference requires one common callback result type, but does not
    // define an identity merge for captured historical pins. Keep each lane
    // and depth exact so a rebind cannot make unrelated concurrent callbacks
    // type-compatible.
    assert_ne!(pin_type("restored_left_root"), pin_type("restored_right_root"));
    assert_ne!(pin_type("restored_left_leaf"), pin_type("restored_right_leaf"));
    assert_ne!(pin_type("restored_left_root"), pin_type("restored_left_leaf"));
    assert_ne!(pin_type("rebound_left_root"), pin_type("restored_left_root"));
    assert_ne!(pin_type("rebound_right_leaf"), pin_type("restored_right_leaf"));
}

#[test]
fn concurrent_tuple_pin_callbacks_reject_cross_lane_rebind_mixing() {
    let source = include_str!("fixtures/historical-concurrent-paired-tuple-pin-rebind-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-concurrent-paired-tuple-pin-rebind-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "parallel callbacks restored from opposite tuple lanes must not collapse to one pin identity: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn concurrent_callback_tuples_preserve_pin_identity_across_rebind() {
    let source = include_str!("fixtures/historical-concurrent-tuple-callback-rebind.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-concurrent-tuple-callback-rebind.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "tuple-stored callbacks must keep their captured pins through rebinding and concurrent use: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );

    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_callback_pins_survive_tuple_rebind")
        })
        .expect("tuple-stored concurrent callback fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_callback_pins_survive_tuple_rebind"]
        .ty
    else {
        panic!("tuple callback proof must be a function");
    };
    let Type::Record(streams) = result.as_ref() else {
        panic!("tuple callback proof must expose concurrent checkpoint streams");
    };
    let pin_type = |name: &str| {
        let Type::Stream(element) = streams.get(name).expect("parallel result field") else {
            panic!("{name} must be a parallel stream");
        };
        assert!(
            matches!(element.as_ref(), Type::Applied { base, .. } if base == "sys.HistoricalCallable"),
            "{name} must preserve its captured historical callable: {element:?}"
        );
        element.as_ref()
    };

    assert_ne!(pin_type("saved_left_root"), pin_type("saved_right_root"));
    assert_ne!(pin_type("saved_left_leaf"), pin_type("saved_right_leaf"));
    assert_ne!(pin_type("saved_left_root"), pin_type("saved_left_leaf"));
    assert_ne!(pin_type("saved_left_root"), pin_type("rebound_left_root"));
    assert_ne!(pin_type("saved_right_leaf"), pin_type("rebound_right_leaf"));
}

#[test]
fn sequential_paired_callback_leaf_rebinds_preserve_sibling_pins() {
    let source = include_str!("fixtures/historical-sequential-paired-callback-leaf-rebinds.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-sequential-paired-callback-leaf-rebinds.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "sequential leaf-lane rebinds must preserve sibling and saved callback pins: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );

    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_callback_leaf_rebinds_preserve_siblings")
        })
        .expect("sequential paired callback rebind fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_callback_leaf_rebinds_preserve_siblings"]
        .ty
    else {
        panic!("paired callback rebind proof must be a function");
    };
    let Type::Record(streams) = result.as_ref() else {
        panic!("paired callback rebind proof must expose concurrent checkpoints");
    };
    let pin_type = |name: &str| {
        let Type::Stream(element) = streams.get(name).expect("parallel result field") else {
            panic!("{name} must be a parallel stream");
        };
        assert!(
            matches!(element.as_ref(), Type::Applied { base, .. } if base == "sys.HistoricalCallable"),
            "{name} must retain its captured pin: {element:?}"
        );
        element.as_ref()
    };

    assert_eq!(pin_type("saved_left_root"), pin_type("middle_left_root"));
    assert_eq!(pin_type("middle_left_root"), pin_type("final_left_root"));
    assert_eq!(pin_type("saved_right_root"), pin_type("final_right_root"));
    assert_ne!(pin_type("saved_left_root"), pin_type("saved_right_root"));

    assert_ne!(pin_type("saved_left_leaf"), pin_type("middle_left_leaf"));
    assert_eq!(pin_type("saved_right_leaf"), pin_type("middle_right_leaf"));
    assert_eq!(pin_type("middle_left_leaf"), pin_type("final_left_leaf"));
    assert_ne!(pin_type("middle_right_leaf"), pin_type("final_right_leaf"));
    assert_ne!(pin_type("final_left_leaf"), pin_type("final_right_leaf"));
}

#[test]
fn sequential_paired_callback_leaf_rebinds_reject_cross_lane_parallel_mix() {
    let source = include_str!("fixtures/historical-sequential-paired-callback-leaf-rebinds-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-sequential-paired-callback-leaf-rebinds-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "separately rebound tuple leaves from opposite lanes must remain type-distinct: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_shadowed_callbacks_specialize_each_chained_inner_pin() {
    let source = include_str!("fixtures/historical-paired-shadowed-callback-rebind.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-shadowed-callback-rebind.orna",
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
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_shadowed_callback_rebinds_preserve_inner_pins")
        })
        .expect("paired shadowed callback fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_shadowed_callback_rebinds_preserve_inner_pins"]
        .ty
    else {
        panic!("paired shadowed callback proof must be a function");
    };
    let Type::Record(streams) = result.as_ref() else {
        panic!("paired shadowed callback proof must expose concurrent results");
    };
    let pin_type = |name: &str| {
        let Type::Stream(element) = streams.get(name).expect("parallel result field") else {
            panic!("{name} must be a stream");
        };
        element.as_ref()
    };
    assert_ne!(
        pin_type("left_first"),
        pin_type("left_second"),
        "each chained invocation must specialize the shadowed inner pin independently"
    );
    assert_ne!(pin_type("right_first"), pin_type("right_second"));
    assert_ne!(pin_type("left_first"), pin_type("right_first"));
}

#[test]
fn paired_shadowed_callback_depth_rebinds_preserve_selected_pins() {
    let source = include_str!("fixtures/historical-paired-shadowed-callback-depth.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-shadowed-callback-depth.orna",
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
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_shadowed_callback_depth_rebinds_preserve_selected_pins")
        })
        .expect("paired shadowed callback depth fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_shadowed_callback_depth_rebinds_preserve_selected_pins"]
        .ty
    else {
        panic!("paired shadowed callback depth proof must be a function");
    };
    let Type::Record(streams) = result.as_ref() else {
        panic!("paired shadowed callback depth proof must expose concurrent results");
    };
    let pin_type = |name: &str| {
        let Type::Stream(element) = streams.get(name).expect("parallel result field") else {
            panic!("{name} must be a stream");
        };
        element.as_ref()
    };
    assert_eq!(pin_type("left_first"), pin_type("left_second"));
    assert_eq!(pin_type("right_first"), pin_type("right_second"));
    assert_ne!(pin_type("left_first"), pin_type("right_first"));
    assert_ne!(pin_type("left_first"), pin_type("rebound_left"));
    assert_ne!(pin_type("right_first"), pin_type("rebound_right"));
}

#[test]
fn paired_shadowed_callback_parameter_contracts_keep_depth_pin_scope() {
    let source = include_str!("fixtures/historical-paired-shadowed-callback-contract.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);

    let nested_callback = Type::Function {
        parameters: Vec::new(),
        parameter_names: Some(Vec::new()),
        default_parameters: BTreeSet::new(),
        result: Box::new(Type::Applied {
            base: "sys.HistoricalCallable".into(),
            arguments: vec![
                Type::Applied {
                    base: "sys.SnapshotRefContext".into(),
                    arguments: vec![Type::Named("selector:parameter:pin".into())],
                },
                Type::Function {
                    parameters: Vec::new(),
                    parameter_names: Some(Vec::new()),
                    default_parameters: BTreeSet::new(),
                    result: Box::new(Type::Int),
                },
            ],
        }),
    };
    let parameters = vec![Type::Named("sys.SnapshotRef".into()), nested_callback];
    let maker = Symbol {
        kind: SymbolKind::Function,
        ty: Type::Function {
            parameters: vec![Type::Named("sys.SnapshotRef".into())],
            parameter_names: Some(vec!["pin".into()]),
            default_parameters: BTreeSet::new(),
            result: Box::new(Type::Function {
                parameters,
                parameter_names: Some(vec!["pin".into(), "callback".into()]),
                default_parameters: BTreeSet::new(),
                result: Box::new(Type::Text),
            }),
        },
        public: true,
        effects: EffectSummary::default(),
        generic_parameters: Vec::new(),
        enum_variants: BTreeSet::new(),
        table_schema: None,
    };
    let symbols = BTreeMap::from([("maker".to_owned(), maker)]);
    let catalogue = Catalogue::authoritative_fixture().with_historical_modules([ModuleHeader {
        namespace: Namespace(vec!["energy".into()]),
        exports: symbols.clone(),
        symbols,
        generic_functions: BTreeMap::new(),
        prelude_exports: BTreeSet::new(),
        implicit: true,
    }]);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-shadowed-callback-contract.orna",
            source,
        )],
        &catalogue,
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

    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_shadowed_callback_contracts_keep_depth_pin_scope")
        })
        .expect("paired shadowed callback contract fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_shadowed_callback_contracts_keep_depth_pin_scope"]
        .ty
    else {
        panic!("paired callback contract proof must be a function");
    };
    let Type::Tuple(lanes) = result.as_ref() else {
        panic!("paired callback contract proof must expose its two lanes");
    };
    let [left, right] = lanes.as_slice() else {
        panic!("paired callback contract proof must expose exactly two lanes");
    };
    fn callback_pin(maker: &Type) -> &Type {
        let Type::Applied { base, arguments } = maker else {
            panic!("historical maker must retain its context wrapper: {maker:?}");
        };
        assert_eq!(base, "sys.HistoricalCallable");
        let [_, Type::Function { parameters, .. }] = arguments.as_slice() else {
            panic!("historical maker must expose its returned callback: {maker:?}");
        };
        let Type::Function {
            result: callback_result,
            ..
        } = &parameters[1]
        else {
            panic!("nested callback contract must be a function: {:?}", parameters[1]);
        };
        let Type::Applied { base, arguments } = callback_result.as_ref() else {
            panic!("callback contract result must retain its historical pin");
        };
        assert_eq!(base, "sys.HistoricalCallable");
        let [pin, _] = arguments.as_slice() else {
            panic!("historical result must carry one pin");
        };
        pin
    }
    let left_pin = callback_pin(left);
    let right_pin = callback_pin(right);
    let expected_shadowed_pin = Type::Applied {
        base: "sys.SnapshotRefContext".into(),
        arguments: vec![Type::Named("selector:parameter:pin".into())],
    };
    assert_eq!(left_pin, &expected_shadowed_pin);
    assert_eq!(right_pin, &expected_shadowed_pin);
}

#[test]
fn paired_shadowed_callback_depth_rebinds_retain_untouched_lanes() {
    let source = include_str!("fixtures/historical-paired-shadowed-callback-retention.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-shadowed-callback-retention.orna",
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
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_shadowed_callback_depth_rebinds_retain_untouched_lanes")
        })
        .expect("paired shadowed callback retention fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_shadowed_callback_depth_rebinds_retain_untouched_lanes"]
        .ty
    else {
        panic!("paired shadowed callback retention proof must be a function");
    };
    let Type::Record(streams) = result.as_ref() else {
        panic!("paired shadowed callback retention proof must expose concurrent pins");
    };
    let pin_type = |name: &str| {
        let Type::Stream(element) = streams.get(name).expect("parallel result field") else {
            panic!("{name} must be a stream");
        };
        let Type::Applied { base, arguments } = element.as_ref() else {
            panic!("{name} must retain a historical callable pin: {element:?}");
        };
        assert_eq!(base, "sys.HistoricalCallable");
        let [pin, _] = arguments.as_slice() else {
            panic!("historical callable must expose its pin: {arguments:?}");
        };
        pin
    };
    assert_eq!(pin_type("saved_left"), pin_type("middle_left"));
    assert_eq!(pin_type("saved_right"), pin_type("middle_right"));
    assert_eq!(pin_type("middle_left"), pin_type("final_left"));
    assert_eq!(pin_type("middle_right"), pin_type("final_right"));
    assert_ne!(pin_type("final_left"), pin_type("final_right"));
    assert_eq!(
        pin_type("final_left"),
        &Type::Applied {
            base: "sys.SnapshotRefContext".into(),
            arguments: vec![Type::Named("selector:HEAD~100".into())],
        }
    );
    assert_eq!(
        pin_type("final_right"),
        &Type::Applied {
            base: "sys.SnapshotRefContext".into(),
            arguments: vec![Type::Named("selector:HEAD~90".into())],
        }
    );
}

#[test]
fn paired_shadowed_callback_depths_retain_each_lane() {
    let source = include_str!("fixtures/historical-paired-shadowed-callback-depth-capture.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-shadowed-callback-depth-capture.orna",
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
    let module = result
        .modules
        .values()
        .find(|module| module.symbols.contains_key("paired_shadowed_callback_depths_retain_each_lane"))
        .expect("paired callback depth capture fixture module");
    let Type::Function { result, .. } = &module.symbols["paired_shadowed_callback_depths_retain_each_lane"].ty else {
        panic!("paired callback depth capture proof must be a function");
    };
    let Type::Record(streams) = result.as_ref() else {
        panic!("paired callback depth capture must expose its lanes");
    };
    let pin_type = |name: &str| {
        let Type::Stream(element) = streams.get(name).expect("parallel result field") else {
            panic!("{name} must be a stream");
        };
        let Type::Applied { base, arguments } = element.as_ref() else {
            panic!("{name} must preserve its historical pin: {element:?}");
        };
        assert_eq!(base, "sys.HistoricalCallable");
        let [pin, _] = arguments.as_slice() else {
            panic!("historical callable must expose its pin: {arguments:?}");
        };
        pin
    };
    let expected = |selector: &str| Type::Applied {
        base: "sys.SnapshotRefContext".into(),
        arguments: vec![Type::Named(format!("selector:{selector}"))],
    };
    assert_eq!(pin_type("saved_left"), &expected("HEAD~80"));
    assert_eq!(pin_type("saved_right"), &expected("HEAD~70"));
    assert_eq!(pin_type("middle_left"), &expected("HEAD~79"));
    assert_eq!(pin_type("middle_right"), &expected("HEAD~70"));
    assert_eq!(pin_type("final_left"), &expected("HEAD~79"));
    assert_eq!(pin_type("final_right"), &expected("HEAD~69"));
}

#[test]
fn paired_shadowed_callback_depth_waves_preserve_capture_identity() {
    let source = include_str!("fixtures/historical-paired-shadowed-callback-depth-wave.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-shadowed-callback-depth-wave.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired shadowed callback depth waves must keep every captured pin through save, rebind, and restore: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_shadowed_callback_depth_waves_preserve_capture_identity")
        })
        .expect("paired shadowed callback depth wave fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_shadowed_callback_depth_waves_preserve_capture_identity"]
        .ty
    else {
        panic!("paired callback depth wave proof must be a function");
    };
    let Type::Record(streams) = result.as_ref() else {
        panic!("paired callback depth wave proof must expose captured pins");
    };
    let pin_type = |name: &str| {
        let Type::Stream(element) = streams.get(name).expect("parallel result field") else {
            panic!("{name} must be a stream");
        };
        let Type::Applied { base, arguments } = element.as_ref() else {
            panic!("{name} must retain its historical callable pin: {element:?}");
        };
        assert_eq!(base, "sys.HistoricalCallable");
        let [pin, _] = arguments.as_slice() else {
            panic!("historical callable must expose its captured pin: {arguments:?}");
        };
        pin
    };
    let expected = |selector: &str| Type::Applied {
        base: "sys.SnapshotRefContext".into(),
        arguments: vec![Type::Named(format!("selector:{selector}"))],
    };
    for (field, selector) in [
        ("left_old_maker", "HEAD~500"),
        ("left_old_first", "HEAD~450"),
        ("left_old_second", "HEAD~400"),
        ("left_old_third", "HEAD~350"),
        ("left_restored_maker", "HEAD~500"),
        ("left_restored_first", "HEAD~450"),
        ("left_restored_second", "HEAD~400"),
        ("left_restored_third", "HEAD~350"),
        ("left_rebound_maker", "HEAD~500"),
        ("left_rebound_first", "HEAD~449"),
        ("left_rebound_second", "HEAD~399"),
        ("left_rebound_third", "HEAD~349"),
        ("right_old_maker", "HEAD~490"),
        ("right_old_first", "HEAD~440"),
        ("right_old_second", "HEAD~390"),
        ("right_old_third", "HEAD~340"),
        ("right_rebound_maker", "HEAD~490"),
        ("right_rebound_first", "HEAD~439"),
        ("right_rebound_second", "HEAD~389"),
        ("right_rebound_third", "HEAD~339"),
    ] {
        assert_eq!(pin_type(field), &expected(selector), "{field}");
    }
    assert_eq!(pin_type("left_old_maker"), pin_type("left_rebound_maker"));
    assert_ne!(pin_type("left_old_first"), pin_type("left_rebound_first"));
    assert_ne!(pin_type("left_old_second"), pin_type("left_rebound_second"));
    assert_ne!(pin_type("left_old_third"), pin_type("left_rebound_third"));
    assert_ne!(pin_type("left_rebound_maker"), pin_type("right_rebound_maker"));
    assert_ne!(pin_type("left_rebound_first"), pin_type("right_rebound_first"));
    assert_ne!(pin_type("left_rebound_second"), pin_type("right_rebound_second"));
    assert_ne!(pin_type("left_rebound_third"), pin_type("right_rebound_third"));
}

#[test]
fn paired_shadowed_callback_depth_waves_reject_cross_lane_capture_mix() {
    let source = include_str!("fixtures/historical-paired-shadowed-callback-depth-wave-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-shadowed-callback-depth-wave-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "captured pins from opposite shadowed callback waves must remain incompatible: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_tuple_shadow_waves_preserve_local_pin_scope() {
    let source = include_str!("fixtures/historical-paired-shadowed-tuple-capture-depth-wave.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-shadowed-tuple-capture-depth-wave.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "tuple-destructured shadowed local pins must remain scoped through a callback depth wave: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_tuple_shadow_waves_preserve_local_pin_scope")
        })
        .expect("paired tuple shadow wave fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_tuple_shadow_waves_preserve_local_pin_scope"]
        .ty
    else {
        panic!("paired tuple shadow wave proof must be a function");
    };
    let Type::Record(streams) = result.as_ref() else {
        panic!("paired tuple shadow wave proof must expose both lanes");
    };
    let pin_type = |name: &str| {
        let Type::Stream(element) = streams.get(name).expect("parallel result field") else {
            panic!("{name} must be a stream");
        };
        let Type::Applied { base, arguments } = element.as_ref() else {
            panic!("{name} must retain a historical callable pin: {element:?}");
        };
        assert_eq!(base, "sys.HistoricalCallable");
        let [pin, _] = arguments.as_slice() else {
            panic!("historical callable must expose its pin: {arguments:?}");
        };
        pin
    };
    let expected = |selector: &str| Type::Applied {
        base: "sys.SnapshotRefContext".into(),
        arguments: vec![Type::Named(format!("selector:{selector}"))],
    };
    for (field, selector) in [
        ("left_selected", "HEAD~450"),
        ("left_sibling", "HEAD~440"),
        ("left_terminal", "HEAD~400"),
        ("right_selected", "HEAD~430"),
        ("right_sibling", "HEAD~420"),
        ("right_terminal", "HEAD~390"),
    ] {
        assert_eq!(pin_type(field), &expected(selector), "{field}");
    }
    assert_ne!(pin_type("left_selected"), pin_type("left_sibling"));
    assert_ne!(pin_type("left_selected"), pin_type("right_selected"));
}

#[test]
fn paired_nested_tuple_shadow_waves_keep_each_capture_depth_pin() {
    let source = include_str!("fixtures/historical-paired-nested-tuple-shadow-depth-waves.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-tuple-shadow-depth-waves.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "nested tuple pins must stay scoped to their capture depth: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
    let module = result
        .modules
        .values()
        .find(|module| module.symbols.contains_key("paired_nested_tuple_shadow_waves"))
        .expect("nested tuple shadow wave fixture module");
    let Type::Function { result, .. } = &module.symbols["paired_nested_tuple_shadow_waves"].ty
    else {
        panic!("nested tuple shadow wave proof must be a function");
    };
    let Type::Record(lanes) = result.as_ref() else {
        panic!("nested tuple shadow wave proof must expose both lanes");
    };
    let expected = |selector: &str| Type::Applied {
        base: "sys.SnapshotRefContext".into(),
        arguments: vec![Type::Named(format!("selector:{selector}"))],
    };
    for (lane_name, fields) in [
        (
            "left",
            [
                ("outer_selected", "HEAD~450"),
                ("outer_left", "HEAD~440"),
                ("outer_right", "HEAD~430"),
                ("middle_selected", "HEAD~380"),
                ("middle_left", "HEAD~370"),
                ("middle_right", "HEAD~360"),
                ("terminal_selected", "HEAD~320"),
                ("terminal_left", "HEAD~310"),
                ("terminal_right", "HEAD~300"),
            ],
        ),
        (
            "right",
            [
                ("outer_selected", "HEAD~420"),
                ("outer_left", "HEAD~410"),
                ("outer_right", "HEAD~400"),
                ("middle_selected", "HEAD~350"),
                ("middle_left", "HEAD~340"),
                ("middle_right", "HEAD~330"),
                ("terminal_selected", "HEAD~290"),
                ("terminal_left", "HEAD~280"),
                ("terminal_right", "HEAD~270"),
            ],
        ),
    ] {
        let Type::Record(streams) = lanes.get(lane_name).expect("paired lane") else {
            panic!("{lane_name} must expose its pin streams");
        };
        for (field, selector) in fields {
            let Type::Stream(element) = streams.get(field).expect("parallel result field") else {
                panic!("{lane_name}.{field} must be a stream");
            };
            let Type::Applied { base, arguments } = element.as_ref() else {
                panic!("{lane_name}.{field} must retain a historical callable pin");
            };
            assert_eq!(base, "sys.HistoricalCallable");
            let [pin, _] = arguments.as_slice() else {
                panic!("historical callable must expose its pin: {arguments:?}");
            };
            assert_eq!(pin, &expected(selector), "{lane_name}.{field}");
        }
    }
}

#[test]
fn malformed_nested_tuple_does_not_partially_bind_snapshot_identities() {
    let source = include_str!("fixtures/historical-nested-tuple-pin-shape-mismatch.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-nested-tuple-pin-shape-mismatch.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "an incompatible nested tuple must be rejected"
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("malformed_nested_tuple_call_keeps_pin_binders_symbolic")
        })
        .expect("nested tuple mismatch fixture module");
    let ty = &module.symbols["malformed_nested_tuple_call_keeps_pin_binders_symbolic"].ty;
    let summary = format!("{ty:?}");
    assert!(
        !summary.contains("HEAD~450") && !summary.contains("HEAD~440"),
        "mismatched nested tuple leaves must not partially specialize pin contexts: {summary}"
    );
}

#[test]
fn nested_tuple_error_wave_keeps_paired_capture_pins_atomic() {
    let source = include_str!("fixtures/historical-paired-nested-tuple-error-wave.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-tuple-error-wave.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNRESOLVED),
        "the recovery wave must retain its unresolved-name diagnostic"
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_nested_tuple_error_wave_atomicity")
        })
        .expect("paired tuple error-wave fixture module");
    let ty = &module.symbols["paired_nested_tuple_error_wave_atomicity"].ty;
    let summary = format!("{ty:?}");
    assert!(
        !summary.contains("HEAD~380") && !summary.contains("HEAD~370"),
        "an error in a later nested tuple leaf must not specialize earlier pins in that wave: {summary}"
    );
    assert!(
        summary.contains("HEAD~450") && summary.contains("HEAD~320"),
        "the failed wave must preserve earlier and later valid capture depths: {summary}"
    );
}

#[test]
fn nested_tuple_bottom_wave_keeps_paired_capture_pins_atomic() {
    let source = include_str!("fixtures/historical-paired-nested-tuple-bottom-wave.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-tuple-bottom-wave.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "the non-returning tuple leaf is statically compatible: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_nested_tuple_bottom_wave_atomicity")
        })
        .expect("paired tuple bottom-wave fixture module");
    let ty = &module.symbols["paired_nested_tuple_bottom_wave_atomicity"].ty;
    let summary = format!("{ty:?}");
    assert!(
        !summary.contains("HEAD~380") && !summary.contains("HEAD~370"),
        "a non-returning nested tuple argument must not specialize sibling pins: {summary}"
    );
    assert!(
        summary.contains("HEAD~450")
            && summary.contains("HEAD~320")
            && summary.contains("HEAD~350")
            && summary.contains("HEAD~290"),
        "skipping the incomplete wave must preserve prior, later, and opposite-lane pins: {summary}"
    );
}

#[test]
fn nested_tuple_bottom_terminal_wave_keeps_paired_capture_pins_atomic() {
    let source = include_str!("fixtures/historical-paired-nested-tuple-bottom-wave.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-tuple-bottom-wave.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "the non-returning tuple leaf is statically compatible: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_nested_tuple_bottom_terminal_wave_atomicity")
        })
        .expect("paired terminal tuple bottom-wave fixture module");
    let ty = &module.symbols["paired_nested_tuple_bottom_terminal_wave_atomicity"].ty;
    let summary = format!("{ty:?}");
    assert!(
        !summary.contains("HEAD~320") && !summary.contains("HEAD~310"),
        "a non-returning leaf in the terminal tuple must not bind earlier siblings: {summary}"
    );
    assert!(
        summary.contains("HEAD~450")
            && summary.contains("HEAD~380")
            && summary.contains("HEAD~290"),
        "the terminal incomplete wave must preserve prior depths and the opposite lane: {summary}"
    );
}

#[test]
fn bottom_terminal_tuple_wave_preserves_each_capture_depth_pin_identity() {
    let source = include_str!("fixtures/historical-paired-nested-tuple-bottom-wave.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-tuple-bottom-wave.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_nested_tuple_bottom_terminal_depth_identities")
        })
        .expect("bottom terminal identity fixture module");
    let Type::Function { result, .. } =
        &module.symbols["paired_nested_tuple_bottom_terminal_depth_identities"].ty
    else {
        panic!("bottom terminal identity proof must be a function");
    };
    let Type::Record(lanes) = result.as_ref() else {
        panic!("bottom terminal identity proof must expose both lanes");
    };
    fn pin_type<'a>(lanes: &'a BTreeMap<String, Type>, lane: &str, field: &str) -> &'a Type {
        let Type::Record(fields) = lanes.get(lane).expect("paired lane") else {
            panic!("{lane} lane must expose pin streams");
        };
        let Type::Stream(element) = fields.get(field).expect("capture depth field") else {
            panic!("{lane}.{field} must be a stream");
        };
        let Type::Applied { base, arguments } = element.as_ref() else {
            panic!("{lane}.{field} must retain a historical callable");
        };
        assert_eq!(base, "sys.HistoricalCallable");
        let [pin, _] = arguments.as_slice() else {
            panic!("historical callable must expose its pin: {arguments:?}");
        };
        pin
    }
    let expected = |selector: &str| Type::Applied {
        base: "sys.SnapshotRefContext".into(),
        arguments: vec![Type::Named(format!("selector:{selector}"))],
    };
    for (field, selector) in [
        ("outer_selected", "HEAD~450"),
        ("outer_left", "HEAD~440"),
        ("outer_right", "HEAD~430"),
        ("middle_selected", "HEAD~380"),
        ("middle_left", "HEAD~370"),
        ("middle_right", "HEAD~360"),
    ] {
        assert_eq!(pin_type(lanes, "left", field), &expected(selector), "left.{field}");
    }
    for (field, selector) in [
        ("outer_selected", "HEAD~420"),
        ("outer_left", "HEAD~410"),
        ("outer_right", "HEAD~400"),
        ("middle_selected", "HEAD~350"),
        ("middle_left", "HEAD~340"),
        ("middle_right", "HEAD~330"),
        ("terminal_selected", "HEAD~290"),
        ("terminal_left", "HEAD~280"),
        ("terminal_right", "HEAD~270"),
    ] {
        assert_eq!(
            pin_type(lanes, "right", field),
            &expected(selector),
            "right.{field}"
        );
    }
    let summary = format!("{result:?}");
    assert!(
        !summary.contains("HEAD~320") && !summary.contains("HEAD~310"),
        "a failed bottom leaf must not bind any terminal tuple sibling: {summary}"
    );
}

#[test]
fn bottom_cascade_keeps_completed_capture_depth_identities() {
    let source = include_str!("fixtures/historical-paired-nested-tuple-bottom-wave.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-tuple-bottom-wave.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_nested_tuple_bottom_cascade_identity")
        })
        .expect("bottom cascade fixture module");
    let summary = format!("{:?}", module.symbols["paired_nested_tuple_bottom_cascade_identity"].ty);
    for selector in [
        "HEAD~450",
        "HEAD~440",
        "HEAD~430",
        "HEAD~420",
        "HEAD~410",
        "HEAD~400",
        "HEAD~350",
        "HEAD~340",
        "HEAD~330",
    ] {
        assert!(
            summary.contains(&format!("selector:{selector}")),
            "completed capture depth lost {selector}: {summary}"
        );
    }
    for selector in [
        "HEAD~380",
        "HEAD~370",
        "HEAD~360",
        "HEAD~320",
        "HEAD~310",
        "HEAD~290",
        "HEAD~280",
        "HEAD~270",
    ] {
        assert!(
            !summary.contains(selector),
            "incomplete tuple wave leaked {selector}: {summary}"
        );
    }
}

#[test]
fn nested_bottom_cascade_preserves_tuple_pin_identity_by_capture_depth_wave() {
    let source = include_str!("fixtures/historical-nested-tuple-bottom-cascade-depth-wave.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-nested-tuple-bottom-cascade-depth-wave.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_bottom_cascade_tuple_depth_identities")
        })
        .expect("bottom cascade tuple identity fixture module");
    let Type::Function { result, .. } =
        &module.symbols["paired_bottom_cascade_tuple_depth_identities"].ty
    else {
        panic!("bottom cascade identity proof must be a function");
    };
    let Type::Record(lanes) = result.as_ref() else {
        panic!("bottom cascade identity proof must expose both lanes");
    };
    fn pin_selector<'a>(lanes: &'a BTreeMap<String, Type>, lane: &str, field: &str) -> &'a str {
        let Type::Record(fields) = lanes.get(lane).expect("paired lane") else {
            panic!("{lane} lane must expose capture depths");
        };
        let Type::Stream(element) = fields.get(field).expect("capture depth field") else {
            panic!("{lane}.{field} must be a stream");
        };
        let Type::Applied { base, arguments } = element.as_ref() else {
            panic!("{lane}.{field} must retain a historical callable");
        };
        assert_eq!(base, "sys.HistoricalCallable");
        let [pin, _] = arguments.as_slice() else {
            panic!("{lane}.{field} must retain its contextual snapshot pin: {arguments:?}");
        };
        let Type::Applied {
            base,
            arguments: context_arguments,
        } = pin
        else {
            panic!("{lane}.{field} must retain a contextual snapshot pin: {pin:?}");
        };
        assert_eq!(base, "sys.SnapshotRefContext");
        let [Type::Named(selector)] = context_arguments.as_slice() else {
            panic!("{lane}.{field} snapshot context must have one identity: {context_arguments:?}");
        };
        selector
            .strip_prefix("selector:")
            .expect("snapshot context selector")
    }
    fn assert_pinned_wave(
        lanes: &BTreeMap<String, Type>,
        lane: &str,
        depth: &str,
        selectors: [(&str, &str); 4],
    ) {
        for (leaf, expected) in selectors {
            let field = format!("{depth}_{leaf}");
            assert_eq!(
                pin_selector(lanes, lane, &field),
                expected,
                "{lane}.{field}"
            );
        }
    }
    fn assert_unbound_wave(
        lanes: &BTreeMap<String, Type>,
        lane: &str,
        depth: &str,
        attempted_selectors: &[&str],
    ) -> BTreeSet<String> {
        let mut binders = BTreeSet::new();
        for leaf in ["selected", "left", "middle", "right"] {
            let field = format!("{depth}_{leaf}");
            let selector = pin_selector(lanes, lane, &field);
            assert!(
                selector.starts_with(
                    "dynamic-call:historical-nested-tuple-bottom-cascade-depth-wave.orna:"
                ),
                "{lane}.{field} must retain its own unresolved binder after an incomplete tuple wave: {selector}"
            );
            assert!(
                selector.ends_with(&format!(":parameter:{leaf}")),
                "{lane}.{field} rebound to a sibling tuple binder: {selector}"
            );
            for attempted in attempted_selectors {
                assert!(
                    !selector.contains(attempted),
                    "{lane}.{field} leaked a pin from a non-returning wave: {selector}"
                );
            }
            assert!(
                binders.insert(selector.to_owned()),
                "tuple leaves share a binder in {lane}.{depth}"
            );
        }
        binders
    }

    assert_pinned_wave(
        lanes,
        "left",
        "outer",
        [
            ("selected", "HEAD~450"),
            ("left", "HEAD~440"),
            ("middle", "HEAD~430"),
            ("right", "HEAD~420"),
        ],
    );
    assert_pinned_wave(
        lanes,
        "right",
        "outer",
        [
            ("selected", "HEAD~410"),
            ("left", "HEAD~400"),
            ("middle", "HEAD~390"),
            ("right", "HEAD~380"),
        ],
    );
    let left_failed_wave = assert_unbound_wave(
        lanes,
        "left",
        "first",
        &["HEAD~370", "HEAD~350", "HEAD~340"],
    );
    assert_pinned_wave(
        lanes,
        "right",
        "first",
        [
            ("selected", "HEAD~330"),
            ("left", "HEAD~320"),
            ("middle", "HEAD~310"),
            ("right", "HEAD~300"),
        ],
    );
    assert_pinned_wave(
        lanes,
        "left",
        "second",
        [
            ("selected", "HEAD~290"),
            ("left", "HEAD~280"),
            ("middle", "HEAD~270"),
            ("right", "HEAD~260"),
        ],
    );
    let right_failed_wave = assert_unbound_wave(
        lanes,
        "right",
        "second",
        &["HEAD~240", "HEAD~230", "HEAD~220"],
    );
    assert!(
        left_failed_wave.is_disjoint(&right_failed_wave),
        "incomplete tuple waves from opposite lanes must retain separate binder identities"
    );
    let left_terminal_failed_wave = assert_unbound_wave(
        lanes,
        "left",
        "third",
        &["HEAD~210", "HEAD~200", "HEAD~190"],
    );
    assert!(
        left_terminal_failed_wave.is_disjoint(&left_failed_wave),
        "separate incomplete capture depths must retain separate binder identities"
    );
    assert_pinned_wave(
        lanes,
        "right",
        "third",
        [
            ("selected", "HEAD~170"),
            ("left", "HEAD~160"),
            ("middle", "HEAD~150"),
            ("right", "HEAD~140"),
        ],
    );
}

#[test]
fn bottom_incomplete_tuple_argument_does_not_rebind_sibling_snapshot_pin() {
    let source = include_str!("fixtures/historical-nested-tuple-bottom-cascade-depth-wave.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-nested-tuple-bottom-cascade-depth-wave.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("scalar_pin_after_incomplete_tuple_wave")
        })
        .expect("scalar pin / incomplete tuple fixture module");
    let Type::Function { result, .. } =
        &module.symbols["scalar_pin_after_incomplete_tuple_wave"].ty
    else {
        panic!("scalar pin proof must be a function");
    };
    let Type::Record(cases) = result.as_ref() else {
        panic!("scalar pin proof must expose both calls");
    };
    fn global_selector<'a>(cases: &'a BTreeMap<String, Type>, case: &str) -> &'a str {
        let Type::Record(fields) = cases.get(case).expect("call case") else {
            panic!("{case} call must expose captured values");
        };
        let Type::Stream(element) = fields.get("global").expect("global snapshot stream") else {
            panic!("{case}.global must remain a stream");
        };
        let Type::Applied { base, arguments } = element.as_ref() else {
            panic!("{case}.global must retain a historical callable");
        };
        assert_eq!(base, "sys.HistoricalCallable");
        let [pin, _] = arguments.as_slice() else {
            panic!("{case}.global must retain its snapshot identity: {arguments:?}");
        };
        let Type::Applied {
            base,
            arguments: context_arguments,
        } = pin
        else {
            panic!("{case}.global must retain a snapshot context: {pin:?}");
        };
        assert_eq!(base, "sys.SnapshotRefContext");
        let [Type::Named(selector)] = context_arguments.as_slice() else {
            panic!("{case}.global context must have one identity: {context_arguments:?}");
        };
        selector
            .strip_prefix("selector:")
            .expect("snapshot context selector")
    }
    let incomplete = global_selector(cases, "incomplete");
    assert!(
        incomplete.starts_with(
            "dynamic-call:historical-nested-tuple-bottom-cascade-depth-wave.orna:"
        ),
        "a call with a non-returning tuple argument cannot bind its sibling snapshot: {incomplete}"
    );
    assert!(
        incomplete.ends_with(":parameter:global_pin"),
        "the unresolved sibling must retain the scalar parameter identity: {incomplete}"
    );
    assert_eq!(
        global_selector(cases, "complete"),
        "HEAD~60",
        "a complete tuple wave still binds its sibling snapshot"
    );
}

#[test]
fn malformed_tuple_wave_suppresses_rebinding_across_sibling_captures() {
    let source = include_str!("fixtures/historical-tuple-pin-sibling-capture-suppression.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-pin-sibling-capture-suppression.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "the malformed tuple leaf must remain a type error"
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("malformed_tuple_sibling_capture_wave")
        })
        .expect("sibling capture suppression fixture module");
    let Type::Function { result, .. } =
        &module.symbols["malformed_tuple_sibling_capture_wave"].ty
    else {
        panic!("sibling capture suppression proof must be a function");
    };
    let Type::Record(cases) = result.as_ref() else {
        panic!("sibling capture suppression proof must expose both waves");
    };
    let incomplete = cases.get("incomplete").expect("incomplete wave");
    let mut incomplete_contexts = BTreeSet::new();
    collect_snapshot_contexts(incomplete, &mut incomplete_contexts);
    for selector in [
        "HEAD~600",
        "HEAD~590",
        "HEAD~580",
        "HEAD~550",
        "HEAD~530",
    ] {
        assert!(
            !incomplete_contexts
                .iter()
                .any(|context| context.contains(selector)),
            "malformed tuple wave leaked {selector} into a sibling capture: {incomplete_contexts:?}"
        );
    }
    let unresolved_siblings = incomplete_contexts
        .iter()
        .filter(|context| context.starts_with("selector:dynamic-call:"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        unresolved_siblings.len(),
        4,
        "failed tuple wave must preserve distinct unresolved identities for all four tuple captures: {incomplete_contexts:?}"
    );
    assert!(
        incomplete_contexts.contains("selector:HEAD~570"),
        "the completed sibling leaf argument must keep its own selector: {incomplete_contexts:?}"
    );
    let complete = cases.get("complete").expect("complete wave");
    let mut complete_contexts = BTreeSet::new();
    collect_snapshot_contexts(complete, &mut complete_contexts);
    for selector in ["HEAD~560", "HEAD~550", "HEAD~540", "HEAD~530", "HEAD~520"] {
        assert!(
            complete_contexts.contains(&format!("selector:{selector}")),
            "complete tuple wave lost sibling capture {selector}: {complete_contexts:?}"
        );
    }
}

#[test]
fn missing_tuple_wave_suppresses_sibling_rebinding_without_stub_results() {
    let source = include_str!("fixtures/historical-missing-tuple-pin-suppresses-sibling-captures.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-missing-tuple-pin-suppresses-sibling-captures.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "omitting the required right tuple must remain a call error"
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("omitted_tuple_suppresses_sibling_capture_rebinding")
        })
        .expect("omitted tuple capture fixture module");
    let Type::Function { result, .. } =
        &module.symbols["omitted_tuple_suppresses_sibling_capture_rebinding"].ty
    else {
        panic!("omitted tuple proof must be a function");
    };
    let Type::Record(cases) = result.as_ref() else {
        panic!("omitted tuple proof must expose incomplete and complete calls");
    };
    fn assert_real_factory_values(record: &Type) {
        let Type::Record(fields) = record else {
            panic!("capture call must return a computed record: {record:?}");
        };
        for name in [
            "left_selected_capture",
            "left_sibling_capture",
            "right_selected_capture",
            "right_sibling_capture",
            "terminal_capture",
        ] {
            let Type::Applied { base, arguments } = fields.get(name).expect("capture field") else {
                panic!("{name} must be a real historical database callable");
            };
            assert_eq!(base, "sys.HistoricalCallable", "{name}");
            let [pin, Type::Function { result, .. }] = arguments.as_slice() else {
                panic!("{name} must retain its snapshot and computed result: {arguments:?}");
            };
            assert!(
                matches!(pin, Type::Applied { base, .. } if base == "sys.SnapshotRefContext"),
                "{name} must retain its exact snapshot context: {pin:?}"
            );
            let Type::Record(read_members) = result.as_ref() else {
                panic!("{name} must expose the database read result: {result:?}");
            };
            assert!(
                matches!(read_members.get("read"), Some(Type::Applied { base, .. }) if base == "sys.HistoricalCallable"),
                "{name} must retain its real read callable: {read_members:?}"
            );
        }
    }
    let incomplete = cases.get("incomplete").expect("incomplete capture call");
    assert_real_factory_values(incomplete);
    let mut incomplete_contexts = BTreeSet::new();
    collect_snapshot_contexts(incomplete, &mut incomplete_contexts);
    for selector in ["HEAD~600", "HEAD~590"] {
        assert!(
            !incomplete_contexts
                .iter()
                .any(|context| context.contains(selector)),
            "a supplied sibling tuple leaked {selector} despite the omitted tuple: {incomplete_contexts:?}"
        );
    }
    let unresolved_siblings = incomplete_contexts
        .iter()
        .filter(|context| context.starts_with("selector:dynamic-call:"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        unresolved_siblings.len(),
        4,
        "both tuple captures must retain distinct unresolved pin identities: {incomplete_contexts:?}"
    );
    assert!(
        incomplete_contexts.contains("selector:HEAD~570"),
        "the later complete leaf call must keep its own selected pin: {incomplete_contexts:?}"
    );

    let complete = cases.get("complete").expect("complete capture call");
    assert_real_factory_values(complete);
    let mut complete_contexts = BTreeSet::new();
    collect_snapshot_contexts(complete, &mut complete_contexts);
    for selector in ["HEAD~560", "HEAD~550", "HEAD~540", "HEAD~530", "HEAD~520"] {
        assert!(
            complete_contexts.contains(&format!("selector:{selector}")),
            "complete paired tuple wave lost computed capture {selector}: {complete_contexts:?}"
        );
    }
}

#[test]
fn missing_required_sibling_suppresses_tuple_pin_rebinding() {
    let source =
        include_str!("fixtures/historical-missing-sibling-scalar-preserves-tuple-pins.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-missing-sibling-scalar-preserves-tuple-pins.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "omitting the required label must remain a call error"
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("omitted_required_label_suppresses_tuple_capture_rebinding")
        })
        .expect("missing sibling argument fixture module");
    let Type::Function { result, .. } =
        &module.symbols["omitted_required_label_suppresses_tuple_capture_rebinding"].ty
    else {
        panic!("missing sibling argument proof must be a function");
    };
    let Type::Record(cases) = result.as_ref() else {
        panic!("missing sibling argument proof must expose both calls");
    };
    let incomplete = cases.get("incomplete").expect("incomplete call");
    let Type::Record(fields) = incomplete else {
        panic!("incomplete call must retain computed capture values: {incomplete:?}");
    };
    fn assert_real_capture_values(fields: &BTreeMap<String, Type>) {
        for name in [
            "left_selected_capture",
            "left_sibling_capture",
            "terminal_capture",
        ] {
            let Type::Applied { base, arguments } = fields.get(name).expect("capture field") else {
                panic!("{name} must remain a real historical callable: {fields:?}");
            };
            assert_eq!(base, "sys.HistoricalCallable", "{name}");
            let [pin, Type::Function { result, .. }] = arguments.as_slice() else {
                panic!("{name} must retain its snapshot and read result: {arguments:?}");
            };
            assert!(
                matches!(pin, Type::Applied { base, .. } if base == "sys.SnapshotRefContext"),
                "{name} must retain a computed snapshot context: {pin:?}"
            );
            let Type::Record(read_members) = result.as_ref() else {
                panic!("{name} must expose a computed database read result: {result:?}");
            };
            assert!(
                matches!(read_members.get("read"), Some(Type::Applied { base, .. }) if base == "sys.HistoricalCallable"),
                "{name} must contain its real read callable: {read_members:?}"
            );
        }
    }
    assert_real_capture_values(fields);
    let mut incomplete_contexts = BTreeSet::new();
    collect_snapshot_contexts(incomplete, &mut incomplete_contexts);
    for selector in ["HEAD~700", "HEAD~690"] {
        assert!(
            !incomplete_contexts
                .iter()
                .any(|context| context.contains(selector)),
            "a supplied tuple pin leaked through the missing required sibling: {incomplete_contexts:?}"
        );
    }
    let unresolved_tuple_pins = incomplete_contexts
        .iter()
        .filter(|context| context.starts_with("selector:dynamic-call:"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        unresolved_tuple_pins.len(),
        2,
        "the incomplete call must preserve distinct unresolved tuple identities: {incomplete_contexts:?}"
    );
    assert!(
        incomplete_contexts.contains("selector:HEAD~680"),
        "the later leaf pin still resolves at its own call: {incomplete_contexts:?}"
    );

    let complete = cases.get("complete").expect("complete call");
    let Type::Record(complete_fields) = complete else {
        panic!("complete call must return computed captures: {complete:?}");
    };
    assert_real_capture_values(complete_fields);
    let mut complete_contexts = BTreeSet::new();
    collect_snapshot_contexts(complete, &mut complete_contexts);
    for selector in ["HEAD~670", "HEAD~660", "HEAD~650"] {
        assert!(
            complete_contexts.contains(&format!("selector:{selector}")),
            "a complete call must preserve computed pin {selector}: {complete_contexts:?}"
        );
    }

}

#[test]
fn omitted_sibling_preserves_width_of_paired_tuple_rebinds() {
    let source = include_str!(
        "fixtures/historical-paired-rebind-width-suppressed-by-omission.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-rebind-width-suppressed-by-omission.orna",
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
        "only the intentionally omitted required tuple should be rejected: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("omitted_sibling_keeps_paired_rebind_width")
        })
        .expect("paired omitted-sibling fixture module");
    let Type::Function { result, .. } =
        &module.symbols["omitted_sibling_keeps_paired_rebind_width"].ty
    else {
        panic!("paired width proof must export a function");
    };
    let Type::Record(checkpoints) = result.as_ref() else {
        panic!("paired width proof must expose saved and rebound values");
    };

    let mut saved_width = None;
    for name in ["saved", "rebound"] {
        let value = checkpoints.get(name).expect("computed checkpoint list");
        let Type::List(element) = value else {
            panic!("{name} must remain a list of computed historical values: {value:?}");
        };
        let Type::Record(checkpoint) = element.as_ref() else {
            panic!("{name} must retain its computed checkpoint record: {element:?}");
        };
        let Type::Applied { base, arguments } =
            checkpoint.get("capture").expect("real historical capture")
        else {
            panic!("{name} capture must remain a historical callable: {checkpoint:?}");
        };
        assert_eq!(base, "sys.HistoricalCallable", "{name}");
        let [_, Type::Function { result, .. }] = arguments.as_slice() else {
            panic!("{name} must keep its real callable result: {arguments:?}");
        };
        let Type::Record(read_members) = result.as_ref() else {
            panic!("{name} must preserve the computed database record: {result:?}");
        };
        assert!(
            matches!(read_members.get("read"), Some(Type::Applied { base, .. }) if base == "sys.HistoricalCallable"),
            "{name} must return the real database read callable: {read_members:?}"
        );
        assert_canonical_snapshot_context_maps(value);
        let mut contexts = BTreeSet::new();
        collect_snapshot_contexts(value, &mut contexts);
        if name == "rebound" {
            let unresolved_paired_pins = contexts
                .iter()
                .filter(|context| context.starts_with("selector:dynamic-call:"))
                .count();
            assert_eq!(
                unresolved_paired_pins, 4,
                "the omitted sibling must leave each paired tuple slot symbolic: {contexts:?}"
            );
            assert!(
                contexts.contains("selector:HEAD~870"),
                "the later complete terminal binding must stay concrete: {contexts:?}"
            );
            assert_eq!(contexts.len(), 5, "paired rebind width changed: {contexts:?}");
            assert_eq!(
                Some(contexts.len()),
                saved_width,
                "suppression must leave paired rebind width unchanged"
            );
        } else {
            assert_eq!(
                contexts,
                BTreeSet::from([
                    "selector:HEAD~960".to_owned(),
                    "selector:HEAD~950".to_owned(),
                    "selector:HEAD~940".to_owned(),
                    "selector:HEAD~930".to_owned(),
                    "selector:HEAD~920".to_owned(),
                ]),
                "the complete paired rebind must retain all five concrete pins"
            );
            saved_width = Some(contexts.len());
        }
    }
}

#[test]
fn unknown_paired_width_suppresses_sibling_pin_promotion() {
    let source = include_str!(
        "fixtures/historical-unknown-paired-width-rebind-suppression.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-unknown-paired-width-rebind-suppression.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNRESOLVED),
        "the unknown paired leaf must be reported during recovery"
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_unknown_width_does_not_promote_siblings")
        })
        .expect("unknown paired-width fixture module");
    let Type::Function { result, .. } =
        &module.symbols["paired_unknown_width_does_not_promote_siblings"].ty
    else {
        panic!("paired-width proof must be a function");
    };
    let Type::Record(stages) = result.as_ref() else {
        panic!("paired-width proof must retain its computed result record: {result:?}");
    };
    for stage in ["saved", "suppressed"] {
        let value = stages.get(stage).expect("computed capture list");
        let Type::List(element) = value else {
            panic!("{stage} must remain a computed historical list: {value:?}");
        };
        let Type::Record(checkpoint) = element.as_ref() else {
            panic!("{stage} must retain the computed checkpoint record: {element:?}");
        };
        assert!(
            matches!(checkpoint.get("capture"), Some(Type::Applied { base, .. }) if base == "sys.HistoricalCallable"),
            "{stage} must return its real historical callable value: {checkpoint:?}"
        );
        assert_canonical_snapshot_context_maps(value);
        let mut contexts = BTreeSet::new();
        collect_snapshot_contexts(value, &mut contexts);
        if stage == "saved" {
            assert_eq!(
                contexts,
                ["HEAD~60", "HEAD~59", "HEAD~58", "HEAD~57", "HEAD~56"]
                    .map(|selector| format!("selector:{selector}"))
                    .into_iter()
                    .collect(),
                "a fully paired call must promote all five supplied identities"
            );
        } else {
            let symbolic_pins = contexts
                .iter()
                .filter(|context| context.starts_with("selector:dynamic-call:"))
                .count();
            assert_eq!(
                symbolic_pins, 4,
                "an unknown leaf suppresses all four paired identities: {contexts:?}"
            );
            assert_eq!(contexts.len(), 5, "paired width must be retained: {contexts:?}");
            assert!(
                contexts.contains("selector:HEAD~46"),
                "the independent terminal pin still binds concretely: {contexts:?}"
            );
            assert!(
                !contexts.iter().any(|context| {
                    ["HEAD~50", "HEAD~49", "HEAD~48"].iter().any(|selector| {
                        context == &format!("selector:{selector}")
                    })
                }),
                "no valid sibling identity may be promoted across an unknown pair width: {contexts:?}"
            );
        }
    }
}

#[test]
fn paired_pin_identities_survive_omissions_at_outer_and_middle_depths() {
    let source = include_str!(
        "fixtures/historical-paired-omission-depth-identity-preservation.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-omission-depth-identity-preservation.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    let type_diagnostics = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == DIAG_TYPE)
        .count();
    assert_eq!(
        type_diagnostics, 2,
        "only the two calls with intentionally omitted required siblings should fail: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("omitted_sibling_waves_keep_pair_identities")
        })
        .expect("paired omission-depth fixture module");
    let Type::Function { result, .. } =
        &module.symbols["omitted_sibling_waves_keep_pair_identities"].ty
    else {
        panic!("omission-depth proof must be a function");
    };
    let Type::Record(stages) = result.as_ref() else {
        panic!("omission-depth proof must retain its computed stage record: {result:?}");
    };
    for (stage, concrete_selectors) in [
        (
            "complete",
            vec![
                "HEAD~120", "HEAD~119", "HEAD~118", "HEAD~117", "HEAD~116", "HEAD~115",
                "HEAD~114", "HEAD~113", "HEAD~112",
            ],
        ),
        (
            "outer_suppressed",
            vec!["HEAD~90", "HEAD~89", "HEAD~88", "HEAD~87", "HEAD~86"],
        ),
        (
            "middle_suppressed",
            vec!["HEAD~120", "HEAD~119", "HEAD~118", "HEAD~117", "HEAD~76"],
        ),
    ] {
        let value = stages.get(stage).expect("computed stage value");
        let Type::List(element) = value else {
            panic!("{stage} must remain a computed historical list: {value:?}");
        };
        let Type::Record(checkpoint) = element.as_ref() else {
            panic!("{stage} must retain its computed checkpoint record: {element:?}");
        };
        assert!(
            matches!(checkpoint.get("capture"), Some(Type::Applied { base, .. }) if base == "sys.HistoricalCallable"),
            "{stage} must keep a real historical callable result: {checkpoint:?}"
        );
        assert_canonical_snapshot_context_maps(value);
        let mut contexts = BTreeSet::new();
        collect_snapshot_contexts(value, &mut contexts);
        assert_eq!(contexts.len(), 9, "{stage} collapsed paired slots: {contexts:?}");
        let symbolic = contexts
            .iter()
            .filter(|context| context.starts_with("selector:dynamic-call:"))
            .count();
        assert_eq!(
            symbolic,
            if stage == "complete" { 0 } else { 4 },
            "only the omitted depth's four tuple pins should remain symbolic: {contexts:?}"
        );
        for selector in concrete_selectors {
            assert!(
                contexts.contains(&format!("selector:{selector}")),
                "{stage} lost independent concrete selector {selector}: {contexts:?}"
            );
        }
    }
}

#[test]
fn missing_outer_tuple_keeps_sibling_pins_symbolic_across_later_depths() {
    let source =
        include_str!("fixtures/historical-missing-outer-tuple-preserves-sibling-pins.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-missing-outer-tuple-preserves-sibling-pins.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "omitting the required sibling tuple must remain a call error"
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("omitted_outer_tuple_preserves_sibling_pins_across_later_depths")
        })
        .expect("nested missing tuple fixture module");
    let Type::Function { result, .. } =
        &module.symbols["omitted_outer_tuple_preserves_sibling_pins_across_later_depths"].ty
    else {
        panic!("nested missing tuple proof must be a function");
    };
    let Type::Record(cases) = result.as_ref() else {
        panic!("nested missing tuple proof must return its incomplete case");
    };
    fn assert_real_factory_values(record: &Type) {
        let Type::Record(fields) = record else {
            panic!("capture wave must return computed values: {record:?}");
        };
        for name in [
            "left_selected_capture",
            "left_sibling_capture",
            "right_selected_capture",
            "right_sibling_capture",
            "middle_selected_capture",
            "middle_sibling_capture",
            "terminal_capture",
        ] {
            let Type::Applied { base, arguments } = fields.get(name).expect("capture field") else {
                panic!("{name} must be a real historical callable: {fields:?}");
            };
            assert_eq!(base, "sys.HistoricalCallable", "{name}");
            let [pin, Type::Function { result, .. }] = arguments.as_slice() else {
                panic!("{name} must retain its pin and read result: {arguments:?}");
            };
            assert!(
                matches!(pin, Type::Applied { base, .. } if base == "sys.SnapshotRefContext"),
                "{name} must retain a computed pin context: {pin:?}"
            );
            let Type::Record(read_members) = result.as_ref() else {
                panic!("{name} must expose its database read result: {result:?}");
            };
            assert!(
                matches!(read_members.get("read"), Some(Type::Applied { base, .. }) if base == "sys.HistoricalCallable"),
                "{name} must retain its real read callable: {read_members:?}"
            );
        }
    }

    let incomplete = cases.get("incomplete").expect("incomplete nested wave");
    assert_real_factory_values(incomplete);
    let mut incomplete_contexts = BTreeSet::new();
    collect_snapshot_contexts(incomplete, &mut incomplete_contexts);
    for selector in ["HEAD~900", "HEAD~890"] {
        assert!(
            !incomplete_contexts
                .iter()
                .any(|context| context.contains(selector)),
            "the incomplete outer wave leaked {selector} into a sibling capture: {incomplete_contexts:?}"
        );
    }
    let unresolved_outer_pins = incomplete_contexts
        .iter()
        .filter(|context| context.starts_with("selector:dynamic-call:"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        unresolved_outer_pins.len(),
        4,
        "the missing tuple must leave four distinct outer pins symbolic: {incomplete_contexts:?}"
    );
    for selector in ["HEAD~880", "HEAD~870", "HEAD~860"] {
        assert!(
            incomplete_contexts.contains(&format!("selector:{selector}")),
            "later valid depth must still compute pin {selector}: {incomplete_contexts:?}"
        );
    }

}

#[test]
fn concurrent_callback_tuples_reject_cross_lane_identity_mix_after_rebind() {
    let source = include_str!("fixtures/historical-concurrent-tuple-callback-rebind-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-concurrent-tuple-callback-rebind-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "tuple-stored callbacks from opposite paired lanes must keep distinct pins after restore: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_pin_cascade_paired_depths_reject_cross_pair_mixing_after_storm_rebinds() {
    let source = include_str!("fixtures/historical-tuple-pin-cascade-paired-depth-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-pin-cascade-paired-depth-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "paired cascade results from separate pin trees must remain type-distinct: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_pin_cascade_paired_chains_preserve_every_depth_through_storm_rebinds() {
    let source = include_str!("fixtures/historical-tuple-pin-cascade-paired-chain-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-pin-cascade-paired-chain-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired tuple cascade chains must preserve all depth pins through each storm rebind: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_pin_cascade_paired_chains_reject_cross_chain_mixing_after_storm_rebinds() {
    let source = include_str!("fixtures/historical-tuple-pin-cascade-paired-chain-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-pin-cascade-paired-chain-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "cascade results from separate paired chains must remain type-distinct: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_cascade_chains_preserve_root_bridge_and_leaf_pins_through_storm_rebinds() {
    let source = include_str!("fixtures/historical-paired-cascade-chains-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-cascade-chains-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired root-to-leaf closure chains must preserve each captured pin at every saved storm stage: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_cascade_chains_reject_cross_chain_leaf_mixing_after_storm_rebinds() {
    let source = include_str!("fixtures/historical-paired-cascade-chains-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-cascade-chains-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "leaf results from separate paired closure chains must remain type-distinct: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_closure_chains_preserve_each_pin_through_storm_rebinds() {
    let source = include_str!("fixtures/historical-paired-nested-closure-chains-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-closure-chains-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired nested closure chains must preserve root, bridge, and leaf pins through forwarded storm rebinds: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_closure_chains_reject_cross_chain_leaf_mixing() {
    let source = include_str!("fixtures/historical-paired-nested-closure-chains-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-closure-chains-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "leaf results from separate nested closure chains must remain type-distinct: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_closure_chain_depth_storms_preserve_each_captured_pin() {
    let source = include_str!("fixtures/historical-paired-nested-closure-chain-depth-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-closure-chain-depth-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired nested closure chains must preserve root pins through the first storm and captured bridge pins through the second: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_closure_chain_depth_storms_reject_cross_chain_leaf_mixing() {
    let source = include_str!("fixtures/historical-paired-nested-closure-chain-depth-storm-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-closure-chain-depth-storm-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "leaf values captured by opposite sides of the nested closure pair must remain type-distinct: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_chain_rebind_cascades_preserve_each_captured_depth() {
    let source = include_str!("fixtures/historical-paired-nested-chain-rebind-cascades.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-chain-rebind-cascades.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired root, bridge, and leaf closures must preserve their captured pins through chained rebind cascades: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_chain_rebind_cascades_reject_cross_chain_terminal_mixing() {
    let source = include_str!("fixtures/historical-paired-nested-chain-rebind-cascades-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-chain-rebind-cascades-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "terminal values from opposite sides of the paired nested chain must remain type-distinct: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_chain_rebind_replays_preserve_captured_pin_identity() {
    let source = include_str!("fixtures/historical-paired-nested-chain-rebind-replay-stability.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-chain-rebind-replay-stability.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "replaying saved paired closures after nested decoy storms must preserve root, bridge, leaf, and terminal pin identity: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_chain_rebind_replays_reject_cross_chain_terminal_mixing() {
    let source = include_str!("fixtures/historical-paired-nested-chain-rebind-replay-stability-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-chain-rebind-replay-stability-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "replayed terminal closures from opposite sides of a paired chain must remain type-distinct: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_chain_reproduction_after_depth_storms_preserves_pin_identity() {
    let source = include_str!("fixtures/historical-paired-nested-chain-rebind-reproduction.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-chain-rebind-reproduction.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "a freshly reproduced paired chain must retain each original root, bridge, leaf, and terminal pin after intervening depth storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_chain_reproduction_rejects_cross_chain_terminal_mixing() {
    let source = include_str!("fixtures/historical-paired-nested-chain-rebind-reproduction-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-chain-rebind-reproduction-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "terminal pins from a reproduced chain must remain distinct across pair sides after depth storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_chain_rebind_paths_stay_consistent_across_storm_orders() {
    let source = include_str!("fixtures/historical-paired-nested-chain-rebind-consistency.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-chain-rebind-consistency.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "direct, restored, and freshly rebuilt paired chains must agree on every captured pin across different depth-storm orders: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_chain_rebind_consistency_keeps_rebuilt_pair_sides_distinct() {
    let source = include_str!("fixtures/historical-paired-nested-chain-rebind-consistency-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-chain-rebind-consistency-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "a rebuilt right-side terminal must not type-check as the original left-side terminal after storm-order variation: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_chain_reproductions_stay_consistent_at_each_storm_depth() {
    let source = include_str!("fixtures/historical-paired-nested-chain-rebind-checkpoints.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-chain-rebind-checkpoints.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "fresh paired-chain reproductions after root, bridge, and leaf storms must retain identical pins at every downstream depth: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_nested_chain_reproductions_keep_checkpoint_sides_distinct() {
    let source = include_str!("fixtures/historical-paired-nested-chain-rebind-checkpoints-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-nested-chain-rebind-checkpoints-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "terminal identities from opposite sides must not merge across reproduction checkpoints: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_depth_storm_reproductions_keep_nested_pin_consistency() {
    let source = include_str!("fixtures/historical-paired-depth-storm-rebind-reproduction.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-rebind-reproduction.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired sides stormed at different nested depths and in different orders must reproduce the same root, bridge, leaf, and terminal pins: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_depth_storm_reproductions_reject_cross_lane_terminal_mixing() {
    let source = include_str!("fixtures/historical-paired-depth-storm-rebind-reproduction-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-rebind-reproduction-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "left and right terminal pins must remain distinct after cross-depth reproduction: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_depth_storm_reproductions_preserve_chained_nested_identity() {
    let source = include_str!("fixtures/historical-paired-depth-storm-chained-reproduction.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-chained-reproduction.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "the reproduced paired chain and its chained follow-up must agree on every captured pin after depth storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_depth_storm_chained_reproductions_reject_cross_pair_mixing() {
    let source = include_str!("fixtures/historical-paired-depth-storm-chained-reproduction-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-chained-reproduction-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "terminal identities in chained follow-ups must remain isolated across the paired reproductions: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_depth_storm_chained_rebinds_preserve_followup_stability() {
    let source = include_str!("fixtures/historical-paired-depth-storm-chained-rebind-stability.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-chained-rebind-stability.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "restored and freshly reproduced follow-up chains must remain consistent after their own root, bridge, and leaf rebind storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_depth_storm_chained_rebinds_reject_followup_pair_mixing() {
    let source = include_str!("fixtures/historical-paired-depth-storm-chained-rebind-stability-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-chained-rebind-stability-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "follow-up terminal pins must remain distinct across paired rebind storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_depth_storm_followup_continuations_preserve_rebind_identity() {
    let source = include_str!("fixtures/historical-paired-depth-storm-followup-continuation.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-followup-continuation.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "a continuation captured by an original or reproduced follow-up must retain its paired root, bridge, and leaf identities after rebind storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_depth_storm_followup_continuations_reject_cross_pair_mixing() {
    let source = include_str!("fixtures/historical-paired-depth-storm-followup-continuation-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-followup-continuation-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "follow-up continuations from different reproduced sides must retain distinct snapshot identities: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_depth_storm_followup_continuation_chains_remain_consistent() {
    let source = include_str!("fixtures/historical-paired-depth-storm-followup-continuation-chain.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-followup-continuation-chain.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "terminal follow-up chains captured by original and reproduced continuations must stay consistent through a second paired root, bridge, and leaf storm: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_depth_storm_followup_continuation_chains_reject_cross_pair_mixing() {
    let source = include_str!("fixtures/historical-paired-depth-storm-followup-continuation-chain-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-followup-continuation-chain-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "terminal follow-up chains from different reproduced sides must retain distinct snapshot identities: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_followup_continuation_checkpoints_verify_nested_storm_rebinds() {
    let source = include_str!("fixtures/historical-paired-depth-storm-followup-continuation-verification.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-followup-continuation-verification.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "saved and restored roots, bridges, leaves, and terminal values must remain consistent throughout both nested follow-up chains: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_followup_continuation_checkpoints_reject_cross_pair_mixing() {
    let source = include_str!("fixtures/historical-paired-depth-storm-followup-continuation-verification-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-depth-storm-followup-continuation-verification-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "terminal outputs from different paired follow-up chains must retain distinct snapshot identities: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_rebind_chains_verify_depth_storms() {
    let source = include_str!("fixtures/historical-paired-continuation-rebind-chain-verification.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-rebind-chain-verification.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "saved and restored roots, bridges, leaves, and terminal values must remain consistent through a third nested paired continuation chain: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_rebind_chains_reject_cross_pair_mixing() {
    let source = include_str!("fixtures/historical-paired-continuation-rebind-chain-verification-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-rebind-chain-verification-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "deepest follow-up outputs from different paired sides must retain distinct snapshot identities: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_chain_depth_storms_preserve_rebind_stability() {
    let source = include_str!("fixtures/historical-paired-continuation-chain-storm-stability.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-chain-storm-stability.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired continuation roots, bridges, and leaves must restore their captured identities across repeated depth storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_chain_depth_storms_reject_mixed_lanes() {
    let source = include_str!("fixtures/historical-paired-continuation-chain-storm-stability-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-chain-storm-stability-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "terminal values from the left and right paired continuation chains must retain distinct identities after restoration: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_chains_survive_chained_depth_storm_rebinds() {
    let source = include_str!("fixtures/historical-paired-continuation-chain-depth-storm-rebinds.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-chain-depth-storm-rebinds.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired roots, bridges, and leaves must retain identity through two successive continuation chains and their rebind storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_chains_reject_final_cross_lane_mixing() {
    let source = include_str!("fixtures/historical-paired-continuation-chain-depth-storm-rebinds-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-chain-depth-storm-rebinds-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "final terminal values from paired continuation chains must remain distinct after chained storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_chain_consistency_survives_rebind_order_changes() {
    let source = include_str!("fixtures/historical-paired-continuation-chain-consistency-rebind-order.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-chain-consistency-rebind-order.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired continuation chains must retain consistent captured identities when the two sides restore rebind depths in different orders: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_chain_consistency_rebind_orders_reject_cross_pairing() {
    let source = include_str!("fixtures/historical-paired-continuation-chain-consistency-rebind-order-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-chain-consistency-rebind-order-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "cross-paired terminal pins must remain distinct after restoring a chained continuation storm: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_rebind_cascades_remain_consistent() {
    let source = include_str!("fixtures/historical-paired-continuation-rebind-cascade-stability.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-rebind-cascade-stability.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "captured follow-up and terminal continuations must keep consistent identities through successive paired rebind cascades: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_rebind_cascades_reject_cross_lane_mixing() {
    let source = include_str!("fixtures/historical-paired-continuation-rebind-cascade-stability-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-rebind-cascade-stability-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "terminal values from opposite cascade lanes must keep distinct snapshot identities after restoration: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_rebind_reproductions_remain_consistent() {
    let source = include_str!("fixtures/historical-paired-continuation-rebind-reproduction.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-rebind-reproduction.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "freshly reconstructed paired continuation chains must agree with captured outputs after chained depth storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_rebind_reproductions_reject_cross_lane_mixing() {
    let source = include_str!("fixtures/historical-paired-continuation-rebind-reproduction-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-rebind-reproduction-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "reproduced terminal outputs from distinct pair lanes must keep their snapshot identities after storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_reproductions_remain_stable_across_storm_cycles() {
    let source = include_str!("fixtures/historical-paired-continuation-reproduction-stability.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-reproduction-stability.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired continuation outputs must remain stable after repeated rebind storms and fresh reproduction at both nested depths: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_reproductions_reject_cross_lane_mixing() {
    let source = include_str!("fixtures/historical-paired-continuation-reproduction-stability-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-reproduction-stability-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "terminal outputs from reproduced continuations on different pair lanes must retain distinct identities: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_reproduction_consistency_survives_repeated_storm_rebinds() {
    let source = include_str!("fixtures/historical-paired-continuation-reproduction-consistency.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-reproduction-consistency.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "repeatedly reconstructed continuations must preserve each lane's restored observations through chained depth-storm rebinds: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_reproduction_consistency_rejects_lane_mixing() {
    let source = include_str!("fixtures/historical-paired-continuation-reproduction-consistency-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-reproduction-consistency-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "repeatedly reproduced terminal values from distinct lanes must retain separate snapshot identities after restoration: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_reproductions_remain_consistent_across_paired_depth_storms() {
    let source = include_str!("fixtures/historical-paired-continuation-reproduction-paired-storms.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-reproduction-paired-storms.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "repeatedly reconstructed pairs must retain each lane's saved closure identity after restoring every depth of paired rebind storms: {:?}",
        result.diagnostics
    );
}

#[test]
fn paired_continuation_reproduction_storms_reject_cross_lane_mixing() {
    let source = include_str!("fixtures/historical-paired-continuation-reproduction-paired-storms-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-reproduction-paired-storms-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "repeatedly reproduced continuations must keep the paired lanes distinct after depth-storm restoration: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_reproductions_remain_stable_across_chained_rebind_orders() {
    let source = include_str!("fixtures/historical-paired-continuation-reproduction-stability-rebind-order.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-reproduction-stability-rebind-order.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "repeated paired continuation reproductions must remain stable across opposite chained depth-storm rebind orders: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_continuation_reproduction_storms_reject_mixed_repeated_lanes() {
    let source = include_str!("fixtures/historical-paired-continuation-reproduction-stability-rebind-order-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-continuation-reproduction-stability-rebind-order-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "cross-paired repeated continuation outputs must keep lane-specific identities after chained storms: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_reproductions_remain_stable_across_alternating_storm_orders() {
    let source = include_str!("fixtures/historical-paired-reproduction-stability-roundtrip.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-reproduction-stability-roundtrip.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "paired continuation outputs must remain stable when repeated chains alternate root-first and descendant-first storm restoration: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );

    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_reproductions_remain_stable_across_chained_storm_orders")
        })
        .expect("paired reproduction verification module");
    let function_ty = &module.symbols
        ["paired_reproductions_remain_stable_across_chained_storm_orders"]
        .ty;
    let Type::Function { result, .. } = function_ty else {
        panic!("paired reproduction proof must export a function");
    };
    let Type::Record(outputs) = result.as_ref() else {
        panic!("paired reproduction proof must expose its verification record");
    };
    for lane in ["left_checkpoints", "right_checkpoints"] {
        let Some(Type::Record(checkpoints)) = outputs.get(lane) else {
            panic!("{lane} must expose depth checkpoints");
        };
        for depth in ["roots", "bridges", "leaves", "outputs"] {
            let Some(Type::List(element)) = checkpoints.get(depth) else {
                panic!("{lane}.{depth} must expose a homogeneous verification sequence");
            };
            assert!(
                !matches!(element.as_ref(), Type::Error),
                "{lane}.{depth} must retain a valid static type"
            );
        }
    }
    assert_ne!(
        outputs.get("left_checkpoints"),
        outputs.get("right_checkpoints"),
        "paired storm orders must retain lane-specific snapshot identities"
    );
}

#[test]
fn paired_checkpoints_retain_their_selected_lane_contexts() {
    let source = include_str!("fixtures/historical-paired-reproduction-stability-roundtrip.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-reproduction-stability-roundtrip.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_reproductions_remain_stable_across_chained_storm_orders")
        })
        .expect("paired reproduction checkpoint module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_reproductions_remain_stable_across_chained_storm_orders"]
        .ty
    else {
        panic!("paired reproduction proof must export a function");
    };
    let Type::Record(checkpoints) = result.as_ref() else {
        panic!("paired reproduction proof must expose checkpoint records");
    };

    for (lane, expected_depths) in [
        (
            "left_checkpoints",
            [
                ("roots", &["selector:HEAD~760"][..]),
                (
                    "bridges",
                    &["selector:HEAD~730", "selector:HEAD~740", "selector:HEAD~760"][..],
                ),
                (
                    "leaves",
                    &[
                        "selector:HEAD~710",
                        "selector:HEAD~720",
                        "selector:HEAD~730",
                        "selector:HEAD~740",
                        "selector:HEAD~760",
                    ][..],
                ),
                (
                    "outputs",
                    &[
                        "selector:HEAD~690",
                        "selector:HEAD~700",
                        "selector:HEAD~710",
                        "selector:HEAD~720",
                        "selector:HEAD~730",
                        "selector:HEAD~740",
                        "selector:HEAD~760",
                    ][..],
                ),
            ],
        ),
        (
            "right_checkpoints",
            [
                ("roots", &["selector:HEAD~750"][..]),
                (
                    "bridges",
                    &["selector:HEAD~730", "selector:HEAD~750"][..],
                ),
                (
                    "leaves",
                    &[
                        "selector:HEAD~710",
                        "selector:HEAD~730",
                        "selector:HEAD~750",
                    ][..],
                ),
                (
                    "outputs",
                    &[
                        "selector:HEAD~690",
                        "selector:HEAD~710",
                        "selector:HEAD~730",
                        "selector:HEAD~750",
                    ][..],
                ),
            ],
        ),
    ] {
        let ty = checkpoints.get(lane).expect("paired lane checkpoint record");
        let Type::Record(depths) = ty else {
            panic!("{lane} must expose checkpoint depth records");
        };
        for (depth, expected) in expected_depths {
            let ty = depths.get(depth).expect("checkpoint depth field");
            let mut contexts = BTreeSet::new();
            collect_snapshot_contexts(ty, &mut contexts);
            let selected_contexts = contexts
                .iter()
                .filter(|context| context.starts_with("selector:HEAD~"))
                .cloned()
                .collect::<BTreeSet<_>>();
            assert_eq!(
                selected_contexts,
                expected.iter().map(|context| (*context).to_owned()).collect(),
                "{lane}.{depth} must keep every concrete pin selected at that rebind depth"
            );
            assert!(
                contexts
                    .iter()
                    .all(|context| !context.starts_with("selector:parameter:")),
                "{lane}.{depth} must not leave a same-named pin parameter unresolved: {contexts:?}"
            );
        }
    }
}

fn collect_snapshot_contexts(ty: &Type, contexts: &mut BTreeSet<String>) {
    match ty {
        Type::Named(name) if name.starts_with("selector:") => {
            contexts.insert(name.clone());
        }
        Type::List(inner)
        | Type::Range(inner)
        | Type::Relation(inner)
        | Type::Stream(inner)
        | Type::Optional(inner) => collect_snapshot_contexts(inner, contexts),
        Type::Applied { arguments, .. } => {
            for argument in arguments {
                collect_snapshot_contexts(argument, contexts);
            }
        }
        Type::Function {
            parameters, result, ..
        } => {
            for parameter in parameters {
                collect_snapshot_contexts(parameter, contexts);
            }
            collect_snapshot_contexts(result, contexts);
        }
        Type::Record(fields) => {
            for field in fields.values() {
                collect_snapshot_contexts(field, contexts);
            }
        }
        Type::Tuple(items) => {
            for item in items {
                collect_snapshot_contexts(item, contexts);
            }
        }
        Type::MoneyPerUnit { currency, unit } => {
            collect_snapshot_contexts(currency, contexts);
            collect_snapshot_contexts(unit, contexts);
        }
        _ => {}
    }
}

fn assert_canonical_snapshot_context_maps(ty: &Type) {
    match ty {
        Type::Applied { base, arguments } => {
            if base == "semantic.SnapshotContextMap" {
                assert!(arguments.len() > 1, "singleton maps must use SnapshotRefContext");
                assert!(arguments.iter().all(|argument| matches!(
                    argument,
                    Type::Named(selector) if selector.starts_with("selector:")
                )), "maps contain only canonical snapshot selectors");
                assert!(arguments.windows(2).all(|pair| matches!(
                    pair,
                    [Type::Named(left), Type::Named(right)] if left < right
                )), "map selectors are sorted and unique");
            }
            for argument in arguments {
                assert_canonical_snapshot_context_maps(argument);
            }
        }
        Type::List(inner)
        | Type::Range(inner)
        | Type::Relation(inner)
        | Type::Stream(inner)
        | Type::Optional(inner) => assert_canonical_snapshot_context_maps(inner),
        Type::Record(fields) => {
            for field in fields.values() {
                assert_canonical_snapshot_context_maps(field);
            }
        }
        Type::Tuple(items) => {
            for item in items {
                assert_canonical_snapshot_context_maps(item);
            }
        }
        Type::Function {
            parameters, result, ..
        } => {
            for parameter in parameters {
                assert_canonical_snapshot_context_maps(parameter);
            }
            assert_canonical_snapshot_context_maps(result);
        }
        Type::MoneyPerUnit { currency, unit } => {
            assert_canonical_snapshot_context_maps(currency);
            assert_canonical_snapshot_context_maps(unit);
        }
        _ => {}
    }
}

fn checkpoint_output_fields(ty: &Type) -> &BTreeMap<String, Type> {
    match ty {
        Type::List(inner) => checkpoint_output_fields(inner),
        Type::Function { result, .. } => checkpoint_output_fields(result),
        Type::Record(fields) => fields,
        _ => panic!("checkpoint must resolve to a record of pinned outputs: {ty:?}"),
    }
}

#[test]
fn paired_checkpoint_outputs_keep_lane_to_snapshot_mapping() {
    let source = include_str!("fixtures/historical-paired-reproduction-stability-roundtrip.orna");
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-reproduction-stability-roundtrip.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_reproductions_remain_stable_across_chained_storm_orders")
        })
        .expect("paired reproduction checkpoint module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_reproductions_remain_stable_across_chained_storm_orders"]
        .ty
    else {
        panic!("paired reproduction proof must export a function");
    };
    let Type::Record(checkpoints) = result.as_ref() else {
        panic!("paired reproduction proof must expose checkpoint records");
    };

    for (lane, expected_depths) in [
        (
            "left_checkpoints",
            [
                ("roots", &["760"][..], None, None, None),
                ("bridges", &["760"][..], Some(&["740", "730"][..]), None, None),
                (
                    "leaves",
                    &["760"][..],
                    Some(&["740", "730"][..]),
                    Some(&["720", "710"][..]),
                    None,
                ),
                (
                    "outputs",
                    &["760"][..],
                    Some(&["740", "730"][..]),
                    Some(&["720", "710"][..]),
                    Some(&["700", "690"][..]),
                ),
            ],
        ),
        (
            "right_checkpoints",
            [
                ("roots", &["750"][..], None, None, None),
                ("bridges", &["750"][..], Some(&["730"][..]), None, None),
                (
                    "leaves",
                    &["750"][..],
                    Some(&["730"][..]),
                    Some(&["710"][..]),
                    None,
                ),
                (
                    "outputs",
                    &["750"][..],
                    Some(&["730"][..]),
                    Some(&["710"][..]),
                    Some(&["690"][..]),
                ),
            ],
        ),
    ] {
        let ty = checkpoints.get(lane).expect("paired lane checkpoint record");
        let Type::Record(depths) = ty else {
            panic!("{lane} must expose checkpoint depth records");
        };
        for (depth, root, bridge, leaf, terminal) in expected_depths {
            let fields = checkpoint_output_fields(
                depths.get(depth).expect("checkpoint depth field"),
            );
            for (field, expected_contexts) in [
                ("root", Some(root)),
                ("bridge", bridge),
                ("leaf", leaf),
                ("terminal", terminal),
            ] {
                let ty = fields.get(field).expect("pinned output field");
                let mut contexts = BTreeSet::new();
                collect_snapshot_contexts(ty, &mut contexts);
                if let Some(expected_contexts) = expected_contexts {
                    assert_eq!(
                        contexts,
                        expected_contexts
                            .iter()
                            .map(|context| format!("selector:HEAD~{context}"))
                            .collect(),
                        "{lane}.{depth}.{field} must retain its lane-specific snapshot map"
                    );
                } else {
                    assert_eq!(contexts.len(), 1, "{lane}.{depth}.{field} pin binder");
                    assert!(
                        contexts
                            .iter()
                            .all(|context| context.starts_with("selector:binder:")),
                        "{lane}.{depth}.{field} must keep the not-yet-invoked lexical pin: {contexts:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn shadowed_paired_checkpoint_parameters_keep_their_innermost_pins() {
    let source = include_str!("fixtures/historical-paired-shadowed-checkpoint-rebinds.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-shadowed-checkpoint-rebinds.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| module.symbols.contains_key("paired_nested_rebind_checkpoints"))
        .expect("paired nested checkpoint module");
    let Type::Function { result, .. } = &module.symbols["paired_nested_rebind_checkpoints"].ty
    else {
        panic!("paired nested checkpoints must be callable");
    };
    let Type::Record(checkpoints) = result.as_ref() else {
        panic!("paired nested checkpoints must expose both lanes");
    };

    for (lane, expected) in [("left", "selector:HEAD~1"), ("right", "selector:HEAD~2")] {
        let mut contexts = BTreeSet::new();
        collect_snapshot_contexts(
            checkpoints.get(lane).expect("paired lane output"),
            &mut contexts,
        );
        assert_eq!(
            contexts,
            BTreeSet::from([expected.to_owned()]),
            "{lane} output must use its innermost shadowed snapshot parameter"
        );
    }
}
#[test]
fn paired_checkpoint_lists_preserve_field_specific_pin_maps() {
    let source = include_str!("fixtures/historical-paired-checkpoint-field-map-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-checkpoint-field-map-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_checkpoint_field_maps_across_rebind_storms")
        })
        .expect("paired checkpoint field-map module");
    let Type::Function { result, .. } =
        &module.symbols["paired_checkpoint_field_maps_across_rebind_storms"].ty
    else {
        panic!("paired checkpoint field map must be callable");
    };
    let Type::Record(lanes) = result.as_ref() else {
        panic!("paired checkpoint field map must retain both lanes");
    };

    for (lane, expected_maps) in [
        (
            "left",
            [
                (
                    "saved",
                    &["selector:HEAD~840", "selector:HEAD~838"][..],
                    &["selector:HEAD~740", "selector:HEAD~741"][..],
                ),
                (
                    "after_storm",
                    &["selector:HEAD~837", "selector:HEAD~840"][..],
                    &["selector:HEAD~742", "selector:HEAD~743"][..],
                ),
                (
                    "restored",
                    &["selector:HEAD~840", "selector:HEAD~838"][..],
                    &["selector:HEAD~740", "selector:HEAD~741"][..],
                ),
            ],
        ),
        (
            "right",
            [
                (
                    "saved",
                    &["selector:HEAD~830", "selector:HEAD~828"][..],
                    &["selector:HEAD~730", "selector:HEAD~731"][..],
                ),
                (
                    "after_storm",
                    &["selector:HEAD~827", "selector:HEAD~830"][..],
                    &["selector:HEAD~732", "selector:HEAD~733"][..],
                ),
                (
                    "restored",
                    &["selector:HEAD~830", "selector:HEAD~828"][..],
                    &["selector:HEAD~730", "selector:HEAD~731"][..],
                ),
            ],
        ),
    ] {
        let Type::Record(checkpoint_maps) = lanes.get(lane).expect("paired checkpoint lane") else {
            panic!("{lane} must retain its saved, storm, and restored checkpoint maps");
        };
        for (checkpoint_map, roots, children) in expected_maps {
            let Type::List(element) = checkpoint_maps.get(checkpoint_map).expect("checkpoint map")
            else {
                panic!("{lane}.{checkpoint_map} must remain a list");
            };
            let Type::Record(checkpoint) = element.as_ref() else {
                panic!("{lane}.{checkpoint_map} list elements must retain their field map");
            };
            for (field, expected) in [
                ("root_pin", roots),
                ("root", roots),
                ("child_pin", children),
                ("child", children),
            ] {
                let mut contexts = BTreeSet::new();
                collect_snapshot_contexts(
                    checkpoint.get(field).expect("checkpoint field"),
                    &mut contexts,
                );
                assert_eq!(
                    contexts,
                    expected.iter().map(|context| (*context).to_owned()).collect(),
                    "{lane}.{checkpoint_map}.{field} must keep its own snapshot map across rebind waves"
                );
            }
        }
    }
}

#[test]
fn paired_checkpoint_maps_merge_distinct_lambda_binders_without_cross_field_loss() {
    let source = include_str!(
        "fixtures/historical-paired-checkpoint-distinct-binder-field-map-storm.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-checkpoint-distinct-binder-field-map-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_checkpoint_distinct_binder_field_maps_across_storms")
        })
        .expect("paired distinct-binder checkpoint module");
    let Type::Function { result, .. } =
        &module.symbols["paired_checkpoint_distinct_binder_field_maps_across_storms"].ty
    else {
        panic!("paired distinct-binder checkpoints must be callable");
    };
    let Type::Record(lanes) = result.as_ref() else {
        panic!("paired distinct-binder checkpoints must retain both lanes");
    };

    let mut lane_child_maps = BTreeMap::new();
    for (lane, expected_stages) in [
        (
            "left",
            [
                ("saved", &["840", "838"][..]),
                ("after_storm", &["835", "834"][..]),
                ("restored", &["840", "838"][..]),
            ],
        ),
        (
            "right",
            [
                ("saved", &["830", "828"][..]),
                ("after_storm", &["825", "824"][..]),
                ("restored", &["830", "828"][..]),
            ],
        ),
    ] {
        let Type::Record(stages) = lanes.get(lane).expect("paired lane") else {
            panic!("{lane} must retain saved, storm, and restored maps");
        };
        let mut lane_child_map = None;
        for (stage, expected_roots) in expected_stages {
            let fields = checkpoint_output_fields(stages.get(stage).expect("checkpoint stage"));
            let expected_roots = expected_roots
                .iter()
                .map(|root| format!("selector:HEAD~{root}"))
                .collect::<BTreeSet<_>>();
            for field in ["root_pin", "root"] {
                let mut contexts = BTreeSet::new();
                collect_snapshot_contexts(
                    fields.get(field).expect("root checkpoint field"),
                    &mut contexts,
                );
                assert_eq!(
                    contexts, expected_roots,
                    "{lane}.{stage}.{field} must retain only that lane's root map"
                );
            }

            let mut child_contexts = BTreeSet::new();
            for field in ["child_pin", "child"] {
                let mut contexts = BTreeSet::new();
                collect_snapshot_contexts(
                    fields.get(field).expect("child checkpoint field"),
                    &mut contexts,
                );
                assert_eq!(
                    contexts.len(),
                    2,
                    "{lane}.{stage}.{field} must retain both distinct lambda binders: {contexts:?}"
                );
                assert!(
                    contexts
                        .iter()
                        .all(|context| context.starts_with("selector:binder:")),
                    "{lane}.{stage}.{field} must not absorb either root selector: {contexts:?}"
                );
                if field == "child_pin" {
                    child_contexts = contexts;
                } else {
                    assert_eq!(
                        contexts, child_contexts,
                        "{lane}.{stage} child pin and callable fields must preserve the same map"
                    );
                }
            }
            if let Some(expected) = &lane_child_map {
                assert_eq!(
                    &child_contexts, expected,
                    "{lane}.{stage} must retain its original child binder map through storms"
                );
            } else {
                lane_child_map = Some(child_contexts);
            }
        }
        lane_child_maps.insert(lane, lane_child_map.expect("child map"));
    }
    assert_ne!(
        lane_child_maps.get("left"),
        lane_child_maps.get("right"),
        "the two lanes' child binders must remain distinct"
    );
}

#[test]
fn exact_checkpoint_map_equality_returns_boolean_and_real_checkpoint() {
    let source = include_str!("fixtures/historical-checkpoint-map-exact-equality.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-checkpoint-map-exact-equality.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("exact_checkpoint_map_equality_returns_value")
        })
        .expect("exact-equality fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["exact_checkpoint_map_equality_returns_value"]
        .ty
    else {
        panic!("exact map equality fixture must export a function");
    };
    let Type::Record(fields) = result.as_ref() else {
        panic!("exact map equality function must return computed fields");
    };
    assert_eq!(
        fields.get("same_checkpoint"),
        Some(&Type::Bool),
        "equality of a real checkpoint value with its exact type returns Bool"
    );
    let checkpoint = fields
        .get("checkpoint")
        .expect("the computed checkpoint value is returned");
    assert_canonical_snapshot_context_maps(checkpoint);
    let mut contexts = BTreeSet::new();
    collect_snapshot_contexts(checkpoint, &mut contexts);
    assert!(
        !contexts.is_empty(),
        "the returned checkpoint must retain its actual snapshot map"
    );
}

#[test]
fn checkpoint_map_compatible_rebind_returns_each_real_snapshot_value() {
    let source = include_str!(
        "fixtures/historical-checkpoint-map-compatible-rebind-values.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-checkpoint-map-compatible-rebind-values.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("checkpoint_map_compatible_rebind_returns_values")
        })
        .expect("checkpoint-map compatibility fixture module");
    let Type::Function { result, .. } =
        &module.symbols["checkpoint_map_compatible_rebind_returns_values"].ty
    else {
        panic!("checkpoint-map rebind proof must export a function");
    };
    let Type::Record(values) = result.as_ref() else {
        panic!("checkpoint-map rebind must return its computed values");
    };
    for (name, selector) in [
        ("saved", "selector:HEAD~12"),
        ("rolling", "selector:HEAD~10"),
    ] {
        let value = values.get(name).expect("real checkpoint value");
        assert_canonical_snapshot_context_maps(value);
        let mut contexts = BTreeSet::new();
        collect_snapshot_contexts(value, &mut contexts);
        assert_eq!(contexts, BTreeSet::from([selector.to_owned()]), "{name}");
    }
}

#[test]
fn paired_checkpoint_map_shape_rebind_preserves_each_real_selector_set() {
    let source = include_str!(
        "fixtures/historical-paired-checkpoint-map-compatible-shape-rebind.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-checkpoint-map-compatible-shape-rebind.orna",
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
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_checkpoint_map_compatible_shape_rebind_returns_values")
        })
        .expect("paired checkpoint-map shape fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_checkpoint_map_compatible_shape_rebind_returns_values"]
        .ty
    else {
        panic!("paired checkpoint-map rebind proof must export a function");
    };
    let Type::Record(values) = result.as_ref() else {
        panic!("paired checkpoint-map rebind must return its computed fields");
    };
    for (name, expected) in [
        (
            "saved",
            ["selector:HEAD~12", "selector:HEAD~11"],
        ),
        (
            "rolling",
            ["selector:HEAD~10", "selector:HEAD~9"],
        ),
    ] {
        let Type::Record(fields) = values.get(name).expect("checkpoint record value") else {
            panic!("{name} must remain a computed checkpoint record");
        };
        for (field, selector) in [("first", expected[0]), ("second", expected[1])] {
            let value = fields.get(field).expect("computed checkpoint field");
            assert_canonical_snapshot_context_maps(value);
            let mut contexts = BTreeSet::new();
            collect_snapshot_contexts(value, &mut contexts);
            assert_eq!(contexts, BTreeSet::from([selector.to_owned()]), "{name}.{field}");
        }
    }
}

#[test]
fn tuple_checkpoint_pin_identity_survives_map_compaction_rebind() {
    let source = include_str!("fixtures/historical-tuple-checkpoint-map-compaction-rebind.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-checkpoint-map-compaction-rebind.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.is_ok(),
        "tuple pins with the same identity in each slot must survive checkpoint-map compaction and rebind: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("tuple_checkpoint_maps_survive_compaction_rebind")
        })
        .expect("tuple checkpoint compaction fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["tuple_checkpoint_maps_survive_compaction_rebind"]
        .ty
    else {
        panic!("tuple checkpoint compaction proof must return computed values");
    };
    let Type::Record(checkpoints) = result.as_ref() else {
        panic!("tuple checkpoint proof must expose its saved and compacted values");
    };

    let slot_maps = |name: &str| {
        let value = checkpoints.get(name).expect("computed checkpoint value");
        assert_canonical_snapshot_context_maps(value);
        let Type::List(element) = value else {
            panic!("{name} must remain a compacted checkpoint list: {value:?}");
        };
        let Type::Tuple(slots) = element.as_ref() else {
            panic!("{name} checkpoints must preserve tuple slots: {element:?}");
        };
        let maps = slots
            .iter()
            .map(|slot| {
                let mut contexts = BTreeSet::new();
                collect_snapshot_contexts(slot, &mut contexts);
                assert!(!contexts.is_empty(), "{name} slot must retain a real pin map");
                contexts
            })
            .collect::<Vec<_>>();
        assert_eq!(maps.len(), 2, "{name} must preserve both tuple positions");
        assert_eq!(maps[0], maps[1], "{name} must preserve same-pin slot identity");
        maps[0].clone()
    };
    assert_eq!(
        slot_maps("saved"),
        BTreeSet::from(["selector:HEAD~11".into(), "selector:HEAD~12".into()]),
        "saved checkpoint map must retain its actual selectors"
    );
    assert_eq!(
        slot_maps("compacted"),
        BTreeSet::from(["selector:HEAD~10".into(), "selector:HEAD~9".into()]),
        "rebound checkpoint map must retain its computed selectors"
    );
}

#[test]
fn tuple_checkpoint_rebind_rejects_pin_identity_collapse() {
    let source = include_str!("fixtures/historical-tuple-checkpoint-map-rebind-collapse.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-checkpoint-map-rebind-collapse.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "rebind must reject a tuple whose shared pin splits into two selectors: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tuple_checkpoint_map_promotion_stops_at_width_drift() {
    let source = include_str!("fixtures/historical-tuple-checkpoint-map-width-drift.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-checkpoint-map-width-drift.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "width-drifted checkpoint tuples must not be promoted or rebound: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("tuple_checkpoint_promotion_is_suppressed_on_width_drift")
        })
        .expect("tuple checkpoint width-drift fixture module");
    let Type::Function { result, .. } = &module.symbols
        ["tuple_checkpoint_promotion_is_suppressed_on_width_drift"]
        .ty
    else {
        panic!("tuple width-drift proof must return computed values");
    };
    let Type::Record(values) = result.as_ref() else {
        panic!("tuple width-drift proof must expose saved, retained and candidate values");
    };

    let slot_maps = |name: &str| {
        let value = values.get(name).expect("computed checkpoint value");
        assert_canonical_snapshot_context_maps(value);
        let Type::List(element) = value else {
            panic!("{name} must remain a checkpoint list: {value:?}");
        };
        let Type::Tuple(slots) = element.as_ref() else {
            panic!("{name} must retain its tuple element: {element:?}");
        };
        assert_eq!(slots.len(), 2, "{name} must preserve both tuple positions");
        slots
            .iter()
            .map(|slot| {
                let mut contexts = BTreeSet::new();
                collect_snapshot_contexts(slot, &mut contexts);
                assert!(!contexts.is_empty(), "{name} must retain actual selector values");
                contexts
            })
            .collect::<Vec<_>>()
    };

    let saved = slot_maps("saved");
    let retained = slot_maps("checkpoint");
    let candidate = slot_maps("candidate");
    assert_eq!(
        saved,
        vec![
            BTreeSet::from([
                "selector:HEAD~86".into(),
                "selector:HEAD~87".into(),
                "selector:HEAD~89".into(),
                "selector:HEAD~90".into(),
            ]),
            BTreeSet::from(["selector:HEAD~85".into(), "selector:HEAD~88".into()]),
        ],
        "saved checkpoint must retain only its computed selector maps"
    );
    assert_eq!(
        retained, saved,
        "a failed width-drift rebind must leave the old map intact"
    );
    assert_eq!(
        candidate,
        vec![
            BTreeSet::from(["selector:HEAD~79".into(), "selector:HEAD~80".into()]),
            BTreeSet::from(["selector:HEAD~78".into()]),
        ],
        "candidate inference must stop at the first tuple width instead of promoting later pins"
    );
}

#[test]
fn tuple_checkpoint_compaction_fold_preserves_slot_pin_identity() {
    let source = include_str!("fixtures/historical-tuple-checkpoint-cross-slot-fold.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-checkpoint-cross-slot-fold.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "compaction must reject a fold that invents cross-slot pin identities: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
    let module = result
        .modules
        .values()
        .find(|module| module.symbols.contains_key("tuple_checkpoint_cross_slot_compaction_fold"))
        .expect("tuple checkpoint cross-slot fold fixture module");
    let Type::Function { result, .. } =
        &module.symbols["tuple_checkpoint_cross_slot_compaction_fold"].ty
    else {
        panic!("tuple checkpoint fold proof must return computed values");
    };
    let Type::Record(values) = result.as_ref() else {
        panic!("tuple checkpoint fold proof must expose saved and folded values");
    };

    let slot_maps = |name: &str| {
        let value = values.get(name).expect("computed checkpoint value");
        assert_canonical_snapshot_context_maps(value);
        let Type::List(element) = value else {
            panic!("{name} must remain a checkpoint list: {value:?}");
        };
        let Type::Tuple(slots) = element.as_ref() else {
            panic!("{name} must retain its tuple element: {element:?}");
        };
        slots
            .iter()
            .map(|slot| {
                let mut contexts = BTreeSet::new();
                collect_snapshot_contexts(slot, &mut contexts);
                assert!(!contexts.is_empty(), "{name} must retain actual selector values");
                contexts
            })
            .collect::<Vec<_>>()
    };

    let expected = vec![
        BTreeSet::from(["selector:HEAD~20".into()]),
        BTreeSet::from(["selector:HEAD~21".into()]),
    ];
    assert_eq!(slot_maps("saved"), expected, "saved tuple pins stay slot-local");
    assert_eq!(
        slot_maps("folded"),
        expected,
        "a rejected width fold must not union opposite-slot pins"
    );
}

#[test]
fn tuple_checkpoint_label_fold_rebind_keeps_computed_pin_maps() {
    let source = include_str!("fixtures/historical-tuple-checkpoint-label-fold.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-tuple-checkpoint-label-fold.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "a folded rebind with changed selector-label membership must fail: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("tuple_checkpoint_label_fold_rebind")
        })
        .expect("tuple checkpoint label-fold fixture module");
    let Type::Function { result, .. } =
        &module.symbols["tuple_checkpoint_label_fold_rebind"].ty
    else {
        panic!("tuple checkpoint label-fold proof must return computed values");
    };
    let Type::Record(values) = result.as_ref() else {
        panic!("tuple checkpoint proof must expose saved, retained and candidate values");
    };

    let slot_maps = |name: &str| {
        let value = values.get(name).expect("computed checkpoint value");
        assert_canonical_snapshot_context_maps(value);
        let Type::List(element) = value else {
            panic!("{name} must remain a checkpoint list: {value:?}");
        };
        let Type::Tuple(slots) = element.as_ref() else {
            panic!("{name} must retain its tuple element: {element:?}");
        };
        assert_eq!(slots.len(), 3, "{name} must preserve all tuple slots");
        slots
            .iter()
            .map(|slot| {
                let mut selectors = BTreeSet::new();
                collect_snapshot_contexts(slot, &mut selectors);
                selectors.retain(|selector| selector.starts_with("selector:HEAD~"));
                assert!(
                    !selectors.is_empty(),
                    "{name} must expose its computed selector map: {slot:?}"
                );
                selectors
            })
            .collect::<Vec<_>>()
    };

    let saved = slot_maps("saved");
    assert_eq!(
        saved,
        vec![
            BTreeSet::from(["selector:HEAD~30".into(), "selector:HEAD~31".into()]),
            BTreeSet::from(["selector:HEAD~30".into(), "selector:HEAD~32".into()]),
            BTreeSet::from(["selector:HEAD~31".into(), "selector:HEAD~32".into()]),
        ],
        "saved folded maps must have one distinct shared label per slot pair"
    );
    assert_eq!(
        slot_maps("retained"),
        saved,
        "failed label-topology rebind must retain saved computed maps"
    );
    assert_eq!(
        slot_maps("candidate"),
        vec![
            BTreeSet::from(["selector:HEAD~40".into(), "selector:HEAD~41".into()]),
            BTreeSet::from(["selector:HEAD~40".into(), "selector:HEAD~42".into()]),
            BTreeSet::from(["selector:HEAD~40".into(), "selector:HEAD~43".into()]),
        ],
        "candidate folded maps must expose the new shared all-slot label"
    );
}

#[test]
fn pinned_callable_map_width_rebind_preserves_computed_values() {
    let source = include_str!("fixtures/historical-pinned-map-width-rebind-values.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-map-width-rebind-values.orna",
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
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("pinned_map_width_rebind_returns_values")
        })
        .expect("pinned-map-width fixture module");
    let Type::Function { result, .. } =
        &module.symbols["pinned_map_width_rebind_returns_values"].ty
    else {
        panic!("pinned-map-width rebind proof must export a function");
    };
    let Type::Record(stages) = result.as_ref() else {
        panic!("pinned-map-width rebind must return computed values");
    };
    for (stage, expected) in [
        ("saved", ["selector:HEAD~11", "selector:HEAD~12"]),
        ("rolling", ["selector:HEAD~9", "selector:HEAD~10"]),
    ] {
        let value = stages.get(stage).expect("computed checkpoint stage");
        assert_canonical_snapshot_context_maps(value);
        let Type::List(_) = value else {
            panic!("{stage} must remain a computed checkpoint list: {value:?}");
        };
        let mut contexts = BTreeSet::new();
        collect_snapshot_contexts(value, &mut contexts);
        assert_eq!(
            contexts,
            expected.map(str::to_owned).into_iter().collect(),
            "{stage} must retain a two-selector pin map with its actual selectors"
        );
    }
}

#[test]
fn pinned_callable_map_width_growth_is_rejected_on_rebind() {
    let source = include_str!("fixtures/historical-pinned-map-width-mismatch-rebind.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-pinned-map-width-mismatch-rebind.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_TYPE
                && diagnostic.message() == "static types are incompatible"
        }),
        "a three-selector pinned callable must not rebind into a two-selector slot: {:?}",
        result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn paired_checkpoint_snapshot_maps_survive_chained_depth_storms() {
    let source = include_str!(
        "fixtures/historical-paired-checkpoint-snapshot-retention-depth-storm.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-checkpoint-snapshot-retention-depth-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_checkpoint_snapshot_retention_across_depth_storms")
        })
        .expect("paired depth-storm snapshot-retention module");
    let Type::Function { result, .. } =
        &module.symbols["paired_checkpoint_snapshot_retention_across_depth_storms"].ty
    else {
        panic!("paired checkpoint retention must be callable");
    };
    let Type::Record(lanes) = result.as_ref() else {
        panic!("paired checkpoint retention must retain both lanes");
    };

    let mut retained_lane_maps = BTreeMap::new();
    for (lane, stages) in [
        (
            "left",
            [
                ("saved", &["950", "948"][..]),
                ("after_storm", &["945", "944"][..]),
                ("restored", &["950", "948"][..]),
            ],
        ),
        (
            "right",
            [
                ("saved", &["930", "928"][..]),
                ("after_storm", &["925", "924"][..]),
                ("restored", &["930", "928"][..]),
            ],
        ),
    ] {
        let Type::Record(checkpoints) = lanes.get(lane).expect("paired lane") else {
            panic!("{lane} must retain all checkpoint stages");
        };
        let mut field_maps = BTreeMap::new();
        for (stage, roots) in stages {
            let fields = checkpoint_output_fields(checkpoints.get(stage).expect("stage"));
            let expected_roots = roots
                .iter()
                .map(|root| format!("selector:HEAD~{root}"))
                .collect::<BTreeSet<_>>();
            for field in ["root_pin", "root"] {
                let mut contexts = BTreeSet::new();
                collect_snapshot_contexts(
                    fields.get(field).expect("root field"),
                    &mut contexts,
                );
                assert_eq!(contexts, expected_roots, "{lane}.{stage}.{field} root map");
            }
            for field in ["middle_pin", "middle", "leaf_pin", "leaf"] {
                let mut contexts = BTreeSet::new();
                collect_snapshot_contexts(
                    fields.get(field).expect("nested checkpoint field"),
                    &mut contexts,
                );
                assert_eq!(
                    contexts.len(),
                    2,
                    "{lane}.{stage}.{field} must retain both nested snapshot binders: {contexts:?}"
                );
                assert!(
                    contexts
                        .iter()
                        .all(|context| context.starts_with("selector:binder:")),
                    "{lane}.{stage}.{field} must not absorb the root snapshot map: {contexts:?}"
                );
                if let Some(expected) = field_maps.get(field) {
                    assert_eq!(
                        &contexts, expected,
                        "{lane}.{stage}.{field} pin map must survive rebind and restore"
                    );
                } else {
                    field_maps.insert(field.to_owned(), contexts);
                }
            }
            assert_eq!(
                field_maps.get("middle_pin"),
                field_maps.get("middle"),
                "{lane}.{stage} middle pin and historical field maps must agree"
            );
            assert_eq!(
                field_maps.get("leaf_pin"),
                field_maps.get("leaf"),
                "{lane}.{stage} leaf pin and historical field maps must agree"
            );
        }
        retained_lane_maps.insert(lane, field_maps);
    }
    assert_ne!(
        retained_lane_maps
            .get("left")
            .and_then(|maps| maps.get("middle_pin")),
        retained_lane_maps
            .get("right")
            .and_then(|maps| maps.get("middle_pin")),
        "paired lanes must retain separate nested snapshot maps"
    );
}

#[test]
fn paired_checkpoint_retains_merged_snapshot_maps_through_rebind_storms() {
    let source = include_str!("fixtures/historical-paired-checkpoint-retained-maps-rebind-storm.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-checkpoint-retained-maps-rebind-storm.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_checkpoint_retained_maps_across_rebind_storms")
        })
        .expect("paired retained-map checkpoint module");
    let Type::Function { result, .. } =
        &module.symbols["paired_checkpoint_retained_maps_across_rebind_storms"].ty
    else {
        panic!("paired retained-map checkpoint must be callable");
    };
    let Type::Record(lanes) = result.as_ref() else {
        panic!("paired retained-map checkpoint must expose both lanes");
    };

    let mut lane_maps = BTreeMap::new();
    for lane in ["left", "right"] {
        let Type::Record(stages) = lanes.get(lane).expect("paired lane") else {
            panic!("{lane} must retain saved, storm, and restored maps");
        };
        let mut stage_maps = BTreeMap::new();
        for stage in ["saved", "after_storm", "restored"] {
            let mut contexts = BTreeSet::new();
            collect_snapshot_contexts(
                stages.get(stage).expect("checkpoint stage"),
                &mut contexts,
            );
            assert_eq!(
                contexts.len(),
                2,
                "{lane}.{stage} must retain both function-parameter map entries: {contexts:?}"
            );
            assert!(
                contexts
                    .iter()
                    .all(|context| context.starts_with("selector:binder:")),
                "{lane}.{stage} must retain binder maps, not substitute other snapshots: {contexts:?}"
            );
            stage_maps.insert(stage, contexts);
        }
        assert_eq!(
            stage_maps.get("saved"),
            stage_maps.get("restored"),
            "{lane} restoring a checkpoint must restore its original field map"
        );
        assert_ne!(
            stage_maps.get("saved"),
            stage_maps.get("after_storm"),
            "{lane} storm maps must retain their own snapshot identities"
        );
        lane_maps.insert(lane, stage_maps);
    }
    assert_ne!(
        lane_maps["left"]["saved"],
        lane_maps["right"]["saved"],
        "paired lanes must not collapse retained checkpoint maps"
    );
}

#[test]
fn paired_depth_storms_retain_field_maps_at_each_rebound_depth() {
    let source = include_str!(
        "fixtures/historical-paired-checkpoint-depth-storm-chained-rebind.orna"
    );
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-checkpoint-depth-storm-chained-rebind.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules
        .values()
        .find(|module| {
            module
                .symbols
                .contains_key("paired_checkpoint_depth_storm_chained_rebind_retains_maps")
        })
        .expect("paired depth-storm checkpoint module");
    let Type::Function { result, .. } = &module.symbols
        ["paired_checkpoint_depth_storm_chained_rebind_retains_maps"]
        .ty
    else {
        panic!("paired depth-storm checkpoint must be callable");
    };
    let Type::Record(lanes) = result.as_ref() else {
        panic!("paired depth-storm checkpoint must expose both lanes");
    };
    assert_canonical_snapshot_context_maps(result.as_ref());

    let cases: [
        (
            &str,
            &str,
            &str,
            &[&str],
            Option<&[&str]>,
            Option<&[&str]>,
        );
        18
    ] = [
        ("left", "root", "saved", &["950", "948"], None, None),
        (
            "left",
            "root",
            "after_storm",
            &["945", "944"],
            None,
            None,
        ),
        ("left", "root", "restored", &["950", "948"], None, None),
        (
            "left",
            "middle",
            "saved",
            &["950", "948"],
            Some(&["850", "848"]),
            None,
        ),
        (
            "left",
            "middle",
            "after_storm",
            &["945", "944"],
            Some(&["845", "844"]),
            None,
        ),
        (
            "left",
            "middle",
            "restored",
            &["950", "948"],
            Some(&["850", "848"]),
            None,
        ),
        (
            "left",
            "leaf",
            "saved",
            &["950", "948"],
            Some(&["850", "848"]),
            Some(&["750", "748"]),
        ),
        (
            "left",
            "leaf",
            "after_storm",
            &["945", "944"],
            Some(&["845", "844"]),
            Some(&["745", "744"]),
        ),
        (
            "left",
            "leaf",
            "restored",
            &["950", "948"],
            Some(&["850", "848"]),
            Some(&["750", "748"]),
        ),
        ("right", "root", "saved", &["930", "928"], None, None),
        (
            "right",
            "root",
            "after_storm",
            &["925", "924"],
            None,
            None,
        ),
        ("right", "root", "restored", &["930", "928"], None, None),
        (
            "right",
            "middle",
            "saved",
            &["930", "928"],
            Some(&["830", "828"]),
            None,
        ),
        (
            "right",
            "middle",
            "after_storm",
            &["925", "924"],
            Some(&["825", "824"]),
            None,
        ),
        (
            "right",
            "middle",
            "restored",
            &["930", "928"],
            Some(&["830", "828"]),
            None,
        ),
        (
            "right",
            "leaf",
            "saved",
            &["930", "928"],
            Some(&["830", "828"]),
            Some(&["730", "728"]),
        ),
        (
            "right",
            "leaf",
            "after_storm",
            &["925", "924"],
            Some(&["825", "824"]),
            Some(&["725", "724"]),
        ),
        (
            "right",
            "leaf",
            "restored",
            &["930", "928"],
            Some(&["830", "828"]),
            Some(&["730", "728"]),
        ),
    ];

    for (lane, depth, stage, roots, middles, leaves) in cases {
        let Type::Record(stages) = lanes.get(lane).expect("paired lane") else {
            panic!("{lane} must preserve each checkpoint depth");
        };
        let Type::Record(depths) = stages.get(depth).expect("checkpoint depth") else {
            panic!("{lane}.{depth} must retain stage maps");
        };
        let fields = checkpoint_output_fields(depths.get(stage).expect("checkpoint stage"));
        for (field, expected) in [
            ("root_pin", Some(roots)),
            ("root", Some(roots)),
            ("middle_pin", middles),
            ("middle", middles),
            ("leaf_pin", leaves),
            ("leaf", leaves),
        ] {
            let mut contexts = BTreeSet::new();
            collect_snapshot_contexts(fields.get(field).expect("checkpoint field"), &mut contexts);
            if let Some(expected) = expected {
                assert_eq!(
                    contexts,
                    expected
                        .iter()
                        .map(|selector| format!("selector:HEAD~{selector}"))
                        .collect(),
                    "{lane}.{depth}.{stage}.{field} must retain its selected snapshot map"
                );
            } else {
                assert_eq!(
                    contexts.len(),
                    2,
                    "{lane}.{depth}.{stage}.{field} must retain both nested binders: {contexts:?}"
                );
                assert!(
                    contexts
                        .iter()
                        .all(|context| context.starts_with("selector:binder:")),
                    "{lane}.{depth}.{stage}.{field} must remain isolated from selected maps: {contexts:?}"
                );
            }
        }
        for (pin_field, value_field) in [
            ("root_pin", "root"),
            ("middle_pin", "middle"),
            ("leaf_pin", "leaf"),
        ] {
            let mut pin_contexts = BTreeSet::new();
            collect_snapshot_contexts(
                fields.get(pin_field).expect("snapshot pin field"),
                &mut pin_contexts,
            );
            let mut value_contexts = BTreeSet::new();
            collect_snapshot_contexts(
                fields.get(value_field).expect("historical value field"),
                &mut value_contexts,
            );
            assert_eq!(
                pin_contexts, value_contexts,
                "{lane}.{depth}.{stage}.{pin_field} and {value_field} must share one map"
            );
        }
    }
}

#[test]
fn paired_reproduction_checkpoint_types_stay_stable_across_interleaved_analyses() {
    const FUNCTION: &str =
        "paired_reproductions_remain_stable_across_chained_storm_orders";
    let paired = include_str!("fixtures/historical-paired-reproduction-stability-roundtrip.orna");
    let mixed =
        include_str!("fixtures/historical-paired-reproduction-stability-roundtrip-mixed.orna");
    let catalogue = historical_nested_callable_catalogue();
    let analyze_fixture = |path, source| {
        analyze_with_catalogue(&[ModuleInput::new(path, source)], &catalogue)
    };
    let checkpoint_type = |analysis: &orna_semantic_v1::Analysis| {
        analysis
            .modules
            .values()
            .find_map(|module| module.symbols.get(FUNCTION))
            .expect("paired reproduction function")
            .ty
            .clone()
    };

    let first = analyze_fixture("paired-reproduction.orna", paired);
    assert!(first.is_ok(), "{:?}", first.diagnostics);
    let first_checkpoint_type = checkpoint_type(&first);

    let mixed_result = analyze_fixture("paired-reproduction-mixed.orna", mixed);
    assert!(
        mixed_result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "cross-lane checkpoint mixing must remain rejected: {:?}",
        mixed_result
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );

    let repeated = analyze_fixture("paired-reproduction.orna", paired);
    assert!(repeated.is_ok(), "{:?}", repeated.diagnostics);
    assert_eq!(
        first_checkpoint_type,
        checkpoint_type(&repeated),
        "rejected cross-lane analysis must not change either lane's checkpoint identity"
    );
}

#[test]
fn paired_reproductions_reject_cross_lane_after_alternating_storms() {
    let source = include_str!("fixtures/historical-paired-reproduction-stability-roundtrip-mixed.orna");
    let parsed = orna_syntax_v1::parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "historical-paired-reproduction-stability-roundtrip-mixed.orna",
            source,
        )],
        &historical_nested_callable_catalogue(),
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "terminal outputs from repeated paired chains must retain separate pin identities after chained rebind storms: {:?}",
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
