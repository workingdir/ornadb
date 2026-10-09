//! Snapshot-selector judgement shared by every `--at` read.
//!
//! `orna history --at` and `orna query --at` must agree on which selectors are
//! refused, so the test lives in one place and both verbs map its failure to
//! their own diagnostic.

/// Whether a `--at` selector names one branch, tag or remote branch but is not
/// spelled as a full ref, so Git would resolve it by ref lookup precedence.
///
/// `git rev-parse` *succeeds* on such a name and only warns, so a read pinned
/// with `--at amb` would silently return whichever of the colliding refs it
/// preferred. Nothing in the walk could report that choice, so the read is
/// refused instead: the selector is compared against the branch, tag and
/// remote-branch namespaces, and two exact matches mean the name does not
/// identify one snapshot.
///
/// Only exact matches count. `git for-each-ref <pattern>` matches by prefix, so
/// `refs/remotes/origin` also lists `refs/remotes/origin/main`; those are
/// different names and must not be read as a collision.
pub(super) fn bare_ref_selector_is_ambiguous(
    directory: &str,
    selector: &str,
) -> Result<bool, String> {
    // Only a name that could collide is checked. A commit id, a `HEAD~2`-style
    // expression, a revision `@`/`:` syntax, a `a..b` range or a full `refs/`
    // path names one object by construction. Slashed names such as
    // `origin/main` are *not* skipped: a branch and a remote-tracking branch
    // can share one.
    if selector.is_empty()
        || selector.starts_with('-')
        || selector.starts_with("refs/")
        || selector == "HEAD"
        || selector.contains([':', '^', '~', '@'])
        || selector.contains("..")
    {
        return Ok(false);
    }
    let output = std::process::Command::new("git")
        .args([
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/heads/{selector}"),
            &format!("refs/tags/{selector}"),
            &format!("refs/remotes/{selector}"),
            &format!("refs/remotes/{selector}/HEAD"),
        ])
        .current_dir(directory)
        .output()
        .map_err(|error| format!("check that Git is installed and available on PATH: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let wanted = [
        format!("refs/heads/{selector}"),
        format!("refs/tags/{selector}"),
        format!("refs/remotes/{selector}"),
        format!("refs/remotes/{selector}/HEAD"),
    ];
    let matches = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|name| wanted.iter().any(|candidate| candidate == name))
        .count();
    Ok(matches > 1)
}
