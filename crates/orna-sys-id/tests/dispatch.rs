//! Proof that the generated callable dispatch is exhaustive and wildcard-free.
//!
//! [`SysFunctionHost`] has one required method per catalogue variant, so this host
//! classifies every identity explicitly; an added catalogue callable fails to compile
//! here until the new identity is classified.

use orna_sys_id::{
    Origin, SYS_FUNCTION_COUNT, SysFunction, SysFunctionBinding, SysFunctionHost, dispatch,
};

const GENERATED_DISPATCH: &str = include_str!(concat!(env!("OUT_DIR"), "/dispatch.rs"));

struct ClassificationHost;

impl ClassificationHost {
    fn binding(&self, function: SysFunction) -> SysFunctionBinding {
        match function {
            SysFunction::BlobRead
            | SysFunction::BlobAnnotate
            | SysFunction::RtInfo
            | SysFunction::ExplainQuery => SysFunctionBinding::Implemented,
            _ => SysFunctionBinding::Unavailable(Origin::Portable),
        }
    }
}

impl SysFunctionHost for ClassificationHost {
    fn meta(&self) -> SysFunctionBinding {
        self.binding(SysFunction::Meta)
    }
    fn object(&self) -> SysFunctionBinding {
        self.binding(SysFunction::Object)
    }
    fn resolve(&self) -> SysFunctionBinding {
        self.binding(SysFunction::Resolve)
    }
    fn resolve_function(&self) -> SysFunctionBinding {
        self.binding(SysFunction::ResolveFunction)
    }
    fn describe(&self) -> SysFunctionBinding {
        self.binding(SysFunction::Describe)
    }
    fn source_object_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::SourceObjectRef)
    }
    fn source_file_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::SourceFileRef)
    }
    fn history_object_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::HistoryObjectRef)
    }
    fn history_file_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::HistoryFileRef)
    }
    fn blame_file_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlameFileRef)
    }
    fn blame_row_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlameRowRef)
    }
    fn dependencies(&self) -> SysFunctionBinding {
        self.binding(SysFunction::Dependencies)
    }
    fn dependents(&self) -> SysFunctionBinding {
        self.binding(SysFunction::Dependents)
    }
    fn snapshot_snapshot_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::SnapshotSnapshotRef)
    }
    fn snapshot_commit_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::SnapshotCommitRef)
    }
    fn snapshot_branch_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::SnapshotBranchRef)
    }
    fn snapshot_tag_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::SnapshotTagRef)
    }
    fn snapshot_git_oid(&self) -> SysFunctionBinding {
        self.binding(SysFunction::SnapshotGitOid)
    }
    fn snapshot_str(&self) -> SysFunctionBinding {
        self.binding(SysFunction::SnapshotStr)
    }
    fn diff(&self) -> SysFunctionBinding {
        self.binding(SysFunction::Diff)
    }
    fn changes(&self) -> SysFunctionBinding {
        self.binding(SysFunction::Changes)
    }
    fn explain_query(&self) -> SysFunctionBinding {
        self.binding(SysFunction::ExplainQuery)
    }
    fn explain_function_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::ExplainFunctionRef)
    }
    fn explain_diagnostic(&self) -> SysFunctionBinding {
        self.binding(SysFunction::ExplainDiagnostic)
    }
    fn checkpoint(&self) -> SysFunctionBinding {
        self.binding(SysFunction::Checkpoint)
    }
    fn render_diagnostic(&self) -> SysFunctionBinding {
        self.binding(SysFunction::RenderDiagnostic)
    }
    fn rt_info(&self) -> SysFunctionBinding {
        self.binding(SysFunction::RtInfo)
    }
    fn invoke_value(&self) -> SysFunctionBinding {
        self.binding(SysFunction::InvokeValue)
    }
    fn invoke_t(&self) -> SysFunctionBinding {
        self.binding(SysFunction::InvokeT)
    }
    fn start_value(&self) -> SysFunctionBinding {
        self.binding(SysFunction::StartValue)
    }
    fn start_t(&self) -> SysFunctionBinding {
        self.binding(SysFunction::StartT)
    }
    fn r#await(&self) -> SysFunctionBinding {
        self.binding(SysFunction::Await)
    }
    fn cancel(&self) -> SysFunctionBinding {
        self.binding(SysFunction::Cancel)
    }
    fn admin_commit(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCommit)
    }
    fn admin_checkout_snapshot_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCheckoutSnapshotRef)
    }
    fn admin_checkout_commit_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCheckoutCommitRef)
    }
    fn admin_checkout_branch_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCheckoutBranchRef)
    }
    fn admin_checkout_tag_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCheckoutTagRef)
    }
    fn admin_checkout_git_oid(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCheckoutGitOid)
    }
    fn admin_checkout_str(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCheckoutStr)
    }
    fn admin_create_branch_snapshot_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCreateBranchSnapshotRef)
    }
    fn admin_create_branch_commit_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCreateBranchCommitRef)
    }
    fn admin_create_branch_branch_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCreateBranchBranchRef)
    }
    fn admin_create_branch_tag_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCreateBranchTagRef)
    }
    fn admin_create_branch_git_oid(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCreateBranchGitOid)
    }
    fn admin_create_branch_str(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCreateBranchStr)
    }
    fn admin_flush(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminFlush)
    }
    fn admin_compact(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCompact)
    }
    fn admin_verify(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminVerify)
    }
    fn admin_cancel_run(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCancelRun)
    }
    fn admin_pause_stream(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminPauseStream)
    }
    fn admin_resume_stream(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminResumeStream)
    }
    fn admin_reset_checkpoint(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminResetCheckpoint)
    }
    fn admin_retry_failure(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminRetryFailure)
    }
    fn admin_skip_failure(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminSkipFailure)
    }
    fn admin_replay_failure(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminReplayFailure)
    }
    fn admin_resolve_failure(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminResolveFailure)
    }
    fn consumer_identity(&self) -> SysFunctionBinding {
        self.binding(SysFunction::ConsumerIdentity)
    }
    fn admin_plan_checkout_snapshot_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminPlanCheckoutSnapshotRef)
    }
    fn admin_plan_checkout_commit_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminPlanCheckoutCommitRef)
    }
    fn admin_plan_checkout_branch_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminPlanCheckoutBranchRef)
    }
    fn admin_plan_checkout_tag_ref(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminPlanCheckoutTagRef)
    }
    fn admin_plan_checkout_git_oid(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminPlanCheckoutGitOid)
    }
    fn admin_plan_checkout_str(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminPlanCheckoutStr)
    }
    fn blob_length(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobLength)
    }
    fn blob_digest(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobDigest)
    }
    fn blob_read(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobRead)
    }
    fn blob_verify(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobVerify)
    }
    fn blob_from_bytes(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobFromBytes)
    }
    fn blob_to_bytes(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobToBytes)
    }
    fn blob_begin(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobBegin)
    }
    fn blob_append(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobAppend)
    }
    fn blob_finish(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobFinish)
    }
    fn blob_abort(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobAbort)
    }
    fn blob_capture_file(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobCaptureFile)
    }
    fn blob_resource(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobResource)
    }
    fn admin_review_commit(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminReviewCommit)
    }
    fn admin_commit_reviewed(&self) -> SysFunctionBinding {
        self.binding(SysFunction::AdminCommitReviewed)
    }
    fn blob_media_type(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobMediaType)
    }
    fn blob_suffix(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobSuffix)
    }
    fn blob_annotate(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobAnnotate)
    }
    fn blob_same_content(&self) -> SysFunctionBinding {
        self.binding(SysFunction::BlobSameContent)
    }
}

#[test]
fn generated_dispatch_has_one_arm_per_catalogue_variant_and_no_wildcard() {
    let start = GENERATED_DISPATCH
        .find("pub fn dispatch")
        .expect("the generated file contains the dispatcher");
    let dispatcher = &GENERATED_DISPATCH[start..];
    assert!(
        !dispatcher.contains("_ =>"),
        "the generated dispatcher must not contain a wildcard arm"
    );
    assert_eq!(
        dispatcher.matches("=> host.").count(),
        SYS_FUNCTION_COUNT,
        "the generated dispatcher must have exactly one arm per catalogue callable"
    );
}

#[test]
fn dispatch_classifies_every_catalogue_variant() {
    let host = ClassificationHost;
    let implemented: Vec<SysFunction> = SysFunction::ALL
        .into_iter()
        .filter(|function| dispatch(&host, *function) == SysFunctionBinding::Implemented)
        .collect();
    assert_eq!(
        implemented,
        [
            SysFunction::ExplainQuery,
            SysFunction::RtInfo,
            SysFunction::BlobRead,
            SysFunction::BlobAnnotate,
        ]
    );
    for function in SysFunction::ALL {
        let binding = dispatch(&host, function);
        assert!(
            binding == SysFunctionBinding::Implemented
                || binding == SysFunctionBinding::Unavailable(Origin::Portable),
            "{} is not classified: {binding:?}",
            function.label()
        );
    }
}
