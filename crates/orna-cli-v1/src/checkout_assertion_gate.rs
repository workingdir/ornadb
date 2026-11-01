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
///
/// The CLI refuses `orna checkout` in this build, so only tests construct the
/// gate until the checkout verb is wired (ornadb-5il1.141).
#[cfg_attr(not(test), allow(dead_code))]
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use orna_repository_v1::{
        CandidateAssertionRefusal, Repository, RepositoryError, RuntimeGeneration,
    };

    use super::CheckoutGate;
    use crate::{ReplRuntime, format3, repl_runtime, run_repl_input};

    /// A `Book` table with no assertion; the fixture records its rows.
    const PLAIN_BOOK: &str = "pub table Book(id: Int) {\n    title: Str,\n}\n";

    /// The same table with a member assertion (ORNA-ASSERT-042) that every
    /// title is non-empty.
    const ASSERTING_BOOK: &str = "pub table Book(id: Int) {\n    title: Str,\n    \
assert every(row => row.title != \"\");\n}\n";

    fn git_stdout(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout)
            .expect("git prints UTF-8")
            .trim()
            .to_owned()
    }

    /// Runs one REPL write on the checked-out branch and publishes the pending
    /// row as a commit on that branch. The runtime is dropped before returning,
    /// so its owner lock never overlaps a checkout.
    fn write_and_publish(repository: Repository, source: &str, key: &str) -> Repository {
        let ReplRuntime {
            repository,
            mut session,
            mut transactions,
        } = repl_runtime(Some(repository)).expect("the REPL opens on the fixture");
        let repository = repository.expect("the REPL keeps the repository");
        let transactions_ref = transactions
            .as_mut()
            .expect("the fixture is a writable format-3 project");
        run_repl_input(
            Some(&repository),
            Some(&mut *transactions_ref),
            &mut session,
            source,
        )
        .expect("the REPL write is admitted");
        let state = transactions_ref
            .state
            .as_ref()
            .expect("the write opens the runtime state");
        transactions_ref
            .runtime
            .block_on(format3::publish_media_row(&repository, state, key, 1));
        drop(transactions);
        repository
    }

    /// Commits the asserting source on the checked-out branch with plain Git, as
    /// a source written outside the commit gate reaches a branch (precedent:
    /// tests/repl_table_assert.rs `commit_asserting_source_outside_the_gate`).
    fn commit_asserting_source(root: &Path) {
        std::fs::write(root.join("main.orna"), ASSERTING_BOOK)
            .expect("write the asserting source");
        format3::git(root, &["add", "main.orna"]);
        format3::git(root, &["commit", "-m", "asserting source outside the gate"]);
    }

    /// Plans the `target` branch from a freshly opened repository and runs the
    /// gated checkout, as a new invocation would.
    fn checkout_target(root: &Path) -> Result<(), RepositoryError> {
        let repository = Repository::discover(root.to_str().expect("UTF-8 path"))
            .expect("the fixture reopens");
        let plan = repository
            .plan_checkout("target", RuntimeGeneration::new(0))
            .expect("the target resolves");
        repository.execute_nonconflicting_git_checkout(&plan, &CheckoutGate)
    }

    #[test]
    fn checkout_refuses_a_target_whose_assertion_is_false_over_its_changed_rows() {
        let (directory, repository) =
            format3::format3_repository_with_tables(PLAIN_BOOK, &["Book"], &[]);
        let root = directory.path().to_path_buf();
        format3::git(&root, &["switch", "-c", "target"]);
        write_and_publish(repository, "main.Book.insert({ id: 2, title: \"\" })", "2");
        commit_asserting_source(&root);
        format3::git(&root, &["switch", "-"]);
        let base = git_stdout(&root, &["rev-parse", "HEAD"]);

        let result = checkout_target(&root);

        assert!(
            matches!(
                result,
                Err(RepositoryError::CandidateAssertionRefused(
                    CandidateAssertionRefusal::AssertionFalse(_)
                ))
            ),
            "{result:?}"
        );
        assert_eq!(git_stdout(&root, &["rev-parse", "HEAD"]), base);
        assert_eq!(git_stdout(&root, &["status", "--porcelain"]), "");
    }

    #[test]
    fn checkout_admits_a_target_without_changed_rows_without_evaluating_it() {
        let (directory, repository) =
            format3::format3_repository_with_tables(PLAIN_BOOK, &["Book"], &[]);
        let root = directory.path().to_path_buf();
        write_and_publish(repository, "main.Book.insert({ id: 2, title: \"\" })", "2");
        format3::git(&root, &["switch", "-c", "target"]);
        commit_asserting_source(&root);
        let target = git_stdout(&root, &["rev-parse", "HEAD"]);
        format3::git(&root, &["switch", "-"]);

        checkout_target(&root).expect("a target that changes no rows is admitted");

        assert_eq!(git_stdout(&root, &["rev-parse", "HEAD"]), target);
    }

    #[test]
    fn checkout_admits_a_target_whose_assertion_holds_over_its_changed_rows() {
        let (directory, repository) =
            format3::format3_repository_with_tables(PLAIN_BOOK, &["Book"], &[]);
        let root = directory.path().to_path_buf();
        format3::git(&root, &["switch", "-c", "target"]);
        write_and_publish(repository, "main.Book.insert({ id: 2, title: \"Dune\" })", "2");
        commit_asserting_source(&root);
        let target = git_stdout(&root, &["rev-parse", "HEAD"]);
        format3::git(&root, &["switch", "-"]);

        checkout_target(&root).expect("a target whose assertion holds is admitted");

        assert_eq!(git_stdout(&root, &["rev-parse", "HEAD"]), target);
    }
}
