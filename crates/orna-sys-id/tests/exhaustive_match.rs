//! Compile-time proof that the closed identity sets stay closed.
//!
//! A catalogue change regenerates the enums. This test independently enumerates every
//! `SysFunction` variant in a `match` with no wildcard arm, so adding or removing a
//! catalogue callable stops this test compiling until the binding is reviewed - the
//! same discipline stage S3 applies to the dispatcher.

use orna_sys_id::{
    AbiVersion, DiagnosticIdentity, Namespace, Origin, PortableRemoval, SysEffect, SysFailureCode,
    SysFunction, SysMemberPath, SysTypeKind, VersionedId,
};

/// The hand-reviewed portable binding: every catalogue callable is portable.
fn bound_origin(function: SysFunction) -> Origin {
    match function {
        SysFunction::Meta
        | SysFunction::Object
        | SysFunction::Resolve
        | SysFunction::ResolveFunction
        | SysFunction::Describe
        | SysFunction::SourceObjectRef
        | SysFunction::SourceFileRef
        | SysFunction::HistoryObjectRef
        | SysFunction::HistoryFileRef
        | SysFunction::BlameFileRef
        | SysFunction::BlameRowRef
        | SysFunction::Dependencies
        | SysFunction::Dependents
        | SysFunction::SnapshotSnapshotRef
        | SysFunction::SnapshotCommitRef
        | SysFunction::SnapshotBranchRef
        | SysFunction::SnapshotTagRef
        | SysFunction::SnapshotGitOid
        | SysFunction::SnapshotStr
        | SysFunction::Diff
        | SysFunction::Changes
        | SysFunction::ExplainQuery
        | SysFunction::ExplainFunctionRef
        | SysFunction::ExplainDiagnostic
        | SysFunction::Checkpoint
        | SysFunction::RenderDiagnostic
        | SysFunction::RtInfo
        | SysFunction::InvokeValue
        | SysFunction::InvokeT
        | SysFunction::StartValue
        | SysFunction::StartT
        | SysFunction::Await
        | SysFunction::Cancel
        | SysFunction::AdminCommit
        | SysFunction::AdminCheckoutSnapshotRef
        | SysFunction::AdminCheckoutCommitRef
        | SysFunction::AdminCheckoutBranchRef
        | SysFunction::AdminCheckoutTagRef
        | SysFunction::AdminCheckoutGitOid
        | SysFunction::AdminCheckoutStr
        | SysFunction::AdminCreateBranchSnapshotRef
        | SysFunction::AdminCreateBranchCommitRef
        | SysFunction::AdminCreateBranchBranchRef
        | SysFunction::AdminCreateBranchTagRef
        | SysFunction::AdminCreateBranchGitOid
        | SysFunction::AdminCreateBranchStr
        | SysFunction::AdminFlush
        | SysFunction::AdminCompact
        | SysFunction::AdminVerify
        | SysFunction::AdminCancelRun
        | SysFunction::AdminPauseStream
        | SysFunction::AdminResumeStream
        | SysFunction::AdminResetCheckpoint
        | SysFunction::AdminRetryFailure
        | SysFunction::AdminSkipFailure
        | SysFunction::AdminReplayFailure
        | SysFunction::AdminResolveFailure
        | SysFunction::ConsumerIdentity
        | SysFunction::AdminPlanCheckoutSnapshotRef
        | SysFunction::AdminPlanCheckoutCommitRef
        | SysFunction::AdminPlanCheckoutBranchRef
        | SysFunction::AdminPlanCheckoutTagRef
        | SysFunction::AdminPlanCheckoutGitOid
        | SysFunction::AdminPlanCheckoutStr
        | SysFunction::BlobLength
        | SysFunction::BlobDigest
        | SysFunction::BlobRead
        | SysFunction::BlobVerify
        | SysFunction::BlobFromBytes
        | SysFunction::BlobToBytes
        | SysFunction::BlobBegin
        | SysFunction::BlobAppend
        | SysFunction::BlobFinish
        | SysFunction::BlobAbort
        | SysFunction::BlobCaptureFile
        | SysFunction::BlobResource
        | SysFunction::AdminReviewCommit
        | SysFunction::AdminCommitReviewed
        | SysFunction::BlobMediaType
        | SysFunction::BlobSuffix
        | SysFunction::BlobAnnotate
        | SysFunction::BlobSameContent => Origin::Portable,
    }
}

#[test]
fn every_sys_function_variant_is_bound_exactly_once() {
    assert_eq!(SysFunction::ALL.len(), 82);
    for function in SysFunction::ALL {
        assert_eq!(bound_origin(function), function.origin());
        assert_eq!(function.origin(), Origin::Portable);
    }
}

#[test]
fn closed_sets_have_exhaustive_matches_without_wildcards() {
    for origin in Origin::ALL {
        let label = match origin {
            Origin::Portable => "portable",
            Origin::Engine => "engine",
        };
        assert_eq!(origin.as_str(), label);
        assert_eq!(Origin::parse(label), Some(origin));
    }
    for effect in SysEffect::ALL {
        let label = match effect {
            SysEffect::Read => "read",
            SysEffect::Invoke => "invoke",
            SysEffect::Admin => "admin",
        };
        assert_eq!(effect.as_str(), label);
        assert_eq!(SysEffect::parse(label), Some(effect));
    }
    for kind in SysTypeKind::ALL {
        let label = match kind {
            SysTypeKind::Opaque => "opaque",
            SysTypeKind::OpaqueGeneric => "opaque-generic",
            SysTypeKind::Record => "record",
            SysTypeKind::RecordGeneric => "record-generic",
            SysTypeKind::Alias => "alias",
        };
        assert_eq!(kind.as_str(), label);
        assert_eq!(SysTypeKind::parse(label), Some(kind));
    }
    for namespace in Namespace::ALL {
        let label = match namespace {
            Namespace::Host => "host",
            Namespace::Langitem => "langitem",
            Namespace::Orna => "orna",
            Namespace::Std => "std",
            Namespace::Sys => "sys",
            Namespace::Vendor => "vendor",
        };
        assert_eq!(namespace.as_str(), label);
        assert_eq!(Namespace::parse(label), Some(namespace));
    }
}

#[test]
fn failure_code_origins_are_exhaustive_and_ordered() {
    assert_eq!(SysFailureCode::PORTABLE.len(), 67);
    assert_eq!(
        SysFailureCode::ALL.len(),
        orna_sys_id::SYS_FAILURE_CODE_COUNT
    );
    for (index, code) in SysFailureCode::ALL.iter().copied().enumerate() {
        let origin_label = match code.origin() {
            Origin::Portable => "portable",
            Origin::Engine => "engine",
        };
        assert_eq!(origin_label, code.origin().as_str());
        assert_eq!(code.is_portable(), code.origin() == Origin::Portable);
        assert_eq!(code.origin() == Origin::Portable, index < 67);
        assert_eq!(SysFailureCode::parse(code.as_str()), Some(code));
        assert_eq!(code.to_string(), code.as_str());
    }
    assert_eq!(
        &SysFailureCode::ALL[..67],
        SysFailureCode::PORTABLE,
        "portable codes must occupy the dense prefix"
    );
}

#[test]
fn removal_and_path_shapes_are_exhaustive() {
    let replaced = PortableRemoval::Replaced {
        by: SysMemberPath::Singleton(orna_sys_id::SysSingleton::Rt),
        diagnostic: DiagnosticIdentity::Rule(orna_sys_id::ORNA100_E_SYS_RUNTIME),
    };
    let retired = PortableRemoval::Retired {
        diagnostic: DiagnosticIdentity::Failure(SysFailureCode::VersionIncompatible),
        reason: "reason",
    };
    for removal in [replaced, retired] {
        match removal {
            PortableRemoval::Replaced { by, diagnostic } => {
                assert!(!by.to_string().is_empty());
                assert!(!diagnostic.as_str().is_empty());
            }
            PortableRemoval::Retired { diagnostic, reason } => {
                assert_eq!(reason, "reason");
                assert_eq!(diagnostic.as_str(), "sys.version.incompatible");
            }
        }
    }
    for path in [
        SysMemberPath::Callable(SysFunction::Meta),
        SysMemberPath::Relation(orna_sys_id::SysRelation::Database),
        SysMemberPath::GroupedHandle(orna_sys_id::SysGroupedHandle::CatalogDatabases),
        SysMemberPath::Singleton(orna_sys_id::SysSingleton::Rt),
        SysMemberPath::SingletonField(
            orna_sys_id::SysSingleton::Database,
            orna_sys_id::SysField::Id,
        ),
    ] {
        let text = match path {
            SysMemberPath::Callable(function) => function.label().to_owned(),
            SysMemberPath::Relation(relation) => relation.name().to_owned(),
            SysMemberPath::GroupedHandle(handle) => handle.name().to_owned(),
            SysMemberPath::Singleton(singleton) => singleton.name().to_owned(),
            SysMemberPath::SingletonField(singleton, field) => {
                format!("{}.{}", singleton.name(), field.name())
            }
        };
        assert_eq!(path.to_string(), text);
    }
    let version = AbiVersion::new(1, Some(0));
    let parsed: VersionedId = "host.std.io.fs.read@1.0".parse().unwrap();
    assert_eq!(parsed.version(), version);
}
