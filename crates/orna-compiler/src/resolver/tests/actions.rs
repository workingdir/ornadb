use super::*;

#[test]
fn legacy_action_call_fails_closed() {
    let standard = check_standard_library_source(&verified_standard_library_with_action_for_test())
        .unwrap();
    let base = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS std.Action AS std.action.call(target => tasks.run, arguments => std.call.args());";
    let report = check_standard_application(&bundle([("action.orna", source)]), &context);
    assert!(report.diagnostics().iter().any(|diagnostic| {
        diagnostic.code() == DiagnosticCode::UnknownQualifiedName
            && diagnostic.message() == "unknown CLIENT function std.action.call"
    }));
    assert!(report.preparation_view().is_none());
}

#[test]
fn rejects_actions_in_client_state_returns_before_preparation() {
    let target_id = FunctionId::from_bytes([0x51; 16]);
    let argument_type = ResolvedType::Scalar(StandardScalar::Integer);
    let base = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x53; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x54; 16]),
            QualifiedSemanticName::new(["tasks"]).unwrap(),
        )],
        Vec::new(),
        vec![FunctionDefinition::new(
            target_id,
            QualifiedSemanticName::new(["tasks", "run"]).unwrap(),
            FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(argument_type),
            FunctionRevisionId::from_bytes([0x55; 16]),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Immutable,
        )],
    )
    .unwrap();
    let standard =
        check_standard_library_source(&verified_standard_library_with_action_for_test()).unwrap();
    let context = StandardApplicationCheckContext::try_new(&base, &standard).unwrap();
    let source = "CREATE SCHEMA ui; CREATE CLIENT FUNCTION ui.run() RETURNS std.Action IS \
            STATE ready INTEGER; \
            BEGIN RETURN std.action.call(target => tasks.run, arguments => std.call.args()); END;";
    let report = check_standard_application(&bundle([("state-action.orna", source)]), &context);
    assert_eq!(report.diagnostics().len(), 1, "{:?}", report.diagnostics());
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::DomainIncompatible,
        "{:?}",
        report.diagnostics()
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "CLIENT state blocks do not support action expressions"
    );
    assert!(report.preparation_view().is_none());
}


#[test]
fn checked_opaque_standard_remains_definition_only_for_applications() {
    let snapshot = verified_standard_library_with_opaque_for_test();
    let standard = check_standard_library_source(&snapshot).unwrap();
    assert_eq!(standard.value_types().len(), 2);
    assert_eq!(standard.value_types()[0].kind(), ValueTypeKind::Primitive);
    assert_eq!(standard.value_types()[1].kind(), ValueTypeKind::Opaque);
    assert_eq!(
        standard.value_types()[1].representation_contract(),
        "std.token@1"
    );
    let application = empty_catalogue();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();

    let source = "CREATE SCHEMA app;CREATE TYPE app.item AS OBJECT (token std.TOKEN NOT NULL);";
    let report = check_standard_application(&bundle([("opaque-use.orna", source)]), &context);
    assert!(report.checked_bundle().is_none());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "unknown type name std.token"
    );
}


pub(super) fn empty_version_two_active(
    standard: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> ActiveDatabaseRevision {
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x41; 16]),
        0,
        "active.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x42; 16]),
        SourceRevisionId::from_bytes([0x43; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x42; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        context,
    )
    .unwrap()
}

pub(super) fn active_from_prepared(prepared: &DeployableRevision) -> ActiveDatabaseRevision {
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            prepared.candidate_pair(),
            prepared.source().clone(),
            prepared.candidate().clone(),
            prepared.catalogue_hash(),
            ActiveRevisionContent::new(
                prepared.expressions().to_vec(),
                prepared
                    .current_function_revisions()
                    .map_or_else(Vec::new, ToOwned::to_owned),
                prepared.origins().to_vec(),
                prepared.references().to_vec(),
            ),
        ),
        prepared.catalogue_hash_context().clone(),
    )
    .unwrap()
}

pub(super) fn expression_use<'a>(
    uses: &[&'a CheckedApplicationTypeUse],
    ordinal: u32,
) -> &'a CheckedApplicationTypeUse {
    let matches = uses
        .iter()
        .copied()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Expression {
                    ordinal: candidate,
                    ..
                } if candidate == ordinal
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected expression ordinal {ordinal}");
    matches[0]
}

pub(super) fn result_use<'a>(
    uses: &[&'a CheckedApplicationTypeUse],
    ordinal: u32,
) -> &'a CheckedApplicationTypeUse {
    let matches = uses
        .iter()
        .copied()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Result {
                    ordinal: candidate,
                    ..
                } if candidate == ordinal
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected result ordinal {ordinal}");
    matches[0]
}

pub(super) fn assert_type_use_span(type_use: &CheckedApplicationTypeUse, start: usize, text: &str) {
    assert_eq!(type_use.location().span().start(), start);
    assert_eq!(type_use.location().span().end(), start + text.len());
}

pub(super) fn checked_use_index(
    uses: &[CheckedApplicationTypeUse],
    kind: CheckedTypeUseKind,
    start: usize,
    end: usize,
) -> usize {
    let matches = uses
        .iter()
        .enumerate()
        .filter(|(_, type_use)| {
            type_use.kind() == kind
                && type_use.location().span().start() == start
                && type_use.location().span().end() == end
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected one exact arena use");
    matches[0]
}
