use super::*;

#[test]
fn version_one_keeps_the_type_only_contract_without_executable_facts() {
    let verified = verified_standard_library_for_relational_test();
    let checked = check_standard_library_source(&verified).unwrap();
    assert!(checked.checked_executable().is_none());
    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.value_types().len(), 1);
    assert_eq!(checked.type_bindings().len(), 2);
}

#[test]
fn rejects_v1_source_unit_identity_mutations() {
    let verified = verified_standard_library_for_relational_test();
    assert!(check_standard_library_source(&verified).is_ok());
    let stored = &verified.source().units()[0];

    for (label, id, logical_path) in [
        (
            "stable source-unit id",
            SourceUnitId::from_bytes([0x55; 16]),
            stored.logical_path(),
        ),
        ("logical path", stored.id(), "std/renamed.orna"),
    ] {
        let mutated = verified_v1_with_source_unit_identity(&verified, id, logical_path, 0);
        let error = check_standard_library_source(&mutated).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: unexpected rejection: {error}"
        );
    }

    let ordinal = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        1,
        stored.logical_path(),
        stored.content(),
        stored.content_hash(),
    )
    .unwrap();
    assert!(matches!(
        check_standard_library_source_v1_identity(&ordinal),
        Err(StandardLibraryCheckError::SourceMismatch)
    ));
}

fn verified_v1_with_source_unit_identity(
    verified: &VerifiedStandardLibrarySnapshot,
    id: SourceUnitId,
    logical_path: &str,
    ordinal: u32,
) -> VerifiedStandardLibrarySnapshot {
    let stored = &verified.source().units()[0];
    let unit = StoredSourceUnit::new(
        id,
        ordinal,
        logical_path,
        stored.content(),
        stored.content_hash(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        verified.source().bundle(),
        verified.source().id(),
        verified.source().parent(),
        vec![unit],
        bundle_hash,
        source_revision_record_digest(
            verified.source().bundle(),
            verified.source().parent(),
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap();
    let origins = verified
        .origins()
        .iter()
        .map(|origin| {
            let source_origin = origin.source();
            let source_unit = if source_origin.source_unit() == stored.id() {
                id
            } else {
                source_origin.source_unit()
            };
            DefinitionOrigin::new(
                origin.identity(),
                SourceOrigin::new(
                    source_unit,
                    source_origin.byte_start(),
                    source_origin.byte_end(),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let provisional = StandardLibrarySnapshot::new(
        verified.revision(),
        verified.digest_version(),
        source,
        verified.language_version(),
        verified.catalogue().clone(),
        origins,
        Sha256Digest::from_bytes([0; 32]),
    )
    .unwrap();
    let digest = calculate_standard_library_digest(&provisional).unwrap();
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            provisional.revision(),
            provisional.digest_version(),
            provisional.source().clone(),
            provisional.language_version(),
            provisional.catalogue().clone(),
            provisional.origins().to_vec(),
            digest,
        )
        .unwrap(),
    )
    .unwrap()
}
