use super::{CommitDeclaration, HistoricalDeclaration, ScanProgress};
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};

/// Commits of one table's history, oldest first. The third commit declares the generation of
/// the second again, so the newest-first list must keep only the distinct generations.
fn commits() -> ScanProgress<u32, u32> {
    ScanProgress::new(vec![1, 2, 2, 3])
}

#[allow(clippy::trivially_copy_pass_by_ref, clippy::unnecessary_wraps)]
fn declare(commit: &u32) -> Result<CommitDeclaration<u32>, String> {
    Ok(CommitDeclaration::Declares(HistoricalDeclaration {
        generation: *commit,
        field_ids: HashSet::new(),
        defaults: BTreeMap::new(),
    }))
}

#[test]
fn a_pass_without_budget_visits_no_commit() {
    let visited = Cell::new(0_usize);
    let counted = |commit: &u32| {
        visited.set(visited.get() + 1);
        declare(commit)
    };
    let mut progress = commits();
    let complete = progress
        .advance(|| false, &counted)
        .expect("a pass with no budget should not fail");
    assert!(!complete);
    assert_eq!(visited.get(), 0);
    assert_eq!(progress.next, 0);
}

#[test]
fn a_paused_pass_resumes_where_it_stopped_and_matches_an_uninterrupted_pass() {
    let visited = Cell::new(0_usize);
    let counted = |commit: &u32| {
        visited.set(visited.get() + 1);
        declare(commit)
    };
    let mut budget = 2_usize;
    let mut progress = commits();
    let complete = progress
        .advance(
            || {
                if budget == 0 {
                    false
                } else {
                    budget -= 1;
                    true
                }
            },
            &counted,
        )
        .expect("the first pass should not fail");
    assert!(!complete);
    assert_eq!(visited.get(), 2);
    assert_eq!(progress.next, 2);

    let complete = progress
        .advance(|| true, &counted)
        .expect("the resumed pass should not fail");
    assert!(complete);
    assert_eq!(visited.get(), 4);
    let resumed = progress.finish();

    let mut uninterrupted = commits();
    uninterrupted
        .advance(|| true, &counted)
        .expect("the uninterrupted pass should not fail");
    assert_eq!(resumed, uninterrupted.finish());
    assert_eq!(resumed.0, vec![3, 2, 1]);
}

/// Commit `0` has no committed identities and commit `9` does not declare the table. The other
/// commits declare their own generation.
#[allow(clippy::trivially_copy_pass_by_ref, clippy::unnecessary_wraps)]
fn classify(commit: &u32) -> Result<CommitDeclaration<u32>, String> {
    match commit {
        0 => Ok(CommitDeclaration::Unidentified),
        9 => Ok(CommitDeclaration::Undeclared),
        generation => Ok(CommitDeclaration::Declares(HistoricalDeclaration {
            generation: *generation,
            field_ids: HashSet::new(),
            defaults: BTreeMap::new(),
        })),
    }
}

#[test]
fn identity_less_commits_are_counted_and_declare_no_generation() {
    let mut progress = ScanProgress::new(vec![0, 1, 9, 0, 2]);
    let complete = progress
        .advance(|| true, &classify)
        .expect("the pass should not fail");
    assert!(complete);
    let (generations, introductions, unidentified) = progress.finish();
    assert_eq!(unidentified, 2, "each identity-less commit is counted");
    assert_eq!(
        generations,
        vec![2, 1],
        "identity-less commits declare no generation"
    );
    assert!(introductions.is_empty());
}
