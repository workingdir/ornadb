mod support;

use std::{
    str::FromStr,
    time::{Duration, Instant},
};

use orna_compiler::{
    PrepareStandardUpgradeError, StandardApplicationCheckContext, check,
    check_standard_application, prepare, prepare_standard_application,
};
use orna_core::{
    CatalogueRevisionId, FieldId, ObjectId, SourceBundleId, SourceRevisionId, SourceUnitId,
    StandardLibraryRevisionId, TypeId,
    canonical_hash::{
        catalogue_digest_with_context, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest, verify_standard_library_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, FunctionReturn, TypeLookupName, ValueTypeKind, ValueTypeMutability,
        ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity,
        DefinitionReferenceKind, DefinitionReferenceTarget, DeployableRevision,
        DeployableRevisionContent, DeployableRevisionInput, FunctionRevisionRecord, RevisionPair,
        Sha256Digest, SourceOrigin, StandardLibraryDigestVersion, StandardLibrarySnapshot,
        StoredSourceRevision, StoredSourceUnit,
    },
    source::{SourceBundle, SourceUnit},
    types::ResolvedType,
    value::RuntimeValue,
};
use orna_kernel_postgres::{PostgresKernel, PostgresKernelError};
use support::{TestDatabase, TestResult, failure, with_test_database};

const BASIC_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.widget AS OBJECT (name TEXT NOT NULL, active BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION app.list_widgets()\n\
    RETURNS ROWS (name TEXT)\n\
    AS SELECT widget.name FROM app.widget widget WHERE widget.active = FALSE;\n";

const BASIC_SOURCE_ONLY_EDIT: &str = "-- source-only formatting edit\n\
    CREATE SCHEMA app;\n\n\
    CREATE TYPE app.widget AS OBJECT ( name TEXT NOT NULL, active BOOL NOT NULL );\n\
    CREATE SERVER FUNCTION app.list_widgets() RETURNS ROWS (name TEXT)\n\
    AS SELECT widget.name FROM app.widget widget WHERE widget.active = FALSE;\n";

const BASIC_CHANGED_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.widget AS OBJECT (name TEXT NOT NULL, active BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION app.list_widgets()\n\
    RETURNS ROWS (name TEXT)\n\
    AS SELECT widget.name FROM app.widget widget WHERE widget.active = TRUE;\n";

const STANDARD_APPLICATION_SOURCE: &str = "CREATE SCHEMA app;\n";

const ENUM_APPLICATION_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.stage AS ENUM ('lead', 'owner''s', 'customer');\n";

const STANDARD_UPGRADE_V1_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);\n\
    CREATE SERVER FUNCTION app.read()\n\
    RETURNS ROWS (visible BOOLEAN) TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT item.done FROM app.item item;\n";

const STANDARD_APPLICATION_SOURCE_EDIT: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.item AS OBJECT (done BOOLEAN NOT NULL);\n\
    CREATE SERVER FUNCTION app.read()\n\
    RETURNS ROWS (visible BOOLEAN) TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT item.done FROM app.item item;\n\
    CREATE SERVER FUNCTION app.create(p_ref REF app.item, p_done BOOLEAN)\n\
    RETURNS ROWS (created REF app.item) TRANSACTION ATOMIC\n\
    AS INSERT INTO app.item AS made (done) VALUES (p_done) RETURNING REF(made);\n\
    CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN FALSE;\n";

const FIELD_RENAME_ORIGINAL_SOURCE: &str = "CREATE SCHEMA people;\n\
    CREATE TYPE people.person AS OBJECT (email TEXT NOT NULL);\n\
    CREATE SERVER FUNCTION people.list_emails()\n\
    RETURNS ROWS (email TEXT)\n\
    AS SELECT p.email FROM people.person p;\n";

const FIELD_RENAME_FINAL_SOURCE: &str = "CREATE SCHEMA people;\n\
    CREATE TYPE people.person AS OBJECT (primary_email TEXT NOT NULL);\n\
    ALTER TYPE people.person RENAME FIELD email TO primary_email;\n\
    CREATE SERVER FUNCTION people.list_emails()\n\
    RETURNS ROWS (email TEXT)\n\
    AS SELECT p.primary_email FROM people.person p;\n";

const UNIQUE_REFERENCE_ORIGINAL_SOURCE: &str = "CREATE SCHEMA assignments;\n\
    CREATE TYPE assignments.person AS OBJECT ();\n\
    CREATE TYPE assignments.assignment AS OBJECT (\n\
        owner REF assignments.person NOT NULL UNIQUE\n\
    );\n";

const UNIQUE_REFERENCE_RENAMED_SOURCE: &str = "CREATE SCHEMA assignments;\n\
    CREATE TYPE assignments.person AS OBJECT ();\n\
    CREATE TYPE assignments.assignment AS OBJECT (\n\
        assignee REF assignments.person NOT NULL UNIQUE\n\
    );\n\
    ALTER TYPE assignments.assignment RENAME FIELD owner TO assignee;\n";

const MUTUAL_REFERENCE_SOURCE: &str = "CREATE SCHEMA graph;\n\
    CREATE TYPE graph.left AS OBJECT (right REF graph.right);\n\
    CREATE TYPE graph.right AS OBJECT (left REF graph.left);\n";

const RACE_LEFT_SOURCE: &str = "CREATE SCHEMA race_left;\n\
    CREATE TYPE race_left.item AS OBJECT (enabled BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION race_left.list_items()\n\
    RETURNS ROWS (enabled BOOL)\n\
    AS SELECT item.enabled FROM race_left.item item;\n";

const RACE_RIGHT_SOURCE: &str = "CREATE SCHEMA race_right;\n\
    CREATE TYPE race_right.item AS OBJECT (enabled BOOL NOT NULL);\n\
    CREATE SERVER FUNCTION race_right.list_items()\n\
    RETURNS ROWS (enabled BOOL)\n\
    AS SELECT item.enabled FROM race_right.item item;\n";

const APPLY_TIMEOUT: Duration = Duration::from_secs(5);
const RACE_LOCK_KEY: i64 = 0x4f52_4e41_4150_504c;
const EMPTY_STANDARD_SOURCE: &str = "CREATE SCHEMA std.;CREATE SCHEMA ;CREATE SCHEMA std;";
const EMPTY_STANDARD_DIGEST: [u8; 32] = [
    0x6d, 0x3f, 0xaa, 0x32, 0x82, 0x0e, 0xeb, 0x73, 0x77, 0xc5, 0xbd, 0xfa, 0x3e, 0x8d, 0x6c, 0xaf,
    0xdc, 0x95, 0xa6, 0x7c, 0xbd, 0xef, 0x5b, 0x02, 0x63, 0x1f, 0x29, 0x1d, 0x14, 0xcc, 0x68, 0xae,
];

#[derive(Clone, Copy)]
enum FailurePoint {
    SourceBundle,
    CatalogueSchema,
    FunctionArtifact,
    DefinitionReference,
    DeferredReference,
    StatusSweep,
    ActivePointer,
    PostPointerRecovery,
}

impl FailurePoint {
    const ALL: [Self; 8] = [
        Self::SourceBundle,
        Self::CatalogueSchema,
        Self::FunctionArtifact,
        Self::DefinitionReference,
        Self::DeferredReference,
        Self::StatusSweep,
        Self::ActivePointer,
        Self::PostPointerRecovery,
    ];
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_a_compiler_candidate_and_recovers_exactly() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = candidate(BASIC_SOURCE, &active)?;

        let applied = kernel.apply(&candidate).await?;
        let recovered = kernel.recover().await?;

        require_recovered_new_candidate(&candidate, &applied)?;
        require_recovered_new_candidate(&candidate, &recovered)?;
        require(
            recovered.catalogue().schemas().len() == 1
                && recovered.catalogue().object_types().len() == 1
                && recovered.catalogue().functions().len() == 1
                && recovered.function_revisions().len() == 1,
            "basic apply did not recover one schema, object, function, and immutable revision",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_a_version_two_candidate_before_any_apply_write() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = standard_context_candidate(active.pair())?;
        require(
            candidate.catalogue_hash_context().version() == CatalogueHashVersion::Version2
                && candidate.catalogue_hash() != active.catalogue_hash(),
            "version-two transition fixture did not carry a distinct later catalogue hash",
        )?;
        let before = baseline(&database, &active).await?;

        let error = failed_apply_error(
            kernel.apply(&candidate).await,
            "version-two candidate unexpectedly reached a successful normal apply",
        )?;

        require(
            error.to_string()
                == "the active and candidate catalogue hash versions require a standard context transition"
                && std::error::Error::source(&error).is_none(),
            "standard context transition error did not preserve its exact source-free contract",
        )?;
        match error {
            PostgresKernelError::StandardContextTransitionRequired {
                active: CatalogueHashVersion::Version1,
                candidate: CatalogueHashVersion::Version2,
            } => {}
            error => {
                return Err(failure(format!(
                    "expected standard context transition error, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn checks_the_expected_base_before_standard_context_transition() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let stale = RevisionPair::new(
            SourceRevisionId::from_bytes([0x91; 16]),
            active.pair().catalogue(),
        );
        let candidate = standard_context_candidate(stale)?;
        let before = baseline(&database, &active).await?;

        let error = failed_apply_error(
            kernel.apply(&candidate).await,
            "stale version-two candidate unexpectedly reached a successful apply",
        )?;

        require(
            error.to_string() == "expected revision pair is not active"
                && std::error::Error::source(&error).is_none(),
            "expected-base mismatch did not preserve its existing source-free contract",
        )?;
        match error {
            PostgresKernelError::ExpectedBaseMismatch {
                expected,
                active: actual_active,
            } => require(
                expected == stale && actual_active == active.pair(),
                "expected-base mismatch did not win before the standard context guard",
            )?,
            error => {
                return Err(failure(format!(
                    "expected stale-base mismatch before standard transition, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_the_standard_upgrade_then_reuses_normal_version_two_apply() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_APPLICATION_SOURCE, &empty)?;
        let version_one = kernel.apply(&version_one_candidate).await?;

        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        require(
            version_two.catalogue_hash_context().version() == CatalogueHashVersion::Version2,
            "standard upgrade did not install a version-two catalogue context",
        )?;
        require_standard_context(&version_two, upgrade.verified_standard_snapshot())?;
        require_recovered_snapshot(upgrade.application_revision(), &version_two)?;
        let replay_baseline = baseline(&database, &version_two).await?;
        let replay = failed_apply_error(
            kernel.apply_standard_upgrade(&upgrade).await,
            "replaying a standard upgrade unexpectedly succeeded",
        )?;
        require(
            replay.to_string() == "expected revision pair is not active"
                && std::error::Error::source(&replay).is_none(),
            "standard-upgrade replay changed the exact expected-base error contract",
        )?;
        match replay {
            PostgresKernelError::ExpectedBaseMismatch { expected, active } => require(
                expected == upgrade.application_revision().expected_base()
                    && active == version_two.pair(),
                "standard-upgrade replay did not fail before collision scanning",
            )?,
            error => {
                return Err(failure(format!(
                    "expected standard-upgrade replay to fail with ExpectedBaseMismatch, got {error}"
                )));
            }
        }
        require_baseline(&database, &replay_baseline, &kernel).await?;

        let repeated = orna_standard::prepare_standard_upgrade(&version_two)
            .expect_err("re-preparing an installed standard upgrade unexpectedly succeeded");
        require(
            repeated.to_string()
                == format!(
                    "standard library {} is already installed",
                    upgrade.verified_standard_snapshot().revision()
                ),
            "re-preparing an installed standard did not preserve the exact compiler error",
        )?;
        match repeated {
            orna_standard::StandardUpgradeError::Prepare {
                source: PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled {
                    revision,
                },
            } => require(
                revision == upgrade.verified_standard_snapshot().revision(),
                "re-preparation reported the wrong installed standard revision",
            )?,
            error => {
                return Err(failure(format!(
                    "expected StandardLibraryAlreadyInstalled, got {error}"
                )));
            }
        }

        let second_candidate = standard_application_candidate(
            STANDARD_APPLICATION_SOURCE_EDIT,
            &version_two,
            &upgrade,
        )?;
        let second = kernel.apply(&second_candidate).await?;
        require(
            second.catalogue_hash_context().version() == CatalogueHashVersion::Version2,
            "normal same-context apply did not retain the installed standard context",
        )?;
        require_standard_context(&second, upgrade.verified_standard_snapshot())?;
        require_recovered_snapshot(&second_candidate, &second)?;
        require_standard_upgrade_storage(&database, &second, &upgrade, &second_candidate).await?;

        let restarted = named_kernel(&database, "orna-standard-restart")?
            .recover()
            .await?;
        require_standard_context(&restarted, upgrade.verified_standard_snapshot())?;
        require(
            same_recovered(&restarted, &second),
            "reconnect changed current or historical function revision facts",
        )?;
        require_recovered_snapshot(&second_candidate, &restarted)
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_and_recovers_ordered_catalogue_enum_labels() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_APPLICATION_SOURCE, &empty)?;
        let version_one = kernel.apply(&version_one_candidate).await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let version_two = kernel.apply_standard_upgrade(&upgrade).await?;
        let candidate =
            standard_application_candidate(ENUM_APPLICATION_SOURCE, &version_two, &upgrade)?;
        let expected = candidate
            .candidate()
            .enum_types()
            .first()
            .ok_or_else(|| failure("enum candidate did not contain its declaration"))?;
        let expected_schema = candidate
            .candidate()
            .schemas()
            .first()
            .ok_or_else(|| failure("enum candidate did not contain its schema"))?;
        let expected_origin = candidate
            .origins()
            .iter()
            .find(|origin| origin.identity() == DefinitionIdentity::ValueType(expected.id()))
            .ok_or_else(|| failure("enum candidate did not contain its source origin"))?;

        let applied = kernel.apply(&candidate).await?;
        require_recovered_snapshot(&candidate, &applied)?;

        let session = database.open().await?;
        let row = session
            .client()
            .query_one(
                "SELECT type_id, schema_id, name_parts, labels,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.catalogue_enum_types
                 WHERE catalogue_revision_id = $1",
                &[&candidate.candidate().revision().to_bytes().to_vec()],
            )
            .await?;
        let postgres_enum_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*)
                 FROM pg_catalog.pg_type AS type
                 JOIN pg_catalog.pg_namespace AS namespace
                   ON namespace.oid = type.typnamespace
                 WHERE type.typtype = 'e'
                   AND namespace.nspname IN ('_orna_kernel', '_orna_data')",
                &[],
            )
            .await?
            .try_get(0)?;
        require(
            row.try_get::<_, Vec<u8>>(0)? == expected.id().to_bytes().to_vec()
                && row.try_get::<_, Vec<u8>>(1)? == expected_schema.id().to_bytes().to_vec()
                && row.try_get::<_, Vec<String>>(2)? == expected.name().parts()
                && row.try_get::<_, Vec<String>>(3)? == expected.labels()
                && row.try_get::<_, Vec<u8>>(4)?
                    == expected_origin.source().source_unit().to_bytes().to_vec()
                && row.try_get::<_, i64>(5)? == i64::from(expected_origin.source().byte_start())
                && row.try_get::<_, i64>(6)? == i64::from(expected_origin.source().byte_end())
                && postgres_enum_count == 0,
            "enum apply did not preserve the exact protected catalogue row",
        )?;
        session.shutdown().await?;

        let restarted = named_kernel(&database, "orna-enum-restart")?
            .recover()
            .await?;
        require_recovered_snapshot(&candidate, &restarted)
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn prepares_standard_upgrade_from_postgres_recovered_version_one_members() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_UPGRADE_V1_SOURCE, &empty)?;
        kernel.apply(&version_one_candidate).await?;

        let recovered = named_kernel(&database, "orna-standard-preparation-recovery")?
            .recover()
            .await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&recovered)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        require(
            upgrade.application_revision().expected_base() == recovered.pair(),
            "standard upgrade expected base did not use the PostgreSQL-recovered pair",
        )?;

        let upgraded = kernel.apply_standard_upgrade(&upgrade).await?;
        require(
            upgraded.catalogue_hash_context().version() == CatalogueHashVersion::Version2,
            "standard upgrade did not install a version-two catalogue context",
        )?;
        require_standard_context(&upgraded, upgrade.verified_standard_snapshot())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn rejects_an_inactive_standard_revision_collision_before_standard_writes() -> TestResult<()>
{
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let version_one_candidate = candidate(STANDARD_APPLICATION_SOURCE, &empty)?;
        let version_one = kernel.apply(&version_one_candidate).await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&version_one)
            .map_err(|error| failure(format!("standard upgrade preparation failed: {error}")))?;
        let reserved_revision = upgrade.verified_standard_snapshot().revision();
        let hostile_catalogue = CatalogueRevisionId::from_bytes([0xe1; 16]);
        require(
            hostile_catalogue != upgrade.verified_standard_snapshot().catalogue().revision(),
            "hostile standard revision collision accidentally reused the standard catalogue ID",
        )?;

        let session = database.open().await?;
        session
            .client()
            .execute(
                "INSERT INTO _orna_kernel.standard_library_revisions
                    (id, source_revision_id, catalogue_revision_id, digest_version,
                     language_version, content_hash, hash_algorithm)
                 VALUES ($1, $2, $3, 1, 'orna.test/hostile', $4, 'sha256')",
                &[
                    &reserved_revision.to_bytes().to_vec(),
                    &version_one.source().id().to_bytes().to_vec(),
                    &hostile_catalogue.to_bytes().to_vec(),
                    &vec![0_u8; 32],
                ],
            )
            .await?;
        session.shutdown().await?;
        let before = baseline(&database, &version_one).await?;

        let error = failed_apply_error(
            kernel.apply_standard_upgrade(&upgrade).await,
            "inactive standard identity collision unexpectedly allowed the upgrade",
        )?;
        require(
            error.to_string()
                == "the database contains an identity reserved for the standard library"
                && std::error::Error::source(&error).is_none(),
            "inactive standard identity collision changed its exact source-free error contract",
        )?;
        match error {
            PostgresKernelError::ReservedStandardIdentity {
                identity: orna_standard::StandardUpgradeIdentity::StandardLibraryRevision(revision),
            } => require(
                revision == reserved_revision,
                "inactive standard identity collision returned the wrong durable identity",
            )?,
            error => {
                return Err(failure(format!(
                    "expected inactive standard revision collision, got {error}"
                )));
            }
        }
        require_baseline(&database, &before, &kernel).await?;

        let session = database.open().await?;
        let hostile = session
            .client()
            .query_one(
                "SELECT source_revision_id, catalogue_revision_id, digest_version,
                        language_version, content_hash, hash_algorithm
                 FROM _orna_kernel.standard_library_revisions WHERE id = $1",
                &[&reserved_revision.to_bytes().to_vec()],
            )
            .await?;
        require(
            hostile.try_get::<_, Vec<u8>>(0)? == version_one.source().id().to_bytes()
                && hostile.try_get::<_, Vec<u8>>(1)? == hostile_catalogue.to_bytes()
                && hostile.try_get::<_, i16>(2)? == 1
                && hostile.try_get::<_, String>(3)? == "orna.test/hostile"
                && hostile.try_get::<_, Vec<u8>>(4)? == vec![0_u8; 32]
                && hostile.try_get::<_, String>(5)? == "sha256",
            "inactive standard revision collision row changed after the rejected upgrade",
        )?;
        let standard_rows = session
            .client()
            .query_one(
                "SELECT
                    (SELECT count(*) FROM _orna_kernel.standard_catalogue_schemas),
                    (SELECT count(*) FROM _orna_kernel.standard_catalogue_value_types),
                    (SELECT count(*) FROM _orna_kernel.standard_catalogue_type_bindings)",
                &[],
            )
            .await?;
        require(
            standard_rows.try_get::<_, i64>(0)? == 0
                && standard_rows.try_get::<_, i64>(1)? == 0
                && standard_rows.try_get::<_, i64>(2)? == 0,
            "rejected collision unexpectedly wrote standard catalogue rows",
        )?;
        session.shutdown().await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn applies_mutual_references_with_real_postgres_foreign_keys() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = candidate(MUTUAL_REFERENCE_SOURCE, &active)?;
        let applied = kernel.apply(&candidate).await?;

        let left = applied.catalogue().object_types()[0].id();
        let right = applied.catalogue().object_types()[1].id();
        let session = database.open().await?;
        let foreign_keys = session
            .client()
            .query(
                "SELECT conrelid::regclass::text, confrelid::regclass::text, confdeltype::text\n                 FROM pg_constraint\n                 WHERE contype = 'f'\n                   AND conrelid::regclass::text = ANY($1::text[])\n                 ORDER BY conrelid::regclass::text",
                &[&vec![relation(left), relation(right)]],
            )
            .await?
            .into_iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
            .collect::<Result<Vec<(String, String, String)>, tokio_postgres::Error>>()?;
        session.shutdown().await?;
        require(
            same_members(
                &foreign_keys,
                &[
                    (relation(left), relation(right), "a".into()),
                    (relation(right), relation(left), "a".into()),
                ],
            ),
            "mutual REF apply did not install exact left/right NO ACTION foreign keys",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn source_only_edit_reuses_the_immutable_function_revision_and_artifact() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let first = kernel
            .apply(&candidate(BASIC_SOURCE, &kernel.recover().await?)?)
            .await?;
        let first_revision = only_revision(&first)?.clone();
        let before = immutable_rows(&database, &first_revision).await?;
        let candidate = candidate(BASIC_SOURCE_ONLY_EDIT, &first)?;
        require(
            candidate.new_function_revisions().is_empty(),
            "source-only compiler preparation allocated an immutable function revision",
        )?;

        let applied = kernel.apply(&candidate).await?;
        let reused = only_revision(&applied)?;
        require_recovered_snapshot(&candidate, &applied)?;
        require(
            reused == &first_revision,
            "source-only apply changed the complete immutable function revision record",
        )?;
        let after = immutable_rows(&database, reused).await?;
        require(
            before == after,
            "source-only apply rewrote or added immutable function revision or artifact rows",
        )?;
        require(
            applied.historical_function_revisions().is_empty(),
            "source-only apply invented function revision history",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn replay_safe_field_rename_preserves_live_storage_and_execution() -> TestResult<()> {
    with_test_database(|database| async move {
        let initial_kernel = kernel(&database)?;
        initial_kernel.bootstrap().await?;
        let original_candidate = candidate(
            FIELD_RENAME_ORIGINAL_SOURCE,
            &initial_kernel.recover().await?,
        )?;
        let original = initial_kernel.apply(&original_candidate).await?;
        let object = original.catalogue().object_types()[0].id();
        let original_field = original.catalogue().object_types()[0].fields()[0].id();
        let function = original.catalogue().functions()[0].id();
        let original_revision = only_revision(&original)?.clone();
        let original_immutable = immutable_rows(&database, &original_revision).await?;
        let original_physical = physical_catalogue(&database, object).await?;
        let stored_object = ObjectId::from_bytes([91; 16]);
        insert_private_text(
            &database,
            object,
            original_field,
            stored_object,
            "kept@example.test",
        )
        .await?;
        let proof = RenameProof {
            object,
            field: original_field,
            function,
            revision: original_revision,
            immutable: original_immutable,
            physical: original_physical,
            stored_object,
        };

        let renamed_candidate = candidate(FIELD_RENAME_FINAL_SOURCE, &original)?;
        require(
            renamed_candidate.new_function_revisions().is_empty(),
            "field rename allocated a new immutable function revision",
        )?;
        require(
            renamed_candidate.source().bundle_hash() != original.source().bundle_hash()
                && renamed_candidate.source().revision_hash() != original.source().revision_hash()
                && renamed_candidate.catalogue_hash() != original.catalogue_hash(),
            "field rename did not change all source and catalogue hashes",
        )?;
        require_rename_semantics(
            &renamed_candidate,
            proof.object,
            proof.field,
            proof.function,
            proof.revision.id(),
        )?;

        let renamed = initial_kernel.apply(&renamed_candidate).await?;
        require_recovered_snapshot(&renamed_candidate, &renamed)?;
        require_rename_state(&database, &renamed, &proof).await?;

        let replay_kernel = kernel(&database)?;
        let recovered = replay_kernel.recover().await?;
        require_rename_state(&database, &recovered, &proof).await?;
        let replay_candidate = candidate(FIELD_RENAME_FINAL_SOURCE, &recovered)?;
        require(
            replay_candidate.new_function_revisions().is_empty(),
            "exact field-rename replay allocated a new immutable function revision",
        )?;
        require_rename_semantics(
            &replay_candidate,
            proof.object,
            proof.field,
            proof.function,
            proof.revision.id(),
        )?;

        let replayed = replay_kernel.apply(&replay_candidate).await?;
        let final_kernel = kernel(&database)?;
        let final_recovered = final_kernel.recover().await?;
        require_recovered_snapshot(&replay_candidate, &replayed)?;
        require_recovered_snapshot(&replay_candidate, &final_recovered)?;
        require_rename_state(&database, &final_recovered, &proof).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn required_unique_reference_replay_and_rename_preserve_physical_identity() -> TestResult<()>
{
    with_test_database(|database| async move {
        let initial_kernel = kernel(&database)?;
        initial_kernel.bootstrap().await?;
        let original_candidate = candidate(
            UNIQUE_REFERENCE_ORIGINAL_SOURCE,
            &initial_kernel.recover().await?,
        )?;
        let original = initial_kernel.apply(&original_candidate).await?;
        require_recovered_snapshot(&original_candidate, &original)?;

        let person = original
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["assignments", "person"])
            .ok_or_else(|| failure("initial apply did not create assignments.person"))?;
        let assignment = original
            .catalogue()
            .object_types()
            .iter()
            .find(|object| object.name().parts() == ["assignments", "assignment"])
            .ok_or_else(|| failure("initial apply did not create assignments.assignment"))?;
        let owner = assignment
            .field_by_name("owner")
            .ok_or_else(|| failure("initial apply did not create assignment.owner"))?;
        require(
            assignment.fields().len() == 1
                && owner.is_required_unique_reference()
                && owner.resolved_type() == ResolvedType::reference(person.id()),
            "initial apply changed the required unique reference semantics",
        )?;

        let assignment_physical = physical_catalogue(&database, assignment.id()).await?;
        let person_physical = physical_catalogue(&database, person.id()).await?;
        let field_hex = format!("{:032x}", u128::from_be_bytes(owner.id().to_bytes()));
        let unique_name = format!("uq_{field_hex}");
        let foreign_key_name = format!("fk_{field_hex}");
        require(
            assignment_physical
                .constraints
                .iter()
                .any(|(_, name, _)| name == &unique_name)
                && assignment_physical
                    .constraints
                    .iter()
                    .any(|(_, name, _)| name == &foreign_key_name)
                && assignment_physical
                    .indexes
                    .iter()
                    .any(|(_, name, _)| name == &unique_name),
            "initial apply did not install the stable unique and foreign-key identities",
        )?;
        let proof = UniqueReferenceProof {
            person: person.id(),
            assignment: assignment.id(),
            field: owner.id(),
            person_physical,
            assignment_physical,
        };

        let replay_kernel = kernel(&database)?;
        let recovered = replay_kernel.recover().await?;
        require_unique_reference_state(&database, &recovered, &proof, "owner").await?;
        let replay_candidate = candidate(UNIQUE_REFERENCE_ORIGINAL_SOURCE, &recovered)?;
        let replayed = replay_kernel.apply(&replay_candidate).await?;
        require_recovered_snapshot(&replay_candidate, &replayed)?;
        require_unique_reference_state(&database, &replayed, &proof, "owner").await?;

        let rename_candidate = candidate(UNIQUE_REFERENCE_RENAMED_SOURCE, &replayed)?;
        let renamed = replay_kernel.apply(&rename_candidate).await?;
        require_recovered_snapshot(&rename_candidate, &renamed)?;
        require_unique_reference_state(&database, &renamed, &proof, "assignee").await?;

        let final_recovered = kernel(&database)?.recover().await?;
        require_unique_reference_state(&database, &final_recovered, &proof, "assignee").await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn changed_function_history_is_retained_and_revert_reactivates_it() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let first = kernel
            .apply(&candidate(BASIC_SOURCE, &kernel.recover().await?)?)
            .await?;
        let original = only_revision(&first)?.clone();
        let changed_candidate = candidate(BASIC_CHANGED_SOURCE, &first)?;
        let changed = kernel.apply(&changed_candidate).await?;
        let changed_revision = only_revision(&changed)?.clone();
        require(
            changed_revision.id() != original.id()
                && changed.historical_function_revisions() == [original.clone()],
            "changed function apply did not retain the previous immutable revision",
        )?;
        require_recovered_snapshot(&changed_candidate, &changed)?;
        require(
            changed.function_revisions() == changed_candidate.new_function_revisions(),
            "changed function apply did not activate its newly prepared immutable revision",
        )?;

        let revert_candidate = candidate(BASIC_SOURCE, &changed)?;
        require(
            revert_candidate.new_function_revisions().is_empty(),
            "revert preparation allocated a new immutable function revision",
        )?;
        let reverted = kernel.apply(&revert_candidate).await?;
        require_recovered_snapshot(&revert_candidate, &reverted)?;
        require(
            only_revision(&reverted)?.id() == original.id(),
            "revert did not reactivate the retained matching immutable revision",
        )?;
        require(
            reverted.historical_function_revisions() == [changed_revision],
            "revert did not retire the changed immutable revision",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn same_base_concurrent_apply_has_one_winner_and_no_loser_residue() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel = kernel(&database)?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let left = candidate(RACE_LEFT_SOURCE, &empty)?;
        let right = candidate(RACE_RIGHT_SOURCE, &empty)?;
        install_race_pause_trigger(&database).await?;
        let coordinator = database.open().await?;
        coordinator.client().query_one("SELECT pg_advisory_lock($1)", &[&RACE_LOCK_KEY]).await?;
        let left_kernel = named_kernel(&database, "orna-apply-race-a")?;
        let left_for_task = left.clone();
        let left_task = tokio::spawn(async move { left_kernel.apply(&left_for_task).await });
        wait_for_advisory_wait(&database, "orna-apply-race-a").await?;
        let right_kernel = named_kernel(&database, "orna-apply-race-b")?;
        let right_for_task = right.clone();
        let right_task = tokio::spawn(async move { right_kernel.apply(&right_for_task).await });
        wait_for_active_lock_block(&database, "orna-apply-race-a", "orna-apply-race-b").await?;
        coordinator.client().query_one("SELECT pg_advisory_unlock($1)", &[&RACE_LOCK_KEY]).await?;
        coordinator.shutdown().await?;
        let left_result = wait_for_apply_task(left_task, "left").await?;
        let right_result = wait_for_apply_task(right_task, "right").await?;
        let (winner, winner_candidate, loser_candidate) = match (left_result, right_result) {
            (
                Ok(winner),
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
            ) if expected == empty.pair() && active == left.candidate_pair() => {
                (winner, &left, &right)
            }
            (
                Err(PostgresKernelError::ExpectedBaseMismatch { expected, active }),
                Ok(winner),
            ) if expected == empty.pair() && active == right.candidate_pair() => {
                (winner, &right, &left)
            }
            (left, right) => {
                return Err(failure(format!(
                    "same-base apply race must have one success and one typed stale failure; left={left:?} right={right:?}"
                )));
            }
        };

        let recovered = kernel.recover().await?;
        require_recovered_new_candidate(winner_candidate, &winner)?;
        require_recovered_new_candidate(winner_candidate, &recovered)?;
        require(
            recovered.pair() == winner_candidate.candidate_pair(),
            "same-base apply race recovered a revision other than the winning candidate",
        )?;
        require_no_candidate_residue(&database, loser_candidate, &empty).await
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn every_apply_failure_point_rolls_back_to_the_exact_base() -> TestResult<()> {
    for point in FailurePoint::ALL {
        with_test_database(|database| async move {
            let kernel = kernel(&database)?;
            kernel.bootstrap().await?;
            let initial = kernel.recover().await?;
            let (base, candidate) = if matches!(point, FailurePoint::StatusSweep) {
                let committed = kernel.apply(&candidate(BASIC_SOURCE, &initial)?).await?;
                let changed = candidate(BASIC_CHANGED_SOURCE, &committed)?;
                (committed, changed)
            } else {
                let candidate = candidate(BASIC_SOURCE, &initial)?;
                (initial, candidate)
            };
            if matches!(
                point,
                FailurePoint::DefinitionReference | FailurePoint::DeferredReference
            ) {
                require(
                    !candidate.references().is_empty(),
                    "reference trigger fixture must contain references",
                )?;
            }
            let baseline = baseline(&database, &base).await?;
            install_failure_point(&database, point, &candidate).await?;

            let error = kernel
                .apply(&candidate)
                .await
                .expect_err("triggered apply must fail");
            assert_failure_shape(point, &error)?;
            require_baseline(&database, &baseline, &kernel).await?;
            require_no_candidate_residue(&database, &candidate, &base).await
        })
        .await?;
    }
    Ok(())
}

async fn install_race_pause_trigger(database: &TestDatabase) -> TestResult<()> {
    let session = database.open().await?;
    session
        .client()
        .execute("SELECT pg_advisory_unlock_all()", &[])
        .await?;
    session.client().batch_execute(
        "CREATE FUNCTION _orna_kernel.test_apply_pause_pointer() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN PERFORM pg_advisory_xact_lock(5715716919262203980); RETURN NEW; END $$;
         CREATE TRIGGER pause_active_pointer BEFORE UPDATE ON _orna_kernel.active_revision
         FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_pause_pointer();",
    ).await?;
    session.shutdown().await
}

async fn wait_for_advisory_wait(database: &TestDatabase, application: &str) -> TestResult<()> {
    let deadline = Instant::now() + APPLY_TIMEOUT;
    loop {
        let session = database.open().await?;
        let waiting: bool = session
            .client()
            .query_one(
                "SELECT EXISTS (
                SELECT 1 FROM pg_stat_activity
                WHERE application_name = $1
                  AND wait_event_type = 'Lock'
                  AND wait_event = 'advisory'
             )",
                &[&application],
            )
            .await?
            .try_get(0)?;
        session.shutdown().await?;
        if waiting {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(failure(format!(
                "timed out waiting for {application} to block on the advisory lock"
            )));
        }
        tokio::task::yield_now().await;
    }
}

async fn wait_for_active_lock_block(
    database: &TestDatabase,
    holder: &str,
    waiter: &str,
) -> TestResult<()> {
    let deadline = Instant::now() + APPLY_TIMEOUT;
    loop {
        let session = database.open().await?;
        let blocked: bool = session
            .client()
            .query_one(
                "SELECT EXISTS (
                SELECT 1
                FROM pg_stat_activity AS holder
                JOIN pg_stat_activity AS waiter ON holder.pid = ANY(pg_blocking_pids(waiter.pid))
                WHERE holder.application_name = $1
                  AND waiter.application_name = $2
                  AND waiter.wait_event_type = 'Lock'
             )",
                &[&holder, &waiter],
            )
            .await?
            .try_get(0)?;
        session.shutdown().await?;
        if blocked {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(failure(
                "timed out waiting for B to block on A's active revision lock",
            ));
        }
        tokio::task::yield_now().await;
    }
}

async fn wait_for_apply_task(
    task: tokio::task::JoinHandle<Result<ActiveDatabaseRevision, PostgresKernelError>>,
    name: &'static str,
) -> TestResult<Result<ActiveDatabaseRevision, PostgresKernelError>> {
    let deadline = Instant::now() + APPLY_TIMEOUT;
    while !task.is_finished() {
        if Instant::now() >= deadline {
            task.abort();
            return Err(failure(format!("timed out waiting for {name} apply task")));
        }
        tokio::task::yield_now().await;
    }
    task.await
        .map_err(|error| failure(format!("{name} apply task failed: {error}")))
}

#[derive(Clone, Debug)]
struct Baseline {
    active_source: Vec<u8>,
    active_catalogue: Vec<u8>,
    active_updated_at: String,
    counts: Vec<i64>,
    statuses: Vec<(Vec<u8>, String)>,
    recovered: ActiveDatabaseRevision,
}

async fn baseline(
    database: &TestDatabase,
    recovered: &ActiveDatabaseRevision,
) -> TestResult<Baseline> {
    let session = database.open().await?;
    let active = session
        .client()
        .query_one(
            "SELECT source_revision_id, catalogue_revision_id, updated_at::text
         FROM _orna_kernel.active_revision WHERE singleton = true",
            &[],
        )
        .await?;
    let counts = session
        .client()
        .query_one(
            "SELECT
          (SELECT count(*) FROM _orna_kernel.schema_migrations),
          (SELECT count(*) FROM _orna_kernel.source_bundles),
          (SELECT count(*) FROM _orna_kernel.source_units),
          (SELECT count(*) FROM _orna_kernel.source_revisions),
          (SELECT count(*) FROM _orna_kernel.catalogue_revisions),
          (SELECT count(*) FROM _orna_kernel.catalogue_schemas),
          (SELECT count(*) FROM _orna_kernel.catalogue_object_types),
          (SELECT count(*) FROM _orna_kernel.catalogue_expressions),
          (SELECT count(*) FROM _orna_kernel.catalogue_fields),
          (SELECT count(*) FROM _orna_kernel.catalogue_functions),
          (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters),
          (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns),
          (SELECT count(*) FROM _orna_kernel.function_revisions),
          (SELECT count(*) FROM _orna_kernel.function_artifacts),
          (SELECT count(*) FROM _orna_kernel.active_revision),
          (SELECT count(*) FROM _orna_kernel.definition_references)",
            &[],
        )
        .await?;
    let statuses = session
        .client()
        .query(
            "SELECT id, status FROM _orna_kernel.function_revisions ORDER BY id",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| Ok((row.try_get(0)?, row.try_get(1)?)))
        .collect::<Result<Vec<(Vec<u8>, String)>, tokio_postgres::Error>>()?;
    let counts = (0..16)
        .map(|index| counts.try_get(index))
        .collect::<Result<Vec<i64>, _>>()?;
    let result = Baseline {
        active_source: active.try_get(0)?,
        active_catalogue: active.try_get(1)?,
        active_updated_at: active.try_get(2)?,
        counts,
        statuses,
        recovered: recovered.clone(),
    };
    session.shutdown().await?;
    Ok(result)
}

async fn require_baseline(
    database: &TestDatabase,
    expected: &Baseline,
    kernel: &PostgresKernel,
) -> TestResult<()> {
    let actual = baseline(database, &kernel.recover().await?).await?;
    require(
        actual.active_source == expected.active_source
            && actual.active_catalogue == expected.active_catalogue
            && actual.active_updated_at == expected.active_updated_at
            && actual.counts == expected.counts
            && actual.statuses == expected.statuses
            && same_recovered(&actual.recovered, &expected.recovered),
        "failed apply changed the exact durable base baseline",
    )
}

async fn require_standard_upgrade_storage(
    database: &TestDatabase,
    active: &ActiveDatabaseRevision,
    upgrade: &orna_standard::StandardUpgrade,
    candidate: &DeployableRevision,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation = async {
        let catalogue_id = active.catalogue().revision().to_bytes().to_vec();
        let standard = upgrade.verified_standard_snapshot();
        let standard_revision = standard.revision().to_bytes().to_vec();
        let object = active
            .catalogue()
            .object_types()
            .first()
            .ok_or_else(|| failure("standard application object row is missing"))?;
        let field = object
            .fields()
            .first()
            .ok_or_else(|| failure("standard application field definition is missing"))?;
        require(
            standard
                .catalogue()
                .value_types()
                .iter()
                .all(|value_type| value_type.id() != object.id()),
            "application object TypeId unexpectedly overlaps a standard Value TypeId",
        )?;
        let boolean_type_id = field
            .resolved_type()
            .value_type()
            .ok_or_else(|| failure("standard application field did not retain a Value identity"))?;
        let boolean_definition = standard
            .catalogue()
            .value_type_by_id(boolean_type_id)
            .ok_or_else(|| failure("application Boolean Value identity is not pinned"))?;
        require(
            boolean_definition.representation_contract() == "orna.kernel.value.boolean@1",
            "application Boolean Value identity does not select the verified Boolean contract",
        )?;
        let boolean_type = boolean_type_id.to_bytes().to_vec();

        let revision = session
            .client()
            .query_one(
                "SELECT id, source_revision_id, catalogue_revision_id, digest_version,
                        language_version, content_hash
                 FROM _orna_kernel.standard_library_revisions
                 WHERE id = $1",
                &[&standard_revision],
            )
            .await?;
        require(
            revision.try_get::<_, Vec<u8>>(0)? == standard.revision().to_bytes()
                && revision.try_get::<_, Vec<u8>>(1)? == standard.source().id().to_bytes()
                && revision.try_get::<_, Vec<u8>>(2)? == standard.catalogue().revision().to_bytes()
                && revision.try_get::<_, i16>(3)? == standard.digest_version().to_u32() as i16
                && revision.try_get::<_, String>(4)? == standard.language_version()
                && revision.try_get::<_, Vec<u8>>(5)? == standard.digest().to_bytes(),
            "standard library revision rows changed during atomic installation",
        )?;

        let source_bundle_row = session
            .client()
            .query_one(
                "SELECT id, content_hash, hash_algorithm, hash_contract_version
                 FROM _orna_kernel.source_bundles WHERE id = $1",
                &[&standard.source().bundle().to_bytes().to_vec()],
            )
            .await?;
        require(
            source_bundle_row.try_get::<_, Vec<u8>>(0)? == standard.source().bundle().to_bytes()
                && source_bundle_row.try_get::<_, Vec<u8>>(1)?
                    == standard.source().bundle_hash().to_bytes()
                && source_bundle_row.try_get::<_, String>(2)? == "sha256"
                && source_bundle_row.try_get::<_, i16>(3)? == 1,
            "standard source bundle row differs from the verified snapshot",
        )?;
        let source_revision_row = session
            .client()
            .query_one(
                "SELECT id, parent_source_revision_id, bundle_id, content_hash,
                        hash_algorithm, hash_contract_version
                 FROM _orna_kernel.source_revisions WHERE id = $1",
                &[&standard.source().id().to_bytes().to_vec()],
            )
            .await?;
        require(
            source_revision_row.try_get::<_, Vec<u8>>(0)? == standard.source().id().to_bytes()
                && source_revision_row.try_get::<_, Option<Vec<u8>>>(1)?
                    == standard.source().parent().map(|id| id.to_bytes().to_vec())
                && source_revision_row.try_get::<_, Vec<u8>>(2)?
                    == standard.source().bundle().to_bytes()
                && source_revision_row.try_get::<_, Vec<u8>>(3)?
                    == standard.source().revision_hash().to_bytes()
                && source_revision_row.try_get::<_, String>(4)? == "sha256"
                && source_revision_row.try_get::<_, i16>(5)? == 1,
            "standard source revision row differs from the verified snapshot",
        )?;

        let catalogue_revision_row = session
            .client()
            .query_one(
                "SELECT id, source_revision_id, parent_catalogue_revision_id, content_hash,
                        canonical_hash_version, standard_library_revision_id, hash_contract_version
                 FROM _orna_kernel.catalogue_revisions WHERE id = $1",
                &[&catalogue_id],
            )
            .await?;
        require(
            catalogue_revision_row.try_get::<_, Vec<u8>>(0)?
                == active.catalogue().revision().to_bytes()
                && catalogue_revision_row.try_get::<_, Vec<u8>>(1)?
                    == active.source().id().to_bytes()
                && catalogue_revision_row.try_get::<_, Option<Vec<u8>>>(2)?
                    == Some(candidate.parent_catalogue().to_bytes().to_vec())
                && catalogue_revision_row.try_get::<_, Vec<u8>>(3)?
                    == candidate.catalogue_hash().to_bytes()
                && catalogue_revision_row.try_get::<_, i16>(4)? == 2
                && catalogue_revision_row.try_get::<_, Option<Vec<u8>>>(5)?
                    == Some(standard.revision().to_bytes().to_vec())
                && catalogue_revision_row.try_get::<_, i16>(6)? == 1,
            "application catalogue revision did not retain its exact version-two context and hash",
        )?;

        let semantic_revisions = session
            .client()
            .query(
                "SELECT id, function_id, semantic_hash_version
                 FROM _orna_kernel.function_revisions
                 WHERE status = 'active' ORDER BY function_id",
                &[],
            )
            .await?;
        require(
            semantic_revisions.len() == active.function_revisions().len()
                && semantic_revisions.iter().all(|row| {
                    active.function_revisions().iter().any(|revision| {
                        row.try_get::<_, Vec<u8>>(0).ok() == Some(revision.id().to_bytes().to_vec())
                            && row.try_get::<_, Vec<u8>>(1).ok()
                                == Some(revision.function().to_bytes().to_vec())
                            && row.try_get::<_, i16>(2).ok()
                                == Some(revision.semantic_hash_version().to_u32() as i16)
                    })
                }),
            "standard application function revisions did not retain semantic hash version two",
        )?;
        let expected_current = candidate
            .current_function_revisions()
            .ok_or_else(|| failure("standard application candidate omitted current revisions"))?;
        require(
            same_members(active.function_revisions(), expected_current),
            "recovered current function revisions differ from the prepared candidate",
        )?;

        let source_units = session
            .client()
            .query(
                "SELECT id, bundle_id, ordinal, logical_path, content, content_hash
                 FROM _orna_kernel.source_units
                 WHERE bundle_id = $1 ORDER BY ordinal",
                &[&standard.source().bundle().to_bytes().to_vec()],
            )
            .await?;
        require(
            source_units.len() == standard.source().units().len(),
            "standard source-unit row count differs from the verified snapshot",
        )?;
        for unit in standard.source().units() {
            let row = source_units
                .iter()
                .find(|row| {
                    row.try_get::<_, Vec<u8>>(0).ok() == Some(unit.id().to_bytes().to_vec())
                })
                .ok_or_else(|| failure("verified standard source unit is missing"))?;
            require(
                row.try_get::<_, Vec<u8>>(1)? == standard.source().bundle().to_bytes()
                    && row.try_get::<_, i64>(2)? == i64::from(unit.ordinal())
                    && row.try_get::<_, String>(3)? == unit.logical_path()
                    && row.try_get::<_, String>(4)? == unit.content()
                    && row.try_get::<_, Vec<u8>>(5)? == unit.content_hash().to_bytes(),
                "standard source-unit row differs from the verified snapshot",
            )?;
        }

        let schemas = session
            .client()
            .query(
                "SELECT schema_id, name_parts, source_unit_id, source_start, source_end
                 FROM _orna_kernel.standard_catalogue_schemas
                 WHERE standard_library_revision_id = $1 ORDER BY schema_id",
                &[&standard_revision],
            )
            .await?;
        require(
            schemas.len() == standard.catalogue().schemas().len(),
            "standard schema row count differs from the verified snapshot",
        )?;
        for schema in standard.catalogue().schemas() {
            let row = schemas
                .iter()
                .find(|row| {
                    row.try_get::<_, Vec<u8>>(0).ok() == Some(schema.id().to_bytes().to_vec())
                })
                .ok_or_else(|| failure("verified standard schema row is missing"))?;
            let origin = standard
                .origins()
                .iter()
                .find(|origin| origin.identity() == DefinitionIdentity::Schema(schema.id()))
                .ok_or_else(|| failure("verified standard schema origin is missing"))?
                .source();
            require(
                row.try_get::<_, Vec<String>>(1)? == schema.name().parts()
                    && row.try_get::<_, Vec<u8>>(2)? == origin.source_unit().to_bytes()
                    && row.try_get::<_, i64>(3)? == i64::from(origin.byte_start())
                    && row.try_get::<_, i64>(4)? == i64::from(origin.byte_end()),
                "standard schema row differs from the verified snapshot",
            )?;
        }

        let value_types = session
            .client()
            .query(
                "SELECT type_id, schema_id, name_parts, value_kind, mutability,
                        persistence, representation_contract, source_unit_id,
                        source_start, source_end
                 FROM _orna_kernel.standard_catalogue_value_types
                 WHERE standard_library_revision_id = $1 ORDER BY type_id",
                &[&standard_revision],
            )
            .await?;
        require(
            value_types.len() == standard.catalogue().value_types().len(),
            "standard value-type row count differs from the verified snapshot",
        )?;
        for value_type in standard.catalogue().value_types() {
            let row = value_types
                .iter()
                .find(|row| {
                    row.try_get::<_, Vec<u8>>(0).ok() == Some(value_type.id().to_bytes().to_vec())
                })
                .ok_or_else(|| failure("verified standard value-type row is missing"))?;
            let schema_name = value_type
                .name()
                .parts()
                .get(..value_type.name().parts().len().saturating_sub(1))
                .filter(|parts| !parts.is_empty())
                .ok_or_else(|| failure("standard value type name has no schema part"))?;
            let schema = standard
                .catalogue()
                .schemas()
                .iter()
                .find(|schema| schema.name().parts() == schema_name)
                .ok_or_else(|| failure("standard value type schema is missing"))?;
            let origin = standard
                .origins()
                .iter()
                .find(|origin| origin.identity() == DefinitionIdentity::ValueType(value_type.id()))
                .ok_or_else(|| failure("verified standard value-type origin is missing"))?
                .source();
            require(
                row.try_get::<_, Vec<u8>>(1)? == schema.id().to_bytes(),
                "standard value-type schema differs from the verified snapshot",
            )?;
            require(
                row.try_get::<_, Vec<String>>(2)? == value_type.name().parts(),
                "standard value-type name differs from the verified snapshot",
            )?;
            require(
                row.try_get::<_, String>(3)?
                    == if matches!(value_type.kind(), ValueTypeKind::Primitive) {
                        "primitive"
                    } else {
                        "unsupported"
                    }
                    && row.try_get::<_, String>(4)?
                        == if matches!(value_type.mutability(), ValueTypeMutability::Immutable) {
                            "immutable"
                        } else {
                            "unsupported"
                        }
                    && row.try_get::<_, String>(5)?
                        == if matches!(value_type.persistence(), ValueTypePersistence::Persistable)
                        {
                            "persistable"
                        } else {
                            "transient"
                        }
                    && row.try_get::<_, String>(6)? == value_type.representation_contract(),
                "standard value-type contract facts differ from the verified snapshot",
            )?;
            require(
                row.try_get::<_, Vec<u8>>(7)? == origin.source_unit().to_bytes()
                    && row.try_get::<_, i64>(8)? == i64::from(origin.byte_start())
                    && row.try_get::<_, i64>(9)? == i64::from(origin.byte_end()),
                "standard value-type origin differs from the verified snapshot",
            )?;
        }

        let bindings = session
            .client()
            .query(
                "SELECT type_binding_id, kind, name_parts, target_type_id,
                        source_unit_id, source_start, source_end
                 FROM _orna_kernel.standard_catalogue_type_bindings
                 WHERE standard_library_revision_id = $1 ORDER BY type_binding_id",
                &[&standard_revision],
            )
            .await?;
        require(
            bindings.len() == standard.catalogue().type_bindings().len(),
            "standard type-binding row count differs from the verified snapshot",
        )?;
        for binding in standard.catalogue().type_bindings() {
            let row = bindings
                .iter()
                .find(|row| {
                    row.try_get::<_, Vec<u8>>(0).ok() == Some(binding.id().to_bytes().to_vec())
                })
                .ok_or_else(|| failure("verified standard type-binding row is missing"))?;
            let (kind, parts) = match binding.name() {
                TypeLookupName::Qualified(name) => ("qualified", name.parts().to_vec()),
                TypeLookupName::Prelude(name) => ("prelude", name.words().to_vec()),
                _ => return Err(failure("standard type binding has an unknown name shape")),
            };
            let origin = standard
                .origins()
                .iter()
                .find(|origin| origin.identity() == DefinitionIdentity::TypeBinding(binding.id()))
                .ok_or_else(|| failure("verified standard type-binding origin is missing"))?
                .source();
            require(
                row.try_get::<_, String>(1)? == kind
                    && row.try_get::<_, Vec<String>>(2)? == parts
                    && row.try_get::<_, Vec<u8>>(3)? == binding.target().to_bytes()
                    && row.try_get::<_, Vec<u8>>(4)? == origin.source_unit().to_bytes()
                    && row.try_get::<_, i64>(5)? == i64::from(origin.byte_start())
                    && row.try_get::<_, i64>(6)? == i64::from(origin.byte_end()),
                "standard type-binding row differs from the verified snapshot",
            )?;
        }

        let field_rows = session
            .client()
            .query(
                "SELECT owner_type_id, field_id, type_kind, scalar_type, target_type_id,
                        value_type_id, value_standard_library_revision_id
                 FROM _orna_kernel.catalogue_fields
                 WHERE catalogue_revision_id = $1 ORDER BY owner_type_id, ordinal",
                &[&catalogue_id],
            )
            .await?;
        require(
            field_rows.len() == 1,
            "standard application fixture must retain one object field row",
        )?;
        require(
            field_rows[0].try_get::<_, Vec<u8>>(0)? == object.id().to_bytes()
                && field_rows[0].try_get::<_, Vec<u8>>(1)? == field.id().to_bytes()
                && field_rows[0].try_get::<_, String>(2)? == "value"
                && field_rows[0].try_get::<_, Option<String>>(3)?.is_none()
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(4)?.is_none()
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(5)? == Some(boolean_type.clone())
                && field_rows[0].try_get::<_, Option<Vec<u8>>>(6)?
                    == Some(standard_revision.clone()),
            "application field did not persist the exact Boolean value tuple and pin",
        )?;

        let parameter_rows = session
            .client()
            .query(
                "SELECT function_id, parameter_id, ordinal, type_kind, scalar_type,
                        target_type_id, value_type_id, value_standard_library_revision_id
                 FROM _orna_kernel.catalogue_function_parameters
                 WHERE catalogue_revision_id = $1 ORDER BY function_id, ordinal",
                &[&catalogue_id],
            )
            .await?;
        let value_parameters = parameter_rows
            .iter()
            .filter(|row| row.try_get::<_, String>(3).ok().as_deref() == Some("value"))
            .collect::<Vec<_>>();
        let reference_parameters = parameter_rows
            .iter()
            .filter(|row| row.try_get::<_, String>(3).ok().as_deref() == Some("reference"))
            .collect::<Vec<_>>();
        let expected_value_parameters = active
            .catalogue()
            .functions()
            .iter()
            .flat_map(|function| function.parameters())
            .filter(|parameter| parameter.resolved_type().value_type().is_some())
            .count();
        let expected_reference_parameters = active
            .catalogue()
            .functions()
            .iter()
            .flat_map(|function| function.parameters())
            .filter(|parameter| parameter.resolved_type().reference_target().is_some())
            .count();
        require(
            value_parameters.len() == expected_value_parameters
                && reference_parameters.len() == expected_reference_parameters,
            "application parameter rows differ from the active Value and REF families",
        )?;
        for row in value_parameters {
            require(
                row.try_get::<_, Option<String>>(4)?.is_none()
                    && row.try_get::<_, Option<Vec<u8>>>(5)?.is_none()
                    && row.try_get::<_, Option<Vec<u8>>>(6)? == Some(boolean_type.clone())
                    && row.try_get::<_, Option<Vec<u8>>>(7)? == Some(standard_revision.clone()),
                "application parameter did not persist the exact Boolean value tuple and pin",
            )?;
        }
        let object_id = object.id().to_bytes().to_vec();
        for row in reference_parameters {
            require(
                row.try_get::<_, Option<String>>(4)?.is_none()
                    && row.try_get::<_, Option<Vec<u8>>>(5)? == Some(object_id.clone())
                    && row.try_get::<_, Option<Vec<u8>>>(6)?.is_none()
                    && row.try_get::<_, Option<Vec<u8>>>(7)?.is_none(),
                "REF parameter did not retain its exact object target and null value pin",
            )?;
        }
        for function in active.catalogue().functions() {
            for parameter in function.parameters() {
                let row = parameter_rows
                    .iter()
                    .find(|row| {
                        row.try_get::<_, Vec<u8>>(0).ok() == Some(function.id().to_bytes().to_vec())
                            && row.try_get::<_, Vec<u8>>(1).ok()
                                == Some(parameter.id().to_bytes().to_vec())
                            && row.try_get::<_, i64>(2).ok() == Some(i64::from(parameter.ordinal()))
                    })
                    .ok_or_else(|| failure("active function parameter row is missing"))?;
                if let Some(value_type) = parameter.resolved_type().value_type() {
                    require(
                        row.try_get::<_, String>(3)? == "value"
                            && row.try_get::<_, Option<Vec<u8>>>(6)?
                                == Some(value_type.to_bytes().to_vec())
                            && row.try_get::<_, Option<Vec<u8>>>(7)?
                                == Some(standard_revision.clone()),
                        "active Value parameter does not match its exact durable row",
                    )?;
                } else if let Some(target) = parameter.resolved_type().reference_target() {
                    require(
                        row.try_get::<_, String>(3)? == "reference"
                            && row.try_get::<_, Option<Vec<u8>>>(5)?
                                == Some(target.to_bytes().to_vec())
                            && row.try_get::<_, Option<Vec<u8>>>(6)?.is_none()
                            && row.try_get::<_, Option<Vec<u8>>>(7)?.is_none(),
                        "active REF parameter does not match its exact durable row",
                    )?;
                } else {
                    return Err(failure(
                        "active parameter has an unsupported resolved shape",
                    ));
                }
            }
        }

        let return_rows = session
            .client()
            .query(
                "SELECT function_id, ordinal, type_kind, scalar_type, target_type_id,
                        value_type_id, value_standard_library_revision_id
                 FROM _orna_kernel.catalogue_function_return_columns
                 WHERE catalogue_revision_id = $1 ORDER BY function_id, ordinal",
                &[&catalogue_id],
            )
            .await?;
        let value_return_rows = return_rows
            .iter()
            .filter(|row| row.try_get::<_, String>(2).ok().as_deref() == Some("value"))
            .collect::<Vec<_>>();
        let reference_return_rows = return_rows
            .iter()
            .filter(|row| row.try_get::<_, String>(2).ok().as_deref() == Some("reference"))
            .collect::<Vec<_>>();
        let expected_value_returns = active
            .catalogue()
            .functions()
            .iter()
            .filter_map(|function| match function.return_type() {
                FunctionReturn::Rows(columns) => Some(columns),
                FunctionReturn::Single(_) => None,
            })
            .flatten()
            .filter(|column| column.resolved_type().value_type().is_some())
            .count();
        let expected_reference_returns = active
            .catalogue()
            .functions()
            .iter()
            .filter_map(|function| match function.return_type() {
                FunctionReturn::Rows(columns) => Some(columns),
                FunctionReturn::Single(_) => None,
            })
            .flatten()
            .filter(|column| column.resolved_type().reference_target().is_some())
            .count();
        require(
            value_return_rows.len() == expected_value_returns
                && reference_return_rows.len() == expected_reference_returns,
            "application ROWS return rows differ from the active Value and REF families",
        )?;
        for function in active.catalogue().functions() {
            if let FunctionReturn::Rows(columns) = function.return_type() {
                for column in columns {
                    let row = return_rows
                        .iter()
                        .find(|row| {
                            row.try_get::<_, Vec<u8>>(0).ok()
                                == Some(function.id().to_bytes().to_vec())
                                && row.try_get::<_, i64>(1).ok()
                                    == Some(i64::from(column.ordinal()))
                        })
                        .ok_or_else(|| failure("active ROWS return row is missing"))?;
                    if let Some(value_type) = column.resolved_type().value_type() {
                        require(
                            row.try_get::<_, String>(2)? == "value"
                                && row.try_get::<_, Option<Vec<u8>>>(5)?
                                    == Some(value_type.to_bytes().to_vec())
                                && row.try_get::<_, Option<Vec<u8>>>(6)?
                                    == Some(standard_revision.clone()),
                            "active Value ROWS return does not match its exact durable row",
                        )?;
                    } else if let Some(target) = column.resolved_type().reference_target() {
                        require(
                            row.try_get::<_, String>(2)? == "reference"
                                && row.try_get::<_, Option<Vec<u8>>>(4)?
                                    == Some(target.to_bytes().to_vec())
                                && row.try_get::<_, Option<Vec<u8>>>(5)?.is_none()
                                && row.try_get::<_, Option<Vec<u8>>>(6)?.is_none(),
                            "active REF ROWS return does not match its exact durable row",
                        )?;
                    } else {
                        return Err(failure(
                            "active ROWS return has an unsupported resolved shape",
                        ));
                    }
                }
            }
        }

        let single_rows = session
            .client()
            .query(
                "SELECT function_id, return_type_kind, return_scalar_type,
                        return_target_type_id, return_value_type_id,
                        return_standard_library_revision_id
                 FROM _orna_kernel.catalogue_functions
                 WHERE catalogue_revision_id = $1 AND return_shape = 'single'",
                &[&catalogue_id],
            )
            .await?;
        let expected_single_count = active
            .catalogue()
            .functions()
            .iter()
            .filter(|function| matches!(function.return_type(), FunctionReturn::Single(_)))
            .count();
        require(
            single_rows.len() == expected_single_count,
            "application SINGLE return rows differ from the active catalogue",
        )?;
        for function in active.catalogue().functions() {
            if let FunctionReturn::Single(resolved) = function.return_type() {
                let row = single_rows
                    .iter()
                    .find(|row| {
                        row.try_get::<_, Vec<u8>>(0).ok() == Some(function.id().to_bytes().to_vec())
                    })
                    .ok_or_else(|| failure("active SINGLE return row is missing"))?;
                if let Some(value_type) = resolved.value_type() {
                    require(
                        row.try_get::<_, String>(1)? == "value"
                            && row.try_get::<_, Option<Vec<u8>>>(4)?
                                == Some(value_type.to_bytes().to_vec())
                            && row.try_get::<_, Option<Vec<u8>>>(5)?
                                == Some(standard_revision.clone()),
                        "active Value SINGLE return does not match its exact durable row",
                    )?;
                } else {
                    return Err(failure(
                        "active SINGLE return has an unsupported resolved shape",
                    ));
                }
            }
        }

        let references = session
            .client()
            .query(
                "SELECT source_function_id, source_function_revision_id, ordinal,
                        target_definition_id, target_standard_library_revision_id
                 FROM _orna_kernel.definition_references
                 WHERE catalogue_revision_id = $1 AND target_kind = 'value_type'
                 ORDER BY source_function_id, ordinal",
                &[&catalogue_id],
            )
            .await?;
        let expected_references = active
            .references()
            .iter()
            .filter_map(|reference| match reference.target() {
                DefinitionReferenceTarget::ValueType(target) => Some((
                    reference.source_function().to_bytes().to_vec(),
                    reference.source_revision().to_bytes().to_vec(),
                    reference.ordinal() as i64,
                    target.to_bytes().to_vec(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        require(
            !expected_references.is_empty(),
            "standard application fixture did not retain a ValueType reference",
        )?;
        require(
            references.len() == expected_references.len(),
            "ValueType reference rows differ from the recovered application evidence",
        )?;
        for expected in expected_references {
            require(
                references.iter().any(|row| {
                    row.try_get::<_, Vec<u8>>(0).ok() == Some(expected.0.clone())
                        && row.try_get::<_, Vec<u8>>(1).ok() == Some(expected.1.clone())
                        && row.try_get::<_, i64>(2).ok() == Some(expected.2)
                        && row.try_get::<_, Vec<u8>>(3).ok() == Some(expected.3.clone())
                        && row.try_get::<_, Vec<u8>>(4).ok() == Some(standard_revision.clone())
                }),
                "ValueType reference row did not retain its exact target pin",
            )?;
        }
        Ok(())
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "standard upgrade storage inspection",
    )
}

fn require_standard_context(
    active: &ActiveDatabaseRevision,
    expected: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> TestResult<()> {
    let selected = active
        .catalogue_hash_context()
        .standard()
        .ok_or_else(|| failure("active catalogue did not retain a standard context"))?;
    require(
        selected.revision() == expected.revision()
            && selected.catalogue().revision() == expected.catalogue().revision()
            && selected.source().bundle() == expected.source().bundle()
            && selected.source().id() == expected.source().id()
            && selected.source().bundle_hash() == expected.source().bundle_hash()
            && selected.source().revision_hash() == expected.source().revision_hash()
            && selected.digest() == expected.digest(),
        "active catalogue standard context does not match the verified upgrade snapshot",
    )
}

fn same_recovered(left: &ActiveDatabaseRevision, right: &ActiveDatabaseRevision) -> bool {
    left.pair() == right.pair()
        && left.source() == right.source()
        && left.catalogue().revision() == right.catalogue().revision()
        && left.catalogue().schemas() == right.catalogue().schemas()
        && left.catalogue().object_types() == right.catalogue().object_types()
        && left.catalogue().enum_types() == right.catalogue().enum_types()
        && left.catalogue().functions() == right.catalogue().functions()
        && left.catalogue_hash() == right.catalogue_hash()
        && left.expressions() == right.expressions()
        && same_members(left.origins(), right.origins())
        && left.references() == right.references()
        && left.function_revisions() == right.function_revisions()
        && left.historical_function_revisions() == right.historical_function_revisions()
}

async fn install_failure_point(
    database: &TestDatabase,
    point: FailurePoint,
    candidate: &DeployableRevision,
) -> TestResult<()> {
    let session = database.open().await?;
    let statement = match point {
        FailurePoint::SourceBundle => source_bundle_failure_trigger(candidate)?,
        FailurePoint::CatalogueSchema => catalogue_schema_failure_trigger(candidate),
        FailurePoint::FunctionArtifact => function_artifact_failure_trigger(candidate),
        FailurePoint::DefinitionReference => definition_reference_failure_trigger(candidate),
        FailurePoint::DeferredReference => "
            CREATE FUNCTION _orna_kernel.test_apply_fail() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'deferred definition reference' USING ERRCODE = 'P0001'; END $$;
            CREATE CONSTRAINT TRIGGER deferred_definition_reference
            AFTER INSERT ON _orna_kernel.definition_references
            DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_fail();
            CREATE FUNCTION _orna_kernel.test_status_sentinel() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'deferred reached status transition' USING ERRCODE = 'P0001'; END $$;
            CREATE TRIGGER deferred_status_sentinel BEFORE UPDATE OF status
            ON _orna_kernel.function_revisions FOR EACH ROW
            EXECUTE FUNCTION _orna_kernel.test_status_sentinel();
            CREATE FUNCTION _orna_kernel.test_pointer_sentinel() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'deferred reached active pointer' USING ERRCODE = 'P0001'; END $$;
            CREATE TRIGGER deferred_pointer_sentinel BEFORE UPDATE ON _orna_kernel.active_revision
            FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_pointer_sentinel();".into(),
        FailurePoint::StatusSweep => "
            CREATE FUNCTION _orna_kernel.test_apply_status_invalid() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN NEW.status := 'invalid'; RETURN NEW; END $$;
            CREATE TRIGGER rewrite_active_status BEFORE UPDATE OF status
            ON _orna_kernel.function_revisions FOR EACH ROW
            WHEN (NEW.status = 'active') EXECUTE FUNCTION _orna_kernel.test_apply_status_invalid();
            CREATE FUNCTION _orna_kernel.test_pointer_sentinel() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN RAISE EXCEPTION 'status sweep reached active pointer' USING ERRCODE = 'P0001'; END $$;
            CREATE TRIGGER status_pointer_sentinel BEFORE UPDATE ON _orna_kernel.active_revision
            FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_pointer_sentinel();".into(),
        FailurePoint::ActivePointer => fail_trigger("active_revision", "before_active_pointer", "BEFORE", "UPDATE", "before active pointer"),
        FailurePoint::PostPointerRecovery => "
            CREATE FUNCTION _orna_kernel.test_apply_tamper_source() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
              UPDATE _orna_kernel.source_revisions
              SET content_hash = decode(repeat('00', 32), 'hex')
              WHERE id = NEW.source_revision_id;
              RETURN NEW;
            END $$;
            CREATE TRIGGER tamper_after_active_pointer AFTER UPDATE
            ON _orna_kernel.active_revision FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_tamper_source();".into(),
    };
    session.client().batch_execute(&statement).await?;
    session.shutdown().await
}

fn source_bundle_failure_trigger(candidate: &DeployableRevision) -> TestResult<String> {
    let object = candidate
        .candidate()
        .object_types()
        .first()
        .ok_or_else(|| failure("source-bundle rollback fixture requires an object type"))?;
    let expected_relation = relation(object.id());
    Ok(prerequisite_trigger(
        "source_bundles",
        "before_source_bundle",
        format!("pg_catalog.to_regclass('{expected_relation}') IS NOT NULL"),
        "physical plan missing before source bundle",
        "before source bundle",
    ))
}

fn catalogue_schema_failure_trigger(candidate: &DeployableRevision) -> String {
    let state = CandidateSqlState::from_candidate(candidate);
    let source_complete = state.source_complete_condition();
    let catalogue_present = state.catalogue_revision_present_condition();
    prerequisite_trigger(
        "catalogue_schemas",
        "before_catalogue_schema",
        format!("{source_complete} AND {catalogue_present}"),
        "source or catalogue state missing before catalogue schema",
        "before catalogue schema",
    )
}

fn function_artifact_failure_trigger(candidate: &DeployableRevision) -> String {
    let state = CandidateSqlState::from_candidate(candidate);
    let semantics_complete = state.semantics_complete_condition();
    let revisions_complete = state.function_revisions_complete_condition();
    prerequisite_trigger(
        "function_artifacts",
        "before_function_artifact",
        format!("{semantics_complete} AND {revisions_complete}"),
        "candidate semantics or revision missing before function artifact",
        "before function artifact",
    )
}

fn definition_reference_failure_trigger(candidate: &DeployableRevision) -> String {
    let state = CandidateSqlState::from_candidate(candidate);
    let artifacts_complete = state.artifacts_complete_condition();
    prerequisite_trigger(
        "definition_references",
        "before_definition_reference",
        artifacts_complete,
        "candidate artifact missing before definition reference",
        "before definition reference",
    )
}

fn prerequisite_trigger(
    table: &str,
    trigger: &str,
    prerequisite: String,
    missing_marker: &str,
    expected_marker: &str,
) -> String {
    format!(
        "CREATE FUNCTION _orna_kernel.test_apply_fail() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NOT ({prerequisite}) THEN
             RAISE EXCEPTION '{missing_marker}' USING ERRCODE = 'P0001';
           END IF;
           RAISE EXCEPTION '{expected_marker}' USING ERRCODE = 'P0001';
         END $$;
         CREATE TRIGGER {trigger} BEFORE INSERT ON _orna_kernel.{table}
         FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_fail();"
    )
}

#[derive(Debug)]
struct CandidateSqlState {
    source_bundle_id: String,
    source_revision_id: String,
    catalogue_revision_id: String,
    source_unit_count: usize,
    schema_count: usize,
    object_type_count: usize,
    expression_count: usize,
    field_count: usize,
    function_count: usize,
    parameter_count: usize,
    return_column_count: usize,
    new_function_revision_ids: Vec<String>,
}

impl CandidateSqlState {
    fn from_candidate(candidate: &DeployableRevision) -> Self {
        let catalogue = candidate.candidate();
        let functions = catalogue.functions();
        Self {
            source_bundle_id: hex_bytes(candidate.source().bundle().to_bytes()),
            source_revision_id: hex_bytes(candidate.source().id().to_bytes()),
            catalogue_revision_id: hex_bytes(catalogue.revision().to_bytes()),
            source_unit_count: candidate.source().units().len(),
            schema_count: catalogue.schemas().len(),
            object_type_count: catalogue.object_types().len(),
            expression_count: candidate.expressions().len(),
            field_count: catalogue
                .object_types()
                .iter()
                .map(|object| object.fields().len())
                .sum(),
            function_count: functions.len(),
            parameter_count: functions
                .iter()
                .map(|function| function.parameters().len())
                .sum(),
            return_column_count: functions
                .iter()
                .map(|function| match function.return_type() {
                    FunctionReturn::Single(_) => 0,
                    FunctionReturn::Rows(columns) => columns.len(),
                })
                .sum(),
            new_function_revision_ids: candidate
                .new_function_revisions()
                .iter()
                .map(|revision| hex_bytes(revision.id().to_bytes()))
                .collect(),
        }
    }

    fn source_complete_condition(&self) -> String {
        let source_bundle = self.source_bundle();
        let source_revision = self.source_revision();
        format!(
            "EXISTS (SELECT 1 FROM _orna_kernel.source_bundles
                      WHERE id = {source_bundle})
             AND (SELECT count(*) FROM _orna_kernel.source_units
                  WHERE bundle_id = {source_bundle}) = {source_unit_count}
             AND EXISTS (SELECT 1 FROM _orna_kernel.source_revisions
                         WHERE id = {source_revision} AND bundle_id = {source_bundle})",
            source_unit_count = self.source_unit_count,
        )
    }

    fn catalogue_revision_present_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        let source_revision = self.source_revision();
        format!(
            "EXISTS (SELECT 1 FROM _orna_kernel.catalogue_revisions
                      WHERE id = {catalogue_revision}
                        AND source_revision_id = {source_revision})"
        )
    }

    fn semantics_complete_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        format!(
            "{catalogue_present}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_schemas
                  WHERE catalogue_revision_id = {catalogue_revision}) = {schema_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_object_types
                  WHERE catalogue_revision_id = {catalogue_revision}) = {object_type_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_expressions
                  WHERE catalogue_revision_id = {catalogue_revision}) = {expression_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_fields
                  WHERE catalogue_revision_id = {catalogue_revision}) = {field_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_functions
                  WHERE catalogue_revision_id = {catalogue_revision}) = {function_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters
                  WHERE catalogue_revision_id = {catalogue_revision}) = {parameter_count}
             AND (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns
                  WHERE catalogue_revision_id = {catalogue_revision}) = {return_column_count}",
            catalogue_present = self.catalogue_revision_present_condition(),
            schema_count = self.schema_count,
            object_type_count = self.object_type_count,
            expression_count = self.expression_count,
            field_count = self.field_count,
            function_count = self.function_count,
            parameter_count = self.parameter_count,
            return_column_count = self.return_column_count,
        )
    }

    fn function_revisions_complete_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        conjunction(self.new_function_revision_ids.iter().map(|revision_id| {
            format!(
                "EXISTS (SELECT 1 FROM _orna_kernel.function_revisions
                          WHERE id = {} AND introduced_catalogue_revision_id = {catalogue_revision})",
                bytea(revision_id),
            )
        }))
    }

    fn artifacts_complete_condition(&self) -> String {
        let catalogue_revision = self.catalogue_revision();
        conjunction(self.new_function_revision_ids.iter().map(|revision_id| {
            format!(
                "EXISTS (SELECT 1
                          FROM _orna_kernel.function_artifacts AS artifact
                          JOIN _orna_kernel.function_revisions AS revision
                            ON revision.id = artifact.function_revision_id
                          WHERE revision.id = {}
                            AND revision.introduced_catalogue_revision_id = {catalogue_revision})",
                bytea(revision_id),
            )
        }))
    }

    fn source_bundle(&self) -> String {
        bytea(&self.source_bundle_id)
    }

    fn source_revision(&self) -> String {
        bytea(&self.source_revision_id)
    }

    fn catalogue_revision(&self) -> String {
        bytea(&self.catalogue_revision_id)
    }
}

fn conjunction(conditions: impl IntoIterator<Item = String>) -> String {
    let conditions = conditions.into_iter().collect::<Vec<_>>();
    if conditions.is_empty() {
        "TRUE".into()
    } else {
        conditions.join(" AND ")
    }
}

fn bytea(hex: &str) -> String {
    format!("decode('{hex}', 'hex')")
}

fn hex_bytes(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fail_trigger(table: &str, trigger: &str, timing: &str, event: &str, marker: &str) -> String {
    format!(
        "CREATE FUNCTION _orna_kernel.test_apply_fail() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION '{marker}' USING ERRCODE = 'P0001'; END $$;
         CREATE TRIGGER {trigger} {timing} {event} ON _orna_kernel.{table}
         FOR EACH ROW EXECUTE FUNCTION _orna_kernel.test_apply_fail();"
    )
}

fn assert_failure_shape(point: FailurePoint, error: &PostgresKernelError) -> TestResult<()> {
    match point {
        FailurePoint::StatusSweep => require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.function_revisions",
                    ..
                }
            ),
            "status rewrite must fail the global status sweep with a function revision invariant",
        ),
        FailurePoint::PostPointerRecovery => require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.source_revisions",
                    ..
                }
            ),
            "post-pointer source tampering must fail recovery with a source invariant",
        ),
        _ => match error {
            PostgresKernelError::Database(source) => {
                let marker = source
                    .as_db_error()
                    .map(|detail| detail.message())
                    .unwrap_or("");
                require(
                    source.code().is_some_and(|code| code.code() == "P0001")
                        && marker == trigger_marker(point).expect("trigger point has a marker"),
                    "trigger error did not preserve SQLSTATE P0001 and its exact marker",
                )
            }
            _ => Err(failure(format!(
                "expected PostgreSQL P0001 trigger failure, got {error}"
            ))),
        },
    }
}

fn trigger_marker(point: FailurePoint) -> Option<&'static str> {
    match point {
        FailurePoint::SourceBundle => Some("before source bundle"),
        FailurePoint::CatalogueSchema => Some("before catalogue schema"),
        FailurePoint::FunctionArtifact => Some("before function artifact"),
        FailurePoint::DefinitionReference => Some("before definition reference"),
        FailurePoint::DeferredReference => Some("deferred definition reference"),
        FailurePoint::ActivePointer => Some("before active pointer"),
        FailurePoint::StatusSweep | FailurePoint::PostPointerRecovery => None,
    }
}

fn kernel(database: &TestDatabase) -> TestResult<PostgresKernel> {
    Ok(PostgresKernel::from_str(&database.connection_string())?)
}

fn named_kernel(database: &TestDatabase, application_name: &str) -> TestResult<PostgresKernel> {
    let mut config = database.config()?;
    config.application_name(application_name);
    Ok(PostgresKernel::new(config))
}

fn standard_context_candidate(expected_base: RevisionPair) -> TestResult<DeployableRevision> {
    let context = CatalogueHashContext::version_two(verified_empty_non_golden_standard()?);
    let bundle = SourceBundleId::from_bytes([0x92; 16]);
    let bundle_hash = source_bundle_digest(&[])?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0x93; 16]),
        Some(expected_base.source()),
        vec![],
        bundle_hash,
        source_revision_record_digest(bundle, Some(expected_base.source()), bundle_hash)?,
    )?;
    let catalogue =
        CatalogueSnapshot::new(CatalogueRevisionId::from_bytes([0x94; 16]), vec![], vec![])?;
    let catalogue_hash = catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[])?;

    Ok(DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source,
            expected_base.catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(vec![], vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        context,
    )?)
}

fn verified_empty_non_golden_standard()
-> TestResult<orna_core::revision::VerifiedStandardLibrarySnapshot> {
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([4; 16]),
        0,
        "std/malformed.orna",
        EMPTY_STANDARD_SOURCE,
        source_unit_content_digest(EMPTY_STANDARD_SOURCE)?,
    )?;
    let bundle = SourceBundleId::from_bytes([5; 16]);
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit))?;
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([6; 16]),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(bundle, None, bundle_hash)?,
    )?;
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![],
        vec![],
        vec![],
        vec![],
    )?;
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([7; 16]),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        vec![],
        Sha256Digest::from_bytes(EMPTY_STANDARD_DIGEST),
    )?;

    Ok(verify_standard_library_snapshot(snapshot)?)
}

fn failed_apply_error(
    result: Result<ActiveDatabaseRevision, PostgresKernelError>,
    success_message: &'static str,
) -> TestResult<PostgresKernelError> {
    result.err().ok_or_else(|| failure(success_message))
}

fn candidate(source: &str, active: &ActiveDatabaseRevision) -> TestResult<DeployableRevision> {
    let bundle = SourceBundle::new([SourceUnit::new("main.orna", source)])?;
    let report = check(&bundle, active.catalogue());
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "compiler diagnostics prevented candidate preparation: {:?}",
            report.diagnostics()
        )));
    }
    Ok(prepare(&report, active.pair(), active)?)
}

fn standard_application_candidate(
    source: &str,
    active: &ActiveDatabaseRevision,
    upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<DeployableRevision> {
    let context = StandardApplicationCheckContext::try_new(
        active.catalogue(),
        upgrade.checked_standard_library(),
    )
    .map_err(|error| failure(format!("standard application context failed: {error}")))?;
    let bundle = SourceBundle::new([SourceUnit::new("main.orna", source)])?;
    let report = check_standard_application(&bundle, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "standard application diagnostics prevented candidate preparation: {:?}",
            report.diagnostics()
        )));
    }
    Ok(prepare_standard_application(
        &report,
        active.pair(),
        active,
    )?)
}

fn only_revision(active: &ActiveDatabaseRevision) -> TestResult<&FunctionRevisionRecord> {
    active
        .function_revisions()
        .first()
        .filter(|_| active.function_revisions().len() == 1)
        .ok_or_else(|| failure("expected exactly one active function revision"))
}

fn require_rename_semantics(
    candidate: &DeployableRevision,
    object: TypeId,
    field: FieldId,
    function: orna_core::FunctionId,
    revision: orna_core::FunctionRevisionId,
) -> TestResult<()> {
    let renamed_object = candidate
        .candidate()
        .object_type_by_id(object)
        .ok_or_else(|| failure("renamed candidate lost the original object TypeId"))?;
    let renamed_field = renamed_object
        .field_by_id(field)
        .ok_or_else(|| failure("renamed candidate lost the original FieldId"))?;
    require(
        renamed_field.name() == "primary_email"
            && candidate.candidate().functions().iter().any(|definition| {
                definition.id() == function && definition.current_revision() == revision
            }),
        "renamed candidate changed stable semantic identities",
    )?;

    let source_unit = candidate.source().units()[0].id();
    let declaration_origin = token_origin(
        source_unit,
        FIELD_RENAME_FINAL_SOURCE,
        "CREATE TYPE people.person AS OBJECT (",
        "primary_email TEXT NOT NULL",
    )?;
    let query_origin = token_origin(
        source_unit,
        FIELD_RENAME_FINAL_SOURCE,
        "AS SELECT p.",
        "primary_email",
    )?;
    let field_origin = candidate
        .origins()
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Field {
                    owner: object,
                    field,
                }
        })
        .ok_or_else(|| failure("renamed field definition origin is absent"))?;
    require(
        field_origin.source() == declaration_origin,
        "renamed field origin does not select the final CREATE TYPE token",
    )?;
    let query_reference = candidate
        .references()
        .iter()
        .find(|reference| {
            reference.source_function() == function
                && reference.kind() == DefinitionReferenceKind::QueryField
                && reference.target()
                    == DefinitionReferenceTarget::Field {
                        owner: object,
                        field,
                    }
        })
        .ok_or_else(|| failure("renamed QueryField reference is absent"))?;
    require(
        query_reference.source_revision() == revision
            && query_reference.source_origin() == query_origin,
        "renamed QueryField reference changed identity or token origin",
    )
}

fn token_origin(
    source_unit: orna_core::SourceUnitId,
    source: &str,
    anchor: &str,
    token: &str,
) -> TestResult<SourceOrigin> {
    let anchor_start = source
        .find(anchor)
        .ok_or_else(|| failure("source-origin anchor is absent"))?;
    let token_start = source[anchor_start + anchor.len()..]
        .find(token)
        .map(|offset| anchor_start + anchor.len() + offset)
        .ok_or_else(|| failure("source-origin token is absent after its anchor"))?;
    let token_end = token_start + token.len();
    Ok(SourceOrigin::new(
        source_unit,
        u32::try_from(token_start)?,
        u32::try_from(token_end)?,
    )?)
}

struct RenameProof {
    object: TypeId,
    field: FieldId,
    function: orna_core::FunctionId,
    revision: FunctionRevisionRecord,
    immutable: ImmutableRevisionRows,
    physical: PhysicalCatalogue,
    stored_object: ObjectId,
}

struct UniqueReferenceProof {
    person: TypeId,
    assignment: TypeId,
    field: FieldId,
    person_physical: PhysicalCatalogue,
    assignment_physical: PhysicalCatalogue,
}

async fn require_unique_reference_state(
    database: &TestDatabase,
    active: &ActiveDatabaseRevision,
    proof: &UniqueReferenceProof,
    field_name: &str,
) -> TestResult<()> {
    let person = active
        .catalogue()
        .object_type_by_id(proof.person)
        .ok_or_else(|| failure("required unique reference recovery lost the person TypeId"))?;
    let assignment = active
        .catalogue()
        .object_type_by_id(proof.assignment)
        .ok_or_else(|| failure("required unique reference recovery lost the assignment TypeId"))?;
    let reference = assignment
        .field_by_id(proof.field)
        .ok_or_else(|| failure("required unique reference recovery lost the FieldId"))?;
    require(
        person.name().parts() == ["assignments", "person"]
            && assignment.name().parts() == ["assignments", "assignment"]
            && assignment.fields().len() == 1
            && reference.name() == field_name
            && reference.is_required_unique_reference()
            && reference.resolved_type() == ResolvedType::reference(proof.person),
        "required unique reference recovery changed semantic identity or field properties",
    )?;
    require(
        physical_catalogue(database, proof.person).await? == proof.person_physical
            && physical_catalogue(database, proof.assignment).await? == proof.assignment_physical,
        "required unique reference replay or rename changed physical identities",
    )
}

async fn require_rename_state(
    database: &TestDatabase,
    active: &ActiveDatabaseRevision,
    proof: &RenameProof,
) -> TestResult<()> {
    let current_object = active
        .catalogue()
        .object_type_by_id(proof.object)
        .ok_or_else(|| failure("recovered rename lost the original TypeId"))?;
    let current_field = current_object
        .field_by_id(proof.field)
        .ok_or_else(|| failure("recovered rename lost the original FieldId"))?;
    require(
        current_field.name() == "primary_email"
            && only_revision(active)? == &proof.revision
            && active.catalogue().functions()[0].current_revision() == proof.revision.id()
            && active.historical_function_revisions().is_empty(),
        "recovered rename changed identity, immutable revision, or history",
    )?;
    require(
        immutable_rows(database, &proof.revision).await? == proof.immutable,
        "field rename rewrote the immutable function revision or artifact",
    )?;
    require(
        physical_catalogue(database, proof.object).await? == proof.physical,
        "field rename changed the stable-ID PostgreSQL physical catalogue",
    )?;
    require_private_text(
        database,
        proof.object,
        proof.field,
        proof.stored_object,
        "kept@example.test",
    )
    .await?;
    let result = kernel(database)?
        .execute_server_select(proof.function)
        .await?;
    require(
        result.rows().rows().len() == 1
            && result.rows().rows()[0].values()
                == [RuntimeValue::Text(String::from("kept@example.test"))],
        "renamed SERVER SELECT did not read the pre-existing private field value",
    )
}

#[derive(Debug, Eq, PartialEq)]
struct PhysicalCatalogue {
    relation: (i64, i64, String, String),
    attributes: Vec<(i16, String, i64, i32, bool, bool)>,
    constraints: Vec<(i64, String, String)>,
    indexes: Vec<(i64, String, String)>,
}

async fn physical_catalogue(
    database: &TestDatabase,
    object: TypeId,
) -> TestResult<PhysicalCatalogue> {
    let session = database.open().await?;
    let operation = async {
        let relation_name = relation(object);
        let relation_row = session
            .client()
            .query_one(
                "SELECT c.oid::bigint, c.relfilenode::bigint, c.relkind::text,
                        n.nspname || '.' || c.relname
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE c.oid = to_regclass($1)",
                &[&relation_name],
            )
            .await?;
        let attributes = session
            .client()
            .query(
                "SELECT a.attnum, a.attname, a.atttypid::bigint, a.atttypmod,
                        a.attnotnull, a.attisdropped
                 FROM pg_attribute a
                 WHERE a.attrelid = to_regclass($1) AND a.attnum > 0
                 ORDER BY a.attnum",
                &[&relation_name],
            )
            .await?
            .into_iter()
            .map(|row| {
                Ok((
                    row.try_get(0)?,
                    row.try_get(1)?,
                    row.try_get(2)?,
                    row.try_get(3)?,
                    row.try_get(4)?,
                    row.try_get(5)?,
                ))
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
        let constraints = session
            .client()
            .query(
                "SELECT oid::bigint, conname, pg_get_constraintdef(oid, false)
                 FROM pg_constraint WHERE conrelid = to_regclass($1) ORDER BY oid",
                &[&relation_name],
            )
            .await?
            .into_iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
        let indexes = session
            .client()
            .query(
                "SELECT i.indexrelid::bigint, c.relname, pg_get_indexdef(i.indexrelid)
                 FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid
                 WHERE i.indrelid = to_regclass($1) ORDER BY i.indexrelid",
                &[&relation_name],
            )
            .await?
            .into_iter()
            .map(|row| Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?)))
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
        Ok(PhysicalCatalogue {
            relation: (
                relation_row.try_get(0)?,
                relation_row.try_get(1)?,
                relation_row.try_get(2)?,
                relation_row.try_get(3)?,
            ),
            attributes,
            constraints,
            indexes,
        })
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "physical catalogue inspection",
    )
}

async fn insert_private_text(
    database: &TestDatabase,
    object: TypeId,
    field_id: FieldId,
    object_id: ObjectId,
    value: &str,
) -> TestResult<()> {
    private_text(
        database,
        object,
        field_id,
        object_id,
        PrivateTextOperation::Insert(value),
    )
    .await
}

async fn require_private_text(
    database: &TestDatabase,
    object: TypeId,
    field_id: FieldId,
    object_id: ObjectId,
    expected: &str,
) -> TestResult<()> {
    private_text(
        database,
        object,
        field_id,
        object_id,
        PrivateTextOperation::Require(expected),
    )
    .await
}

enum PrivateTextOperation<'a> {
    Insert(&'a str),
    Require(&'a str),
}

async fn private_text(
    database: &TestDatabase,
    object: TypeId,
    field_id: FieldId,
    object_id: ObjectId,
    operation: PrivateTextOperation<'_>,
) -> TestResult<()> {
    let session = database.open().await?;
    let operation = async {
        match operation {
            PrivateTextOperation::Insert(value) => {
                session
                    .client()
                    .execute(
                        &format!(
                            "INSERT INTO {} (_orna_object_id, {}) VALUES ($1, $2)",
                            relation(object),
                            field(field_id)
                        ),
                        &[&object_id.to_bytes().to_vec(), &value],
                    )
                    .await?;
                Ok(())
            }
            PrivateTextOperation::Require(expected) => {
                let row = session
                    .client()
                    .query_one(
                        &format!(
                            "SELECT {} FROM {} WHERE _orna_object_id = $1",
                            field(field_id),
                            relation(object)
                        ),
                        &[&object_id.to_bytes().to_vec()],
                    )
                    .await?;
                let actual: String = row.try_get(0)?;
                require(
                    actual == expected,
                    "pre-existing private field value did not survive the rename",
                )
            }
        }
    }
    .await;
    finish_test_session(operation, session.shutdown().await, "private row operation")
}

fn finish_test_session<T>(
    operation: TestResult<T>,
    shutdown: TestResult<()>,
    context: &str,
) -> TestResult<T> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(shutdown)) => Err(failure(format!(
            "{context} failed: {operation}; session shutdown also failed: {shutdown}"
        ))),
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ImmutableRevisionRows {
    revision_count: i64,
    revision_xmin: String,
    introduced_catalogue_revision_id: Vec<u8>,
    function_id: Vec<u8>,
    revision_number: i64,
    declaration_hash: Vec<u8>,
    semantic_hash: Vec<u8>,
    hash_algorithm: String,
    language_version: String,
    status: String,
    hash_contract_version: i16,
    artifact_count: i64,
    artifact_xmin: String,
    artifact_function_revision_id: Vec<u8>,
    artifact_kind: String,
    artifact_format: String,
    artifact_version: i32,
    artifact_payload: Vec<u8>,
    artifact_hash: Vec<u8>,
    artifact_hash_algorithm: String,
    artifact_hash_contract_version: i16,
}

async fn immutable_rows(
    database: &TestDatabase,
    revision: &FunctionRevisionRecord,
) -> TestResult<ImmutableRevisionRows> {
    let session = database.open().await?;
    let operation = async {
        let revision_id = revision.id().to_bytes().to_vec();
        let revision_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.function_revisions WHERE id = $1",
                &[&revision_id],
            )
            .await?
            .try_get(0)?;
        let revision_row = session
            .client()
            .query_one(
                "SELECT xmin::text, introduced_catalogue_revision_id, function_id,
                    revision_number, content_hash, semantic_ir_hash, hash_algorithm,
                    language_version, status, hash_contract_version
             FROM _orna_kernel.function_revisions
             WHERE id = $1",
                &[&revision_id],
            )
            .await?;
        let artifact_count: i64 = session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.function_artifacts WHERE function_revision_id = $1",
                &[&revision_id],
            )
            .await?
            .try_get(0)?;
        let artifact_row = session
            .client()
            .query_one(
                "SELECT xmin::text, function_revision_id, artifact_kind, format, format_version,
                    payload, content_hash, hash_algorithm, hash_contract_version
             FROM _orna_kernel.function_artifacts
             WHERE function_revision_id = $1",
                &[&revision_id],
            )
            .await?;
        Ok(ImmutableRevisionRows {
            revision_count,
            revision_xmin: revision_row.try_get(0)?,
            introduced_catalogue_revision_id: revision_row.try_get(1)?,
            function_id: revision_row.try_get(2)?,
            revision_number: revision_row.try_get(3)?,
            declaration_hash: revision_row.try_get(4)?,
            semantic_hash: revision_row.try_get(5)?,
            hash_algorithm: revision_row.try_get(6)?,
            language_version: revision_row.try_get(7)?,
            status: revision_row.try_get(8)?,
            hash_contract_version: revision_row.try_get(9)?,
            artifact_count,
            artifact_xmin: artifact_row.try_get(0)?,
            artifact_function_revision_id: artifact_row.try_get(1)?,
            artifact_kind: artifact_row.try_get(2)?,
            artifact_format: artifact_row.try_get(3)?,
            artifact_version: artifact_row.try_get(4)?,
            artifact_payload: artifact_row.try_get(5)?,
            artifact_hash: artifact_row.try_get(6)?,
            artifact_hash_algorithm: artifact_row.try_get(7)?,
            artifact_hash_contract_version: artifact_row.try_get(8)?,
        })
    }
    .await;
    finish_test_session(
        operation,
        session.shutdown().await,
        "immutable row inspection",
    )
}

async fn require_no_candidate_residue(
    database: &TestDatabase,
    candidate: &DeployableRevision,
    base: &ActiveDatabaseRevision,
) -> TestResult<()> {
    let session = database.open().await?;
    let source_bundle = candidate.source().bundle().to_bytes().to_vec();
    let source_revision = candidate.source().id().to_bytes().to_vec();
    let catalogue = candidate.candidate().revision().to_bytes().to_vec();
    let source_bundle_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.source_bundles WHERE id = $1",
            &[&source_bundle],
        )
        .await?
        .try_get(0)?;
    let source_unit_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.source_units WHERE bundle_id = $1",
            &[&source_bundle],
        )
        .await?
        .try_get(0)?;
    let source_revision_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*) FROM _orna_kernel.source_revisions WHERE id = $1",
            &[&source_revision],
        )
        .await?
        .try_get(0)?;
    let catalogue_and_semantic_rows: i64 = session
        .client()
        .query_one(
            "SELECT
                (SELECT count(*) FROM _orna_kernel.catalogue_revisions WHERE id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_schemas WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_object_types WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_expressions WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_fields WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_functions WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_function_parameters WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.catalogue_function_return_columns WHERE catalogue_revision_id = $1)
              + (SELECT count(*) FROM _orna_kernel.definition_references WHERE catalogue_revision_id = $1)",
            &[&catalogue],
        )
        .await?
        .try_get(0)?;
    let mut immutable_rows = 0_i64;
    for revision in candidate.new_function_revisions() {
        let revision_id = revision.id().to_bytes().to_vec();
        immutable_rows += session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.function_revisions WHERE id = $1",
                &[&revision_id],
            )
            .await?
            .try_get::<_, i64>(0)?;
        immutable_rows += session
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.function_artifacts WHERE function_revision_id = $1",
                &[&revision_id],
            )
            .await?
            .try_get::<_, i64>(0)?;
    }
    let names = candidate
        .candidate()
        .object_types()
        .iter()
        .filter(|object| base.catalogue().object_type_by_id(object.id()).is_none())
        .map(|object| relation(object.id()))
        .collect::<Vec<_>>();
    let physical_rows: i64 = session
        .client()
        .query_one(
            "SELECT count(*)
             FROM unnest($1::text[]) AS expected(name)
             WHERE to_regclass(expected.name) IS NOT NULL",
            &[&names],
        )
        .await?
        .try_get(0)?;
    session.shutdown().await?;
    require(
        source_bundle_rows == 0
            && source_unit_rows == 0
            && source_revision_rows == 0
            && catalogue_and_semantic_rows == 0
            && immutable_rows == 0
            && physical_rows == 0,
        "losing apply left source, catalogue, semantic, immutable, artifact, or physical residue",
    )
}

fn require_recovered_new_candidate(
    candidate: &DeployableRevision,
    active: &ActiveDatabaseRevision,
) -> TestResult<()> {
    require_recovered_snapshot(candidate, active)?;
    require(
        same_members(
            active.function_revisions(),
            candidate.new_function_revisions(),
        ),
        "recovered candidate current function revisions differ",
    )?;
    require(
        active.historical_function_revisions().is_empty(),
        "new candidate apply unexpectedly recovered function history",
    )
}

fn require_recovered_snapshot(
    candidate: &DeployableRevision,
    active: &ActiveDatabaseRevision,
) -> TestResult<()> {
    require(
        active.pair() == candidate.candidate_pair(),
        "recovered candidate pair differs",
    )?;
    require(
        active.source() == candidate.source(),
        "recovered candidate source differs",
    )?;
    // Recovery uses durable record order, while preparation uses candidate
    // snapshot order. Stable identities and embedded ordinals carry equality.
    require(
        active.catalogue().revision() == candidate.candidate().revision()
            && same_members(
                active.catalogue().schemas(),
                candidate.candidate().schemas(),
            )
            && same_members(
                active.catalogue().object_types(),
                candidate.candidate().object_types(),
            )
            && same_members(
                active.catalogue().enum_types(),
                candidate.candidate().enum_types(),
            )
            && same_members(
                active.catalogue().functions(),
                candidate.candidate().functions(),
            ),
        "recovered candidate catalogue differs",
    )?;
    require(
        active.catalogue_hash() == candidate.catalogue_hash(),
        "recovered candidate catalogue hash differs",
    )?;
    require(
        same_members(active.expressions(), candidate.expressions()),
        "recovered candidate expressions differ",
    )?;
    require(
        same_members(active.origins(), candidate.origins()),
        "recovered candidate origins differ",
    )?;
    require(
        same_members(active.references(), candidate.references()),
        "recovered candidate references differ",
    )?;
    Ok(())
}

fn same_members<T>(left: &[T], right: &[T]) -> bool
where
    T: Eq,
{
    if left.len() != right.len() {
        return false;
    }
    let mut unmatched = right.iter().collect::<Vec<_>>();
    for member in left {
        let Some(index) = unmatched.iter().position(|candidate| *candidate == member) else {
            return false;
        };
        unmatched.swap_remove(index);
    }
    unmatched.is_empty()
}

fn relation(type_id: TypeId) -> String {
    format!(
        "_orna_data.t_{:032x}",
        u128::from_be_bytes(type_id.to_bytes())
    )
}

fn field(field_id: FieldId) -> String {
    format!("f_{:032x}", u128::from_be_bytes(field_id.to_bytes()))
}

fn require(condition: bool, message: &'static str) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}
