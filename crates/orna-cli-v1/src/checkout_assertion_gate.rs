//! The CLI's checkout assertion gate (ORNA-ASSERT-044). A checkout that would
//! make a target commit current runs the target through the same validator that
//! `orna commit` and `orna publish` use, before any Git state changes. The
//! shared rules live in `assertion_gate`, so no command disagrees about what an
//! admitted assertion means.

use orna_repository_v1::{
    CandidateAssertionRefusal, CheckoutAssertionGate, CheckoutPreflight, Repository,
};

use super::assertion_gate::validate_committed_target;

/// The gate that `Repository::execute_nonconflicting_git_checkout` runs after it
/// re-verifies the preflight and before any `git switch`. It validates the
/// target commit that the preflight names.
pub(crate) struct CheckoutGate;

impl CheckoutAssertionGate for CheckoutGate {
    fn validate(
        &self,
        repository: &Repository,
        preflight: &CheckoutPreflight,
    ) -> Result<(), CandidateAssertionRefusal> {
        validate_committed_target(repository, preflight.target().commit())
    }
}
