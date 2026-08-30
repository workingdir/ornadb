#[path = "standard/catalogue.rs"]
mod catalogue;
#[path = "standard/executables.rs"]
mod executables;
#[path = "standard/header.rs"]
mod header;
use catalogue::load_standard_catalogue;
#[cfg(test)]
pub(super) use catalogue::{decode_standard_binding_target, recovered_standard_value_definition};
use executables::{load_standard_executable_facts, require_no_standard_executable_rows};
use header::{RecoveredStandardHeader, load_standard_header};

use super::*;

pub(crate) async fn load_verified_standard_library(
    transaction: &Transaction<'_>,
    expected_revision: StandardLibraryRevisionId,
) -> Result<VerifiedStandardLibrarySnapshot, PostgresKernelError> {
    let header = load_standard_header(transaction, expected_revision).await?;
    let units = load_source_units(transaction, header.bundle).await?;
    let source = StoredSourceRevision::new(
        header.bundle,
        header.source,
        header.source_parent,
        units,
        header.bundle_hash,
        header.source_hash,
    )
    .map_err(PostgresKernelError::RevisionInvariant)?;
    let bundle_record =
        DurableRecord::new("_orna_kernel.source_bundles", header.bundle.canonical());
    let computed_bundle_hash =
        source_bundle_digest(source.units()).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_bundle_hash != header.bundle_hash {
        return Err(bundle_record.invariant(
            "standard source bundle digest must match the ordered source unit records",
        ));
    }
    let source_record =
        DurableRecord::new("_orna_kernel.source_revisions", header.source.canonical());
    let computed_source_hash =
        source_revision_digest(&source).map_err(PostgresKernelError::CanonicalHash)?;
    if computed_source_hash != header.source_hash {
        return Err(source_record.invariant(
            "standard source revision digest must match its bundle, parent, and bundle digest",
        ));
    }

    let (catalogue, origins) = load_standard_catalogue(transaction, &header).await?;
    let snapshot = match header.digest_version {
        StandardLibraryDigestVersion::Version1 => {
            require_no_standard_executable_rows(transaction, header.revision).await?;
            StandardLibrarySnapshot::new(
                header.revision,
                header.digest_version,
                source,
                header.language_version,
                catalogue,
                origins,
                header.digest,
            )
            .map_err(PostgresKernelError::RevisionInvariant)?
        }
        StandardLibraryDigestVersion::Version2 => {
            let executables =
                load_standard_executable_facts(transaction, header.revision, &catalogue).await?;
            StandardLibrarySnapshot::new_with_executables(
                header.revision,
                header.digest_version,
                source,
                header.language_version,
                catalogue,
                executables,
                origins,
                header.digest,
            )
            .map_err(PostgresKernelError::RevisionInvariant)?
        }
        _ => {
            return Err(DurableRecord::new(
                "_orna_kernel.standard_library_revisions",
                header.revision.canonical(),
            )
            .invariant("standard library digest version is unsupported"));
        }
    };
    #[cfg(feature = "test-hooks")]
    {
        verify_recovered_standard_snapshot_for_test_hooks(snapshot)
    }
    #[cfg(not(feature = "test-hooks"))]
    {
        verify_recovered_standard_snapshot(snapshot)
    }
}

pub(super) fn verify_recovered_standard_snapshot(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, PostgresKernelError> {
    let revision = snapshot.revision();
    let result = match revision {
        STANDARD_LIBRARY_REVISION_ID => verify_standard_library_snapshot(snapshot),
        STANDARD_LIBRARY_V2_REVISION_ID => verify_standard_library_v2_snapshot(snapshot),
        STANDARD_LIBRARY_V3_REVISION_ID => verify_standard_library_v3_snapshot(snapshot),
        STANDARD_LIBRARY_V4_REVISION_ID => verify_standard_library_v4_snapshot(snapshot),
        STANDARD_LIBRARY_V5_REVISION_ID => verify_standard_library_v5_snapshot(snapshot),
        STANDARD_LIBRARY_V6_REVISION_ID => verify_standard_library_v6_snapshot(snapshot),
        STANDARD_LIBRARY_V7_REVISION_ID => verify_standard_library_v7_snapshot(snapshot),
        STANDARD_LIBRARY_V8_REVISION_ID => verify_standard_library_v8_snapshot(snapshot),
        STANDARD_LIBRARY_V9_REVISION_ID => verify_standard_library_v9_snapshot(snapshot),
        _ => {
            return Err(DurableRecord::new(
                "_orna_kernel.standard_library_revisions",
                revision.canonical(),
            )
            .invariant("standard library revision identity is not an accepted retained revision"));
        }
    };
    result.map_err(|error| map_recovered_standard_verifier_error(error, revision))
}

fn map_recovered_standard_verifier_error(
    error: orna_standard::StandardLibraryError,
    revision: StandardLibraryRevisionId,
) -> PostgresKernelError {
    match error {
        orna_standard::StandardLibraryError::CanonicalHash { source } => {
            PostgresKernelError::CanonicalHash(source)
        }
        orna_standard::StandardLibraryError::Revision { source } => {
            PostgresKernelError::RevisionInvariant(source)
        }
        _ => DurableRecord::new(
            "_orna_kernel.standard_library_revisions",
            revision.canonical(),
        )
        .invariant("standard library retained verifier rejected the recovered snapshot"),
    }
}

#[cfg(feature = "test-hooks")]
fn verify_recovered_standard_snapshot_for_test_hooks(
    snapshot: StandardLibrarySnapshot,
) -> Result<VerifiedStandardLibrarySnapshot, PostgresKernelError> {
    let revision = snapshot.revision();
    if matches!(
        revision,
        STANDARD_LIBRARY_REVISION_ID
            | STANDARD_LIBRARY_V2_REVISION_ID
            | STANDARD_LIBRARY_V3_REVISION_ID
            | STANDARD_LIBRARY_V4_REVISION_ID
            | STANDARD_LIBRARY_V5_REVISION_ID
            | STANDARD_LIBRARY_V6_REVISION_ID
            | STANDARD_LIBRARY_V7_REVISION_ID
            | STANDARD_LIBRARY_V8_REVISION_ID
            | STANDARD_LIBRARY_V9_REVISION_ID
    ) {
        return verify_recovered_standard_snapshot(snapshot);
    }

    let result = match snapshot.digest_version() {
        StandardLibraryDigestVersion::Version1 => {
            verify_structural_standard_library_snapshot(snapshot)
        }
        StandardLibraryDigestVersion::Version2 => {
            verify_structural_standard_library_v2_snapshot(snapshot)
        }
        _ => {
            return Err(DurableRecord::new(
                "_orna_kernel.standard_library_revisions",
                revision.canonical(),
            )
            .invariant("standard library test fixture digest version is unsupported"));
        }
    };
    result.map_err(PostgresKernelError::CanonicalHash)
}

fn require_standard_library_revision(
    row: &Row,
    record: &DurableRecord,
    expected: StandardLibraryRevisionId,
    member: &'static str,
) -> Result<(), PostgresKernelError> {
    let standard = StandardLibraryRevisionId::from_bytes(identity_bytes(
        record.column(
            row,
            "standard_library_revision_id",
            "standard catalogue member revision identity must be 16 bytes",
        )?,
        record,
        "standard catalogue member revision identity must be 16 bytes",
    )?);
    if standard != expected {
        return Err(record.invariant(match member {
            "schema" => "standard schema must belong to the selected standard library revision",
            "value type" => {
                "standard value type must belong to the selected standard library revision"
            }
            "enum type" => {
                "standard enum type must belong to the selected standard library revision"
            }
            "function" => {
                "standard function must belong to the selected standard library revision"
            }
            "parameter" => {
                "standard parameter must belong to the selected standard library revision"
            }
            "function revision" => {
                "standard function revision must belong to the selected standard library revision"
            }
            "function artifact" => {
                "standard function artifact must belong to the selected standard library revision"
            }
            "definition reference" => {
                "standard definition reference must belong to the selected standard library revision"
            }
            _ => "standard type binding must belong to the selected standard library revision",
        }));
    }
    Ok(())
}
