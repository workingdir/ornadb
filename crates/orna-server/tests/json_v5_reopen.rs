#![cfg(unix)]

use orna_core::{
    canonical_hash::{
        catalogue_digest_with_context, source_bundle_digest, source_revision_record_digest,
    },
    catalogue::CatalogueSnapshot,
    invocation::{
        InvocationArgument, InvocationCallerContext, InvocationCallerKind, InvocationClientOffer,
        InvocationEventBody, InvocationEventKind, InvocationParameterSelector,
        InvocationTarget as InvocationRequestTarget, InvocationTracePolicy, InvokeRequest,
        InvokeRequestInput, InvokeValue,
    },
    revision::{
        ActiveDatabaseRevision, CatalogueHashContext, DeployableRevision,
        DeployableRevisionContent, DeployableRevisionInput, StoredSourceRevision,
        VerifiedStandardLibrarySnapshot,
    },
    security::{
        ExecuteGrant, LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus,
        SecurityFunctionTarget, SecuritySnapshot,
    },
    value::{OpaqueValue, RuntimeValue},
    CatalogueRevisionId, SourceBundleId, SourceRevisionId,
};
use orna_postgres::{PostgresKernel, SealedInvocationResult};
use orna_protocol::encode_invoke_request;
use orna_standard::{
    registered_opaque_codecs, retained_standard_library_snapshot,
    retained_standard_library_v2_snapshot, retained_standard_library_v3_snapshot,
    retained_standard_library_v5_snapshot, verify_standard_library_snapshot,
    verify_standard_library_v2_snapshot, verify_standard_library_v3_snapshot,
    verify_standard_library_v5_snapshot, BYTE_STREAM_MAGIC, JSON_MAGIC,
    STANDARD_LIBRARY_V5_REVISION_ID, STD_INVOKE_ECHO_FUNCTION_ID,
    STD_INVOKE_ECHO_FUNCTION_REVISION_ID, STD_IO_BYTE_STREAM_TYPE_ID, STD_JSON_ENCODE_FUNCTION_ID,
    STD_JSON_ENCODE_FUNCTION_REVISION_ID, STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_VALUE_TYPE_ID,
};

#[path = "../../orna-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use postgres_test_support::{failure, with_test_database, TestDatabase, TestResult};

const JSON_USER: orna_core::PrincipalId = orna_core::PrincipalId::from_bytes([0x7a; 16]);

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn v5_json_encode_survives_reopen_with_exact_retained_identity() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let installed = install_v5_standard(&kernel, &empty, &database).await?;
        let expected =
            verify_standard_library_v5_snapshot(retained_standard_library_v5_snapshot()?)?;
        assert_v5_identity(&installed, &expected)?;
        drop(kernel);

        let reopened_kernel = PostgresKernel::new(database.config()?);
        let recovered = reopened_kernel.recover().await?;
        require(
            recovered.pair() == installed.pair(),
            "reopening the V5 database changed the active revision pair",
        )?;
        assert_v5_identity(&recovered, &expected)?;

        let standard = recovered
            .catalogue_hash_context()
            .standard()
            .ok_or_else(|| failure("recovery did not retain a selected V5 standard snapshot"))?;
        let registry = registered_opaque_codecs(standard)?;
        let body = br#"{"items":[1,2],"ok":true}"#;
        let mut json_payload = Vec::from(JSON_MAGIC.as_bytes());
        json_payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        json_payload.extend_from_slice(body);
        let json_value =
            OpaqueValue::new(&recovered, &registry, STD_JSON_VALUE_TYPE_ID, &json_payload)?;

        let mut expected_payload = Vec::from(BYTE_STREAM_MAGIC.as_bytes());
        expected_payload.extend_from_slice(&16_u32.to_be_bytes());
        expected_payload.extend_from_slice(b"application/json");
        expected_payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
        expected_payload.extend_from_slice(body);

        let uid = nix::unistd::geteuid().as_raw();
        let security = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
            recovered.pair(),
            vec![
                SecurityFunctionTarget::verified_standard(
                    STD_INVOKE_ECHO_FUNCTION_ID,
                    expected.revision(),
                    STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
                ),
                SecurityFunctionTarget::verified_standard(
                    STD_JSON_ENCODE_FUNCTION_ID,
                    expected.revision(),
                    STD_JSON_ENCODE_FUNCTION_REVISION_ID,
                ),
            ],
            vec![Principal::new(
                JSON_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![ExecuteGrant::new(JSON_USER, STD_JSON_ENCODE_FUNCTION_ID)],
            vec![LocalPeerCredential::new(uid, JSON_USER)],
        )?;
        reopened_kernel.replace_security_snapshot(&security).await?;
        let session = reopened_kernel.authenticate_local_peer(uid).await?;
        let request = sealed_json_encode_request(json_value)?;
        let retained = encode_invoke_request(&recovered, &registry, &request)?;
        let result = reopened_kernel
            .dispatch_sealed_sys_invoke(&session, 5, &retained)
            .await?;
        assert_json_encode_payload(&result, &expected_payload)
    })
    .await
}

fn assert_v5_identity(
    active: &ActiveDatabaseRevision,
    expected: &VerifiedStandardLibrarySnapshot,
) -> TestResult<()> {
    let selected = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| failure("active revision has no standard snapshot"))?;
    require(
        selected.revision() == STANDARD_LIBRARY_V5_REVISION_ID,
        "active standard is not V5",
    )?;
    require(
        selected.revision() == expected.revision(),
        "V5 standard revision identity changed",
    )?;
    require(
        selected.catalogue().revision() == expected.catalogue().revision()
            && selected.source() == expected.source()
            && selected.digest_version() == expected.digest_version()
            && selected.digest() == expected.digest(),
        "V5 catalogue, source, or digest identity changed after recovery",
    )?;
    let json_type = selected
        .catalogue()
        .type_definition_by_id(STD_JSON_VALUE_TYPE_ID)
        .and_then(|definition| definition.as_value())
        .ok_or_else(|| failure("recovered V5 catalogue is missing std.json.Value"))?;
    require(
        json_type.representation_contract() == "orna.std.value.json@1",
        "recovered std.json.Value codec contract changed",
    )?;
    let encode = selected
        .catalogue()
        .function_by_id(STD_JSON_ENCODE_FUNCTION_ID)
        .ok_or_else(|| failure("recovered V5 catalogue is missing std.json.encode"))?;
    require(
        encode.current_revision() == STD_JSON_ENCODE_FUNCTION_REVISION_ID
            && encode
                .parameter_by_id(STD_JSON_ENCODE_PARAMETER_ID)
                .is_some(),
        "recovered std.json.encode identity changed",
    )?;
    Ok(())
}

async fn install_v5_standard(
    kernel: &PostgresKernel,
    empty: &ActiveDatabaseRevision,
    database: &TestDatabase,
) -> TestResult<ActiveDatabaseRevision> {
    seed_standard_source_chain(database).await?;
    let snapshot = verify_standard_library_v3_snapshot(retained_standard_library_v3_snapshot()?)?;
    let candidate = v3_standard_upgrade_candidate(empty, &snapshot)?;
    let version_three = kernel
        .apply_test_standard_upgrade(&candidate, &snapshot)
        .await?;
    let upgrade_v4 = orna_standard::prepare_standard_upgrade_v3_to_v4(&version_three)?;
    let version_four = kernel.apply_standard_upgrade(&upgrade_v4).await?;
    let upgrade_v5 = orna_standard::prepare_standard_upgrade_v4_to_v5(&version_four)?;
    Ok(kernel.apply_standard_upgrade(&upgrade_v5).await?)
}

async fn seed_standard_source_chain(database: &TestDatabase) -> TestResult<()> {
    let version_one = verify_standard_library_snapshot(retained_standard_library_snapshot()?)?;
    let version_two =
        verify_standard_library_v2_snapshot(retained_standard_library_v2_snapshot()?)?;
    let sources = [version_one.source(), version_two.source()];
    let session = database.open().await?;
    for source in sources {
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_bundles
                    (id, content_hash, hash_algorithm, hash_contract_version)
                 VALUES ($1, $2, 'sha256', 1)",
                &[
                    &source.bundle().to_bytes().to_vec(),
                    &source.bundle_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
    }
    for source in sources {
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.source_revisions
                    (id, parent_source_revision_id, bundle_id, content_hash,
                     hash_algorithm, hash_contract_version)
                 VALUES ($1, $2, $3, $4, 'sha256', 1)",
                &[
                    &source.id().to_bytes().to_vec(),
                    &source.parent().map(|parent| parent.to_bytes().to_vec()),
                    &source.bundle().to_bytes().to_vec(),
                    &source.revision_hash().to_bytes().to_vec(),
                ],
            )
            .await?;
    }
    session.shutdown().await
}

fn v3_standard_upgrade_candidate(
    empty: &ActiveDatabaseRevision,
    standard: &VerifiedStandardLibrarySnapshot,
) -> TestResult<DeployableRevision> {
    let source_bundle = SourceBundleId::from_bytes([0xb1; 16]);
    let source_revision = SourceRevisionId::from_bytes([0xb2; 16]);
    let bundle_hash = source_bundle_digest(&[])?;
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        Some(empty.pair().source()),
        Vec::new(),
        bundle_hash,
        source_revision_record_digest(source_bundle, Some(empty.pair().source()), bundle_hash)?,
    )?;
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0xb3; 16]),
        Vec::new(),
        Vec::new(),
    )?;
    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue_hash = catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[])?;
    Ok(DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            empty.pair(),
            source,
            empty.pair().catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new())
                .with_current_function_revisions(Vec::new()),
        ),
        context,
    )?)
}

fn sealed_json_encode_request(value: OpaqueValue) -> TestResult<InvokeRequest> {
    Ok(InvokeRequest::new(InvokeRequestInput {
        target: InvocationRequestTarget::function_id(STD_JSON_ENCODE_FUNCTION_ID),
        arguments: vec![InvocationArgument::new(
            InvocationParameterSelector::parameter_id(STD_JSON_ENCODE_PARAMETER_ID),
            InvokeValue::new(RuntimeValue::Opaque(value))?,
        )],
        caller_context: InvocationCallerContext::new(
            InvocationCallerKind::TestRunner,
            false,
            false,
            None,
            None,
            "en-GB",
            "UTC",
            None,
        )?,
        client_offer: InvocationClientOffer::new(
            5,
            "en-GB",
            "UTC",
            Vec::new(),
            Vec::new(),
            1_024,
            0,
            None,
            None,
        )?,
        output_requirement: None,
        state_profile: None,
        trace_policy: InvocationTracePolicy::Off,
        idempotency_key: None,
        parent_invocation_id: None,
        observer_context: None,
    })?)
}

fn assert_json_encode_payload(
    result: &SealedInvocationResult,
    expected_payload: &[u8],
) -> TestResult<()> {
    let SealedInvocationResult::Completed { events, .. } = result else {
        return Err(failure("reopened V5 JSON invocation did not complete"));
    };
    let records = events.records();
    require(
        records.len() == 3
            && records[0].outer_sequence() == 1
            && records[1].outer_sequence() == 2
            && records[2].outer_sequence() == 3
            && records[0].event().sequence() == 0
            && records[1].event().sequence() == 1
            && records[2].event().sequence() == 2
            && records[0].event().kind() == InvocationEventKind::InvocationStarted
            && records[1].event().kind() == InvocationEventKind::ValueBatch
            && records[2].event().kind() == InvocationEventKind::InvocationCompleted,
        "reopened V5 JSON invocation returned an unexpected event sequence",
    )?;
    let InvocationEventBody::ValueBatch {
        schema: None,
        values,
    } = records[1].event().body()
    else {
        return Err(failure(
            "reopened V5 JSON invocation did not return a plain value batch",
        ));
    };
    let Some(RuntimeValue::Opaque(value)) = values.first().map(|value| value.value()) else {
        return Err(failure(
            "reopened V5 JSON invocation did not return a ByteStream",
        ));
    };
    require(
        value.opaque_type() == STD_IO_BYTE_STREAM_TYPE_ID
            && value.canonical_payload() == expected_payload,
        "reopened V5 std.json.encode did not emit the exact application/json ByteStream",
    )
}

fn kernel(database: &TestDatabase) -> TestResult<PostgresKernel> {
    Ok(database.connection_string().parse()?)
}

fn require(condition: bool, message: &str) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}
